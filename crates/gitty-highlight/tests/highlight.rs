use gitty_highlight::{CAPTURES, Engine, Highlighter, Highlights, Lang, detect};

fn hl(path: &str, src: &str) -> Highlights {
    Highlighter::new()
        .highlight(path, src.as_bytes(), &|| false)
        .unwrap_or_else(|| panic!("{path}: no highlights"))
}

/// Capture name covering the first byte of `needle` on `line`.
fn cap_at(h: &Highlights, src: &str, line: usize, needle: &str) -> Option<&'static str> {
    let text = src.split('\n').nth(line).unwrap();
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} not on line {line}")) as u32;
    h.line(line as u32)
        .iter()
        .find(|s| s.start <= at && at < s.end)
        .map(|s| CAPTURES[s.cap as usize])
}

#[test]
fn detection() {
    let ts = |p: &str| detect(p, b"").map(|l| l.engine());
    assert_eq!(ts("src/main.rs"), Some(Engine::TreeSitter));
    assert_eq!(detect("a/b.tsx", b"").map(|l| l.name()), Some("tsx"));
    assert_eq!(detect("x.hpp", b"").map(|l| l.name()), Some("cpp"));
    assert_eq!(detect("Cargo.lock", b"").map(|l| l.name()), Some("toml"));
    assert_eq!(detect(".zshrc", b"").map(|l| l.name()), Some("bash"));
    assert_eq!(
        detect("run", b"#!/usr/bin/env python3\n").map(|l| l.name()),
        Some("python")
    );
    assert_eq!(
        detect("tool", b"#!/bin/sh -e\n").map(|l| l.name()),
        Some("bash")
    );
    assert_eq!(ts("Makefile"), Some(Engine::Syntect));
    assert_eq!(ts("Dockerfile"), Some(Engine::Syntect));
    assert_eq!(ts("README.md"), Some(Engine::Syntect));
    assert_eq!(ts("lib.rb"), Some(Engine::Syntect));
    assert_eq!(ts("data.unknownext"), None);
    let _: Option<Lang> = detect("x", b"");
}

#[test]
fn rust_keywords_strings_comments() {
    let src = "fn main() {\n    let s = \"hi\"; // note\n}\n";
    let h = hl("main.rs", src);
    assert_eq!(cap_at(&h, src, 0, "fn"), Some("keyword"));
    assert_eq!(cap_at(&h, src, 0, "main"), Some("function"));
    assert_eq!(cap_at(&h, src, 1, "\"hi\""), Some("string"));
    assert_eq!(cap_at(&h, src, 1, "// note"), Some("comment"));
    assert_eq!(h.lines(), 4);
}

#[test]
fn typescript_inherits_javascript() {
    let src = "const x: number = 1;\nfunction f() { return x; }\n";
    let h = hl("a.ts", src);
    assert_eq!(cap_at(&h, src, 0, "const"), Some("keyword"));
    assert_eq!(cap_at(&h, src, 0, "number"), Some("type"));
    assert_eq!(cap_at(&h, src, 0, "1"), Some("number"));
    assert_eq!(cap_at(&h, src, 1, "return"), Some("keyword"));
}

#[test]
fn tsx_tags() {
    let src = "const a = <div className=\"x\">hi</div>;\n";
    let h = hl("a.tsx", src);
    assert_eq!(cap_at(&h, src, 0, "div"), Some("tag"));
}

#[test]
fn cpp_inherits_c() {
    let src = "class A {};\n// c comment\nint main() { return 0; }\n";
    let h = hl("a.cpp", src);
    assert_eq!(cap_at(&h, src, 0, "class"), Some("keyword"));
    assert_eq!(cap_at(&h, src, 1, "// c"), Some("comment"));
    assert_eq!(cap_at(&h, src, 2, "return"), Some("keyword"));
}

#[test]
fn html_injects_javascript() {
    let src = "<p>hi</p>\n<script>\nlet a = 1;\n</script>\n";
    let h = hl("index.html", src);
    assert_eq!(cap_at(&h, src, 0, "p"), Some("tag"));
    assert_eq!(cap_at(&h, src, 2, "let"), Some("keyword"));
}

#[test]
fn multiline_comment_spans_each_line() {
    let src = "/* one\ntwo */\nint x;\n";
    let h = hl("a.c", src);
    assert_eq!(cap_at(&h, src, 0, "one"), Some("comment"));
    assert_eq!(cap_at(&h, src, 1, "two"), Some("comment"));
    assert_ne!(cap_at(&h, src, 2, "int"), Some("comment"));
    for i in 0..h.lines() as u32 {
        let len = src.split('\n').nth(i as usize).unwrap().len() as u32;
        assert!(
            h.line(i).iter().all(|s| s.start < s.end && s.end <= len),
            "line {i} spans stay inside the line"
        );
    }
}

#[test]
fn crlf_lines() {
    let src = "fn a() {}\r\nfn b() {}\r\n";
    let h = hl("a.rs", src);
    assert_eq!(cap_at(&h, src, 1, "fn"), Some("keyword"));
}

#[test]
fn every_bundled_language_highlights() {
    let samples = [
        ("a.rs", "fn a() {}"),
        ("a.ts", "let a = 1;"),
        ("a.tsx", "let a = <b/>;"),
        ("a.js", "let a = 1;"),
        ("a.py", "def a():\n    return 1"),
        ("a.go", "package a\nfunc b() {}"),
        ("a.c", "int a;"),
        ("a.cc", "int a;"),
        ("a.json", "{\"a\": 1}"),
        ("a.yaml", "a: 1"),
        ("a.toml", "a = 1"),
        ("a.sh", "echo hi"),
        ("a.css", "a { color: red; }"),
        ("a.html", "<a href=\"x\">y</a>"),
        ("a.sql", "SELECT a FROM b;"),
        ("a.swift", "let a = 1"),
    ];
    for (p, s) in samples {
        assert_eq!(
            detect(p, b"").map(|l| l.engine()),
            Some(Engine::TreeSitter),
            "{p}"
        );
        let h = hl(p, s);
        assert!(
            (0..h.lines() as u32).any(|i| !h.line(i).is_empty()),
            "{p} produced no spans"
        );
    }
}

#[test]
fn syntect_fallback() {
    let src = "def greet(name)\n  puts \"hi #{name}\"\nend\n";
    let h = hl("a.rb", src);
    assert_eq!(cap_at(&h, src, 0, "def"), Some("keyword"));
    assert_eq!(cap_at(&h, src, 1, "\"hi"), Some("string"));
    let md = "# Title\n\nsome `code`\n";
    let h = hl("README.md", md);
    assert_eq!(cap_at(&h, md, 0, "Title"), Some("keyword"));
}

#[test]
fn limits() {
    let big = "a".repeat(3 * 1024 * 1024);
    assert!(
        Highlighter::new()
            .highlight("a.rs", big.as_bytes(), &|| false)
            .is_none()
    );
    let long = format!("let s = \"{}\";\nlet t = 1;\n", "x".repeat(5000));
    let h = hl("a.rs", &long);
    assert!(h.line(0).is_empty(), "very long lines stay uncoloured");
    assert_eq!(cap_at(&h, &long, 1, "let"), Some("keyword"));
}

#[test]
fn cancellation() {
    let src = "fn a() {}\n".repeat(2000);
    assert!(
        Highlighter::new()
            .highlight("a.rs", src.as_bytes(), &|| true)
            .is_none()
    );
    assert!(
        Highlighter::new()
            .highlight("a.rb", "def a\nend\n".repeat(500).as_bytes(), &|| true)
            .is_none()
    );
}

#[test]
fn invalid_utf8_and_binaryish_input_do_not_panic() {
    let mut src = b"fn a() { let s = \"".to_vec();
    src.extend_from_slice(b"\xff\xfe\x00");
    src.extend_from_slice(b"\"; }\n");
    let _ = Highlighter::new().highlight("a.rs", &src, &|| false);
    let _ = Highlighter::new().highlight("a.rb", &src, &|| false);
}
