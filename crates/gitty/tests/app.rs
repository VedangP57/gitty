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
use gitty::msg::{DiffKey, Gens, HlKey, Msg, Request};
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
    let requested = r.iter().any(|r| matches!(r, Request::Files { id, prefetch: false, .. } if *id == CommitId::from_hex(&ids[3]).unwrap()));
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
    t.app.handle_msg(Msg::Files { generation: stale_gen, id: id(&ids[3]), files: Arc::new(vec![]), prefetch: false });
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
    t.app.handle_msg(Msg::FilesError { generation: 0, id: id(&ids[13]), prefetch: true, detail: "boom".into() });
    assert_eq!(t.app.prefetch_in_flight(), 9, "a failed prefetch frees its slot");
    assert!(t.app.toast.is_none(), "prefetch failures are not the user's problem");
    t.ch('G');
    let generation = t.gens.commit.load(std::sync::atomic::Ordering::SeqCst);
    let sel = t.selected_id();
    t.app.handle_msg(Msg::FilesError { generation, id: sel, prefetch: false, detail: "object not found".into() });
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
                gitty::msg::WriteOp::UndoCommit => "undo".into(),
                gitty::msg::WriteOp::RefreshIndex => "refresh index".into(),
                gitty::msg::WriteOp::Seq(ops) => format!("seq of {}", ops.len()),
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
fn rejected_push_says_pull_first() {
    let (f, bare) = remote_fixture();
    common::push_as_someone_else(&bare, "b.txt");
    f.write("c.txt", "mine\n");
    f.commit("mine", 1_700_000_100);
    let mut t = H::new(&f);
    t.pump();
    t.ch('P');
    t.pump();
    let toast = t.app.toast.clone().unwrap();
    assert!(toast.error && toast.what.contains("pull first"), "{toast:?}");
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
