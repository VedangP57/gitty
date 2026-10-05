//! A small text editor for the commit box: one line or many, grapheme-aware cursor moves.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Editable text with a byte cursor that always sits on a grapheme boundary. It may hold a
/// password, so Debug shows only the length and drop zeroes the bytes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Editor {
    text: String,
    cursor: usize,
    multiline: bool,
}

impl Editor {
    pub fn single() -> Self {
        Self::default()
    }

    pub fn multi() -> Self {
        Self { text: String::new(), cursor: 0, multiline: true }
    }

    pub fn reserve(&mut self, n: usize) {
        self.text.reserve(n);
    }

    /// Moves the text out without copying it, leaving the editor empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set(&mut self, s: &str) {
        self.text.clear();
        self.cursor = 0;
        self.insert(s);
    }

    /// Inserts typed or pasted text. CRLF becomes LF; a single-line editor turns line breaks into spaces.
    pub fn insert(&mut self, s: &str) {
        let s = s.replace("\r\n", "\n").replace('\r', "\n");
        let s = if self.multiline { s } else { s.replace('\n', " ") };
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    fn prev(&self) -> usize {
        self.text[..self.cursor].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.text[self.cursor..].graphemes(true).next().map_or(self.cursor, |g| self.cursor + g.len())
    }

    pub fn backspace(&mut self) {
        let p = self.prev();
        self.text.replace_range(p..self.cursor, "");
        self.cursor = p;
    }

    pub fn delete(&mut self) {
        let n = self.next();
        self.text.replace_range(self.cursor..n, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.prev();
    }

    pub fn right(&mut self) {
        self.cursor = self.next();
    }

    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map_or(self.text.len(), |i| self.cursor + i)
    }

    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    pub fn word_left(&mut self) {
        let before = &self.text[..self.cursor];
        let mut it = before.grapheme_indices(true).rev().peekable();
        let mut at = self.cursor;
        while let Some((i, _)) = it.next_if(|(_, g)| g.chars().all(char::is_whitespace)) {
            at = i;
        }
        while let Some((i, _)) = it.next_if(|(_, g)| !g.chars().all(char::is_whitespace)) {
            at = i;
        }
        self.cursor = at;
    }

    pub fn word_right(&mut self) {
        let mut it = self.text[self.cursor..].graphemes(true).peekable();
        let mut at = self.cursor;
        while let Some(g) = it.next_if(|g| g.chars().all(char::is_whitespace)) {
            at += g.len();
        }
        while let Some(g) = it.next_if(|g| !g.chars().all(char::is_whitespace)) {
            at += g.len();
        }
        self.cursor = at;
    }

    /// Byte offset of display column `col` on the line starting at `start` (clamped to its end).
    fn at_column(&self, start: usize, col: usize) -> usize {
        let line = self.text[start..].split('\n').next().unwrap_or("");
        let mut w = 0;
        for (i, g) in line.grapheme_indices(true) {
            if w >= col {
                return start + i;
            }
            w += g.width();
        }
        start + line.len()
    }

    /// Moves to the previous line; false at the first line (single-line: always false).
    pub fn up(&mut self) -> bool {
        let start = self.line_start();
        if start == 0 {
            return false;
        }
        let col = self.position().1;
        let prev = self.text[..start - 1].rfind('\n').map_or(0, |i| i + 1);
        self.cursor = self.at_column(prev, col);
        true
    }

    /// Moves to the next line; false at the last line.
    pub fn down(&mut self) -> bool {
        let end = self.line_end();
        if end == self.text.len() {
            return false;
        }
        let col = self.position().1;
        self.cursor = self.at_column(end + 1, col);
        true
    }

    /// (line, column in display cells) of the cursor.
    pub fn position(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.matches('\n').count();
        (line, before[self.line_start()..].width())
    }

    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.text.split('\n')
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Prompt answers pass through an editor: leave no copy behind in freed memory.
impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Editor {{ {} bytes, cursor {} }}", self.text.len(), self.cursor)
    }
}

/// Zeroes a string's bytes in place (pasted secrets, editor text on drop).
pub fn wipe(s: &mut str) {
    // SAFETY: zero bytes are valid UTF-8
    unsafe { s.as_bytes_mut() }.fill(0);
}

impl Drop for Editor {
    fn drop(&mut self) {
        wipe(&mut self.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wipe_zeroes_in_place() {
        let mut s = String::from("hunter2");
        let p = s.as_ptr();
        wipe(&mut s);
        assert_eq!(s.as_bytes(), [0u8; 7]);
        assert_eq!(s.as_ptr(), p, "same buffer, not a copy");
    }

    fn ed(s: &str) -> Editor {
        let mut e = Editor::multi();
        e.insert(s);
        e
    }

    #[test]
    fn inserts_and_deletes_at_the_cursor() {
        let mut e = Editor::single();
        e.insert("helo");
        e.left();
        e.insert("l");
        assert_eq!(e.text(), "hello");
        e.home();
        e.delete();
        assert_eq!(e.text(), "ello");
        e.end();
        e.backspace();
        assert_eq!(e.text(), "ell");
        assert_eq!(e.cursor(), 3);
        e.home();
        e.backspace();
        e.end();
        e.delete();
        assert_eq!(e.text(), "ell", "no-ops at the edges");
    }

    #[test]
    fn moves_by_grapheme_not_byte() {
        let mut e = Editor::single();
        e.insert("aé👍🏽b");
        e.left();
        e.left();
        e.backspace();
        assert_eq!(e.text(), "a👍🏽b");
        e.right();
        e.backspace();
        assert_eq!(e.text(), "ab");
        assert_eq!(e.position(), (0, 1));
    }

    #[test]
    fn single_line_flattens_pasted_newlines() {
        let mut e = Editor::single();
        e.insert("fix\r\nthe bug\n");
        assert_eq!(e.text(), "fix the bug ");
        assert!(!e.up());
        assert!(!e.down());
    }

    #[test]
    fn words_skip_spaces_then_a_word() {
        let mut e = Editor::single();
        e.insert("fix  the bug");
        e.word_left();
        assert_eq!(e.cursor(), 9);
        e.word_left();
        assert_eq!(e.cursor(), 5);
        e.word_left();
        assert_eq!(e.cursor(), 0);
        e.word_right();
        assert_eq!(e.cursor(), 3);
        e.word_right();
        assert_eq!(e.cursor(), 8);
    }

    #[test]
    fn lines_keep_the_column_and_home_end_stay_on_the_line() {
        let mut e = ed("first line\nab\nthird");
        assert_eq!(e.position(), (2, 5));
        assert!(e.up());
        assert_eq!(e.position(), (1, 2), "clamped to the shorter line");
        assert!(e.up());
        assert_eq!(e.position(), (0, 2));
        assert!(!e.up());
        e.end();
        assert_eq!(e.position(), (0, 10));
        e.home();
        assert_eq!(e.position(), (0, 0));
        assert!(e.down());
        e.end();
        e.insert("c");
        assert_eq!(e.text(), "first line\nabc\nthird");
        e.right();
        assert_eq!(e.position(), (2, 0), "right crosses the newline");
        e.backspace();
        assert_eq!(e.text(), "first line\nabcthird");
    }

    #[test]
    fn set_replaces_and_puts_the_cursor_at_the_end() {
        let mut e = Editor::multi();
        e.set("a\r\nb");
        assert_eq!(e.text(), "a\nb");
        assert_eq!(e.position(), (1, 1));
        let mut s = Editor::single();
        s.set("x\ny");
        assert_eq!(s.text(), "x y");
    }

    #[test]
    fn wide_characters_count_two_columns() {
        let e = ed("日本");
        assert_eq!(e.position(), (0, 4));
    }
}
