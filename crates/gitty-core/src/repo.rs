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
    workdir: Option<PathBuf>,
}

impl Repo {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Repo> {
        let path = path.as_ref();
        let ts = gix::ThreadSafeRepository::discover(path)
            .with_context(|| format!("not a git repository: {}", path.display()))?;
        let local = ts.to_thread_local();
        let git_dir = local.git_dir().to_path_buf();
        let workdir = local.workdir().map(Path::to_path_buf);
        Ok(Repo { inner: Arc::new(Inner { ts, git: GitBin::resolve(), git_dir, workdir }) })
    }
    pub fn workdir(&self) -> Option<&Path> {
        self.inner.workdir.as_deref()
    }
    pub fn git_dir(&self) -> &Path {
        &self.inner.git_dir
    }
    pub fn git(&self) -> &GitBin {
        &self.inner.git
    }
    /// Thread-local handle. Cheap; create one per worker thread and reuse it.
    pub fn handle(&self) -> Handle {
        let mut repo = self.inner.ts.to_thread_local();
        repo.object_cache_size_if_unset(16 * 1024 * 1024);
        Handle { repo, owner: self.clone() }
    }
}

/// Thread-local view of a [`Repo`]; all read operations live here.
pub struct Handle {
    pub(crate) repo: gix::Repository,
    pub(crate) owner: Repo,
}

impl Handle {
    #[allow(dead_code)]
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.repo
    }
    pub fn owner(&self) -> &Repo {
        &self.owner
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
