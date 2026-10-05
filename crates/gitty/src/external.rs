//! External tools ($EDITOR, the configured difftool). Commands are split with shell-like
//! quoting but never run by a shell, and paths are always their own arguments, so nothing in a
//! path or a config value can run as a command.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicU64, Ordering};

/// What the app asks the main loop to run with the terminal handed over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum External {
    /// Open `path` (in the working tree) at `line`.
    Edit { path: PathBuf, line: Option<u32> },
    /// Show the two sides of `path` in the difftool.
    Diff { path: String, old: Vec<u8>, new: Vec<u8> },
}

/// Splits a command line the way a POSIX shell splits words: whitespace separates, `'…'` is
/// literal, `"…"` honours `\"`, `\\`, `\$` and `` \` ``, and a backslash outside quotes escapes
/// the next character. Nothing is expanded: `$(…)`, `;` and `|` are ordinary characters.
pub fn split_command(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\\' => {
                in_word = true;
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err(format!("unterminated ' in {s:?}")),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n @ ('"' | '\\' | '$' | '`')) => word.push(n),
                            Some(n) => {
                                word.push('\\');
                                word.push(n);
                            }
                            None => return Err(format!("unterminated \" in {s:?}")),
                        },
                        Some(c) => word.push(c),
                        None => return Err(format!("unterminated \" in {s:?}")),
                    }
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        out.push(word);
    }
    if out.is_empty() {
        return Err("the command is empty".into());
    }
    Ok(out)
}

/// `editor` plus the file at `line`: `+N path` for vi-likes and most terminal editors,
/// `--goto path:N` for VS Code and its forks, `path:N` for editors that take that form.
pub fn editor_argv(editor: &str, path: &Path, line: Option<u32>) -> Result<Vec<String>, String> {
    let mut argv = split_command(editor)?;
    let prog = Path::new(&argv[0]).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let p = path.to_string_lossy().into_owned();
    match line {
        None => argv.push(p),
        Some(n) => match prog.as_str() {
            "code" | "code-insiders" | "codium" | "cursor" => argv.extend(["--goto".to_string(), format!("{p}:{n}")]),
            "subl" | "zed" | "hx" | "helix" => argv.push(format!("{p}:{n}")),
            _ => argv.extend([format!("+{n}"), p]),
        },
    }
    Ok(argv)
}

pub fn difftool_argv(tool: &str, old: &Path, new: &Path) -> Result<Vec<String>, String> {
    let mut argv = split_command(tool)?;
    argv.push(old.to_string_lossy().into_owned());
    argv.push(new.to_string_lossy().into_owned());
    Ok(argv)
}

/// The two sides of a file in a private temp directory, removed on drop.
#[derive(Debug)]
pub struct TempPair {
    dir: PathBuf,
    pub old: PathBuf,
    pub new: PathBuf,
}

impl Drop for TempPair {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Writes `old` and `new` as `…/old/<name>` and `…/new/<name>` (the file's own name, so the tool
/// picks the right syntax) under the system temp directory.
pub fn temp_pair(path: &str, old: &[u8], new: &[u8]) -> io::Result<TempPair> {
    use std::os::unix::fs::DirBuilderExt;
    static N: AtomicU64 = AtomicU64::new(0);
    let name = Path::new(path).file_name().map_or_else(|| "file".into(), |n| n.to_os_string());
    let dir = std::env::temp_dir().join(format!("gitty-diff-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let pair = TempPair { old: dir.join("old").join(&name), new: dir.join("new").join(&name), dir };
    for (p, bytes) in [(&pair.old, old), (&pair.new, new)] {
        std::fs::create_dir(p.parent().expect("has a parent"))?;
        std::fs::write(p, bytes)?;
    }
    Ok(pair)
}

/// A command ready to run, and the temp files that must outlive it.
#[derive(Debug)]
pub struct Prepared {
    pub argv: Vec<String>,
    pub temp: Option<TempPair>,
}

/// Builds the command for `ask`. `editor` is `$VISUAL` or `$EDITOR`; `difftool` comes from the
/// config.
pub fn prepare(ask: &External, editor: Option<&str>, difftool: Option<&str>) -> Result<Prepared, String> {
    match ask {
        External::Edit { path, line } => {
            let editor = editor.filter(|e| !e.trim().is_empty()).ok_or("Set $EDITOR (or $VISUAL) to open files from gitty")?;
            Ok(Prepared { argv: editor_argv(editor, path, *line)?, temp: None })
        }
        External::Diff { path, old, new } => {
            let tool = difftool.filter(|t| !t.trim().is_empty()).ok_or("Set difftool in gitty's config.toml, e.g. difftool = \"delta\"")?;
            let pair = temp_pair(path, old, new).map_err(|e| format!("writing temp files: {e}"))?;
            Ok(Prepared { argv: difftool_argv(tool, &pair.old, &pair.new)?, temp: Some(pair) })
        }
    }
}

/// Runs `argv` in the foreground with inherited stdio and waits. Meanwhile gitty ignores the
/// terminal's SIGINT and SIGQUIT (a Ctrl-C belongs to the tool); the child gets the defaults.
pub fn run_foreground(argv: &[String], cwd: &Path) -> io::Result<ExitStatus> {
    use std::os::unix::process::CommandExt;
    let (prog, args) = argv.split_first().ok_or_else(|| io::Error::other("empty command"))?;
    // SAFETY: signal() is async-signal-safe; the dispositions are restored right after.
    let old_int = unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
    let old_quit = unsafe { libc::signal(libc::SIGQUIT, libc::SIG_IGN) };
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd);
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::signal(libc::SIGINT, libc::SIG_DFL);
            libc::signal(libc::SIGQUIT, libc::SIG_DFL);
            Ok(())
        });
    }
    let status = cmd.status();
    unsafe {
        libc::signal(libc::SIGINT, old_int);
        libc::signal(libc::SIGQUIT, old_quit);
    }
    status
}
