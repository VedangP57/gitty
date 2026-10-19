//! Copying text out of the Files viewer and the diff panes (the mouse is captured, so the
//! terminal's own selection does not reach them): a drag selects whole lines, release copies
//! them through OSC 52, and `y` / `Y` copy by keyboard.
//!
//! A selection is a range of lines (rows of the diff, lines of the viewed file), not a range of
//! screen cells: a wrapped line copies as the one line it is, horizontal scroll does not cut it,
//! and what is copied is the file's own bytes (tabs stay tabs). Line numbers, the `+`/`-`
//! markers, hunk headers, gap rows and the other half of a split view are never part of it.
//!
//! The selection stays (highlighted) after the release, through scrolling, until Esc, the next
//! click, or a change to what the pane shows (another file, another diff, an expansion, split ↔
//! unified, a reload with a different line count) makes it stale; a stale one is dropped.
//!
//! A masked secret file in the viewer never has content to copy: a drag on it does not start a
//! selection, and `y` says to reveal it first.

use std::path::PathBuf;

use crossterm::event::KeyModifiers;
use gitty_core::diff::text::Text;
use gitty_core::diff::view::{Row, SplitRow};
use ratatui::layout::{Position, Rect};

use super::diffstate::{DiffState, VRow};
use super::files::Viewing;
use super::{App, Tab, Toast};
use crate::msg::{DiffKey, FileView};

/// The most a copy puts on the clipboard; longer text is cut (at a character boundary) and the
/// toast says so. Some terminals and multiplexers drop larger OSC 52 payloads.
pub const COPY_CAP: usize = 1024 * 1024;

/// Which lines of a diff row a selection takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSide {
    /// Unified view: each row's one line.
    Unified,
    /// Split view, left half.
    Old,
    /// Split view, right half.
    New,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelPane {
    /// The Files viewer; lines of the shown file.
    Viewer,
    /// The History or Changes diff; rows of the diff in the current layout.
    Diff(DiffSide),
}

/// What the selection was made over: when it no longer matches, the selection is stale.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Stamp {
    Viewer { path: PathBuf, lines: usize },
    Diff { tab: Tab, key: DiffKey, split: bool, rows: usize },
}

#[derive(Debug, Clone)]
pub struct TextSel {
    pub pane: SelPane,
    pub anchor: usize,
    pub head: usize,
    /// The button is still down.
    dragging: bool,
    /// The pointer moved since the press: a press and release in place is a click, not a copy.
    moved: bool,
    stamp: Stamp,
}

/// `s` cut to at most [`COPY_CAP`] bytes at a character boundary; true when it was cut.
pub fn cap(mut s: String) -> (String, bool) {
    if s.len() <= COPY_CAP {
        return (s, false);
    }
    let mut end = COPY_CAP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

/// Lines `lo..=hi` of a file, joined by `\n` (no terminator after the last; a final `\r` of
/// each line dropped, as the viewer shows it).
pub fn text_lines(text: &Text, lo: usize, hi: usize) -> (String, usize) {
    let n = text.len() as usize;
    let mut out = String::new();
    let mut count = 0;
    for i in lo..=hi.min(n.saturating_sub(1)) {
        if i >= n {
            break;
        }
        if count > 0 {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(text.line(i as u32)));
        count += 1;
        if out.len() > COPY_CAP {
            break;
        }
    }
    (out, count)
}

/// The line one diff row shows on `side`: (old?, index). None for headers, gaps and fillers.
pub fn row_line(v: &VRow, side: DiffSide) -> Option<(bool, u32)> {
    match v {
        VRow::Header(_) => None,
        VRow::Row(r) => match r {
            Row::Gap { .. } => None,
            Row::Context { new, .. } | Row::Add { new, .. } => Some((false, *new)),
            Row::Del { old, .. } => Some((true, *old)),
        },
        VRow::Split(r) => match r {
            SplitRow::Gap { .. } => None,
            SplitRow::Context { old, new } => Some(if side == DiffSide::Old { (true, *old) } else { (false, *new) }),
            SplitRow::Change { old, new, .. } => match side {
                DiffSide::Old => old.map(|o| (true, o)),
                _ => new.map(|n| (false, n)),
            },
        },
    }
}

/// The text of diff rows `lo..=hi` on `side`, one line per row that has one, joined by `\n`;
/// without line numbers or `+`/`-` markers. (text, lines).
pub fn diff_lines(d: &DiffState, split: bool, side: DiffSide, lo: usize, hi: usize) -> (String, usize) {
    let mut out = String::new();
    let mut count = 0;
    for i in lo..=hi {
        let Some(v) = d.vrow(i, split) else { break };
        let Some((old, n)) = row_line(&v, side) else { continue };
        let text = if old { &d.diff.old } else { &d.diff.new };
        if count > 0 {
            out.push('\n');
        }
        out.push_str(&String::from_utf8_lossy(text.line(n)));
        count += 1;
        if out.len() > COPY_CAP {
            break;
        }
    }
    (out, count)
}

/// The side a split row's keyboard copy takes: the new line when the row has one, else the old.
pub fn split_side_of(d: &DiffState, row: usize) -> DiffSide {
    match d.vrow(row, true) {
        Some(VRow::Split(SplitRow::Change { new: None, old: Some(_), .. })) => DiffSide::Old,
        _ => DiffSide::New,
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

impl App {
    fn stamp_now(&self, pane: SelPane) -> Option<Stamp> {
        match pane {
            SelPane::Viewer => {
                if self.tab != Tab::Files || self.files_tab.masked() {
                    return None;
                }
                let lines = self.view_lines();
                (lines > 0).then(|| Stamp::Viewer { path: self.files_tab.shown.clone().unwrap_or_default(), lines })
            }
            SelPane::Diff(_) => {
                if self.tab == Tab::Files || self.conflict_active() {
                    return None;
                }
                let d = self.diff.as_ref().filter(|d| d.diff.is_text())?;
                let split = self.split_active();
                Some(Stamp::Diff { tab: self.tab, key: d.key.clone(), split, rows: d.rows(split) })
            }
        }
    }

    /// The selection on screen as (pane, first, last), when there is one and it is not stale.
    pub fn text_selection(&self) -> Option<(SelPane, usize, usize)> {
        let s = self.text_sel.as_ref().filter(|s| s.moved)?;
        if self.stamp_now(s.pane).as_ref() != Some(&s.stamp) {
            return None;
        }
        Some((s.pane, s.anchor.min(s.head), s.anchor.max(s.head)))
    }

    /// Esc: drops the selection; true when there was one to drop.
    pub(super) fn clear_text_selection(&mut self) -> bool {
        let had = self.text_selection().is_some();
        self.text_sel = None;
        had
    }

    /// A press in the viewer's or a diff's text: a selection may start here (it exists once the
    /// pointer moves). Not on the Changes line-number gutters (a drag there stages lines), not
    /// on a masked secret, not on a gap row (its click expands the diff).
    pub(super) fn begin_text_sel(&mut self, x: u16, y: u16, mods: KeyModifiers) {
        self.text_sel = None;
        if mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL) {
            return;
        }
        let hit = |r: Option<Rect>| r.filter(|r| r.contains(Position { x, y }));
        let (pane, row) = if self.tab == Tab::Files {
            let Some(r) = hit(self.hits.viewer_rows) else { return };
            let line = self.hits.viewer_first + usize::from(y - r.y);
            if line >= self.view_lines() {
                return;
            }
            (SelPane::Viewer, line)
        } else {
            let Some(r) = hit(self.hits.diff_rows) else { return };
            let Some(&i) = self.hits.diff_lines.get(usize::from(y - r.y)) else { return };
            let (og, ng) = (self.hits.diff_old_gutter, self.hits.diff_new_gutter);
            if self.tab == Tab::Changes && ((og.0..og.0 + og.1).contains(&x) || (ng.0..ng.0 + ng.1).contains(&x)) {
                return;
            }
            let split = self.split_active();
            let Some(d) = self.diff.as_ref() else { return };
            if matches!(d.vrow(i, split), None | Some(VRow::Row(Row::Gap { .. }) | VRow::Split(SplitRow::Gap { .. }))) {
                return;
            }
            let side = if !split {
                DiffSide::Unified
            } else if x < r.x + r.width.saturating_sub(1) / 2 {
                DiffSide::Old
            } else {
                DiffSide::New
            };
            (SelPane::Diff(side), i)
        };
        let Some(stamp) = self.stamp_now(pane) else { return };
        self.text_sel = Some(TextSel { pane, anchor: row, head: row, dragging: true, moved: false, stamp });
    }

    pub(super) fn text_sel_dragging(&self) -> bool {
        self.text_sel.as_ref().is_some_and(|s| s.dragging)
    }

    /// Moves the selection's end to the line under `y`; past the pane's top or bottom edge the
    /// pane scrolls a line per drag event and the selection follows.
    pub(super) fn drag_text_sel(&mut self, y: u16) {
        let Some(pane) = self.text_sel.as_ref().map(|s| s.pane) else { return };
        let head = match pane {
            SelPane::Viewer => {
                let Some(r) = self.hits.viewer_rows else { return };
                let n = self.view_lines();
                if n == 0 {
                    return;
                }
                let cap = usize::from(r.height).max(1);
                let f = &mut self.files_tab;
                if y < r.y {
                    f.vscroll = f.vscroll.saturating_sub(1);
                    f.vscroll
                } else if y >= r.bottom() {
                    f.vscroll = (f.vscroll + 1).min(n.saturating_sub(cap));
                    (f.vscroll + cap - 1).min(n - 1)
                } else {
                    (f.vscroll + usize::from(y - r.y)).min(n - 1)
                }
            }
            SelPane::Diff(_) => {
                let Some(r) = self.hits.diff_rows else { return };
                let split = self.split_active();
                let (cap, wrap) = (self.diff_capacity(), self.diff_wrap());
                let row = usize::from(y.clamp(r.y, r.bottom().saturating_sub(1)) - r.y);
                let hit = self.hits.diff_lines.get(row).copied();
                let cur = self.text_sel.as_ref().map_or(0, |s| s.head);
                let Some(d) = self.diff.as_mut() else { return };
                let last = d.rows(split).saturating_sub(1);
                // the hit regions are from the last frame, which may be scrolled since: never step back
                let head = if y >= r.bottom() {
                    (hit.map_or(cur, |i| i.max(cur)) + 1).min(last)
                } else if y < r.y {
                    hit.map_or(cur, |i| i.min(cur)).saturating_sub(1)
                } else if let Some(i) = hit {
                    i
                } else {
                    return;
                };
                // scroll the head into view without moving the cursor
                let keep = d.cursor;
                d.cursor = head;
                d.ensure_visible(cap, split, wrap);
                d.cursor = keep;
                head
            }
        };
        if let Some(s) = self.text_sel.as_mut() {
            s.head = head;
            s.moved = true;
        }
    }

    /// Release: a selection that was dragged is copied; a press without a drag leaves none.
    pub(super) fn end_text_sel(&mut self) {
        let Some(s) = self.text_sel.as_mut().filter(|s| s.dragging) else { return };
        s.dragging = false;
        if s.moved {
            self.copy_selection();
        } else {
            self.text_sel = None;
        }
    }

    /// Copies the selection; false when there is none.
    fn copy_selection(&mut self) -> bool {
        let Some((pane, lo, hi)) = self.text_selection() else { return false };
        let (text, n) = match pane {
            SelPane::Viewer => {
                if self.files_tab.masked() {
                    return self.refuse_secret();
                }
                match &self.files_tab.viewing {
                    Viewing::Ready(FileView::Text { text, .. }) => text_lines(text, lo, hi),
                    _ => return false,
                }
            }
            SelPane::Diff(side) => {
                let split = self.split_active();
                match self.diff.as_ref() {
                    Some(d) => diff_lines(d, split, side, lo, hi),
                    None => return false,
                }
            }
        };
        self.copy_lines(text, n);
        true
    }

    fn refuse_secret(&mut self) -> bool {
        self.toast = Some(Toast { what: "Reveal the file first (v): a hidden secret is not copied".into(), detail: String::new(), error: false });
        true
    }

    fn nothing_to_copy(&mut self, why: &str) {
        self.toast = Some(Toast { what: format!("Nothing to copy: {why}"), detail: String::new(), error: false });
    }

    /// Puts `n` lines of text on the clipboard, saying how many (never what: it may be a file's
    /// content).
    fn copy_lines(&mut self, text: String, n: usize) {
        if n == 0 {
            return self.nothing_to_copy("no lines with text selected");
        }
        let (text, cut) = cap(text);
        let what = if cut { format!("Copied the first 1 MiB of {n} line{} (truncated)", plural(n)) } else { format!("Copied {n} line{}", plural(n)) };
        self.copy_as(&text, what);
    }

    /// `y` in the Files viewer or a diff: the selection; without one, the whole file in the
    /// viewer, or in a diff the line at the cursor (Changes: the `v` range, if one is open).
    pub(super) fn copy_key(&mut self) {
        if self.tab == Tab::Files {
            if self.files_tab.masked() {
                self.refuse_secret();
                return;
            }
            if self.copy_selection() {
                return;
            }
            return match &self.files_tab.viewing {
                Viewing::Ready(FileView::Text { text, .. }) => {
                    let n = text.len() as usize;
                    if n == 0 {
                        return self.nothing_to_copy("the file is empty");
                    }
                    let (s, cut) = cap(String::from_utf8_lossy(text.bytes()).into_owned());
                    let what = if cut { "Copied the first 1 MiB of the file (truncated)".to_string() } else { format!("Copied the file ({n} line{})", plural(n)) };
                    self.copy_as(&s, what);
                }
                Viewing::Ready(FileView::Binary { .. }) => self.nothing_to_copy("binary file"),
                Viewing::Ready(FileView::TooLarge { .. }) => self.nothing_to_copy("the file is too large to show"),
                Viewing::Ready(FileView::Masked) => {
                    self.refuse_secret();
                }
                Viewing::Ready(_) | Viewing::Failed(_) => self.nothing_to_copy("not a text file"),
                Viewing::Nothing | Viewing::Loading => self.nothing_to_copy("no file shown"),
            };
        }
        if self.conflict_active() {
            return self.nothing_to_copy("the conflict view has no selection (e opens the file)");
        }
        if self.copy_selection() {
            return;
        }
        let split = self.split_active();
        let visual = if self.tab == Tab::Changes { self.changes.visual } else { None };
        let Some(d) = self.diff.as_ref().filter(|d| d.diff.is_text()) else { return self.nothing_to_copy("no text diff shown") };
        let side = if split { split_side_of(d, d.cursor) } else { DiffSide::Unified };
        let (lo, hi) = match visual {
            Some(a) => (a.min(d.cursor), a.max(d.cursor)),
            None => (d.cursor, d.cursor),
        };
        let (text, n) = diff_lines(d, split, side, lo, hi);
        self.copy_lines(text, n);
    }

    /// `Y` in the Files viewer or a diff: the shown file's path, relative to the repository.
    pub(super) fn copy_path_key(&mut self) {
        let path = match self.tab {
            Tab::Files => self.files_tab.shown.as_ref().map(|p| p.to_string_lossy().into_owned()),
            Tab::Changes => self.changes.selected().map(|e| e.path.clone()),
            Tab::History => self.current_file().map(|f| f.path.clone()),
        };
        match path {
            Some(p) => self.copy_as(&p, format!("Copied {p}")),
            None => self.nothing_to_copy("no file shown"),
        }
    }

    /// `y` / `Y` in the Files tree: the selected entry's path, relative to the repository or
    /// absolute.
    pub(super) fn copy_entry_path(&mut self, absolute: bool) {
        let Some(row) = self.files_tab.selected().filter(|r| !matches!(r.kind, super::files::RowKind::Note { .. })) else {
            return self.nothing_to_copy("no entry selected");
        };
        let rel = row.path.clone();
        let p = match (&self.workdir, absolute) {
            (Some(root), true) => root.join(&rel),
            _ => rel,
        };
        let p = p.to_string_lossy().into_owned();
        self.copy_as(&p, format!("Copied {p}"));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gitty_core::diff::{DiffOptions, FileDiff};

    use super::*;

    fn state(old: &str, new: &str) -> DiffState {
        let fd = FileDiff::from_bytes("f.txt", None, old.as_bytes().to_vec(), new.as_bytes().to_vec(), 0o100644, 0o100644, DiffOptions::default());
        let key = DiffKey { old: None, new: None, path: "f.txt".into(), old_path: None, old_mode: 0, new_mode: 0, opts: DiffOptions::default(), force_text: false };
        DiffState::new(key, Arc::new(fd))
    }

    fn all(d: &DiffState, split: bool, side: DiffSide) -> String {
        diff_lines(d, split, side, 0, d.rows(split).saturating_sub(1)).0
    }

    #[test]
    fn unified_rows_copy_their_lines_without_markers_or_numbers() {
        let d = state("a\n\tb\nc\n", "a\n\tB 日本\nc\n");
        assert_eq!(all(&d, false, DiffSide::Unified), "a\n\tb\n\tB 日本\nc", "the deleted and the added line, tabs kept");
        let (_, n) = diff_lines(&d, false, DiffSide::Unified, 0, d.rows(false) - 1);
        assert_eq!(n, 4, "the synthesised hunk header is not a line");
    }

    #[test]
    fn split_rows_copy_only_the_dragged_half() {
        let d = state("a\nold one\nc\n", "a\nnew one\nnew two\nc\n");
        assert_eq!(all(&d, true, DiffSide::Old), "a\nold one\nc");
        assert_eq!(all(&d, true, DiffSide::New), "a\nnew one\nnew two\nc");
    }

    #[test]
    fn gaps_are_skipped_and_a_missing_final_newline_adds_nothing() {
        let old: String = (0..30).map(|i| format!("l{i}\n")).collect();
        let new = old.replace("l2\n", "two\n").replace("l29\n", "end");
        let d = state(&old, &new);
        let text = all(&d, false, DiffSide::Unified);
        assert!(!text.contains('⋯') && !text.contains("@@"), "{text}");
        assert!(text.ends_with("l29\nend"), "the last line has no newline after it: {text:?}");
        assert!(text.starts_with("l0\nl1\nl2\ntwo\n"), "{text:?}");
    }

    #[test]
    fn empty_and_crlf_lines() {
        let d = state("x\n", "a\r\n\r\nb\r\n");
        assert_eq!(all(&d, false, DiffSide::Unified), "x\na\n\nb", "CR dropped as the diff shows it, the empty line kept");
    }

    #[test]
    fn a_long_line_copies_whole_whatever_the_horizontal_scroll() {
        let long = "x".repeat(500) + "END";
        let mut d = state("a\n", &format!("{long}\n"));
        d.hscroll = 300;
        d.visible = 40;
        assert!(all(&d, false, DiffSide::Unified).contains(&long));
    }

    #[test]
    fn file_lines_and_ranges() {
        let t = Text::new(b"one\n\ttwo\n\n\xe6\x97\xa5\xe6\x9c\xac\r\nlast".to_vec());
        assert_eq!(text_lines(&t, 0, 4), ("one\n\ttwo\n\n日本\nlast".to_string(), 5));
        assert_eq!(text_lines(&t, 3, 99), ("日本\nlast".to_string(), 2), "clamped to the file");
        assert_eq!(text_lines(&t, 2, 2), (String::new(), 1), "an empty line is one line of nothing");
        assert_eq!(text_lines(&Text::new(Vec::new()), 0, 3), (String::new(), 0));
        assert_eq!(text_lines(&Text::new(b"a\xffb\n".to_vec()), 0, 0).0, "a\u{fffd}b", "invalid UTF-8 copies as U+FFFD");
    }

    #[test]
    fn the_cap_cuts_at_a_character_boundary() {
        let (s, cut) = cap("ab".into());
        assert_eq!((s.as_str(), cut), ("ab", false));
        let (s, cut) = cap("é".repeat(COPY_CAP / 2) + "x");
        assert!(cut && s.len() == COPY_CAP, "{}", s.len());
        let (s, cut) = cap("x".to_string() + &"é".repeat(COPY_CAP / 2));
        assert!(cut && s.len() == COPY_CAP - 1, "a two-byte character straddling the cap is left out: {}", s.len());
    }
}
