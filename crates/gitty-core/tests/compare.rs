mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::commit_files::FileStatus;
use gitty_core::compare::compare;
use gitty_core::git_cli::GitCli;
use gitty_core::types::CommitId;

fn id(s: &str) -> CommitId {
    CommitId::from_hex(s.trim()).unwrap()
}

fn rev_list(f: &Fixture, args: &[&str]) -> Vec<CommitId> {
    let mut a = vec!["rev-list", "--topo-order"];
    a.extend(args);
    f.git(&a).lines().map(id).collect()
}

/// main: base, m1, m2 · feature (from base): f1, f2, f3, plus a merge of main's m1.
fn diverged() -> Fixture {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["branch", "feature"]);
    f.write("m.txt", "1\n");
    f.commit("m1", 1_700_000_100);
    f.write("m.txt", "2\n");
    f.commit("m2", 1_700_000_200);
    f.git(&["checkout", "-q", "feature"]);
    for i in 1..=3 {
        f.write("f.txt", format!("{i}\n"));
        f.commit(&format!("f{i}"), 1_700_000_300 + i * 100);
    }
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge m1", "main~1"], &[("GIT_AUTHOR_DATE", "1700001000 +0000".into()), ("GIT_COMMITTER_DATE", "1700001000 +0000".into())]);
    f.git(&["checkout", "-q", "main"]);
    f
}

#[test]
fn compare_lists_both_sides_in_topo_order_with_the_merge_base() {
    let f = diverged();
    let repo = Repo::open(f.path()).unwrap();
    let cli = GitCli::new(&repo);
    let (head, other) = (id(&f.git(&["rev-parse", "main"])), id(&f.git(&["rev-parse", "feature"])));
    let c = compare(&cli, head, other).unwrap();
    assert_eq!(c.behind, rev_list(&f, &["feature", "^main"]), "in feature, not in main");
    assert_eq!(c.ahead, rev_list(&f, &["main", "^feature"]), "in main, not in feature");
    assert_eq!(c.behind.len(), 4);
    assert_eq!(c.ahead.len(), 1);
    assert_eq!(c.merge_base, Some(id(&f.git(&["merge-base", "main", "feature"]))));
}

#[test]
fn compare_with_unrelated_history_has_no_merge_base() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("a", 1_700_000_000);
    f.git(&["checkout", "-q", "--orphan", "other"]);
    f.write("b.txt", "b\n");
    f.commit("b", 1_700_000_100);
    let repo = Repo::open(f.path()).unwrap();
    let cli = GitCli::new(&repo);
    let c = compare(&cli, id(&f.git(&["rev-parse", "main"])), id(&f.git(&["rev-parse", "other"]))).unwrap();
    assert_eq!((c.ahead.len(), c.behind.len(), c.merge_base), (1, 1, None));
}

#[test]
fn diff_commits_matches_git_diff_including_renames_and_the_empty_tree() {
    let f = Fixture::new();
    f.write("keep.txt", "k\n");
    f.write("old.txt", "a long enough line to be detected as a rename\nsecond line\n");
    f.write("gone.txt", "g\n");
    let a = f.commit("a", 1_700_000_000);
    f.git(&["mv", "old.txt", "new.txt"]);
    f.git(&["rm", "-q", "gone.txt"]);
    f.write("keep.txt", "k2\n");
    f.write("added.txt", "n\n");
    let b = f.commit("b", 1_700_000_100);
    let repo = Repo::open(f.path()).unwrap();
    let h = repo.handle();
    let name = |s: &FileStatus| match s {
        FileStatus::Renamed { .. } => "Renamed".to_string(),
        s => format!("{s:?}"),
    };
    let mut got: Vec<String> = h.diff_commits(Some(id(&a)), id(&b), true).unwrap().iter().map(|c| format!("{} {}", name(&c.status), c.path)).collect();
    got.sort();
    let mut want: Vec<String> = f.git(&["diff", "--name-status", "-M", &a, &b]).lines().map(|l| {
        let mut p = l.split('\t');
        let st = p.next().unwrap();
        let path = p.next_back().unwrap();
        let st = match &st[..1] { "A" => "Added", "D" => "Deleted", "M" => "Modified", "R" => "Renamed", s => s };
        format!("{st} {path}")
    }).collect();
    want.sort();
    assert_eq!(got, want);
    let root: Vec<String> = h.diff_commits(None, id(&a), true).unwrap().iter().map(|c| c.path.clone()).collect();
    assert_eq!(root.len(), 3, "None diffs against the empty tree: {root:?}");
}
