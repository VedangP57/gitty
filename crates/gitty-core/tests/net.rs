mod common;

use std::time::{Duration, Instant};

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::net::{Job, Mode, NetCmd, Outcome, push_target, remote_of};

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
        Outcome::Rejected { refs } => assert!(refs.iter().any(|r| r.flag == '!' && r.remote.ends_with("main")), "{refs:?}"),
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
