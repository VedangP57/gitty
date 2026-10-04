//! The input thread: blocks on terminal events, then drains whatever else is already queued so
//! a burst (wheel scrolling, resizes) becomes one batch and one frame.

use std::time::Duration;

use crossbeam_channel::Sender;
use crossterm::event::{self, Event, MouseEventKind};

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
        if let (Some(prev), true) = (out.last_mut(), is_wheel(&ev)) {
            if let (Event::Mouse(a), Event::Mouse(b)) = (&prev.ev, &ev) {
                if a.kind == b.kind && a.column == b.column && a.row == b.row {
                    prev.repeat = prev.repeat.saturating_add(1);
                    continue;
                }
            }
        }
        out.push(InputEvent { ev, repeat: 1 });
    }
    out
}

pub fn spawn(tx: Sender<Vec<InputEvent>>) {
    let _ = std::thread::Builder::new().name("gitty-input".into()).spawn(move || {
        while let Ok(first) = event::read() {
            let mut batch = vec![first];
            while event::poll(Duration::ZERO).unwrap_or(false) {
                match event::read() {
                    Ok(e) => batch.push(e),
                    Err(_) => break,
                }
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
