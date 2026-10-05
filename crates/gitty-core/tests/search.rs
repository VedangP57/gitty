mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::git_cli::GitCli;
use gitty_core::search::{Query, path_commits};
use gitty_core::types::CommitId;

#[test]
fn parse_splits_text_and_path() {
    let q = Query::parse("fix crash path:src/ui").unwrap();
    assert_eq!((q.text.as_deref(), q.path.as_deref()), (Some("fix crash"), Some("src/ui")));
    let q = Query::parse("path:\"dir with space/a b.txt\"").unwrap();
    assert_eq!((q.text, q.path.as_deref()), (None, Some("dir with space/a b.txt")));
    assert!(Query::parse("   ").is_none());
    assert!(Query::parse("path:").is_none());
}

fn row_of(f: &Fixture, id: &str) -> gitty_core::history::CommitRow {
    Repo::open(f.path()).unwrap().handle().decode_row(CommitId::from_hex(id).unwrap()).unwrap()
}

#[test]
fn smart_case_over_summary_and_author() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    let id = f.commit("Ärger mit dem Parser", 1_700_000_000);
    let row = row_of(&f, &id);
    let m = |s: &str| Query::parse(s).unwrap().matches(&row);
    assert!(m("ärger"), "lowercase folds unicode");
    assert!(m("parser"));
    assert!(m("Parser"));
    assert!(!m("PARSER"), "an uppercase letter makes the query case-sensitive");
    assert!(m("test user"), "author name");
    assert!(m("example.com"), "author email");
    assert!(!m("nothing like it"));
}

#[test]
fn path_filter_is_literal_and_covers_directories() {
    let f = Fixture::new();
    f.write("src/ui/a.rs", "a\n");
    let a = f.commit("ui", 1_700_000_000);
    f.write("dir with space/a b.txt", "x\n");
    let b = f.commit("spaced", 1_700_000_100);
    f.write("star*.txt", "s\n");
    let c = f.commit("star", 1_700_000_200);
    f.write("starry.txt", "s\n");
    let d = f.commit("starry", 1_700_000_300);
    let repo = Repo::open(f.path()).unwrap();
    let cli = GitCli::new(&repo);
    let tips = [CommitId::from_hex(&d).unwrap()];
    let ids = |p: &str| {
        let mut v: Vec<String> = path_commits(&cli, &tips, p).unwrap().into_iter().map(|i| i.to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(ids("src"), [a]);
    assert_eq!(ids("dir with space/a b.txt"), [b]);
    assert_eq!(ids("star*.txt"), [c], "no glob: starry.txt is not matched");
    assert!(ids("missing/path").is_empty());
}
