//! Writer-thread side of [`WriteOp`]s: every mutating git call and every worktree write.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use gitty_core::Handle;
use gitty_core::commit_files::BlobId;
use gitty_core::git_cli::GitCli;
use gitty_core::merge::{MergeOutcome, MidMerge};
use gitty_core::stage::{Plan, plan};

use crate::msg::WriteOp;

/// Held by every index or worktree write: the writer thread's ops, and the network thread's local
/// steps (fast-forward, merge, rebase), so they never race for `index.lock` (spec §12.1).
pub fn lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Where discarded files are copied first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trash {
    /// A plain directory: the macOS Trash, or `$GITTY_TRASH_DIR`.
    Dir(PathBuf),
    /// A freedesktop.org Trash (Linux desktops): each copy in `files/` with a `.trashinfo`
    /// record in `info/`, so the file manager lists it and can restore it.
    Xdg(PathBuf),
}

/// `$GITTY_TRASH_DIR`, else the desktop Trash: `~/.Trash` on macOS, `$XDG_DATA_HOME/Trash`
/// (`~/.local/share/Trash` by default) elsewhere. None without a home directory.
pub fn trash_location(get: impl Fn(&str) -> Option<OsString>, macos: bool) -> Option<Trash> {
    if let Some(d) = get("GITTY_TRASH_DIR") {
        return Some(Trash::Dir(d.into()));
    }
    let home = get("HOME").map(PathBuf::from);
    if macos {
        return home.map(|h| Trash::Dir(h.join(".Trash")));
    }
    // the spec ignores a relative XDG_DATA_HOME
    let data = get("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute()).or_else(|| home.map(|h| h.join(".local/share")))?;
    Some(Trash::Xdg(data.join("Trash")))
}

/// Copies `file` into `trash` under a name of its own; returns the copy's path.
pub fn copy_to_trash(file: &Path, trash: &Trash) -> anyhow::Result<PathBuf> {
    match trash {
        Trash::Dir(dir) => copy_into(file, dir),
        Trash::Xdg(root) => copy_into_xdg(file, root),
    }
}

/// The freedesktop.org Trash: the `.trashinfo` record is created first and exclusively, which
/// reserves the name; then the copy goes to `files/` under that name.
fn copy_into_xdg(file: &Path, root: &Path) -> anyhow::Result<PathBuf> {
    use std::io::Write;
    let (files, info) = (root.join("files"), root.join("info"));
    for d in [&files, &info] {
        std::fs::create_dir_all(d).with_context(|| format!("creating {}", d.display()))?;
    }
    let original = std::path::absolute(file).with_context(|| format!("resolving {}", file.display()))?;
    let name = file.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let record = format!("[Trash Info]\nPath={}\nDeletionDate={}\n", percent_encode(&original), local_timestamp());
    for k in 1.. {
        let candidate = if k == 1 { name.clone() } else { format!("{name}.{k}") };
        let dest = files.join(&candidate);
        if dest.exists() {
            continue;
        }
        let info_path = info.join(format!("{candidate}.trashinfo"));
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&info_path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("writing {}", info_path.display())),
        };
        f.write_all(record.as_bytes()).with_context(|| format!("writing {}", info_path.display()))?;
        if let Err(e) = std::fs::copy(file, &dest) {
            let _ = std::fs::remove_file(&info_path);
            return Err(e).with_context(|| format!("copying {} to {}", file.display(), files.display()));
        }
        return Ok(dest);
    }
    unreachable!("the name search above is unbounded")
}

/// A path as a `.trashinfo` `Path=` value: bytes outside the unreserved set are %XX-escaped.
fn percent_encode(p: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut s = String::new();
    for &b in p.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Local time as `YYYY-MM-DDThh:mm:ss`, the `.trashinfo` `DeletionDate` format.
fn local_timestamp() -> String {
    // SAFETY: time() with a null argument only returns; localtime_r writes into `tm` only
    let tm = unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// Where copies go when the Trash cannot be written (macOS refuses it without Full Disk Access).
fn fallback_trash_dir() -> PathBuf {
    crate::config::paths::state_dir(|k| std::env::var(k).ok()).join("trash")
}

/// Copies `file` into the Trash ([`trash_location`]) under a unique name, or into
/// [`fallback_trash_dir`] when the Trash is not writable; returns the fallback directory when it
/// was used. Missing files need no backup.
fn to_trash(file: &Path) -> anyhow::Result<Option<PathBuf>> {
    if !file.is_file() {
        return Ok(None);
    }
    let trash = trash_location(|k| std::env::var_os(k), cfg!(target_os = "macos"));
    let primary = trash.context("no Trash directory (HOME is unset)").and_then(|t| copy_to_trash(file, &t));
    match primary {
        Ok(_) => Ok(None),
        Err(e) => {
            let dir = fallback_trash_dir();
            copy_into(file, &dir).with_context(|| format!("{e:#}; and then"))?;
            Ok(Some(dir))
        }
    }
}

fn copy_into(file: &Path, dir: &Path) -> anyhow::Result<PathBuf> {
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
    Ok(dest)
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockId {
    path: PathBuf,
    ino: u64,
    mtime_ns: i128,
}

impl LockId {
    pub fn of(path: &Path) -> Option<LockId> {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(LockId { path: path.to_path_buf(), ino: m.ino(), mtime_ns: i128::from(m.mtime()) * 1_000_000_000 + i128::from(m.mtime_nsec()) })
    }

    /// The same lock file is still there (not removed, not replaced).
    pub fn still_there(&self) -> bool {
        LockId::of(&self.path).as_ref() == Some(self)
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

/// The worktree file's git-form blob now. A file that needed no conversion when its diff was
/// made is hashed as it is (no filter pipeline, which re-reads the whole index); otherwise the
/// conversion runs again.
fn worktree_blob(h: &Handle, entry: &gitty_core::status::StatusEntry, texts: &gitty_core::stage::Texts) -> anyhow::Result<BlobId> {
    let full = workdir(h)?.join(&entry.path);
    if texts.wt_is_raw && std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_file()) {
        let raw = std::fs::read(&full).with_context(|| format!("reading {}", entry.path))?;
        if BlobId::hash_of(&raw) == texts.wt_blob {
            return Ok(texts.wt_blob);
        }
    }
    Ok(h.stage_texts(entry)?.wt_blob)
}

fn workdir(h: &Handle) -> anyhow::Result<PathBuf> {
    h.owner().workdir().map(Path::to_path_buf).context("bare repository")
}

/// Pops the auto-stash (`pushed`, the `refs/stash` the push made) back, saying what became of
/// the changes. A stash list that moved since is left alone.
fn pop_back(cli: &GitCli, pushed: &Option<String>) -> String {
    if cli.stash_ref() != *pushed {
        return "the stash list changed, so your changes were left in the stash".to_string();
    }
    match cli.stash_pop_index(0).or_else(|_| cli.stash_pop(0)) {
        Ok(()) => "your changes were put back".to_string(),
        Err(p) => format!("putting your changes back failed ({p:#}); they are still in the stash (stash@{{0}})"),
    }
}

/// [`pop_back`] after an action that failed or changed nothing. None when HEAD or the branch
/// moved anyway: the changes stay in the stash.
fn put_back(cli: &GitCli, head: &Option<String>, branch: &Option<String>, pushed: &Option<String>) -> Option<String> {
    (cli.head_id() == *head && cli.current_branch() == *branch).then(|| pop_back(cli, pushed))
}

/// The notice for a merge of `name` into `into`.
fn merge_note(name: &str, into: &str, outcome: &MergeOutcome) -> String {
    match outcome {
        MergeOutcome::UpToDate => "Already up to date".into(),
        MergeOutcome::FastForward | MergeOutcome::Merged => format!("Merged {name} into {into}"),
        MergeOutcome::Conflicts(files) => {
            let shown = files.iter().take(3).map(String::as_str).collect::<Vec<_>>().join(", ");
            let more = files.len().saturating_sub(3);
            let more = if more > 0 { format!(" and {more} more") } else { String::new() };
            let n = files.len();
            format!("Merge of {name} has conflicts in {n} file{}: {shown}{more}. Nothing was changed - resolve in a terminal: git merge {name}", if n == 1 { "" } else { "s" })
        }
    }
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
            if worktree_blob(h, entry, texts)? != texts.wt_blob {
                bail!("{} changed on disk since its diff was loaded; refreshing", entry.path);
            }
            // a rename's HEAD side is the original path
            if h.head_blob(entry.orig_path.as_deref().unwrap_or(&entry.path))? != entry.head_blob {
                bail!("{} changed in HEAD since its diff was loaded; refreshing", entry.path);
            }
            let plan = plan(entry, texts, &diff.ops, flags);
            // a patch checks the index itself, just before applying
            if !matches!(plan, Plan::Patch { .. }) && h.index_entry_blob(&entry.path)? != entry.index_blob {
                bail!("{} changed in the index since its diff was loaded; refreshing", entry.path);
            }
            match plan {
                Plan::Nothing => {}
                Plan::StageFile(paths) => cli.stage_paths(&paths)?,
                Plan::UnstageFile(paths) => cli.unstage_paths(&paths)?,
                Plan::Patch { patch, expect, target } => h.apply_cached(&patch, &entry.path, expect, target)?,
            }
        }
        WriteOp::WriteFile { path, bytes, expect, head_path, head } => {
            let full = workdir(h)?.join(path);
            let now = std::fs::read(&full).with_context(|| format!("reading {path}"))?;
            if BlobId::hash_of(&now) != *expect {
                bail!("{path} changed on disk since its diff was loaded; nothing was discarded");
            }
            if h.head_blob(head_path)? != *head {
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
        WriteOp::SwitchBranch { name, remote: false } => cli.switch_branch(name)?,
        WriteOp::SwitchBranch { name, remote: true } => cli.switch_tracking(name)?,
        WriteOp::CreateBranch { name } => cli.create_branch(name, None)?,
        WriteOp::RenameBranch { old, new } => cli.rename_branch(old, new)?,
        WriteOp::DeleteBranch { name, force } => cli.delete_branch(name, *force)?,
        WriteOp::StashPush { message } => {
            if !cli.stash_push(message)? {
                return Ok(Some("Nothing to stash".into()));
            }
        }
        WriteOp::StashApply { index, expect } | WriteOp::StashPop { index, expect } | WriteOp::StashDrop { index, expect } => {
            if cli.stash_id_at(*index).as_deref() != Some(expect.as_str()) {
                bail!("the stash list changed; reopen it");
            }
            match op {
                WriteOp::StashApply { .. } => cli.stash_apply(*index)?,
                WriteOp::StashPop { .. } => cli.stash_pop(*index)?,
                _ => cli.stash_drop(*index)?,
            }
        }
        WriteOp::StashAndSwitch { name, remote, message } => {
            let (head, branch) = (cli.head_id(), cli.current_branch());
            let stashed = cli.stash_push(message)?;
            let pushed = cli.stash_ref();
            let switched = if *remote { cli.switch_tracking(name) } else { cli.switch_branch(name) };
            if let Err(e) = switched {
                if stashed {
                    match put_back(&cli, &head, &branch, &pushed) {
                        // git can fail after switching (a failing post-checkout hook); popping then would put the work on the wrong branch
                        None => bail!("the branch was switched but git reported a failure: {e:#}; your changes are in the stash (stash@{{0}})"),
                        Some(back) => bail!("{e:#}; {back}"),
                    }
                }
                return Err(e);
            }
        }
        WriteOp::Merge { name, remote } => {
            let into = cli.current_branch().unwrap_or_default();
            return Ok(Some(merge_note(name, &into, &cli.merge_branch_logged(name, *remote, log)?)));
        }
        WriteOp::StashAndMerge { name, remote, message } => {
            let (head, branch) = (cli.head_id(), cli.current_branch());
            let stashed = cli.stash_push(message)?;
            let pushed = cli.stash_ref();
            let merged = match cli.merge_branch_logged(name, *remote, log) {
                Ok(m) => m,
                Err(e) if stashed => {
                    // a merge that left the repository or the tree altered: nothing goes on top of it
                    if e.downcast_ref::<MidMerge>().is_some() {
                        bail!("{e:#}; your changes are in the stash (stash@{{0}})");
                    }
                    match put_back(&cli, &head, &branch, &pushed) {
                        None => bail!("the branch changed but git reported a failure: {e:#}; your changes are in the stash (stash@{{0}})"),
                        Some(back) => bail!("{e:#}; {back}"),
                    }
                }
                Err(e) => return Err(e),
            };
            let note = merge_note(name, branch.as_deref().unwrap_or_default(), &merged);
            if !stashed {
                return Ok(Some(note));
            }
            // the changes go back whether or not the merge moved HEAD: it is the same branch
            let tail = if cli.current_branch() == branch { pop_back(&cli, &pushed) } else { "your changes are in the stash (stash@{0})".to_string() };
            return Ok(Some(format!("{note}; {tail}")));
        }
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
