# M4 Changes Tab — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** The Changes tab shows working-tree status live and stages files, hunks and single lines into the real index. It also commits, amends and undoes, and streams hook output. Every write is safe against user config, concurrent CLI use and the EOL corruption that lazygit and Desktop share.

**Architecture:**
- **gitty-core gains four modules:**
  - `git_cli` — spawn rules and the write commands.
  - `status` — a porcelain v2 parser.
  - `stage` — the staging model and patch writer.
  - `watch` — FSEvents watcher, classifier and debouncer.
- **The `gitty` crate adds:**
  - a writer thread;
  - a watcher thread feeding the main `select!`;
  - Changes-tab state, input and rendering;
  - a commit box with a small text editor.

**Staging model (one primitive).** Each Changes-tab file has three texts: HEAD blob, index blob, and the worktree file converted to git form (gix filter pipeline: autocrlf, eol, clean filters).
- **Display:** the diff shown is HEAD → worktree (`FileDiff::from_bytes`). Each changed line is marked staged or unstaged, derived from the HEAD→index and index→worktree alignments.
- **Every staging action works the same way:**
  1. Compute the desired index text `T = build(HEAD, WT, staged set)`. Within a change block this keeps the unstaged deletions, then adds the staged additions (Desktop's "old, new" order). A line with no EOL that is followed by another line gets a `\n` (the EOL fix).
  2. Write a unified patch `index → T` at U3 using gitty's own diff ops.
  3. Apply it with `git apply --cached --whitespace=nowarn -`.
- Stage and unstage are the same operation, so no reverse patch and no `-R` is needed.
- **Divergence check.** If `build(HEAD, WT, derived staged set) != index`, the index holds content that matches neither side (e.g. staged with `git add -p` and then edited). Line toggles are then disabled for that file, with a notice; whole-file toggles still work.

**Tech Stack:** notify 8.2 (FSEvents only); the git CLI for every write; gix for reads, filters and excludes.

**Spec:** docs/superpowers/specs/2026-10-02-gitty-design.md, mainly:
- §5.4 status and refresh
- §11.3 Changes tab
- §11.4 keys, §11.5 mouse
- §12.1 spawn rules, §12.2 staging, §12.3 commit
- §8 budgets

Research notes: docs/research/2026-10-02-gitty-research.md §4.7, §4.8, §8.1–8.3 and §9.3.

## Global Constraints
- **Spawn rules for every git call:**
  - the resolved git binary;
  - `GIT_TERMINAL_PROMPT=0`;
  - reads add `GIT_OPTIONAL_LOCKS=0` / `--no-optional-locks`;
  - each process runs in a new session (`setsid` in `pre_exec`) and is killed by process group when superseded.
- **Writes** go through one writer thread, in order. A stale `index.lock` gives a clear error.
- **Never** let gix write the index, and never call gix `write_changes()`.
- **Status:** `git --no-optional-locks status --porcelain=v2 -z --no-ahead-behind --untracked-files=all`. Results are sorted by path.
- **Watcher:**
  - Ignore `.git/objects/**`, `.git/logs/**`, `*.lock`, `fsmonitor--daemon*`, `COMMIT_EDITMSG`, `AUTO_MERGE*` and gitignored worktree paths.
  - Debounce: 50 ms after the first event, extended while events arrive, at most 300 ms.
  - Drop INDEX events whose `(mtime, size, ino)` fingerprint matches the one recorded after gitty's own status.
- **Refresh triggers:** FocusGained; a 60 s backstop only while focused; and after every write.
- **Whole-file only:** binary, mode-only, submodule, LFS, conflicted, whitespace-hidden views, and divergent files.
- **Whole-file commands:**
  - stage: `git add -A -- <p>`
  - unstage: `git restore --staged -- <p>`
  - on an unborn HEAD, unstage: `git rm --cached -q -r -- <p>`
- **Commit:** `git commit -F -` with `--amend` when amending. Hook stderr streams to a log. Undo is `git reset --mixed HEAD^`, or `git update-ref -d HEAD` for a root commit, and is offered only when HEAD is unpushed.
- **Discard lines** writes the worktree file directly (mode kept) with `build(HEAD, WT, kept lines)`. It is allowed only when the file's git form equals its raw bytes; otherwise discard is whole-file only. It always asks for confirmation.

## Review Focus
1. Staging never corrupts the index: no-EOL last lines, CRLF files, new or deleted files, a file emptied by a partial unstage, quoted paths, an unborn HEAD.
2. TOCTOU: the index changes between diff and apply (concurrent `git add`). The apply must fail or re-diff, never stage the wrong lines.
3. Discard touches only the selected lines, keeps the file mode, refuses files that need conversion, and is never one keystroke from data loss.
4. The watcher never self-triggers in a loop (gitty's own status and index writes), and a `cargo build` in an ignored `target/` causes no status refreshes.
5. The UI stays responsive while writes, hooks or a slow status run. Stale status or diff results are dropped by generation.

## Tasks
- [ ] **T1 git_cli + status** (`crates/gitty-core/src/{git_cli,status}.rs`).
  - **API:**
    - `GitCli::new(&Repo)`; `cmd(args, Kind::{Read,Write}) -> Command` (spawn rules applied);
    - `run(cmd, stdin: Option<&[u8]>, on_stderr: &mut dyn FnMut(&str)) -> Result<Output, GitError>`;
    - `status() -> Status { branch: Option<String>, entries: Vec<StatusEntry> }`;
    - `StatusEntry { path, orig_path, x: char, y: char, kind: Ordinary|Renamed|Unmerged|Untracked, head_mode, index_mode, wt_mode, head_blob, index_blob }`;
    - `StatusEntry::check() -> Check::{Staged, Unstaged, Partial}`;
    - whole-file `stage_paths`, `unstage_paths`, `stage_all`, `unstage_all`; `commit(msg, amend, on_stderr)`; `undo_commit()`; `head_message()`.
  - **Tests:**
    - porcelain v2 parse: ordinary, rename, unmerged, untracked, paths with spaces/newlines/unicode under `-z`;
    - fixture repos: unborn HEAD, rename staged, conflict;
    - whole-file stage/unstage round trips;
    - commit with a failing hook returns its stderr;
    - undo restores the index and returns the message;
    - `setsid` (child sid != parent sid).
- [ ] **T2 stage model + patch writer** (`crates/gitty-core/src/stage.rs`).
  - **API:**
    - `Texts { head, index, wt }`, all `Arc<Text>`;
    - `Handle::stage_texts(&StatusEntry) -> Result<Texts>` (worktree via the gix filter pipeline; also returns whether git form == raw);
    - `staged_set(&Texts, &[Op]) -> Option<Vec<bool>>` (per changed line of HEAD→WT, in op order; None = divergent);
    - `build(head, wt, ops, staged: &[bool]) -> Vec<u8>`;
    - `unified_patch(path, old: &Text, new: &[u8], new_file_mode: Option<u32>) -> Vec<u8>`;
    - `GitCli::apply_cached(patch, expect_index_blob: Option<BlobId>)`, which checks the index entry first.
  - **Tests:**
    - property test vs real `git apply --cached`: random HEAD/WT texts (CRLF, no-EOL, empty, new file, deleted file), random staged sets; after apply, `git show :p` == `build(...)`, and `staged_set` of the new state reproduces the selection;
    - C-quoted paths;
    - TOCTOU: the index changes before apply, so apply errors and nothing is staged;
    - divergence detection.
- [ ] **T3 watcher** (`crates/gitty-core/src/watch.rs`).
  - **API:** `Watcher::spawn(&Repo, tx: Sender<Changed>)`, where `Changed` is a bitflag of WORKTREE, INDEX, REFS, REMOTE, STATE, CONFIG and IGNORE_RULES.
    - `classify(rel: &Path, in_git_dir) -> Class`
    - a `Debouncer` with an injectable clock
    - `IndexFingerprint::of(path)` and `Watcher::note_own_status(fp)`
  - **Tests:**
    - classifier table;
    - debouncer timing (50/300 ms) with a fake clock;
    - integration: touching a file emits WORKTREE; writing in an ignored dir emits nothing; `git add` emits INDEX.
- [ ] **T4 wiring.**
  - **Requests and messages:**
    - `Request::Status{generation}` → `Msg::Status`
    - `Request::StageTexts{generation, entry}` → `Msg::ChangesDiff{key, diff, texts, staged}`
    - `Request::Write(WriteOp)` on a new writer thread → `Msg::WriteDone{op, result, log}`
    - watcher events → `Msg::Changed(mask)`
  - **Run loop:** enable focus-change reporting; FocusGained refreshes status and refs; a 60 s backstop while focused. REFS changes refresh refs and restart the walk, keeping the selection.
  - **App:** Changes-tab state (entries, selection, file generation, staged marks, divergent flag, filter).
  - **Tests:** app tests for status → selection → diff request; stale drops; write → refresh; watcher msg → status request.
- [ ] **T5 Changes UI.**
  - **Layout:** file list (with checkboxes and a header toggle-all) over the commit box on the left, diff on the right; narrow mode drills in.
  - **Diff:** ✓ gutter marks on staged lines.
  - **Keys:**
    - `Space` toggles the line; `v` + `Space` toggles a range; `H` toggles the hunk; `a` toggles all; `d` discards after confirmation;
    - `F` cycles the filter in the file list.
  - **Mouse:** click a checkbox; click or drag in the gutter.
  - **Notices:** whole-file-only and divergent files.
  - **Tests:** render snapshots; checkbox click; line toggle issues the right Write; discard confirm flow; whitespace-hidden disables selection.
- [ ] **T6 commit box.**
  - **Editor:** summary, description and co-authors fields (a single-line and multi-line editor: insert, delete, arrows, Home/End, word left/right, paste).
  - **Box:**
    - counter yellow >50, red >72;
    - placeholder "Update <file>";
    - button "Commit N files to <branch>" or "Amend last commit";
    - `c` focuses, Esc leaves; Alt+Enter or Ctrl+Enter commits.
  - **Amend:** `A` toggles it, shows the banner and loads HEAD's message.
  - **Hooks:** a log panel streams hook output; a failure opens a modal.
  - **After commit:** a bar shows "Committed just now · [u] Undo"; `u` undoes and restores the message.
  - **Tests:** editor unit tests; commit/amend/undo end to end against a fixture; hook failure modal; snapshot.
- [ ] **T7 measure.** Status latency (git repo, gitty repo), save-to-refresh latency through the watcher, stage-a-line latency. Record in bench/README.md.
