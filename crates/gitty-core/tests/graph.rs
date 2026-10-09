mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::graph::{GraphArt, topo_walk};
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

fn ids(cli: &GitCli, tips: &[CommitId]) -> Vec<CommitId> {
    let mut v = Vec::new();
    assert!(topo_walk(cli, tips, &|| false, &mut |id| {
        v.push(id);
        true
    })
    .unwrap());
    v
}

fn rows(a: &GraphArt, n: usize) -> Vec<(CommitId, String, Vec<String>)> {
    (0..n).map(|i| (a.id(i), a.commit_line(i).to_string(), a.connectors(i).map(String::from).collect())).collect()
}

#[test]
fn the_walk_and_the_art_list_the_same_commits_in_the_same_order() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    let walked = ids(&cli, &tips);
    let art = GraphArt::load(&cli, &tips, 1000, &|| false).unwrap().unwrap();
    assert!(art.complete());
    assert_eq!((0..art.len()).map(|i| art.id(i)).collect::<Vec<_>>(), walked);
    let count: usize = f.git(&["rev-list", "--count", "--all"]).parse().unwrap();
    assert_eq!(walked.len(), count);
    // the merges draw their fan-out below them, and every commit line has exactly one dot
    let merges: Vec<CommitId> = f.git(&["rev-list", "--merges", "--all"]).lines().filter_map(CommitId::from_hex).collect();
    assert_eq!(merges.len(), 2);
    for i in 0..art.len() {
        assert_eq!(art.commit_line(i).matches('*').count(), 1, "{:?}", art.commit_line(i));
        if merges.contains(&art.id(i)) {
            assert!(art.connector_count(i) >= 1, "merge row {i} has no connector");
        }
    }
    let octopus = (0..art.len()).find(|&i| art.id(i) == merges[0]).unwrap();
    assert!(art.commit_line(octopus).starts_with("*-") || art.commit_line(octopus).contains("*-"), "{:?}", art.commit_line(octopus));
}

#[test]
fn pages_continue_the_lanes_of_the_full_graph() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    let full = GraphArt::load(&cli, &tips, 1000, &|| false).unwrap().unwrap();
    for k in 1..full.len() {
        let page = GraphArt::load(&cli, &tips, k, &|| false).unwrap().unwrap();
        assert_eq!(page.len(), k);
        assert!(!page.complete());
        assert_eq!(rows(&page, k), rows(&full, k), "page of {k} rows");
    }
    let exact = GraphArt::load(&cli, &tips, full.len(), &|| false).unwrap().unwrap();
    assert!(exact.complete());
    assert_eq!(rows(&exact, full.len()), rows(&full, full.len()));
}

#[test]
fn a_history_keeps_the_topological_order_with_and_without_a_commit_graph() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    let graph_file = f.path().join(".git/objects/info/commit-graph");
    let mut orders = Vec::new();
    for with_graph in [false, true] {
        if with_graph {
            f.git(&["commit-graph", "write", "--reachable"]);
        }
        assert_eq!(graph_file.exists(), with_graph);
        // git's own order, the walk's, the graph's and the history's must all be one
        let git: Vec<CommitId> = f.git(&["rev-list", "--topo-order", "--all"]).lines().filter_map(CommitId::from_hex).collect();
        let walked = ids(&cli, &tips);
        assert_eq!(walked, git, "commit-graph: {with_graph}");
        let art = GraphArt::load(&cli, &tips, 1000, &|| false).unwrap().unwrap();
        assert_eq!((0..art.len()).map(|i| art.id(i)).collect::<Vec<_>>(), git, "commit-graph: {with_graph}");
        let h = Repo::open(f.path()).unwrap().handle();
        let mut history = h.empty_history();
        for &id in &walked {
            history.push(id);
        }
        assert_eq!(history.ids(0..walked.len()), git, "commit-graph: {with_graph}");
        orders.push(git);
    }
    assert_eq!(orders[0], orders[1], "a commit-graph file does not change git's order");
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
    let walked = ids(&cli, &tips);
    assert_eq!(walked, tips, "every commit is a tip of its own, newest first");
    let art = GraphArt::load(&cli, &tips, 10, &|| false).unwrap().unwrap();
    assert_eq!((0..art.len()).map(|i| art.id(i)).collect::<Vec<_>>(), tips[..10]);
}

#[test]
fn head_and_upstream_scope_and_a_detached_head() {
    let f = branchy();
    let cli = cli(&f);
    f.git(&["switch", "-q", "--detach", "feature~1"]);
    let head = CommitId::from_hex(&f.git(&["rev-parse", "HEAD"])).unwrap();
    let walked = ids(&cli, &[head]);
    assert_eq!(walked.len(), 3, "feature~1, its parent and the root");
    let art = GraphArt::load(&cli, &[head], 10, &|| false).unwrap().unwrap();
    assert_eq!((0..art.len()).map(|i| (art.id(i), art.commit_line(i))).collect::<Vec<_>>(), walked.iter().map(|&id| (id, "*")).collect::<Vec<_>>());
}

#[test]
fn an_empty_repository_has_an_empty_graph() {
    let f = Fixture::new();
    let cli = cli(&f);
    assert!(ids(&cli, &[]).is_empty());
    let art = GraphArt::load(&cli, &[], 10, &|| false).unwrap().unwrap();
    assert!(art.is_empty() && art.complete());
}

#[test]
fn a_cancelled_load_returns_nothing() {
    let f = branchy();
    let (cli, tips) = (cli(&f), all_tips(&f));
    assert!(GraphArt::load(&cli, &tips, 10, &|| true).unwrap().is_none());
    assert!(!topo_walk(&cli, &tips, &|| true, &mut |_| true).unwrap());
}
