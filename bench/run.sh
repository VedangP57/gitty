#!/usr/bin/env bash
# Runs the core probe against benchmark repos and checks the spec §8 latency budget.
# Usage: bench/run.sh <repo-with-blobs>... ; set BLOBLESS=<repo> for history-only checks (e.g. linux).
# Exits non-zero when any budget is missed; each miss prints the budget and the measured value.
# The frame budget is a criterion bench: cargo bench -p gitty-cli --bench frame.
set -uo pipefail
cd "$(dirname "$0")/.."
cargo build --release -q -p gitty-core --example probe || exit 1
P=target/release/examples/probe
failed=0
check() {
  "$P" "$@" --check || failed=1
}
for repo in "$@"; do
  echo "== $repo"
  # spec §8 budgets are for a warm cache: the first run only warms it
  "$P" walk "$repo" all > /dev/null || failed=1
  check walk "$repo" all
  check walk "$repo" head
  "$P" rows "$repo" 500 || failed=1
  check files "$repo" 300
  "$P" files "$repo" 100 stats || failed=1
  check diffs "$repo" 300
  check ab "$repo"
  check status "$repo"
done
if [[ -n "${BLOBLESS:-}" ]]; then
  echo "== $BLOBLESS (blobless: history only)"
  "$P" walk "$BLOBLESS" all > /dev/null || failed=1
  check walk "$BLOBLESS" all
  check walk "$BLOBLESS" head
  check abrefs "$BLOBLESS" master v6.0
fi
if [[ $failed -ne 0 ]]; then
  echo "Some budgets were missed (see BUDGET MISSED above)."
  exit 1
fi
echo "All budgets met."
