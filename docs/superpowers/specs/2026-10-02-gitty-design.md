# gitty — Design Spec

Date: 2026-10-02 · Status: draft for review · Research: `docs/research/2026-10-02-gitty-research.md`

## 1. Purpose

gitty is a terminal git client with one job: the GitHub Desktop experience, done faster.

- **History.** You can see every commit, local and on the server.
- **Commit view.** Selecting a commit shows exactly which files changed. Its diff is as clear as GitHub Desktop's.
- **Changes.** You can stage and commit your working-tree changes.

It competes with lazygit, gitui and tig. It wins on two things: **diff clarity** and **speed at kernel scale**.

### Success criteria
- Diff view has the following, on one screen with no drill-down:
  - full-width red/green rows
  - word-level highlights
  - correct syntax colours
  - dual line-number gutters
  - context you can expand
  - unified or split layout
  - commit header, file list and diff together
- Speed matches the latency budget in §8 on the Linux kernel (~1.5M commits). It is enforced by benchmarks.
- Idle CPU is 0%. The UI thread never waits on git.
- Line and hunk staging is byte-exact: property-tested against `git apply`.

### Non-goals (v1)
- Rebase, cherry-pick, reset-to, stash, branch CRUD, bisect, worktrees, submodule management. That is lazygit's territory.
- Commit-graph lanes. The list is flat with badges; an optional graph column may come later.
- Merge-conflict resolution UI. Conflicted files show as whole-file only.
- GitHub API, PR, issue or AI features. Structural (difftastic-style) diffs. Image diffs.
- Windows support. macOS first, Linux second.

## 2. User decisions (fixed)

| Topic | Decision |
|---|---|
| Scope | View, plus stage (file/hunk/line), commit/amend/undo, fetch/pull/push |
| Diff layout | Unified by default. `s` toggles split; split is automatic at ≥200 cols |
| Scale | Kernel-scale from day one |
| History list | Flat list with ↑ unpushed / ↓ unpulled markers and branch/tag badges |
| Default history scope | Current branch plus its upstream, commit-time order, unpulled rows dimmed. A key toggles all refs |
| Input | Keyboard and mouse |
| Staging model | Real git index. Checkboxes always reflect `git status` |
| Repo tuning | Automatic: write the commit-graph, enable fsmonitor and untracked cache on large repos. Show a one-line notice and offer an undo command |
| Signing | Not used today. Commits go through `git commit`, so signing works if it is ever enabled |
| Themes | Multiple built-in themes plus user TOML themes, switchable live |
| GitAxon | Separate codebase. Copy its good parts (CLI remote layer, parsers, watcher snapshot idea); do not depend on it |

## 3. Stack

- **Language and build:** Rust 1.97 workspace. Release profile uses thin LTO and `codegen-units = 1`.
- **gix 0.88** (default features + `anyhow`): every read. gitty must **never write the index with gix**, because gix drops the UNTR/FSMN/link extensions.
- **imara-diff** via `gix::diff::blob`. Do not add a separate `imara-diff` dependency.
- **UI:** ratatui 0.30.2 and crossterm 0.29 (sync, no event-stream).
- **Threading:** crossbeam-channel. No tokio.
- **File watching:** notify 8.2 with FSEvents. Never kqueue. The debouncer is our own.
- **Syntax highlighting:**
  - tree-sitter + tree-sitter-highlight, one pinned version, with per-language cargo features.
  - syntect 5.3 (onig backend, never regex-fancy) and two-face 0.5 as the fallback for languages without a grammar.
- **Utilities:** unicode-width 0.2, unicode-segmentation, signal-hook, anyhow / thiserror, serde + toml for config and themes.
- **Rejected:** git2/libgit2, `similar`, arborium, giallo, notify-debouncer-full, tokio.

## 4. Architecture

```
gitty/                         cargo workspace
├── crates/gitty-core/         no UI code; the only crate that uses gix
│   ├── repo.rs                ThreadSafeRepository; resolve the real git binary (not the /usr/bin xcrun shim); repo tuning
│   ├── history/               commit-graph-native walker, arena, lazy row decode
│   ├── refs.rs                badges; two-colour ahead/behind walk
│   ├── commit_files.rs        tree diff → file list; lazy +/- counts
│   ├── diff/                  op-list diff model, intraline, whitespace modes, file classification
│   ├── status.rs              gix status (small repos) or git CLI porcelain v2 (large/fsmonitor)
│   ├── watch.rs               FSEvents + path classifier + debouncer
│   ├── patch.rs               hunk/line patch generator (stage / unstage / discard)
│   └── git_cli.rs             spawn rules, writer queue, commit, network ops, askpass, progress parsing
└── crates/gitty/              TUI binary
    ├── main.rs / app.rs       state + update; main loop
    ├── workers.rs             thread pools, generation-based cancellation
    ├── term.rs                terminal guard (modes, restore, suspend)
    ├── theme/                 theme model, built-ins, TOML loader, 256-colour mapping
    ├── highlight.rs           tree-sitter / syntect workers, span cache
    ├── config.rs              ~/.config/gitty/config.toml
    └── ui/                    commit_list, header, file_list, diff_view (unified/split), changes, commit_box,
                               status_bar, overlays (help, theme picker, branch picker, prompts, toasts)
```

**Boundary rule:** gix types never leak out of gitty-core. Its public API uses gitty-owned types such as `CommitId`, `CommitRow`, `FileChange` and `FileDiff`. gix minor versions break APIs, and this keeps that contained.

### 4.1 Threads
- **main:** blocks on crossbeam `select!` over input, worker results and watcher events. It drains all queues, applies updates, and draws once. There is no tick.
- **input:** reads crossterm events and coalesces bursts of wheel and resize events.
- **walker (1):** owns the history arena for the session.
- **reader (N = cores − 2, min 2):** row decode, tree diffs, blob loads, line stats, search.
- **diff/highlight (2–4):** a separate pool so highlighting never starves history.
- **writer (1):** serialises every mutating git CLI call.
- **watcher:** notify plus the classifier.

Each worker gets `ThreadSafeRepository::to_thread_local()` and reuses its diff resource cache.

### 4.2 Requests and cancellation
- Every request carries `(pane, generation)`. Moving the selection bumps the generation, and stale results are dropped on arrival.
- Long jobs check the generation every N files or hunks.
- A superseded child process is killed by process group.
- Diff requests are debounced by about 30 ms after the last selection change.
- Cache keys never contain timestamps. This avoids gitui bug #2823.

## 5. Data layer

### 5.1 History
- **Walker:**
  - Commits are `u32` commit-graph positions in an arena.
  - Order comes from a newest-first max-heap on commit time.
  - **Hybrid phase:** commits newer than the graph (normal right after any write) are walked from the object database until they join graph positions. They are stored in an overflow list of ObjectIds.
  - If there is no commit-graph, use the object-database walk for the session and write the graph in the background (auto-tuning).
- **Never use topo order.** It is what makes lazygit take 12+ s on the kernel.
- **Scope:**
  - Default is HEAD plus `@{upstream}`.
  - The all-refs toggle walks every local branch, remote branch and tag.
  - Detached HEAD and unborn branches are handled.
- **Streaming:** first 500 rows, then the rest in batches. The UI shows the row count growing.
- **Row decode** (subject, author name and email, author time, co-author trailers): lazy, for the viewport ± 2 screens, on the reader pool. Decoded rows are cached in the arena.
- **Search:**
  - `/` matches summary and author, decoding in parallel and streaming matches.
  - `path:` filters by path via `git log --format=%H -- <path>` until gix reads Bloom filters.

### 5.2 Refs and ahead/behind
- Refs are listed with gix and peeled once. They are re-read only when the watcher reports a ref change.
- **Ahead/behind:**
  - Uses a custom two-colour walk on generation numbers, which marks every ↑ and ↓ commit in one pass.
  - Results are cached by `(local tip, upstream tip)`.
  - Do not use gix `with_hidden`; it measured 2–5× slower.
- **Badges:**
  - Current branch: inverse.
  - Local branches: green.
  - Remote refs: dim, folded into the local badge when the commit is the same.
  - Tags: yellow, the first tag plus `+N`.

### 5.3 Commit files
- Files come from a tree diff of the commit against its first parent (empty tree for a root commit). Rename detection is on only for the selected commit, with `diff.renameLimit` respected.
- `+/-` counts are computed lazily for visible files on the reader pool.
- Merge commits use first-parent diffs. Kernel merges with 10k+ files must render the list before any counts arrive.
- A multi-commit range shows the diff `oldest^..newest`.

### 5.4 Status and refresh
- **Status source:**
  - Index under ~20k entries and no fsmonitor: gix status.
  - Otherwise: `git --no-optional-locks status --porcelain=v2 -z --no-ahead-behind` (≈38–60 ms at 120k files with fsmonitor).
  - These thresholds are provisional until measured on a real kernel checkout.
- **Watcher classes:**
  - Worktree paths (after gitignore): refresh status.
  - `.git/index`: refresh status.
  - `HEAD`, `refs/**`, `packed-refs`, `FETCH_HEAD`: refresh refs, ahead/behind and the history head.
  - Other `.git` churn is ignored.
- **Other triggers:**
  - Terminal FocusGained.
  - A 60 s backstop timer, only while focused.
- **Auto-tuning on large repos:**
  - Write the commit-graph (`--reachable --changed-paths`) in the background, and again after fetches and commits when it goes stale.
  - Enable `core.fsmonitor=true` and `core.untrackedCache=true`.
  - Show a one-line notice. `gitty untune` reverts the config.
  - Auto-tuning never runs on a bare repo.

### 5.5 Caches (commits are immutable, so entries never go stale)

| Cache | Key | Bound |
|---|---|---|
| Decoded rows | commit position | everything decoded, ~100 B/row |
| File lists | commit id | LRU ~1k |
| Diff op lists | (old blob, new blob, algorithm, ws-mode) | LRU ~200 |
| Syntax spans | blob id + language | LRU by line count |
| Ahead/behind | (tip, upstream tip) | invalidated on ref change |

Prefetch: the file lists of the ±10 neighbouring commits once the selection settles.

## 6. Diff engine

### 6.1 Model
A diff is a **view over full op lists**. It never stores patch text.

```
FileDiff {
  old, new: BlobRef                       // oid, or worktree content hash
  class: Text | Binary | Lfs | Submodule | ModeOnly | TooLarge | LargeText | Generated
  ops: Vec<Op>                            // Equal{old_start,new_start,len} | Change{old,new: Range<u32>}
  gaps: Vec<Gap{len, top_shown, bottom_shown}>   // view state, default 3/3
  line_index_old/new: Vec<u32>            // line-start offsets
  flags: no_eol_*, eol_style_*, bidi_warning, ws_mode
  intraline: lazy per Change block → per-line byte ranges
}
```

- Rows, hunk headers and split rows are derived from the ops at render time, and only for the viewport.
- Content is git-normalised (clean filters, autocrlf) so worktree diffs match `git diff`.

### 6.2 Algorithm
- Default is Myers plus the indent heuristic, for parity with git and Desktop.
- Honour `diff.algorithm`, `diff.indentHeuristic` and per-driver algorithm. Histogram is available.
- Use a byte-level prefix/suffix trim before interning, keeping a 3-line margin.

### 6.3 Context expansion
- **Per gap:**
  - If the hidden part is ≤20 lines, a single row expands it.
  - Otherwise `↑ ⋯` shows 20 more lines at the bottom of the gap and `↓ ⋯` shows 20 more at the top.
- The leading gap only expands up. The trailing gap (to end of file) only expands down.
- `e` expands the gap nearest the cursor. `E` toggles the whole file. Clicking an arrow row also expands.
- Expansion is allowed when both blobs are loaded and each is ≤16 MiB.

### 6.4 Intraline highlighting
For each Change block with D deleted and A added lines:
1. **Skip lines** of ≥1024 display chars.
2. **Tokenise once per block:**
   - word runs `[A-Za-z0-9_]` or non-ASCII
   - whitespace runs
   - single punctuation characters
3. **Pairing:**
   - If D×A ≤ 4096: greedy monotone pairing. Scan at most 32 candidates and accept the first with distance ≤0.6 (delta's formula: changed / (changed + 2×equal) over token widths). A cheap prefilter runs first.
   - If D×A is larger: positional pairing when D == A, still subject to the distance check.
4. **Emphasis ranges:**
   - Come from the token Myers diff.
   - Ranges separated only by whitespace are merged.
   - Emphasis covering only leading or trailing whitespace is dropped.
5. **When computed:** lazily for the viewport ± 2 screens, under the job's generation.

### 6.5 Split view
- Rows come from the same pairing. A paired (deletion, addition) shares a row, and unpaired lines get an empty opposite cell. Order is preserved.
- Positional (Desktop) pairing is a config option.

### 6.6 Whitespace modes
- `w` cycles: show all → ignore all (`-w`) → ignore amount (`-b`). `--ignore-cr-at-eol` is a config option.
- The original bytes are always displayed.
- Line and hunk staging is disabled in the ignore modes, and the status bar says why.

### 6.7 File classification (in order)
1. Submodule (gitlink): "Submodule a→b", plus dirty.
2. Mode or type change only: header only.
3. LFS pointer: "LFS object a→b, size x→y".
4. Binary (attribute, or NUL in the first 8000 bytes): sizes only.
5. TooLarge (>64 MiB): never diffed.
6. LargeText (>4 MiB, any line >5000 chars, or >20k changed lines): collapsed with stats. Enter loads it.
7. Generated (`linguist-generated`, lockfiles, minified heuristics, `@generated` / "DO NOT EDIT" markers): collapsed by default.
8. Text.

Edge cases:
- **CRLF:** strip CR for display and keep a per-line bit. A file-wide EOL change shows a banner.
- **No newline at EOF:** a dim marker row.
- **Bidi control characters** in changed lines: a warning.
- **Renames:** shown as "old → new (R087)". A pure rename has no rows.

## 7. Syntax highlighting
1. The diff draws immediately with diff colours only.
2. A diff/highlight worker loads the full old and new blobs and runs tree-sitter-highlight over each whole file. It caches per-line spans as theme **capture ids** (not colours), keyed by blob id.
3. When the spans arrive, the pane redraws.

Fallback and limits:
- Languages without a bundled grammar fall back to syntect (onig) plus two-face.
- Files beyond the LargeText threshold, or highlights exceeding a time budget (progress-callback timeout), stay uncoloured.
- **Never highlight each hunk on its own** (5–92% wrong colours were measured). The only exception is a stopgap while the full-file job runs.
- Language is detected from file extension, filename and shebang, honouring `linguist-language`.
- Grammars are cargo features. The default set covers Rust, TS/TSX, JS, Python, Go, C/C++, JSON, YAML, TOML, Markdown, Bash, CSS, HTML, SQL and Swift. Binary size is measured and recorded.

## 8. Latency budget (warm cache, enforced by benchmarks)

| Operation | Budget | Measured in research |
|---|---|---|
| Launch to layout drawn | <16 ms | — |
| First 500 history rows (kernel) | <50 ms | 25–32 ms |
| Full history walk (kernel) | <400 ms, background | 242–317 ms |
| Keypress to frame | <16 ms | ~1.1 ms full frame |
| Select commit to file list | <5 ms typical, <100 ms merges | p50 0.2 ms |
| Select file to diff drawn (uncoloured) | <10 ms | ~1 ms typical |
| Syntax colours arrive | <100 ms for ≤10k lines | 39–92 ms tree-sitter |
| Ahead/behind, worst case (kernel) | <150 ms | 133 ms |
| Status: small repo / 120k files with fsmonitor | <70 ms / <60 ms | 15–68 / 38–60 ms |
| Idle CPU | 0% | — |

The kernel cold start (~0.9 s to the first rows) is covered by drawing the layout immediately and filling rows as they stream in. It is measured separately after `purge`.

## 9. Rendering and terminal
- **Custom widgets:** every pane writes visible rows directly into the ratatui `Buffer`, with an ASCII fast path. There is no `List` or `Paragraph` over large content.
- **Background colour:** full-width row backgrounds are painted on the row rect before spans are drawn.
- **Frame output:**
  - Each frame is wrapped in synchronized output (`?2026h/l`).
  - Output goes through a 256 KB BufWriter.
  - Use `Viewport::Fixed`, resized on resize events.
- **Mouse:** enable 1000, 1002 and 1006 ourselves. Never 1003 (any-motion) in v1.
- **Keyboard:** kitty keyboard enhancement flags when supported, enabling Ctrl+Enter and unambiguous Esc.
- **Terminal guard** restores everything on exit, panic, SIGTERM and SIGHUP: alt screen, raw mode, mouse, kitty flags, bracketed paste, sync output and cursor.
- **Suspend and resume** for `$EDITOR` and terminal-mode signing retries: restore the terminal, run with inherited stdio, then re-enter.
- **Content sanitising:** tabs expand to `tabSize` (default 4, configurable); control characters and ANSI escapes are escaped; grapheme width uses unicode-width.
- **Colour fallback:**
  - Truecolor when `COLORTERM` is truecolor/24bit.
  - Otherwise theme colours map to the nearest of 256 colours, once at theme load.
  - Light or dark is detected via an OSC 11 query at startup.

## 10. Themes
- **A theme TOML defines:**
  - **UI palette:** background, panel, border, focus, selection, muted, accent, badge colours, ↑ and ↓ colours, status-bar colours.
  - **Diff colours:** add/del row backgrounds, add/del gutter backgrounds, add/del emphasis backgrounds, hunk header, expand rows.
  - **Syntax colours:** a map from tree-sitter capture names (`keyword`, `string`, `function`, `type`, `comment`, …) to styles.
- `inherit = "<theme>"` allows partial overrides.
- **Built-ins:** github-dark (default), github-light, rose-pine, rose-pine-dawn, catppuccin-mocha, catppuccin-latte, tokyo-night, dracula, gruvbox-dark, solarized-dark, solarized-light.
- **User themes:** `~/.config/gitty/themes/*.toml`.
- **Theme picker:** `T` opens a live preview. The choice persists to `config.toml`. `theme = "auto"` picks the light or dark variant from the terminal background.
- **Emphasis strength:** default alpha 0.25 over the row background, which is readable alongside syntax colours. The github-dark theme ships GitHub's exact 0.40 as an option to compare in kitty.
- **Later (v1.1):** importing Helix themes, which use the same capture names.

## 11. UX

### 11.1 Layout by width
| Cols | Layout |
|---|---|
| <120 | One pane at a time: Enter drills down list → files → diff; Esc goes back |
| 120–159 | History on the left; header, file list and unified diff stacked on the right |
| 160–199 | History, then files, then diff. The header spans files and diff |
| ≥200 | Same three columns. Split view is automatic when each half fits (gutter + 50 text cols) |

- **Top bar:** repo, branch, `↑N ↓M`, "fetched 2m ago", and the [1] Changes / [2] History tabs.
- **Bottom bar:** context keys for the focused pane.
- Pane sizes can be dragged and are persisted per repo.

### 11.2 History tab
- **Row:**
  - marker: ↑ yellow, or ↓ cyan with a dimmed row
  - summary, or a dim "Empty commit message"
  - right-aligned badges
  - author initials, coloured by an email hash
  - date
- **Truncation order** as width shrinks: badges, then date, then summary.
- `z` toggles a two-line density mode with author and date, like Desktop.
- **Dates:** compact relative (now, 12m, 5h, 3d) that switches to absolute after 7 days. `D` cycles relative / absolute / both. Relative dates are recomputed only at the thresholds.
- **Header:**
  - bold summary
  - initials, authors and co-authors, short SHA, `+A −D`
  - `o` expands the description and shows the full SHA
  - `y`/`Y` copies the short/full SHA via OSC 52
- **File list:**
  - coloured status letter (A, M, D, R→, C)
  - dim directory, bright filename, right-aligned `+n −m`
  - Desktop-style middle truncation by display width
  - `t` toggles tree view
- **Multi-select:** `V` plus `j`/`k`, or Shift-click, selects a contiguous range.
- **Compare:** `b` opens a fuzzy branch picker. It has Behind (N) / Ahead (M) / Files tabs; Files is the merge-base diff.
- **Search:** `/` searches summary and author; `path:` filters by path; `n`/`N` move between matches.

### 11.3 Changes tab
- **File rows:** `[x]` / `[ ]` / `[~]` reflect the real index. The header checkbox toggles all.
- **Filter (`F` while the file list is focused):** included, excluded, new, modified, deleted.
- **Diff:**
  - ✓ in the gutter on staged lines.
  - `Space` toggles the line, `v` selects a range, `H` toggles the hunk.
  - Click or drag in the gutter selects lines.
  - `d` discards, after a confirmation.
- **Commit box:**
  - summary with a counter (yellow >50, red >72) and placeholder "Update <file>"
  - expandable description
  - co-authors line, written as trailers
  - button "Commit N files to <branch>"
  - `c` focuses the box. Alt+Enter commits, as does Ctrl+Enter with kitty flags.
  - `A` toggles amend, with a warning banner.
  - After a commit, the bar shows "Committed just now · [u] Undo".

### 11.4 Keys
| Keys | Action |
|---|---|
| `j/k`, `↓/↑`, `g/G`, `Ctrl-d/u`, `PgDn/PgUp` | move, top/bottom, half page, page |
| `h/l` | horizontal scroll in the diff |
| `Tab`/`S-Tab` | next/previous pane |
| `1` / `2` | Changes / History |
| `Enter` / `Esc` | drill in / back |
| `[` `]` / `{` `}` | previous/next hunk / previous/next file |
| `e` / `E` | expand context near the cursor / toggle whole file |
| `s` / `w` / `W` | split view / whitespace mode / wrap |
| `F` | full-screen diff (in diff pane); filter (in Changes file list) |
| `f` / `p` / `P` | fetch / pull / push |
| `c` / `A` / `u` | commit box / amend / undo last commit |
| `Space` / `a` / `v` / `H` / `d` | toggle / toggle all / visual range / toggle hunk / discard |
| `/`, `n`/`N` | search, next/previous match |
| `y` / `Y` | copy short / full SHA |
| `o` | expand the commit header |
| `t` / `D` / `z` | tree view / date mode / density |
| `b` | compare with a branch |
| `r` | toggle history scope (branch+upstream ↔ all refs) |
| `T` | theme picker |
| `O` | open the file in the external difftool |
| `<` / `>` | resize the focused pane |
| `?` / `q` | help / quit |

`F` is context-sensitive, and that is intentional. All bindings are remappable in config.

### 11.5 Mouse
- **Selecting:**
  - Click selects.
  - Shift-click extends.
  - Ctrl-click toggles a commit in a multi-select.
- **Scrolling:**
  - The wheel scrolls the pane under the pointer.
  - Bursts are merged into one frame.
- **Checkboxes and line selection:**
  - Clicking the checkbox column toggles.
  - Dragging in the gutter selects lines, auto-scrolling at the edges. In split view, the side is chosen by x position.
  - The one-column hunk handle toggles the whole hunk.
- **Context and panes:**
  - Clicking `↑ ⋯` / `↓ ⋯` expands.
  - Dragging a border resizes panes.
- **Other:**
  - Double-click a file to open `$EDITOR` at that line.

## 12. Write path

### 12.1 Spawn rules (every git call)
- Use the resolved real git binary.
- Read-only calls use `GIT_OPTIONAL_LOCKS=0`.
- Set `GIT_TERMINAL_PROMPT=0`.
- Spawn in a new session via `setsid` in `pre_exec`, and kill by process group.
- Diff and patch calls add `--no-color --no-ext-diff --no-textconv --no-relative --src-prefix=a/ --dst-prefix=b/ -U3 -c diff.noprefix=false -c core.quotepath=false`.
- All writes go through the single writer thread.
- A stale `index.lock` produces a clear error, with an offer to remove it only if no git process is running.

### 12.2 Staging (real index)
- **Patch base:**
  - Stage: an index → worktree diff.
  - Unstage: a HEAD → index diff.
  - Always regenerated at U3. Never U0.
- **Line rules (Desktop):**
  - Selected lines are kept.
  - An unselected `−` becomes context.
  - An unselected `+` is dropped.
  - Context passes through.
- **Headers:**
  - Forward patches keep `old_start` and take `new_start` from a running offset.
  - Reverse patches keep `new_start` and derive `old_start`.
- **EOL fix:** a target-side line marked no-EOL that is not the last target-side line is rewritten. This prevents the `a\nbc` corruption that lazygit and Desktop share.
- **Apply:**
  - Stage: `git apply --cached --whitespace=nowarn -`.
  - Unstage: add `-R`.
  - The patch goes on stdin.
- **TOCTOU guard:** check the blob ids in the patch against the current index and worktree before applying. On a mismatch, re-diff instead of applying.
- **Special cases:**
  - Untracked files: partial staging via a `/dev/null` base. No `git add -N`.
  - Whole-file selections:
    - stage: `git add`
    - stage a deletion: `git rm --cached -q`
    - unstage: `git restore --staged`
    - unstage a new file: `git rm --cached -q -f`
    - Alternatively, emit correct new/deleted-file headers.
  - Renames: diff both paths and rewrite the header.
  - Executable new files: fix the mode.
  - Paths in headers use git C-style quoting.
- **Whole-file only:** binary, mode-only, submodule, LFS, conflicted, and whitespace-ignored views.
- **Discard lines:** the same generator applied with `git apply -R` to the worktree. It has its own fuzz suite and always asks for confirmation.

### 12.3 Commit
- **Command:** `git commit -F -` (message on stdin), plus `--amend`. Use `--cleanup=strip` when `#` comments are allowed.
- **Hooks:** their output streams to a log panel. On failure, the full output opens in a modal.
- **Undo:** `git reset --mixed HEAD^` restores the message into the box. It is only offered for the latest commit when that commit is unpushed.
- **Signing:**
  - Signing runs through git, so whatever the user configures applies.
  - SSH keys with a passphrase use the askpass trampoline (§12.4).
  - If GPG signing fails without pinentry-mac, offer "retry in terminal": suspend, set `GPG_TTY`, run with inherited stdio, then restore.

### 12.4 Network
- **Commands:**
  - `git fetch --progress --prune`
  - `git pull --no-edit --progress --ff-only`. On divergence, offer merge or rebase.
  - `git push --progress --porcelain`
- **Askpass trampoline:**
  - Set `GIT_ASKPASS` and `SSH_ASKPASS` to the gitty executable, with `SSH_ASKPASS_REQUIRE=force`, `DISPLAY=:0` and `GITTY_ASKPASS_SOCK`.
  - The helper process relays username, password, SSH passphrase and host-key yes/no prompts to masked TUI prompts.
  - `credential.helper` (osxkeychain) keeps working.
- **Progress:**
  - Parse stderr split on `[\r\n]`.
  - Phase weights follow Desktop. Fetch: compress 0.1, receive 0.7, resolve 0.2. Push: compress 0.2, write 0.7, resolve 0.1.
  - Updates are throttled to the frame rate.
- **Auto-fetch:**
  - Every 5 min while focused (configurable, can be disabled).
  - Uses `GIT_SSH_COMMAND='ssh -o BatchMode=yes'` with no askpass.
  - On an auth failure, mark the remote "needs auth" and stop auto-fetching it.
- **Cancellation:** all network jobs can be cancelled.

## 13. Errors
- **Error types:**
  - gitty-core's public API returns `anyhow::Result`. Typed, downcastable errors (`GitError` for CLI failures) are defined with `thiserror`. gix 0.88's `Exn` errors convert cleanly only into anyhow.
  - `anyhow` in the binary.
- **Bad repo data** never panics.
- **Panics:** the panic hook restores the terminal, then prints.
- **User-facing failures:**
  - They appear as a toast with an expandable detail, and the app stays usable.
  - Git stderr is shown verbatim in the detail.

## 14. Configuration
`~/.config/gitty/config.toml`:
- theme, tab size, diff algorithm override
- whitespace default, split threshold
- date mode, density
- auto-fetch interval, auto-tune on/off
- keymap overrides
- external difftool

Per-repo UI state (pane sizes, history scope) lives in `~/.local/state/gitty/`. gitty never writes UI state into the repo.

## 15. Testing and benchmarks
- **Property tests:**
  - Patch generator (stage, unstage, discard) against real `git apply`. Inputs are random files including CRLF, missing EOF newline, new/deleted files, renames and quoted paths.
  - Intraline pairing invariants.
  - Gap and expansion math.
- **Unit tests:** date formatting, path truncation, progress parsing, porcelain v2 parsing, theme loading and inheritance, 256-colour mapping.
- **Integration tests:** fixture repos built by a script covering:
  - renames, CRLF, binary, LFS pointer, submodule
  - merges, root commit
  - unborn branch, detached HEAD
  - conflicted index, no upstream, diverged branches
- **Parity tests:** gitty ops against `git diff` hunks across a slice of git/git history.
- **Snapshot tests:** `TestBackend` + `insta` per layout width (100/140/180/220 cols) × github-dark and github-light.
- **Benchmarks:**
  - `criterion` for the walker, ahead/behind, tree diff, line diff, intraline, highlight and frame render.
  - `bench/run.sh` (`hyperfine`) against git/git and a kernel clone, checking the §8 budget.
  - A cold-start run after `purge`.

## 16. Delivery milestones
1. **M1 Core read path:**
   - Workspace, gitty-core repo/refs/walker/ahead-behind/commit-files, a CLI `gitty-core` bench harness.
   - Parity and benchmark tests.
2. **M2 History TUI:**
   - Terminal guard, event loop, workers.
   - Commit list, header, file list, diff (unified, intraline, expansion, special files).
   - Themes (built-ins and loader), mouse, layouts.
3. **M3 Syntax highlighting** and split view.
4. **M4 Changes tab:**
   - Status, watcher.
   - Patch generator with fuzzing, staging UI, commit box, amend/undo, hooks log.
5. **M5 Network:** fetch/pull/push, askpass trampoline, progress, auto-fetch, auto-tuning.
6. **M6 Polish:**
   - Search, compare-to-branch, multi-select, config/keymap, help overlay.
   - Benchmarks enforced, README.

## 17. Open risks
- **Kernel-scale working-tree status** has not been measured on a real checkout. Status thresholds are provisional.
- **The hybrid walker** (commits not yet in the graph) and incremental history refresh were designed but not built during research.
- **Search over 1.5M commits** may take seconds. A persistent row cache is the fallback.
- **Tree-sitter on huge or minified files** needs timeouts and caps, measured in M3.
- **Untested write paths:**
  - GPG signing
  - SSH askpass and host-key prompts
  - quoted paths
  - sparse/skip-worktree entries
- **Wide characters:** emoji and ZWJ width can disagree between unicode-width and kitty. The impact is contained per rect.
- **gix API churn:** isolated behind gitty-core's public types.
