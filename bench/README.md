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

## Diff engine — 2026-10-02 (M2a), load average ~25

`probe diffs <repo> 300`: for every file of the first 300 commits on HEAD, this loads blobs, diffs them
(Myers + indent heuristic), computes intraline for every change block, and builds the view.

| Repo | files | change blocks | p50 / file | p99 / file | max | Budget |
|---|---|---|---|---|---|---|
| git-cg | 1,108 | 7,586 | **0.18–0.22 ms** | **6.2–6.9 ms** | 13–15 ms | p50 < 1 ms, p99 < 10 ms ✅ |
After the M2a review fix (no manual trim, lazy pairing): p50 0.21–0.25 ms, p99 6.4–9.4 ms. The probe still
computes intraline for every block, which is the worst case.
