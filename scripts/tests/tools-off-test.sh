#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <tools_smoke binary> <eukhe> <scratch dir>"; exit 2; }
out=$("$1" "$2" "$3" 2>&1)
code=$?
for step in \
  "ok   turning both off took the server from the session before another message" \
  "ok   orchard: the person's own local-studio server got the call" \
  "ok   turning computer use off reached a turn that kept running across a restart"; do
  grep -qxF "$step" <<< "$out" || { tail -n 20 <<< "$out"; echo "missing: $step"; exit 1; }
done
[ $code -eq 0 ] || { echo "tools_smoke exited $code"; exit 1; }
echo ok
