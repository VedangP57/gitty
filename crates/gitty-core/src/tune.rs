//! Auto-tuning for large repositories (spec §5.4): a commit-graph for the walker, and fsmonitor
//! plus the untracked cache for status. gitty never overrides a key the user set, records the
//! keys it sets in `gitty.tuned`, and `untune` removes exactly those.

use crate::Handle;
use crate::git_cli::{GitCli, Kind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `git commit-graph write --reachable --changed-paths` (missing, or HEAD not in it).
    CommitGraph,
    Fsmonitor,
    UntrackedCache,
}

impl Action {
    fn key(self) -> Option<&'static str> {
        match self {
            Action::CommitGraph => None,
            Action::Fsmonitor => Some("core.fsmonitor"),
            Action::UntrackedCache => Some("core.untrackedCache"),
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Action::CommitGraph => "commit-graph",
            Action::Fsmonitor => "fsmonitor",
            Action::UntrackedCache => "untracked cache",
        }
    }
}

/// When a repository counts as large.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thresholds {
    pub commits: usize,
    pub index_entries: usize,
}

impl Thresholds {
    pub const DEFAULT: Thresholds = Thresholds { commits: 10_000, index_entries: 20_000 };
}

const RECORD: &str = "gitty.tuned";

fn is_set(cli: &GitCli, key: &str) -> bool {
    cli.run(cli.cmd(Kind::Read, &["config", "--get", key]), None, &mut |_| {}).is_ok()
}

/// What this repository needs, in apply order. Empty for bare repositories.
pub fn plan(h: &Handle, history_len: usize, th: Thresholds) -> Vec<Action> {
    let repo = h.owner();
    if repo.workdir().is_none() {
        return Vec::new();
    }
    let cli = GitCli::new(repo);
    let mut v = Vec::new();
    if history_len >= th.commits {
        let head = h.repo.head_id().ok().map(|id| id.detach());
        let in_graph = match (h.commit_graph(), head) {
            (Some(g), Some(id)) => g.lookup(id).is_some(),
            (_, None) => true,
            (None, Some(_)) => false,
        };
        if !in_graph {
            v.push(Action::CommitGraph);
        }
    }
    let entries = h.repo.index_or_empty().map_or(0, |i| i.entries().len());
    if entries >= th.index_entries {
        for a in [Action::Fsmonitor, Action::UntrackedCache] {
            if a.key().is_some_and(|k| !is_set(&cli, k)) {
                v.push(a);
            }
        }
    }
    v
}

/// Applies `actions`; returns those that succeeded. Config keys are recorded for [`untune`].
pub fn apply(cli: &GitCli, actions: &[Action]) -> anyhow::Result<Vec<Action>> {
    let mut done = Vec::new();
    for &a in actions {
        match a.key() {
            Some(k) => {
                cli.run(cli.cmd(Kind::Write, &["config", "--local", k, "true"]), None, &mut |_| {})?;
                cli.run(cli.cmd(Kind::Write, &["config", "--local", "--add", RECORD, k]), None, &mut |_| {})?;
            }
            None => {
                cli.run(cli.cmd(Kind::Write, &["commit-graph", "write", "--reachable", "--changed-paths"]), None, &mut |_| {})?;
            }
        }
        done.push(a);
    }
    Ok(done)
}

/// Unsets the keys gitty set (and nothing else); returns them.
pub fn untune(cli: &GitCli) -> anyhow::Result<Vec<String>> {
    let out = cli.run(cli.cmd(Kind::Read, &["config", "--local", "--get-all", RECORD]), None, &mut |_| {}).unwrap_or_default();
    let mut keys: Vec<String> = String::from_utf8_lossy(&out).lines().map(str::to_string).filter(|k| !k.is_empty()).collect();
    keys.dedup();
    for k in &keys {
        // the user may have removed it already
        let _ = cli.run(cli.cmd(Kind::Write, &["config", "--local", "--unset-all", k]), None, &mut |_| {});
    }
    if !keys.is_empty() {
        cli.run(cli.cmd(Kind::Write, &["config", "--local", "--unset-all", RECORD]), None, &mut |_| {})?;
        let _ = cli.run(cli.cmd(Kind::Write, &["config", "--local", "--remove-section", "gitty"]), None, &mut |_| {});
    }
    if keys.iter().any(|k| k == "core.fsmonitor") {
        let _ = cli.run(cli.cmd(Kind::Write, &["fsmonitor--daemon", "stop"]), None, &mut |_| {});
    }
    Ok(keys)
}
