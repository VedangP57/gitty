# M3 Syntax Highlighting, Split Polish and Wrap — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** Diffs get correct syntax colours (whole-file tree-sitter, syntect fallback) without ever blocking the UI. Split view and the cursor stay stable. `W` wraps long lines.

**Architecture:**
- A new workspace crate, `gitty-highlight`, does the highlighting: path + bytes in, per-line spans of capture ids out. It knows nothing about git or UI.
- The `gitty` crate runs it on a dedicated 2-thread highlight pool, keyed by blob id, and paints the theme's `[syntax]` colours under the diff and emphasis backgrounds.

**Tech Stack:**
- tree-sitter 0.26 + tree-sitter-highlight 0.26 (the benchmarked version).
- Grammar crates behind cargo features: rust, typescript/tsx, javascript, python, go, c, cpp, json, yaml, toml, bash, css, html, sql, swift.
- syntect 5.3 (onig, parsing only) + two-face 0.5 for every other language, including Markdown.

**Spec:** docs/superpowers/specs/2026-10-02-gitty-design.md §7 (highlighting), §6.5 (split), §11.4 (`W`), §8 (colours < 100 ms for ≤ 10k lines).

## Global Constraints
- Highlight whole files, never single hunks (spec §7). The diff draws first with diff colours only; spans arrive later.
- Spans store capture ids, not colours, so a theme switch needs no recompute. Cache key: blob id (+ language implied by path).
- Limits:
  - files over 2 MiB or 50k lines get no syntax colour
  - lines over 1000 bytes get no syntax colour
  - the highlight job checks cancellation (generation) and a 2 s budget
- Highlighting runs on its own pool and never starves history or diff workers (spec §4.1).

## Review Focus
1. Stale highlights from another blob are never painted on the current diff (blob-keyed lookup, generation drop).
2. Syntax fg composes with diff backgrounds and word-emphasis backgrounds (bg stays the diff's).
3. Wrapped rows: cursor, scroll, mouse hit-testing and gap-row clicks map screen lines to the right logical row.
4. Hostile input (invalid UTF-8, NUL, a 3 MiB minified file) never panics in tree-sitter or syntect.
5. Theme switch recolours syntax immediately without new highlight requests.

## Tasks
- [ ] **T1 gitty-highlight crate** (`crates/gitty-highlight`). Test: `tests/highlight.rs`.
  - API: `CAPTURES`, `detect(path, first_line) -> Option<Lang>`, `Lang::{name, engine}`, `Highlighter::highlight(path, bytes, cancel) -> Option<Highlights>`, `Highlights::{line(i) -> &[Span], lines()}`, `Span{start,end,cap}`.
  - TS = JS highlights + TS highlights; TSX adds JSX; C++ = C++ + C queries; HTML injects JS/CSS.
- [ ] **T2 Wiring and render.**
  - `Request::Highlight{generation, blob, path, text}` → `Msg::Highlighted{blob, spans}`, on a highlight pool.
  - App: LRU (64 entries) by blob. Requests for both sides when a diff is installed (old side only for Del/context-old use).
  - Render: glyph fg from `theme.syntax[CAPTURES[cap]]` when no emphasis override; context rows use the new side's spans.
  - Tests: exec, app (request once, cache hit, stale dropped), render (keyword fg colour on add row keeps add_bg; theme switch recolours).
- [ ] **T3 Wrap, split polish, folded minors.**
  - `W` toggles wrap: rows take ceil(width/text_w) screen lines.
  - `hits.diff_lines` maps screen line → virtual row; `ensure_visible` accounts for heights.
  - Re-anchor the split cursor when pairing arrives (M2b minor M7).
  - Lay out only the visible columns of long lines when not wrapping (M2b minor M6).
  - Tests: wrap snapshot; cursor/hit mapping; long-line render time; split cursor stable across IntralineDone.
- [ ] **T4 Measure.** Binary size with and without grammars; highlight time on diff.c / a 10k-line TS file. Record in bench/README.md.
