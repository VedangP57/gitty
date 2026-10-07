//! Merging a branch into the checked-out one (spec: branches and stash): one `git merge`, and a
//! merge that stops on conflicts is aborted again, so gitty never leaves the repository mid-merge.

use anyhow::{anyhow, bail};

use crate::git_cli::{GitCli, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    UpToDate,
    FastForward,
    /// A merge commit was made.
    Merged,
    /// The merge stopped on these files and was aborted: nothing changed.
    Conflicts(Vec<String>),
}

/// A merge failed and left the repository or the tree not as it was: nothing may be stacked on it.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MidMerge(pub String);

impl GitCli {
    /// Merges the local branch `name`, or with `remote` the remote-tracking branch `origin/x`,
    /// into the checked-out branch. Refuses before git runs on a detached HEAD, a branch into
    /// itself, and a merge, rebase, cherry-pick or revert already in progress; when git itself
    /// refuses (local changes would be overwritten) its message is the error and nothing changed.
    pub fn merge_branch(&self, name: &str, remote: bool) -> anyhow::Result<MergeOutcome> {
        self.merge_branch_logged(name, remote, &mut |_| {})
    }

    /// [`merge_branch`](Self::merge_branch), with git's and the hooks' stderr going to `log`.
    pub fn merge_branch_logged(&self, name: &str, remote: bool, log: &mut dyn FnMut(&str)) -> anyhow::Result<MergeOutcome> {
        self.check_branch_name(name)?;
        for (path, what) in [("MERGE_HEAD", "merge"), ("rebase-merge", "rebase"), ("rebase-apply", "rebase"), ("CHERRY_PICK_HEAD", "cherry-pick"), ("REVERT_HEAD", "revert")] {
            if self.git_path_exists(path) {
                bail!("a {what} is already in progress; finish or abort it in a terminal first");
            }
        }
        let Some(current) = self.current_branch() else { bail!("No branch checked out: switch to a branch first") };
        if !remote && current == name {
            bail!("`{name}` is the checked-out branch; it cannot be merged into itself");
        }
        // the full ref name, so a tag or another branch of the same name cannot be taken instead
        let target = format!("{}/{name}", if remote { "refs/remotes" } else { "refs/heads" });
        let tip = self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", &format!("{target}^{{commit}}")], None).map_err(|_| anyhow!("`{name}` is not a branch"))?;
        // the tip's id is passed, which no option or other ref can be mistaken for; git would name
        // the full ref in the message
        let tip = String::from_utf8_lossy(&tip).trim().to_string();
        let message = format!("Merge {} '{name}'", if remote { "remote-tracking branch" } else { "branch" });
        let Some(before) = self.head_id() else { bail!("`{current}` has no commits yet") };
        let tree = self.quiet(Kind::Read, &["status", "--porcelain=v1", "-z"], None)?;
        // `--commit --no-squash` beat a configured `branch.<name>.mergeOptions` of --no-commit or --squash
        let merge = self.cmd(Kind::Write, &["merge", "--no-edit", "--commit", "--no-squash", "-m", &message, &tip]);
        if let Err(e) = self.run(merge, None, log) {
            if !self.git_path_exists("MERGE_HEAD") {
                return Err(e);
            }
            // the unmerged paths are gone once the merge is aborted
            let files = self.quiet(Kind::Read, &["diff", "--name-only", "--diff-filter=U", "-z"], None)?;
            let files: Vec<String> = files.split(|&b| b == 0).filter(|f| !f.is_empty()).map(|f| String::from_utf8_lossy(f).into_owned()).collect();
            self.abort_merge(&before, &tree)?;
            // stopped without conflicts (a pre-merge-commit hook, say): git's own message
            return if files.is_empty() { Err(anyhow!("{e:#}; the merge was aborted")) } else { Ok(MergeOutcome::Conflicts(files)) };
        }
        if self.git_path_exists("MERGE_HEAD") {
            self.abort_merge(&before, &tree)?;
            bail!("git stopped before committing the merge; it was aborted");
        }
        if self.git_path_exists("SQUASH_MSG") {
            return Err(MidMerge("git squashed the merge instead of committing it; the changes are staged: check `git status`".into()).into());
        }
        let after = self.head_id();
        if after.as_ref() == Some(&before) {
            return Ok(MergeOutcome::UpToDate);
        }
        // a fast-forward leaves HEAD on the branch's own tip; `merge.ff=false` still makes a commit
        Ok(if after.as_deref() == Some(tip.as_str()) { MergeOutcome::FastForward } else { MergeOutcome::Merged })
    }

    /// `git merge --abort`, then checks HEAD and the status are what they were before the merge.
    fn abort_merge(&self, before: &str, tree: &[u8]) -> anyhow::Result<()> {
        if let Err(a) = self.quiet(Kind::Write, &["merge", "--abort"], None) {
            return Err(MidMerge(format!("the merge could not be aborted ({a:#}); the repository is mid-merge: run `git merge --abort` in a terminal")).into());
        }
        if self.head_id().as_deref() != Some(before) || self.quiet(Kind::Read, &["status", "--porcelain=v1", "-z"], None)? != tree {
            return Err(MidMerge("the merge was aborted but the working tree is not as it was before; check `git status`".into()).into());
        }
        Ok(())
    }

    /// `name` inside the git dir (`MERGE_HEAD`, `rebase-merge`) exists, in this worktree.
    fn git_path_exists(&self, name: &str) -> bool {
        let Ok(out) = self.quiet(Kind::Read, &["rev-parse", "--git-path", name], None) else { return false };
        // relative to the directory git ran in; an absolute path replaces it
        self.dir().join(String::from_utf8_lossy(&out).trim()).exists()
    }
}
