mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::merge::MergeOutcome;

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn base() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base", 1_700_000_000);
    f
}

/// `topic` and `main` each made one commit after `base`; main is checked out. `file` is what
/// both edit (None: they touch different files).
fn diverged(file: Option<&str>) -> Fixture {
    let f = base();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write(file.unwrap_or("t.txt"), "topic\n");
    f.commit("topic work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write(file.unwrap_or("m.txt"), "main\n");
    f.commit("main work", 1_700_000_200);
    f
}

fn parents(f: &Fixture) -> usize {
    f.git(&["rev-list", "--parents", "-n1", "HEAD"]).split_whitespace().count() - 1
}

fn no_merge_in_progress(f: &Fixture) {
    assert!(!f.path().join(".git/MERGE_HEAD").exists(), "MERGE_HEAD left behind");
}

#[test]
fn a_diverged_branch_is_merged_with_a_merge_commit() {
    let f = diverged(None);
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::Merged);
    assert_eq!(parents(&f), 2);
    assert!(f.path().join("t.txt").exists() && f.path().join("m.txt").exists());
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Merge branch 'topic'");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_branch_ahead_of_head_fast_forwards() {
    let f = base();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::FastForward);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), f.git(&["rev-parse", "topic"]));
    assert_eq!(parents(&f), 1);
}

#[test]
fn fast_forwarding_to_a_merge_commit_is_still_a_fast_forward() {
    let f = diverged(None);
    f.git(&["switch", "-q", "topic"]);
    f.git(&["merge", "-q", "--no-edit", "main"]);
    f.git(&["switch", "-q", "main"]);
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::FastForward);
}

#[test]
fn a_fast_forward_that_git_turns_into_a_merge_commit_is_merged() {
    let f = base();
    f.git(&["config", "merge.ff", "false"]);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::Merged);
    assert_eq!(parents(&f), 2);
}

#[test]
fn a_branch_already_in_head_is_up_to_date() {
    let f = base();
    f.git(&["branch", "old"]);
    f.write("m.txt", "m\n");
    f.commit("main work", 1_700_000_100);
    let head = f.git(&["rev-parse", "HEAD"]);
    assert_eq!(cli(&f).merge_branch("old", false).unwrap(), MergeOutcome::UpToDate);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn conflicts_are_aborted_and_listed_leaving_everything_as_it_was() {
    let f = base();
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic a\n");
    f.write("b.txt", "topic b\n");
    f.write("t.txt", "t\n");
    f.commit("topic work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main a\n");
    f.write("b.txt", "main b\n");
    f.commit("main work", 1_700_000_200);
    let (head, index) = (f.git(&["rev-parse", "HEAD"]), f.git(&["ls-files", "--stage"]));
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::Conflicts(vec!["a.txt".into(), "b.txt".into()]));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["ls-files", "--stage"]), index, "the index has no conflict stages");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "main a\n");
    assert!(!f.path().join("t.txt").exists(), "the clean part of the merge was undone too");
    no_merge_in_progress(&f);
}

#[test]
fn conflicts_with_unrelated_local_changes_keep_those_changes() {
    let f = diverged(Some("a.txt"));
    f.write("b.txt", "dirty\n");
    f.write("new.txt", "untracked\n");
    assert!(matches!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::Conflicts(_)));
    assert_eq!(std::fs::read_to_string(f.path().join("b.txt")).unwrap(), "dirty\n");
    assert_eq!(f.git(&["status", "--porcelain"]), "M b.txt\n?? new.txt");
    no_merge_in_progress(&f);
}

#[test]
fn a_merge_git_refuses_for_local_changes_returns_its_message_and_changes_nothing() {
    let f = diverged(Some("a.txt"));
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    f.write("a.txt", "dirty\n");
    let e = cli(&f).merge_branch("topic", false).unwrap_err();
    assert!(format!("{e:#}").contains("would be overwritten"), "{e:#}");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "dirty\n");
    no_merge_in_progress(&f);
}

#[test]
fn hostile_and_unknown_names_are_refused_before_anything_changes() {
    let f = diverged(None);
    let head = f.git(&["rev-parse", "HEAD"]);
    for bad in ["", "-x", "--abort", "--no-ff", "a..b", "x@{1}", "a b", "nope", "HEAD", "main~1"] {
        assert!(cli(&f).merge_branch(bad, false).is_err(), "{bad:?} accepted");
        assert!(cli(&f).merge_branch(bad, true).is_err(), "{bad:?} accepted as a remote branch");
    }
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn a_tag_of_the_same_name_is_not_taken_for_the_branch() {
    let f = diverged(None);
    f.git(&["tag", "topic", "main"]);
    assert_eq!(cli(&f).merge_branch("topic", false).unwrap(), MergeOutcome::Merged);
    assert!(f.path().join("t.txt").exists(), "the branch was merged, not the tag");
}

#[test]
fn a_detached_head_is_refused() {
    let f = diverged(None);
    f.git(&["checkout", "-q", "--detach"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    let e = cli(&f).merge_branch("topic", false).unwrap_err();
    assert!(format!("{e:#}").contains("No branch checked out"), "{e:#}");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn a_branch_is_not_merged_into_itself() {
    let f = diverged(None);
    let e = cli(&f).merge_branch("main", false).unwrap_err();
    assert!(format!("{e:#}").contains("itself"), "{e:#}");
}

#[test]
fn a_merge_already_in_progress_is_refused() {
    let f = diverged(Some("a.txt"));
    let _ = std::process::Command::new("git").current_dir(f.path()).args(["merge", "topic"]).output().unwrap();
    assert!(f.path().join(".git/MERGE_HEAD").exists());
    let e = cli(&f).merge_branch("topic", false).unwrap_err();
    assert!(format!("{e:#}").contains("already in progress"), "{e:#}");
    assert!(f.path().join(".git/MERGE_HEAD").exists(), "the user's merge was left alone");
}

#[test]
fn a_rebase_in_progress_is_refused() {
    let f = diverged(Some("a.txt"));
    f.git(&["branch", "other", "topic"]);
    let _ = std::process::Command::new("git").current_dir(f.path()).args(["rebase", "topic"]).output().unwrap();
    assert!(f.path().join(".git/rebase-merge").exists());
    let e = cli(&f).merge_branch("other", false).unwrap_err();
    assert!(format!("{e:#}").contains("rebase is already in progress"), "{e:#}");
}

#[test]
fn a_remote_only_branch_is_merged_by_its_remote_tracking_name() {
    let f = base();
    f.add_bare_upstream();
    f.git(&["switch", "-q", "-c", "feature"]);
    f.write("f.txt", "f\n");
    f.commit("feature work", 1_700_000_100);
    f.git(&["push", "-q", "origin", "feature"]);
    f.git(&["switch", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "feature"]);
    f.write("m.txt", "m\n");
    f.commit("main work", 1_700_000_200);
    assert_eq!(cli(&f).merge_branch("origin/feature", true).unwrap(), MergeOutcome::Merged);
    assert!(f.path().join("f.txt").exists());
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Merge remote-tracking branch 'origin/feature'");
    assert!(cli(&f).merge_branch("origin/feature", false).is_err(), "not a local branch");
}

#[test]
fn a_merge_stopped_by_a_hook_is_aborted_with_git_s_message() {
    use std::os::unix::fs::PermissionsExt;
    let f = diverged(None);
    let hook = f.path().join(".git/hooks/pre-merge-commit");
    std::fs::write(&hook, "#!/bin/sh\necho no merges today >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let head = f.git(&["rev-parse", "HEAD"]);
    let e = cli(&f).merge_branch("topic", false).unwrap_err();
    assert!(format!("{e:#}").contains("no merges today") && format!("{e:#}").contains("aborted"), "{e:#}");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    no_merge_in_progress(&f);
}
