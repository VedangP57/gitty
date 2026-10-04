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
    assert_eq!(plan(&h, 3, LOW), [Action::CommitGraph, Action::Fsmonitor, Action::UntrackedCache]);
    assert_eq!(plan(&h, 3, Thresholds::DEFAULT), []);
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
    let done = apply(&cli, &actions).unwrap();
    assert_eq!(done, actions);
    assert_eq!(f.git(&["config", "core.fsmonitor"]), "true");
    assert_eq!(f.git(&["config", "core.untrackedCache"]), "true");
    assert!(f.path().join(".git/objects/info/commit-graph").exists() || f.path().join(".git/objects/info/commit-graphs").exists());
    let h = Repo::open(f.path()).unwrap().handle();
    assert_eq!(plan(&h, 3, LOW), [], "nothing left to do: the graph holds HEAD, the keys are set");
    let removed = untune(&cli).unwrap();
    assert_eq!(removed, ["core.fsmonitor", "core.untrackedCache"]);
    assert_eq!(local_config(&f), before);
    assert_eq!(untune(&cli).unwrap(), Vec::<String>::new(), "idempotent");
}

#[test]
fn a_stale_graph_is_rewritten() {
    let f = three_commits();
    let repo = Repo::open(f.path()).unwrap();
    apply(&GitCli::new(&repo), &[Action::CommitGraph]).unwrap();
    f.write("new.txt", "n\n");
    f.commit("after the graph", 1_700_000_100);
    let h = Repo::open(f.path()).unwrap().handle();
    assert_eq!(plan(&h, 4, Thresholds { commits: 2, index_entries: usize::MAX }), [Action::CommitGraph]);
}
