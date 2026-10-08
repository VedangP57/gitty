#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Fixture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gitty::app::diffstate::{DiffState, VRow};
use gitty::app::{App, AppInit, Focus};
use gitty::config::{Config, UiState};
use gitty::exec::exec;
use gitty::msg::{DiffKey, FilesOf, Gens, HlKey, Msg, Request};
use gitty::msg::WriteOp;
use gitty::theme::{ColorDepth, Registry};
use gitty_core::diff::ops::WsMode;
use gitty_core::diff::view::Row;
use gitty_core::diff::{DiffOptions, FileDiff};
use gitty_core::{CommitId, Handle, Repo};

const NOW: i64 = 1_790_899_200;

struct H {
    app: App,
    h: Handle,
    gens: Arc<Gens>,
    clock: Instant,
    /// Every highlight request executed so far.
    highlights: Vec<HlKey>,
}

impl H {
    fn new(f: &Fixture) -> H {
        H::with(f, Config::default(), None)
    }
    fn with(f: &Fixture, config: Config, config_path: Option<std::path::PathBuf>) -> H {
        let repo = Repo::open(f.path()).unwrap();
        let registry = Registry::load(None);
        let theme = registry.resolve("github-dark", ColorDepth::True, None).unwrap();
        let gens = Arc::new(Gens::default());
        let clock = Instant::now();
        let app = App::new(AppInit {
            repo_name: "repo".into(),
            config,
            registry,
            theme,
            depth: ColorDepth::True,
            ui_state: UiState::default(),
            config_path,
            state_path: None,
            gens: gens.clone(),
            now: NOW,
            clock,
            size: (140, 40),
        });
        H { app, h: repo.handle(), gens, clock, highlights: Vec::new() }
    }
    fn exec_all(&mut self, reqs: Vec<Request>) -> Vec<Msg> {
        let mut out = Vec::new();
        for r in reqs {
            if let Request::Highlight { key, .. } = &r {
                self.highlights.push(key.clone());
            }
            exec(&self.h, r, &mut |m| out.push(m), &self.gens);
        }
        out
    }
    /// Runs requests and delivers results until quiet, advancing the clock past debounces.
    fn pump(&mut self) {
        for _ in 0..1000 {
            let reqs = self.app.take_requests();
            if reqs.is_empty() {
                match self.app.next_deadline() {
                    Some(d) => {
                        self.clock = self.clock.max(d) + Duration::from_millis(1);
                        self.app.tick(self.clock);
                        continue;
                    }
                    None => return,
                }
            }
            for m in self.exec_all(reqs) {
                self.app.handle_msg(m);
            }
        }
        panic!("pump did not settle");
    }
    /// Runs requests and delivers results until none are left, ignoring timers (the focused
    /// status backstop re-arms forever, so [`H::pump`] cannot settle while focused).
    fn drain(&mut self) {
        for _ in 0..1000 {
            let reqs = self.app.take_requests();
            if reqs.is_empty() {
                return;
            }
            for m in self.exec_all(reqs) {
                self.app.handle_msg(m);
            }
        }
        panic!("drain did not settle");
    }
    fn key(&mut self, c: KeyCode) {
        self.app.handle_key(KeyEvent::new(c, KeyModifiers::NONE));
    }
    fn ch(&mut self, c: char) {
        self.key(KeyCode::Char(c));
    }
    fn selected_id(&self) -> CommitId {
        self.app.selected_id().unwrap()
    }
}

fn id(hex: &str) -> CommitId {
    CommitId::from_hex(hex).unwrap()
}

fn commits(f: &Fixture, n: usize) -> Vec<String> {
    (0..n)
        .map(|i| {
            f.write("a.txt", format!("line {i}\n"));
            if i % 2 == 0 {
                f.write(&format!("dir/f{i}.txt"), "x\n");
            }
            f.commit(&format!("commit {i}"), 1_700_000_000 + i as i64 * 100)
        })
        .collect()
}

#[test]
fn startup_requests_refs_then_walk_then_rows() {
    let f = Fixture::new();
    let ids = commits(&f, 5);
    let mut t = H::new(&f);
    let r = t.app.take_requests();
    assert!(matches!(r.as_slice(), [Request::Refs, Request::Status { .. }]));
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    let r = t.app.take_requests();
    assert!(r.iter().any(|r| matches!(r, Request::Walk { .. })));
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    let r = t.app.take_requests();
    assert!(r.iter().any(|r| matches!(r, Request::Rows { ids, .. } if ids.len() == 5)));
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    t.pump();
    assert_eq!(t.app.history_len, 5);
    assert_eq!(t.app.rows.len(), 5);
    assert_eq!(t.selected_id(), id(&ids[4]));
    assert_eq!(t.app.detail.as_ref().unwrap().row.id, id(&ids[4]));
    assert!(t.app.files.is_some());
    assert!(t.app.diff.is_some(), "first file's diff loads on startup");
}

#[test]
fn selecting_commit_requests_files_and_detail() {
    let f = Fixture::new();
    let ids = commits(&f, 5);
    let mut t = H::new(&f);
    t.pump();
    t.ch('j');
    let r = t.app.take_requests();
    let requested = r.iter().any(|r| matches!(r, Request::Files { of: FilesOf::Commit(id), prefetch: false, .. } if *id == CommitId::from_hex(&ids[3]).unwrap()));
    assert!(requested || t.app.files_for() == Some(id(&ids[3])), "files requested or served from the prefetch cache");
    assert!(r.iter().any(|r| matches!(r, Request::Detail { .. })) || t.app.detail.is_some());
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    t.pump();
    assert_eq!(t.app.files_for(), Some(id(&ids[3])));
    assert_eq!(t.app.detail.as_ref().unwrap().row.id, id(&ids[3]));
}

#[test]
fn stale_files_message_ignored() {
    let f = Fixture::new();
    let ids = commits(&f, 5);
    let mut t = H::new(&f);
    t.pump();
    t.ch('j');
    let _ = t.app.take_requests();
    let stale_gen = t.gens.commit.load(std::sync::atomic::Ordering::SeqCst);
    t.ch('j');
    let shown = t.app.files_for();
    t.app.handle_msg(Msg::Files { generation: stale_gen, of: FilesOf::Commit(id(&ids[3])), files: Arc::new(vec![]), prefetch: false });
    assert_eq!(t.app.files_for(), shown);
    assert_ne!(t.app.files_for(), Some(id(&ids[3])));
    t.pump();
    assert_eq!(t.app.files_for(), Some(id(&ids[2])));
}

#[test]
fn files_arrival_selects_first_and_debounces_diff() {
    let f = Fixture::new();
    commits(&f, 5);
    let mut t = H::new(&f);
    t.pump();
    t.ch('j');
    let r = t.app.take_requests();
    let msgs = t.exec_all(r.into_iter().filter(|r| matches!(r, Request::Files { prefetch: false, .. })).collect());
    for m in msgs {
        t.app.handle_msg(m);
    }
    assert_eq!(t.app.file_sel, 0);
    let r = t.app.take_requests();
    assert!(!r.iter().any(|r| matches!(r, Request::Diff { .. })), "diff must wait for the debounce");
    let deadline = t.app.next_deadline().expect("diff debounce pending");
    t.app.tick(deadline - Duration::from_millis(5));
    assert!(!t.app.take_requests().iter().any(|r| matches!(r, Request::Diff { .. })));
    t.app.tick(deadline);
    assert!(t.app.take_requests().iter().any(|r| matches!(r, Request::Diff { .. })));
}

#[test]
fn diff_lru_hit_skips_request() {
    let f = Fixture::new();
    commits(&f, 5);
    let mut t = H::new(&f);
    t.pump();
    let first = t.app.diff.as_ref().unwrap().key.clone();
    t.ch('j');
    t.pump();
    t.ch('k');
    let mut saw_diff = false;
    for _ in 0..20 {
        let r = t.app.take_requests();
        saw_diff |= r.iter().any(|r| matches!(r, Request::Diff { .. }));
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
        if let Some(d) = t.app.next_deadline() {
            t.app.tick(d);
        }
    }
    assert!(!saw_diff, "cached diff must not be recomputed");
    assert_eq!(t.app.diff.as_ref().unwrap().key, first);
}

#[test]
fn prefetch_neighbors_issued() {
    let f = Fixture::new();
    commits(&f, 30);
    let mut t = H::new(&f);
    let mut prefetched = 0;
    for _ in 0..200 {
        let r = t.app.take_requests();
        prefetched += r.iter().filter(|r| matches!(r, Request::Files { prefetch: true, .. })).count();
        if r.is_empty() {
            match t.app.next_deadline() {
                Some(d) => t.app.tick(d),
                None => break,
            }
            continue;
        }
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
    }
    assert_eq!(prefetched, 10, "the 10 commits after the newest (none before it)");
    t.ch('j');
    assert!(t.app.files.is_some(), "the prefetched file list shows immediately");
    let r = t.app.take_requests();
    assert!(r.iter().any(|r| matches!(r, Request::Files { prefetch: false, .. })), "stats are computed on selection");
}

#[test]
fn scope_toggle_restarts_walk_and_reselects() {
    let f = Fixture::new();
    let ids = commits(&f, 4);
    f.git(&["checkout", "-q", "-b", "side", &ids[1]]);
    f.write("side.txt", "s\n");
    let side = f.commit("side work", 1_700_000_250);
    f.git(&["checkout", "-q", "main"]);
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.history_len, 4);
    t.ch('j');
    t.pump();
    let sel = t.selected_id();
    assert_eq!(sel, id(&ids[2]));
    t.ch('r');
    assert!(t.app.take_requests_peek().iter().any(|r| matches!(r, Request::Walk { tips, .. } if tips.contains(&id(&side)))));
    t.pump();
    assert_eq!(t.app.history_len, 5);
    assert_eq!(t.selected_id(), sel);
    assert_eq!(t.app.selected, 2, "side work (t=250) sorts between commit 3 (300) and commit 2 (200)");
}

fn three_hunks() -> FileDiff {
    let mut old = String::new();
    for i in 0..60 {
        old.push_str(&format!("line {i}\n"));
    }
    let new = old.replace("line 10\n", "line ten\n").replace("line 30\n", "line thirty\n").replace("line 50\n", "line fifty\n");
    FileDiff::from_bytes("f.txt", None, old.into_bytes(), new.into_bytes(), 0o100644, 0o100644, DiffOptions::default())
}
fn key() -> DiffKey {
    DiffKey { old: None, new: None, path: "f.txt".into(), old_path: None, old_mode: 0, new_mode: 0, opts: DiffOptions::default(), force_text: false }
}
fn row_kind(d: &DiffState, i: usize) -> String {
    match d.vrow(i, false) {
        None => "none".into(),
        Some(VRow::Header(_)) => "header".into(),
        Some(VRow::Row(Row::Gap { .. })) => "gap".into(),
        Some(VRow::Row(Row::Context { new, .. })) => format!("ctx{new}"),
        Some(VRow::Row(Row::Del { old, .. })) => format!("del{old}"),
        Some(VRow::Row(Row::Add { new, .. })) => format!("add{new}"),
        Some(VRow::Split(_)) => "split".into(),
    }
}

#[test]
fn expand_near_cursor_rules() {
    let mut d = DiffState::new(key(), Arc::new(three_hunks()));
    let gaps: Vec<usize> = (0..d.rows(false)).filter(|&i| row_kind(&d, i) == "gap").collect();
    assert_eq!(gaps.len(), 4, "leading, two middle, trailing");
    // cursor on a middle gap row: expands it entirely
    let before = d.rows(false);
    d.cursor = gaps[1];
    d.expand_near_cursor(false);
    assert!(d.rows(false) > before);
    assert_eq!((0..d.rows(false)).filter(|&i| row_kind(&d, i) == "gap").count(), 3);
    // cursor below a gap: expands upward (towards the cursor) and the cursor keeps its line
    let mut d = DiffState::new(key(), Arc::new(three_hunks()));
    let target = (0..d.rows(false)).find(|&i| row_kind(&d, i) == "add30").unwrap();
    d.cursor = target;
    d.expand_near_cursor(false);
    assert_eq!(row_kind(&d, d.cursor), "add30");
    // cursor above a gap (last line of hunk 1's context): expands downward, cursor stays
    let mut d = DiffState::new(key(), Arc::new(three_hunks()));
    let c = (0..d.rows(false)).find(|&i| row_kind(&d, i) == "ctx13").unwrap();
    d.cursor = c;
    let before = d.rows(false);
    d.expand_near_cursor(false);
    assert!(d.rows(false) > before);
    assert_eq!(row_kind(&d, d.cursor), "ctx13");
    assert_eq!(row_kind(&d, d.cursor + 1), "ctx14");
}

#[test]
fn whole_file_toggle_keeps_cursor_content() {
    let mut d = DiffState::new(key(), Arc::new(three_hunks()));
    d.cursor = (0..d.rows(false)).find(|&i| row_kind(&d, i) == "add50").unwrap();
    d.toggle_whole_file(false);
    assert_eq!(row_kind(&d, d.cursor), "add50");
    assert!(d.view.is_fully_expanded());
    d.toggle_whole_file(false);
    assert_eq!(row_kind(&d, d.cursor), "add50");
    assert!(!d.view.is_fully_expanded());
    d.toggle_whole_file(true);
    let split_row = d.cursor;
    assert!(split_row < d.rows(true));
}

#[test]
fn hunk_navigation() {
    let mut d = DiffState::new(key(), Arc::new(three_hunks()));
    d.cursor = 0;
    d.next_hunk(false, 1);
    assert_eq!(row_kind(&d, d.cursor), "del10");
    d.next_hunk(false, 1);
    assert_eq!(row_kind(&d, d.cursor), "del30");
    d.next_hunk(false, 1);
    d.next_hunk(false, 1);
    assert_eq!(row_kind(&d, d.cursor), "del50", "stays on the last hunk");
    d.next_hunk(false, -1);
    assert_eq!(row_kind(&d, d.cursor), "del30");
    assert_eq!(d.scroll, d.cursor - 3);
}

#[test]
fn synthetic_first_header_when_change_at_top() {
    let fd = FileDiff::from_bytes("t", None, b"a\nb\nc\n".to_vec(), b"A\nb\nc\nd\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    let d = DiffState::new(key(), Arc::new(fd));
    assert_eq!(d.first_header.as_deref(), Some("@@ -1,3 +1,4 @@"));
    assert_eq!(row_kind(&d, 0), "header");
    assert_eq!(row_kind(&d, 1), "del0");
    assert_eq!(d.rows(false), 1 + d.view.row_count());
    let d = DiffState::new(key(), Arc::new(three_hunks()));
    assert!(d.first_header.is_none(), "a gap row already heads the first hunk");
    let fd = FileDiff::from_bytes("t", None, Vec::new(), b"new\n".to_vec(), 0, 0o100644, DiffOptions::default());
    let d = DiffState::new(key(), Arc::new(fd));
    assert_eq!(d.first_header.as_deref(), Some("@@ -0,0 +1,1 @@"));
}

#[test]
fn empty_view_rows_zero_no_panic() {
    let fd = FileDiff::from_bytes("t", None, b"same\n".to_vec(), b"same\n".to_vec(), 0o100644, 0o100644, DiffOptions::default());
    let mut d = DiffState::new(key(), Arc::new(fd));
    for split in [false, true] {
        assert!(d.vrow(0, split).is_none() || d.rows(split) > 0);
        d.expand_near_cursor(split);
        d.toggle_whole_file(split);
        d.next_hunk(split, 1);
        d.next_hunk(split, -1);
        d.apply_ready_pairing(split);
        let _ = d.rows(split);
    }
}

const ALL_KEYS: &[char] = &['j', 'k', 'g', 'G', 'h', 'l', '[', ']', '{', '}', 'e', 'E', 's', 'w', 'W', 'F', 'o', 'D', 'z', 'r', 'y', 'Y', '<', '>', '1', '2', '?', 'T', '!'];

fn mash(t: &mut H) {
    for focus in [Focus::History, Focus::Files, Focus::Diff] {
        t.app.focus = focus;
        for &c in ALL_KEYS {
            t.ch(c);
            t.key(KeyCode::Esc);
            t.pump();
        }
        for k in [KeyCode::Enter, KeyCode::Tab, KeyCode::BackTab, KeyCode::PageDown, KeyCode::PageUp, KeyCode::Up, KeyCode::Down, KeyCode::Home, KeyCode::End, KeyCode::Enter, KeyCode::Esc] {
            t.key(k);
            t.pump();
        }
        t.app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        t.app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        t.pump();
    }
}

#[test]
fn unborn_repo_keys_dont_panic() {
    let f = Fixture::new();
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.history_len, 0);
    assert!(t.app.selected_id().is_none());
    for size in [(140, 40), (100, 30), (200, 50), (10, 3), (0, 0)] {
        t.app.handle_resize(size.0, size.1);
        mash(&mut t);
    }
    assert!(!t.app.quit);
}

#[test]
fn identical_rename_keys_dont_panic() {
    let f = Fixture::new();
    f.write("a.txt", "same\n");
    f.commit("one", 1_700_000_000);
    f.git(&["mv", "a.txt", "b.txt"]);
    f.commit("two", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    let d = t.app.diff.as_ref().unwrap();
    assert!(d.diff.changes.is_empty());
    for size in [(140, 40), (220, 50), (90, 20)] {
        t.app.handle_resize(size.0, size.1);
        mash(&mut t);
    }
}

#[test]
fn theme_picker_preview_and_revert() {
    let f = Fixture::new();
    commits(&f, 2);
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("gitty/config.toml");
    let mut t = H::with(&f, Config::default(), Some(cfg.clone()));
    t.pump();
    let original = t.app.theme.name.clone();
    t.ch('T');
    assert!(t.app.overlay.is_some());
    t.ch('j');
    assert_ne!(t.app.theme.name, original, "preview applies live");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
    assert_eq!(t.app.theme.name, original);
    t.ch('T');
    t.ch('j');
    t.ch('j');
    let chosen = t.app.theme.name.clone();
    t.key(KeyCode::Enter);
    assert!(t.app.overlay.is_none());
    assert_eq!(t.app.config.theme, chosen);
    let (saved, _) = Config::load(&cfg);
    assert_eq!(saved.theme, chosen);
}

#[test]
fn whitespace_cycle_rerequests() {
    let f = Fixture::new();
    commits(&f, 3);
    let mut t = H::new(&f);
    t.pump();
    t.ch('w');
    assert_eq!(t.app.ws, WsMode::IgnoreAll);
    let d = t.app.next_deadline().unwrap();
    t.app.tick(d);
    let r = t.app.take_requests();
    assert!(r.iter().any(|r| matches!(r, Request::Diff { opts, .. } if opts.ws == WsMode::IgnoreAll)));
}

#[test]
fn enter_and_esc_move_focus() {
    let f = Fixture::new();
    commits(&f, 3);
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.focus, Focus::History);
    t.key(KeyCode::Enter);
    assert_eq!(t.app.focus, Focus::Files);
    t.key(KeyCode::Enter);
    assert_eq!(t.app.focus, Focus::Diff);
    t.key(KeyCode::Esc);
    assert_eq!(t.app.focus, Focus::Files);
    t.key(KeyCode::Tab);
    assert_eq!(t.app.focus, Focus::Diff);
    t.key(KeyCode::Tab);
    assert_eq!(t.app.focus, Focus::History);
    t.key(KeyCode::BackTab);
    assert_eq!(t.app.focus, Focus::Diff);
    t.ch('q');
    assert!(t.app.quit);
}

#[test]
fn copy_sha_emits_osc52() {
    let f = Fixture::new();
    let ids = commits(&f, 2);
    let mut t = H::new(&f);
    t.pump();
    t.ch('Y');
    let osc = t.app.osc_out.pop().unwrap();
    assert!(osc.starts_with("\x1b]52;c;") && osc.ends_with('\x07'));
    let short = t.app.osc_out.is_empty();
    assert!(short);
    t.ch('y');
    assert_eq!(t.app.osc_out.len(), 1);
    let _ = ids;
}

#[test]
fn hold_j_through_200_commits_final_state_matches_selection() {
    let f = Fixture::new();
    commits(&f, 200);
    let mut t = H::new(&f);
    t.pump();
    let mut late: Vec<Msg> = Vec::new();
    for _ in 0..150 {
        t.ch('j');
        let r = t.app.take_requests();
        let msgs = t.exec_all(r);
        for m in late.drain(..) {
            t.app.handle_msg(m);
        }
        late = msgs;
        t.clock += Duration::from_millis(5);
        t.app.tick(t.clock);
    }
    for m in late.drain(..) {
        t.app.handle_msg(m);
    }
    t.pump();
    let sel = t.selected_id();
    assert_eq!(t.app.selected, 150);
    assert_eq!(t.app.files_for(), Some(sel));
    assert_eq!(t.app.detail.as_ref().unwrap().row.id, sel);
    let files = t.app.files.as_ref().unwrap();
    assert_eq!(t.app.diff.as_ref().unwrap().key.path, files[t.app.file_sel].path);
}

#[test]
fn diff_cache_distinguishes_mode_changes() {
    let f = Fixture::new();
    f.write("a.sh", "echo\n");
    f.commit("add", 1_700_000_000);
    f.git(&["update-index", "--chmod=+x", "a.sh"]);
    f.git_env(&["commit", "-q", "-m", "exec"], &[("GIT_AUTHOR_DATE", "1700000100 +0000".into()), ("GIT_COMMITTER_DATE", "1700000100 +0000".into())]);
    f.git(&["update-index", "--chmod=-x", "a.sh"]);
    f.git_env(&["commit", "-q", "-m", "unexec"], &[("GIT_AUTHOR_DATE", "1700000200 +0000".into()), ("GIT_COMMITTER_DATE", "1700000200 +0000".into())]);
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.diff.as_ref().unwrap().diff.modes(), (0o100755, 0o100644));
    t.ch('j');
    t.pump();
    assert_eq!(t.app.diff.as_ref().unwrap().diff.modes(), (0o100644, 0o100755), "a cached diff with other modes must not be reused");
}

#[test]
fn uncached_commit_shows_loading_not_previous_diff() {
    let f = Fixture::new();
    commits(&f, 15);
    let mut t = H::new(&f);
    t.pump();
    assert!(t.app.diff.is_some());
    t.ch('G');
    let _withheld = t.app.take_requests();
    assert!(t.app.files.is_none());
    assert!(t.app.diff.is_none(), "the previous commit's diff must not stay on screen");
    assert!(t.app.diff_loading());
}

#[test]
fn files_error_is_shown_and_prefetch_errors_are_quiet() {
    let f = Fixture::new();
    let ids = commits(&f, 15);
    let mut t = H::new(&f);
    let _ = t.app.take_requests();
    for m in t.exec_all(vec![Request::Refs]) {
        t.app.handle_msg(m);
    }
    // run the walk etc. but withhold prefetch results
    for _ in 0..50 {
        let r: Vec<Request> = t.app.take_requests().into_iter().filter(|r| !matches!(r, Request::Files { prefetch: true, .. })).collect();
        if r.is_empty() {
            match t.app.next_deadline() {
                Some(d) => t.app.tick(d),
                None => break,
            }
            continue;
        }
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
    }
    let in_flight = t.app.prefetch_in_flight();
    assert_eq!(in_flight, 10);
    t.app.handle_msg(Msg::FilesError { generation: 0, of: FilesOf::Commit(id(&ids[13])), prefetch: true, detail: "boom".into() });
    assert_eq!(t.app.prefetch_in_flight(), 9, "a failed prefetch frees its slot");
    assert!(t.app.toast.is_none(), "prefetch failures are not the user's problem");
    t.ch('G');
    let generation = t.gens.commit.load(std::sync::atomic::Ordering::SeqCst);
    let sel = t.selected_id();
    t.app.handle_msg(Msg::FilesError { generation, of: FilesOf::Commit(sel), prefetch: false, detail: "object not found".into() });
    assert_eq!(t.app.files_error.as_deref(), Some("object not found"));
    assert!(!t.app.diff_loading());
}

fn many(f: &Fixture, n: usize) {
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
fn user_navigation_beats_pending_reselect() {
    let f = Fixture::new();
    many(&f, 300);
    let mut t = H::new(&f);
    t.pump();
    t.app.select(280);
    t.pump();
    let target = t.selected_id();
    t.ch('r');
    assert!(t.app.files.is_none() && t.app.diff.is_none(), "nothing from the old selection is shown while re-walking");
    let walk: Vec<Request> = t.app.take_requests();
    let mut msgs = t.exec_all(walk).into_iter();
    // HistoryStarted and the first 256-row chunk (target is at 280: not yet found)
    t.app.handle_msg(msgs.next().unwrap());
    t.app.handle_msg(msgs.next().unwrap());
    t.ch('j');
    for m in msgs {
        t.app.handle_msg(m);
    }
    t.pump();
    assert_eq!(t.app.selected, 1, "the user's choice wins over the pending reselect");
    assert_ne!(t.selected_id(), target);
}

#[test]
fn double_scope_toggle_keeps_target() {
    let f = Fixture::new();
    many(&f, 300);
    let mut t = H::new(&f);
    t.pump();
    t.app.select(280);
    t.pump();
    let target = t.selected_id();
    t.ch('r');
    t.ch('r');
    t.pump();
    assert_eq!(t.selected_id(), target);
    assert_eq!(t.app.selected, 280);
}

fn rust_pair(f: &Fixture) {
    f.write("src/lib.rs", "fn a() {}\n");
    f.commit("one", 1_700_000_000);
    f.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    f.commit("two", 1_700_000_100);
}

#[test]
fn highlight_requested_once_per_blob_and_cached() {
    let f = Fixture::new();
    rust_pair(&f);
    let mut t = H::new(&f);
    t.pump();
    let d = t.app.diff.as_ref().unwrap().key.clone();
    let new = HlKey { blob: d.new.unwrap(), path: "src/lib.rs".into() };
    assert_eq!(t.highlights, vec![new.clone()], "new side only: nothing was removed");
    let (old, hl) = t.app.diff_highlights();
    assert!(old.is_none());
    assert!(hl.unwrap().line(1).iter().any(|s| gitty_highlight::CAPTURES[s.cap as usize] == "keyword"));
    t.ch('j');
    t.pump();
    t.ch('k');
    t.pump();
    assert_eq!(t.highlights.iter().filter(|k| **k == new).count(), 1, "cache hit on return");
    assert!(t.app.diff_highlights().1.is_some());
}

#[test]
fn cancelled_or_foreign_highlights_are_not_used() {
    let f = Fixture::new();
    rust_pair(&f);
    let mut t = H::new(&f);
    // run everything except highlight requests, which go stale before they execute
    let mut held = Vec::new();
    for _ in 0..200 {
        let reqs = t.app.take_requests();
        let (hl, rest): (Vec<_>, Vec<_>) = reqs.into_iter().partition(|r| matches!(r, Request::Highlight { .. }));
        held.extend(hl);
        if rest.is_empty() {
            match t.app.next_deadline() {
                Some(d) => t.app.tick(d + Duration::from_millis(1)),
                None => break,
            }
        }
        for m in t.exec_all(rest) {
            t.app.handle_msg(m);
        }
    }
    assert_eq!(held.len(), 1);
    let Request::Highlight { key, .. } = &held[0] else { unreachable!() };
    let key = key.clone();
    let foreign = HlKey { blob: gitty_core::commit_files::BlobId([7; 20]), path: key.path.clone() };
    let spans = Some(Arc::new(gitty_highlight::Highlighter::new().highlight("x.rs", b"fn x() {}", &|| false).unwrap()));
    t.app.handle_msg(Msg::Highlighted { key: foreign, spans, cancelled: false });
    assert!(t.app.diff_highlights().1.is_none(), "another blob's spans never apply");
    Gens::bump(&t.gens.file);
    for m in t.exec_all(held) {
        t.app.handle_msg(m);
    }
    assert!(t.app.diff_highlights().1.is_none());
    t.ch('j');
    t.pump();
    t.ch('k');
    t.pump();
    assert!(t.highlights.contains(&key), "a cancelled highlight is requested again");
    assert!(t.app.diff_highlights().1.is_some());
}

#[test]
fn split_cursor_stays_on_content_when_pairing_arrives() {
    let mut old = String::from("head\n");
    let mut new = old.clone();
    old.push_str("alpha alpha alpha alpha\nlet value = compute(1, 2);\n");
    new.push_str("let value = compute(1, 3);\nzzz qqq www eee\n");
    for i in 0..10 {
        old.push_str(&format!("tail {i}\n"));
        new.push_str(&format!("tail {i}\n"));
    }
    let fd = Arc::new(FileDiff::from_bytes("f.txt", None, old.into_bytes(), new.into_bytes(), 0o100644, 0o100644, DiffOptions::default()));
    let mut d = DiffState::new(key(), fd.clone());
    let before = d.rows(true);
    let tail = (0..before).find(|&i| matches!(d.vrow(i, true), Some(VRow::Split(gitty_core::diff::view::SplitRow::Context { new: 5, .. })))).unwrap();
    d.cursor = tail;
    d.scroll = 1;
    for c in 0..fd.changes.len() {
        fd.intraline(c);
    }
    d.apply_ready_pairing(true);
    assert_ne!(d.rows(true), before, "fixture: pairing must change the split layout");
    assert!(matches!(d.vrow(d.cursor, true), Some(VRow::Split(gitty_core::diff::view::SplitRow::Context { new: 5, .. }))), "cursor stays on tail 3");
    assert_eq!(d.cursor - d.scroll, tail - 1, "and at the same screen offset");
}

#[test]
fn forced_large_text_is_not_highlighted() {
    let f = Fixture::new();
    f.write("min.js", "var a = 1;\n");
    f.commit("one", 1_700_000_000);
    f.write("min.js", format!("var a = [{}];\n", "1,".repeat(4000)));
    f.commit("two", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.app.force_show();
    t.pump();
    assert!(t.app.diff.as_ref().unwrap().key.force_text);
    assert!(t.highlights.is_empty(), "{:?}", t.highlights);
}

fn changes_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\nb\n");
    f.write("b.txt", "b\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nB\n");
    f.write("new.txt", "n\n");
    f
}

#[test]
fn startup_reads_status_and_changes_tab_loads_the_first_file() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    assert!(t.app.take_requests_peek().iter().any(|r| matches!(r, Request::Status { .. })));
    t.pump();
    let paths: Vec<String> = t.app.changes.status.as_ref().unwrap().entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(paths, ["a.txt", "new.txt"]);
    assert!(t.app.diff.is_some(), "History shows its own diff");
    t.app.set_tab(gitty::app::Tab::Changes);
    assert!(t.app.diff.is_none(), "the History diff is not shown on Changes");
    t.pump();
    let d = t.app.diff.as_ref().unwrap();
    assert_eq!(d.key.path, "a.txt");
    assert_eq!(t.app.changes.current.as_ref().unwrap().staged, Some(vec![false, false]));
    t.app.set_tab(gitty::app::Tab::History);
    t.pump();
    assert_ne!(t.app.diff.as_ref().map(|d| d.key.path.as_str()), Some("new.txt"));
}

#[test]
fn history_gets_back_the_pane_it_had_focused() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    t.pump();
    let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    assert_eq!(t.app.focus, Focus::History);
    t.app.handle_key(key('1'));
    t.app.handle_key(key('2'));
    assert_eq!(t.app.focus, Focus::History, "not the Changes file list");
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(t.app.focus, Focus::Files);
    t.app.handle_key(key('1'));
    t.app.handle_key(key('2'));
    assert_eq!(t.app.focus, Focus::Files, "History remembers its own pane");
    // a click on the History tab while writing a message must not carry the editor over
    t.app.handle_key(key('1'));
    t.app.handle_key(key('c'));
    assert_eq!(t.app.focus, Focus::Commit);
    t.app.set_tab(gitty::app::Tab::History);
    assert_eq!(t.app.focus, Focus::Files);
}

#[test]
fn stale_change_diff_is_dropped() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.set_tab(gitty::app::Tab::Changes);
    let reqs = t.app.take_requests();
    let old = t.exec_all(reqs);
    // the selection moves on before the reply lands
    t.app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    for m in old {
        t.app.handle_msg(m);
    }
    assert!(t.app.diff.as_ref().is_none_or(|d| d.key.path == "new.txt"));
    t.pump();
    assert_eq!(t.app.diff.as_ref().unwrap().key.path, "new.txt");
}

#[test]
fn watcher_bursts_coalesce_into_one_status_at_a_time() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    t.pump();
    let count = |r: &[Request]| r.iter().filter(|r| matches!(r, Request::Status { .. })).count();
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::WORKTREE));
    let first = t.app.take_requests();
    assert_eq!(count(&first), 1);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::INDEX));
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::WORKTREE));
    assert_eq!(count(t.app.take_requests_peek()), 0, "one status in flight at a time");
    for m in t.exec_all(first) {
        t.app.handle_msg(m);
    }
    assert_eq!(count(t.app.take_requests_peek()), 1, "changes during the run cause exactly one rerun");
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    assert!(t.app.take_requests_peek().iter().any(|r| matches!(r, Request::Refs)));
}

#[test]
fn refs_refresh_restarts_history_only_when_tips_move() {
    let f = Fixture::new();
    commits(&f, 3);
    let mut t = H::new(&f);
    t.pump();
    t.ch('j');
    t.pump();
    let sel = t.selected_id();
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    let walks = |t: &mut H| {
        let r = t.app.take_requests();
        let n = r.iter().filter(|r| matches!(r, Request::Walk { .. })).count();
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
        n
    };
    walks(&mut t);
    assert_eq!(walks(&mut t), 0, "same tips: no new walk");
    f.write("a.txt", "next\n");
    f.commit("next", 1_700_009_000);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    walks(&mut t);
    assert_eq!(walks(&mut t), 1, "HEAD moved: history restarts");
    t.pump();
    assert_eq!(t.app.history_len, 4);
    assert_eq!(t.selected_id(), sel, "the selected commit stays selected");
}

#[test]
fn writes_refresh_status_and_errors_toast() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.write(gitty::msg::WriteOp::StageAll);
    assert_eq!(t.app.changes.busy, 1);
    t.pump();
    assert_eq!(t.app.changes.busy, 0);
    assert!(t.app.changes.status.as_ref().unwrap().entries.iter().all(|e| e.check() == gitty_core::status::Check::Staged));
    t.app.write(gitty::msg::WriteOp::Commit { message: "Commit it".into(), amend: false });
    t.pump();
    assert_eq!(t.app.history_len, 2, "a commit refreshes refs and history");
    std::fs::write(f.path().join(".git/index.lock"), "").unwrap();
    f.write("a.txt", "again\n");
    t.app.write(gitty::msg::WriteOp::StageAll);
    t.pump();
    let toast = t.app.toast.as_ref().expect("error toast");
    assert!(toast.error && toast.detail.contains("index.lock"), "{}", toast.detail);
}

/// The stale-lock check asks `pgrep -x git`; other tests run git all the time, so this binary
/// answers "none running" (`false` exits 1, as pgrep does when nothing matches).
fn no_git_running() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: set once, before any write in this binary reads it
    ONCE.call_once(|| unsafe { std::env::set_var("GITTY_PGREP", "false") });
}

#[test]
fn a_stale_index_lock_is_offered_for_removal() {
    no_git_running();
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    std::fs::write(f.path().join(".git/index.lock"), "").unwrap();
    t.app.write(gitty::msg::WriteOp::StageAll);
    t.pump();
    match &t.app.overlay {
        Some(gitty::app::Overlay::Confirm { title, op: gitty::msg::WriteOp::RemoveIndexLock { .. }, .. }) => {
            assert_eq!(title, "Remove the stale .git/index.lock?")
        }
        _ => panic!("no offer to remove the lock; toast {:?}", t.app.toast.as_ref().map(|t| &t.detail)),
    }
    t.key(KeyCode::Enter);
    t.pump();
    assert!(!f.path().join(".git/index.lock").exists());
    t.app.write(gitty::msg::WriteOp::StageAll);
    t.pump();
    assert!(t.app.changes.status.as_ref().unwrap().entries.iter().all(|e| e.check() == gitty_core::status::Check::Staged));
}

#[test]
fn the_stale_lock_offer_waits_for_an_open_overlay() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.ch('?');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Help { .. })));
    let lock = f.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let seen = gitty::write::LockId::of(&lock).unwrap();
    t.app.handle_msg(Msg::StaleIndexLock { seen });
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Help { .. })), "help stays open");
    t.key(KeyCode::Esc);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { op: gitty::msg::WriteOp::RemoveIndexLock { .. }, .. })), "then the offer");
}

#[test]
fn a_lock_gone_before_its_offer_opens_is_never_asked_about() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.ch('?');
    let lock = f.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    let seen = gitty::write::LockId::of(&lock).unwrap();
    t.app.handle_msg(Msg::StaleIndexLock { seen });
    std::fs::remove_file(&lock).unwrap();
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none(), "{:?}", t.app.overlay.as_ref().map(|_| "an overlay"));
}

#[test]
fn a_waiting_prompt_opens_before_a_queued_lock_offer() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.ch('?');
    let lock = f.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    t.app.handle_msg(Msg::StaleIndexLock { seen: gitty::write::LockId::of(&lock).unwrap() });
    started(&mut t, gitty::msg::NetOp::Fetch, "Fetching origin", false);
    t.app.handle_msg(Msg::Ask(ask(1, "Username for 'https://example.com': ")));
    t.key(KeyCode::Esc);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Prompt { .. })), "git is waiting on the prompt");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none(), "the offer waits for the job to end");
    done(&mut t, gitty::msg::NetOp::Fetch, false, gitty_core::net::Outcome::NeedsAuth { detail: "fatal: could not read Username".into() });
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { op: gitty::msg::WriteOp::RemoveIndexLock { .. }, .. })), "then the offer");
}

#[test]
fn a_lock_offer_waits_out_the_job_between_its_prompts() {
    use gitty::app::Overlay;
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.ch('?');
    let lock = f.path().join(".git/index.lock");
    std::fs::write(&lock, "").unwrap();
    t.app.handle_msg(Msg::StaleIndexLock { seen: gitty::write::LockId::of(&lock).unwrap() });
    started(&mut t, gitty::msg::NetOp::Fetch, "Fetching origin", false);
    t.app.handle_msg(Msg::Ask(ask(1, "Username for 'https://example.com': ")));
    t.key(KeyCode::Esc);
    assert!(matches!(t.app.overlay, Some(Overlay::Prompt { .. })));
    typed(&mut t, "ann");
    t.key(KeyCode::Enter);
    // git has not asked for the password yet: no question slips in while its job runs
    assert!(t.app.overlay.is_none(), "nothing opens between the prompts");
    t.app.handle_msg(Msg::Ask(ask(2, "Password for 'https://ann@example.com': ")));
    assert!(matches!(t.app.overlay, Some(Overlay::Prompt { .. })), "the password prompt");
    // the job ends with its prompt open: the prompt closes and the offer follows
    done(&mut t, gitty::msg::NetOp::Fetch, false, gitty_core::net::Outcome::NeedsAuth { detail: "fatal: Authentication failed".into() });
    assert!(matches!(t.app.overlay, Some(Overlay::Confirm { op: gitty::msg::WriteOp::RemoveIndexLock { .. }, .. })), "then the offer");
}

#[test]
fn focus_gained_and_backstop_refresh_status() {
    let f = changes_fixture();
    let mut t = H::new(&f);
    t.pump();
    let has_status = |t: &H| t.app.take_requests_peek().iter().any(|r| matches!(r, Request::Status { .. }));
    let run_now = |t: &mut H| {
        let r = t.app.take_requests();
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
    };
    assert_eq!(t.app.next_deadline(), None, "unfocused at start: no backstop timer");
    t.app.handle_focus(true);
    assert!(has_status(&t));
    run_now(&mut t);
    let at = t.app.next_deadline().expect("backstop armed while focused");
    assert!(at >= t.clock + Duration::from_secs(59));
    t.app.clock = at;
    t.app.tick(at);
    assert!(has_status(&t), "60 s backstop while focused");
    run_now(&mut t);
    t.app.handle_focus(false);
    assert_eq!(t.app.next_deadline(), None, "no backstop while unfocused");
}

/// Changes tab with `a.txt` selected and its diff loaded.
fn changes_tab(f: &Fixture) -> H {
    let mut t = H::new(f);
    t.pump();
    t.app.set_tab(gitty::app::Tab::Changes);
    t.pump();
    t
}

fn writes(r: &[Request]) -> Vec<String> {
    r.iter()
        .filter_map(|r| match r {
            Request::Write(op) => Some(match op {
                gitty::msg::WriteOp::Stage(p) => format!("stage {p:?}"),
                gitty::msg::WriteOp::Unstage(p) => format!("unstage {p:?}"),
                gitty::msg::WriteOp::StageAll => "stage all".into(),
                gitty::msg::WriteOp::UnstageAll => "unstage all".into(),
                gitty::msg::WriteOp::SetStaged { flags, .. } => format!("lines {flags:?}"),
                gitty::msg::WriteOp::WriteFile { path, .. } => format!("write {path}"),
                gitty::msg::WriteOp::DiscardFiles { restore, remove } => format!("discard {restore:?} {remove:?}"),
                gitty::msg::WriteOp::Commit { message, amend } => format!("commit {message:?} {amend}"),
                gitty::msg::WriteOp::UndoCommit { .. } => "undo".into(),
                gitty::msg::WriteOp::RemoveIndexLock { .. } => "remove index.lock".into(),
                gitty::msg::WriteOp::RefreshIndex => "refresh index".into(),
                gitty::msg::WriteOp::Seq(ops) => format!("seq of {}", ops.len()),
                gitty::msg::WriteOp::SwitchBranch { name, .. } => format!("switch {name}"),
                gitty::msg::WriteOp::CreateBranch { name } => format!("create branch {name}"),
                gitty::msg::WriteOp::RenameBranch { old, new } => format!("rename branch {old} {new}"),
                gitty::msg::WriteOp::DeleteBranch { name, force } => format!("delete branch {name} {force}"),
                gitty::msg::WriteOp::StashPush { message } => format!("stash push {message}"),
                gitty::msg::WriteOp::StashApply { index, .. } => format!("stash apply {index}"),
                gitty::msg::WriteOp::StashPop { index, .. } => format!("stash pop {index}"),
                gitty::msg::WriteOp::StashDrop { index, .. } => format!("stash drop {index}"),
                gitty::msg::WriteOp::StashAndSwitch { name, .. } => format!("stash and switch {name}"),
                gitty::msg::WriteOp::Merge { name, remote } => format!("merge {name} {remote}"),
                gitty::msg::WriteOp::StashAndMerge { name, .. } => format!("stash and merge {name}"),
            }),
            _ => None,
        })
        .collect()
}

fn index_of(f: &Fixture, p: &str) -> String {
    f.git(&["show", &format!(":{p}")])
}

fn diff_row(t: &H, want: &str) -> usize {
    let d = t.app.diff.as_ref().unwrap();
    (0..d.rows(false)).find(|&i| row_kind(d, i) == want).unwrap_or_else(|| panic!("no {want} row"))
}

#[test]
fn the_index_mark_is_the_index_status_read_not_a_later_one() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    let mark = gitty_core::watch::IndexMark::new(&f.path().join(".git"));
    t.app.set_index_mark(mark.clone());
    t.app.request_status();
    let reqs = t.app.take_requests();
    let msgs = t.exec_all(reqs);
    // staged elsewhere after status read the index, before its reply is handled
    f.git(&["add", "-A"]);
    for m in msgs {
        t.app.handle_msg(m);
    }
    assert!(!mark.is_seen(), "the watcher must still report the external add");
}

#[test]
fn space_and_a_toggle_whole_files() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.ch(' ');
    assert_eq!(writes(t.app.take_requests_peek()), ["stage [\"a.txt\"]"]);
    t.pump();
    assert_eq!(t.app.changes.selected().unwrap().check(), gitty_core::status::Check::Staged);
    t.ch(' ');
    assert_eq!(writes(t.app.take_requests_peek()), ["unstage [\"a.txt\"]"]);
    t.pump();
    t.ch('a');
    assert_eq!(writes(t.app.take_requests_peek()), ["stage all"]);
    t.pump();
    t.ch('a');
    assert_eq!(writes(t.app.take_requests_peek()), ["unstage all"]);
}

#[test]
fn space_stages_the_line_under_the_cursor_and_range_and_hunk() {
    let base: Vec<String> = (1..=12).map(|i| i.to_string()).collect();
    let mut wt = base.clone();
    wt[1] = "TWO".into();
    wt.extend(["x13".to_string(), "x14".to_string()]);
    let text = |v: &[String]| v.iter().map(|l| format!("{l}\n")).collect::<String>();
    let f = Fixture::new();
    f.write("a.txt", text(&base));
    f.commit("base", 1_700_000_000);
    f.write("a.txt", text(&wt));
    let index = |f: &Fixture| format!("{}\n", index_of(f, "a.txt"));
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    assert_eq!(t.app.focus, Focus::Diff);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add1");
    t.ch(' ');
    assert_eq!(writes(t.app.take_requests_peek()), ["lines [false, true, false, false]"]);
    assert_eq!(t.app.changes.current.as_ref().unwrap().staged, Some(vec![false, true, false, false]), "marks update at once");
    t.pump();
    let mut want = base.clone();
    want.insert(2, "TWO".into());
    assert_eq!(index(&f), text(&want), "unstaged deletion stays, staged addition follows it");
    // v + j + space: both added lines at the end
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add12");
    t.ch('v');
    t.ch('j');
    t.ch(' ');
    t.pump();
    want.extend(["x13".to_string(), "x14".to_string()]);
    assert_eq!(index(&f), text(&want));
    // H on the first hunk stages its deletion too; H again unstages that hunk only
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "del1");
    t.ch('H');
    t.pump();
    assert_eq!(index(&f), text(&wt));
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "del1");
    t.ch('H');
    t.pump();
    let mut want = base.clone();
    want.extend(["x13".to_string(), "x14".to_string()]);
    assert_eq!(index(&f), text(&want));
}

#[test]
fn intent_to_add_files_stage_line_by_line() {
    let f = Fixture::new();
    f.write("base.txt", "b\n");
    f.commit("base", 1_700_000_000);
    f.write("f", "one\ntwo\n");
    f.git(&["add", "-N", "f"]);
    let mut t = changes_tab(&f);
    let i = t.app.changes.status.as_ref().unwrap().entries.iter().position(|e| e.path == "f").unwrap();
    t.app.select_change(i);
    t.pump();
    t.key(KeyCode::Enter);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add0");
    t.ch(' ');
    t.pump();
    assert!(t.app.toast.as_ref().is_none_or(|t| !t.error), "{:?}", t.app.toast.as_ref().map(|t| (&t.what, &t.detail)));
    assert_eq!(index_of(&f, "f"), "one");
}

#[test]
fn whitespace_hidden_or_divergent_files_refuse_line_staging() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    t.ch('w');
    t.pump();
    t.app.diff.as_mut().unwrap().cursor = 1;
    t.ch(' ');
    assert!(writes(t.app.take_requests_peek()).is_empty());
    assert!(t.app.toast.as_ref().unwrap().what.to_lowercase().contains("whitespace"), "{:?}", t.app.toast.as_ref().map(|t| &t.what));
}

/// One Trash directory for the whole test binary: the variable is process-wide and tests run in
/// parallel. Tests that count copies use file names no other test discards.
fn trash_dir() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        // SAFETY: set once, before any write in this binary reads it
        unsafe { std::env::set_var("GITTY_TRASH_DIR", d.path()) };
        d
    })
    .path()
}

#[test]
fn discard_asks_first_and_keeps_a_trash_copy() {
    let trash = trash_dir();
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n3\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "1\nTWO\n3\nfour\n");
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add3");
    t.ch('d');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { .. })));
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
    assert!(writes(t.app.take_requests_peek()).is_empty(), "cancelled");
    t.ch('d');
    t.key(KeyCode::Enter);
    assert_eq!(writes(t.app.take_requests_peek()), ["write a.txt"]);
    t.pump();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "1\nTWO\n3\n");
    let copies = std::fs::read_dir(trash).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("a.txt (discarded")).count();
    assert_eq!(copies, 1);
    // whole file from the file list
    t.key(KeyCode::Esc);
    assert_eq!(t.app.focus, Focus::Files);
    t.ch('d');
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "1\n2\n3\n");
    assert!(t.app.changes.entries().is_empty());
}

#[test]
fn filter_cycles_what_the_list_shows() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    assert_eq!(t.app.changes.visible().len(), 2);
    t.ch('F');
    assert_eq!(t.app.changes.filter, gitty::app::changes::Filter::Included);
    assert_eq!(t.app.changes.visible().len(), 0);
    t.ch('F');
    t.ch('F');
    assert_eq!(t.app.changes.filter, gitty::app::changes::Filter::New);
    t.pump();
    assert_eq!(t.app.changes.selected().unwrap().path, "new.txt");
    assert_eq!(t.app.diff.as_ref().unwrap().key.path, "new.txt");
}

// ---- commit box ----

fn typed(t: &mut H, s: &str) {
    for c in s.chars() {
        t.ch(c);
    }
}

fn commit_key(t: &mut H, m: KeyModifiers) {
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, m));
}

/// One modified, staged file in a repo with one commit ("base").
fn staged_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "a\nb\n");
    f.git(&["add", "a.txt"]);
    f
}

#[test]
fn commit_box_commits_with_trailers_and_undo_restores_the_message() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    t.ch('c');
    assert_eq!(t.app.focus, Focus::Commit);
    typed(&mut t, "Add b quickly");
    t.key(KeyCode::Tab);
    typed(&mut t, "Because b matters.");
    t.key(KeyCode::Tab);
    typed(&mut t, "Ann <ann@example.org>");
    commit_key(&mut t, KeyModifiers::ALT);
    t.pump();
    assert_eq!(f.git(&["log", "-1", "--format=%B"]), "Add b quickly\n\nBecause b matters.\n\nCo-authored-by: Ann <ann@example.org>");
    assert_eq!(t.app.changes.commit.summary.text(), "", "box cleared after a commit");
    assert!(t.app.commit_bar().is_some_and(|b| b.contains("Committed just now")), "{:?}", t.app.commit_bar());
    t.key(KeyCode::Esc);
    assert_eq!(t.app.focus, Focus::Files);
    t.ch('u');
    t.pump();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
    assert_eq!(f.git(&["diff", "--cached", "--name-only"]), "a.txt", "changes stay staged");
    assert_eq!(t.app.changes.commit.summary.text(), "Add b quickly");
    assert_eq!(t.app.changes.commit.body.text(), "Because b matters.");
    assert_eq!(t.app.changes.commit.coauthors.text(), "Ann <ann@example.org>");
    assert_eq!(t.app.commit_bar(), None);
}

#[test]
fn undo_is_not_offered_once_the_commit_is_pushed() {
    let f = staged_fixture();
    f.add_bare_upstream();
    let mut t = changes_tab(&f);
    t.ch('c');
    typed(&mut t, "Add b");
    commit_key(&mut t, KeyModifiers::ALT);
    t.pump();
    assert!(t.app.commit_bar().is_some());
    f.git(&["push", "-q"]);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::ALL));
    t.pump();
    assert_eq!(t.app.commit_bar(), None, "a pushed commit is not offered for undo");
    t.app.set_tab(gitty::app::Tab::Changes);
    t.key(KeyCode::Esc);
    t.ch('u');
    assert!(writes(t.app.take_requests_peek()).is_empty());
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Add b");
}

#[test]
fn typing_in_the_commit_box_does_not_trigger_shortcuts() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    t.ch('c');
    typed(&mut t, "q12?T a");
    assert!(!t.app.quit);
    assert_eq!(t.app.tab, gitty::app::Tab::Changes);
    assert!(t.app.overlay.is_none());
    assert_eq!(t.app.changes.commit.summary.text(), "q12?T a");
    assert!(writes(t.app.take_requests_peek()).is_empty());
}

#[test]
fn empty_summary_uses_the_placeholder_and_nothing_staged_refuses() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    assert_eq!(t.app.commit_placeholder(), "Update a.txt");
    t.ch('c');
    commit_key(&mut t, KeyModifiers::CONTROL);
    t.pump();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Update a.txt");
    t.ch('c');
    typed(&mut t, "nothing here");
    commit_key(&mut t, KeyModifiers::ALT);
    assert!(writes(t.app.take_requests_peek()).is_empty());
    assert!(t.app.toast.as_ref().is_some_and(|t| t.what.contains("Nothing staged")), "{:?}", t.app.toast);
}

#[test]
fn amend_loads_the_head_message_and_rewrites_it() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    t.ch('A');
    t.pump();
    assert!(t.app.changes.commit.amend);
    assert_eq!(t.app.changes.commit.summary.text(), "base");
    assert_eq!(t.app.commit_button(), "Amend last commit");
    t.ch('c');
    typed(&mut t, " v2");
    commit_key(&mut t, KeyModifiers::ALT);
    t.pump();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base v2");
    assert_eq!(f.git(&["rev-list", "--count", "HEAD"]), "1");
    assert!(!t.app.changes.commit.amend, "amend is one-shot");
}

#[test]
fn toggling_amend_off_restores_the_draft() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    t.ch('c');
    typed(&mut t, "my draft");
    t.key(KeyCode::Esc);
    t.ch('A');
    t.pump();
    assert_eq!(t.app.changes.commit.summary.text(), "base");
    t.ch('A');
    assert_eq!(t.app.changes.commit.summary.text(), "my draft");
    assert_eq!(t.app.commit_button(), "Commit 1 file to main");
}

#[test]
fn hook_failure_opens_a_modal_and_keeps_the_message() {
    let f = staged_fixture();
    let hook = f.path().join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint failed: bad.rs' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut t = changes_tab(&f);
    t.ch('c');
    typed(&mut t, "Will fail");
    commit_key(&mut t, KeyModifiers::ALT);
    t.pump();
    match &t.app.overlay {
        Some(gitty::app::Overlay::Log { title, body }) => {
            assert!(title.contains("Commit failed"), "{title}");
            assert!(body.contains("lint failed: bad.rs"), "{body}");
        }
        _ => panic!("expected the hook log modal"),
    }
    assert_eq!(t.app.changes.commit.summary.text(), "Will fail");
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "base");
}

#[test]
fn slow_status_refreshes_the_index_at_most_once_a_minute() {
    let f = staged_fixture();
    let mut t = changes_tab(&f);
    t.app.handle_msg(Msg::StatusSlow);
    assert_eq!(writes(t.app.take_requests_peek()), ["refresh index"]);
    t.pump();
    assert_eq!(t.app.changes.busy, 0, "a refresh does not show as work");
    t.app.handle_msg(Msg::StatusSlow);
    assert!(writes(t.app.take_requests_peek()).is_empty(), "throttled");
    t.app.tick(t.clock + Duration::from_secs(61));
    t.app.handle_msg(Msg::StatusSlow);
    assert_eq!(writes(t.app.take_requests_peek()), ["refresh index"]);
}

#[test]
fn discarding_a_staged_rename_restores_the_original() {
    let _ = trash_dir();
    let f = Fixture::new();
    f.write("old.txt", "one\ntwo\nthree\n");
    f.commit("base", 1_700_000_000);
    f.git(&["mv", "old.txt", "new.txt"]);
    f.write("new.txt", "one\ntwo\nthree\nfour\n");
    let mut t = changes_tab(&f);
    let entries = t.app.changes.entries().to_vec();
    let pos = t.app.changes.visible().iter().position(|&i| entries[i].path == "new.txt").unwrap();
    t.app.select_change(pos);
    t.ch('d');
    t.key(KeyCode::Enter);
    t.pump();
    assert!(t.app.toast.is_none(), "{:?}", t.app.toast);
    assert_eq!(std::fs::read_to_string(f.path().join("old.txt")).unwrap(), "one\ntwo\nthree\n");
    assert!(!f.path().join("new.txt").exists());
    assert_eq!(f.git(&["status", "--porcelain"]), "", "back to HEAD");
}

#[test]
fn quick_successive_line_toggles_all_apply() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "1\nX\n2\n3\n4\n5\n6\n7\n8\n9\nY\n10\nZ\n");
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add1");
    t.ch(' ');
    // the second toggle comes before the first write's status/diff round trip
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add10");
    t.ch(' ');
    t.pump();
    assert!(t.app.toast.as_ref().is_none_or(|t| !t.error), "{:?}", t.app.toast);
    assert_eq!(f.git(&["show", ":a.txt"]), "1\nX\n2\n3\n4\n5\n6\n7\n8\n9\nY\n10", "X and Y staged, Z not");
}

#[test]
fn a_discard_outside_the_trash_says_where_the_copies_are() {
    let f = changes_fixture();
    let mut t = changes_tab(&f);
    let note = "The Trash is not writable: copies of the discarded files are in /x/gitty/trash".to_string();
    let op = gitty::msg::WriteOp::DiscardFiles { restore: vec!["a.txt".into()], remove: vec![] };
    t.app.handle_msg(Msg::WriteDone { op, result: Ok(Some(note.clone())) });
    let toast = t.app.toast.as_ref().expect("a toast");
    assert!(!toast.error && toast.what == note, "{:?}", toast.what);
}

#[test]
fn discarding_a_staged_line_unstages_it_too() {
    let _ = trash_dir();
    let f = Fixture::new();
    f.write("b.txt", "1\n2\n3\n");
    f.commit("base", 1_700_000_000);
    f.write("b.txt", "1\n2\n3\nfour\n");
    f.git(&["add", "b.txt"]);
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add3");
    t.ch('d');
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(std::fs::read_to_string(f.path().join("b.txt")).unwrap(), "1\n2\n3\n");
    assert_eq!(f.git(&["status", "--porcelain"]), "", "the discarded line is not left staged");
}

#[test]
fn a_renamed_file_stages_and_discards_line_by_line() {
    let _ = trash_dir();
    let f = Fixture::new();
    f.write("old.txt", "1\n2\n3\n");
    f.commit("base", 1_700_000_000);
    f.git(&["mv", "old.txt", "new.txt"]);
    f.write("new.txt", "1\n2\n3\n4\n5\n");
    let mut t = changes_tab(&f);
    let i = t.app.changes.status.as_ref().unwrap().entries.iter().position(|e| e.path == "new.txt").unwrap();
    t.app.select_change(i);
    t.pump();
    t.key(KeyCode::Enter);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add3");
    t.ch(' ');
    t.pump();
    assert!(t.app.toast.as_ref().is_none_or(|t| !t.error), "{:?}", t.app.toast.as_ref().map(|t| &t.detail));
    assert_eq!(index_of(&f, "new.txt"), "1\n2\n3\n4");
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add4");
    t.ch('d');
    t.key(KeyCode::Enter);
    t.pump();
    assert!(t.app.toast.as_ref().is_none_or(|t| !t.error), "{:?}", t.app.toast.as_ref().map(|t| &t.detail));
    assert_eq!(std::fs::read_to_string(f.path().join("new.txt")).unwrap(), "1\n2\n3\n4\n");
}

#[test]
fn line_staging_refuses_after_head_moves_under_it() {
    let f = Fixture::new();
    f.write("a.txt", "1\n2\n3\n");
    f.commit("base", 1_700_000_000);
    f.write("a.txt", "1\n2\n3\n4\n5\n");
    f.git(&["add", "a.txt"]);
    let mut t = changes_tab(&f);
    t.key(KeyCode::Enter);
    // committed elsewhere: the index and the worktree are what gitty saw, HEAD:a.txt is not
    f.git(&["commit", "-qm", "elsewhere"]);
    t.app.diff.as_mut().unwrap().cursor = diff_row(&t, "add3");
    t.ch(' ');
    t.pump();
    let toast = t.app.toast.as_ref().map(|t| t.detail.clone()).unwrap_or_default();
    assert!(toast.contains("changed in HEAD"), "{toast:?}");
    assert_eq!(index_of(&f, "a.txt"), "1\n2\n3\n4\n5", "the index is left alone");
}

// ---- network ----

fn remote_fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    let bare = f.add_bare_upstream();
    (f, bare)
}

fn toast_text(t: &H) -> String {
    t.app.toast.as_ref().map(|t| format!("{} | {}", t.what, t.detail)).unwrap_or_default()
}

#[test]
fn an_unpublished_branch_marks_what_a_push_would_publish() {
    let (f, _bare) = remote_fixture();
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("x.txt", "x\n");
    let a1 = CommitId::from_hex(&f.commit("local 1", 1_700_000_100)).unwrap();
    f.write("y.txt", "y\n");
    let a2 = CommitId::from_hex(&f.commit("local 2", 1_700_000_200)).unwrap();
    let mut t = H::new(&f);
    t.pump();
    assert!(t.app.refs.as_ref().unwrap().upstream.is_none(), "never pushed");
    assert_eq!(t.app.ahead, [a1, a2].into(), "the commits no remote branch has");
    assert!(t.app.behind.is_empty());
    t.ch('P');
    t.pump();
    assert!(t.app.refs.as_ref().unwrap().upstream.is_some(), "P published it");
    assert!(t.app.ahead.is_empty() && t.app.behind.is_empty(), "nothing left to push");
}

#[test]
fn a_repository_without_remote_branches_marks_nothing() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.git(&["remote", "add", "origin", "https://example.invalid/never-fetched.git"]);
    let mut t = H::new(&f);
    t.pump();
    assert!(t.app.ahead.is_empty(), "nothing to compare with yet");
}

#[test]
fn losing_the_upstream_drops_its_marks() {
    let (f, _bare) = remote_fixture();
    f.write("x.txt", "x\n");
    f.commit("local", 1_700_000_100);
    f.git(&["branch", "-q", "--set-upstream-to", "origin/main"]);
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.ahead.len(), 1);
    // the upstream goes, and the remote branch with it: nothing to compare with
    f.git(&["branch", "-q", "--unset-upstream"]);
    f.git(&["branch", "-q", "-r", "-d", "origin/main"]);
    for m in t.exec_all(vec![Request::Refs]) {
        t.app.handle_msg(m);
    }
    t.pump();
    assert!(t.app.refs.as_ref().unwrap().upstream.is_none());
    assert!(t.app.ahead.is_empty(), "no stale ↑ marks");
}

#[test]
fn f_fetches_and_p_fast_forwards() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "b.txt");
    let mut t = H::new(&f);
    t.pump();
    t.ch('f');
    t.pump();
    assert_eq!(f.git(&["rev-list", "--count", "HEAD..origin/main"]), "1");
    assert!(toast_text(&t).contains("Fetched"), "{}", toast_text(&t));
    assert_eq!(t.app.behind.len(), 1, "refs and ahead/behind refreshed");
    assert!(t.app.net.is_none());
    t.ch('p');
    t.pump();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), f.git(&["rev-parse", "origin/main"]));
    assert!(f.path().join("b.txt").exists());
}

#[test]
fn diverged_pull_offers_merge_or_rebase() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "b.txt");
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('p');
    t.pump();
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Diverged)), "{}", toast_text(&t));
    t.ch('r');
    t.pump();
    assert_eq!(f.git(&["rev-list", "--count", "HEAD..origin/main"]), "0");
    assert_eq!(f.git(&["rev-list", "--count", "origin/main..HEAD"]), "1");
    assert!(t.app.overlay.is_none());
}

#[test]
fn shift_p_publishes_a_new_branch() {
    let (f, _bare) = remote_fixture();
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('P');
    t.pump();
    assert_eq!(f.git(&["config", "branch.topic.merge"]), "refs/heads/topic");
    assert!(toast_text(&t).contains("Pushed"), "{}", toast_text(&t));
}

#[test]
fn a_fetch_first_rejection_says_to_fetch_and_offers_no_force_push() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "b.txt");
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('P');
    t.pump();
    let toast = t.app.toast.clone().unwrap();
    assert!(toast.error && toast.what == "The remote has new commits. Fetch first (f)", "{toast:?}");
    assert!(t.app.overlay.is_none());
}

#[test]
fn one_job_at_a_time_and_x_cancels() {
    let (f, _) = remote_fixture();
    f.script_remote("hang", "sleep 30");
    f.git(&["config", "branch.main.remote", "hang"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('f');
    let reqs: Vec<Request> = t.app.take_requests().into_iter().filter(|r| matches!(r, Request::Net { .. })).collect();
    assert_eq!(reqs.len(), 1);
    let (tx, rx) = std::sync::mpsc::channel();
    let (path, gens) = (f.path(), t.gens.clone());
    let req = reqs.into_iter().next().unwrap();
    std::thread::spawn(move || {
        let h = gitty_core::Repo::open(&path).unwrap().handle();
        exec(&h, req, &mut |m| {
            let _ = tx.send(m);
        }, &gens)
    });
    let started = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(started, Msg::NetStarted { .. }), "{started:?}");
    t.app.handle_msg(started);
    assert!(t.app.net_bar().is_some_and(|b| b.contains("x cancel")), "{:?}", t.app.net_bar());
    t.ch('p');
    assert!(toast_text(&t).contains("Wait for"), "{}", toast_text(&t));
    assert!(t.app.take_requests().iter().all(|r| !matches!(r, Request::Net { .. })));
    t.ch('x');
    let done = loop {
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            m @ Msg::NetDone { .. } => break m,
            m => t.app.handle_msg(m),
        }
    };
    t.app.handle_msg(done);
    assert!(t.app.net.is_none());
    assert!(toast_text(&t).contains("cancelled"), "{}", toast_text(&t));
    assert!(!t.app.toast.as_ref().unwrap().error, "cancel is not an error");
}

#[test]
fn prompt_overlay_relays_the_answer_and_esc_cancels() {
    let (tx, rx) = std::sync::mpsc::channel();
    let server = gitty::askpass::AskServer::start(move |a| {
        let _ = tx.send(a);
    })
    .unwrap();
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    let mut t = H::new(&f);
    t.pump();
    t.app.ask_handle = Some(server.handle());
    let helper = |prompt: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_gitty")).arg(prompt).env("GITTY_ASKPASS_SOCK", server.socket()).stdout(std::process::Stdio::piped()).spawn().unwrap()
    };
    let child = helper("Password for 'https://ann@example.com': ");
    t.app.handle_msg(Msg::Ask(rx.recv_timeout(Duration::from_secs(5)).unwrap()));
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Prompt { .. })));
    for c in "hunter2q".chars() {
        t.ch(c);
    }
    t.key(KeyCode::Backspace);
    assert!(!t.app.quit, "typing q into a prompt does not quit");
    t.key(KeyCode::Enter);
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "hunter2\n");
    assert!(t.app.overlay.is_none());
    let child = helper("Username for 'https://example.com': ");
    t.app.handle_msg(Msg::Ask(rx.recv_timeout(Duration::from_secs(5)).unwrap()));
    t.key(KeyCode::Esc);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1));
}

// ---- auto-fetch ----

fn net_requests(t: &mut H) -> Vec<Request> {
    t.app.take_requests().into_iter().filter(|r| matches!(r, Request::Net { .. })).collect()
}

/// Runs only the network requests (with their follow-ups' messages), never the clock.
fn run_net(t: &mut H, reqs: Vec<Request>) {
    for m in t.exec_all(reqs) {
        t.app.handle_msg(m);
    }
    t.app.focused = false;
    t.pump();
    t.app.focused = true;
}

const FIVE_MIN: Duration = Duration::from_secs(300);

#[test]
fn auto_fetch_waits_for_focus_and_idleness() {
    let (f, _) = remote_fixture();
    let mut t = H::new(&f);
    t.pump();
    assert_eq!(t.app.auto_fetch_deadline(), None, "unfocused");
    t.app.focused = true;
    let d = t.app.auto_fetch_deadline().expect("focused, idle, with an upstream");
    assert!(d >= t.clock + FIVE_MIN - Duration::from_secs(1) && d <= t.clock + FIVE_MIN + Duration::from_secs(1));
    t.app.config.auto_fetch_minutes = 0;
    assert_eq!(t.app.auto_fetch_deadline(), None, "disabled");
    t.app.config.auto_fetch_minutes = 5;
    t.app.start_net(gitty::msg::NetOp::Push);
    assert_eq!(t.app.auto_fetch_deadline(), None, "a job is running");
}

#[test]
fn auto_fetch_runs_quietly_and_keeps_the_selection() {
    let (f, bare) = remote_fixture();
    f.write("b.txt", "b\n");
    f.commit("second", 1_700_000_100);
    f.git(&["push", "-q"]);
    common::push_as_someone_else(&bare, "c.txt");
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    let selected = t.selected_id();
    t.app.focused = true;
    t.app.tick(t.clock + FIVE_MIN + Duration::from_secs(1));
    let reqs = net_requests(&mut t);
    assert!(matches!(reqs.as_slice(), [Request::Net { background: true, .. }]), "{} requests", reqs.len());
    assert_eq!(t.app.net_bar().as_deref(), Some("fetching…"));
    run_net(&mut t, reqs);
    assert_eq!(f.git(&["rev-list", "--count", "HEAD..origin/main"]), "1");
    assert!(t.app.toast.is_none(), "{:?}", t.app.toast);
    assert_eq!(t.selected_id(), selected);
    assert_eq!(t.app.focus, Focus::History);
}

#[test]
fn needs_auth_stops_auto_fetch_until_a_manual_fetch_works() {
    let (f, bare) = remote_fixture();
    f.script_remote("locked", "echo \"fatal: could not read Username for 'https://example.com': terminal prompts disabled\" >&2; exit 128");
    let locked = f.git(&["remote", "get-url", "locked"]);
    f.git(&["remote", "set-url", "origin", &locked]);
    let mut t = H::new(&f);
    t.pump();
    t.app.focused = true;
    t.app.tick(t.clock + FIVE_MIN + Duration::from_secs(1));
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    assert_eq!(t.app.needs_auth.as_deref(), Some("origin"));
    assert!(t.app.toast.is_none(), "quiet: the top bar says it");
    assert_eq!(t.app.auto_fetch_deadline(), None);
    f.git(&["remote", "set-url", "origin", bare.to_str().unwrap()]);
    t.ch('f');
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    assert_eq!(t.app.needs_auth, None);
    assert!(t.app.auto_fetch_deadline().is_some());
}

// ---- auto-tuning ----

fn many_commits_fixture() -> Fixture {
    let f = Fixture::new();
    for i in 0..3 {
        f.write("a.txt", format!("{i}\n"));
        f.commit(&format!("c{i}"), 1_700_000_000 + i);
    }
    f
}

#[test]
fn large_history_gets_a_commit_graph_and_one_notice() {
    let f = many_commits_fixture();
    let mut t = H::new(&f);
    // only the commit-graph: fsmonitor would start a daemon for this temp repo
    t.app.tune_thresholds = gitty_core::tune::Thresholds { commits: 2, index_entries: usize::MAX };
    t.pump();
    let graph = f.path().join(".git/objects/info");
    assert!(graph.join("commit-graph").exists() || graph.join("commit-graphs").exists());
    assert!(toast_text(&t).contains("commit-graph") && toast_text(&t).contains("gitty untune"), "{}", toast_text(&t));
}

#[test]
fn auto_tune_off_changes_nothing() {
    let f = many_commits_fixture();
    let mut t = H::new(&f);
    t.app.config.auto_tune = false;
    t.app.tune_thresholds = gitty_core::tune::Thresholds { commits: 2, index_entries: usize::MAX };
    t.pump();
    assert!(!f.path().join(".git/objects/info/commit-graph").exists());
    assert!(t.app.toast.is_none());
}

#[test]
fn untune_subcommand_reverts_what_gitty_set() {
    let f = many_commits_fixture();
    f.git(&["config", "core.untrackedCache", "true"]);
    f.git(&["config", "--add", "gitty.tuned", "core.untrackedCache"]);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_gitty")).args(["untune", f.path().to_str().unwrap()]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("core.untrackedCache"));
    assert!(std::process::Command::new("git").current_dir(f.path()).args(["config", "core.untrackedCache"]).output().unwrap().stdout.is_empty());
    let again = std::process::Command::new(env!("CARGO_BIN_EXE_gitty")).args(["untune", f.path().to_str().unwrap()]).output().unwrap();
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to undo"));
}

#[test]
fn a_conflicting_merge_says_what_to_do() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "a.txt");
    f.write("a.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('p');
    t.pump();
    t.ch('m');
    t.pump();
    let toast = t.app.toast.clone().unwrap();
    assert!(toast.what.contains("conflict") && toast.what.contains("git merge --abort"), "{toast:?}");
    assert!(toast.detail.contains("CONFLICT"), "{toast:?}");
}

/// 12 commits; i % 3 == 0 are "Fix thing i" (history indices 2, 5, 8, 11), the rest "commit i".
fn search_fixture() -> Fixture {
    let f = Fixture::new();
    for i in 0..12 {
        f.write("a.txt", format!("line {i}\n"));
        let msg = if i % 3 == 0 { format!("Fix thing {i}") } else { format!("commit {i}") };
        f.commit(&msg, 1_700_000_000 + i * 100);
    }
    f
}

fn search(t: &mut H, q: &str) {
    t.ch('/');
    typed(t, q);
    t.key(KeyCode::Enter);
}

fn hits(t: &H) -> Vec<usize> {
    t.app.search.hits.iter().copied().collect()
}

#[test]
fn search_jumps_to_the_first_match_at_or_after_the_selection_and_n_wraps() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(3);
    t.pump();
    search(&mut t, "fix");
    t.pump();
    assert_eq!(hits(&t), [2, 5, 8, 11]);
    assert_eq!(t.app.selected, 5);
    t.ch('n');
    assert_eq!(t.app.selected, 8);
    t.ch('n');
    t.ch('n');
    assert_eq!(t.app.selected, 2, "n wraps to the first match");
    t.ch('N');
    assert_eq!(t.app.selected, 11, "N wraps to the last match");
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  4/4"));
}

#[test]
fn the_label_rank_follows_moves_and_late_matches() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    search(&mut t, "fix");
    t.pump();
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  1/4"));
    t.ch('n');
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  2/4"));
    t.ch('j');
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  -/4"));
    t.ch('k');
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  2/4"));
    // a match that arrives before the selection moves its rank
    let generation = t.app.search.generation;
    t.app.handle_msg(Msg::SearchHits { generation, range: 0..1, hits: vec![0] });
    assert_eq!(t.app.search_label().as_deref(), Some("/fix  3/5"));
}

#[test]
fn search_chunks_merge_in_any_order_and_a_stale_generation_is_dropped() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.search_chunk = 5;
    search(&mut t, "fix");
    let reqs = t.app.take_requests();
    let ranges: Vec<_> = reqs.iter().filter_map(|r| match r {
        Request::Search { range, .. } => Some(range.clone()),
        _ => None,
    }).collect();
    assert_eq!(ranges, [0..5, 5..10, 10..12]);
    // the last chunk answers first: the selection jumps, then moves back as earlier hits arrive
    for r in reqs.into_iter().rev() {
        for m in t.exec_all(vec![r]) {
            t.app.handle_msg(m);
        }
        assert!(t.app.search.hits.contains(&t.app.selected));
    }
    assert_eq!(hits(&t), [2, 5, 8, 11]);
    assert_eq!(t.app.selected, 2);

    search(&mut t, "commit");
    let reqs = t.app.take_requests();
    let old = t.exec_all(reqs);
    search(&mut t, "thing 9");
    for m in old {
        t.app.handle_msg(m);
    }
    assert!(hits(&t).is_empty(), "hits of a replaced query are dropped");
    t.pump();
    assert_eq!(hits(&t), [2]);
}

#[test]
fn the_search_bar_closes_on_a_tab_switch_or_a_click() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('/');
    typed(&mut t, "fi");
    t.app.set_tab(gitty::app::Tab::Changes);
    assert!(t.app.search.bar.is_none(), "the tab switch closes the bar");
    t.ch('2');
    assert_eq!(t.app.tab, gitty::app::Tab::History, "keys act again");
    t.ch('/');
    let m = |kind| crossterm::event::MouseEvent { kind, column: 5, row: 5, modifiers: KeyModifiers::NONE };
    t.app.handle_mouse(m(crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)));
    assert!(t.app.search.bar.is_none(), "a click closes the bar");
}

#[test]
fn search_bar_takes_every_key_and_esc_clears() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('/');
    typed(&mut t, "q1n?");
    assert!(!t.app.quit && t.app.overlay.is_none());
    assert_eq!(t.app.search.bar.as_ref().map(|b| b.text()), Some("q1n?"));
    t.key(KeyCode::Esc);
    assert!(t.app.search.bar.is_none() && t.app.search_label().is_none());

    search(&mut t, "fix");
    t.pump();
    assert_eq!(hits(&t).len(), 4);
    t.key(KeyCode::Esc);
    assert!(hits(&t).is_empty() && t.app.search_label().is_none(), "Esc clears an active search");
    let before = t.app.selected;
    t.ch('n');
    assert_eq!(t.app.selected, before);
}

#[test]
fn path_search_keeps_commits_touching_the_path() {
    let f = Fixture::new();
    commits(&f, 6); // dir/f0, f2, f4 → history indices 5, 3, 1
    let mut t = H::new(&f);
    t.pump();
    search(&mut t, "path:dir");
    t.pump();
    assert_eq!(hits(&t), [1, 3, 5]);
    search(&mut t, "commit 4 path:dir");
    t.pump();
    assert_eq!(hits(&t), [1]);
    search(&mut t, "path:nowhere");
    t.pump();
    assert!(hits(&t).is_empty());
    assert_eq!(t.app.search_label().as_deref(), Some("/path:nowhere  no matches"));
}

/// c0: a=1 · c1: a=2, b · c2: a=3 · c3: c (history indices 3, 2, 1, 0).
fn range_fixture() -> (Fixture, Vec<String>) {
    let f = Fixture::new();
    let mut ids = Vec::new();
    f.write("a.txt", "1\n");
    ids.push(f.commit("c0", 1_700_000_000));
    f.write("a.txt", "2\n");
    f.write("b.txt", "b\n");
    ids.push(f.commit("c1", 1_700_000_100));
    f.write("a.txt", "3\n");
    ids.push(f.commit("c2", 1_700_000_200));
    f.write("c.txt", "c\n");
    ids.push(f.commit("c3", 1_700_000_300));
    (f, ids)
}

fn file_paths(t: &H) -> Vec<String> {
    let mut v: Vec<String> = t.app.files.as_ref().unwrap().iter().map(|f| f.path.clone()).collect();
    v.sort();
    v
}

fn git_names(f: &Fixture, a: &str, b: &str) -> Vec<String> {
    let mut v: Vec<String> = f.git(&["diff", "--name-only", a, b]).lines().map(str::to_string).collect();
    v.sort();
    v
}

#[test]
fn range_lists_the_union_of_files_with_one_combined_diff_each() {
    let (f, ids) = range_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(0);
    t.ch('V');
    t.ch('j');
    t.ch('j');
    t.pump();
    assert_eq!(t.app.selected_range(), Some((2, 0)));
    assert_eq!(t.app.files_of(), Some(FilesOf::Range { oldest: id(&ids[1]), newest: id(&ids[3]) }));
    assert_eq!(file_paths(&t), ["a.txt", "b.txt", "c.txt"]);
    let i = t.app.files.as_ref().unwrap().iter().position(|f| f.path == "a.txt").unwrap();
    t.app.select_file(i);
    t.pump();
    let d = &t.app.diff.as_ref().unwrap().diff;
    assert_eq!((d.old.bytes(), d.new.bytes()), (&b"1\n"[..], &b"3\n"[..]), "a file changed twice shows one diff");
    assert_eq!(t.app.focus, Focus::History);
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.app.selected_range(), None, "Esc ends the range");
    assert_eq!(t.app.files_for(), Some(id(&ids[1])));
}

#[test]
fn range_from_the_root_commit_diffs_against_the_empty_tree() {
    let (f, ids) = range_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    t.ch('V');
    t.ch('G');
    t.pump();
    assert_eq!(t.app.files_of(), Some(FilesOf::Range { oldest: id(&ids[0]), newest: id(&ids[2]) }));
    assert_eq!(file_paths(&t), ["a.txt", "b.txt"]);
    t.ch('V');
    t.pump();
    assert_eq!(t.app.selected_range(), None, "V again ends the range");
}

#[test]
fn range_across_a_merge_matches_git_diff() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-qb", "feature"]);
    f.write("f.txt", "f\n");
    f.commit("feature work", 1_700_000_100);
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("main work", 1_700_000_200);
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge feature", "feature"], &[("GIT_AUTHOR_DATE", "1700000300 +0000".into()), ("GIT_COMMITTER_DATE", "1700000300 +0000".into())]);
    let mut t = H::new(&f);
    t.pump();
    t.app.select(0);
    t.ch('V');
    t.ch('j');
    t.pump();
    let Some(FilesOf::Range { oldest, newest }) = t.app.files_of() else { panic!("{:?}", t.app.files_of()) };
    assert_eq!(file_paths(&t), git_names(&f, &format!("{oldest}^"), &newest.to_string()));
}

#[test]
fn a_stale_range_file_list_is_dropped_after_the_selection_moves() {
    let (f, ids) = range_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(0);
    t.pump();
    t.ch('V');
    t.ch('j');
    let first = t.app.take_requests();
    assert!(first.iter().any(|r| matches!(r, Request::Files { of: FilesOf::Range { .. }, prefetch: false, .. })));
    t.ch('j');
    let msgs = t.exec_all(first);
    for m in msgs {
        t.app.handle_msg(m);
    }
    assert_ne!(t.app.files_of(), Some(FilesOf::Range { oldest: id(&ids[2]), newest: id(&ids[3]) }), "old range not installed");
    t.pump();
    assert_eq!(t.app.files_of(), Some(FilesOf::Range { oldest: id(&ids[1]), newest: id(&ids[3]) }));

    // ending the range does not move the selection: only the list identity tells the replies apart
    let mut t = H::new(&f);
    t.pump();
    t.app.select(0);
    t.pump();
    t.ch('V');
    t.ch('j');
    let reqs = t.app.take_requests();
    t.ch('V');
    for m in t.exec_all(reqs) {
        t.app.handle_msg(m);
    }
    assert!(!matches!(t.app.files_of(), Some(FilesOf::Range { .. })), "a range list arriving after the range ended is dropped");
    t.pump();
    assert_eq!(t.app.files_for(), Some(id(&ids[2])));
}

#[test]
fn fuzzy_branch_ranking() {
    use gitty::app::compare::fuzzy_rank;
    let names: Vec<String> = ["feature", "fix-tests", "main", "origin/feature"].map(String::from).to_vec();
    let rank = |q: &str| fuzzy_rank(q, &names).into_iter().map(|i| names[i].as_str()).collect::<Vec<_>>();
    assert_eq!(rank("ft"), ["fix-tests", "feature", "origin/feature"], "word starts beat scattered letters; ties by name");
    assert_eq!(rank("FT"), rank("ft"), "case-insensitive");
    assert_eq!(rank("feat"), ["feature", "origin/feature"]);
    assert_eq!(rank("mn"), ["main"]);
    assert_eq!(rank(""), ["feature", "fix-tests", "main", "origin/feature"]);
    assert!(rank("zz").is_empty());
}

/// main: base, m1, m2 · feature (from base): f1, f2, f3 and a merge of m1. main checked out.
fn compare_fixture() -> Fixture {
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
fn compare_flow_picks_a_branch_switches_tabs_and_esc_restores() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    t.pump();
    let before = (t.app.selected, t.selected_id());
    t.ch('b');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::BranchPicker { .. })));
    typed(&mut t, "fea");
    t.key(KeyCode::Enter);
    t.pump();
    let tip = id(&f.git(&["rev-parse", "feature"]));
    let head = id(&f.git(&["rev-parse", "main"]));
    let mb = id(&f.git(&["merge-base", "main", "feature"]));
    {
        let c = t.app.compare.as_ref().expect("compare mode");
        assert_eq!(c.other, "feature");
        let r = c.result.as_ref().unwrap();
        assert_eq!((r.behind.len(), r.ahead.len()), (4, 1));
    }
    assert_eq!(t.selected_id(), tip, "Behind tab selects the branch's newest commit");
    assert_eq!(t.app.files_for(), Some(tip));
    t.ch('j');
    t.pump();
    assert_eq!(t.app.files_for(), Some(id(&f.git(&["rev-parse", "feature~1"]))), "j moves in the compare list");
    t.ch('l');
    t.pump();
    assert_eq!(t.selected_id(), head, "Ahead tab");
    t.ch('l');
    t.pump();
    assert_eq!(t.app.files_of(), Some(FilesOf::Between { from: Some(mb), to: tip }));
    assert_eq!(file_paths(&t), git_names(&f, &mb.to_string(), "feature"));
    t.ch('l');
    t.ch('h');
    t.pump();
    assert_eq!(t.selected_id(), head, "h goes back to Ahead");
    t.key(KeyCode::Esc);
    t.pump();
    assert!(t.app.compare.is_none());
    assert_eq!((t.app.selected, t.selected_id()), before, "Esc restores the history selection");
    assert_eq!(t.app.files_for(), Some(before.1));
}

#[test]
fn branch_picker_takes_letters_as_query_and_esc_cancels() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('b');
    typed(&mut t, "jkq");
    assert!(!t.app.quit);
    match &t.app.overlay {
        Some(gitty::app::Overlay::BranchPicker { query, .. }) => assert_eq!(query.text(), "jkq"),
        _ => panic!("picker closed"),
    }
    t.key(KeyCode::Enter);
    assert!(t.app.compare.is_none(), "Enter with no match does nothing");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none() && t.app.compare.is_none());
}

#[test]
fn leaving_compare_restores_the_range_the_search_and_the_file() {
    let f = compare_fixture();
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("two files", 1_700_002_000);
    let mut t = H::new(&f);
    t.pump();
    // the file: second file of the newest commit
    t.app.select_file(1);
    t.pump();
    assert_eq!(t.app.current_file().map(|f| f.path.as_str()), Some("b.txt"));
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.app.current_file().map(|f| f.path.as_str()), Some("b.txt"), "file restored");
    // the range
    t.ch('V');
    t.ch('j');
    t.pump();
    let range = t.app.selected_range();
    assert!(range.is_some());
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.app.selected_range(), range, "range restored");
    t.key(KeyCode::Esc);
    t.pump();
    // the search
    t.ch('/');
    typed(&mut t, "m");
    t.key(KeyCode::Enter);
    t.pump();
    let (label, sel) = (t.app.search_label(), t.app.selected);
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    t.key(KeyCode::Esc);
    t.pump();
    assert!(t.app.search_active(), "search restored");
    assert_eq!((t.app.search_label(), t.app.selected), (label, sel));
}

#[test]
fn a_range_saved_before_compare_survives_a_refresh_during_it() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('V');
    t.ch('j');
    t.pump();
    let ends = |t: &H| t.app.selected_range().map(|(o, n)| (t.app.history_id_at(o), t.app.history_id_at(n)));
    let before = ends(&t);
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    f.write("new.txt", "n\n");
    f.commit("new on main", 1_700_002_000);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    t.pump();
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(ends(&t), before, "the same two commits, at their new rows");
}

#[test]
fn a_file_restore_never_lands_on_a_later_list() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["branch", "old"]);
    f.write("a.txt", "a\n");
    f.write("b.txt", "b\n");
    f.commit("two files", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.app.select_file(1);
    t.pump();
    // nothing behind `old`: compare opens on Ahead, which shows the same HEAD commit
    t.ch('b');
    typed(&mut t, "old");
    t.key(KeyCode::Enter);
    t.pump();
    t.app.select_file(0);
    t.pump();
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.app.current_file().map(|f| f.path.as_str()), Some("b.txt"), "restored at once");
    t.app.select_file(0);
    t.pump();
    t.ch('V');
    t.ch('j');
    t.pump();
    assert_eq!(t.app.current_file().map(|f| f.path.as_str()), Some("a.txt"), "the range list starts on its first file");
}

#[test]
fn leaving_compare_mid_walk_still_finds_the_saved_commit() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    t.pump();
    let before = t.selected_id();
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    f.write("new.txt", "n\n");
    f.commit("new on main", 1_700_002_000);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    // the refs arrive and a new walk starts, but no rows yet
    let refs: Vec<_> = t.app.take_requests().into_iter().filter(|r| matches!(r, Request::Refs)).collect();
    for m in t.exec_all(refs) {
        t.app.handle_msg(m);
    }
    assert!(t.app.take_requests_peek().iter().any(|r| matches!(r, Request::Walk { .. })), "a new walk is queued");
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.selected_id(), before, "the saved commit, found by the new walk");
}

#[test]
fn leaving_compare_mid_walk_shows_row_0_when_the_saved_commit_is_gone() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    t.pump();
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    // the saved commit (m1) leaves the history while compare is open
    f.git(&["reset", "-q", "--hard", "main~2"]);
    let base = f.git(&["rev-parse", "HEAD"]);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::REFS));
    let refs: Vec<_> = t.app.take_requests().into_iter().filter(|r| matches!(r, Request::Refs)).collect();
    for m in t.exec_all(refs) {
        t.app.handle_msg(m);
    }
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.selected_id().to_string(), base.trim(), "the walk ended without it: row 0 is shown");
    assert_eq!(t.app.detail.as_ref().map(|d| d.row.id), Some(t.selected_id()), "and its detail, not the compare commit's");
}

#[test]
fn a_click_on_the_open_search_bar_keeps_it() {
    let f = search_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('/');
    typed(&mut t, "fi");
    t.app.hits.panes = t.app.panes();
    let row = t.app.hits.panes.bottom.y;
    let m = crossterm::event::MouseEvent { kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left), column: 3, row, modifiers: KeyModifiers::NONE };
    t.app.handle_mouse(m);
    assert_eq!(t.app.search.bar.as_ref().map(|b| b.text()), Some("fi"), "the bar and its text stay");
}

#[test]
fn a_range_says_when_it_covers_commits_outside_the_selected_rows() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("s.txt", "1\n");
    f.commit("s1", 1_700_000_100);
    f.write("s.txt", "2\n");
    f.commit("s2", 1_700_000_200);
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "1\n");
    f.commit("m1", 1_700_000_300);
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge side", "side"], &[("GIT_AUTHOR_DATE", "1700000400 +0000".into()), ("GIT_COMMITTER_DATE", "1700000400 +0000".into())]);
    let mut t = H::new(&f);
    t.pump();
    let summary = |t: &H, i: usize| t.app.rows.get(&i).map(|r| r.summary.clone()).unwrap_or_default();
    assert_eq!((summary(&t, 0), summary(&t, 1)), ("merge side".into(), "m1".into()));
    // merge + m1: m1^..merge also holds s1 and s2
    t.ch('V');
    t.ch('j');
    t.pump();
    assert_eq!(t.app.range_extra(), Some(2));
    t.key(KeyCode::Esc);
    t.pump();
    // s2 + s1: linear, nothing extra
    t.app.select(2);
    t.pump();
    t.ch('V');
    t.ch('j');
    t.pump();
    assert_eq!((summary(&t, 2), summary(&t, 3)), ("s2".into(), "s1".into()));
    assert_eq!(t.app.range_extra(), None);
}

#[test]
fn a_range_ending_at_a_merge_counts_the_merged_side() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("s.txt", "1\n");
    f.commit("s1", 1_700_000_100);
    f.write("s.txt", "2\n");
    f.commit("s2", 1_700_000_200);
    f.git(&["checkout", "-q", "main"]);
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge side", "side"], &[("GIT_AUTHOR_DATE", "1700000400 +0000".into()), ("GIT_COMMITTER_DATE", "1700000400 +0000".into())]);
    f.write("a.txt", "1\n");
    f.commit("after", 1_700_000_500);
    let mut t = H::new(&f);
    t.pump();
    let summary = |t: &H, i: usize| t.app.rows.get(&i).map(|r| r.summary.clone()).unwrap_or_default();
    assert_eq!((summary(&t, 0), summary(&t, 1)), ("after".into(), "merge side".into()));
    // after + merge: merge^..after diffs against base, so it also holds s1 and s2
    t.ch('V');
    t.ch('j');
    t.pump();
    assert_eq!(t.app.range_extra(), Some(2));
}

#[test]
fn a_range_note_counts_commits_not_rows_in_all_refs_scope() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", 1_700_000_000);
    f.git(&["checkout", "-q", "-b", "side"]);
    f.write("s.txt", "1\n");
    f.commit("s1", 1_700_000_100);
    f.write("s.txt", "2\n");
    f.commit("s2", 1_700_000_200);
    f.git(&["checkout", "-q", "main"]);
    f.write("m.txt", "1\n");
    f.commit("m1", 1_700_000_300);
    f.git(&["checkout", "-q", "-b", "other", "main~1"]);
    f.write("o.txt", "1\n");
    f.commit("o1", 1_700_000_350);
    f.git(&["checkout", "-q", "main"]);
    f.git_env(&["merge", "-q", "--no-ff", "-m", "merge side", "side"], &[("GIT_AUTHOR_DATE", "1700000400 +0000".into()), ("GIT_COMMITTER_DATE", "1700000400 +0000".into())]);
    let mut t = H::new(&f);
    t.pump();
    t.app.toggle_scope();
    t.pump();
    let summary = |t: &H, i: usize| t.app.rows.get(&i).map(|r| r.summary.clone()).unwrap_or_default();
    let top: Vec<String> = (0..3).map(|i| summary(&t, i)).collect();
    assert_eq!(top, ["merge side", "o1", "m1"]);
    // merge, o1, m1 selected: the diff m1^..merge holds merge, m1, s1 and s2, not o1
    t.ch('V');
    t.ch('j');
    t.ch('j');
    t.pump();
    assert_eq!(t.app.range_extra(), Some(2));
}

#[test]
fn a_history_refresh_during_compare_keeps_both_selections() {
    let f = compare_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.select(1);
    t.pump();
    let before = t.selected_id();
    t.ch('b');
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    let shown = t.selected_id();
    f.write("new.txt", "n\n");
    f.commit("new on main", 1_700_002_000);
    let msgs = t.exec_all(vec![Request::Refs]);
    for m in msgs {
        t.app.handle_msg(m);
    }
    t.pump();
    assert_eq!(t.selected_id(), shown, "the walk does not steal the compare selection");
    t.key(KeyCode::Esc);
    t.pump();
    assert_eq!(t.selected_id(), before, "the same commit is selected again, at its new row");
    assert_eq!(t.app.selected, 2);
}

#[test]
fn a_list_hidden_under_a_folded_directory_starts_on_that_directory() {
    let f = Fixture::new();
    f.write("src/a.rs", "a\n");
    f.commit("one", 1_700_000_000);
    f.write("src/b.rs", "b\n");
    f.commit("two", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('t');
    t.pump();
    t.app.focus = Focus::Files;
    t.app.select_file_row(0);
    assert!(t.app.toggle_dir(), "src/ folds");
    t.app.select(1);
    t.pump();
    assert_eq!(t.app.file_rows().len(), 1, "only the folded src/ row");
    t.key(KeyCode::Enter);
    assert_eq!(t.app.focus, Focus::Files, "Enter on the directory unfolds it instead of opening a hidden file");
    assert_eq!(t.app.file_rows().len(), 2);
}

fn tree_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("seed", "s\n");
    f.commit("seed", 1_700_000_000);
    for p in ["a.txt", "src/ui/x.rs", "src/ui/y.rs", "src/main.rs", "z.txt"] {
        f.write(p, "x\n");
    }
    f.commit("tree", 1_700_000_100);
    f
}

fn rows_text(t: &H) -> Vec<String> {
    use gitty::app::tree::FileRow;
    let files = t.app.files.clone().unwrap();
    t.app.file_rows().iter().cloned().map(|r| match r {
        FileRow::Dir { name, depth, collapsed, .. } => format!("{}{}{name}/", "  ".repeat(depth), if collapsed { "+" } else { "-" }),
        FileRow::File { idx, depth } => format!("{}{}", "  ".repeat(depth), files[idx].path.rsplit('/').next().unwrap()),
    }).collect()
}

#[test]
fn tree_view_groups_by_directory_collapses_and_selection_follows_files() {
    let f = tree_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('t');
    assert!(t.app.ui_state.tree_view, "the setting is kept in UI state");
    assert_eq!(rows_text(&t), ["-src/", "  -ui/", "    x.rs", "    y.rs", "  main.rs", "a.txt", "z.txt"]);
    assert_eq!(t.app.current_file().unwrap().path, "a.txt", "toggling keeps the selected file");
    assert_eq!(t.app.file_cursor(), 5);
    t.app.select(1);
    t.pump();
    t.app.select(0);
    t.pump();
    assert_eq!(t.app.current_file().unwrap().path, "src/ui/x.rs", "a new list starts on the first file row");
    t.key(KeyCode::Enter); // history → files
    assert_eq!(t.app.focus, Focus::Files);
    t.ch('k');
    t.ch('k');
    assert_eq!(t.app.file_cursor(), 0, "the cursor can rest on a directory");
    t.ch('j');
    t.key(KeyCode::Enter);
    assert_eq!(rows_text(&t), ["-src/", "  +ui/", "  main.rs", "a.txt", "z.txt"], "Enter on a directory collapses it");
    assert_eq!(t.app.focus, Focus::Files);
    t.ch('j');
    t.pump();
    assert_eq!(t.app.current_file().unwrap().path, "src/main.rs");
    assert_eq!(t.app.diff.as_ref().map(|d| d.key.path.as_str()), Some("src/main.rs"), "the diff follows file rows");
    t.ch('}');
    assert_eq!(t.app.current_file().unwrap().path, "a.txt", "}} goes to the next file row");
    t.ch('{');
    t.ch('{');
    assert_eq!(t.app.current_file().unwrap().path, "src/main.rs", "{{ skips directories and hidden files");
    t.key(KeyCode::Esc);
    t.ch('t');
    assert_eq!(rows_text(&t).len(), 5, "list view: one row per file");
    assert!(rows_text(&t).iter().all(|r| !r.ends_with('/')));
}

fn keys_config(s: &str) -> Config {
    Config { keys: toml::from_str(s).unwrap(), ..Config::default() }
}

#[test]
fn a_remapped_fetch_runs_on_the_new_key_only() {
    let (f, _bare) = remote_fixture();
    let mut t = H::with(&f, keys_config("fetch = \"F5\"\n"), None);
    t.pump();
    t.ch('f');
    assert!(!t.app.take_requests().iter().any(|r| matches!(r, Request::Net { .. })), "f no longer fetches");
    t.key(KeyCode::F(5));
    assert!(t.app.take_requests().iter().any(|r| matches!(r, Request::Net { op: gitty::msg::NetOp::Fetch, .. })));
}

#[test]
fn text_inputs_ignore_remaps() {
    let f = staged_fixture();
    let mut t = H::with(&f, keys_config("quit = \"a\"\nstage = \"x\"\n"), None);
    t.pump();
    t.ch('1');
    t.pump();
    t.ch('c');
    typed(&mut t, "a fix");
    assert!(!t.app.quit, "typing a in the commit box is text");
    assert_eq!(t.app.changes.commit.summary.text(), "a fix");
    t.key(KeyCode::Esc);
    t.ch('2');
    t.ch('/');
    typed(&mut t, "a");
    assert!(!t.app.quit, "nor in the search bar");
    t.key(KeyCode::Esc);
    t.ch('a');
    assert!(t.app.quit, "outside text inputs the remap applies");
}

// ---- M5 follow-ups ----

fn started(t: &mut H, op: gitty::msg::NetOp, label: &str, background: bool) {
    t.app.handle_msg(Msg::NetStarted { op, label: label.into(), remote: Some("origin".into()), cancel: None });
    if background {
        t.app.net.as_mut().unwrap().background = true;
    }
}

fn done(t: &mut H, op: gitty::msg::NetOp, background: bool, outcome: gitty_core::net::Outcome) {
    t.app.handle_msg(Msg::NetDone { op, background, outcome });
}

fn ask(id: u64, prompt: &str) -> gitty::askpass::Ask {
    gitty::askpass::Ask { id, prompt: prompt.into(), kind: gitty::askpass::classify(prompt) }
}

#[test]
fn a_hook_decline_says_the_remote_refused_not_pull_first() {
    let (f, bare) = remote_fixture();
    let hook = bare.join("hooks/pre-receive");
    std::fs::write(&hook, "#!/bin/sh\necho 'policy: frozen' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    f.write("c.txt", "c\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('P');
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    let s = toast_text(&t);
    assert!(s.contains("refused") && !s.contains("pull first"), "{s}");
    assert!(s.contains("policy: frozen") || s.contains("hook declined"), "the reason is in the details: {s}");
}

#[test]
fn esc_at_a_prompt_reads_as_cancelled_and_prompts_close_with_their_job() {
    use gitty::msg::NetOp;
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    let mut t = H::new(&f);
    t.pump();
    started(&mut t, NetOp::Fetch, "Fetching origin", false);
    t.app.handle_msg(Msg::Ask(ask(1, "Username for 'https://example.com': ")));
    t.key(KeyCode::Esc);
    done(&mut t, NetOp::Fetch, false, gitty_core::net::Outcome::NeedsAuth { detail: "fatal: could not read Username".into() });
    let s = toast_text(&t);
    assert!(s.contains("cancelled at the prompt") && !s.contains("refused"), "{s}");

    started(&mut t, NetOp::Fetch, "Fetching origin", false);
    t.app.handle_msg(Msg::Ask(ask(2, "Username for 'https://example.com': ")));
    t.app.handle_msg(Msg::Ask(ask(3, "Password for 'https://ann@example.com': ")));
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Prompt { .. })));
    done(&mut t, NetOp::Fetch, false, gitty_core::net::Outcome::Failed { detail: "fatal: boom".into() });
    assert!(!matches!(t.app.overlay, Some(gitty::app::Overlay::Prompt { .. })), "the job's prompt closes");
    assert_eq!(t.app.pending_asks(), 0, "and its queued prompts go too");

    // Ctrl-C at a prompt answers it as cancelled, like Esc, and does not quit
    started(&mut t, NetOp::Fetch, "Fetching origin", false);
    t.app.handle_msg(Msg::Ask(ask(4, "Username for 'https://example.com': ")));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(!t.app.quit && t.app.overlay.is_none());
    done(&mut t, NetOp::Fetch, false, gitty_core::net::Outcome::NeedsAuth { detail: "fatal: could not read Username".into() });
    assert!(toast_text(&t).contains("cancelled at the prompt"), "{}", toast_text(&t));
}

#[test]
fn quitting_while_a_job_runs_asks_and_then_cancels_it() {
    let (f, _) = remote_fixture();
    f.script_remote("hang", "sleep 30");
    f.git(&["config", "branch.main.remote", "hang"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('f');
    let req = t.app.take_requests().into_iter().find(|r| matches!(r, Request::Net { .. })).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let (path, gens) = (f.path(), t.gens.clone());
    std::thread::spawn(move || {
        let h = gitty_core::Repo::open(&path).unwrap().handle();
        exec(&h, req, &mut |m| {
            let _ = tx.send(m);
        }, &gens)
    });
    t.app.handle_msg(rx.recv_timeout(Duration::from_secs(5)).unwrap());
    t.ch('q');
    assert!(!t.app.quit, "asks first");
    match &t.app.overlay {
        Some(gitty::app::Overlay::Quit { label }) => assert!(label.contains("Fetching"), "{label}"),
        _ => panic!("no quit question"),
    }
    t.ch('n');
    assert!(!t.app.quit && t.app.overlay.is_none());
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    t.app.handle_key(ctrl_c);
    assert!(!t.app.quit && matches!(t.app.overlay, Some(gitty::app::Overlay::Quit { .. })), "Ctrl-C asks too");
    t.ch('n');
    t.ch('?');
    t.app.handle_key(ctrl_c);
    assert!(!t.app.quit && matches!(t.app.overlay, Some(gitty::app::Overlay::Quit { .. })), "over help as well");
    t.app.handle_key(ctrl_c);
    assert!(t.app.quit, "a second Ctrl-C at the question quits");
    t.app.quit = false;
    t.ch('q');
    t.ch('y');
    assert!(t.app.quit);
    let end = loop {
        if let m @ Msg::NetDone { .. } = rx.recv_timeout(Duration::from_secs(5)).expect("the job ends") {
            break m;
        }
    };
    assert!(matches!(end, Msg::NetDone { outcome: gitty_core::net::Outcome::Cancelled, .. }), "{end:?}");
}

#[test]
fn background_fetch_failures_and_cancels_are_visible() {
    use gitty::msg::NetOp;
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    let mut t = H::new(&f);
    t.pump();
    started(&mut t, NetOp::Fetch, "Fetching origin", true);
    done(&mut t, NetOp::Fetch, true, gitty_core::net::Outcome::Failed { detail: "fatal: unable to access: Could not resolve host".into() });
    assert!(t.app.toast.is_none(), "no toast for a background job");
    assert_eq!(t.app.background_problem().as_deref(), Some("auto-fetch failed · !"));
    t.ch('!');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::ErrorDetail)));
    assert!(toast_text(&t).contains("Could not resolve host"), "{}", toast_text(&t));
    t.key(KeyCode::Esc);
    started(&mut t, NetOp::Fetch, "Fetching origin", true);
    done(&mut t, NetOp::Fetch, true, gitty_core::net::Outcome::Ok { summary: String::new() });
    assert_eq!(t.app.background_problem(), None, "a good fetch clears it");
    started(&mut t, NetOp::Fetch, "Fetching origin", true);
    t.ch('x');
    done(&mut t, NetOp::Fetch, true, gitty_core::net::Outcome::Cancelled);
    assert!(toast_text(&t).contains("Auto-fetch cancelled"), "{}", toast_text(&t));
}

#[test]
fn a_diverged_pull_waits_behind_an_open_overlay() {
    use gitty::msg::NetOp;
    let f = Fixture::new();
    f.commit("base", 1_700_000_000);
    let mut t = H::new(&f);
    t.pump();
    started(&mut t, NetOp::Pull, "Pulling origin", false);
    t.ch('?');
    done(&mut t, NetOp::Pull, false, gitty_core::net::Outcome::Diverged);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Help { .. })), "help stays up");
    t.key(KeyCode::Esc);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Diverged)), "then the question comes");
}

#[test]
fn a_merge_or_rebase_pull_rechecks_tuning() {
    use gitty::msg::NetOp;
    let f = many_commits_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.take_requests();
    for op in [NetOp::PullMerge, NetOp::PullRebase] {
        started(&mut t, op, "Merging", false);
        done(&mut t, op, false, gitty_core::net::Outcome::Ok { summary: String::new() });
        assert!(t.app.take_requests().iter().any(|r| matches!(r, Request::Tune { .. })), "{op:?}: HEAD moved, so tuning is checked again");
    }
}

#[test]
fn editor_debug_never_prints_the_text() {
    let mut e = gitty::editor::Editor::single();
    e.insert("hunter2");
    let d = format!("{e:?}");
    assert!(!d.contains("hunter2") && d.contains('7'), "{d}");
}

fn current_branch(f: &Fixture) -> String {
    f.git(&["branch", "--show-current"])
}

#[test]
fn branch_write_ops_run_on_the_writer_and_refresh_refs() {
    let f = Fixture::new();
    commits(&f, 2);
    f.git(&["branch", "topic"]);
    let mut t = H::new(&f);
    t.pump();
    t.app.write(WriteOp::SwitchBranch { name: "topic".into(), remote: false });
    t.pump();
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(t.app.refs.as_ref().unwrap().head_branch(), Some("topic"), "refs refreshed after the switch");
    t.app.write(WriteOp::CreateBranch { name: "feat/x".into() });
    t.pump();
    assert_eq!(current_branch(&f), "feat/x");
    t.app.write(WriteOp::RenameBranch { old: "feat/x".into(), new: "feat/y".into() });
    t.pump();
    assert_eq!(t.app.refs.as_ref().unwrap().head_branch(), Some("feat/y"));
    t.app.write(WriteOp::SwitchBranch { name: "main".into(), remote: false });
    t.pump();
    t.app.write(WriteOp::DeleteBranch { name: "feat/y".into(), force: false });
    t.pump();
    assert_eq!(f.git(&["branch", "--list", "feat/y"]), "");
    assert!(t.app.toast.is_none(), "{:?}", t.app.toast);
}

#[test]
fn a_refused_branch_write_toasts_git_s_message() {
    let f = Fixture::new();
    commits(&f, 1);
    let mut t = H::new(&f);
    t.pump();
    t.app.write(WriteOp::CreateBranch { name: "bad..name".into() });
    t.pump();
    let toast = t.app.toast.as_ref().expect("an error toast");
    assert!(toast.error && toast.detail.contains("not a valid branch name"), "{toast:?}");
    assert_eq!(current_branch(&f), "main");
}

fn ctrl(t: &mut H, c: char) {
    t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
}

/// main: two commits; topic: branches off main's first commit and adds t.txt.
fn branch_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic work", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("m.txt", "m\n");
    f.commit("main work", 1_700_000_200);
    f
}

#[test]
fn switcher_lists_the_current_branch_first_and_filters() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Switcher { .. })));
    let names: Vec<_> = t.app.switcher_matches("").into_iter().map(|x| (x.name, x.kind)).collect();
    assert_eq!(names, [("main".into(), gitty_core::refs::TargetKind::Current), ("topic".into(), gitty_core::refs::TargetKind::Local)]);
    typed(&mut t, "top");
    let m = t.app.switcher_matches("top");
    assert_eq!(m.len(), 1);
    assert_eq!(m[0].name, "topic");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
}

#[test]
fn enter_switches_a_clean_tree_and_history_follows() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    t.pump();
    assert!(t.app.overlay.is_none());
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(t.app.refs.as_ref().unwrap().head_branch(), Some("topic"));
    assert_eq!(t.selected_id(), id(&f.git(&["rev-parse", "topic"])), "history shows the new branch's tip");
}

#[test]
fn a_dirty_tree_asks_and_switch_anyway_carries_the_change() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::DirtySwitch { .. })), "asks first");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
    assert_eq!(current_branch(&f), "main", "cancel changes nothing");
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    t.ch('w');
    t.drain();
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n", "the change came along");
}

#[test]
fn a_switch_git_refuses_shows_the_error_and_stays() {
    let f = branch_fixture();
    f.git(&["switch", "-q", "topic"]);
    f.write("a.txt", "topic edit\n");
    f.commit("topic edits a", 1_700_000_300);
    f.git(&["switch", "-q", "main"]);
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "dirty\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    t.ch('w');
    t.drain();
    assert_eq!(current_branch(&f), "main");
    assert!(t.app.toast.as_ref().is_some_and(|x| x.error), "{:?}", t.app.toast);
}

#[test]
fn ctrl_n_makes_a_branch_from_the_typed_name() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "feat/x");
    ctrl(&mut t, 'n');
    match &t.app.overlay {
        Some(gitty::app::Overlay::NameInput { input, .. }) => assert_eq!(input.text(), "feat/x", "prefilled from the query"),
        _ => panic!("no name input"),
    }
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(current_branch(&f), "feat/x");
}

#[test]
fn ctrl_r_renames_the_highlighted_branch() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'r');
    ctrl(&mut t, 'u');
    typed(&mut t, "renamed");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(f.git(&["branch", "--list", "topic"]), "");
    assert!(f.git(&["branch", "--list", "renamed"]).contains("renamed"));
    assert_eq!(current_branch(&f), "main");
}

#[test]
fn ctrl_d_asks_then_offers_force_for_an_unmerged_branch() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'd');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { .. })));
    t.key(KeyCode::Enter);
    t.pump();
    assert!(
        matches!(&t.app.overlay, Some(gitty::app::Overlay::Confirm { op: WriteOp::DeleteBranch { force: true, .. }, .. })),
        "unmerged: asks again before forcing"
    );
    assert!(f.git(&["branch", "--list", "topic"]).contains("topic"), "nothing deleted yet");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(f.git(&["branch", "--list", "topic"]), "");
}

#[test]
fn the_current_branch_cannot_be_renamed_away_or_deleted_from_the_picker() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    ctrl(&mut t, 'd');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Switcher { .. })), "stays open");
    assert!(t.app.toast.is_some());
    assert_eq!(f.git(&["branch", "--list", "main"]).trim_start_matches("* "), "main");
}

#[test]
fn the_picker_opens_in_a_repository_without_commits() {
    let f = Fixture::new();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Switcher { .. })));
    assert!(t.app.switcher_matches("").is_empty());
    t.key(KeyCode::Enter);
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
}

#[test]
fn stash_ops_push_list_apply_pop_and_drop() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.write(WriteOp::StashPush { message: "wip".into() });
    t.pump();
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(t.app.stashes.iter().map(|s| s.message.as_str()).collect::<Vec<_>>(), ["wip"], "the list refreshed after the push");
    let expect = t.app.stashes[0].id.clone();
    t.app.write(WriteOp::StashApply { index: 0, expect });
    t.pump();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n");
    assert_eq!(t.app.stashes.len(), 1);
    f.git(&["checkout", "--", "a.txt"]);
    let expect = t.app.stashes[0].id.clone();
    t.app.write(WriteOp::StashPop { index: 0, expect });
    t.pump();
    assert!(t.app.stashes.is_empty());
    t.app.write(WriteOp::StashPush { message: "again".into() });
    t.pump();
    let expect = t.app.stashes[0].id.clone();
    t.app.write(WriteOp::StashDrop { index: 0, expect });
    t.pump();
    assert!(t.app.stashes.is_empty());
    assert_eq!(f.git(&["status", "--porcelain"]), "", "dropping does not restore");
}

#[test]
fn stashing_nothing_says_so_without_an_error() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.write(WriteOp::StashPush { message: "x".into() });
    t.pump();
    let toast = t.app.toast.as_ref().expect("a note");
    assert!(!toast.error && toast.what.contains("Nothing to stash"), "{toast:?}");
}

#[test]
fn stash_and_switch_parks_the_work_then_switches() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    f.write("new.txt", "fresh\n");
    t.app.write(WriteOp::StashAndSwitch { name: "topic".into(), remote: false, message: "gitty: auto-stash from main".into() });
    t.pump();
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(f.git(&["status", "--porcelain"]), "", "the work is parked");
    assert_eq!(t.app.stashes[0].message, "gitty: auto-stash from main");
    assert_eq!(t.app.refs.as_ref().unwrap().head_branch(), Some("topic"));
}

#[test]
fn a_failed_stash_and_switch_puts_the_work_back() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    f.git(&["add", "a.txt"]);
    f.write("m.txt", "unstaged\n");
    t.app.write(WriteOp::StashAndSwitch { name: "no-such-branch".into(), remote: false, message: "m".into() });
    t.pump();
    let toast = t.app.toast.as_ref().expect("an error");
    assert!(toast.error && toast.detail.contains("put back"), "{toast:?}");
    assert_eq!(current_branch(&f), "main");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n", "nothing lost");
    assert_eq!(f.git(&["status", "--porcelain", "a.txt"]), "M  a.txt", "staged stays staged");
    assert_eq!(f.git(&["diff", "--name-only"]), "m.txt", "unstaged stays unstaged");
    assert!(f.git(&["stash", "list"]).is_empty());
    assert!(t.app.stashes.is_empty());
}

#[test]
fn a_switch_that_fails_after_moving_head_keeps_the_stash() {
    use std::os::unix::fs::PermissionsExt;
    let f = branch_fixture();
    let hook = f.path().join(".git/hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 2\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // a global core.hooksPath would otherwise make git ignore .git/hooks
    f.git(&["config", "core.hooksPath", hook.parent().unwrap().to_str().unwrap()]);
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.write(WriteOp::StashAndSwitch { name: "topic".into(), remote: false, message: "m".into() });
    t.pump();
    assert_eq!(current_branch(&f), "topic", "the switch happened");
    assert_eq!(f.git(&["stash", "list"]).lines().count(), 1, "the stash is kept");
    assert_eq!(f.git(&["status", "--porcelain"]), "", "the work was not applied here");
    let toast = t.app.toast.as_ref().expect("an error");
    assert!(toast.error && toast.detail.contains("stash@{0}") && !toast.detail.contains("put back"), "{toast:?}");
}

#[test]
fn s_at_the_dirty_prompt_stashes_and_switches() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    t.ch('s');
    t.drain();
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert_eq!(t.app.stashes[0].message, "gitty: auto-stash from main");
}

#[test]
fn the_stash_list_applies_pops_and_drops() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "one\n");
    t.app.write(WriteOp::StashPush { message: "one".into() });
    t.pump();
    f.write("a.txt", "two\n");
    t.app.write(WriteOp::StashPush { message: "two".into() });
    t.pump();
    t.ch('S');
    t.pump();
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Stashes { .. })));
    assert_eq!(t.app.stashes.len(), 2);
    t.ch('j');
    t.ch('p');
    t.pump();
    assert!(t.app.overlay.is_none(), "pop closes the list");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "one\n", "the older stash came back");
    assert_eq!(t.app.stashes.len(), 1);
    t.ch('S');
    t.pump();
    t.ch('d');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { .. })), "drop asks");
    t.key(KeyCode::Enter);
    t.pump();
    assert!(t.app.stashes.is_empty());
}

#[test]
fn a_stash_acted_on_after_the_list_changed_is_left_alone() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "one\n");
    t.app.write(WriteOp::StashPush { message: "one".into() });
    t.pump();
    t.ch('S');
    t.pump();
    f.write("a.txt", "two\n");
    f.git(&["stash", "push", "-m", "behind your back"]);
    t.ch('p');
    t.pump();
    let toast = t.app.toast.as_ref().expect("an error");
    assert!(toast.error && toast.detail.contains("list changed"), "{toast:?}");
    assert_eq!(f.git(&["stash", "list"]).lines().count(), 2, "pop did nothing");
    t.ch('S');
    t.pump();
    f.write("a.txt", "three\n");
    f.git(&["stash", "push", "-m", "again"]);
    t.ch('d');
    match &t.app.overlay {
        Some(gitty::app::Overlay::Confirm { body, .. }) => assert!(body.contains("\"") && body.contains("(main)"), "{body}"),
        _ => panic!("no confirm"),
    }
    t.key(KeyCode::Enter);
    t.pump();
    let toast = t.app.toast.as_ref().expect("an error");
    assert!(toast.error && toast.detail.contains("list changed"), "{toast:?}");
    assert_eq!(f.git(&["stash", "list"]).lines().count(), 3, "drop did nothing");
}

#[test]
fn z_in_changes_asks_for_a_message_then_stashes() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('1');
    t.ch('Z');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::NameInput { kind: gitty::app::branches::NameKind::Stash, .. })));
    typed(&mut t, "half done");
    t.key(KeyCode::Enter);
    t.drain();
    assert_eq!(t.app.stashes[0].message, "half done");
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn an_empty_stash_message_gets_a_default() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('1');
    t.ch('Z');
    t.key(KeyCode::Enter);
    t.drain();
    assert_eq!(t.app.stashes[0].message, "gitty: stash on main");
}

#[test]
fn a_detached_head_has_no_current_entry_and_switches_to_a_branch() {
    let f = branch_fixture();
    f.git(&["checkout", "-q", "--detach"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    let m = t.app.switcher_matches("");
    assert!(m.iter().all(|x| x.kind != gitty_core::refs::TargetKind::Current), "{m:?}");
    typed(&mut t, "main");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(current_branch(&f), "main");
}

#[test]
fn s_at_the_dirty_prompt_on_a_detached_head_names_head_in_the_stash() {
    let f = branch_fixture();
    f.git(&["checkout", "-q", "--detach"]);
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    t.key(KeyCode::Enter);
    t.ch('s');
    t.drain();
    assert_eq!(current_branch(&f), "topic");
    assert_eq!(t.app.stashes[0].message, "gitty: auto-stash from HEAD");
}

/// `feature` was pushed and then deleted locally, so only `origin/feature` is left.
fn remote_only_fixture() -> Fixture {
    let f = branch_fixture();
    f.add_bare_upstream();
    f.git(&["branch", "feature"]);
    f.git(&["push", "-q", "origin", "feature"]);
    f.git(&["branch", "-q", "-D", "feature"]);
    f
}

#[test]
fn a_remote_only_branch_is_listed_and_switching_creates_a_tracking_branch() {
    let f = remote_only_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    let m = t.app.switcher_matches("");
    assert!(m.iter().any(|x| x.name == "origin/feature" && x.kind == gitty_core::refs::TargetKind::Remote), "{m:?}");
    typed(&mut t, "feature");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(current_branch(&f), "feature");
    assert_eq!(f.git(&["config", "branch.feature.remote"]), "origin");
}

#[test]
fn rename_and_delete_leave_a_remote_branch_alone() {
    let f = remote_only_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "origin/feature");
    assert_eq!(t.app.switcher_matches("origin/feature")[0].name, "origin/feature");
    let before = f.git(&["branch", "-a"]);
    for c in ['r', 'd'] {
        t.app.toast = None;
        ctrl(&mut t, c);
        assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Switcher { .. })), "ctrl-{c} keeps the picker open");
        assert!(t.app.toast.is_some(), "ctrl-{c} explains");
        assert!(t.app.take_requests_peek().is_empty(), "ctrl-{c} sends nothing");
        assert_eq!(f.git(&["branch", "-a"]), before, "ctrl-{c} changed nothing");
    }
}

#[test]
fn a_in_the_stash_list_applies_and_keeps_the_entry() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "one\n");
    t.app.write(WriteOp::StashPush { message: "one".into() });
    t.pump();
    f.write("a.txt", "two\n");
    t.app.write(WriteOp::StashPush { message: "two".into() });
    t.pump();
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "a\n");
    t.ch('S');
    t.pump();
    t.ch('a');
    t.pump();
    assert!(t.app.overlay.is_none(), "apply closes the list");
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "two\n", "the highlighted (newest) stash came back");
    assert_eq!(t.app.stashes.len(), 2);
    assert_eq!(f.git(&["stash", "list"]).lines().count(), 2, "the entry is kept");
}

#[test]
fn n_in_the_stash_list_asks_for_a_message_then_stashes() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.ch('S');
    t.pump();
    t.ch('n');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::NameInput { kind: gitty::app::branches::NameKind::Stash, .. })));
    typed(&mut t, "from the list");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(t.app.stashes[0].message, "from the list");
    assert_eq!(f.git(&["stash", "list"]).lines().count(), 1);
}

fn pr_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "feat/x"]);
    f
}

/// `R` on `f`, the toast it ends in and the page it asked to open.
fn press_r(f: &Fixture) -> (String, Option<String>) {
    let mut t = H::new(f);
    t.pump();
    t.app.handle_focus(true);
    t.drain();
    t.ch('R');
    t.drain();
    (toast_text(&t), t.app.open_url.clone())
}

#[test]
fn r_on_a_detached_head_says_there_is_no_branch() {
    let f = pr_fixture();
    f.git(&["checkout", "-q", "--detach"]);
    let (toast, url) = press_r(&f);
    assert!(toast.starts_with("No branch checked out"), "{toast}");
    assert_eq!(url, None);
}

#[test]
fn r_without_a_remote_says_so() {
    let (toast, url) = press_r(&pr_fixture());
    assert!(toast.starts_with("No remote to open"), "{toast}");
    assert_eq!(url, None);
}

#[test]
fn r_on_an_unpushed_branch_asks_for_a_push() {
    let f = pr_fixture();
    f.git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
    let (toast, url) = press_r(&f);
    assert!(toast.starts_with("Push the branch first (P)"), "{toast}");
    assert_eq!(url, None);
}

#[test]
fn r_on_a_non_github_remote_says_so() {
    let f = pr_fixture();
    f.git(&["remote", "add", "origin", "git@gitlab.com:o/r.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    let (toast, url) = press_r(&f);
    assert!(toast.starts_with("Only GitHub remotes are supported"), "{toast}");
    assert_eq!(url, None);
}

#[test]
fn r_on_a_pushed_branch_asks_to_open_its_pull_request_page() {
    let f = pr_fixture();
    f.git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
    f.git(&["update-ref", "refs/remotes/origin/feat/x", "HEAD"]);
    let (toast, url) = press_r(&f);
    assert_eq!(url.as_deref(), Some("https://github.com/o/r/pull/new/feat/x"), "{toast}");
}

fn pr_info(number: u64, state: gitty_core::forge::PrState) -> gitty_core::forge::PrInfo {
    gitty_core::forge::PrInfo { number, state, url: format!("https://github.com/o/r/pull/{number}") }
}

fn badge_requests(reqs: &[Request]) -> Vec<&str> {
    reqs.iter().filter_map(|r| if let Request::PrBadge { branch } = r { Some(branch.as_str()) } else { None }).collect()
}

#[test]
fn startup_asks_for_the_branchs_pull_request_and_the_reply_sets_the_badge() {
    use gitty_core::forge::PrState;
    let f = pr_fixture();
    let mut t = H::new(&f);
    let r = t.app.take_requests();
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    let r = t.app.take_requests();
    assert_eq!(badge_requests(&r), ["feat/x"]);
    // this repository has no remote: gh is never asked, and there is no badge
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    assert_eq!(t.app.pr_badge, None);
    assert!(t.app.toast.is_none());
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(Some(pr_info(7, PrState::Open))) });
    assert_eq!(t.app.pr_badge, Some(("feat/x".into(), pr_info(7, PrState::Open))));
    // an answer of "none" (gh says the PR is gone) takes it off again
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(None) });
    assert_eq!(t.app.pr_badge, None);
}

#[test]
fn a_failed_lookup_keeps_the_badge_and_a_missing_pull_request_removes_it() {
    use gitty_core::forge::PrState;
    let f = pr_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(Some(pr_info(7, PrState::Open))) });
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Err(gitty_core::forge::PrUnknown) });
    assert_eq!(t.app.pr_badge, Some(("feat/x".into(), pr_info(7, PrState::Open))), "timeout, offline: still the last answer");
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(None) });
    assert_eq!(t.app.pr_badge, None, "gh said there is none");
}

#[test]
fn a_reply_for_another_branch_is_ignored() {
    use gitty_core::forge::PrState;
    let f = pr_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.app.handle_msg(Msg::PrBadge { branch: "main".into(), result: Ok(Some(pr_info(7, PrState::Merged))) });
    assert_eq!(t.app.pr_badge, None);
}

#[test]
fn switching_branches_clears_the_badge_and_asks_again() {
    use gitty_core::forge::PrState;
    let f = pr_fixture();
    f.git(&["branch", "other"]);
    let mut t = H::new(&f);
    t.pump();
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(Some(pr_info(7, PrState::Open))) });
    f.git(&["switch", "-q", "other"]);
    t.app.handle_focus(true);
    let r = t.app.take_requests();
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    assert_eq!(t.app.pr_badge, None, "gone as soon as the new HEAD is known");
    // a late answer for the old branch changes nothing
    t.app.handle_msg(Msg::PrBadge { branch: "feat/x".into(), result: Ok(Some(pr_info(7, PrState::Open))) });
    assert_eq!(t.app.pr_badge, None);
    assert_eq!(badge_requests(&t.app.take_requests()), ["other"]);
}

#[test]
fn the_pull_request_is_not_asked_for_more_than_every_30_seconds() {
    let f = pr_fixture();
    let mut t = H::new(&f);
    t.drain();
    // every refresh of the refs (focus, fetch, push, commit) passes by the question
    for _ in 0..3 {
        t.app.handle_focus(true);
        let r = t.app.take_requests();
        assert!(badge_requests(&r).is_empty(), "asked again at once");
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
        let r = t.app.take_requests();
        assert!(badge_requests(&r).is_empty());
    }
    t.app.tick(t.clock + Duration::from_secs(31));
    t.app.handle_focus(true);
    let r = t.app.take_requests();
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    t.app.tick(t.clock + Duration::from_secs(31));
    assert_eq!(badge_requests(&t.app.take_requests()), ["feat/x"], "after the minimum, a refresh asks again");
}

#[test]
fn switching_back_does_not_queue_a_second_lookup_while_one_is_out() {
    let f = pr_fixture();
    f.git(&["branch", "other"]);
    let mut t = H::new(&f);
    let r = t.app.take_requests();
    for m in t.exec_all(r) {
        t.app.handle_msg(m);
    }
    assert_eq!(badge_requests(&t.app.take_requests()), ["feat/x"], "out, no reply yet");
    for branch in ["other", "feat/x"] {
        f.git(&["switch", "-q", branch]);
        t.app.handle_focus(true);
        let r = t.app.take_requests();
        for m in t.exec_all(r) {
            t.app.handle_msg(m);
        }
        let r = t.app.take_requests();
        assert_eq!(badge_requests(&r), if branch == "other" { vec!["other"] } else { vec![] }, "{branch}");
    }
    assert_eq!(gitty::workers::route(&Request::PrBadge { branch: "x".into() }), gitty::workers::Pool::Maintenance);
}

#[test]
fn the_badge_refreshes_by_itself_every_5_minutes_while_focused() {
    let f = pr_fixture();
    let mut t = H::new(&f);
    t.drain();
    assert_eq!(t.app.next_deadline(), None, "unfocused: no timer");
    t.app.handle_focus(true);
    t.drain();
    let at = t.clock + Duration::from_secs(301);
    t.app.tick(at);
    assert_eq!(badge_requests(&t.app.take_requests()), ["feat/x"]);
}

/// branch_fixture, but topic and main also edit a.txt differently.
fn conflicting_fixture() -> Fixture {
    let f = branch_fixture();
    f.git(&["switch", "-q", "topic"]);
    f.write("a.txt", "topic a\n");
    f.write("b.txt", "topic b\n");
    f.commit("topic edits", 1_700_000_300);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main a\n");
    f.write("b.txt", "main b\n");
    f.commit("main edits", 1_700_000_400);
    f
}

fn parent_count(f: &Fixture) -> usize {
    f.git(&["rev-list", "--parents", "-n1", "HEAD"]).split_whitespace().count() - 1
}

#[test]
fn ctrl_g_asks_then_merges_the_highlighted_branch() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    match &t.app.overlay {
        Some(gitty::app::Overlay::Confirm { body, op: WriteOp::Merge { name, remote: false }, .. }) => {
            assert_eq!(body, "Merge `topic` into `main`?");
            assert_eq!(name, "topic");
        }
        _ => panic!("no merge prompt"),
    }
    assert_eq!(parent_count(&f), 1, "asking changes nothing");
    t.key(KeyCode::Enter);
    t.pump();
    assert!(t.app.overlay.is_none());
    assert_eq!(parent_count(&f), 2);
    assert!(f.path().join("t.txt").exists());
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Merged topic into main", "{toast:?}");
}

#[test]
fn m_is_typed_into_the_filter_not_taken_as_merge() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "m");
    match &t.app.overlay {
        Some(gitty::app::Overlay::Switcher { query, .. }) => assert_eq!(query.text(), "m"),
        _ => panic!("the picker closed"),
    }
}

#[test]
fn y_confirms_and_n_or_esc_cancel_the_merge_prompt() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    for cancel in [KeyCode::Esc, KeyCode::Char('n')] {
        t.ch('B');
        typed(&mut t, "topic");
        ctrl(&mut t, 'g');
        t.key(cancel);
        assert!(t.app.overlay.is_none());
        assert!(t.app.take_requests_peek().is_empty(), "nothing was sent");
        assert_eq!(parent_count(&f), 1);
    }
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('y');
    t.pump();
    assert_eq!(parent_count(&f), 2);
}

#[test]
fn ctrl_g_on_the_current_branch_or_a_detached_head_explains_and_sends_nothing() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    ctrl(&mut t, 'g');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Switcher { .. })), "the picker stays open");
    assert!(toast_text(&t).contains("itself"), "{}", toast_text(&t));
    assert!(t.app.take_requests_peek().is_empty());
    t.key(KeyCode::Esc);
    f.git(&["checkout", "-q", "--detach"]);
    t.app.handle_focus(true);
    t.drain();
    t.app.toast = None;
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    assert!(toast_text(&t).contains("No branch checked out"), "{}", toast_text(&t));
    assert!(!matches!(t.app.overlay, Some(gitty::app::Overlay::Confirm { .. })));
}

#[test]
fn a_fast_forward_and_an_up_to_date_merge_each_say_so() {
    let f = branch_fixture();
    f.git(&["switch", "-q", "-c", "behind", "HEAD~1"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "main");
    ctrl(&mut t, 'g');
    t.key(KeyCode::Enter);
    t.pump();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Merged main into behind", "{toast:?}");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), f.git(&["rev-parse", "main"]), "a fast-forward");
    t.app.toast = None;
    t.ch('B');
    typed(&mut t, "main");
    ctrl(&mut t, 'g');
    t.key(KeyCode::Enter);
    t.pump();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Already up to date", "{toast:?}");
}

#[test]
fn conflicts_are_reported_as_a_notice_and_nothing_changes() {
    let f = conflicting_fixture();
    let head = f.git(&["rev-parse", "HEAD"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.key(KeyCode::Enter);
    t.pump();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error, "{toast:?}");
    assert_eq!(toast.what, "Merge of topic has conflicts in 2 files: a.txt, b.txt. Nothing was changed - resolve in a terminal: git merge topic");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert!(!f.path().join(".git/MERGE_HEAD").exists());
}

#[test]
fn a_merge_git_refuses_shows_an_error() {
    let f = branch_fixture();
    f.write("t.txt", "in the way\n");
    f.git(&["add", "t.txt"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::DirtySwitch { merge: true, .. })), "a staged file is a change");
    t.ch('w');
    t.drain();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(toast.error && toast.what == "merging failed", "{toast:?}");
    assert_eq!(parent_count(&f), 1);
}

#[test]
fn a_remote_only_branch_is_merged_from_the_picker() {
    let f = branch_fixture();
    f.add_bare_upstream();
    f.git(&["switch", "-q", "-c", "feature"]);
    f.write("f.txt", "f\n");
    f.commit("feature work", 1_700_000_300);
    f.git(&["push", "-q", "origin", "feature"]);
    f.git(&["switch", "-q", "main"]);
    f.git(&["branch", "-q", "-D", "feature"]);
    let mut t = H::new(&f);
    t.pump();
    t.ch('B');
    typed(&mut t, "origin/feature");
    ctrl(&mut t, 'g');
    assert!(matches!(&t.app.overlay, Some(gitty::app::Overlay::Confirm { op: WriteOp::Merge { remote: true, .. }, .. })));
    t.key(KeyCode::Enter);
    t.pump();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Merged origin/feature into main", "{toast:?}");
    assert_eq!(current_branch(&f), "main");
}

#[test]
fn a_dirty_tree_asks_whether_to_stash_before_merging() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::DirtySwitch { merge: true, .. })), "asks first");
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
    assert_eq!(parent_count(&f), 1, "cancel changes nothing");
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('w');
    t.drain();
    assert_eq!(parent_count(&f), 2);
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n", "the change stayed");
    assert!(t.app.stashes.is_empty());
}

#[test]
fn s_at_the_dirty_merge_prompt_stashes_merges_and_puts_the_changes_back() {
    let f = branch_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('s');
    t.drain();
    assert_eq!(parent_count(&f), 2);
    assert_eq!(std::fs::read_to_string(f.path().join("a.txt")).unwrap(), "edited\n", "back on the same branch");
    assert!(t.app.stashes.is_empty(), "the stash was popped");
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Merged topic into main; your changes were put back", "{toast:?}");
}

#[test]
fn a_stale_squash_msg_does_not_strand_the_stash() {
    let f = branch_fixture();
    f.git(&["merge", "--squash", "topic"]);
    f.git(&["restore", "--staged", "--worktree", "."]);
    f.git(&["clean", "-fdq"]);
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('s');
    t.drain();
    assert_eq!(parent_count(&f), 2);
    assert!(t.app.stashes.is_empty(), "the stash was popped");
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what == "Merged topic into main; your changes were put back", "{toast:?}");
}

#[test]
fn a_stash_that_will_not_pop_after_the_merge_is_kept_and_reported() {
    let f = branch_fixture();
    f.git(&["switch", "-q", "topic"]);
    f.write("a.txt", "topic a\n");
    f.commit("topic edits a", 1_700_000_300);
    f.git(&["switch", "-q", "main"]);
    let mut t = H::new(&f);
    t.pump();
    f.write("a.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('s');
    t.drain();
    assert_eq!(parent_count(&f), 2, "the merge happened");
    assert_eq!(t.app.stashes.len(), 1, "the stash was kept");
    let toast = t.app.toast.as_ref().unwrap();
    assert!(toast.what.starts_with("Merged topic into main; ") && toast.what.contains("putting your changes back failed") && toast.what.contains("stash@{0}"), "{toast:?}");
}

#[test]
fn stash_and_merge_puts_the_changes_back_when_the_merge_hits_conflicts_or_fails() {
    let f = conflicting_fixture();
    let mut t = H::new(&f);
    t.pump();
    f.write("m.txt", "edited\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "topic");
    ctrl(&mut t, 'g');
    t.ch('s');
    t.drain();
    let toast = t.app.toast.as_ref().unwrap();
    assert!(!toast.error && toast.what.contains("conflicts in 2 files") && toast.what.contains("your changes were put back"), "{toast:?}");
    assert_eq!(std::fs::read_to_string(f.path().join("m.txt")).unwrap(), "edited\n");
    assert!(t.app.stashes.is_empty(), "the stash was popped");
    assert_eq!(parent_count(&f), 1);

    // up to date: nothing happened, so the work goes back too
    f.git(&["checkout", "--", "m.txt"]);
    f.git(&["branch", "same"]);
    f.write("m.txt", "edited again\n");
    t.app.handle_focus(true);
    t.drain();
    t.ch('B');
    typed(&mut t, "same");
    ctrl(&mut t, 'g');
    t.ch('s');
    t.drain();
    assert!(toast_text(&t).starts_with("Already up to date") && toast_text(&t).contains("put back"), "{}", toast_text(&t));
    assert_eq!(std::fs::read_to_string(f.path().join("m.txt")).unwrap(), "edited again\n");
    assert!(t.app.stashes.is_empty());
}

// ---- force push with lease ----

/// `topic` is pushed, then its tip is amended: a normal push is rejected.
fn amended_topic() -> (Fixture, std::path::PathBuf) {
    let (f, bare) = remote_fixture();
    f.git(&["checkout", "-q", "-b", "topic"]);
    f.write("t.txt", "t\n");
    f.commit("topic", 1_700_000_100);
    f.git(&["push", "-q", "-u", "origin", "topic"]);
    f.write("t.txt", "t amended\n");
    f.git_env(&["commit", "-q", "-a", "--amend", "-m", "topic amended"], &[("GIT_COMMITTER_DATE", "1700000200 +0000".into())]);
    (f, bare)
}

fn remote_rev(bare: &std::path::Path, branch: &str) -> String {
    let out = std::process::Command::new("git").arg("--git-dir").arg(bare).args(["rev-parse", branch]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Pushes `P` and runs it; the rejection arrives with the force-push question.
fn push_rejected(t: &mut H) {
    t.ch('P');
    let reqs = net_requests(t);
    run_net(t, reqs);
}

#[test]
fn a_rejected_push_offers_a_force_push_with_lease() {
    let (f, _bare) = amended_topic();
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    assert!(matches!(&t.app.overlay, Some(gitty::app::Overlay::ForcePush { plan }) if plan.branch == "topic" && plan.expected == f.git(&["rev-parse", "origin/topic"])), "{}", toast_text(&t));
    assert!(toast_text(&t).contains("pull first"), "the rejection stays under the question");
}

#[test]
fn esc_cancels_the_force_push_and_runs_nothing() {
    let (f, bare) = amended_topic();
    let before = remote_rev(&bare, "topic");
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    t.key(KeyCode::Esc);
    assert!(t.app.overlay.is_none());
    assert!(net_requests(&mut t).is_empty());
    assert_eq!(remote_rev(&bare, "topic"), before);
}

#[test]
fn enter_force_pushes_with_the_lease() {
    let (f, bare) = amended_topic();
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    t.key(KeyCode::Enter);
    let reqs = net_requests(&mut t);
    assert!(matches!(reqs.as_slice(), [Request::Net { op: gitty::msg::NetOp::ForcePush, force: Some(_), .. }]), "{} requests", reqs.len());
    run_net(&mut t, reqs);
    assert_eq!(remote_rev(&bare, "topic"), f.git(&["rev-parse", "HEAD"]));
    assert!(toast_text(&t).contains("Force pushed topic"), "{}", toast_text(&t));
}

#[test]
fn a_force_push_the_lease_refuses_says_to_fetch_first() {
    let (f, bare) = amended_topic();
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    // someone pushes after the question came up
    let tmp = tempfile::tempdir().unwrap();
    let git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(tmp.path(), &["clone", "-q", "-b", "topic", bare.to_str().unwrap(), "o"]);
    let o = tmp.path().join("o");
    std::fs::write(o.join("theirs.txt"), "theirs\n").unwrap();
    git(&o, &["add", "-A"]);
    git(&o, &["-c", "user.name=O", "-c", "user.email=o@example.com", "commit", "-qm", "theirs"]);
    git(&o, &["push", "-q", "origin", "topic"]);
    let theirs = remote_rev(&bare, "topic");
    t.key(KeyCode::Enter);
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    let s = toast_text(&t);
    assert!(s.contains("The remote has new commits you haven't seen. Fetch first (f) and look at them."), "{s}");
    assert_eq!(remote_rev(&bare, "topic"), theirs);
}

#[test]
fn main_is_never_offered_a_force_push() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "b.txt");
    f.git(&["fetch", "-q"]);
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    assert!(t.app.overlay.is_none());
    assert!(toast_text(&t).contains("Force pushing main is blocked in gitty"), "{}", toast_text(&t));
}

#[test]
fn an_offer_that_finds_another_overlay_open_says_how_to_get_it_back() {
    let (f, _bare) = amended_topic();
    let mut t = H::new(&f);
    t.pump();
    t.ch('P');
    let reqs = net_requests(&mut t);
    t.app.overlay = Some(gitty::app::Overlay::Help { scroll: 0 });
    run_net(&mut t, reqs);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::Help { .. })));
    assert!(toast_text(&t).contains("press P to see the force push option"), "{}", toast_text(&t));
}

#[test]
fn enter_during_an_auto_fetch_keeps_the_question() {
    let (f, _bare) = amended_topic();
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    started(&mut t, gitty::msg::NetOp::Fetch, "Fetching", true);
    t.key(KeyCode::Enter);
    assert!(matches!(t.app.overlay, Some(gitty::app::Overlay::ForcePush { .. })));
    assert!(toast_text(&t).contains("Fetch in progress"), "{}", toast_text(&t));
    assert!(net_requests(&mut t).is_empty());
}

#[test]
fn a_switch_during_the_push_does_not_change_the_offer() {
    let (f, _bare) = amended_topic();
    f.git(&["branch", "other", "main"]);
    // the push is running when the user switches branches
    let hook = f.path().join(".git/hooks/pre-push");
    std::fs::write(&hook, "#!/bin/sh\nunset GIT_DIR GIT_INDEX_FILE\ngit checkout -q other\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    assert_eq!(f.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "other", "the hook switched");
    assert!(matches!(&t.app.overlay, Some(gitty::app::Overlay::ForcePush { plan }) if plan.branch == "topic"), "{}", toast_text(&t));
}

#[test]
fn a_force_push_confirmed_after_the_branch_changed_is_not_run() {
    let (f, bare) = amended_topic();
    let before = remote_rev(&bare, "topic");
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    f.git(&["checkout", "-q", "main"]);
    t.key(KeyCode::Enter);
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    assert!(toast_text(&t).contains("HEAD is no longer on topic"), "{}", toast_text(&t));
    assert_eq!(remote_rev(&bare, "topic"), before);
}

#[test]
fn a_branch_that_moved_while_the_question_was_open_is_not_force_pushed() {
    let (f, bare) = amended_topic();
    let before = remote_rev(&bare, "topic");
    let mut t = H::new(&f);
    t.pump();
    push_rejected(&mut t);
    f.git(&["reset", "-q", "--hard", "HEAD~1"]);
    t.key(KeyCode::Enter);
    let reqs = net_requests(&mut t);
    run_net(&mut t, reqs);
    assert!(toast_text(&t).contains("The branch changed since this screen: push again (P)"), "{}", toast_text(&t));
    assert_eq!(remote_rev(&bare, "topic"), before);
}

// ---- Files tab ----

const FAKE_SECRET: &str = "fake-secret-value-123";

fn files_fixture() -> Fixture {
    let f = Fixture::new();
    f.write(".env", format!("TOKEN={FAKE_SECRET}\n"));
    f.write(".env.example", "TOKEN=\n");
    f.write(".gitignore", "target/\n");
    f.write("README.md", "# readme\n");
    f.write("src/main.rs", "fn main() {}\n");
    f.write("src/lib.rs", "pub fn lib() {}\n");
    f.write("src/deep/mod.rs", "// deep\n");
    f.write("data.bin", b"ab\0cd");
    f.commit("base", 1_700_000_000);
    f.write("big.txt", vec![b'a'; 2 * 1024 * 1024 + 1]);
    f.write("target/out.txt", "o\n");
    f
}

/// The Files tab opened on `f`, as the main loop would (the work tree is known).
fn files_tab(f: &Fixture) -> H {
    let mut t = H::new(f);
    t.app.workdir = Some(f.path().to_path_buf());
    t.pump();
    t.key(KeyCode::Char('3'));
    t.pump();
    t
}

/// Rows as "<indent>name", for comparing listings.
fn rows(t: &H) -> Vec<String> {
    t.app.files_tab.rows.iter().map(|r| format!("{}{}", "  ".repeat(r.depth as usize), r.name)).collect()
}

fn select(t: &mut H, name: &str) {
    let i = t.app.files_tab.rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("no row {name}: {:?}", rows(t)));
    t.app.select_files_row(i);
}

/// What the Files requests in a batch are, without running them.
fn labels(reqs: &[Request]) -> Vec<String> {
    reqs.iter()
        .filter_map(|r| match r {
            Request::ReadDir { dir, .. } => Some(format!("dir {}", dir.display())),
            Request::ReadFile { path, reveal, .. } => Some(format!("file {}{}", path.display(), if *reveal { " (reveal)" } else { "" })),
            _ => None,
        })
        .collect()
}

/// Runs everything queued, recording the Files requests that went out.
fn drain_files(t: &mut H) -> Vec<String> {
    let mut sent = Vec::new();
    for _ in 0..100 {
        let reqs = t.app.take_requests();
        if reqs.is_empty() {
            return sent;
        }
        sent.extend(labels(&reqs));
        for m in t.exec_all(reqs) {
            t.app.handle_msg(m);
        }
    }
    panic!("did not settle");
}

fn viewing_text(t: &H) -> Option<String> {
    match &t.app.files_tab.viewing {
        gitty::app::files::Viewing::Ready(gitty::msg::FileView::Text { text, .. }) => Some(String::from_utf8_lossy(text.bytes()).into_owned()),
        _ => None,
    }
}

#[test]
fn key_3_opens_the_files_tab_and_lists_only_the_root() {
    let f = files_fixture();
    let mut t = H::new(&f);
    t.app.workdir = Some(f.path().to_path_buf());
    t.pump();
    t.ch('3');
    assert_eq!(t.app.tab, gitty::app::Tab::Files);
    assert_eq!(t.app.focus, Focus::Files);
    assert_eq!(labels(t.app.take_requests_peek()), ["dir "], "only the root is listed");
    t.pump();
    assert_eq!(rows(&t), ["src", "target", ".env", ".env.example", ".gitignore", "big.txt", "data.bin", "README.md"]);
    // 1 and 2 still switch
    t.ch('1');
    assert_eq!(t.app.tab, gitty::app::Tab::Changes);
    t.ch('3');
    t.ch('2');
    assert_eq!(t.app.tab, gitty::app::Tab::History);
}

#[test]
fn a_bare_repository_has_no_files_tab() {
    let f = files_fixture();
    let mut t = H::new(&f);
    t.pump();
    assert!(t.app.workdir.is_none());
    t.ch('3');
    assert_eq!(t.app.tab, gitty::app::Tab::History);
    assert!(t.app.take_requests_peek().iter().all(|r| !matches!(r, Request::ReadDir { .. })));
}

#[test]
fn enter_expands_one_directory_with_one_request_and_h_collapses() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    assert_eq!(t.app.files_tab.selected().unwrap().name, "src");
    t.key(KeyCode::Enter);
    assert_eq!(labels(t.app.take_requests_peek()), ["dir src"]);
    assert!(matches!(t.app.files_tab.rows[0].kind, gitty::app::files::RowKind::Dir { open: true, loading: true }), "shows loading… until the reply");
    t.pump();
    assert_eq!(&rows(&t)[..5], ["src", "  deep", "  lib.rs", "  main.rs", "target"]);
    // h on an open directory closes it; opening it again shows the kept listing at once and asks anew
    t.ch('h');
    assert_eq!(&rows(&t)[..2], ["src", "target"]);
    t.ch('l');
    assert_eq!(labels(t.app.take_requests_peek()), ["dir src"]);
    assert_eq!(&rows(&t)[..3], ["src", "  deep", "  lib.rs"]);
    t.pump();
    // h on a file goes to its directory; h on that closes it
    select(&mut t, "main.rs");
    t.ch('h');
    assert_eq!(t.app.files_tab.selected().unwrap().name, "src");
    t.ch('h');
    assert_eq!(&rows(&t)[..2], ["src", "target"]);
    // h on a top-level file has no parent to go to
    select(&mut t, "README.md");
    t.ch('h');
    assert_eq!(t.app.files_tab.selected().unwrap().name, "README.md");
}

#[test]
fn nested_directories_stay_expanded_through_a_refresh() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    t.key(KeyCode::Enter);
    t.pump();
    select(&mut t, "deep");
    t.key(KeyCode::Enter);
    t.pump();
    assert_eq!(&rows(&t)[..5], ["src", "  deep", "    mod.rs", "  lib.rs", "  main.rs"]);
    t.app.refresh_files();
    assert_eq!(labels(t.app.take_requests_peek()), ["dir ", "dir src", "dir src/deep"]);
    t.pump();
    assert_eq!(&rows(&t)[..3], ["src", "  deep", "    mod.rs"]);
}

#[test]
fn a_reply_from_an_older_generation_is_dropped() {
    let f = files_fixture();
    let mut t = H::new(&f);
    t.app.workdir = Some(f.path().to_path_buf());
    t.pump();
    t.ch('3');
    let first = t.app.take_requests();
    assert_eq!(labels(&first), ["dir "]);
    // a refresh begins before the first listing came back
    t.app.refresh_files();
    for m in t.exec_all(first) {
        t.app.handle_msg(m);
    }
    assert!(t.app.files_tab.loading_root(), "the stale reply installed nothing");
    assert!(t.app.files_tab.rows.is_empty());
    t.pump();
    assert!(!t.app.files_tab.rows.is_empty());
}

#[test]
fn selecting_a_file_requests_it_and_shows_its_text() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    t.key(KeyCode::Enter);
    t.pump();
    t.ch('j');
    t.ch('j');
    assert_eq!(t.app.files_tab.selected().unwrap().name, "lib.rs");
    assert_eq!(labels(t.app.take_requests_peek()), ["file src/lib.rs"]);
    t.pump();
    assert_eq!(viewing_text(&t).as_deref(), Some("pub fn lib() {}\n"));
    // moving to a directory empties the viewer
    select(&mut t, "target");
    assert!(t.app.files_tab.shown.is_none());
    t.pump();
    // Enter on a file moves to the viewer; Esc comes back
    select(&mut t, "README.md");
    t.pump();
    t.key(KeyCode::Enter);
    assert_eq!(t.app.focus, Focus::Diff);
    t.key(KeyCode::Esc);
    assert_eq!(t.app.focus, Focus::Files);
}

#[test]
fn only_the_last_selection_is_shown_when_replies_arrive_out_of_order() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, "README.md");
    let first = t.app.take_requests();
    select(&mut t, ".gitignore");
    let second = t.app.take_requests();
    // the older read is cancelled by the generation (it may not even run) and never installs
    let late = t.exec_all(first);
    let current = t.exec_all(second);
    for m in current.into_iter().chain(late) {
        t.app.handle_msg(m);
    }
    t.pump();
    assert_eq!(viewing_text(&t).as_deref(), Some("target/\n"));
}

#[test]
fn a_secret_file_is_never_requested_until_revealed() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, ".env");
    let sent = drain_files(&mut t);
    assert!(sent.iter().all(|s| !s.starts_with("file ")), "no ReadFile for a masked file: {sent:?}");
    assert!(t.app.files_tab.masked());
    assert!(matches!(t.app.files_tab.viewing, gitty::app::files::Viewing::Nothing), "nothing of the file is held");
    // the example is not a secret
    select(&mut t, ".env.example");
    assert_eq!(drain_files(&mut t), ["file .env.example"]);
    assert_eq!(viewing_text(&t).as_deref(), Some("TOKEN=\n"));
    // v reveals the selected secret only, with a request that says so
    select(&mut t, ".env");
    drain_files(&mut t);
    t.ch('v');
    assert!(!t.app.files_tab.masked());
    assert_eq!(drain_files(&mut t), ["file .env (reveal)"]);
    assert_eq!(viewing_text(&t), Some(format!("TOKEN={FAKE_SECRET}\n")));
    // v again hides it and drops the content
    t.ch('v');
    assert!(t.app.files_tab.masked());
    assert!(viewing_text(&t).is_none());
    assert!(drain_files(&mut t).is_empty());
    // v on an ordinary file does nothing
    select(&mut t, "README.md");
    drain_files(&mut t);
    t.ch('v');
    assert!(labels(t.app.take_requests_peek()).is_empty());
}

#[test]
fn the_reveal_ends_with_the_selection_and_with_the_tab() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, ".env");
    t.ch('v');
    drain_files(&mut t);
    assert!(viewing_text(&t).is_some());
    select(&mut t, "README.md");
    drain_files(&mut t);
    select(&mut t, ".env");
    assert!(t.app.files_tab.masked(), "back on the secret: hidden again");
    assert!(drain_files(&mut t).iter().all(|s| !s.starts_with("file ")));
    // leaving the tab hides it too
    t.ch('v');
    drain_files(&mut t);
    assert!(viewing_text(&t).is_some());
    t.ch('1');
    t.ch('3');
    drain_files(&mut t);
    assert!(t.app.files_tab.masked());
    assert!(viewing_text(&t).is_none());
}

#[test]
fn a_reply_that_lands_after_the_reveal_was_undone_is_not_shown() {
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, ".env");
    drain_files(&mut t);
    t.ch('v');
    let reveal_read = t.app.take_requests();
    assert_eq!(labels(&reveal_read), ["file .env (reveal)"]);
    t.ch('v');
    for m in t.exec_all(reveal_read) {
        t.app.handle_msg(m);
    }
    assert!(t.app.files_tab.masked());
    assert!(viewing_text(&t).is_none());
}

#[test]
fn e_opens_the_selected_file_in_the_editor_even_a_masked_one() {
    use gitty::external::External;
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, ".env");
    t.ch('e');
    assert_eq!(t.app.external, Some(External::Edit { path: f.path().join(".env"), line: None }));
    t.app.external = None;
    select(&mut t, "src");
    t.ch('e');
    assert_eq!(t.app.external, None, "a directory is not opened");
    t.key(KeyCode::Enter);
    t.pump();
    select(&mut t, "main.rs");
    t.ch('e');
    assert_eq!(t.app.external, Some(External::Edit { path: f.path().join("src/main.rs"), line: None }));
}

#[test]
fn binary_and_large_files_say_so_without_loading() {
    use gitty::app::files::Viewing;
    use gitty::msg::FileView;
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, "data.bin");
    drain_files(&mut t);
    assert!(matches!(t.app.files_tab.viewing, Viewing::Ready(FileView::Binary { size: 5 })));
    select(&mut t, "big.txt");
    drain_files(&mut t);
    assert!(matches!(t.app.files_tab.viewing, Viewing::Ready(FileView::TooLarge { size }) if size == 2 * 1024 * 1024 + 1));
}

#[test]
fn a_worktree_change_refreshes_the_open_tab_only() {
    use gitty_core::watch::Changed;
    let f = files_fixture();
    let mut t = files_tab(&f);
    select(&mut t, "README.md");
    drain_files(&mut t);
    f.write("README.md", "# changed\n");
    f.write("added.txt", "x\n");
    t.app.handle_msg(Msg::Changed(Changed::WORKTREE));
    assert_eq!(labels(t.app.take_requests_peek()), ["dir ", "file README.md"]);
    t.pump();
    assert!(rows(&t).contains(&"added.txt".to_string()));
    assert_eq!(viewing_text(&t).as_deref(), Some("# changed\n"));
    t.ch('2');
    t.app.take_requests();
    t.app.handle_msg(Msg::Changed(Changed::WORKTREE));
    assert!(labels(t.app.take_requests_peek()).is_empty(), "other tabs do not list the tree");
}

#[test]
fn an_unreadable_directory_is_an_inline_error_row() {
    use std::os::unix::fs::PermissionsExt;
    let f = files_fixture();
    let mut t = files_tab(&f);
    let dir = f.path().join("src");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    t.key(KeyCode::Enter);
    t.pump();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    // running as root can read it anyway
    if rows(&t)[1] != "  deep" {
        assert!(rows(&t)[1].trim_start().starts_with("cannot read"), "{:?}", rows(&t));
        assert!(matches!(t.app.files_tab.rows[1].kind, gitty::app::files::RowKind::Note { error: true }));
        // the rest keeps working
        select(&mut t, "README.md");
        t.pump();
        assert!(viewing_text(&t).is_some());
    }
}
