#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use common::Fixture;
use gitty::msg::{Gens, Msg, Request};
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
    let msgs = run(&f, Request::Files { generation: 0, id: id(&c), prefetch: false });
    let Msg::Files { files, .. } = &msgs[0] else { panic!("{msgs:?}") };
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["a.txt", "c.bin"]);
    let stats: Vec<_> = msgs[1..]
        .iter()
        .flat_map(|m| if let Msg::Stats { start, stats, .. } = m { stats.iter().enumerate().map(|(i, s)| (start + i, *s)).collect() } else { vec![] })
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
    let msgs = run_with(&f.path(), &gens, Request::Files { generation: 1, id: id(&ids[1]), prefetch: false });
    assert!(msgs.is_empty(), "{msgs:?}");
    let msgs = run_with(&f.path(), &gens, Request::Detail { generation: 1, id: id(&ids[1]) });
    assert!(msgs.is_empty(), "{msgs:?}");
}

#[test]
fn prefetch_runs_even_if_stale() {
    let f = Fixture::new();
    let ids = five(&f);
    let gens = Gens::default();
    gens.commit.store(9, SeqCst);
    let msgs = run_with(&f.path(), &gens, Request::Files { generation: 1, id: id(&ids[1]), prefetch: true });
    assert!(matches!(&msgs[0], Msg::Files { prefetch: true, .. }), "{msgs:?}");
}

fn files_of(f: &Fixture, c: &str) -> Arc<Vec<gitty_core::commit_files::FileChange>> {
    match &run(f, Request::Files { generation: 0, id: id(c), prefetch: false })[0] {
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
    let key = gitty::msg::DiffKey { old: None, new: None, path: "a".into(), opts: DiffOptions::default(), force_text: false };
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
    let msgs = run(&f, Request::Files { generation: 0, id: id(&c), prefetch: false });
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
        Request::Files { generation: 0, id: bogus, prefetch: false },
        Request::Detail { generation: 0, id: bogus },
        Request::Rows { session: 0, ids: vec![(0, bogus)] },
    ] {
        let msgs = run(&f, req);
        assert!(msgs.iter().all(|m| !matches!(m, Msg::Files { .. } | Msg::Detail { .. })), "{msgs:?}");
    }
}

#[test]
fn ahead_behind_message() {
    let f = Fixture::new();
    let ids = five(&f);
    let msgs = run(&f, Request::AheadBehind { local: id(&ids[4]), upstream: id(&ids[2]) });
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
