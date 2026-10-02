mod common;
use common::Fixture;
use gitty_core::refs::{Head, HistoryScope, RefKind};
use gitty_core::{CommitId, Repo};

fn id(s: &str) -> CommitId {
    CommitId::from_hex(s).unwrap()
}

#[test]
fn unborn_branch() {
    let f = Fixture::new();
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    assert!(matches!(refs.head, Head::Branch { ref name, id: None } if name == "main"));
    assert!(refs.tips(HistoryScope::HeadAndUpstream).is_empty());
    assert!(refs.tips(HistoryScope::AllRefs).is_empty());
}

#[test]
fn labels_upstream_and_tags() {
    let f = Fixture::new();
    let c1 = id(&f.commit("one", 1_700_000_000));
    f.add_bare_upstream();
    f.git(&["tag", "v1.0"]);
    f.git(&["tag", "-a", "v1.1", "-m", "annotated"]);
    let c2 = id(&f.commit("two", 1_700_000_100));
    f.git(&["branch", "feature"]);
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    assert_eq!(refs.head_id(), Some(c2));
    assert_eq!(refs.head_branch(), Some("main"));
    assert_eq!(refs.upstream.as_ref().map(|u| (u.0.as_str(), u.1)), Some(("origin/main", c1)));
    let l2: Vec<_> = refs.labels[&c2].iter().map(|l| (l.kind, l.name.as_str(), l.is_head)).collect();
    assert_eq!(l2, vec![(RefKind::LocalBranch, "main", true), (RefKind::LocalBranch, "feature", false)]);
    let l1: Vec<_> = refs.labels[&c1].iter().map(|l| (l.kind, l.name.as_str())).collect();
    assert_eq!(l1, vec![(RefKind::RemoteBranch, "origin/main"), (RefKind::Tag, "v1.0"), (RefKind::Tag, "v1.1")]);
    let mut tips = refs.tips(HistoryScope::HeadAndUpstream);
    tips.sort();
    let mut exp = vec![c1, c2];
    exp.sort();
    assert_eq!(tips, exp);
}

#[test]
fn detached_head_and_no_upstream() {
    let f = Fixture::new();
    let c1 = f.commit("one", 1_700_000_000);
    f.commit("two", 1_700_000_100);
    f.git(&["checkout", "-q", "--detach", &c1]);
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    assert!(matches!(refs.head, Head::Detached { id: d } if d == id(&c1)));
    assert!(refs.upstream.is_none());
    assert_eq!(refs.tips(HistoryScope::HeadAndUpstream), vec![id(&c1)]);
    assert_eq!(refs.tips(HistoryScope::AllRefs).len(), 2);
}

#[test]
fn tag_pointing_at_tree_is_ignored() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("one", 1_700_000_000);
    let tree = f.git(&["rev-parse", "HEAD^{tree}"]);
    f.git(&["tag", "treetag", &tree]);
    let refs = Repo::open(f.path()).unwrap().handle().refs().unwrap();
    assert_eq!(refs.tips(HistoryScope::AllRefs).len(), 1);
    assert!(refs.labels.values().flatten().all(|l| l.name != "treetag"));
}
