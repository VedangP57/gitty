#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use common::Fixture;
use gitty::msg::{FilesOf, Gens, Msg, Request};
use gitty::{exec::exec, workers::Workers};
use gitty_core::diff::DiffOptions;
use gitty_core::refs::HistoryScope;
use gitty_core::{CommitId, Repo};

fn run_with(f: &std::path::Path, gens: &Gens, req: Request) -> Vec<Msg> {
    let h = Repo::open(f).unwrap().handle();
    let mut out = Vec::new();
    exec(&h, req, &mut |m| out.push(m), gens);
    out
}
fn run(f: &Fixture, req: Request) -> Vec<Msg> {
    run_with(&f.path(), &Gens::default(), req)
}
fn id(hex: &str) -> CommitId {
    CommitId::from_hex(hex).unwrap()
}
fn five(f: &Fixture) -> Vec<String> {
    (0..5)
        .map(|i| {
            f.write("a.txt", format!("line {i}\n"));
            f.commit(&format!("commit {i}"), 1_700_000_000 + i * 100)
        })
        .collect()
}
fn tips(f: &Fixture) -> Vec<CommitId> {
    match &run(f, Request::Refs)[0] {
        Msg::Refs { refs, .. } => refs.tips(HistoryScope::HeadAndUpstream),
        m => panic!("{m:?}"),
    }
}
fn errors(v: &[Msg]) -> Vec<String> {
    v.iter().filter_map(|m| if let Msg::Error { what, detail } = m { Some(format!("{what}: {detail}")) } else { None }).collect()
}

#[test]
fn refs_and_walk_streams_whole_history() {
    let f = Fixture::new();
    let ids = five(&f);
    let msgs = run(&f, Request::Walk { session: 0, tips: tips(&f) });
    assert!(errors(&msgs).is_empty(), "{:?}", errors(&msgs));
    let Msg::HistoryStarted { session: 0, history } = &msgs[0] else { panic!("{:?}", msgs[0]) };
    let Some(Msg::HistoryProgress { session: 0, len: 5, done: true }) = msgs.last() else { panic!("{:?}", msgs.last()) };
    let h = history.read().unwrap();
    assert_eq!(h.id(0), id(&ids[4]));
    assert_eq!(h.id(4), id(&ids[0]));
}

fn many_commits(f: &Fixture, n: usize) {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("commit refs/heads/main\nmark :{}\ncommitter T <t@t> {} +0000\ndata 3\nmsg\n", i + 1, 1_700_000_000 + i));
        if i > 0 {
            s.push_str(&format!("from :{i}\n"));
        }
        s.push('\n');
    }
    let mut c = std::process::Command::new("git");
    c.current_dir(f.path()).args(["fast-import", "--quiet"]).stdin(std::process::Stdio::piped());
    let mut child = c.spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn walk_stops_when_session_bumped() {
    let f = Fixture::new();
    many_commits(&f, 10_000);
    let gens = Gens::default();
    let h = Repo::open(f.path()).unwrap().handle();
    let mut last = None;
    let tips = tips(&f);
    exec(&h, Request::Walk { session: 0, tips }, &mut |m| {
        if let Msg::HistoryProgress { len, done, .. } = m {
            gens.session.store(1, SeqCst);
            last = Some((len, done));
        }
    }, &gens);
    let (len, done) = last.unwrap();
    assert!(done);
    assert!(len < 10_000, "walked {len}");
}

#[test]
fn rows_decode_in_order() {
    let f = Fixture::new();
    let ids = five(&f);
    let req = Request::Rows { session: 3, ids: vec![(0, id(&ids[4])), (1, id(&ids[3]))] };
    let msgs = run(&f, req);
    let Msg::Rows { session: 3, rows } = &msgs[0] else { panic!() };
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].0, rows[0].1.summary.as_str()), (0, "commit 4"));
    assert_eq!((rows[1].0, rows[1].1.summary.as_str()), (1, "commit 3"));
}

#[test]
fn detail_has_body() {
    let f = Fixture::new();
    f.write("a", "a");
    let c = f.commit("subject\n\nbody line\n\nCo-authored-by: X <x@x>", 1_700_000_000);
    let msgs = run(&f, Request::Detail { generation: 0, id: id(&c) });
    let Msg::Detail { generation: 0, detail } = &msgs[0] else { panic!("{msgs:?}") };
    assert!(detail.body.contains("body line"));
    assert_eq!(detail.row.co_authors.len(), 1);
}

#[test]
fn files_then_stats() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n3\n");
    f.write("b.txt", "x\n");
    f.commit("one", 1_700_000_000);
    f.write("a.txt", "1\nTWO\n3\n4\n");
    f.write("c.bin", b"\x00\x01\x02");
    let c = f.commit("two", 1_700_000_100);
    let msgs = run(&f, Request::Files { generation: 0, of: FilesOf::Commit(id(&c)), prefetch: false });
    let Msg::Files { files, .. } = &msgs[0] else { panic!("{msgs:?}") };
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "c.bin"]);
    let stats: Vec<_> = msgs[1..]
        .iter()
        .flat_map(|m| if let Msg::Stats { start, stats, .. } = m { stats.iter().enumerate().map(|(i, s)| (start + i, s.unwrap())).collect() } else { vec![] })
        .collect();
    assert_eq!(stats.len(), 2);
    assert_eq!((stats[0].1.added, stats[0].1.removed), (2, 1));
    assert!(stats[1].1.binary);
    assert!(matches!(msgs.last(), Some(Msg::Stats { done: true, .. })));
}

#[test]
fn stale_files_dropped_after_bump() {
    let f = Fixture::new();
    let ids = five(&f);
    let gens = Gens::default();
    gens.commit.store(9, SeqCst);
    let msgs = run_with(&f.path(), &gens, Request::Files { generation: 1, of: FilesOf::Commit(id(&ids[1])), prefetch: false });
    assert!(msgs.is_empty(), "{msgs:?}");
    let msgs = run_with(&f.path(), &gens, Request::Detail { generation: 1, id: id(&ids[1]) });
    assert!(msgs.is_empty(), "{msgs:?}");
}

#[test]
fn a_range_count_for_an_old_selection_answers_nothing() {
    let f = Fixture::new();
    let ids = five(&f);
    let gens = Gens::default();
    gens.commit.store(9, SeqCst);
    let (oldest, newest) = (id(&ids[1]), id(&ids[3]));
    // rows 1..4 of the walk are ids[3], ids[2], ids[1]: the whole range, nothing extra
    let history = run(&f, Request::Walk { session: 0, tips: tips(&f) }).into_iter().find_map(|m| match m {
        Msg::HistoryStarted { history, .. } => Some(history),
        _ => None,
    }).unwrap();
    let msgs = run_with(&f.path(), &gens, Request::RangeCount { generation: 1, oldest, newest, history: history.clone(), rows: 1..4 });
    assert!(msgs.is_empty(), "{msgs:?}");
    let msgs = run_with(&f.path(), &gens, Request::RangeCount { generation: 9, oldest, newest, history, rows: 1..4 });
    assert!(matches!(msgs[..], [Msg::RangeCount { extra: 0, .. }]), "{msgs:?}");
}

#[test]
fn prefetch_runs_even_if_stale() {
    let f = Fixture::new();
    let ids = five(&f);
    let gens = Gens::default();
    gens.commit.store(9, SeqCst);
    let msgs = run_with(&f.path(), &gens, Request::Files { generation: 1, of: FilesOf::Commit(id(&ids[1])), prefetch: true });
    assert!(matches!(&msgs[0], Msg::Files { prefetch: true, .. }), "{msgs:?}");
}

fn files_of(f: &Fixture, c: &str) -> Arc<Vec<gitty_core::commit_files::FileChange>> {
    match &run(f, Request::Files { generation: 0, of: FilesOf::Commit(id(c)), prefetch: false })[0] {
        Msg::Files { files, .. } => files.clone(),
        m => panic!("{m:?}"),
    }
}

#[test]
fn diff_then_intraline_done() {
    let f = Fixture::new();
    f.write("a.txt", "fn a() {\n    one();\n}\nmid\nmid\nmid\nmid\nmid\nmid\nmid\nfn b() {\n    two();\n}\n");
    f.commit("one", 1_700_000_000);
    f.write("a.txt", "fn a() {\n    uno();\n}\nmid\nmid\nmid\nmid\nmid\nmid\nmid\nfn b() {\n    dos();\n}\n");
    let c = f.commit("two", 1_700_000_100);
    let file = files_of(&f, &c)[0].clone();
    let msgs = run(&f, Request::Diff { generation: 0, file, opts: DiffOptions::default(), force_text: false });
    let Msg::Diff { diff, key, .. } = &msgs[0] else { panic!("{msgs:?}") };
    assert_eq!(diff.changes.len(), 2);
    assert!(matches!(&msgs[1], Msg::IntralineDone { key: k } if k == key));
    assert!((0..2).all(|c| diff.intraline_ready(c).is_some()));
}

#[test]
fn intraline_request_finishes_partial_diff() {
    let d = gitty_core::diff::FileDiff::from_bytes("a", None, b"a\nb\n".to_vec(), b"a\nc\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    let d = Arc::new(d);
    let f = Fixture::new();
    let key = gitty::msg::DiffKey { old: None, new: None, path: "a".into(), old_path: None, old_mode: 0, new_mode: 0, opts: DiffOptions::default(), force_text: false };
    let msgs = run(&f, Request::Intraline { generation: 0, key: key.clone(), diff: d.clone() });
    assert!(matches!(&msgs[0], Msg::IntralineDone { key: k } if *k == key));
    assert!(d.intraline_ready(0).is_some());
}

#[test]
fn binary_and_identical_diffs_do_not_panic() {
    let f = Fixture::new();
    f.write("a.txt", "same\n");
    f.write("b.bin", b"\x00\x01");
    f.commit("one", 1_700_000_000);
    f.git(&["mv", "a.txt", "moved.txt"]);
    f.write("b.bin", b"\x00\x02");
    let c = f.commit("two", 1_700_000_100);
    for file in files_of(&f, &c).iter() {
        let msgs = run(&f, Request::Diff { generation: 0, file: file.clone(), opts: DiffOptions::default(), force_text: true });
        assert!(errors(&msgs).is_empty(), "{:?}", errors(&msgs));
        assert!(matches!(&msgs[0], Msg::Diff { .. }));
    }
}

#[test]
fn unborn_repo_refs_and_walk() {
    let f = Fixture::new();
    let msgs = run(&f, Request::Refs);
    let Msg::Refs { refs, .. } = &msgs[0] else { panic!("{msgs:?}") };
    let tips = refs.tips(HistoryScope::HeadAndUpstream);
    assert!(tips.is_empty());
    let msgs = run(&f, Request::Walk { session: 0, tips });
    assert!(matches!(msgs.last(), Some(Msg::HistoryProgress { len: 0, done: true, .. })), "{msgs:?}");
}

#[test]
fn empty_commit_has_zero_files() {
    let f = Fixture::new();
    let c = f.commit("empty", 1_700_000_000);
    let msgs = run(&f, Request::Files { generation: 0, of: FilesOf::Commit(id(&c)), prefetch: false });
    let Msg::Files { files, .. } = &msgs[0] else { panic!("{msgs:?}") };
    assert!(files.is_empty());
    assert!(matches!(msgs.last(), Some(Msg::Stats { done: true, .. })));
}

#[test]
fn bad_id_reports_error_not_panic() {
    let f = Fixture::new();
    five(&f);
    let bogus = CommitId([7; 20]);
    for req in [
        Request::Files { generation: 0, of: FilesOf::Commit(bogus), prefetch: false },
        Request::Detail { generation: 0, id: bogus },
        Request::Rows { session: 0, ids: vec![(0, bogus)] },
    ] {
        let msgs = run(&f, req);
        assert!(msgs.iter().all(|m| !matches!(m, Msg::Files { .. } | Msg::Detail { .. })), "{msgs:?}");
    }
    let msgs = run(&f, Request::Files { generation: 0, of: FilesOf::Commit(bogus), prefetch: true });
    assert!(matches!(&msgs[..], [Msg::FilesError { prefetch: true, .. }]), "{msgs:?}");
    {
    }
}

#[test]
fn ahead_behind_message() {
    let f = Fixture::new();
    let ids = five(&f);
    let msgs = run(&f, Request::AheadBehind { local: id(&ids[4]), upstream: Some(id(&ids[2])) });
    let Msg::AheadBehind { ab, .. } = &msgs[0] else { panic!("{msgs:?}") };
    assert_eq!(ab.ahead.len(), 2);
    assert!(ab.behind.is_empty());
}

#[test]
fn workers_spawn_and_answer() {
    let f = Fixture::new();
    five(&f);
    let (tx, rx) = crossbeam_channel::unbounded();
    let w = Workers::spawn(Repo::open(f.path()).unwrap(), Arc::new(Gens::default()), tx);
    w.submit(Request::Refs);
    let m = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let Msg::Refs { refs, .. } = m else { panic!("{m:?}") };
    w.submit(Request::Walk { session: 0, tips: refs.tips(HistoryScope::HeadAndUpstream) });
    let mut done = false;
    while let Ok(m) = rx.recv_timeout(Duration::from_secs(10)) {
        if let Msg::HistoryProgress { len: 5, done: true, .. } = m {
            done = true;
            break;
        }
    }
    assert!(done);
}

/// Partial clones lack blobs: stats are unknown (None) and the diff reports an error.
#[test]
fn missing_blob_reports_unknown_stats_and_diff_error() {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.commit("one", 1_700_000_000);
    f.write("a.txt", "one\ntwo\n");
    let c = f.commit("two", 1_700_000_100);
    let blob = f.git(&["rev-parse", "HEAD:a.txt"]);
    std::fs::remove_file(f.path().join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    let msgs = run(&f, Request::Files { generation: 0, of: FilesOf::Commit(id(&c)), prefetch: false });
    let Msg::Files { files, .. } = &msgs[0] else { panic!("{msgs:?}") };
    let Some(Msg::Stats { stats, .. }) = msgs.last() else { panic!("{msgs:?}") };
    assert_eq!(stats[0], None);
    let msgs = run(&f, Request::Diff { generation: 0, file: files[0].clone(), opts: DiffOptions::default(), force_text: false });
    assert!(matches!(&msgs[..], [Msg::DiffError { .. }]), "{msgs:?}");
}

/// The UI thread reads the shared history while the walker runs; it must never wait long.
#[test]
fn walk_never_starves_readers() {
    let f = Fixture::new();
    many_commits(&f, 30_000);
    let gens = Gens::default();
    let h = Repo::open(f.path()).unwrap().handle();
    let tips = tips(&f);
    let worst = Arc::new(std::sync::Mutex::new(Duration::ZERO));
    let mut reader = None;
    exec(&h, Request::Walk { session: 0, tips }, &mut |m| {
        if let Msg::HistoryStarted { history, .. } = m {
            let worst = worst.clone();
            reader = Some(std::thread::spawn(move || {
                let end = std::time::Instant::now() + Duration::from_millis(300);
                while std::time::Instant::now() < end {
                    let t = std::time::Instant::now();
                    let n = history.read().unwrap().len();
                    let waited = t.elapsed();
                    let mut w = worst.lock().unwrap();
                    *w = (*w).max(waited);
                    drop(w);
                    if n >= 30_000 {
                        break;
                    }
                    std::thread::sleep(Duration::from_micros(200));
                }
            }));
        }
    }, &gens);
    reader.unwrap().join().unwrap();
    let w = *worst.lock().unwrap();
    assert!(w < Duration::from_millis(5), "a reader waited {w:?} for the history lock");
}

#[test]
fn prefetch_lists_files_without_stats() {
    let f = Fixture::new();
    let ids = five(&f);
    let msgs = run(&f, Request::Files { generation: 0, of: FilesOf::Commit(id(&ids[1])), prefetch: true });
    assert!(matches!(&msgs[..], [Msg::Files { prefetch: true, .. }]), "{msgs:?}");
}

#[test]
fn highlight_returns_spans_and_honours_cancel() {
    use gitty::msg::HlKey;
    use gitty_core::commit_files::BlobId;
    use gitty_core::diff::text::Text;
    let f = Fixture::new();
    let gens = Gens::default();
    let text = Arc::new(Text::new(b"fn main() {}\n".to_vec()));
    let key = HlKey { blob: BlobId([1; 20]), path: "src/a.rs".into() };
    let out = run_with(&f.path(), &gens, Request::Highlight { generation: 0, key: key.clone(), text: text.clone() });
    match &out[..] {
        [Msg::Highlighted { key: k, spans: Some(h), cancelled: false }] => {
            assert_eq!(k, &key);
            assert_eq!(gitty_highlight::CAPTURES[h.line(0)[0].cap as usize], "keyword");
        }
        m => panic!("{m:?}"),
    }
    Gens::bump(&gens.file);
    let out = run_with(&f.path(), &gens, Request::Highlight { generation: 0, key, text });
    assert!(matches!(&out[..], [Msg::Highlighted { spans: None, cancelled: true, .. }]), "{out:?}");
}

fn status_entry(f: &Fixture, path: &str) -> gitty_core::status::StatusEntry {
    match &run(f, Request::Status { generation: 1, mark: None })[..] {
        [Msg::Status { generation: 1, result: Ok(st) }] => st.entries.iter().find(|e| e.path == path).cloned().expect("entry"),
        m => panic!("{m:?}"),
    }
}

fn change_diff(f: &Fixture, path: &str) -> (gitty_core::status::StatusEntry, gitty::msg::DiffKey, Arc<gitty_core::diff::FileDiff>, gitty_core::stage::Texts, Option<Vec<bool>>, bool) {
    let entry = status_entry(f, path);
    match run(f, Request::ChangeDiff { generation: 3, entry, opts: DiffOptions::default(), force_text: false }).pop() {
        Some(Msg::ChangeDiff { generation: 3, entry, key, diff, texts, staged, divergent }) => (entry, key, diff, texts, staged, divergent),
        m => panic!("{m:?}"),
    }
}

fn write(f: &Fixture, op: gitty::msg::WriteOp) -> (Result<Option<String>, String>, Vec<String>) {
    let mut log = Vec::new();
    let mut done = None;
    for m in run(f, Request::Write(op)) {
        match m {
            Msg::WriteLog { line } => log.push(line),
            Msg::WriteDone { result, .. } => done = Some(result),
            m => panic!("{m:?}"),
        }
    }
    (done.expect("WriteDone"), log)
}

#[test]
fn change_diff_reports_staged_lines_and_set_staged_writes_them() {
    let f = Fixture::new();
    f.write("a.txt", "a\nb\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nB\nc\n");
    let (entry, key, diff, texts, staged, divergent) = change_diff(&f, "a.txt");
    assert_eq!(staged, Some(vec![false, false, false]));
    assert!(!divergent);
    assert_eq!(key.new, Some(texts.wt_blob));
    let flags = vec![false, false, true];
    let (r, _) = write(&f, gitty::msg::WriteOp::SetStaged { entry, texts, diff, flags });
    assert_eq!(r, Ok(None));
    assert_eq!(f.git(&["show", ":a.txt"]), "a\nb\nc");
    let (_, _, _, _, staged, _) = change_diff(&f, "a.txt");
    assert_eq!(staged, Some(vec![false, false, true]));
}

#[test]
fn commit_streams_hook_output_and_undo_returns_the_message() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let hook = f.path().join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'checking style' >&2\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.write("a.txt", "a\nb\n");
    let (r, _) = write(&f, gitty::msg::WriteOp::StageAll);
    assert_eq!(r, Ok(None));
    let (r, log) = write(&f, gitty::msg::WriteOp::Commit { message: "Add b".into(), amend: false });
    assert_eq!(r, Ok(Some(f.git(&["rev-parse", "HEAD"]))), "the new HEAD");
    assert!(log.iter().any(|l| l.contains("checking style")), "{log:?}");
    let head = f.git(&["rev-parse", "HEAD"]);
    let (r, _) = write(&f, gitty::msg::WriteOp::UndoCommit { expect: f.git(&["rev-parse", "HEAD^"]) });
    assert!(r.unwrap_err().contains("HEAD is no longer"), "not the commit gitty made");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    let (r, _) = write(&f, gitty::msg::WriteOp::UndoCommit { expect: head });
    assert_eq!(r, Ok(Some("Add b\n".into())));
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
}

#[test]
fn undo_refuses_a_commit_already_on_the_upstream() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.add_bare_upstream();
    f.write("a.txt", "a\nb\n");
    f.commit("pushed", 1_700_000_100);
    f.git(&["push", "-q"]);
    let head = f.git(&["rev-parse", "HEAD"]);
    let (r, _) = write(&f, gitty::msg::WriteOp::UndoCommit { expect: head.clone() });
    assert!(r.unwrap_err().contains("already pushed"));
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
}

#[test]
fn discards_copy_to_trash_and_refuse_changed_files() {
    let trash = tempfile::tempdir().unwrap();
    // SAFETY: tests in this binary that read GITTY_TRASH_DIR all set the same value
    unsafe { std::env::set_var("GITTY_TRASH_DIR", trash.path()) };
    let f = Fixture::new();
    f.write("a.txt", "a\nb\n");
    f.write("gone.txt", "g\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nB\n");
    f.write("gone.txt", "edited\n");
    f.write("new.txt", "fresh\n");
    let stale = gitty_core::commit_files::BlobId::hash_of(b"something else");
    let (r, _) = write(&f, gitty::msg::WriteOp::WriteFile { path: "a.txt".into(), bytes: b"a\nb\n".to_vec(), expect: stale, head_path: "a.txt".into(), head: None });
    assert!(r.unwrap_err().contains("changed on disk"));
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "a\nB\n");
    let expect = gitty_core::commit_files::BlobId::hash_of(b"a\nB\n");
    let (r, _) = write(&f, gitty::msg::WriteOp::WriteFile { path: "a.txt".into(), bytes: b"a\nb\n".to_vec(), expect, head_path: "a.txt".into(), head: Some(stale) });
    assert!(r.unwrap_err().contains("changed in HEAD"), "HEAD:a.txt is not the one the diff was made against");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "a\nB\n");
    let head = Some(gitty_core::commit_files::BlobId::hash_of(b"a\nb\n"));
    let (r, _) = write(&f, gitty::msg::WriteOp::WriteFile { path: "a.txt".into(), bytes: b"a\nb\n".to_vec(), expect, head_path: "a.txt".into(), head });
    assert_eq!(r, Ok(None));
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "a\nb\n");
    let (r, _) = write(&f, gitty::msg::WriteOp::DiscardFiles { restore: vec!["gone.txt".into()], remove: vec!["new.txt".into()] });
    assert_eq!(r, Ok(None));
    assert_eq!(std::fs::read_to_string(f.path().join("gone.txt")).unwrap(), "g\n");
    assert!(!f.path().join("new.txt").exists());
    let saved: Vec<String> = std::fs::read_dir(trash.path()).unwrap().map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap()).collect();
    for want in ["a\nB\n", "edited\n", "fresh\n"] {
        assert!(saved.iter().any(|s| s == want), "{want:?} not in trash: {saved:?}");
    }
}

#[test]
fn refresh_index_keeps_status_and_tolerates_changes() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "changed\n");
    std::fs::remove_file(f.path().join("b.txt")).unwrap();
    let before = f.git(&["status", "--porcelain"]);
    let (r, _) = write(&f, gitty::msg::WriteOp::RefreshIndex);
    assert_eq!(r, Ok(None));
    assert_eq!(f.git(&["status", "--porcelain"]), before);
}

#[test]
fn staging_every_line_refuses_a_worktree_saved_since_the_diff() {
    let f = Fixture::new();
    f.write("a.txt", "a\nb\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nB\nb\n");
    let (entry, _, diff, texts, staged, _) = change_diff(&f, "a.txt");
    let flags = vec![true; staged.unwrap().len()];
    f.write("a.txt", "a\nB\nb\nSECRET\n");
    let (r, _) = write(&f, gitty::msg::WriteOp::SetStaged { entry, texts, diff, flags });
    assert!(r.as_ref().is_err_and(|e| e.contains("changed")), "{r:?}");
    assert_eq!(f.git(&["show", ":a.txt"]), "a\nb", "index untouched");
}

#[test]
fn symlink_diffs_offer_no_line_staging() {
    let f = Fixture::new();
    std::os::unix::fs::symlink("old-target", f.path().join("link")).unwrap();
    f.commit("base", 1_700_000_000);
    std::fs::remove_file(f.path().join("link")).unwrap();
    std::os::unix::fs::symlink("new-target", f.path().join("link")).unwrap();
    let (_, _, _, _, staged, _) = change_diff(&f, "link");
    assert_eq!(staged, None);
}

#[test]
fn local_pull_steps_wait_for_the_writer() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let bare = f.add_bare_upstream();
    common::push_as_someone_else(&bare, "b.txt");
    f.git(&["fetch", "-q"]);
    let guard = gitty::write::lock();
    let (tx, rx) = std::sync::mpsc::channel();
    let path = f.path();
    std::thread::spawn(move || {
        let h = gitty_core::Repo::open(&path).unwrap().handle();
        let req = Request::Net { op: gitty::msg::NetOp::Pull, mode: gitty_core::net::Mode::Background, background: false };
        exec(&h, req, &mut |m| {
            let _ = tx.send(m);
        }, &Gens::default());
    });
    // the fetch half may run; the fast-forward must not start while a write holds the lock
    let mut seen = Vec::new();
    while let Ok(m) = rx.recv_timeout(Duration::from_millis(500)) {
        seen.push(format!("{m:?}"));
    }
    assert!(!seen.iter().any(|m| m.contains("Updating")), "{seen:?}");
    drop(guard);
    let done = loop {
        let m = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        if matches!(m, Msg::NetDone { .. }) {
            break m;
        }
    };
    assert!(format!("{done:?}").contains("Ok"), "{done:?}");
}

#[test]
fn a_panicking_status_run_still_answers_the_status_request() {
    let reply = gitty::workers::panic_reply(&Request::Status { generation: 7, mark: None });
    match reply("index out of bounds".into()) {
        Msg::Status { generation: 7, result: Err(e) } => assert!(e.contains("index out of bounds"), "{e}"),
        m => panic!("wanted a failed Status, got {m:?}"),
    }
}

#[test]
fn search_work_has_its_own_pool_so_readers_never_wait_behind_it() {
    use gitty::workers::{Pool, route};
    let f = Fixture::new();
    f.commit("one", 1_700_000_000);
    let history = run(&f, Request::Walk { session: 0, tips: tips(&f) }).into_iter().find_map(|m| match m {
        Msg::HistoryStarted { history, .. } => Some(history),
        _ => None,
    });
    let query = Arc::new(gitty_core::search::Query::parse("x").unwrap());
    assert_eq!(route(&Request::Search { generation: 1, query, paths: None, history: history.clone().unwrap(), range: 0..1 }), Pool::Search, "the per-chunk scan");
    assert_eq!(route(&Request::SearchPath { generation: 1, tips: vec![], path: "p".into() }), Pool::Search);
    assert_eq!(route(&Request::RangeCount { generation: 1, oldest: CommitId::from_hex(&"a".repeat(40)).unwrap(), newest: CommitId::from_hex(&"b".repeat(40)).unwrap(), history: history.unwrap(), rows: 0..0 }), Pool::Search);
    assert_eq!(route(&Request::Refs), Pool::Readers);
    assert_eq!(route(&Request::Status { generation: 1, mark: None }), Pool::Readers);
}

#[test]
fn a_lock_replaced_after_the_offer_is_not_removed() {
    // SAFETY: tests in this binary that read GITTY_PGREP all set the same value
    unsafe { std::env::set_var("GITTY_PGREP", "false") };
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let lock = f.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let seen = gitty::write::LockId::of(&lock).expect("the lock exists");
    // another git takes the lock again in between
    std::fs::remove_file(&lock).unwrap();
    std::fs::write(&lock, "busy").unwrap();
    let (r, _) = write(&f, gitty::msg::WriteOp::RemoveIndexLock { seen });
    assert!(r.unwrap_err().contains("changed"), "a different lock");
    assert!(lock.exists());
    let seen = gitty::write::LockId::of(&lock).unwrap();
    let (r, _) = write(&f, gitty::msg::WriteOp::RemoveIndexLock { seen });
    assert_eq!(r, Ok(None));
    assert!(!lock.exists());
}
