//! Branch writes (spec: branches and stash): switch, create, rename, delete. Each is one `git`
//! call through [`GitCli`], after the names pass [`GitCli::check_branch_name`].

use anyhow::bail;

use crate::GitError;
use crate::git_cli::{GitCli, Kind};

/// `delete_branch` without `force` refused: the branch has commits no other branch has.
pub fn is_unmerged(e: &anyhow::Error) -> bool {
    e.downcast_ref::<GitError>().is_some_and(|g| g.stderr.contains("not fully merged"))
}

impl GitCli {
    /// The checked-out branch; None when HEAD is detached.
    pub fn current_branch(&self) -> Option<String> {
        let out = self.quiet(Kind::Read, &["symbolic-ref", "-q", "--short", "HEAD"], None).ok()?;
        let s = String::from_utf8_lossy(&out).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// The commit HEAD points at, or None in a repository without commits.
    pub fn head_id(&self) -> Option<String> {
        let out = self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "HEAD"], None).ok()?;
        Some(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// `name` is a legal branch name that cannot be taken for an option or for `@{-1}`.
    pub fn check_branch_name(&self, name: &str) -> anyhow::Result<()> {
        if name.is_empty() || name.starts_with('-') || name.contains("@{") || self.quiet(Kind::Read, &["check-ref-format", "--branch", name], None).is_err() {
            bail!("`{name}` is not a valid branch name");
        }
        Ok(())
    }

    pub fn switch_branch(&self, name: &str) -> anyhow::Result<()> {
        self.check_branch_name(name)?;
        self.quiet(Kind::Write, &["switch", "-q", name], None).map(|_| ())
    }

    /// `origin/feature` → a new local `feature` that tracks it.
    pub fn switch_tracking(&self, remote_branch: &str) -> anyhow::Result<()> {
        self.check_branch_name(remote_branch)?;
        self.quiet(Kind::Write, &["switch", "-q", "--track", remote_branch], None).map(|_| ())
    }

    /// Creates `name` at `start` (HEAD when None) and switches to it.
    pub fn create_branch(&self, name: &str, start: Option<&str>) -> anyhow::Result<()> {
        self.check_branch_name(name)?;
        let mut args = vec!["switch", "-q", "-c", name];
        if let Some(s) = start {
            self.check_branch_name(s)?;
            args.push(s);
        }
        self.quiet(Kind::Write, &args, None).map(|_| ())
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> anyhow::Result<()> {
        self.check_branch_name(old)?;
        self.check_branch_name(new)?;
        self.quiet(Kind::Write, &["branch", "-m", old, new], None).map(|_| ())
    }

    /// `-d` refuses a branch with unmerged commits ([`is_unmerged`]); `force` is `-D`. The
    /// checked-out branch is never deleted.
    pub fn delete_branch(&self, name: &str, force: bool) -> anyhow::Result<()> {
        self.check_branch_name(name)?;
        if self.current_branch().as_deref() == Some(name) {
            bail!("`{name}` is checked out; switch to another branch first");
        }
        self.quiet(Kind::Write, &["branch", if force { "-D" } else { "-d" }, name], None).map(|_| ())
    }
}
