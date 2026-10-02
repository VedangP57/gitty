#!/usr/bin/env bash
# Runs the core probe against benchmark repos; compare with the spec §8 latency budget.
# Usage: bench/run.sh <repo-with-blobs>... ; set BLOBLESS=<repo> for history-only checks (e.g. linux).
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q -p gitty-core --example probe
P=target/release/examples/probe
for repo in "$@"; do
  echo "== $repo"
  "$P" walk "$repo" all
  "$P" walk "$repo" head
  "$P" rows "$repo" 500
  "$P" files "$repo" 300
  "$P" files "$repo" 100 stats
  "$P" ab "$repo" || true
done
if [[ -n "${BLOBLESS:-}" ]]; then
  echo "== $BLOBLESS (blobless: history only)"
  "$P" walk "$BLOBLESS" all
  "$P" walk "$BLOBLESS" head
  "$P" abrefs "$BLOBLESS" master v6.0 || true
fi
echo "Budget: first500 < 50ms; kernel full walk < 400ms; commit files p50 < 5ms; ahead/behind worst < 150ms"
