use std::collections::HashMap;

use crate::repo::{from_oid, Handle};
use crate::types::CommitId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel {
    pub kind: RefKind,
    pub name: String,
    pub is_head: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch { name: String, id: Option<CommitId> },
    Detached { id: CommitId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryScope {
    HeadAndUpstream,
    AllRefs,
}

#[derive(Debug, Clone)]
pub struct RefsSnapshot {
    pub head: Head,
    pub upstream: Option<(String, CommitId)>,
    pub labels: HashMap<CommitId, Vec<RefLabel>>,
    pub all_tips: Vec<CommitId>,
}

impl RefsSnapshot {
    pub fn head_id(&self) -> Option<CommitId> {
        match &self.head {
            Head::Branch { id, .. } => *id,
            Head::Detached { id } => Some(*id),
        }
    }
    pub fn head_branch(&self) -> Option<&str> {
        match &self.head {
            Head::Branch { name, .. } => Some(name),
            Head::Detached { .. } => None,
        }
    }
    pub fn tips(&self, scope: HistoryScope) -> Vec<CommitId> {
        match scope {
            HistoryScope::AllRefs => self.all_tips.clone(),
            HistoryScope::HeadAndUpstream => {
                let mut v: Vec<CommitId> = self.head_id().into_iter().collect();
                if let Some((_, u)) = &self.upstream
                    && !v.contains(u)
                {
                    v.push(*u);
                }
                v
            }
        }
    }
}

impl Handle {
    /// Snapshot of HEAD, its upstream, and every branch/tag peeled to a commit.
    pub fn refs(&self) -> anyhow::Result<RefsSnapshot> {
        let repo = self.gix();
        let head_name = repo.head_name()?;
        let head_commit = repo.head_id().ok().map(|id| from_oid(&id));
        let head = match &head_name {
            Some(n) => Head::Branch { name: n.shorten().to_string(), id: head_commit },
            None => Head::Detached {
                id: head_commit.ok_or_else(|| anyhow::anyhow!("HEAD is detached but does not resolve"))?,
            },
        };
        let upstream = head_name.as_ref().and_then(|n| {
            let tracking = repo.branch_remote_tracking_ref_name(n.as_ref(), gix::remote::Direction::Fetch)?.ok()?;
            let mut r = repo.find_reference(tracking.as_ref()).ok()?;
            let id = r.peel_to_id().ok()?;
            Some((tracking.as_ref().shorten().to_string(), from_oid(&id)))
        });
        let head_branch = head_name.as_ref().map(|n| n.shorten().to_string());

        let mut labels: HashMap<CommitId, Vec<RefLabel>> = HashMap::new();
        let mut tips: Vec<CommitId> = Vec::new();
        let platform = repo.references()?;
        let kinds = [
            (RefKind::LocalBranch, platform.local_branches()?),
            (RefKind::RemoteBranch, platform.remote_branches()?),
            (RefKind::Tag, platform.tags()?),
        ];
        for (kind, iter) in kinds {
            for r in iter {
                let Ok(mut r) = r else { continue };
                let name = r.name().shorten().to_string();
                if kind == RefKind::RemoteBranch && name.ends_with("/HEAD") {
                    continue;
                }
                let Ok(id) = r.peel_to_id() else { continue };
                let Ok(header) = repo.find_header(id.detach()) else { continue };
                if header.kind() != gix::object::Kind::Commit {
                    continue;
                }
                let cid = from_oid(&id);
                tips.push(cid);
                let is_head = kind == RefKind::LocalBranch && head_branch.as_deref() == Some(name.as_str());
                labels.entry(cid).or_default().push(RefLabel { kind, name, is_head });
            }
        }
        if let Some(h) = head_commit {
            tips.push(h);
        }
        tips.sort();
        tips.dedup();
        for v in labels.values_mut() {
            v.sort_by(|a, b| (!a.is_head, a.kind, &a.name).cmp(&(!b.is_head, b.kind, &b.name)));
        }
        Ok(RefsSnapshot { head, upstream, labels, all_tips: tips })
    }
}
