//! What kind of diff a file gets: text, or a one-line card (binary, LFS, submodule, ...).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsPointer {
    pub oid: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LargeReason {
    Size(u64),
    LongLine(u32),
    ManyChanges(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileClass {
    Text,
    Binary { old_size: u64, new_size: u64 },
    Lfs { old: Option<LfsPointer>, new: Option<LfsPointer> },
    Submodule { old: Option<String>, new: Option<String> },
    ModeOnly { old_mode: u32, new_mode: u32 },
    TooLarge { old_size: u64, new_size: u64 },
    LargeText { reason: LargeReason },
    Generated { reason: &'static str },
}

pub struct ClassifyInput<'a> {
    pub path: &'a str,
    pub old: &'a [u8],
    pub new: &'a [u8],
    pub old_mode: u32,
    pub new_mode: u32,
    pub same_content: bool,
}

pub const MODE_SUBMODULE: u32 = 0o160000;
pub const TOO_LARGE: u64 = 64 * 1024 * 1024;
pub const LARGE_TEXT: u64 = 4 * 1024 * 1024;
pub const LONG_LINE: u32 = 5000;
pub const MANY_CHANGES: u32 = 20_000;
const BINARY_SNIFF: usize = 8000;
const LFS_HEADER: &[u8] = b"version https://git-lfs.github.com/spec/v1";

const LOCKFILES: &[&str] = &[
    "package-lock.json", "yarn.lock", "pnpm-lock.yaml", "bun.lock", "bun.lockb", "Cargo.lock", "Gemfile.lock",
    "poetry.lock", "composer.lock", "go.sum", "Podfile.lock", "flake.lock", "uv.lock",
];

/// Classification that needs only the raw contents (everything except `ManyChanges`).
pub fn classify_pre(inp: &ClassifyInput) -> FileClass {
    if inp.old_mode == MODE_SUBMODULE || inp.new_mode == MODE_SUBMODULE {
        let hex = |b: &[u8], m: u32| (m == MODE_SUBMODULE && !b.is_empty()).then(|| String::from_utf8_lossy(b).into_owned());
        return FileClass::Submodule { old: hex(inp.old, inp.old_mode), new: hex(inp.new, inp.new_mode) };
    }
    if inp.same_content && inp.old_mode != inp.new_mode {
        return FileClass::ModeOnly { old_mode: inp.old_mode, new_mode: inp.new_mode };
    }
    let (lo, ln) = (parse_lfs(inp.old), parse_lfs(inp.new));
    if lo.is_some() || ln.is_some() {
        return FileClass::Lfs { old: lo, new: ln };
    }
    let (os, ns) = (inp.old.len() as u64, inp.new.len() as u64);
    if is_binary(inp.old) || is_binary(inp.new) {
        return FileClass::Binary { old_size: os, new_size: ns };
    }
    if os > TOO_LARGE || ns > TOO_LARGE {
        return FileClass::TooLarge { old_size: os, new_size: ns };
    }
    if os > LARGE_TEXT || ns > LARGE_TEXT {
        return FileClass::LargeText { reason: LargeReason::Size(os.max(ns)) };
    }
    let longest = longest_line_chars(inp.old).max(longest_line_chars(inp.new));
    if longest > LONG_LINE {
        return FileClass::LargeText { reason: LargeReason::LongLine(longest) };
    }
    if let Some(reason) = is_generated(inp.path, if inp.new.is_empty() { inp.old } else { inp.new }) {
        return FileClass::Generated { reason };
    }
    FileClass::Text
}

/// Applies the post-diff rule: a text file with too many changed lines is collapsed.
pub fn classify_post(pre: FileClass, changed_lines: u32) -> FileClass {
    match pre {
        FileClass::Text if changed_lines > MANY_CHANGES => FileClass::LargeText { reason: LargeReason::ManyChanges(changed_lines) },
        other => other,
    }
}

pub fn is_binary(d: &[u8]) -> bool {
    d[..d.len().min(BINARY_SNIFF)].contains(&0)
}

pub fn parse_lfs(b: &[u8]) -> Option<LfsPointer> {
    if b.len() >= 1024 || !b.starts_with(LFS_HEADER) {
        return None;
    }
    let s = std::str::from_utf8(b).ok()?;
    let mut oid = None;
    let mut size = None;
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("oid ") {
            oid = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("size ") {
            size = v.trim().parse().ok();
        }
    }
    Some(LfsPointer { oid: oid?, size: size? })
}

/// Character count (UTF-8 continuation bytes excluded) of the longest line.
fn longest_line_chars(d: &[u8]) -> u32 {
    let mut best = 0u32;
    let mut cur = 0u32;
    for &b in d {
        if b == b'\n' {
            best = best.max(cur);
            cur = 0;
        } else if b & 0xC0 != 0x80 {
            cur += 1;
        }
    }
    best.max(cur)
}

/// Lockfiles, minified bundles, and files that declare themselves generated.
pub fn is_generated(path: &str, sample: &[u8]) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if LOCKFILES.contains(&name) {
        return Some("lockfile");
    }
    if name.ends_with(".min.js") || name.ends_with(".min.css") {
        return Some("minified");
    }
    let head = &sample[..sample.len().min(64 * 1024)];
    if name.ends_with(".js") || name.ends_with(".css") || name.ends_with(".mjs") {
        let lines = head.iter().filter(|&&b| b == b'\n').count().max(1);
        if head.len() / lines > 110 {
            return Some("minified");
        }
        let tail_start = sample.len().saturating_sub(512);
        let tail = String::from_utf8_lossy(&sample[tail_start..]);
        if tail.lines().rev().take(2).any(|l| l.contains("sourceMappingURL")) {
            return Some("bundled (source map)");
        }
    }
    let text = String::from_utf8_lossy(head);
    for line in text.lines().take(40) {
        if line.contains("@generated") || (line.contains("Code generated") && line.contains("DO NOT EDIT")) {
            return Some("marked generated");
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn inp<'a>(path: &'a str, old: &'a [u8], new: &'a [u8]) -> ClassifyInput<'a> {
        ClassifyInput { path, old, new, old_mode: 0o100644, new_mode: 0o100644, same_content: old == new }
    }
    #[test]
    fn order_and_kinds() {
        assert!(matches!(classify_pre(&inp("a.rs", b"x\n", b"y\n")), FileClass::Text));
        assert!(matches!(classify_pre(&inp("a.bin", b"\0\x01", b"x")), FileClass::Binary { old_size: 2, new_size: 1 }));
        let mut m = inp("s", b"x\n", b"x\n");
        m.new_mode = 0o100755;
        assert!(matches!(classify_pre(&m), FileClass::ModeOnly { old_mode: 0o100644, new_mode: 0o100755 }));
        let mut sm = inp("sub", b"abc", b"def");
        sm.old_mode = 0o160000;
        sm.new_mode = 0o160000;
        assert!(matches!(classify_pre(&sm), FileClass::Submodule { .. }));
        let lfs = b"version https://git-lfs.github.com/spec/v1\noid sha256:abcd\nsize 1234\n";
        match classify_pre(&inp("big.psd", b"", lfs)) {
            FileClass::Lfs { old: None, new: Some(p) } => assert_eq!((p.oid.as_str(), p.size), ("sha256:abcd", 1234)),
            other => panic!("{other:?}"),
        }
        let long = format!("{}\n", "x".repeat(6000));
        assert!(matches!(
            classify_pre(&inp("a.txt", b"", long.as_bytes())),
            FileClass::LargeText { reason: LargeReason::LongLine(6000) }
        ));
        assert!(matches!(classify_pre(&inp("Cargo.lock", b"a\n", b"b\n")), FileClass::Generated { .. }));
        assert!(matches!(classify_pre(&inp("web/app.min.js", b"a\n", b"b\n")), FileClass::Generated { .. }));
        assert!(matches!(
            classify_pre(&inp("gen.go", b"", b"// Code generated by x. DO NOT EDIT.\npackage a\n")),
            FileClass::Generated { .. }
        ));
        let minified = format!("{}\n", "var a=1;".repeat(30));
        assert!(matches!(classify_pre(&inp("bundle.js", b"", minified.as_bytes())), FileClass::Generated { .. }));
        assert!(matches!(classify_post(FileClass::Text, 20_001), FileClass::LargeText { reason: LargeReason::ManyChanges(20_001) }));
        assert!(matches!(classify_post(FileClass::Text, 5), FileClass::Text));
        assert!(matches!(classify_post(FileClass::Generated { reason: "x" }, 30_000), FileClass::Generated { .. }));
    }
    #[test]
    fn lfs_needs_header() {
        assert!(parse_lfs(b"oid sha256:abcd\nsize 3\n").is_none());
        assert!(parse_lfs(b"version https://git-lfs.github.com/spec/v1\noid sha256:ab\n").is_none());
    }
}
