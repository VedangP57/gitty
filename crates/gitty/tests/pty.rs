//! Runs the real binary in a pseudo-terminal.

#[path = "../../gitty-core/tests/common/mod.rs"]
mod common;

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::Fixture;

struct Pty {
    child: Child,
    master: std::fs::File,
    out: Arc<Mutex<Vec<u8>>>,
    _home: tempfile::TempDir,
}

fn spawn(dir: &std::path::Path, args: &[&str]) -> Pty {
    let (mut m, mut s) = (0, 0);
    let mut ws = libc::winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    assert_eq!(unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) }, 0);
    let slave = unsafe { OwnedFd::from_raw_fd(s) };
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_gitty"));
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave));
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().unwrap();
    let master = unsafe { std::fs::File::from_raw_fd(m) };
    let out = Arc::new(Mutex::new(Vec::new()));
    let (mut r, o) = (master.try_clone().unwrap(), out.clone());
    std::thread::spawn(move || {
        let mut buf = [0u8; 65536];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            o.lock().unwrap().extend_from_slice(&buf[..n]);
        }
    });
    Pty { child, master, out, _home: home }
}

impl Pty {
    fn output(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }
    fn wait_for(&self, needle: &str) {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(15) {
            if self.output().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("never saw {needle:?} in output:\n{}", self.output());
    }
    fn exit_code(&mut self) -> i32 {
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(10) {
            if let Some(s) = self.child.try_wait().unwrap() {
                use std::os::unix::process::ExitStatusExt;
                return s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("gitty did not exit; output:\n{}", self.output());
    }
}

fn repo() -> Fixture {
    let f = Fixture::new();
    f.write("a.txt", "one\n");
    f.commit("first commit", 1_700_000_000);
    f.write("a.txt", "one\ntwo\n");
    f.commit("second commit", 1_700_000_100);
    f
}

#[test]
fn launches_draws_and_quits() {
    let f = repo();
    let mut p = spawn(&f.path(), &[]);
    p.wait_for("second commit");
    p.wait_for("a.txt");
    p.master.write_all(b"j").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    p.master.write_all(b"q").unwrap();
    assert_eq!(p.exit_code(), 0);
    std::thread::sleep(Duration::from_millis(100));
    let out = p.output();
    assert!(out.contains("\x1b[?1049h") && out.contains("\x1b[?1049l"), "alt screen entered and left");
    assert!(out.contains("\x1b[?1000h") && out.contains("\x1b[?1000l"));
    assert!(out.contains("\x1b[?1006h") && out.contains("\x1b[?1006l"));
    assert!(out.contains("\x1b[?2026h") && out.contains("\x1b[?2026l"), "synchronized output");
    assert!(!out.contains("\x1b[?1003h"), "any-motion mouse must never be enabled");
    assert!(out.rfind("\x1b[?1049l") > out.rfind("\x1b[?2026h"), "restore happens after the last frame");
}

#[test]
fn not_a_repo_exits_1_without_alt_screen() {
    let d = tempfile::tempdir().unwrap();
    let mut p = spawn(d.path(), &[]);
    assert_eq!(p.exit_code(), 1);
    std::thread::sleep(Duration::from_millis(100));
    let out = p.output();
    assert!(out.contains("not a git repository"), "{out}");
    assert!(!out.contains("\x1b[?1049h"));
}

#[test]
fn version_flag() {
    let out = Command::new(env!("CARGO_BIN_EXE_gitty")).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("gitty "));
}

#[test]
fn sigterm_restores_terminal() {
    let f = repo();
    let mut p = spawn(&f.path(), &[]);
    p.wait_for("second commit");
    unsafe { libc::kill(p.child.id() as i32, libc::SIGTERM) };
    assert_eq!(p.exit_code(), 128 + libc::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    let out = p.output();
    assert!(out.rfind("\x1b[?1049l") > out.rfind("\x1b[?1049h"), "alt screen left after SIGTERM");
}
