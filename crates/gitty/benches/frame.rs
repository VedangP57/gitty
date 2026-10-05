//! One full 220×60 frame (history list, file list and a highlighted diff) against the spec §8
//! keypress-to-frame budget of 16 ms. Criterion reports the timing; afterwards the bench times
//! 200 more frames and exits 2 when the slowest one misses the budget.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::Criterion;
use gitty::app::{App, AppInit};
use gitty::config::{Config, UiState};
use gitty::exec::exec;
use gitty::msg::Gens;
use gitty::theme::{ColorDepth, Registry};
use gitty_core::{Handle, Repo};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const SIZE: (u16, u16) = (220, 60);
const BUDGET: Duration = Duration::from_millis(16);

fn git(dir: &Path, args: &[&str], date: Option<i64>) {
    let mut c = Command::new("git");
    c.current_dir(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1");
    if let Some(d) = date {
        c.env("GIT_AUTHOR_DATE", format!("{d} +0000")).env("GIT_COMMITTER_DATE", format!("{d} +0000"));
    }
    let out = c.output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

/// 300 commits; the last rewrites every tenth line of a 3,000-line Rust file.
fn fixture(dir: &Path) {
    git(dir, &["init", "-q", "-b", "main"], None);
    git(dir, &["config", "user.name", "Bench"], None);
    git(dir, &["config", "user.email", "bench@example.com"], None);
    let line = |i: usize, v: &str| format!("    let value_{i} = compute({i}, \"{v}\").unwrap_or_default(); // step {i}\n");
    let file = |v: &str, every: usize| -> String {
        let mut s = String::from("fn main() {\n");
        for i in 0..3000 {
            s.push_str(&line(i, if every > 0 && i % every == 0 { v } else { "base" }));
        }
        s.push_str("}\n");
        s
    };
    for k in 0..299 {
        std::fs::write(dir.join(format!("notes-{}.txt", k % 20)), format!("note {k}\n")).unwrap();
        if k == 0 {
            std::fs::write(dir.join("main.rs"), file("base", 0)).unwrap();
        }
        git(dir, &["add", "-A"], None);
        git(dir, &["commit", "-q", "-m", &format!("Change number {k} to the notes")], Some(1_700_000_000 + k as i64 * 3600));
    }
    std::fs::write(dir.join("main.rs"), file("changed", 10)).unwrap();
    git(dir, &["commit", "-q", "-am", "Rewrite every tenth step"], Some(1_700_000_000 + 300 * 3600));
}

fn settle(app: &mut App, h: &Handle, gens: &Gens, clock: &mut Instant) {
    for _ in 0..1000 {
        let reqs = app.take_requests();
        if reqs.is_empty() {
            match app.next_deadline() {
                Some(d) => {
                    *clock = (*clock).max(d) + Duration::from_millis(1);
                    app.tick(*clock);
                    continue;
                }
                None => return,
            }
        }
        let mut out = Vec::new();
        for r in reqs {
            exec(h, r, &mut |m| out.push(m), gens);
        }
        for m in out {
            app.handle_msg(m);
        }
    }
    panic!("the app did not settle");
}

fn main() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let repo = Repo::open(tmp.path()).unwrap();
    let h = repo.handle();
    let registry = Registry::load(None);
    let theme = registry.resolve("github-dark", ColorDepth::True, None).unwrap();
    let gens = Arc::new(Gens::default());
    let mut clock = Instant::now();
    let mut app = App::new(AppInit {
        repo_name: "bench".into(),
        config: Config::default(),
        registry,
        theme,
        depth: ColorDepth::True,
        ui_state: UiState::default(),
        config_path: None,
        state_path: None,
        gens: gens.clone(),
        now: 1_700_000_000 + 301 * 3600,
        clock,
        size: SIZE,
    });
    app.handle_resize(SIZE.0, SIZE.1);
    settle(&mut app, &h, &gens, &mut clock);
    let i = app.files.as_ref().and_then(|f| f.iter().position(|f| f.path == "main.rs")).expect("main.rs in the newest commit");
    app.select_file(i);
    settle(&mut app, &h, &gens, &mut clock);
    assert!(app.diff.is_some(), "the diff is loaded");

    let mut term = Terminal::new(TestBackend::new(SIZE.0, SIZE.1)).unwrap();
    term.draw(|f| gitty::ui::draw(&mut app, f)).unwrap();
    let screen: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("Rewrite every tenth step") && screen.contains("\"changed\""), "history and diff on screen");
    let mut c = Criterion::default().configure_from_args();
    c.bench_function("frame 220x60 history+diff", |b| {
        b.iter(|| {
            term.draw(|f| gitty::ui::draw(&mut app, f)).unwrap();
        })
    });
    c.final_summary();

    let mut times: Vec<Duration> = (0..200)
        .map(|_| {
            let t = Instant::now();
            term.draw(|f| gitty::ui::draw(&mut app, f)).unwrap();
            t.elapsed()
        })
        .collect();
    times.sort();
    let (p50, max) = (times[100], times[199]);
    println!("frame 220x60: p50 {:.2}ms, slowest {:.2}ms, budget < {}ms", p50.as_secs_f64() * 1e3, max.as_secs_f64() * 1e3, BUDGET.as_millis());
    if max >= BUDGET {
        eprintln!("BUDGET MISSED frame 220x60: slowest {:.2}ms, budget < {}ms", max.as_secs_f64() * 1e3, BUDGET.as_millis());
        std::process::exit(2);
    }
}
