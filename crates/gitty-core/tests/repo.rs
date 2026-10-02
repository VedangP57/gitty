mod common;
use common::{path_of, Fixture};
use gitty_core::Repo;

#[test]
fn opens_repo_from_subdir() {
    let f = Fixture::new();
    f.write("a/b.txt", "x\n");
    f.commit("init", 1_700_000_000);
    let r = Repo::open(f.path().join("a")).unwrap();
    assert_eq!(path_of(r.workdir().unwrap()), path_of(&f.path()));
    assert!(r.git_dir().ends_with(".git"));
    let _h = r.handle();
}

#[test]
fn rejects_non_repo() {
    let d = tempfile::tempdir().unwrap();
    assert!(Repo::open(d.path()).is_err());
}
