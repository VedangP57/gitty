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
        ("vendor/lib", "Submodule"),
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
    assert!(s.contains("expand context"), "{s}");
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
