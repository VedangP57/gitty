mod common;

use std::sync::Arc;

use common::Fixture;
use gitty_core::Repo;
use gitty_core::diff::ops::{DiffAlgorithm, WsMode, compute_ops};
use gitty_core::diff::text::Text;
use gitty_core::git_cli::GitCli;
use gitty_core::stage::{self, Plan, Texts, build, change_lines, plan, staged_set};
use proptest::prelude::*;

const P: &str = "f.txt";

fn t(s: &[u8]) -> Arc<Text> {
    Arc::new(Text::new(s.to_vec()))
}

fn ops(a: &Text, b: &Text) -> Vec<gitty_core::diff::ops::Op> {
    compute_ops(a, b, DiffAlgorithm::Myers, WsMode::Show)
}

#[test]
fn build_keeps_unstaged_deletions_then_staged_additions() {
    let (h, w) = (t(b"a\nold\nz\n"), t(b"a\nnew\nz\n"));
    let o = ops(&h, &w);
    assert_eq!(change_lines(&o).len(), 2);
    assert_eq!(build(&h, &w, &o, &[false, false]), b"a\nold\nz\n");
    assert_eq!(build(&h, &w, &o, &[true, true]), b"a\nnew\nz\n");
    assert_eq!(build(&h, &w, &o, &[false, true]), b"a\nold\nnew\nz\n", "Desktop order: old, new");
    assert_eq!(build(&h, &w, &o, &[true, false]), b"a\nz\n");
}

#[test]
fn build_never_glues_a_no_eol_line_to_the_next() {
    // the lazygit/Desktop corruption: "a\nb" + "+c" must not become "a\nbc"
    let (h, w) = (t(b"a\nb"), t(b"a\nb\nc"));
    let o = ops(&h, &w);
    let lines = change_lines(&o);
    let staged: Vec<bool> = lines.iter().map(|l| l.new == Some(2)).collect();
    assert_eq!(build(&h, &w, &o, &staged), b"a\nb\nc");
}

#[test]
fn quoting_follows_git() {
    assert_eq!(stage::quote_path("a/plain name.txt"), "a/plain name.txt");
    assert_eq!(stage::quote_path("a/tab\there"), "\"a/tab\\there\"");
    assert_eq!(stage::quote_path("a/q\"b\\c"), "\"a/q\\\"b\\\\c\"");
    assert_eq!(stage::quote_path("a/\u{1}x"), "\"a/\\001x\"");
    assert_eq!(stage::quote_path("a/é.txt"), "a/é.txt");
}

/// Commits `head` (None: path absent), writes `wt` (None: deleted) and returns the repo.
fn setup(f: &Fixture, path: &str, head: Option<&[u8]>, wt: Option<&[u8]>) {
    f.write("keep.txt", "keep\n");
    if let Some(h) = head {
        f.write(path, h);
    }
    f.commit("base", 1_700_000_000);
    let p = f.path().join(path);
    match wt {
        Some(w) => f.write(path, w),
        None => {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn index_bytes(f: &Fixture, path: &str) -> Option<Vec<u8>> {
    let out = std::process::Command::new("git").current_dir(f.path()).args(["show", &format!(":{path}")]).output().unwrap();
    out.status.success().then_some(out.stdout)
}

/// Loads the current texts and HEAD→WT ops for `path` the way the app does.
fn load(repo: &Repo, path: &str) -> Option<(gitty_core::status::StatusEntry, Texts, Vec<gitty_core::diff::ops::Op>)> {
    let cli = GitCli::new(repo);
    let e = cli.status().unwrap().entries.into_iter().find(|e| e.path == path)?;
    let texts = repo.handle().stage_texts(&e).unwrap();
    let o = ops(&texts.head, &texts.wt);
    Some((e, texts, o))
}

/// Stages exactly `want` of the HEAD→WT change lines and checks the index against `build`.
fn stage_and_check(f: &Fixture, repo: &Repo, path: &str, pick: &dyn Fn(usize, usize) -> bool) {
    let Some((e, texts, o)) = load(repo, path) else { return };
    let n = change_lines(&o).len();
    let want: Vec<bool> = (0..n).map(|i| pick(i, n)).collect();
    let target = build(&texts.head, &texts.wt, &o, &want);
    let cli = GitCli::new(repo);
    match plan(&e, &texts, &o, &want) {
        Plan::Nothing => {}
        Plan::StageFile(paths) => cli.stage_paths(&paths).unwrap(),
        Plan::UnstageFile(paths) => cli.unstage_paths(&paths).unwrap(),
        Plan::Patch { patch, expect, target: t } => repo
            .handle()
            .apply_cached(&patch, path, expect, t)
            .unwrap_or_else(|err| panic!("{err:#}\npatch:\n{}", String::from_utf8_lossy(&patch))),
    }
    let got = index_bytes(f, path);
    let nothing_left = target.is_empty() && texts.head.is_empty() && texts.wt.is_empty();
    match got {
        Some(g) => assert_eq!(String::from_utf8_lossy(&g), String::from_utf8_lossy(&target), "index after staging {want:?}"),
        None => assert!(
            (target.is_empty() && (texts.head.is_empty() || texts.wt.is_empty())) || nothing_left,
            "path vanished from the index but target is {:?}",
            String::from_utf8_lossy(&target)
        ),
    }
    // the derived marks reproduce the index
    if let Some((_, texts2, o2)) = load(repo, path) {
        let derived = staged_set(&texts2, &o2).expect("index built by gitty is never divergent");
        assert_eq!(build(&texts2.head, &texts2.wt, &o2, &derived), texts2.index.bytes());
    }
}

fn content() -> impl Strategy<Value = Vec<u8>> {
    let line = prop_oneof![Just("a"), Just("b"), Just("c"), Just(""), Just("x y"), Just("}"), Just("long line here")];
    (prop::collection::vec(line, 0..9), any::<bool>(), any::<bool>()).prop_map(|(ls, crlf, eol)| {
        let nl = if crlf { "\r\n" } else { "\n" };
        let mut s = ls.join(nl);
        if eol && !ls.is_empty() {
            s.push_str(nl);
        }
        s.into_bytes()
    })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn staging_any_selection_produces_exactly_the_built_index(
        head in prop::option::weighted(0.85, content()),
        wt in prop::option::weighted(0.9, content()),
        mask in prop::collection::vec(any::<bool>(), 32),
        mask2 in prop::collection::vec(any::<bool>(), 32),
    ) {
        let f = Fixture::new();
        setup(&f, P, head.as_deref(), wt.as_deref());
        let repo = Repo::open(f.path()).unwrap();
        stage_and_check(&f, &repo, P, &|i, _| mask[i % 32]);
        // a second toggle from a partially staged state
        stage_and_check(&f, &repo, P, &|i, _| mask2[i % 32]);
    }
}

#[test]
fn quoted_and_spaced_paths_stage_by_line() {
    for path in ["sp ace.txt", "tab\tname.txt", "quo\"te.txt", "é.txt"] {
        let f = Fixture::new();
        setup(&f, path, Some(b"a\nb\nc\n"), Some(b"a\nB\nc\nd\n"));
        let repo = Repo::open(f.path()).unwrap();
        stage_and_check(&f, &repo, path, &|i, _| i == 1);
    }
}

#[test]
fn untracked_file_partial_stage_keeps_exec_mode() {
    let f = Fixture::new();
    setup(&f, "run.sh", None, Some(b"#!/bin/sh\necho a\necho b\n"));
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(f.path().join("run.sh"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let repo = Repo::open(f.path()).unwrap();
    stage_and_check(&f, &repo, "run.sh", &|i, _| i < 2);
    let ls = f.git(&["ls-files", "-s", "run.sh"]);
    assert!(ls.starts_with("100755"), "{ls}");
}

#[test]
fn index_changed_since_diff_is_refused() {
    let f = Fixture::new();
    setup(&f, P, Some(b"a\nb\n"), Some(b"a\nB\nc\n"));
    let repo = Repo::open(f.path()).unwrap();
    let (e, texts, o) = load(&repo, P).unwrap();
    let want = vec![false, false, true];
    let Plan::Patch { patch, expect, target } = plan(&e, &texts, &o, &want) else { panic!("expected a patch") };
    // someone stages the file from another terminal meanwhile
    f.git(&["add", P]);
    let before = index_bytes(&f, P);
    let err = repo.handle().apply_cached(&patch, P, expect, target).unwrap_err();
    assert!(format!("{err:#}").contains("changed"), "{err:#}");
    assert_eq!(index_bytes(&f, P), before, "nothing applied");
}

#[test]
fn an_index_that_is_not_the_target_after_apply_is_reported() {
    let f = Fixture::new();
    setup(&f, P, Some(b"a\nb\n"), Some(b"a\nB\nc\n"));
    let repo = Repo::open(f.path()).unwrap();
    let (e, texts, o) = load(&repo, P).unwrap();
    let Plan::Patch { patch, expect, target } = plan(&e, &texts, &o, &[false, false, true]) else { panic!("expected a patch") };
    assert_eq!(target, gitty_core::commit_files::BlobId::hash_of(b"a\nb\nc\n"));
    // git applying the patch differently (a config or a git bug) leaves some other blob
    let wrong = gitty_core::commit_files::BlobId::hash_of(b"what the user picked\n");
    let err = format!("{:#}", repo.handle().apply_cached(&patch, P, expect, wrong).unwrap_err());
    assert!(err.contains("not what was selected"), "{err}");
    assert!(err.contains(&wrong.to_string()[..7]) && err.contains(&target.to_string()[..7]), "both blobs named: {err}");
}

#[test]
fn index_matching_neither_side_is_divergent() {
    let f = Fixture::new();
    setup(&f, P, Some(b"a\nb\n"), Some(b"a\nB\n"));
    f.write(P, "a\nSTAGED\n");
    f.git(&["add", P]);
    f.write(P, "a\nB\n");
    let repo = Repo::open(f.path()).unwrap();
    let (_, texts, o) = load(&repo, P).unwrap();
    assert_eq!(staged_set(&texts, &o), None);
}

#[test]
fn crlf_autocrlf_worktree_is_compared_in_git_form() {
    let f = Fixture::new();
    f.git(&["config", "core.autocrlf", "true"]);
    setup(&f, P, Some(b"a\nb\n"), Some(b"a\r\nb\r\nc\r\n"));
    let repo = Repo::open(f.path()).unwrap();
    let (_, texts, o) = load(&repo, P).unwrap();
    assert_eq!(texts.wt.bytes(), b"a\nb\nc\n");
    assert!(!texts.wt_is_raw, "conversion happened, so line discard must be refused");
    assert_eq!(change_lines(&o).len(), 1, "only the added line differs");
}

#[test]
fn worktree_blob_id_matches_git_hash_object() {
    let f = Fixture::new();
    setup(&f, P, Some(b"a\n"), Some(b"a\nb\n"));
    let repo = Repo::open(f.path()).unwrap();
    let (_, texts, _) = load(&repo, P).unwrap();
    let want = f.git(&["hash-object", P]);
    let got: String = texts.wt_blob.0.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, want);
}
