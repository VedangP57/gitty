//! Writer-thread side of [`WriteOp`]s: every mutating git call and every worktree write.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use gitty_core::Handle;
use gitty_core::commit_files::BlobId;
use gitty_core::git_cli::GitCli;
use gitty_core::stage::{Plan, plan};

use crate::msg::WriteOp;

/// Held by every index or worktree write: the writer thread's ops, and the network thread's local
/// steps (fast-forward, merge, rebase), so they never race for `index.lock` (spec §12.1).
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where discarded files are copied first: `$GITTY_TRASH_DIR`, else the macOS Trash.
fn trash_dir() -> Option<PathBuf> {
    std::env::var_os("GITTY_TRASH_DIR").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".Trash")))
}

/// Where copies go when the Trash cannot be written (macOS refuses it without Full Disk Access).
fn fallback_trash_dir() -> PathBuf {
    crate::config::paths::state_dir(|k| std::env::var(k).ok()).join("trash")
}

/// Copies `file` into the Trash under a unique name, or into [`fallback_trash_dir`] when the
/// Trash is not writable; returns the fallback directory when it was used. Missing files need
/// no backup.
fn to_trash(file: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !file.is_file() {
        return Ok(None);
    }
    let primary = trash_dir().context("no Trash directory (HOME is unset)").and_then(|d| copy_into(file, &d));
    match primary {
        Ok(()) => Ok(None),
        Err(e) => {
            let dir = fallback_trash_dir();
            copy_into(file, &dir).with_context(|| format!("{e:#}; and then"))?;
            Ok(Some(dir))
        }
    }
}

fn copy_into(file: &Path, dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = file.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut dest = dir.join(format!("{name} (discarded by gitty {stamp})"));
    let mut k = 1;
    while dest.exists() {
        dest = dir.join(format!("{name} (discarded by gitty {stamp}-{k})"));
        k += 1;
    }
    std::fs::copy(file, &dest).with_context(|| format!("copying {} to {}", file.display(), dir.display()))?;
    Ok(())
}

/// The note a discard returns when its copies did not go to the Trash.
fn fallback_note(dir: Option<PathBuf>) -> Option<String> {
    dir.map(|d| format!("The Trash is not writable: copies of the discarded files are in {}", d.display()))
}

/// Some git process other than an fsmonitor daemon (which runs for days and never holds the
/// index lock) is running, by `pgrep -x git` (`$GITTY_PGREP` replaces pgrep in tests). pgrep
/// exits 1 when nothing matched; anything unexpected counts as running: a lock is never removed
/// on a guess.
pub fn git_running() -> bool {
    use std::process::{Command, Stdio};
    let prog = std::env::var_os("GITTY_PGREP").unwrap_or_else(|| "pgrep".into());
    let Ok(out) = Command::new(prog).args(["-x", "git"]).stderr(Stdio::null()).output() else { return true };
    match out.status.code() {
        Some(1) => return false,
        Some(0) => {}
        _ => return true,
    }
    let pids: Vec<String> = String::from_utf8_lossy(&out.stdout).split_whitespace().map(str::to_string).collect();
    let Ok(ps) = Command::new("ps").args(["-o", "command=", "-p", &pids.join(",")]).stderr(Stdio::null()).output() else { return true };
    any_live_git(&String::from_utf8_lossy(&ps.stdout))
}

/// `ps -o command=` lines of git processes: does any of them count? A git that exited since
/// pgrep leaves no line.
fn any_live_git(commands: &str) -> bool {
    // `--daemon` helpers (fsmonitor, credential-cache) run for minutes or days and never take
    // the index lock
    commands.lines().any(|l| !l.trim().is_empty() && !l.contains("--daemon"))
}

/// One particular lock file: removal is refused if the lock was replaced since it was offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockId {
    ino: u64,
    mtime_ns: i128,
}

impl LockId {
    pub fn of(path: &Path) -> Option<LockId> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(LockId { ino: m.ino(), mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()) })
    }
}

/// A failed write left `index.lock` behind and no git holds it: the lock to offer removing.
pub fn stale_index_lock(h: &Handle, error: &str) -> Option<LockId> {
    if !error.contains("index.lock") {
        return None;
    }
    let id = LockId::of(&h.owner().git_dir().join("index.lock"))?;
    (!git_running()).then_some(id)
}

fn workdir(h: &Handle) -> anyhow::Result<PathBuf> {
    h.owner().workdir().map(Path::to_path_buf).context("bare repository")
}

/// Runs one write. `log` receives git and hook output as it arrives. Returns the new HEAD for
/// [`WriteOp::Commit`], the undone commit's message for [`WriteOp::UndoCommit`], and for
/// discards a note saying where the copies went when that was not the Trash.
pub fn run(h: &Handle, op: &WriteOp, log: &mut dyn FnMut(&str)) -> anyhow::Result<Option<String>> {
    let cli = GitCli::new(h.owner());
    match op {
        WriteOp::Stage(paths) => cli.stage_paths(paths)?,
        WriteOp::Unstage(paths) => cli.unstage_paths(paths)?,
        WriteOp::StageAll => cli.stage_all()?,
        WriteOp::UnstageAll => cli.unstage_all()?,
        WriteOp::SetStaged { entry, texts, diff, flags } => {
            // whole-file plans run `git add`/`restore`, which take the file as it is now: refuse
            // if it is not what the user saw (an autosave or formatter in between)
            let now = h.stage_texts(entry)?;
            if now.wt_blob != texts.wt_blob {
                bail!("{} changed on disk since its diff was loaded; refreshing", entry.path);
            }
            if cli.index_blob(&entry.path)? != entry.index_blob {
                bail!("{} changed in the index since its diff was loaded; refreshing", entry.path);
            }
            // a rename's HEAD side is the original path
            if cli.head_blob(entry.orig_path.as_deref().unwrap_or(&entry.path))? != entry.head_blob {
                bail!("{} changed in HEAD since its diff was loaded; refreshing", entry.path);
            }
            match plan(entry, texts, &diff.ops, flags) {
                Plan::Nothing => {}
                Plan::StageFile(paths) => cli.stage_paths(&paths)?,
                Plan::UnstageFile(paths) => cli.unstage_paths(&paths)?,
                Plan::Patch { patch, expect, target } => cli.apply_cached(&patch, &entry.path, expect, target)?,
            }
        }
        WriteOp::WriteFile { path, bytes, expect, head_path, head } => {
            let full = workdir(h)?.join(path);
            let now = std::fs::read(&full).with_context(|| format!("reading {path}"))?;
            if BlobId::hash_of(&now) != *expect {
                bail!("{path} changed on disk since its diff was loaded; nothing was discarded");
            }
            if cli.head_blob(head_path)? != *head {
                bail!("{path} changed in HEAD since its diff was loaded; nothing was discarded");
            }
            let note = fallback_note(to_trash(&full)?);
            // write in place so the file keeps its mode, owner and inode
            std::fs::write(&full, bytes).with_context(|| format!("writing {path}"))?;
            return Ok(note);
        }
        WriteOp::DiscardFiles { restore, remove } => {
            let root = workdir(h)?;
            let mut fallback = None;
            for p in restore.iter().chain(remove) {
                fallback = to_trash(&root.join(p))?.or(fallback);
            }
            if !restore.is_empty() {
                let mut args = vec!["--literal-pathspecs", "restore", "--source=HEAD", "--staged", "--worktree", "--"];
                args.extend(restore.iter().map(String::as_str));
                let cmd = cli.cmd(gitty_core::git_cli::Kind::Write, &args);
                cli.run(cmd, None, log)?;
            }
            if !remove.is_empty() {
                let mut args = vec!["--literal-pathspecs", "rm", "--cached", "-f", "-q", "--ignore-unmatch", "--"];
                args.extend(remove.iter().map(String::as_str));
                let cmd = cli.cmd(gitty_core::git_cli::Kind::Write, &args);
                cli.run(cmd, None, log)?;
                for p in remove {
                    let full = root.join(p);
                    if full.is_file() || full.is_symlink() {
                        std::fs::remove_file(&full).with_context(|| format!("removing {p}"))?;
                    }
                }
            }
            return Ok(fallback_note(fallback));
        }
        WriteOp::Commit { message, amend } => {
            cli.commit(message, *amend, log)?;
            let head = cli.run(cli.cmd(gitty_core::git_cli::Kind::Read, &["rev-parse", "HEAD"]), None, log)?;
            return Ok(Some(String::from_utf8_lossy(&head).trim().to_string()));
        }
        WriteOp::UndoCommit { expect } => return Ok(Some(cli.undo_commit(expect)?)),
        WriteOp::Seq(ops) => {
            let mut note = None;
            for op in ops {
                note = run(h, op, log)?.or(note);
            }
            return Ok(note);
        }
        WriteOp::RemoveIndexLock { seen } => {
            if git_running() {
                bail!("a git process is running and may hold the lock; nothing was removed");
            }
            let lock = h.owner().git_dir().join("index.lock");
            if LockId::of(&lock).is_some_and(|now| now != *seen) {
                bail!("the index.lock changed since it was offered (another git took it); nothing was removed");
            }
            match std::fs::remove_file(&lock) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e).with_context(|| format!("removing {}", lock.display())),
                _ => {}
            }
        }
        WriteOp::RefreshIndex => {
            // exit 1 just means some files differ from the index; that is not a failure
            let mut cmd = cli.cmd(gitty_core::git_cli::Kind::Write, &["update-index", "-q", "--refresh"]);
            let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()?;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    #[test]
    fn fsmonitor_daemons_do_not_hold_the_lock() {
        let daemon = "/usr/libexec/git-core/git fsmonitor--daemon run --detach --ipc-threads=8\n";
        assert!(!super::any_live_git(daemon));
        assert!(!super::any_live_git(""));
        assert!(super::any_live_git(&format!("{daemon}git commit -q\n")));
        // the credential cache stays up for 15 minutes after a push and never touches the index
        assert!(!super::any_live_git("git credential-cache--daemon /Users/u/.cache/git/credential/socket\n"));
    }
}
