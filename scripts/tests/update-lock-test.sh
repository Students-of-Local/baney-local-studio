#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <update_smoke example binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SCRATCH=$2
rm -rf "$SCRATCH"
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
failures=0

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

out=$("$BIN" "$SCRATCH" 2>&1)
code=$?
echo "$out" | sed 's/^/  /'

[ "$code" = 0 ] && echo "$out" | grep -qx 'PASS update smoke' && pass "update_smoke" || fail "update_smoke"

for step in \
  "a second apply while one holds the update is refused and touches nothing" \
  "apply swaps in the new build and keeps the previous one" \
  "a new build that fails after the swap is rolled back"; do
  echo "$out" | grep -qxF "ok: $step" && pass "$step" || fail "$step"
done

echo "failures: $failures"
exit "$failures"
