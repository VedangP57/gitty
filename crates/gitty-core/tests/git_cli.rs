mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::status::{Check, EntryKind, Status};

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn status(f: &Fixture) -> Status {
    cli(f).status().unwrap()
}

fn check(f: &Fixture, path: &str) -> Option<Check> {
    status(f).entries.iter().find(|e| e.path == path).map(|e| e.check())
}

#[test]
fn status_covers_unborn_untracked_rename_and_conflict() {
    let f = Fixture::new();
    f.write("a b.txt", "x\n");
    f.write("dir/new.txt", "n\n");
    let st = status(&f);
    assert_eq!((st.branch.as_deref(), st.head.as_deref()), (Some("main"), None));
    assert_eq!(st.entries.len(), 2);
    assert!(st.entries.iter().all(|e| e.kind == EntryKind::Untracked && e.check() == Check::Unstaged));

    f.write("old.txt", "one\ntwo\nthree\n");
    f.commit("base", 1_700_000_000);
    f.git(&["mv", "old.txt", "renamed.txt"]);
    let e = status(&f).entries.into_iter().find(|e| e.path == "renamed.txt").unwrap();
    assert_eq!((e.kind, e.orig_path.as_deref()), (EntryKind::Renamed, Some("old.txt")));
    f.commit("rename", 1_700_000_100);

    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("renamed.txt", "one\nside\nthree\n");
    f.commit("side", 1_700_000_200);
    f.git(&["checkout", "-q", "main"]);
    f.write("renamed.txt", "one\nmain\nthree\n");
    f.commit("main", 1_700_000_300);
    let out = std::process::Command::new("git").current_dir(f.path()).args(["merge", "-q", "side"]).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap();
    assert!(!out.status.success());
    let e = status(&f).entries.into_iter().find(|e| e.path == "renamed.txt").unwrap();
    assert!(e.is_conflicted());
}

#[test]
fn whole_file_stage_and_unstage_round_trip() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("*.txt", "literal star\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nb\n");
    f.write("*.txt", "changed\n");
    f.write("new.txt", "n\n");
    let g = cli(&f);
    g.stage_paths(&["*.txt".into()]).unwrap();
    assert_eq!(check(&f, "*.txt"), Some(Check::Staged));
    assert_eq!(check(&f, "a.txt"), Some(Check::Unstaged), "pathspecs are literal");
    g.stage_paths(&["new.txt".into(), "a.txt".into()]).unwrap();
    assert_eq!(check(&f, "new.txt"), Some(Check::Staged));
    g.unstage_paths(&["new.txt".into(), "*.txt".into()]).unwrap();
    assert_eq!(check(&f, "new.txt"), Some(Check::Unstaged));
    assert_eq!(check(&f, "*.txt"), Some(Check::Unstaged));
    assert_eq!(check(&f, "a.txt"), Some(Check::Staged));
    std::fs::remove_file(f.path().join("a.txt")).unwrap();
    g.stage_paths(&["a.txt".into()]).unwrap();
    let e = status(&f).entries.into_iter().find(|e| e.path == "a.txt").unwrap();
    assert_eq!((e.x, e.check()), ('D', Check::Staged), "staging a deletion");
    g.unstage_all().unwrap();
    assert!(status(&f).entries.iter().all(|e| e.check() == Check::Unstaged));
    g.stage_all().unwrap();
    assert!(status(&f).entries.iter().all(|e| e.check() == Check::Staged));
}

#[test]
fn unstage_on_unborn_head() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    let g = cli(&f);
    g.stage_all().unwrap();
    g.unstage_paths(&["a.txt".into()]).unwrap();
    assert_eq!(check(&f, "a.txt"), Some(Check::Unstaged));
    assert_eq!(check(&f, "b.txt"), Some(Check::Staged));
    g.unstage_all().unwrap();
    assert_eq!(check(&f, "b.txt"), Some(Check::Unstaged));
    g.unstage_all().unwrap();
}

#[test]
fn commit_amend_and_undo() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    let g = cli(&f);
    g.stage_all().unwrap();
    g.commit("First\n\nBody line\n", false, &mut |_| {}).unwrap();
    assert_eq!(g.head_message().unwrap(), "First\n\nBody line\n");
    f.write("a.txt", "a\nb\n");
    g.stage_all().unwrap();
    g.commit("Second", false, &mut |_| {}).unwrap();
    g.commit("Second, amended", true, &mut |_| {}).unwrap();
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), "2");
    assert_eq!(g.head_message().unwrap().trim(), "Second, amended");

    let msg = g.undo_commit().unwrap();
    assert_eq!(msg.trim(), "Second, amended");
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), "1");
    assert_eq!(check(&f, "a.txt"), Some(Check::Staged), "undo keeps the changes staged");

    let msg = g.undo_commit().unwrap();
    assert!(msg.starts_with("First"));
    let st = status(&f);
    assert_eq!(st.head, None, "undoing the root commit leaves an unborn branch");
    assert_eq!(st.entries[0].check(), Check::Staged);
}

#[test]
fn failing_hook_streams_and_returns_its_output() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    let hook = f.path().join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint: first problem' >&2\necho 'lint: second problem' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let g = cli(&f);
    g.stage_all().unwrap();
    let mut lines = Vec::new();
    let err = g.commit("msg", false, &mut |l| lines.push(l.to_string())).unwrap_err();
    let ge = err.downcast_ref::<gitty_core::GitError>().expect("GitError");
    assert!(ge.stderr.contains("second problem"), "{}", ge.stderr);
    assert!(lines.iter().any(|l| l.contains("first problem")), "{lines:?}");
    assert_eq!(status(&f).head, None);
}

#[test]
fn stale_index_lock_is_a_clear_error() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    std::fs::write(f.path().join(".git/index.lock"), "").unwrap();
    let err = cli(&f).stage_all().unwrap_err();
    assert!(format!("{err:#}").contains("index.lock"), "{err:#}");
}

#[test]
fn children_run_in_their_own_session() {
    let f = Fixture::new();
    let g = cli(&f);
    let cmd = g.cmd(gitty_core::git_cli::Kind::Read, &["-c", "alias.pg=!ps -o pgid= -p $$", "pg"]);
    let out = g.run(cmd, None, &mut |_| {}).unwrap();
    let child: i32 = String::from_utf8_lossy(&out).trim().parse().unwrap();
    let ours = unsafe { libc::getpgrp() };
    assert_ne!(child, ours);
}

#[test]
fn conflict_entries_take_head_from_stage_two() {
    let f = Fixture::new();
    f.write("f.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("f.txt", "side\n");
    f.write("g.txt", "side g\n");
    f.commit("side", 1_700_000_100);
    f.git(&["checkout", "-q", "main"]);
    f.write("f.txt", "main\n");
    f.write("g.txt", "main g\n");
    f.commit("main", 1_700_000_200);
    let _ = std::process::Command::new("git").current_dir(f.path()).args(["merge", "-q", "side"]).output().unwrap();
    let st = status(&f);
    for p in ["f.txt", "g.txt"] {
        let e = st.entries.iter().find(|e| e.path == p).unwrap();
        assert!(e.is_conflicted(), "{p}");
        let want = f.git(&["rev-parse", &format!("HEAD:{p}")]);
        let got: String = e.head_blob.expect("HEAD has it (add/add too)").0.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(got, want, "{p}: HEAD is stage 2, not the merge base");
        assert_eq!(e.head_mode, 0o100644);
    }
}

#[test]
fn symlinks_and_type_changes_are_whole_file_only() {
    let f = Fixture::new();
    f.write("plain.txt", "a\n");
    f.write("t.txt", "text\n");
    std::os::unix::fs::symlink("old-target", f.path().join("link")).unwrap();
    f.commit("base", 1_700_000_000);
    std::fs::remove_file(f.path().join("link")).unwrap();
    std::os::unix::fs::symlink("new-target", f.path().join("link")).unwrap();
    std::fs::remove_file(f.path().join("t.txt")).unwrap();
    std::os::unix::fs::symlink("plain.txt", f.path().join("t.txt")).unwrap();
    f.write("plain.txt", "a\nb\n");
    let st = status(&f);
    let e = |p: &str| st.entries.iter().find(|e| e.path == p).unwrap().clone();
    assert!(!e("link").line_stageable(), "symlink retarget");
    assert!(!e("t.txt").line_stageable(), "file → symlink");
    assert!(e("plain.txt").line_stageable());
}
