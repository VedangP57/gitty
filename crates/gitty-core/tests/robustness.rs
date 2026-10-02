//! Regression tests from the M1 review: repos that git handles and gitty must not choke on.
mod common;
use common::Fixture;
use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn walk_all(repo: &Repo) -> Vec<CommitId> {
    let h = repo.handle();
    let refs = h.refs().unwrap();
    let mut w = h.walker(&refs.tips(HistoryScope::AllRefs)).unwrap();
    let mut hist = w.new_history();
    while w.step(&h, &mut hist, 64).unwrap() {}
    hist.ids(0..hist.len())
}

fn git_log_all(dir: &std::path::Path) -> Vec<CommitId> {
    let out = std::process::Command::new("git").current_dir(dir).args(["log", "--all", "--format=%H"]).output().unwrap();
    String::from_utf8(out.stdout).unwrap().lines().map(|l| CommitId::from_hex(l).unwrap()).collect()
}

#[test]
fn shallow_clone_walks_and_diffs_boundary() {
    let f = Fixture::new();
    for i in 0..5 {
        f.write("a.txt", format!("v{i}\n"));
        f.commit(&format!("c{i}"), 1_700_000_000 + i * 100);
    }
    let shallow = f.shallow_clone(2);
    let path = shallow.path().join("repo");
    let repo = Repo::open(&path).unwrap();
    let got = walk_all(&repo);
    assert_eq!(got, git_log_all(&path));
    assert_eq!(got.len(), 2);
    // the boundary commit diffs against the empty tree, like git shows it
    let boundary = *got.last().unwrap();
    let files = repo.handle().commit_files(boundary, false).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].status, gitty_core::commit_files::FileStatus::Added);
    assert!(repo.handle().decode_row(boundary).unwrap().parents.is_empty());
}

#[test]
fn malformed_committer_date_does_not_abort_walk() {
    let f = Fixture::new();
    f.write("a.txt", "x\n");
    let base = f.commit("base", 1_700_000_000);
    let tree = f.git(&["rev-parse", "HEAD^{tree}"]);
    for (i, line) in ["committer t <t@t> notadate +0000", "committer t <t@t> 99999999999999999999 +0000", "committer t <t@t>"]
        .iter()
        .enumerate()
    {
        f.raw_commit(&tree, &base, line, &format!("bad{i}"));
    }
    let repo = Repo::open(f.path()).unwrap();
    let got = walk_all(&repo);
    assert_eq!(got.len(), 4, "{got:?}");
    for id in &got {
        repo.handle().decode_row(*id).unwrap();
    }
}

#[test]
fn corrupt_commit_graph_falls_back() {
    let f = Fixture::new();
    f.commit("a", 1_700_000_000);
    f.add_bare_upstream();
    let c = f.commit("b", 1_700_000_100);
    f.git(&["commit-graph", "write", "--reachable", "--split"]);
    use std::os::unix::fs::PermissionsExt;
    let layers = f.path().join(".git/objects/info/commit-graphs");
    for e in std::fs::read_dir(&layers).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "graph") {
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap(); // git writes 0444
            std::fs::write(&p, b"CGPH\x01\x01").unwrap(); // too small even for an empty graph
        }
    }
    let repo = Repo::open(f.path()).unwrap();
    assert_eq!(walk_all(&repo).len(), 2);
    let refs = repo.handle().refs().unwrap();
    let ab = repo.handle().ahead_behind(CommitId::from_hex(&c).unwrap(), refs.upstream.unwrap().1).unwrap();
    assert_eq!(ab.ahead.len(), 1);
}

/// Pins the chosen order under clock skew: plain `git log --all` (commit-time heap), not
/// `--date-order` (which adds a topological constraint we deliberately don't pay for).
#[test]
fn clock_skew_matches_git_log_default_order() {
    let f = Fixture::new();
    f.commit("a", 1_700_000_500);
    f.commit("b (clock behind its parent)", 1_700_000_100);
    f.commit("c", 1_700_000_300);
    for graph in [false, true] {
        if graph {
            f.git(&["commit-graph", "write", "--reachable"]);
        }
        let repo = Repo::open(f.path()).unwrap();
        assert_eq!(walk_all(&repo), git_log_all(&f.path()), "graph={graph}");
    }
}
