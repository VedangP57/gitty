mod common;
use common::Fixture;
use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn build_history(f: &Fixture) {
    // main: a(100) - b(200) - d(400) - m(merge of feature, 500); feature from b: c(300) - e(450)
    f.commit("a", 1_700_000_100);
    f.commit("b", 1_700_000_200);
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("f.txt", "feature\n");
    f.commit("c", 1_700_000_300);
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "main\n");
    f.commit("d", 1_700_000_400);
    f.git(&["checkout", "-q", "feature"]);
    f.write("f.txt", "feature2\n");
    f.commit("e", 1_700_000_450);
    f.git(&["checkout", "-q", "main"]);
    let date = "1700000500 +0000".to_string();
    f.git_env(
        &["merge", "-q", "--no-ff", "-m", "m", "feature"],
        &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)],
    );
}

fn walk_all(repo: &Repo, batch: usize) -> Vec<CommitId> {
    let h = repo.handle();
    let refs = h.refs().unwrap();
    let mut w = h.walker(&refs.tips(HistoryScope::AllRefs)).unwrap();
    let mut hist = w.new_history();
    while w.step(&h, &mut hist, batch).unwrap() {}
    hist.ids(0..hist.len())
}

fn git_order(f: &Fixture) -> Vec<CommitId> {
    f.git(&["log", "--all", "--date-order", "--format=%H"]).lines().map(|l| CommitId::from_hex(l).unwrap()).collect()
}

#[test]
fn walk_without_graph() {
    let f = Fixture::new();
    build_history(&f);
    let repo = Repo::open(f.path()).unwrap();
    assert!(!repo.handle().walker(&[]).unwrap().uses_graph());
    assert_eq!(walk_all(&repo, 2), git_order(&f));
}

#[test]
fn walk_with_graph() {
    let f = Fixture::new();
    build_history(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    let repo = Repo::open(f.path()).unwrap();
    assert!(repo.handle().walker(&[]).unwrap().uses_graph());
    assert_eq!(walk_all(&repo, 3), git_order(&f));
}

#[test]
fn walk_with_stale_graph() {
    let f = Fixture::new();
    build_history(&f);
    f.git(&["commit-graph", "write", "--reachable"]);
    f.commit("after graph 1", 1_700_000_600);
    f.commit("after graph 2", 1_700_000_700);
    let repo = Repo::open(f.path()).unwrap();
    let got = walk_all(&repo, 1);
    assert_eq!(got.len(), 8);
    assert_eq!(got, git_order(&f));
}

#[test]
fn walk_empty_tips() {
    let f = Fixture::new();
    let repo = Repo::open(f.path()).unwrap();
    let h = repo.handle();
    let mut w = h.walker(&[]).unwrap();
    let mut hist = w.new_history();
    assert!(!w.step(&h, &mut hist, 100).unwrap());
    assert!(hist.is_empty());
}

#[test]
fn decode_row_fields() {
    let f = Fixture::new();
    let msg = "feat: thing\n\nBody line\n\nCo-authored-by: Ana B <ana@x.io>\nCo-Authored-By: Raj K <raj@y.io>";
    let id = CommitId::from_hex(&f.commit(msg, 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let row = h.decode_row(id).unwrap();
    assert_eq!(row.id, id);
    assert_eq!(row.summary, "feat: thing");
    assert_eq!(row.author.name, "Test User");
    assert_eq!(row.author.email, "test@example.com");
    assert_eq!(row.author.time, 1_700_000_100);
    assert_eq!(row.committer_time, 1_700_000_100);
    assert!(row.parents.is_empty());
    assert_eq!(row.co_authors, vec![("Ana B".into(), "ana@x.io".into()), ("Raj K".into(), "raj@y.io".into())]);
    let d = h.commit_detail(id).unwrap();
    assert!(d.body.starts_with("Body line"), "{:?}", d.body);
}
