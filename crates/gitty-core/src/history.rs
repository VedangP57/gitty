//! Commit-time ordered history walk, commit-graph native with an ODB fallback.
//!
//! Entries are `u32`: a commit-graph position, or `OVERFLOW_BIT | i` indexing commits that are
//! not (yet) in the graph. Never topo-sorted: a flat list only needs commit-time order.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use smallvec::SmallVec;

use crate::repo::{from_oid, to_oid, Handle};
use crate::types::{CommitId, Signature};

pub const OVERFLOW_BIT: u32 = 1 << 31;

type Graph = gix::commitgraph::Graph;

pub struct History {
    entries: Vec<u32>,
    overflow: Vec<gix::ObjectId>,
    graph: Option<Arc<Graph>>,
}

impl History {
    /// Moves `other`'s entries (from the same walker) to the end of `self`, leaving it empty.
    /// Lets a walker fill a private chunk and publish it under a lock in microseconds.
    pub fn append(&mut self, other: &mut History) {
        let base = self.overflow.len() as u32;
        self.entries.extend(other.entries.drain(..).map(|e| if e & OVERFLOW_BIT != 0 { OVERFLOW_BIT | ((e & !OVERFLOW_BIT) + base) } else { e }));
        self.overflow.append(&mut other.overflow);
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn id(&self, i: usize) -> CommitId {
        let e = self.entries[i];
        if e & OVERFLOW_BIT != 0 {
            from_oid(&self.overflow[(e & !OVERFLOW_BIT) as usize])
        } else {
            let g = self.graph.as_ref().expect("graph entry without graph");
            from_oid(g.id_at(gix::commitgraph::Position(e)))
        }
    }
    pub fn ids(&self, r: Range<usize>) -> Vec<CommitId> {
        let end = r.end.min(self.len());
        (r.start.min(end)..end).map(|i| self.id(i)).collect()
    }
}

enum Node {
    Graph(u32),
    Odb { id: gix::ObjectId, parents: SmallVec<[gix::ObjectId; 2]> },
}

struct Item {
    time: i64,
    seq: u64,
    node: Node,
}

impl PartialEq for Item {
    fn eq(&self, o: &Self) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Item {}
impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Item {
    /// Newest first; on equal times, first pushed pops first.
    fn cmp(&self, o: &Self) -> Ordering {
        self.time.cmp(&o.time).then_with(|| o.seq.cmp(&self.seq))
    }
}

pub struct Walker {
    graph: Option<Arc<Graph>>,
    seen_graph: Vec<u64>,
    seen_odb: HashSet<gix::ObjectId>,
    heap: BinaryHeap<Item>,
    seq: u64,
}

impl Handle {
    /// Start a commit-time-ordered walk from `tips`. Uses the commit-graph when enabled.
    pub fn walker(&self, tips: &[CommitId]) -> anyhow::Result<Walker> {
        let graph = self.commit_graph().map(Arc::new);
        let n = graph.as_ref().map(|g| g.num_commits() as usize).unwrap_or(0);
        let mut w = Walker {
            graph,
            seen_graph: vec![0; n.div_ceil(64)],
            seen_odb: HashSet::new(),
            heap: BinaryHeap::new(),
            seq: 0,
        };
        for t in tips {
            w.push_id(self, to_oid(*t))?;
        }
        Ok(w)
    }
}

impl Walker {
    pub fn uses_graph(&self) -> bool {
        self.graph.is_some()
    }

    /// An empty history sharing this walker's graph, to be filled by [`Walker::step`].
    pub fn new_history(&self) -> History {
        History { entries: Vec::new(), overflow: Vec::new(), graph: self.graph.clone() }
    }

    fn push_graph(&mut self, p: u32) {
        let (w, b) = ((p / 64) as usize, p % 64);
        if self.seen_graph[w] & (1 << b) != 0 {
            return;
        }
        self.seen_graph[w] |= 1 << b;
        let g = self.graph.as_ref().expect("graph");
        let time = g.commit_at(gix::commitgraph::Position(p)).committer_timestamp() as i64;
        self.seq += 1;
        self.heap.push(Item { time, seq: self.seq, node: Node::Graph(p) });
    }

    fn push_id(&mut self, h: &Handle, id: gix::ObjectId) -> anyhow::Result<()> {
        if let Some(pos) = self.graph.as_ref().and_then(|g| g.lookup(id)) {
            self.push_graph(pos.0);
            return Ok(());
        }
        if !self.seen_odb.insert(id) {
            return Ok(());
        }
        let commit = h.gix().find_commit(id)?;
        // A malformed or missing committer date sorts as time 0 (git does the same).
        let time = commit.committer().ok().and_then(|c| c.time().ok()).map_or(0, |t| t.seconds);
        let parents = h.parents_of(&commit);
        self.seq += 1;
        self.heap.push(Item { time, seq: self.seq, node: Node::Odb { id, parents } });
        Ok(())
    }

    /// Appends up to `max` entries to `out`. Returns false once the walk is exhausted.
    pub fn step(&mut self, h: &Handle, out: &mut History, max: usize) -> anyhow::Result<bool> {
        for _ in 0..max {
            let Some(item) = self.heap.pop() else { return Ok(false) };
            match item.node {
                Node::Graph(p) => {
                    out.entries.push(p);
                    let g = self.graph.clone().expect("graph node without graph");
                    for parent in g.commit_at(gix::commitgraph::Position(p)).iter_parents() {
                        self.push_graph(parent?.0);
                    }
                }
                Node::Odb { id, parents } => {
                    out.entries.push(OVERFLOW_BIT | out.overflow.len() as u32);
                    out.overflow.push(id);
                    for pid in parents {
                        self.push_id(h, pid)?;
                    }
                }
            }
        }
        Ok(!self.heap.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub id: CommitId,
    pub parents: SmallVec<[CommitId; 2]>,
    pub summary: String,
    pub author: Signature,
    pub committer_time: i64,
    pub co_authors: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct CommitDetail {
    pub row: CommitRow,
    pub body: String,
    pub committer: Signature,
}

fn sig(s: Result<gix::actor::SignatureRef<'_>, impl std::fmt::Debug>) -> Signature {
    let Ok(s) = s else { return Signature::default() };
    let t = s.time().unwrap_or_default();
    Signature { name: s.name.to_string(), email: s.email.to_string(), time: t.seconds, offset_secs: t.offset }
}

/// Splits a commit message into (summary line, body), trimming blank lines around both.
pub fn split_message(message: &str) -> (String, String) {
    let m = message.trim_start_matches(['\n', '\r', ' ', '\t']);
    let mut it = m.splitn(2, '\n');
    let summary = it.next().unwrap_or("").trim_end().to_string();
    let body = it.next().unwrap_or("").trim_matches(['\n', '\r']).trim_end().to_string();
    (summary, body)
}

/// `Co-authored-by: Name <email>` trailers, case-insensitive.
pub fn parse_co_authors(message: &str) -> Vec<(String, String)> {
    const KEY: &str = "co-authored-by:";
    message
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.get(..KEY.len()).filter(|p| p.eq_ignore_ascii_case(KEY)).map(|_| l[KEY.len()..].trim())?;
            let lt = rest.rfind('<')?;
            let gt = rest.rfind('>')?;
            (gt > lt).then(|| (rest[..lt].trim().to_string(), rest[lt + 1..gt].trim().to_string()))
        })
        .collect()
}

impl Handle {
    fn decode_full(&self, id: CommitId) -> anyhow::Result<(CommitRow, String, Signature)> {
        let c = self.gix().find_commit(to_oid(id))?;
        let msg = c.message_raw_sloppy().to_string();
        let (summary, body) = split_message(&msg);
        let author = sig(c.author());
        let committer = sig(c.committer());
        let parents = self.parents_of(&c).iter().map(|p| from_oid(p)).collect();
        let row = CommitRow {
            id,
            parents,
            summary,
            committer_time: committer.time,
            author,
            co_authors: parse_co_authors(&msg),
        };
        Ok((row, body, committer))
    }

    /// The fields a history row shows. ~12-16 µs; call for visible rows only.
    pub fn decode_row(&self, id: CommitId) -> anyhow::Result<CommitRow> {
        Ok(self.decode_full(id)?.0)
    }

    pub fn commit_detail(&self, id: CommitId) -> anyhow::Result<CommitDetail> {
        let (row, body, committer) = self.decode_full(id)?;
        Ok(CommitDetail { row, body, committer })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_message_cases() {
        assert_eq!(split_message("s\n\nb\nc\n"), ("s".into(), "b\nc".into()));
        assert_eq!(split_message("only"), ("only".into(), "".into()));
        assert_eq!(split_message("\n\n  lead\nx"), ("lead".into(), "x".into()));
        assert_eq!(split_message(""), ("".into(), "".into()));
    }
    #[test]
    fn co_author_parsing() {
        assert_eq!(parse_co_authors("x\n\nco-authored-by: A <a@b>\nnope: B <c>"), vec![("A".into(), "a@b".into())]);
        assert!(parse_co_authors("Co-authored-by: no email").is_empty());
    }
}
