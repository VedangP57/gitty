//! Changes tab file list: checkbox (real index state), status letter, dim directory + name.

use gitty_core::status::Check;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::commit_list::title;
use super::paint::{centered, fill, spans, text, text_right};
use crate::app::changes::Filter;
use crate::app::commit::Field;
use crate::app::{App, Focus};
use crate::editor::Editor;
use crate::text::truncate_middle;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

/// `s` without its first `cols` display columns.
fn skip_cols(s: &str, cols: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true) {
        if w >= cols {
            return &s[i..];
        }
        w += g.width();
    }
    ""
}

/// One editor in `r`: scrolled so the cursor shows, placeholder when empty, cursor cell reversed.
fn field(buf: &mut Buffer, r: Rect, ed: &Editor, placeholder: &str, active: bool, st: Style, muted: Style) {
    fill(buf, r, st);
    if r.width == 0 || r.height == 0 {
        return;
    }
    let (line, col) = ed.position();
    let w = r.width as usize - 1;
    let top = if active { line.saturating_sub(r.height as usize - 1) } else { 0 };
    let left = if active { col.saturating_sub(w) } else { 0 };
    if ed.is_empty() {
        text(buf, r.x, r.y, r.right(), placeholder, muted);
    }
    for (k, l) in ed.lines().skip(top).take(r.height as usize).enumerate() {
        let shown = if k + top == line { skip_cols(l, left) } else { skip_cols(l, 0) };
        text(buf, r.x, r.y + k as u16, r.right(), shown, st);
    }
    if active {
        let (cx, cy) = (r.x + (col - left) as u16, r.y + (line - top) as u16);
        if cx < r.right() && cy < r.bottom() {
            let cell = &mut buf[(cx, cy)];
            if cell.symbol().is_empty() {
                cell.set_symbol(" ");
            }
            cell.set_style(Style::new().add_modifier(Modifier::REVERSED));
        }
    }
}

pub fn draw_commit(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width < 8 {
        return;
    }
    let focused = app.focus == Focus::Commit;
    let c = &app.changes.commit;
    if c.amend {
        let st = base.fg(ui.warning).add_modifier(Modifier::BOLD);
        text(buf, r.x + 1, r.y, r.right(), "⚠ Amending the last commit · A cancels", st);
    } else {
        for x in r.left()..r.right() {
            buf[(x, r.y)].set_symbol("─").set_style(base.fg(ui.border));
        }
    }
    let (x, right) = (r.x + 1, r.right().saturating_sub(1));
    let w = right.saturating_sub(x);
    let rows = r.height - 1;
    // summary, description (up to 3 lines), co-authors, button, bar
    let body_h = rows.saturating_sub(4).min(3);
    let fst = Style::new().bg(ui.panel).fg(ui.fg);
    let muted = fst.fg(ui.muted);
    let active = |f: Field| focused && c.field == f;
    let mut y = r.y + 1;
    let mut fields = Vec::new();
    if y < r.bottom() {
        let n = c.summary.text().chars().count();
        let counter = if n == 0 { String::new() } else { n.to_string() };
        let cst = match n {
            0..=50 => base.fg(ui.muted),
            51..=72 => base.fg(ui.warning),
            _ => base.fg(ui.error).add_modifier(Modifier::BOLD),
        };
        let cw = counter.len() as u16;
        let fr = Rect::new(x, y, w.saturating_sub(if cw > 0 { cw + 1 } else { 0 }), 1);
        field(buf, fr, &c.summary, &app.commit_placeholder(), active(Field::Summary), fst.add_modifier(Modifier::BOLD), muted);
        text_right(buf, x, right, y, &counter, cst);
        fields.push((Rect::new(x, y, w, 1), Field::Summary));
        y += 1;
    }
    if body_h > 0 {
        let fr = Rect::new(x, y, w, body_h);
        field(buf, fr, &c.body, "Description", active(Field::Body), fst, muted);
        fields.push((fr, Field::Body));
        y += body_h;
    }
    if y < r.bottom() {
        let lx = text(buf, x, y, right, "Co-authors ", base.fg(ui.muted));
        let fr = Rect::new(lx, y, right.saturating_sub(lx), 1);
        field(buf, fr, &c.coauthors, "Name <email>, …", active(Field::CoAuthors), fst, muted);
        fields.push((Rect::new(x, y, w, 1), Field::CoAuthors));
        y += 1;
    }
    let mut button = None;
    if y < r.bottom() {
        let label = format!(" {} ", app.commit_button());
        let ready = c.amend || app.changes.entries().iter().any(|e| e.check() != Check::Unstaged);
        let st = if ready && !c.committing {
            Style::new().bg(ui.accent).fg(ui.bg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(ui.panel).fg(ui.muted)
        };
        let end = text(buf, x, y, right, &label, st);
        button = Some(Rect::new(x, y, end.saturating_sub(x), 1));
        y += 1;
    }
    if let (Some(bar), true) = (app.commit_bar(), y < r.bottom()) {
        let st = if c.committing { base.fg(ui.warning) } else { base.fg(ui.muted) };
        text(buf, x, y, right, &bar, st);
    }
    app.hits.commit_fields = fields;
    app.hits.commit_button = button;
}
