mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::tune::{Action, Thresholds, apply, plan, untune};

const LOW: Thresholds = Thresholds { commits: 2, index_entries: 1 };

fn three_commits() -> Fixture {
    let f = Fixture::new();
    for i in 0..3 {
        f.write(&format!("f{i}.txt"), "x\n");
        f.commit(&format!("c{i}"), 1_700_000_000 + i);
    }
    f
}

fn local_config(f: &Fixture) -> String {
    f.git(&["config", "--local", "--list"])
}

#[test]
fn plan_follows_the_thresholds() {
    let f = three_commits();
    let h = Repo::open(f.path()).unwrap().handle();
    let mut want = vec![Action::CommitGraph, Action::Fsmonitor, Action::UntrackedCache];
    if !f.git(&["version", "--build-options"]).contains("feature: fsmonitor--daemon") {
        want.retain(|&a| a != Action::Fsmonitor);
    }
    assert_eq!(plan(&h, 3, LOW), want);
    assert_eq!(plan(&h, 3, Thresholds::DEFAULT), []);
}

/// git builds without the fsmonitor daemon (most Linux packages) cannot honour core.fsmonitor.
#[test]
fn fsmonitor_is_planned_only_where_git_has_the_daemon() {
    let f = three_commits();
    let h = Repo::open(f.path()).unwrap().handle();
    let has = f.git(&["version", "--build-options"]).contains("feature: fsmonitor--daemon");
    assert_eq!(plan(&h, 3, LOW).contains(&Action::Fsmonitor), has);
}

#[test]
fn user_settings_are_never_overridden() {
    let f = three_commits();
    f.git(&["config", "core.fsmonitor", "false"]);
    f.git(&["config", "core.untrackedCache", "keep"]);
    let h = Repo::open(f.path()).unwrap().handle();
    assert_eq!(plan(&h, 3, LOW), [Action::CommitGraph]);
}

#[test]
fn bare_repositories_are_left_alone() {
    let f = three_commits();
    let bare = f.add_bare_upstream();
    let h = Repo::open(&bare).unwrap().handle();
    assert_eq!(plan(&h, 3, LOW), []);
}

#[test]
fn apply_then_untune_restores_the_config_exactly() {
    let f = three_commits();
    let before = local_config(&f);
    let repo = Repo::open(f.path()).unwrap();
    let cli = GitCli::new(&repo);
    let h = repo.handle();
    let actions = plan(&h, 3, LOW);
    let done = apply(&h, &actions).unwrap();
    assert_eq!(done, actions);
    // fsmonitor only where git has the daemon (fsmonitor_is_planned_only_where_git_has_the_daemon)
    let fsmonitor = actions.contains(&Action::Fsmonitor);
    if fsmonitor {
        assert_eq!(f.git(&["config", "core.fsmonitor"]), "true");
    }
    assert_eq!(f.git(&["config", "core.untrackedCache"]), "true");
    assert!(f.path().join(".git/objects/info/commit-graph").exists() || f.path().join(".git/objects/info/commit-graphs").exists());
    let h = Repo::open(f.path()).unwrap().handle();
    assert_eq!(plan(&h, 3, LOW), [], "nothing left to do: the graph holds HEAD, the keys are set");
    let removed = untune(&cli).unwrap();
    let want: &[&str] = if fsmonitor { &["core.fsmonitor", "core.untrackedCache"] } else { &["core.untrackedCache"] };
    assert_eq!(removed, want);
    assert_eq!(local_config(&f), before);
    assert_eq!(untune(&cli).unwrap(), Vec::<String>::new(), "idempotent");
}

#[test]
fn a_stale_graph_is_rewritten() {
    let f = three_commits();
    let repo = Repo::open(f.path()).unwrap();
    apply(&repo.handle(), &[Action::CommitGraph]).unwrap();
    f.write("new.txt", "n\n");
    f.commit("after the graph", 1_700_000_100);
    let h = Repo::open(f.path()).unwrap().handle();
    assert_eq!(plan(&h, 4, Thresholds { commits: 2, index_entries: usize::MAX }), [Action::CommitGraph]);
}

#[test]
fn no_graph_when_the_repo_turned_it_off_or_is_shallow() {
    let f = three_commits();
    f.git(&["config", "core.commitGraph", "false"]);
    let h = Repo::open(f.path()).unwrap().handle();
    assert!(!plan(&h, 3, LOW).contains(&Action::CommitGraph), "core.commitGraph=false");
    let g = Fixture::new();
    let shallow = g.path().join("shallow");
    let out = std::process::Command::new("git").args(["clone", "-q", "--depth", "1"]).arg(format!("file://{}", f.path().display())).arg(&shallow).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    let h = Repo::open(&shallow).unwrap().handle();
    assert!(!plan(&h, 3, LOW).contains(&Action::CommitGraph), "shallow clones cannot use a graph");
}

#[test]
fn a_graph_is_reported_only_when_it_holds_head_and_layers_are_split() {
    let f = three_commits();
    let repo = Repo::open(f.path()).unwrap();
    assert_eq!(apply(&repo.handle(), &[Action::CommitGraph]).unwrap(), [Action::CommitGraph]);
    assert!(f.path().join(".git/objects/info/commit-graphs/commit-graph-chain").exists(), "written with --split");
    f.git(&["config", "core.commitGraph", "false"]);
    f.write("more.txt", "m\n");
    f.commit("more", 1_700_000_500);
    let repo = Repo::open(f.path()).unwrap();
    assert_eq!(apply(&repo.handle(), &[Action::CommitGraph]).unwrap(), [], "git wrote it, but gitty cannot use it: not reported");
}

#[test]
fn untune_leaves_keys_the_user_changed_since() {
    let f = three_commits();
    let repo = Repo::open(f.path()).unwrap();
    let cli = GitCli::new(&repo);
    apply(&repo.handle(), &[Action::UntrackedCache, Action::Fsmonitor]).unwrap();
    f.git(&["config", "core.untrackedCache", "keep"]);
    assert_eq!(untune(&cli).unwrap(), ["core.fsmonitor"]);
    assert_eq!(f.git(&["config", "core.untrackedCache"]), "keep", "the user's value stays");
    assert!(!local_config(&f).contains("gitty.tuned"), "the record is gone");
}
