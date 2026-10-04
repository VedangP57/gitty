//! Top bar (repo, branch, ahead/behind, fetch age, tabs) and bottom bar (keys, toast).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{fill, spans, text, width};
use crate::app::{App, Focus, Tab};
use crate::dates::{DateMode, format_date};
use gitty_core::refs::Head;

pub fn top(app: &mut App, buf: &mut Buffer, r: Rect) {
    if r.height == 0 {
        return;
    }
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.status_bg).fg(ui.fg);
    fill(buf, r, base);
    let muted = base.fg(ui.muted);
    let y = r.y;
    let tabs = [(Tab::Changes, "[1] Changes"), (Tab::History, "[2] History")];
    let tabs_w: u16 = tabs.iter().map(|t| width(t.1) + 2).sum();
    let right = r.right();
    let mut tx = right.saturating_sub(tabs_w);
    let mut tab_hits = Vec::new();
    if r.width >= tabs_w + 20 {
        for (tab, label) in tabs {
            let st = if app.tab == tab { base.fg(ui.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { muted };
            let w = width(label);
            text(buf, tx + 1, y, right, label, st);
            tab_hits.push((Rect::new(tx, y, w + 2, 1), tab));
            tx += w + 2;
        }
    }
    let max_x = right.saturating_sub(tabs_w + 1);
    let mut x = spans(buf, r.x, y, max_x, &[(" gitty ", base.fg(ui.accent).add_modifier(Modifier::BOLD)), (&app.repo_name, base.add_modifier(Modifier::BOLD))]);
    if let Some(refs) = &app.refs {
        let branch = match &refs.head {
            Head::Branch { name, .. } => format!("  ⎇ {name}"),
            Head::Detached { id } => format!("  detached {}", id.short(7)),
        };
        x = text(buf, x, y, max_x, &branch, base);
        if refs.upstream.is_some() {
            let ahead = format!("  ↑{}", app.ahead.len());
            let behind = format!(" ↓{}", app.behind.len());
            x = spans(buf, x, y, max_x, &[(&ahead, base.fg(ui.ahead)), (&behind, base.fg(ui.behind))]);
        }
    }
    if let Some(t) = app.fetched_at {
        let ago = format_date(t, 0, app.now, DateMode::Relative);
        let s = if ago == "now" { "  fetched just now".to_string() } else { format!("  fetched {ago} ago") };
        text(buf, x, y, max_x, &s, muted);
    }
    app.hits.tabs = tab_hits;
}

fn hints(app: &App) -> &'static [(&'static str, &'static str)] {
    if app.tab == Tab::Changes {
        return &[("2", "history"), ("T", "theme"), ("?", "help"), ("q", "quit")];
    }
    match app.focus {
        Focus::History => &[("j/k", "move"), ("enter", "files"), ("tab", "pane"), ("r", "scope"), ("D", "dates"), ("z", "density"), ("T", "theme"), ("?", "help"), ("q", "quit")],
        Focus::Files => &[("j/k", "file"), ("enter", "diff"), ("esc", "back"), ("{ }", "file"), ("[ ]", "hunk"), ("?", "help"), ("q", "quit")],
        Focus::Diff => &[("j/k", "line"), ("[ ]", "hunk"), ("e/E", "expand"), ("s", "split"), ("w", "whitespace"), ("h/l", "scroll"), ("F", "full"), ("esc", "back")],
    }
}

pub fn bottom(app: &App, buf: &mut Buffer, r: Rect) {
    if r.height == 0 {
        return;
    }
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.status_bg).fg(ui.status_fg);
    fill(buf, r, base);
    let mut max_x = r.right();
    if let Some(t) = &app.toast {
        let (s, st) = if t.error {
            (format!(" ✗ {}  ! details ", t.what), base.fg(ui.error).add_modifier(Modifier::BOLD))
        } else {
            (format!(" {} ", t.what), base.fg(ui.accent))
        };
        let w = width(&s).min(r.width * 2 / 3);
        let x = r.right().saturating_sub(w);
        text(buf, x, r.y, r.right(), &s, st);
        max_x = x;
    }
    let mut x = r.x + 1;
    for (k, d) in hints(app) {
        if x + width(k) + width(d) + 3 > max_x {
            break;
        }
        x = spans(buf, x, r.y, max_x, &[(k, base.fg(ui.accent)), (" ", base), (d, base), ("  ", base)]);
    }
}
