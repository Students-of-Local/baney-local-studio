#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <relay_setup_smoke binary> <local-studio> <scratch dir>"; exit 2; }
out=$("$1" "$2" "$3" 2>&1)
code=$?
for step in "ok: off on a relay another machine took" "relay_setup_smoke: every step passed"; do
  grep -qxF "$step" <<< "$out" || { echo "$out" | tail -5; echo "missing: $step"; exit 1; }
done
[ $code -eq 0 ] || { echo "relay_setup_smoke exited $code"; exit 1; }
echo ok
