//! The History list's commit graph: git draws it (`git log --graph`) and gitty re-skins the art.
//!
//! Re-skinning git's own ASCII graph instead of laying out lanes, the glyph map and the lane
//! colouring rule follow the editor druk's commit graph (https://github.com/letstri/druk, MIT,
//! Copyright (c) Valerii Strilets).
//!
//! With the graph on, History lists git's topological order: [`topo_walk`] streams the ids and
//! [`GraphArt::load`] draws the first rows with the same revisions. git keeps a graph's lane
//! state inside the process, so a deeper page runs git from the top again; the caller doubles
//! the pages, which keeps the total work linear in the deepest row asked for.

use crate::git_cli::{GitCli, Kind};
use crate::types::CommitId;

/// Separates the art from the commit id on a commit's line (`%x1f`; argv cannot hold a NUL).
const US: u8 = 0x1f;

/// `git log --graph` output for the first rows of a history: each commit's line, then the
/// connector lines (`|\`, `|/`…) drawn before the next commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphArt {
    /// Every kept line's art, concatenated (git draws it in ASCII).
    text: String,
    /// End of each line in `text`.
    ends: Vec<u32>,
    /// Each commit's id and the index of its line.
    commits: Vec<(CommitId, u32)>,
    /// Commits kept: the line of the one after them only ends the last one's connectors.
    max: usize,
    /// A commit past `max` was seen: there is more history than this.
    more: bool,
    complete: bool,
}

impl GraphArt {
    fn new(max: usize) -> GraphArt {
        GraphArt { max, ..Default::default() }
    }

    /// Parses `git log --graph --format=%x1f%H` output, keeping `max` commits.
    pub fn parse(out: &[u8], max: usize) -> GraphArt {
        let mut art = GraphArt::new(max);
        let all = out.split(|&b| b == b'\n').all(|l| art.push_line(l));
        art.finish(all)
    }

    /// Takes one line of output; false once the line of the commit after the last kept one
    /// is in (the lines before it are the last commit's connectors).
    fn push_line(&mut self, l: &[u8]) -> bool {
        let (art, id) = match l.iter().position(|&b| b == US) {
            Some(us) => {
                // only the id is asked for; anything after a second separator is ignored
                let field = l[us + 1..].split(|&b| b == US).next().unwrap_or_default();
                let Some(id) = std::str::from_utf8(field).ok().and_then(|s| CommitId::from_hex(s.trim())) else { return true };
                (&l[..us], Some(id))
            }
            None => (l, None),
        };
        match id {
            Some(_) if self.commits.len() == self.max => {
                self.more = true;
                return false;
            }
            Some(id) => self.commits.push((id, self.ends.len() as u32)),
            // nothing comes before the first commit; a blank line connects nothing
            None if self.commits.is_empty() || art.trim_ascii().is_empty() => return true,
            None => {}
        }
        self.text.push_str(String::from_utf8_lossy(art).trim_end());
        self.ends.push(self.text.len() as u32);
        true
    }

    fn finish(mut self, read_all: bool) -> GraphArt {
        self.complete = read_all && !self.more;
        self
    }

    /// Runs `git log --graph` over `tips` (in [`topo_walk`]'s order) and keeps `rows` commits.
    /// `None` when `cancelled` stopped it.
    pub fn load(cli: &GitCli, tips: &[CommitId], rows: usize, cancelled: &dyn Fn() -> bool) -> anyhow::Result<Option<GraphArt>> {
        let mut art = GraphArt::new(rows);
        if tips.is_empty() {
            return Ok(Some(art.finish(true)));
        }
        // one commit more than kept: its line closes the last kept commit's connectors
        let n = format!("-n{}", rows.saturating_add(1));
        let hex: Vec<String> = tips.iter().map(CommitId::to_hex).collect();
        let mut args = vec!["--no-optional-locks", "log", "--graph", "--topo-order", "--no-color", "--no-show-signature", "--format=%x1f%H", &n];
        args.extend(hex.iter().map(String::as_str));
        args.push("--");
        let read_all = cli.read_lines(cli.cmd(Kind::Read, &args), cancelled, &mut |l| art.push_line(l))?;
        if cancelled() {
            return Ok(None);
        }
        Ok(Some(art.finish(read_all)))
    }

    /// Commits drawn.
    pub fn len(&self) -> usize {
        self.commits.len()
    }
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }
    /// The whole history is drawn: no deeper page exists.
    pub fn complete(&self) -> bool {
        self.complete
    }
    pub fn id(&self, i: usize) -> CommitId {
        self.commits[i].0
    }
    fn line(&self, k: usize) -> &str {
        let start = if k == 0 { 0 } else { self.ends[k - 1] as usize };
        &self.text[start..self.ends[k] as usize]
    }
    /// The art on commit `i`'s own line (`| * |`).
    pub fn commit_line(&self, i: usize) -> &str {
        self.line(self.commits[i].1 as usize)
    }
    /// The connector lines between commit `i` and the next one.
    pub fn connectors(&self, i: usize) -> impl Iterator<Item = &str> {
        let from = self.commits[i].1 as usize + 1;
        let to = self.commits.get(i + 1).map_or(self.ends.len(), |c| c.1 as usize);
        (from..to).map(|k| self.line(k))
    }
    pub fn connector_count(&self, i: usize) -> usize {
        let from = self.commits[i].1 as usize + 1;
        self.commits.get(i + 1).map_or(self.ends.len(), |c| c.1 as usize) - from
    }
}

/// Streams the ids of the commits reachable from `tips` in the order [`GraphArt::load`] draws
/// them (`git rev-list --topo-order`). `id` returns false to stop. Returns whether the walk
/// reached its end.
pub fn topo_walk(cli: &GitCli, tips: &[CommitId], cancelled: &dyn Fn() -> bool, id: &mut dyn FnMut(CommitId) -> bool) -> anyhow::Result<bool> {
    if tips.is_empty() {
        return Ok(true);
    }
    let hex: Vec<String> = tips.iter().map(CommitId::to_hex).collect();
    let mut args = vec!["--no-optional-locks", "rev-list", "--topo-order"];
    args.extend(hex.iter().map(String::as_str));
    args.push("--");
    cli.read_lines(cli.cmd(Kind::Read, &args), cancelled, &mut |l| match std::str::from_utf8(l).ok().and_then(|s| CommitId::from_hex(s.trim())) {
        Some(c) => id(c),
        None => true,
    })
}

/// One drawn cell of an art line: its column, glyph and lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub col: usize,
    pub glyph: char,
    pub lane: usize,
}

/// The cells of an art line in box glyphs, blanks left out. git gives each lane two columns;
/// a diagonal belongs to the lane it reaches for. Colours go by lane, which is a column, so a
/// branch that git shifts sideways changes colour (git's art does not say which branch a
/// column carries; following branches needs gitty's own lane layout).
pub fn cells(line: &str) -> impl Iterator<Item = Cell> + '_ {
    line.chars().enumerate().filter(|(_, c)| *c != ' ').map(|(col, c)| {
        let (glyph, lane) = match c {
            '*' => ('●', col / 2),
            '|' => ('│', col / 2),
            '/' => ('╱', col.div_ceil(2)),
            '\\' => ('╲', col.div_ceil(2)),
            '-' | '_' => ('─', col / 2),
            c => (c, col / 2),
        };
        Cell { col, glyph, lane }
    })
}

/// A filler line under a commit line that has no connector below it (a second row of text):
/// a vertical wherever `above` and `below` both have a line or a commit in that column.
pub fn padding(above: &str, below: &str) -> String {
    let v = |c: Option<char>| matches!(c, Some('|' | '*'));
    let below: Vec<char> = below.chars().collect();
    let line: String = above.chars().enumerate().map(|(i, c)| if v(Some(c)) && v(below.get(i).copied()) { '|' } else { ' ' }).collect();
    line.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u8) -> String {
        format!("{n:02x}").repeat(20)
    }
    fn id(n: u8) -> CommitId {
        CommitId::from_hex(&h(n)).unwrap()
    }
    fn out(lines: &[String]) -> Vec<u8> {
        let mut s = lines.join("\n");
        s.push('\n');
        s.into_bytes()
    }
    fn c(art: &str, n: u8) -> String {
        format!("{art}\x1f{}", h(n))
    }
    fn rows(a: &GraphArt) -> Vec<(CommitId, String, Vec<String>)> {
        (0..a.len()).map(|i| (a.id(i), a.commit_line(i).to_string(), a.connectors(i).map(String::from).collect())).collect()
    }

    #[test]
    fn linear_history() {
        let a = GraphArt::parse(&out(&[c("* ", 1), c("* ", 2), c("* ", 3)]), 10);
        assert_eq!(rows(&a), vec![(id(1), "*".into(), vec![]), (id(2), "*".into(), vec![]), (id(3), "*".into(), vec![])]);
        assert!(a.complete());
    }

    #[test]
    fn a_merge_keeps_its_connector_rows() {
        let lines = [c("*   ", 1), "|\\  ".into(), c("| * ", 2), c("* | ", 3), "|/  ".into(), c("* ", 4)];
        let a = GraphArt::parse(&out(&lines), 10);
        assert_eq!(a.len(), 4);
        assert_eq!(a.connectors(0).collect::<Vec<_>>(), ["|\\"]);
        assert_eq!(a.connector_count(1), 0);
        assert_eq!(a.connectors(2).collect::<Vec<_>>(), ["|/"]);
        assert_eq!(a.commit_line(1), "| *");
    }

    #[test]
    fn an_octopus_merge_fans_out_over_several_connector_rows() {
        let lines = [c("*-.   ", 1), "|\\ \\  ".into(), c("| | * ", 2), c("| * | ", 3), "| |/  ".into(), c("* | ", 4), "|/  ".into(), c("* ", 5)];
        let a = GraphArt::parse(&out(&lines), 10);
        assert_eq!(a.len(), 5);
        assert_eq!(a.commit_line(0), "*-.");
        assert_eq!(a.connectors(0).collect::<Vec<_>>(), ["|\\ \\"]);
        assert_eq!(a.connectors(2).collect::<Vec<_>>(), ["| |/"]);
        let dash: Vec<Cell> = cells("*-.").collect();
        assert_eq!(dash.iter().map(|c| c.glyph).collect::<String>(), "●─.");
    }

    #[test]
    fn only_the_id_after_the_separator_is_read() {
        // a stray separator or odd text after the id never becomes art or a second commit
        let lines = [format!("* \x1f{}\x1f| * \\ / ü \x1f x", h(1)), c("* ", 2)];
        let a = GraphArt::parse(&out(&lines), 10);
        assert_eq!(rows(&a), vec![(id(1), "*".into(), vec![]), (id(2), "*".into(), vec![])]);
    }

    #[test]
    fn a_line_without_a_valid_id_is_skipped() {
        let lines = [c("* ", 1), "* \x1fnot-an-id".into(), c("* ", 2)];
        assert_eq!(GraphArt::parse(&out(&lines), 10).len(), 2);
    }

    #[test]
    fn empty_output_is_an_empty_complete_graph() {
        let a = GraphArt::parse(b"", 10);
        assert!(a.is_empty() && a.complete());
    }

    #[test]
    fn the_commit_after_the_page_closes_it_and_marks_more() {
        let lines = [c("*   ", 1), "|\\  ".into(), c("| * ", 2), c("* | ", 3)];
        let a = GraphArt::parse(&out(&lines), 1);
        assert_eq!(a.len(), 1);
        assert_eq!(a.connectors(0).collect::<Vec<_>>(), ["|\\"]);
        assert!(!a.complete());
        // exactly `max` commits in the whole history: complete
        assert!(GraphArt::parse(&out(&[c("* ", 1)]), 1).complete());
    }

    #[test]
    fn glyphs_and_lanes() {
        let got: Vec<(char, usize)> = cells("| | * |").map(|c| (c.glyph, c.lane)).collect();
        assert_eq!(got, [('│', 0), ('│', 1), ('●', 2), ('│', 3)]);
        // a diagonal takes the lane it reaches for (the higher of the two it spans)
        let got: Vec<(usize, char, usize)> = cells("|\\ /").map(|c| (c.col, c.glyph, c.lane)).collect();
        assert_eq!(got, [(0, '│', 0), (1, '╲', 1), (3, '╱', 2)]);
        assert_eq!(cells("|_|/").map(|c| c.glyph).collect::<String>(), "│─│╱");
    }

    #[test]
    fn padding_continues_lines_that_go_on() {
        assert_eq!(padding("| * |", "| * |"), "| | |");
        // a root commit's lane ends: nothing under it
        assert_eq!(padding("| *", "*"), "|");
        assert_eq!(padding("*", ""), "");
    }
}
