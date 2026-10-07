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
fn branch_cut_from_another_branch_uses_its_own_name() {
    let f = on_branch("feature");
    f.git(&["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
    f.git(&["config", "branch.feature.remote", "origin"]);
    f.git(&["config", "branch.feature.merge", "refs/heads/main"]);
    f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    assert!(err(&f, "feature").contains("Push the branch first"));
    f.git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feature").unwrap(), "https://github.com/acme/widgets/pull/new/feature");
}

#[test]
fn local_upstream_falls_back_like_push() {
    let f = on_branch("feature");
    f.git(&["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
    f.git(&["config", "branch.feature.remote", "."]);
    f.git(&["config", "branch.feature.merge", "refs/heads/main"]);
    assert!(err(&f, "feature").contains("Push the branch first"));
    f.git(&["update-ref", "refs/remotes/origin/feature", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feature").unwrap(), "https://github.com/acme/widgets/pull/new/feature");
}

#[test]
fn fork_workflow_uses_the_push_remote() {
    let f = on_branch("feature");
    f.git(&["remote", "add", "upstream", "https://github.com/acme/widgets.git"]);
    f.git(&["remote", "add", "fork", "https://github.com/me/widgets.git"]);
    f.git(&["config", "branch.feature.remote", "upstream"]);
    f.git(&["config", "branch.feature.merge", "refs/heads/main"]);
    f.git(&["config", "remote.pushDefault", "fork"]);
    f.git(&["update-ref", "refs/remotes/upstream/feature", "HEAD"]);
    assert!(err(&f, "feature").contains("Push the branch first"));
    f.git(&["update-ref", "refs/remotes/fork/feature", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feature").unwrap(), "https://github.com/me/widgets/pull/new/feature");
    f.git(&["config", "branch.feature.pushRemote", "upstream"]);
    assert_eq!(cli(&f).pr_url("feature").unwrap(), "https://github.com/acme/widgets/pull/new/feature");
}

#[test]
fn insteadof_rewrite_is_applied() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "gh:acme/widgets.git"]);
    f.git(&["config", "url.https://github.com/.insteadOf", "gh:"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feat/x").unwrap(), "https://github.com/acme/widgets/pull/new/feat/x");
}

#[test]
fn push_url_wins_over_url() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "https://github.com/acme/widgets.git"]);
    f.git(&["remote", "set-url", "--push", "origin", "git@github.com:me/fork.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    assert_eq!(cli(&f).pr_url("feat/x").unwrap(), "https://github.com/me/fork/pull/new/feat/x");
}

#[test]
fn hostile_branch_names_are_refused() {
    let f = on_branch("feat/x");
    f.git(&["remote", "add", "origin", "git@github.com:acme/widgets.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    assert!(cli(&f).pr_url("feat/x").is_ok());
    for name in ["-x", "a b"] {
        assert_eq!(err(&f, name), format!("`{name}` is not a valid branch name"));
    }
}
