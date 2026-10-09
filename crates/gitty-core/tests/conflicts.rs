mod common;

use std::path::Path;
use std::process::Command;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::commit_files::BlobId;
use gitty_core::conflicts::{self, Choice, Loaded, Style};
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
            let Loaded::Text { bytes, conflicts, .. } = conflicts::read(&root, Path::new("a.txt"), Style::default()).unwrap() else { panic!("{op:?}: not text") };
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
        let Loaded::Text { bytes, conflicts, .. } = conflicts::read(&f.path(), Path::new("a.txt"), Style::default()).unwrap() else { panic!() };
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
    assert_eq!(names(RepoOp::Revert), ("Current branch (main)".into(), "Without change (main edit)".into()));
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
    assert!(c.take_side("b.txt", false, false, &mut || Ok(())).is_err());
    assert!(c.take_side("b.txt", true, true, &mut || Ok(())).is_err());
    assert_eq!(c.unmerged_stages("b.txt").unwrap(), vec![1, 3]);
    c.take_side("b.txt", true, false, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read_to_string(f.path().join("b.txt")).unwrap(), "topic\n");
    assert!(c.unmerged_stages("b.txt").unwrap().is_empty());
    assert!(c.take_side("b.txt", true, false, &mut || Ok(())).is_err(), "no longer conflicted");

    let f = deleted_by_us();
    let c = cli(&f);
    c.take_side("b.txt", false, true, &mut || Ok(())).unwrap();
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
    assert!(matches!(conflicts::read(&f.path(), Path::new("[x] *.bin"), Style::default()).unwrap(), Loaded::Other(_)));
    c.take_side("[x] *.bin", true, false, &mut || Ok(())).unwrap();
    assert_eq!(std::fs::read(f.path().join("[x] *.bin")).unwrap(), b"topic\0");
    assert!(c.unmerged_stages("[x] *.bin").unwrap().is_empty());
}

/// Runs `args` in `f`'s work tree; for the commands that are expected to stop.
fn stop_in(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "protocol.file.allow=always"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{args:?} did not stop");
}

#[test]
fn a_conflict_marker_size_attribute_is_read_and_used() {
    let f = Fixture::new();
    // the user's own config is read too: pin the style
    f.git(&["config", "merge.conflictStyle", "merge"]);
    f.write(".gitattributes", "a.txt conflict-marker-size=9\nb.txt conflict-marker-size=banana\nc.txt conflict-marker-size=1000\n");
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic\n");
    f.commit("topic", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("main", 1_700_000_200);
    assert!(!try_git(&f, &["merge", "topic"]));
    let c = cli(&f);
    assert_eq!(c.conflict_style("a.txt"), Style { size: 9, diff3: false });
    assert_eq!(c.conflict_style("b.txt").size, 7, "an invalid value is ignored");
    assert_eq!(c.conflict_style("c.txt").size, 64, "clamped like git");
    assert_eq!(c.conflict_style("plain.txt").size, 7);
    let text = std::fs::read_to_string(f.path().join("a.txt")).unwrap();
    assert!(text.starts_with("<<<<<<<<< HEAD"), "{text}");
    let Loaded::Text { bytes, conflicts, unknown } = conflicts::read(&f.path(), Path::new("a.txt"), c.conflict_style("a.txt")).unwrap() else { panic!() };
    assert_eq!((conflicts.len(), unknown), (1, false));
    let new = conflicts::resolve(std::str::from_utf8(&bytes).unwrap(), &conflicts[0], Choice::Theirs).unwrap();
    assert_eq!(new, "topic\n");
    // read with the wrong size: the markers are not understood, and say so
    let Loaded::Text { conflicts, unknown, .. } = conflicts::read(&f.path(), Path::new("a.txt"), Style::default()).unwrap() else { panic!() };
    assert_eq!((conflicts.len(), unknown), (0, true));
}

#[test]
fn the_conflict_style_config_says_whether_a_base_is_written() {
    let f = diverged();
    f.git(&["config", "merge.conflictStyle", "merge"]);
    assert!(!cli(&f).conflict_style("a.txt").diff3);
    f.git(&["config", "merge.conflictStyle", "zdiff3"]);
    assert!(cli(&f).conflict_style("a.txt").diff3);
    assert!(!try_git(&f, &["merge", "topic"]));
    let Loaded::Text { conflicts, .. } = conflicts::read(&f.path(), Path::new("a.txt"), cli(&f).conflict_style("a.txt")).unwrap() else { panic!() };
    assert!(conflicts[0].base.is_some() && !conflicts[0].ambiguous);
}

#[test]
fn a_setext_underline_in_a_conflicting_side_is_flagged_not_misresolved() {
    let f = Fixture::new();
    f.write("a.rst", "Chapter\n=======\ntext\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.rst", "Chapter\n=======\ntopic text\n");
    f.commit("topic", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.rst", "Chapter\n=======\nmain text\n");
    f.commit("main", 1_700_000_200);
    assert!(!try_git(&f, &["merge", "topic"]));
    // the common head stays outside the block here; make the underline part of the block
    let text = "<<<<<<< HEAD\nChapter\n=======\nmain text\n=======\ntopic text\n>>>>>>> topic\n";
    f.write("a.rst", text);
    let Loaded::Text { bytes, conflicts, .. } = conflicts::read(&f.path(), Path::new("a.rst"), Style::default()).unwrap() else { panic!() };
    assert_eq!(conflicts.len(), 1);
    assert!(conflicts[0].ambiguous);
    for choice in [Choice::Ours, Choice::Theirs, Choice::Both] {
        assert_eq!(conflicts::resolve(std::str::from_utf8(&bytes).unwrap(), &conflicts[0], choice), None);
    }
}

#[test]
fn a_submodule_conflict_takes_the_chosen_commit_not_the_work_trees() {
    let f = Fixture::new();
    let sub = tempfile::tempdir().unwrap();
    let sp = sub.path().join("sub");
    std::fs::create_dir(&sp).unwrap();
    let sg = |args: &[&str]| {
        let out = Command::new("git").current_dir(&sp).args(["-c", "user.name=T", "-c", "user.email=t@e.c"]).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    sg(&["init", "-q", "-b", "main"]);
    std::fs::write(sp.join("f"), "1").unwrap();
    sg(&["add", "f"]);
    sg(&["commit", "-qm", "s1"]);
    f.write("keep.txt", "x\n");
    f.commit("base", 1_700_000_000);
    f.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", sp.to_str().unwrap(), "s"]);
    f.commit("add submodule", 1_700_000_100);
    f.git(&["switch", "-q", "-c", "topic"]);
    let s = f.path().join("s");
    let in_s = |args: &[&str]| {
        let out = Command::new("git").current_dir(&s).args(["-c", "user.name=T", "-c", "user.email=t@e.c"]).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    in_s(&["switch", "-qc", "x1"]);
    std::fs::write(s.join("f"), "2").unwrap();
    in_s(&["commit", "-qam", "x1"]);
    let theirs = in_s(&["rev-parse", "HEAD"]);
    f.commit("topic moves it", 1_700_000_200);
    f.git(&["switch", "-q", "main"]);
    in_s(&["switch", "-q", "main"]);
    in_s(&["switch", "-qc", "x2"]);
    std::fs::write(s.join("f"), "3").unwrap();
    in_s(&["commit", "-qam", "x2"]);
    let ours = in_s(&["rev-parse", "HEAD"]);
    f.commit("main moves it", 1_700_000_300);
    stop_in(&f.path(), &["merge", "topic"]);
    let c = cli(&f);
    assert_eq!(c.unmerged_stages("s").unwrap(), vec![1, 2, 3]);
    assert_eq!(c.unmerged_entries("s").unwrap()[2].mode, 0o160000);
    // theirs: the index records their commit even though the work tree still holds ours
    c.take_side("s", true, false, &mut || Ok(())).unwrap();
    assert!(c.unmerged_stages("s").unwrap().is_empty());
    assert!(f.git(&["ls-files", "-s", "s"]).contains(&theirs), "{}", f.git(&["ls-files", "-s", "s"]));
    assert!(!f.git(&["ls-files", "-s", "s"]).contains(&ours));
    assert_eq!(in_s(&["rev-parse", "HEAD"]), ours, "the submodule's own checkout is not touched");
}

#[test]
fn the_backup_runs_after_the_stage_check_and_not_when_it_fails() {
    let f = deleted_by_us();
    let c = cli(&f);
    let mut ran = 0;
    // refused (ours has no file, so a checkout is not what the user was asked): no backup
    assert!(c.take_side("b.txt", false, false, &mut || { ran += 1; Ok(()) }).is_err());
    assert_eq!(ran, 0);
    c.take_side("b.txt", true, false, &mut || { ran += 1; Ok(()) }).unwrap();
    assert_eq!(ran, 1);
    // a failing backup stops everything
    let f = deleted_by_us();
    let c = cli(&f);
    assert!(c.take_side("b.txt", true, false, &mut || anyhow::bail!("no trash")).is_err());
    assert_eq!(c.unmerged_stages("b.txt").unwrap(), vec![1, 3]);
}
