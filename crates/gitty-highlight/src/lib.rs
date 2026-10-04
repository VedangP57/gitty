//! Syntax highlighting for whole files: path + bytes in, per-line spans of capture ids out.
//!
//! Bundled tree-sitter grammars (cargo features) cover the common languages; everything else
//! falls back to syntect (onig) with two-face's syntax set. Spans carry capture ids from
//! [`CAPTURES`], never colours, so a theme switch needs no re-highlighting.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use syntect::parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent};

/// Theme `[syntax]` keys; a span's `cap` indexes this list.
pub const CAPTURES: [&str; 19] = [
    "keyword",
    "string",
    "comment",
    "function",
    "type",
    "constant",
    "number",
    "operator",
    "variable",
    "property",
    "punctuation",
    "attribute",
    "tag",
    "label",
    "module",
    "constructor",
    "macro",
    "escape",
    "embedded",
];

const KW: u8 = 0;
const STR: u8 = 1;
const COMMENT: u8 = 2;
const FUNC: u8 = 3;
const TYPE: u8 = 4;
const CONST: u8 = 5;
const NUM: u8 = 6;
const OP: u8 = 7;
const VAR: u8 = 8;
const PROP: u8 = 9;
const PUNCT: u8 = 10;
const ATTR: u8 = 11;
const TAG: u8 = 12;
const LABEL: u8 = 13;
const MODULE: u8 = 14;
const CTOR: u8 = 15;
const MACRO: u8 = 16;
const ESC: u8 = 17;
const EMBED: u8 = 18;

/// tree-sitter capture names we recognise (the longest dotted match wins) and their capture id.
const TS_NAMES: &[(&str, u8)] = &[
    ("keyword", KW),
    ("keyword.operator", KW),
    ("conditional", KW),
    ("repeat", KW),
    ("include", KW),
    ("import", KW),
    ("storageclass", KW),
    ("type.qualifier", KW),
    ("operator", OP),
    ("function", FUNC),
    ("function.macro", MACRO),
    ("type", TYPE),
    ("string", STR),
    ("string.escape", ESC),
    ("escape", ESC),
    ("character", STR),
    ("comment", COMMENT),
    ("number", NUM),
    ("float", NUM),
    ("constant", CONST),
    ("boolean", CONST),
    ("variable", VAR),
    ("variable.builtin", CONST),
    ("parameter", VAR),
    ("property", PROP),
    ("field", PROP),
    ("variable.member", PROP),
    ("punctuation", PUNCT),
    ("delimiter", PUNCT),
    ("attribute", ATTR),
    ("tag", TAG),
    ("label", LABEL),
    ("module", MODULE),
    ("namespace", MODULE),
    ("constructor", CTOR),
    ("embedded", EMBED),
];

/// Files above this get no syntax colour (diff colours only).
pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_LINES: usize = 50_000;
/// Lines longer than this get no syntax colour.
pub const MAX_LINE_BYTES: usize = 1000;
const BUDGET: Duration = Duration::from_secs(2);
/// syntect cannot be interrupted inside a line, so files with a line this long get no colour.
const MAX_SYNTECT_LINE: usize = 8 * MAX_LINE_BYTES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte range within the line.
    pub start: u32,
    pub end: u32,
    pub cap: u8,
}

/// Per-line spans of one file.
#[derive(Debug, Default, Clone)]
pub struct Highlights {
    spans: Vec<Span>,
    /// `spans[starts[i]..starts[i + 1]]` belong to line `i`.
    starts: Vec<u32>,
}

impl Highlights {
    pub fn lines(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }
    pub fn line(&self, i: u32) -> &[Span] {
        let i = i as usize;
        match (self.starts.get(i), self.starts.get(i + 1)) {
            (Some(&a), Some(&b)) => &self.spans[a as usize..b as usize],
            _ => &[],
        }
    }
    /// Approximate heap size, for cache bounds.
    pub fn bytes(&self) -> usize {
        self.spans.len() * std::mem::size_of::<Span>() + self.starts.len() * 4
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    TreeSitter,
    Syntect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ts {
    Rust,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Go,
    C,
    Cpp,
    Json,
    Yaml,
    Toml,
    Bash,
    Css,
    Html,
    Sql,
    Swift,
}

const TS_ALL: [Ts; 16] = [
    Ts::Rust,
    Ts::TypeScript,
    Ts::Tsx,
    Ts::JavaScript,
    Ts::Python,
    Ts::Go,
    Ts::C,
    Ts::Cpp,
    Ts::Json,
    Ts::Yaml,
    Ts::Toml,
    Ts::Bash,
    Ts::Css,
    Ts::Html,
    Ts::Sql,
    Ts::Swift,
];

impl Ts {
    fn name(self) -> &'static str {
        match self {
            Ts::Rust => "rust",
            Ts::TypeScript => "typescript",
            Ts::Tsx => "tsx",
            Ts::JavaScript => "javascript",
            Ts::Python => "python",
            Ts::Go => "go",
            Ts::C => "c",
            Ts::Cpp => "cpp",
            Ts::Json => "json",
            Ts::Yaml => "yaml",
            Ts::Toml => "toml",
            Ts::Bash => "bash",
            Ts::Css => "css",
            Ts::Html => "html",
            Ts::Sql => "sql",
            Ts::Swift => "swift",
        }
    }

    fn by_name(n: &str) -> Option<Ts> {
        let n = n.to_ascii_lowercase();
        let alias = match n.as_str() {
            "js" | "jsx" | "ecmascript" => "javascript",
            "ts" => "typescript",
            "sh" | "shell" | "zsh" => "bash",
            "c++" => "cpp",
            "py" => "python",
            "yml" => "yaml",
            other => other,
        };
        TS_ALL.into_iter().find(|t| t.name() == alias)
    }

    /// (language, highlights, injections, locals), or None when the grammar is not compiled in.
    #[allow(unreachable_code, unused_variables)]
    fn parts(self) -> Option<(tree_sitter::Language, String, &'static str, &'static str)> {
        Some(match self {
            #[cfg(feature = "rust")]
            Ts::Rust => (
                tree_sitter_rust::LANGUAGE.into(),
                tree_sitter_rust::HIGHLIGHTS_QUERY.into(),
                tree_sitter_rust::INJECTIONS_QUERY,
                "",
            ),
            #[cfg(feature = "typescript")]
            Ts::TypeScript => (
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            #[cfg(feature = "typescript")]
            Ts::Tsx => (
                tree_sitter_typescript::LANGUAGE_TSX.into(),
                format!(
                    "{}\n{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY,
                    tree_sitter_typescript::HIGHLIGHTS_QUERY
                ),
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_typescript::LOCALS_QUERY,
            ),
            #[cfg(feature = "javascript")]
            Ts::JavaScript => (
                tree_sitter_javascript::LANGUAGE.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_javascript::HIGHLIGHT_QUERY,
                    tree_sitter_javascript::JSX_HIGHLIGHT_QUERY
                ),
                tree_sitter_javascript::INJECTIONS_QUERY,
                tree_sitter_javascript::LOCALS_QUERY,
            ),
            #[cfg(feature = "python")]
            Ts::Python => (
                tree_sitter_python::LANGUAGE.into(),
                tree_sitter_python::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "go")]
            Ts::Go => (
                tree_sitter_go::LANGUAGE.into(),
                tree_sitter_go::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "c")]
            Ts::C => (
                tree_sitter_c::LANGUAGE.into(),
                tree_sitter_c::HIGHLIGHT_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "cpp")]
            Ts::Cpp => (
                tree_sitter_cpp::LANGUAGE.into(),
                format!(
                    "{}\n{}",
                    tree_sitter_cpp::HIGHLIGHT_QUERY,
                    tree_sitter_c::HIGHLIGHT_QUERY
                ),
                "",
                "",
            ),
            #[cfg(feature = "json")]
            Ts::Json => (
                tree_sitter_json::LANGUAGE.into(),
                tree_sitter_json::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "yaml")]
            Ts::Yaml => (
                tree_sitter_yaml::LANGUAGE.into(),
                tree_sitter_yaml::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "toml")]
            Ts::Toml => (
                tree_sitter_toml_ng::LANGUAGE.into(),
                tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "bash")]
            Ts::Bash => (
                tree_sitter_bash::LANGUAGE.into(),
                tree_sitter_bash::HIGHLIGHT_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "css")]
            Ts::Css => (
                tree_sitter_css::LANGUAGE.into(),
                tree_sitter_css::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "html")]
            Ts::Html => (
                tree_sitter_html::LANGUAGE.into(),
                tree_sitter_html::HIGHLIGHTS_QUERY.into(),
                tree_sitter_html::INJECTIONS_QUERY,
                "",
            ),
            #[cfg(feature = "sql")]
            Ts::Sql => (
                tree_sitter_sequel::LANGUAGE.into(),
                tree_sitter_sequel::HIGHLIGHTS_QUERY.into(),
                "",
                "",
            ),
            #[cfg(feature = "swift")]
            Ts::Swift => (
                tree_sitter_swift::LANGUAGE.into(),
                tree_sitter_swift::HIGHLIGHTS_QUERY.into(),
                tree_sitter_swift::INJECTIONS_QUERY,
                tree_sitter_swift::LOCALS_QUERY,
            ),
            #[allow(unreachable_patterns)]
            _ => return None,
        })
    }

    /// Compiled once per process, on first use (queries take 4–40 ms to compile).
    fn config(self) -> Option<&'static HighlightConfiguration> {
        static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; 16] =
            [const { OnceLock::new() }; 16];
        let i = TS_ALL.iter().position(|t| *t == self)?;
        CONFIGS[i]
            .get_or_init(|| {
                let (lang, highlights, injections, locals) = self.parts()?;
                let mut c =
                    HighlightConfiguration::new(lang, self.name(), &highlights, injections, locals)
                        .ok()?;
                let names: Vec<&str> = TS_NAMES.iter().map(|(n, _)| *n).collect();
                c.configure(&names);
                Some(c)
            })
            .as_ref()
    }
}

/// A detected language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lang(Kind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ts(Ts),
    /// Index into the two-face syntax set.
    Syntect(usize),
}

impl Lang {
    pub fn engine(self) -> Engine {
        match self.0 {
            Kind::Ts(_) => Engine::TreeSitter,
            Kind::Syntect(_) => Engine::Syntect,
        }
    }
    pub fn name(self) -> &'static str {
        match self.0 {
            Kind::Ts(t) => t.name(),
            Kind::Syntect(i) => syntaxes()
                .syntaxes()
                .get(i)
                .map_or("?", |s| s.name.as_str()),
        }
    }
}

fn syntaxes() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(two_face::syntax::extra_newlines)
}

fn ts_by_path(path: &str) -> Option<Ts> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let lower = file.to_ascii_lowercase();
    let by_file = match lower.as_str() {
        "cargo.lock" | "pipfile" | "poetry.lock" | "uv.lock" => Some(Ts::Toml),
        ".bashrc" | ".bash_profile" | ".zshrc" | ".zprofile" | ".profile" | ".envrc" => {
            Some(Ts::Bash)
        }
        _ => None,
    };
    if by_file.is_some() {
        return by_file;
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "rs" => Ts::Rust,
        "ts" | "mts" | "cts" => Ts::TypeScript,
        "tsx" => Ts::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Ts::JavaScript,
        "py" | "pyi" | "pyw" => Ts::Python,
        "go" => Ts::Go,
        "c" | "h" => Ts::C,
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" | "h++" | "ipp" | "inl" => Ts::Cpp,
        "json" | "jsonc" | "json5" | "geojson" => Ts::Json,
        "yaml" | "yml" => Ts::Yaml,
        "toml" => Ts::Toml,
        "sh" | "bash" | "zsh" | "ksh" => Ts::Bash,
        "css" => Ts::Css,
        "html" | "htm" | "xhtml" => Ts::Html,
        "sql" => Ts::Sql,
        "swift" => Ts::Swift,
        _ => return None,
    })
}

fn ts_by_shebang(first: &[u8]) -> Option<Ts> {
    let line = std::str::from_utf8(first.strip_prefix(b"#!")?).ok()?;
    let line = line.lines().next()?;
    let mut words = line.split_whitespace();
    let mut prog = words.next()?.rsplit('/').next()?;
    if prog == "env" {
        prog = words.find(|w| !w.starts_with('-'))?;
    }
    let prog = prog.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    Some(match prog {
        "sh" | "bash" | "zsh" | "dash" | "ksh" => Ts::Bash,
        "python" => Ts::Python,
        "node" | "deno" | "bun" => Ts::JavaScript,
        _ => return None,
    })
}

fn usable(t: Ts) -> Option<Lang> {
    t.parts().map(|_| Lang(Kind::Ts(t)))
}

/// Language of `path`, from the file name, extension, or the shebang in `first_line`.
pub fn detect(path: &str, first_line: &[u8]) -> Option<Lang> {
    if let Some(l) = ts_by_path(path).and_then(usable) {
        return Some(l);
    }
    if let Some(l) = ts_by_shebang(first_line).and_then(usable) {
        return Some(l);
    }
    let set = syntaxes();
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit_once('.').map_or(file, |(_, e)| e);
    let found: Option<&SyntaxReference> = set
        .find_syntax_by_extension(file)
        .or_else(|| set.find_syntax_by_extension(ext))
        .or_else(|| {
            std::str::from_utf8(first_line)
                .ok()
                .and_then(|l| set.find_syntax_by_first_line(l))
        });
    let found = found.filter(|s| s.name != "Plain Text")?;
    let i = set.syntaxes().iter().position(|s| std::ptr::eq(s, found))?;
    Some(Lang(Kind::Syntect(i)))
}

/// Byte offsets where each line starts (after each `\n`), plus the end.
fn line_starts(src: &[u8]) -> Vec<usize> {
    let mut v = vec![0];
    v.extend(
        src.iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .map(|(i, _)| i + 1),
    );
    v
}

struct Builder<'a> {
    src: &'a [u8],
    starts: Vec<usize>,
    per_line: Vec<Vec<Span>>,
}

impl<'a> Builder<'a> {
    fn new(src: &'a [u8]) -> Self {
        let starts = line_starts(src);
        let n = starts.len();
        Builder {
            src,
            starts,
            per_line: vec![Vec::new(); n],
        }
    }

    /// Records `cap` over absolute byte range `[a, b)`, split at line boundaries.
    fn add(&mut self, a: usize, b: usize, cap: u8) {
        let mut line = self.starts.partition_point(|&s| s <= a).saturating_sub(1);
        let mut a = a;
        while a < b && line < self.starts.len() {
            let ls = self.starts[line];
            let le = self.starts.get(line + 1).map_or(self.src.len(), |&n| n - 1);
            let (s, e) = (a.max(ls), b.min(le));
            if s < e {
                self.per_line[line].push(Span {
                    start: (s - ls) as u32,
                    end: (e - ls) as u32,
                    cap,
                });
            }
            line += 1;
            a = self.starts.get(line).copied().unwrap_or(b);
        }
    }

    fn finish(self) -> Highlights {
        let mut h = Highlights {
            spans: Vec::new(),
            starts: Vec::with_capacity(self.per_line.len() + 1),
        };
        for (i, mut spans) in self.per_line.into_iter().enumerate() {
            h.starts.push(h.spans.len() as u32);
            let len = self.starts.get(i + 1).map_or(self.src.len(), |&n| n - 1) - self.starts[i];
            if len <= MAX_LINE_BYTES {
                // merge neighbours with the same capture
                spans.dedup_by(|b, a| {
                    if a.end == b.start && a.cap == b.cap {
                        a.end = b.end;
                        true
                    } else {
                        false
                    }
                });
                h.spans.extend(spans);
            }
        }
        h.starts.push(h.spans.len() as u32);
        h
    }
}

/// Syntect scope prefixes → capture id; a string or comment anywhere in the stack wins.
fn syntect_table() -> &'static [(Scope, u8)] {
    static T: OnceLock<Vec<(Scope, u8)>> = OnceLock::new();
    T.get_or_init(|| {
        [
            ("constant.character.escape", ESC),
            ("constant.numeric", NUM),
            ("constant", CONST),
            ("keyword.operator", OP),
            ("keyword", KW),
            ("storage", KW),
            ("entity.name.function", FUNC),
            ("support.function", FUNC),
            ("entity.name.tag", TAG),
            ("entity.other.attribute-name", ATTR),
            ("entity.name.type", TYPE),
            ("entity.name.class", TYPE),
            ("support.type", TYPE),
            ("support.class", TYPE),
            ("entity.name.namespace", MODULE),
            ("entity.name.section", KW),
            ("markup.heading", KW),
            ("markup.raw", STR),
            ("markup.underline.link", STR),
            ("variable.language", CONST),
            ("variable.parameter", VAR),
            ("variable", VAR),
            ("punctuation", PUNCT),
        ]
        .into_iter()
        .filter_map(|(s, c)| Scope::new(s).ok().map(|s| (s, c)))
        .collect()
    })
}

fn syntect_cap(stack: &ScopeStack) -> Option<u8> {
    static OUTER: OnceLock<[(Scope, u8); 2]> = OnceLock::new();
    let outer = OUTER.get_or_init(|| {
        [
            (Scope::new("comment").expect("scope"), COMMENT),
            (Scope::new("string").expect("scope"), STR),
        ]
    });
    let scopes = stack.as_slice();
    for s in scopes {
        if let Some((_, c)) = outer.iter().find(|(p, _)| p.is_prefix_of(*s)) {
            return Some(*c);
        }
    }
    scopes.iter().rev().find_map(|s| {
        syntect_table()
            .iter()
            .find(|(p, _)| p.is_prefix_of(*s))
            .map(|(_, c)| *c)
    })
}

/// Reusable per-thread highlighter.
pub struct Highlighter {
    ts: tree_sitter_highlight::Highlighter,
}

impl Default for Highlighter {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlighter {
    pub fn new() -> Highlighter {
        Highlighter {
            ts: tree_sitter_highlight::Highlighter::new(),
        }
    }

    /// Highlights the whole file. None: unknown language, over the limits, cancelled, timed out,
    /// or the engine failed (the diff then simply stays uncoloured).
    pub fn highlight(
        &mut self,
        path: &str,
        src: &[u8],
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Option<Highlights> {
        if src.len() > MAX_BYTES || src.iter().filter(|&&b| b == b'\n').count() >= MAX_LINES {
            return None;
        }
        let first = src.split(|&b| b == b'\n').next().unwrap_or(&[]);
        let lang = detect(path, first)?;
        if cancel() {
            return None;
        }
        match lang.0 {
            Kind::Ts(t) => self.tree_sitter(t, src, cancel),
            Kind::Syntect(i) => syntect(i, src, cancel),
        }
    }

    fn tree_sitter(
        &mut self,
        t: Ts,
        src: &[u8],
        cancel: &(dyn Fn() -> bool + Sync),
    ) -> Option<Highlights> {
        let config = t.config()?;
        let started = Instant::now();
        // The parse runs inside `highlight` before the first event, so a watcher thread turns
        // cancel and the budget into tree-sitter's cancellation flag.
        let flag = AtomicUsize::new(0);
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let watcher = scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    if cancel() || started.elapsed() > BUDGET {
                        flag.store(1, Ordering::Release);
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(2));
                }
            });
            let r = self.tree_sitter_events(config, src, &flag);
            done.store(true, Ordering::Release);
            watcher.thread().unpark();
            r
        })
    }

    fn tree_sitter_events(
        &mut self,
        config: &'static HighlightConfiguration,
        src: &[u8],
        flag: &AtomicUsize,
    ) -> Option<Highlights> {
        let events = self
            .ts
            .highlight(config, src, Some(flag), |name| {
                Ts::by_name(name).and_then(Ts::config)
            })
            .ok()?;
        let mut b = Builder::new(src);
        let mut stack: Vec<u8> = Vec::new();
        for ev in events {
            match ev.ok()? {
                HighlightEvent::HighlightStart(h) => {
                    stack.push(TS_NAMES.get(h.0).map_or(VAR, |x| x.1))
                }
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    if let Some(&cap) = stack.last() {
                        b.add(start, end, cap);
                    }
                }
            }
        }
        Some(b.finish())
    }
}

fn syntect(i: usize, src: &[u8], cancel: &(dyn Fn() -> bool + Sync)) -> Option<Highlights> {
    let text = std::str::from_utf8(src).ok()?;
    if text.split('\n').any(|l| l.len() > MAX_SYNTECT_LINE) {
        return None;
    }
    let set = syntaxes();
    let syntax = set.syntaxes().get(i)?;
    let started = Instant::now();
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut b = Builder::new(src);
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        if cancel() || started.elapsed() > BUDGET {
            return None;
        }
        let ops = state.parse_line(line, set).ok()?;
        let mut pos = 0;
        for (at, op) in ops {
            if at > pos {
                if let Some(c) = syntect_cap(&stack) {
                    b.add(offset + pos, offset + at, c);
                }
                pos = at;
            }
            stack.apply(&op).ok()?;
        }
        if pos < line.len()
            && let Some(c) = syntect_cap(&stack)
        {
            b.add(offset + pos, offset + line.len(), c);
        }
        offset += line.len();
    }
    Some(b.finish())
}
