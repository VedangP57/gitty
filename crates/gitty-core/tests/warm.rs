mod common;
use common::Fixture;
use gitty_core::Repo;

#[test]
fn warm_on_unborn_and_normal_repo() {
    let f = Fixture::new();
    Repo::open(f.path()).unwrap().handle().warm();
    f.write("a.txt", "a\n");
    f.commit("one", 1_700_000_000);
    f.write("a.txt", "b\n");
    f.commit("two", 1_700_000_100);
    Repo::open(f.path()).unwrap().handle().warm();
}
