//! Stash (spec: branches and stash): push, list, apply, pop, drop, each one `git stash` call.

use anyhow::anyhow;

use crate::git_cli::{GitCli, Kind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StashEntry {
    /// `n` of `stash@{n}`; 0 is the newest.
    pub index: usize,
    /// The branch it was made on.
    pub branch: String,
    pub message: String,
}

/// `git stash list --format=%gd%x1f%gs`: one `stash@{n}` and one subject per line, which reads
/// `On <branch>: <message>` or `WIP on <branch>: <hash> <subject>`.
pub fn parse_list(out: &str) -> Vec<StashEntry> {
    out.lines()
        .filter_map(|l| {
            let (gd, gs) = l.split_once('\x1f')?;
            let index = gd.strip_prefix("stash@{")?.strip_suffix('}')?.parse().ok()?;
            let (branch, message) = gs
                .strip_prefix("WIP on ")
                .or_else(|| gs.strip_prefix("On "))
                .and_then(|rest| rest.split_once(": "))
                .map_or((String::new(), gs.to_string()), |(b, m)| (b.to_string(), m.to_string()));
            Some(StashEntry { index, branch, message })
        })
        .collect()
}

impl GitCli {
    pub fn stash_list(&self) -> anyhow::Result<Vec<StashEntry>> {
        let out = self.quiet(Kind::Read, &["stash", "list", "--format=%gd%x1f%gs"], None)?;
        Ok(parse_list(&String::from_utf8_lossy(&out)))
    }

    /// Stashes tracked and untracked changes under `message`. False when there was nothing to
    /// stash (git says so and exits 0); detected by `refs/stash` moving, not by git's wording.
    pub fn stash_push(&self, message: &str) -> anyhow::Result<bool> {
        let before = self.stash_ref();
        self.quiet(Kind::Write, &["stash", "push", "-u", "-m", message], None)?;
        Ok(self.stash_ref() != before)
    }

    /// The commit `refs/stash` points at, or None when there is no stash.
    fn stash_ref(&self) -> Option<String> {
        let out = self.quiet(Kind::Read, &["rev-parse", "-q", "--verify", "refs/stash"], None).ok()?;
        Some(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Applies `stash@{index}`, keeping the entry. Conflicts leave their markers and the entry.
    pub fn stash_apply(&self, index: usize) -> anyhow::Result<()> {
        self.stash_verb("apply", index)
    }

    /// Applies and drops `stash@{index}`; when it stops on conflicts the entry is kept.
    pub fn stash_pop(&self, index: usize) -> anyhow::Result<()> {
        self.stash_verb("pop", index)
    }

    pub fn stash_drop(&self, index: usize) -> anyhow::Result<()> {
        self.quiet(Kind::Write, &["stash", "drop", "-q", &format!("stash@{{{index}}}")], None).map(|_| ())
    }

    fn stash_verb(&self, verb: &str, index: usize) -> anyhow::Result<()> {
        self.quiet(Kind::Write, &["stash", verb, "-q", &format!("stash@{{{index}}}")], None).map(|_| ()).map_err(|e| {
            let kept = self.stash_list().is_ok_and(|l| l.iter().any(|s| s.index == index));
            anyhow!("`git stash {verb}` failed: {e:#}{}", if kept { "; the stash was kept" } else { "" })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_subject_shapes() {
        let out = "stash@{0}\x1fOn main: wip parser\nstash@{1}\x1fWIP on topic: 1a2b3c4 some subject\nstash@{2}\x1ffix: thing\nnoise\n";
        assert_eq!(
            parse_list(out),
            [
                StashEntry { index: 0, branch: "main".into(), message: "wip parser".into() },
                StashEntry { index: 1, branch: "topic".into(), message: "1a2b3c4 some subject".into() },
                StashEntry { index: 2, branch: String::new(), message: "fix: thing".into() },
            ]
        );
    }
}
