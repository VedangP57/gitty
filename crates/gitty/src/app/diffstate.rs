//! Display state of one file's diff: the expandable view plus cursor, scroll and horizontal
//! scroll. Rows are addressed in "virtual" space: a synthesised hunk header (when the first
//! hunk has no gap row above it) is row 0 and shifts the view's rows by one.

use std::sync::Arc;

use gitty_core::diff::FileDiff;
use gitty_core::diff::view::{DiffView, Expand, Row, SplitRow};

use crate::msg::DiffKey;
use crate::text::{Glyph, layout, line_width, max_hscroll, wrap_starts};

/// Text widths for wrapped rows: `left` is the unified text width, or the left split half's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wrap {
    pub left: u32,
    pub right: u32,
    pub tab: u8,
}

/// Screen lines `bytes` takes when wrapped at `width` columns.
pub fn wrapped_lines(bytes: &[u8], width: u32, tab: u8, scratch: &mut Vec<Glyph>, starts: &mut Vec<usize>) -> usize {
    if bytes.len() <= width as usize && bytes.iter().all(|&b| (0x20..0x7f).contains(&b)) {
        return 1;
    }
    layout(bytes, tab, scratch);
    wrap_starts(scratch, width, starts);
    starts.len()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VRow {
    Header(String),
    Row(Row),
    Split(SplitRow),
}

pub struct DiffState {
    pub key: DiffKey,
    pub diff: Arc<FileDiff>,
    pub view: DiffView,
    pub cursor: usize,
    pub scroll: usize,
    pub hscroll: u16,
    /// Display width (capped) of the widest line the view shows; measured when first needed and
    /// again after the view expands.
    widest: Option<u32>,
    /// Columns of text each side showed last frame: what sideways scrolling is measured against.
    pub visible: u16,
    /// Header for the first hunk when no gap row precedes it.
    pub first_header: Option<String>,
    /// Every change block's pairing has been applied to the split layout.
    pub paired_all: bool,
}

/// The content a row stands for, used to keep the cursor on the same line across rebuilds.
#[derive(Debug, Clone, Copy)]
struct Anchor {
    old: Option<u32>,
    new: Option<u32>,
}

impl DiffState {
    pub fn new(key: DiffKey, diff: Arc<FileDiff>) -> DiffState {
        let view = diff.view();
        let mut s = DiffState { key, diff, view, cursor: 0, scroll: 0, hscroll: 0, widest: None, visible: 0, first_header: None, paired_all: false };
        s.apply_ready_pairing(false);
        s.refresh_header();
        s
    }

    fn view_rows(&self, split: bool) -> usize {
        if split { self.view.split_row_count() } else { self.view.row_count() }
    }

    fn offset(&self) -> usize {
        usize::from(self.first_header.is_some())
    }

    pub fn rows(&self, split: bool) -> usize {
        let n = self.view_rows(split);
        if n == 0 { 0 } else { n + self.offset() }
    }

    pub fn vrow(&self, i: usize, split: bool) -> Option<VRow> {
        if i >= self.rows(split) {
            return None;
        }
        if let Some(h) = &self.first_header
            && i == 0 {
                return Some(VRow::Header(h.clone()));
            }
        let j = i - self.offset();
        Some(if split { VRow::Split(self.view.split_row(j)) } else { VRow::Row(self.view.row(j)) })
    }

    /// Width of the widest line of the view, tabs expanded as drawn (measured once per view).
    pub fn widest(&mut self, tab: u8) -> u32 {
        if let Some(w) = self.widest {
            return w;
        }
        let (mut scratch, mut w) = (Vec::new(), 0);
        let d = &self.diff;
        for i in 0..self.view.row_count() {
            let lines = match self.view.row(i) {
                Row::Gap { .. } => [None, None],
                Row::Context { old, new } => [Some((&d.old, old)), Some((&d.new, new))],
                Row::Del { old, .. } => [Some((&d.old, old)), None],
                Row::Add { new, .. } => [None, Some((&d.new, new))],
            };
            for (text, line) in lines.into_iter().flatten() {
                // the " ⊘" after a last line without a newline takes two more columns
                let eol = if line + 1 == text.len() && text.no_eol() { 2 } else { 0 };
                w = w.max(line_width(text.line(line), tab, &mut scratch) + eol);
            }
        }
        self.widest = Some(w);
        w
    }

    /// The farthest the diff can scroll sideways: to the end of its widest line.
    pub fn max_hscroll(&mut self, tab: u8) -> u16 {
        max_hscroll(self.widest(tab), u32::from(self.visible)) as u16
    }

    fn refresh_header(&mut self) {
        self.first_header = None;
        if self.view.row_count() == 0 || matches!(self.view.row(0), Row::Gap { .. }) {
            return;
        }
        let (mut ol, mut nl) = (0u32, 0u32);
        for i in 0..self.view.row_count() {
            match self.view.row(i) {
                Row::Gap { .. } => break,
                Row::Context { .. } => {
                    ol += 1;
                    nl += 1;
                }
                Row::Del { .. } => ol += 1,
                Row::Add { .. } => nl += 1,
            }
        }
        let start = |n: u32| u32::from(n > 0);
        self.first_header = Some(format!("@@ -{},{ol} +{},{nl} @@", start(ol), start(nl)));
    }

    fn anchor(&self, i: usize, split: bool) -> Option<Anchor> {
        match self.vrow(i, split)? {
            VRow::Header(_) => None,
            VRow::Row(r) => match r {
                Row::Gap { .. } => None,
                Row::Context { old, new } => Some(Anchor { old: Some(old), new: Some(new) }),
                Row::Del { old, .. } => Some(Anchor { old: Some(old), new: None }),
                Row::Add { new, .. } => Some(Anchor { old: None, new: Some(new) }),
            },
            VRow::Split(r) => match r {
                SplitRow::Gap { .. } => None,
                SplitRow::Context { old, new } => Some(Anchor { old: Some(old), new: Some(new) }),
                SplitRow::Change { old, new, .. } => Some(Anchor { old, new }),
            },
        }
    }

    /// First row at or after the anchored content.
    fn find(&self, a: Anchor, split: bool) -> usize {
        let n = self.rows(split);
        for i in 0..n {
            let Some(b) = self.anchor(i, split) else { continue };
            let reached = match ((a.new, b.new), (a.old, b.old)) {
                ((Some(an), Some(bn)), _) => bn >= an,
                (_, (Some(ao), Some(bo))) => bo >= ao,
                _ => continue,
            };
            if reached {
                return i;
            }
        }
        n.saturating_sub(1)
    }

    fn clamp(&mut self, split: bool) {
        let n = self.rows(split);
        self.cursor = self.cursor.min(n.saturating_sub(1));
        self.scroll = self.scroll.min(n.saturating_sub(1));
    }

    fn gap_at(&self, i: usize, split: bool) -> Option<usize> {
        match self.vrow(i, split)? {
            VRow::Row(Row::Gap { gap, .. }) | VRow::Split(SplitRow::Gap { gap, .. }) => Some(gap),
            _ => None,
        }
    }

    /// Expands the view and keeps the cursor on the content it was on.
    pub fn expand(&mut self, e: Expand, split: bool) {
        let anchor = self.anchor(self.cursor, split);
        let on_gap = self.gap_at(self.cursor, split);
        self.view.expand(e);
        self.widest = None;
        self.refresh_header();
        // on a gap or header row the cursor keeps its position
        if let (Some(a), None) = (anchor, on_gap) {
            self.cursor = self.find(a, split);
        }
        self.clamp(split);
    }

    /// `e`: on a gap row, reveal all of it; otherwise grow the nearest gap toward the cursor.
    pub fn expand_near_cursor(&mut self, split: bool) {
        let n = self.rows(split);
        let nearest = (0..n).filter_map(|i| self.gap_at(i, split).map(|g| (i, g))).min_by_key(|&(i, _)| i.abs_diff(self.cursor));
        let Some((row, gap)) = nearest else { return };
        let e = match row.cmp(&self.cursor) {
            std::cmp::Ordering::Equal => Expand::All(gap),
            std::cmp::Ordering::Less => Expand::Up(gap),
            std::cmp::Ordering::Greater => Expand::Down(gap),
        };
        self.expand(e, split);
    }

    /// `E`: whole file ↔ default context.
    pub fn toggle_whole_file(&mut self, split: bool) {
        let e = if self.view.is_fully_expanded() { Expand::Collapse } else { Expand::WholeFile };
        self.expand(e, split);
    }

    /// Moves the cursor to the next (`dir > 0`) or previous hunk start and scrolls it near the top.
    pub fn next_hunk(&mut self, split: bool, dir: i32) {
        let off = self.offset();
        let starts: Vec<usize> = if split { self.view.split_hunk_starts() } else { self.view.hunk_starts() }.into_iter().map(|s| s + off).collect();
        let target = if dir > 0 {
            starts.iter().copied().find(|&s| s > self.cursor).or(starts.last().copied())
        } else {
            starts.iter().rev().copied().find(|&s| s < self.cursor).or(starts.first().copied())
        };
        if let Some(t) = target {
            self.cursor = t;
            self.scroll = t.saturating_sub(3);
        }
    }

    /// Re-expresses the cursor in the other layout's row space (unified ↔ split).
    pub fn remap_cursor(&mut self, from_split: bool, to_split: bool) {
        if from_split == to_split {
            return;
        }
        if let Some(a) = self.anchor(self.cursor, from_split) {
            self.cursor = self.find(a, to_split);
        }
        self.clamp(to_split);
    }

    /// Applies split-view pairing for every change block whose intraline is computed. Pairing
    /// moves split rows, so in split view the cursor follows its content (`split`: the active
    /// layout) and keeps its screen offset.
    pub fn apply_ready_pairing(&mut self, split: bool) {
        if self.paired_all {
            return;
        }
        let d = self.diff.clone();
        let n = d.changes.len();
        let ready: Vec<usize> = (0..n).filter(|&c| d.intraline_ready(c).is_some()).collect();
        self.paired_all = ready.len() == n;
        if ready.is_empty() {
            return;
        }
        let anchor = self.anchor(self.cursor, split).ok_or(self.gap_at(self.cursor, split));
        let offset = self.cursor.saturating_sub(self.scroll);
        self.view.set_pairings(ready.iter().filter_map(|&c| d.intraline_ready(c).map(|h| (c, h.pair_of_del.as_slice()))));
        if split {
            let found = match anchor {
                Ok(a) => Some(self.find(a, true)),
                Err(Some(gap)) => (0..self.rows(true)).find(|&i| self.gap_at(i, true) == Some(gap)),
                Err(None) => None,
            };
            if let Some(i) = found {
                self.cursor = i;
                self.scroll = i.saturating_sub(offset);
            }
            self.clamp(true);
        }
    }

    /// Screen lines row `i` takes (1 unless wrapping).
    pub fn row_lines(&self, i: usize, split: bool, wrap: Option<Wrap>) -> usize {
        let Some(w) = wrap else { return 1 };
        let (mut scratch, mut starts) = (Vec::new(), Vec::new());
        let mut lines = |text: &gitty_core::diff::text::Text, line: Option<u32>, width: u32| {
            line.map_or(1, |l| wrapped_lines(text.line(l), width, w.tab, &mut scratch, &mut starts))
        };
        let d = &self.diff;
        match self.vrow(i, split) {
            Some(VRow::Row(r)) => match r {
                Row::Context { new, .. } | Row::Add { new, .. } => lines(&d.new, Some(new), w.left),
                Row::Del { old, .. } => lines(&d.old, Some(old), w.left),
                Row::Gap { .. } => 1,
            },
            Some(VRow::Split(r)) => match r {
                SplitRow::Context { old, new } => lines(&d.old, Some(old), w.left).max(lines(&d.new, Some(new), w.right)),
                SplitRow::Change { old, new, .. } => lines(&d.old, old, w.left).max(lines(&d.new, new, w.right)),
                SplitRow::Gap { .. } => 1,
            },
            _ => 1,
        }
    }

    /// How many rows a wrapped page of `lines` screen lines moves from `from` in direction
    /// `dir`: as many as fit together on screen, at least one.
    pub fn rows_in_lines(&self, from: usize, lines: usize, dir: i64, split: bool, wrap: Option<Wrap>) -> usize {
        let n = self.rows(split);
        let (mut used, mut k) = (0, 0);
        loop {
            let next = if dir < 0 { from.checked_sub(k + 1) } else { Some(from + k + 1).filter(|&i| i < n) };
            let Some(i) = next else { break };
            used += self.row_lines(i, split, wrap);
            if used > lines && k > 0 {
                break;
            }
            k += 1;
            if used >= lines {
                break;
            }
        }
        k.max(1)
    }

    /// Keeps all of the cursor's row inside a `height`-line window starting at row `scroll`.
    pub fn ensure_visible(&mut self, height: usize, split: bool, wrap: Option<Wrap>) {
        let h = height.max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
            return;
        }
        // the lowest scroll that still shows the cursor's row whole (or from its top, if taller)
        let mut used = 0;
        let mut top = self.cursor;
        loop {
            used += self.row_lines(top, split, wrap);
            if used > h || top <= self.scroll {
                break;
            }
            top -= 1;
        }
        if used > h && top < self.cursor {
            top += 1;
        }
        self.scroll = self.scroll.max(top);
    }
}

/// A fixed line above the diff rows (rename, mode change, line endings, bidi warning).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub warn: bool,
}

fn eol_name(e: gitty_core::diff::text::EolStyle) -> &'static str {
    use gitty_core::diff::text::EolStyle;
    match e {
        EolStyle::None => "none",
        EolStyle::Lf => "LF",
        EolStyle::Crlf => "CRLF",
        EolStyle::Mixed => "mixed",
    }
}

impl DiffState {
    pub fn banners(&self) -> Vec<Banner> {
        let d = &self.diff;
        let mut out = Vec::new();
        if let Some(old) = &d.old_path {
            out.push(Banner { text: format!("Renamed from {old}"), warn: false });
        }
        let (om, nm) = d.modes();
        if om != 0 && nm != 0 && om != nm {
            out.push(Banner { text: format!("Mode changed {om:o} → {nm:o}"), warn: false });
        }
        if let Some((a, b)) = d.eol_change {
            out.push(Banner { text: format!("Line endings changed {} → {}", eol_name(a), eol_name(b)), warn: false });
        }
        if d.bidi_warning {
            out.push(Banner { text: "⚠ Changed lines contain bidirectional Unicode control characters".into(), warn: true });
        }
        out
    }
}
