//! A merge, rebase, cherry-pick or revert that stopped for the user (spec: conflict help): how
//! to tell, and how to finish or abort it. Every check re-reads the git dir, so a state started
//! or ended in another terminal is seen as it is.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::bail;

use crate::git_cli::{GitCli, Kind};
use crate::status::{EntryKind, Status};

/// Which operation stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoOp {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl RepoOp {
    /// The git subcommand, and what a sentence calls it.
    pub fn name(self) -> &'static str {
        match self {
            RepoOp::Merge => "merge",
            RepoOp::Rebase => "rebase",
            RepoOp::CherryPick => "cherry-pick",
            RepoOp::Revert => "revert",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpState {
    pub op: RepoOp,
    /// Unmerged entries of the status this was read with.
    pub conflicts: usize,
    /// The branch being merged or rebased; empty when unknown (and for a cherry-pick or revert).
    pub detail: String,
    /// A rebase's (current, total) step, when git says.
    pub step: Option<(usize, usize)>,
    /// Which one it is: MERGE_HEAD, the rebase's original HEAD, CHERRY_PICK_HEAD or REVERT_HEAD.
    /// A dialog that acted on a finished operation must not act on the next one of the same kind.
    pub id: String,
}

/// Staged files that still hold conflict markers (the paths): continuing would commit them.
#[derive(Debug, thiserror::Error)]
#[error("{} staged file(s) still contain conflict markers", .0.len())]
pub struct StagedMarkers(pub Vec<String>);

/// What [`GitCli::continue_op`] left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continued {
    Finished,
    /// The next step stopped too: with `conflicts` to resolve, or (0) for an edit or a break.
    Stopped { conflicts: usize },
    /// Nothing was in progress any more (finished or aborted elsewhere).
    Gone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aborted {
    Done,
    /// Nothing was in progress any more.
    Gone,
}

/// The paths asked of `rev-parse --git-path`, in this order.
const PATHS: [&str; 7] = ["MERGE_HEAD", "MERGE_MSG", "rebase-merge", "rebase-apply", "CHERRY_PICK_HEAD", "REVERT_HEAD", "sequencer"];
/// `git name-rev` on a huge history must not hold a status refresh.
const NAME_REV_BUDGET: Duration = Duration::from_secs(2);

impl GitCli {
    /// The operation in progress with the conflicts `status` shows. Reads a few files of the git
    /// dir; the git calls it makes (a name for the merged commit) are cheap or bounded.
    pub fn op_state(&self, status: &Status) -> Option<OpState> {
        let mut state = self.read_op(true)?;
        state.conflicts = status.entries.iter().filter(|e| e.kind == EntryKind::Unmerged).count();
        Some(state)
    }

    /// The operation in progress, whichever it is (rebase, then cherry-pick and revert, then
    /// merge). `named`: also find the merged branch's name, which the writer has no use for.
    fn read_op(&self, named: bool) -> Option<OpState> {
        let mut args = vec!["rev-parse"];
        for p in PATHS {
            args.extend(["--git-path", p]);
        }
        let out = self.quiet(Kind::Read, &args, None).ok()?;
        let out = String::from_utf8_lossy(&out);
        // relative to the directory git ran in; an absolute path replaces it
        let paths: Vec<PathBuf> = out.lines().map(|l| self.dir().join(l)).collect();
        let [merge_head, merge_msg, rebase_merge, rebase_apply, pick_head, revert_head, sequencer] = paths.as_slice() else { return None };
        let read = |dir: &PathBuf, name: &str| std::fs::read_to_string(dir.join(name)).ok().map(|s| s.trim().to_string());
        let first_line = |file: &PathBuf| std::fs::read_to_string(file).ok().and_then(|s| s.lines().next().map(|l| l.trim().to_string())).unwrap_or_default();
        let num = |dir: &PathBuf, name: &str| read(dir, name).and_then(|s| s.parse::<usize>().ok());
        let state = |op, detail: String, step, id: String| Some(OpState { op, conflicts: 0, detail, step, id });
        // `git am` shares rebase-apply; it is not an operation gitty can finish
        for (dir, now, total) in [(rebase_merge, "msgnum", "end"), (rebase_apply, "next", "last")] {
            if dir.is_dir() && !dir.join("applying").exists() {
                let branch = read(dir, "head-name").map(|h| h.strip_prefix("refs/heads/").unwrap_or(&h).to_string()).filter(|h| h != "detached HEAD").unwrap_or_default();
                let step = num(dir, now).zip(num(dir, total)).filter(|&(n, m)| n > 0 && m > 0);
                let id = read(dir, "orig-head").or_else(|| read(dir, "head")).unwrap_or_default();
                return state(RepoOp::Rebase, branch, step, id);
            }
        }
        if revert_head.exists() {
            return state(RepoOp::Revert, String::new(), None, first_line(revert_head));
        }
        if pick_head.exists() {
            return state(RepoOp::CherryPick, String::new(), None, first_line(pick_head));
        }
        // a multi-pick between two commits has no *_HEAD; its todo says which kind it is, and a
        // leftover directory with nothing to do is no operation
        if sequencer.is_dir() {
            let todo = std::fs::read_to_string(sequencer.join("todo")).unwrap_or_default();
            if let Some(first) = todo.lines().map(str::trim_start).find(|l| !l.is_empty() && !l.starts_with('#')) {
                let op = if first.starts_with("revert") { RepoOp::Revert } else { RepoOp::CherryPick };
                return state(op, String::new(), None, read(sequencer, "head").unwrap_or_default());
            }
        }
        if merge_head.exists() {
            let id = first_line(merge_head);
            let name = if named { self.merge_name(&id, merge_msg) } else { String::new() };
            return state(RepoOp::Merge, name, None, id);
        }
        None
    }

    /// What to call the commit being merged: a branch that points at it, else the name in the
    /// merge message, else a bounded `name-rev`, else its short id.
    fn merge_name(&self, id: &str, merge_msg: &PathBuf) -> String {
        if id.is_empty() {
            return String::new();
        }
        // exact and cheap; refs/heads sorts before refs/remotes
        let refs = self.quiet(Kind::Read, &["for-each-ref", "--points-at", id, "--format=%(refname)", "refs/heads", "refs/remotes"], None).unwrap_or_default();
        let refs = String::from_utf8_lossy(&refs);
        let branch = refs.lines().filter(|r| !r.ends_with("/HEAD")).find_map(|r| r.strip_prefix("refs/heads/").or_else(|| r.strip_prefix("refs/remotes/")));
        if let Some(b) = branch {
            return b.to_string();
        }
        // a tag or a bare commit: "Merge tag 'v1'"; "Merge commit '<sha>'" names nothing
        if let Some(n) = std::fs::read_to_string(merge_msg).ok().and_then(|m| quoted(m.lines().next()?).filter(|n| !is_hex_id(n)).map(str::to_string)) {
            return n;
        }
        let start = Instant::now();
        let cmd = self.cmd(Kind::Read, &["name-rev", "--name-only", "--no-undefined", "--refs=refs/heads/*", "--refs=refs/remotes/*", id]);
        let named = self.read_cancellable(cmd, &|| start.elapsed() > NAME_REV_BUDGET).ok().map(|o| String::from_utf8_lossy(&o).trim().to_string()).filter(|n| !n.is_empty());
        named.unwrap_or_else(|| id.chars().take(7).collect())
    }

    /// Staged files with conflict markers left in them, asked of git now (`diff --cached --check`,
    /// which also reports whitespace problems; those are not this).
    pub fn staged_conflict_markers(&self) -> anyhow::Result<Vec<String>> {
        let out = self.cmd(Kind::Read, &["diff", "--cached", "--check"]).output()?;
        // 2: problems were found, listed on stdout
        if !matches!(out.status.code(), Some(0 | 2)) {
            bail!("git diff --cached --check failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let mut files: Vec<String> = Vec::new();
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some((path, _)) = line.strip_suffix(": leftover conflict marker").and_then(|h| h.rsplit_once(':'))
                && !files.iter().any(|f| f == path)
            {
                files.push(path.to_string());
            }
        }
        Ok(files)
    }

    /// How many paths the index holds unmerged, asked of git now.
    fn unmerged_now(&self) -> anyhow::Result<usize> {
        let out = self.quiet(Kind::Read, &["ls-files", "-u", "-z"], None)?;
        let mut paths: Vec<&[u8]> = out.split(|&b| b == 0).filter_map(|r| r.splitn(2, |&b| b == b'\t').nth(1)).collect();
        paths.dedup();
        Ok(paths.len())
    }

    /// The operation is the one the caller saw: another one in its place, or the next of the same
    /// kind, is not theirs to finish. None: it is gone.
    fn expect_op(&self, op: RepoOp, id: &str) -> anyhow::Result<Option<OpState>> {
        match self.read_op(false) {
            Some(now) if now.op != op => bail!("a {} is in progress now, not a {}; look again", now.op.name(), op.name()),
            Some(now) if now.id != id => bail!("a different {} is in progress now; look again", op.name()),
            now => Ok(now),
        }
    }

    /// Finishes the step the operation stopped at, as `git commit` / `git <op> --continue` would,
    /// with no editor: the prepared message is used. `id` is the [`OpState::id`] the caller saw.
    /// Refuses while any path is unmerged, asked of git now, and with staged conflict markers in
    /// files other than `accepted` ([`StagedMarkers`]). A rebase or pick that runs into its next
    /// conflict is [`Continued::Stopped`], not an error. Hook output streams to `log`.
    pub fn continue_op(&self, op: RepoOp, id: &str, accepted: &[String], log: &mut dyn FnMut(&str)) -> anyhow::Result<Continued> {
        if self.expect_op(op, id)?.is_none() {
            return Ok(Continued::Gone);
        }
        let n = self.unmerged_now()?;
        if n > 0 {
            bail!("{n} file{} still conflict{}: resolve them and stage them first", if n == 1 { "" } else { "s" }, if n == 1 { "s" } else { "" });
        }
        let marked: Vec<String> = self.staged_conflict_markers()?.into_iter().filter(|f| !accepted.contains(f)).collect();
        if !marked.is_empty() {
            return Err(StagedMarkers(marked).into());
        }
        let args: &[&str] = match op {
            RepoOp::Merge => &["commit", "--no-edit"],
            RepoOp::Rebase => &["rebase", "--continue"],
            RepoOp::CherryPick => &["cherry-pick", "--continue"],
            RepoOp::Revert => &["revert", "--continue"],
        };
        // the environment beats `core.editor`, `sequence.editor` and the user's `$EDITOR`
        let mut cmd = self.cmd(Kind::Write, args);
        cmd.env("GIT_EDITOR", "true").env("GIT_SEQUENCE_EDITOR", "true");
        let ran = self.run(cmd, None, log);
        let open = self.read_op(false).is_some_and(|s| s.op == op);
        match (ran, open) {
            (Ok(_), false) => Ok(Continued::Finished),
            (Ok(_), true) if op == RepoOp::Merge => bail!("git stopped before committing the merge"),
            (Ok(_), true) => Ok(Continued::Stopped { conflicts: self.unmerged_now()? }),
            // the next commit of a rebase or pick conflicts: git exits 1 and leaves it open;
            // if that cannot be told, git's own error is the one to show
            (Err(e), true) if op != RepoOp::Merge => match self.unmerged_now() {
                Ok(conflicts) if conflicts > 0 => Ok(Continued::Stopped { conflicts }),
                _ => Err(e),
            },
            (Err(e), _) => Err(e),
        }
    }

    /// `git <op> --abort`, then checks the state is gone. Nothing in progress is [`Aborted::Gone`].
    pub fn abort_op(&self, op: RepoOp, id: &str) -> anyhow::Result<Aborted> {
        if self.expect_op(op, id)?.is_none() {
            return Ok(Aborted::Gone);
        }
        let mut cmd = self.cmd(Kind::Write, &[op.name(), "--abort"]);
        cmd.env("GIT_EDITOR", "true");
        let ran = self.run(cmd, None, &mut |_| {});
        if self.read_op(false).is_some() {
            return Err(match ran {
                Err(e) => e.context(format!("the {} could not be aborted", op.name())),
                Ok(_) => anyhow::anyhow!("git ran `{} --abort` but the {} is still in progress", op.name(), op.name()),
            });
        }
        // the abort worked, or the state ended just before it: either way it is gone
        Ok(Aborted::Done)
    }
}

/// The first `'quoted'` word of a merge message (`Merge branch 'topic' into main`).
fn quoted(line: &str) -> Option<&str> {
    let rest = &line[line.find('\'')? + 1..];
    Some(&rest[..rest.find('\'')?]).filter(|s| !s.is_empty())
}

fn is_hex_id(s: &str) -> bool {
    s.len() >= 7 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::quoted;

    #[test]
    fn the_merged_name_is_the_first_quoted_word() {
        assert_eq!(quoted("Merge branch 'topic'"), Some("topic"));
        assert_eq!(quoted("Merge remote-tracking branch 'origin/x' into main"), Some("origin/x"));
        assert_eq!(quoted("Merge commit 'abc1234'"), Some("abc1234"));
        assert_eq!(quoted("Merge stuff"), None);
        assert_eq!(quoted("Merge ''"), None);
    }
}
