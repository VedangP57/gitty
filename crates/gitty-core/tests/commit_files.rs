mod common;
use common::Fixture;
use gitty_core::commit_files::FileStatus;
use gitty_core::{CommitId, Repo};

fn id(s: &str) -> CommitId {
    CommitId::from_hex(s).unwrap()
}

#[test]
fn root_commit_lists_added() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n");
    f.write("dir/b.txt", "x\n");
    let c = id(&f.commit("root", 1_700_000_000));
    let h = Repo::open(f.path()).unwrap().handle();
    let files = h.commit_files(c, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| (x.path.as_str(), x.status.clone())).collect();
    assert_eq!(paths, vec![("a.txt", FileStatus::Added), ("dir/b.txt", FileStatus::Added)]);
    let s = h.line_stats(&files[0]).unwrap();
    assert_eq!((s.added, s.removed, s.binary), (2, 0, false));
}

#[test]
fn modify_delete_rename_binary() {
    let f = Fixture::new();
    f.write("keep.txt", "a\nb\nc\n");
    f.write("gone.txt", "bye\n");
    f.write("old_name.txt", "line1\nline2\nline3\nline4\nline5\n");
    f.write("img.bin", [0u8, 1, 2, 3, 0, 5]);
    f.commit("base", 1_700_000_000);
    f.write("keep.txt", "a\nB\nc\nd\n");
    std::fs::remove_file(f.path().join("gone.txt")).unwrap();
    f.git(&["mv", "old_name.txt", "new_name.txt"]);
    f.write("img.bin", [0u8, 9, 9, 9, 0, 5]);
    let c = id(&f.commit("change", 1_700_000_100));
    let h = Repo::open(f.path()).unwrap().handle();

    let files = h.commit_files(c, true).unwrap();
    let by = |p: &str| files.iter().find(|x| x.path == p).unwrap_or_else(|| panic!("missing {p}: {files:?}"));
    assert_eq!(by("keep.txt").status, FileStatus::Modified);
    assert_eq!(by("gone.txt").status, FileStatus::Deleted);
    assert!(matches!(by("new_name.txt").status, FileStatus::Renamed { .. }), "{:?}", by("new_name.txt"));
    assert_eq!(by("new_name.txt").old_path.as_deref(), Some("old_name.txt"));
    assert_eq!(files.len(), 4);
    let ks = h.line_stats(by("keep.txt")).unwrap();
    assert_eq!((ks.added, ks.removed, ks.binary), (2, 1, false));
    assert!(h.line_stats(by("img.bin")).unwrap().binary);
    assert_eq!(h.line_stats(by("gone.txt")).unwrap().removed, 1);

    let files = h.commit_files(c, false).unwrap();
    assert!(files.iter().any(|x| x.path == "old_name.txt" && x.status == FileStatus::Deleted));
    assert!(files.iter().any(|x| x.path == "new_name.txt" && x.status == FileStatus::Added));
}

#[test]
fn merge_diffs_against_first_parent() {
    let f = Fixture::new();
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("side.txt", "s\n");
    f.commit("side", 1_700_000_100);
    f.git(&["checkout", "-q", "main"]);
    f.write("main.txt", "m\n");
    f.commit("main", 1_700_000_200);
    f.git(&["merge", "-q", "--no-ff", "-m", "merge", "side"]);
    let m = id(&f.git(&["rev-parse", "HEAD"]));
    let files = Repo::open(f.path()).unwrap().handle().commit_files(m, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(paths, vec!["side.txt"]);
}

#[test]
fn range_files_spans_commits() {
    let f = Fixture::new();
    f.write("a.txt", "1\n");
    f.commit("one", 1_700_000_000);
    f.write("b.txt", "2\n");
    let c2 = id(&f.commit("two", 1_700_000_100));
    f.write("c.txt", "3\n");
    let c3 = id(&f.commit("three", 1_700_000_200));
    let files = Repo::open(f.path()).unwrap().handle().range_files(c2, c3, false).unwrap();
    let paths: Vec<_> = files.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(paths, vec!["b.txt", "c.txt"]);
}

#[test]
fn nested_paths_and_mode_change() {
    let f = Fixture::new();
    f.write("src/deep/x.sh", "echo\n");
    f.commit("one", 1_700_000_000);
    f.git(&["update-index", "--chmod=+x", "src/deep/x.sh"]);
    let date = "1700000100 +0000".to_string();
    f.git_env(&["commit", "-q", "-m", "chmod"], &[("GIT_AUTHOR_DATE", date.clone()), ("GIT_COMMITTER_DATE", date)]);
    let c = id(&f.git(&["rev-parse", "HEAD"]));
    let files = Repo::open(f.path()).unwrap().handle().commit_files(c, false).unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "src/deep/x.sh");
    assert_eq!((files[0].old_mode, files[0].new_mode), (0o100644, 0o100755));
}
