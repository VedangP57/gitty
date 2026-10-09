mod common;

use std::process::Command;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::op_state::{Aborted, Continued, OpState, RepoOp};

fn cli_at(path: &std::path::Path) -> GitCli {
    GitCli::new(&Repo::open(path).unwrap())
}

fn cli(f: &Fixture) -> GitCli {
    cli_at(&f.path())
}

fn state_at(path: &std::path::Path) -> Option<OpState> {
    let c = cli_at(path);
    c.op_state(&c.status().unwrap())
}

fn state(f: &Fixture) -> Option<OpState> {
    state_at(&f.path())
}

/// Runs git where `f` lives and says whether it succeeded: the commands that stop on a conflict fail.
fn try_git(f: &Fixture, args: &[&str]) -> bool {
    try_git_in(&f.path(), args)
}

fn try_git_in(dir: &std::path::Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(dir)
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

/// `a.txt` is "base"; `topic` and `main` both change it, so combining them conflicts. main is
/// checked out.
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

/// An executable hook `name` of `f`; its own hooks directory, so a global `core.hooksPath` does not take over.
fn hook(f: &Fixture, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let dir = f.path().join(".git/test-hooks");
    std::fs::create_dir_all(&dir).unwrap();
    f.git(&["config", "core.hooksPath", dir.to_str().unwrap()]);
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A repository stopped in `op` on `a.txt`.
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

fn resolve(f: &Fixture, text: &str) {
    f.write("a.txt", text);
    f.git(&["add", "a.txt"]);
}

fn noop() -> impl FnMut(&str) {
    |_| {}
}

#[test]
fn nothing_is_in_progress_in_a_clean_repository() {
    let f = diverged();
    assert_eq!(state(&f), None);
}

#[test]
fn a_merge_is_named_by_its_branch_and_counts_its_conflicts() {
    let f = stopped(RepoOp::Merge);
    assert_eq!(state(&f), Some(OpState { op: RepoOp::Merge, conflicts: 1, detail: "topic".into(), step: None }));
    // staged by hand: the merge goes on, with nothing left to resolve
    resolve(&f, "both\n");
    assert_eq!(state(&f).unwrap().conflicts, 0);
}

#[test]
fn a_merge_of_a_bare_commit_is_named_by_a_branch_that_holds_it() {
    let f = diverged();
    let id = f.git(&["rev-parse", "topic"]);
    assert!(!try_git(&f, &["merge", &id]));
    // "Merge commit '<sha>'": the name comes from name-rev, not from the message
    let s = state(&f).unwrap();
    assert_eq!((s.op, s.detail.as_str()), (RepoOp::Merge, "topic"));
}

#[test]
fn a_rebase_shows_its_branch_and_step() {
    let f = diverged();
    f.git(&["switch", "-q", "topic"]);
    f.write("b.txt", "second\n");
    f.commit("topic second", 1_700_000_150);
    assert!(!try_git(&f, &["rebase", "main"]));
    assert_eq!(state(&f), Some(OpState { op: RepoOp::Rebase, conflicts: 1, detail: "topic".into(), step: Some((1, 2)) }));
}

#[test]
fn a_cherry_pick_and_a_revert_are_told_apart() {
    assert_eq!(state(&stopped(RepoOp::CherryPick)).map(|s| (s.op, s.conflicts)), Some((RepoOp::CherryPick, 1)));
    assert_eq!(state(&stopped(RepoOp::Revert)).map(|s| (s.op, s.conflicts)), Some((RepoOp::Revert, 1)));
}

#[test]
fn a_multi_pick_that_stopped_after_a_commit_is_still_seen() {
    let f = diverged();
    f.git(&["switch", "-q", "-c", "picks", "topic"]);
    f.write("a.txt", "picks\n");
    f.commit("p1", 1_700_000_300);
    f.write("b.txt", "p2\n");
    f.commit("p2", 1_700_000_400);
    f.git(&["switch", "-q", "main"]);
    // the first pick conflicts; resolving it by committing leaves the sequencer with the rest
    assert!(!try_git(&f, &["cherry-pick", "picks~1", "picks"]));
    resolve(&f, "p1 resolved\n");
    f.git_env(&["commit", "-q", "-m", "p1"], &[("GIT_EDITOR", "true".into())]);
    // between the two picks nothing conflicts and there is no CHERRY_PICK_HEAD, but the second pick waits
    assert!(!f.path().join(".git/CHERRY_PICK_HEAD").exists());
    assert_eq!(state(&f).map(|s| (s.op, s.conflicts)), Some((RepoOp::CherryPick, 0)));
    assert_eq!(cli(&f).continue_op(RepoOp::CherryPick, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "p2");
}

#[test]
fn rebase_wins_over_a_pick_in_the_same_directory() {
    // an interactive rebase of a pick that conflicts has both rebase-merge and CHERRY_PICK_HEAD
    let f = stopped(RepoOp::Rebase);
    std::fs::write(f.path().join(".git/CHERRY_PICK_HEAD"), f.git(&["rev-parse", "main"]) + "\n").unwrap();
    assert_eq!(state(&f).unwrap().op, RepoOp::Rebase);
}

#[test]
fn a_linked_worktree_sees_its_own_state_and_the_main_one_does_not() {
    let f = diverged();
    let wt = f.path().parent().unwrap().join("wt");
    f.git(&["worktree", "add", "-q", "-b", "other", wt.to_str().unwrap(), "topic"]);
    assert!(!try_git_in(&wt, &["merge", "main"]));
    assert_eq!(state_at(&wt), Some(OpState { op: RepoOp::Merge, conflicts: 1, detail: "main".into(), step: None }));
    assert_eq!(state(&f), None);
    // and it can be finished there
    std::fs::write(wt.join("a.txt"), "both\n").unwrap();
    assert!(try_git_in(&wt, &["add", "a.txt"]));
    assert_eq!(cli_at(&wt).continue_op(RepoOp::Merge, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(state_at(&wt), None);
}

#[test]
fn a_rebase_in_a_linked_worktree_is_found() {
    let f = diverged();
    let wt = f.path().parent().unwrap().join("wt");
    f.git(&["worktree", "add", "-q", "-b", "other", wt.to_str().unwrap(), "topic"]);
    assert!(!try_git_in(&wt, &["rebase", "main"]));
    assert_eq!(state_at(&wt).map(|s| (s.op, s.step)), Some((RepoOp::Rebase, Some((1, 1)))));
}

#[test]
fn continue_is_refused_while_files_still_conflict() {
    for op in OPS {
        let f = stopped(op);
        let e = cli(&f).continue_op(op, &mut noop()).unwrap_err();
        assert_eq!(format!("{e}"), "1 file still conflicts: resolve them and stage them first", "{op:?}");
        assert_eq!(state(&f).map(|s| s.op), Some(op), "{op:?} is untouched");
    }
}

#[test]
fn continue_counts_every_unmerged_path_once() {
    let f = Fixture::new();
    for n in ["a.txt", "b.txt"] {
        f.write(n, "base\n");
    }
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "t\n");
    f.write("b.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "m\n");
    f.write("b.txt", "m\n");
    f.commit("main", 1_700_000_200);
    assert!(!try_git(&f, &["merge", "topic"]));
    let e = cli(&f).continue_op(RepoOp::Merge, &mut noop()).unwrap_err();
    assert_eq!(format!("{e}"), "2 files still conflict: resolve them and stage them first");
}

#[test]
fn continue_checks_git_not_a_stale_state() {
    let f = stopped(RepoOp::Merge);
    // the caller saw 0 conflicts a moment ago; the index says otherwise now
    assert!(cli(&f).continue_op(RepoOp::Merge, &mut noop()).is_err());
}

#[test]
fn continuing_a_resolved_merge_commits_it() {
    let f = stopped(RepoOp::Merge);
    resolve(&f, "both\n");
    assert_eq!(cli(&f).continue_op(RepoOp::Merge, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Merge branch 'topic'");
    assert_eq!(f.git(&["rev-list", "--parents", "-n1", "HEAD"]).split_whitespace().count(), 3);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(state(&f), None);
}

#[test]
fn continuing_a_resolved_rebase_replays_the_branch() {
    let f = stopped(RepoOp::Rebase);
    resolve(&f, "both\n");
    assert_eq!(cli(&f).continue_op(RepoOp::Rebase, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), "topic");
    assert_eq!(f.git(&["rev-parse", "HEAD~1"]), f.git(&["rev-parse", "main"]));
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "topic edit");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn continuing_a_resolved_cherry_pick_commits_it_with_the_original_message() {
    let f = stopped(RepoOp::CherryPick);
    resolve(&f, "both\n");
    assert_eq!(cli(&f).continue_op(RepoOp::CherryPick, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "topic edit");
    // main is the branch the pick was made on
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "topic edit\nmain edit");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn continuing_a_resolved_revert_commits_it() {
    let f = stopped(RepoOp::Revert);
    resolve(&f, "reverted\n");
    assert_eq!(cli(&f).continue_op(RepoOp::Revert, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Revert \"main edit\"");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(state(&f), None);
}

#[test]
fn a_rebase_that_runs_into_the_next_conflict_is_not_an_error() {
    let f = diverged();
    f.git(&["switch", "-q", "topic"]);
    f.write("a.txt", "topic two\n");
    f.commit("topic two", 1_700_000_150);
    assert!(!try_git(&f, &["rebase", "main"]));
    assert_eq!(state(&f).unwrap().step, Some((1, 2)));
    resolve(&f, "first\n");
    assert_eq!(cli(&f).continue_op(RepoOp::Rebase, &mut noop()).unwrap(), Continued::Stopped { conflicts: 1 });
    let s = state(&f).unwrap();
    assert_eq!((s.op, s.conflicts, s.step), (RepoOp::Rebase, 1, Some((2, 2))));
    resolve(&f, "second\n");
    assert_eq!(cli(&f).continue_op(RepoOp::Rebase, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "--format=%s", "main..topic"]), "topic two\ntopic edit");
    assert_eq!(f.git(&["show", "HEAD:a.txt"]), "second");
}

#[test]
fn a_multi_pick_continues_into_its_next_conflict() {
    let f = diverged();
    f.git(&["switch", "-q", "-c", "picks", "topic"]);
    f.write("a.txt", "topic two\n");
    f.commit("topic two", 1_700_000_150);
    f.git(&["switch", "-q", "main"]);
    assert!(!try_git(&f, &["cherry-pick", "topic", "picks"]));
    resolve(&f, "first\n");
    assert_eq!(cli(&f).continue_op(RepoOp::CherryPick, &mut noop()).unwrap(), Continued::Stopped { conflicts: 1 });
    resolve(&f, "second\n");
    assert_eq!(cli(&f).continue_op(RepoOp::CherryPick, &mut noop()).unwrap(), Continued::Finished);
    assert_eq!(f.git(&["log", "-2", "--format=%s"]), "topic two\ntopic edit");
}

#[test]
fn abort_puts_head_and_the_tree_back_exactly() {
    for op in OPS {
        let f = stopped(op);
        // a rebase leaves its branch where it was until it finishes; the others never move HEAD
        let (branch, before) = if op == RepoOp::Rebase { ("topic", f.git(&["rev-parse", "topic"])) } else { ("main", f.git(&["rev-parse", "HEAD"])) };
        assert_eq!(cli(&f).abort_op(op).unwrap(), Aborted::Done, "{op:?}");
        assert_eq!(state(&f), None, "{op:?}");
        assert_eq!(f.git(&["rev-parse", "HEAD"]), before, "{op:?}");
        assert_eq!(f.git(&["symbolic-ref", "--short", "HEAD"]), branch, "{op:?}");
        assert_eq!(f.git(&["status", "--porcelain"]), "", "{op:?}");
        assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap().trim_end(), f.git(&["show", "HEAD:a.txt"]), "{op:?}: the file is HEAD's");
    }
}

#[test]
fn aborting_after_resolving_discards_the_resolution() {
    let f = stopped(RepoOp::Merge);
    resolve(&f, "both\n");
    assert_eq!(cli(&f).abort_op(RepoOp::Merge).unwrap(), Aborted::Done);
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "main\n");
}

#[test]
fn a_state_that_ended_elsewhere_is_gone_not_an_error() {
    for op in OPS {
        let f = stopped(op);
        assert!(try_git(&f, &[op.name(), "--abort"]));
        assert_eq!(cli(&f).abort_op(op).unwrap(), Aborted::Gone, "{op:?}");
        assert_eq!(cli(&f).continue_op(op, &mut noop()).unwrap(), Continued::Gone, "{op:?}");
    }
}

#[test]
fn another_operation_in_the_place_of_the_expected_one_is_refused() {
    let f = stopped(RepoOp::Merge);
    let e = cli(&f).abort_op(RepoOp::Rebase).unwrap_err();
    assert_eq!(format!("{e}"), "a merge is in progress now, not a rebase; look again");
    assert_eq!(state(&f).map(|s| s.op), Some(RepoOp::Merge));
    assert!(cli(&f).continue_op(RepoOp::Revert, &mut noop()).is_err());
}

#[test]
fn a_hook_that_refuses_the_commit_is_an_error_and_leaves_the_merge_open() {
    let f = stopped(RepoOp::Merge);
    resolve(&f, "both\n");
    hook(&f, "pre-commit","echo 'no merges today' >&2\nexit 1");
    let mut log = Vec::new();
    let e = cli(&f).continue_op(RepoOp::Merge, &mut |l| log.push(l.to_string())).unwrap_err();
    assert!(format!("{e}").contains("no merges today"), "{e}");
    assert!(log.iter().any(|l| l.contains("no merges today")), "{log:?}");
    assert_eq!(state(&f).map(|s| (s.op, s.conflicts)), Some((RepoOp::Merge, 0)));
}

#[test]
fn a_failing_hook_in_a_cherry_pick_is_an_error_not_a_stop() {
    let f = stopped(RepoOp::CherryPick);
    resolve(&f, "both\n");
    hook(&f, "pre-commit", "echo 'lint failed' >&2\nexit 1");
    let e = cli(&f).continue_op(RepoOp::CherryPick, &mut noop()).unwrap_err();
    assert!(format!("{e}").contains("lint failed"), "{e}");
    assert_eq!(state(&f).map(|s| (s.op, s.conflicts)), Some((RepoOp::CherryPick, 0)));
}

#[test]
fn no_editor_is_ever_started() {
    use std::os::unix::fs::PermissionsExt;
    for op in OPS {
        let f = stopped(op);
        let marker = f.path().join(".git/editor-ran");
        let script = f.path().join(".git/hostile-editor.sh");
        std::fs::write(&script, format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display())).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for key in ["core.editor", "sequence.editor"] {
            f.git(&["config", key, script.to_str().unwrap()]);
        }
        resolve(&f, "both\n");
        assert_eq!(cli(&f).continue_op(op, &mut noop()).unwrap(), Continued::Finished, "{op:?}");
        assert!(!marker.exists(), "{op:?} started the editor");
    }
}
