//! Branch writes (spec: branches and stash): switch, create, rename, delete. Each is one `git`
//! call through [`GitCli`], after the names pass [`GitCli::check_branch_name`].

use anyhow::bail;

use crate::GitError;
use crate::git_cli::{GitCli, Kind};

/// What an [`Unmerged`] says after the branch name.
pub const UNMERGED: &str = "has commits no other branch has";

/// `delete_branch` without `force` refused: the branch has commits no other branch has. Found
/// by asking git about ancestry, not by reading its (possibly translated) message.
#[derive(Debug, thiserror::Error)]
#[error("`{0}` {UNMERGED}")]
pub struct Unmerged(pub String);

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
        let id;
        if let Some(s) = start {
            id = self.resolve_commit(s)?;
            args.push(&id);
        }
        self.quiet(Kind::Write, &args, None).map(|_| ())
    }

    /// The commit a revision (branch, tag, `origin/x`, sha) names; never an option in disguise.
    fn resolve_commit(&self, rev: &str) -> anyhow::Result<String> {
        if rev.is_empty() || rev.starts_with('-') || rev.chars().any(|c| c.is_control() || c.is_whitespace()) {
            bail!("`{rev}` is not a valid start point");
        }
        let peeled = format!("{rev}^{{commit}}");
        match self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "--end-of-options", &peeled], None) {
            Ok(out) => Ok(String::from_utf8_lossy(&out).trim().to_string()),
            Err(_) => bail!("`{rev}` is not a commit"),
        }
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> anyhow::Result<()> {
        self.check_branch_name(old)?;
        self.check_branch_name(new)?;
        self.quiet(Kind::Write, &["branch", "-m", old, new], None).map(|_| ())
    }

    /// `-d` refuses a branch with unmerged commits ([`Unmerged`]); `force` is `-D`. The
    /// checked-out branch is never deleted.
    pub fn delete_branch(&self, name: &str, force: bool) -> anyhow::Result<()> {
        self.check_branch_name(name)?;
        if self.current_branch().as_deref() == Some(name) {
            bail!("`{name}` is checked out; switch to another branch first");
        }
        match self.quiet(Kind::Write, &["branch", if force { "-D" } else { "-d" }, name], None) {
            Err(_) if !force && self.unmerged(name) => Err(Unmerged(name.to_string()).into()),
            r => r.map(|_| ()),
        }
    }

    /// `name` is not an ancestor of what `branch -d` compares it with: its upstream, else HEAD.
    fn unmerged(&self, name: &str) -> bool {
        let branch = format!("refs/heads/{name}");
        let upstream = format!("{name}@{{upstream}}");
        let against = if self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", &upstream], None).is_ok() { upstream } else { "HEAD".to_string() };
        // exit 1 is "not an ancestor"; anything else (128) is an error, not an answer
        matches!(self.quiet(Kind::Read, &["merge-base", "--is-ancestor", &branch, &against], None), Err(e) if e.downcast_ref::<GitError>().is_some_and(|g| g.code == Some(1)))
    }
}
