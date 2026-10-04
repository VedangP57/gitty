//! Turning untrusted line bytes into terminal cells: tab expansion, escaped control characters,
//! grapheme widths, and width-aware truncation.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One drawable unit: a grapheme (or an escaped byte) at display column `col`, `width` columns
/// wide, that came from byte offset `byte` of the source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glyph {
    pub byte: u32,
    pub col: u32,
    pub width: u8,
    /// An escaped control character, drawn muted.
    pub ctrl: bool,
    sym: Sym,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Sym {
    Byte(u8),
    Inline([u8; 15], u8),
    Heap(Box<str>),
}

impl Glyph {
    pub fn sym(&self) -> &str {
        match &self.sym {
            // only ASCII bytes are stored as `Byte`
            Sym::Byte(b) => std::str::from_utf8(std::slice::from_ref(b)).unwrap_or(" "),
            Sym::Inline(buf, n) => std::str::from_utf8(&buf[..*n as usize]).unwrap_or(" "),
            Sym::Heap(s) => s,
        }
    }
}

fn sym_of(s: &str) -> Sym {
    if s.len() == 1 {
        Sym::Byte(s.as_bytes()[0])
    } else if s.len() <= 15 {
        let mut buf = [0u8; 15];
        buf[..s.len()].copy_from_slice(s.as_bytes());
        Sym::Inline(buf, s.len() as u8)
    } else {
        Sym::Heap(s.into())
    }
}

/// Lays out `line` into `out` (cleared first). Tabs expand to the next multiple of `tab`.
pub fn layout(line: &[u8], tab: u8, out: &mut Vec<Glyph>) {
    layout_until(line, tab, u32::MAX, out);
}

/// [`layout`], stopping once glyphs reach display column `max_col` (so drawing the visible part
/// of a very long line costs only the visible part).
pub fn layout_until(line: &[u8], tab: u8, max_col: u32, out: &mut Vec<Glyph>) {
    out.clear();
    let tab = tab.max(1) as u32;
    let ascii = &line[..line.len().min(max_col as usize)];
    // a non-ASCII byte right after the cut could be a combining mark joining the last glyph
    if ascii.iter().all(|&b| (0x20..0x7f).contains(&b)) && line.get(ascii.len()).is_none_or(u8::is_ascii) {
        out.extend(ascii.iter().enumerate().map(|(i, &b)| Glyph {
            byte: i as u32,
            col: i as u32,
            width: 1,
            ctrl: false,
            sym: Sym::Byte(b),
        }));
        return;
    }
    let mut col = 0u32;
    let mut base = 0usize;
    for chunk in line.utf8_chunks() {
        let valid = chunk.valid();
        for (i, g) in valid.grapheme_indices(true) {
            if col >= max_col {
                return;
            }
            let byte = (base + i) as u32;
            // a cluster holding control bytes (e.g. "\r\n" is one grapheme) is escaped char by char
            if g.len() > 1 && g.bytes().any(|b| b < 0x20 || b == 0x7f) {
                for (ci, c) in g.char_indices() {
                    let byte = (base + i + ci) as u32;
                    let (sym, width, ctrl) = if (c as u32) < 0x20 || c == '\x7f' {
                        (sym_of(&format!("^{}", ((c as u8) ^ 0x40) as char)), 2, true)
                    } else {
                        let mut b = [0u8; 4];
                        let s: &str = c.encode_utf8(&mut b);
                        match UnicodeWidthStr::width(s) as u32 {
                            0 => continue,
                            w => (sym_of(s), w, false),
                        }
                    };
                    out.push(Glyph { byte, col, width: width as u8, ctrl, sym });
                    col += width;
                }
                continue;
            }
            let first = g.as_bytes()[0];
            let (sym, width, ctrl) = if g == "\t" {
                (Sym::Byte(b' '), tab - col % tab, false)
            } else if g.len() == 1 && (first < 0x20 || first == 0x7f) {
                (sym_of(&format!("^{}", (first ^ 0x40) as char)), 2, true)
            } else if let Some(c) = g.chars().next().filter(|c| ('\u{80}'..='\u{9f}').contains(c)) {
                (sym_of(&format!("<{:02x}>", c as u32)), 4, true)
            } else {
                let w = UnicodeWidthStr::width(g) as u32;
                if w == 0 {
                    continue;
                }
                (sym_of(g), w, false)
            };
            out.push(Glyph { byte, col, width: width.min(255) as u8, ctrl, sym });
            col += width;
        }
        base += valid.len();
        if !chunk.invalid().is_empty() {
            out.push(Glyph { byte: base as u32, col, width: 1, ctrl: false, sym: sym_of("\u{fffd}") });
            col += 1;
            base += chunk.invalid().len();
        }
    }
}

/// Splits laid-out glyphs into screen lines of `width` columns, breaking only between glyphs.
/// `out` gets the glyph index each line starts at; there is always at least one line.
pub fn wrap_starts(gs: &[Glyph], width: u32, out: &mut Vec<usize>) {
    out.clear();
    out.push(0);
    let width = width.max(1);
    let mut line_col = 0;
    for (i, g) in gs.iter().enumerate() {
        if i > 0 && g.col + u32::from(g.width) - line_col > width {
            out.push(i);
            line_col = g.col;
        }
    }
}

/// Display width of `s` in terminal columns.
pub fn display_width(s: &str) -> usize {
    s.graphemes(true).map(UnicodeWidthStr::width).sum()
}

/// The longest prefix of `s` that fits in `max` columns.
fn head(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true) {
        let gw = UnicodeWidthStr::width(g);
        if w + gw > max {
            return &s[..i];
        }
        w += gw;
    }
    s
}

/// The longest suffix of `s` that fits in `max` columns.
fn tail(s: &str, max: usize) -> &str {
    let mut w = 0;
    for (i, g) in s.grapheme_indices(true).rev() {
        let gw = UnicodeWidthStr::width(g);
        if w + gw > max {
            return &s[i + g.len()..];
        }
        w += gw;
    }
    s
}

/// `s` cut to `max` columns with a trailing `…` when it does not fit.
pub fn truncate_end(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}…", head(s, max - 1))
}

/// Desktop-style path truncation: keep the leading directories that fit and the whole file
/// name, eliding the middle (`src/…/file.rs`). A file name that alone is too long keeps its tail.
pub fn truncate_middle(path: &str, max: usize) -> String {
    if display_width(path) <= max {
        return path.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let (dir, file) = match path.rfind('/') {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let fw = display_width(file);
    if !dir.is_empty() && fw + 2 <= max {
        let budget = max - fw - 2;
        let mut keep = 0;
        for (i, _) in dir.match_indices('/') {
            if display_width(&dir[..=i]) <= budget {
                keep = i + 1;
            } else {
                break;
            }
        }
        return format!("{}…/{}", &dir[..keep], file);
    }
    format!("…{}", tail(file, max - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lay(s: &[u8]) -> Vec<Glyph> {
        let mut v = Vec::new();
        layout(s, 4, &mut v);
        v
    }
    fn syms(g: &[Glyph]) -> Vec<String> {
        g.iter().map(|g| g.sym().to_string()).collect()
    }

    #[test]
    fn ascii_fast_path() {
        let g = lay(b"ab");
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].col, g[1].col, g[1].width), (0, 1, 1));
        assert_eq!(syms(&g), ["a", "b"]);
    }

    #[test]
    fn tab_expands_to_next_stop() {
        let g = lay(b"a\tb");
        let b = g.iter().find(|g| g.sym() == "b").unwrap();
        assert_eq!(b.col, 4);
        assert_eq!(b.byte, 2);
        let total: u32 = g.iter().map(|g| g.width as u32).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn control_chars_are_caret() {
        let g = lay(b"\x1b[31m");
        assert_eq!(g[0].sym(), "^[");
        assert_eq!(g[0].width, 2);
        assert!(g[0].ctrl);
        assert!(g.iter().all(|g| !g.sym().contains('\x1b')));
        let g = lay(b"\x00\x7f");
        assert_eq!(syms(&g), ["^@", "^?"]);
    }

    #[test]
    fn crlf_grapheme_is_escaped() {
        assert_eq!(syms(&lay(b"a\r\nb")), ["a", "^M", "^J", "b"]);
        assert_eq!(syms(&lay("x\u{1b}\u{301}y".as_bytes())).concat(), "x^[y");
        for g in lay("a\r\nb\x07".as_bytes()) {
            assert!(!g.sym().chars().any(char::is_control), "{:?}", g.sym());
        }
    }

    #[test]
    fn c1_controls_escaped() {
        let g = lay("\u{9b}x".as_bytes());
        assert_eq!(g[0].sym(), "<9b>");
        assert_eq!(g[0].width, 4);
        assert_eq!(g[1].byte, 2);
    }

    #[test]
    fn invalid_utf8_is_replacement() {
        let g = lay(b"a\xffb");
        assert_eq!(syms(&g), ["a", "\u{fffd}", "b"]);
        assert_eq!(g[2].byte, 2);
        assert_eq!(g[1].width, 1);
    }

    #[test]
    fn wide_cjk_width_2() {
        let g = lay("日本".as_bytes());
        assert_eq!((g[0].col, g[1].col, g[0].width), (0, 2, 2));
        assert_eq!(g[1].byte, 3);
    }

    #[test]
    fn emoji_zwj_is_one_grapheme() {
        let g = lay("👩‍💻x".as_bytes());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].width, 2);
        assert_eq!(g[1].col, 2);
    }

    #[test]
    fn zero_width_leading_combining_is_skipped_not_panicking() {
        let g = lay("\u{301}a".as_bytes());
        assert!(g.iter().all(|g| g.width > 0));
        assert_eq!(g.last().unwrap().sym(), "a");
    }

    #[test]
    fn byte_offsets_track_source() {
        let src = "é\tz".as_bytes();
        let g = lay(src);
        for gl in &g {
            if !gl.ctrl && gl.sym() != " " {
                assert!(src[gl.byte as usize..].starts_with(gl.sym().as_bytes()));
            }
        }
    }

    #[test]
    fn display_width_counts_graphemes() {
        assert_eq!(display_width("ab日"), 4);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn truncate_middle_keeps_filename() {
        assert_eq!(truncate_middle("src/very/long/dir/file.rs", 16), "src/…/file.rs");
        assert_eq!(truncate_middle("src/a.rs", 16), "src/a.rs");
    }

    #[test]
    fn truncate_middle_long_filename_keeps_tail() {
        let t = truncate_middle("dir/averyveryverylongfilename.rs", 10);
        assert_eq!(display_width(&t), 10);
        assert!(t.starts_with('…') && t.ends_with("name.rs"));
    }

    #[test]
    fn truncate_middle_tiny_width() {
        for w in 0..6 {
            let t = truncate_middle("abc/defghij/klm.rs", w);
            assert!(display_width(&t) <= w, "{w}: {t}");
        }
    }

    #[test]
    fn truncate_end_wide_chars() {
        assert_eq!(truncate_end("hello", 10), "hello");
        assert_eq!(truncate_end("hello world", 6), "hello…");
        for w in 0..8 {
            let t = truncate_end("日本語のテキスト", w);
            assert!(display_width(&t) <= w, "{w}: {t}");
        }
    }

    #[test]
    fn layout_until_stops_at_the_column() {
        let mut v = Vec::new();
        layout_until(&[b'a'; 100_000], 4, 200, &mut v);
        assert_eq!(v.len(), 200);
        let s = "é".repeat(50_000);
        layout_until(s.as_bytes(), 4, 200, &mut v);
        assert_eq!(v.len(), 200);
        let mut full = Vec::new();
        layout(s.as_bytes(), 4, &mut full);
        assert_eq!(&full[..200], &v[..]);
    }

    #[test]
    fn wrap_starts_break_at_glyph_boundaries() {
        let mut gs = Vec::new();
        let mut starts = Vec::new();
        layout("ab世cd".as_bytes(), 4, &mut gs);
        wrap_starts(&gs, 3, &mut starts);
        assert_eq!(starts, vec![0, 2, 4], "the wide glyph moves to the next line whole");
        layout(b"", 4, &mut gs);
        wrap_starts(&gs, 3, &mut starts);
        assert_eq!(starts, vec![0]);
        layout(b"abcdef", 4, &mut gs);
        wrap_starts(&gs, 3, &mut starts);
        assert_eq!(starts, vec![0, 3]);
    }
}
