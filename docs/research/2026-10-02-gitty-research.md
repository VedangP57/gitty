# gitty: Research Report (Lead Architect Synthesis)

Date: 2026-10-02. Sources: 8 research tracks (gix-backend, diff-engine, syntax-highlight, tui-render, competitor-internals, staging-writes, status-watch, github-desktop-ux). An adversarial verification pass checked each track.

Evidence tags used throughout:
- **[M]** measured in a probe during this research.
- **[S]** sourced from code, official docs or issue threads.
- **[I]** inferred.
- **[U]** uncertain or unverified.

Claims that verification refuted have been dropped or corrected; Section 11 lists them.

Benchmark conditions: every benchmark ran on an Apple Silicon Mac at load average 5-58, because other tracks ran at the same time. Wall times are the minimum warm figure unless marked; ratios held across reruns. Absolute numbers on an idle machine should be equal or lower. Cold-cache numbers are much worse, and are called out where they were measured.

---

## 1. Executive summary

gitty can be the fastest GitHub-Desktop-style git client for three reasons, all of which competitors get wrong:

1. It never does whole-history work before the first paint.
2. It never blocks the UI thread on git or on parsing.
3. It renders only what is on screen.

Measured headroom is large:
- First 500 commits of the Linux kernel arrive in about 25-32 ms warm.
- A full walk of 1.48M commits takes 0.24-0.32 s with a custom commit-graph walker.
- Changed-file lists take about 0.2 ms per commit.
- A full 400x110 frame costs about 1.1 ms.
- A typical file diff takes about 0.1 ms.

The remaining slow parts are:
- Cold start: about 0.9 s to the first 500 Linux rows on a cold page cache.
- Working-tree status on very large trees without fsmonitor.
- Syntax highlighting of big files.
- Anything that requires topo-order or lacks a commit-graph.

gitty can also be the best. Competitor trackers and source show these are the most-requested missing diff features:
- Word-level intraline highlights.
- Split view.
- Highlighting that is correct across hunks.
- Context expansion.
- Absolute dates.
- Full-screen diff.
- Line staging with a good diff.
- Mouse drag-to-stage.

All of them fit the design below.

### The 10 decisive decisions

1. **gix 0.88 for reads; the git CLI for every write.** gix 0.88.0 is current (2026-09-25, MSRV 1.88). Reads include refs, rev-walks, tree diffs, blobs and small-repo status. Writes include index mutation, commit, fetch/pull/push and hooks. **Never let gix write the index**: gix-index 0.56 only writes the TREE, sdir and EOIE extensions, so a write drops UNTR, FSMN, REUC and split-index `link`, and it cannot write v4 [S].
2. **Commit-graph-native history walker, commit-time order, streamed.** Use gix-commitgraph u32 positions with a time-ordered heap. Use a hybrid ODB fallback for the newest commits, which are normally not in the graph yet. Never use topo-order: it is the measured cause of lazygit's multi-second load on the kernel. If no commit-graph exists, offer to write one.
3. **Arena of positions; lazy row decoding.** Store about 4 bytes per commit and decode subject, author and time (about 12-16 µs each [M]) only for the viewport plus a prefetch margin, on a worker. Search decodes in parallel.
4. **Custom two-colour ahead/behind walk on generation numbers.** It takes 4-135 ms and gives exactly git's counts [M]. It also marks the ↑/↓ rows in the same pass. Do not use gix `with_hidden`, which is 2-5x slower than git.
5. **Status triggered by events, not by polling.** Use notify 8.2 with FSEvents, your own path classifier and debouncer, plus terminal FocusGained.
   - Small repos: gix status.
   - Large repos: `git --no-optional-locks status --porcelain=v2 -z --no-ahead-behind`, with fsmonitor and the untracked cache. That takes 38-60 ms at 120k files, against 250-320 ms for gix [M].
6. **In-process diff: imara-diff through `gix::diff::blob`, as a view over full op lists.** Store Equal/Change ops over the complete blobs. Context size and expansion become view state at O(1) cost, with no re-diff and no patch text. Default to Myers plus the indent heuristic (git and Desktop parity), honour `diff.algorithm`, and offer Histogram.
7. **Better-than-Desktop intraline highlights.** Pair lines greedily by similarity (delta-style, distance ≤ 0.6) and diff word tokens with Myers. Cap the work per block. This highlights the 32% of blocks with unequal line counts that Desktop and diff-highlight skip [M].
8. **Syntax highlighting: render first, colour later.** On a worker, run tree-sitter over the full old and new blobs, and cache per-line spans keyed by blob id. Fall back to syntect with the onig backend plus two-face. Highlighting each hunk on its own is wrong on 5-92% of lines [M], so never do it except as a stopgap.
9. **Custom Buffer widgets, no tick, coalesced input, synchronized output.**
   - Write only visible rows into the ratatui Buffer, with an ASCII fast path.
   - The main thread blocks in crossbeam `select!` and drains every queue before drawing once.
   - Each frame is wrapped in `?2026h/l` and written through a 256 KB BufWriter.
   - Use `Viewport::Fixed` and resize only on resize events.
   - Mouse uses 1000/1002/1006, not 1003 (any-motion).
10. **Own the line-staging patch generator.** Use Desktop's per-line rules, lazygit's header math, an EOL fix (both reference implementations corrupt blobs) and exact reverse headers. Always use 3 lines of context, and pipe the patch to `git apply --cached [-R] --whitespace=nowarn -`. The fuzz passed 589/589 cases in each direction [M].
    - Staging model: the real index, as lazygit does.
    - Spawn git with `setsid`, an askpass trampoline and `GIT_OPTIONAL_LOCKS=0` on reads.
    - Use the real git binary, not the `/usr/bin/git` xcrun shim, which adds about 4 ms per spawn.

---

## 2. Measured numbers (verified only)

"git CLI" means wall time for the whole process. "gix" means in-process time unless noted. Repos:
- **git**: git/git without a commit-graph, 85,887 commits, 4,857 files.
- **git-cg**: the same repo with a commit-graph.
- **linux**: blobless kernel clone, 1,484,291 commits, with a commit-graph.
- **big**: synthetic tree of 120k files. It has `index.skipHash=true`, which flatters its index I/O.

| Operation | Repo | gix / gitty approach | git CLI | Others | Source |
|---|---|---|---|---|---|
| First 500 commits, all refs, ByCommitTime, decoding message and author | git | 12-16 ms | `log --all -n500` 40-42 ms; `--date-order` 549-805 ms | — | gix-backend `walk` |
| same | git-cg | 7-9 ms | 44-45 ms; `--date-order` 83-127 ms | — | gix-backend |
| same | linux | 25-32 ms warm (about 20 ms of it is resolving and peeling 945 refs); **about 930 ms cold** | 57-65 ms; `--date-order` 1.29-1.71 s | — | gix-backend + verifier |
| `log --topo-order -300` (HEAD) | linux | n/a (never do this) | 18-37 s with no graph; 1.4-2.2 s with a graph; date order 65-250 ms | lazygit default is topo-order (12 s per its maintainer [S]) | competitor verifier |
| Full walk, ids only | linux | custom cgwalk **242-317 ms**, 179 MiB RSS; gix `rev_walk` 1.1-1.5 s | `rev-list --all --count` 0.91 s | — | gix-backend |
| Full walk with the commit-graph off | linux | 15.5 s, 1.08 GB [U: not re-run] | 17.9 s | — | gix-backend |
| Full walk | git-cg | cgwalk 22 ms; rev_walk 36-53 ms | 93 ms | — | gix-backend |
| Decode message and author | git / linux | about 12 / 16 µs per commit (mostly object lookup and inflate) | log --format: same order | — | gix-backend |
| Changed-file list per commit (tree diff) | git-cg | p50 0.16-0.25 ms, p99 1-2 ms; file counts equal git's | 121 ms for 500 commits with `--name-status` | — | gix-backend |
| +/- line counts, 500 non-merge commits | git-cg | 643-711 ms CPU on 1 thread; 184-290 ms wall on 8 threads; totals exact | `--numstat` 430-590 ms CPU | — | gix-backend |
| Ahead/behind master...v6.0 (360,606) | linux | custom 2-colour walk 133-135 ms CPU; gix `with_hidden` 1.74-2.2 s | `rev-list --left-right --count` 0.43-0.44 s | — | gix-backend |
| Ahead/behind, typical branch gaps | git-cg, linux | 4-42 ms | 20-90 ms | — | gix-backend |
| Ref listing with peeling | git / linux | 3 ms / 20 ms | for-each-ref 24-47 ms / 22-123 ms (two probes disagree) | — | gix-backend, status-watch |
| Status, clean | git (4.9k files) | 15-68 ms | 30-98 ms | — | gix-backend, status-watch |
| Status | big (120k files) | 251-320 ms (about 900 ms CPU) | plain 225-350 ms; fsmonitor + untracked cache 38-60 ms; with `-uall` and the cache config mismatched, 145-184 ms | — | status-watch + verifier |
| Status after touching 30k files (stale stat data) | big | 0.74-1.0 s on every run | `--no-optional-locks` 1.8-2.1 s on every run; after one locked status, 158-168 ms | — | status-watch |
| Line diff, 10.7 MB / 397k lines | allc | imara Histogram 70-160 ms | `--no-index --histogram` 0.18 s | similar 3.2: 1.3-3.1 s | diff-engine + verifier |
| Line diff, typical file (diff.c) | git | 0.7-1.5 ms | — | similar 4.4-75 ms | diff-engine |
| 1,683 file pairs (v2.45 to v2.50) | git | 70-129 ms total; worst file 4.5-6.7 ms | — | similar 354-2225 ms | diff-engine |
| Byte-level prefix/suffix trim before interning | git | 1.86x faster on a per-commit batch; 1.18x on the release batch | — | — | diff-engine verifier |
| Hunk parity vs `git diff --histogram -U0` | git | 29,344 of 29,411 hunks identical (10 of 1,689 files differ) | — | — | diff-engine |
| Intraline, all 15,538 blocks | git | imara word-Myers greedy 181-231 ms total; uncapped worst block 6-9 ms; p50 about 2.4 µs [U: p50 not re-run] | — | Desktop prefix/suffix 0.7 ms, but covers only 68% of blocks | diff-engine |
| Whitespace-ignore diff (WsKey interning) | allc | 158 ms; counts equal `git diff -w` or within 0.1% | — | — | diff-engine |
| Whole-file syntax highlighting | diff.c, 7.9k lines | tree-sitter 39-70 ms | — | syntect-onig 249-292 ms; syntect-fancy 734-841 ms; giallo 382 ms | syntax-highlight + verifier |
| Whole-file syntax highlighting | app-store.ts, 10.9k lines | tree-sitter 46-92 ms | — | syntect-onig + two-face 0.9-2.2 s; syntect-fancy 6.0 s | syntax-highlight |
| tree-sitter query over a 60-line window / incremental reparse | C, Rust, TS | 0.07-0.2 ms / 3.3 ms (full parse 29 ms) | — | — | syntax-highlight |
| Lines wrongly coloured when each hunk is highlighted alone | C / Rust / TS | tree-sitter 5.0 / 10.1 / 14.0% | — | syntect 8.0 / 7.7 / 91.6% | syntax-highlight |
| Binary size added | — | tree-sitter + C, Rust, JS, TS grammars +4.59 MB | — | syntect-onig + two-face +2.06 MB; arborium +5.53 MB | syntax-highlight |
| Render the commit list (1M rows), 200x60 / 400x110 | — | direct ASCII 41-94 µs | — | `set_stringn` 164-328 µs; List of visible rows 187-485 µs; List of all 1M rows 52-71 ms | tui-render |
| Render a 50k-line diff | — | direct 55-144 µs | — | Paragraph of visible rows 227-509 µs; Paragraph of all rows with `.scroll` 7.2-7.5 ms | tui-render |
| Full frame (render + buffer diff + escape encoding) | — | 0.36 ms / 1.1 ms (diff scroll) | — | — | tui-render |
| ratatui buffer diff | — | about 8-9 ns per cell (0.1-0.36 ms at 12k-44k cells, even with no changes) | — | — | tui-render |
| Escape output per 1-line scroll | — | diff 25 KB / 61 KB; list 10-19 KB | — | — | tui-render |
| Sanitize + tab-expand + measure width | — | 2.7 ms per 50k ASCII lines; 12.9 ms per 50k mixed-Unicode lines | — | — | tui-render |
| `git apply --cached` | git (4.9k entries) | — | 14.6 ms through the shim (about 10 ms with the real binary) | gix in-process staging 2.8 ms (**rejected**, see §4) | staging-writes |
| `git apply --cached` | synthetic 90k-entry index | — | 31 ms; 15 ms with index v4 + skipHash | — | staging verifier |
| `/usr/bin/git` xcrun shim overhead | — | — | +4 ms per spawn (`--version` 8.4 vs 4.4 ms) | — | staging-writes |
| `commit -F -` / `--amend --no-edit` | git | — | 25 / 24 ms | — | staging-writes |
| Staging fuzz (U3, CRLF, missing final newline) | random | 589/589 stage and 589/589 unstage byte-exact (rerun: 291/291) | — | lazygit HEAD corrupts `a\nb` + staged `+c` into `a\nbc` | staging-writes |
| FSEvents watcher (notify 8.2) | big | setup 4.4 ms, 0 extra fds, event latency 4.5-12.5 ms | — | kqueue backend: 3.1 s, then EMFILE at 61,437 fds | status-watch |
| gitignore check (gix excludes) | — | about 5 µs per path | — | — | status-watch |
| Events from a 3,000-file checkout | big | 9,215 events over 1.27 s, 0 rescans | — | — | status-watch |

---

## 3. Recommended stack

| Crate | Version | Features / notes | Evidence |
|---|---|---|---|
| `gix` | **0.88.0** (MSRV 1.88) | Defaults plus `"anyhow"`. Defaults already include `max-performance-safe`, blob-diff, revision, status, dirwalk, parallel and sha1. **`max-performance` is identical to the default in 0.88, so it adds nothing.** With `default-features = false` you must add `sha1` explicitly, or gix-hash fails to compile. Without `anyhow`, `?` from gix's `Exn` errors works only into `Box<dyn Error + Send + Sync>`. | [M][S] |
| `gix::commitgraph` | re-exported by gix | Use `repo.commit_graph_if_enabled()`, which respects `core.commitGraph=false` and returns None when there is no graph. | [S] |
| `gix::diff::blob` (gix-imara-diff 0.3.0) | through gix-diff 0.68 | Don't add the separate `imara-diff` 0.2 crate. The fork has the same speed, is a superset of 0.2's API, and includes `sources::words` and `Hunk::latin_word_diff`. | [M][S] |
| `ratatui` | **0.30.2** | Default features. Optional `scrolling-regions` later (for SSH). Avoid APIs marked `#[instability::unstable]` unless you pin them. | [S] |
| `crossterm` | **0.29.0** | Default (sync, no `event-stream`). | [S] |
| `crossbeam-channel` | 0.5.x [U: version not checked] | `select!`, `after`, bounded channels. | — |
| `notify` | **8.2.0** | Default `macos_fsevent`. **Never `macos_kqueue`.** Skip notify-debouncer-full (on macOS it walks and stats the whole tree) and notify-debouncer-mini; write your own about 60-line debouncer. 9.0.0-rc.5 adds `with_fsevent_latency`; re-evaluate when it is stable. | [M][S] |
| `tree-sitter` / `tree-sitter-highlight` | Benchmarked at **0.26.13**; 0.27.0 is current | Pin one version; grammar crates pin an ABI. Check the 0.26 to 0.27 changelog before upgrading. Grammars tested: tree-sitter-c 0.24, -rust 0.24, -typescript 0.23. | [M][S] |
| `syntect` | **5.3.0** | `default-features = false`, onig backend (`default-onig`). **Never `regex-fancy`.** Cap Oniguruma retries at about 100k (tuicr does this). | [M] |
| `two-face` | **0.5.2** (bat 0.26.1 syntaxes) | Onig syntax set. Needed for TS/TSX. | [M] |
| `unicode-width` | 0.2 | Per grapheme, at render time. | [M] |
| `unicode-segmentation` | current [U] | Grapheme handling on non-ASCII paths. | — |
| `signal-hook` | current [U] | SIGTERM, SIGHUP and SIGCONT handling. | — |
| `anyhow` | 1.x | With gix's `anyhow` feature. | — |

**Rejected:** `similar` (5-30x slower than imara); `arborium` (links its own tree-sitter fork, so the two cannot share a build); `giallo` (whole-text only, no resumable state); syntect `regex-fancy`; libgit2/git2; `notify-debouncer-full`; the kqueue backend.

**git binary:** resolve it once at startup. Take the first `git` on PATH that is not the `/usr/bin/git` xcrun shim; `xcrun -f git` gives the CommandLineTools path. Homebrew git wins if it is on PATH.

**Build profile:** release, thin LTO (all benchmarks used it), `codegen-units = 1`. Try `opt-level = "s"` for grammar crates through profile overrides, to shrink them [U: not measured]. A probe with gix defaults was 8.6 MB and 165 crates [M].

---

## 4. Data layer design

### 4.1 Threads and handles
- Keep one `gix::ThreadSafeRepository`. Each worker gets its own `.to_thread_local()` handle and a reused `diff_resource_cache`. Call `clear_resource_cache_keep_allocation()` between files.
- Pools: **walker** (1 thread, owns history for the session); **reader** (N threads: row decode, tree diffs, blobs, line stats); **diff/highlight** (2-4 threads, separate so highlighting never starves history); **writer** (1 thread, serialises all git CLI writes); **watcher** (notify plus classifier).
- Every request carries `(pane, generation)`. Results with a stale generation are dropped. Long jobs check the generation every N steps; spawned git processes are killed (process group) when superseded. Never put timestamps or ticks in request or cache keys: this is gitui bug #2823, where status was reloaded 30+ times.

### 4.2 History walker (hybrid, commit-graph native)
- **Tips:** the default scope is `HEAD` + `@{u}`, which gives ↑ and ↓ in one flat list. A toggle switches to "all refs" (about 1,000 refs in git and linux).
- Optimise ref resolution, which costs about 20 ms warm and about 740 ms cold on linux:
  - Check commit-graph membership with `g.lookup(id)` before calling `find_header`.
  - Use the peeled lines in `packed-refs`.
  - Cache the tip set and update it only from ref-change events.
- **Algorithm:**
  - Use a max-heap on `committer_timestamp`, a `Vec<bool>`/bitset seen-set over graph positions, and `g.commit_at(pos).iter_parents()`.
  - **Hybrid phase:** tips or parents not in the graph come from the object DB (parse the commit, push its parents), until every frontier entry has a graph position. After any commit, amend, fetch or pull the newest commits are normally outside the graph. The probe code dropped them (cgwalk) or panicked on them (ab), so this must work from day one.
- **Arena:** `Vec<u32>` of graph positions, plus an overflow `Vec<ObjectId>` for non-graph commits, using a tagged index. That is about 6 MB for 1.48M commits, against 30 MB for raw ObjectIds.
- **Streaming:** send the first page of about 200-500 rows to the UI immediately (25-32 ms warm on linux), then larger batches, with no sleeps (gitui sleeps 2 ms per 3k-commit chunk). The scrollbar shows "≥N" until the walk finishes (0.24-0.32 s on linux).
- **Order:** commit time, newest first. This is not topo-safe: under clock skew a parent can appear above its child. That is acceptable for a flat list and is what Desktop and default `git log` accept. Code must never assume topological order.
- **Refresh:** if HEAD, `@{u}` and the tip set are unchanged, skip it. Otherwise walk from the new tips into the existing seen-set and prepend the results [U: incremental cost not measured].
- **No commit-graph, or one badly stale:** show a one-key prompt, "Write commit-graph for fast history (git commit-graph write --reachable --changed-paths --split)". Without a graph, linux takes 15-18 s to walk and about 1 GB of RSS, and merge-base on git.git takes about 0.9 s. After gitty's own writes, optionally run `git commit-graph write --reachable --split` in the background (needs user consent; see §12). Until then, fall back to `rev_walk().sorting(ByCommitTime(NewestFirst))`, streaming.

### 4.3 Row metadata
- Decode lazily for the viewport plus about 2 screens, on the reader pool, into an LRU keyed by position. 50 rows take under 1 ms.
- For list rows use `Commit::message_raw_sloppy()` (infallible) and take the subject. Author name, email and time come from the commit; mailmap is optional.
- **Search** (`/`): scan loaded rows first, then stream matches from N parallel decoders. A full kernel scan is about 24 s of CPU over N threads, so it takes seconds; show progress. A persistent on-disk row cache (sha to subject/author/time) is a v2 option for instant kernel search.
- **Path filter / file history:** fall back to `git log --format=... -- <path>`, which uses the changed-path Bloom filters. gix support for those filters was not verified [U].

### 4.4 Badges and ↑/↓ markers
- Refs: `repo.references()?.all()?.peeled()?`, built once into `HashMap<ObjectId, SmallVec<RefBadge>>`. Peel annotated tags lazily, which avoids lazygit #4770's 7.9 s with 8k tags. Upstream: `repo.branch_remote_tracking_ref_name(name, Direction::Fetch)`.
- **Ahead/behind and markers:** a two-colour walk on a max-heap over `generation()`, with a flags byte per position (A=1, B=2, done=4). Keep a counter of single-coloured queued entries rather than `heap.iter().any()` on every pop, which is O(heap). Stop when no single-coloured entries remain. The A-only set gives the ↑ rows and the B-only set gives the ↓ rows.
- Tips not in the graph: use the same hybrid ODB phase as the walker. If that is not yet built, use `git rev-list --left-right --count A...B` as a cancellable job (0.44 s worst case on linux).
- No upstream: ↑ = `HEAD --not --remotes` (Desktop's rule). Hide ↑ entirely when there is no remote (Desktop does).

### 4.5 Selected-commit details
1. Header from the cached row, plus the full message and the full SHA.
2. **File list first:** `commit.tree()?.changes()?` with `track_rewrites(Some(Rewrites::default()))` (50% similarity, limit 1000, copies off), then `for_each_to_obtain_tree_with_cache`. That is p50 0.2 ms. Merges diff against the first parent [I: Desktop parity not verified]. Root commits diff against `repo.empty_tree()`.
3. **+/- per file as a separate cancellable job:** `change.diff(&mut cache)?.line_counts()`. Typical p50 is about 1 ms; the worst merge takes 255-385 ms. Compute visible rows first on huge merges. Treat counts as display values: merges were off by 1 against git.
4. Blob prefetch for the diff of the selected file, then its neighbours.

### 4.6 Partial and blobless clones
- gix cannot fetch promisor objects; blob reads error out. Detect `remote.*.promisor=true` or `extensions.partialClone`. Route blob reads to `git cat-file --batch` or `git show`, which fetch lazily over the network. Show a "fetching blob…" state and never block another pane on it. Turn rename detection off for these diffs (it reads blobs).
- **Never swallow these errors.** gix's own `Platform::stats` uses `.ok()`, which shows "+0 −0".

### 4.7 Working-tree status
- **Strategy by size.** Below about 20k index entries, use gix: `repo.status(Discard)?.untracked_files(UntrackedFiles::Files).index_worktree_rewrites(None).into_iter(None)?`. That is 15-68 ms and saves the roughly 10 ms git spawn. Above that, or once a status measures over about 150 ms, use the git CLI:
  ```
  git --no-optional-locks status --porcelain=v2 -z --no-ahead-behind --untracked-files=<repo's configured mode>
  ```
  - `--no-ahead-behind`: plain `--branch` computes ahead/behind on every status, which is hundreds of ms on diverged kernel branches. Ahead/behind comes from §4.4 instead.
  - Using the repo's own untracked mode avoids invalidating the untracked cache. If you want Desktop's `-uall` look, add `-c status.showUntrackedFiles=all`, or list a collapsed untracked directory on demand with `git ls-files -o --exclude-standard -- dir/`.
- **gix status results arrive in no fixed order**; sort them.
- **Large repos:** offer once to set `core.fsmonitor=true` and `core.untrackedCache=true`, or `feature.manyFiles=true`, which also sets index v4 and skipHash and halves index write cost. On 120k files status drops from 225-350 ms to 38-60 ms. Say in the UI that the fsmonitor daemon outlives gitty. Check that the daemon actually runs (`git fsmonitor--daemon status`): in sandboxes and on network volumes it fails silently and the cost goes back to about 190 ms.
- **Stale stat data:** with `--no-optional-locks`, stat data is never written back, so after a checkout every refresh re-hashes (1.8-2.1 s at 30k touched files). After a user action, on focus gain, or when a refresh takes more than 3x its moving average, run **one locked** `git status` (the lazygit model) so git saves the stat refresh and keeps the fsmonitor and untracked-cache data current. **Never call gix `Outcome::write_changes()`**: it strips the extensions.
- After staging, refresh only the touched path (gix status with a pathspec, or `git status -- <p>`) and keep the previous untracked list. lazygit #5455 shows a full `-uall` rescan after each stage taking seconds.

### 4.8 Watching and refresh triggers
- **Watcher:** one raw notify watcher, `RecursiveMode::Recursive`, on the worktree root. Add the gitdir or commondir if it lies outside the root (linked worktrees, a `.git` file, submodules) before events start: every `watch()` restarts the FSEvents stream from SinceNow, so events in between are lost. Do a full refresh after any change to the watch set. Canonicalize roots (`/tmp` becomes `/private/tmp`).
- **Classifier** (on the watcher thread) produces a bitmask:
  - **Ignore:** `.git/objects/**`, `.git/logs/**`, `*.lock`, `.git/fsmonitor--daemon*`, `COMMIT_EDITMSG`, `AUTO_MERGE*`, and gitignored worktree paths. Check a cache of known-ignored directory prefixes first, then gix excludes (about 5 µs per path). A cargo or node build in an ignored directory otherwise floods the debouncer.
  - **INDEX:** `.git/index`.
  - **REFS:** `HEAD`, `refs/**`, `packed-refs`, `reftable/**` [U: reftable untested].
  - **REMOTE:** `FETCH_HEAD`, `refs/remotes/**`.
  - **STATE:** `MERGE_HEAD`, `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `rebase-merge/`, `rebase-apply/`, `sequencer/`, `BISECT_LOG`.
  - **IGNORE-RULES:** `.gitignore`, `.git/info/exclude`. These invalidate the ignored-prefix cache and force a full status.
  - **CONFIG:** `.git/config` (upstream or remote edits).
  - **STASH:** `refs/stash`.
  - **WORKTREE:** everything else.
  - `Rescan` or `MustScanSubDirs` flags mark every class dirty.
- **Debounce:** fire 50 ms after the first event and extend while events keep arriving, but never past 300 ms after the first. Run one job per class at a time; events that arrive during a job set a rerun flag.
- **Don't trigger yourself:** after each status gitty runs, record `(mtime_ns, size, ino)` of `.git/index`. Drop INDEX events whose fingerprint matches.
- **Backstops:**
  - Terminal `FocusGained` (crossterm `EnableFocusChange`; kitty supports it) triggers a full refresh, as Desktop does.
  - While focused: a stat fingerprint check every 2 s (under 1 ms) and a full status every 60 s.
  - While unfocused: nothing.
  - Background fetch: every 5 min while focused, 60 min otherwise, with 0-30 s random skew; configurable. Stop for remotes that need auth.
- **Target latency:** about 100-120 ms from save to an updated Changes tab at 120k files with fsmonitor; about 80 ms on small repos; about 100 ms from a ref change to updated badges.

---

## 5. Diff engine design

### 5.1 Algorithm
- **Implementation:** imara-diff through `gix::diff::blob`. Do the diff after git-normalising content (gix-diff pipeline `Mode::ToGit`, which applies clean filters and autocrlf), so worktree diffs match `git diff`.
- **Algorithm choice (corrected):** **git's default is Myers, not Histogram.** Desktop and lazygit run plain `git diff`, so they show Myers plus the indent heuristic. gitty reads `diff.algorithm`, `diff.indentHeuristic` and per-path `diff.<driver>.algorithm`. The default is Myers plus `postprocess_lines` (indent heuristic) for parity; Histogram is a setting. Both are fast on real files (release batch 73 vs 84 ms [M]). Our Histogram reproduces `git diff --histogram` on 99.4% of files; residual slider shifts are harmless for display.
- **Per-worker reuse:** reuse the `Diff` (`compute_with` reuses its buffers). `InternedInput<&str>` borrows the blob, so reuse across files needs owned tokens, an arena, or u32 range tokens over a long-lived buffer.
- **Byte-level prefix/suffix trim before interning, snapped to line boundaries:** 1.86x faster on per-commit diffs. It moves sliders slightly. Mitigate by keeping about 3 lines of margin inside the trim so the indent heuristic still sees context, and accept the small drift explicitly.

### 5.2 Data model: the diff is a view over full ops
```
FileDiff {
  old: BlobRef, new: BlobRef,          // oid, or a worktree content hash
  class: Text | Binary{..} | Lfs{..} | Submodule{..} | ModeOnly | TooLarge | LargeText | Generated,
  ops: Vec<Op>,                        // Equal{old_start,new_start,len} | Change{old:Range<u32>,new:Range<u32>}
  gaps: Vec<Gap{len, top_shown, bottom_shown}>,   // view state
  line_index_old/new: Vec<u32>,        // byte offsets of line starts in the blobs
  flags: no_eol_old/new, eol_style_old/new, bidi_warning, ws_mode,
  intraline: lazily filled per Change block -> per-line byte ranges,
  syntax: lazily filled per-line spans (see §6),
}
```
- Never store patch text. Context lines, hunk headers and split rows are derived from the ops at render time, and only for the viewport. No per-row structs are materialised for a whole file.
- Cache key: `(old_oid, new_oid, ws_mode, algorithm)`. Worktree sides have no stable oid, so key them by a content hash (gix hashes blobs cheaply), or by `(path, mtime, size, ino)` with a hash check.

### 5.3 Context expansion (Desktop parity, simpler)
- Each Equal run between changes is a Gap, defaulting to 3/3 (git's -U3).
  - If `len - top - bottom ≤ 20`, show a single "expand all" control (Desktop's Short).
  - Otherwise ↑ adds 20 to bottom_shown and ↓ adds 20 to top_shown.
  - The leading gap has ↑ only; the trailing gap to EOF has ↓ only (Desktop's dummy hunk).
  - "Expand whole file" shows every gap; "collapse" resets them.
- Context text comes from the new blob, and both line numbers are known from the Equal run. In whitespace-ignore mode, "equal" lines can differ: show the new side's text and keep both numbers, as `git -w` does.
- Allow expansion whenever both blobs are loaded and each is ≤16 MiB. Desktop's 1 MiB limit is a JavaScript-string constraint we don't have.
- **Staging patches are independent of the view:** they always regenerate at -U3 against the index blob.

### 5.4 Intraline (word-level) highlighting
For each Change block with D removed lines and A added lines:
1. If D or A is 0, there is nothing to do.
2. Exclude lines of 1024 or more **display chars** from pairing and emphasis. GitHub and Desktop count UTF-16 units; a byte cap would skip non-ASCII lines too early.
3. **Tokenise each line once per block** and cache the token ranges. delta re-tokenises on every candidate; that is where its 94 ms goes. Tokens are runs of `[A-Za-z0-9_]` or non-ASCII, runs of whitespace, and single punctuation characters. On gix 0.88, `sources::words` is available, but `latin_word_diff` works on whole hunks, so the per-pair diff is our own code.
4. **Pairing:**
   - If D×A ≤ 4096: greedy monotone pairing. For each removed line, scan forward over unpaired added lines, at most 32 candidates (gitty's own cap; delta has none), and accept the first with distance ≤ 0.6.
   - Run a cheap prefilter first (length ratio or token-bag overlap).
   - If D×A > 4096: positional pairing only when D == A, still subject to the distance check.
5. **Distance** = changed / (changed + 2×equal), over trimmed token widths (delta's formula), computed from an imara Myers diff over tokens.
6. **Emphasis:**
   - Take the ranges from the same token diff.
   - Merge ranges separated only by whitespace.
   - Drop emphasis that covers only leading or trailing whitespace (diff-highlight's "boring" rule).
   - Store byte ranges and convert to cells at render time.
7. Compute lazily for the viewport plus about 2 screens, under the diff job's generation.

### 5.5 Split view
Build rows from the same pairing:
- A paired (del, add) shares a row.
- An unpaired del gets an empty right cell; an unpaired add gets an empty left cell. Rows stay in order.

Desktop's behaviour (zip by position, then the leftover deletions, then the leftover additions) is what you get when pairing finds nothing better. Offer it as an option (§12).

### 5.6 Whitespace modes
Use interning keys:
- `-w`: ignore all whitespace.
- `-b`: collapse whitespace runs and ignore trailing whitespace.
- `--ignore-cr-at-eol`.

Display the original bytes. **Turn off hunk and line staging** in any whitespace mode (Desktop does): the ops no longer correspond to a valid patch.

### 5.7 Classification order (per file)
1. Gitlink mode 160000: Submodule{old, new}, plus a dirty flag from status.
2. Mode-only or type change: header only.
3. LFS pointer (under 1024 bytes and starting with `version https://git-lfs.github.com/spec/v1`): "LFS object a→b, size x→y". Check index and HEAD blobs; worktree files may be smudged.
4. Binary: a `binary`, `-diff` or textconv-less driver attribute, or a NUL in the first 8000 bytes of either side. Show sizes. Images by extension can come later (kitty graphics, P2).
5. TooLarge: a side over 64 MiB. Never auto-diff it.
6. LargeText: a side over 4 MiB, any line over 5000 chars, or more than 20k changed lines. Collapse with stats and require Enter (Desktop thresholds: 70 MB unrenderable, about 4.375 MB "large", 5000-char lines).
7. Generated: the `linguist-generated` attribute, lockfile names, `.js` or `.css` with average line length over 110 or a `sourceMappingURL` in the last 2 lines, or "Code generated … DO NOT EDIT" or `@generated` in the first 40 lines. Collapse by default.
8. Otherwise Text.

### 5.8 Other edge cases
- **CRLF:** strip one `\r` per line for display, keeping a per-line `cr` bit. If the whole file's EOL style changes, show an "LF → CRLF" banner. If only some lines differ by CR, show a dim ␍ marker on those lines. gix does not emit git's CRLF warning, so compare EOL styles ourselves.
- **No newline at EOF:** a flag on each side's last line, shown as a dim row.
- **Bidi controls** (U+202A–202E, U+2066–2069) in changed lines: show a warning, as Desktop does.
- **Renames:** "old → new (R087)". Diff the old blob against the new one, so a pure rename shows a header and zero rows.
- **Line indices:** u32. Files that could overflow are already stopped by the TooLarge limit.

---

## 6. Syntax highlighting design

**Pipeline (the UI never waits):**
1. Draw the diff immediately with diff colours only.
2. A worker loads the old and new blobs. Old: the parent, HEAD or index blob. New: the blob, or the worktree file.
3. Highlight each side over the **full file**, in parallel on 2 threads of a dedicated highlight pool.
4. Produce compact spans per line, `Vec<(u16 col_byte, u16 len, u8 style_id)>`, for the rows needed: hunk lines, visible rows and expanded context.
5. Swap them in on the next frame, dropping the result if its generation is stale. This is Desktop's flow.

**Engine choice (corrected by verification):**
- The fast windowed path (raw `Query` + `QueryCursor` with `set_byte_range`, 0.07-0.2 ms per window) does **not** handle injections, locals or overlapping-capture priority. `tree-sitter-highlight` handles all three but cannot work over byte ranges.
- **Decision for v1:** run a full-file `tree-sitter-highlight` pass on the worker (39-92 ms measured for 8-11k-line files, well off the UI thread). Cache the **per-line spans, not the trees**. Tree memory is unmeasured and likely many times the source size. Bound the cache by bytes.
- Later, for big files, a windowed Query path with your own priority resolution and incremental reparse of the new side from the old one (3.3 ms measured).
- **Cancellation:** `cancellation_flag` (AtomicUsize) for tree-sitter-highlight, or `ParseOptions::progress_callback` returning `ControlFlow::Break` once the generation has moved on. Also use it as a timeout for pathological inputs.
- **Languages:** bundle about 15-25: Rust, C/C++, TS/TSX/JS, Python, Go, Java, JSON, TOML, YAML, Markdown, Bash, CSS, HTML, Ruby, Kotlin, Swift, Lua. Compile each query lazily on first use (4-40 ms). The binary budget is **[U] unknown**: the 15-25 MB extrapolation was not verified, and C++, Swift and Kotlin parser tables are large. Measure the real list. Normalise capture names; arborium's and lumis's curated queries can be reused without their runtimes.
- **Fallback for the long tail:** syntect with onig plus two-face (+2.1 MB, about 220 syntaxes).
  - Highlight from line 0 up to the highest needed line only, saving `(ParseState, HighlightState)` every 128-256 lines. A clone costs 0.1-0.3 µs and resuming is exact.
  - Check the generation every about 256 lines.
  - Cap Oniguruma backtracking at about 100k steps.
- **Language detection:** extension, exact filename (Makefile, Dockerfile, .bashrc), shebang or first line, and the `linguist-language` attribute.
- **Limits:**
  - Per line: no syntax colour past 400-1000 chars (delta uses 400).
  - Per file: no syntax colour above about 1-2 MiB or about 50k lines; keep diff colours.
  - Note that Desktop **truncates** at 1 MiB and highlights the prefix rather than skipping. A truncated tree-sitter parse produces error nodes near the cut, so gitty skips instead.
  - Stopgap while blobs load, or when a promisor fetch misses: highlight the hunk alone (5-14% of lines wrong with tree-sitter), then replace.
- **Mixed diffs:** colour context lines from the old side (Desktop), because the worktree side can change mid-parse.

**Colours (GitHub Primer dark, pre-mixed on #0d1117; truecolor):**

| Role | Colour |
|---|---|
| add line bg | `#12261e` |
| del line bg | `#25181c` |
| add gutter bg | `#1c4328` |
| del gutter bg | `#532426` |
| add word bg | `#153522` (alpha 0.25; GitHub uses 0.40 = `#1a4a29`) |
| del word bg | `#482124` (alpha 0.25; GitHub `#6b2b2b`) |
| syntax: keyword | `#ff7b72` |
| syntax: string | `#a5d6ff` |
| syntax: comment | `#9198a1` |
| syntax: entity | `#d2a8ff` |
| syntax: constant | `#79c0ff` |
| syntax: variable | `#ffa657` |
| syntax: tag | `#7ee787` |
| syntax: default text | `#f0f6fc` |

- At alpha 0.25 every syntax colour keeps at least 4.6:1 contrast. At 0.40 comments fall to 3.5:1 [M].
- Use syntax **foreground** colours only. gitty sets row backgrounds itself, across the full row width.
- **256-colour fallback (hand-picked; nearest-match turns every one of these into grey):** add line 22, del line 52, add word 28, del word 88. Text inside word spans uses plain fg 255, because syntax colours fall to 1.6-3.4:1 on 28. 16-colour mode: no syntax colours, terminal green and red.
- **Truecolor detection:** `COLORTERM=truecolor|24bit`, or TERM/TERM_PROGRAM matching kitty, WezTerm, iTerm or ghostty. Provide a `--color` override and honour `NO_COLOR`. A Primer light theme and OSC 11 background detection are v1.1.

---

## 7. Rendering and event-loop design

**Widgets**
- Write the commit list, file list and diff as custom widgets that write only visible rows straight into the `Buffer`:
  - **ASCII fast path:** `set_char` per byte plus `set_style`.
  - **Non-ASCII fallback:** grapheme- and width-aware writes that keep the wide-character skip cell, and clip a wide grapheme at the right edge (split view).
- Use List and Paragraph only for small, static text. `Paragraph::scroll` is a u16 and stops at line 65,535.
- Keep gutters and right-aligned fields in separate Rects, so a width disagreement between unicode-width and kitty cannot spread across panes.

**Text hygiene** (worker side, once per line, 2.7-12.9 ms per 50k lines):
- Expand tabs by display column (8, or per .editorconfig).
- Map C0 and DEL to U+2400 control pictures and C1 to `?`.
- Strip ESC, CSI and OSC sequences from commit messages, author names and paths; treat them as hostile.
- Replace invalid UTF-8 with U+FFFD. `set_stringn` silently drops tabs and controls, and the fast path must sanitize too.

**Event loop**
- **Input thread:** gitui-style polling (`poll(10s)` when idle, `poll(100ms)` after an event) feeding a bounded channel of 1024. Wheel events use `try_send`, so they are dropped when the channel is full. Pausing requires a handshake (park plus an acknowledgement) so `$EDITOR` gets its keys (gitui #1506). crossterm's Waker is `pub(crate)`, so polling is the only way out of a blocked read.
- **Main thread:** `select!{ recv(input), recv(worker_results), recv(timer) }`, with the timer present only while a spinner or toast is active. After each wake:
  - Drain every channel with `try_recv`.
  - Apply the events: sum wheel deltas into `Scroll{steps}` (accelerating when several merged), keep only the last Resize, merge Drag and Moved events.
  - Draw **once, and only if dirty**.
- **No tick.** Idle CPU is zero apart from a wakeup every 10 s.
- **Frame cap:** draw the first event after idle immediately (leading edge). Within 8 ms of the previous frame, arm `after(remaining)` and keep draining. kitty repaints at about 100 FPS anyway (`repaint_delay` 10 ms).
- **Debounced diff loads:** start a diff after about 30 ms of selection stability during fast j/k (lazygit's adaptive throttle). The file list (0.2 ms) can load immediately.

**Frame emit**
- `Terminal::with_options(CrosstermBackend::new(BufWriter::with_capacity(256*1024, stdout().lock()-ish)), Viewport::Fixed(area))`. Call `terminal.resize()` only on `Event::Resize`. `Viewport::Fullscreen` opens `/dev/tty` and runs an ioctl on every draw [S].
- Each frame: `queue!(BeginSynchronizedUpdate)`, render, then `execute!(EndSynchronizedUpdate)`.
- Better still, use a manual frame path (render into `current_buffer_mut()`, `flush()`, `swap_buffers()`, then one backend flush). This avoids `draw()` resending `Hide` with an extra flush every frame.
- Detecting mode 2026 with DECRQM is optional: kitty, WezTerm, iTerm2, Alacritty 0.13+, foot and Ghostty support it, and other terminals ignore it.
- The ratatui buffer diff costs about 0.1-0.36 ms per frame whatever changed. That is acceptable; don't optimise escape output yet. For SSH later: the `scrolling-regions` feature could cut a 1-line scroll from 61 KB to about 1 KB.

**Mouse:** do not use `EnableMouseCapture`, which turns on `?1003` any-motion tracking. Emit `\x1b[?1000h\x1b[?1002h\x1b[?1006h` with a matching disable.
- A hit map, `Vec<(Rect, HitTarget)>`, is rebuilt on every render.
- Pane splitters are 1-column Rects. Dragging one updates the ratio, clamped to minimum widths.
- The wheel scrolls the pane under the pointer, 3 rows per notch.
- Hover effects need `?1003`; gitty ships without hover by default (see §12).
- Mouse capture blocks native selection, so provide OSC 52 copy (kitty supports it). Shift-drag still selects natively in most terminals.

**Keyboard:** after `EnterAlternateScreen`, push `DISAMBIGUATE_ESCAPE_CODES | REPORT_ALTERNATE_KEYS` unconditionally (terminals that don't support it ignore `CSI > u`). Pop the flags **before** leaving the alternate screen, because kitty keeps a separate stack per screen. If you query support, do it before the input thread starts reading (it races for the reply). It costs about one round-trip, not 2 s (corrected). Add `REPORT_EVENT_TYPES` only if needed, and filter to `Press`.

**Terminal guard:**
- Setup: raw mode, alt screen, kitty flags, mouse modes, bracketed paste, focus events, hidden cursor.
- Idempotent teardown: `EndSynchronizedUpdate`, pop flags, mouse off, paste off, focus off, show cursor, leave the alt screen, disable raw mode. Write teardown directly to stdout and ignore errors.
- Run teardown from `Drop`, from a panic hook installed before any worker starts (take_hook, teardown, then the previous hook), and on SIGTERM and SIGHUP.
- ratatui's own restore only toggles raw mode and the alt screen.

**Editor and suspend:**
1. Resolve the editor with `git var GIT_EDITOR`.
2. Pause input and wait for the acknowledgement.
3. Tear down, run `sh -c '<editor> "$@"' <editor> <file>` with inherited stdio, and wait.
4. Set up again, call `terminal.clear()` to force a full repaint, and resume input.

Ctrl-Z uses the same teardown, then `raise(SIGTSTP)`; set up again on SIGCONT.

**First frame:** draw the layout and a skeleton before any git work completes, then fill the history progressively. Cold start on linux can take about 0.9 s to the first rows.

---

## 8. Write path recipe

**General spawn rules (every git call):**
- Use the resolved real git binary.
- Read-only calls: `GIT_OPTIONAL_LOCKS=0` (or `--no-optional-locks`).
- `LANG=C LC_ALL=C` for progress parsing; consider leaving the user's locale for hook-running commands.
- `GIT_TERMINAL_PROMPT=0`.
- Spawn in a new session (`setsid` in `pre_exec`). This stops hooks and helpers opening `/dev/tty` and drawing over the TUI, and allows killing the whole process group.
- Defend diff output and patches against user config: `--no-color --no-ext-diff --no-textconv --no-relative --src-prefix=a/ --dst-prefix=b/ -U3`, plus `-c diff.noprefix=false -c core.quotepath=false`.
- All writes go through the single **writer** thread.
- A stale `.git/index.lock` is a visible error. Offer to remove it only when no git process is running.

### 8.1 Staging model
**Use the real index (lazygit model).** Checkboxes show actual index state, partial files show `[~]`, and a commit is a plain `git commit`. Desktop's model (reset the index at commit time and re-apply the selections) destroys whatever the user staged with the CLI. See §12.

### 8.2 Hunk and line staging (patch generator)
- **Base:** stage = index → worktree diff (`git diff -- <p>`, or gitty's own ops over the index blob and the clean-filtered worktree). Unstage = HEAD → index (`git diff --cached`). Always regenerate at U3. Never use U0: without `--unidiff-zero` it fails, and with it reverse patches misplace lines.
- **Per-line rules (Desktop):**
  - Selected lines are kept.
  - An unselected `-` becomes context in place.
  - An unselected `+` is dropped.
  - Context passes through.
  - This yields Desktop's "old, new" order for partially staged replacements; lazygit's order is "new, old".
- **Headers:** forward patches keep `old_start` and take `new_start` from a running offset. **Reverse (unstage) patches keep `new_start` and derive `old_start`** (lazygit's formula is wrong in reverse).
- **EOL fix** (lazygit and, by code identity, Desktop corrupt `a\nb` + `+c` into `a\nbc`):
  - Any target-side line marked no-EOL that is not the last target-side line must be rewritten.
  - A context line becomes a source-side line with no-EOL (`-b` + `\ No newline`) followed by a target-side `+b`.
  - A pure target-side line just loses its flag.
  - The target side is ` `+`+` when staging and ` `+`-` when unstaging.
- **Apply:** `git apply --cached --whitespace=nowarn -` (stage) or `git apply --cached -R --whitespace=nowarn -` (unstage), with the patch on stdin.
- **TOCTOU:** check that the `index <old>..<new>` blob ids in the diff still match the current index entry and worktree before applying. Otherwise re-diff. Key selections to the diff generation.
- **Untracked files:** `git diff --no-index /dev/null <p>` and a `--- /dev/null` header. Preferred over `git add -N`, which leaves intent-to-add entries behind if the user cancels.
- **Whole-file selections (empty-blob trap):** applying every line of a deletion, or unstaging every line of a new file, leaves an empty blob in the index. Either:
  - emit proper headers (`diff --git a/p b/p` + `deleted file mode <m>` + `+++ /dev/null`, or `new file mode <m>` + `--- /dev/null` with `-R`), which verification measured to work; or
  - route to whole-file commands.
- **Whole-file commands:**
  - Stage: `git add -- <p>`.
  - Stage a deletion: `git rm --cached -q -- <p>`.
  - Unstage: `git restore --staged -- <p>`.
  - Unstage a new file: `git rm --cached -q -f -- <p>`.
- **Renames:** diff both paths (`git diff --cached -M -- old new`) and rewrite the header to `a/new b/new`. A diff of only the new path removes the wrong lines.
- **Whole-file only:** binary, mode-only, submodule, LFS, conflicted (stages 1-3) and whitespace-hidden views.
- **Executable new files:** a rewritten header loses the mode. Fix it with `update-index --chmod=+x` or stage the whole file.
- **Path quoting:** use git's C-style quoting in headers for tab, newline, quote, backslash and non-ASCII characters [U: untested].
- **Discard lines:** the same generator applied with `git apply -R` to the **worktree**. It needs its own fuzz pass because it touches user files (CRLF and smudge).
- **Tests:** port stagelab's `transform()` and `expected()` oracle as a property test in gitty-core, adding new files, renames and quoted paths.
- **Rejected for now:** in-process gix staging (2.8 ms vs about 10-15 ms). gix index writes drop UNTR, FSMN and link extensions and cannot write v4, and gix#2421 needs `remove_tree()`. Revisit only once gix preserves extensions.
- **Cost at kernel scale:** each write rewrites the whole index (31 ms at 90k entries, 15 ms with v4 + skipHash). After a write, refresh only the touched path.

### 8.3 Commit
- `git commit -F -` with the message on stdin. Add `--amend`, `--no-verify`, `--signoff`, `--allow-empty` and `--cleanup=strip` when the box allows `#` comments (`-F` defaults to whitespace cleanup). Merges use `--no-edit --cleanup=strip`.
- **Hooks:** stdin is empty, and hook stdout goes to git's stderr. Stream stderr into a log panel and show the full output in a modal on failure. Hooks that would prompt fail fast under setsid; make that clear in the message.
- **Undo** (latest local commit): `git reset --mixed HEAD^` and restore the summary, body and co-authors into the box (Desktop).
- **Signing:**
  - `gpg.format=ssh` with a passphrase key: askpass trampoline (`SSH_ASKPASS=<gitty exe>`, `SSH_ASKPASS_REQUIRE=force`, `DISPLAY=:0`, a `GITTY_ASKPASS_SOCK` Unix socket). gitty re-execs itself as the helper and the TUI shows a masked prompt. ssh-keygen's prompt is `Enter passphrase for "<path>":`; lazygit's regex misses it.
  - openpgp or x509: try captured first (works with pinentry-mac or a cached agent). On a signing error, **"retry in terminal" is required**: suspend the TUI, set `GPG_TTY`, run with inherited stdio, then restore. setsid blocks pinentry-curses and pinentry-tty. gpg is not installed here, so this path is untested [U].

### 8.4 Network operations
- `git fetch --progress --prune <remote>`
- `git pull --no-edit --progress [--ff-only|--rebase]`
- `git push --progress --porcelain <remote> <ref>` (per-ref results on stdout)
- Keep the user's `credential.helper` (osxkeychain) working.
- Askpass: `GIT_ASKPASS` and `SSH_ASKPASS` point at the trampoline (`SSH_ASKPASS_REQUIRE=force`). The trampoline receives `Username for '…':` and `Password for '…':`. Host-key prompts also arrive through askpass and need a yes/no UI [I].
- **Progress:** read stderr incrementally and split on `[\r\n]`. Parse with
  `^(?:remote: )?(?P<title>.+?):\s+(?:(?P<pct>\d{1,3})% \((?P<cur>\d+)/(?P<tot>\d+)\)|(?P<count>\d+))(?P<rest>.*)$`
  and weight the phases as Desktop does:
  - Fetch: Compressing 0.1, Receiving 0.7, Resolving 0.2.
  - Push: Compressing 0.2, Writing 0.7, remote Resolving 0.1.
  - Throttle UI updates to the frame rate.
- **Background fetch:** add `GIT_SSH_COMMAND='ssh -o BatchMode=yes'` and no askpass. On `terminal prompts disabled` or `Permission denied`, mark the remote "needs auth" and stop auto-fetching it (lazygit's FailOnCredentialRequest).
- Every write and network job can be cancelled (kill the process group).

---

## 9. UX spec

### 9.1 Layouts by width (columns)
| Width | Layout |
|---|---|
| < 120 | One pane at a time. Enter or Tab drills list → files → diff; Esc goes back. |
| 120-159 | `[history 42][detail 78]`. Detail stacks the header (2-3 rows), the file list (up to 8 rows, resizable with +/-) and a unified diff. |
| 160-199 | `[history 44][files 34][diff ~82]`. The header spans files and diff. Unified only. |
| ≥ 200 | `[history 48][files 38][diff 114+]`. Split view allowed (`s`). Auto-split when each half has at least 5 gutter + 1 marker + 50 text columns. Forcing split below 200 is allowed, with a warning. |

The status bar shows branch, `↑N ↓M` and "fetched 3m ago". Pane ratios persist.

### 9.2 History tab
- **Row (one line):**
  - `[marker 2]` — `↑` yellow for unpushed; `↓` cyan with the row dimmed for unpulled.
  - `[summary, flexible]` — an empty one shows a dim "Empty commit message".
  - `[badges, right-aligned]`:
    - current branch: inverse;
    - local branches: green;
    - remote refs: dim red, folded into the local badge when they point at the same commit;
    - tags: yellow, first tag plus `+N`.
  - `[initials 3]` — 2 letters, coloured by a hash of the email.
  - `[date 4-12]`.
  - Truncation order as width shrinks: badges, then the date, then the summary.
- Density toggle `z`: a second line with "Author, Co-author • 3 days ago", or "N people".
- **Dates:** compact relative by default (now, 45s, 12m, 5h, 3d, 2mo, 4y, using Desktop's 45 s/45 min/24 h/30 d/18 mo thresholds), switching to absolute (`2026-10-02 14:18`) after 7 days. `D` cycles relative / absolute / both. Recompute every minute below 1 h, hourly below 1 day, every 6 h after that. Relative-only dates are Desktop's most-complained-about history problem (#17702 144, #14611 116 upvotes).
- **Commit header:**
  - Line 1: bold summary (`:emoji:` rendered, `#123` underlined).
  - Line 2: initials, authors, short SHA, `+A −D`, tags.
  - `o` expands the description (up to half the pane) and shows the full SHA.
  - Committer shown only if it differs from the author, isn't a co-author and isn't GitHub web-flow.
  - Multi-select: "Showing changes from N commits", plus "N unreachable commits not included" when relevant.
- **File list:**
  - `[status glyph coloured: A green, M yellow, D red, R→ blue, C cyan, U/! red-bold]`, then a dim directory and a bright file name, then right-aligned `+n −m`.
  - The header shows "N files +A −D".
  - Path truncation follows Desktop's `truncatePath`, using display width. Example corrected: `app/src/…/commit-list.tsx` is wrong; the 24-column result is `app/src…/commit-list.tsx`.
  - `t` toggles a tree view (#2417).
- **Multi-select:** `V` plus j/k, or Shift-click, selects a contiguous range. The diff is `oldest^..newest` (the null tree for a root commit). Non-contiguous selection with Ctrl-click shows the commits' patches in sequence rather than Desktop's empty screen.
- **Compare (`b`):** a fuzzy branch picker titled "Select Branch to Compare…". Tabs: `Behind (N) | Ahead (M) | Files`. Files is the merge-base diff, which matches Desktop's "Preview Pull Request" (corrected: this is parity, not an improvement). Counts show `…` until the walk returns.
- **Search:** `/` incrementally searches summaries and authors (streaming); `path:` filters history to a file (#3754); n/N move between matches. `/` in the diff searches the diff.

### 9.3 Changes tab
- File rows `[x]`, `[ ]` or `[~]` + status + path. The header `[~] 12 changed files` toggles all.
- A filter (`F`): included, excluded, new, modified, deleted.
- **In the diff:**
  - The gutter shows ✓ on staged lines.
  - Space toggles the line under the cursor; `v` starts a range, then Space applies it.
  - `H` toggles the hunk; `d` discards (with confirmation).
  - With whitespace hidden (`w`), selection is off and the status line explains why.
- **Commit box** at the bottom left:
  - Summary with a counter (yellow over 50, red over 72), placeholder "Update `<file>`".
  - Expandable description; a co-authors line that writes trailers.
  - Button text "Commit 3 files to main" or "Amend …".
  - `c` or `i` focuses the box. Ctrl+Enter (needs kitty flags) or Alt+Enter commits.
  - `A` toggles amend and shows the banner "will modify your most recent commit".
  - After a commit, a bar shows "Committed just now: …  [u] Undo".

### 9.4 Keybindings (vim navigation stays free; lazygit-compatible actions)
| Keys | Action |
|---|---|
| `j/k/↓/↑`, `g/G`, `Ctrl-d/u`, `PgUp/PgDn` | Move, jump to top/bottom, half page, page |
| `h/l` | Horizontal scroll in the diff (no-wrap is the default; `W` toggles wrap, #11052) |
| `Tab/S-Tab`, `←/→` | Switch pane |
| `1` / `2` | Changes / History (Desktop Cmd+1/2) |
| `Enter` / `Esc` | Drill in or focus / back or cancel |
| `/`, `n/N` | Search, next/previous |
| `y` / `Y` | Copy short / full SHA (OSC 52) |
| `o` | Expand the commit header |
| `s`, `w`, `W` | Split view, hide whitespace, wrap |
| `e` / `[` / `]` / `E` | Expand context: both, up, down / whole file (toggle) |
| `t`, `D`, `z` | Tree view, date mode, row density |
| `b` | Compare to branch |
| `f` / `p` / `P` | Fetch / pull / push |
| `c` / `A` / `u` | Commit box / amend / undo |
| `Space`, `a`, `v`/`V`, `H`, `d` | Toggle, toggle all, visual line / visual range, toggle hunk, discard |
| `x` | Commit actions menu: revert, checkout, branch, tag, cherry-pick, reset, copy |
| `O` | Open in an external difftool (#1765) |
| `<`/`>` or `Ctrl-h/l` | Resize panes |
| `?` | Help overlay for the focused pane |
| `q` | Quit |

### 9.5 Mouse
- Click selects a row; Shift-click extends; Ctrl-click toggles (macOS Cmd-click never reaches the terminal).
- The wheel scrolls the pane under the pointer.
- Clicking the checkbox column toggles.
- **Click or drag in the line-number gutter selects lines to stage**, auto-scrolling past the edge. In split view the half is chosen by x position.
- The hunk handle (1 column left of the gutter) toggles the whole hunk.
- Click a ↑ or ↓ glyph to expand context.
- Drag pane borders to resize.
- Double-click a file to open `$EDITOR` at that line.
- Right-click opens the `x` menu.
- Hover highlighting only if 1003 is enabled (§12).

---

## 10. Competitor lessons

**Copy:**
- **lazier:** one walker thread owns the gix walk; pull-based chunks; skip the refresh when tips are unchanged; watcher-driven status with a 300-path cap that falls back to a full scan; skip the HEAD-tree-to-index diff when the cache-tree root equals HEAD's tree; renames off in partial clones.
- **lazygit:**
  - `GIT_OPTIONAL_LOCKS=0` on background commands; a foreground refresh takes the lock.
  - Kill a stale diff process when the selection changes; adaptive 30 ms throttle.
  - Render off-screen and swap.
  - transform.go edge cases (new file as diff against empty, StripRename, FileNameOverride). Port its tests, but **not** its EOL or reverse-header bugs.
  - PTY-free credential handling via FailOnCredentialRequest for background fetch.
- **gitui:** hash request parameters and publish a result only if the hash still matches; one running job plus one overwritable pending slot.
- **keifu:** merge wheel bursts into `Scroll{steps}`; drop excess wheel events when the queue is full.
- **tig:** stream, redraw only dirty lines, round loading counters.
- **tuicr:** 20-line ↑/↓/↕ expanders; a cap on Oniguruma retries.
- **GitHub Desktop:**
  - Render first, then add tokens from full-file contents on a worker.
  - Size thresholds; expansion behaviour; refresh on focus; progress weights; the "unreachable commits" note; the absolute-date setting; contiguous range diff.

**Avoid:**
- topo-order by default (lazygit: 12 s on the kernel); reloading the whole history past row 200 (lazygit).
- An eager full walk with `use_commit_graph(false)` and sleeps (gitui).
- A synchronous 1200-commit decode on the UI thread (gitui).
- libgit2 TOPOLOGICAL sorting (keifu).
- A full `-uall` status after every stage, or on every focus change (lazygit #5455).
- Timestamps in cache keys (gitui #2823).
- No size limits on diffs (gitui #1698, #2665).
- Per-hunk highlighting (delta, gitu).
- Prefix/suffix-only intraline highlighting (Desktop, diff-highlight, lazier).
- Shelling out to delta as a pager, which breaks line staging (lazygit #2117) and needs re-running on resize (#4415).
- libgit2 push credentials (gitui #495).
- GPG pinentry taking over the UI (lazygit #1146).
- An idle spinner causing redraws (lazygit #4734).
- Walking only HEAD, which cannot show ↓ (lazier, gitui).
- Checking only HEAD before skipping a refresh (lazier misses fetches).
- Citing lazier's benchmark: it is self-reported, includes a constant 0.7 s key delay, used a blobless clone, and its harness was deleted.

**Unmet demand that gitty should treat as first-class:** full-screen diff (lazygit #1113), word diff (gitui #358), split view (gitui #1294), highlighting while staging (lazygit #2117), tree-sitter highlighting (#2128), drag-to-select lines to stage (gitu #303), commit search (Desktop #7022), file history (#3754), absolute dates, stashing selected files (#11531), an external difftool (#1765), and no-wrap (#11052).

---

## 11. Corrections to earlier assumptions

| Earlier assumption | Correction |
|---|---|
| gix 0.81 (or 0.56) | **gix 0.88.0** (2026-09-25, MSRV 1.88). The 0.56 figure is gix-index's version. Signing exists in 0.88; crate-status.md is stale ("Generated by Codex"). |
| `features = ["max-performance"]` | Does nothing in 0.88 (equals the default `max-performance-safe`). Add `anyhow` instead. |
| "gix for all reads" | Mostly right, with exceptions: (a) working-tree status on large repos goes through the git CLI (fsmonitor, untracked cache, and git persists the index); (b) blob reads in promisor/partial clones; (c) path-limited history (Bloom filters) for now; (d) ahead/behind when tips are not in the graph, until the hybrid walker exists. |
| gix status never writes the index | It can (`Outcome::write_changes`), but **must not**: it drops UNTR, FSMN, REUC and link and cannot write v4. The same applies to gix index writes for staging. |
| Commits stored as 20-byte ObjectIds | Store u32 commit-graph positions plus an overflow list of ObjectIds for non-graph commits. A stale graph is the normal state right after any write. |
| A plain `rev_walk` is enough | A custom commit-graph walker is 3.5x faster than rev_walk (0.24-0.32 s vs 1.1-1.5 s on linux). A custom ahead/behind walk is about 4-15x faster than `with_hidden`. |
| Warm numbers mean "instant" | Cold start on linux: about 740 ms for refs, about 930 ms to the first 500 rows. Design a skeleton first paint. |
| git's default diff is Histogram | git, Desktop and lazygit default to **Myers** with the indent heuristic. Honour `diff.algorithm`. |
| gix-imara-diff 0.3 only adds bstr sources | It also includes `sources::words`, `Words` and `Hunk::latin_word_diff`. |
| Use the `git apply --cached` / `--unidiff-zero` approach from Desktop | Use U3 patches with exact reverse headers and the EOL fix. Both lazygit and Desktop's algorithm corrupt EOF cases, and U0 misplaces lines when unstaging. |
| Whole-file selections need separate commands | The patch generator can emit `deleted file mode` / `new file mode` headers instead; both approaches work. |
| `git` = `/usr/bin/git` | It is an xcrun shim that adds 4 ms per spawn. Resolve the real binary. |
| crossterm `EnableMouseCapture` | It enables `?1003` any-motion tracking. Emit 1000/1002/1006 yourself. |
| ratatui `init()`/restore and the default panic hook are enough | They only toggle raw mode and the alt screen. You need your own guard (kitty flags, mouse, paste, sync, cursor). |
| `Viewport::Fullscreen` | It opens `/dev/tty` and runs an ioctl on every draw. Use `Viewport::Fixed` and resize on events. |
| `supports_keyboard_enhancement()` blocks for 2 s | It returns on the DA1 reply (one round-trip). It still must run before the input thread starts, or skip it and push flags unconditionally. |
| notify-debouncer-mini has a fixed 2 s delay | Its timeout is configurable (500 ms default). A custom per-class debouncer is still preferred; debouncer-full walks the whole tree on macOS. |
| kqueue watching (GitAxon's choice) | It fails at about 61k fds on 120k files and breaks on the fsmonitor socket. Use FSEvents. |
| `git status --branch` is cheap | It computes ahead/behind on every call. Use `--no-ahead-behind`. |
| Desktop has no combined branch diff | It has "Preview Pull Request" (merge-base diff). gitty's Compare→Files tab is parity. |
| Desktop skips highlighting over 1 MiB | It truncates at 1 MiB and highlights the prefix. Only expansion is disabled. |
| Desktop's History shows unpulled commits | It shows the current branch only, paged 100 at a time, with ↑ only. gitty's ↓ markers and badges go beyond Desktop. |
| Tree-sitter windowed queries equal full highlighting | Raw Query/QueryCursor lacks injections, locals and capture priority. Use full-file tree-sitter-highlight on the worker for v1. |
| 15-25 MB budget for grammars | Unverified extrapolation. Measure the actual language list. |
| Topo order is needed for a sane list | Not for a flat list. Commit-time order plus the commit-graph is the fast path; accept clock-skew artefacts. |
| A UI "tick" for refresh | No tick. Events, focus and slow backstop timers only. |

---

## 12. Risks and open questions for the user

**Decisions needed:**
1. **Staging model:** the real index (recommended: lazygit-like, safe alongside the CLI), or Desktop-exact checkboxes with an index reset at commit time?
2. **History scope and order:** default `HEAD + @{u}` in commit-time order with ↓ rows dimmed (recommended), versus all refs, versus a separate "Incoming (N)" group at the top. Is the occasional clock-skew mis-order acceptable?
3. **Writing to the user's repo config and state:** may gitty (a) write or refresh the commit-graph in the background after writes, and (b) enable `core.fsmonitor`, `core.untrackedCache` or `feature.manyFiles` on large repos? Both leave lasting changes (the fsmonitor daemon outlives gitty). Recommended: ask once per repo.
4. **Diff algorithm default:** Myers for git/Desktop parity (recommended), or Histogram as the opinionated default?
5. **Split-view pairing:** similarity-based (recommended, delta-like), or Desktop's positional zip?
6. **Intraline background strength:** alpha 0.25 (readable syntax colours, recommended) vs GitHub's 0.40 (exact look). Check visually in kitty.
7. **Hover:** enable `?1003` for hover highlights (more events, merged per frame), or ship without hover (recommended for v1)?
8. **Binary-size budget** for bundled grammars, and whether to ship per-language cargo features.
9. **Commit signing:** is pinentry-mac installed or planned? gpg is absent now. If GPG signing is used without pinentry-mac, every commit needs the suspend-TUI path.
10. **Optional graph column later?** It is among Desktop's most-upvoted requests (#9452 206, #1634 151), despite the "no lanes" brief.

**Risks and unmeasured items:**
- **Kernel-scale working tree not measured:** the linux bench repo is blobless with no checkout. Status numbers come from a synthetic 120k-file tree with `index.skipHash=true`, which flatters it. Measure a real kernel checkout before fixing the about 20k-entry and 150 ms thresholds.
- **Cold-cache performance** was measured only incidentally (about 0.9 s to the first linux rows). Measure first launch after `sudo purge`.
- The **hybrid walker** (ODB phase for non-graph commits) and incremental refresh are designed but unbuilt and unmeasured. The probe code dropped or panicked on these commits.
- **Search over 1.48M commits** takes seconds even in parallel. A persistent row cache may be needed for instant kernel search.
- **Tree-sitter memory and time on huge or minified files** are unmeasured. Use progress-callback timeouts and per-line caps.
- **Untested write paths:**
  - GPG signing (no gpg installed);
  - SSH-transport askpass and host-key prompts (no SSH remote);
  - quoted or odd paths in patches;
  - conflicted, sparse and skip-worktree entries;
  - submodules;
  - discard-lines on the worktree (needs its own fuzz pass).
- **Reftable repos** and split commit-graph chains are untested.
- **Path-limited history in gix** (Bloom filters) was not evaluated; the plan uses the git CLI for now.
- **Merge commits:** first-parent diffs are assumed [I]; combined diffs are out of scope. Huge kernel merges (10k+ files) need line counts loaded lazily for visible rows only.
- **Wide characters:** unicode-width vs kitty disagreements on emoji and ZWJ sequences can shift columns. Contain them per Rect, and add CJK and emoji tests.
- **Over SSH or tmux:** a 1-line scroll emits about 61 KB; COLORTERM is often lost; tmux needs passthrough for sync output. Wheel merging matters there.
- **gix API churn:** minor versions break APIs (0.81 to 0.88 changed error types and features). Keep all gix calls behind one gitty-core module, as lazier does.
- **Benchmark noise:** all numbers came from a loaded machine. Re-run the decisive probes (walk, status, render, highlight) on an idle machine before quoting them externally.
