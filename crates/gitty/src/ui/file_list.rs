//! Changed files: status letter, dim directory + bright name, right-aligned +n −m.

use gitty_core::commit_files::FileStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::commit_list::title;
use super::paint::{centered, fill, spans, text, text_right};
use crate::app::{App, Focus};
use crate::text::truncate_middle;

pub fn draw(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    let focused = app.focus == Focus::Files;
    let rows = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    app.hits.files_rows = Some(rows);
    app.hits.files_first = app.file_scroll;
    let Some(files) = app.files.clone() else {
        let label = if app.selected_id().is_some() { "Loading files…" } else { "Files" };
        title(app, buf, r, label, focused, "");
        return;
    };
    let n = files.len();
    let label = format!("{n} changed file{}", if n == 1 { "" } else { "s" });
    title(app, buf, r, &label, focused, "");
    if n == 0 {
        if rows.height > 0 {
            centered(buf, rows, rows.y, "No files changed", base.fg(ui.muted));
        }
        return;
    }
    for (k, i) in (app.file_scroll..n).take(rows.height as usize).enumerate() {
        let f = &files[i];
        let y = rows.y + k as u16;
        let bg = if i == app.file_sel { if focused { ui.selection } else { ui.selection_inactive } } else { ui.bg };
        let row = Style::new().bg(bg).fg(ui.fg);
        fill(buf, Rect::new(rows.x, y, rows.width, 1), row);
        let (letter, color) = match f.status {
            FileStatus::Added => ("A", ui.status_added),
            FileStatus::Deleted => ("D", ui.status_deleted),
            FileStatus::Modified => ("M", ui.status_modified),
            FileStatus::Renamed { .. } => ("R", ui.status_renamed),
            FileStatus::Copied => ("C", ui.status_renamed),
            FileStatus::TypeChange => ("T", ui.status_modified),
        };
        let right = rows.right().saturating_sub(1);
        let x = text(buf, rows.x + 1, y, right, letter, row.fg(color).add_modifier(Modifier::BOLD)) + 1;
        let mut rx = right;
        if let Some(Some(s)) = app.stats.get(i) {
            let (a, d) = (format!("+{}", s.added), format!("−{}", s.removed));
            if s.binary {
                rx = text_right(buf, x, rx, y, "bin", row.fg(ui.muted));
            } else {
                rx = text_right(buf, x, rx, y, &d, row.fg(ui.status_deleted));
                rx = text_right(buf, x, rx.saturating_sub(1), y, &a, row.fg(ui.status_added));
            }
            rx = rx.saturating_sub(1);
        }
        let room = rx.saturating_sub(x) as usize;
        let shown = truncate_middle(&f.path, room);
        let (dir, name) = match shown.rfind('/') {
            Some(p) => shown.split_at(p + 1),
            None => ("", shown.as_str()),
        };
        spans(buf, x, y, rx, &[(dir, row.fg(ui.muted)), (name, row)]);
    }
}
