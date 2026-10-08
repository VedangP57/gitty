//! Files tab: the working tree as a tree (only the visible slice of the flat row list is drawn)
//! and the read-only viewer of the selected file.

use gitty_highlight::{CAPTURES, Span};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::commit_list::title;
use super::paint::{centered, fill, glyphs, spans, text, width};
use crate::app::files::{RowKind, Viewing};
use crate::app::{App, Focus, digits};
use crate::msg::FileView;
use crate::text::{Glyph, layout_until};

/// Lines longer than this (bytes) get no syntax colour: they are clipped and cost nothing.
const NO_SYNTAX_OVER: usize = 1000;

pub fn draw_tree(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    let focused = app.focus == Focus::Files;
    let rows = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    app.files_tab.scroll = app.files_tab.scroll.min(app.files_tab.rows.len().saturating_sub(1));
    app.hits.files_rows = Some(rows);
    app.hits.files_first = app.files_tab.scroll;
    title(app, buf, r, "Files", focused, if app.files_tab.loading_root() { "  loading…" } else { "" });
    let f = &app.files_tab;
    let right = rows.right().saturating_sub(1);
    for (k, (i, row)) in f.rows.iter().enumerate().skip(f.scroll).take(rows.height as usize).enumerate() {
        let y = rows.y + k as u16;
        let bg = if i == f.sel { if focused { ui.selection } else { ui.selection_inactive } } else { ui.bg };
        let st = Style::new().bg(bg).fg(ui.fg);
        fill(buf, Rect::new(rows.x, y, rows.width, 1), st);
        let x = rows.x + 1 + 2 * row.depth;
        let muted = st.fg(ui.muted);
        // ignored entries are dimmed; directories take the accent colour
        let name_st = if row.ignored { muted } else { st };
        match &row.kind {
            RowKind::Dir { open, loading } => {
                let dir_st = if row.ignored { muted } else { st.fg(ui.accent).add_modifier(Modifier::BOLD) };
                let mark = if *open { "▾ " } else { "▸ " };
                let end = spans(buf, x, y, right, &[(mark, muted), (&row.name, dir_st), ("/", muted)]);
                if *loading {
                    text(buf, end + 1, y, right, "loading…", muted);
                }
            }
            RowKind::File => {
                // the marker column keeps names aligned with the directories above them
                let end = text(buf, x + 2, y, right, &row.name, name_st);
                if row.secret {
                    text(buf, end, y, right, " (secret)", st.fg(ui.warning));
                }
            }
            RowKind::Symlink { target } => {
                let end = spans(buf, x + 2, y, right, &[(&row.name, name_st), (" -> ", muted), (target, muted)]);
                if row.secret {
                    text(buf, end, y, right, " (secret)", st.fg(ui.warning));
                }
            }
            RowKind::Submodule => {
                spans(buf, x, y, right, &[("▸ ", muted), (&row.name, name_st), ("/", muted), (" (submodule)", muted)]);
            }
            RowKind::Note { error } => {
                text(buf, x + 2, y, right, &row.name, if *error { st.fg(ui.error) } else { muted });
            }
        }
    }
}

fn mib(n: u64) -> String {
    format!("{:.1} MiB", n as f64 / 1_048_576.0)
}

pub fn draw_viewer(app: &mut App, buf: &mut Buffer, r: Rect) {
    let theme = app.theme.clone();
    let ui = &theme.ui;
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    let focused = app.focus == Focus::Diff;
    let body = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    let muted = base.fg(ui.muted);
    let Some(path) = app.files_tab.shown.clone() else {
        title(app, buf, r, "File", focused, "");
        if body.height > 0 {
            centered(buf, body, body.y + body.height / 2, "Select a file to view it", muted);
        }
        return;
    };
    let shown = path.to_string_lossy().into_owned();
    // a masked secret says nothing about the file: no size, no line count
    if app.files_tab.masked() {
        title(app, buf, r, &shown, focused, "");
        if body.height > 0 {
            spans(buf, body.x + 2, body.y + body.height.min(2) / 2, body.right(), &[("Hidden: this looks like a secret file. Press v to reveal.", base.fg(ui.warning))]);
        }
        return;
    }
    let lines = app.view_lines();
    let extra = match &app.files_tab.viewing {
        Viewing::Loading => "  loading…".to_string(),
        Viewing::Ready(FileView::Text { .. }) => format!("  {lines} line{}", if lines == 1 { "" } else { "s" }),
        _ => String::new(),
    };
    title(app, buf, r, &shown, focused, &extra);
    if body.height == 0 {
        return;
    }
    let msg = |buf: &mut Buffer, line: &str, style: Style| {
        spans(buf, body.x + 2, body.y + body.height.min(2) / 2, body.right(), &[(line, style)]);
    };
    let text_lines = match &app.files_tab.viewing {
        Viewing::Nothing | Viewing::Loading => return,
        Viewing::Failed(e) => {
            msg(buf, "Could not read this file", base.fg(ui.error).add_modifier(Modifier::BOLD));
            if body.height > 3 {
                text(buf, body.x + 2, body.y + 3, body.right(), e.lines().next().unwrap_or(""), muted);
            }
            return;
        }
        Viewing::Ready(FileView::Binary { size }) => return msg(buf, &format!("binary file ({size} bytes)"), base),
        Viewing::Ready(FileView::TooLarge { size }) => return msg(buf, &format!("file too large ({}), press e to open in your editor", mib(*size)), base),
        Viewing::Ready(FileView::Lfs { size }) => return msg(buf, &format!("Git LFS pointer: the real file ({size} bytes) is not checked out"), base),
        Viewing::Ready(FileView::Symlink { target }) => return msg(buf, &format!("symlink -> {}", target.to_string_lossy()), base),
        Viewing::Ready(FileView::Special) => return msg(buf, "not a regular file (not opened)", base),
        Viewing::Ready(FileView::Masked) => return msg(buf, "Hidden: this looks like a secret file. Press v to reveal.", base.fg(ui.warning)),
        Viewing::Ready(FileView::Text { text, .. }) => text.clone(),
    };
    if text_lines.is_empty() {
        return msg(buf, "(empty file)", muted);
    }
    app.files_tab.vscroll = app.files_tab.vscroll.min(lines.saturating_sub(1));
    let hl = app.view_highlights();
    let syntax: Vec<Option<Style>> = CAPTURES.iter().map(|c| theme.syntax.get(*c).map(|s| Style { bg: None, ..*s })).collect();
    let digits = digits(lines as u32) as u16;
    let gutter = base.fg(theme.diff.lineno);
    let (hscroll, tab) = (u32::from(app.files_tab.hscroll), app.config.tab_size);
    let tx = body.x + digits + 2;
    let mut scratch: Vec<Glyph> = Vec::new();
    // clipped, not wrapped: `W` does not apply here, `h` and `l` scroll sideways
    for k in 0..usize::from(body.height) {
        let i = app.files_tab.vscroll + k;
        if i >= lines {
            break;
        }
        let y = body.y + k as u16;
        let n = (i + 1).to_string();
        text(buf, body.x + 1 + digits.saturating_sub(width(&n)), y, tx, &n, gutter);
        let line = text_lines.line(i as u32);
        layout_until(line, tab, hscroll + u32::from(body.right().saturating_sub(tx)) + 1, &mut scratch);
        let syn: &[Span] = match &hl {
            Some(h) if line.len() <= NO_SYNTAX_OVER => h.line(i as u32),
            _ => &[],
        };
        glyphs(buf, tx, y, body.right(), hscroll, &scratch, |g| {
            if g.ctrl {
                return base.fg(ui.muted);
            }
            let j = syn.partition_point(|s| s.end <= g.byte);
            match syn.get(j).filter(|s| s.start <= g.byte).and_then(|s| syntax.get(s.cap as usize).copied().flatten()) {
                Some(cap) => base.patch(cap),
                None => base,
            }
        });
    }
}
