//! The conflict view: the file's lines with each conflict block drawn as two tinted sides. Marker
//! lines are not shown raw: `<<<<<<<` and `=======` become the sides' header lines and `>>>>>>>`
//! a rule closing the block (with the keys, in the block the cursor is in). Every file line is
//! one screen line, so line numbers stay the file's own.

use gitty_highlight::{CAPTURES, Span};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::commit_list::title;
use super::paint::{edge_marks, fill, glyphs, spans, text, width};
use crate::app::conflict::{ConflictView, Role, describe, has_file};
use crate::app::{App, Focus, digits};
use crate::keymap::Action;
use crate::msg::ConflictBody;
use crate::text::{Glyph, layout_until};

/// Lines longer than this (bytes) get no syntax colour: they are clipped and cost nothing.
const NO_SYNTAX_OVER: usize = 1000;

/// `o keep Current (main) · t take Incoming (topic) · b both`, from the live keymap.
pub fn key_hint(app: &App, view: &ConflictView) -> String {
    let key = |a: Action| app.keymap.keys_of(a).first().map_or("?".to_string(), |k| k.label());
    let both = if view.conflicts().is_empty() { String::new() } else { format!(" · {} both", key(Action::ConflictBoth)) };
    format!("{} keep {} · {} take {}{both}", key(Action::ConflictOurs), view.sides.ours.title, key(Action::ConflictTheirs), view.sides.theirs.title)
}

pub fn draw(app: &mut App, buf: &mut Buffer, r: Rect) {
    let theme = app.theme.clone();
    let ui = &theme.ui;
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    app.hits.diff_rows = None;
    let focused = app.focus == Focus::Diff;
    let path = app.changes.selected().map(|e| e.path.clone()).unwrap_or_default();
    let body = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    if app.conflict_view().is_none() {
        title(app, buf, r, &path, focused, "  loading…");
        return;
    }
    let hl = app.conflict_highlights();
    let Some(mut view) = app.changes.conflict.take() else { return };
    let n = view.conflicts().len();
    let extra = match &view.body {
        ConflictBody::Text { .. } if n > 0 => format!("  conflict {}/{n}", view.cur + 1),
        ConflictBody::Text { .. } if view.unknown() => "  markers not understood".to_string(),
        ConflictBody::Text { .. } => "  no conflict markers left".to_string(),
        ConflictBody::Other(_) => "  conflicted".to_string(),
    };
    title(app, buf, r, &path, focused, &extra);
    let hint = key_hint(app, &view);
    match view.body {
        ConflictBody::Text { .. } if n > 0 => draw_blocks(app, buf, body, &mut view, hl.as_deref(), &hint),
        ConflictBody::Text { .. } if matches!((view.entry.x, view.entry.y), ('U', 'U') | ('A', 'A')) => {
            let done = if view.unknown() { crate::app::conflict::NOT_UNDERSTOOD } else { "No conflict markers are left in this file. Press Space to stage it." };
            draw_message(app, buf, body, &view, Some(done));
        }
        _ => draw_message(app, buf, body, &view, None),
    }
    app.changes.conflict = Some(view);
}

/// A conflict with no blocks to draw: what happened, and what each key does.
fn draw_message(app: &App, buf: &mut Buffer, body: Rect, view: &ConflictView, done: Option<&str>) {
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    let muted = base.fg(ui.muted);
    let (x, right) = (body.x + 2, body.right());
    let mut y = body.y + body.height.min(2) / 2;
    let mut line = |buf: &mut Buffer, parts: &[(&str, Style)]| {
        if y < body.bottom() {
            spans(buf, x, y, right, parts);
        }
        y += 1;
    };
    line(buf, &[(&describe(&view.entry, &view.sides), base.fg(ui.warning).add_modifier(Modifier::BOLD))]);
    if let Some(done) = done {
        line(buf, &[("", base)]);
        return line(buf, &[(done, base.fg(if view.unknown() { ui.warning } else { ui.accent }))]);
    }
    if let (ConflictBody::Other(why), ('U', 'U') | ('A', 'A')) = (&view.body, (view.entry.x, view.entry.y)) {
        line(buf, &[(why, muted)]);
    }
    line(buf, &[("", base)]);
    let (ours_has, theirs_has) = has_file(view.entry.x, view.entry.y);
    let key = |a: Action| app.keymap.keys_of(a).first().map_or("?".to_string(), |k| k.label());
    for (a, side, has) in [(Action::ConflictOurs, &view.sides.ours, ours_has), (Action::ConflictTheirs, &view.sides.theirs, theirs_has)] {
        let outcome = if has { "keep its version of the file" } else { "delete the file" };
        line(buf, &[(&key(a), base.fg(ui.accent).add_modifier(Modifier::BOLD)), ("  ", base), (&side.label(), base), (": ", muted), (outcome, muted)]);
    }
    line(buf, &[("", base)]);
    line(buf, &[("Either choice is staged for you; a copy of the file goes to the Trash first.", muted)]);
}

fn draw_blocks(app: &mut App, buf: &mut Buffer, body: Rect, view: &mut ConflictView, hl: Option<&gitty_highlight::Highlights>, hint: &str) {
    let theme = app.theme.clone();
    let (ui, d) = (&theme.ui, &theme.diff);
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    let Some(text_lines) = view.text().cloned() else { return };
    let lines = text_lines.len() as usize;
    let digits = digits(lines as u32) as u16;
    let tx = body.x + digits + 2;
    let tab = app.config.tab_size;
    view.vscroll = view.vscroll.min(lines.saturating_sub(1));
    view.visible = body.right().saturating_sub(tx);
    view.hscroll = view.hscroll.min(view.max_hscroll());
    let hscroll = u32::from(view.hscroll);
    // only a highlighted file needs the capture styles; the row's own background always shows
    let syntax: Vec<Option<Style>> = if hl.is_some() { CAPTURES.iter().map(|c| theme.syntax.get(*c).map(|s| Style { bg: None, ..*s })).collect() } else { Vec::new() };
    let mut scratch: Vec<Glyph> = Vec::new();
    for k in 0..usize::from(body.height) {
        let i = view.vscroll + k;
        if i >= lines {
            break;
        }
        let y = body.y + k as u16;
        let row = Rect::new(body.x, y, body.width, 1);
        let role = view.role(i);
        let block = match role {
            Role::Plain => None,
            Role::OursHead(b) | Role::Ours(b) | Role::BaseHead(b) | Role::Base(b) | Role::TheirsHead(b) | Role::Theirs(b) | Role::End(b) => Some(b),
        };
        let current = block == Some(view.cur);
        let (bg, head) = match role {
            Role::Plain | Role::End(_) => (ui.bg, false),
            Role::Ours(_) => (if current { d.ours_current_bg } else { d.ours_bg }, false),
            Role::Theirs(_) => (if current { d.theirs_current_bg } else { d.theirs_bg }, false),
            Role::Base(_) => (d.base_bg, false),
            Role::OursHead(_) => (d.ours_head, true),
            Role::TheirsHead(_) => (d.theirs_head, true),
            Role::BaseHead(_) => (d.base_bg, true),
        };
        let st = Style::new().bg(bg).fg(if matches!(role, Role::Base(_) | Role::BaseHead(_)) { ui.muted } else { ui.fg });
        fill(buf, row, st);
        if current {
            buf[(body.x, y)].set_symbol("▌").set_style(Style::new().bg(bg).fg(ui.accent));
        }
        if head {
            let (label, mark) = match role {
                Role::OursHead(b) => (view.label(b, true), "◂ "),
                Role::TheirsHead(b) => (view.label(b, false), "▸ "),
                _ => ("Base".to_string(), "│ "),
            };
            let bold = st.add_modifier(Modifier::BOLD);
            // the sides of a block like this cannot be told apart: it is left to the editor
            let warn = match role {
                Role::OursHead(b) if view.conflicts()[b].ambiguous => "  ⚠ ambiguous markers: e to edit",
                _ => "",
            };
            spans(buf, body.x + 2, y, body.right(), &[(mark, bold.fg(ui.accent)), (&label, bold), (warn, bold.fg(ui.warning))]);
            continue;
        }
        if let Role::End(_) = role {
            let rule = Style::new().bg(ui.bg).fg(ui.border);
            for x in body.x + 1..body.right() {
                buf[(x, y)].set_symbol("─").set_style(rule);
            }
            // the keys sit on the rule of the block the cursor is in
            if current {
                let label = format!(" {hint} ");
                let w = width(&label);
                if body.width > w + 4 {
                    text(buf, body.x + 2, y, body.right(), &label, base.fg(ui.accent));
                }
            }
            continue;
        }
        let n = (i + 1).to_string();
        text(buf, body.x + 1 + digits.saturating_sub(width(&n)), y, tx, &n, Style::new().bg(bg).fg(d.lineno));
        let line = text_lines.line(i as u32);
        layout_until(line, tab, hscroll + u32::from(body.right().saturating_sub(tx)) + 1, &mut scratch);
        let syn: &[Span] = match hl {
            Some(h) if line.len() <= NO_SYNTAX_OVER => h.line(i as u32),
            _ => &[],
        };
        glyphs(buf, tx, y, body.right(), hscroll, &scratch, |g| {
            if g.ctrl {
                return st.fg(ui.muted);
            }
            let j = syn.partition_point(|s| s.end <= g.byte);
            match syn.get(j).filter(|s| s.start <= g.byte).and_then(|s| syntax.get(s.cap as usize).copied().flatten()) {
                Some(cap) => st.patch(cap),
                None => st,
            }
        });
        edge_marks(buf, tx, y, body.right(), hscroll, &scratch, ui.muted);
    }
}
