//! Filesystem watching (spec §5.4): one FSEvents stream over the worktree (and the git dir when
//! it lives elsewhere), a path classifier, an ignore check and a debouncer. The owner gets a
//! [`Changed`] mask per burst and decides what to refresh.

use std::collections::HashMap;
use std::ops::{BitOr, BitOrAssign};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::Watcher as _;

use crate::repo::Repo;

/// What changed, as a bit set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Changed(pub u16);

impl Changed {
    pub const NONE: Changed = Changed(0);
    pub const WORKTREE: Changed = Changed(1);
    pub const INDEX: Changed = Changed(1 << 1);
    pub const REFS: Changed = Changed(1 << 2);
    pub const REMOTE: Changed = Changed(1 << 3);
    /// Merge, rebase, cherry-pick, revert or bisect in progress.
    pub const STATE: Changed = Changed(1 << 4);
    pub const CONFIG: Changed = Changed(1 << 5);
    pub const IGNORE_RULES: Changed = Changed(1 << 6);
    pub const STASH: Changed = Changed(1 << 7);
    pub const ALL: Changed = Changed(0xff);

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
    pub fn contains(self, o: Changed) -> bool {
        self.0 & o.0 == o.0
    }
    pub fn intersects(self, o: Changed) -> bool {
        self.0 & o.0 != 0
    }
}

impl BitOr for Changed {
    type Output = Changed;
    fn bitor(self, o: Changed) -> Changed {
        Changed(self.0 | o.0)
    }
}

impl BitOrAssign for Changed {
    fn bitor_assign(&mut self, o: Changed) {
        self.0 |= o.0;
    }
}

/// Class of a path relative to the git dir. Churn gitty does not show is `NONE`.
pub fn classify_git(rel: &Path) -> Changed {
    let s = rel.to_string_lossy();
    let s = s.as_ref();
    let first = s.split('/').next().unwrap_or("");
    if s.ends_with(".lock") || matches!(first, "objects" | "logs" | "hooks" | "lfs") || first.starts_with("fsmonitor--daemon") {
        return Changed::NONE;
    }
    match s {
        "index" => Changed::INDEX,
        "HEAD" | "packed-refs" => Changed::REFS,
        "FETCH_HEAD" => Changed::REMOTE,
        "config" => Changed::CONFIG,
        "info/exclude" => Changed::IGNORE_RULES,
        "refs/stash" => Changed::STASH,
        "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD" | "BISECT_LOG" | "MERGE_MSG" => Changed::STATE,
        _ if s.starts_with("refs/remotes/") => Changed::REMOTE,
        _ if s.starts_with("refs/") || s.starts_with("reftable/") => Changed::REFS,
        _ if matches!(first, "rebase-merge" | "rebase-apply" | "sequencer") => Changed::STATE,
        _ => Changed::NONE,
    }
}

/// Class of a worktree path (ignore rules are checked separately).
pub fn classify_worktree(rel: &Path) -> Changed {
    if rel.file_name().is_some_and(|n| n == ".gitignore") {
        Changed::WORKTREE | Changed::IGNORE_RULES
    } else {
        Changed::WORKTREE
    }
}

pub const QUIET: Duration = Duration::from_millis(50);
pub const MAX_WAIT: Duration = Duration::from_millis(300);

/// Fires [`QUIET`] after the last event of a burst, but never later than [`MAX_WAIT`] after its
/// first event.
#[derive(Debug, Default)]
pub struct Debouncer {
    first: Option<Instant>,
    last: Option<Instant>,
    pending: Changed,
}

impl Debouncer {
    pub fn add(&mut self, now: Instant, m: Changed) {
        if m.is_empty() {
            return;
        }
        self.first.get_or_insert(now);
        self.last = Some(now);
        self.pending |= m;
    }
    pub fn deadline(&self) -> Option<Instant> {
        Some((self.last? + QUIET).min(self.first? + MAX_WAIT))
    }
    pub fn take_due(&mut self, now: Instant) -> Option<Changed> {
        if self.deadline()? > now {
            return None;
        }
        let m = self.pending;
        *self = Debouncer::default();
        Some(m)
    }
}

/// Identity of the index file's current contents, cheap to take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexFingerprint {
    mtime_ns: i128,
    size: u64,
    ino: u64,
}

impl IndexFingerprint {
    pub fn of(path: &Path) -> Option<IndexFingerprint> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).ok()?;
        Some(IndexFingerprint { mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()), size: m.size(), ino: m.ino() })
    }
}

/// Lets status runs mark the index state they saw, so the watcher drops INDEX events for it.
#[derive(Clone)]
pub struct IndexMark {
    path: PathBuf,
    seen: Arc<Mutex<Option<IndexFingerprint>>>,
}

impl IndexMark {
    /// Call after gitty's own status run.
    pub fn note(&self) {
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) = IndexFingerprint::of(&self.path);
    }
    fn is_seen(&self) -> bool {
        let now = IndexFingerprint::of(&self.path);
        now.is_some() && *self.seen.lock().unwrap_or_else(|e| e.into_inner()) == now
    }
}

/// gitignore checks with a cache of directory verdicts (a build in an ignored directory sends
/// thousands of events).
struct Ignores {
    repo: gix::Repository,
    dirs: HashMap<PathBuf, bool>,
}

impl Ignores {
    fn new(repo: gix::Repository) -> Ignores {
        Ignores { repo, dirs: HashMap::new() }
    }
    fn reset(&mut self) {
        self.dirs.clear();
    }
    fn is_ignored(&mut self, rel: &Path) -> bool {
        let comps: Vec<_> = rel.components().collect();
        let dirs = || {
            comps[..comps.len().saturating_sub(1)].iter().scan(PathBuf::new(), |d, c| {
                d.push(c);
                Some(d.clone())
            })
        };
        // fast path: an ignored ancestor already known (a build in `target/`)
        for d in dirs() {
            match self.dirs.get(&d) {
                Some(true) => return true,
                Some(false) => continue,
                None => break,
            }
        }
        let Ok(index) = self.repo.index_or_empty() else { return false };
        let Ok(mut stack) = self.repo.excludes(&index, None, Default::default()) else { return false };
        if self.dirs.len() > 50_000 {
            self.dirs.clear();
        }
        for d in dirs() {
            let v = match self.dirs.get(&d) {
                Some(&v) => v,
                None => {
                    let v = stack.at_path(&d, Some(gix::index::entry::Mode::DIR)).is_ok_and(|p| p.is_excluded());
                    self.dirs.insert(d, v);
                    v
                }
            };
            if v {
                return true;
            }
        }
        stack.at_path(rel, None).is_ok_and(|p| p.is_excluded())
    }
}

/// Keeps the FSEvents stream alive; dropping it stops watching.
pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    mark: IndexMark,
}

impl Watcher {
    /// Starts watching. `on_change` runs on the watcher thread once per debounced burst.
    pub fn spawn(repo: &Repo, on_change: impl Fn(Changed) + Send + 'static) -> anyhow::Result<Watcher> {
        let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let git_dir = canon(repo.git_dir());
        let workdir = repo.workdir().map(canon);
        let mark = IndexMark { path: git_dir.join("index"), seen: Arc::new(Mutex::new(None)) };
        let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
        let mut inner = notify::recommended_watcher(move |e| {
            let _ = tx.send(e);
        })?;
        // every watch() restarts the stream, so register all roots before events matter
        if let Some(w) = &workdir {
            inner.watch(w, notify::RecursiveMode::Recursive)?;
        }
        if workdir.as_ref().is_none_or(|w| !git_dir.starts_with(w)) {
            inner.watch(&git_dir, notify::RecursiveMode::Recursive)?;
        }
        let ignores_repo = repo.handle().repo;
        let thread_mark = mark.clone();
        std::thread::Builder::new().name("gitty-watcher".into()).spawn(move || {
            let mut ignores = Ignores::new(ignores_repo);
            let mut deb = Debouncer::default();
            loop {
                let ev = match deb.deadline() {
                    Some(d) => match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                        Ok(e) => Some(e),
                        Err(mpsc::RecvTimeoutError::Timeout) => None,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    },
                    None => match rx.recv() {
                        Ok(e) => Some(e),
                        Err(_) => return,
                    },
                };
                if let Some(ev) = ev {
                    let mask = match ev {
                        Ok(ev) if ev.need_rescan() => Changed::ALL,
                        Ok(ev) => ev.paths.iter().fold(Changed::NONE, |m, p| m | classify(p, &git_dir, workdir.as_deref(), &mut ignores)),
                        Err(_) => Changed::ALL,
                    };
                    if mask.intersects(Changed::IGNORE_RULES) {
                        ignores.reset();
                    }
                    deb.add(Instant::now(), mask);
                }
                if let Some(mut m) = deb.take_due(Instant::now()) {
                    if m.contains(Changed::INDEX) && thread_mark.is_seen() {
                        m = Changed(m.0 & !Changed::INDEX.0);
                    }
                    if !m.is_empty() {
                        on_change(m);
                    }
                }
            }
        })?;
        Ok(Watcher { _inner: inner, mark })
    }

    pub fn index_mark(&self) -> IndexMark {
        self.mark.clone()
    }
}

fn classify(p: &Path, git_dir: &Path, workdir: Option<&Path>, ignores: &mut Ignores) -> Changed {
    if let Ok(rel) = p.strip_prefix(git_dir) {
        return classify_git(rel);
    }
    match workdir.and_then(|w| p.strip_prefix(w).ok()) {
        Some(rel) if rel.as_os_str().is_empty() => Changed::WORKTREE,
        Some(rel) if ignores.is_ignored(rel) => Changed::NONE,
        Some(rel) => classify_worktree(rel),
        None => Changed::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_dir_classes() {
        let c = |s: &str| classify_git(Path::new(s));
        assert_eq!(c("index"), Changed::INDEX);
        assert_eq!(c("index.lock"), Changed::NONE);
        assert_eq!(c("HEAD"), Changed::REFS);
        assert_eq!(c("refs/heads/main"), Changed::REFS);
        assert_eq!(c("refs/heads/main.lock"), Changed::NONE);
        assert_eq!(c("packed-refs"), Changed::REFS);
        assert_eq!(c("refs/remotes/origin/main"), Changed::REMOTE);
        assert_eq!(c("FETCH_HEAD"), Changed::REMOTE);
        assert_eq!(c("refs/stash"), Changed::STASH);
        assert_eq!(c("MERGE_HEAD"), Changed::STATE);
        assert_eq!(c("rebase-merge/done"), Changed::STATE);
        assert_eq!(c("config"), Changed::CONFIG);
        assert_eq!(c("info/exclude"), Changed::IGNORE_RULES);
        for ignored in ["objects/ab/cdef", "logs/HEAD", "COMMIT_EDITMSG", "AUTO_MERGE", "fsmonitor--daemon.ipc", "ORIG_HEAD", "hooks/pre-commit"] {
            assert_eq!(c(ignored), Changed::NONE, "{ignored}");
        }
        assert_eq!(classify_worktree(Path::new("src/.gitignore")), Changed::WORKTREE | Changed::IGNORE_RULES);
        assert_eq!(classify_worktree(Path::new("src/main.rs")), Changed::WORKTREE);
    }

    #[test]
    fn debouncer_waits_for_quiet_but_not_forever() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut d = Debouncer::default();
        assert_eq!(d.deadline(), None);
        d.add(t0, Changed::WORKTREE);
        assert_eq!(d.deadline(), Some(ms(50)));
        assert_eq!(d.take_due(ms(49)), None);
        d.add(ms(40), Changed::INDEX);
        assert_eq!(d.deadline(), Some(ms(90)), "extended by a new event");
        assert_eq!(d.take_due(ms(90)), Some(Changed::WORKTREE | Changed::INDEX));
        assert_eq!(d.deadline(), None);
        // a steady stream still fires at 300 ms
        for k in 0..20 {
            d.add(ms(1000 + k * 20), Changed::WORKTREE);
        }
        assert_eq!(d.deadline(), Some(ms(1300)));
        d.add(ms(1000), Changed::NONE);
    }
}
