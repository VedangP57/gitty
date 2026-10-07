mod common;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::branch::Unmerged;
use gitty_core::git_cli::GitCli;

/// Alone in its binary: it sets the process environment, which every git child inherits. Where the
/// locale or git's translations are missing git answers in English, and the ancestry check holds.
#[test]
fn an_unmerged_delete_is_still_recognised_under_a_german_locale() {
    // SAFETY: the only test in this binary, so no other thread reads the environment meanwhile.
    unsafe {
        std::env::set_var("LC_ALL", "de_DE.UTF-8");
        std::env::set_var("LANGUAGE", "de");
    }
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "wip"]);
    f.write("w.txt", "w\n");
    f.commit("wip work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    let c = GitCli::new(&Repo::open(f.path()).unwrap());
    let e = c.delete_branch("wip", false).unwrap_err();
    assert!(e.downcast_ref::<Unmerged>().is_some(), "{e:#}");
    c.delete_branch("wip", true).unwrap();
}
