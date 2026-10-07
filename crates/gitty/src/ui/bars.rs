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
    let changes = match app.changes.entries().len() {
        0 => "[1] Changes".to_string(),
        n => format!("[1] Changes ({n})"),
    };
    let tabs = [(Tab::Changes, changes.as_str()), (Tab::History, "[2] History")];
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
        } else if refs.unpublished() {
            let n = app.ahead.len();
            let s = if n > 0 { format!("  ↑{n} not published") } else { "  not published".to_string() };
            x = text(buf, x, y, max_x, &s, base.fg(ui.ahead));
        }
    }
    if let Some(r) = app.needs_auth.as_deref().filter(|_| app.net.is_none()) {
        text(buf, x, y, max_x, &format!("  {r} needs auth · f"), base.fg(ui.warning));
    } else if let Some(p) = app.background_problem().filter(|_| app.net.is_none()) {
        text(buf, x, y, max_x, &format!("  {p}"), base.fg(ui.warning));
    } else if let Some(bar) = app.net_bar() {
        text(buf, x, y, max_x, &format!("  {bar}"), base.fg(ui.accent));
    } else if let Some(t) = app.fetched_at {
        let ago = format_date(t, 0, app.now, DateMode::Relative);
        let s = if ago == "now" { "  fetched just now".to_string() } else { format!("  fetched {ago} ago") };
        text(buf, x, y, max_x, &s, muted);
    }
    app.hits.tabs = tab_hits;
}

/// `G` stays `G`; named keys read lowercase (`enter`, `ctrl-d`).
fn hint_label(k: &crate::keymap::Key) -> String {
    let l = k.label();
    if l.chars().count() == 1 { l } else { l.to_lowercase() }
}

/// Bottom-bar hints for the focused pane, with the keys the keymap really uses (`alt+enter` is
/// fixed).
fn hints(app: &App) -> Vec<(String, &'static str)> {
    use crate::keymap::Action as A;
    let list: &[(&[A], &str)] = if app.tab == Tab::Changes {
        match app.focus {
            Focus::Diff => &[(&[A::Stage], "stage line"), (&[A::LineRange], "range"), (&[A::StageHunk], "hunk"), (&[A::StageAll], "file"), (&[A::Discard], "discard"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Back], "back")],
            Focus::Commit => return vec![("alt+enter".into(), "commit"), ("tab".into(), "field"), ("esc".into(), "leave")],
            _ => &[(&[A::Stage], "stage"), (&[A::StageAll], "all"), (&[A::Discard], "discard"), (&[A::Filter], "filter"), (&[A::Open], "diff"), (&[A::HistoryTab], "history"), (&[A::Help], "help"), (&[A::Quit], "quit")],
        }
    } else {
        match app.focus {
            Focus::History if app.compare.is_some() => &[(&[A::Down, A::Up], "move"), (&[A::CompareBehind, A::CompareAhead], "tab"), (&[A::Open], "files"), (&[A::Compare], "other branch"), (&[A::Back], "leave"), (&[A::Help], "help")],
            Focus::History => &[(&[A::Down, A::Up], "move"), (&[A::Open], "files"), (&[A::Search], "search"), (&[A::Range], "range"), (&[A::Compare], "compare"), (&[A::NextPane], "pane"), (&[A::Scope], "scope"), (&[A::Help], "help"), (&[A::Quit], "quit")],
            Focus::Files => &[(&[A::Down, A::Up], "file"), (&[A::Open], "diff"), (&[A::Tree], "tree"), (&[A::Back], "back"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Help], "help"), (&[A::Quit], "quit")],
            Focus::Commit => &[],
            Focus::Diff => &[(&[A::Down, A::Up], "line"), (&[A::PrevHunk, A::NextHunk], "hunk"), (&[A::Expand, A::ExpandFile], "expand"), (&[A::Split], "split"), (&[A::Whitespace], "whitespace"), (&[A::Difftool], "difftool"), (&[A::Fullscreen], "full"), (&[A::Back], "back")],
        }
    };
    list.iter()
        .filter_map(|(acts, what)| {
            let keys: Vec<String> = acts.iter().filter_map(|a| app.keymap.keys_of(*a).first().map(hint_label)).collect();
            (!keys.is_empty()).then(|| (keys.join("/"), *what))
        })
        .collect()
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
    if let Some(label) = app.search_label() {
        let end = text(buf, x, r.y, max_x, &label, base.fg(ui.accent).add_modifier(Modifier::BOLD));
        if let Some(bar) = &app.search.bar {
            // block cursor after "/" and the text before the editor's cursor
            let cx = x + 1 + width(&bar.text()[..bar.cursor()]);
            if cx < max_x {
                let cell = &mut buf[(cx, r.y)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
        x = end + 2;
    }
    // an overlay has the keys: it prints its own hints, the pane's would mislead
    let pane_hints = if app.overlay.is_some() { Vec::new() } else { hints(app) };
    for (k, d) in pane_hints {
        if x + width(&k) + width(d) + 3 > max_x {
            break;
        }
        x = spans(buf, x, r.y, max_x, &[(&k, base.fg(ui.accent)), (" ", base), (d, base), ("  ", base)]);
    }
}
