use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;

use crate::git_bin::GitBin;
use crate::types::CommitId;

/// A repository shared across threads. Call [`Repo::handle`] once per thread.
#[derive(Clone)]
pub struct Repo {
    inner: Arc<Inner>,
}

struct Inner {
    ts: gix::ThreadSafeRepository,
    git: GitBin,
    git_dir: PathBuf,
    common_dir: PathBuf,
    workdir: Option<PathBuf>,
}

impl Repo {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Repo> {
        let path = path.as_ref();
        let ts = gix::ThreadSafeRepository::discover(path)
            .with_context(|| format!("not a git repository: {}", path.display()))?;
        let local = ts.to_thread_local();
        let git_dir = local.git_dir().to_path_buf();
        let common_dir = local.common_dir().to_path_buf();
        let workdir = local.workdir().map(Path::to_path_buf);
        Ok(Repo { inner: Arc::new(Inner { ts, git: GitBin::resolve(), git_dir, common_dir, workdir }) })
    }
    pub fn workdir(&self) -> Option<&Path> {
        self.inner.workdir.as_deref()
    }
    pub fn git_dir(&self) -> &Path {
        &self.inner.git_dir
    }
    /// Where branches, tags and config live: the main `.git` for a linked worktree, otherwise
    /// the git dir itself.
    pub fn common_dir(&self) -> &Path {
        &self.inner.common_dir
    }
    pub fn git(&self) -> &GitBin {
        &self.inner.git
    }
    /// Thread-local handle. Cheap; create one per worker thread and reuse it.
    pub fn handle(&self) -> Handle {
        let mut repo = self.inner.ts.to_thread_local();
        repo.object_cache_size_if_unset(16 * 1024 * 1024);
        // Shallow boundary commits have parents that are not in the object database; treat
        // them as roots, as git does. A broken `shallow` file is treated as "not shallow".
        let shallow = repo
            .shallow_commits()
            .ok()
            .flatten()
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        Handle { repo, owner: self.clone(), shallow, tree_diff_cache: std::cell::RefCell::new(None) }
    }
}

/// Thread-local view of a [`Repo`]; all read operations live here.
pub struct Handle {
    pub(crate) repo: gix::Repository,
    pub(crate) owner: Repo,
    pub(crate) shallow: std::collections::HashSet<gix::ObjectId>,
    /// Reused across tree diffs: building it costs ~10 ms (attribute stack) on large repos.
    pub(crate) tree_diff_cache: std::cell::RefCell<Option<gix::diff::blob::Platform>>,
}

impl Handle {
    #[allow(dead_code)]
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.repo
    }
    pub fn owner(&self) -> &Repo {
        &self.owner
    }

    /// Pays the one-time cost of the first tree diff (attribute stack, pack indices) by diffing
    /// HEAD against its parent. Best effort: errors and an unborn HEAD are ignored.
    pub fn warm(&self) {
        let Ok(id) = self.repo.head_id() else { return };
        let _ = self.commit_files(from_oid(&id.detach()), true);
    }

    /// The commit's parents as git sees them: none for a shallow boundary commit.
    pub(crate) fn parents_of(&self, c: &gix::Commit<'_>) -> smallvec::SmallVec<[gix::ObjectId; 2]> {
        if self.shallow.contains(&c.id) {
            return smallvec::SmallVec::new();
        }
        c.parent_ids().map(|p| p.detach()).collect()
    }

    /// The commit-graph, if enabled and readable. A corrupt graph is ignored (git warns and
    /// walks without it).
    pub(crate) fn commit_graph(&self) -> Option<gix::commitgraph::Graph> {
        if !self.shallow.is_empty() {
            return None;
        }
        self.gix().commit_graph_if_enabled().ok().flatten()
    }
}

#[allow(dead_code)]
pub(crate) fn to_oid(id: CommitId) -> gix::ObjectId {
    gix::ObjectId::from_bytes_or_panic(&id.0)
}

#[allow(dead_code)]
pub(crate) fn from_oid(oid: &gix::oid) -> CommitId {
    let mut b = [0u8; 20];
    b.copy_from_slice(oid.as_bytes());
    CommitId(b)
}
