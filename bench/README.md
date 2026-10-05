# Benchmarks

`bench/run.sh <repo>...` builds `crates/gitty-core/examples/probe` (release, thin LTO) and checks the
spec §8 latency budget. `BLOBLESS=<repo>` adds history-only checks for a blobless clone (no diffs).
Criterion micro-benches: `GITTY_BENCH_REPO=<repo> cargo bench -p gitty-core`.

## Baseline — 2026-10-02, Apple Silicon, macOS, load average ~28 (noisy machine)

Repos: git/git (85,887 commits, 1,010 refs), with and without commit-graph; Linux kernel blobless
clone (1,484,291 commits, 945 refs, commit-graph present).

| Operation | git (no graph) | git-cg | linux | Budget |
|---|---|---|---|---|
| refs snapshot (warm) | 22.8 ms | 11.4 ms | 25 ms | — |
| first 500 rows, branch+upstream scope | 30.6 ms | 15.1 ms | **23.8 ms** | < 50 ms ✅ |
| first 500 rows, all refs (warm) | 94.7 ms | 11.8 ms | **27.6 ms** | < 50 ms ✅ (graph) |
| first screen decoded (60 rows) | +0.3 ms | +0.5 ms | +0.8 ms | — |
| full walk, branch+upstream | 721 ms | 24.8 ms | **233 ms** | < 400 ms ✅ |
| full walk, all refs (warm) | 1,409 ms | 19.2 ms | **268–307 ms** | < 400 ms ✅ |
| row decode | 2.8 µs | 2.8–15.9 µs | — | — |
| commit file list (300 commits) | p50 0.23 ms, p99 1.8 ms | p50 0.26 ms, p99 2.1 ms | blobless | p50 < 5 ms ✅ |
| file list + line stats (≤40 files) | p50 0.76 ms, p99 78 ms | p50 2.7 ms, p99 117 ms | blobless | lazy, visible only |
| ahead/behind master...v6.0 (360,606) | — | — | **88 ms** | < 150 ms ✅ |

Notes:
- **Cold page cache:** the first run after other I/O spends ~0.8–1.0 s in `refs()` on the kernel. That is 945 refs, each peeled and with a header lookup. The UI has to draw its layout first and stream rows in after (spec §8). A later optimisation is to skip `find_header` for refs that packed-refs records as peeled.
- **Repos without a commit-graph:** the walk falls back to ODB decoding (git/git: 1.4 s for a full all-refs walk). gitty writes a commit-graph automatically on large repos (M5 auto-tuning).
- **Line stats p99:** this is driven by commits that touch many files. Stats are computed only for visible files, on workers.

## After the M1 review fixes (reused tree-diff resource cache)

| Operation | linux | git-cg |
|---|---|---|
| commit file list, 300 commits (warm) | p50 **0.05–0.17 ms**, p99 1.0–2.6 ms | p50 0.13 ms, p99 1.4 ms |
| first call on a fresh handle | ~0.66 s cold (attribute stack + pack index). The UI warms this on a worker at startup | — |
| first 500 / full walk (branch+upstream) | 26 ms / 250 ms | — |
## Diff engine — 2026-10-02 (M2a), load average ~25

`probe diffs <repo> 300`: for every file of the first 300 commits on HEAD, this loads blobs, diffs them
(Myers + indent heuristic), computes intraline for every change block, and builds the view.

| Repo | files | change blocks | p50 / file | p99 / file | max | Budget |
|---|---|---|---|---|---|---|
| git-cg | 1,108 | 7,586 | **0.18–0.22 ms** | **6.2–6.9 ms** | 13–15 ms | p50 < 1 ms, p99 < 10 ms ✅ |
After the M2a review fix (no manual trim, lazy pairing): p50 0.21–0.25 ms, p99 6.4–9.4 ms. The probe still
computes intraline for every block, which is the worst case.

## TUI end to end (M2b) — 2026-10-04, release build, 200×50 pty, warm cache

Measured with a `pyte` screen emulator driving the real binary, plus `GITTY_TRACE=<file>` event
timestamps (ms since start). The terminal answers the startup probe (DA1) as a real one does.

| Milestone | git (no graph) | git-cg | linux (1.48M, blobless) | Budget |
|---|---|---|---|---|
| first frame (layout drawn) | 14 ms | 14–15 ms | 14 ms | < 16 ms ✅ |
| first rows on screen | 48 ms | 48–85 ms | 57–72 ms | — |
| full history walk done | 617 ms | 30 ms | 263 ms | < 400 ms (kernel) ✅ |
| slowest frame after the first | — | — | 5.8 ms | < 16 ms ✅ |

- Cold kernel start (first run after build): rows at ~1.0 s. Cold `refs()` (~0.7 s) dominates; the
  layout is already drawn at 14 ms.
- Fixed during measurement: the walker held the history write lock while walking, which starved the UI
  thread for the length of the walk (~350 ms on the kernel). It now walks into a private chunk and
  publishes it with `History::append` (regression test `walk_never_starves_readers`).
- The linux bench repo is a blobless partial clone. Diffs there report "Could not load this diff",
  and line stats are left blank instead of showing `+0 −0`.
- `cargo run --release -p gitty --example trace -- <repo>` times each worker request in isolation.

## Syntax highlighting (M3) — 2026-10-04, release build, Apple Silicon

`cargo run --release -p gitty-highlight --example hl_time -- FILE...` times one whole-file pass.
"First" includes compiling that language's queries (once per process); "warm" is the best of 5.
The diff is drawn before highlighting finishes; colours arrive from the 2-thread highlight pool.

| File | engine | lines | first | warm | Budget |
|---|---|---|---|---|---|
| git `diff.c` | tree-sitter C | 7,860 | 37 ms | 34 ms | < 100 ms ✅ |
| synthetic `big.ts` (408 KiB) | tree-sitter TS | 10,000 | 124 ms | 90 ms | < 100 ms (warm ✅, first ✗ by query compile) |
| git `git-p4.py` | tree-sitter Python | 4,628 | 25 ms | 21 ms | ✅ |
| git `t/test-lib.sh` | tree-sitter Bash | 2,019 | 9 ms | 5 ms | ✅ |
| gitty `Cargo.lock` | tree-sitter TOML | 3,988 | 8 ms | 7 ms | ✅ |
| gitty `ui/diff.rs` | tree-sitter Rust | 396 | 18 ms | 4 ms | ✅ |
| git `Makefile` | syntect | 4,124 | 41 ms | 38 ms | ✅ |
| git `README.md` | syntect | 75 | 3 ms | 1 ms | ✅ |

Binary size (release, stripped): **24.4 MB** with all grammars, **5.6 MB** with
`--no-default-features` (syntect fallback only). The largest grammars, as static libraries:
Swift 4.2 MB, C++ 3.4 MB, TypeScript/TSX 3.0 MB, SQL 2.5 MB, Bash 1.5 MB, Rust 1.2 MB; the
rest are under 1 MB each. Every grammar is a cargo feature of `gitty-highlight`.

## Changes tab (M4) — 2026-10-04, release build, Apple Silicon, load average ~6.8

`cargo run --release -p gitty --example changes -- <repo> <file>` works on a throwaway `--shared` clone:
status (best/median of 10), a worktree save until the watcher fires and the status it triggers, and
staging one line the way Space does (change diff load, then `SetStaged` → `git apply --cached`).

| Operation | git (4,857 files) | gitty (109 files) | Budget |
|---|---|---|---|
| status, fresh checkout (racily clean index) | 142–154 ms | 9.4 ms | — |
| status after gitty's index refresh | **18.5–19.4 ms** | 5.7–5.9 ms | < 70 ms ✅ |
| save → watcher fired | 63–67 ms | 63–67 ms | — (50 ms quiet debounce) |
| save → status on screen | **92–107 ms** | 73–94 ms | < 150 ms ✅ |
| change diff load (HEAD → worktree + staged lines) | 0.7 ms | 0.2 ms | < 16 ms ✅ |
| stage a line (patch + `git apply --cached`) | **11.7–12.0 ms** | 9.2–9.5 ms | < 50 ms ✅ |

- **Fixed during measurement:** status runs with `GIT_OPTIONAL_LOCKS=0`, so git never saves the stat
  data it refreshes. After a checkout every racily clean entry is re-hashed on every status (190 ms
  on git/git, for ever, until some other git command writes the index). When a status takes over
  100 ms, gitty now queues `git update-index -q --refresh` on the writer thread, at most once a
  minute (`slow_status_refreshes_the_index_at_most_once_a_minute`).
- **Status budget (spec §8):** under 70 ms on a small repo, and under 60 ms on a 120k-file repo with
  fsmonitor. `bench/run.sh` checks the small-repo figure.
- Save → watcher is dominated by the 50 ms quiet window of the debouncer (FSEvents itself delivers in
  ~10–15 ms); bursts such as a formatter rewriting many files still produce one status run.

## Network (M5) — 2026-10-04, release build, 140×30 pty (pyte), local bare remote, load average ~5

Driven end to end through the real binary: fetch, a diverged pull (rebase), push, a password prompt
answered through the askpass trampoline by a real `ext::` transport that calls `$GIT_ASKPASS`, and
cancel on a hanging remote.

| Operation | Time | Budget |
|---|---|---|
| `f` → progress in the top bar | 38 ms | < 100 ms ✅ |
| fetch of a 20 MB commit (local remote) | 0.95 s | — |
| prompt request → masked prompt on screen | 0.24 s (includes the transport starting) | — |
| `x` → "Fetch cancelled" (process group killed, nothing left running) | 7 ms | < 1 s ✅ |
| `git commit-graph write --reachable --changed-paths`, git/git (85k commits), maintenance thread | 6.3 s | off the UI and writer threads ✅ |

## Budgets enforced (M6) — 2026-10-05, release build, Apple Silicon, load average ~6

`bench/run.sh <repo>...` now passes `--check` to the probe: each spec §8 budget prints
`budget <what>: <measured> < <budget> ok|MISSED`, and the script exits 1 when any is missed (the
probe exits 2; `GITTY_BUDGET_SCALE=0.0001` forces a miss to prove it). A warm-up walk runs first,
because the budgets are for a warm cache. Walk budgets are checked only when the walk uses a
commit-graph, which gitty writes on large repos.

| Check | git (no graph) | git-cg | linux (blobless) | Budget |
|---|---|---|---|---|
| first 500 rows, all refs | 15.8 ms (not checked) | 10.9 ms | 22.9 ms | < 50 ms ✅ |
| full walk, all refs | 443 ms (not checked) | 18.9 ms | 258 ms | < 400 ms ✅ |
| commit file list p50 | 0.11 ms | 0.12 ms | blobless | < 5 ms ✅ |
| file diff p50 | 0.19 ms | 0.19 ms | blobless | < 10 ms ✅ |
| ahead/behind master...v6.0 (360,606) | — | — | 88 ms | < 150 ms ✅ |
| status median of 10, after a refresh | 16.4 ms | 17.2 ms | — | < 70 ms ✅ |

`cargo bench -p gitty --bench frame`: a 220×60 frame of a 300-commit fixture (history, file list,
and a highlighted 3,000-line Rust diff) takes **0.52 ms** (criterion mean 0.515 ms; slowest of 200
frames 0.63 ms), against the < 16 ms keypress-to-frame budget. The bench exits 2 on a miss.

## Line staging after the M6 guards (M7) — 2026-10-05, release build, load average ~6

The M6 write guards (intent-to-add check, HEAD check, index check after `apply --cached`) added
git processes to every line toggle. `--example changes` on git/git, staging one line:

| Build | stage a line (median) | Budget |
|---|---|---|
| M6 (HEAD checked with `git ls-tree`) | 29.5 ms | < 50 ms ✅ |
| M7 (HEAD read in process with gix) | 24.1–24.3 ms | < 50 ms ✅ |
| M7 final (index read in process; raw files hashed, not re-filtered) | **9.7 ms** | < 50 ms ✅ |
| M7 final, 120k-entry index (9.6 MB, synthetic) | 55.3 ms (was 70.3 ms) | ≈ status at that size (< 60 ms) |

What is left per toggle is `git apply --cached` (git reads and rewrites the whole index) and two
in-process index reads (~8 ms each at 120k entries), before and after the apply. The M4 figure
(11.7–12.0 ms) predates the guards.
