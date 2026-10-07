mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn on_branch(name: &str) -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", name]);
    f
}

fn err(f: &Fixture, branch: &str) -> String {
    cli(f).pr_url(branch).unwrap_err().to_string()
}

#[test]
fn pushed_branch_gets_its_pull_request_url() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feat/x").unwrap(), "https://github.com/acme/widgets/pull/new/feat/x");
}

#[test]
fn unpushed_branch_asks_for_a_push() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    assert!(err(&f, "feat/x").contains("Push the branch first"));
}

#[test]
fn no_remote_is_reported() {
    let f = on_branch("feat/x");
    assert!(err(&f, "feat/x").contains("No remote to open"));
}

#[test]
fn non_github_remote_is_reported() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "git@gitlab.com:acme/widgets.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    assert_eq!(err(&f, "feat/x"), "Only GitHub remotes are supported");
}

#[test]
fn differently_named_upstream_branch_is_used() {
    let f = on_branch("mine");
    f.git(&["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
    f.git(&["config", "branch.mine.remote", "origin"]);
    f.git(&["config", "branch.mine.merge", "refs/heads/theirs"]);
    f.git(&["update-ref", "refs/remotes/origin/theirs", "HEAD"]);
    assert_eq!(cli(&f).pr_url("mine").unwrap(), "https://github.com/acme/widgets/pull/new/theirs");
}

#[test]
fn hostile_branch_names_are_refused() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    assert!(cli(&f).pr_url("-x").is_err());
    assert!(cli(&f).pr_url("a b").is_err());
}
