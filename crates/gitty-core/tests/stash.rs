mod common;

use common::Fixture;
use gitty_core::Repo;
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

#[test]
fn push_takes_tracked_and_untracked_changes_and_pop_restores_them() {
    let f = base();
    let c = cli(&f);
    f.write("a.txt", "edited\n");
    f.write("new.txt", "fresh\n");
    assert!(c.stash_push("wip: parser").unwrap());
    assert_eq!(f.git(&["status", "--porcelain"]), "", "the tree is clean");
    assert!(!f.path().join("new.txt").exists());
    let list = c.stash_list().unwrap();
    assert_eq!((list.len(), list[0].index, list[0].branch.as_str(), list[0].message.as_str()), (1, 0, "main", "wip: parser"));
    c.stash_pop(0).unwrap();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n");
    assert!(f.path().join("new.txt").exists());
    assert!(c.stash_list().unwrap().is_empty(), "pop drops the entry");
}

#[test]
fn nothing_to_stash_is_not_an_error() {
    let f = base();
    let c = cli(&f);
    assert!(!c.stash_push("nothing").unwrap());
    assert!(c.stash_list().unwrap().is_empty());
}

#[test]
fn list_is_newest_first_and_apply_keeps_the_entry() {
    let f = base();
    let c = cli(&f);
    f.write("a.txt", "one\n");
    c.stash_push("first").unwrap();
    f.write("a.txt", "two\n");
    c.stash_push("second").unwrap();
    let list = c.stash_list().unwrap();
    assert_eq!(list.iter().map(|s| s.message.as_str()).collect::<Vec<_>>(), ["second", "first"]);
    c.stash_apply(1).unwrap();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "one\n");
    assert_eq!(c.stash_list().unwrap().len(), 2, "apply keeps it");
}

#[test]
fn drop_removes_one_entry() {
    let f = base();
    let c = cli(&f);
    f.write("a.txt", "one\n");
    c.stash_push("first").unwrap();
    f.write("a.txt", "two\n");
    c.stash_push("second").unwrap();
    c.stash_drop(0).unwrap();
    let list = c.stash_list().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].message, "first");
}

#[test]
fn a_conflicting_apply_or_pop_keeps_the_stash() {
    let f = base();
    let c = cli(&f);
    f.write("a.txt", "stashed\n");
    c.stash_push("conflicts later").unwrap();
    f.write("a.txt", "committed meanwhile\n");
    f.commit("meanwhile", 1_700_000_100);
    let e = c.stash_pop(0).unwrap_err();
    assert!(format!("{e:#}").contains("kept"), "{e:#}");
    assert_eq!(c.stash_list().unwrap().len(), 1, "the entry survives a conflicting pop");
    assert!(c.stash_apply(0).is_err());
    assert_eq!(c.stash_list().unwrap().len(), 1);
}

#[test]
fn messages_starting_with_a_dash_are_taken_literally() {
    let f = base();
    let c = cli(&f);
    f.write("a.txt", "x\n");
    assert!(c.stash_push("-weird --message").unwrap());
    assert_eq!(c.stash_list().unwrap()[0].message, "-weird --message");
}

#[test]
fn an_invalid_index_fails_without_claiming_the_stash_was_kept() {
    let f = base();
    let c = cli(&f);
    let e = c.stash_pop(7).unwrap_err();
    assert!(!format!("{e:#}").contains("kept"), "{e:#}");
}

#[test]
fn push_reports_true_only_when_the_stash_ref_moved() {
    let f = base();
    let c = cli(&f);
    assert!(!c.stash_push("clean").unwrap());
    f.write("a.txt", "one\n");
    assert!(c.stash_push("first").unwrap());
    f.write("a.txt", "two\n");
    assert!(c.stash_push("second").unwrap(), "true even though a stash already existed");
    assert!(!c.stash_push("clean again").unwrap());
    assert_eq!(c.stash_list().unwrap().len(), 2);
}
