//! Renderable rows over a diff's ops, with expandable context gaps.
//!
//! Every `Equal` op is a gap: `top` lines shown after the previous change, `bottom` lines shown
//! before the next change, and a single `Row::Gap` standing for the hidden middle. Rows are not
//! materialised; a prefix-summed segment list maps a row index to its content in O(log n).

use std::ops::Range;

use super::ops::Op;
use super::text::Text;

pub const CONTEXT: u32 = 3;
pub const EXPAND_STEP: u32 = 20;
const FUNCNAME_SCAN: u32 = 5000;
const FUNCNAME_MAX: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// Collapsed context standing for `hidden` lines, followed by the hunk `header` describes.
    Gap { gap: usize, hidden: u32, header: String, can_up: bool, can_down: bool },
    /// 0-based line indices into the old and new texts.
    Context { old: u32, new: u32 },
    Del { old: u32, change: usize },
    Add { new: u32, change: usize },
}

/// A row of the side-by-side view: context and gaps span both sides; change rows hold an
/// optional old line (left) and an optional new line (right).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitRow {
    Gap { gap: usize, hidden: u32, header: String, can_up: bool, can_down: bool },
    Context { old: u32, new: u32 },
    Change { old: Option<u32>, new: Option<u32>, change: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expand {
    /// Reveal more lines above the following hunk.
    Up(usize),
    /// Reveal more lines below the previous hunk.
    Down(usize),
    All(usize),
    WholeFile,
    Collapse,
}

#[derive(Debug, Clone)]
struct Gap {
    old: u32,
    new: u32,
    len: u32,
    top: u32,
    bottom: u32,
    leading: bool,
    trailing: bool,
}

impl Gap {
    fn hidden(&self) -> u32 {
        self.len - self.top - self.bottom
    }
    fn reset(&mut self) {
        self.top = if self.leading { 0 } else { CONTEXT };
        self.bottom = if self.trailing { 0 } else { CONTEXT };
        if self.top + self.bottom >= self.len {
            // too short to hide anything: show it all (an identical file stays one collapsed gap)
            (self.top, self.bottom) = match (self.leading, self.trailing) {
                (true, true) => (0, 0),
                (true, false) => (0, self.len),
                _ => (self.len, 0),
            };
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Seg {
    Ctx { old: u32, new: u32 },
    GapRow { gap: usize },
    Del { old: u32, change: usize },
    Add { new: u32, change: usize },
}

#[derive(Debug, Clone, Copy)]
struct Segment {
    start: usize,
    len: u32,
    seg: Seg,
}

enum Item {
    Gap(usize),
    Change(usize),
}

#[derive(Debug, Clone, Copy)]
enum SplitSeg {
    Ctx { old: u32, new: u32 },
    GapRow { gap: usize },
    Change { change: usize },
}

#[derive(Debug, Clone, Copy)]
struct SplitSegment {
    start: usize,
    seg: SplitSeg,
}

pub struct DiffView {
    split_segments: Vec<SplitSegment>,
    split_total: usize,
    /// Per change: (old line, new line) rows, absolute 0-based indices.
    split_change_rows: Vec<Vec<(Option<u32>, Option<u32>)>>,
    gaps: Vec<Gap>,
    changes: Vec<(Range<u32>, Range<u32>)>,
    items: Vec<Item>,
    segments: Vec<Segment>,
    total: usize,
    headers: Vec<String>,
    funcnames: Vec<String>,
}

impl DiffView {
    pub fn new(ops: &[Op], old: &Text, _new: &Text) -> DiffView {
        let n_changes = ops.iter().filter(|o| matches!(o, Op::Change { .. })).count();
        let mut gaps = Vec::new();
        let mut changes = Vec::new();
        let mut items = Vec::new();
        for op in ops {
            match op {
                Op::Equal { old, new, len } => {
                    let mut g = Gap {
                        old: *old,
                        new: *new,
                        len: *len,
                        top: 0,
                        bottom: 0,
                        leading: changes.is_empty(),
                        trailing: changes.len() == n_changes,
                    };
                    g.reset();
                    items.push(Item::Gap(gaps.len()));
                    gaps.push(g);
                }
                Op::Change { old, new } => {
                    items.push(Item::Change(changes.len()));
                    changes.push((old.clone(), new.clone()));
                }
            }
        }
        // funcname for the hunk following each gap: nearest line above its first shown line
        // can change with expansion, so compute lazily per rebuild from cached Text-free data
        // by scanning the old text once per gap now (bounded).
        let funcnames = gaps.iter().map(|_| String::new()).collect();
        let split_change_rows = changes.iter().map(|(o, n)| split_rows_for(o, n, None)).collect();
        let mut v = DiffView {
            split_segments: Vec::new(),
            split_total: 0,
            split_change_rows,
            gaps,
            changes,
            items,
            segments: Vec::new(),
            total: 0,
            headers: Vec::new(),
            funcnames,
        };
        v.compute_funcnames(old);
        v.rebuild();
        v
    }

    /// Funcname candidates are recomputed only at construction: expanding context shifts the
    /// hunk start, so we store the funcname for the *change* start (stable under expansion),
    /// mirroring what a reader expects ("which function is this change in").
    fn compute_funcnames(&mut self, old: &Text) {
        for (gi, g) in self.gaps.iter().enumerate() {
            if g.trailing {
                continue;
            }
            // first line of the following change in the old text
            let change_start = g.old + g.len;
            let mut i = change_start;
            let stop = change_start.saturating_sub(FUNCNAME_SCAN);
            while i > stop {
                i -= 1;
                let l = old.line(i);
                if l.first().is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_' || b == b'$') {
                    let s = String::from_utf8_lossy(l);
                    let s = s.trim_end();
                    let mut end = s.len().min(FUNCNAME_MAX);
                    while !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    self.funcnames[gi] = s[..end].to_string();
                    break;
                }
            }
        }
    }

    fn rebuild(&mut self) {
        self.segments.clear();
        let mut row = 0usize;
        let mut push = |segs: &mut Vec<Segment>, len: u32, seg: Seg| {
            if len > 0 {
                segs.push(Segment { start: row, len, seg });
                row += len as usize;
            }
        };
        for it in &self.items {
            match *it {
                Item::Gap(gi) => {
                    let g = &self.gaps[gi];
                    push(&mut self.segments, g.top, Seg::Ctx { old: g.old, new: g.new });
                    if g.hidden() > 0 {
                        push(&mut self.segments, 1, Seg::GapRow { gap: gi });
                    }
                    let skip = g.len - g.bottom;
                    push(&mut self.segments, g.bottom, Seg::Ctx { old: g.old + skip, new: g.new + skip });
                }
                Item::Change(ci) => {
                    let (o, n) = &self.changes[ci];
                    push(&mut self.segments, o.len() as u32, Seg::Del { old: o.start, change: ci });
                    push(&mut self.segments, n.len() as u32, Seg::Add { new: n.start, change: ci });
                }
            }
        }
        self.total = row;
        self.headers = vec![String::new(); self.gaps.len()];
        for (si, s) in self.segments.iter().enumerate() {
            let Seg::GapRow { gap } = s.seg else { continue };
            let g = &self.gaps[gap];
            if g.trailing {
                continue;
            }
            let (os, ns) = (g.old + g.len - g.bottom, g.new + g.len - g.bottom);
            let (mut ol, mut nl) = (0u32, 0u32);
            for t in &self.segments[si + 1..] {
                match t.seg {
                    Seg::GapRow { .. } => break,
                    Seg::Ctx { .. } => {
                        ol += t.len;
                        nl += t.len;
                    }
                    Seg::Del { .. } => ol += t.len,
                    Seg::Add { .. } => nl += t.len,
                }
            }
            let start = |s: u32, l: u32| if l == 0 { s } else { s + 1 };
            let mut h = format!("@@ -{},{} +{},{} @@", start(os, ol), ol, start(ns, nl), nl);
            if !self.funcnames[gap].is_empty() {
                h.push(' ');
                h.push_str(&self.funcnames[gap]);
            }
            self.headers[gap] = h;
        }
        self.rebuild_split();
    }

    fn rebuild_split(&mut self) {
        self.split_segments.clear();
        let mut row = 0usize;
        let mut push = |segs: &mut Vec<SplitSegment>, len: u32, seg: SplitSeg| {
            if len > 0 {
                segs.push(SplitSegment { start: row, seg });
                row += len as usize;
            }
        };
        for it in &self.items {
            match *it {
                Item::Gap(gi) => {
                    let g = &self.gaps[gi];
                    push(&mut self.split_segments, g.top, SplitSeg::Ctx { old: g.old, new: g.new });
                    if g.hidden() > 0 {
                        push(&mut self.split_segments, 1, SplitSeg::GapRow { gap: gi });
                    }
                    let skip = g.len - g.bottom;
                    push(&mut self.split_segments, g.bottom, SplitSeg::Ctx { old: g.old + skip, new: g.new + skip });
                }
                Item::Change(ci) => {
                    let n = self.split_change_rows[ci].len() as u32;
                    push(&mut self.split_segments, n, SplitSeg::Change { change: ci });
                }
            }
        }
        self.split_total = row;
    }

    /// Pairs deleted and added lines of change block `change` (relative indices, as produced by
    /// intraline pairing) so the split view puts modified lines side by side.
    pub fn set_pairing(&mut self, change: usize, pair_of_del: &[Option<u32>]) {
        let Some((o, n)) = self.changes.get(change) else { return };
        self.split_change_rows[change] = split_rows_for(o, n, Some(pair_of_del));
        self.rebuild_split();
    }

    pub fn split_row_count(&self) -> usize {
        self.split_total
    }

    pub fn split_row(&self, i: usize) -> SplitRow {
        let si = self.split_segments.partition_point(|s| s.start <= i) - 1;
        let s = &self.split_segments[si];
        let k = (i - s.start) as u32;
        match s.seg {
            SplitSeg::Ctx { old, new } => SplitRow::Context { old: old + k, new: new + k },
            SplitSeg::Change { change } => {
                let (old, new) = self.split_change_rows[change][k as usize];
                SplitRow::Change { old, new, change }
            }
            SplitSeg::GapRow { gap } => {
                let g = &self.gaps[gap];
                SplitRow::Gap {
                    gap,
                    hidden: g.hidden(),
                    header: self.headers[gap].clone(),
                    can_up: !g.trailing,
                    can_down: !g.leading,
                }
            }
        }
    }

    pub fn split_rows(&self, r: Range<usize>) -> Vec<SplitRow> {
        let end = r.end.min(self.split_total);
        (r.start.min(end)..end).map(|i| self.split_row(i)).collect()
    }

    /// Split-view row of each Gap row (for navigation).
    pub fn split_gap_rows(&self) -> Vec<usize> {
        self.split_segments.iter().filter(|s| matches!(s.seg, SplitSeg::GapRow { .. })).map(|s| s.start).collect()
    }

    /// First split row of each change block.
    pub fn split_hunk_starts(&self) -> Vec<usize> {
        self.split_segments.iter().filter(|s| matches!(s.seg, SplitSeg::Change { .. })).map(|s| s.start).collect()
    }

    pub fn row_count(&self) -> usize {
        self.total
    }

    pub fn row(&self, i: usize) -> Row {
        let si = self.segments.partition_point(|s| s.start <= i) - 1;
        let s = &self.segments[si];
        let k = (i - s.start) as u32;
        match s.seg {
            Seg::Ctx { old, new } => Row::Context { old: old + k, new: new + k },
            Seg::Del { old, change } => Row::Del { old: old + k, change },
            Seg::Add { new, change } => Row::Add { new: new + k, change },
            Seg::GapRow { gap } => {
                let g = &self.gaps[gap];
                Row::Gap {
                    gap,
                    hidden: g.hidden(),
                    header: self.headers[gap].clone(),
                    can_up: !g.trailing,
                    can_down: !g.leading,
                }
            }
        }
    }

    pub fn rows(&self, r: Range<usize>) -> Vec<Row> {
        let end = r.end.min(self.total);
        (r.start.min(end)..end).map(|i| self.row(i)).collect()
    }

    pub fn expand(&mut self, e: Expand) {
        let grow = |g: &mut Gap, up: bool| {
            let hidden = g.hidden();
            let step = if hidden <= EXPAND_STEP { hidden } else { EXPAND_STEP };
            if up {
                g.bottom += step;
            } else {
                g.top += step;
            }
        };
        match e {
            Expand::Up(gi) => {
                if let Some(g) = self.gaps.get_mut(gi) {
                    grow(g, true)
                }
            }
            Expand::Down(gi) => {
                if let Some(g) = self.gaps.get_mut(gi) {
                    grow(g, false)
                }
            }
            Expand::All(gi) => {
                if let Some(g) = self.gaps.get_mut(gi) {
                    g.top = g.len - g.bottom;
                }
            }
            Expand::WholeFile => {
                for g in &mut self.gaps {
                    g.top = g.len - g.bottom;
                }
            }
            Expand::Collapse => {
                for g in &mut self.gaps {
                    g.reset();
                }
            }
        }
        self.rebuild();
    }

    /// Whether any gap is hidden (used to toggle whole-file expansion).
    pub fn is_fully_expanded(&self) -> bool {
        self.gaps.iter().all(|g| g.hidden() == 0)
    }

    pub fn gap_rows(&self) -> Vec<usize> {
        self.segments.iter().filter(|s| matches!(s.seg, Seg::GapRow { .. })).map(|s| s.start).collect()
    }

    /// First row of each change block.
    pub fn hunk_starts(&self) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut last = usize::MAX;
        for s in &self.segments {
            if let Seg::Del { change, .. } | Seg::Add { change, .. } = s.seg {
                if change != last {
                    out.push(s.start);
                    last = change;
                }
            }
        }
        out
    }

    pub fn changes(&self) -> &[(Range<u32>, Range<u32>)] {
        &self.changes
    }
}

/// Rows for one change block. With pairing, paired lines share a row and unpaired lines get an
/// empty opposite cell, preserving order on both sides; without, lines are zipped by position.
fn split_rows_for(old: &Range<u32>, new: &Range<u32>, pairing: Option<&[Option<u32>]>) -> Vec<(Option<u32>, Option<u32>)> {
    let (d, a) = (old.len() as u32, new.len() as u32);
    let mut rows = Vec::with_capacity(d.max(a) as usize);
    match pairing {
        None => {
            for i in 0..d.max(a) {
                rows.push(((i < d).then(|| old.start + i), (i < a).then(|| new.start + i)));
            }
        }
        Some(p) => {
            let (mut di, mut ai) = (0u32, 0u32);
            for (pd, pa) in p.iter().enumerate().filter_map(|(i, x)| x.map(|j| (i as u32, j))) {
                if pd < di || pa < ai || pa >= a {
                    continue; // not monotone; ignore
                }
                while di < pd {
                    rows.push((Some(old.start + di), None));
                    di += 1;
                }
                while ai < pa {
                    rows.push((None, Some(new.start + ai)));
                    ai += 1;
                }
                rows.push((Some(old.start + pd), Some(new.start + pa)));
                di = pd + 1;
                ai = pa + 1;
            }
            while di < d {
                rows.push((Some(old.start + di), None));
                di += 1;
            }
            while ai < a {
                rows.push((None, Some(new.start + ai)));
                ai += 1;
            }
        }
    }
    rows
}
