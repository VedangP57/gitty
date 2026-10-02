//! A blob split into lines, with per-line CR and file-level no-EOL flags.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EolStyle {
    None,
    Lf,
    Crlf,
    Mixed,
}

#[derive(Debug, Clone, Default)]
pub struct Text {
    bytes: Vec<u8>,
    starts: Vec<u32>,
}

impl Text {
    pub fn new(bytes: Vec<u8>) -> Text {
        let mut starts = Vec::new();
        if !bytes.is_empty() {
            starts.push(0);
            let last = bytes.len() - 1;
            for (i, &b) in bytes.iter().enumerate() {
                if b == b'\n' && i < last {
                    starts.push(i as u32 + 1);
                }
            }
        }
        Text { bytes, starts }
    }
    /// Number of lines. `""` → 0, `"a"` → 1, `"a\n"` → 1, `"a\nb"` → 2.
    pub fn len(&self) -> u32 {
        self.starts.len() as u32
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// The line exactly as stored, including its terminator.
    pub fn raw_line(&self, i: u32) -> &[u8] {
        let s = self.starts[i as usize] as usize;
        let e = self.starts.get(i as usize + 1).map(|&e| e as usize).unwrap_or(self.bytes.len());
        &self.bytes[s..e]
    }
    /// The line content without `\n` and without a final `\r`.
    pub fn line(&self, i: u32) -> &[u8] {
        let l = self.raw_line(i);
        let l = l.strip_suffix(b"\n").unwrap_or(l);
        l.strip_suffix(b"\r").unwrap_or(l)
    }
    pub fn has_cr(&self, i: u32) -> bool {
        let l = self.raw_line(i);
        let l = l.strip_suffix(b"\n").unwrap_or(l);
        l.ends_with(b"\r")
    }
    /// The last line lacks a `\n` terminator.
    pub fn no_eol(&self) -> bool {
        self.bytes.last().is_some_and(|&b| b != b'\n')
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn eol_style(&self) -> EolStyle {
        let (mut lf, mut crlf) = (0u32, 0u32);
        for i in 0..self.len() {
            let l = self.raw_line(i);
            if l.ends_with(b"\r\n") {
                crlf += 1;
            } else if l.ends_with(b"\n") {
                lf += 1;
            }
        }
        match (lf, crlf) {
            (0, 0) => EolStyle::None,
            (_, 0) => EolStyle::Lf,
            (0, _) => EolStyle::Crlf,
            _ => EolStyle::Mixed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_and_lines() {
        let t = Text::new(b"a\nbb\r\nc".to_vec());
        assert_eq!(t.len(), 3);
        assert_eq!(t.line(0), b"a");
        assert_eq!(t.line(1), b"bb");
        assert!(t.has_cr(1) && !t.has_cr(0));
        assert_eq!(t.line(2), b"c");
        assert!(t.no_eol());
        assert_eq!(t.raw_line(1), b"bb\r\n");
        assert_eq!(t.eol_style(), EolStyle::Mixed);
    }
    #[test]
    fn edge_cases() {
        assert_eq!(Text::new(vec![]).len(), 0);
        assert!(!Text::new(vec![]).no_eol());
        let t = Text::new(b"x\n".to_vec());
        assert_eq!((t.len(), t.no_eol(), t.eol_style()), (1, false, EolStyle::Lf));
        assert_eq!(Text::new(b"\n\n".to_vec()).len(), 2);
        assert_eq!(Text::new(b"\n\n".to_vec()).line(1), b"");
        assert_eq!(Text::new(b"a\r\nb\r\n".to_vec()).eol_style(), EolStyle::Crlf);
        assert_eq!(Text::new(b"abc".to_vec()).eol_style(), EolStyle::None);
        let cr_only_eof = Text::new(b"a\r".to_vec());
        assert_eq!(cr_only_eof.line(0), b"a");
        assert!(cr_only_eof.has_cr(0) && cr_only_eof.no_eol());
    }
}
