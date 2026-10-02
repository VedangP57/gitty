# M1 — Core Read Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `gitty-core`'s read path. It opens a repo, lists refs and badges, streams history in commit-time order from the commit-graph (or the ODB when there is no graph), decodes rows lazily, computes ahead/behind with per-commit markers, and lists the changed files of a commit with lazy +/- stats. Everything is measured by a probe binary and criterion benches.

**Architecture:** This is a Cargo workspace with `crates/gitty-core` (a library; the only crate that touches gix) and `crates/gitty` (the TUI binary, an empty `main` in M1). gix types stay inside gitty-core. The public API uses `CommitId`, `CommitRow`, `RefsSnapshot`, `History`, `FileChange`, `LineStats` and `AheadBehind`. A `Repo` (Arc around `gix::ThreadSafeRepository`) is shared across threads, and each thread calls `repo.handle()` to get a thread-local `Handle`, which carries all the read methods.

**Tech Stack:** Rust 1.97, gix 0.88 (default features + `anyhow`), anyhow, thiserror, smallvec. Dev: tempfile, criterion 0.8.

**Spec:** `docs/superpowers/specs/2026-10-02-gitty-design.md` (§3, §4, §5.1–5.3, §8, §15)

## Global Constraints
- gix types must not appear in gitty-core's public API, except `Handle::gix()` which is `pub(crate)`.
- History order is commit-time (committer timestamp), newest first. **Never topo order.**
- History entries are stored as `u32`. A graph position is `< OVERFLOW_BIT`; `OVERFLOW_BIT | idx` indexes `History::overflow`.
- Writes are never done with gix (no index writes) in any milestone.
- The real git binary is resolved once. Skip `/usr/bin/git` (xcrun shim) when another git is on PATH.
- Release profile: `lto = "thin"`, `codegen-units = 1`.
- Commit messages: short imperative subject with no trailing period, ending with the `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>` trailer.

## Review Focus
1. **Repo with no commit-graph, or a stale one** (new commits made after the graph was written): history must still list every commit in the same order as `git log --all --date-order` with timestamp ties allowed. Covered by Task 4 tests `walk_without_graph` and `walk_with_stale_graph`.
2. **Unborn branch (fresh `git init`) and detached HEAD**: refs and history must not error. History is empty for unborn; detached HEAD walks from the HEAD commit. Covered by Task 3 tests.
3. **Branch with no upstream**: scope falls back to HEAD only and ahead/behind is `None`, not an error. Covered by Task 3/5 tests.
4. **Root commit and merge commit file lists**: root diffs against the empty tree, merge against the first parent. Covered by Task 6 tests.
5. **Binary file in line stats**: returns `LineStats { binary: true, .. }`, never garbage counts. Covered by Task 6 test.

---

### Task 1: Workspace skeleton + git binary resolution

**Files:**
- Create: `Cargo.toml` (workspace), `crates/gitty-core/Cargo.toml`, `crates/gitty-core/src/lib.rs`, `crates/gitty-core/src/git_bin.rs`, `crates/gitty/Cargo.toml`, `crates/gitty/src/main.rs`, `rust-toolchain.toml`

**Interfaces:**
- Produces: `gitty_core::git_bin::GitBin { path: PathBuf }`, `GitBin::resolve() -> GitBin`, `GitBin::command(&self) -> std::process::Command`, `pick_git(candidates: impl IntoIterator<Item=PathBuf>) -> Option<PathBuf>`

- [ ] **Step 1: Workspace files**

`Cargo.toml`:
```toml
[workspace]
resolver = "3"
members = ["crates/gitty-core", "crates/gitty"]

[workspace.package]
edition = "2024"
version = "0.1.0"
license = "MIT"
rust-version = "1.88"

[workspace.dependencies]
gix = { version = "0.88", features = ["anyhow"] }
anyhow = "1"
thiserror = "2"
smallvec = "1"

[profile.release]
lto = "thin"
codegen-units = 1
debug = 1

[profile.bench]
lto = "thin"
codegen-units = 1
debug = 1
```

`crates/gitty-core/Cargo.toml`:
```toml
[package]
name = "gitty-core"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
gix.workspace = true
anyhow.workspace = true
thiserror.workspace = true
smallvec.workspace = true

[dev-dependencies]
tempfile = "3"
```

`crates/gitty/Cargo.toml`:
```toml
[package]
name = "gitty"
edition.workspace = true
version.workspace = true
license.workspace = true

[dependencies]
gitty-core = { path = "../gitty-core" }
anyhow.workspace = true
```

`crates/gitty/src/main.rs`:
```rust
fn main() {
    println!("gitty {}", env!("CARGO_PKG_VERSION"));
}
```

- [ ] **Step 2: Failing test for `pick_git`**

`crates/gitty-core/src/git_bin.rs`:
```rust
//! Resolve the real `git` binary once. `/usr/bin/git` on macOS is an xcrun shim (+~4 ms/spawn).

use std::path::{Path, PathBuf};
use std::process::Command;

const SHIM: &str = "/usr/bin/git";

#[derive(Debug, Clone)]
pub struct GitBin {
    pub path: PathBuf,
}

/// First candidate that exists and is not the xcrun shim; else the shim if present.
pub fn pick_git(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut shim = None;
    for c in candidates {
        if !c.is_file() {
            continue;
        }
        if c == Path::new(SHIM) {
            shim.get_or_insert(c);
            continue;
        }
        return Some(c);
    }
    shim
}

impl GitBin {
    pub fn resolve() -> GitBin {
        let mut cands: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).map(|d| d.join("git")).collect())
            .unwrap_or_default();
        if let Ok(out) = Command::new("xcrun").args(["-f", "git"]).output() {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    cands.push(PathBuf::from(s));
                }
            }
        }
        cands.push(PathBuf::from(SHIM));
        GitBin { path: pick_git(cands).unwrap_or_else(|| PathBuf::from("git")) }
    }

    pub fn command(&self) -> Command {
        Command::new(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_non_shim() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("git");
        std::fs::write(&real, "").unwrap();
        let got = pick_git([PathBuf::from(SHIM), real.clone()]);
        // /usr/bin/git may or may not exist on the test machine; real must win either way
        assert_eq!(got, Some(real));
    }

    #[test]
    fn skips_missing() {
        assert_eq!(pick_git([PathBuf::from("/nonexistent/git")]), None);
    }

    #[test]
    fn resolve_runs_git() {
        let g = GitBin::resolve();
        let out = g.command().arg("--version").output().unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version"));
    }
}
```

`crates/gitty-core/src/lib.rs`:
```rust
//! gitty-core: the read/write engine behind gitty. The only crate that talks to gix.

pub mod git_bin;
```

- [ ] **Step 3: Run** `cargo test -p gitty-core`. Expected: 3 passed.
- [ ] **Step 4: Commit** with the message `Add workspace skeleton and git binary resolution`.

---

### Task 2: Core types, Repo/Handle, and test fixtures

**Files:**
- Create: `crates/gitty-core/src/types.rs`, `crates/gitty-core/src/repo.rs`, `crates/gitty-core/src/error.rs`, `crates/gitty-core/tests/common/mod.rs`, `crates/gitty-core/tests/repo.rs`
- Modify: `crates/gitty-core/src/lib.rs`

**Interfaces:**
- Produces:
  - `CommitId([u8; 20])`:
    - `from_hex(&str) -> Option<CommitId>`
    - `to_hex() -> String`
    - `short(n) -> String`
    - `Display`, `Hash`, `Ord`, `Copy`
  - `Signature { name: String, email: String, time: i64, offset_secs: i32 }`
  - `Repo::open(path: impl AsRef<Path>) -> anyhow::Result<Repo>`, `Repo: Clone + Send + Sync`
  - Paths and git binary:
    - `Repo::workdir() -> Option<&Path>`
    - `Repo::git_dir() -> &Path`
    - `Repo::git() -> &GitBin`
  - `Repo::handle() -> Handle`
  - `Handle::gix() -> &gix::Repository` (`pub(crate)`)
  - `GitError { args: Vec<String>, code: Option<i32>, stderr: String }` (thiserror, can be downcast from anyhow)
- Test fixture produced (`tests/common/mod.rs`):
  - `Fixture::new() -> Fixture` (tempdir + `git init -b main` with deterministic identity)
  - `Fixture::path() -> &Path`
  - `Fixture::git(&[&str]) -> String`
  - `Fixture::write(path, contents)`
  - `Fixture::commit(msg, epoch_secs) -> String (hex id)`
  - `Fixture::add_bare_upstream() -> PathBuf`

- [ ] **Step 1: types.rs**
```rust
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(pub [u8; 20]);

impl CommitId {
    pub fn from_hex(s: &str) -> Option<CommitId> {
        let b = s.as_bytes();
        if b.len() != 40 {
            return None;
        }
        let mut out = [0u8; 20];
        for i in 0..20 {
            let hi = (b[2 * i] as char).to_digit(16)?;
            let lo = (b[2 * i + 1] as char).to_digit(16)?;
            out[i] = (hi * 16 + lo) as u8;
        }
        Some(CommitId(out))
    }
    pub fn to_hex(&self) -> String {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(40);
        for b in self.0 {
            s.push(H[(b >> 4) as usize] as char);
            s.push(H[(b & 15) as usize] as char);
        }
        s
    }
    pub fn short(&self, n: usize) -> String {
        let mut h = self.to_hex();
        h.truncate(n.min(40));
        h
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl fmt::Debug for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitId({})", self.short(10))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since the Unix epoch.
    pub time: i64,
    pub offset_secs: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_roundtrip() {
        let h = "4f69cdad0123456789abcdef0123456789abcdef";
        let id = CommitId::from_hex(h).unwrap();
        assert_eq!(id.to_hex(), h);
        assert_eq!(id.short(7), "4f69cda");
        assert!(CommitId::from_hex("xyz").is_none());
    }
}
```
Internal conversion helpers live in `repo.rs`: `pub(crate) fn to_oid(id: CommitId) -> gix::ObjectId` and `pub(crate) fn from_oid(oid: &gix::oid) -> CommitId`.

- [ ] **Step 2: error.rs**
```rust
#[derive(Debug, thiserror::Error)]
#[error("git {args:?} failed (code {code:?}): {stderr}")]
pub struct GitError {
    pub args: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
}
```

- [ ] **Step 3: repo.rs**
```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;

use crate::git_bin::GitBin;
use crate::types::CommitId;

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

pub struct Handle {
    pub(crate) repo: gix::Repository,
    pub(crate) owner: Repo,
}

impl Handle {
    pub(crate) fn gix(&self) -> &gix::Repository {
        &self.repo
    }
    pub fn owner(&self) -> &Repo {
        &self.owner
    }
}

pub(crate) fn to_oid(id: CommitId) -> gix::ObjectId {
    gix::ObjectId::Sha1(id.0)
}
pub(crate) fn from_oid(oid: &gix::oid) -> CommitId {
    let mut b = [0u8; 20];
    b.copy_from_slice(oid.as_bytes());
    CommitId(b)
}
```
(If `gix::ObjectId::Sha1` is not a tuple variant in 0.88, use `gix::ObjectId::from_bytes_or_panic(&id.0)`.)

- [ ] **Step 4: lib.rs** exports
```rust
pub mod error;
pub mod git_bin;
pub mod repo;
pub mod types;

pub use error::GitError;
pub use repo::{Handle, Repo};
pub use types::{CommitId, Signature};
```

- [ ] **Step 5: test fixture** `crates/gitty-core/tests/common/mod.rs`
```rust
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    pub fn new() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let f = Fixture { dir };
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.name", "Test User"]);
        f.git(&["config", "user.email", "test@example.com"]);
        f.git(&["config", "commit.gpgsign", "false"]);
        f.git(&["config", "core.autocrlf", "false"]);
        f
    }
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
    pub fn git(&self, args: &[&str]) -> String {
        self.git_env(args, &[])
    }
    pub fn git_env(&self, args: &[&str], env: &[(&str, String)]) -> String {
        let mut c = Command::new("git");
        c.current_dir(self.path()).args(args);
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
    pub fn write(&self, rel: &str, contents: impl AsRef<[u8]>) {
        let p = self.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }
    /// Commit everything with a fixed author+committer time. Returns the full hex id.
    pub fn commit(&self, msg: &str, epoch: i64) -> String {
        self.git(&["add", "-A"]);
        let date = format!("{epoch} +0000");
        self.git_env(
            &["commit", "-q", "--allow-empty", "-m", msg],
            &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)],
        );
        self.git(&["rev-parse", "HEAD"])
    }
    /// Creates a bare clone as `origin`, pushes main, sets upstream. Returns the bare path.
    pub fn add_bare_upstream(&self) -> PathBuf {
        let bare = self.path().join("..").join(format!(
            "{}-origin.git",
            self.path().file_name().unwrap().to_string_lossy()
        ));
        let bare = std::fs::canonicalize(self.path().parent().unwrap()).unwrap().join(bare.file_name().unwrap());
        let out = Command::new("git").args(["init", "-q", "--bare", "-b", "main"]).arg(&bare).output().unwrap();
        assert!(out.status.success());
        self.git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        self.git(&["push", "-q", "-u", "origin", "main"]);
        bare
    }
}
```

- [ ] **Step 6: integration test** `crates/gitty-core/tests/repo.rs`
```rust
mod common;
use common::Fixture;
use gitty_core::Repo;

#[test]
fn opens_repo_and_subdir() {
    let f = Fixture::new();
    f.write("a/b.txt", "x\n");
    f.commit("init", 1_700_000_000);
    let r = Repo::open(f.path().join("a")).unwrap();
    assert_eq!(std::fs::canonicalize(r.workdir().unwrap()).unwrap(), std::fs::canonicalize(f.path()).unwrap());
    let _h = r.handle();
}

#[test]
fn rejects_non_repo() {
    let d = tempfile::tempdir().unwrap();
    assert!(Repo::open(d.path()).is_err());
}
```

- [ ] **Step 7: Run** `cargo test -p gitty-core`. Expected: all pass.
- [ ] **Step 8: Commit** with the message `Add core types, Repo handle, and test fixtures`.

---

### Task 3: Refs snapshot, HEAD state, upstream, history scope tips

**Files:**
- Create: `crates/gitty-core/src/refs.rs`, `crates/gitty-core/tests/refs.rs`

**Interfaces:**
- Produces:
```rust
pub enum RefKind { LocalBranch, RemoteBranch, Tag }
pub struct RefLabel { pub kind: RefKind, pub name: String /* short: "main", "origin/main", "v1.0" */, pub is_head: bool }
pub enum Head { Branch { name: String, id: Option<CommitId> /* None = unborn */ }, Detached { id: CommitId } }
pub struct RefsSnapshot {
    pub head: Head,
    pub upstream: Option<(String /* "origin/main" */, CommitId)>,
    pub labels: HashMap<CommitId, Vec<RefLabel>>,
    pub all_tips: Vec<CommitId>,  // every ref peeled to a commit + HEAD, deduped
}
pub enum HistoryScope { HeadAndUpstream, AllRefs }
impl RefsSnapshot {
    pub fn head_id(&self) -> Option<CommitId>;
    pub fn tips(&self, scope: HistoryScope) -> Vec<CommitId>;
}
impl Handle { pub fn refs(&self) -> anyhow::Result<RefsSnapshot>; }
```
- Label order per commit: the head branch first, then local branches, then remotes, then tags. Within each kind, names are sorted.

- [ ] **Step 1: Write failing tests** `tests/refs.rs`
```rust
mod common;
use common::Fixture;
use gitty_core::refs::{Head, HistoryScope, RefKind};
use gitty_core::Repo;

#[test]
fn unborn_branch() {
    let f = Fixture::new();
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    assert!(matches!(refs.head, Head::Branch { ref name, id: None } if name == "main"));
    assert!(refs.tips(HistoryScope::HeadAndUpstream).is_empty());
}

#[test]
fn labels_upstream_and_tags() {
    let f = Fixture::new();
    let c1 = f.commit("one", 1_700_000_000);
    f.add_bare_upstream();
    f.git(&["tag", "v1.0"]);
    f.git(&["tag", "-a", "v1.1", "-m", "annotated"]);
    let c2 = f.commit("two", 1_700_000_100);
    f.git(&["branch", "feature"]);
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    let id1 = gitty_core::CommitId::from_hex(&c1).unwrap();
    let id2 = gitty_core::CommitId::from_hex(&c2).unwrap();
    assert_eq!(refs.head_id(), Some(id2));
    assert_eq!(refs.upstream.as_ref().map(|u| (u.0.as_str(), u.1)), Some(("origin/main", id1)));
    let l2: Vec<_> = refs.labels[&id2].iter().map(|l| (l.kind, l.name.as_str(), l.is_head)).collect();
    assert_eq!(l2, vec![(RefKind::LocalBranch, "main", true), (RefKind::LocalBranch, "feature", false)]);
    let l1: Vec<_> = refs.labels[&id1].iter().map(|l| (l.kind, l.name.as_str())).collect();
    assert_eq!(l1, vec![(RefKind::RemoteBranch, "origin/main"), (RefKind::Tag, "v1.0"), (RefKind::Tag, "v1.1")]);
    let mut tips = refs.tips(HistoryScope::HeadAndUpstream);
    tips.sort();
    let mut exp = vec![id1, id2];
    exp.sort();
    assert_eq!(tips, exp);
}

#[test]
fn detached_head_and_no_upstream() {
    let f = Fixture::new();
    let c1 = f.commit("one", 1_700_000_000);
    f.commit("two", 1_700_000_100);
    f.git(&["checkout", "-q", "--detach", &c1]);
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    let id1 = gitty_core::CommitId::from_hex(&c1).unwrap();
    assert!(matches!(refs.head, Head::Detached { id } if id == id1));
    assert!(refs.upstream.is_none());
    assert_eq!(refs.tips(HistoryScope::HeadAndUpstream), vec![id1]);
    assert_eq!(refs.tips(HistoryScope::AllRefs).len(), 2);
}
```
`RefKind` derives `Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash`.

- [ ] **Step 2: Run.** It fails to compile because there is no `refs` module.
- [ ] **Step 3: Implement** `refs.rs`
```rust
use std::collections::HashMap;

use crate::repo::{from_oid, Handle};
use crate::types::CommitId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind { LocalBranch, RemoteBranch, Tag }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLabel { pub kind: RefKind, pub name: String, pub is_head: bool }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Head {
    Branch { name: String, id: Option<CommitId> },
    Detached { id: CommitId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryScope { HeadAndUpstream, AllRefs }

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
                if let Some((_, u)) = &self.upstream {
                    if !v.contains(u) {
                        v.push(*u);
                    }
                }
                v
            }
        }
    }
}

impl Handle {
    pub fn refs(&self) -> anyhow::Result<RefsSnapshot> {
        let repo = self.gix();
        let head_name = repo.head_name()?; // Option<FullName>
        let head_commit = repo.head_id().ok().map(|id| from_oid(&id));
        let head = match &head_name {
            Some(n) => Head::Branch { name: n.shorten().to_string(), id: head_commit },
            None => Head::Detached { id: head_commit.ok_or_else(|| anyhow::anyhow!("HEAD is detached but unresolvable"))? },
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
        for (kind, iter) in [
            (RefKind::LocalBranch, platform.local_branches()?),
            (RefKind::RemoteBranch, platform.remote_branches()?),
            (RefKind::Tag, platform.tags()?),
        ] {
            for r in iter {
                let Ok(mut r) = r else { continue };
                let name = r.name().shorten().to_string();
                if kind == RefKind::RemoteBranch && name.ends_with("/HEAD") {
                    continue;
                }
                let Ok(id) = r.peel_to_id() else { continue };
                // keep only refs that peel to commits (skip tags of trees/blobs)
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
```
Add `pub mod refs;` to lib.rs. If an exact gix method name differs in 0.88 (for example `peel_to_id` vs `peel_to_id_in_place`, or `shorten` on `FullNameRef`), fix it to the compiler's suggestion and keep the behaviour.

- [ ] **Step 4: Run** `cargo test -p gitty-core --test refs`. Expected: 3 passed.
- [ ] **Step 5: Commit** with the message `Add refs snapshot with badges, HEAD state, and upstream`.

---

### Task 4: History walker (commit-graph native + ODB hybrid) and lazy row decode

**Files:**
- Create: `crates/gitty-core/src/history.rs`, `crates/gitty-core/tests/history.rs`

**Interfaces:**
- Produces:
```rust
pub const OVERFLOW_BIT: u32 = 1 << 31;
pub struct History { entries: Vec<u32>, overflow: Vec<gix::ObjectId> /* private */, graph: Option<Arc<gix::commitgraph::Graph>> }
impl History {
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn id(&self, i: usize) -> CommitId;
    pub fn ids(&self, range: Range<usize>) -> Vec<CommitId>;
}
pub struct Walker { /* owns graph, heap, seen sets */ }
impl Handle {
    /// Start a commit-time-ordered walk from `tips`. Uses the commit-graph when enabled.
    pub fn walker(&self, tips: &[CommitId]) -> anyhow::Result<Walker>;
}
impl Walker {
    /// Appends up to `max` entries to `out`. Returns false when the walk is exhausted.
    pub fn step(&mut self, handle: &Handle, out: &mut History, max: usize) -> anyhow::Result<bool>;
    pub fn new_history(&self) -> History;   // empty History sharing the walker's graph
    pub fn uses_graph(&self) -> bool;
}
pub struct CommitRow {
    pub id: CommitId,
    pub parents: SmallVec<[CommitId; 2]>,
    pub summary: String,
    pub author: Signature,
    pub committer_time: i64,
    pub co_authors: Vec<(String, String)>,
}
pub struct CommitDetail { pub row: CommitRow, pub body: String, pub committer: Signature }
impl Handle {
    pub fn decode_row(&self, id: CommitId) -> anyhow::Result<CommitRow>;
    pub fn commit_detail(&self, id: CommitId) -> anyhow::Result<CommitDetail>;
}
pub fn parse_co_authors(message: &str) -> Vec<(String, String)>;
pub fn split_message(message: &str) -> (String /*summary*/, String /*body*/);
```

**Algorithm (walker):**
- **Heap:** a max-heap of `Item { time: i64, seq: u64, node: Node }`, ordered by `(time, Reverse(seq))`, so for equal timestamps the item pushed first pops first.
- **Nodes:** `Node::Graph(u32)` or `Node::Odb { id, parents: SmallVec<[ObjectId;2]> }`.
- **Tips:** if a tip has a graph position, push it as a Graph node with `committer_timestamp()`. Otherwise decode the commit from the ODB (committer time and parents) and push it as an Odb node.
- **Seen sets:** a `Vec<u64>` bitset over graph positions, plus a `HashSet<ObjectId>` for Odb nodes. Mark a node seen when it is pushed.
- **Pop:**
  - Append the entry: the graph position, or `OVERFLOW_BIT | overflow.len()` after pushing the id.
  - For each parent, push it as a Graph node if the graph contains it, otherwise as an Odb node. Use graph parents for Graph nodes and decoded parents for Odb nodes.
- **No graph (or `core.commitGraph=false`):** every node is Odb. The behaviour is the same, just slower.

- [ ] **Step 1: Failing tests** `tests/history.rs`
```rust
mod common;
use common::Fixture;
use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn build_history(f: &Fixture) -> Vec<String> {
    // main: a(100) - b(200) - d(400) - m(merge of feature, 500)
    // feature from b: c(300) - e(450)
    let mut ids = vec![];
    ids.push(f.commit("a", 1_700_000_100));
    ids.push(f.commit("b", 1_700_000_200));
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("f.txt", "feature\n");
    ids.push(f.commit("c", 1_700_000_300));
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "main\n");
    ids.push(f.commit("d", 1_700_000_400));
    f.git(&["checkout", "-q", "feature"]);
    f.write("f.txt", "feature2\n");
    ids.push(f.commit("e", 1_700_000_450));
    f.git(&["checkout", "-q", "main"]);
    f.git_env(
        &["merge", "-q", "--no-ff", "-m", "m", "feature"],
        &[("GIT_AUTHOR_DATE", "1700000500 +0000".into()), ("GIT_COMMITTER_DATE", "1700000500 +0000".into())],
    );
    ids.push(f.git(&["rev-parse", "HEAD"]));
    ids
}

fn walk_all(repo: &Repo) -> Vec<CommitId> {
    let h = repo.handle();
    let refs = h.refs().unwrap();
    let mut w = h.walker(&refs.tips(HistoryScope::AllRefs)).unwrap();
    let mut hist = w.new_history();
    while w.step(&h, &mut hist, 2).unwrap() {}
    hist.ids(0..hist.len())
}

fn git_order(f: &Fixture) -> Vec<CommitId> {
    f.git(&["log", "--all", "--date-order", "--format=%H"]).lines().map(|l| CommitId::from_hex(l).unwrap()).collect()
}

#[test]
fn walk_without_graph() {
    let f = Fixture::new();
    build_history(&f);
    let repo = Repo::open(f.path()).unwrap();
    assert!(!repo.handle().walker(&[]).unwrap().uses_graph());
    assert_eq!(walk_all(&repo), git_order(&f));
}

#[test]
fn walk_with_graph() {
    let f = Fixture::new();
    build_history(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    let repo = Repo::open(f.path()).unwrap();
    assert!(repo.handle().walker(&[]).unwrap().uses_graph());
    assert_eq!(walk_all(&repo), git_order(&f));
}

#[test]
fn walk_with_stale_graph() {
    let f = Fixture::new();
    build_history(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    f.commit("after graph 1", 1_700_000_600);
    f.commit("after graph 2", 1_700_000_700);
    let repo = Repo::open(f.path()).unwrap();
    let got = walk_all(&repo);
    assert_eq!(got.len(), 8);
    assert_eq!(got, git_order(&f));
}

#[test]
fn walk_empty_tips() {
    let f = Fixture::new();
    let repo = Repo::open(f.path()).unwrap();
    let h = repo.handle();
    let mut w = h.walker(&[]).unwrap();
    let mut hist = w.new_history();
    assert!(!w.step(&h, &mut hist, 100).unwrap());
    assert!(hist.is_empty());
}

#[test]
fn decode_row_fields() {
    let f = Fixture::new();
    let msg = "feat: thing\n\nBody line\n\nCo-authored-by: Ana B <ana@x.io>\nCo-Authored-By: Raj K <raj@y.io>";
    let id = CommitId::from_hex(&f.commit(msg, 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let row = h.decode_row(id).unwrap();
    assert_eq!(row.summary, "feat: thing");
    assert_eq!(row.author.name, "Test User");
    assert_eq!(row.author.time, 1_700_000_100);
    assert_eq!(row.committer_time, 1_700_000_100);
    assert!(row.parents.is_empty());
    assert_eq!(row.co_authors, vec![("Ana B".into(), "ana@x.io".into()), ("Raj K".into(), "raj@y.io".into())]);
    let d = h.commit_detail(id).unwrap();
    assert!(d.body.starts_with("Body line"));
}
```
Unit tests in `history.rs`:
```rust
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
}
```

- [ ] **Step 2: Run.** It fails to compile because there is no history module.
- [ ] **Step 3: Implement** `history.rs`
```rust
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use smallvec::SmallVec;

use crate::repo::{from_oid, to_oid, Handle};
use crate::types::{CommitId, Signature};

pub const OVERFLOW_BIT: u32 = 1 << 31;

pub struct History {
    entries: Vec<u32>,
    overflow: Vec<gix::ObjectId>,
    graph: Option<Arc<gix::commitgraph::Graph>>,
}

impl History {
    pub fn len(&self) -> usize { self.entries.len() }
    pub fn is_empty(&self) -> bool { self.entries.is_empty() }
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

struct Item { time: i64, seq: u64, node: Node }
impl PartialEq for Item { fn eq(&self, o: &Self) -> bool { self.cmp(o) == Ordering::Equal } }
impl Eq for Item {}
impl PartialOrd for Item { fn partial_cmp(&self, o: &Self) -> Option<Ordering> { Some(self.cmp(o)) } }
impl Ord for Item {
    fn cmp(&self, o: &Self) -> Ordering {
        self.time.cmp(&o.time).then_with(|| o.seq.cmp(&self.seq))
    }
}

pub struct Walker {
    graph: Option<Arc<gix::commitgraph::Graph>>,
    seen_graph: Vec<u64>,
    seen_odb: HashSet<gix::ObjectId>,
    heap: BinaryHeap<Item>,
    seq: u64,
}

impl Handle {
    pub fn walker(&self, tips: &[CommitId]) -> anyhow::Result<Walker> {
        let graph = self.gix().commit_graph_if_enabled()?.map(Arc::new);
        let n = graph.as_ref().map(|g| g.num_commits() as usize).unwrap_or(0);
        let mut w = Walker { graph, seen_graph: vec![0; n.div_ceil(64)], seen_odb: HashSet::new(), heap: BinaryHeap::new(), seq: 0 };
        for t in tips {
            w.push(self, to_oid(*t))?;
        }
        Ok(w)
    }
}

impl Walker {
    pub fn uses_graph(&self) -> bool { self.graph.is_some() }
    pub fn new_history(&self) -> History {
        History { entries: Vec::new(), overflow: Vec::new(), graph: self.graph.clone() }
    }

    fn push(&mut self, h: &Handle, id: gix::ObjectId) -> anyhow::Result<()> {
        if let Some(g) = &self.graph {
            if let Some(pos) = g.lookup(&id) {
                let p = pos.0;
                let (w, b) = ((p / 64) as usize, p % 64);
                if self.seen_graph[w] & (1 << b) != 0 { return Ok(()); }
                self.seen_graph[w] |= 1 << b;
                let time = g.commit_at(pos).committer_timestamp() as i64;
                self.seq += 1;
                self.heap.push(Item { time, seq: self.seq, node: Node::Graph(p) });
                return Ok(());
            }
        }
        if !self.seen_odb.insert(id) { return Ok(()); }
        let commit = h.gix().find_commit(id)?;
        let time = commit.committer()?.time()?.seconds;
        let parents: SmallVec<[gix::ObjectId; 2]> = commit.parent_ids().map(|p| p.detach()).collect();
        self.seq += 1;
        self.heap.push(Item { time, seq: self.seq, node: Node::Odb { id, parents } });
        Ok(())
    }

    pub fn step(&mut self, h: &Handle, out: &mut History, max: usize) -> anyhow::Result<bool> {
        let mut n = 0;
        while n < max {
            let Some(item) = self.heap.pop() else { return Ok(false) };
            match item.node {
                Node::Graph(p) => {
                    out.entries.push(p);
                    let g = self.graph.clone().expect("graph node without graph");
                    for parent in g.commit_at(gix::commitgraph::Position(p)).iter_parents() {
                        let pp = parent?;
                        let pid = g.id_at(pp).to_owned();
                        self.push(h, pid)?;
                    }
                }
                Node::Odb { id, parents } => {
                    out.entries.push(OVERFLOW_BIT | out.overflow.len() as u32);
                    out.overflow.push(id);
                    for pid in parents { self.push(h, pid)?; }
                }
            }
            n += 1;
        }
        Ok(!self.heap.is_empty())
    }
}
```
Note: pushing a Graph parent goes through `push()`, which does a `lookup` again. This is acceptable in M1. Optimise later by adding a `push_graph(pos)` fast path, measured in Task 7.

Row decode:
```rust
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
pub struct CommitDetail { pub row: CommitRow, pub body: String, pub committer: Signature }

fn sig(s: gix::actor::SignatureRef<'_>) -> anyhow::Result<Signature> {
    let t = s.time()?;
    Ok(Signature { name: s.name.to_string(), email: s.email.to_string(), time: t.seconds, offset_secs: t.offset })
}

pub fn split_message(message: &str) -> (String, String) {
    let m = message.trim_start_matches(['\n', '\r', ' ', '\t']);
    let mut it = m.splitn(2, '\n');
    let summary = it.next().unwrap_or("").trim_end().to_string();
    let body = it.next().unwrap_or("").trim_matches(['\n', '\r']).trim_end().to_string();
    (summary, body)
}

pub fn parse_co_authors(message: &str) -> Vec<(String, String)> {
    message
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            let rest = l.get(..15).filter(|p| p.eq_ignore_ascii_case("co-authored-by:")).map(|_| l[15..].trim())?;
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
        let author = sig(c.author()?)?;
        let committer = sig(c.committer()?)?;
        let parents = c.parent_ids().map(|p| from_oid(&p)).collect();
        let row = CommitRow { id, parents, summary, committer_time: committer.time, author, co_authors: parse_co_authors(&msg) };
        Ok((row, body, committer))
    }
    pub fn decode_row(&self, id: CommitId) -> anyhow::Result<CommitRow> {
        Ok(self.decode_full(id)?.0)
    }
    pub fn commit_detail(&self, id: CommitId) -> anyhow::Result<CommitDetail> {
        let (row, body, committer) = self.decode_full(id)?;
        Ok(CommitDetail { row, body, committer })
    }
}
```
(`time()` returning `Result` vs a plain field: adapt to the 0.88 compiler error. `offset` may be named `offset` in seconds.)

- [ ] **Step 4: Run** `cargo test -p gitty-core --test history` and `cargo test -p gitty-core --lib`. Expected: all pass. Ordering ties: the fixture uses distinct timestamps, so the order must match git exactly.
- [ ] **Step 5: Commit** with the message `Add commit-graph native history walker and lazy row decode`.

---

### Task 5: Ahead/behind with per-commit markers

**Files:**
- Create: `crates/gitty-core/src/ahead_behind.rs`, `crates/gitty-core/tests/ahead_behind.rs`

**Interfaces:**
- Produces:
```rust
pub struct AheadBehind { pub ahead: Vec<CommitId>, pub behind: Vec<CommitId> }
impl Handle { pub fn ahead_behind(&self, local: CommitId, upstream: CommitId) -> anyhow::Result<AheadBehind>; }
```
- `ahead` is the set of commits reachable from `local` but not `upstream` (↑ unpushed). `behind` is the reverse (↓ unpulled). Both are unordered sets returned as Vecs.

**Algorithm:**
- **Fast path (both tips in the commit-graph):**
  - Do a two-colour walk with a max-heap keyed by generation number. Flags: 1 = reachable from local, 2 = reachable from upstream, 4 = done, 8 = queued.
  - Pop the highest generation. Mark it done. If its colour is exactly 1 or exactly 2, record it.
  - Propagate its colours to its parents and queue them if they are not already queued. Colours are OR-merged in place.
  - Stop when the heap is empty, or when every queued item has colour 3 (checked every 256 pops).
- **Fallback (either tip not in the graph, or a generation is 0):** run `git rev-list --left-right local...upstream`. Lines starting with `<` are ahead and lines starting with `>` are behind.

- [ ] **Step 1: Failing test**
```rust
mod common;
use common::Fixture;
use gitty_core::{CommitId, Repo};
use std::collections::HashSet;

fn diverge(f: &Fixture) -> (CommitId, CommitId, HashSet<CommitId>, HashSet<CommitId>) {
    f.commit("base", 1_700_000_000);
    f.add_bare_upstream();
    let a1 = f.commit("local 1", 1_700_000_100);
    let a2 = f.commit("local 2", 1_700_000_200);
    // simulate unpulled commits: commit on a temp branch from origin/main and push it to origin
    f.git(&["checkout", "-q", "-b", "tmp", "origin/main"]);
    f.write("r.txt", "r\n");
    let b1 = f.commit("remote 1", 1_700_000_150);
    f.git(&["push", "-q", "origin", "tmp:main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "tmp"]);
    let id = |s: &str| CommitId::from_hex(s).unwrap();
    (id(&a2), id(&b1), [id(&a1), id(&a2)].into(), [id(&b1)].into())
}

fn check(f: &Fixture) {
    let (local, up, exp_a, exp_b) = diverge(f);
    let h = Repo::open(f.path()).unwrap().handle();
    let ab = h.ahead_behind(local, up).unwrap();
    assert_eq!(ab.ahead.into_iter().collect::<HashSet<_>>(), exp_a);
    assert_eq!(ab.behind.into_iter().collect::<HashSet<_>>(), exp_b);
}

#[test]
fn ahead_behind_fallback() {
    check(&Fixture::new());
}

#[test]
fn ahead_behind_graph() {
    let f = Fixture::new();
    let (local, up, exp_a, exp_b) = diverge(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    let h = Repo::open(f.path()).unwrap().handle();
    let ab = h.ahead_behind(local, up).unwrap();
    assert_eq!(ab.ahead.into_iter().collect::<HashSet<_>>(), exp_a);
    assert_eq!(ab.behind.into_iter().collect::<HashSet<_>>(), exp_b);
}

#[test]
fn equal_tips() {
    let f = Fixture::new();
    let c = CommitId::from_hex(&f.commit("x", 1_700_000_000)).unwrap();
    let ab = Repo::open(f.path()).unwrap().handle().ahead_behind(c, c).unwrap();
    assert!(ab.ahead.is_empty() && ab.behind.is_empty());
}
```
- [ ] **Step 2: Run.** It fails to compile.
- [ ] **Step 3: Implement**
```rust
use std::collections::BinaryHeap;

use anyhow::Context;

use crate::repo::{from_oid, to_oid, Handle};
use crate::types::CommitId;
use crate::GitError;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AheadBehind { pub ahead: Vec<CommitId>, pub behind: Vec<CommitId> }

const A: u8 = 1;
const B: u8 = 2;
const DONE: u8 = 4;
const QUEUED: u8 = 8;

impl Handle {
    pub fn ahead_behind(&self, local: CommitId, upstream: CommitId) -> anyhow::Result<AheadBehind> {
        if local == upstream {
            return Ok(AheadBehind::default());
        }
        if let Some(g) = self.gix().commit_graph_if_enabled()? {
            if let (Some(pa), Some(pb)) = (g.lookup(&to_oid(local)), g.lookup(&to_oid(upstream))) {
                let gen = |p: u32| g.commit_at(gix::commitgraph::Position(p)).generation();
                if gen(pa.0) > 0 && gen(pb.0) > 0 {
                    return graph_ahead_behind(&g, pa.0, pb.0);
                }
            }
        }
        self.cli_ahead_behind(local, upstream)
    }

    fn cli_ahead_behind(&self, local: CommitId, upstream: CommitId) -> anyhow::Result<AheadBehind> {
        let range = format!("{}...{}", local, upstream);
        let args = vec!["rev-list".to_string(), "--left-right".into(), range];
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
            return Err(GitError { args, code: out.status.code(), stderr: String::from_utf8_lossy(&out.stderr).into() }.into());
        }
        let mut ab = AheadBehind::default();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let (side, hex) = line.split_at(1);
            if let Some(id) = CommitId::from_hex(hex.trim()) {
                if side == "<" { ab.ahead.push(id) } else if side == ">" { ab.behind.push(id) }
            }
        }
        Ok(ab)
    }
}

fn graph_ahead_behind(g: &gix::commitgraph::Graph, pa: u32, pb: u32) -> anyhow::Result<AheadBehind> {
    let gen = |p: u32| g.commit_at(gix::commitgraph::Position(p)).generation();
    let mut flags = vec![0u8; g.num_commits() as usize];
    let mut heap: BinaryHeap<(u32, u32)> = BinaryHeap::new();
    flags[pa as usize] = A | QUEUED;
    flags[pb as usize] |= B | QUEUED;
    heap.push((gen(pa), pa));
    if pa != pb { heap.push((gen(pb), pb)); }
    let mut ab = AheadBehind::default();
    let mut pops = 0u32;
    while let Some((_, p)) = heap.pop() {
        let f = flags[p as usize];
        if f & DONE != 0 { continue; }
        flags[p as usize] |= DONE;
        let colour = f & (A | B);
        match colour {
            A => ab.ahead.push(from_oid(g.id_at(gix::commitgraph::Position(p)))),
            B => ab.behind.push(from_oid(g.id_at(gix::commitgraph::Position(p)))),
            _ => {}
        }
        for parent in g.commit_at(gix::commitgraph::Position(p)).iter_parents() {
            let pp = parent?.0 as usize;
            let old = flags[pp];
            flags[pp] = old | colour;
            if old & QUEUED == 0 {
                flags[pp] |= QUEUED;
                heap.push((gen(pp as u32), pp as u32));
            }
        }
        pops += 1;
        if pops % 256 == 0 && heap.iter().all(|&(_, q)| flags[q as usize] & (A | B) == A | B) {
            break;
        }
    }
    Ok(ab)
}
```
Also add the final stop check at every pop when the heap has 16 or fewer items, so small walks terminate early. This is an optional micro-optimisation; correctness does not depend on it.

Correctness note: a commit popped while it has colour A may later receive B through another path only if a child with a higher generation was still unprocessed. That cannot happen, because every child has a strictly higher generation and the max-heap pops all higher generations first. So colours are final at pop time.

- [ ] **Step 4: Run** `cargo test -p gitty-core --test ahead_behind`. Expected: 3 passed.
- [ ] **Step 5: Commit** with the message `Add ahead/behind with per-commit markers`.

---

### Task 6: Commit file list and lazy line stats

**Files:**
- Create: `crates/gitty-core/src/commit_files.rs`, `crates/gitty-core/tests/commit_files.rs`

**Interfaces:**
- Produces:
```rust
pub enum FileStatus { Added, Deleted, Modified, Renamed { similarity: Option<u8> }, Copied, TypeChange }
pub struct FileChange {
    pub path: String,               // new path (old path for deletions)
    pub old_path: Option<String>,   // Some for renames/copies
    pub status: FileStatus,
    pub old_id: Option<gix::ObjectId>, // pub(crate) accessors below instead of pub fields of gix type
    ...
}
```
gix types are not allowed in the public API, so blob ids are stored as `BlobId([u8;20])`:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)] pub struct BlobId(pub [u8; 20]);
pub struct FileChange {
    pub path: String, pub old_path: Option<String>, pub status: FileStatus,
    pub old_blob: Option<BlobId>, pub new_blob: Option<BlobId>,
    pub old_mode: u32, pub new_mode: u32, // git mode bits, 0 if absent
}
pub struct LineStats { pub added: u32, pub removed: u32, pub binary: bool }
impl Handle {
    pub fn commit_files(&self, commit: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>>;
    pub fn range_files(&self, oldest: CommitId, newest: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>>; // oldest^..newest
    pub fn line_stats(&self, change: &FileChange) -> anyhow::Result<LineStats>;
}
```
- File list order: by path, case-sensitive and bytewise, which matches git's default.
- Rules:
  - Submodule entries (mode 160000) are included with status Modified/Added/Deleted. `line_stats` returns 0/0 for them.
  - Trees are excluded.

- [ ] **Step 1: Failing tests**
```rust
mod common;
use common::Fixture;
use gitty_core::commit_files::FileStatus;
use gitty_core::{CommitId, Repo};

#[test]
fn root_commit_lists_added() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n");
    f.write("dir/b.txt", "x\n");
    let c = CommitId::from_hex(&f.commit("root", 1_700_000_000)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let files = h.commit_files(c, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| (x.path.as_str(), x.status.clone())).collect();
    assert_eq!(paths, vec![("a.txt", FileStatus::Added), ("dir/b.txt", FileStatus::Added)]);
    let s = h.line_stats(&files[0]).unwrap();
    assert_eq!((s.added, s.removed, s.binary), (2, 0, false));
}

#[test]
fn modify_delete_rename_binary() {
    let f = Fixture::new();
    f.write("keep.txt", "a\nb\nc\n");
    f.write("gone.txt", "bye\n");
    f.write("old_name.txt", "line1\nline2\nline3\nline4\nline5\n");
    f.write("img.bin", [0u8, 1, 2, 3, 0, 5]);
    f.commit("base", 1_700_000_000);
    f.write("keep.txt", "a\nB\nc\nd\n");
    std::fs::remove_file(f.path().join("gone.txt")).unwrap();
    f.git(&["mv", "old_name.txt", "new_name.txt"]);
    f.write("img.bin", [0u8, 9, 9, 9, 0, 5]);
    let c = CommitId::from_hex(&f.commit("change", 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();

    let files = h.commit_files(c, true).unwrap();
    let by = |p: &str| files.iter().find(|x| x.path == p).unwrap_or_else(|| panic!("missing {p}: {files:?}"));
    assert_eq!(by("keep.txt").status, FileStatus::Modified);
    assert_eq!(by("gone.txt").status, FileStatus::Deleted);
    assert!(matches!(by("new_name.txt").status, FileStatus::Renamed { .. }));
    assert_eq!(by("new_name.txt").old_path.as_deref(), Some("old_name.txt"));
    let ks = h.line_stats(by("keep.txt")).unwrap();
    assert_eq!((ks.added, ks.removed), (2, 1));
    assert!(h.line_stats(by("img.bin")).unwrap().binary);

    // without rename detection: delete + add
    let files = h.commit_files(c, false).unwrap();
    assert!(files.iter().any(|x| x.path == "old_name.txt" && x.status == FileStatus::Deleted));
    assert!(files.iter().any(|x| x.path == "new_name.txt" && x.status == FileStatus::Added));
}

#[test]
fn merge_diffs_against_first_parent() {
    let f = Fixture::new();
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("side.txt", "s\n");
    f.commit("side", 1_700_000_100);
    f.git(&["checkout", "-q", "main"]);
    f.write("main.txt", "m\n");
    f.commit("main", 1_700_000_200);
    f.git(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    let m = CommitId::from_hex(&f.git(&["rev-parse", "HEAD"])).unwrap();
    let files = Repo::open(f.path()).unwrap().handle().commit_files(m, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(paths, vec!["side.txt"]);
}

#[test]
fn range_files_spans_commits() {
    let f = Fixture::new();
    f.write("a.txt", "1\n");
    f.commit("one", 1_700_000_000);
    f.write("b.txt", "2\n");
    let c2 = CommitId::from_hex(&f.commit("two", 1_700_000_100)).unwrap();
    f.write("c.txt", "3\n");
    let c3 = CommitId::from_hex(&f.commit("three", 1_700_000_200)).unwrap();
    let files = Repo::open(f.path()).unwrap().handle().range_files(c2, c3, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(paths, vec!["b.txt", "c.txt"]);
}
```
- [ ] **Step 2: Run.** It fails to compile.
- [ ] **Step 3: Implement** `commit_files.rs`
```rust
use crate::repo::{to_oid, Handle};
use crate::types::CommitId;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BlobId(pub [u8; 20]);

impl BlobId {
    pub(crate) fn from_oid(o: &gix::oid) -> BlobId {
        let mut b = [0u8; 20];
        b.copy_from_slice(o.as_bytes());
        BlobId(b)
    }
    pub(crate) fn oid(&self) -> gix::ObjectId {
        gix::ObjectId::Sha1(self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus { Added, Deleted, Modified, Renamed { similarity: Option<u8> }, Copied, TypeChange }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub old_blob: Option<BlobId>,
    pub new_blob: Option<BlobId>,
    pub old_mode: u32,
    pub new_mode: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineStats { pub added: u32, pub removed: u32, pub binary: bool }

const SUBMODULE: u32 = 0o160000;

impl Handle {
    pub fn commit_files(&self, commit: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        let repo = self.gix();
        let c = repo.find_commit(to_oid(commit))?;
        let new_tree = c.tree()?;
        let old_tree = match c.parent_ids().next() {
            Some(p) => repo.find_commit(p.detach())?.tree()?,
            None => repo.empty_tree(),
        };
        self.diff_trees(&old_tree, &new_tree, detect_renames)
    }

    pub fn range_files(&self, oldest: CommitId, newest: CommitId, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        let repo = self.gix();
        let o = repo.find_commit(to_oid(oldest))?;
        let old_tree = match o.parent_ids().next() {
            Some(p) => repo.find_commit(p.detach())?.tree()?,
            None => repo.empty_tree(),
        };
        let new_tree = repo.find_commit(to_oid(newest))?.tree()?;
        self.diff_trees(&old_tree, &new_tree, detect_renames)
    }

    fn diff_trees(&self, old: &gix::Tree<'_>, new: &gix::Tree<'_>, detect_renames: bool) -> anyhow::Result<Vec<FileChange>> {
        use gix::object::tree::diff::Change;
        let mut out = Vec::new();
        let mut plat = old.changes()?;
        plat.options(|o| {
            o.track_path();
            o.track_rewrites(if detect_renames { Some(Default::default()) } else { None });
        });
        plat.for_each_to_obtain_tree(new, |ch| {
            if ch.entry_mode().is_tree() {
                return Ok::<_, gix::Exn>(gix::object::tree::diff::Action::Continue(()));
            }
            let fc = match ch {
                Change::Addition { location, entry_mode, id, .. } => FileChange {
                    path: location.to_string(), old_path: None, status: FileStatus::Added,
                    old_blob: None, new_blob: Some(BlobId::from_oid(&id)), old_mode: 0, new_mode: entry_mode.value() as u32,
                },
                Change::Deletion { location, entry_mode, id, .. } => FileChange {
                    path: location.to_string(), old_path: None, status: FileStatus::Deleted,
                    old_blob: Some(BlobId::from_oid(&id)), new_blob: None, old_mode: entry_mode.value() as u32, new_mode: 0,
                },
                Change::Modification { location, previous_entry_mode, previous_id, entry_mode, id, .. } => {
                    let tc = previous_entry_mode.kind() != entry_mode.kind();
                    FileChange {
                        path: location.to_string(), old_path: None,
                        status: if tc { FileStatus::TypeChange } else { FileStatus::Modified },
                        old_blob: Some(BlobId::from_oid(&previous_id)), new_blob: Some(BlobId::from_oid(&id)),
                        old_mode: previous_entry_mode.value() as u32, new_mode: entry_mode.value() as u32,
                    }
                }
                Change::Rewrite { source_location, source_entry_mode, source_id, entry_mode, id, location, copy, diff, .. } => FileChange {
                    path: location.to_string(), old_path: Some(source_location.to_string()),
                    status: if copy { FileStatus::Copied } else {
                        FileStatus::Renamed { similarity: diff.map(|d| (d.similarity * 100.0).round() as u8) }
                    },
                    old_blob: Some(BlobId::from_oid(&source_id)), new_blob: Some(BlobId::from_oid(&id)),
                    old_mode: source_entry_mode.value() as u32, new_mode: entry_mode.value() as u32,
                },
            };
            out.push(fc);
            Ok(gix::object::tree::diff::Action::Continue(()))
        })?;
        out.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        Ok(out)
    }

    pub fn line_stats(&self, ch: &FileChange) -> anyhow::Result<LineStats> {
        if ch.old_mode == SUBMODULE || ch.new_mode == SUBMODULE {
            return Ok(LineStats::default());
        }
        let load = |b: Option<BlobId>| -> anyhow::Result<Vec<u8>> {
            Ok(match b { Some(b) => self.gix().find_object(b.oid())?.data.clone(), None => Vec::new() })
        };
        let (old, new) = (load(ch.old_blob)?, load(ch.new_blob)?);
        if is_binary(&old) || is_binary(&new) {
            return Ok(LineStats { binary: true, ..Default::default() });
        }
        let (added, removed) = crate::diff_lines::count(&old, &new);
        Ok(LineStats { added, removed, binary: false })
    }
}

pub(crate) fn is_binary(d: &[u8]) -> bool {
    d[..d.len().min(8000)].contains(&0)
}
```
Create `crates/gitty-core/src/diff_lines.rs`. It is the line-count helper that M2's diff engine will grow into:
```rust
use gix::diff::blob::{Algorithm, Diff, InternedInput};

/// (added, removed) line counts with Myers + indent heuristic (git default).
pub fn count(old: &[u8], new: &[u8]) -> (u32, u32) {
    let input = InternedInput::new(old, new);
    let mut d = Diff::compute(Algorithm::Myers, &input);
    d.postprocess_lines(&input);
    (d.count_additions(), d.count_removals())
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts() {
        assert_eq!(super::count(b"a\nb\nc\n", b"a\nB\nc\nd\n"), (2, 1));
        assert_eq!(super::count(b"", b"x\n"), (1, 0));
    }
}
```
(If `gix::diff::blob` in 0.88 does not re-export the gix-imara-diff 0.3 `Diff` API, add `gix-imara-diff = "0.3"` directly as a workspace dependency, pinned to the version gix-diff uses (check `cargo tree -i gix-imara-diff`), and note it in the spec.)

Add `pub mod commit_files; pub mod diff_lines; pub mod ahead_behind; pub mod history;` to lib.rs.

- [ ] **Step 4: Run** `cargo test -p gitty-core`. Expected: all pass.
- [ ] **Step 5: Commit** with the message `Add commit file list with lazy line stats`.

---

### Task 7: Probe binary + criterion benches + budget check against git/git and linux

**Files:**
- Create: `crates/gitty-core/examples/probe.rs`, `crates/gitty-core/benches/core.rs`, `bench/run.sh`, `bench/README.md`
- Modify: `crates/gitty-core/Cargo.toml` (criterion dev-dep and a `[[bench]]` with `harness = false`)

**Interfaces:**
- Consumes everything above.
- The probe CLI: `probe walk <repo> [all|head]` prints `first500_ms`, `total_ms`, `count`, `uses_graph`; `probe ab <repo>` prints ahead/behind against the upstream; `probe files <repo> <n>` prints p50/p99 ms for the file lists of the first n commits; `probe rows <repo> <n>` prints µs per row decode.

- [ ] **Step 1: probe.rs**
```rust
use std::time::Instant;

use gitty_core::refs::HistoryScope;
use gitty_core::Repo;

fn ms(t: Instant) -> f64 { t.elapsed().as_secs_f64() * 1e3 }

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    let cmd = a.get(1).map(String::as_str).unwrap_or("walk");
    let path = a.get(2).map(String::as_str).unwrap_or(".");
    let t0 = Instant::now();
    let repo = Repo::open(path)?;
    let h = repo.handle();
    let refs = h.refs()?;
    let t_refs = ms(t0);
    match cmd {
        "walk" => {
            let scope = if a.get(3).map(String::as_str) == Some("head") { HistoryScope::HeadAndUpstream } else { HistoryScope::AllRefs };
            let tips = refs.tips(scope);
            let mut w = h.walker(&tips)?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, 500)?;
            let first = ms(t0);
            while w.step(&h, &mut hist, 65_536)? {}
            println!("walk refs={t_refs:.1}ms first500={first:.1}ms total={:.1}ms count={} graph={} tips={}", ms(t0), hist.len(), w.uses_graph(), tips.len());
        }
        "rows" => {
            let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let mut w = h.walker(&refs.tips(HistoryScope::AllRefs))?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, n)?;
            let t = Instant::now();
            for id in hist.ids(0..hist.len()) { std::hint::black_box(h.decode_row(id)?); }
            println!("rows n={} per_row={:.1}us", hist.len(), t.elapsed().as_secs_f64() * 1e6 / hist.len().max(1) as f64);
        }
        "files" => {
            let n: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let mut w = h.walker(&refs.tips(HistoryScope::HeadAndUpstream))?;
            let mut hist = w.new_history();
            w.step(&h, &mut hist, n)?;
            let mut times = vec![];
            let mut nfiles = 0;
            for id in hist.ids(0..hist.len()) {
                let t = Instant::now();
                nfiles += h.commit_files(id, false)?.len();
                times.push(ms(t));
            }
            times.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!("files commits={} files={} p50={:.3}ms p99={:.3}ms max={:.3}ms", times.len(), nfiles,
                times[times.len() / 2], times[(times.len() * 99 / 100).min(times.len() - 1)], times.last().unwrap());
        }
        "ab" => {
            let (Some(l), Some((name, u))) = (refs.head_id(), refs.upstream.clone()) else { anyhow::bail!("no upstream") };
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab vs {name}: ahead={} behind={} {:.2}ms", ab.ahead.len(), ab.behind.len(), ms(t));
        }
        "abrefs" => {
            let l = gitty_core::CommitId::from_hex(&rev(&repo, &a[3])?).unwrap();
            let u = gitty_core::CommitId::from_hex(&rev(&repo, &a[4])?).unwrap();
            let t = Instant::now();
            let ab = h.ahead_behind(l, u)?;
            println!("ab {}...{}: ahead={} behind={} {:.2}ms", a[3], a[4], ab.ahead.len(), ab.behind.len(), ms(t));
        }
        _ => anyhow::bail!("usage: probe walk|rows|files|ab|abrefs <repo> ..."),
    }
    Ok(())
}

fn rev(repo: &Repo, r: &str) -> anyhow::Result<String> {
    let out = repo.git().command().arg("-C").arg(repo.git_dir()).args(["rev-parse", &format!("{r}^{{commit}}")]).output()?;
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
```
- [ ] **Step 2: criterion bench** `benches/core.rs`. It reads `GITTY_BENCH_REPO` and skips when the variable is unset.
```rust
use criterion::{criterion_group, criterion_main, Criterion};
use gitty_core::refs::HistoryScope;
use gitty_core::Repo;

fn benches(c: &mut Criterion) {
    let Ok(path) = std::env::var("GITTY_BENCH_REPO") else { eprintln!("GITTY_BENCH_REPO unset; skipping"); return };
    let repo = Repo::open(&path).unwrap();
    let h = repo.handle();
    let refs = h.refs().unwrap();
    let tips = refs.tips(HistoryScope::AllRefs);
    c.bench_function("refs", |b| b.iter(|| h.refs().unwrap()));
    c.bench_function("walk_first_500", |b| b.iter(|| {
        let mut w = h.walker(&tips).unwrap();
        let mut hist = w.new_history();
        w.step(&h, &mut hist, 500).unwrap();
        hist.len()
    }));
    let mut w = h.walker(&tips).unwrap();
    let mut hist = w.new_history();
    w.step(&h, &mut hist, 200).unwrap();
    let ids = hist.ids(0..hist.len());
    c.bench_function("decode_row", |b| { let mut i = 0; b.iter(|| { i = (i + 1) % ids.len(); h.decode_row(ids[i]).unwrap() }) });
    c.bench_function("commit_files", |b| { let mut i = 0; b.iter(|| { i = (i + 1) % ids.len(); h.commit_files(ids[i], false).unwrap() }) });
}

criterion_group!(g, benches);
criterion_main!(g);
```
Add these to `crates/gitty-core/Cargo.toml`:
```toml
[dev-dependencies]
tempfile = "3"
criterion = "0.8"

[[bench]]
name = "core"
harness = false
```
- [ ] **Step 3: bench/run.sh**
```bash
#!/usr/bin/env bash
# Runs the probe against benchmark repos and prints results vs the spec §8 budget.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q -p gitty-core --example probe
P=target/release/examples/probe
for repo in "$@"; do
  echo "== $repo"
  "$P" walk "$repo" all
  "$P" walk "$repo" head
  "$P" rows "$repo" 500
  "$P" files "$repo" 300 || true
  "$P" ab "$repo" || true
done
echo "Budget: first500 < 50ms; full walk (kernel) < 400ms; files p50 < 5ms"
```
- [ ] **Step 4: Run it** on `$BENCH/git-cg` and `$BENCH/linux`. On linux, run `walk` and `ab`/`abrefs master v6.0` only, because blobless clones fetch blobs on demand. Record the numbers in `bench/README.md` with the date and machine. If first500 > 50 ms or the kernel full walk > 400 ms, profile with `samply` or with `cargo build --release` + Instruments, add the `push_graph(pos)` fast path, and re-measure.
- [ ] **Step 5: Commit** with the message `Add probe binary, criterion benches, and recorded baselines`.

---

## Self-review notes
- **Spec coverage:**
  - §5.1 walker, hybrid phase, scope and lazy decode: Tasks 3 and 4.
  - §5.2 ahead/behind and badges: Tasks 3 and 5.
  - §5.3 commit files, lazy stats, merges and ranges: Task 6.
  - §8 budget measurement: Task 7.
  - Search (§5.1) and caches (§5.5) belong to the M2 worker layer.
- **Types:** `CommitId`, `Handle` and `History` are used consistently across tasks. `BlobId` is introduced in Task 6 and reused by M2.
- **Error-type deviation from spec §13:** the public API returns `anyhow::Result` with downcastable `GitError`, instead of per-module thiserror enums, because gix 0.88's `Exn` errors convert cleanly only into anyhow. The spec has been updated to match.
