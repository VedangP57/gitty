#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Fixture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use gitty::app::{App, AppInit, Focus};
use gitty::config::{Config, GraphStyle, UiState};
use gitty::exec::exec;
use gitty::msg::{Gens, Msg};
use gitty::theme::{ColorDepth, Registry};
use gitty::ui;
use gitty_core::{Handle, Repo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;

const NOW: i64 = 1_790_899_200;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

struct H {
    app: App,
    h: Handle,
    gens: Arc<Gens>,
    clock: Instant,
}

impl H {
    fn new(f: &Fixture, theme: &str, size: (u16, u16)) -> H {
        let repo = Repo::open(f.path()).unwrap();
        let registry = Registry::load(None);
        let theme = registry.resolve(theme, ColorDepth::True, None).unwrap();
        let gens = Arc::new(Gens::default());
        let clock = Instant::now();
        let app = App::new(AppInit {
            repo_name: "repo".into(),
            // one line per commit unless a test asks for the roomy graph (see `roomy`)
            config: Config { history_graph_style: GraphStyle::Compact, ..Config::default() },
            registry,
            theme,
            depth: ColorDepth::True,
            ui_state: UiState::default(),
            config_path: None,
            state_path: None,
            gens: gens.clone(),
            now: NOW,
            clock,
            size,
        });
        let mut t = H { app, h: repo.handle(), gens, clock };
        t.pump();
        t
    }
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
            let mut out: Vec<Msg> = Vec::new();
            for r in reqs {
                exec(&self.h, r, &mut |m| out.push(m), &self.gens);
            }
            for m in out {
                self.app.handle_msg(m);
            }
        }
        panic!("pump did not settle");
    }
    /// Like [`H::pump`] but ignores timers: the focused status backstop re-arms forever.
    fn drain(&mut self) {
        for _ in 0..1000 {
            let reqs = self.app.take_requests();
            if reqs.is_empty() {
                return;
            }
            let mut out: Vec<Msg> = Vec::new();
            for r in reqs {
                exec(&self.h, r, &mut |m| out.push(m), &self.gens);
            }
            for m in out {
                self.app.handle_msg(m);
            }
        }
        panic!("drain did not settle");
    }
    fn key(&mut self, c: KeyCode) {
        self.app.handle_key(KeyEvent::new(c, KeyModifiers::NONE));
        self.pump();
    }
    fn render(&mut self, w: u16, h: u16) -> Buffer {
        self.app.handle_resize(w, h);
        self.pump();
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(&mut self.app, f)).unwrap();
        term.backend().buffer().clone()
    }
    fn select_file(&mut self, path: &str) {
        let i = self.app.files.as_ref().unwrap().iter().position(|f| f.path == path).unwrap_or_else(|| panic!("no {path}"));
        self.app.select_file(i);
        self.pump();
        assert_eq!(self.app.diff.as_ref().map(|d| d.key.path.as_str()), Some(path));
    }
    fn select_change(&mut self, path: &str) {
        let i = self.app.changes.visible().iter().position(|&i| self.app.changes.entries()[i].path == path).unwrap_or_else(|| panic!("no {path}"));
        self.app.select_change(i);
        self.pump();
        assert_eq!(self.app.diff.as_ref().map(|d| d.key.path.as_str()), Some(path));
    }
    fn click(&mut self, x: u16, y: u16) {
        let m = |kind| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
        self.app.handle_mouse(m(MouseEventKind::Down(MouseButton::Left)));
        self.app.handle_mouse(m(MouseEventKind::Up(MouseButton::Left)));
        self.pump();
    }
}

fn text(b: &Buffer) -> String {
    let mut s = String::new();
    for y in 0..b.area.height {
        let mut line = String::new();
        let mut skip = 0;
        for x in 0..b.area.width {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let sym = b[(x, y)].symbol();
            skip = gitty::text::display_width(sym).saturating_sub(1);
            line.push_str(sym);
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

fn col(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("{r:02x}{g:02x}{b:02x}"),
        Color::Reset => "-".into(),
        c => format!("{c:?}"),
    }
}

/// One line per row: runs of identical (fg/bg) as `count:fg/bg`.
fn digest(b: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..b.area.height {
        let mut runs: Vec<(String, usize)> = Vec::new();
        for x in 0..b.area.width {
            let c = &b[(x, y)];
            let k = format!("{}/{}", col(c.fg), col(c.bg));
            match runs.last_mut() {
                Some((last, n)) if *last == k => *n += 1,
                _ => runs.push((k, 1)),
            }
        }
        let line: Vec<String> = runs.into_iter().map(|(k, n)| format!("{n}:{k}")).collect();
        out.push_str(&format!("{y:2} {}\n", line.join(" ")));
    }
    out
}

fn main_rs(variant: u32) -> String {
    let mut s = String::from("use std::io;\n\nfn main() {\n");
    for i in 0..30 {
        let line = match (variant, i) {
            (1, 5) => "    let total = compute(5, 7);".to_string(),
            (1, 22) => "    println!(\"done: {}\", total);".to_string(),
            _ => format!("    step({i});"),
        };
        s.push_str(&line);
        s.push('\n');
    }
    s.push_str("}\n");
    s
}

/// Deterministic history: dates relative to NOW, an upstream, a tag, a side branch.
fn fixture() -> Fixture {
    let f = Fixture::new();
    f.write("src/main.rs", main_rs(0));
    f.write("README.md", "# demo\n");
    f.commit("Initial commit", NOW - 40 * DAY);
    f.write("src/main.rs", main_rs(1));
    f.write("src/util.rs", "pub fn compute(a: i32, b: i32) -> i32 {\n    a * b\n}\n");
    f.write("docs/a/very/deeply/nested/directory/structure/notes.md", "notes\n");
    f.commit("Add compute helper and print the total at the end of main", NOW - 3 * DAY);
    f.git(&["tag", "v1.0"]);
    f.add_bare_upstream();
    f.write("README.md", "# demo\n\nA demo repository.\n");
    f.commit("Describe the project", NOW - 5 * HOUR);
    let d = format!("{} +0000", NOW - 12 * 60);
    f.git_env(&["commit", "-q", "--allow-empty", "--allow-empty-message", "-m", ""], &[("GIT_AUTHOR_DATE", d.clone()), ("GIT_COMMITTER_DATE", d)]);
    f
}

#[test]
fn snapshots() {
    let f = fixture();
    for theme in ["github-dark", "github-light"] {
        for w in [100u16, 140, 180, 220] {
            let mut t = H::new(&f, theme, (w, 30));
            t.app.select(1);
            t.pump();
            if w < 120 {
                t.app.focus = Focus::History;
            }
            let b = t.render(w, 30);
            let name = format!("{w}_{theme}");
            insta::assert_snapshot!(format!("{name}_text"), text(&b));
            insta::assert_snapshot!(format!("{name}_style"), digest(&b));
        }
    }
}

#[test]
fn narrow_drilldown_enter_esc() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (100, 30));
    t.app.select(2);
    t.pump();
    let b = t.render(100, 30);
    assert!(text(&b).contains("Add compute helper"));
    t.key(KeyCode::Enter);
    let s = text(&t.render(100, 30));
    assert!(s.contains("changed files"), "{s}");
    assert!(s.contains("util.rs"));
    t.key(KeyCode::Enter);
    let s = text(&t.render(100, 30));
    assert!(s.contains("@@"), "{s}");
    t.key(KeyCode::Esc);
    t.key(KeyCode::Esc);
    let s = text(&t.render(100, 30));
    assert!(s.contains("Initial commit"), "{s}");
    let insta_narrow = s;
    insta::assert_snapshot!("narrow_back_to_history", insta_narrow);
}

fn find(b: &Buffer, needle: &str) -> Option<(u16, u16)> {
    for y in 0..b.area.height {
        let mut line = String::new();
        let mut xs = Vec::new();
        for x in 0..b.area.width {
            for _ in b[(x, y)].symbol().chars() {
                xs.push(x);
            }
            line.push_str(b[(x, y)].symbol());
        }
        if let Some(i) = line.find(needle) {
            let ci = line[..i].chars().count();
            return Some((xs[ci], y));
        }
    }
    None
}

fn with_pr(t: &mut H, number: u64, state: gitty_core::forge::PrState) {
    let branch = t.app.refs.as_ref().unwrap().head_branch().unwrap().to_string();
    let url = format!("https://github.com/o/r/pull/{number}");
    t.app.handle_msg(Msg::PrBadge { branch, result: Ok(Some(gitty_core::forge::PrInfo { number, state, url })) });
}

#[test]
fn the_pull_request_badge_is_coloured_by_state_and_underlined() {
    use gitty_core::forge::PrState;
    use ratatui::style::Modifier;
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    let ui = t.app.theme.ui.clone();
    let (open, draft, merged, closed) = (ui.pr_open, ui.pr_draft, ui.pr_merged, ui.pr_closed);
    assert_eq!(open, ui.status_added, "green");
    assert_eq!(draft, ui.muted);
    assert_eq!(merged, t.app.theme.avatar[4], "magenta");
    assert_eq!(closed, ui.error, "red");
    let all = [open, draft, merged, closed];
    assert!(all.iter().enumerate().all(|(i, c)| all.iter().skip(i + 1).all(|d| c != d)), "four distinct colours");
    assert!(find(&t.render(140, 30), "PR #").is_none(), "no pull request, no badge");
    for (state, color) in [(PrState::Open, open), (PrState::Draft, draft), (PrState::Merged, merged), (PrState::Closed, closed)] {
        with_pr(&mut t, 42, state);
        let b = t.render(140, 30);
        let (x, y) = find(&b, "PR #42").unwrap_or_else(|| panic!("{state:?}: {}", text(&b)));
        let line: String = (0..140).map(|x| b[(x, 0)].symbol().to_string()).collect();
        assert!(line.contains("↑") && line.find("PR #42") > line.find("↓"), "after the ahead/behind marks: {line}");
        for dx in 0..6 {
            let c = &b[(x + dx, y)];
            assert_eq!(c.fg, color, "{state:?} cell {dx}");
            assert_eq!(c.bg, ui.status_bg);
        }
        // the number is the link
        assert!(!b[(x, y)].modifier.contains(Modifier::UNDERLINED), "PR");
        assert!((3..6).all(|dx| b[(x + dx, y)].modifier.contains(Modifier::UNDERLINED)), "#42");
        assert_eq!(t.app.hits.pr_badge, Some(ratatui::layout::Rect::new(x, y, 6, 1)));
    }
}

#[test]
fn the_pull_request_badge_gives_way_at_80_columns() {
    use gitty_core::forge::PrState;
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (80, 24));
    with_pr(&mut t, 12345, PrState::Open);
    let wide = text(&t.render(140, 30));
    assert!(wide.lines().next().unwrap().contains("PR #12345"));
    let b = t.render(80, 24);
    let top: String = (0..80).map(|x| b[(x, 0)].symbol().to_string()).collect();
    assert!(top.contains("PR #12345"), "{top}");
    assert!(top.contains("[2] History"), "{top}");
    assert!(!top.contains("fetched"), "the age gives way first: {top}");
    // narrower still: the badge goes before the branch name or the marks are cut
    let mut dropped = false;
    for w in (30..=80).rev() {
        let b = t.render(w, 24);
        let top: String = (0..w).map(|x| b[(x, 0)].symbol().to_string()).collect();
        let badge = top.contains("PR #12345");
        assert_eq!(badge, t.app.hits.pr_badge.is_some(), "{w}: {top}");
        if badge {
            assert!(top.contains("⎇ main  ↑") && top.contains("↓"), "{w}: {top}");
        } else {
            dropped = true;
        }
    }
    assert!(dropped);
}

#[test]
fn clicking_the_pull_request_badge_opens_it() {
    use gitty_core::forge::PrState;
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.render(140, 30);
    // nothing there yet: the same place is just the bar
    t.click(40, 0);
    assert_eq!(t.app.open_url, None);
    with_pr(&mut t, 42, PrState::Merged);
    let b = t.render(140, 30);
    let (x, y) = find(&b, "PR #42").unwrap();
    t.click(x + 4, y);
    assert_eq!(t.app.open_url.as_deref(), Some("https://github.com/o/r/pull/42"));
    t.app.open_url = None;
    t.click(x + 7, y);
    assert_eq!(t.app.open_url, None, "only the badge is a link");
}

#[test]
fn diff_rows_have_full_width_backgrounds() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 40));
    t.app.select(2);
    t.pump();
    t.select_file("src/main.rs");
    let b = t.render(180, 40);
    let diff = t.app.hits.diff_rows.unwrap();
    let th = t.app.theme.diff.clone();
    let (_, y) = find(&b, "-    step(5);").or_else(|| find(&b, "    step(5);")).expect("deleted line drawn");
    for x in diff.x..diff.right() {
        let bg = b[(x, y)].bg;
        assert!(bg == th.del_bg || bg == th.del_gutter || bg == th.del_emph, "x={x} bg={bg:?}");
    }
    let (_, y) = find(&b, "compute(5, 7)").expect("added line drawn");
    for x in diff.x..diff.right() {
        let bg = b[(x, y)].bg;
        assert!(bg == th.add_bg || bg == th.add_gutter || bg == th.add_emph, "x={x} bg={bg:?}");
    }
}

#[test]
fn emphasis_marks_changed_word_only() {
    let f = Fixture::new();
    f.write("a.txt", "let value = old_name(1);\n");
    f.commit("one", NOW - DAY);
    f.write("a.txt", "let value = new_name(1);\n");
    f.commit("two", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (180, 30));
    let b = t.render(180, 30);
    let th = t.app.theme.diff.clone();
    let (x, y) = find(&b, "new_name").unwrap();
    assert_eq!(b[(x, y)].bg, th.add_emph);
    let (vx, _) = find(&b, "value = new").unwrap();
    assert_eq!(b[(vx, y)].bg, th.add_bg, "unchanged words keep the row background");
}

#[test]
fn an_added_or_deleted_file_uses_the_full_width_even_in_split_view() {
    let f = Fixture::new();
    f.write("keep.txt", "a\nold\nc\n");
    f.write("gone.txt", "bye\n");
    f.commit("base", NOW - DAY);
    f.write("keep.txt", "a\nnew\nc\n");
    f.write("fresh.txt", "a brand new line that is long enough to need more than half the pane\n");
    f.git(&["rm", "-q", "gone.txt"]);
    f.git(&["add", "-A"]);
    f.commit("change", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (220, 30));
    t.app.select(0);
    t.pump();
    t.select_file("keep.txt");
    t.render(220, 30);
    assert!(t.app.split_active(), "both sides: split");
    for path in ["fresh.txt", "gone.txt"] {
        t.select_file(path);
        let b = t.render(220, 30);
        assert!(!t.app.split_active(), "{path}: one side only, so unified");
        assert!(!text(&b).contains("· split"), "{path}: the title does not say split");
    }
    let b = t.render(220, 30);
    let (x, _) = find(&b, "bye").expect("the deleted line");
    let d = t.app.hits.panes.diff.unwrap();
    assert!(x < d.x + d.width / 2, "starts in the left half, not after an empty one");
    // s still switches the next two-sided file
    t.key(KeyCode::Char('s'));
    t.select_file("keep.txt");
    t.render(220, 30);
    assert!(!t.app.split_active(), "s turned split off");
}

#[test]
fn an_added_or_deleted_file_has_one_line_number_column() {
    let f = Fixture::new();
    f.write("keep.txt", "a\nold\nc\n");
    f.write("gone.txt", "bye now\n");
    f.commit("base", NOW - DAY);
    f.write("keep.txt", "a\nnew\nc\n");
    f.write("fresh.txt", "hello there\n");
    f.git(&["rm", "-q", "gone.txt"]);
    f.git(&["add", "-A"]);
    f.commit("change", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.select(0);
    t.pump();
    for (path, line) in [("fresh.txt", "hello there"), ("gone.txt", "bye now")] {
        t.select_file(path);
        let b = t.render(140, 30);
        let (og, ng) = (t.app.hits.diff_old_gutter, t.app.hits.diff_new_gutter);
        assert_eq!(og, ng, "{path}: one number column, which clicks on either side reach");
        let (x, _) = find(&b, line).unwrap();
        assert_eq!(x, ng.0 + ng.1 + 2, "{path}: the code follows that one column and the marker");
        let (hx, _) = find(&b, "@@").unwrap();
        assert_eq!(hx, x, "{path}: the hunk header lines up with the code");
    }
    t.select_file("keep.txt");
    t.render(140, 30);
    let (og, ng) = (t.app.hits.diff_old_gutter, t.app.hits.diff_new_gutter);
    assert_eq!(ng.0, og.0 + og.1, "a two-sided file keeps both columns");
}

#[test]
fn split_auto_at_220_unified_at_180() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (220, 40));
    t.app.select(2);
    t.pump();
    t.select_file("src/main.rs");
    t.render(220, 40);
    assert!(t.app.split_active());
    t.render(180, 40);
    assert!(!t.app.split_active());
    t.key(KeyCode::Char('s'));
    assert!(t.app.split_active());
    let s = text(&t.render(180, 40));
    assert!(s.contains("step(5);") && s.contains("compute(5, 7)"));
}

#[test]
fn a_split_gap_label_stays_left_of_the_divider() {
    let f = fixture();
    for w in [120u16, 140, 180] {
        let mut t = H::new(&f, "github-dark", (w, 40));
        t.app.select(2);
        t.pump();
        t.select_file("src/main.rs");
        t.app.split_pref = Some(true);
        let s = text(&t.render(w, 40));
        assert!(t.app.split_active(), "{w}: split");
        let gaps: Vec<&str> = s.lines().filter(|l| l.contains('⋯')).collect();
        assert!(!gaps.is_empty(), "{w}: a gap row is drawn");
        for row in gaps {
            let right = &row[row.rfind('│').expect("the split divider") + '│'.len_utf8()..];
            assert!(["", "↑", "↓"].contains(&right.trim()), "{w}: the label runs past the divider: {row:?}");
        }
    }
}

#[test]
fn gap_row_click_expands() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 40));
    t.app.select(2);
    t.pump();
    t.select_file("src/main.rs");
    let b = t.render(180, 40);
    let before = t.app.diff.as_ref().unwrap().rows(false);
    let (x, y) = find(&b, "⋯").expect("a gap row is drawn");
    t.click(x + 4, y);
    let after = t.app.diff.as_ref().unwrap().rows(false);
    assert!(after > before, "{before} → {after}");
    // click on the ↑ handle of the remaining gap row
    let b = t.render(180, 40);
    if let Some((x, y)) = find(&b, "↑") {
        let before = t.app.diff.as_ref().unwrap().rows(false);
        t.click(x, y);
        assert!(t.app.diff.as_ref().unwrap().rows(false) >= before);
    }
}

#[test]
fn a_column_zero_gap_click_expands_where_no_hunk_handle_is_drawn() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 40));
    t.app.select(2);
    t.pump();
    t.select_file("src/main.rs");
    let b = t.render(180, 40);
    let (_, y) = find(&b, "⋯").expect("a gap row is drawn");
    let x0 = t.app.hits.diff_rows.expect("diff drawn").x;
    let before = t.app.diff.as_ref().unwrap().rows(false);
    t.click(x0, y);
    assert!(t.app.diff.as_ref().unwrap().rows(false) > before, "History draws no hunk handle: column 0 is the gap's ↑");
    assert!(t.app.toast.is_none(), "{:?}", t.app.toast.as_ref().map(|t| &t.what));
}

/// Two branches merged into main, then an octopus merge of three more, dated relative to NOW.
fn graph_fixture() -> Fixture {
    let f = Fixture::new();
    let mut t = NOW - 30 * DAY;
    let mut commit = |f: &Fixture, msg: &str| {
        t += DAY;
        f.write(&format!("{}.txt", msg.replace(' ', "_")), "x\n");
        f.commit(msg, t)
    };
    let merge = |f: &Fixture, args: &[&str], at: i64| {
        let d = format!("{at} +0000");
        let mut all = vec!["merge", "-q", "--no-ff", "--no-edit"];
        all.extend_from_slice(args);
        f.git_env(&all, &[("GIT_AUTHOR_DATE", d.clone()), ("GIT_COMMITTER_DATE", d)]);
    };
    commit(&f, "Initial commit");
    f.git(&["switch", "-q", "-c", "feature"]);
    commit(&f, "Add the parser");
    commit(&f, "Parse nested lists");
    f.git(&["switch", "-q", "main"]);
    commit(&f, "Fix the build");
    merge(&f, &["feature"], NOW - 20 * DAY);
    for b in ["docs", "ci", "lint"] {
        f.git(&["switch", "-q", "-c", b, "main"]);
        commit(&f, &format!("Update {b}"));
    }
    f.git(&["switch", "-q", "main"]);
    merge(&f, &["docs", "ci", "lint"], NOW - 10 * DAY);
    commit(&f, "Release 1.0");
    f
}

const GRAPH_STYLES: [(GraphStyle, &str); 2] = [(GraphStyle::Roomy, "roomy"), (GraphStyle::Compact, "compact")];

#[test]
fn the_commit_graph_at_80_and_140_columns() {
    let f = graph_fixture();
    for (style, name) in GRAPH_STYLES {
        for w in [80u16, 140] {
            let mut t = H::new(&f, "github-dark", (w, 30));
            t.app.config.history_graph_style = style;
            t.app.focus = Focus::History;
            let b = t.render(w, 30);
            let s = text(&b);
            assert!(s.contains("◉─┬─┬─╮"), "the octopus opens its lines on its own row: {s}");
            assert!(s.contains("◎ Release 1.0"), "HEAD has its own node: {s}");
            assert!(s.contains('┃'), "the current branch is heavy: {s}");
            assert!(s.contains("Release 1.0") && s.contains("Parse nested lists"), "subjects stay readable: {s}");
            insta::assert_snapshot!(format!("graph_{name}_{w}_text"), s);
            insta::assert_snapshot!(format!("graph_{name}_{w}_style"), digest(&b));
        }
    }
}

#[test]
fn a_43_column_history_pane_keeps_the_graph() {
    let f = graph_fixture();
    for (style, name) in GRAPH_STYLES {
        let mut t = H::new(&f, "github-dark", (180, 30));
        t.app.config.history_graph_style = style;
        t.app.ui_state.history_width = Some(43);
        let b = t.render(180, 30);
        assert_eq!(t.app.hits.panes.history.map(|r| r.width), Some(43));
        let s = text(&b);
        assert!(s.contains('●'), "{s}");
        // the subjects keep their room, cut with an ellipsis
        assert!(s.contains("Merge branches") && s.contains('…'), "{s}");
        let pane: String = s.lines().map(|l| l.chars().take(43).collect::<String>().trim_end().to_string() + "\n").collect();
        insta::assert_snapshot!(format!("graph_{name}_43_pane"), pane);
    }
}

#[test]
fn roomy_rows_continue_every_lane_and_carry_the_second_line() {
    let f = graph_fixture();
    let mut t = H::new(&f, "github-dark", (80, 30));
    t.app.config.history_graph_style = GraphStyle::Roomy;
    t.app.focus = Focus::History;
    let b = t.render(80, 30);
    let s = text(&b);
    let lines: Vec<&str> = s.lines().collect();
    let (_, y) = find(&b, "Release 1.0").unwrap();
    // HEAD's node, then its heavy line on with the author and date beside it
    assert!(lines[y as usize].contains("◎ Release 1.0"), "{s}");
    assert!(lines[y as usize + 1].contains("┃ TU Test User"), "{s}");
    let (_, y) = find(&b, "Update lint").unwrap();
    assert!(lines[y as usize + 1].contains("┃ │ │ │ TU Test User"), "{s}");
    // a click on either line selects the commit
    let (_, y) = find(&b, "Update ci").unwrap();
    t.click(40, y + 1);
    let sel = t.app.selected;
    t.click(40, y);
    assert_eq!(t.app.selected, sel);
    assert_eq!(t.app.rows.get(&sel).map(|r| r.summary.as_str()), Some("Update ci"));
}

#[test]
fn branch_labels_take_their_commits_lane_colour() {
    let f = graph_fixture();
    let mut t = H::new(&f, "github-dark", (80, 24));
    t.app.focus = Focus::History;
    let b = t.render(80, 24);
    let lanes = t.app.theme.ui.lanes;
    for label in ["lint", "docs"] {
        let (_, y) = find(&b, &format!("Update {label}")).unwrap();
        let line: String = (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect();
        let x = line.rfind(&format!(" {label} ")).unwrap();
        let x = line[..x].chars().count() as u16 + 1;
        let node = (3..20).find(|&x| gitty_core::graph::is_node(b[(x, y)].symbol().chars().next().unwrap_or(' '))).unwrap();
        assert_eq!(b[(x, y)].bg, b[(node, y)].fg, "{label}: the pill is the lane's colour");
        assert!(lanes.contains(&b[(x, y)].bg));
    }
}

#[test]
fn comfortable_rows_carry_the_lanes_through_their_second_line() {
    let f = graph_fixture();
    let mut t = H::new(&f, "github-dark", (80, 30));
    t.app.focus = Focus::History;
    t.key(KeyCode::Char('z'));
    insta::assert_snapshot!("graph_80_comfortable", text(&t.render(80, 30)));
}

#[test]
fn the_graph_leaves_with_a_search_and_comes_back() {
    let f = graph_fixture();
    let mut t = H::new(&f, "github-dark", (80, 24));
    t.app.focus = Focus::History;
    assert!(text(&t.render(80, 24)).contains('◉'));
    t.key(KeyCode::Char('/'));
    for c in "parse".chars() {
        t.key(KeyCode::Char(c));
    }
    t.key(KeyCode::Enter);
    let s = text(&t.render(80, 24));
    assert!(!s.contains('●') && !s.contains('│'), "{s}");
    assert!(s.contains("Parse nested lists"));
    t.key(KeyCode::Esc);
    assert!(text(&t.render(80, 24)).contains('◉'));
    // too narrow for it: the subjects keep the room
    let s = text(&t.render(33, 24));
    assert!(!s.contains('●'), "{s}");
}

#[test]
fn each_subject_starts_right_after_its_own_rows_graph() {
    let f = graph_fixture();
    let mut t = H::new(&f, "github-dark", (80, 24));
    t.app.focus = Focus::History;
    let b = t.render(80, 24);
    let s = text(&b);
    // (subject, its row's graph): the subject one column after the graph's last cell
    let mut ends = Vec::new();
    for (subject, graph) in [("Release 1.0", "◎"), ("Merge branches", "◉─┬─┬─╮"), ("Update lint", "┃ │ │ ●"), ("Initial commit", "●─╯")] {
        let (x, y) = find(&b, subject).unwrap_or_else(|| panic!("{subject}: {s}"));
        let line = s.lines().nth(y as usize).unwrap();
        assert_eq!(x as usize, 3 + graph.chars().count() + 1, "{subject}: {line}");
        assert!(line.chars().skip(3).collect::<String>().starts_with(&format!("{graph} {subject}")), "{line}");
        // the dates stay right-aligned whatever the graph's width
        ends.push(line.trim_end().chars().count());
    }
    assert!(ends.iter().all(|&e| e == ends[0]), "{ends:?}\n{s}");
}

#[test]
fn lanes_wider_than_the_pane_are_cut_with_a_marker() {
    let f = Fixture::new();
    f.write("a.txt", "x\n");
    f.commit("Initial commit", NOW - 30 * DAY);
    for i in 0..12 {
        f.git(&["switch", "-q", "-c", &format!("b{i}"), "main"]);
        f.write(&format!("b{i}.txt"), "x\n");
        f.commit(&format!("Branch {i}"), NOW - 20 * DAY + i * DAY);
    }
    f.git(&["switch", "-q", "main"]);
    let mut args = vec!["merge".to_string(), "-q".into(), "--no-edit".into()];
    args.extend((0..12).map(|i| format!("b{i}")));
    f.git(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let mut t = H::new(&f, "github-dark", (180, 24));
    t.app.ui_state.history_width = Some(43);
    let s = text(&t.render(180, 24));
    let pane: Vec<String> = s.lines().map(|l| l.chars().take(43).collect()).collect();
    assert!(pane.iter().any(|l| l.contains('›')), "{}", pane.join("\n"));
    assert!(pane.iter().any(|l| l.contains("Branch 0")), "the subjects keep their room: {}", pane.join("\n"));
    insta::assert_snapshot!("graph_43_clipped", pane.iter().map(|l| l.trim_end().to_string() + "\n").collect::<String>());
}

#[test]
fn tiny_sizes_never_panic() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 40));
    t.app.select(2);
    t.pump();
    t.select_file("src/main.rs");
    for split in [false, true] {
        t.app.split_pref = Some(split);
        for focus in [Focus::History, Focus::Files, Focus::Diff] {
            t.app.focus = focus;
            for w in 1..=45 {
                for h in 1..=8 {
                    t.render(w, h);
                }
            }
            for w in [119u16, 120, 159, 160, 200] {
                for h in 1..=6 {
                    t.render(w, h);
                }
            }
        }
    }
    t.app.header_expanded = true;
    t.app.fullscreen = true;
    for w in 1..=30 {
        t.render(w, 3);
    }
}

#[test]
fn hostile_text_stays_in_rect() {
    let f = Fixture::new();
    f.write("evil.txt", "x\n");
    f.commit("one", NOW - DAY);
    let mut evil = Vec::new();
    evil.extend_from_slice(b"esc \x1b[31mred\x1b[0m and \x1b]0;title\x07 osc\n");
    evil.extend_from_slice(b"bell \x07 del \x7f cr \r mid\n");
    evil.extend_from_slice(b"\ttab\t\tdeep\n");
    evil.extend_from_slice("日本語のテキストと絵文字👩‍💻👍🏽 wide\n".as_bytes());
    evil.extend_from_slice(b"bad utf8 \xff\xfe\xc3 end\n");
    evil.extend_from_slice("\u{202e}reversed\u{202c}\n".as_bytes());
    evil.extend(std::iter::repeat_n(b'w', 100_000));
    evil.push(b'\n');
    std::fs::write(f.path().join("evil.txt"), &evil).unwrap();
    f.commit("summary with \x1b[2J escape", NOW - HOUR);
    for (w, split) in [(180u16, false), (220, true), (100, false)] {
        let mut t = H::new(&f, "github-dark", (w, 40));
        t.app.split_pref = Some(split);
        t.app.focus = if w < 120 { Focus::Diff } else { Focus::History };
        t.render(w, 40);
        t.app.force_show();
        t.pump();
        let started = Instant::now();
        let b = t.render(w, 40);
        for hs in [0u16, 7, 13, 9_990] {
            t.app.diff.as_mut().unwrap().hscroll = hs;
            let b = t.render(w, 40);
            for c in b.content() {
                assert!(!c.symbol().chars().any(|ch| ch.is_control()), "control char in cell: {:?}", c.symbol());
            }
        }
        assert!(started.elapsed() < Duration::from_secs(2), "rendering hostile text took {:?}", started.elapsed());
        let s = text(&b);
        assert!(s.contains("^["), "escapes drawn as caret notation:\n{s}");
        if w >= 120 {
            assert!(s.contains("summary with ^[[2J escape"), "{s}");
        }
        assert!(t.app.diff.as_ref().unwrap().diff.is_text(), "the fixture must stay a text diff");
        if split {
            let d = t.app.hits.diff_rows.unwrap();
            let mid = t.app.hits.diff_new_gutter.0 - 1;
            for y in d.y..d.bottom() {
                let sym = b[(mid, y)].symbol();
                assert!(sym == "│" || t.app.diff.as_ref().is_some_and(|_| sym == " " || !sym.is_empty()), "divider overwritten at y={y}: {sym:?}");
            }
        }
    }
}

fn special(f: &Fixture) {
    f.write("img.bin", b"\x00\x01\x02binary");
    f.write("same.txt", "unchanged content\n");
    f.write("run.sh", "echo hi\n");
    f.write("package-lock.json", "{}\n");
    f.commit("base", NOW - 2 * DAY);
    f.write("img.bin", b"\x00\x01\x03binary!");
    f.git(&["mv", "same.txt", "moved.txt"]);
    f.git(&["update-index", "--chmod=+x", "run.sh"]);
    let mut lock = String::from("{\n");
    for i in 0..200 {
        lock.push_str(&format!("  \"pkg{i}\": \"1.0.{i}\",\n"));
    }
    lock.push_str("}\n");
    f.write("package-lock.json", lock);
    f.git(&["add", "img.bin", "package-lock.json"]);
    f.git(&["update-index", "--add", "--cacheinfo", "160000,1111111111111111111111111111111111111111,vendor/lib"]);
    f.git_env(
        &["commit", "-q", "-m", "special files"],
        &[("GIT_AUTHOR_DATE", format!("{} +0000", NOW - HOUR)), ("GIT_COMMITTER_DATE", format!("{} +0000", NOW - HOUR))],
    );
}

#[test]
fn special_classes_show_messages() {
    let f = Fixture::new();
    special(&f);
    let mut t = H::new(&f, "github-dark", (180, 30));
    for (path, want) in [
        ("img.bin", "Binary file"),
        ("moved.txt", "No content changes"),
        ("run.sh", "Mode changed 100644 → 100755"),
        ("package-lock.json", "press Enter to show"),
        ("vendor/lib", "Submodule vendor/lib: none..1111111"),
    ] {
        t.select_file(path);
        let s = text(&t.render(180, 30));
        assert!(s.contains(want), "{path}: wanted {want:?}\n{s}");
    }
    t.select_file("package-lock.json");
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Enter);
    let s = text(&t.render(180, 30));
    assert!(s.contains("pkg5"), "Enter reveals the hidden diff\n{s}");
}

#[test]
fn wheel_scrolls_pane_under_pointer() {
    let f = Fixture::new();
    for i in 0..80 {
        f.commit(&format!("commit {i}"), NOW - 80 * HOUR + i * HOUR);
    }
    let mut t = H::new(&f, "github-dark", (180, 30));
    t.render(180, 30);
    let r = t.app.hits.panes.history.unwrap();
    let sel = t.app.selected;
    t.app.handle_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: r.x + 2, row: r.y + 3, modifiers: KeyModifiers::NONE });
    t.pump();
    assert_eq!(t.app.list_scroll, 3);
    assert_eq!(t.app.selected, sel, "wheel moves the viewport, not the selection");
    let s = text(&t.render(180, 30));
    let first_row = s.lines().nth(2).unwrap();
    assert!(first_row.contains("commit 76"), "{s}");
}

#[test]
fn separator_drag_resizes() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 30));
    t.render(180, 30);
    let (sep, _) = t.app.hits.panes.seps[0];
    let m = |kind, x| MouseEvent { kind, column: x, row: 10, modifiers: KeyModifiers::NONE };
    t.app.handle_mouse(m(MouseEventKind::Down(MouseButton::Left), sep.x));
    t.app.handle_mouse(m(MouseEventKind::Drag(MouseButton::Left), sep.x + 10));
    t.app.handle_mouse(m(MouseEventKind::Up(MouseButton::Left), sep.x + 10));
    t.render(180, 30);
    assert_eq!(t.app.hits.panes.history.unwrap().width, sep.x + 10);
    assert_eq!(t.app.ui_state.history_width, Some(sep.x + 10));
}

#[test]
fn theme_picker_and_help_render() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('T'));
    let s = text(&t.render(140, 30));
    assert!(s.contains("rose-pine-dawn") && s.contains("solarized-light"), "{s}");
    t.key(KeyCode::Esc);
    t.key(KeyCode::Char('?'));
    let s = text(&t.render(140, 30));
    assert!(s.contains("Everywhere") && s.contains("fetch"), "{s}");
    t.key(KeyCode::Esc);
    t.key(KeyCode::Char('1'));
    let s = text(&t.render(140, 30));
    assert!(s.contains("Changes"), "{s}");
}

#[test]
fn unborn_repo_renders_placeholder() {
    let f = Fixture::new();
    let mut t = H::new(&f, "github-dark", (140, 30));
    let s = text(&t.render(140, 30));
    assert!(s.contains("No commits yet"), "{s}");
}

#[test]
fn missing_blob_shows_error_in_diff_pane() {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.commit("one", NOW - DAY);
    f.write("a.txt", "one\ntwo\n");
    f.commit("two", NOW - HOUR);
    let blob = f.git(&["rev-parse", "HEAD:a.txt"]);
    std::fs::remove_file(f.path().join(".git/objects").join(&blob[..2]).join(&blob[2..])).unwrap();
    let mut t = H::new(&f, "github-dark", (180, 30));
    let s = text(&t.render(180, 30));
    assert!(s.contains("Could not load this diff"), "{s}");
    assert!(!s.contains("loading…"), "{s}");
    assert!(!s.contains("+0 −0"), "unknown stats are not shown as zero:\n{s}");
}

#[test]
fn files_error_rendered() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (180, 30));
    t.app.files = None;
    t.app.files_error = Some("object abc could not be found".into());
    let s = text(&t.render(180, 30));
    assert!(s.contains("Could not list files"), "{s}");
}

#[test]
fn syntax_colours_keep_diff_backgrounds_and_follow_theme() {
    let f = Fixture::new();
    f.write("src/lib.rs", "fn a() {}\n");
    f.commit("one", NOW - DAY);
    f.write("src/lib.rs", "fn a() {}\nfn b() {}\n");
    f.commit("two", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (180, 30));
    let b = t.render(180, 30);
    let kw = t.app.theme.syntax["keyword"].fg.unwrap();
    let (x, y) = find(&b, "fn b").unwrap();
    assert_eq!((b[(x, y)].fg, b[(x, y)].bg), (kw, t.app.theme.diff.add_bg), "added keyword");
    let (x, y) = find(&b, "fn a").unwrap();
    assert_eq!((b[(x, y)].fg, b[(x, y)].bg), (kw, t.app.theme.ui.bg), "context keyword");

    let light = t.app.registry.resolve("github-light", ColorDepth::True, None).unwrap();
    let kw2 = light.syntax["keyword"].fg.unwrap();
    assert_ne!(kw, kw2);
    t.app.theme = light;
    assert!(t.app.take_requests().is_empty(), "no recompute on theme switch");
    let b = t.render(180, 30);
    let (x, y) = find(&b, "fn b").unwrap();
    assert_eq!((b[(x, y)].fg, b[(x, y)].bg), (kw2, t.app.theme.diff.add_bg));
}

#[test]
fn wrap_breaks_long_lines_and_maps_screen_lines_to_rows() {
    let f = Fixture::new();
    let long: String = (0..60).map(|i| format!("w{i:02} ")).collect();
    let body = |extra: &str| format!("first\n{extra}\nlast\n");
    f.write("a.txt", body("short"));
    f.commit("one", NOW - DAY);
    f.write("a.txt", body(&long));
    f.commit("two", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (120, 30));
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('W'));
    assert!(t.app.wrap);
    let b = t.render(120, 30);
    let s = text(&b);
    insta::assert_snapshot!("wrap_120", s);
    assert!(s.contains("w00") && s.contains("w59"), "the whole line is visible: {s}");
    let (_, y0) = find(&b, "w00").unwrap();
    let (_, y1) = find(&b, "w59").unwrap();
    assert!(y1 > y0, "continuation lines below");
    let r = t.app.hits.diff_rows.unwrap();
    let lines = t.app.hits.diff_lines.clone();
    let row = lines[(y0 - r.y) as usize];
    assert_eq!(lines[(y1 - r.y) as usize], row, "every screen line of a wrapped row maps to it");
    t.click(r.x + 20, y1);
    assert_eq!(t.app.diff.as_ref().unwrap().cursor, row);
    let (_, ylast) = find(&b, "last").unwrap();
    t.click(r.x + 20, ylast);
    assert_eq!(t.app.diff.as_ref().unwrap().cursor, lines[(ylast - r.y) as usize]);
    assert!(t.app.diff.as_ref().unwrap().cursor > row);
    // the cursor's lines stay on screen when moving through wrapped rows in a short pane
    let mut t = H::new(&f, "github-dark", (120, 12));
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('W'));
    for _ in 0..8 {
        t.key(KeyCode::Char('j'));
        t.render(120, 12);
        let d = t.app.diff.as_ref().unwrap();
        let n = t.app.hits.diff_lines.iter().filter(|&&v| v == d.cursor).count();
        assert!(n > 0, "cursor row {} visible in {:?}", d.cursor, t.app.hits.diff_lines);
    }
    t.key(KeyCode::Char('W'));
    assert!(!t.app.wrap);
}

#[test]
fn wrap_keeps_the_last_character_of_a_line_exactly_one_width_long() {
    let probe = Fixture::new();
    probe.write("a.txt", "a\nb\n");
    probe.commit("one", NOW - DAY);
    probe.write("a.txt", "a\nc\n");
    probe.commit("two", NOW - HOUR);
    let mut t = H::new(&probe, "github-dark", (120, 30));
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('W'));
    let w = t.app.diff_wrap().unwrap().left as usize;
    for len in [w - 1, w, w + 1, 2 * w] {
        let f = Fixture::new();
        f.write("a.txt", "a\nb\n");
        f.commit("one", NOW - DAY);
        f.write("a.txt", format!("a\n{}Z\n", "x".repeat(len - 1)));
        f.commit("two", NOW - HOUR);
        let mut t = H::new(&f, "github-dark", (120, 30));
        t.app.focus = Focus::Diff;
        t.key(KeyCode::Char('W'));
        let s = text(&t.render(120, 30));
        assert!(s.contains('Z'), "line of {len} (wrap width {w}) lost its last character:\n{s}");
    }
}

#[test]
fn wrap_paging_shows_every_row() {
    let f = Fixture::new();
    let long = |i: usize| format!("{i:03} {}", "lorem ipsum dolor ".repeat(8));
    let old: String = (0..40).map(|i| format!("{}\n", long(i))).collect();
    f.write("a.txt", "x\n");
    f.commit("one", NOW - DAY);
    f.write("a.txt", old);
    f.commit("two", NOW - HOUR);
    let mut t = H::new(&f, "github-dark", (120, 16));
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('W'));
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..60 {
        t.render(120, 16);
        seen.extend(t.app.hits.diff_lines.iter().copied());
        t.key(KeyCode::PageDown);
    }
    let rows = t.app.diff.as_ref().unwrap().rows(false);
    let missing: Vec<usize> = (0..rows).filter(|r| !seen.contains(r)).collect();
    assert!(missing.is_empty(), "PgDn skipped rows {missing:?}");
}

fn changes_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("src/main.rs", main_rs(0));
    f.write("notes.txt", "a\nb\n");
    f.commit("base", NOW - DAY);
    f.write("src/main.rs", main_rs(1));
    f.write("notes.txt", "a\nB\n");
    f.write("new file.txt", "fresh\n");
    // stage the first changed line of main.rs only
    let staged = main_rs(0).replace("    step(5);\n", "    let total = compute(5, 7);\n");
    let p = f.path().join("src/main.rs");
    std::fs::write(&p, &staged).unwrap();
    f.git(&["add", "src/main.rs"]);
    std::fs::write(&p, main_rs(1)).unwrap();
    f
}

#[test]
fn changes_tab_snapshots() {
    let f = changes_fixture();
    for (w, focus_diff) in [(140u16, false), (100, false), (100, true)] {
        let mut t = H::new(&f, "github-dark", (w, 30));
        t.key(KeyCode::Char('1'));
        t.select_change("src/main.rs");
        if focus_diff {
            t.key(KeyCode::Enter);
        }
        let s = text(&t.render(w, 30));
        insta::assert_snapshot!(format!("changes_{w}_{}", if focus_diff { "diff" } else { "files" }), s);
    }
}

#[test]
fn a_moved_submodule_shows_a_card_in_changes() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - HOUR);
    let sub = f.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(&sub).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["-c", "user.name=T", "-c", "user.email=t@example.com", "commit", "-q", "--allow-empty", "-m", "sub"]);
    let new = git(&["rev-parse", "HEAD"]);
    f.git(&["update-index", "--add", "--cacheinfo", "160000,1111111111111111111111111111111111111111,sub"]);
    // not f.commit: its `add -A` would record the nested repo's real HEAD
    f.git(&["commit", "-q", "-m", "add sub"]);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("sub");
    let s = text(&t.render(140, 30));
    let want = format!("Submodule sub: 1111111..{}", &new[..7]);
    assert!(s.contains(&want), "wanted {want:?}\n{s}");
}

#[test]
fn staged_lines_show_a_check_mark() {
    let f = changes_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("src/main.rs");
    let b = t.render(140, 30);
    let s = text(&b);
    let line = |needle: &str| s.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle}:\n{s}")).to_string();
    assert!(line("let total = compute(5, 7);").contains('✓'), "{s}");
    assert!(!line("println!").contains('✓'));
    assert!(line("[~]").contains("main.rs"), "partially staged file shows [~]");
}

#[test]
fn clicks_toggle_checkboxes_and_gutter_lines() {
    let f = changes_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    let b = t.render(140, 30);
    let (x, y) = find(&b, "notes.txt").unwrap();
    let _ = x;
    t.click(t.app.hits.files_rows.unwrap().x + 1, y);
    t.render(140, 30);
    let e = t.app.changes.entries().iter().find(|e| e.path == "notes.txt").unwrap();
    assert_eq!(e.check(), gitty_core::status::Check::Staged, "checkbox click stages the file");
    // gutter click on the println line stages it
    t.select_change("src/main.rs");
    let b = t.render(140, 30);
    let (_, y) = find(&b, "println!").unwrap();
    let gx = t.app.hits.diff_old_gutter.0;
    t.click(gx, y);
    let b = t.render(140, 30);
    let (_, y) = find(&b, "println!").unwrap();
    let row: String = (0..140).map(|x| b[(x, y)].symbol().to_string()).collect();
    assert!(row.contains('✓'), "{row}");
    // header checkbox: toggle all
    let r = t.app.hits.files_rows.unwrap();
    t.click(r.x + 1, r.y - 1);
    t.render(140, 30);
    assert!(t.app.changes.entries().iter().all(|e| e.check() == gitty_core::status::Check::Staged));
}

fn type_str(t: &mut H, s: &str) {
    for c in s.chars() {
        t.key(KeyCode::Char(c));
    }
}

#[test]
fn commit_box_renders_fields_counter_and_button() {
    let f = changes_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("src/main.rs");
    let s = text(&t.render(140, 30));
    assert!(s.contains("Update main.rs"), "placeholder:\n{s}");
    assert!(s.contains("Commit 1 file to main"), "{s}");
    t.key(KeyCode::Char('c'));
    let summary = "Compute the total once and print it after the loop!";
    assert!(summary.chars().count() > 50 && summary.chars().count() <= 72);
    type_str(&mut t, summary);
    t.key(KeyCode::Tab);
    type_str(&mut t, "Saves a pass.");
    let b = t.render(140, 30);
    insta::assert_snapshot!("changes_commit_box", text(&b));
    // the counter turns yellow past 50
    let (_, y) = find(&b, "Compute the total").unwrap();
    let n = summary.chars().count().to_string();
    let w = t.app.hits.files_rows.unwrap().width;
    let row: String = (0..w).map(|x| b[(x, y)].symbol().to_string()).collect();
    let at = row.rfind(&n).unwrap_or_else(|| panic!("counter {n} in {row:?}")) as u16;
    assert_eq!(b[(at, y)].fg, t.app.theme.ui.warning);
    // clicking the button commits
    let (bx, by) = find(&b, "Commit 1 file to main").unwrap();
    t.click(bx, by);
    let s = text(&t.render(140, 30));
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), summary);
    assert!(s.contains("Committed just now · [u] Undo"), "{s}");
}

#[test]
fn amend_shows_a_warning_banner_and_clicking_a_field_focuses_it() {
    let f = changes_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.key(KeyCode::Char('A'));
    let b = t.render(140, 30);
    let s = text(&b);
    assert!(s.contains("Amending the last commit"), "{s}");
    assert!(s.contains("Amend last commit"), "{s}");
    let (x, y) = find(&b, "Co-authors").unwrap();
    t.click(x, y);
    assert_eq!(t.app.focus, Focus::Commit);
    assert_eq!(t.app.changes.commit.field, gitty::app::commit::Field::CoAuthors);
}

#[test]
fn password_prompt_is_masked_and_progress_shows_in_the_top_bar() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (120, 30));
    t.app.handle_msg(Msg::NetStarted { op: gitty::msg::NetOp::Fetch, label: "Fetching origin".into(), remote: Some("origin".into()), cancel: None });
    t.app.handle_msg(Msg::NetProgress { op: gitty::msg::NetOp::Fetch, fraction: 0.425 });
    let ask = gitty::askpass::Ask { id: 7, prompt: "Password for 'https://ann@example.com': ".into(), kind: gitty::askpass::AskKind::Secret };
    t.app.handle_msg(Msg::Ask(ask));
    type_str(&mut t, "hunter2");
    let s = text(&t.render(120, 30));
    assert!(s.contains("Fetching origin 42%"), "{s}");
    assert!(s.contains("•••••••") && !s.contains("hunter2"), "{s}");
    insta::assert_snapshot!("password_prompt", s);
}

#[test]
fn host_key_prompt_shows_the_fingerprint_and_the_question() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (100, 30));
    let prompt = "The authenticity of host 'github.com (140.82.121.4)' can't be established.\nED25519 key fingerprint is SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU.\nThis key is not known by any other names.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ";
    t.app.handle_msg(Msg::Ask(gitty::askpass::Ask { id: 1, prompt: prompt.into(), kind: gitty::askpass::classify(prompt) }));
    let s = text(&t.render(100, 30));
    assert!(s.contains("SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"), "{s}");
    assert!(s.contains("continue connecting"), "{s}");
    assert!(s.contains("y yes"), "{s}");
}

#[test]
fn search_bar_shows_position_and_progress_and_matches_are_highlighted() {
    let f = Fixture::new();
    for i in 0..30 {
        f.write("a.txt", format!("{i}\n"));
        let msg = if i % 5 == 0 { format!("plain {i}") } else { format!("fix {i}") };
        f.commit(&msg, NOW - DAY + i * 60);
    }
    let mut t = H::new(&f, "github-dark", (120, 40));
    t.app.search_chunk = 6;
    t.app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    for c in "fix".chars() {
        t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let b = t.render(120, 40);
    assert!(text(&b).lines().last().unwrap().starts_with(" /fix"), "the bar shows what is typed");
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    // answer the first two of five chunks only: 12 rows, 40%
    let reqs: Vec<_> = t.app.take_requests().into_iter().take(2).collect();
    let mut out = Vec::new();
    for r in reqs {
        exec(&t.h, r, &mut |m| out.push(m), &t.gens);
    }
    for m in out {
        t.app.handle_msg(m);
    }
    t.app.search_step(true);
    t.app.search_step(true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|fr| ui::draw(&mut t.app, fr)).unwrap();
    let b = term.backend().buffer().clone();
    let s = text(&b);
    let bottom = s.lines().last().unwrap();
    assert!(bottom.starts_with(" /fix  3/10 · searching… 40%"), "{bottom}");
    let warning = t.app.theme.ui.warning;
    let (x, y) = find(&b, "fix 29").expect("newest match drawn");
    assert_eq!(b[(x, y)].fg, warning, "matched summaries are highlighted");
    let (x, y) = find(&b, "plain 25").expect("non-match drawn");
    assert_ne!(b[(x, y)].fg, warning);
}

#[test]
fn shift_click_extends_a_range_and_the_header_shows_it() {
    let f = Fixture::new();
    for i in 0..5 {
        f.write("a.txt", format!("{i}\n"));
        f.write(&format!("f{i}.txt"), "x\nx\n");
        f.commit(&format!("change {i}"), NOW - DAY + i * 60);
    }
    let mut t = H::new(&f, "github-dark", (140, 40));
    let b = t.render(140, 40);
    let (x, y) = find(&b, "change 2").unwrap();
    let m = |kind| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::SHIFT };
    t.app.handle_mouse(m(MouseEventKind::Down(MouseButton::Left)));
    t.app.handle_mouse(m(MouseEventKind::Up(MouseButton::Left)));
    t.pump();
    assert_eq!(t.app.selected_range(), Some((2, 0)), "shift-click anchors at the selection and extends");
    let b = t.render(140, 40);
    let s = text(&b);
    let (oldest, newest) = (f.git(&["rev-parse", "--short=7", "HEAD~2"]), f.git(&["rev-parse", "--short=7", "HEAD"]));
    let header = s.lines().nth(1).unwrap();
    assert!(header.contains(&format!("3 commits · {oldest}..{newest}")), "{header}");
    // a.txt 2→4 (+1 −1) and three new two-line files
    assert!(header.contains("+7 −1"), "{header}");
    let ctrl = |kind| MouseEvent { kind, column: x, row: y + 1, modifiers: KeyModifiers::CONTROL };
    t.app.handle_mouse(ctrl(MouseEventKind::Down(MouseButton::Left)));
    t.pump();
    assert_eq!(t.app.selected_range(), Some((3, 0)), "ctrl-click extends like shift-click");
    let plain = |kind| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    t.app.handle_mouse(plain(MouseEventKind::Down(MouseButton::Left)));
    t.pump();
    assert_eq!(t.app.selected_range(), None, "a plain click ends the range");
}

#[test]
fn compare_mode_shows_title_tabs_and_the_branch_commits() {
    let f = Fixture::new();
    f.write("base.txt", "0\n");
    f.commit("base", NOW - DAY);
    f.git(&["branch", "topic"]);
    f.write("m.txt", "1\n");
    f.commit("on main", NOW - DAY + 60);
    f.git(&["checkout", "-q", "topic"]);
    f.write("t.txt", "1\n");
    f.commit("topic one", NOW - DAY + 120);
    f.write("t.txt", "2\n");
    f.commit("topic two", NOW - DAY + 180);
    f.git(&["checkout", "-q", "main"]);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE));
    let b = t.render(140, 30);
    let s = text(&b);
    assert!(s.contains("Compare with"), "picker title\n{s}");
    assert!(s.contains("topic"), "{s}");
    t.key(KeyCode::Enter);
    let b = t.render(140, 30);
    let s = text(&b);
    assert!(s.contains("Compare with topic"), "{s}");
    assert!(s.contains("Behind (2)") && s.contains("Ahead (1)") && s.contains("Files"), "{s}");
    assert!(s.contains("topic two") && s.contains("topic one"), "{s}");
    assert!(!s.contains("on main"), "the Behind tab lists only the branch's commits\n{s}");
    let footer = s.lines().last().unwrap_or_default();
    assert!(footer.contains("b other branch") && footer.contains("esc leave"), "`b` picks another branch to compare with\n{footer}");
}

#[test]
fn tree_view_renders_directories_indented() {
    let f = Fixture::new();
    f.write("seed", "s\n");
    f.commit("seed", NOW - DAY);
    for p in ["docs/guide.md", "src/ui/view.rs", "src/lib.rs", "top.txt"] {
        f.write(p, "x\n");
    }
    f.commit("tree", NOW - DAY + 60);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
    let b = t.render(140, 30);
    let s = text(&b);
    let (dx, dy) = find(&b, "▾ docs/").expect(&s);
    let (vx, vy) = find(&b, "view.rs").expect(&s);
    let (ux, uy) = find(&b, "▾ ui/").expect(&s);
    assert!(ux > dx && vx > ux, "deeper rows are indented further");
    assert!(dy < uy && uy < vy);
    assert!(find(&b, "src/ui/view.rs").is_none(), "file rows show only the name");
    let (tx, _) = find(&b, "top.txt").unwrap();
    assert!(tx < vx);
}

fn editor_fixture() -> Fixture {
    let f = Fixture::new();
    f.write("dir with space/f.txt", "1\n2\n3\n4\n5\n6\n");
    f.commit("base", NOW - DAY);
    f.write("dir with space/f.txt", "1\n2\n3\nfour\n5\n6\n");
    f.commit("change four", NOW - DAY + 60);
    f
}

#[test]
fn double_click_opens_the_file_at_the_line_in_the_editor() {
    use gitty::external::External;
    let f = editor_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.workdir = Some(f.path().to_path_buf());
    let b = t.render(140, 30);
    let path = f.path().join("dir with space/f.txt");
    let (x, y) = find(&b, "+ four").expect("diff line drawn");
    t.click(x, y);
    assert_eq!(t.app.external, None, "one click only moves the cursor");
    t.click(x, y);
    assert_eq!(t.app.external, Some(External::Edit { path: path.clone(), line: Some(4) }), "double-click opens at the new line");
    t.app.external = None;
    let b = t.render(140, 30);
    let (x, y) = find(&b, "f.txt").expect("file row drawn");
    t.click(x, y);
    t.click(x, y);
    assert_eq!(t.app.external, Some(External::Edit { path, line: Some(4) }), "a file row opens at its first change");
    t.app.external = None;
    t.clock += Duration::from_secs(1);
    t.app.tick(t.clock);
    t.click(x, y);
    assert_eq!(t.app.external, None, "clicks far apart are not a double-click");
}

#[test]
fn double_click_opens_the_clicked_file_even_before_its_diff_loads() {
    use gitty::external::External;
    let f = Fixture::new();
    f.write("a.txt", "1\n");
    f.write("src/b.txt", "1\n");
    f.commit("base", NOW - DAY);
    f.write("a.txt", "1\n2\n");
    f.write("src/b.txt", "1\nB\n");
    f.commit("both", NOW - DAY + 60);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.workdir = Some(f.path().to_path_buf());
    let b = t.render(140, 30);
    let (x, y) = find(&b, "b.txt").expect("file row drawn");
    // two clicks with no time for b.txt's diff to arrive in between
    let m = |kind| MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE };
    for _ in 0..2 {
        t.app.handle_mouse(m(MouseEventKind::Down(MouseButton::Left)));
        t.app.handle_mouse(m(MouseEventKind::Up(MouseButton::Left)));
    }
    assert_eq!(t.app.external, Some(External::Edit { path: f.path().join("src/b.txt"), line: None }), "the clicked file, not the shown one");
    t.app.external = None;
    t.pump();
    // tree view: a directory row folds; a double-click on it opens nothing
    t.key(KeyCode::Char('t'));
    let b = t.render(140, 30);
    let (x, y) = find(&b, "src/").expect("directory row drawn");
    t.click(x, y);
    t.click(x, y);
    assert_eq!(t.app.external, None, "a directory is not opened in the editor");
}

#[test]
fn o_sends_both_sides_to_the_difftool_or_says_to_set_one() {
    use gitty::external::External;
    let f = editor_fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.render(140, 30);
    t.app.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert_eq!(t.app.external, None);
    assert!(t.app.toast.as_ref().is_some_and(|m| m.what.contains("difftool")), "{:?}", t.app.toast);
    t.app.config.difftool = Some("delta".into());
    t.app.handle_key(KeyEvent::new(KeyCode::Char('O'), KeyModifiers::SHIFT));
    assert_eq!(
        t.app.external,
        Some(External::Diff { path: "dir with space/f.txt".into(), old: b"1\n2\n3\n4\n5\n6\n".to_vec(), new: b"1\n2\n3\nfour\n5\n6\n".to_vec() })
    );
}

fn find_all(b: &Buffer, needle: &str) -> Vec<(u16, u16)> {
    (0..b.area.height).filter_map(|y| {
        let line: String = (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect();
        line.find(needle).map(|i| (line[..i].chars().count() as u16, y))
    }).collect()
}

#[test]
fn clicking_a_hunk_header_handle_stages_that_hunk() {
    let f = Fixture::new();
    let base: String = (1..=40).map(|i| format!("line {i}\n")).collect();
    f.write("big.txt", &base);
    f.commit("base", NOW - DAY);
    f.write("big.txt", base.replace("line 3\n", "line three\n").replace("line 35\n", "line thirty-five\n"));
    let mut t = H::new(&f, "github-dark", (140, 40));
    t.key(KeyCode::Char('1'));
    t.select_change("big.txt");
    let b = t.render(140, 40);
    let headers = find_all(&b, "@@");
    assert!(headers.len() >= 2, "two hunks\n{}", text(&b));
    let r = t.app.hits.diff_rows.unwrap();
    t.click(r.x, headers[1].1);
    t.render(140, 40);
    let cached = f.git(&["diff", "--cached", "-U0", "--", "big.txt"]);
    assert!(cached.contains("+line thirty-five") && !cached.contains("+line three"), "only the clicked hunk:\n{cached}");
    assert!(text(&b).lines().nth(headers[1].1 as usize).unwrap().contains('±'), "the handle is drawn");
}

#[test]
fn a_gutter_drag_at_the_bottom_edge_scrolls_and_keeps_selecting() {
    let f = Fixture::new();
    f.write("long.txt", "start\n");
    f.commit("base", NOW - DAY);
    let added: String = (1..=80).map(|i| format!("added {i}\n")).collect();
    f.write("long.txt", format!("start\n{added}"));
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("long.txt");
    let b = t.render(140, 30);
    let (_, y0) = find(&b, "added 1").unwrap();
    let gx = t.app.hits.diff_new_gutter.0;
    let r = t.app.hits.diff_rows.unwrap();
    let ev = |kind, y| MouseEvent { kind, column: gx, row: y, modifiers: KeyModifiers::NONE };
    t.app.handle_mouse(ev(MouseEventKind::Down(MouseButton::Left), y0));
    for _ in 0..10 {
        t.app.handle_mouse(ev(MouseEventKind::Drag(MouseButton::Left), r.bottom() - 1));
    }
    assert!(t.app.diff.as_ref().unwrap().scroll >= 10, "each drag event at the edge scrolls a row");
    t.app.handle_mouse(ev(MouseEventKind::Up(MouseButton::Left), r.bottom() - 1));
    t.pump();
    let staged = f.git(&["diff", "--cached", "--", "long.txt"]).lines().filter(|l| l.starts_with("+added")).count();
    let visible = r.height as usize;
    assert!(staged > visible, "the selection grew past the first screen: {staged} staged, {visible} visible");
}

#[test]
fn split_view_space_stages_only_the_side_under_the_pointer() {
    let f = Fixture::new();
    f.write("n.txt", "a\nlet value = 1;\nc\n");
    f.commit("base", NOW - DAY);
    f.write("n.txt", "a\nlet value = 2;\nc\n");
    let mut t = H::new(&f, "github-dark", (220, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("n.txt");
    let b = t.render(220, 30);
    assert!(t.app.split_active(), "220 columns: split view");
    let (lx, ly) = find(&b, "let value = 1;").expect("deletion on the left");
    let (rx, ry) = find(&b, "let value = 2;").expect("addition on the right");
    assert_eq!(ly, ry, "one paired row");
    assert!(rx > lx);
    t.click(rx + 4, ry);
    t.key(KeyCode::Char(' '));
    t.render(220, 30);
    assert_eq!(f.git(&["show", ":n.txt"]), "a\nlet value = 1;\nlet value = 2;\nc", "only the addition is staged");
    let b = t.render(220, 30);
    let (lx, ly) = find(&b, "let value = 1;").unwrap();
    t.click(lx + 4, ly);
    t.key(KeyCode::Char(' '));
    t.render(220, 30);
    assert_eq!(f.git(&["show", ":n.txt"]), "a\nlet value = 2;\nc", "then the deletion too");
}

#[test]
fn a_split_click_side_does_not_stick_to_later_keyboard_actions() {
    let trash = tempfile::tempdir().unwrap();
    // SAFETY: the only test in this binary that discards
    unsafe { std::env::set_var("GITTY_TRASH_DIR", trash.path()) };
    let f = Fixture::new();
    f.write("n.txt", "a\nlet value = 1;\nc\n");
    f.commit("base", NOW - DAY);
    f.write("n.txt", "a\nlet value = 2;\nc\n");
    let mut t = H::new(&f, "github-dark", (220, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("n.txt");
    let b = t.render(220, 30);
    let (rx, ry) = find(&b, "let value = 2;").unwrap();
    t.click(rx + 4, ry);
    // moved away and back with the keyboard: Space acts on the whole pair
    t.key(KeyCode::Char('j'));
    t.key(KeyCode::Char('k'));
    t.key(KeyCode::Char(' '));
    t.render(220, 30);
    assert_eq!(f.git(&["show", ":n.txt"]), "a\nlet value = 2;\nc", "both sides staged");
    t.key(KeyCode::Char(' '));
    t.render(220, 30);
    let b = t.render(220, 30);
    let (lx, ly) = find(&b, "let value = 1;").unwrap();
    t.click(lx + 4, ly);
    // d after a click discards the whole change, not only its deletion
    t.key(KeyCode::Char('d'));
    t.key(KeyCode::Enter);
    t.render(220, 30);
    assert_eq!(std::fs::read_to_string(f.path().join("n.txt")).unwrap(), "a\nlet value = 1;\nc\n");
}

#[test]
fn help_is_generated_from_the_keymap() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("one", NOW - DAY);
    let repo = Repo::open(f.path()).unwrap();
    let registry = Registry::load(None);
    let theme = registry.resolve("github-dark", ColorDepth::True, None).unwrap();
    let gens = Arc::new(Gens::default());
    let config = Config { keys: toml::from_str("fetch = \"F5\"\n").unwrap(), ..Config::default() };
    let app = App::new(AppInit { repo_name: "repo".into(), config, registry, theme, depth: ColorDepth::True, ui_state: UiState::default(), config_path: None, state_path: None, gens: gens.clone(), now: NOW, clock: Instant::now(), size: (140, 60) });
    let mut t = H { app, h: repo.handle(), gens, clock: Instant::now() };
    t.pump();
    t.key(KeyCode::Char('?'));
    let b = t.render(140, 60);
    let s = text(&b);
    let line = s.lines().find(|l| l.contains("fetch") && !l.contains("fetched")).unwrap_or_else(|| panic!("{s}"));
    assert!(line.contains("F5"), "help shows the remapped key: {line}");
    assert!(!s.lines().any(|l| l.contains("  f  ") && l.contains("fetch")));
    assert!(s.contains("search history"), "M6 actions are listed\n{s}");
}

#[test]
fn an_unpublished_branch_says_so_and_marks_its_commits() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - 2 * DAY);
    f.add_bare_upstream();
    f.git(&["checkout", "-q", "-b", "feature"]);
    f.write("b.txt", "b\n");
    f.commit("Only on this machine", NOW - DAY);
    let mut t = H::new(&f, "github-dark", (140, 20));
    t.pump();
    let s = text(&t.render(140, 20));
    let top = s.lines().next().unwrap();
    assert!(top.contains("⎇ feature  ↑1 not published"), "{top}");
    let row = s.lines().find(|l| l.contains("Only on this machine") && l.contains(" feature ")).expect("the commit row");
    assert!(row.trim_start().starts_with('↑'), "{row}");
    let base = s.lines().find(|l| l.contains("base") && l.contains("main")).expect("the base row");
    assert!(!base.contains('↑'), "already on the remote: {base}");
}

#[test]
fn committing_everything_leaves_no_stale_diff_title() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.write("a.txt", "a\nchanged\n");
    let mut t = H::new(&f, "github-dark", (140, 24));
    t.pump();
    t.key(KeyCode::Char('1'));
    t.pump();
    assert!(text(&t.render(140, 24)).contains("a.txt"));
    t.key(KeyCode::Char('a'));
    t.pump();
    t.key(KeyCode::Char('c'));
    for c in "Change a".chars() {
        t.key(KeyCode::Char(c));
    }
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    t.pump();
    assert_eq!(f.git(&["log", "-1", "--format=%s"]), "Change a");
    let s = text(&t.render(140, 24));
    assert!(s.contains("No local changes"), "{s}");
    assert!(!s.contains("a.txt"), "no diff title for a file that has no change left:\n{s}");
}

#[test]
fn branch_picker_shows_title_marks_the_current_branch_and_footer_keys() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.git(&["branch", "topic"]);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
    let s = text(&t.render(140, 30));
    assert!(s.contains("Branches"), "{s}");
    assert!(s.contains("● main") && s.contains("topic"), "{s}");
    assert!(s.contains("^N new") && s.contains("^R rename") && s.contains("^D delete") && s.contains("^G merge"), "{s}");
}

#[test]
fn dirty_switch_prompt_and_name_input_render() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.git(&["branch", "topic"]);
    f.write("a.txt", "edited\n");
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.handle_focus(true);
    t.drain();
    t.app.handle_focus(false); // the focused status backstop would keep `render` pumping forever
    for c in "Btopic".chars() {
        t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let s = text(&t.render(140, 30));
    assert!(s.contains("uncommitted changes") && s.contains("switch anyway"), "{s}");
    t.app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::NONE));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    let s = text(&t.render(140, 30));
    assert!(s.contains("New branch") && s.contains("Enter confirm"), "{s}");
}

#[test]
fn merge_prompts_render_for_a_clean_and_a_dirty_tree() {
    for (w, h) in [(80u16, 24u16), (140, 30)] {
        let f = Fixture::new();
        f.write("a.txt", "a\n");
        f.commit("base", NOW - DAY);
        f.git(&["branch", "feat/branches-and-stash"]);
        let mut t = H::new(&f, "github-dark", (w, h));
        t.app.handle_focus(true);
        t.drain();
        t.app.handle_focus(false);
        for c in "Bfeat/branches-and-stash".chars() {
            t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let s = text(&t.render(w, h));
        assert!(s.contains("Merge `feat/branches-and-stash` into `main`?"), "{w}x{h}\n{s}");
        assert!(s.contains("Enter merge") && s.contains("Esc cancel"), "{w}x{h}\n{s}");
        t.app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        f.write("a.txt", "edited\n");
        t.app.handle_focus(true);
        t.drain();
        t.app.handle_focus(false);
        for c in "Bfeat/branches-and-stash".chars() {
            t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        let s = text(&t.render(w, h));
        assert!(s.contains("uncommitted changes") && s.contains("Merging feat/branches-and-stash into main"), "{w}x{h}\n{s}");
        assert!(s.contains("stash and merge") && s.contains("merge anyway"), "{w}x{h}\n{s}");
    }
}

#[test]
fn an_open_overlay_does_not_leave_the_pane_hints_in_the_footer() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.write("a.txt", "edited\n");
    let mut t = H::new(&f, "github-dark", (140, 30));
    let footer = |t: &mut H| text(&t.render(140, 30)).lines().last().unwrap_or_default().to_string();
    assert!(footer(&mut t).contains("quit"), "the pane's hints show without an overlay");
    for key in ['B', 'S'] {
        t.app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
        let s = footer(&mut t);
        assert!(!s.contains("quit") && !s.contains("stage"), "{key}: {s}");
        t.key(KeyCode::Esc);
    }
    assert!(footer(&mut t).contains("quit"), "the hints come back");
}

#[test]
fn the_error_detail_says_how_to_close_it() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.toast = Some(gitty::app::Toast { what: "switching failed".into(), detail: "line one\nline two".into(), error: true });
    t.app.overlay = Some(gitty::app::Overlay::ErrorDetail);
    let s = text(&t.render(140, 30));
    assert!(s.contains("line two") && s.contains("Esc close"), "{s}");
}

#[test]
fn a_confirmation_names_what_enter_does() {
    use gitty::msg::WriteOp;
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    let mut t = H::new(&f, "github-dark", (140, 30));
    let ops = [
        (WriteOp::DeleteBranch { name: "x".into(), force: true }, "Enter delete"),
        (WriteOp::StashDrop { index: 0, expect: String::new() }, "Enter drop"),
        (WriteOp::Merge { name: "x".into(), remote: false }, "Enter merge"),
        (WriteOp::DiscardFiles { restore: vec![], remove: vec![] }, "Enter discard"),
    ];
    for (op, label) in ops {
        t.app.overlay = Some(gitty::app::Overlay::Confirm { title: "Sure".into(), body: "body".into(), op });
        let s = text(&t.render(140, 30));
        assert!(s.contains(label), "{label}\n{s}");
    }
}

#[test]
fn stash_prompts_say_untracked_files_are_included_and_count_a_big_pile() {
    for (n, note) in [(1, "Stash includes untracked files"), (501, "Stash includes 502 untracked files: this can take a while")] {
        let f = Fixture::new();
        f.write("a.txt", "a\n");
        f.commit("base", NOW - DAY);
        f.git(&["branch", "topic"]);
        f.write("a.txt", "edited\n");
        for i in 0..n {
            f.write(&format!("pile/f{i}.txt"), "x\n");
        }
        f.write("new.txt", "n\n");
        let mut t = H::new(&f, "github-dark", (140, 30));
        t.app.open_stash_name();
        let s = text(&t.render(140, 30));
        assert!(s.contains(note) && (n > 500) == s.contains("this can take a while"), "{n}\n{s}");
        t.key(KeyCode::Esc);
        for c in "Btopic".chars() {
            t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let s = text(&t.render(140, 30));
        assert!(s.contains("stash and switch") && s.contains(note), "{n}\n{s}");
    }
}

#[test]
fn stash_list_and_the_stash_choice_render() {
    let f = Fixture::new();
    f.write("a.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.git(&["branch", "topic"]);
    f.write("a.txt", "edited\n");
    f.git_env(&["stash", "push", "-q", "-m", "half done"], &[("GIT_COMMITTER_DATE", format!("{}", NOW - 3 * HOUR))]);
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.handle_key(KeyEvent::new(KeyCode::Char('S'), KeyModifiers::NONE));
    let s = text(&t.render(140, 30));
    assert!(s.contains("Stashes") && s.contains("stash@{0}") && s.contains("half done"), "{s}");
    assert!(s.lines().any(|l| l.contains("half done") && l.contains("(main)") && l.contains(" 3h")), "the row shows its age\n{s}");
    assert!(s.contains("a apply") && s.contains("p pop") && s.contains("d drop"), "{s}");
    t.key(KeyCode::Esc);
    f.write("a.txt", "dirty again\n");
    t.app.handle_focus(true);
    t.drain();
    for c in "Btopic".chars() {
        t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    t.app.handle_focus(false);
    let s = text(&t.render(140, 30));
    assert!(s.contains("stash and switch"), "{s}");
}

#[test]
fn dirty_switch_prompt_is_not_cut_off_with_a_long_branch_name() {
    for (w, h) in [(80u16, 24u16), (140, 30)] {
        let f = Fixture::new();
        f.write("a.txt", "a\n");
        f.commit("base", NOW - DAY);
        f.git(&["branch", "feat/branches-and-stash"]);
        f.write("a.txt", "edited\n");
        let mut t = H::new(&f, "github-dark", (w, h));
        t.app.handle_focus(true);
        t.drain();
        t.app.handle_focus(false);
        for c in "Bfeat/branches-and-stash".chars() {
            t.app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let s = text(&t.render(w, h));
        assert!(s.contains("git carries the changes over, or refuses."), "{w}x{h}\n{s}");
        assert!(s.contains("stash and switch"), "{w}x{h}\n{s}");
    }
}

fn force_question_at(removal: Option<gitty_core::net::Removal>, size: (u16, u16)) -> String {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", size);
    let target = gitty_core::net::PushTarget { remote: "origin".into(), refspec: "refs/heads/feat:refs/heads/topic".into(), set_upstream: false };
    let plan = gitty_core::net::ForcePush { branch: "feat".into(), target, expected: "a".repeat(40), tip: "b".repeat(40), removal };
    t.app.overlay = Some(gitty::app::Overlay::ForcePush { plan });
    text(&t.render(size.0, size.1))
}

fn removal(total: usize, others: usize, top: &[&str]) -> Option<gitty_core::net::Removal> {
    Some(gitty_core::net::Removal { total, others, top: top.iter().map(|s| s.to_string()).collect() })
}

#[test]
fn the_force_push_question_for_a_plain_amend() {
    let s = force_question_at(removal(1, 0, &[]), (80, 24));
    assert!(s.contains("Force push `feat` with lease?"), "{s}");
    assert!(s.contains("It replaces your earlier version of this branch (1 commit)."), "{s}");
    assert!(s.contains("If anyone pushes after this screen, git refuses instead."), "{s}");
    assert!(s.contains("Enter force push · Esc cancel"), "{s}");
    assert!(force_question_at(removal(0, 0, &[]), (80, 24)).contains("Nothing on the remote is lost."));
}

#[test]
fn the_force_push_question_lists_what_the_remote_loses() {
    let s = force_question_at(removal(6, 5, &["abc1234 Ann: one", "def5678 Bob: two", "0123456 Cy: three"]), (80, 24));
    assert!(s.contains("origin/topic has 5 commits by others"), "the remote's name for the branch: {s}");
    assert!(s.contains("they will be removed from"), "{s}");
    assert!(s.contains("  abc1234 Ann: one") && s.contains("  0123456 Cy: three"), "{s}");
    assert!(s.contains("…and 2 more"), "{s}");
    assert!(s.contains("and 1 of your own earlier commit"), "{s}");
    assert!(s.contains("Enter force push · Esc cancel"), "{s}");
}

#[test]
fn the_force_push_question_warns_when_git_cannot_tell() {
    let s = force_question_at(None, (80, 24));
    assert!(s.contains("Could not tell which commits on origin/topic would be lost."), "{s}");
    assert!(!s.contains("earlier version"), "{s}");
    assert!(s.contains("Enter force push · Esc cancel"), "{s}");
}

#[test]
fn a_short_terminal_cuts_the_force_push_text_not_the_keys() {
    let s = force_question_at(removal(6, 5, &["abc1234 Ann: one", "def5678 Bob: two", "0123456 Cy: three"]), (80, 14));
    assert!(s.contains("Force push `feat` with lease?"), "{s}");
    assert!(s.contains("Enter force push · Esc cancel"), "{s}");
}

// ---- Files tab ----

const FAKE_SECRET: &str = "fake-secret-value-123";

fn files_fixture() -> Fixture {
    let f = Fixture::new();
    f.write(".env", format!("TOKEN={FAKE_SECRET}\n"));
    f.write(".gitignore", "target/\n");
    f.write("README.md", "# readme\n");
    f.write("src/main.rs", main_rs(0));
    f.write("src/lib.rs", "pub fn lib() {}\n");
    f.write("data.bin", b"ab\0cd");
    f.commit("base", NOW - DAY);
    f.write("target/out.txt", "o\n");
    std::os::unix::fs::symlink("src/main.rs", f.path().join("link")).unwrap();
    f
}

fn files_tab(f: &Fixture, size: (u16, u16)) -> H {
    let mut t = H::new(f, "github-dark", size);
    t.app.workdir = Some(f.path().to_path_buf());
    t.key(KeyCode::Char('3'));
    t
}

fn pick(t: &mut H, name: &str) {
    let i = t.app.files_tab.rows.iter().position(|r| r.name == name).unwrap_or_else(|| panic!("no row {name}"));
    t.app.select_files_row(i);
    t.pump();
}

/// No cell, row or whole screen carries `needle` (cells are checked on their own too, in case a
/// string were split across them).
fn assert_absent(b: &Buffer, needle: &str) {
    assert!(!text(b).contains(needle), "{needle} is on screen");
    let all: String = (0..b.area.height).flat_map(|y| (0..b.area.width).map(move |x| (x, y))).map(|(x, y)| b[(x, y)].symbol().to_string()).collect();
    assert!(!all.contains(needle), "{needle} is in the cells");
}

#[test]
fn files_tab_tree_and_viewer_snapshots() {
    let f = files_fixture();
    for (w, focus_viewer) in [(140u16, false), (100, false), (100, true)] {
        let mut t = files_tab(&f, (w, 30));
        t.key(KeyCode::Enter);
        pick(&mut t, "main.rs");
        if focus_viewer {
            t.key(KeyCode::Enter);
        }
        let s = text(&t.render(w, 30));
        insta::assert_snapshot!(format!("files_{w}_{}", if focus_viewer { "viewer" } else { "tree" }), s);
    }
}

#[test]
fn the_tree_marks_directories_symlinks_secrets_and_ignored_entries() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    t.key(KeyCode::Enter);
    let b = t.render(140, 30);
    let s = text(&b);
    assert!(s.contains("▾ src/"), "{s}");
    assert!(s.contains("▸ target/"), "{s}");
    assert!(s.contains("link -> src/main.rs"), "{s}");
    assert!(s.contains(".env (secret)"), "{s}");
    assert!(s.contains("  main.rs"), "children are indented under src: {s}");
    assert!(!s.contains(".git/"), "{s}");
    // the ignored directory is dimmed against a normal one
    let theme = t.app.theme.ui.clone();
    let (tx, ty) = find(&b, "target").unwrap();
    let (sx, sy) = find(&b, "src").unwrap();
    assert_eq!(b[(tx, ty)].fg, theme.muted, "ignored is muted");
    assert_eq!(b[(sx, sy)].fg, theme.accent, "directories take the accent colour");
    let (rx, ry) = find(&b, "README.md").unwrap();
    assert_eq!(b[(rx, ry)].fg, theme.fg);
}

#[test]
fn the_viewer_shows_numbered_lines_with_syntax_colours() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    t.key(KeyCode::Enter);
    pick(&mut t, "main.rs");
    let b = t.render(140, 30);
    let s = text(&b);
    assert!(s.contains("src/main.rs"), "{s}");
    assert!(s.contains("3 fn main"), "line numbers then text: {s}");
    let (x, y) = find(&b, "fn").expect("keyword drawn");
    assert_ne!(b[(x, y)].fg, t.app.theme.ui.fg, "highlighted");
}

#[test]
fn a_masked_secret_never_reaches_the_screen_until_revealed() {
    let f = files_fixture();
    for (w, h) in [(140u16, 30u16), (100, 30)] {
        let mut t = files_tab(&f, (w, h));
        pick(&mut t, ".env");
        if w < 120 {
            // narrow: the viewer is its own screen
            t.key(KeyCode::Enter);
        }
        let b = t.render(w, h);
        assert_absent(&b, FAKE_SECRET);
        assert_absent(&b, "TOKEN=");
        if w >= 120 {
            assert!(text(&b).contains("Hidden: this looks like a secret file. Press v to reveal."), "{}", text(&b));
            assert_absent(&b, "1 line");
        }
        // reveal: the content shows
        t.key(KeyCode::Char('v'));
        let b = t.render(w, h);
        assert!(text(&b).contains(FAKE_SECRET), "{}", text(&b));
        // and is gone again after hiding it
        t.key(KeyCode::Char('v'));
        assert_absent(&t.render(w, h), FAKE_SECRET);
    }
}

#[test]
fn a_directory_shows_loading_until_its_listing_arrives() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    t.app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let mut term = Terminal::new(TestBackend::new(140, 30)).unwrap();
    term.draw(|fr| ui::draw(&mut t.app, fr)).unwrap();
    let s = text(term.backend().buffer());
    assert!(s.contains("src/ loading…"), "{s}");
    t.pump();
    assert!(!text(&t.render(140, 30)).contains("loading…"));
}

#[test]
fn binary_files_and_symlinks_get_a_message() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "data.bin");
    let s = text(&t.render(140, 30));
    assert!(s.contains("binary file (5 bytes)"), "{s}");
    pick(&mut t, "link");
    let s = text(&t.render(140, 30));
    assert!(s.contains("symlink -> src/main.rs"), "{s}");
}

#[test]
fn files_tab_label_and_tiny_sizes() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    assert!(text(&t.render(140, 30)).contains("[3] Files"));
    t.key(KeyCode::Enter);
    pick(&mut t, "main.rs");
    for w in [20u16, 24, 40, 80, 119, 120, 200] {
        for h in [5u16, 6, 8, 30] {
            for focus_viewer in [false, true] {
                t.app.focus = if focus_viewer { Focus::Diff } else { Focus::Files };
                let _ = t.render(w, h);
            }
        }
    }
}

#[test]
fn hostile_file_names_and_lines_stay_in_their_pane() {
    let f = Fixture::new();
    f.write("plain.txt", "x\n");
    f.commit("base", NOW - DAY);
    f.write("esc\u{1b}[31mred.txt", format!("\u{1b}[31m{}\n{}\n", "x".repeat(5000), "ü".repeat(400)));
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "esc\u{1b}[31mred.txt");
    let b = t.render(140, 30);
    for y in 0..b.area.height {
        for x in 0..b.area.width {
            assert!(!b[(x, y)].symbol().contains('\u{1b}'), "raw escape in a cell");
        }
    }
    // sideways scrolling works and never panics
    t.app.focus = Focus::Diff;
    for _ in 0..5 {
        t.key(KeyCode::Char('l'));
        let _ = t.render(140, 30);
    }
}

#[test]
fn a_big_tree_draws_within_the_frame_budget() {
    let f = Fixture::new();
    for i in 0..5000 {
        std::fs::write(f.path().join(format!("f{i:05}.txt")), "x").unwrap();
    }
    f.commit("many", NOW - DAY);
    // every file changed: 5000 marks to look up, and each row draws one
    for i in 0..5000 {
        std::fs::write(f.path().join(format!("f{i:05}.txt")), "y").unwrap();
    }
    let mut t = files_tab(&f, (140, 40));
    assert_eq!(t.app.files_tab.rows.len(), 5000);
    assert_eq!(t.app.files_tab.marks.len(), 5000);
    let ms = t.render(140, 40);
    let m = ms.content.iter().filter(|c| c.symbol() == "M" && c.fg == t.app.theme.ui.status_modified).count();
    assert!(m >= 30, "the visible rows carry their mark: {m}");
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    let start = Instant::now();
    for _ in 0..20 {
        term.draw(|fr| ui::draw(&mut t.app, fr)).unwrap();
    }
    let per_frame = start.elapsed() / 20;
    eprintln!("5000-row Files tree: {per_frame:?} per frame (unoptimised)");
    // unoptimised builds on a loaded machine are many times slower than the 16 ms release budget
    if std::env::var_os("GITTY_SKIP_TIMING").is_none() {
        assert!(per_frame < Duration::from_millis(256), "{per_frame:?}");
    }
}

#[test]
fn files_clicks_select_and_toggle_and_a_double_click_edits() {
    use gitty::external::External;
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    let b = t.render(140, 30);
    let (x, y) = find(&b, "src").unwrap();
    t.click(x, y);
    assert!(t.app.files_tab.rows.iter().any(|r| r.name == "main.rs"), "a click on a directory opens it");
    // a second click right after is a double-click: no toggle back, and a directory is not edited
    t.click(x, y);
    assert!(t.app.files_tab.rows.iter().any(|r| r.name == "main.rs"));
    assert_eq!(t.app.external, None);
    t.clock += Duration::from_secs(1);
    t.app.tick(t.clock);
    let b = t.render(140, 30);
    let (x, y) = find(&b, "README.md").unwrap();
    t.click(x, y);
    assert_eq!(t.app.files_tab.shown.as_deref(), Some(std::path::Path::new("README.md")));
    assert_eq!(t.app.external, None, "one click only selects");
    t.click(x, y);
    assert_eq!(t.app.external, Some(External::Edit { path: f.path().join("README.md"), line: None }));
}

#[test]
fn the_wheel_scrolls_the_pane_under_the_pointer_in_files() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 20));
    t.key(KeyCode::Enter);
    pick(&mut t, "main.rs");
    let b = t.render(140, 20);
    let (vx, vy) = find(&b, "fn main").unwrap();
    let wheel = |t: &mut H, x, y| t.app.handle_mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: x, row: y, modifiers: KeyModifiers::NONE });
    wheel(&mut t, vx, vy);
    assert_eq!(t.app.files_tab.vscroll, 3);
    assert_eq!(t.app.files_tab.scroll, 0, "the tree stays put");
    let s = text(&t.render(140, 20));
    assert!(s.contains(" 4     step(0);") || s.contains("step(0)"), "{s}");
    // keys scroll the viewer when it has focus
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('j'));
    assert_eq!(t.app.files_tab.vscroll, 4);
    t.key(KeyCode::Char('g'));
    assert_eq!(t.app.files_tab.vscroll, 0);
    t.key(KeyCode::Char('G'));
    assert!(t.app.files_tab.vscroll > 10);
}

#[test]
fn a_double_click_off_the_file_rows_is_ordinary_clicks() {
    let f = files_fixture();
    let mut t = files_tab(&f, (140, 30));
    t.key(KeyCode::Enter);
    pick(&mut t, "main.rs");
    let b = t.render(140, 30);
    // the tab bar: two clicks on [3] Files while a file is selected
    let (x, y) = find(&b, "[3] Files").unwrap();
    t.click(x, y);
    t.click(x, y);
    assert_eq!(t.app.external, None);
    assert_eq!(t.app.tab, gitty::app::Tab::Files);
    // two clicks on [1] Changes switch the tab, and edit nothing
    let (x, y) = find(&b, "[1] Changes").unwrap();
    t.click(x, y);
    t.click(x, y);
    assert_eq!(t.app.external, None);
    assert_eq!(t.app.tab, gitty::app::Tab::Changes);
    t.key(KeyCode::Char('3'));
    t.render(140, 30);
    // the viewer, the separator, the bottom bar and the title row
    let b = t.render(140, 30);
    let (vx, vy) = find(&b, "fn main").unwrap();
    for (x, y) in [(vx, vy), (42, 20), (5, 29), (3, 1)] {
        t.clock += Duration::from_secs(1);
        t.app.tick(t.clock);
        t.click(x, y);
        t.click(x, y);
        assert_eq!(t.app.external, None, "({x},{y})");
    }
    assert_eq!(t.app.tab, gitty::app::Tab::Files);
}

#[test]
fn a_symlink_to_a_secret_shows_only_the_target_name() {
    let f = files_fixture();
    std::os::unix::fs::symlink(".env", f.path().join("notes-link")).unwrap();
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "notes-link");
    let b = t.render(140, 30);
    assert!(text(&b).contains("symlink -> .env"), "{}", text(&b));
    assert_absent(&b, FAKE_SECRET);
    assert_absent(&b, "TOKEN=");
}

fn mouse(t: &mut H, kind: MouseEventKind, x: u16, y: u16, mods: KeyModifiers) {
    t.app.handle_mouse(MouseEvent { kind, column: x, row: y, modifiers: mods });
}

#[test]
fn sideways_wheel_scrolls_the_viewer_only_and_stops_at_the_longest_line() {
    let f = Fixture::new();
    f.write("wide.txt", format!("short\n{}END\n", "x".repeat(200)));
    f.commit("base", NOW - DAY);
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "wide.txt");
    let b = t.render(140, 30);
    let (vx, vy) = find(&b, "short").unwrap();
    // the tree row (the viewer's title also says wide.txt)
    let (tx, ty) = (3, 2);
    assert_eq!(b[(tx, ty)].symbol(), "w");
    let none = KeyModifiers::NONE;
    mouse(&mut t, MouseEventKind::ScrollRight, vx, vy, none);
    assert_eq!(t.app.files_tab.hscroll, 8);
    mouse(&mut t, MouseEventKind::ScrollLeft, vx, vy, none);
    assert_eq!(t.app.files_tab.hscroll, 0);
    mouse(&mut t, MouseEventKind::ScrollLeft, vx, vy, none);
    assert_eq!(t.app.files_tab.hscroll, 0, "not past the left edge");
    // over the tree: nothing
    mouse(&mut t, MouseEventKind::ScrollRight, tx, ty, none);
    assert_eq!(t.app.files_tab.hscroll, 0);
    // Shift+wheel is a sideways wheel
    mouse(&mut t, MouseEventKind::ScrollDown, vx, vy, KeyModifiers::SHIFT);
    assert_eq!(t.app.files_tab.hscroll, 8);
    mouse(&mut t, MouseEventKind::ScrollUp, vx, vy, KeyModifiers::SHIFT);
    assert_eq!(t.app.files_tab.hscroll, 0);
    assert_eq!(t.app.files_tab.vscroll, 0, "Shift+wheel does not scroll down");
    // the end of the longest line (203 columns) is the limit: its last column is the pane's last
    let max = 203 - t.app.files_tab.visible;
    for _ in 0..100 {
        mouse(&mut t, MouseEventKind::ScrollRight, vx, vy, none);
    }
    assert_eq!(t.app.files_tab.hscroll, max);
    let s = text(&t.render(140, 30));
    assert!(s.contains("END"), "the end of the longest line is shown: {s}");
    // the keys obey the same limit
    t.app.focus = Focus::Diff;
    for _ in 0..40 {
        t.key(KeyCode::Char('l'));
    }
    assert_eq!(t.app.files_tab.hscroll, max);
    t.key(KeyCode::Char('h'));
    assert_eq!(t.app.files_tab.hscroll, max - 8);
}

/// The cells of the diff or viewer pane that hold `sym`.
fn marks(t: &H, b: &Buffer, sym: &str) -> Vec<(u16, u16)> {
    let r = t.app.hits.panes.diff.unwrap();
    (r.y..r.bottom()).flat_map(|y| (r.x..r.right()).map(move |x| (x, y))).filter(|&(x, y)| b[(x, y)].symbol() == sym).collect()
}

fn no_marks(t: &H, b: &Buffer) -> bool {
    marks(t, b, "‹").is_empty() && marks(t, b, "›").is_empty()
}

fn wide_changes(size: (u16, u16)) -> (Fixture, H) {
    let f = Fixture::new();
    f.write("wide.txt", "short\nkeep\n");
    f.commit("base", NOW - DAY);
    f.write("wide.txt", format!("short\n{}END\n", "y".repeat(300)));
    let mut t = H::new(&f, "github-dark", size);
    t.key(KeyCode::Char('1'));
    t.select_change("wide.txt");
    t.render(size.0, size.1);
    (f, t)
}

#[test]
fn diff_sideways_scroll_stops_at_the_widest_line_and_marks_hidden_text() {
    let (_f, mut t) = wide_changes((140, 30));
    let none = KeyModifiers::NONE;
    let (x, y) = (120, 10);
    let b = t.render(140, 30);
    assert!(marks(&t, &b, "‹").is_empty(), "nothing hidden on the left yet");
    assert_eq!(marks(&t, &b, "›").len(), 1, "only the long line continues");
    mouse(&mut t, MouseEventKind::ScrollRight, x, y, none);
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 8);
    mouse(&mut t, MouseEventKind::ScrollDown, x, y, KeyModifiers::SHIFT);
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 16);
    mouse(&mut t, MouseEventKind::ScrollLeft, x, y, none);
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 8);
    let b = t.render(140, 30);
    assert_eq!(marks(&t, &b, "‹").len(), 3, "every line has text hidden on the left");
    assert_eq!(marks(&t, &b, "›").len(), 1);
    // 303 columns of text is the limit
    let max = 303 - t.app.diff.as_ref().unwrap().visible;
    for _ in 0..100 {
        mouse(&mut t, MouseEventKind::ScrollRight, x, y, none);
    }
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, max);
    let b = t.render(140, 30);
    assert!(text(&b).contains("END"));
    assert!(marks(&t, &b, "›").is_empty(), "the end is reached");
    assert_eq!(marks(&t, &b, "‹").len(), 3);
    t.app.focus = Focus::Diff;
    for _ in 0..3 {
        t.key(KeyCode::Char('l'));
    }
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, max);
}

#[test]
fn diff_sideways_scroll_does_nothing_when_everything_fits() {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.commit("base", NOW - DAY);
    f.write("a.txt", "one\ntwo\n");
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.key(KeyCode::Char('1'));
    t.select_change("a.txt");
    t.render(140, 30);
    mouse(&mut t, MouseEventKind::ScrollRight, 120, 10, KeyModifiers::NONE);
    mouse(&mut t, MouseEventKind::ScrollDown, 120, 10, KeyModifiers::SHIFT);
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('l'));
    t.key(KeyCode::Right);
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 0);
    let b = t.render(140, 30);
    assert!(no_marks(&t, &b));
}

#[test]
fn split_diff_scrolls_by_the_narrower_side_and_a_wrapped_diff_not_at_all() {
    let (_f, mut t) = wide_changes((140, 30));
    t.app.split_pref = Some(true);
    let b = t.render(140, 30);
    // each half has one gutter (3 columns here), the marker and its space
    let w = t.app.hits.panes.diff.unwrap().width;
    let (left, right) = ((w - 1) / 2, w - (w - 1) / 2 - 1);
    let visible = t.app.diff.as_ref().unwrap().visible;
    assert_eq!(visible, left.min(right) - 5, "the narrower half decides");
    assert!(!marks(&t, &b, "›").is_empty());
    t.app.focus = Focus::Diff;
    for _ in 0..100 {
        t.key(KeyCode::Char('l'));
    }
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 303 - visible);
    // wrapped: nothing to scroll, nothing marked
    t.key(KeyCode::Char('W'));
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 0);
    t.key(KeyCode::Char('l'));
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 0);
    let b = t.render(140, 30);
    assert!(no_marks(&t, &b));
}

#[test]
fn a_resize_keeps_the_diff_scroll_valid() {
    let (_f, mut t) = wide_changes((100, 30));
    t.app.split_pref = Some(false);
    t.app.focus = Focus::Diff;
    for _ in 0..100 {
        t.key(KeyCode::Char('l'));
    }
    t.render(100, 30);
    let narrow = t.app.diff.as_ref().unwrap().hscroll;
    assert_eq!(narrow, 303 - t.app.diff.as_ref().unwrap().visible);
    // a wider pane shows more: the end moves left and the offset follows it
    t.render(160, 30);
    let d = t.app.diff.as_ref().unwrap();
    assert!(d.hscroll < narrow);
    assert_eq!(d.hscroll, 303 - d.visible);
    // a narrower pane's end is farther: the offset stays valid and can grow again
    t.render(100, 30);
    assert!(t.app.diff.as_ref().unwrap().hscroll <= narrow);
    t.key(KeyCode::Char('l'));
    for _ in 0..100 {
        t.key(KeyCode::Char('l'));
    }
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, narrow);
    // wide enough for every line: back at the left edge, no markers
    let b = t.render(400, 30);
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 0);
    assert!(no_marks(&t, &b));
}

#[test]
fn files_viewer_resize_open_and_refresh_follow_the_rule() {
    let f = Fixture::new();
    f.write("wide.txt", format!("short\n{}END\n", "x".repeat(200)));
    f.write("other.txt", format!("{}\n", "z".repeat(200)));
    f.write("tiny.txt", "t\n");
    f.commit("base", NOW - DAY);
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "wide.txt");
    t.render(140, 30);
    t.app.focus = Focus::Diff;
    for _ in 0..50 {
        t.key(KeyCode::Char('l'));
    }
    let b = t.render(140, 30);
    let at140 = t.app.files_tab.hscroll;
    assert_eq!(u32::from(at140), 203 - u32::from(t.app.files_tab.visible));
    assert!(marks(&t, &b, "›").is_empty());
    assert_eq!(marks(&t, &b, "‹").len(), 2, "both lines have text hidden on the left");
    // growing the pane pulls the offset back to the new end
    t.render(180, 30);
    assert_eq!(u32::from(t.app.files_tab.hscroll), 203 - u32::from(t.app.files_tab.visible));
    assert!(t.app.files_tab.hscroll < at140);
    // shrinking keeps it valid
    t.render(100, 30);
    assert!(u32::from(t.app.files_tab.hscroll) <= 203 - u32::from(t.app.files_tab.visible));
    // the same file read again keeps the place
    t.render(140, 30);
    let before = t.app.files_tab.hscroll;
    t.app.refresh_files();
    t.pump();
    assert_eq!(t.app.files_tab.hscroll, before);
    // another file starts at the left edge, and one that fits cannot scroll
    t.app.focus = Focus::Files;
    pick(&mut t, "other.txt");
    assert_eq!(t.app.files_tab.hscroll, 0);
    pick(&mut t, "tiny.txt");
    t.app.focus = Focus::Diff;
    t.render(140, 30);
    t.key(KeyCode::Char('l'));
    mouse(&mut t, MouseEventKind::ScrollRight, 100, 10, KeyModifiers::NONE);
    assert_eq!(t.app.files_tab.hscroll, 0);
    let b = t.render(140, 30);
    assert!(no_marks(&t, &b));
}

#[test]
fn viewer_edge_marks_show_only_where_text_is_hidden() {
    let f = Fixture::new();
    f.write("wide.txt", format!("short\n{}END\n", "x".repeat(200)));
    f.commit("base", NOW - DAY);
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "wide.txt");
    let b = t.render(140, 30);
    let (tx, y) = find(&b, "short").unwrap();
    let r = t.app.hits.panes.diff.unwrap();
    assert_eq!(b[(r.right() - 1, y + 1)].symbol(), "›", "the long line runs on");
    assert_ne!(b[(r.right() - 1, y)].symbol(), "›", "the short line does not");
    assert_eq!(b[(tx, y + 1)].symbol(), "x", "no left mark at the left edge");
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('l'));
    let b = t.render(140, 30);
    assert_eq!(b[(tx, y + 1)].symbol(), "‹");
    assert_eq!(b[(tx, y)].symbol(), "‹", "a line scrolled out of view entirely has hidden text too");
    assert_eq!(b[(tx - 1, y + 1)].symbol(), " ", "the gutter is untouched");
}

#[test]
fn the_no_newline_mark_is_reachable_at_the_end_of_the_diff() {
    let f = Fixture::new();
    f.write("n.txt", "a\n");
    f.commit("base", NOW - DAY);
    f.write("n.txt", format!("a\n{}", "q".repeat(300)));
    let mut t = H::new(&f, "github-dark", (140, 30));
    t.app.split_pref = Some(false);
    t.key(KeyCode::Char('1'));
    t.select_change("n.txt");
    t.render(140, 30);
    t.app.focus = Focus::Diff;
    for _ in 0..100 {
        t.key(KeyCode::Char('l'));
    }
    assert_eq!(t.app.diff.as_ref().unwrap().hscroll, 302 - t.app.diff.as_ref().unwrap().visible);
    assert!(text(&t.render(140, 30)).contains("q ⊘"));
}

#[test]
fn a_refresh_that_turns_text_into_binary_resets_the_viewer_scroll() {
    let f = Fixture::new();
    f.write("wide.txt", format!("{}\n", "x".repeat(300)));
    f.commit("base", NOW - DAY);
    let mut t = files_tab(&f, (140, 30));
    pick(&mut t, "wide.txt");
    t.render(140, 30);
    t.app.focus = Focus::Diff;
    t.key(KeyCode::Char('l'));
    assert_eq!(t.app.files_tab.hscroll, 8);
    f.write("wide.txt", b"ab\0cd");
    t.app.refresh_files();
    t.pump();
    assert!(matches!(t.app.files_tab.viewing, gitty::app::files::Viewing::Ready(gitty::msg::FileView::Binary { .. })));
    assert_eq!((t.app.files_tab.hscroll, t.app.files_tab.widest), (0, 0));
}

#[test]
fn a_megabyte_single_line_file_scrolls_to_the_cap_without_slowness() {
    let f = Fixture::new();
    f.write("one.txt", "x\n");
    f.commit("base", NOW - DAY);
    f.write("huge.txt", "w".repeat(1_000_000));
    let mut t = files_tab(&f, (140, 30));
    let start = Instant::now();
    pick(&mut t, "huge.txt");
    t.render(140, 30);
    assert_eq!(t.app.files_tab.widest, 10_000);
    t.app.focus = Focus::Diff;
    for _ in 0..1300 {
        t.key(KeyCode::Char('l'));
    }
    let b = t.render(140, 30);
    assert_eq!(u32::from(t.app.files_tab.hscroll), 10_000 - u32::from(t.app.files_tab.visible));
    assert!(text(&b).contains("www"));
    if std::env::var_os("GITTY_SKIP_TIMING").is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "{:?}", start.elapsed());
    }
}

#[test]
fn shift_wheel_events_are_not_merged_with_plain_ones() {
    use crossterm::event::Event;
    let ev = |mods| Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown, column: 5, row: 5, modifiers: mods });
    let out = gitty::input::coalesce(vec![ev(KeyModifiers::NONE), ev(KeyModifiers::NONE), ev(KeyModifiers::SHIFT)]);
    assert_eq!(out.iter().map(|e| e.repeat).collect::<Vec<_>>(), [2, 1]);
}

// ---- a merge in progress ----

/// `topic` and `main` both changed a.txt, and `git merge topic` stopped on it.
fn merging() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic\n");
    f.commit("topic edit", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("main edit", 1_700_000_200);
    let out = std::process::Command::new("git")
        .current_dir(f.path())
        .args(["merge", "topic"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(!out.status.success());
    f
}

fn top_line(b: &Buffer, w: u16) -> String {
    (0..w).map(|x| b[(x, 0)].symbol().to_string()).collect()
}

#[test]
fn the_banner_names_the_merge_in_the_warning_colour() {
    use gitty_core::forge::PrState;
    let f = merging();
    let mut t = H::new(&f, "github-dark", (140, 30));
    with_pr(&mut t, 42, PrState::Open);
    let b = t.render(140, 30);
    let top = top_line(&b, 140);
    assert!(top.contains("⎇ main  MERGING topic · 1 conflict"), "{top}");
    let (x, y) = find(&b, "MERGING").unwrap();
    let ui = t.app.theme.ui.clone();
    assert_eq!(b[(x, y)].fg, ui.warning);
    assert_eq!(b[(x, y)].bg, ui.status_bg);
    // the PR badge comes after it and still answers clicks
    let (px, py) = find(&b, "PR #42").unwrap();
    assert!(px > x);
    assert_eq!(t.app.hits.pr_badge, Some(ratatui::layout::Rect::new(px, py, 6, 1)));
    assert!(top.contains("[2] History"), "{top}");
}

#[test]
fn the_banner_gives_up_details_before_the_word_at_narrow_widths() {
    use gitty_core::forge::PrState;
    let f = merging();
    let mut t = H::new(&f, "github-dark", (80, 24));
    with_pr(&mut t, 12345, PrState::Open);
    let b = t.render(80, 24);
    let top = top_line(&b, 80);
    assert!(top.contains("MERGING"), "{top}");
    assert!(top.contains("[2] History") && top.contains("⎇ main"), "{top}");
    let mut seen = std::collections::BTreeSet::new();
    for w in (30..=140).rev() {
        let b = t.render(w, 24);
        let top = top_line(&b, w);
        // below ~64 columns the repository name and branch use the room: no banner, no panic
        assert!(w < 64 || top.contains("MERGING"), "{w}: {top}");
        let badge = top.contains("PR #12345");
        assert_eq!(badge, t.app.hits.pr_badge.is_some(), "{w}: {top}");
        seen.insert((top.contains("MERGING topic"), top.contains("1 conflict")));
    }
    // full, then without the branch, then the bare word
    assert!(seen.contains(&(true, true)) && seen.contains(&(false, true)) && seen.contains(&(false, false)), "{seen:?}");
}

#[test]
fn no_operation_no_banner() {
    let f = fixture();
    let mut t = H::new(&f, "github-dark", (140, 30));
    assert!(find(&t.render(140, 30), "MERGING").is_none());
}

#[test]
fn the_dialog_shows_continue_disabled_with_the_reason_then_enabled() {
    let f = merging();
    let mut t = H::new(&f, "github-dark", (100, 30));
    t.key(KeyCode::Char('m'));
    let b = t.render(100, 30);
    let s = text(&b);
    assert!(s.contains("Merge in progress"), "{s}");
    assert!(s.contains("Merging topic into main"), "{s}");
    assert!(s.contains("1 file still conflicts: resolve it and stage it first"), "{s}");
    assert!(s.contains("c continue (not yet)") && s.contains("a abort") && s.contains("Esc close"), "{s}");
    let (x, y) = find(&b, "c continue").unwrap();
    assert_eq!(b[(x, y)].fg, t.app.theme.ui.muted, "disabled looks disabled");
    // resolved and staged
    f.write("a.txt", "both\n");
    f.git(&["add", "a.txt"]);
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::STATE));
    t.pump();
    let b = t.render(100, 30);
    let s = text(&b);
    assert!(s.contains("No conflicts left") && !s.contains("still conflict") && s.contains("c continue ·"), "{s}");
    let (x, y) = find(&b, "c continue").unwrap();
    assert_eq!(b[(x, y)].fg, t.app.theme.ui.accent);
}

#[test]
fn the_abort_question_shows_all_of_its_words_at_80_columns() {
    let f = merging();
    let mut t = H::new(&f, "github-dark", (80, 24));
    t.key(KeyCode::Char('m'));
    t.key(KeyCode::Char('a'));
    let s = text(&t.render(80, 24));
    assert!(s.contains("Abort the merge?"), "{s}");
    assert!(s.contains("restore them."), "the end of the sentence is not cut: {s}");
    assert!(s.contains("Enter abort") && s.contains("Esc cancel"), "{s}");
}

#[test]
fn the_dialogs_fit_small_terminals_and_keep_their_keys() {
    let f = merging();
    for (w, h) in [(30u16, 12u16), (80, 14), (40, 8), (30, 6)] {
        let mut t = H::new(&f, "github-dark", (w, h));
        t.key(KeyCode::Char('m'));
        let s = text(&t.render(w, h));
        assert!(s.contains("a abort") && s.contains("Esc"), "{w}x{h}: {s}");
        if w >= 80 {
            assert!(s.contains("c continue (not yet)") && s.contains("Esc close"), "{w}x{h}: {s}");
        }
        t.key(KeyCode::Char('a'));
        let b = t.render(w, h);
        let s = text(&b);
        assert!(s.contains("Enter abort") && s.contains("Esc cancel"), "{w}x{h}: {s}");
        // nothing is drawn outside the screen, and the box stays on it
        assert_eq!(b.area.width, w);
        if w >= 80 {
            assert!(s.contains("restore them."), "{w}x{h}: the sentence is whole: {s}");
        }
    }
}

// ---- the conflict view ----

/// Runs a command that is expected to stop on conflicts.
fn stops(f: &Fixture, args: &[&str]) {
    let out = std::process::Command::new("git").current_dir(f.path()).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env("GIT_EDITOR", "true").output().unwrap();
    assert!(!out.status.success(), "{args:?} did not stop");
}

/// `a.txt` has two conflicting changes (far apart), merged in diff3 style so the base shows.
fn merging_two_blocks() -> Fixture {
    let f = Fixture::new();
    f.git(&["config", "merge.conflictStyle", "diff3"]);
    let body = |a: &str, b: &str| format!("one\n{a}\n{}three\n{b}\nend\n", (0..12).map(|i| format!("filler {i}\n")).collect::<String>());
    f.write("a.txt", body("two", "four"));
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", body("two topic", "four topic"));
    f.commit("topic edit", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", body("two main", "four main"));
    f.commit("main edit", 1_700_000_200);
    stops(&f, &["merge", "topic"]);
    f
}

fn conflict_view(f: &Fixture, size: (u16, u16)) -> H {
    let mut t = H::new(f, "github-dark", size);
    t.app.set_tab(gitty::app::Tab::Changes);
    t.pump();
    // below 120 columns the view is a screen of its own: Tab goes to it
    if size.0 < 120 {
        t.key(KeyCode::Tab);
    }
    t
}

fn bg_of(b: &Buffer, needle: &str) -> Color {
    let (x, y) = find(b, needle).unwrap_or_else(|| panic!("no {needle:?}"));
    b[(x, y)].bg
}

#[test]
fn the_conflict_view_tints_the_sides_and_names_them() {
    for w in [100u16, 140] {
        let f = merging();
        let mut t = conflict_view(&f, (w, 30));
        let b = t.render(w, 30);
        let d = t.app.theme.diff.clone();
        // the markers are not shown raw: headers and a rule stand in for them
        for raw in ["<<<<<<<", "=======", ">>>>>>>"] {
            assert!(find(&b, raw).is_none(), "{w}: {raw}");
        }
        assert!(find(&b, "◂ Current (main)").is_some() && find(&b, "▸ Incoming (topic)").is_some(), "{w}");
        // the only block is the current one: its sides are tinted strongly
        let (hx, hy) = find(&b, "◂ Current").unwrap();
        assert_eq!(b[(hx, hy)].bg, d.ours_head);
        assert_eq!(b[(hx + 20, hy + 1)].bg, d.ours_current_bg, "{w}: the text row under the header");
        let (hx, hy) = find(&b, "▸ Incoming").unwrap();
        assert_eq!(b[(hx, hy)].bg, d.theirs_head);
        assert_eq!(b[(hx + 20, hy + 1)].bg, d.theirs_current_bg, "{w}");
        // the file list counts the blocks, the title says which one this is, the bar names the keys
        assert_eq!(find(&b, "a.txt (1)").is_some(), w >= 120, "{w}: the list shows with the view from 120 columns");
        assert!(find(&b, "conflict 1/1").is_some(), "{w}");
        assert!(find(&b, "o keep Current").is_some() && find(&b, "t take Incoming").is_some(), "{w}");
        assert!(t.app.diff.is_none());
    }
}

#[test]
fn the_current_block_is_marked_and_n_moves_it() {
    let f = merging_two_blocks();
    let mut t = conflict_view(&f, (120, 40));
    let d = t.app.theme.diff.clone();
    let b = t.render(120, 40);
    // diff3: the base is dim
    assert!(find(&b, "│ Base").is_some());
    let (bx, by) = find(&b, "│ Base").unwrap();
    assert_eq!(b[(bx + 12, by + 1)].bg, d.base_bg, "the base text of the first block");
    assert_eq!(bg_of(&b, "two main"), d.ours_current_bg);
    assert_eq!(bg_of(&b, "four main"), d.ours_bg, "the other block is tinted lightly");
    assert!(find(&b, "conflict 1/2").is_some());
    // the keys sit on the closing rule of the current block only (and in the bar)
    assert_eq!(find_all(&b, "o keep Current").len(), 2);
    let (_, my) = find(&b, "two main").unwrap();
    assert_eq!(b[(t.app.hits.panes.diff.unwrap().x, my)].symbol(), "▌");
    t.key(KeyCode::Char('n'));
    let b = t.render(120, 40);
    assert!(find(&b, "conflict 2/2").is_some());
    assert_eq!(bg_of(&b, "four main"), d.ours_current_bg, "the view scrolled to it");
    // wraps round
    t.key(KeyCode::Char('n'));
    assert!(find(&t.render(120, 40), "conflict 1/2").is_some());
    t.key(KeyCode::Char('p'));
    assert!(find(&t.render(120, 40), "conflict 2/2").is_some());
}

#[test]
fn controls_in_a_conflict_are_shown_as_carets() {
    let f = Fixture::new();
    f.write("a.txt", "base\n");
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("a.txt", "topic \u{1b}[31mred\n");
    f.commit("topic edit", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("a.txt", "main\n");
    f.commit("main edit", 1_700_000_200);
    stops(&f, &["merge", "topic"]);
    let mut t = conflict_view(&f, (100, 30));
    let b = t.render(100, 30);
    assert!(find(&b, "topic ^[").is_some() && find(&b, "[31mred").is_some());
    for y in 0..30 {
        for x in 0..100 {
            assert!(!b[(x, y)].symbol().contains('\u{1b}'));
        }
    }
}

#[test]
fn a_narrow_conflict_view_clips_without_panicking() {
    let f = merging_two_blocks();
    let mut t = conflict_view(&f, (100, 30));
    for (w, h) in [(60, 20), (44, 12), (30, 8), (21, 6), (100, 30)] {
        let b = t.render(w, h);
        assert_eq!(b.area.width, w);
    }
    let b = t.render(100, 30);
    assert!(find(&b, "conflict 1/2").is_some());
}

#[test]
fn a_binary_conflict_explains_itself_and_names_what_the_keys_do() {
    let f = Fixture::new();
    f.write("img.bin", b"base\0".as_slice());
    f.commit("base", 1_700_000_000);
    f.git(&["switch", "-q", "-c", "topic"]);
    f.write("img.bin", b"topic\0".as_slice());
    f.commit("topic", 1_700_000_100);
    f.git(&["switch", "-q", "main"]);
    f.write("img.bin", b"main\0".as_slice());
    f.commit("main", 1_700_000_200);
    stops(&f, &["merge", "topic"]);
    let mut t = conflict_view(&f, (110, 30));
    let b = t.render(110, 30);
    assert!(find(&b, "Both sides changed this file").is_some());
    assert!(find(&b, "binary file").is_some());
    assert!(find(&b, "Current (main): keep its version of the file").is_some());
    assert!(find(&b, "Incoming (topic): keep its version of the file").is_some());
    assert!(find(&b, "o keep Current").is_some());
    assert!(find(&b, "both").is_none(), "no markers, no keep-both");
}

#[test]
fn the_key_hint_shows_on_the_rule_and_in_the_bar_at_80_100_and_140_columns() {
    for w in [80u16, 100, 140] {
        let f = merging();
        let mut t = conflict_view(&f, (w, 24));
        let b = t.render(w, 24);
        let hints = find_all(&b, "o keep Current");
        assert_eq!(hints.len(), 2, "{w}: on the closing rule and in the bar");
        assert!(find(&b, "t take Incoming · b both").is_some(), "{w}: the rule has all three");
        assert!(find(&b, "u undo").is_some(), "{w}: undo is always listed");
        // the shadowing of p, u and e is said where there is room for it
        assert_eq!(find(&b, "p/u/e act on conflicts here").is_some(), w >= 120, "{w}");
        assert!(find(&b, "◂ Current (main)").is_some() && find(&b, "▸ Incoming (topic)").is_some(), "{w}");
    }
}

#[test]
fn an_ambiguous_block_and_unknown_markers_are_flagged_on_screen() {
    let f = merging();
    f.write("a.txt", "<<<<<<< HEAD\nTitle\n=======\nmain\n=======\ntopic\n>>>>>>> topic\n");
    let mut t = conflict_view(&f, (120, 24));
    let b = t.render(120, 24);
    assert!(find(&b, "⚠ ambiguous markers: e to edit").is_some());
    assert!(find(&b, "a.txt (1)").is_some(), "still counted");
    f.write("a.txt", "<<<<<<<< x\nmain\n========\ntopic\n>>>>>>>> y\n");
    t.app.handle_msg(Msg::Changed(gitty_core::watch::Changed::WORKTREE));
    let b = t.render(120, 24);
    assert!(find(&b, "Conflict markers not understood: open in the editor (e)").is_some());
    assert!(find(&b, "markers not understood").is_some());
}

#[test]
fn the_resolve_now_prompt_shows_every_key_and_its_words_at_80_and_140_columns() {
    let f = merging();
    for (w, h) in [(80u16, 24u16), (140, 30)] {
        let mut t = H::new(&f, "github-dark", (w, h));
        let state = t.app.op.clone().unwrap();
        let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"].map(String::from).to_vec();
        t.app.handle_msg(Msg::Conflicted { doing: "Merging topic into main".into(), files, state, stash: Some(gitty::msg::MergeStash { pushed: "abc".into(), message: "gitty: auto-stash from main".into() }) });
        let b = t.render(w, h);
        let s = text(&b);
        assert!(s.contains("Resolve now?"), "{w}: {s}");
        assert!(s.contains("Merging topic into main hit conflicts in 5 files (a.rs, b.rs, c.rs, +2)."), "{w}: {s}");
        assert!(s.contains("auto-stash from"), "{w}: {s}");
        assert!(s.contains("finish the merge."), "{w}: the sentence is whole: {s}");
        assert!(s.contains("Enter resolve now") && s.contains("a abort the merge") && s.contains("Esc decide later"), "{w}: {s}");
        assert!(s.contains("Deciding later keeps the merge open: press m"), "{w}: {s}");
        let (x, y) = find(&b, "Resolve now?").unwrap();
        assert_eq!(b[(x, y)].fg, t.app.theme.ui.warning, "{w}");
    }
}

/// The foreground of the first cell on row `y` that shows `sym` in colour `fg`.
fn marked(b: &Buffer, y: u16, sym: &str, fg: Color) -> bool {
    (0..b.area.width).any(|x| b[(x, y)].symbol() == sym && b[(x, y)].fg == fg)
}

#[test]
fn marks_draw_in_the_changes_colours_and_folded_folders_carry_a_dot() {
    let f = files_fixture();
    f.write("README.md", "# changed\n");
    f.write("src/main.rs", main_rs(1));
    std::fs::remove_file(f.path().join("src/lib.rs")).unwrap();
    for w in [100u16, 140] {
        let mut t = files_tab(&f, (w, 30));
        let ui = t.app.theme.ui.clone();
        let b = t.render(w, 30);
        let row = |name: &str| find(&b, name).unwrap_or_else(|| panic!("no {name}: {}", text(&b))).1;
        // a modified file: its letter in the modified colour
        assert!(marked(&b, row("README.md"), "M", ui.status_modified), "{w}: {}", text(&b));
        // untracked is A in the added colour, like the Changes list
        assert!(marked(&b, row("link ->"), "A", ui.status_added), "{w}: {}", text(&b));
        // src is folded: one dot, in the strongest colour below it (deleted beats modified)
        assert!(marked(&b, row("src/"), "●", ui.status_deleted), "{w}: {}", text(&b));
        assert!(t.app.files_tab.rows.iter().all(|r| r.name != "main.rs"), "src is still folded");
        // unchanged rows carry nothing
        let y = row(".gitignore");
        assert!(!(0..b.area.width).any(|x| matches!(b[(x, y)].symbol(), "●" | "M" | "A" | "D")), "{w}");
        // opened: the file has its own letter and the folder keeps its dot
        pick(&mut t, "src");
        t.key(KeyCode::Enter);
        let b = t.render(w, 30);
        let y = find(&b, " main.rs").unwrap().1;
        assert!(marked(&b, y, "M", ui.status_modified), "{w}: {}", text(&b));
        assert!(marked(&b, find(&b, "src/").unwrap().1, "●", ui.status_deleted), "{w}");
        assert!(!text(&b).contains("lib.rs"), "a deleted file is not listed");
        // a selected row keeps its mark, on the selection background
        pick(&mut t, "main.rs");
        let b = t.render(w, 30);
        let y = find(&b, " main.rs").unwrap().1;
        let c = (0..b.area.width).map(|x| &b[(x, y)]).find(|c| c.symbol() == "M" && c.fg == ui.status_modified).unwrap();
        assert_eq!(c.bg, ui.selection);
        // the ignored toggle shows in the footer and the toast
        t.key(KeyCode::Char('i'));
        let s = text(&t.render(w, 30));
        assert!(s.contains("ignored files hidden"), "{w}: {s}");
        assert!(!s.contains("target"), "{w}: {s}");
    }
}

#[test]
fn the_files_footer_at_80_columns_keeps_help_and_quit() {
    let f = files_fixture();
    let mut t = files_tab(&f, (80, 24));
    let b = t.render(80, 24);
    let last = text(&b).lines().last().unwrap_or("").to_string();
    assert!(last.contains("? help") && last.contains("q quit"), "{last}");
}
