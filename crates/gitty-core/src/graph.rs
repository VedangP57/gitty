//! The History list's commit graph: one row per commit, laid out the way lazygit lays out its
//! commit graph.
//!
//! The layout ([`Lanes::row`]) and the cells it draws ([`Row`]) are a port of lazygit's
//! `pkg/gui/presentation/graph` (graph.go and cell.go, box drawing symbols; MIT, Copyright (c)
//! 2018 Jesse Duffield). Each row is a set of pipes: lines from a commit to one of its parents
//! that start in the commit's row, continue through the rows between (moving left into room
//! that frees up) and terminate in the parent's row. A commit takes the column of the leftmost
//! pipe that ends in it (a new column on the right if none does), its first parent's pipe starts
//! there, and each further parent's pipe starts in the leftmost column free for it.
//!
//! What differs from lazygit: commits stream in from a topological walk ([`topo_walk`]) and the
//! layout state carries over from one chunk to the next; rows are stored compactly ([`Rows`])
//! and cut at [`MAX_LANES`] columns; there is no highlighting of the selected commit's lines
//! (the list draws its own selection); and a pipe's colour is not its commit author's but a
//! lane colour that stays with a branch: a commit's first-parent pipe keeps the colour of the
//! pipe it took its column from, and a merge's other pipes each open a new one.

use smallvec::SmallVec;

use crate::git_cli::{GitCli, Kind};
use crate::types::CommitId;

/// Lane colours cycle through this many indices. The UI maps them to a palette of this many
/// entries built from the theme's distinct hues (fewer distinct ones repeat in the cycle).
pub const COLOURS: u8 = 7;
/// Columns stored per row: no pane draws more, and a pathological history stays small.
const MAX_LANES: usize = 48;

// lazygit's `Cell`, packed: the edges its lines touch, its type and its two colours
const UP: u16 = 1;
const DOWN: u16 = 2;
const LEFT: u16 = 4;
const RIGHT: u16 = 8;
const TYPE: u16 = 4;
const TYPE_MASK: u16 = 0b11 << TYPE;
const COMMIT: u16 = 1 << TYPE;
const MERGE: u16 = 2 << TYPE;
/// The colour of the cell's own glyph.
const STYLE: u16 = 6;
const STYLE_MASK: u16 = 0b111 << STYLE;
/// The colour of the horizontal line after it (lazygit's `rightStyle`).
const RIGHT_STYLE: u16 = 9;
const RIGHT_STYLE_MASK: u16 = 0b111 << RIGHT_STYLE;
/// On a row's last stored cell: the row has more columns than are stored.
const MORE: u16 = 1 << 12;
/// `rightStyle` was set (lazygit's nil check).
const HAS_RIGHT_STYLE: u16 = 1 << 13;

pub const COMMIT_SYMBOL: char = '○';
pub const MERGE_SYMBOL: char = '◎';

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PipeKind {
    Terminates,
    Starts,
    Continues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Pipe {
    /// None: the start of the graph, above its first commit.
    from: Option<CommitId>,
    /// None: the empty tree, below a root commit.
    to: Option<CommitId>,
    from_pos: usize,
    to_pos: usize,
    kind: PipeKind,
    colour: u8,
}

impl Pipe {
    fn left(&self) -> usize {
        self.from_pos.min(self.to_pos)
    }
    fn right(&self) -> usize {
        self.from_pos.max(self.to_pos)
    }
}

/// The layout state of a topological walk: the previous row's pipes.
#[derive(Debug, Default)]
pub struct Lanes {
    pipes: Vec<Pipe>,
    started: bool,
    opened: u32,
}

/// A set of columns (lazygit's `set.New[int]`).
#[derive(Default)]
struct Spots(Vec<bool>);

impl Spots {
    fn add(&mut self, i: usize) {
        if i >= self.0.len() {
            self.0.resize(i + 1, false);
        }
        self.0[i] = true;
    }
    fn has(&self, i: usize) -> bool {
        self.0.get(i).copied().unwrap_or(false)
    }
}

impl Lanes {
    fn open(&mut self) -> u8 {
        let c = (self.opened % u32::from(COLOURS)) as u8;
        self.opened = self.opened.wrapping_add(1);
        c
    }

    /// Lays out the next commit of a topological walk and appends its row to `out`.
    pub fn row(&mut self, id: CommitId, parents: &[CommitId], out: &mut Rows) {
        if !self.started {
            self.started = true;
            let colour = self.open();
            self.pipes = vec![Pipe { from: None, to: Some(id), from_pos: 0, to_pos: 0, kind: PipeKind::Starts, colour }];
        }
        let prev = std::mem::take(&mut self.pipes);
        self.pipes = self.next_pipes(&prev, id, parents);
        render(&self.pipes, out);
    }

    /// lazygit's `getNextPipes`.
    fn next_pipes(&mut self, prev: &[Pipe], id: CommitId, parents: &[CommitId]) -> Vec<Pipe> {
        // a pipe that terminated in the previous row has no bearing on this one, nor does the
        // pipe from a root commit to the empty tree
        let current: Vec<Pipe> = prev.iter().filter(|p| p.kind != PipeKind::Terminates && p.to.is_some()).copied().collect();
        let max_pos = current.iter().map(|p| p.to_pos + 1).max().unwrap_or(0);
        let mut new: Vec<Pipe> = Vec::with_capacity(current.len() + parents.len());
        // a commit no pipe leads to (a branch tip) goes on the far right; one that has a
        // descendant goes under the first pipe leading to it
        let first = current.iter().find(|p| p.to == Some(id));
        let pos = first.map_or(max_pos, |p| p.to_pos);
        let own = match first {
            Some(p) => p.colour,
            None => self.open(),
        };
        // spots a current pipe ends on, and spots one starts on, ends on or passes through
        let mut taken = Spots::default();
        let mut traversed = Spots::default();
        new.push(Pipe { from: Some(id), to: parents.first().copied(), from_pos: pos, to_pos: pos, kind: PipeKind::Starts, colour: own });
        let mut traversed_by_continuing = Spots::default();
        for p in &current {
            if p.to != Some(id) {
                traversed_by_continuing.add(p.to_pos);
            }
        }
        let traverse = |taken: &mut Spots, traversed: &mut Spots, from: usize, to: usize| {
            for i in from.min(to)..=from.max(to) {
                traversed.add(i);
            }
            taken.add(to);
        };
        for p in &current {
            if p.to == Some(id) {
                // terminating here
                new.push(Pipe { from_pos: p.to_pos, to_pos: pos, kind: PipeKind::Terminates, ..*p });
                traverse(&mut taken, &mut traversed, p.to_pos, pos);
            } else if p.to_pos < pos {
                // continuing here
                let available = (0..).find(|&i| !traversed.has(i)).expect("a free spot");
                new.push(Pipe { from_pos: p.to_pos, to_pos: available, kind: PipeKind::Continues, ..*p });
                traverse(&mut taken, &mut traversed, p.to_pos, available);
            }
        }
        if parents.len() > 1 {
            for &parent in &parents[1..] {
                // a new pipe may not end on a taken spot, nor on one a continuing pipe traverses
                let available = (0..).find(|&i| !taken.has(i) && !traversed_by_continuing.has(i)).expect("a free spot");
                let colour = self.open();
                new.push(Pipe { from: Some(id), to: Some(parent), from_pos: pos, to_pos: available, kind: PipeKind::Starts, colour });
                taken.add(available);
            }
        }
        for p in &current {
            if p.to != Some(id) && p.to_pos > pos {
                // continuing on, potentially moving left to fill in a blank spot
                let mut last = p.to_pos;
                let mut i = p.to_pos;
                while i > pos {
                    if taken.has(i) || traversed.has(i) {
                        break;
                    }
                    last = i;
                    i -= 1;
                }
                new.push(Pipe { from_pos: p.to_pos, to_pos: last, kind: PipeKind::Continues, ..*p });
                traverse(&mut taken, &mut traversed, p.to_pos, last);
            }
        }
        new.sort_by_key(|p| (p.to_pos, p.kind));
        new
    }
}

/// lazygit's `Cell` while a row is drawn.
#[derive(Debug, Clone, Copy, Default)]
struct Cell {
    up: bool,
    down: bool,
    left: bool,
    right: bool,
    ty: u16,
    style: Option<u8>,
    right_style: Option<u8>,
}

impl Cell {
    fn set_up(&mut self, c: u8) {
        self.up = true;
        self.style = Some(c);
    }
    fn set_down(&mut self, c: u8) {
        self.down = true;
        self.style = Some(c);
    }
    fn set_left(&mut self, c: u8) {
        self.left = true;
        if !self.up && !self.down {
            // vertical trumps left
            self.style = Some(c);
        }
    }
    fn set_right(&mut self, c: u8, over: bool) {
        self.right = true;
        if self.right_style.is_none() || over {
            self.right_style = Some(c);
        }
    }
    fn pack(&self) -> u16 {
        let mut v = self.ty;
        for (on, bit) in [(self.up, UP), (self.down, DOWN), (self.left, LEFT), (self.right, RIGHT)] {
            if on {
                v |= bit;
            }
        }
        let style = self.style.or(self.right_style).unwrap_or(0);
        v |= u16::from(style) << STYLE;
        if let Some(r) = self.right_style {
            v |= HAS_RIGHT_STYLE | u16::from(r) << RIGHT_STYLE;
        }
        v
    }
}

/// lazygit's `renderPipeSet` (no selected commit), appending the row's cells to `out`.
fn render(pipes: &[Pipe], out: &mut Rows) {
    let mut max_pos = 0;
    let mut commit_pos = 0;
    let mut starts = 0;
    for p in pipes {
        match p.kind {
            PipeKind::Starts => {
                starts += 1;
                commit_pos = p.from_pos;
            }
            PipeKind::Terminates => commit_pos = p.to_pos,
            PipeKind::Continues => {}
        }
        max_pos = max_pos.max(p.right());
    }
    // cells past the store are not drawn; the commit's own is kept below
    let n = (max_pos + 1).min(MAX_LANES);
    let mut cells: SmallVec<[Cell; 16]> = SmallVec::from_elem(Cell::default(), n);
    let draw = |cells: &mut [Cell], p: &Pipe, over: bool| {
        let (left, right) = (p.left(), p.right());
        if left != right {
            for cell in cells.iter_mut().take(right.min(n)).skip(left + 1) {
                // lazygit's setHorizontal
                cell.set_left(p.colour);
                cell.set_right(p.colour, over);
            }
            if left < n {
                cells[left].set_right(p.colour, over);
            }
            if right < n {
                cells[right].set_left(p.colour);
            }
        }
        if matches!(p.kind, PipeKind::Starts | PipeKind::Continues) && p.to_pos < n {
            cells[p.to_pos].set_down(p.colour);
        }
        if matches!(p.kind, PipeKind::Terminates | PipeKind::Continues) && p.from_pos < n {
            cells[p.from_pos].set_up(p.colour);
        }
    };
    for p in pipes.iter().filter(|p| p.kind == PipeKind::Starts) {
        draw(&mut cells, p, true);
    }
    for p in pipes.iter().filter(|p| p.kind != PipeKind::Starts) {
        if p.kind == PipeKind::Terminates && p.from_pos == commit_pos && p.to_pos == commit_pos {
            // the line from above into the commit keeps the commit's own colour; the start of
            // the graph has no line
            if p.from.is_some() && commit_pos < n {
                cells[commit_pos].up = true;
            }
            continue;
        }
        draw(&mut cells, p, false);
    }
    // there is no line below a root commit
    for p in pipes {
        if p.kind == PipeKind::Starts && p.to.is_none() && p.to_pos < n {
            cells[p.to_pos].down = false;
        }
    }
    let ty = if starts > 1 { MERGE } else { COMMIT };
    let mut packed: SmallVec<[u16; 16]> = cells.iter().map(Cell::pack).collect();
    if commit_pos < n {
        packed[commit_pos] = (packed[commit_pos] & !TYPE_MASK) | ty;
    }
    if max_pos + 1 > n {
        packed[n - 1] |= MORE;
        if commit_pos >= n {
            // the commit's own column is cut off: its symbol takes the last stored one
            let own = pipes.iter().find(|p| p.kind == PipeKind::Starts).map_or(0, |p| p.colour);
            packed[n - 1] = ty | MORE | u16::from(own) << STYLE;
        }
    }
    out.push(&packed);
}

/// The rows of a graph, stored flat: two bytes per column pair.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rows {
    cells: Vec<u16>,
    /// End of each row in `cells`.
    ends: Vec<u32>,
}

impl Rows {
    fn push(&mut self, cells: &[u16]) {
        self.cells.extend_from_slice(cells);
        self.ends.push(self.cells.len() as u32);
    }
    /// Moves `other`'s rows to the end of these.
    pub fn append(&mut self, other: &mut Rows) {
        let base = self.cells.len() as u32;
        self.ends.extend(other.ends.drain(..).map(|e| e + base));
        self.cells.append(&mut other.cells);
    }
    pub fn len(&self) -> usize {
        self.ends.len()
    }
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }
    pub fn get(&self, i: usize) -> Option<Row<'_>> {
        let end = *self.ends.get(i)? as usize;
        let start = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        Some(Row(&self.cells[start..end]))
    }
    /// Heap bytes held.
    pub fn bytes(&self) -> usize {
        self.cells.capacity() * 2 + self.ends.capacity() * 4
    }
}

/// One commit's row.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a>(&'a [u16]);

/// lazygit's `getBoxDrawingChars`: a cell's glyph and the character after it.
fn box_chars(cell: u16) -> (char, char) {
    let (u, d, l, r) = (cell & UP != 0, cell & DOWN != 0, cell & LEFT != 0, cell & RIGHT != 0);
    match (u, d, l, r) {
        (true, true, true, true) => ('│', '─'),
        (true, true, true, false) => ('│', ' '),
        (true, true, false, true) => ('│', '─'),
        (true, true, false, false) => ('│', ' '),
        (true, false, true, true) => ('┴', '─'),
        (true, false, true, false) => ('╯', ' '),
        (true, false, false, true) => ('╰', '─'),
        (true, false, false, false) => ('╵', ' '),
        (false, true, true, true) => ('┬', '─'),
        (false, true, true, false) => ('╮', ' '),
        (false, true, false, true) => ('╭', '─'),
        (false, true, false, false) => ('╷', ' '),
        (false, false, true, true) => ('─', '─'),
        (false, false, true, false) => ('─', ' '),
        (false, false, false, true) => ('╶', '─'),
        (false, false, false, false) => (' ', ' '),
    }
}

impl Row<'_> {
    /// Terminal columns the row's cells take.
    pub fn columns(&self) -> usize {
        (self.0.len() * 2).saturating_sub(1)
    }
    /// More columns than are stored: the row is cut on the right.
    pub fn clipped(&self) -> bool {
        self.0.last().is_some_and(|c| c & MORE != 0)
    }
    /// Each column's glyph and colour index (None: blank), two per cell, the last cell's
    /// trailing blank left off.
    pub fn glyphs(&self) -> impl Iterator<Item = Option<(char, u8)>> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &cell)| {
            let (mut first, second) = box_chars(cell);
            match cell & TYPE_MASK {
                COMMIT => first = COMMIT_SYMBOL,
                MERGE => first = MERGE_SYMBOL,
                _ => {}
            }
            let style = ((cell & STYLE_MASK) >> STYLE) as u8;
            let right = if cell & HAS_RIGHT_STYLE != 0 { ((cell & RIGHT_STYLE_MASK) >> RIGHT_STYLE) as u8 } else { style };
            let own = (first != ' ').then_some((first, style));
            let after = (second != ' ').then_some((second, right));
            std::iter::once(own).chain((i + 1 < self.0.len()).then_some(after))
        })
    }
    /// The line under the row (a second line of text): every line that goes on down.
    pub fn filler(&self) -> impl Iterator<Item = Option<(char, u8)>> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &cell)| {
            let own = (cell & DOWN != 0).then_some(('│', ((cell & STYLE_MASK) >> STYLE) as u8));
            std::iter::once(own).chain((i + 1 < self.0.len()).then_some(None))
        })
    }
    /// The glyphs as text, blanks as spaces.
    pub fn text(&self) -> String {
        let s: String = self.glyphs().map(|g| g.map_or(' ', |g| g.0)).collect();
        s.trim_end().to_string()
    }
}

/// Streams the commits reachable from `tips` with their parents, in git's topological order
/// (`git rev-list --topo-order --parents`; tips on stdin, so tens of thousands of refs fit).
/// `commit` returns false to stop. Returns whether the walk reached its end.
pub fn topo_walk(cli: &GitCli, tips: &[CommitId], cancelled: &dyn Fn() -> bool, commit: &mut dyn FnMut(CommitId, &[CommitId]) -> bool) -> anyhow::Result<bool> {
    if tips.is_empty() {
        return Ok(true);
    }
    let mut input = Vec::with_capacity(tips.len() * 41);
    for t in tips {
        input.extend_from_slice(t.to_hex().as_bytes());
        input.push(b'\n');
    }
    let args = ["--no-optional-locks", "rev-list", "--topo-order", "--parents", "--stdin"];
    let mut parents: SmallVec<[CommitId; 2]> = SmallVec::new();
    cli.read_lines(cli.cmd(Kind::Read, &args), input, cancelled, &mut |l| {
        let mut ids = std::str::from_utf8(l).unwrap_or_default().split(' ').filter_map(|s| CommitId::from_hex(s.trim()));
        let Some(id) = ids.next() else { return true };
        parents.clear();
        parents.extend(ids);
        commit(id, &parents)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A commit id named by a short string, as lazygit's tests name them.
    fn id(name: &str) -> CommitId {
        let mut b = [0u8; 20];
        b[..name.len()].copy_from_slice(name.as_bytes());
        CommitId(b)
    }

    /// Lays out `(commit, parents)` in the given order.
    fn layout(history: &[(&str, &[&str])]) -> (Rows, Lanes) {
        let (mut lanes, mut rows) = (Lanes::default(), Rows::default());
        for (c, ps) in history {
            let ps: Vec<CommitId> = ps.iter().map(|p| id(p)).collect();
            lanes.row(id(c), &ps, &mut rows);
        }
        (rows, lanes)
    }

    /// lazygit's `TestRenderCommitGraph` output: each row as "<name> <graph>".
    fn graph(history: &[(&str, &[&str])]) -> String {
        let (rows, _) = layout(history);
        history.iter().enumerate().map(|(i, (c, _))| format!("{c} {}", rows.get(i).unwrap().text()).trim().to_string() + "\n").collect()
    }

    fn expected(s: &str) -> String {
        s.trim_start_matches('\n').lines().map(|l| l.trim().to_string() + "\n").collect()
    }

    // ---- lazygit's TestRenderCommitGraph, case by case ----

    #[test]
    fn with_some_merges() {
        let h: &[(&str, &[&str])] = &[
            ("1", &["2"]),
            ("2", &["3"]),
            ("3", &["4"]),
            ("4", &["5", "7"]),
            ("7", &["5"]),
            ("5", &["8"]),
            ("8", &["9"]),
            ("9", &["A", "B"]),
            ("B", &["D"]),
            ("D", &["D"]),
            ("A", &["E"]),
            ("E", &["F"]),
            ("F", &["D"]),
            ("D", &["G"]),
        ];
        assert_eq!(graph(h), expected("
            1 ○
            2 ○
            3 ○
            4 ◎─╮
            7 │ ○
            5 ○─╯
            8 ○
            9 ◎─╮
            B │ ○
            D │ ○
            A ○ │
            E ○ │
            F ○ │
            D ○─╯"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("4", &["3", "5"]), ("3", &["5"]), ("5", &["6"]), ("6", &["7"])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            4 │ ◎─╮
            3 ○─╯ │
            5 ○───╯
            6 ○"));
    }

    #[test]
    fn with_a_new_commit() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("4", &["3", "5"]), ("Z", &["Z"]), ("3", &["5"]), ("5", &["6"]), ("6", &["7"])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            4 │ ◎─╮
            Z │ │ │ ○
            3 ○─╯ │ │
            5 ○───╯ │
            6 ○ ╭───╯"));
    }

    #[test]
    fn with_a_root_commit_followed_by_an_unrelated_history() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &[]), ("A", &["B"]), ("B", &[])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ○
            A ○
            B ○"));
    }

    #[test]
    fn with_a_merge_of_an_unrelated_history() {
        let h: &[(&str, &[&str])] = &[("1", &["2", "A"]), ("2", &["3"]), ("A", &[]), ("3", &[])];
        assert_eq!(graph(h), expected("
            1 ◎─╮
            2 ○ │
            A │ ○
            3 ○"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left_and_continues() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("3", &["5", "4"]), ("5", &["7", "8"]), ("4", &["7"]), ("7", &["11"])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            3 ◎─│─╮
            5 ◎─│─│─╮
            4 │ ○─╯ │
            7 ○─╯ ╭─╯"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left_and_continues_2() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("3", &["5", "4"]), ("5", &["7", "8"]), ("7", &["4", "A"]), ("4", &["B"]), ("B", &["C"])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            3 ◎─│─╮
            5 ◎─│─│─╮
            7 ◎─│─│─│─╮
            4 ○─┴─╯ │ │
            B ○ ╭───╯ │"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left_and_continues_3() {
        let h: &[(&str, &[&str])] = &[("1", &["2", "3"]), ("3", &["2"]), ("2", &["4", "5"]), ("4", &["6", "7"]), ("6", &["8"])];
        assert_eq!(graph(h), expected("
            1 ◎─╮
            3 │ ○
            2 ◎─│
            4 ◎─│─╮
            6 ○ │ │"));
    }

    #[test]
    fn new_merge_path_fills_gap_before_continuing_path_on_right() {
        let h: &[(&str, &[&str])] = &[("1", &["2", "3", "4", "5"]), ("4", &["2"]), ("2", &["A"]), ("A", &["6", "B"]), ("B", &["C"])];
        assert_eq!(graph(h), expected("
            1 ◎─┬─┬─╮
            4 │ │ ○ │
            2 ○─│─╯ │
            A ◎─│─╮ │
            B │ │ ○ │"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left_and_continues_4() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("3", &["5", "4"]), ("5", &["7", "8"]), ("7", &["4", "A"]), ("4", &["B"]), ("B", &["C"]), ("C", &["D"])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            3 ◎─│─╮
            5 ◎─│─│─╮
            7 ◎─│─│─│─╮
            4 ○─┴─╯ │ │
            B ○ ╭───╯ │
            C ○ │ ╭───╯"));
    }

    #[test]
    fn with_a_path_that_has_room_to_move_to_the_left_and_continues_5() {
        let h: &[(&str, &[&str])] = &[
            ("1", &["2"]),
            ("2", &["3", "4"]),
            ("3", &["5", "4"]),
            ("5", &["7", "G"]),
            ("7", &["8", "A"]),
            ("8", &["4", "E"]),
            ("4", &["B"]),
            ("B", &["C"]),
            ("C", &["D"]),
            ("D", &["F"]),
        ];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            3 ◎─│─╮
            5 ◎─│─│─╮
            7 ◎─│─│─│─╮
            8 ◎─│─│─│─│─╮
            4 ○─┴─╯ │ │ │
            B ○ ╭───╯ │ │
            C ○ │ ╭───╯ │
            D ○ │ │ ╭───╯"));
    }

    // ---- lazygit's TestRenderPipeSet (no selection), glyphs and colours ----

    fn pipe(from: &str, to: &str, from_pos: usize, to_pos: usize, kind: PipeKind, colour: u8) -> Pipe {
        let h = |s: &str| if s == "empty" { None } else { Some(id(s)) };
        Pipe { from: h(from), to: h(to), from_pos, to_pos, kind, colour }
    }

    /// One row drawn from `pipes`: its glyphs and their colours, blanks as (' ', None).
    fn cells(pipes: &[Pipe]) -> (String, Vec<Option<u8>>) {
        let mut rows = Rows::default();
        render(pipes, &mut rows);
        let g: Vec<Option<(char, u8)>> = rows.get(0).unwrap().glyphs().collect();
        (g.iter().map(|g| g.map_or(' ', |g| g.0)).collect(), g.iter().map(|g| g.map(|g| g.1)).collect())
    }

    use PipeKind::{Continues as C, Starts as S, Terminates as T};
    const CYAN: u8 = 0;
    const RED: u8 = 1;
    const GREEN: u8 = 2;
    const YELLOW: u8 = 3;
    const MAGENTA: u8 = 4;

    #[test]
    fn pipe_set_single_cell() {
        assert_eq!(cells(&[pipe("a", "b", 0, 0, T, CYAN), pipe("b", "c", 0, 0, S, GREEN)]), ("○".into(), vec![Some(GREEN)]));
    }

    #[test]
    fn pipe_set_terminating_hook_and_starting_hook_prioritise_the_terminating_one() {
        let p = [pipe("a", "b", 0, 0, T, RED), pipe("c", "b", 1, 0, T, MAGENTA), pipe("b", "d", 0, 0, S, GREEN), pipe("b", "e", 0, 1, S, GREEN)];
        assert_eq!(cells(&p), ("◎─│".into(), vec![Some(GREEN), Some(GREEN), Some(MAGENTA)]));
    }

    #[test]
    fn pipe_set_starting_and_terminating_pipe_sharing_some_space() {
        let p = [pipe("a1", "a2", 0, 0, T, RED), pipe("a2", "a3", 0, 0, S, YELLOW), pipe("b1", "b2", 1, 1, C, MAGENTA), pipe("e1", "a2", 3, 0, T, GREEN), pipe("a2", "c3", 0, 2, S, YELLOW)];
        assert_eq!(cells(&p), ("◎─│─┬─╯".into(), vec![Some(YELLOW), Some(YELLOW), Some(MAGENTA), Some(YELLOW), Some(YELLOW), Some(GREEN), Some(GREEN)]));
    }

    #[test]
    fn pipe_set_many_terminating_pipes() {
        let p = [pipe("a1", "a2", 0, 0, T, RED), pipe("a2", "a3", 0, 0, S, YELLOW), pipe("b1", "a2", 1, 0, T, MAGENTA), pipe("c1", "a2", 2, 0, T, GREEN)];
        assert_eq!(cells(&p), ("○─┴─╯".into(), vec![Some(YELLOW), Some(MAGENTA), Some(MAGENTA), Some(GREEN), Some(GREEN)]));
    }

    #[test]
    fn pipe_set_starting_pipe_passing_through() {
        let p = [pipe("a1", "a2", 0, 0, T, RED), pipe("a2", "a3", 0, 0, S, YELLOW), pipe("a2", "d3", 0, 3, S, YELLOW), pipe("b1", "b3", 1, 1, C, MAGENTA), pipe("c1", "c3", 2, 2, C, GREEN)];
        assert_eq!(cells(&p), ("◎─│─│─╮".into(), vec![Some(YELLOW), Some(YELLOW), Some(MAGENTA), Some(YELLOW), Some(GREEN), Some(YELLOW), Some(YELLOW)]));
    }

    #[test]
    fn pipe_set_starting_and_terminating_path_crossing_continuing_path() {
        let p = [pipe("a1", "a2", 0, 0, T, RED), pipe("a2", "a3", 0, 0, S, YELLOW), pipe("a2", "b3", 0, 1, S, YELLOW), pipe("b1", "a2", 1, 1, C, GREEN), pipe("c1", "a2", 2, 0, T, MAGENTA)];
        assert_eq!(cells(&p), ("◎─│─╯".into(), vec![Some(YELLOW), Some(YELLOW), Some(GREEN), Some(MAGENTA), Some(MAGENTA)]));
    }

    #[test]
    fn pipe_set_another_clash_of_starting_and_terminating_paths() {
        let p = [pipe("a1", "a2", 0, 0, T, RED), pipe("a2", "a3", 0, 0, S, YELLOW), pipe("a2", "b3", 0, 1, S, YELLOW), pipe("c1", "c3", 2, 2, C, GREEN), pipe("d1", "a2", 3, 0, T, MAGENTA)];
        assert_eq!(cells(&p), ("◎─┬─│─╯".into(), vec![Some(YELLOW), Some(YELLOW), Some(YELLOW), Some(MAGENTA), Some(GREEN), Some(MAGENTA), Some(MAGENTA)]));
    }

    #[test]
    fn pipe_set_root_commit_has_no_line_below() {
        let p = [pipe("a", "root", 0, 0, T, CYAN), pipe("root", "empty", 0, 0, S, GREEN)];
        let mut rows = Rows::default();
        render(&p, &mut rows);
        assert_eq!(rows.get(0).unwrap().filler().flatten().count(), 0);
    }

    // ---- lazygit's TestGetNextPipes ----

    #[test]
    fn next_pipes() {
        let mut lanes = Lanes::default();
        let (a, b, c, d, e) = (id("a"), id("b"), id("c"), id("d"), id("e"));
        let p = |from, to, from_pos, to_pos, kind| Pipe { from: Some(from), to, from_pos, to_pos, kind, colour: 0 };
        let colourless = |v: Vec<Pipe>| v.into_iter().map(|p| Pipe { colour: 0, ..p }).collect::<Vec<_>>();
        assert_eq!(colourless(lanes.next_pipes(&[p(a, Some(b), 0, 0, S)], b, &[c])), [p(a, Some(b), 0, 0, T), p(b, Some(c), 0, 0, S)]);
        let prev = [p(a, Some(b), 0, 0, T), p(b, Some(c), 0, 0, S), p(b, Some(d), 0, 1, S)];
        assert_eq!(colourless(lanes.next_pipes(&prev, d, &[e])), [p(b, Some(c), 0, 0, C), p(b, Some(d), 1, 1, T), p(d, Some(e), 1, 1, S)]);
        let root = id("root");
        assert_eq!(colourless(lanes.next_pipes(&[p(a, Some(root), 0, 0, T)], root, &[])), [p(root, None, 0, 0, S)]);
    }

    // ---- gitty's own ----

    fn commit_colour(r: Row<'_>) -> u8 {
        r.glyphs().flatten().find(|g| g.0 == COMMIT_SYMBOL || g.0 == MERGE_SYMBOL).unwrap().1
    }

    #[test]
    fn a_branch_keeps_its_colour_and_the_main_line_keeps_its_own_past_a_merge() {
        let h: &[(&str, &[&str])] = &[("1", &["2"]), ("2", &["3", "4"]), ("4", &["5"]), ("3", &["5"]), ("5", &["6"]), ("6", &[])];
        assert_eq!(graph(h), expected("
            1 ○
            2 ◎─╮
            4 │ ○
            3 ○ │
            5 ○─╯
            6 ○"));
        let (rows, _) = layout(h);
        let c: Vec<u8> = (0..6).map(|i| commit_colour(rows.get(i).unwrap())).collect();
        assert!(c[0] == c[1] && c[1] == c[3] && c[3] == c[4] && c[4] == c[5], "the main line: {c:?}");
        assert_ne!(c[2], c[0], "the branch has its own");
        // the join is drawn in the branch's colour
        let last: Vec<(char, u8)> = rows.get(4).unwrap().glyphs().flatten().collect();
        assert_eq!(last, [('○', c[0]), ('─', c[2]), ('╯', c[2])]);
    }

    #[test]
    fn rows_wider_than_the_store_are_marked_and_keep_the_commit() {
        let tips: Vec<(String, Vec<&str>)> = (0..60).map(|i| (format!("t{i}"), vec!["0"])).collect();
        let h: Vec<(&str, &[&str])> = tips.iter().map(|(c, p)| (c.as_str(), p.as_slice())).chain([("0", &[][..])]).collect();
        let (rows, lanes) = layout(&h);
        let last = rows.get(60).unwrap();
        assert!(last.clipped() && last.columns() == MAX_LANES * 2 - 1);
        assert!(!rows.get(0).unwrap().clipped());
        // a commit in a column past the store keeps its symbol, in the last stored one
        let far = rows.get(55).unwrap();
        assert!(far.clipped());
        assert_eq!(far.glyphs().flatten().filter(|g| g.0 == COMMIT_SYMBOL).count(), 1);
        assert_eq!(far.glyphs().last().flatten().map(|g| g.0), Some(COMMIT_SYMBOL));
        assert!(lanes.pipes.iter().all(|p| p.kind == PipeKind::Terminates || p.to.is_none()));
    }

    #[test]
    fn filler_continues_the_lines_that_go_down() {
        let (rows, _) = layout(&[("4", &["3", "2"]), ("2", &["1"]), ("3", &["1"]), ("1", &[])]);
        let f = |i: usize| rows.get(i).unwrap().filler().map(|g| g.map_or(' ', |g| g.0)).collect::<String>().trim_end().to_string();
        assert_eq!((f(0), f(1), f(3)), ("│ │".into(), "│ │".into(), "".into()));
    }

    mod props {
        use super::*;
        use proptest::prelude::*;

        /// A random DAG in topological order: commit `i`'s parents are later commits.
        fn dag() -> impl Strategy<Value = Vec<Vec<usize>>> {
            (2usize..40).prop_flat_map(|n| {
                (0..n)
                    .map(move |i| {
                        let later = (i + 1)..n;
                        if later.is_empty() { Just(Vec::new()).boxed() } else { proptest::collection::vec(later, 0..4).boxed() }
                    })
                    .collect::<Vec<_>>()
            })
        }

        proptest! {
            #[test]
            fn every_commit_has_one_symbol_and_every_parent_a_pipe(parents in dag()) {
                let name = |i: usize| id(&format!("c{i}"));
                let mut lanes = Lanes::default();
                let mut rows = Rows::default();
                for (i, ps) in parents.iter().enumerate() {
                    let ps: Vec<CommitId> = ps.iter().map(|&p| name(p)).collect();
                    lanes.row(name(i), &ps, &mut rows);
                    let row = rows.get(i).unwrap();
                    prop_assert_eq!(row.glyphs().flatten().filter(|g| g.0 == COMMIT_SYMBOL || g.0 == MERGE_SYMBOL).count(), 1);
                    let c = row.0.iter().position(|cell| cell & TYPE_MASK != 0).unwrap();
                    // the commit's line goes on down to its first parent from its own column
                    if let Some(&first) = ps.first() {
                        prop_assert!(lanes.pipes.iter().any(|p| p.kind == PipeKind::Starts && p.from_pos == c && p.to_pos == c && p.to == Some(first)));
                        prop_assert!(row.0[c] & DOWN != 0);
                    }
                    // every parent has a pipe leading down to it
                    for p in &ps {
                        prop_assert!(lanes.pipes.iter().any(|q| q.kind != PipeKind::Terminates && q.to == Some(*p)), "parent without a pipe");
                    }
                    // every pipe still open leads to a commit not yet shown
                    for p in lanes.pipes.iter().filter(|p| p.kind != PipeKind::Terminates) {
                        prop_assert!(p.to.is_none() || (i + 1..parents.len()).any(|j| Some(name(j)) == p.to));
                    }
                }
                prop_assert!(lanes.pipes.iter().all(|p| p.kind == PipeKind::Terminates || p.to.is_none()), "pipes left open after the last commit");
            }
        }
    }
}
