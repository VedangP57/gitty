//! Changes tab file list: checkbox (real index state), status letter, dim directory + name.

use gitty_core::status::Check;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::commit_list::title;
use super::paint::{centered, fill, spans, text};
use crate::app::changes::Filter;
use crate::app::{App, Focus};
use crate::text::truncate_middle;

fn checkbox(c: Check) -> &'static str {
    match c {
        Check::Staged => "[x]",
        Check::Partial => "[~]",
        Check::Unstaged => "[ ]",
    }
}

pub fn draw_files(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    let focused = app.focus == Focus::Files;
    let rows = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    app.hits.files_rows = Some(rows);
    app.hits.files_first = app.changes.scroll;
    let mut extra = String::new();
    if app.changes.filter != Filter::All {
        extra.push_str(&format!(" · {}", app.changes.filter.label()));
    }
    if app.changes.busy > 0 {
        extra.push_str(" · working…");
    }
    let Some(_) = &app.changes.status else {
        let label = match &app.changes.status_error {
            Some(_) => "Changes",
            None => "Reading changes…",
        };
        title(app, buf, r, label, focused, "");
        if let (Some(err), true) = (app.changes.status_error.clone(), rows.height > 1) {
            text(buf, rows.x + 1, rows.y, rows.right(), "Could not read the working tree status", base.fg(ui.error).add_modifier(Modifier::BOLD));
            text(buf, rows.x + 1, rows.y + 1, rows.right(), err.lines().next().unwrap_or(""), base.fg(ui.muted));
        }
        return;
    };
    let entries = app.changes.entries().to_vec();
    let visible = app.changes.visible();
    let all = if entries.is_empty() {
        Check::Unstaged
    } else if entries.iter().all(|e| e.check() == Check::Staged) {
        Check::Staged
    } else if entries.iter().all(|e| e.check() == Check::Unstaged) {
        Check::Unstaged
    } else {
        Check::Partial
    };
    let n = entries.len();
    let label = format!("{} {n} changed file{}", checkbox(all), if n == 1 { "" } else { "s" });
    title(app, buf, r, &label, focused, &extra);
    if visible.is_empty() {
        if rows.height > 0 {
            let msg = if n == 0 { "No local changes" } else { "No files match the filter (F)" };
            centered(buf, rows, rows.y + rows.height.min(2) / 2, msg, base.fg(ui.muted));
        }
        return;
    }
    for (k, pos) in (app.changes.scroll..visible.len()).take(rows.height as usize).enumerate() {
        let e = &entries[visible[pos]];
        let y = rows.y + k as u16;
        let bg = if pos == app.changes.sel { if focused { ui.selection } else { ui.selection_inactive } } else { ui.bg };
        let row = Style::new().bg(bg).fg(ui.fg);
        fill(buf, Rect::new(rows.x, y, rows.width, 1), row);
        let check = e.check();
        let cst = match check {
            Check::Staged => row.fg(ui.accent).add_modifier(Modifier::BOLD),
            Check::Partial => row.fg(ui.warning).add_modifier(Modifier::BOLD),
            Check::Unstaged => row.fg(ui.muted),
        };
        let letter = e.letter();
        let color = match letter {
            'A' => ui.status_added,
            'D' => ui.status_deleted,
            'R' | 'C' => ui.status_renamed,
            'U' => ui.error,
            _ => ui.status_modified,
        };
        let right = rows.right().saturating_sub(1);
        let x = text(buf, rows.x + 1, y, right, checkbox(check), cst) + 1;
        let x = text(buf, x, y, right, &letter.to_string(), row.fg(color).add_modifier(Modifier::BOLD)) + 1;
        let shown = truncate_middle(&e.path, right.saturating_sub(x) as usize);
        let (dir, name) = match shown.rfind('/') {
            Some(p) => shown.split_at(p + 1),
            None => ("", shown.as_str()),
        };
        spans(buf, x, y, right, &[(dir, row.fg(ui.muted)), (name, row)]);
    }
}

pub fn draw_commit(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = &app.theme.ui;
    fill(buf, r, Style::new().bg(ui.bg).fg(ui.fg));
    if r.height == 0 {
        return;
    }
    for x in r.left()..r.right() {
        buf[(x, r.y)].set_symbol("─").set_style(Style::new().bg(ui.bg).fg(ui.border));
    }
}
