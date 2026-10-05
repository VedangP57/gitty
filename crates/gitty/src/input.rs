//! The input thread: blocks on terminal events, then drains whatever else is already queued so
//! a burst (wheel scrolling, resizes) becomes one batch and one frame.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use crossbeam_channel::Sender;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};

/// Drops a terminal OSC reply that arrived after the startup probe stopped listening (a slow
/// link). crossterm reads `ESC ] … ST` as Alt+`]`, the body as plain keys, then Alt+`\` (or
/// Ctrl-G for a BEL terminator). gitty binds no Alt keys, so Alt+`]` safely starts a reply.
#[derive(Debug, Default)]
pub struct OscFilter {
    active: bool,
    dropped: u16,
}

/// Longest reply body we expect (`11;rgb:rrrr/gggg/bbbb` is 21 chars).
const MAX_REPLY: u16 = 64;

impl OscFilter {
    pub fn keep(&mut self, e: &Event) -> bool {
        let Event::Key(k) = e else {
            self.active = false;
            return true;
        };
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        if !self.active {
            if alt && k.code == KeyCode::Char(']') {
                self.active = true;
                self.dropped = 0;
                return false;
            }
            return true;
        }
        self.dropped += 1;
        let end = (alt && k.code == KeyCode::Char('\\')) || (k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('g'));
        if end || self.dropped > MAX_REPLY {
            self.active = false;
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct InputEvent {
    pub ev: Event,
    /// How many identical wheel events this stands for.
    pub repeat: u16,
}

fn is_wheel(e: &Event) -> bool {
    matches!(e, Event::Mouse(m) if matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown | MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight))
}

pub fn coalesce(evs: Vec<Event>) -> Vec<InputEvent> {
    let last_resize = evs.iter().rposition(|e| matches!(e, Event::Resize(..)));
    let mut out: Vec<InputEvent> = Vec::with_capacity(evs.len());
    for (i, ev) in evs.into_iter().enumerate() {
        if matches!(ev, Event::Resize(..)) && Some(i) != last_resize {
            continue;
        }
        if let (Some(prev), true) = (out.last_mut(), is_wheel(&ev))
            && let (Event::Mouse(a), Event::Mouse(b)) = (&prev.ev, &ev)
                && a.kind == b.kind && a.column == b.column && a.row == b.row {
                    prev.repeat = prev.repeat.saturating_add(1);
                    continue;
                }
        out.push(InputEvent { ev, repeat: 1 });
    }
    out
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum GateState {
    #[default]
    Running,
    PauseAsked,
    Paused,
}

/// Lets the main loop stop the input thread from reading the terminal while an external tool
/// owns it (otherwise the two would split the keystrokes).
#[derive(Debug, Default, Clone)]
pub struct Gate(Arc<(Mutex<GateState>, Condvar)>);

/// How long a read waits before checking the gate.
const POLL: Duration = Duration::from_millis(100);

impl Gate {
    /// Returns once the input thread is idle (at most one poll interval, plus a margin).
    pub fn pause(&self) {
        let (m, cv) = &*self.0;
        let mut st = m.lock().unwrap_or_else(PoisonError::into_inner);
        *st = GateState::PauseAsked;
        let deadline = std::time::Instant::now() + 10 * POLL;
        while *st == GateState::PauseAsked {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            st = cv.wait_timeout(st, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
    pub fn resume(&self) {
        let (m, cv) = &*self.0;
        *m.lock().unwrap_or_else(PoisonError::into_inner) = GateState::Running;
        cv.notify_all();
    }
    /// The input thread's side: parks while paused.
    fn checkpoint(&self) {
        let (m, cv) = &*self.0;
        let mut st = m.lock().unwrap_or_else(PoisonError::into_inner);
        if *st == GateState::PauseAsked {
            *st = GateState::Paused;
            cv.notify_all();
        }
        while *st == GateState::Paused {
            st = cv.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

pub fn spawn(tx: Sender<Vec<InputEvent>>, gate: Gate) {
    let _ = std::thread::Builder::new().name("gitty-input".into()).spawn(move || {
        let mut osc = OscFilter::default();
        loop {
            gate.checkpoint();
            match event::poll(POLL) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(_) => break,
            }
            let Ok(first) = event::read() else { break };
            let mut batch: Vec<Event> = std::iter::once(first).filter(|e| osc.keep(e)).collect();
            while event::poll(Duration::ZERO).unwrap_or(false) {
                match event::read() {
                    Ok(e) => {
                        if osc.keep(&e) {
                            batch.push(e);
                        }
                    }
                    Err(_) => break,
                }
            }
            if batch.is_empty() {
                continue;
            }
            if tx.send(coalesce(batch)).is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    fn wheel(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE })
    }
    fn key(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    #[test]
    fn coalesce_wheel_bursts() {
        let v = coalesce(vec![
            wheel(MouseEventKind::ScrollDown, 5, 5),
            wheel(MouseEventKind::ScrollDown, 5, 5),
            wheel(MouseEventKind::ScrollDown, 5, 5),
            wheel(MouseEventKind::ScrollUp, 5, 5),
            wheel(MouseEventKind::ScrollDown, 9, 9),
        ]);
        assert_eq!(v.iter().map(|e| e.repeat).collect::<Vec<_>>(), [3, 1, 1]);
    }

    #[test]
    fn coalesce_keeps_last_resize() {
        let v = coalesce(vec![Event::Resize(10, 10), key('a'), Event::Resize(20, 20), Event::Resize(30, 30)]);
        assert_eq!(v.len(), 2);
        assert!(matches!(v[0].ev, Event::Key(_)));
        assert!(matches!(v[1].ev, Event::Resize(30, 30)));
    }

    #[test]
    fn keys_never_coalesced() {
        let v = coalesce(vec![key('j'), key('j'), key('j')]);
        assert_eq!(v.len(), 3);
        assert!(v.iter().all(|e| e.repeat == 1));
    }
}

#[cfg(test)]
mod osc_tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn k(c: char, m: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), m))
    }

    /// A late OSC 11 reply as crossterm parses it: ESC ] → Alt+], body chars, ESC \ → Alt+\.
    fn late_reply(st_bel: bool) -> Vec<Event> {
        let mut v = vec![k(']', KeyModifiers::ALT)];
        v.extend("11;rgb:0d0d/1111/1717".chars().map(|c| k(c, KeyModifiers::NONE)));
        v.push(if st_bel { k('g', KeyModifiers::CONTROL) } else { k('\\', KeyModifiers::ALT) });
        v
    }

    #[test]
    fn late_osc_reply_is_dropped_keys_survive() {
        for bel in [false, true] {
            let mut f = OscFilter::default();
            let mut evs = vec![k('j', KeyModifiers::NONE)];
            evs.extend(late_reply(bel));
            evs.push(k('q', KeyModifiers::NONE));
            let kept: Vec<Event> = evs.into_iter().filter(|e| f.keep(e)).collect();
            assert_eq!(kept, vec![k('j', KeyModifiers::NONE), k('q', KeyModifiers::NONE)]);
        }
    }

    #[test]
    fn reply_split_across_batches_and_runaway_capped() {
        let mut f = OscFilter::default();
        let r = late_reply(false);
        let (a, b) = r.split_at(5);
        assert!(a.iter().all(|e| !f.keep(e)));
        assert!(b.iter().all(|e| !f.keep(e)));
        assert!(f.keep(&k('j', KeyModifiers::NONE)));
        let mut f = OscFilter::default();
        assert!(!f.keep(&k(']', KeyModifiers::ALT)));
        let kept = (0..200).filter(|_| f.keep(&k('x', KeyModifiers::NONE))).count();
        assert!(kept > 100, "an unterminated sequence stops being filtered");
    }
}
