//! Process entry: argument parsing, terminal setup and the event loop (no tick: the loop
//! sleeps until input, a worker message, a signal, or a deadline).

use std::io::{BufWriter, Write};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use crossbeam_channel::{select, unbounded};
use crossterm::event::Event;
use gitty_core::Repo;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::app::{App, AppInit, Toast};
use crate::config::{Config, UiState, paths};
use crate::msg::Gens;
use crate::theme::{ColorDepth, Registry};
use crate::workers::Workers;
use crate::{input, term, ui};

const USAGE: &str = "usage: gitty [--theme NAME] [PATH]\n       gitty untune [PATH]   undo the config gitty set on a large repo\n\nA fast terminal git client. Press ? inside for keys.";
const PROBE_TIMEOUT: Duration = Duration::from_millis(150);
const OUTPUT_BUFFER: usize = 256 * 1024;

struct Args {
    path: String,
    theme: Option<String>,
}

fn parse(args: Vec<String>) -> Result<Args, i32> {
    let mut a = Args { path: ".".into(), theme: None };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("gitty {}", env!("CARGO_PKG_VERSION"));
                return Err(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return Err(0);
            }
            "--theme" => match it.next() {
                Some(t) => a.theme = Some(t),
                None => {
                    eprintln!("gitty: --theme needs a name\n{USAGE}");
                    return Err(2);
                }
            },
            s if s.starts_with('-') => {
                eprintln!("gitty: unknown option {s}\n{USAGE}");
                return Err(2);
            }
            _ => a.path = arg,
        }
    }
    Ok(a)
}

fn epoch() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Runs gitty; returns the process exit code.
/// `gitty untune [PATH]`: unsets the config keys auto-tuning set, and nothing else.
fn untune(args: &[String]) -> anyhow::Result<i32> {
    let path = args.first().map_or(".", String::as_str);
    let repo = Repo::open(path)?;
    let keys = gitty_core::tune::untune(&gitty_core::git_cli::GitCli::new(&repo))?;
    if keys.is_empty() {
        println!("gitty untune: nothing to undo (gitty has not tuned this repository)");
    } else {
        println!("gitty untune: unset {}", keys.join(", "));
    }
    Ok(0)
}

pub fn run(args: Vec<String>) -> anyhow::Result<i32> {
    if args.first().map(String::as_str) == Some("untune") {
        return untune(&args[1..]);
    }
    let args = match parse(args) {
        Ok(a) => a,
        Err(code) => return Ok(code),
    };
    let repo = Repo::open(&args.path)?;
    let env = |k: &str| std::env::var(k).ok();
    let config_dir = paths::config_dir(env);
    let config_path = config_dir.join("config.toml");
    let (config, warnings) = Config::load(&config_path);
    let registry = Registry::load(Some(&config_dir.join("themes")));
    let depth = ColorDepth::detect(env);
    let root = repo.workdir().unwrap_or(repo.git_dir()).to_path_buf();
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let state_path = paths::repo_state_file(&paths::state_dir(env), &root);
    let ui_state = UiState::load(&state_path);
    let repo_name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());

    crossterm::terminal::enable_raw_mode().context("enabling raw mode (is stdin a terminal?)")?;
    let probe = term::probe(PROBE_TIMEOUT);
    let wanted = args.theme.clone().unwrap_or_else(|| config.theme.clone());
    let wanted = if wanted == "auto" { Registry::pick_auto(probe.light.unwrap_or(false)).to_string() } else { wanted };
    let mut theme_error = None;
    let theme = match registry.resolve(&wanted, depth, config.emph_alpha) {
        Ok(t) => t,
        Err(e) => {
            theme_error = Some(format!("{e:#}"));
            registry.resolve("github-dark", depth, config.emph_alpha)?
        }
    };

    let guard = term::Guard::enter(probe.kitty)?;
    let (w, h) = crossterm::terminal::size().unwrap_or((80, 24));
    let backend = CrosstermBackend::new(BufWriter::with_capacity(OUTPUT_BUFFER, std::io::stdout()));
    let mut terminal = Terminal::with_options(backend, TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, w, h)) })?;
    // resize (not clear) resets the back buffer without a cursor-position query
    terminal.resize(Rect::new(0, 0, w, h))?;

    let gens = Arc::new(Gens::default());
    let (msg_tx, msg_rx) = unbounded();
    let (in_tx, in_rx) = unbounded();
    let (sig_tx, sig_rx) = unbounded();
    let mut signals = signal_hook::iterator::Signals::new([libc::SIGTERM, libc::SIGHUP])?;
    std::thread::Builder::new().name("gitty-signals".into()).spawn(move || {
        for s in signals.forever() {
            if sig_tx.send(s).is_err() {
                return;
            }
        }
    })?;
    let watch_tx = msg_tx.clone();
    let watcher = gitty_core::watch::Watcher::spawn(&repo, move |c| {
        let _ = watch_tx.send(crate::msg::Msg::Changed(c));
    });
    let ask_tx = msg_tx.clone();
    let asker = crate::askpass::AskServer::start(move |a| {
        let _ = ask_tx.send(crate::msg::Msg::Ask(a));
    });
    let workers = Workers::spawn(repo.clone(), gens.clone(), msg_tx);
    let gate = input::Gate::default();
    input::spawn(in_tx, gate.clone());

    let mut app = App::new(AppInit {
        repo_name,
        config,
        registry,
        theme,
        depth,
        ui_state,
        config_path: Some(config_path),
        state_path: Some(state_path),
        gens,
        now: epoch(),
        clock: Instant::now(),
        size: (w, h),
    });
    // focus reports arrive only on change, so assume the terminal is focused at launch
    app.focused = true;
    app.workdir = repo.workdir().map(std::path::Path::to_path_buf);
    let mut problems: Vec<String> = warnings;
    match (&asker, std::env::current_exe()) {
        (Ok(s), Ok(exe)) => {
            app.set_askpass(exe, s.socket().to_path_buf());
            app.ask_handle = Some(s.handle());
        }
        (Err(e), _) => problems.push(format!("password prompts are unavailable; network jobs that need one will fail: {e:#}")),
        (_, Err(e)) => problems.push(format!("password prompts are unavailable (gitty cannot find its own executable): {e}")),
    }
    match &watcher {
        Ok(w) => app.set_index_mark(w.index_mark()),
        Err(e) => problems.push(format!("watching the repository for changes failed; refresh on focus only: {e:#}")),
    }
    problems.extend(app.registry.errors().iter().cloned());
    problems.extend(app.key_warnings.iter().cloned());
    problems.extend(theme_error);
    if !problems.is_empty() {
        app.toast = Some(Toast { what: problems[0].lines().next().unwrap_or("").to_string(), detail: problems.join("\n"), error: true });
    }

    let mut trace = std::env::var_os("GITTY_TRACE").and_then(|p| std::fs::File::create(p).ok()).map(BufWriter::new);
    let started = Instant::now();
    macro_rules! trace {
        ($($a:tt)*) => {
            if let Some(t) = trace.as_mut() {
                let _ = writeln!(t, "{:>9.3} {}", started.elapsed().as_secs_f64() * 1e3, format_args!($($a)*));
            }
        };
    }
    let mut exit = 0;
    loop {
        for r in app.take_requests() {
            workers.submit(r);
        }
        if app.quit {
            break;
        }
        if let Some(ask) = app.external.take() {
            let editor = ["VISUAL", "EDITOR"].iter().find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()));
            let result = match crate::external::prepare(&ask, editor.as_deref(), app.config.difftool.as_deref()) {
                Err(e) => Err(e),
                Ok(prepared) => {
                    gate.pause();
                    let ran = guard.hand_over(|| crate::external::run_foreground(&prepared.argv, &root));
                    gate.resume();
                    let ran = ran?;
                    ran.map(|s| s.code()).map_err(|e| format!("{}: {e}", prepared.argv[0]))
                }
            };
            // the alternate screen is new: resizing resets ratatui's buffers so the next frame
            // draws everything (Terminal::clear would query the cursor, and the input thread
            // would eat the reply)
            let (w, h) = crossterm::terminal::size().unwrap_or(app.size);
            terminal_resize(&mut terminal, w, h)?;
            app.handle_resize(w, h);
            app.external_done(result);
            continue;
        }
        if app.suspend {
            app.suspend = false;
            guard.suspend()?;
            let (w, h) = crossterm::terminal::size().unwrap_or(app.size);
            terminal_resize(&mut terminal, w, h)?;
            app.handle_resize(w, h);
            app.dirty = true;
        }
        if app.dirty {
            app.dirty = false;
            app.now = epoch();
            let out = terminal.backend_mut();
            out.write_all(b"\x1b[?2026h")?;
            let t = Instant::now();
            terminal.draw(|f| ui::draw(&mut app, f))?;
            trace!("draw {:.3} ms", t.elapsed().as_secs_f64() * 1e3);
            let out = terminal.backend_mut();
            for s in app.osc_out.drain(..) {
                out.write_all(s.as_bytes())?;
            }
            out.write_all(b"\x1b[?2026l")?;
            out.flush()?;
            // drawing records hit regions only; requests it caused (none today) go out next turn
            continue;
        }
        let now = Instant::now();
        let date_at = app.date_refresh_at().map(|t| now + Duration::from_secs(t.saturating_sub(epoch()).max(1) as u64));
        let deadline = [app.next_deadline(), date_at].into_iter().flatten().min();
        let timeout = deadline.map(|d| d.saturating_duration_since(now));
        let on_input = |app: &mut App, batch: Vec<input::InputEvent>, terminal: &mut Terminal<_>| -> anyhow::Result<()> {
            for ie in batch {
                match ie.ev {
                    Event::Key(k) => app.handle_key(k),
                    Event::Mouse(m) => {
                        for _ in 0..ie.repeat {
                            app.handle_mouse(m);
                        }
                    }
                    Event::Resize(w, h) => {
                        terminal_resize(terminal, w, h)?;
                        app.handle_resize(w, h);
                    }
                    Event::FocusGained => app.handle_focus(true),
                    Event::FocusLost => app.handle_focus(false),
                    Event::Paste(mut s) => {
                        app.handle_paste(&s);
                        // it may have been a password
                        crate::editor::wipe(&mut s);
                    }
                }
            }
            Ok(())
        };
        app.clock = Instant::now();
        let mut fired = false;
        match timeout {
            Some(t) => select! {
                recv(in_rx) -> b => match b {
                    Ok(b) => on_input(&mut app, b, &mut terminal)?,
                    // the terminal is gone (input thread ended): quit instead of spinning
                    Err(_) => {
                        app.quit_now();
                        break;
                    }
                },
                recv(msg_rx) -> m => if let Ok(m) = m { trace!("msg {m:?}"); app.handle_msg(m) },
                recv(sig_rx) -> s => if let Ok(s) = s { exit = 128 + s; app.quit_now(); break },
                default(t) => fired = true,
            },
            None => select! {
                recv(in_rx) -> b => match b {
                    Ok(b) => on_input(&mut app, b, &mut terminal)?,
                    // the terminal is gone (input thread ended): quit instead of spinning
                    Err(_) => break,
                },
                recv(msg_rx) -> m => if let Ok(m) = m { trace!("msg {m:?}"); app.handle_msg(m) },
                recv(sig_rx) -> s => if let Ok(s) = s { exit = 128 + s; app.quit_now(); break },
            },
        }
        while let Ok(b) = in_rx.try_recv() {
            on_input(&mut app, b, &mut terminal)?;
        }
        while let Ok(m) = msg_rx.try_recv() {
            trace!("msg {m:?}");
            app.handle_msg(m);
        }
        if let Ok(s) = sig_rx.try_recv() {
            exit = 128 + s;
            break;
        }
        app.tick(Instant::now());
        if fired {
            app.dirty = true;
        }
    }
    drop(watcher);
    drop(terminal);
    drop(guard);
    Ok(exit)
}

fn terminal_resize<B: ratatui::backend::Backend>(t: &mut Terminal<B>, w: u16, h: u16) -> anyhow::Result<()> {
    t.resize(Rect::new(0, 0, w, h)).map_err(|e| anyhow::anyhow!("resizing: {e:?}"))?;
    Ok(())
}
