//! Low-level cell writers. All clip to `[x, max_x)` and never index outside the buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;

use crate::text::{Glyph, display_width, layout};

/// Paints `rect` with blanks in `style`.
pub fn fill(buf: &mut Buffer, rect: Rect, style: Style) {
    let r = rect.intersection(buf.area);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}

/// Sets only the style (keeps symbols) over `rect`.
pub fn restyle(buf: &mut Buffer, rect: Rect, style: Style) {
    let r = rect.intersection(buf.area);
    buf.set_style(r, style);
}

fn limit(buf: &Buffer, y: u16, max_x: u16) -> Option<u16> {
    (y < buf.area.bottom()).then(|| max_x.min(buf.area.right()))
}

/// Draws `glyphs` with their first `skip` display columns scrolled off, starting at screen `x`.
/// Returns the x after the last cell written.
pub fn glyphs(buf: &mut Buffer, x: u16, y: u16, max_x: u16, skip: u32, gs: &[Glyph], mut style_of: impl FnMut(&Glyph) -> Style) -> u16 {
    let Some(max_x) = limit(buf, y, max_x) else { return x };
    let mut end = x;
    for g in gs {
        let (c0, c1) = (g.col, g.col + u32::from(g.width));
        if c1 <= skip {
            continue;
        }
        let style = style_of(g);
        let sx0 = u32::from(x) + c0.saturating_sub(skip);
        if sx0 >= u32::from(max_x) {
            break;
        }
        let sx1 = u32::from(x) + c1 - skip;
        if c0 < skip || sx1 > u32::from(max_x) {
            // partially visible: blanks in the glyph's style
            for sx in sx0..sx1.min(u32::from(max_x)) {
                buf[(sx as u16, y)].set_symbol(" ").set_style(style);
            }
            end = sx1.min(u32::from(max_x)) as u16;
            if sx1 > u32::from(max_x) {
                break;
            }
            continue;
        }
        let sx0 = sx0 as u16;
        buf[(sx0, y)].set_symbol(g.sym()).set_style(style);
        for k in 1..u16::from(g.width) {
            let cell = &mut buf[(sx0 + k, y)];
            cell.reset();
            cell.set_style(style);
        }
        end = sx1 as u16;
    }
    end
}

/// Writes UI text (sanitised: control characters become caret notation).
pub fn text(buf: &mut Buffer, x: u16, y: u16, max_x: u16, s: &str, style: Style) -> u16 {
    let mut gs = Vec::new();
    layout(s.as_bytes(), 4, &mut gs);
    glyphs(buf, x, y, max_x, 0, &gs, |_| style)
}

/// Writes `s` so that it ends at `right` (exclusive); returns its start x (or `right` if empty).
pub fn text_right(buf: &mut Buffer, min_x: u16, right: u16, y: u16, s: &str, style: Style) -> u16 {
    let w = display_width(s) as u16;
    let x = right.saturating_sub(w).max(min_x);
    text(buf, x, y, right, s, style);
    x
}

/// Text spans in sequence; returns the end x.
pub fn spans(buf: &mut Buffer, mut x: u16, y: u16, max_x: u16, parts: &[(&str, Style)]) -> u16 {
    for (s, st) in parts {
        x = text(buf, x, y, max_x, s, *st);
    }
    x
}

pub fn width(s: &str) -> u16 {
    display_width(s).min(u16::MAX as usize) as u16
}

/// Centered single line inside `r`.
pub fn centered(buf: &mut Buffer, r: Rect, y: u16, s: &str, style: Style) {
    let w = width(s).min(r.width);
    let x = r.x + (r.width - w) / 2;
    text(buf, x, y, r.right(), s, style);
}
