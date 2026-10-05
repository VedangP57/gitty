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

/// Copies `file` into the Trash under a unique name. Missing files need no backup.
fn to_trash(file: &Path) -> anyhow::Result<()> {
    if !file.is_file() {
        return Ok(());
    }
    let dir = trash_dir().context("no Trash directory (HOME is unset)")?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = file.file_name().map_or_else(|| "file".into(), |n| n.to_string_lossy().into_owned());
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let mut dest = dir.join(format!("{name} (discarded by gitty {stamp})"));
    let mut k = 1;
    while dest.exists() {
        dest = dir.join(format!("{name} (discarded by gitty {stamp}-{k})"));
        k += 1;
    }
    std::fs::copy(file, &dest).with_context(|| format!("copying {} to the Trash", file.display()))?;
    Ok(())
}

fn workdir(h: &Handle) -> anyhow::Result<PathBuf> {
    h.owner().workdir().map(Path::to_path_buf).context("bare repository")
}

/// Runs one write. `log` receives git and hook output as it arrives. Returns the new HEAD for
/// [`WriteOp::Commit`] and the undone commit's message for [`WriteOp::UndoCommit`].
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
            if cli.head_blob(&entry.path)? != entry.head_blob {
                bail!("{} changed in HEAD since its diff was loaded; refreshing", entry.path);
            }
            match plan(entry, texts, &diff.ops, flags) {
                Plan::Nothing => {}
                Plan::StageFile(paths) => cli.stage_paths(&paths)?,
                Plan::UnstageFile(paths) => cli.unstage_paths(&paths)?,
                Plan::Patch { patch, expect, target } => cli.apply_cached(&patch, &entry.path, expect, target)?,
            }
        }
        WriteOp::WriteFile { path, bytes, expect, head } => {
            let full = workdir(h)?.join(path);
            let now = std::fs::read(&full).with_context(|| format!("reading {path}"))?;
            if BlobId::hash_of(&now) != *expect {
                bail!("{path} changed on disk since its diff was loaded; nothing was discarded");
            }
            if cli.head_blob(path)? != *head {
                bail!("{path} changed in HEAD since its diff was loaded; nothing was discarded");
            }
            to_trash(&full)?;
            // write in place so the file keeps its mode, owner and inode
            std::fs::write(&full, bytes).with_context(|| format!("writing {path}"))?;
        }
        WriteOp::DiscardFiles { restore, remove } => {
            let root = workdir(h)?;
            for p in restore.iter().chain(remove) {
                to_trash(&root.join(p))?;
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
        }
        WriteOp::Commit { message, amend } => {
            cli.commit(message, *amend, log)?;
            let head = cli.run(cli.cmd(gitty_core::git_cli::Kind::Read, &["rev-parse", "HEAD"]), None, log)?;
            return Ok(Some(String::from_utf8_lossy(&head).trim().to_string()));
        }
        WriteOp::UndoCommit { expect } => return Ok(Some(cli.undo_commit(expect)?)),
        WriteOp::Seq(ops) => {
            for op in ops {
                run(h, op, log)?;
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
