//! The History list's commit graph: one row per commit, its lanes laid out by gitty.
//!
//! With the graph on, History lists git's topological order ([`topo_walk`]: `git rev-list
//! --topo-order --parents`, so a commit always comes before its parents) and [`Lanes`] gives each
//! commit a lane as the walk streams in. The rows are ready together with the list's ids.
//!
//! A lane is two terminal columns: its own cell, then a gap that only carries horizontal links.
//! A lane keeps the colour it was opened with until it ends, so a branch keeps one colour all
//! the way down. Commits that more than one lane waits for (branches meeting at their fork
//! point) take the leftmost lane and the others join it with `╯`; a merge's other parents open
//! lanes with `╮` (`╭` on the left), or link to a lane already waiting for them.

use smallvec::SmallVec;

use crate::git_cli::{GitCli, Kind};
use crate::types::CommitId;

const UP: u16 = 1;
const DOWN: u16 = 2;
const LEFT: u16 = 4;
const RIGHT: u16 = 8;
const DIRS: u16 = UP | DOWN | LEFT | RIGHT;
const COMMIT: u16 = 1 << 4;
const COLOUR: u16 = 5;
const COLOUR_MASK: u16 = 0b111 << COLOUR;
/// The gap after a lane carries a horizontal link, in the colour at `GAP_COLOUR`.
const GAP: u16 = 1 << 8;
const GAP_COLOUR: u16 = 9;
const GAP_COLOUR_MASK: u16 = 0b111 << GAP_COLOUR;
/// On a row's last stored lane: the row has more lanes than are stored.
const MORE: u16 = 1 << 12;
/// Lane colours cycle through this many indices. The UI maps them to a palette of this many
/// entries built from the theme's distinct hues (fewer distinct ones repeat in the cycle).
pub const COLOURS: u8 = 7;
/// Lanes stored per row: no pane draws more, and a pathological history stays small.
const MAX_LANES: usize = 48;

#[derive(Debug, Clone, Copy)]
struct Lane {
    /// The commit this lane leads down to.
    want: CommitId,
    colour: u8,
}

/// The layout state of a topological walk: which commit each lane waits for.
#[derive(Debug, Default)]
pub struct Lanes {
    lanes: Vec<Option<Lane>>,
    opened: u32,
}

fn colour(c: u8) -> u16 {
    u16::from(c) << COLOUR
}

impl Lanes {
    fn open(&mut self) -> u8 {
        let c = (self.opened % u32::from(COLOURS)) as u8;
        self.opened = self.opened.wrapping_add(1);
        c
    }

    /// The first lane free at the start of this row (lanes that end in it are not reused yet).
    fn free(&self, busy: &[usize]) -> usize {
        (0..).find(|&i| i >= self.lanes.len() || (self.lanes[i].is_none() && !busy.contains(&i))).expect("a free lane")
    }

    /// Lays out the next commit of a topological walk and appends its row to `out`.
    pub fn row(&mut self, id: CommitId, parents: &[CommitId], out: &mut Rows) {
        let waiting = |lanes: &[Option<Lane>]| -> SmallVec<[usize; 4]> { (0..lanes.len()).filter(|&i| lanes[i].is_some_and(|l| l.want == id)).collect() };
        let mut matches = waiting(&self.lanes);
        // nothing but free lanes left of the commit's lane (lanes that ended above): the commit
        // moves over to the leftmost and its lane joins it there with `╯`, so a branch does not
        // run on far to the right of an empty stretch. (Not a merge, which opens a lane into the
        // free room instead, nor a root, which ends its lane anyway.)
        if let (Some(&k), 1) = (matches.first(), parents.len()) {
            let f = self.free(&[]);
            if f < k && self.lanes[f..k].iter().all(Option::is_none) {
                self.lanes[f] = Some(Lane { want: id, colour: self.lanes[k].expect("a waiting lane").colour });
                matches = waiting(&self.lanes);
            }
        }
        let c = match matches.first() {
            Some(&c) => c,
            // a branch tip: a new lane
            None => {
                let c = self.free(&[]);
                let colour = self.open();
                if c == self.lanes.len() {
                    self.lanes.push(None);
                }
                self.lanes[c] = Some(Lane { want: id, colour });
                c
            }
        };
        let mut cells: SmallVec<[u16; 16]> = SmallVec::from_elem(0, self.lanes.len());
        for (i, l) in self.lanes.iter().enumerate() {
            if let Some(l) = l
                && i != c
                && !matches.contains(&i)
            {
                cells[i] = UP | DOWN | colour(l.colour);
            }
        }
        let own = self.lanes[c].expect("the commit's lane").colour;
        cells[c] = COMMIT | colour(own) | if matches.is_empty() { 0 } else { UP } | if parents.is_empty() { 0 } else { DOWN };
        // (lane, its colour, the directions it adds at its end)
        let mut links: SmallVec<[(usize, u8, u16); 4]> = SmallVec::new();
        let mut ended: SmallVec<[usize; 4]> = SmallVec::new();
        // the other lanes waiting for this commit end in it
        for &j in matches.iter().skip(1) {
            links.push((j, self.lanes[j].expect("a waiting lane").colour, UP));
            ended.push(j);
        }
        let mut busy: SmallVec<[usize; 4]> = matches.clone();
        match parents.first() {
            Some(&first) => self.lanes[c] = Some(Lane { want: first, colour: own }),
            None => ended.push(c),
        }
        for (n, &p) in parents.iter().enumerate().skip(1) {
            // a parent listed twice links once
            if parents[..n].contains(&p) {
                continue;
            }
            match (0..self.lanes.len()).find(|&k| k != c && !ended.contains(&k) && self.lanes[k].is_some_and(|l| l.want == p)) {
                Some(k) => links.push((k, self.lanes[k].expect("a lane").colour, 0)),
                None => {
                    let k = self.free(&busy);
                    let colour = self.open();
                    if k >= self.lanes.len() {
                        self.lanes.resize(k + 1, None);
                    }
                    self.lanes[k] = Some(Lane { want: p, colour });
                    busy.push(k);
                    links.push((k, colour, DOWN));
                }
            }
        }
        for j in ended {
            self.lanes[j] = None;
        }
        // farthest first: on a stretch two links share, the nearer one's colour wins
        links.sort_by_key(|l| std::cmp::Reverse(l.0.abs_diff(c)));
        for (j, lc, dirs) in links {
            if j >= cells.len() {
                cells.resize(j + 1, 0);
            }
            let (lo, hi) = (c.min(j), c.max(j));
            cells[j] = (cells[j] & !COLOUR_MASK) | dirs | if j > c { LEFT } else { RIGHT } | colour(lc);
            for x in lo..hi {
                cells[x] = (cells[x] & !GAP_COLOUR_MASK) | GAP | (u16::from(lc) << GAP_COLOUR);
                if x > lo {
                    // an empty cell takes the link's colour; a lane crossing it keeps its own
                    if cells[x] & DIRS == 0 {
                        cells[x] |= colour(lc);
                    }
                    cells[x] |= LEFT | RIGHT;
                }
            }
        }
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        while cells.last() == Some(&0) {
            cells.pop();
        }
        if cells.len() > MAX_LANES {
            cells.truncate(MAX_LANES);
            cells[MAX_LANES - 1] |= MORE;
            if c >= MAX_LANES {
                // the commit's own lane is cut off: its dot takes the last stored lane
                cells[MAX_LANES - 1] = COMMIT | MORE | colour(own);
            }
        }
        out.push(&cells);
    }
}

/// The rows of a graph, stored flat: two bytes per lane.
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

fn glyph(cell: u16) -> Option<char> {
    if cell & COMMIT != 0 {
        return Some('●');
    }
    let (u, d, l, r) = (cell & UP != 0, cell & DOWN != 0, cell & LEFT != 0, cell & RIGHT != 0);
    Some(match (u, d, l, r) {
        (false, false, false, false) => return None,
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        // a link that runs on past a lane opening or ending there
        (false, true, true, true) => '┬',
        (true, false, true, true) => '┴',
        (false, true, true, false) => '╮',
        (true, false, true, false) => '╯',
        (false, true, false, true) => '╭',
        (true, false, false, true) => '╰',
        (_, _, false, false) => '│',
        (false, false, _, _) => '─',
    })
}

impl Row<'_> {
    /// Terminal columns the row's lanes take.
    pub fn columns(&self) -> usize {
        (self.0.len() * 2).saturating_sub(1)
    }
    /// More lanes than are stored: the row is cut on the right.
    pub fn clipped(&self) -> bool {
        self.0.last().is_some_and(|c| c & MORE != 0)
    }
    /// Each column's glyph and colour index (None: blank).
    pub fn glyphs(&self) -> impl Iterator<Item = Option<(char, u8)>> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &cell)| {
            let own = glyph(cell).map(|g| (g, ((cell & COLOUR_MASK) >> COLOUR) as u8));
            let gap = (cell & GAP != 0).then_some(('─', ((cell & GAP_COLOUR_MASK) >> GAP_COLOUR) as u8));
            std::iter::once(own).chain((i + 1 < self.0.len()).then_some(gap))
        })
    }
    /// The line under the row (a second line of text): every lane that goes on down.
    pub fn filler(&self) -> impl Iterator<Item = Option<(char, u8)>> + '_ {
        self.0.iter().enumerate().flat_map(|(i, &cell)| {
            let own = (cell & DOWN != 0).then_some(('│', ((cell & COLOUR_MASK) >> COLOUR) as u8));
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

    fn id(n: usize) -> CommitId {
        let mut b = [0u8; 20];
        b[..8].copy_from_slice(&(n as u64 + 1).to_be_bytes());
        CommitId(b)
    }

    /// Lays out `(commit, parents)` in the given (topological) order.
    fn layout(history: &[(usize, &[usize])]) -> (Rows, Lanes) {
        let (mut lanes, mut rows) = (Lanes::default(), Rows::default());
        for (c, ps) in history {
            let ps: Vec<CommitId> = ps.iter().map(|&p| id(p)).collect();
            lanes.row(id(*c), &ps, &mut rows);
        }
        (rows, lanes)
    }
    fn text(history: &[(usize, &[usize])]) -> Vec<String> {
        let (rows, _) = layout(history);
        (0..rows.len()).map(|i| rows.get(i).unwrap().text()).collect()
    }
    fn commit_colour(r: Row<'_>) -> u8 {
        r.glyphs().flatten().find(|g| g.0 == '●').unwrap().1
    }

    #[test]
    fn linear() {
        assert_eq!(text(&[(3, &[2]), (2, &[1]), (1, &[])]), ["●", "●", "●"]);
    }

    #[test]
    fn a_merge_and_its_branch_rejoin() {
        // 4 merges 2 into 3; 2 branched from 1
        assert_eq!(text(&[(4, &[3, 2]), (2, &[1]), (3, &[1]), (1, &[0]), (0, &[])]), ["●─╮", "│ ●", "● │", "●─╯", "●"]);
    }

    #[test]
    fn two_long_lived_branches_keep_their_lanes_and_colours() {
        let h: &[(usize, &[usize])] = &[(10, &[9]), (20, &[19]), (9, &[8]), (19, &[18]), (8, &[1]), (18, &[1]), (1, &[])];
        assert_eq!(text(h), ["●", "│ ●", "● │", "│ ●", "● │", "│ ●", "●─╯"]);
        let (rows, _) = layout(h);
        let left: Vec<u8> = [0, 2, 4].iter().map(|&i| commit_colour(rows.get(i).unwrap())).collect();
        let right: Vec<u8> = [1, 3, 5].iter().map(|&i| commit_colour(rows.get(i).unwrap())).collect();
        assert!(left.iter().all(|&c| c == left[0]) && right.iter().all(|&c| c == right[0]) && left[0] != right[0], "{left:?} {right:?}");
        // the join takes the colour of the lane that ends
        let last: Vec<(char, u8)> = rows.get(6).unwrap().glyphs().flatten().collect();
        assert_eq!(last, [('●', left[0]), ('─', right[0]), ('╯', right[0])]);
    }

    #[test]
    fn an_octopus_merge_opens_and_closes_several_lanes_in_one_row() {
        let h: &[(usize, &[usize])] = &[(9, &[1, 2, 3]), (3, &[0]), (2, &[0]), (1, &[0]), (0, &[])];
        assert_eq!(text(h), ["●─┬─╮", "│ │ ●", "│ ● │", "● │ │", "●─┴─╯"]);
        // each stretch of the fan-out has the colour of the lane it leads to
        let (rows, _) = layout(h);
        let g: Vec<u8> = rows.get(0).unwrap().glyphs().flatten().map(|g| g.1).collect();
        assert_eq!((g[1], g[2], g[3], g[4]), (g[2], g[2], g[4], g[4]));
        assert_ne!(g[2], g[4]);
    }

    #[test]
    fn root_commits_end_their_lanes() {
        // two unrelated roots, then a merge of an orphan branch
        assert_eq!(text(&[(2, &[]), (1, &[])]), ["●", "●"]);
        let (rows, lanes) = layout(&[(5, &[4, 3]), (3, &[]), (4, &[])]);
        assert_eq!((0..3).map(|i| rows.get(i).unwrap().text()).collect::<Vec<_>>(), ["●─╮", "│ ●", "●"]);
        assert!(lanes.lanes.is_empty());
    }

    #[test]
    fn criss_cross_merges_link_to_the_lanes_already_waiting() {
        let h: &[(usize, &[usize])] = &[(8, &[1, 2]), (9, &[2, 1]), (1, &[0]), (2, &[0]), (0, &[])];
        assert_eq!(text(h), ["●─╮", "├─┼─●", "● │ │", "│ ●─╯", "●─╯"]);
    }

    #[test]
    fn a_freed_lane_is_reused_with_a_new_colour() {
        // 2 is a merged orphan root: its lane ends, and the next tip takes it
        let h: &[(usize, &[usize])] = &[(9, &[1, 2]), (2, &[]), (7, &[1]), (1, &[])];
        assert_eq!(text(h), ["●─╮", "│ ●", "│ ●", "●─╯"]);
        let (rows, _) = layout(h);
        assert_ne!(commit_colour(rows.get(1).unwrap()), commit_colour(rows.get(2).unwrap()));
    }

    #[test]
    fn a_lane_opening_left_of_the_commit_curves_the_other_way() {
        // 1 ends lane 0; then 2, in lane 1, opens its merge parent's lane in free lane 0
        let h: &[(usize, &[usize])] = &[(9, &[1]), (8, &[2]), (1, &[]), (2, &[3, 4]), (4, &[3]), (3, &[])];
        assert_eq!(text(h), ["●", "│ ●", "● │", "╭─●", "● │", "●─╯"]);
    }

    #[test]
    fn a_link_running_on_past_an_opening_lane_tees_into_it() {
        // 10, 11 and 12 hold lanes 0 to 2; 1 and 2 end lanes 0 and 1; then 5, in lane 2, opens
        // two lanes to its left
        let h: &[(usize, &[usize])] = &[(10, &[1]), (11, &[2]), (12, &[5]), (1, &[]), (2, &[]), (5, &[3, 4, 6]), (4, &[3]), (6, &[3]), (3, &[])];
        assert_eq!(text(h)[5], "╭─┬─●");
    }

    #[test]
    fn a_branch_left_alone_on_the_right_moves_over_to_the_free_lanes() {
        // three lanes meet at 10, a merge that opens a lane for 12 past the two that end; 12
        // then moves into the room they left
        let h: &[(usize, &[usize])] = &[(30, &[10]), (31, &[10]), (32, &[10]), (10, &[11, 12]), (12, &[11]), (11, &[])];
        assert_eq!(text(h), ["●", "│ ●", "│ │ ●", "●─┴─┴─╮", "│ ●───╯", "●─╯"]);
        // and keeps its colour
        let (rows, _) = layout(h);
        assert_eq!(commit_colour(rows.get(4).unwrap()), rows.get(3).unwrap().glyphs().flatten().last().unwrap().1);
    }

    #[test]
    fn rows_wider_than_the_store_are_marked() {
        let tips: Vec<(usize, Vec<usize>)> = (0..60).map(|i| (100 + i, vec![0])).collect();
        let h: Vec<(usize, &[usize])> = tips.iter().map(|(c, p)| (*c, p.as_slice())).chain([(0, &[][..])]).collect();
        let (rows, lanes) = layout(&h);
        let last = rows.get(60).unwrap();
        assert!(last.clipped() && last.columns() == MAX_LANES * 2 - 1);
        assert!(!rows.get(0).unwrap().clipped());
        // a commit in a lane past the store keeps its dot, in the last stored lane
        let far = rows.get(55).unwrap();
        assert!(far.clipped());
        assert_eq!(far.glyphs().flatten().filter(|g| g.0 == '●').count(), 1);
        assert_eq!(far.glyphs().last().flatten().map(|g| g.0), Some('●'));
        assert!(lanes.lanes.is_empty());
    }

    #[test]
    fn filler_continues_the_lanes_that_go_down() {
        let (rows, _) = layout(&[(4, &[3, 2]), (2, &[1]), (3, &[1]), (1, &[])]);
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
            fn every_commit_has_one_dot_and_every_parent_a_lane(parents in dag()) {
                let mut lanes = Lanes::default();
                let mut rows = Rows::default();
                for (i, ps) in parents.iter().enumerate() {
                    let ps: Vec<CommitId> = ps.iter().map(|&p| id(p)).collect();
                    lanes.row(id(i), &ps, &mut rows);
                    let row = rows.get(i).unwrap();
                    prop_assert_eq!(row.glyphs().flatten().filter(|g| g.0 == '●').count(), 1);
                    let c = row.0.iter().position(|cell| cell & COMMIT != 0).unwrap();
                    // the commit's lane goes on down to its first parent
                    if let Some(&first) = ps.first() {
                        prop_assert!(lanes.lanes[c].is_some_and(|l| l.want == first));
                        prop_assert!(row.0[c] & DOWN != 0);
                    }
                    // every other parent has a lane waiting for it, linked from this row
                    for p in ps.iter().skip(1).filter(|&&p| p != ps[0]) {
                        let k = lanes.lanes.iter().position(|l| l.is_some_and(|l| l.want == *p));
                        prop_assert!(k.is_some(), "parent without a lane");
                        let k = k.unwrap();
                        prop_assert!(k < row.0.len() && row.0[k] & (LEFT | RIGHT) != 0, "parent not linked");
                    }
                    // every lane still open waits for a commit not yet shown
                    for l in lanes.lanes.iter().flatten() {
                        prop_assert!((i + 1..parents.len()).any(|j| id(j) == l.want));
                    }
                }
                prop_assert!(lanes.lanes.is_empty(), "lanes left open after the last commit");
            }
        }
    }
}
