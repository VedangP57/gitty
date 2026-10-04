# M2b History TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working `gitty` binary: the History tab of the spec (commit list, header, file list, unified/split diff with intraline and expandable context), themes, mouse, and width-driven layouts. The UI thread never touches git.

**Architecture:**
- `crates/gitty` becomes a lib plus a thin bin, so integration and snapshot tests can drive `App` directly.
- `App` is a pure state machine. Inputs and worker `Msg`s go in; `Request`s collect in an outbox; `ui::draw(&App, &mut Frame)` renders.
- Workers execute `Request`s through one function, `exec(&Handle, Request, &mut dyn FnMut(Msg), &Gens)`. Tests call that same function synchronously.
- `main.rs` wires the terminal guard, the input thread, the pools and a crossbeam `select!` loop with no tick.

**Tech Stack:**
- ratatui 0.30.2 (no default features; `crossterm_0_29`, `underline-color`, `layout-cache`) and crossterm 0.29.
- crossbeam-channel 0.5, serde + toml 0.9, unicode-width 0.2, unicode-segmentation 1, signal-hook 0.3, libc.
- Dev: insta 1 and tempfile 3.

**Spec:** `docs/superpowers/specs/2026-10-02-gitty-design.md` (§4, §5.5, §8–§11, §13–§16).

## Global Constraints

- gix types never leak out of gitty-core. The `gitty` crate depends only on gitty-core's public types (spec §4).
- The UI thread never calls into git or gitty-core I/O. Only worker threads hold a `Handle` (spec §1, §4.1).
- Rendering:
  - No tick: draw only after an input event, a worker message, or a deadline (debounce, date threshold).
  - Idle CPU is 0% (spec §4.1, §8).
  - Every frame is wrapped in `\x1b[?2026h` … `\x1b[?2026l` and goes through a 256 KB `BufWriter` (spec §9).
  - Use `Viewport::Fixed`, resized on resize events.
- Mouse modes are 1000, 1002 and 1006 only. Never 1003. crossterm's `EnableMouseCapture` turns on 1003, so do not use it (spec §9).
- Custom widgets write visible rows straight into the `Buffer`, with an ASCII fast path. No `List` or `Paragraph` over large content (spec §9).
- Content sanitising: tabs expand to `tab_size` (default 4); control characters render as caret notation (`^[`); width comes from unicode-width per grapheme (spec §9).
- Colour: truecolor when `COLORTERM` is `truecolor` or `24bit`, otherwise the nearest of 256 colours, computed once at theme load (spec §9).
- Built-in themes (spec §10): github-dark (default), github-light, rose-pine, rose-pine-dawn, catppuccin-mocha, catppuccin-latte, tokyo-night, dracula, gruvbox-dark, solarized-dark, solarized-light.
- Emphasis alpha defaults to 0.25 (spec §10).
- Layout thresholds: <120 one pane; 120–159 history plus stacked right; 160–199 three columns; ≥200 split view automatic when each half fits gutter + 50 text columns (spec §11.1).
- Dates are compact relative (now, 12m, 5h, 3d) up to 7 days, absolute after that. `D` cycles relative / absolute / both (spec §11.2).
- Requests carry a generation and stale results are dropped. Diff requests are debounced by 30 ms (spec §4.2).
- Errors: `anyhow` in the binary. Bad repo data never panics. The panic hook restores the terminal first (spec §13).
- Snapshot tests use `TestBackend` + insta at 100/140/180/220 columns × github-dark/github-light (spec §15).
- Every cargo command runs with `CARGO_HOME=/Users/vedangpatel/Documents/personal/gitty/.cargo-home`, and git writes run outside the sandbox. Both are environment facts.

## Review Focus

1. **Empty and degenerate repos:**
   - Covers: unborn HEAD (no commits), a detached HEAD, a branch with no upstream, and a commit with zero files (empty commit).
   - Expected: each shows a clear placeholder, never panics, and no request loops.
2. **Diff special cases:**
   - Covers: identical content (`row_count() == 0`), binary, LFS, submodule, mode-only, rename without changes, TooLarge, and LargeText/Generated (hidden until Enter).
   - Expected: each shows a one-line explanation instead of an empty pane or a panic from `DiffView::row` on an empty view.
3. **Hostile text:**
   - Covers: tabs, CR, ESC sequences, NUL, wide CJK and emoji at the clip edge, invalid UTF-8, and lines of 100k characters.
   - Expected: rendering never writes outside its rect, never emits a raw escape to the terminal, and stays fast.
4. **Stale results:**
   - Covers: holding `j` through hundreds of commits, and toggling scope mid-walk.
   - Expected: the final screen shows exactly the selected commit's files and diff; no result for an older selection is shown.
5. **Tiny terminals and resize:**
   - Covers: widths 1–40 and heights 1–5.
   - Expected: drawing never panics (no subtraction underflow, no out-of-buffer index), and a resize mid-diff keeps the selection.

Each line has a pinning test in the owning task (T5: 1 and 4; T6/T7: 2, 3 and 5).

---

## File map (crates/gitty/src)

| File | Responsibility |
|---|---|
| `lib.rs` | module tree; `pub fn run(args)` used by main |
| `main.rs` | parse args, call `gitty::run` |
| `text.rs` | sanitise and lay out a line into cells (tabs, controls, graphemes, widths, emphasis), middle truncation, display width |
| `dates.rs` | relative/absolute date formatting, the next re-render threshold |
| `theme/mod.rs` | `Theme` (resolved ratatui colours), `ThemeSpec` (TOML), resolve + inherit, the registry of built-ins and user themes |
| `theme/color.rs` | `Rgb` parse/blend/luma, nearest xterm-256 |
| `theme/builtin/*.toml` | 11 embedded themes |
| `config.rs` | `Config` load and save (theme line), per-repo `UiState` load and save |
| `msg.rs` | `Request`, `Msg`, `Gens` |
| `exec.rs` | `exec(&Handle, Request, sink, &Gens)`: all worker logic |
| `workers.rs` | thread pools (walker, readers, diff), routing, per-thread warm-up |
| `app/mod.rs` | `App` state, `App::new`, `App::handle_msg`, outbox |
| `app/input.rs` | key and mouse → actions, the focus and selection logic |
| `app/diffstate.rs` | per-file diff display state: view, cursor, scroll, hscroll, expansion anchoring, pairing |
| `ui/mod.rs` | `draw(&App, &mut Frame)` and the hit-test map (`Hits`) written during draw |
| `ui/layout.rs` | `LayoutMode` and pane rects from width/height and pane sizes |
| `ui/commit_list.rs`, `ui/header.rs`, `ui/file_list.rs`, `ui/diff.rs`, `ui/bars.rs`, `ui/overlay.rs` | widgets |
| `term.rs` | terminal guard, probe (OSC 11 + kitty + DA1), suspend, panic hook |
| `input.rs` | input thread: read, coalesce bursts |

---

### Task 1: Crate scaffold, core additions, text layout

**Files:**
- Modify:
  - `crates/gitty/Cargo.toml` (deps; `[lib]` + `[[bin]]`)
  - `crates/gitty-core/src/diff/mod.rs` (`intraline_ready`)
  - `crates/gitty-core/src/repo.rs` (`Handle::warm`)
- Create: `crates/gitty/src/lib.rs`, `crates/gitty/src/text.rs`, tests inline in `text.rs`, `crates/gitty-core/tests/warm.rs`

**Interfaces — Produces:**
- `FileDiff::intraline_ready(&self, change: usize) -> Option<&BlockHighlights>`: `OnceLock::get`, never computes.
- `Handle::warm(&self)`: resolves HEAD, diffs its tree against its first parent (or the empty tree), and drops the result. All errors are swallowed; an unborn HEAD is a no-op.
- `text::Cell { sym: CompactString-like SmallString (use String), width: u8, emph: bool, ctrl: bool }`. Keep it lean: `struct Glyph { start: u32 /*byte*/, width: u8, kind: GlyphKind }` and `enum GlyphKind { Ascii(u8), Str(Box<str>)? }`. Decision for implementation: `text::layout(line: &[u8], tab: u8, out: &mut Vec<Glyph>)` with `Glyph { byte: u32, col: u32, width: u8, sym: GlyphSym }` and `enum GlyphSym { Byte(u8), Inline([u8; 4], u8 len), Heap(Box<str>) }`. The ASCII fast path pushes `Byte`.
- `text::display_width(s: &str) -> usize` (grapheme-based).
- `text::truncate_middle(path: &str, max: usize) -> String`: Desktop-style, keeping the filename. `"src/very/long/dir/file.rs"` at 16 gives `"src/…/file.rs"`. If even the filename does not fit, keep the filename's tail behind a leading `…`.
- `text::truncate_end(s: &str, max: usize) -> String` with a trailing `…`.

- [ ] Step 1: Write tests in `text.rs`:
  - `ascii_fast_path` ("ab" → 2 glyphs with cols 0, 1)
  - `tab_expands_to_next_stop` ("a\tb", tab 4 → b at col 4)
  - `control_chars_are_caret` (b"\x1b[31m" → first glyph "^[" width 2)
  - `invalid_utf8_is_replacement` (b"\xff" → "�" width 1)
  - `wide_cjk_width_2` ("日本" → cols 0, 2)
  - `emoji_zwj_is_one_grapheme` ("👩‍💻" → 1 glyph width 2)
  - `byte_offsets_track_source` (for emphasis mapping)
  - `truncate_middle_keeps_filename`, `truncate_middle_short_noop`, `truncate_middle_tiny_width` (max 0..3 never panics; result width ≤ max)
  - `truncate_end_wide_chars` (never exceeds max)
  - In gitty-core:
    - `intraline_ready_is_lazy` (None before `intraline(c)`, Some after)
    - `warm_on_unborn_and_normal_repo` (no panic, returns)
- [ ] Step 2: Run `cargo test -p gitty --lib text` and `cargo test -p gitty-core --test warm`. Expected: compile failure, missing items.
- [ ] Step 3: Implement.
  - Layout iterates `String::from_utf8_lossy`, but byte offsets must refer to the original bytes. So walk with `std::str::from_utf8` chunks: `Utf8Chunks` (stable since 1.79) yields valid prefixes and invalid sequences. Valid runs go through grapheme_indices; each invalid sequence becomes one `�`.
  - Characters `< 0x20` (except tab) and `0x7f` become `^X`. C1 controls U+0080–U+009F become `\u{..}`-style `<9b>` (width 4).
  - A zero-width grapheme at line start renders as a space of width 1? No: skip zero-width graphemes entirely (width 0, not emitted).
- [ ] Step 4: Run both test targets. Expected: PASS.
- [ ] Step 5: Commit `Add text layout, intraline_ready and Handle::warm`.

### Task 2: Dates, initials and identity colours

**Files:** Create `crates/gitty/src/dates.rs` (tests inline).

**Interfaces — Produces:**
- `enum DateMode { Relative, Absolute, Both }` with `next()`.
- `fn format_date(t: i64, offset_secs: i32, now: i64, mode: DateMode) -> String`:
  - Relative: `now` (<60 s, including future skew), `12m`, `5h`, `3d` (<7 d); beyond 7 days it is absolute.
  - Absolute: `Sep 14` in the current year, `Sep 14 2023` otherwise, computed in the commit's own UTC offset.
  - Both: `3d · Sep 28`.
- `fn next_threshold(t: i64, now: i64) -> Option<i64>`: the next epoch second at which the relative text changes, or None once absolute.
- `fn initials(name: &str) -> String`: the first letters of the first and last words, uppercased; one word gives one letter; empty gives `?`.
- `fn identity_hue(email: &str) -> u8`: FNV-1a hash of the lowercased email, mod 8. The theme maps it to one of 8 avatar colours.
- Civil-from-days is implemented by hand (Howard Hinnant's algorithm). No chrono.

- [ ] Step 1: Write tests:
  - `relative_thresholds`: 0→now, 59→now, 60→1m, 3599→59m, 3600→1h, 86399→23h, 86400→1d, 6d23h→6d, 7d→absolute.
  - `future_is_now`.
  - `absolute_same_year` (`now` = 2026-10-02, t = 2026-09-14 → "Sep 14").
  - `absolute_other_year` (2023-09-14 → "Sep 14 2023").
  - `offset_applied` (23:30 UTC with +01:00 → next day).
  - `epoch_zero_and_negative_dont_panic`.
  - `both_mode`.
  - `next_threshold_minutes` (t = now−90 → now+30).
  - `initials_cases` ("Vedang Patel"→"VP", "linus"→"L", ""→"?", "  a  b  c "→"AC", "élodie durand"→"ÉD").
  - `hue_stable_and_case_insensitive`.
- [ ] Step 2: Run `cargo test -p gitty --lib dates`. Expected: FAIL (missing module).
- [ ] Step 3: Implement.
- [ ] Step 4: Run. Expected: PASS.
- [ ] Step 5: Commit `Add date formatting and author identity helpers`.

### Task 3: Themes

**Files:** Create `theme/mod.rs`, `theme/color.rs`, `theme/builtin/{11}.toml`, tests inline and in `crates/gitty/tests/themes.rs`.

**Interfaces — Produces:**
- `color::Rgb(u8, u8, u8)`:
  - `Rgb::parse("#rrggbb" | "#rgb")`
  - `blend(fg, bg, alpha) -> Rgb`
  - `luma() -> f32` (Rec. 709 relative luminance)
  - `to_xterm256() -> u8`: the standard 6×6×6 cube (levels 0, 95, 135, 175, 215, 255) vs the 24-step grayscale (8 + 10i), choosing the smaller squared distance.
- `ThemeSpec` (serde):
  - Top level: `name`, `kind: "dark" | "light"`, `inherit: Option<String>`, `emph_alpha: Option<f32>`, `row_alpha: Option<f32>`.
  - `[ui]`, `[diff]` and `[avatar]` are tables of colour strings; `[syntax]` maps a name to `{fg?, bg?, bold?, italic?, underline?}`.
  - Every key is optional, so a spec can be partial.
- `ui` keys:
  - bg, panel, border, border_focus, fg, muted, accent
  - selection, selection_inactive
  - ahead, behind
  - badge_head_fg/bg, badge_local_fg/bg, badge_remote_fg/bg, badge_tag_fg/bg
  - status_bg, status_fg, error, warning
  - status_added, status_modified, status_deleted, status_renamed
- `diff` keys:
  - add_accent, del_accent (row and emphasis colours derive from these)
  - add_bg, del_bg, add_gutter, del_gutter, add_emph, del_emph (explicit overrides)
  - add_fg, del_fg, context_fg, lineno, lineno_add, lineno_del
  - hunk_bg, hunk_fg, expand_bg, expand_fg, filler (empty side of split), cursor
- `[avatar]` holds `c0`..`c7`.
- `Theme`: the resolved, fully populated struct of `ratatui::style::Color`. Fields mirror the keys; `avatar: [Color; 8]`; `syntax: HashMap<String, Style>`; `name`, `is_light`.
- `ColorDepth { True, Ansi256 }`, with `ColorDepth::detect(env: impl Fn(&str) -> Option<String>)`.
- `Registry::load(user_dir: Option<&Path>) -> Registry` (built-ins plus user TOMLs, user overrides built-in by name). Methods:
  - `names() -> Vec<String>`, sorted, built-ins first.
  - `resolve(name, depth, emph_alpha_override: Option<f32>) -> anyhow::Result<Theme>`.
  - `pick_auto(is_light_terminal: bool) -> &'static str` (github-light / github-dark).
  - `errors() -> &[String]`: user files that failed to parse.
- Resolve rules:
  - Follow the inherit chain depth-first, child keys over parent. A cycle, or a depth over 8, is an error naming the chain.
  - Missing required keys after the merge are an error naming the key. Built-ins must be complete (tested).
  - Derivations:
    - `add_bg = blend(add_accent, bg, row_alpha)`, where `row_alpha` defaults to 0.15 on dark themes and 0.12 on light themes.
    - `add_emph = blend(add_accent, add_bg, emph_alpha)`, where `emph_alpha` defaults to 0.25.
    - `add_gutter = blend(add_accent, bg, row_alpha * 2)`.
    - The `del_*` colours derive the same way from `del_accent`.
    - Explicit values win over derived ones.
  - Ansi256 depth maps every colour through `to_xterm256` once at resolve time.

- [ ] Step 1: Write the tests.
  - `color.rs`:
    - parse: `#fff` and `#0d1117`; `bogus` gives Err.
    - blend endpoints: alpha 0 gives bg, alpha 1 gives fg.
    - xterm mappings: #000000→16, #ffffff→231, #808080→244, #ff0000→196, #5f87af→67.
  - `tests/themes.rs`:
    - `every_builtin_resolves_in_both_depths` (all 11)
    - `builtin_names_complete`
    - `inherit_overrides_only_given_keys` (a user theme `inherit = "github-dark"` with `[ui] accent = "#ff0000"`)
    - `inherit_cycle_errors`
    - `unknown_parent_errors`
    - `user_theme_overrides_builtin_by_name`
    - `bad_user_toml_reported_not_fatal`
    - `emph_alpha_override_changes_emph_only`
    - `light_kind_flag`
    - `ansi256_depth_yields_indexed_colors`
    - `colorterm_detection` (truecolor, 24bit, unset, empty)
- [ ] Step 2: Run `cargo test -p gitty --test themes` and `--lib theme`. Expected: FAIL (missing).
- [ ] Step 3: Implement.
  - Built-in palettes use each theme's canonical published colours: GitHub Primer, Rosé Pine, Catppuccin, Tokyo Night, Dracula, Gruvbox and Solarized.
  - Every built-in has a `[syntax]` table with: keyword, string, comment, function, type, constant, number, operator, variable, property, punctuation, attribute, tag, label, module, constructor, macro, escape, embedded.
- [ ] Step 4: Run. Expected: PASS.
- [ ] Step 5: Commit `Add theme model, 11 built-in themes, inheritance and 256-colour mapping`.

### Task 4: Config and per-repo UI state

**Files:** Create `config.rs` (tests inline).

**Interfaces — Produces:**
- `Config`:
  - `theme: String` (default `"auto"`), `tab_size: u8` (4; clamped 1–16)
  - `diff_algorithm: DiffAlgorithm` (myers), `whitespace: WsMode` (show), `split_threshold: u16` (200)
  - `date_mode: DateMode`, `density: Density { Compact, Comfortable }`
  - `emph_alpha: Option<f32>`
  - `auto_fetch_minutes: u32`, `auto_tune: bool` (parsed now, used in M5)
  - `difftool: Option<String>`
- `Config::load(path: &Path) -> (Config, Vec<String> /*warnings*/)`: a missing file gives the defaults. An unknown key warns; a bad value warns and uses the default.
- `config::set_top_level_key(text: &str, key: &str, value_toml: &str) -> String`:
  - Replaces the first top-level `key = …` line (before any `[table]` header), or else inserts after the leading comment block.
  - Keeps every other byte.
- `Config::save_theme(path, name) -> io::Result<()>`: creates parent dirs.
- `paths::config_dir()`: `$XDG_CONFIG_HOME/gitty`, else `~/.config/gitty`. `paths::state_dir()`: `$XDG_STATE_HOME/gitty`, else `~/.local/state/gitty`.
- `UiState { history_width: Option<u16>, files_width: Option<u16>, files_height: Option<u16>, scope_all: bool }`: TOML at `state_dir/repos/<fnv64 of canonical workdir or git dir>.toml`. Use `UiState::load(path)` and `UiState::save(path)`.

- [ ] Step 1: Write the tests.
  - Loading: `defaults_when_missing`, `parses_all_keys`, `unknown_key_warns`, `bad_value_warns_and_defaults`, `tab_size_clamped`.
  - `set_key_*`: replaces an existing key, inserts into an empty text, inserts into a file of tables only, ignores `theme` under a `[table]`, keeps comments.
  - `ui_state_roundtrip`.
- [ ] Step 2: Run `cargo test -p gitty --lib config`. Expected: FAIL.
- [ ] Step 3: Implement. Parse into `toml::Table` first, then pick each key leniently.
- [ ] Step 4: Run. Expected: PASS.
- [ ] Step 5: Commit `Add config loading, theme persistence and per-repo UI state`.

### Task 5: Requests, exec and worker pools

**Files:** Create `msg.rs`, `exec.rs`, `workers.rs`, and `crates/gitty/tests/exec.rs` (uses `#[path = "../../gitty-core/tests/common/mod.rs"] mod common;`).

**Interfaces — Produces:**
```rust
pub struct Gens { pub session: AtomicU64, pub commit: AtomicU64, pub file: AtomicU64 }
pub type SharedHistory = Arc<RwLock<History>>;
pub enum Request {
    Refs,                                                    // → Msg::Refs (+ fetch_head mtime)
    Walk { session: u64, tips: Vec<CommitId> },              // → HistoryStarted, then HistoryProgress*
    AheadBehind { local: CommitId, upstream: CommitId },     // → Msg::AheadBehind
    Rows { session: u64, ids: Vec<(usize, CommitId)> },      // → Msg::Rows
    Detail { gen: u64, id: CommitId },                       // → Msg::Detail
    Files { gen: u64, id: CommitId, prefetch: bool },        // → Msg::Files then Msg::Stats
    Diff { gen: u64, file: FileChange, opts: DiffOptions, force_text: bool }, // → Msg::Diff then Msg::IntralineDone
}
pub enum Msg {
    Refs { refs: RefsSnapshot, fetched_at: Option<i64> },
    HistoryStarted { session: u64, history: SharedHistory },
    HistoryProgress { session: u64, len: usize, done: bool },
    Rows { session: u64, rows: Vec<(usize, CommitRow)> },
    AheadBehind { local: CommitId, upstream: CommitId, ab: AheadBehind },
    Detail { gen: u64, detail: CommitDetail },
    Files { gen: u64, id: CommitId, files: Arc<Vec<FileChange>>, prefetch: bool },
    Stats { id: CommitId, stats: Vec<LineStats> },          // index-aligned with files; keyed by id (cache-worthy even if stale)
    Diff { gen: u64, key: DiffKey, diff: Arc<FileDiff> },
    IntralineDone { key: DiffKey },
    Error { what: String, detail: String },
}
pub struct DiffKey { pub old: Option<BlobId>, pub new: Option<BlobId>, pub path: String, pub opts: DiffOptions, pub force_text: bool } // Hash+Eq+Clone
pub fn exec(h: &Handle, req: Request, sink: &mut dyn FnMut(Msg), gens: &Gens);
pub struct Workers { /* walker, readers, differs */ }
impl Workers {
    pub fn spawn(repo: Repo, gens: Arc<Gens>, tx: Sender<Msg>) -> Workers; // readers = max(2, cores-2), differs = 2
    pub fn submit(&self, req: Request);                                    // Walk→walker, Diff→differs, rest→readers
}
```
- Walk:
  - Build `h.walker(&tips)`, create `Arc<RwLock<History>>`, send HistoryStarted.
  - Loop: `step` 4096 entries under the write lock, then send HistoryProgress. Stop when `gens.session != session`. The final message has `done: true`.
  - An error sends `Msg::Error` and then `HistoryProgress{done: true}`.
- Rows: decode each id; skip failures; one message per request.
- Files:
  - Bail if stale (gen != commit gen and !prefetch).
  - `commit_files(id, true)` → send Files.
  - Then line_stats per file, checking staleness every 16 files for non-prefetch (prefetch never aborts). Send one Stats message.
- Diff:
  - `file_diff` (plus `force_text` if asked) → send Diff.
  - Then compute `diff.intraline(c)` for every change, checking `gens.file != gen` every block, then send IntralineDone. A stale request stops silently.
- Every error becomes `Msg::Error`; nothing panics. Worker threads wrap exec in `catch_unwind` and report a panic as `Msg::Error`.
- Each pool thread calls `h.warm()` once at start, before taking jobs.

- [ ] Step 1: Write `tests/exec.rs`, using a `run(&Fixture, Request) -> Vec<Msg>` helper.
  - `refs_and_walk_streams_whole_history`: 5 commits. HistoryStarted precedes Progress, and the final `len == 5` with `done`.
  - `walk_stops_when_session_bumped`: bump `gens.session` in the sink at the first progress, on a 10k-commit fixture made with fast-import. Assert done and `len < 10000`.
  - `rows_decode_in_order`.
  - `files_then_stats`.
  - `stale_files_dropped_after_bump`: gen bumped before exec gives no Files message.
  - `prefetch_runs_even_if_stale`.
  - `diff_then_intraline_done`, with intraline_ready Some for all changes.
  - `binary_and_identical_diffs_do_not_panic`: a binary file, and a rename with identical content.
  - `unborn_repo_refs_and_walk`: refs ok, walk with empty tips gives done with len 0.
  - `empty_commit_has_zero_files`.
  - `workers_spawn_and_answer` (real threads, `recv_timeout` 5 s).
- [ ] Step 2: Run `cargo test -p gitty --test exec`. Expected: FAIL (missing).
- [ ] Step 3: Implement.
- [ ] Step 4: Run. Expected: PASS.
- [ ] Step 5: Commit `Add request execution and worker pools with generation cancellation`.

### Task 6: App state, input handling, diff display state

**Files:** Create `app/mod.rs`, `app/input.rs`, `app/diffstate.rs`, and `crates/gitty/tests/app.rs`.

**Interfaces — Produces:**
```rust
pub enum Focus { History, Files, Diff }
pub enum Tab { Changes, History }
pub struct App {
    pub theme: Theme, pub registry: Registry, pub config: Config, pub ui_state: UiState,
    pub now: i64, pub size: (u16, u16), pub tab: Tab, pub focus: Focus, pub fullscreen_diff: bool,
    pub refs: Option<RefsSnapshot>, pub fetched_at: Option<i64>, pub ahead: HashSet<CommitId>, pub behind: HashSet<CommitId>,
    pub history: Option<SharedHistory>, pub history_len: usize, pub history_done: bool, pub session: u64, pub scope: HistoryScope,
    pub rows: HashMap<usize, CommitRow>, pub selected: usize, pub list_scroll: usize,
    pub detail: Option<CommitDetail>, pub header_expanded: bool,
    pub files: Option<Arc<Vec<FileChange>>>, pub stats: Option<Vec<LineStats>>, pub file_sel: usize, pub file_scroll: usize,
    pub diff: Option<DiffState>, pub date_mode: DateMode, pub density: Density, pub split_pref: Option<bool>, pub ws: WsMode,
    pub overlay: Option<Overlay>, pub toast: Option<Toast>, pub quit: bool, pub dirty: bool,
    pub outbox: Vec<Request>, pub gens: Arc<Gens>, pub hits: Hits, /* caches: files LRU, diff LRU, pending/in-flight flags, diff_deadline: Option<Instant>, file_cache */
}
pub enum Overlay { ThemePicker { sel: usize, original: String }, Help, ErrorDetail }
impl App {
    pub fn new(repo_name: String, cfg: Config, registry: Registry, theme: Theme, ui_state: UiState, gens: Arc<Gens>, now: i64) -> App; // pushes Request::Refs
    pub fn handle_msg(&mut self, m: Msg);
    pub fn handle_key(&mut self, k: KeyEvent);
    pub fn handle_mouse(&mut self, m: MouseEvent);
    pub fn handle_resize(&mut self, w: u16, h: u16);
    pub fn tick(&mut self, now: Instant);              // fires the debounced diff request when due
    pub fn next_deadline(&self) -> Option<Instant>;    // debounce or date threshold
    pub fn take_requests(&mut self) -> Vec<Request>;
    pub fn split_active(&self) -> bool;                // split_pref or auto (layout ≥200 and half fits 50+gutter)
}
pub struct DiffState { pub key: DiffKey, pub diff: Arc<FileDiff>, pub view: DiffView, pub cursor: usize, pub scroll: usize, pub hscroll: u16, pub paired: bool, pub first_header: Option<String> }
impl DiffState {
    pub fn new(key, diff) -> DiffState;               // computes first_header when row 0 is not a Gap
    pub fn rows(&self, split: bool) -> usize;          // + 1 when first_header is synthesised; 0 when empty
    pub fn expand_near_cursor(&mut self, split: bool); // the `e` rule below
    pub fn toggle_whole_file(&mut self, split: bool);  // `E`, keeps cursor on the same content
    pub fn next_hunk(&mut self, split: bool, dir: i32);
    pub fn apply_ready_pairing(&mut self);            // set_pairings for every change whose intraline_ready is Some
}
```
- Requests:
  - **Selection change** in history: bump `gens.commit` and clear files/diff/detail.
    - Request Files and Detail immediately if none is in flight, otherwise mark them pending (latest wins when the in-flight result arrives).
    - Use the file-list LRU (1024 entries) on a hit.
  - **Files arrival** (current gen): select file 0 and schedule a diff (debounce 30 ms). Then prefetch the ±10 neighbours that are not cached (`prefetch: true`, max 20 in flight).
  - **Diff scheduling:** bump `gens.file`, set `diff_deadline = now + 30 ms`. `tick()` past the deadline pushes Request::Diff, or uses the diff LRU (200 entries keyed by DiffKey).
  - **Visible rows:** after any scroll, selection or progress change, compute the visible range ± one page and request the missing positions not already requested (a `requested: HashSet<usize>` per session), in batches of ≤256.
  - **Scope toggle `r`:** `session += 1`, `gens.session` stored, request Walk with the new tips. Keep the selected id and reselect it when Progress reveals it (scan only the new range). Rows and requested are cleared.
- `e` rule:
  - Find the nearest gap row to the cursor.
  - Cursor on the gap row → `Expand::All`.
  - Gap row above the cursor → `Expand::Up(gap)`, then cursor += Δrows.
  - Gap row below → `Expand::Down(gap)`.
  - The scroll keeps the cursor visible.
- `E` keeps the cursor on the same content:
  - Remember the row identity (old/new line numbers), rebuild, then scan for the first row with `new >= target` (or `old` for Del rows).
  - The scan is linear and runs once per keypress.
- Navigation:
  - `[`/`]` move the cursor to the previous/next hunk start (split or unified list), and scroll so it is 3 rows from the top.
  - `{`/`}` move to the previous/next file and request its diff.
- Keys by focus:
  - **Movement:** j/k, ↓/↑, g/G, Ctrl-d/u and PgDn/PgUp act on the focused pane (history selection, file selection, diff cursor).
  - **Panes:** Tab/S-Tab cycle the visible panes (layout-dependent). Enter drills in, Esc goes back; in the narrow layout these switch the single pane, otherwise Enter on files focuses the diff and Esc goes back to history.
  - **Diff and history toggles:**
    - h/l hscroll by 8.
    - `s` toggles the split preference; `w` cycles whitespace and re-requests the diff.
    - `F` toggles fullscreen diff (focus diff). `o` toggles the header.
    - `D` cycles the date mode, `z` the density, `r` the scope.
  - **Clipboard:** `y`/`Y` push an OSC 52 copy into `App::osc_out` (a `Vec<String>` main writes after the frame).
  - **App:** `T` opens the theme picker (j/k preview live, Enter persists via `Config::save_theme`, Esc restores). `?` toggles Help. `<`/`>` resize the focused pane by 4 and save UiState. `1`/`2` switch tabs. `q` quits; Ctrl-c quits; Ctrl-z sets `app.suspend = true`.
  - Enter on a LargeText/Generated diff re-requests with `force_text`.
- Mouse (uses `self.hits`, the rects recorded by the last draw):
  - A left click in a pane focuses it and selects the row.
  - A click on a gap row expands: the old-lineno column gives Up, the new-lineno column gives Down, elsewhere All.
  - The wheel scrolls the pane under the pointer by 3 rows (history moves the viewport, not the selection).
  - Dragging a separator resizes that pane: `Hits::separators`, while dragging.
  - A click on a top-bar tab switches tabs.

- [ ] Step 1: Write `tests/app.rs`. It drives App with messages produced by `exec` on fixtures, a helper `pump(app, h)` that executes the outbox until empty (and advances the debounce via `tick(now + 1 s)`), and synthetic key events.
  - `startup_requests_refs_then_walk_then_rows`
  - `selecting_commit_requests_files_and_detail_once_in_flight` (two quick j presses give one in-flight plus a pending, and the final files are for the second commit)
  - `stale_files_message_ignored` (deliver Files with the old gen after a selection move gives files None)
  - `files_arrival_selects_first_and_debounces_diff` (no Diff request before tick, one after)
  - `diff_lru_hit_skips_request`
  - `prefetch_neighbors_issued`
  - `scope_toggle_restarts_walk_and_reselects`
  - `expand_near_cursor_rules` (three cases, using a fixture file with 3 hunks)
  - `whole_file_toggle_keeps_cursor_content`
  - `hunk_navigation`
  - `synthetic_first_header_when_change_at_top`
  - `empty_view_rows_zero_no_panic` (identical-content rename, every key in the diff pane)
  - `unborn_repo_keys_dont_panic` (press every bound key on an empty history)
  - `theme_picker_preview_and_revert`
  - `whitespace_cycle_rerequests`
  - `hold_j_through_200_commits_final_state_matches_selection` (Review Focus 4)
- [ ] Step 2: Run `cargo test -p gitty --test app`. Expected: FAIL (missing).
- [ ] Step 3: Implement.
- [ ] Step 4: Run. Expected: PASS.
- [ ] Step 5: Commit `Add app state machine, key and mouse handling, diff display state`.

### Task 7: Rendering — layout, panes, diff widget, bars, overlays

**Files:** Create `ui/*.rs` and `crates/gitty/tests/render.rs` (insta snapshots under `crates/gitty/tests/snapshots/`).

**Interfaces — Produces:**
- `ui::draw(app: &mut App, f: &mut Frame)`. It is `&mut` because it records `app.hits` (pane rects, row→index maps, gap-row handle columns, separators, tab labels).
- `ui::layout::compute(w, h, focus, fullscreen, ui_state) -> Panes { top: Rect, bottom: Rect, history: Option<Rect>, header: Option<Rect>, files: Option<Rect>, diff: Option<Rect>, separators: Vec<(Rect, Sep)> }`.
  - `LayoutMode::{Narrow, Medium, Wide}`. The ≥200 case is Wide plus the auto split.
  - Defaults: history width 40% (clamped 30–70 cols), files width 30% in Wide (clamped 24–60), files height in Medium of min(n + 1, 35%).
  - Header height: 2 collapsed; expanded is 2 + body lines (capped at 12) + 1 SHA line.
- Commit list row: marker (`↑` ahead colour, `↓` behind colour, else space), space, summary (or dim "Empty commit message"; "…" while not decoded), right side `badges initials date`.
  - Truncation: drop badges, then date, then truncate the summary.
  - Badge text is `name` with background colours per kind. HEAD's branch is bold.
  - Dim (muted fg) for rows in `behind`.
  - Selected row: `selection` bg when focused, `selection_inactive` otherwise.
  - Comfortable density: 2 lines per commit (summary + badges / initials, name, date).
  - A title line: `History · 85,887 commits` (`…` suffix while walking).
- Header:
  - Bold summary (or "Empty commit message").
  - Line 2: coloured initials, `Author` (+ `, co1, co2`), `·`, short SHA, `·`, `+A −D` (when stats are complete), `·`, date.
  - Expanded: body lines, a full SHA line, `Committer: …` if it differs, `Parents: a1b2c3d e4f5…`.
- File list: a title `N changed files`. Rows: status letter (A/M/D/R/C/T) coloured, space, dim directory + bright filename (middle-truncated by display width), right-aligned `+n −m` in add_fg/del_fg (blank until stats arrive; `bin` for binary). Selected-row background as in the list.
- Diff:
  - Title: path (`old → new` for renames), `+a −d`, `[split]`/`[ws: ignore all]` flags.
  - Banners (fixed rows): rename similarity, mode change, eol change, bidi warning.
  - Class messages instead of rows: Binary ("Binary file changed"), Lfs ("Git LFS pointer — oid … size …"), Submodule ("Submodule a1b2c3d → e4f5a6b"), ModeOnly ("Mode changed 100644 → 100755"), TooLarge ("File too large to diff (N MB)"), LargeText/Generated ("Large diff hidden (reason) — press Enter to show"), identical text ("No content changes").
- Unified row: old lineno (width = digits of max(old.len, new.len)), new lineno, marker `+`/`-`/space, text.
  - Background: the full row is painted with add_bg/del_bg first, gutters with add_gutter/del_gutter.
  - Emphasis: add_emph/del_emph on glyphs whose byte is inside an emphasis range, when intraline_ready.
  - The cursor row gets the `cursor` bg on the gutter only (keeps the diff colours readable) when the diff pane is focused.
  - Gap row: hunk_bg across the row. `↑` in the old gutter if can_up, `↓` in the new gutter if can_down, then ` ⋯ N lines  @@ … @@ func` in hunk_fg.
  - The synthetic first header uses the same style, without handles.
  - No-EOL: a dim ` ⊘` after the last line's text on that side.
- Split row: two halves separated by a 1-col border. Each half: lineno, marker, text. A None side is painted with `filler`. Context mirrors both sides. Gap rows span both halves.
- hscroll applies to text columns only. Glyphs straddling the left edge or the right clip become spaces.
- Top bar: ` gitty ` (accent), repo name, `⎇ branch` (or `detached a1b2c3d`), `↑N ↓M` when an upstream exists, `fetched 2m ago` from FETCH_HEAD mtime, and tabs `[1] Changes  [2] History` right-aligned, the active one highlighted.
- Bottom bar: context keys for the focused pane on the left, the toast on the right (error colour, `! details`).
- Overlays: a centered box; the theme picker (list with the current one marked); Help (the key table from spec §11.4, implemented keys only); ErrorDetail (stderr verbatim).
- The Changes tab placeholder pane reads "Changes — staging arrives in a later milestone (M4)."
- Never panic on small sizes:
  - All rect math uses `saturating_sub`.
  - Below 20×5 draw only "gitty: terminal too small".

- [ ] Step 1: Write `tests/render.rs`. It builds the App from a deterministic fixture via exec/pump, sets `now = 1_760_000_000`, and renders to `TestBackend`.
  - `snapshot_{100,140,180,220}_{dark,light}` uses `insta::assert_snapshot!(buffer_text(&buf))` for the text, plus `insta::assert_snapshot!(style_digest(&buf))`. The style digest is one line per row: run-length `fg/bg` runs (an RLE of colours), so theme regressions show in review.
  - `diff_rows_have_full_width_backgrounds` (every cell of a Del row in the diff rect has `del_bg` or `del_gutter` bg)
  - `emphasis_marks_changed_word_only`
  - `split_auto_at_220_unified_at_180`
  - `gap_row_handles_and_click_expands` (mouse click at the recorded handle column grows row_count)
  - `tiny_sizes_never_panic` (every w in 1..=45 × h in 1..=8 with a diff loaded, both layouts, split on and off; Review Focus 5)
  - `hostile_text_stays_in_rect` (a fixture file with ESC, NUL, tab, CJK, emoji, invalid UTF-8 and a 100k-char line; no cell outside the diff rect changes vs a blank render, and no cell symbol contains `\x1b`; Review Focus 3)
  - `special_classes_show_messages` (binary, submodule, mode-only, identical, large-hidden; Review Focus 2)
  - `narrow_drilldown_enter_esc`
  - `wheel_scrolls_pane_under_pointer`
  - `separator_drag_resizes`
- [ ] Step 2: Run `cargo test -p gitty --test render`. Expected: FAIL (missing).
- [ ] Step 3: Implement the widgets.
- [ ] Step 4: Run with `INSTA_UPDATE=always` once to write the snapshots. Read every `.snap` file and check:
  - columns line up
  - truncation order
  - badges on the right
  - gutters
  - full-width backgrounds in the digest

  Then run without the env var. Expected: PASS.
- [ ] Step 5: Commit `Render history, header, file list and diff panes with themes and layouts`.

### Task 8: Terminal, input thread, main loop

**Files:** Create `term.rs`, `input.rs`; modify `lib.rs` (`run`) and `main.rs`; create `crates/gitty/tests/pty.rs` (spawns the built binary in a pseudo-terminal via `libc::openpty` + `fork`/`Command` with the slave fd as stdio).

**Interfaces — Produces:**
- `term::Probe { light: Option<bool>, kitty: bool }`. `term::probe(tty_fd, timeout: 150ms) -> Probe`:
  - Writes `ESC]11;?ESC\` + `ESC[?u` + `ESC[c`.
  - Reads until the DA1 reply (`ESC[?…c`) or the timeout.
  - Parses `rgb:RRRR/GGGG/BBBB` (1–4 hex digits per channel) and `ESC[?<n>u`.
  - The parse is a pure function: `parse_probe(&[u8]) -> Probe`.
- `term::Guard::enter(kitty: bool) -> io::Result<Guard>`:
  - Raw mode, then `?1049h ?25l ?1000h ?1002h ?1006h ?2004h`, then `>1u` if kitty.
- `Guard::restore()` is idempotent (a static `AtomicBool`): `<u` if kitty, then `?2004l ?1006l ?1002l ?1000l ?2026l ?25h ?1049l`, then disable raw mode.
  - It is also called from `Drop`, from the panic hook (installed by `Guard::enter`, chaining the previous hook), and from the signal thread (SIGTERM/SIGHUP → restore, then exit with 128+sig after the main loop sees `Event::Quit`).
- `Guard::suspend()`: restore, `raise(SIGTSTP)`, and on return re-enter. The caller then does `terminal.clear()`.
- `input::spawn(tx: Sender<Vec<Event>>)`: a thread that blocks on `crossterm::event::read()` and then drains `poll(0)`. It merges consecutive wheel events on the same cell into the first one with a repeat count (a wrapper `InputEvent { ev: Event, repeat: u16 }`) and keeps only the last Resize.
- `run(args)`:
  1. Open the repo. A failure prints the error and exits 1 before the terminal is touched.
  2. Load config, registry and UiState; detect the colour depth.
  3. `probe`, pick the theme, `Guard::enter`.
  4. Create `Terminal::with_options(CrosstermBackend::new(BufWriter::with_capacity(256 KiB, stdout)), Viewport::Fixed(size))`.
  5. Spawn workers and input; create the App.
  6. Loop:
     - `select!` over input, msgs and signals, with a `default(deadline)` when `next_deadline` is Some.
     - Drain every channel with try_recv; apply each event.
     - Run `app.tick`, then submit `take_requests`.
     - If dirty: write `?2026h`, `terminal.draw(|f| ui::draw(&mut app, f))`, flush the OSC 52 strings, write `?2026l`, flush.
     - A resize event resizes the fixed viewport and clears.
     - On `app.suspend`, call `Guard::suspend()` and redraw.
     - Exit on `app.quit`.
- Args: `gitty [PATH]`, `--version`, `--theme NAME`, `--help`.

- [ ] Step 1: Write the tests.
  - Unit tests in `term.rs`:
    - `parse_probe_dark_and_light` (`rgb:0d0d/1111/1717` → dark; `rgb:ffff/ffff/ffff` → light; 2-digit form)
    - `parse_probe_kitty_flag`
    - `parse_probe_garbage_is_none`
  - Unit tests in `input.rs`: `coalesce_wheel_bursts`, `coalesce_keeps_last_resize`, `keys_never_coalesced` (the pure `coalesce(Vec<Event>) -> Vec<InputEvent>`).
  - `tests/pty.rs`:
    - `launches_draws_and_quits`: spawn on a fixture repo at 120×40, wait for output containing the summary of the newest commit, send `q`. Assert exit 0, that the output contains `\x1b[?1049l` and `\x1b[?1000l`, and that it never contains `\x1b[?1003h`.
    - `not_a_repo_exits_1_without_alt_screen`.
    - `sigterm_restores_terminal`.
- [ ] Step 2: Run `cargo test -p gitty --lib term input` and `cargo test -p gitty --test pty`. Expected: FAIL.
- [ ] Step 3: Implement.
- [ ] Step 4: Run all `cargo test --workspace`. Expected: PASS.
- [ ] Step 5: Manual smoke in a pty against git/git and the kernel bench repos. Use a small `uv run` python pty driver that captures the screen with `pyte` if available, else a raw dump. Check:
  - the first frame arrives in under 100 ms on git-cg
  - rows stream on the kernel
  - j/k is responsive

  Record the numbers in `bench/README.md`.
- [ ] Step 6: Commit `Add terminal guard, input thread and main event loop`.

---

## Self-review notes

- Spec coverage:
  - §4.1 threads: T5, T8. §4.2: T5, T6. §5.5 caches: T6 (files LRU 1k, diff LRU 200, prefetch ±10).
  - §9: T1, T7, T8. §10: T3, T6 (picker), T4 (persist). §11.1: T7. §11.2 (row, truncation, z, D, header, file list): T7.
  - §11.4 keys implemented in M2b: everything except search, compare, f/p/P, c/A/u, O, the Changes-tab keys (`Space`/`a`/`v`/`H`/`d`) and multi-select — those belong to M4–M6.
  - §11.5 mouse: click, wheel, gap-row click, separator drag. Shift-click and Ctrl-click multi-select and double-click-to-editor are M6.
  - §13: T5, T8. §14: T4. §15 snapshots: T7.
- Deferred to later milestones, with the reason ledgered at execution:
  - `W` wrap (M3, alongside split polish)
  - `t` tree view (M6)
  - double-click `$EDITOR` and multi-select (M6)
  - syntax colours (M3)
- No placeholders: the implementation code for T6 and T7 is described by interfaces and rules rather than written in full, because the author and executor are the same session. The tests listed are the contract.
