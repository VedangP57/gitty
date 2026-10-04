//! The diff pane: title, banners, then unified or split rows with full-width backgrounds,
//! dual line-number gutters, word emphasis and expandable gap rows.

use std::ops::Range;

use gitty_core::diff::FileDiff;
use gitty_core::diff::classify::{FileClass, LargeReason};
use gitty_core::diff::ops::WsMode;
use gitty_core::diff::view::{Row, SplitRow};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::commit_list::title;
use super::paint::{fill, glyphs, spans, text, width};
use crate::app::diffstate::VRow;
use crate::app::{App, Focus, digits};
use crate::text::{Glyph, layout};
use crate::theme::Theme;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ctx,
    Del,
    Add,
    Filler,
}

fn human(n: u64) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// One-line explanation for diffs that show no rows.
fn class_message(fd: &FileDiff, ws: WsMode) -> Option<String> {
    let short = |s: &Option<String>| s.as_deref().map_or("none".to_string(), |h| h.chars().take(7).collect());
    Some(match &fd.class {
        FileClass::Text if fd.changes.is_empty() => {
            if ws == WsMode::Show { "No content changes".into() } else { "No content changes (whitespace hidden)".into() }
        }
        FileClass::Text => return None,
        FileClass::Binary { old_size, new_size } => format!("Binary file changed ({} → {})", human(*old_size), human(*new_size)),
        FileClass::Lfs { old, new } => {
            let side = |p: &Option<gitty_core::diff::classify::LfsPointer>| {
                p.as_ref().map_or("none".to_string(), |p| format!("{} ({})", p.oid.chars().take(12).collect::<String>(), human(p.size)))
            };
            format!("Git LFS object {} → {}", side(old), side(new))
        }
        FileClass::Submodule { old, new } => format!("Submodule {} → {}", short(old), short(new)),
        FileClass::ModeOnly { old_mode, new_mode } => format!("Mode changed {old_mode:o} → {new_mode:o}"),
        FileClass::TooLarge { old_size, new_size } => format!("File too large to diff ({})", human((*old_size).max(*new_size))),
        FileClass::LargeText { reason } => {
            let why = match reason {
                LargeReason::Size(n) => human(*n),
                LargeReason::LongLine(n) => format!("a line of {n} characters"),
                LargeReason::ManyChanges(n) => format!("{n} changed lines"),
            };
            format!("Large diff hidden ({why}) — press Enter to show")
        }
        FileClass::Generated { reason } => format!("Generated file hidden ({reason}) — press Enter to show"),
    })
}

struct Styles {
    row: Style,
    gutter: Style,
    text: Style,
    emph: Style,
    marker: Style,
}

fn styles(t: &Theme, kind: Kind) -> Styles {
    let (d, ui) = (&t.diff, &t.ui);
    let mk = |bg: Color, gbg: Color, gfg: Color, fg: Color, ebg: Color| Styles {
        row: Style::new().bg(bg).fg(fg),
        gutter: Style::new().bg(gbg).fg(gfg),
        text: Style::new().bg(bg).fg(fg),
        emph: Style::new().bg(ebg).fg(fg),
        marker: Style::new().bg(bg).fg(gfg),
    };
    match kind {
        Kind::Ctx => mk(ui.bg, ui.bg, d.lineno, d.context_fg, ui.bg),
        Kind::Del => mk(d.del_bg, d.del_gutter, d.lineno_del, d.del_fg, d.del_emph),
        Kind::Add => mk(d.add_bg, d.add_gutter, d.lineno_add, d.add_fg, d.add_emph),
        Kind::Filler => mk(d.filler, d.filler, d.lineno, d.context_fg, d.filler),
    }
}

struct Line<'a> {
    numbers: [Option<u32>; 2],
    n_numbers: usize,
    kind: Kind,
    bytes: Option<&'a [u8]>,
    emph: &'a [Range<u32>],
    no_eol: bool,
}

struct Ctx<'a> {
    theme: &'a Theme,
    digits: u16,
    hscroll: u32,
    tab: u8,
    cursor_bg: Option<Color>,
    scratch: Vec<Glyph>,
}

impl Ctx<'_> {
    /// Draws one side (or the whole unified row) into `[x, max_x)`.
    fn line(&mut self, buf: &mut Buffer, x: u16, y: u16, max_x: u16, l: &Line, cursor: bool) {
        let st = styles(self.theme, l.kind);
        fill(buf, Rect::new(x, y, max_x.saturating_sub(x), 1), st.row);
        let mut gx = x;
        let gutter = if cursor { self.cursor_bg.map_or(st.gutter, |c| st.gutter.bg(c)) } else { st.gutter };
        for n in &l.numbers[..l.n_numbers] {
            let w = self.digits + 2;
            fill(buf, Rect::new(gx, y, w.min(max_x.saturating_sub(gx)), 1), gutter);
            if let Some(n) = n {
                let s = (n + 1).to_string();
                let pad = self.digits.saturating_sub(width(&s));
                text(buf, gx + 1 + pad, y, max_x.min(gx + 1 + self.digits), &s, gutter);
            }
            gx = gx.saturating_add(w);
        }
        let marker = match l.kind {
            Kind::Del => "-",
            Kind::Add => "+",
            _ => " ",
        };
        let tx = text(buf, gx, y, max_x, marker, st.marker).saturating_add(1);
        let Some(bytes) = l.bytes else { return };
        layout(bytes, self.tab, &mut self.scratch);
        let emph = l.emph;
        let (ts, es) = (st.text, st.emph);
        let ctrl = ts.fg(self.theme.ui.muted);
        let end = glyphs(buf, tx, y, max_x, self.hscroll, &self.scratch, |g| {
            if emph.iter().any(|r| r.contains(&g.byte)) {
                es
            } else if g.ctrl {
                ctrl
            } else {
                ts
            }
        });
        if l.no_eol && end > tx.saturating_sub(1) {
            text(buf, end, y, max_x, " ⊘", st.text.fg(self.theme.ui.muted));
        }
    }

    fn gap(&self, buf: &mut Buffer, x: u16, y: u16, max_x: u16, up: Option<(u16, bool)>, down: Option<(u16, bool)>, label: &str) {
        let d = &self.theme.diff;
        let st = Style::new().bg(d.expand_bg).fg(d.hunk_fg);
        fill(buf, Rect::new(x, y, max_x.saturating_sub(x), 1), st);
        let arrow = Style::new().bg(d.expand_bg).fg(d.expand_fg).add_modifier(Modifier::BOLD);
        for (pos, sym) in [(up, "↑"), (down, "↓")] {
            if let Some((gx, true)) = pos {
                text(buf, gx + 1 + self.digits.saturating_sub(1) / 2, y, max_x, sym, arrow);
            }
        }
        let tx = x + 2 * (self.digits + 2) + 2;
        text(buf, tx.min(max_x), y, max_x, label, st);
    }
}

fn emph_of(fd: &FileDiff, change: usize, line: u32, del: bool) -> &[Range<u32>] {
    let Some(h) = fd.intraline_ready(change) else { return &[] };
    let Some((o, n)) = fd.changes.get(change) else { return &[] };
    let (v, start) = if del { (&h.del_emph, o.start) } else { (&h.add_emph, n.start) };
    v.get((line - start) as usize).map_or(&[], |r| r.as_slice())
}

fn old_line(fd: &FileDiff, i: u32) -> Line<'_> {
    Line { numbers: [Some(i), None], n_numbers: 1, kind: Kind::Ctx, bytes: Some(fd.old.line(i)), emph: &[], no_eol: false }
}

pub fn draw(app: &mut App, buf: &mut Buffer, r: Rect) {
    let theme = app.theme.clone();
    let ui = &theme.ui;
    let base = Style::new().bg(ui.bg).fg(ui.fg);
    fill(buf, r, base);
    if r.height == 0 || r.width == 0 {
        return;
    }
    let focused = app.focus == Focus::Diff;
    let split = app.split_active();
    let loading = app.diff_loading();
    let wanted_path = app.current_file().map(|f| f.path.clone());
    if let Some(err) = app.wanted_diff_error().map(str::to_string) {
        title(app, buf, r, wanted_path.as_deref().unwrap_or("Diff"), focused, "");
        let first = err.lines().next().unwrap_or("").to_string();
        if r.height > 2 {
            text(buf, r.x + 2, r.y + 2, r.right(), "Could not load this diff", base.fg(ui.error).add_modifier(Modifier::BOLD));
        }
        if r.height > 3 {
            text(buf, r.x + 2, r.y + 3, r.right(), &first, base.fg(ui.muted));
        }
        app.hits.diff_rows = None;
        return;
    }
    let Some(d) = app.diff.as_ref() else {
        title(app, buf, r, wanted_path.as_deref().unwrap_or("Diff"), focused, if loading { "  loading…" } else { "" });
        app.hits.diff_rows = None;
        return;
    };
    let fd = d.diff.clone();
    let mut label = match &fd.old_path {
        Some(o) => format!("{o} → {}", fd.path),
        None => fd.path.clone(),
    };
    if loading {
        label = wanted_path.unwrap_or(label);
    }
    let mut extra = String::new();
    if !loading && fd.is_text() && !fd.changes.is_empty() {
        extra.push_str(&format!("  +{} −{}", fd.added, fd.removed));
    }
    if split {
        extra.push_str(" · split");
    }
    match app.ws {
        WsMode::Show => {}
        WsMode::IgnoreAll => extra.push_str(" · ws: ignore all"),
        WsMode::IgnoreAmount => extra.push_str(" · ws: ignore amount"),
    }
    if loading {
        extra.push_str("  loading…");
    }
    title(app, buf, r, &label, focused, &extra);
    let mut y = r.y + 1;
    let banners: Vec<_> = d.banners().into_iter().filter(|b| !(matches!(fd.class, FileClass::ModeOnly { .. }) && b.text.starts_with("Mode"))).collect();
    for b in &banners {
        if y >= r.bottom() {
            break;
        }
        let st = if b.warn { base.fg(ui.warning).add_modifier(Modifier::BOLD) } else { base.fg(ui.muted) };
        text(buf, r.x + 1, y, r.right(), &b.text, st);
        y += 1;
    }
    let body = Rect::new(r.x, y, r.width, r.bottom().saturating_sub(y));
    app.hits.diff_rows = Some(body);
    if body.height == 0 {
        return;
    }
    if let Some(msg) = class_message(&fd, app.ws) {
        let style = if fd.is_text() || matches!(fd.class, FileClass::LargeText { .. } | FileClass::Generated { .. }) { base.fg(ui.muted) } else { base.fg(ui.fg) };
        spans(buf, body.x + 2, body.y + body.height.min(2) / 2, body.right(), &[(&msg, style)]);
        app.hits.diff_first = 0;
        return;
    }
    let d = app.diff.as_mut().expect("checked above");
    let rows = d.rows(split);
    d.scroll = d.scroll.min(rows.saturating_sub(1));
    let d = app.diff.as_ref().expect("checked above");
    let digits = digits(fd.old.len().max(fd.new.len())) as u16;
    let mut cx = Ctx {
        theme: &theme,
        digits,
        hscroll: u32::from(d.hscroll),
        tab: app.config.tab_size,
        cursor_bg: focused.then_some(theme.diff.cursor),
        scratch: Vec::new(),
    };
    let (x, right) = (body.x, body.right());
    let mid = x + body.width.saturating_sub(1) / 2;
    if split {
        app.hits.diff_old_gutter = (x, digits + 2);
        app.hits.diff_new_gutter = (mid + 1, digits + 2);
    } else {
        app.hits.diff_old_gutter = (x, digits + 2);
        app.hits.diff_new_gutter = (x + digits + 2, digits + 2);
    }
    app.hits.diff_first = d.scroll;
    let hunk = Style::new().bg(theme.diff.hunk_bg).fg(theme.diff.hunk_fg);
    for k in 0..body.height {
        let vi = d.scroll + k as usize;
        let y = body.y + k;
        let cursor = vi == d.cursor;
        let Some(vr) = d.vrow(vi, split) else { break };
        let label_gap = |hidden: u32, header: &str| {
            let lines = if hidden == 1 { "line" } else { "lines" };
            if header.is_empty() { format!("⋯ {hidden} {lines}") } else { format!("⋯ {hidden} {lines}   {header}") }
        };
        match vr {
            VRow::Header(h) => {
                fill(buf, Rect::new(x, y, body.width, 1), hunk);
                text(buf, (x + 2 * (digits + 2) + 2).min(right), y, right, &h, hunk);
            }
            VRow::Row(row) => match row {
                Row::Gap { hidden, header, can_up, can_down, .. } => {
                    cx.gap(buf, x, y, right, Some((x, can_up)), Some((x + digits + 2, can_down)), &label_gap(hidden, &header));
                }
                Row::Context { old, new } => {
                    let l = Line { numbers: [Some(old), Some(new)], n_numbers: 2, kind: Kind::Ctx, bytes: Some(fd.new.line(new)), emph: &[], no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                }
                Row::Del { old, change } => {
                    let l = Line { numbers: [Some(old), None], n_numbers: 2, kind: Kind::Del, bytes: Some(fd.old.line(old)), emph: emph_of(&fd, change, old, true), no_eol: old + 1 == fd.old.len() && fd.old.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                }
                Row::Add { new, change } => {
                    let l = Line { numbers: [None, Some(new)], n_numbers: 2, kind: Kind::Add, bytes: Some(fd.new.line(new)), emph: emph_of(&fd, change, new, false), no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                }
            },
            VRow::Split(row) => {
                let divider = |buf: &mut Buffer| {
                    if mid < right {
                        buf[(mid, y)].set_symbol("│").set_style(base.fg(ui.border));
                    }
                };
                match row {
                    SplitRow::Gap { hidden, header, can_up, can_down, .. } => {
                        cx.gap(buf, x, y, right, Some((x, can_up)), Some((mid + 1, can_down)), &label_gap(hidden, &header));
                        divider(buf);
                    }
                    SplitRow::Context { old, new } => {
                        let l = Line { kind: Kind::Ctx, ..old_line(&fd, old) };
                        cx.line(buf, x, y, mid, &l, cursor);
                        let r = Line { numbers: [Some(new), None], n_numbers: 1, kind: Kind::Ctx, bytes: Some(fd.new.line(new)), emph: &[], no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                        cx.line(buf, mid + 1, y, right, &r, cursor);
                        divider(buf);
                    }
                    SplitRow::Change { old, new, change } => {
                        let left = match old {
                            Some(o) => Line { kind: Kind::Del, emph: emph_of(&fd, change, o, true), no_eol: o + 1 == fd.old.len() && fd.old.no_eol(), ..old_line(&fd, o) },
                            None => Line { numbers: [None, None], n_numbers: 1, kind: Kind::Filler, bytes: None, emph: &[], no_eol: false },
                        };
                        cx.line(buf, x, y, mid, &left, cursor);
                        let rt = match new {
                            Some(n) => Line { numbers: [Some(n), None], n_numbers: 1, kind: Kind::Add, bytes: Some(fd.new.line(n)), emph: emph_of(&fd, change, n, false), no_eol: n + 1 == fd.new.len() && fd.new.no_eol() },
                            None => Line { numbers: [None, None], n_numbers: 1, kind: Kind::Filler, bytes: None, emph: &[], no_eol: false },
                        };
                        cx.line(buf, mid + 1, y, right, &rt, cursor);
                        divider(buf);
                    }
                }
            }
        }
    }
}
