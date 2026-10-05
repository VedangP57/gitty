//! Which commits are ↑ unpushed (local only) and ↓ unpulled (upstream only).

use std::collections::BinaryHeap;

use anyhow::Context;

use crate::repo::{from_oid, to_oid, Handle};
use crate::types::CommitId;
use crate::GitError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AheadBehind {
    /// Reachable from local but not upstream (unpushed).
    pub ahead: Vec<CommitId>,
    /// Reachable from upstream but not local (unpulled).
    pub behind: Vec<CommitId>,
}

const A: u8 = 1;
const B: u8 = 2;
const DONE: u8 = 4;
const QUEUED: u8 = 8;

impl Handle {
    pub fn ahead_behind(&self, local: CommitId, upstream: CommitId) -> anyhow::Result<AheadBehind> {
        if local == upstream {
            return Ok(AheadBehind::default());
        }
        if let Some(g) = self.commit_graph()
            && let (Some(pa), Some(pb)) = (g.lookup(to_oid(local)), g.lookup(to_oid(upstream)))
        {
            let generation_of = |p: gix::commitgraph::Position| g.commit_at(p).generation();
            if generation_of(pa) > 0 && generation_of(pb) > 0 {
                return graph_ahead_behind(&g, pa.0, pb.0);
            }
        }
        self.cli_ahead_behind(local, upstream)
    }

    fn cli_ahead_behind(&self, local: CommitId, upstream: CommitId) -> anyhow::Result<AheadBehind> {
        let args = vec!["rev-list".to_string(), "--left-right".into(), format!("{local}...{upstream}")];
        let out = self
            .owner()
            .git()
            .command()
            .env("GIT_OPTIONAL_LOCKS", "0")
            .current_dir(self.owner().git_dir())
            .args(&args)
            .output()
            .context("spawn git rev-list")?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            return Err(GitError { args, code: out.status.code(), stderr }.into());
        }
        let mut ab = AheadBehind::default();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let Some(id) = line.get(1..).and_then(|h| CommitId::from_hex(h.trim())) else { continue };
            match line.as_bytes().first() {
                Some(b'<') => ab.ahead.push(id),
                Some(b'>') => ab.behind.push(id),
                _ => {}
            }
        }
        Ok(ab)
    }
}

/// Two-colour walk in decreasing generation order. Every child has a strictly higher generation
/// than its parents, so a commit's colours are final when it is popped.
fn graph_ahead_behind(g: &gix::commitgraph::Graph, pa: u32, pb: u32) -> anyhow::Result<AheadBehind> {
    use gix::commitgraph::Position;
    let generation_of = |p: u32| g.commit_at(Position(p)).generation();
    let mut flags = vec![0u8; g.num_commits() as usize];
    let mut heap: BinaryHeap<(u32, u32)> = BinaryHeap::new();
    flags[pa as usize] |= A | QUEUED;
    flags[pb as usize] |= B | QUEUED;
    heap.push((generation_of(pa), pa));
    heap.push((generation_of(pb), pb));
    let mut ab = AheadBehind::default();
    let mut pops = 0u32;
    while let Some((_, p)) = heap.pop() {
        let f = flags[p as usize];
        flags[p as usize] |= DONE;
        let colour = f & (A | B);
        match colour {
            A => ab.ahead.push(from_oid(g.id_at(Position(p)))),
            B => ab.behind.push(from_oid(g.id_at(Position(p)))),
            _ => {}
        }
        for parent in g.commit_at(Position(p)).iter_parents() {
            let pp = parent?.0 as usize;
            let old = flags[pp];
            flags[pp] = old | colour;
            if old & QUEUED == 0 {
                flags[pp] |= QUEUED;
                heap.push((generation_of(pp as u32), pp as u32));
            }
        }
        pops += 1;
        // Once everything left is reachable from both sides, nothing else can be ahead/behind.
        if (heap.len() <= 16 || pops.is_multiple_of(256)) && heap.iter().all(|&(_, q)| flags[q as usize] & (A | B) == A | B) {
            break;
        }
    }
    Ok(ab)
}
