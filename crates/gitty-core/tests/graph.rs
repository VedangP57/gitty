mod common;

use std::collections::HashSet;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::graph::{Lanes, topo_walk};
use gitty_core::types::CommitId;

fn merge(f: &Fixture, args: &[&str], epoch: i64) {
    let date = format!("{epoch} +0000");
    let mut all = vec!["merge", "-q", "--no-ff", "--no-edit"];
    all.extend_from_slice(args);
    f.git_env(&all, &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)]);
}

/// main with a merged feature branch, an octopus merge, an unmerged side branch and an
/// unrelated root; subjects with graph characters and separators in them.
fn branchy() -> Fixture {
    let f = Fixture::new();
    let mut t = 1_700_000_000;
    let mut commit = |f: &Fixture, msg: &str| {
        t += 100;
        f.write(&format!("f{t}.txt"), "x\n");
        f.commit(msg, t)
    };
    commit(&f, "root");
    commit(&f, "| * \\ / - _ graph chars");
    f.git(&["switch", "-q", "-c", "feature"]);
    commit(&f, "feature 1 \u{1f} with a separator");
    commit(&f, "feature 2 ünïcødé ✓");
    f.git(&["switch", "-q", "main"]);
    commit(&f, "main 1");
    merge(&f, &["feature"], 1_700_001_000);
    for b in ["o1", "o2", "o3"] {
        f.git(&["switch", "-q", "-c", b, "main"]);
        commit(&f, b);
    }
    f.git(&["switch", "-q", "main"]);
    merge(&f, &["o1", "o2", "o3"], 1_700_002_000);
    f.git(&["switch", "-q", "-c", "side", "main~1"]);
    commit(&f, "side");
    f.git(&["switch", "-q", "--orphan", "other"]);
    commit(&f, "other root");
    f.git(&["switch", "-q", "main"]);
    commit(&f, "main 2");
    f
}

fn all_tips(f: &Fixture) -> Vec<CommitId> {
    f.git(&["for-each-ref", "--format=%(objectname)", "refs/heads"]).lines().filter_map(CommitId::from_hex).collect()
}

fn cli(f: &Fixture) -> GitCli {
    GitCli::new(&Repo::open(f.path()).unwrap())
}

fn walk(cli: &GitCli, tips: &[CommitId]) -> Vec<(CommitId, Vec<CommitId>)> {
    let mut v = Vec::new();
    assert!(topo_walk(cli, tips, &|| false, &mut |id, parents| {
        v.push((id, parents.to_vec()));
        true
    })
    .unwrap());
    v
}

fn git_walk(f: &Fixture) -> Vec<(CommitId, Vec<CommitId>)> {
    f.git(&["rev-list", "--topo-order", "--parents", "--all"])
        .lines()
        .map(|l| {
            let mut ids = l.split(' ').filter_map(CommitId::from_hex);
            (ids.next().unwrap(), ids.collect())
        })
        .collect()
}

#[test]
fn the_walk_lists_children_before_parents_with_and_without_a_commit_graph() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    let graph_file = f.path().join(".git/objects/info/commit-graph");
    for with_graph in [false, true] {
        if with_graph {
            f.git(&["commit-graph", "write", "--reachable"]);
        }
        assert_eq!(graph_file.exists(), with_graph);
        let walked = walk(&cli, &tips);
        assert_eq!(walked, git_walk(&f), "commit-graph: {with_graph}");
        let count: usize = f.git(&["rev-list", "--count", "--all"]).parse().unwrap();
        assert_eq!(walked.len(), count);
        let mut seen = HashSet::new();
        for (id, parents) in &walked {
            assert!(parents.iter().all(|p| !seen.contains(p)), "a parent before its child");
            seen.insert(*id);
        }
        // the history keeps the order and lays out one row per commit
        let h = Repo::open(f.path()).unwrap().handle();
        let mut history = h.empty_history();
        let mut lanes = Lanes::default();
        for (id, parents) in &walked {
            history.push_laid_out(*id, parents, &mut lanes);
        }
        assert_eq!(history.ids(0..walked.len()), walked.iter().map(|w| w.0).collect::<Vec<_>>());
        for i in 0..walked.len() {
            let row = history.graph_row(i).unwrap();
            assert_eq!(row.text().matches('●').count(), 1, "row {i}: {}", row.text());
        }
        assert!(history.graph_row(walked.len()).is_none());
    }
}

#[test]
fn the_octopus_and_the_merges_fan_out_on_their_own_row() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    let walked = walk(&cli, &tips);
    let h = Repo::open(f.path()).unwrap().handle();
    let mut history = h.empty_history();
    let mut lanes = Lanes::default();
    for (id, parents) in &walked {
        history.push_laid_out(*id, parents, &mut lanes);
    }
    for (i, (_, parents)) in walked.iter().enumerate() {
        let t = history.graph_row(i).unwrap().text();
        // a merge opens (╮, ╭, ┬) or links (┤, ├, ┼) one lane per other parent
        let links = t.matches(['╮', '╭', '┬', '┤', '├', '┼']).count();
        assert!(links >= parents.len().saturating_sub(1), "row {i} {t:?} for {} parents", parents.len());
    }
}

#[test]
fn tens_of_thousands_of_tips_go_through_stdin() {
    // 30,000 ids are 1.2 MB of hex: past macOS's 1 MB argument limit
    let f = Fixture::new();
    let mut s = String::new();
    for i in 1..=30_000 {
        s.push_str(&format!("commit refs/heads/main\nmark :{i}\ncommitter A <a@a> {} +0000\ndata 1\nc\n", 1_700_000_000 + i));
        if i > 1 {
            s.push_str(&format!("from :{}\n", i - 1));
        }
        s.push('\n');
    }
    let mut c = std::process::Command::new("git");
    c.current_dir(f.path()).args(["fast-import", "--quiet"]).env("GIT_CONFIG_GLOBAL", "/dev/null").stdin(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), s.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
    let cli = cli(&f);
    let tips: Vec<CommitId> = f.git(&["rev-list", "main"]).lines().filter_map(CommitId::from_hex).collect();
    assert_eq!(tips.len(), 30_000);
    let walked: Vec<CommitId> = walk(&cli, &tips).into_iter().map(|w| w.0).collect();
    assert_eq!(walked, tips, "every commit is a tip of its own, newest first");
}

#[test]
fn a_detached_head_walks_its_own_history() {
    let f = branchy();
    let cli = cli(&f);
    f.git(&["switch", "-q", "--detach", "feature~1"]);
    let head = CommitId::from_hex(&f.git(&["rev-parse", "HEAD"])).unwrap();
    let walked = walk(&cli, &[head]);
    assert_eq!(walked.len(), 3, "feature~1, its parent and the root");
    assert_eq!(walked[0].0, head);
    assert!(walked[2].1.is_empty(), "the root has no parents");
}

#[test]
fn an_empty_repository_walks_nothing() {
    let f = Fixture::new();
    assert!(walk(&cli(&f), &[]).is_empty());
}

#[test]
fn a_cancelled_walk_stops() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    assert!(!topo_walk(&cli, &tips, &|| true, &mut |_, _| true).unwrap());
}
