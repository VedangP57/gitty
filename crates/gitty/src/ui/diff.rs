//! The diff pane: title, banners, then unified or split rows with full-width backgrounds,
//! dual line-number gutters, word emphasis and expandable gap rows.

use std::ops::Range;

use gitty_core::diff::FileDiff;
use gitty_core::diff::classify::{FileClass, LargeReason};
use gitty_core::diff::ops::WsMode;
use gitty_core::diff::view::{Row, SplitRow};
use gitty_highlight::{CAPTURES, Highlights, Span};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::commit_list::title;
use super::paint::{fill, glyphs, spans, text, width};
use crate::app::diffstate::VRow;
use crate::app::{App, Focus, Tab, digits};
use crate::text::{Glyph, layout, layout_until, wrap_starts};
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
        FileClass::Submodule { old, new } => format!("Submodule {}: {}..{}", fd.path, short(old), short(new)),
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
    /// Syntax spans for `bytes`.
    syn: &'a [Span],
    no_eol: bool,
}

/// ✓ in the first gutter column of a line whose change is staged (Changes tab).
fn check(buf: &mut Buffer, t: &Theme, gx: u16, y: u16, max_x: u16) {
    if gx < max_x {
        buf[(gx, y)].set_symbol("✓").set_style(Style::new().fg(t.ui.accent).add_modifier(Modifier::BOLD));
    }
}

struct Ctx<'a> {
    theme: &'a Theme,
    digits: u16,
    hscroll: u32,
    tab: u8,
    cursor_bg: Option<Color>,
    /// Theme style per capture id, background removed (the diff's background always shows).
    syntax: Vec<Option<Style>>,
    /// Wrap long lines (`W`) instead of scrolling horizontally.
    wrap: bool,
    /// Screen lines the row being drawn takes.
    lines: u16,
    scratch: Vec<Glyph>,
    starts: Vec<usize>,
}

impl Ctx<'_> {
    /// Draws one side (or the whole unified row) into `[x, max_x)`.
    fn line(&mut self, buf: &mut Buffer, x: u16, y: u16, max_x: u16, l: &Line, cursor: bool) {
        let st = styles(self.theme, l.kind);
        fill(buf, Rect::new(x, y, max_x.saturating_sub(x), self.lines), st.row);
        let mut gx = x;
        let gutter = if cursor { self.cursor_bg.map_or(st.gutter, |c| st.gutter.bg(c)) } else { st.gutter };
        for n in &l.numbers[..l.n_numbers] {
            let w = self.digits + 2;
            fill(buf, Rect::new(gx, y, w.min(max_x.saturating_sub(gx)), self.lines), gutter);
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
        let width = u32::from(max_x.saturating_sub(tx));
        if self.wrap {
            layout(bytes, self.tab, &mut self.scratch);
            wrap_starts(&self.scratch, width, &mut self.starts);
        } else {
            layout_until(bytes, self.tab, self.hscroll + width + 1, &mut self.scratch);
            self.starts.clear();
            self.starts.push(0);
        }
        let (emph, syn, syntax) = (l.emph, l.syn, &self.syntax);
        let (ts, es) = (st.text, st.emph);
        let ctrl = ts.fg(self.theme.ui.muted);
        let mut style_of = |g: &Glyph| {
            let base = if emph.iter().any(|r| r.contains(&g.byte)) {
                es
            } else if g.ctrl {
                return ctrl;
            } else {
                ts
            };
            let i = syn.partition_point(|s| s.end <= g.byte);
            match syn.get(i).filter(|s| s.start <= g.byte).and_then(|s| syntax.get(s.cap as usize).copied().flatten()) {
                Some(cap) => base.patch(cap),
                None => base,
            }
        };
        let (gs, starts) = (&self.scratch, &self.starts);
        let mut end = tx;
        for (k, &s) in starts.iter().enumerate().take(usize::from(self.lines)) {
            let e = starts.get(k + 1).copied().unwrap_or(gs.len());
            // a wrapped line starts at its first glyph's column; unwrapped lines scroll
            let skip = if self.wrap { gs.get(s).map_or(0, |g| g.col) } else { self.hscroll };
            end = glyphs(buf, tx, y + k as u16, max_x, skip, &gs[s..e], &mut style_of);
        }
        let last = (starts.len() - 1) as u16;
        if l.no_eol && end > tx.saturating_sub(1) && last < self.lines {
            text(buf, end, y + last, max_x, " ⊘", st.text.fg(self.theme.ui.muted));
        }
    }

    /// `arrows`: where the ↑ and ↓ expanders sit, and whether each can expand.
    fn gap(&self, buf: &mut Buffer, x: u16, y: u16, max_x: u16, arrows: [Option<(u16, bool)>; 2], label: &str) {
        let d = &self.theme.diff;
        let st = Style::new().bg(d.expand_bg).fg(d.hunk_fg);
        fill(buf, Rect::new(x, y, max_x.saturating_sub(x), 1), st);
        let arrow = Style::new().bg(d.expand_bg).fg(d.expand_fg).add_modifier(Modifier::BOLD);
        for (pos, sym) in arrows.into_iter().zip(["↑", "↓"]) {
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

fn old_line<'a>(fd: &'a FileDiff, i: u32, syn: &'a [Span]) -> Line<'a> {
    Line { numbers: [Some(i), None], n_numbers: 1, kind: Kind::Ctx, bytes: Some(fd.old.line(i)), emph: &[], syn, no_eol: false }
}

fn syn_line(h: Option<&Highlights>, i: u32) -> &[Span] {
    h.map_or(&[], |h| h.line(i))
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
    if let Some(n) = app.changes_notice().filter(|_| y < r.bottom()) {
        text(buf, r.x + 1, y, r.right(), &n, base.fg(ui.warning));
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
        app.hits.diff_lines.clear();
        return;
    }
    let wrap = app.diff_wrap();
    let (old_hl, new_hl) = app.diff_highlights();
    let (old_hl, new_hl) = (old_hl.as_deref(), new_hl.as_deref());
    let d = app.diff.as_mut().expect("checked above");
    let rows = d.rows(split);
    d.scroll = d.scroll.min(rows.saturating_sub(1));
    let d = app.diff.as_ref().expect("checked above");
    let view = if app.tab == Tab::Changes { app.changes.current.as_ref() } else { None };
    let staged = |line: u32, old: bool| view.is_some_and(|v| v.is_staged(line, old));
    let range = view.and(app.changes.visual).map(|a| a.min(d.cursor)..=a.max(d.cursor));
    let digits = digits(fd.old.len().max(fd.new.len())) as u16;
    let mut cx = Ctx {
        theme: &theme,
        digits,
        hscroll: u32::from(d.hscroll),
        tab: app.config.tab_size,
        cursor_bg: focused.then_some(theme.diff.cursor),
        syntax: CAPTURES.iter().map(|c| theme.syntax.get(*c).map(|s| Style { bg: None, ..*s })).collect(),
        wrap: wrap.is_some(),
        lines: 1,
        scratch: Vec::new(),
        starts: Vec::new(),
    };
    let (x, right) = (body.x, body.right());
    let mid = x + body.width.saturating_sub(1) / 2;
    // an added or deleted file (always unified) has no line numbers on its other side
    let solo = fd.old.is_empty() || fd.new.is_empty();
    let numbers = if solo { 1 } else { 2 };
    if split {
        app.hits.diff_old_gutter = (x, digits + 2);
        app.hits.diff_new_gutter = (mid + 1, digits + 2);
    } else if solo {
        // one number column serves the one side; clicks on "either" gutter land in it
        app.hits.diff_old_gutter = (x, digits + 2);
        app.hits.diff_new_gutter = (x, digits + 2);
    } else {
        app.hits.diff_old_gutter = (x, digits + 2);
        app.hits.diff_new_gutter = (x + digits + 2, digits + 2);
    }
    let hunk = Style::new().bg(theme.diff.hunk_bg).fg(theme.diff.hunk_fg);
    let mut screen_rows = Vec::with_capacity(usize::from(body.height));
    let mut vi = d.scroll;
    while screen_rows.len() < usize::from(body.height) {
        let k = screen_rows.len() as u16;
        let y = body.y + k;
        let cursor = vi == d.cursor || range.as_ref().is_some_and(|r| r.contains(&vi));
        let Some(vr) = d.vrow(vi, split) else { break };
        cx.lines = (d.row_lines(vi, split, wrap) as u16).clamp(1, body.height - k);
        screen_rows.extend(std::iter::repeat_n(vi, usize::from(cx.lines)));
        vi += 1;
        let label_gap = |hidden: u32, header: &str| {
            let lines = if hidden == 1 { "line" } else { "lines" };
            if header.is_empty() { format!("⋯ {hidden} {lines}") } else { format!("⋯ {hidden} {lines}   {header}") }
        };
        match vr {
            VRow::Header(h) => {
                fill(buf, Rect::new(x, y, body.width, 1), hunk);
                text(buf, (x + numbers * (digits + 2) + 2).min(right), y, right, &h, hunk);
            }
            VRow::Row(row) => match row {
                Row::Gap { hidden, header, can_up, can_down, .. } => {
                    cx.gap(buf, x, y, right, [Some((x, can_up)), Some((x + digits + 2, can_down))], &label_gap(hidden, &header));
                }
                Row::Context { old, new } => {
                    let l = Line { numbers: [Some(old), Some(new)], n_numbers: 2, kind: Kind::Ctx, bytes: Some(fd.new.line(new)), emph: &[], syn: syn_line(new_hl, new), no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                }
                Row::Del { old, change } => {
                    let l = Line { numbers: [Some(old), None], n_numbers: numbers as usize, kind: Kind::Del, bytes: Some(fd.old.line(old)), emph: emph_of(&fd, change, old, true), syn: syn_line(old_hl, old), no_eol: old + 1 == fd.old.len() && fd.old.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                    if staged(old, true) {
                        check(buf, &theme, x, y, right);
                    }
                }
                Row::Add { new, change } => {
                    let numbers_of = if solo { [Some(new), None] } else { [None, Some(new)] };
                    let l = Line { numbers: numbers_of, n_numbers: numbers as usize, kind: Kind::Add, bytes: Some(fd.new.line(new)), emph: emph_of(&fd, change, new, false), syn: syn_line(new_hl, new), no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                    cx.line(buf, x, y, right, &l, cursor);
                    if staged(new, false) {
                        check(buf, &theme, x, y, right);
                    }
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
                        cx.gap(buf, x, y, right, [Some((x, can_up)), Some((mid + 1, can_down))], &label_gap(hidden, &header));
                        divider(buf);
                    }
                    SplitRow::Context { old, new } => {
                        // without old-side spans, the new side's fit when the line is identical
                        let syn = match old_hl {
                            Some(h) => h.line(old),
                            None if fd.old.line(old) == fd.new.line(new) => syn_line(new_hl, new),
                            None => &[],
                        };
                        let l = old_line(&fd, old, syn);
                        cx.line(buf, x, y, mid, &l, cursor);
                        let r = Line { numbers: [Some(new), None], n_numbers: 1, kind: Kind::Ctx, bytes: Some(fd.new.line(new)), emph: &[], syn: syn_line(new_hl, new), no_eol: new + 1 == fd.new.len() && fd.new.no_eol() };
                        cx.line(buf, mid + 1, y, right, &r, cursor);
                        divider(buf);
                    }
                    SplitRow::Change { old, new, change } => {
                        let left = match old {
                            Some(o) => Line { kind: Kind::Del, emph: emph_of(&fd, change, o, true), no_eol: o + 1 == fd.old.len() && fd.old.no_eol(), ..old_line(&fd, o, syn_line(old_hl, o)) },
                            None => Line { numbers: [None, None], n_numbers: 1, kind: Kind::Filler, bytes: None, emph: &[], syn: &[], no_eol: false },
                        };
                        cx.line(buf, x, y, mid, &left, cursor);
                        if old.is_some_and(|o| staged(o, true)) {
                            check(buf, &theme, x, y, mid);
                        }
                        let rt = match new {
                            Some(n) => Line { numbers: [Some(n), None], n_numbers: 1, kind: Kind::Add, bytes: Some(fd.new.line(n)), emph: emph_of(&fd, change, n, false), syn: syn_line(new_hl, n), no_eol: n + 1 == fd.new.len() && fd.new.no_eol() },
                            None => Line { numbers: [None, None], n_numbers: 1, kind: Kind::Filler, bytes: None, emph: &[], syn: &[], no_eol: false },
                        };
                        cx.line(buf, mid + 1, y, right, &rt, cursor);
                        if new.is_some_and(|n| staged(n, false)) {
                            check(buf, &theme, mid + 1, y, right);
                        }
                        divider(buf);
                    }
                }
            }
        }
    }
    // Changes: a hunk header's first column is a handle that stages the hunk
    if app.tab == crate::app::Tab::Changes && app.changes.current.as_ref().is_some_and(|v| v.staged.is_some()) {
        let split = app.split_active();
        if let Some(d) = &app.diff {
            for (k, &i) in screen_rows.iter().enumerate() {
                let first = k == 0 || screen_rows[k - 1] != i;
                let is_edge = |j: usize| matches!(d.vrow(j, split), None | Some(VRow::Header(_) | VRow::Row(Row::Gap { .. }) | VRow::Split(SplitRow::Gap { .. })));
                // only headers with a hunk below (not the trailing gap)
                if first && is_edge(i) && !is_edge(i + 1) {
                    buf[(body.x, body.y + k as u16)].set_symbol("±").set_style(Style::new().fg(ui.accent).add_modifier(Modifier::BOLD));
                }
            }
        }
    }
    app.hits.diff_lines = screen_rows;
}
