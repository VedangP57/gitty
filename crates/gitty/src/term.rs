//! Terminal setup and teardown. One query round-trip at startup learns the background colour
//! (OSC 11) and kitty keyboard support; the guard restores every mode on exit, panic or signal.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static KITTY: AtomicBool = AtomicBool::new(false);
static HOOKED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probe {
    /// Terminal background is light (None when the terminal did not answer OSC 11).
    pub light: Option<bool>,
    /// The terminal answered the kitty keyboard protocol query.
    pub kitty: bool,
    /// The primary device attributes reply (sent by every terminal) arrived.
    pub complete: bool,
}

fn parse_rgb(s: &str) -> Option<(f32, f32, f32)> {
    let rest = s.strip_prefix("rgb:")?;
    let mut it = rest.split('/');
    let mut ch = || -> Option<f32> {
        let h = it.next()?;
        if h.is_empty() || h.len() > 4 {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(v as f32 / ((1u32 << (4 * h.len())) - 1) as f32)
    };
    let (r, g, b) = (ch()?, ch()?, ch()?);
    Some((r, g, b))
}

pub fn parse_probe(bytes: &[u8]) -> Probe {
    let s = String::from_utf8_lossy(bytes);
    let mut p = Probe::default();
    if let Some(i) = s.find("\x1b]11;") {
        let body = &s[i + 5..];
        let end = body.find(['\x07', '\x1b']).unwrap_or(body.len());
        if let Some((r, g, b)) = parse_rgb(&body[..end]) {
            p.light = Some(0.2126 * r + 0.7152 * g + 0.0722 * b > 0.5);
        }
    }
    let mut rest = &*s;
    while let Some(i) = rest.find("\x1b[?") {
        let tail = &rest[i + 3..];
        let n = tail.find(|c: char| !(c.is_ascii_digit() || c == ';')).unwrap_or(tail.len());
        match tail[n..].chars().next() {
            Some('u') => p.kitty = true,
            Some('c') => p.complete = true,
            _ => {}
        }
        rest = tail;
    }
    p
}

/// Queries the terminal on stdin/stdout (raw mode must be on). Waits up to `timeout`.
pub fn probe(timeout: Duration) -> Probe {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b]11;?\x1b\\\x1b[?u\x1b[c").and_then(|_| out.flush()).is_err() {
        return Probe::default();
    }
    let start = Instant::now();
    let mut buf = Vec::new();
    loop {
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let r = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if r <= 0 {
            break;
        }
        let mut chunk = [0u8; 256];
        let n = unsafe { libc::read(0, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n <= 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
        if parse_probe(&buf).complete {
            break;
        }
    }
    parse_probe(&buf)
}

const ENTER: &str = "\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h";
const LEAVE: &str = "\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2026l\x1b[0m\x1b[?25h\x1b[?1049l";

/// Owns the terminal modes; restores them when dropped.
pub struct Guard;

impl Guard {
    /// Raw mode must already be enabled (it is needed for the probe).
    pub fn enter(kitty: bool) -> std::io::Result<Guard> {
        KITTY.store(kitty, Ordering::SeqCst);
        let mut out = std::io::stdout();
        out.write_all(ENTER.as_bytes())?;
        if kitty {
            out.write_all(b"\x1b[>1u")?;
        }
        out.flush()?;
        ACTIVE.store(true, Ordering::SeqCst);
        if !HOOKED.swap(true, Ordering::SeqCst) {
            let prev = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                prev(info);
            }));
        }
        Ok(Guard)
    }

    /// Hands the terminal back to the shell, stops the process (SIGTSTP), and re-enters on resume.
    pub fn suspend(&self) -> std::io::Result<()> {
        restore();
        let _ = signal_hook::low_level::raise(libc::SIGTSTP);
        crossterm::terminal::enable_raw_mode()?;
        Guard::enter(KITTY.load(Ordering::SeqCst)).map(std::mem::forget)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}

/// Idempotent: leaves the alternate screen and turns off every mode gitty enabled.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = crossterm::terminal::disable_raw_mode();
        return;
    }
    let mut out = std::io::stdout();
    if KITTY.load(Ordering::SeqCst) {
        let _ = out.write_all(b"\x1b[<u");
    }
    let _ = out.write_all(LEAVE.as_bytes());
    let _ = out.flush();
    let _ = crossterm::terminal::disable_raw_mode();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_probe_dark_and_light() {
        let p = parse_probe(b"\x1b]11;rgb:0d0d/1111/1717\x1b\\\x1b[?62;22c");
        assert_eq!(p.light, Some(false));
        assert!(p.complete);
        let p = parse_probe(b"\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?1;2c");
        assert_eq!(p.light, Some(true));
        let p = parse_probe(b"\x1b]11;rgb:fd/f6/e3\x1b\\\x1b[?6c");
        assert_eq!(p.light, Some(true));
    }

    #[test]
    fn parse_probe_kitty_flag() {
        let p = parse_probe(b"\x1b[?0u\x1b[?62c");
        assert!(p.kitty);
        assert_eq!(p.light, None);
        let p = parse_probe(b"\x1b[?62c");
        assert!(!p.kitty && p.complete);
    }

    #[test]
    fn parse_probe_garbage_is_none() {
        for g in [&b""[..], b"\x1b]11;rgb:zz/zz/zz\x07", b"hello", b"\x1b]11;rgb:1/2\x07", b"\x1b[?"] {
            let p = parse_probe(g);
            assert_eq!(p.light, None, "{g:?}");
            assert!(!p.kitty);
        }
        assert!(!parse_probe(b"\x1b]11;rgb:00/00/00\x07").complete);
    }
}
