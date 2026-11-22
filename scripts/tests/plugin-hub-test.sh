#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <plugin_smoke example binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SCRATCH=$2
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
failures=0

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

rm -rf "$SCRATCH/hub"
out=$("$BIN" --hub "$SCRATCH/hub" 2>&1)
code=$?
echo "$out" | sed 's/^/  /'

[ "$code" = 0 ] && echo "$out" | grep -q '^PASS plugin smoke: hub' && pass "plugin_smoke --hub" || fail "plugin_smoke --hub"

for step in \
  "a catalog plugin behind the catalog's pin is offered the pin" \
  "the catalog update reinstalls through the pinned catalog entry" \
  "a plugin that is not told waits for its sensitive permission" \
  "Allow opens a sheet with what it still needs" \
  "then it runs"; do
  echo "$out" | grep -qxF "ok   $step" && pass "$step" || fail "$step"
done

echo "failures: $failures"
exit "$failures"
