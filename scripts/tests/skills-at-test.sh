#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <skills_smoke binary> <local-studio> <scratch dir>"; exit 2; }
out=$("$1" "$2" "$3" 2>&1)
code=$?
for step in "ok   and a plugin keeps its row" "ok   the last of many has its skills" "PASS skills_smoke"; do
  grep -qxF "$step" <<< "$out" || { echo "$out" | grep -v "^ok" | tail -5; echo "missing: $step"; exit 1; }
done
[ $code -eq 0 ] || { echo "skills_smoke exited $code"; exit 1; }
echo ok
