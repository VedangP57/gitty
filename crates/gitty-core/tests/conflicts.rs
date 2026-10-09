mod common;

use std::path::Path;
use std::process::Command;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::commit_files::BlobId;
use gitty_core::conflicts::{self, Choice, Loaded};
use gitty_core::git_cli::GitCli;
use gitty_core::op_state::{Continued, RepoOp};

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn try_git(f: &Fixture, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(f.path())
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .output()
        .unwrap()
        .status
        .success()
}

/// `a.txt` is "base"; `topic` and `main` both change it. main is checked out.
fn diverged() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic\n");
    f.commit("topic edit", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("main edit", 1_700_000_200);
    f
}

fn stopped(op: RepoOp) -> Fixture {
    let f = diverged();
    match op {
        RepoOp::Merge => assert!(!try_git(&f, &["merge", "topic"])),
        RepoOp::Rebase => {
            f.git(&["switch", "-q", "topic"]);
            assert!(!try_git(&f, &["rebase", "main"]));
        }
        RepoOp::CherryPick => assert!(!try_git(&f, &["cherry-pick", "topic"])),
        RepoOp::Revert => {
            f.write("a.txt", "main again\n");
            f.commit("main again", 1_700_000_300);
            assert!(!try_git(&f, &["revert", "--no-edit", "HEAD~1"]));
        }
    }
    f
}

const OPS: [RepoOp; 4] = [RepoOp::Merge, RepoOp::Rebase, RepoOp::CherryPick, RepoOp::Revert];

#[test]
fn every_choice_resolves_into_what_git_continues_with() {
    for op in OPS {
        for choice in [Choice::Ours, Choice::Theirs, Choice::Both] {
            let f = stopped(op);
            let root = f.path();
            let Loaded::Text { bytes, conflicts } = conflicts::read(&root, Path::new("a.txt")).unwrap() else { panic!("{op:?}: not text") };
            assert_eq!(conflicts.len(), 1, "{op:?}");
            let text = String::from_utf8(bytes.clone()).unwrap();
            let new = conflicts::resolve(&text, &conflicts[0], choice).unwrap();
            assert_eq!(new.lines().count(), if choice == Choice::Both { 2 } else { 1 }, "{op:?} {choice:?}: {new:?}");
            conflicts::write_resolved(&root, Path::new("a.txt"), BlobId::hash_of(&bytes), new.as_bytes()).unwrap();
            assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), new);
            f.git(&["add", "--", "a.txt"]);
            let c = cli(&f);
            let id = c.op_state(&c.status().unwrap()).unwrap().id;
            // keeping what HEAD has already leaves the pick or revert with nothing to commit
            if choice == Choice::Ours && matches!(op, RepoOp::CherryPick | RepoOp::Revert) {
                assert!(c.continue_op(op, &id, &[], &mut |_| {}).is_err(), "{op:?}");
                continue;
            }
            let done = c.continue_op(op, &id, &[], &mut |_| {}).unwrap();
            assert_eq!(done, Continued::Finished, "{op:?} {choice:?}");
            assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), new);
            assert!(c.op_state(&c.status().unwrap()).is_none());
        }
    }
}

#[test]
fn ours_and_theirs_follow_git_in_every_operation() {
    // what each side holds: the branch checked out (a rebase has main underneath, whatever is
    // checked out), and what is merged, replayed or reverted
    let sides = |op| match op {
        RepoOp::Merge | RepoOp::Rebase | RepoOp::CherryPick => ("main", "topic"),
        RepoOp::Revert => ("main again", "base"),
    };
    for op in OPS {
        let f = stopped(op);
        let Loaded::Text { bytes, conflicts } = conflicts::read(&f.path(), Path::new("a.txt")).unwrap() else { panic!() };
        let text = String::from_utf8(bytes).unwrap();
        let (ours, theirs) = sides(op);
        assert_eq!(conflicts::resolve(&text, &conflicts[0], Choice::Ours).unwrap().trim(), ours, "{op:?}");
        assert_eq!(conflicts::resolve(&text, &conflicts[0], Choice::Theirs).unwrap().trim(), theirs, "{op:?}");
    }
}

#[test]
fn sides_are_named_by_the_operation() {
    let names = |op| {
        let s = cli(&stopped(op)).conflict_sides();
        (s.ours.label(), s.theirs.label())
    };
    assert_eq!(names(RepoOp::Merge), ("Current (main)".into(), "Incoming (topic)".into()));
    assert_eq!(names(RepoOp::Rebase), ("Base branch (main)".into(), "Your commit (topic edit)".into()));
    assert_eq!(names(RepoOp::CherryPick), ("Current branch (main)".into(), "Picked commit (topic edit)".into()));
    assert_eq!(names(RepoOp::Revert), ("Current branch (main)".into(), "Reverted change (main edit)".into()));
    let f = diverged();
    assert_eq!(cli(&f).conflict_sides(), conflicts::Sides::generic());
}

/// `b.txt` modified on topic and deleted on main; merging topic stops with b.txt deleted by us.
fn deleted_by_us() -> Fixture {
    let f = Fixture::new();
    f.write("b.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("b.txt", "topic\n");
    f.commit("topic edit", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.git(&["rm", "-q", "b.txt"]);
    f.commit("main deletes", 1_700_000_200);
    assert!(!try_git(&f, &["merge", "topic"]));
    f
}

#[test]
fn a_whole_file_side_is_taken_or_deleted() {
    let f = deleted_by_us();
    let c = cli(&f);
    assert_eq!(c.unmerged_stages("b.txt").unwrap(), vec![1, 3]);
    // keeping ours (which has no file) as a plain checkout, or theirs as a delete, is refused
    assert!(c.take_side("b.txt", false, false).is_err());
    assert!(c.take_side("b.txt", true, true).is_err());
    assert_eq!(c.unmerged_stages("b.txt").unwrap(), vec![1, 3]);
    c.take_side("b.txt", true, false).unwrap();
    assert_eq!(std::fs::read_to_string(f.path().join("b.txt")).unwrap(), "topic\n");
    assert!(c.unmerged_stages("b.txt").unwrap().is_empty());
    assert!(c.take_side("b.txt", true, false).is_err(), "no longer conflicted");

    let f = deleted_by_us();
    let c = cli(&f);
    c.take_side("b.txt", false, true).unwrap();
    assert!(!f.path().join("b.txt").exists());
    assert!(c.unmerged_stages("b.txt").unwrap().is_empty());
    let id = c.op_state(&c.status().unwrap()).unwrap().id;
    assert_eq!(c.continue_op(RepoOp::Merge, &id, &[], &mut |_| {}).unwrap(), Continued::Finished);
}

#[test]
fn a_binary_conflict_takes_a_side_and_a_literal_path() {
    let f = Fixture::new();
    f.write("[x] *.bin", b"base\0".as_slice());
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("[x] *.bin", b"topic\0".as_slice());
    f.commit("topic", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("[x] *.bin", b"main\0".as_slice());
    f.commit("main", 1_700_000_200);
    assert!(!try_git(&f, &["merge", "topic"]));
    let c = cli(&f);
    assert!(matches!(conflicts::read(&f.path(), Path::new("[x] *.bin")).unwrap(), Loaded::Other(_)));
    c.take_side("[x] *.bin", true, false).unwrap();
    assert_eq!(std::fs::read(f.path().join("[x] *.bin")).unwrap(), b"topic\0");
    assert!(c.unmerged_stages("[x] *.bin").unwrap().is_empty());
}
