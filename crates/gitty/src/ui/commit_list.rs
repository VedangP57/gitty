//! The history list: marker, summary, right-aligned badges, initials and date.

use std::sync::PoisonError;

use gitty_core::refs::{HistoryScope, RefKind, RefLabel};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{centered, fill, spans, text, width};
use crate::app::{App, Focus};
use crate::config::Density;
use crate::dates::{format_date, identity_hue, initials};
use crate::text::truncate_end;

/// Summary keeps at least this many columns before badges, then the date, are dropped.
const MIN_SUMMARY: u16 = 20;
const MAX_BADGE: usize = 24;

fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn title(app: &App, buf: &mut Buffer, r: Rect, label: &str, focused: bool, extra: &str) {
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.bg);
    fill(buf, Rect::new(r.x, r.y, r.width, 1), base);
    let st = if focused { base.fg(ui.accent).add_modifier(Modifier::BOLD) } else { base.fg(ui.fg).add_modifier(Modifier::BOLD) };
    spans(buf, r.x + 1, r.y, r.right(), &[(label, st), (extra, base.fg(ui.muted))]);
}

fn badge_style(app: &App, l: &RefLabel) -> Style {
    let ui = &app.theme.ui;
    match l.kind {
        RefKind::LocalBranch if l.is_head => Style::new().fg(ui.badge_head_fg).bg(ui.badge_head_bg).add_modifier(Modifier::BOLD),
        RefKind::LocalBranch => Style::new().fg(ui.badge_local_fg).bg(ui.badge_local_bg),
        RefKind::RemoteBranch => Style::new().fg(ui.badge_remote_fg).bg(ui.badge_remote_bg),
        RefKind::Tag => Style::new().fg(ui.badge_tag_fg).bg(ui.badge_tag_bg),
    }
}

pub fn draw(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    fill(buf, r, Style::new().bg(ui.bg).fg(ui.fg));
    if r.height == 0 || r.width == 0 {
        return;
    }
    let count = if app.history_len == 0 && !app.history_done { String::new() } else { format!(" · {}{}", group(app.history_len), if app.history_done { "" } else { "…" }) };
    let scope = if app.scope == HistoryScope::AllRefs { " · all refs" } else { "" };
    title(app, buf, r, "History", app.focus == Focus::History, &format!("{count}{scope}"));
    let rows = Rect::new(r.x, r.y + 1, r.width, r.height.saturating_sub(1));
    let row_h = app.row_height() as u16;
    app.hits.history_rows = Some(rows);
    app.hits.history_first = app.list_scroll;
    app.hits.history_row_h = row_h;
    if rows.height == 0 {
        return;
    }
    if app.history_len == 0 {
        let msg = if app.history_done { "No commits yet" } else { "Loading history…" };
        centered(buf, rows, rows.y + rows.height / 3, msg, Style::new().bg(ui.bg).fg(ui.muted));
        return;
    }
    let ids: Vec<(usize, gitty_core::CommitId)> = {
        let Some(h) = &app.history else { return };
        let h = h.read().unwrap_or_else(PoisonError::into_inner);
        let n = (rows.height / row_h.max(1)) as usize;
        (app.list_scroll..(app.list_scroll + n).min(h.len())).map(|i| (i, h.id(i))).collect()
    };
    let focused = app.focus == Focus::History;
    let range = app.selected_range();
    for (k, (i, id)) in ids.into_iter().enumerate() {
        let y = rows.y + k as u16 * row_h;
        let selected = i == app.selected;
        let in_range = range.is_some_and(|(oldest, newest)| (newest..=oldest).contains(&i));
        let bg = match (selected, in_range) {
            (true, _) if focused => ui.selection,
            (true, _) | (false, true) => ui.selection_inactive,
            _ => ui.bg,
        };
        let base = Style::new().bg(bg).fg(ui.fg);
        fill(buf, Rect::new(rows.x, y, rows.width, row_h.min(rows.bottom() - y)), base);
        let behind = app.behind.contains(&id);
        let (marker, mst) = if app.ahead.contains(&id) {
            ("↑", base.fg(ui.ahead))
        } else if behind {
            ("↓", base.fg(ui.behind))
        } else {
            (" ", base)
        };
        let right = rows.right().saturating_sub(1);
        let x0 = text(buf, rows.x + 1, y, right, marker, mst) + 1;
        let Some(row) = app.rows.get(&i) else {
            text(buf, x0, y, right, "…", base.fg(ui.muted));
            continue;
        };
        let summary_st = if app.search.hits.contains(&i) {
            base.fg(ui.warning).add_modifier(Modifier::BOLD)
        } else if behind {
            base.fg(ui.muted)
        } else {
            base
        };
        let date = format_date(row.author.time, row.author.offset_secs, app.now, app.date_mode);
        let ini = initials(&row.author.name);
        let ini_st = base.fg(app.theme.avatar[identity_hue(&row.author.email) as usize]).add_modifier(Modifier::BOLD);
        let labels: Vec<RefLabel> = app.refs.as_ref().and_then(|r| r.labels.get(&id)).cloned().unwrap_or_default();
        let badges: Vec<(String, Style)> = labels.iter().map(|l| (format!(" {} ", truncate_end(&l.name, MAX_BADGE)), badge_style(app, l))).collect();
        let badges_w: u16 = badges.iter().map(|(s, _)| width(s) + 1).sum();
        let avail = right.saturating_sub(x0);
        let comfortable = app.density == Density::Comfortable;
        let meta_w = if comfortable { 0 } else { width(&ini) + 1 + width(&date) + 1 };
        let (show_badges, show_date) = if avail >= MIN_SUMMARY + badges_w + meta_w {
            (true, true)
        } else if avail >= MIN_SUMMARY + meta_w {
            (false, true)
        } else {
            (false, false)
        };
        let mut rx = right;
        if !comfortable {
            if show_date {
                rx = rx.saturating_sub(width(&date));
                text(buf, rx, y, right, &date, base.fg(ui.muted));
                rx = rx.saturating_sub(1);
            }
            if avail > MIN_SUMMARY / 2 + width(&ini) {
                rx = rx.saturating_sub(width(&ini));
                text(buf, rx, y, right, &ini, ini_st);
                rx = rx.saturating_sub(1);
            }
        }
        if show_badges {
            for (s, st) in badges.iter().rev() {
                rx = rx.saturating_sub(width(s));
                text(buf, rx, y, right, s, *st);
                rx = rx.saturating_sub(1);
            }
        }
        let room = rx.saturating_sub(x0) as usize;
        if row.summary.is_empty() {
            text(buf, x0, y, rx, &truncate_end("Empty commit message", room), base.fg(ui.muted).add_modifier(Modifier::ITALIC));
        } else {
            text(buf, x0, y, rx, &truncate_end(&row.summary, room), summary_st);
        }
        if comfortable && row_h > 1 && y + 1 < rows.bottom() {
            let line = format!("{} · {}", row.author.name, date);
            let x = text(buf, x0, y + 1, right, &ini, ini_st) + 1;
            text(buf, x, y + 1, right, &truncate_end(&line, right.saturating_sub(x) as usize), base.fg(ui.muted));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn group_thousands() {
        assert_eq!(super::group(0), "0");
        assert_eq!(super::group(999), "999");
        assert_eq!(super::group(85_887), "85,887");
        assert_eq!(super::group(1_484_291), "1,484,291");
    }
}
