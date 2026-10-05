mod common;
use common::Fixture;
use gitty_core::{CommitId, Repo};
use std::collections::HashSet;

type Case = (CommitId, CommitId, HashSet<CommitId>, HashSet<CommitId>);

/// main has 2 unpushed commits; origin/main has 1 unpulled commit.
fn diverge(f: &Fixture) -> Case {
    f.commit("base", 1_700_000_000);
    f.add_bare_upstream();
    let a1 = f.commit("local 1", 1_700_000_100);
    let a2 = f.commit("local 2", 1_700_000_200);
    f.git(&["checkout", "-q", "-b", "tmp", "origin/main"]);
    f.write("r.txt", "r\n");
    let b1 = f.commit("remote 1", 1_700_000_150);
    f.git(&["push", "-q", "origin", "tmp:main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "tmp"]);
    let id = |s: &str| CommitId::from_hex(s).unwrap();
    (id(&a2), id(&b1), [id(&a1), id(&a2)].into(), [id(&b1)].into())
}

fn assert_ab(f: &Fixture, (local, up, exp_a, exp_b): Case) {
    let h = Repo::open(f.path()).unwrap().handle();
    let ab = h.ahead_behind(local, up).unwrap();
    assert_eq!(ab.ahead.iter().copied().collect::<HashSet<_>>(), exp_a);
    assert_eq!(ab.behind.iter().copied().collect::<HashSet<_>>(), exp_b);
    assert_eq!(ab.ahead.len(), exp_a.len(), "duplicates in ahead");
    assert_eq!(ab.behind.len(), exp_b.len(), "duplicates in behind");
}

#[test]
fn ahead_behind_without_graph() {
    let f = Fixture::new();
    let case = diverge(&f);
    assert_ab(&f, case);
}

#[test]
fn ahead_behind_with_graph() {
    let f = Fixture::new();
    let case = diverge(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    assert_ab(&f, case);
}

#[test]
fn ahead_behind_graph_with_merges() {
    // local merges a side branch that upstream lacks; upstream has its own commit
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    f.add_bare_upstream();
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("s.txt", "s\n");
    let s1 = f.commit("side", 1_700_000_050);
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "m\n");
    let m1 = f.commit("main", 1_700_000_060);
    let date = "1700000070 +0000".to_string();
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge", "side"], &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)]);
    let mg = f.git(&["rev-parse", "HEAD"]);
    f.git(&["checkout", "-q", "-b", "tmp", "origin/main"]);
    f.write("r.txt", "r\n");
    let r1 = f.commit("remote", 1_700_000_080);
    f.git(&["push", "-q", "origin", "tmp:main"]);
    f.git(&["fetch", "-q", "origin"]);
    f.git(&["checkout", "-q", "main"]);
    f.git(&["commit-graph", "write", "--reachable"]);
    let id = |s: &str| CommitId::from_hex(s).unwrap();
    assert_ab(&f, (id(&mg), id(&r1), [id(&s1), id(&m1), id(&mg)].into(), [id(&r1)].into()));
}

#[test]
fn equal_tips_is_empty() {
    let f = Fixture::new();
    let c = CommitId::from_hex(&f.commit("x", 1_700_000_000)).unwrap();
    let ab = Repo::open(f.path()).unwrap().handle().ahead_behind(c, c).unwrap();
    assert!(ab.ahead.is_empty() && ab.behind.is_empty());
}

#[test]
fn an_unpublished_branch_lists_the_commits_no_remote_branch_has() {
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    f.add_bare_upstream();
    f.write("p.txt", "pushed\n");
    f.commit("pushed", 1_700_000_050);
    f.git(&["push", "-q", "origin", "main"]);
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("a.txt", "a\n");
    let a1 = f.commit("local 1", 1_700_000_100);
    f.write("b.txt", "b\n");
    let a2 = f.commit("local 2", 1_700_000_200);
    let id = |s: &str| CommitId::from_hex(s).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let got = h.unpublished(id(&a2)).unwrap();
    assert_eq!(got.iter().copied().collect::<HashSet<_>>(), [id(&a1), id(&a2)].into());
    assert_eq!(got.len(), 2, "no duplicates");
    // pushed under another name, nothing is left to publish
    f.git(&["push", "-q", "origin", "feature:elsewhere"]);
    assert!(h.unpublished(id(&a2)).unwrap().is_empty());
}
