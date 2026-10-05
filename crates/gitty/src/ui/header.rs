//! Commit header: summary, authors, SHA, totals, date; `o` expands the body.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{fill, spans, text};
use crate::app::App;
use crate::dates::{format_date, identity_hue, initials};
use crate::text::truncate_end;

pub fn draw(app: &App, buf: &mut Buffer, r: Rect) {
    let ui = &app.theme.ui;
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width < 2 {
        return;
    }
    let right = r.right().saturating_sub(1);
    let x0 = r.x + 1;
    // bottom rule separating the header from the panes below
    if r.height >= 2 {
        let y = r.bottom() - 1;
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol("─").set_style(base.fg(ui.border));
        }
    }
    if let Some((oldest, newest)) = app.selected_range().filter(|(o, n)| o != n) {
        return range(app, buf, r, x0, right, base, (oldest, newest));
    }
    let row = app.detail.as_ref().map(|d| &d.row).or_else(|| app.selected_row());
    let Some(row) = row else {
        let msg = if app.history_len == 0 { "" } else { "…" };
        text(buf, x0, r.y, right, msg, base.fg(ui.muted));
        return;
    };
    let room = right.saturating_sub(x0) as usize;
    if row.summary.is_empty() {
        text(buf, x0, r.y, right, "Empty commit message", base.fg(ui.muted).add_modifier(Modifier::ITALIC));
    } else {
        text(buf, x0, r.y, right, &truncate_end(&row.summary, room), base.add_modifier(Modifier::BOLD));
    }
    if r.height < 3 {
        return;
    }
    let y = r.y + 1;
    let ini = initials(&row.author.name);
    let ini_st = base.fg(app.theme.avatar[identity_hue(&row.author.email) as usize]).add_modifier(Modifier::BOLD);
    let mut who = row.author.name.clone();
    for (name, _) in &row.co_authors {
        who.push_str(", ");
        who.push_str(name);
    }
    let sha = row.id.short(7);
    let date = format_date(row.author.time, row.author.offset_secs, app.now, app.date_mode);
    let dot = (" · ", base.fg(ui.muted));
    let mut x = spans(buf, x0, y, right, &[(&ini, ini_st), (" ", base)]);
    let tail_w = (sha.len() + date.len() + 24) as u16;
    let who_room = right.saturating_sub(x).saturating_sub(tail_w).max(8) as usize;
    x = text(buf, x, y, right, &truncate_end(&who, who_room), base);
    x = spans(buf, x, y, right, &[dot, (&sha, base.fg(ui.accent))]);
    x = totals(app, buf, x, y, right, base);
    spans(buf, x, y, right, &[dot, (&date, base.fg(ui.muted))]);
    if !app.header_expanded {
        return;
    }
    let mut y = r.y + 2;
    let last = r.bottom().saturating_sub(1);
    if let Some(d) = &app.detail {
        for line in d.body.lines().take(12) {
            if y >= last {
                return;
            }
            text(buf, x0, y, right, line, base);
            y += 1;
        }
        if !d.body.is_empty() && y < last {
            y += 1;
        }
        let mut meta = format!("commit {}", row.id.to_hex());
        if !row.parents.is_empty() {
            let ps: Vec<String> = row.parents.iter().map(|p| p.short(7)).collect();
            meta.push_str(&format!("  parents {}", ps.join(" ")));
        }
        if d.committer.name != row.author.name || d.committer.email != row.author.email {
            meta.push_str(&format!("  committed by {}", d.committer.name));
        }
        if y < last {
            text(buf, x0, y, right, &meta, base.fg(ui.muted));
        }
    }
}

/// ` · +A −D` once every file's line stats are known.
fn totals(app: &App, buf: &mut Buffer, x: u16, y: u16, right: u16, base: Style) -> u16 {
    let ui = &app.theme.ui;
    let all_known = app.stats.iter().all(Option::is_some);
    if !(app.stats_done && all_known && app.files.as_ref().is_some_and(|f| !f.is_empty())) {
        return x;
    }
    let (added, removed): (u32, u32) = app.stats.iter().flatten().fold((0, 0), |(a, d), s| (a + s.added, d + s.removed));
    let a = format!("+{added}");
    let d = format!(" −{removed}");
    spans(buf, x, y, right, &[(" · ", base.fg(ui.muted)), (&a, base.fg(ui.status_added)), (&d, base.fg(ui.status_deleted))])
}

/// A selected range: `N commits · <oldest>..<newest> · +A −D`, then the two ends' summaries.
fn range(app: &App, buf: &mut Buffer, r: Rect, x0: u16, right: u16, base: Style, (oldest, newest): (usize, usize)) {
    let ui = &app.theme.ui;
    let ends = match app.files_of() {
        Some(crate::msg::FilesOf::Range { oldest, newest }) => format!(" · {}..{}", oldest.short(7), newest.short(7)),
        _ => String::new(),
    };
    let n = format!("{} commits", oldest - newest + 1);
    let x = spans(buf, x0, r.y, right, &[(&n, base.add_modifier(Modifier::BOLD)), (&ends, base.fg(ui.accent))]);
    totals(app, buf, x, r.y, right, base);
    if r.height < 3 {
        return;
    }
    let summary = |i: usize| app.rows.get(&i).map_or("…", |row| row.summary.as_str());
    let line = format!("{}  …  {}", summary(oldest), summary(newest));
    text(buf, x0, r.y + 1, right, &truncate_end(&line, right.saturating_sub(x0) as usize), base.fg(ui.muted));
}
