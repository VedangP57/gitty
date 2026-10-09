//! The history list: marker, commit graph, summary, right-aligned badges, initials and date.

use std::sync::PoisonError;

use gitty_core::history::CommitRow;
use gitty_core::refs::{HistoryScope, RefKind, RefLabel};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{centered, fill, spans, text, width};
use crate::app::compare::CompareTab;
use crate::app::{App, Focus};
use crate::config::Density;
use crate::dates::{format_date, identity_hue, initials};
use crate::text::truncate_end;

/// Summary keeps at least this many columns before badges, then the date, are dropped.
const MIN_SUMMARY: u16 = 20;
const MAX_BADGE: usize = 24;
/// Columns the graph leaves for the marker, the summary and the date (wider rows are cut with
/// `›`). With [`crate::app::GRAPH_MIN_WIDTH`] it leaves the graph at least 6 columns.
const GRAPH_REST: u16 = 28;

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
    if app.compare.is_some() {
        return draw_compare(app, buf, r);
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
    let shown = app.graph_shown();
    let ids: Vec<(usize, gitty_core::CommitId, Option<Graph>)> = {
        let Some(h) = &app.history else { return };
        let h = h.read().unwrap_or_else(PoisonError::into_inner);
        let n = (rows.height / row_h.max(1)) as usize;
        (app.list_scroll..(app.list_scroll + n).min(h.len()))
            .map(|i| {
                let graph = h.graph_row(i).filter(|_| shown).map(|r| Graph { cells: r.glyphs().collect(), filler: r.filler().collect(), clipped: r.clipped() });
                (i, h.id(i), graph)
            })
            .collect()
    };
    let app: &App = app;
    let focused = app.focus == Focus::History;
    let range = app.selected_range();
    let widest = ids.iter().filter_map(|r| r.2.as_ref()).map(|g| g.cells.len() + usize::from(g.clipped)).max().unwrap_or(0) as u16;
    let graph_w = widest.min(rows.width.saturating_sub(GRAPH_REST));
    for (k, (i, id, graph)) in ids.into_iter().enumerate() {
        let y = rows.y + k as u16 * row_h;
        let selected = i == app.selected;
        let in_range = range.is_some_and(|(oldest, newest)| (newest..=oldest).contains(&i));
        let bg = match (selected, in_range) {
            (true, _) if focused => ui.selection,
            (true, _) | (false, true) => ui.selection_inactive,
            _ => ui.bg,
        };
        draw_row(app, buf, rows, y, id, app.rows.get(&i), bg, app.search.hits.contains(&i), graph_w);
        if let Some(g) = graph {
            let x = rows.x + 3;
            draw_graph(app, buf, x, y, graph_w, &g.cells, g.clipped, bg);
            if row_h > 1 && y + 1 < rows.bottom() {
                // a second line of text: the lanes that go on down carry on through it
                draw_graph(app, buf, x, y + 1, graph_w, &g.filler, false, bg);
            }
        }
    }
}

/// A row's graph columns, copied out from under the history lock.
struct Graph {
    cells: Vec<Option<(char, u8)>>,
    filler: Vec<Option<(char, u8)>>,
    clipped: bool,
}

/// One line of the graph, each lane in its colour. A row wider than `w` columns (or wider than
/// the lanes stored) ends in `›`.
#[allow(clippy::too_many_arguments)]
fn draw_graph(app: &App, buf: &mut Buffer, x: u16, y: u16, w: u16, cells: &[Option<(char, u8)>], clipped: bool, bg: ratatui::style::Color) {
    let ui = &app.theme.ui;
    // one theme hue per lane colour index (gitty_core::graph::COLOURS of them)
    let palette = [ui.accent, ui.status_added, ui.status_modified, ui.status_renamed, ui.status_deleted, ui.pr_merged, ui.behind];
    let w = w as usize;
    let cut = clipped || cells.len() > w;
    let shown = if cut { w.saturating_sub(1) } else { w };
    // cells of one colour in a row make one span; blanks join the span before them
    let mut parts: Vec<(String, Style)> = Vec::new();
    for cell in cells.iter().take(shown) {
        match (cell, parts.last_mut()) {
            (Some((g, c)), last) => {
                let st = Style::new().bg(bg).fg(palette[*c as usize % palette.len()]);
                match last {
                    Some((s, l)) if *l == st => s.push(*g),
                    _ => parts.push((g.to_string(), st)),
                }
            }
            (None, Some((s, _))) => s.push(' '),
            (None, None) => parts.push((" ".into(), Style::new().bg(bg))),
        }
    }
    if cut && w > 0 {
        let pad = shown.saturating_sub(cells.len().min(shown));
        parts.push((format!("{}›", " ".repeat(pad)), Style::new().bg(bg).fg(ui.muted)));
    }
    let parts: Vec<(&str, Style)> = parts.iter().map(|(s, st)| (s.as_str(), *st)).collect();
    spans(buf, x, y, x + w as u16, &parts);
}

/// Compare mode: title, the Behind / Ahead / Files tabs, then the tab's commits.
fn draw_compare(app: &mut App, buf: &mut Buffer, r: Rect) {
    let ui = app.theme.ui.clone();
    let Some(c) = &app.compare else { return };
    let focused = app.focus == Focus::History;
    let label = format!("Compare with {}", c.other);
    title(app, buf, r, &label, focused, "");
    if r.height < 3 {
        return;
    }
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    let y = r.y + 1;
    let Some(res) = &c.result else {
        text(buf, r.x + 1, y, r.right(), "Comparing…", base.fg(ui.muted));
        return;
    };
    let tabs = [
        (CompareTab::Behind, format!("Behind ({})", group(res.behind.len()))),
        (CompareTab::Ahead, format!("Ahead ({})", group(res.ahead.len()))),
        (CompareTab::Files, "Files".to_string()),
    ];
    let mut x = r.x + 1;
    for (tab, s) in &tabs {
        let st = if *tab == c.tab { base.fg(ui.accent).add_modifier(Modifier::BOLD | Modifier::UNDERLINED) } else { base.fg(ui.muted) };
        x = text(buf, x, y, r.right(), s, st) + 2;
    }
    let rows = Rect::new(r.x, r.y + 2, r.width, r.height - 2);
    let row_h = app.row_height() as u16;
    let first = c.first_visible();
    app.hits.history_rows = Some(rows);
    app.hits.history_first = first;
    app.hits.history_row_h = row_h;
    let app: &App = app;
    let Some(c) = &app.compare else { return };
    if c.tab == CompareTab::Files {
        let n = app.files.as_ref().map(|f| f.len());
        let msg = match (n, res.merge_base) {
            (None, _) => "Loading files…".to_string(),
            (Some(n), Some(mb)) => format!("{n} files changed on {} since {}", c.other, mb.short(7)),
            (Some(n), None) => format!("{n} files: no common ancestor, so the whole tree"),
        };
        centered(buf, rows, rows.y + rows.height / 3, &msg, base.fg(ui.muted));
        return;
    }
    let list = c.list();
    if list.is_empty() {
        let msg = match c.tab {
            CompareTab::Behind => format!("{} has nothing HEAD does not", c.other),
            _ => format!("HEAD has nothing {} does not", c.other),
        };
        centered(buf, rows, rows.y + rows.height / 3, &msg, base.fg(ui.muted));
        return;
    }
    let sel = c.selected().unwrap_or(0);
    let n = (rows.height / row_h.max(1)) as usize;
    for (k, (i, id)) in list.iter().enumerate().skip(first).take(n).enumerate() {
        let y = rows.y + k as u16 * row_h;
        let bg = if i == sel { if focused { ui.selection } else { ui.selection_inactive } } else { ui.bg };
        draw_row(app, buf, rows, y, *id, c.rows.get(id), bg, false, 0);
    }
}

/// One commit row at `y` (and the line below it in comfortable density), leaving `graph_w`
/// columns after the marker for the graph.
#[allow(clippy::too_many_arguments)]
fn draw_row(app: &App, buf: &mut Buffer, rows: Rect, y: u16, id: gitty_core::CommitId, row: Option<&CommitRow>, bg: ratatui::style::Color, hit: bool, graph_w: u16) {
    let ui = &app.theme.ui;
    let row_h = app.row_height() as u16;
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
    let x0 = if graph_w > 0 { (x0 + graph_w + 1).min(right) } else { x0 };
    let Some(row) = row else {
        text(buf, x0, y, right, "…", base.fg(ui.muted));
        return;
    };
    let summary_st = if hit {
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
