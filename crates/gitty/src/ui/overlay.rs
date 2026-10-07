//! Centered overlays: theme picker, help, error detail, confirmations.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use super::paint::{fill, spans, text, text_right};
use crate::askpass::AskKind;
use crate::app::{App, Overlay};
use crate::dates::{DateMode, format_date};
use crate::keymap::Ctx;

/// Help sections, in display order, and the keys that are not rebindable.
const SECTIONS: &[(Ctx, &str)] = &[
    (Ctx::Global, "Everywhere"),
    (Ctx::Nav, "Moving"),
    (Ctx::History, "History"),
    (Ctx::Compare, "Compare mode"),
    (Ctx::Diff, "Diff"),
    (Ctx::Changes, "Changes"),
    (Ctx::ChangesList, "Changes file list"),
    (Ctx::ChangesDiff, "Changes diff"),
];
const FIXED: &[(&str, &str)] = &[
    ("Ctrl-c", "quit"),
    ("Ctrl-z", "suspend to the shell"),
    ("Alt-Enter", "commit (Ctrl-Enter in kitty)"),
    ("double-click", "open the file in $EDITOR"),
];

/// (keys, what) rows of the help, with `None` keys for section titles; from the live keymap.
pub fn help_lines(app: &App) -> Vec<(Option<String>, String)> {
    let mut out = Vec::new();
    let bindings = app.keymap.bindings();
    for (ctx, title) in SECTIONS {
        out.push((None, title.to_string()));
        for (a, _, keys, help) in &bindings {
            if crate::keymap::ACTIONS.iter().any(|x| x.0 == *a && x.2 == *ctx) {
                let ks = if keys.is_empty() { "(unbound)".to_string() } else { keys.iter().map(|k| k.label()).collect::<Vec<_>>().join(" ") };
                out.push((Some(ks), help.to_string()));
            }
        }
    }
    out.push((None, "Fixed".into()));
    out.extend(FIXED.iter().map(|(k, w)| (Some(k.to_string()), w.to_string())));
    out
}

fn boxed(app: &App, buf: &mut Buffer, area: Rect, w: u16, h: u16, title: &str) -> Rect {
    let ui = &app.theme.ui;
    let w = w.min(area.width);
    let h = h.min(area.height);
    let r = Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h);
    let st = Style::new().bg(ui.panel).fg(ui.fg);
    fill(buf, r, st);
    let b = st.fg(ui.border_focus);
    if w >= 2 && h >= 2 {
        for x in r.left()..r.right() {
            buf[(x, r.top())].set_symbol("─").set_style(b);
            buf[(x, r.bottom() - 1)].set_symbol("─").set_style(b);
        }
        for y in r.top()..r.bottom() {
            buf[(r.left(), y)].set_symbol("│").set_style(b);
            buf[(r.right() - 1, y)].set_symbol("│").set_style(b);
        }
        buf[(r.left(), r.top())].set_symbol("╭");
        buf[(r.right() - 1, r.top())].set_symbol("╮");
        buf[(r.left(), r.bottom() - 1)].set_symbol("╰");
        buf[(r.right() - 1, r.bottom() - 1)].set_symbol("╯");
        text(buf, r.x + 2, r.y, r.right().saturating_sub(1), &format!(" {title} "), b.add_modifier(Modifier::BOLD));
    }
    Rect::new(r.x + 2, r.y + 1, w.saturating_sub(4), h.saturating_sub(2))
}

pub fn draw(app: &App, buf: &mut Buffer, area: Rect) {
    let Some(ov) = &app.overlay else { return };
    let ui = &app.theme.ui;
    let st = Style::new().bg(ui.panel).fg(ui.fg);
    match ov {
        Overlay::ThemePicker { sel, .. } => {
            let names = app.registry.names();
            let inner = boxed(app, buf, area, 40, names.len() as u16 + 4, "Theme");
            let rows = inner.height.saturating_sub(1) as usize;
            let first = sel.saturating_sub(rows.saturating_sub(1));
            for (k, (i, n)) in names.iter().enumerate().skip(first).take(rows).enumerate() {
                let y = inner.y + k as u16;
                let s = if i == *sel { st.bg(ui.selection).add_modifier(Modifier::BOLD) } else { st };
                fill(buf, Rect::new(inner.x, y, inner.width, 1), s);
                let mark = if *n == app.config.theme { "● " } else { "  " };
                spans(buf, inner.x, y, inner.right(), &[(mark, s.fg(ui.accent)), (n, s)]);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "Enter apply · Esc cancel", st.fg(ui.muted));
            }
        }
        Overlay::Help { scroll } => {
            let lines = help_lines(app);
            // two columns when they fit
            let cols = if area.width >= 136 { 2 } else { 1 };
            let per = lines.len().div_ceil(cols);
            let w = if cols == 2 { 132 } else { 66 };
            let inner = boxed(app, buf, area, w, per as u16 + 3, "Keys · j/k scroll · Esc close");
            let rows = inner.height.saturating_sub(1) as usize;
            let first = (*scroll).min(per.saturating_sub(rows));
            for c in 0..cols {
                let x0 = inner.x + (c as u16) * (w / 2);
                for (k, (keys, what)) in lines.iter().skip(c * per).take(per).skip(first).take(rows).enumerate() {
                    let y = inner.y + k as u16;
                    match keys {
                        None => {
                            text(buf, x0, y, x0 + w / 2 - 2, what, st.fg(ui.fg).add_modifier(Modifier::BOLD | Modifier::UNDERLINED));
                        }
                        Some(keys) => {
                            text(buf, x0, y, x0 + 20, keys, st.fg(ui.accent));
                            text(buf, x0 + 21, y, x0 + w / 2 - 2, what, st);
                        }
                    }
                }
            }
        }
        Overlay::Confirm { title, body, op } => {
            let w = (crate::text::display_width(title).max(crate::text::display_width(body)) as u16 + 6).clamp(40, area.width.saturating_sub(4).max(40));
            let inner = boxed(app, buf, area, w, 6, "Confirm");
            text(buf, inner.x, inner.y, inner.right(), title, st.fg(ui.warning).add_modifier(Modifier::BOLD));
            text(buf, inner.x, inner.y + 1, inner.right(), body, st.fg(ui.muted));
            if inner.height > 3 {
                spans(buf, inner.x, inner.y + 3, inner.right(), &[("Enter", st.fg(ui.accent)), (if matches!(op, crate::msg::WriteOp::Merge { .. }) { " merge · " } else { " discard · " }, st), ("Esc", st.fg(ui.accent)), (" cancel", st)]);
            }
        }
        Overlay::Prompt { ask, input } => {
            // ssh's host-key question is several lines; the fingerprint must be readable
            let w = area.width.saturating_sub(8).clamp(30, 96);
            let lines: Vec<String> = ask.prompt.trim().lines().flat_map(|l| wrap_text(l, w.saturating_sub(4) as usize)).collect();
            let n = lines.len() as u16;
            let inner = boxed(app, buf, area, w, n + 6, "git asks");
            for (k, l) in lines.iter().enumerate().take(inner.height.saturating_sub(4) as usize) {
                text(buf, inner.x, inner.y + k as u16, inner.right(), l, st.add_modifier(Modifier::BOLD));
            }
            let inner = Rect::new(inner.x, inner.y + n.saturating_sub(1), inner.width, inner.height.saturating_sub(n.saturating_sub(1)));
            if inner.height > 2 {
                let field = Rect::new(inner.x, inner.y + 2, inner.width, 1);
                fill(buf, field, Style::new().bg(ui.bg).fg(ui.fg));
                let shown = match ask.kind {
                    AskKind::Text => input.text().to_string(),
                    // one bullet per character typed; never the text
                    AskKind::Secret => "•".repeat(input.text().chars().count()),
                    AskKind::YesNo => String::new(),
                };
                let end = text(buf, field.x + 1, field.y, field.right(), &shown, Style::new().bg(ui.bg).fg(ui.fg));
                if ask.kind != AskKind::YesNo && end < field.right() {
                    buf[(end, field.y)].set_symbol(" ").set_style(Style::new().add_modifier(Modifier::REVERSED));
                }
            }
            if inner.height > 4 {
                let keys: &[(&str, &str)] = if ask.kind == AskKind::YesNo { &[("y", " yes · "), ("n", " no · "), ("Esc", " cancel")] } else { &[("Enter", " send · "), ("Esc", " cancel")] };
                let parts: Vec<(&str, Style)> = keys.iter().flat_map(|(k, w)| [(*k, st.fg(ui.accent)), (*w, st)]).collect();
                spans(buf, inner.x, inner.y + 4, inner.right(), &parts);
            }
        }
        Overlay::BranchPicker { query, sel } => {
            let matches = app.picker_matches(query.text());
            let h = (matches.len() as u16).clamp(1, 12) + 5;
            let inner = boxed(app, buf, area, 56, h, "Compare with");
            let x = spans(buf, inner.x, inner.y, inner.right(), &[("> ", st.fg(ui.accent)), (query.text(), st)]);
            if x < inner.right() {
                buf[(x, inner.y)].set_style(st.add_modifier(Modifier::REVERSED));
            }
            let rows = inner.height.saturating_sub(3) as usize;
            if matches.is_empty() {
                text(buf, inner.x, inner.y + 2, inner.right(), "No matching branch", st.fg(ui.muted));
            }
            let first = sel.saturating_sub(rows.saturating_sub(1));
            for (k, (i, (name, _))) in matches.iter().enumerate().skip(first).take(rows).enumerate() {
                let y = inner.y + 2 + k as u16;
                let row = if i == *sel { st.bg(ui.selection).add_modifier(Modifier::BOLD) } else { st };
                fill(buf, Rect::new(inner.x, y, inner.width, 1), row);
                text(buf, inner.x + 1, y, inner.right(), name, row);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "type to filter · ↑/↓ · Enter compare · Esc cancel", st.fg(ui.muted));
            }
        }
        Overlay::Switcher { query, sel } => {
            let matches = app.switcher_matches(query.text());
            let h = (matches.len() as u16).clamp(1, 12) + 5;
            let inner = boxed(app, buf, area, 76, h, "Branches");
            let x = spans(buf, inner.x, inner.y, inner.right(), &[("> ", st.fg(ui.accent)), (query.text(), st)]);
            if x < inner.right() {
                buf[(x, inner.y)].set_style(st.add_modifier(Modifier::REVERSED));
            }
            let rows = inner.height.saturating_sub(3) as usize;
            if matches.is_empty() {
                text(buf, inner.x, inner.y + 2, inner.right(), "No matching branch", st.fg(ui.muted));
            }
            let first = sel.saturating_sub(rows.saturating_sub(1));
            for (k, (i, t)) in matches.iter().enumerate().skip(first).take(rows).enumerate() {
                let y = inner.y + 2 + k as u16;
                let row = if i == *sel { st.bg(ui.selection).add_modifier(Modifier::BOLD) } else { st };
                fill(buf, Rect::new(inner.x, y, inner.width, 1), row);
                let label = match t.kind {
                    gitty_core::refs::TargetKind::Current => format!("● {}", t.name),
                    gitty_core::refs::TargetKind::Local => format!("  {}", t.name),
                    gitty_core::refs::TargetKind::Remote => format!("  {}  (remote)", t.name),
                };
                text(buf, inner.x + 1, y, inner.right(), &label, row);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "Enter switch · ^N new · ^R rename · ^D delete · ^G merge · Esc", st.fg(ui.muted));
            }
        }
        Overlay::NameInput { kind, input } => {
            let title = match kind {
                crate::app::branches::NameKind::Create => "New branch".to_string(),
                crate::app::branches::NameKind::Rename { old } => format!("Rename {old}"),
                crate::app::branches::NameKind::Stash => "Stash changes".to_string(),
            };
            let inner = boxed(app, buf, area, 56, 6, &title);
            let x = spans(buf, inner.x, inner.y, inner.right(), &[("> ", st.fg(ui.accent)), (input.text(), st)]);
            if x < inner.right() {
                buf[(x, inner.y)].set_style(st.add_modifier(Modifier::REVERSED));
            }
            if inner.height > 2 {
                let hint = if matches!(kind, crate::app::branches::NameKind::Stash) { "Enter confirm (empty: default message) · Esc cancel" } else { "Enter confirm · Esc cancel" };
                text(buf, inner.x, inner.y + 2, inner.right(), hint, st.fg(ui.muted));
            }
        }
        Overlay::DirtySwitch { name, merge, .. } => {
            let (title, doing, verb, how) = if *merge {
                ("Merge branch", format!("Merging {name} into {}", app.refs.as_ref().and_then(|r| r.head_branch()).unwrap_or("HEAD")), "merge", "git merges around the changes, or refuses.")
            } else {
                ("Switch branch", format!("Switching to {name}"), "switch", "git carries the changes over, or refuses.")
            };
            let inner = boxed(app, buf, area, 72, 7, title);
            text(buf, inner.x, inner.y, inner.right(), "You have uncommitted changes", st.fg(ui.warning).add_modifier(Modifier::BOLD));
            text(buf, inner.x, inner.y + 1, inner.right(), &doing, st.fg(ui.muted));
            text(buf, inner.x, inner.y + 2, inner.right(), how, st.fg(ui.muted));
            if inner.height > 4 {
                spans(buf, inner.x, inner.y + 4, inner.right(), &[("s", st.fg(ui.accent)), (&format!(" stash and {verb} · "), st), ("w", st.fg(ui.accent)), (&format!(" {verb} anyway · "), st), ("Esc", st.fg(ui.accent)), (" cancel", st)]);
            }
        }
        Overlay::Stashes { sel } => {
            let h = (app.stashes.len() as u16).clamp(1, 12) + 5;
            let inner = boxed(app, buf, area, 76, h, "Stashes");
            if app.stashes.is_empty() {
                text(buf, inner.x, inner.y, inner.right(), "No stashes", st.fg(ui.muted));
            }
            let rows = inner.height.saturating_sub(2) as usize;
            let first = sel.saturating_sub(rows.saturating_sub(1));
            for (k, (i, s)) in app.stashes.iter().enumerate().skip(first).take(rows).enumerate() {
                let y = inner.y + k as u16;
                let row = if i == *sel { st.bg(ui.selection).add_modifier(Modifier::BOLD) } else { st };
                fill(buf, Rect::new(inner.x, y, inner.width, 1), row);
                let age = format_date(s.time, 0, app.now, DateMode::Relative);
                let age_x = text_right(buf, inner.x + 1, inner.right(), y, &age, row.fg(ui.muted));
                text(buf, inner.x + 1, y, age_x.saturating_sub(1), &format!("stash@{{{}}}  {}  ({})", s.index, s.message, s.branch), row);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "a apply · p pop · d drop · n new · Esc close", st.fg(ui.muted));
            }
        }
        Overlay::Quit { label } => {
            let inner = boxed(app, buf, area, 56, 6, "Quit");
            text(buf, inner.x, inner.y, inner.right(), &format!("{label} is still running"), st.fg(ui.warning).add_modifier(Modifier::BOLD));
            if inner.height > 3 {
                spans(buf, inner.x, inner.y + 2, inner.right(), &[("Quit and cancel it? ", st), ("y", st.fg(ui.accent)), (" / ", st), ("n", st.fg(ui.accent))]);
            }
        }
        Overlay::Diverged => {
            let inner = boxed(app, buf, area, 60, 6, "Pull");
            text(buf, inner.x, inner.y, inner.right(), "Your branch and its upstream have both moved on", st.fg(ui.warning).add_modifier(Modifier::BOLD));
            text(buf, inner.x, inner.y + 1, inner.right(), "Fast-forward is not possible.", st.fg(ui.muted));
            if inner.height > 3 {
                spans(buf, inner.x, inner.y + 3, inner.right(), &[("m", st.fg(ui.accent)), (" merge · ", st), ("r", st.fg(ui.accent)), (" rebase · ", st), ("Esc", st.fg(ui.accent)), (" cancel", st)]);
            }
        }
        Overlay::Log { title, body } => {
            let lines: Vec<&str> = body.lines().collect();
            let inner = boxed(app, buf, area, area.width.saturating_sub(8).min(100), (lines.len() as u16 + 4).min(area.height.saturating_sub(2)), title);
            let room = inner.height.saturating_sub(2) as usize;
            // the end of hook output says why it failed
            for (k, l) in lines.iter().skip(lines.len().saturating_sub(room)).enumerate() {
                text(buf, inner.x, inner.y + k as u16, inner.right(), l, st);
            }
            if inner.height > 0 {
                text(buf, inner.x, inner.bottom() - 1, inner.right(), "Esc close · the message is kept", st.fg(ui.muted));
            }
        }
        Overlay::ErrorDetail => {
            let Some(t) = &app.toast else { return };
            let lines: Vec<&str> = t.detail.lines().collect();
            let inner = boxed(app, buf, area, area.width.saturating_sub(8).min(100), lines.len() as u16 + 4, "Error");
            text(buf, inner.x, inner.y, inner.right(), &t.what, st.fg(ui.error).add_modifier(Modifier::BOLD));
            for (k, l) in lines.iter().enumerate().take(inner.height.saturating_sub(2) as usize) {
                text(buf, inner.x, inner.y + 2 + k as u16, inner.right(), l, st);
            }
        }
    }
}

/// Breaks `s` into pieces at most `w` display columns wide, at spaces when possible.
fn wrap_text(s: &str, w: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthStr;
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split(' ') {
        let mut word = word.to_string();
        loop {
            let need = if line.is_empty() { word.width() } else { line.width() + 1 + word.width() };
            if need <= w {
                if !line.is_empty() {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
                continue;
            }
            // a word longer than the line (a fingerprint on a narrow screen): hard break
            let cut = word.char_indices().scan(0, |acc, (i, c)| {
                *acc += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
                Some((i, *acc))
            }).find(|(_, acc)| *acc > w).map_or(word.len(), |(i, _)| i);
            out.push(word[..cut].to_string());
            word = word[cut..].to_string();
            if word.is_empty() {
                break;
            }
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}
