mod common;

use std::time::{Duration, Instant};

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::net::{Job, Mode, NetCmd, Outcome, force_push_plan, push_target, remote_of, removal};

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn run(f: &Fixture, cmd: NetCmd, mode: Mode) -> (Outcome, Vec<f32>) {
    let job = Job::spawn(&cli(f), cmd, mode).unwrap();
    let mut seen = Vec::new();
    let out = job.wait(&mut |p| seen.push(p));
    (out, seen)
}

fn base() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let bare = f.add_bare_upstream();
    (f, bare)
}

#[test]
fn fetch_then_fast_forward() {
    let (f, bare) = base();
    common::push_as_someone_else(&bare, "b.txt");
    let remote = remote_of(&cli(&f), Some("main")).unwrap();
    assert_eq!(remote, "origin");
    let (out, _) = run(&f, NetCmd::Fetch { remote }, Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(f.git(&["rev-list", "--count", "HEAD..origin/main"]), "1");
    let (out, _) = run(&f, NetCmd::FfMerge, Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert!(f.path().join("b.txt").exists());
}

#[test]
fn diverged_pull_is_reported_and_rebase_resolves_it() {
    let (f, bare) = base();
    common::push_as_someone_else(&bare, "b.txt");
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    let (out, _) = run(&f, NetCmd::FfMerge, Mode::Background);
    assert!(matches!(out, Outcome::Diverged), "{out:?}");
    let (out, _) = run(&f, NetCmd::Rebase, Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(f.git(&["rev-list", "--count", "origin/main..HEAD"]), "1");
    assert_eq!(f.git(&["rev-list", "--count", "HEAD..origin/main"]), "0");
}

#[test]
fn push_updates_the_remote_and_rejects_non_fast_forward() {
    let (f, bare) = base();
    f.write("c.txt", "mine\n");
    let head = f.commit("mine", 1_700_000_100);
    let t = push_target(&cli(&f), "main").unwrap();
    assert!(!t.set_upstream);
    let (out, _) = run(&f, NetCmd::Push(t), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    let remote_head = std::process::Command::new("git").arg("--git-dir").arg(&bare).args(["rev-parse", "main"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&remote_head.stdout).trim(), head);

    common::push_as_someone_else(&bare, "d.txt");
    f.write("e.txt", "mine again\n");
    f.commit("mine again", 1_700_000_200);
    let (out, _) = run(&f, NetCmd::Push(push_target(&cli(&f), "main").unwrap()), Mode::Background);
    match out {
        Outcome::Rejected { refs, .. } => assert!(refs.iter().any(|r| r.flag == '!' && r.remote.ends_with("main")), "{refs:?}"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn first_push_of_a_branch_sets_its_upstream() {
    let (f, _bare) = base();
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    let t = push_target(&cli(&f), "topic").unwrap();
    assert!(t.set_upstream);
    assert_eq!(t.remote, "origin");
    let (out, _) = run(&f, NetCmd::Push(t), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(f.git(&["config", "branch.topic.merge"]), "refs/heads/topic");
}

#[test]
fn cancel_kills_the_whole_process_group() {
    let (f, _) = base();
    f.script_remote("hang", "sleep 30 & sleep 30");
    let job = Job::spawn(&cli(&f), NetCmd::Fetch { remote: "hang".into() }, Mode::Background).unwrap();
    let cancel = job.cancel_handle();
    let pgid = cancel.pgid();
    let t = Instant::now();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let out = job.wait(&mut |_| {});
    assert!(matches!(out, Outcome::Cancelled), "{out:?}");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    std::thread::sleep(Duration::from_millis(200));
    // SAFETY: signal 0 only checks for existence
    let alive = unsafe { libc::killpg(pgid, 0) } == 0;
    assert!(!alive, "processes of group {pgid} survived");
}

#[test]
fn background_auth_failure_is_needs_auth() {
    let (f, _) = base();
    f.script_remote("locked", "echo \"fatal: could not read Username for 'https://example.com': terminal prompts disabled\" >&2; exit 128");
    let (out, _) = run(&f, NetCmd::Fetch { remote: "locked".into() }, Mode::Background);
    assert!(matches!(out, Outcome::NeedsAuth { .. }), "{out:?}");
}

#[test]
fn a_branch_tracking_another_name_never_pushes_onto_it() {
    let (f, bare) = base();
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["checkout", "-q", "-b", "feature", "origin/main"]);
    assert_eq!(f.git(&["config", "branch.feature.merge"]), "refs/heads/main");
    f.write("feat.txt", "f\n");
    f.commit("feature work", 1_700_000_100);
    let main_before = std::process::Command::new("git").arg("--git-dir").arg(&bare).args(["rev-parse", "main"]).output().unwrap().stdout;
    let t = push_target(&cli(&f), "feature").unwrap();
    assert_eq!(t.refspec, "refs/heads/feature:refs/heads/feature");
    let (out, _) = run(&f, NetCmd::Push(t), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    let main_after = std::process::Command::new("git").arg("--git-dir").arg(&bare).args(["rev-parse", "main"]).output().unwrap().stdout;
    assert_eq!(main_after, main_before, "the remote's main is untouched");
    assert_eq!(f.git(&["config", "branch.feature.merge"]), "refs/heads/main", "tracking left as the user set it");
}

#[test]
fn push_remote_settings_are_honoured() {
    let (f, _) = base();
    let other = f.path().parent().unwrap().join("other.git");
    assert!(std::process::Command::new("git").args(["init", "-q", "--bare"]).arg(&other).status().unwrap().success());
    f.git(&["remote", "add", "fork", other.to_str().unwrap()]);
    f.git(&["config", "remote.pushDefault", "fork"]);
    assert_eq!(push_target(&cli(&f), "main").unwrap().remote, "fork");
    f.git(&["config", "branch.main.pushRemote", "origin"]);
    assert_eq!(push_target(&cli(&f), "main").unwrap().remote, "origin");
}

#[test]
fn merge_conflicts_are_explained() {
    let (f, bare) = base();
    common::push_as_someone_else(&bare, "a.txt");
    f.write("a.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    match run(&f, NetCmd::Merge, Mode::Background).0 {
        Outcome::Failed { detail } => assert!(detail.contains("CONFLICT"), "{detail:?}"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn rebase_detail_drops_progress_repaints() {
    let (f, bare) = base();
    common::push_as_someone_else(&bare, "a.txt");
    f.write("a.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    match run(&f, NetCmd::Rebase, Mode::Background).0 {
        Outcome::Failed { detail } => {
            assert!(!detail.lines().next().unwrap_or("").starts_with("Rebasing ("), "{detail:?}");
            assert!(detail.contains("CONFLICT"), "{detail:?}");
        }
        o => panic!("{o:?}"),
    }
    f.git(&["rebase", "--abort"]);
}

#[test]
fn background_jobs_never_fall_back_to_core_askpass() {
    let (f, _) = base();
    let marker = f.path().join("asked");
    let ask = f.path().join("ask.sh");
    std::fs::write(&ask, format!("#!/bin/sh\ntouch {}\necho x\n", marker.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ask, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.git(&["config", "core.askPass", ask.to_str().unwrap()]);
    // the transport asks git for credentials, as an https remote would
    f.script_remote("needs", "printf 'protocol=https\\nhost=example.com\\n\\n' | git credential fill >/dev/null 2>&1; echo 'fatal: Authentication failed' >&2; exit 128");
    let (out, _) = run(&f, NetCmd::Fetch { remote: "needs".into() }, Mode::Background);
    assert!(matches!(out, Outcome::NeedsAuth { .. }), "{out:?}");
    assert!(!marker.exists(), "core.askPass was run by a background job");
}

#[test]
fn a_cancel_after_git_already_finished_reports_what_git_did() {
    let (f, _bare) = base();
    let job = Job::spawn(&cli(&f), NetCmd::Fetch { remote: "origin".into() }, Mode::Background).unwrap();
    // git is done (not reaped yet) when the cancel arrives
    std::thread::sleep(Duration::from_millis(800));
    job.cancel_handle().cancel();
    let out = job.wait(&mut |_| {});
    assert!(matches!(out, Outcome::Ok { .. }), "the fetch succeeded: {out:?}");
}

#[test]
fn a_helper_holding_stderr_cannot_block_the_result() {
    let (f, _) = base();
    // the transport leaves a child behind that keeps git's stderr open, then fails
    f.script_remote("linger", "sleep 30 </dev/null >/dev/null &\nexit 1");
    let job = Job::spawn(&cli(&f), NetCmd::Fetch { remote: "linger".into() }, Mode::Background).unwrap();
    let pgid = job.cancel_handle().pgid();
    let t = Instant::now();
    let out = job.wait(&mut |_| {});
    let took = t.elapsed();
    // SAFETY: cleanup of the test's own group
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert!(took < Duration::from_secs(4), "waited {took:?} for a lingering stderr holder");
}

#[test]
fn a_hook_decline_is_not_a_pull_first_rejection() {
    let (f, bare) = base();
    let hook = bare.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'policy: no pushes today' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let (out, _) = run(&f, NetCmd::Push(push_target(&cli(&f), "main").unwrap()), Mode::Background);
    match out {
        Outcome::Rejected { refs, .. } => {
            assert!(refs.iter().all(|r| !r.needs_pull()), "{refs:?}");
            assert!(refs[0].summary.contains("hook declined"), "{refs:?}");
        }
        o => panic!("{o:?}"),
    }
    std::fs::remove_file(&hook).unwrap();
    common::push_as_someone_else(&bare, "d.txt");
    f.write("e.txt", "again\n");
    f.commit("again", 1_700_000_200);
    match run(&f, NetCmd::Push(push_target(&cli(&f), "main").unwrap()), Mode::Background).0 {
        Outcome::Rejected { refs, .. } => assert!(refs.iter().any(|r| r.needs_pull()), "{refs:?}"),
        o => panic!("{o:?}"),
    }
}

/// A pushed `topic` whose tip was then amended: the next normal push is rejected.
fn amended_topic() -> (Fixture, std::path::PathBuf) {
    let (f, bare) = base();
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    let (out, _) = run(&f, NetCmd::Push(push_target(&cli(&f), "topic").unwrap()), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    f.write("t.txt", "t amended\n");
    f.git_env(&["commit", "-q", "-a", "--amend", "-m", "topic amended"], &[("GIT_COMMITTER_DATE", "1700000200 +0000".into())]);
    (f, bare)
}

fn remote_head(bare: &std::path::Path, branch: &str) -> String {
    let out = std::process::Command::new("git").arg("--git-dir").arg(bare).args(["rev-parse", branch]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Another person's clone commits on `topic` and pushes it.
fn someone_else_pushes_topic(bare: &std::path::Path) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let o = tmp.path().join("o");
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(tmp.path(), &["clone", "-q", "-b", "topic", bare.to_str().unwrap(), "o"]);
    std::fs::write(o.join("theirs.txt"), "theirs\n").unwrap();
    git(&o, &["add", "-A"]);
    git(&o, &["-c", "user.name=O", "-c", "user.email=o@example.com", "commit", "-qm", "theirs"]);
    git(&o, &["push", "-q", "origin", "topic"]);
    git(&o, &["rev-parse", "HEAD"])
}

#[test]
fn force_with_lease_replaces_an_amended_commit() {
    let (f, bare) = amended_topic();
    let (out, _) = run(&f, NetCmd::Push(push_target(&cli(&f), "topic").unwrap()), Mode::Background);
    match out {
        Outcome::Rejected { refs, .. } => assert!(refs.iter().any(|r| r.needs_pull()), "{refs:?}"),
        o => panic!("{o:?}"),
    }
    let plan = force_push_plan(&cli(&f), "topic").unwrap();
    assert_eq!(plan.expected, f.git(&["rev-parse", "refs/remotes/origin/topic"]));
    let (out, _) = run(&f, NetCmd::ForcePush(plan), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(remote_head(&bare, "topic"), f.git(&["rev-parse", "HEAD"]));
}

#[test]
fn force_with_lease_refuses_when_the_remote_moved_after_the_last_fetch() {
    let (f, bare) = amended_topic();
    let plan = force_push_plan(&cli(&f), "topic").unwrap();
    let theirs = someone_else_pushes_topic(&bare);
    let (out, _) = run(&f, NetCmd::ForcePush(plan), Mode::Background);
    match out {
        Outcome::Rejected { refs, .. } => assert!(refs.iter().any(|r| r.is_stale()), "{refs:?}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(remote_head(&bare, "topic"), theirs);
}

#[test]
fn a_fetch_between_the_rejection_and_the_confirm_cannot_unlock_unseen_commits() {
    let (f, bare) = amended_topic();
    let plan = force_push_plan(&cli(&f), "topic").unwrap();
    let theirs = someone_else_pushes_topic(&bare);
    // auto-fetch moves the tracking ref, which a bare --force-with-lease would trust
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    assert_eq!(f.git(&["rev-parse", "refs/remotes/origin/topic"]), theirs);
    let (out, _) = run(&f, NetCmd::ForcePush(plan), Mode::Background);
    match out {
        Outcome::Rejected { refs, .. } => assert!(refs.iter().any(|r| r.is_stale()), "{refs:?}"),
        o => panic!("{o:?}"),
    }
    assert_eq!(remote_head(&bare, "topic"), theirs);
}

#[test]
fn force_push_is_blocked_for_main_master_and_the_default_branch() {
    let (f, _bare) = amended_topic();
    let blocked = |b: &str| force_push_plan(&cli(&f), b).unwrap_err().to_string();
    assert_eq!(blocked("main"), "Force pushing main is blocked in gitty");
    f.git(&["branch", "master"]);
    assert_eq!(blocked("master"), "Force pushing master is blocked in gitty");
    assert!(force_push_plan(&cli(&f), "topic").is_ok());
    f.git(&["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/topic"]);
    assert_eq!(blocked("topic"), "Force pushing topic is blocked in gitty");
}

#[test]
fn force_push_needs_a_remote_tracking_branch() {
    let (f, _bare) = base();
    f.git(&["checkout", "-q", "-b", "fresh"]);
    f.write("x.txt", "x\n");
    f.commit("fresh", 1_700_000_100);
    let e = force_push_plan(&cli(&f), "fresh").unwrap_err().to_string();
    assert!(e.contains("no remote-tracking branch"), "{e}");
}

#[test]
fn the_plan_counts_what_the_push_removes_and_whose_it_was() {
    let (f, bare) = amended_topic();
    let r = force_push_plan(&cli(&f), "topic").unwrap().removal.unwrap();
    assert_eq!((r.total, r.others, r.top.len()), (1, 0, 0), "an amend replaces only the user's own commit");
    // a teammate's commit comes in with an auto-fetch
    someone_else_pushes_topic(&bare);
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    let r = force_push_plan(&cli(&f), "topic").unwrap().removal.unwrap();
    assert_eq!((r.total, r.others), (2, 1));
    assert!(r.top[0].contains("O: theirs"), "{:?}", r.top);
}

#[test]
fn a_teammate_commit_you_once_pulled_is_still_theirs() {
    let (f, bare) = base();
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    run(&f, NetCmd::Push(push_target(&cli(&f), "topic").unwrap()), Mode::Background);
    someone_else_pushes_topic(&bare);
    run(&f, NetCmd::Fetch { remote: "origin".into() }, Mode::Background);
    f.git(&["merge", "-q", "--ff-only", "origin/topic"]);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    let r = force_push_plan(&cli(&f), "topic").unwrap().removal.unwrap();
    assert_eq!((r.total, r.others), (1, 1), "in the reflog, but not the user's");
    assert!(r.top[0].contains("O: theirs"), "{:?}", r.top);
}

#[test]
fn a_removal_git_cannot_work_out_is_unknown_not_empty() {
    let (f, _bare) = amended_topic();
    assert_eq!(removal(&cli(&f), "topic", &"0".repeat(40)), None);
}

#[test]
fn main_and_master_are_blocked_in_any_case() {
    let (f, _bare) = amended_topic();
    f.git(&["branch", "Master"]);
    assert_eq!(force_push_plan(&cli(&f), "Master").unwrap_err().to_string(), "Force pushing Master is blocked in gitty");
}

#[test]
fn the_lease_follows_a_differently_named_upstream() {
    let (f, bare) = base();
    f.git(&["checkout", "-q", "-b", "feat"]);
    f.write("t.txt", "t\n");
    f.commit("feat", 1_700_000_100);
    f.git(&["push", "-q", "origin", "feat:refs/heads/other"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["branch", "-q", "--set-upstream-to=origin/other", "feat"]);
    f.git(&["config", "push.default", "upstream"]);
    f.git_env(&["commit", "-q", "-a", "--amend", "-m", "feat amended", "--allow-empty"], &[("GIT_COMMITTER_DATE", "1700000200 +0000".into())]);
    let plan = force_push_plan(&cli(&f), "feat").unwrap();
    assert_eq!((plan.remote_branch(), plan.branch.as_str()), ("other", "feat"));
    assert_eq!(plan.expected, f.git(&["rev-parse", "refs/remotes/origin/other"]));
    let (out, _) = run(&f, NetCmd::ForcePush(plan), Mode::Background);
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    assert_eq!(remote_head(&bare, "other"), f.git(&["rev-parse", "HEAD"]));
}

#[test]
fn the_lease_is_read_from_the_push_remote_not_the_upstream_remote() {
    let (f, _bare) = amended_topic();
    let fork = f.path().parent().unwrap().join("fork.git");
    std::process::Command::new("git").args(["init", "-q", "--bare"]).arg(&fork).output().unwrap();
    f.git(&["remote", "add", "fork", fork.to_str().unwrap()]);
    f.git(&["config", "branch.topic.pushRemote", "fork"]);
    let e = force_push_plan(&cli(&f), "topic").unwrap_err().to_string();
    assert!(e.contains("no remote-tracking branch"), "origin/topic must not stand in for fork/topic: {e}");
}
