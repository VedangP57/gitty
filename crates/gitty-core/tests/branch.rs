mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::branch::is_unmerged;
use gitty_core::git_cli::GitCli;

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn base() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f
}

fn current(f: &Fixture) -> String {
    f.git(&["branch", "--show-current"])
}

#[test]
fn switch_to_an_existing_branch() {
    let f = base();
    f.git(&["branch", "topic"]);
    cli(&f).switch_branch("topic").unwrap();
    assert_eq!(current(&f), "topic");
}

#[test]
fn create_from_head_and_from_a_start_point() {
    let f = base();
    let c = cli(&f);
    c.create_branch("feat/one", None).unwrap();
    assert_eq!(current(&f), "feat/one");
    f.write("b.txt", "b\n");
    f.commit("on feat", 1_700_000_100);
    c.create_branch("from-main", Some("main")).unwrap();
    assert_eq!(current(&f), "from-main");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), f.git(&["rev-parse", "main"]));
    assert!(c.create_branch("from-main", None).is_err(), "an existing name is refused");
}

#[test]
fn switching_to_a_remote_only_name_creates_a_tracking_branch() {
    let f = base();
    f.add_bare_upstream();
    f.git(&["switch", "-q", "-c", "feature"]);
    f.git(&["push", "-q", "origin", "feature"]);
    f.git(&["switch", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "feature"]);
    cli(&f).switch_tracking("origin/feature").unwrap();
    assert_eq!(current(&f), "feature");
    assert_eq!(f.git(&["config", "branch.feature.remote"]), "origin");
}

#[test]
fn rename_the_current_and_another_branch() {
    let f = base();
    let c = cli(&f);
    f.git(&["branch", "other"]);
    c.rename_branch("other", "other2").unwrap();
    assert!(f.git(&["branch", "--list", "other2"]).contains("other2"));
    c.rename_branch("main", "trunk").unwrap();
    assert_eq!(current(&f), "trunk");
}

#[test]
fn delete_merged_refuses_unmerged_then_forces() {
    let f = base();
    let c = cli(&f);
    f.git(&["branch", "merged"]);
    c.delete_branch("merged", false).unwrap();
    f.git(&["switch", "-q", "-c", "wip"]);
    f.write("w.txt", "w\n");
    f.commit("wip work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    let e = c.delete_branch("wip", false).unwrap_err();
    assert!(is_unmerged(&e), "{e:#}");
    assert!(f.git(&["branch", "--list", "wip"]).contains("wip"), "still there");
    c.delete_branch("wip", true).unwrap();
    assert_eq!(f.git(&["branch", "--list", "wip"]), "");
}

#[test]
fn deleting_the_checked_out_branch_is_refused() {
    let f = base();
    let e = cli(&f).delete_branch("main", true).unwrap_err();
    assert!(format!("{e:#}").contains("checked out"), "{e:#}");
    assert_eq!(current(&f), "main");
}

#[test]
fn invalid_names_are_refused_before_git_runs() {
    let f = base();
    let c = cli(&f);
    for bad in ["", "-x", "a..b", "a b", "x@{1}", "end.", "a//b", "~tilde"] {
        assert!(c.create_branch(bad, None).is_err(), "{bad:?} accepted");
        assert!(c.rename_branch("main", bad).is_err(), "{bad:?} accepted as a rename target");
    }
    assert_eq!(f.git(&["branch", "--list"]).lines().count(), 1, "no branch was created");
    for good in ["feature/ünï", "a-b_c.d", "release/1.2"] {
        c.create_branch(good, None).unwrap_or_else(|e| panic!("{good:?}: {e:#}"));
    }
}

#[test]
fn a_switch_git_refuses_leaves_the_tree_alone() {
    let f = base();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic\n");
    f.commit("topic edits a", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "dirty\n");
    let e = cli(&f).switch_branch("topic").unwrap_err();
    assert!(!format!("{e:#}").is_empty());
    assert_eq!(current(&f), "main");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "dirty\n");
}

#[test]
fn a_branch_checked_out_in_another_worktree_cannot_be_deleted() {
    let f = base();
    f.git(&["branch", "elsewhere"]);
    let wt = f.path().join("../wt");
    f.git(&["worktree", "add", "-q", wt.to_str().unwrap(), "elsewhere"]);
    let e = cli(&f).delete_branch("elsewhere", true).unwrap_err();
    assert!(!format!("{e:#}").is_empty());
    assert!(f.git(&["branch", "--list", "elsewhere"]).contains("elsewhere"));
}

#[test]
fn from_a_detached_head_current_branch_is_none_and_switching_works() {
    let f = base();
    f.git(&["branch", "topic"]);
    f.git(&["checkout", "-q", "--detach"]);
    let c = cli(&f);
    assert_eq!(c.current_branch(), None);
    c.switch_branch("topic").unwrap();
    assert_eq!(c.current_branch().as_deref(), Some("topic"));
}
