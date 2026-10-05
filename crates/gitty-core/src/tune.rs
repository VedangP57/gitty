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

fn value(cli: &GitCli, key: &str) -> Option<String> {
    cli.run(cli.cmd(Kind::Read, &["config", "--get", key]), None, &mut |_| {}).ok().map(|v| String::from_utf8_lossy(&v).trim().to_string())
}

/// A graph helps only where git and gix will read it: not shallow, not turned off.
fn graph_usable(h: &Handle, cli: &GitCli) -> bool {
    let off = cli.run(cli.cmd(Kind::Read, &["config", "--type=bool", "--get", "core.commitGraph"]), None, &mut |_| {}).is_ok_and(|v| v.trim_ascii() == b"false");
    !off && h.shallow.is_empty()
}

/// Whether a freshly opened repository finds HEAD in its commit-graph.
fn graph_holds_head(h: &Handle) -> bool {
    let repo = h.owner();
    let Ok(fresh) = crate::Repo::open(repo.workdir().unwrap_or(repo.git_dir())) else { return false };
    let fresh = fresh.handle();
    let head = fresh.repo.head_id().ok().map(|id| id.detach());
    matches!((fresh.commit_graph(), head), (Some(g), Some(id)) if g.lookup(id).is_some())
}

/// What this repository needs, in apply order. Empty for bare repositories.
pub fn plan(h: &Handle, history_len: usize, th: Thresholds) -> Vec<Action> {
    let repo = h.owner();
    if repo.workdir().is_none() {
        return Vec::new();
    }
    let cli = GitCli::new(repo);
    let mut v = Vec::new();
    if history_len >= th.commits && graph_usable(h, &cli) {
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

/// Applies `actions`; returns those that took effect. Config keys are recorded for [`untune`]
/// before they are set, so a key gitty set is never left unrecorded; a set that fails takes its
/// record back out.
pub fn apply(h: &Handle, actions: &[Action]) -> anyhow::Result<Vec<Action>> {
    let cli = GitCli::new(h.owner());
    let cli = &cli;
    let mut done = Vec::new();
    for &a in actions {
        match a.key() {
            Some(k) => {
                cli.run(cli.cmd(Kind::Write, &["config", "--local", "--add", RECORD, k]), None, &mut |_| {})?;
                if let Err(e) = cli.run(cli.cmd(Kind::Write, &["config", "--local", k, "true"]), None, &mut |_| {}) {
                    let _ = cli.run(cli.cmd(Kind::Write, &["config", "--local", "--fixed-value", "--unset-all", RECORD, k]), None, &mut |_| {});
                    return Err(e);
                }
                done.push(a);
            }
            None => {
                // --split: later writes add a small layer instead of rewriting the whole graph
                cli.run(cli.cmd(Kind::Write, &["commit-graph", "write", "--reachable", "--changed-paths", "--split"]), None, &mut |_| {})?;
                if graph_holds_head(h) {
                    done.push(a);
                }
            }
        }
    }
    Ok(done)
}

/// Unsets the keys gitty set (and nothing else); returns them.
pub fn untune(cli: &GitCli) -> anyhow::Result<Vec<String>> {
    let out = cli.run(cli.cmd(Kind::Read, &["config", "--local", "--get-all", RECORD]), None, &mut |_| {}).unwrap_or_default();
    let mut keys: Vec<String> = String::from_utf8_lossy(&out).lines().map(str::to_string).filter(|k| !k.is_empty()).collect();
    keys.dedup();
    // only keys still holding what gitty set: the user may have changed or removed them since
    keys.retain(|k| value(cli, k).as_deref() == Some("true"));
    for k in &keys {
        let _ = cli.run(cli.cmd(Kind::Write, &["config", "--local", "--unset-all", k]), None, &mut |_| {});
    }
    if is_set(cli, RECORD) {
        cli.run(cli.cmd(Kind::Write, &["config", "--local", "--unset-all", RECORD]), None, &mut |_| {})?;
        let _ = cli.run(cli.cmd(Kind::Write, &["config", "--local", "--remove-section", "gitty"]), None, &mut |_| {});
    }
    if keys.iter().any(|k| k == "core.fsmonitor") {
        let _ = cli.run(cli.cmd(Kind::Write, &["fsmonitor--daemon", "stop"]), None, &mut |_| {});
    }
    Ok(keys)
}
