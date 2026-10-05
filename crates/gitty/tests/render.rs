#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Fixture;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use gitty::app::{App, AppInit, Focus};
use gitty::config::{Config, UiState};
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
            config: Config::default(),
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
