# M2a — Diff Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build gitty-core's diff engine. It turns two blobs into a `FileDiff`: a classification plus op lists over the complete texts. From a `FileDiff`, a `DiffView` is derived with expandable context gaps, unified and split rows, git-style hunk headers, and word-level intraline highlights. The UI renders any row range in O(rows on screen).

**Architecture:** `crates/gitty-core/src/diff/` has one file per concern:
- `text.rs`: line index, CR and no-EOL flags
- `ops.rs`: imara-diff ops with whitespace modes and prefix/suffix trim
- `classify.rs`: file classes
- `view.rs`: gaps, row layout and row lookup
- `intraline.rs`: tokenizer, pairing, emphasis
- `mod.rs`: `FileDiff` and `Handle::file_diff`

The diff is never stored as patch text. Rows are computed on demand from ops plus view state.

**Tech Stack:** gix 0.88 (`gix::diff::blob` = gix-imara-diff 0.3), smallvec. Dev: tempfile, proptest.

**Spec:** `docs/superpowers/specs/2026-10-02-gitty-design.md` §6 (whole section), §8, §15

## Global Constraints
- Default algorithm is Myers plus `postprocess_lines` (indent heuristic). Histogram is optional.
- Default context is 3 lines. Expansion step is 20 lines. A gap of ≤20 hidden lines expands fully with one action.
- Intraline: skip lines of ≥1024 chars; greedy pairing when D×A ≤ 4096, scanning ≤32 candidates and accepting distance ≤ 0.6; positional pairing only when D == A when larger.
- Classification thresholds:
  - TooLarge: a side > 64 MiB.
  - LargeText: a side > 4 MiB, any line > 5000 chars, or > 20 000 changed lines.
  - Binary: NUL in the first 8000 bytes.
  - LFS pointer: < 1024 bytes and starts with `version https://git-lfs.github.com/spec/v1`.
- Line numbers are `u32` and 1-based in rows.
- Commit trailer `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Short imperative subjects.

## Review Focus
1. **Files without a trailing newline on one side, and CRLF files.** Rows show the right line numbers. The no-EOL flag sits on the true last line. An "LF → CRLF" whole-file change is reported. Pinned by Task 1 and Task 2 tests.
2. **Empty old or new side** (added/deleted file) and **both sides identical** (pure rename, mode-only change): no panics and zero change rows. Pinned by Task 2 and Task 4 tests.
3. **Expansion at file edges.** The leading gap only expands down to line 1, and the trailing gap only to EOF. Expanding never shows a line twice or skips one. Pinned by the Task 4 property test.
4. **Huge change blocks** (thousands of lines) in intraline: bounded time with no quadratic blowup. Pinned by the Task 5 perf test.
5. **Non-ASCII and wide characters in intraline ranges:** byte ranges always land on char boundaries. Pinned by the Task 5 test.

---

### Task 1: `Text` — line index and line flags

**Files:** Create `crates/gitty-core/src/diff/mod.rs` and `crates/gitty-core/src/diff/text.rs`. Modify `lib.rs` (`pub mod diff;`).

**Interfaces (produces):**
```rust
pub struct Text { bytes: Vec<u8>, starts: Vec<u32> /* start offset of each line */, no_eol: bool }
impl Text {
    pub fn new(bytes: Vec<u8>) -> Text;
    pub fn len(&self) -> u32;               // number of lines; "" → 0, "a" → 1, "a\n" → 1, "a\nb" → 2
    pub fn line(&self, i: u32) -> &[u8];     // without '\n' and without a final '\r'
    pub fn raw_line(&self, i: u32) -> &[u8]; // with terminator(s) exactly as stored
    pub fn has_cr(&self, i: u32) -> bool;    // line ends with "\r\n" (or "\r" at EOF without \n)
    pub fn no_eol(&self) -> bool;            // last line lacks '\n'
    pub fn bytes(&self) -> &[u8];
    pub fn eol_style(&self) -> EolStyle;     // Lf | Crlf | Mixed | None (no line terminators)
}
pub enum EolStyle { None, Lf, Crlf, Mixed }
```

- [ ] **Step 1: Tests** (in `text.rs`)
```rust
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
        assert_eq!(Text::new(b"a\r\nb\r\n".to_vec()).eol_style(), EolStyle::Crlf);
        assert_eq!(Text::new(b"abc".to_vec()).eol_style(), EolStyle::None);
    }
}
```
- [ ] **Step 2: Run.** It fails (`todo!()` stubs).
- [ ] **Step 3: Implement.**
  - `starts` is built in one `memchr`-style pass using a plain loop over bytes: push 0, then for each `\n` at `i < len-1`, push `i+1`.
  - `len` is 0 when bytes are empty.
  - `no_eol` = non-empty and the last byte is not `\n`.
  - `line(i)` slices from `starts[i]` to the next start, or to the end, then strips `\n` and then `\r`.
  - `eol_style`: count lines ending in `\r\n` vs `\n` only. Ignore a last line without a terminator.
- [ ] **Step 4: Run** `cargo test -p gitty-core diff::text`. It passes.
- [ ] **Step 5: Commit** with the message `Add diff Text line index`.

---

### Task 2: Ops — line diff with algorithm, whitespace modes, prefix/suffix trim

**Files:** Create `crates/gitty-core/src/diff/ops.rs` and `crates/gitty-core/tests/diff_ops.rs`. Add `proptest = "1"` to dev-dependencies.

**Interfaces (produces):**
```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)] pub enum DiffAlgorithm { #[default] Myers, Histogram }
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)] pub enum WsMode { #[default] Show, IgnoreAll, IgnoreAmount }
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Op { Equal { old: u32, new: u32, len: u32 }, Change { old: Range<u32>, new: Range<u32> } }
pub fn compute_ops(old: &Text, new: &Text, alg: DiffAlgorithm, ws: WsMode) -> Vec<Op>;
pub fn change_counts(ops: &[Op]) -> (u32 /*added*/, u32 /*removed*/);
```
Invariants:
- Ops cover both files completely and in order, with no empty ops.
- Consecutive `Equal` ops are merged.
- A `Change` has at least one non-empty side.

- [ ] **Step 1: Tests.** `tests/diff_ops.rs` has a reconstruction property test and parity checks against git.
```rust
use gitty_core::diff::ops::{compute_ops, DiffAlgorithm, Op, WsMode};
use gitty_core::diff::text::Text;
use proptest::prelude::*;

fn apply(old: &Text, new: &Text, ops: &[Op]) -> Vec<Vec<u8>> {
    let mut out = vec![];
    let (mut o, mut n) = (0u32, 0u32);
    for op in ops {
        match op {
            Op::Equal { old: os, new: ns, len } => {
                assert_eq!((*os, *ns), (o, n), "ops must be contiguous");
                for i in 0..*len { assert_eq!(old.line(os + i), new.line(ns + i)); out.push(new.line(ns + i).to_vec()); }
                o += len; n += len;
            }
            Op::Change { old: or, new: nr } => {
                assert_eq!((or.start, nr.start), (o, n));
                assert!(!or.is_empty() || !nr.is_empty());
                for i in nr.clone() { out.push(new.line(i).to_vec()); }
                o = or.end; n = nr.end;
            }
        }
    }
    assert_eq!((o, n), (old.len(), new.len()));
    out
}

fn lines_strat() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(prop::sample::select(vec!["a", "b", "c", "  a", "fn x() {", "}", ""]), 0..40)
        .prop_map(|v| { let mut s = v.join("\n"); if !s.is_empty() { s.push('\n'); } s.into_bytes() })
}

proptest! {
    #[test]
    fn ops_reconstruct_new(a in lines_strat(), b in lines_strat(), hist in any::<bool>()) {
        let (ta, tb) = (Text::new(a), Text::new(b.clone()));
        let alg = if hist { DiffAlgorithm::Histogram } else { DiffAlgorithm::Myers };
        let ops = compute_ops(&ta, &tb, alg, WsMode::Show);
        let got = apply(&ta, &tb, &ops);
        let want: Vec<Vec<u8>> = (0..tb.len()).map(|i| tb.line(i).to_vec()).collect();
        prop_assert_eq!(got, want);
        for w in ops.windows(2) { prop_assert!(!matches!((&w[0], &w[1]), (Op::Equal{..}, Op::Equal{..}))); }
    }
}

#[test]
fn identical_is_single_equal() {
    let t = Text::new(b"a\nb\n".to_vec());
    assert_eq!(compute_ops(&t, &t, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Equal { old: 0, new: 0, len: 2 }]);
    let e = Text::new(vec![]);
    assert!(compute_ops(&e, &e, DiffAlgorithm::Myers, WsMode::Show).is_empty());
}

#[test]
fn add_and_delete_whole_file() {
    let e = Text::new(vec![]);
    let t = Text::new(b"x\ny\n".to_vec());
    assert_eq!(compute_ops(&e, &t, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Change { old: 0..0, new: 0..2 }]);
    assert_eq!(compute_ops(&t, &e, DiffAlgorithm::Myers, WsMode::Show), vec![Op::Change { old: 0..2, new: 0..0 }]);
}

#[test]
fn whitespace_modes() {
    let a = Text::new(b"if x {\n  y();\n}\n".to_vec());
    let b = Text::new(b"if x {\n    y();  \n}\n".to_vec());
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::Show).len(), 3);
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::IgnoreAll), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
    assert_eq!(compute_ops(&a, &b, DiffAlgorithm::Myers, WsMode::IgnoreAmount), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
    let c = Text::new(b"if x {\n  y ();\n}\n".to_vec());
    assert_eq!(compute_ops(&a, &c, DiffAlgorithm::Myers, WsMode::IgnoreAmount).len(), 3); // amount != all
    assert_eq!(compute_ops(&a, &c, DiffAlgorithm::Myers, WsMode::IgnoreAll), vec![Op::Equal { old: 0, new: 0, len: 3 }]);
}

/// Parity with `git diff --no-index -U0` on a C-like change where the indent heuristic matters.
#[test]
fn matches_git_hunks_with_indent_heuristic() {
    let old = b"int a() {\n\treturn 1;\n}\n\nint c() {\n\treturn 3;\n}\n".to_vec();
    let new = b"int a() {\n\treturn 1;\n}\n\nint b() {\n\treturn 2;\n}\n\nint c() {\n\treturn 3;\n}\n".to_vec();
    let ops = compute_ops(&Text::new(old.clone()), &Text::new(new.clone()), DiffAlgorithm::Myers, WsMode::Show);
    let ours: Vec<(u32, u32, u32, u32)> = ops.iter().filter_map(|o| match o {
        Op::Change { old, new } => Some((old.start, old.len() as u32, new.start, new.len() as u32)), _ => None }).collect();
    let d = tempfile::tempdir().unwrap();
    std::fs::write(d.path().join("o"), &old).unwrap();
    std::fs::write(d.path().join("n"), &new).unwrap();
    let out = std::process::Command::new("git").current_dir(d.path())
        .args(["-c", "diff.indentHeuristic=true", "diff", "--no-index", "--no-color", "-U0", "o", "n"]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let git: Vec<(u32, u32, u32, u32)> = text.lines().filter(|l| l.starts_with("@@")).map(|l| {
        let p: Vec<&str> = l.split_whitespace().collect();
        let parse = |s: &str| { let s = &s[1..]; let mut it = s.split(','); let a: u32 = it.next().unwrap().parse().unwrap();
            let b: u32 = it.next().map(|x| x.parse().unwrap()).unwrap_or(1); (a, b) };
        let (os, ol) = parse(p[1]); let (ns, nl) = parse(p[2]);
        // git -U0: start is the line *before* for empty ranges; convert to 0-based starts like ours
        (if ol == 0 { os } else { os - 1 }, ol, if nl == 0 { ns } else { ns - 1 }, nl)
    }).collect();
    assert_eq!(ours, git, "git said:\n{text}");
}
```
- [ ] **Step 2: Run.** It fails.
- [ ] **Step 3: Implement** `ops.rs`.
  - **Trim:** count the common prefix lines `p` (comparing `raw_line`s, both sides, while `i < min(len)`) and the common suffix lines `s` (not overlapping the prefix). Keep a margin: `p = p.saturating_sub(3)` and `s = s.saturating_sub(3)` so the indent heuristic sees context.
  - **Tokens:**
    - For `WsMode::Show`, use the raw middle lines (`raw_line`) as `&[u8]` tokens. This keeps CR differences significant, as git does.
    - For `IgnoreAll`, normalise each middle line to bytes with all ASCII whitespace removed.
    - For `IgnoreAmount`, collapse whitespace runs to one space and trim trailing whitespace (`git -b` semantics). Normalised lines live in two `Vec<Vec<u8>>` arenas.
  - **Diff:**
    - Build `InternedInput::default()` and call `update_before`/`update_after` with token slices, then `Diff::compute_with(alg, &input.before, &input.after, input.interner.num_tokens())`.
    - Then call `diff.postprocess_lines(&input)`, which needs `T: AsRef<[u8]>`. Normalised tokens are fine.
    - Map `diff.hunks()` (before/after ranges, offset by `p`) into Change ops, with Equal ops filling the gaps and the trimmed prefix/suffix.
  - **Merge:** merge adjacent Equals and drop empty ops.

  If `InternedInput::default()` or `update_before` doesn't exist in 0.88's re-export, use `InternedInput::new(TokenSourceAdapter)` with a small `impl TokenSource for LineSlices<'a>` returning an iterator of `&'a [u8]`. Record the ruling.
- [ ] **Step 4: Run** `cargo test -p gitty-core --test diff_ops`. It passes (proptest runs 256 cases).
- [ ] **Step 5: Commit** with the message `Add line diff ops with whitespace modes`.

---

### Task 3: File classification

**Files:** Create `crates/gitty-core/src/diff/classify.rs` (unit tests inline).

**Interfaces (produces):**
```rust
pub enum FileClass {
    Text,
    Binary { old_size: u64, new_size: u64 },
    Lfs { old: Option<LfsPointer>, new: Option<LfsPointer> },
    Submodule { old: Option<String>, new: Option<String> },   // commit hex
    ModeOnly { old_mode: u32, new_mode: u32 },
    TooLarge { old_size: u64, new_size: u64 },
    LargeText { reason: LargeReason },
    Generated { reason: &'static str },
}
pub enum LargeReason { Size(u64), LongLine(u32), ManyChanges(u32) }
pub struct LfsPointer { pub oid: String, pub size: u64 }
pub struct ClassifyInput<'a> { pub path: &'a str, pub old: &'a [u8], pub new: &'a [u8], pub old_mode: u32, pub new_mode: u32, pub same_content: bool }
pub fn classify_pre(inp: &ClassifyInput) -> FileClass;            // before diffing: everything except ManyChanges
pub fn classify_post(pre: FileClass, changed_lines: u32) -> FileClass; // Text → LargeText(ManyChanges) if > 20 000
pub fn parse_lfs(b: &[u8]) -> Option<LfsPointer>;
pub fn is_generated(path: &str, sample_new: &[u8]) -> Option<&'static str>;
```
Order, as in spec §6.7:
1. Submodule (either mode 0o160000; the blob ids are the commit ids, so the caller passes their hex as old/new bytes).
2. ModeOnly (`same_content && old_mode != new_mode`).
3. LFS (either side is a pointer).
4. Binary.
5. TooLarge (> 64 MiB).
6. LargeText (> 4 MiB, or any line > 5000 chars).
7. Generated.
8. Text.

Generated means any of:
- the file name is in `{package-lock.json, yarn.lock, pnpm-lock.yaml, bun.lock, bun.lockb, Cargo.lock, Gemfile.lock, poetry.lock, composer.lock, go.sum, Podfile.lock, flake.lock, uv.lock}`;
- a `.min.js`/`.min.css` suffix;
- a `.js`/`.css` file whose average line length is > 110 over the first 64 KiB;
- `sourceMappingURL` in the last 2 lines;
- `@generated` or "Code generated" + "DO NOT EDIT" in the first 40 lines.

- [ ] **Step 1: Tests**
```rust
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
        let mut m = inp("s", b"x\n", b"x\n"); m.new_mode = 0o100755;
        assert!(matches!(classify_pre(&m), FileClass::ModeOnly { .. }));
        let mut sm = inp("sub", b"abc", b"def"); sm.old_mode = 0o160000; sm.new_mode = 0o160000;
        assert!(matches!(classify_pre(&sm), FileClass::Submodule { .. }));
        let lfs = b"version https://git-lfs.github.com/spec/v1\noid sha256:abcd\nsize 1234\n";
        match classify_pre(&inp("big.psd", b"", lfs)) {
            FileClass::Lfs { old: None, new: Some(p) } => assert_eq!((p.oid.as_str(), p.size), ("sha256:abcd", 1234)),
            other => panic!("{other:?}"),
        }
        let long = format!("{}\n", "x".repeat(6000));
        assert!(matches!(classify_pre(&inp("a.txt", b"", long.as_bytes())), FileClass::LargeText { reason: LargeReason::LongLine(_) }));
        assert!(matches!(classify_pre(&inp("Cargo.lock", b"a\n", b"b\n")), FileClass::Generated { .. }));
        assert!(matches!(classify_pre(&inp("gen.go", b"", b"// Code generated by x. DO NOT EDIT.\npackage a\n")), FileClass::Generated { .. }));
        assert!(matches!(classify_post(FileClass::Text, 20_001), FileClass::LargeText { reason: LargeReason::ManyChanges(20_001) }));
        assert!(matches!(classify_post(FileClass::Text, 5), FileClass::Text));
    }
}
```
- [ ] **Step 2: Run.** It fails. **Step 3: Implement** in the order above. **Step 4: Run** and it passes.
- [ ] **Step 5: Commit** with the message `Add diff file classification`.

---

### Task 4: View — gaps, expansion, unified rows, hunk headers

**Files:** Create `crates/gitty-core/src/diff/view.rs` and `crates/gitty-core/tests/diff_view.rs`.

**Interfaces (produces):**
```rust
pub const CONTEXT: u32 = 3;
pub const EXPAND_STEP: u32 = 20;
pub enum Row {
    /// Collapsed context. `hidden` lines, followed by the hunk with this header.
    Gap { gap: usize, hidden: u32, header: String, can_up: bool, can_down: bool },
    Context { old: u32, new: u32 },     // 0-based line indices into old/new Text
    Del { old: u32, change: usize },    // change = index into FileDiff change list
    Add { new: u32, change: usize },
}
pub enum Expand { Up(usize /*gap*/), Down(usize), All(usize), WholeFile, Collapse }
pub struct DiffView { /* gaps state + prefix-summed segments */ }
impl DiffView {
    pub fn new(ops: &[Op], old: &Text, new: &Text) -> DiffView;   // default 3/3 context
    pub fn row_count(&self) -> usize;
    pub fn row(&self, i: usize) -> Row;                             // O(log segments)
    pub fn rows(&self, range: Range<usize>) -> Vec<Row>;
    pub fn expand(&mut self, e: Expand);
    pub fn gap_rows(&self) -> Vec<usize>;                          // row indices of Gap rows (for [ ] navigation)
    pub fn hunk_starts(&self) -> Vec<usize>;                       // first row of each change block
}
pub fn hunk_header(old: &Text, new: &Text, old_line: u32 /*first hidden-after line*/, ...) -> String;
```
**Gap model:**
- Each `Equal` op is a gap with `len`, `top` (lines shown right after the previous change) and `bottom` (lines shown right before the next change).
- Defaults:
  - leading Equal: `top=0, bottom=CONTEXT` (no previous change)
  - trailing Equal: `top=CONTEXT, bottom=0`
  - an Equal that is the whole file (identical sides): `top=0, bottom=0`
  - otherwise: `top=bottom=CONTEXT`
- Clamp so that `top + bottom ≤ len`. If `len - top - bottom == 0`, there is no Gap row.
- **Expansion rules:**
  - `Up(g)`: `bottom += EXPAND_STEP`, so more lines appear above the following hunk.
  - `Down(g)`: `top += EXPAND_STEP`.
  - If the remaining hidden count is ≤ EXPAND_STEP, `Up`/`Down` reveal all of it.
  - `All(g)` reveals the whole gap. `WholeFile` reveals every gap. `Collapse` restores the defaults.
  - `can_up` = there is a following change (not trailing). `can_down` = there is a previous change (not leading).
- **Segments:**
  - Each Change is a run of `Del` rows (old range) then `Add` rows (new range).
  - Each gap emits `top` Context rows, a Gap row if hidden > 0, then `bottom` Context rows.
  - Keep `Vec<Segment>` with cumulative start rows. `row(i)` binary-searches.
- **Hunk header:**
  - The text is `@@ -{os},{ol} +{ns},{nl} @@ {func}`, the way git computes the hunk that follows the gap with the gap's visible bottom context.
  - `os`/`ns` are 1-based starts of that hunk's first shown line.
  - `ol`/`nl` cover from the first bottom-context line through the change and the next gap's top context.
  - `func` = the nearest line above the hunk start in the *old* text whose first byte is ASCII alphabetic, `_` or `$` (git's default funcname). It is trimmed to 80 chars.

- [ ] **Step 1: Tests** `tests/diff_view.rs`
```rust
use gitty_core::diff::ops::{compute_ops, DiffAlgorithm, WsMode};
use gitty_core::diff::text::Text;
use gitty_core::diff::view::{DiffView, Expand, Row};
use proptest::prelude::*;

fn mk(old: &str, new: &str) -> (Text, Text, DiffView) {
    let (o, n) = (Text::new(old.as_bytes().to_vec()), Text::new(new.as_bytes().to_vec()));
    let ops = compute_ops(&o, &n, DiffAlgorithm::Myers, WsMode::Show);
    let v = DiffView::new(&ops, &o, &n);
    (o, n, v)
}
fn numbered(n: u32) -> String { (1..=n).map(|i| format!("line {i}\n")).collect() }

#[test]
fn default_context_and_gaps() {
    let old = numbered(100);
    let new = old.replace("line 50\n", "line fifty\n");
    let (_, _, v) = mk(&old, &new);
    let rows = v.rows(0..v.row_count());
    // gap(46 hidden) + 3 ctx + del + add + 3 ctx + gap(47 hidden)
    assert_eq!(rows.len(), 1 + 3 + 2 + 3 + 1);
    assert!(matches!(rows[0], Row::Gap { hidden: 46, can_up: true, can_down: false, .. }));
    assert!(matches!(rows[1], Row::Context { old: 46, new: 46 }));
    assert!(matches!(rows[4], Row::Del { old: 49, .. }));
    assert!(matches!(rows[5], Row::Add { new: 49, .. }));
    assert!(matches!(rows[9], Row::Gap { hidden: 47, can_up: false, can_down: true, .. }));
    if let Row::Gap { header, .. } = &rows[0] { assert!(header.starts_with("@@ -47,7 +47,7 @@"), "{header}"); }
}

#[test]
fn expand_up_down_all_whole() {
    let old = numbered(100);
    let new = old.replace("line 50\n", "line fifty\n");
    let (_, _, mut v) = mk(&old, &new);
    v.expand(Expand::Up(0));
    assert!(matches!(v.row(0), Row::Gap { hidden: 26, .. }));
    v.expand(Expand::Up(0)); // 26 > 20 → 6 left
    assert!(matches!(v.row(0), Row::Gap { hidden: 6, .. }));
    v.expand(Expand::Up(0)); // ≤ 20 → all
    assert!(matches!(v.row(0), Row::Context { old: 0, new: 0 }));
    v.expand(Expand::WholeFile);
    assert_eq!(v.row_count(), 101); // 100 lines with one del+add pair
    v.expand(Expand::Collapse);
    assert_eq!(v.row_count(), 10);
}

#[test]
fn small_gap_merges_hunks() {
    let old = numbered(20);
    let new = old.replace("line 5\n", "five\n").replace("line 12\n", "twelve\n");
    let (_, _, v) = mk(&old, &new);
    // gap between changes is lines 6..11 (6 lines) → 3+3 shown, no Gap row in between
    let gaps = v.rows(0..v.row_count()).iter().filter(|r| matches!(r, Row::Gap { .. })).count();
    assert_eq!(gaps, 2); // leading and trailing only
}

#[test]
fn added_file_and_identical() {
    let (_, _, v) = mk("", "a\nb\n");
    assert_eq!(v.rows(0..v.row_count()).len(), 2);
    let (_, _, v) = mk("same\n", "same\n");
    assert_eq!(v.row_count(), 1);
    assert!(matches!(v.row(0), Row::Gap { hidden: 1, can_up: false, can_down: false, .. }));
}

#[test]
fn funcname_header() {
    let old = "fn alpha() {\n    let a = 1;\n    let b = 2;\n    let c = 3;\n    let d = 4;\n    let e = 5;\n}\n";
    let new = old.replace("let e = 5;", "let e = 50;");
    let (_, _, v) = mk(old, &new);
    match v.row(0) { Row::Gap { header, .. } => assert!(header.ends_with("fn alpha() {"), "{header}"), r => panic!("{r:?}") }
}

proptest! {
    /// Whatever expansions happen, visible rows never duplicate or skip a line, and stay ordered.
    #[test]
    fn expansion_invariants(seed in prop::collection::vec(0u8..6, 0..12), cut in 1u32..60) {
        let old = numbered(80);
        let new = old.replace(&format!("line {cut}\n"), "changed\n").replace("line 70\n", "");
        let (o, n, mut v) = mk(&old, &new);
        for s in seed {
            let gaps = v.gap_rows();
            let pick = |k: usize| match v.row(gaps[k % gaps.len().max(1)]) { Row::Gap { gap, .. } => gap, _ => 0 };
            if gaps.is_empty() { break; }
            let g = pick(s as usize);
            v.expand(match s % 4 { 0 => Expand::Up(g), 1 => Expand::Down(g), 2 => Expand::All(g), _ => Expand::Up(g) });
        }
        let (mut lo, mut ln) = (None::<u32>, None::<u32>);
        let mut seen_old = 0u32; let mut seen_new = 0u32; let mut hidden = 0u32;
        for r in v.rows(0..v.row_count()) {
            match r {
                Row::Context { old, new } => {
                    prop_assert!(lo.map_or(true, |p| old > p) && ln.map_or(true, |p| new > p));
                    lo = Some(old); ln = Some(new); seen_old += 1; seen_new += 1;
                }
                Row::Del { old, .. } => { prop_assert!(lo.map_or(true, |p| old > p)); lo = Some(old); seen_old += 1; }
                Row::Add { new, .. } => { prop_assert!(ln.map_or(true, |p| new > p)); ln = Some(new); seen_new += 1; }
                Row::Gap { hidden: h, .. } => { hidden += h; }
            }
        }
        prop_assert_eq!(seen_old + hidden, o.len());
        prop_assert_eq!(seen_new + hidden, n.len());
    }
}
```
Note that `hidden` counts lines that are hidden on both sides; Equal lines are present in both.

- [ ] **Step 2: Run.** It fails. **Step 3: Implement** `view.rs` according to the model above. `DiffView` stores `gaps: Vec<GapState { op_index, len, old_start, new_start, top, bottom, leading, trailing }>`, `changes: Vec<(Range<u32>, Range<u32>)>`, and the segment list rebuilt on every `expand`. It also keeps a copy of the hunk header strings, recomputed per rebuild, so `row()` needs no Text access. `DiffView::new` takes `&Text` to compute the funcname headers.
- [ ] **Step 4: Run** `cargo test -p gitty-core --test diff_view`. It passes.
- [ ] **Step 5: Commit** with the message `Add diff view with expandable context gaps`.

---

### Task 5: Intraline highlights (pairing + emphasis)

**Files:** Create `crates/gitty-core/src/diff/intraline.rs` (unit tests inline plus a perf test).

**Interfaces (produces):**
```rust
pub struct BlockHighlights {
    /// For each deleted line (index within the block): Some(added-line index within block) if paired.
    pub pair_of_del: Vec<Option<u32>>,
    pub pair_of_add: Vec<Option<u32>>,
    /// Emphasis byte ranges per line (within `Text::line`, i.e. without terminators).
    pub del_emph: Vec<SmallVec<[Range<u32>; 2]>>,
    pub add_emph: Vec<SmallVec<[Range<u32>; 2]>>,
}
pub fn tokenize(line: &[u8]) -> SmallVec<[Range<u32>; 16]>;      // byte ranges
pub fn block_highlights(dels: &[&[u8]], adds: &[&[u8]]) -> BlockHighlights;
pub const MAX_LINE: usize = 1024; pub const MAX_PRODUCT: usize = 4096; pub const MAX_CANDIDATES: usize = 32; pub const MAX_DISTANCE: f32 = 0.6;
```
**Algorithm:**
- **Tokens:** maximal runs of `[A-Za-z0-9_]` or of bytes ≥ 0x80 (whole UTF-8 sequences stay together because continuation bytes are ≥ 0x80), maximal runs of ASCII whitespace, and single other bytes.
- **Line length check:** use char count. Approximate it as bytes not equal to `0b10xx_xxxx`. Lines ≥ MAX_LINE get no pairing and no emphasis.
- **Distance(a, b):**
  - Do a token Myers diff with imara over interned token slices. Each worker reuses an `InternedInput`/`Diff` passed through a small `Scratch` struct.
  - Let `changed` = the summed trimmed byte widths of removed + added tokens, and `equal` = the summed trimmed widths of equal tokens on the old side.
  - distance = `changed / (changed + 2*equal)`, or 0 if the denominator is 0.
  - Prefilter: skip a candidate when `min(len)/max(len) < 0.2` (by bytes).
- **Pairing:**
  - If `D*A ≤ MAX_PRODUCT`: for each del i in order, scan adds j from `next_free` upward. Look at at most MAX_CANDIDATES unpaired adds and take the first with distance ≤ MAX_DISTANCE. If one is taken, `next_free = j+1` (monotone).
  - Else if `D == A`, try positional pairs (i, i) with the same threshold.
  - Else no pairs.
- **Emphasis for a pair:**
  - Ranges come from the same token diff: the removed tokens on the del side and the added tokens on the add side.
  - Merge ranges separated only by whitespace tokens.
  - Drop a range if it consists only of whitespace at the line's start or end.
  - Unpaired lines get no emphasis (the whole row is already coloured).

- [ ] **Step 1: Tests**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    fn s<'a>(r: &Range<u32>, l: &'a str) -> &'a str { &l[r.start as usize..r.end as usize] }
    #[test]
    fn tokens() {
        let l = "let x_1 = foo(bar);";
        let t: Vec<&str> = tokenize(l.as_bytes()).iter().map(|r| s(r, l)).collect();
        assert_eq!(t, vec!["let", " ", "x_1", " ", "=", " ", "foo", "(", "bar", ")", ";"]);
        let u = "héllo wörld";
        let t: Vec<&str> = tokenize(u.as_bytes()).iter().map(|r| s(r, u)).collect();
        assert_eq!(t, vec!["héllo", " ", "wörld"]);
    }
    #[test]
    fn pairs_similar_and_emphasizes_changed_word() {
        let d = "const total = price * qty;";
        let a = "const total = price * quantity;";
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        assert_eq!(h.pair_of_del, vec![Some(0)]);
        assert_eq!(h.del_emph[0].iter().map(|r| s(r, d)).collect::<Vec<_>>(), vec!["qty"]);
        assert_eq!(h.add_emph[0].iter().map(|r| s(r, a)).collect::<Vec<_>>(), vec!["quantity"]);
    }
    #[test]
    fn unequal_counts_still_pair() {
        let dels = ["foo(a, b);", "bar();"];
        let adds = ["// new comment", "foo(a, b, c);", "baz();", "bar(1);"];
        let h = block_highlights(&dels.map(str::as_bytes), &adds.map(str::as_bytes));
        assert_eq!(h.pair_of_del, vec![Some(1), Some(3)]);
        assert_eq!(h.pair_of_add, vec![None, Some(0), None, Some(1)]);
    }
    #[test]
    fn dissimilar_not_paired() {
        let h = block_highlights(&[b"alpha beta gamma".as_slice()], &[b"}".as_slice()]);
        assert_eq!(h.pair_of_del, vec![None]);
        assert!(h.del_emph[0].is_empty());
    }
    #[test]
    fn whitespace_only_change_has_no_edge_emphasis() {
        let h = block_highlights(&[b"  x = 1;".as_slice()], &[b"    x = 1;".as_slice()]);
        assert_eq!(h.pair_of_del, vec![Some(0)]);
        assert!(h.add_emph[0].is_empty(), "{:?}", h.add_emph);
    }
    #[test]
    fn emph_ranges_on_char_boundaries() {
        let d = "naïve café"; let a = "naïve cafés";
        let h = block_highlights(&[d.as_bytes()], &[a.as_bytes()]);
        for r in h.add_emph[0].iter() { assert!(a.is_char_boundary(r.start as usize) && a.is_char_boundary(r.end as usize)); }
    }
    #[test]
    fn huge_block_is_bounded() {
        let dels: Vec<String> = (0..3000).map(|i| format!("old line number {i} with text")).collect();
        let adds: Vec<String> = (0..3500).map(|i| format!("new line number {i} with other text")).collect();
        let (d, a): (Vec<&[u8]>, Vec<&[u8]>) = (dels.iter().map(|x| x.as_bytes()).collect(), adds.iter().map(|x| x.as_bytes()).collect());
        let t = std::time::Instant::now();
        let h = block_highlights(&d, &a);
        assert_eq!(h.pair_of_del.len(), 3000);
        assert!(t.elapsed().as_millis() < 500, "took {:?}", t.elapsed()); // D*A > 4096 and D != A → no pairing
    }
}
```
- [ ] **Step 2: Run.** It fails. **Step 3: Implement.** **Step 4: Run** `cargo test -p gitty-core intraline`. It passes.
- [ ] **Step 5: Commit** with the message `Add intraline word highlights with similarity pairing`.

---

### Task 6: Split rows

**Files:** Modify `crates/gitty-core/src/diff/view.rs` and its tests.

**Interfaces (produces):**
```rust
pub enum SplitRow {
    Gap { gap: usize, hidden: u32, header: String, can_up: bool, can_down: bool },
    Context { old: u32, new: u32 },
    Change { old: Option<u32>, new: Option<u32>, change: usize },
}
impl DiffView {
    /// Requires pairing per change: `pairs[change] = BlockHighlights.pair_of_del` (positions relative to block).
    pub fn set_pairing(&mut self, change: usize, pair_of_del: &[Option<u32>]);
    pub fn split_row_count(&self) -> usize;
    pub fn split_rows(&self, range: Range<usize>) -> Vec<SplitRow>;
}
```
Rows for a change block are built in order with two cursors:
- An unpaired del emits `(Some(d), None)`.
- An unpaired add emits `(None, Some(a))`.
- A pair (d, a) is emitted together once both cursors reach it. First emit the unpaired dels before d and the unpaired adds before a, each in their own rows (`Desktop` style when no pairing exists).
- Without pairing info, use positional zip: `(d_i, a_i)` for i < min, then the leftovers.

The split segment list is maintained next to the unified one.

- [ ] **Step 1: Tests**
```rust
#[test]
fn split_rows_follow_pairing() {
    let (_, _, mut v) = mk("a\nfoo(a, b);\nbar();\nz\n", "a\n// new comment\nfoo(a, b, c);\nbaz();\nbar(1);\nz\n");
    v.expand(Expand::WholeFile);
    v.set_pairing(0, &[Some(1), Some(3)]);
    let rows = v.split_rows(0..v.split_row_count());
    use gitty_core::diff::view::SplitRow::*;
    let shape: Vec<(Option<u32>, Option<u32>)> = rows.iter().filter_map(|r| match r { Change { old, new, .. } => Some((*old, *new)), _ => None }).collect();
    assert_eq!(shape, vec![(None, Some(1)), (Some(1), Some(2)), (None, Some(3)), (Some(2), Some(4))]);
}
#[test]
fn split_rows_positional_without_pairing() {
    let (_, _, mut v) = mk("x\na\nb\ny\n", "x\nA\nB\nC\ny\n");
    v.expand(Expand::WholeFile);
    let rows = v.split_rows(0..v.split_row_count());
    use gitty_core::diff::view::SplitRow::*;
    let shape: Vec<(Option<u32>, Option<u32>)> = rows.iter().filter_map(|r| match r { Change { old, new, .. } => Some((*old, *new)), _ => None }).collect();
    assert_eq!(shape, vec![(Some(1), Some(1)), (Some(2), Some(2)), (None, Some(3))]);
}
```
- [ ] **Steps 2–4:** RED, then implement, then GREEN.
- [ ] **Step 5: Commit** with the message `Add split view rows from line pairing`.

---

### Task 7: `FileDiff` assembly + `Handle::file_diff` + perf probe

**Files:** Modify `crates/gitty-core/src/diff/mod.rs`. Create `crates/gitty-core/tests/file_diff.rs`. Modify `examples/probe.rs` (adds a `diffs` subcommand).

**Interfaces (produces):**
```rust
pub struct DiffOptions { pub algorithm: DiffAlgorithm, pub ws: WsMode }
pub struct FileDiff {
    pub path: String, pub old_path: Option<String>,
    pub class: FileClass,
    pub old: Arc<Text>, pub new: Arc<Text>,
    pub ops: Vec<Op>,
    pub changes: Vec<(Range<u32>, Range<u32>)>,   // Change ops in order (index = `change` in rows)
    pub added: u32, pub removed: u32,
    pub eol_change: Option<(EolStyle, EolStyle)>, // whole-file style change
    pub bidi_warning: bool,
    pub options: DiffOptions,
    intraline: Vec<OnceLock<BlockHighlights>>,
}
impl FileDiff {
    pub fn from_bytes(path: &str, old_path: Option<&str>, old: Vec<u8>, new: Vec<u8>, old_mode: u32, new_mode: u32, opts: DiffOptions) -> FileDiff;
    pub fn intraline(&self, change: usize) -> &BlockHighlights;   // lazily computed, thread-safe
    pub fn view(&self) -> DiffView;
    pub fn is_text(&self) -> bool;
    /// Force a LargeText/Generated file to be treated as Text (user pressed Enter).
    pub fn force_text(self) -> FileDiff;
}
impl Handle {
    pub fn file_diff(&self, change: &FileChange, opts: DiffOptions) -> anyhow::Result<FileDiff>;
}
```
Rules:
- **Content loading:**
  - Submodule entries do not load blobs. Pass the hex ids as the old/new bytes to classify.
  - TooLarge is checked from object headers (`find_header().size()`) before loading.
- **Non-text classes** keep `ops` empty and both Texts empty, except Generated/LargeText. Those keep the texts so `force_text` can diff without reloading.
- **Text:** compute ops, counts and changes, then `classify_post`.
- **Signals:**
  - `eol_change` is set when both sides are non-empty and their styles differ (ignoring None).
  - `bidi_warning` scans changed lines for U+202A–202E and U+2066–2069 (UTF-8 bytes `E2 80 AA..AE`, `E2 81 A6..A9`).

- [ ] **Step 1: Tests** `tests/file_diff.rs`. Use the fixture from `tests/common` via `mod common;`.
```rust
mod common;
use common::Fixture;
use gitty_core::diff::{classify::FileClass, DiffOptions};
use gitty_core::{CommitId, Repo};

#[test]
fn diff_of_modified_file() {
    let f = Fixture::new();
    f.write("a.rs", "fn main() {\n    println!(\"hi\");\n}\n");
    f.commit("one", 1_700_000_000);
    f.write("a.rs", "fn main() {\n    println!(\"hello\");\n}\n");
    let c = CommitId::from_hex(&f.commit("two", 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let fc = &h.commit_files(c, false).unwrap()[0];
    let d = h.file_diff(fc, DiffOptions::default()).unwrap();
    assert!(matches!(d.class, FileClass::Text));
    assert_eq!((d.added, d.removed), (1, 1));
    let hl = d.intraline(0);
    assert_eq!(hl.pair_of_del, vec![Some(0)]);
    let v = d.view();
    assert_eq!(v.row_count(), 4);
}

#[test]
fn eol_and_bidi_flags() {
    let d = gitty_core::diff::FileDiff::from_bytes("x.txt", None, b"a\nb\n".to_vec(), "a\r\nb\u{202E}\r\n".as_bytes().to_vec(),
        0o100644, 0o100644, DiffOptions::default());
    assert!(d.eol_change.is_some());
    assert!(d.bidi_warning);
}

#[test]
fn binary_and_rename_only() {
    let f = Fixture::new();
    f.write("img.bin", [0u8, 1, 2]);
    f.write("old.txt", "same\ncontent\n");
    f.commit("one", 1_700_000_000);
    f.write("img.bin", [0u8, 3, 4, 5]);
    f.git(&["mv", "old.txt", "new.txt"]);
    let c = CommitId::from_hex(&f.commit("two", 1_700_000_100)).unwrap();
    let h = Repo::open(f.path()).unwrap().handle();
    let files = h.commit_files(c, true).unwrap();
    let bin = files.iter().find(|x| x.path == "img.bin").unwrap();
    assert!(matches!(h.file_diff(bin, DiffOptions::default()).unwrap().class, FileClass::Binary { old_size: 3, new_size: 4 }));
    let ren = files.iter().find(|x| x.path == "new.txt").unwrap();
    let d = h.file_diff(ren, DiffOptions::default()).unwrap();
    assert_eq!((d.added, d.removed), (0, 0));
    assert_eq!(d.old_path.as_deref(), Some("old.txt"));
}
```
- [ ] **Step 2: Run.** It fails. **Step 3: Implement.** **Step 4: Run** the full `cargo test -p gitty-core`. It passes.
- [ ] **Step 5: Probe.** Add `probe diffs <repo> <n>`. It diffs every text file of the first n commits on HEAD, computes the intraline for every change, and prints p50/p99/max ms per file plus the total. Run it on `git-cg` with n=300. Budget: p50 < 1 ms, p99 < 10 ms. Record the result in `bench/README.md`.
- [ ] **Step 6: Commit** with the message `Add FileDiff assembly and diff probe`.

## Self-review notes
- **Spec §6 coverage:**
  - Model: Tasks 1, 2 and 7.
  - Algorithm, whitespace modes and trim: Task 2.
  - Expansion: Task 4.
  - Intraline: Task 5.
  - Split: Task 6.
  - Classification: Task 3.
  - Edge cases (CRLF, no-EOL, bidi, renames): Tasks 1 and 7.
  - Linguist attributes (`linguist-generated`, `-diff`, `binary`) need gitattributes lookups and are deferred to M6. Ruling: heuristics only in M2a.
- **Content normalisation (spec §6.1, clean filters/autocrlf):** applies to worktree content, so it lands in M4 with the worktree diff source. Commit diffs compare blobs exactly as git does.
