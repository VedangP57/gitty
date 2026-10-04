//! Centered overlays: theme picker, help, error detail, confirmations.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{fill, spans, text};
use crate::app::{App, Overlay};

const HELP: &[(&str, &str)] = &[
    ("j/k ↓/↑", "move"),
    ("g/G", "top / bottom"),
    ("Ctrl-d/u PgDn/PgUp", "half page / page"),
    ("Tab S-Tab", "next / previous pane"),
    ("Enter Esc", "drill in / back"),
    ("h/l", "scroll the diff sideways"),
    ("[ ] { }", "previous/next hunk / file"),
    ("e E", "expand context near the cursor / whole file"),
    ("s w", "split view / whitespace mode"),
    ("F", "full-screen diff"),
    ("o", "expand the commit header"),
    ("y Y", "copy short / full SHA"),
    ("D z", "date mode / density"),
    ("r", "branch + upstream ↔ all refs"),
    ("< >", "resize the focused pane"),
    ("T", "theme picker"),
    ("1 2", "Changes / History"),
    ("Space a", "Changes: stage line or file / all"),
    ("v H", "Changes: line range / hunk"),
    ("d F", "Changes: discard (asks first) / filter files"),
    ("!", "error details"),
    ("q", "quit"),
];

fn boxed(app: &App, buf: &mut Buffer, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let ui = &app.theme.ui;
    let w = w.min(area.width);
    let h = h.min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let st = Style::new().bg(ui.panel).fg(ui.fg);
    fill(buf, r, st);
    let b = st.fg(ui.border_focus);
    if w >= 2 && h >= 2 {
        for x in r.left()..r.right() {
            buf[(x, r.top())].set_symbol("─").set_style(b);
            buf[(x, r.bottom() - 1)].set_symbol("─").set_style(b);
        }
        for y in r.top()..r.bottom() {
            buf[(r.left(), y)].set_symbol("│").set_style(b);
            buf[(r.right() - 1, y)].set_symbol("│").set_style(b);
        }
        buf[(r.left(), r.top())].set_symbol("╭");
        buf[(r.right() - 1, r.top())].set_symbol("╮");
        buf[(r.left(), r.bottom() - 1)].set_symbol("╰");
        buf[(r.right() - 1, r.bottom() - 1)].set_symbol("╯");
        text(buf, r.x + 2, r.y, r.right().saturating_sub(1), &format!(" {title} "), b.add_modifier(Modifier::BOLD));
    }
    Rect::new(r.x + 2, r.y + 1, w.saturating_sub(4), h.saturating_sub(2))
}

pub fn draw(app: &App, buf: &mut Buffer, area: Rect) {
    let Some(ov) = &app.overlay else { return };
    let ui = &app.theme.ui;
    let st = Style::new().bg(ui.panel).fg(ui.fg);
    match ov {
        Overlay::ThemePicker { sel, .. } => {
            let names = app.registry.names();
            let inner = boxed(app, buf, area, 40, names.len() as u16 + 4, "Theme");
            let rows = inner.height.saturating_sub(1) as usize;
            let first = sel.saturating_sub(rows.saturating_sub(1));
            for (k, (i, n)) in names.iter().enumerate().skip(first).take(rows).enumerate() {
                let y = inner.y + k as u16;
                let s = if i == *sel { st.bg(ui.selection).add_modifier(Modifier::BOLD) } else { st };
                fill(buf, Rect::new(inner.x, y, inner.width, 1), s);
                let mark = if *n == app.config.theme { "● " } else { "  " };
                spans(buf, inner.x, y, inner.right(), &[(mark, s.fg(ui.accent)), (n, s)]);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "Enter apply · Esc cancel", st.fg(ui.muted));
            }
        }
        Overlay::Help => {
            let inner = boxed(app, buf, area, 64, HELP.len() as u16 + 3, "Keys");
            for (k, (keys, what)) in HELP.iter().enumerate().take(inner.height as usize) {
                let y = inner.y + k as u16;
                text(buf, inner.x, y, inner.right(), keys, st.fg(ui.accent));
                text(buf, inner.x + 22, y, inner.right(), what, st);
            }
        }
        Overlay::Confirm { title, body, .. } => {
            let w = (crate::text::display_width(title).max(crate::text::display_width(body)) as u16 + 6).clamp(40, area.width.saturating_sub(4).max(40));
            let inner = boxed(app, buf, area, w, 6, "Confirm");
            text(buf, inner.x, inner.y, inner.right(), title, st.fg(ui.warning).add_modifier(Modifier::BOLD));
            text(buf, inner.x, inner.y + 1, inner.right(), body, st.fg(ui.muted));
            if inner.height > 3 {
                spans(buf, inner.x, inner.y + 3, inner.right(), &[("Enter", st.fg(ui.accent)), (" discard · ", st), ("Esc", st.fg(ui.accent)), (" cancel", st)]);
            }
        }
        Overlay::ErrorDetail => {
            let Some(t) = &app.toast else { return };
            let lines: Vec<&str> = t.detail.lines().collect();
            let inner = boxed(app, buf, area, area.width.saturating_sub(8).min(100), lines.len() as u16 + 4, "Error");
            text(buf, inner.x, inner.y, inner.right(), &t.what, st.fg(ui.error).add_modifier(Modifier::BOLD));
            for (k, l) in lines.iter().enumerate().take(inner.height.saturating_sub(2) as usize) {
                text(buf, inner.x, inner.y + 2 + k as u16, inner.right(), l, st);
            }
        }
    }
}
