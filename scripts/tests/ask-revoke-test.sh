#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <ask_smoke binary> <eukhe> <scratch dir>"; exit 2; }
out=$("$1" "$2" "$3" 2>&1)
code=$?
for step in "ok   askfix runs in thread two through its ACP registration, with the ask variables" \
  "ok   with Studio closed and the daemon keeping thread two, askfix's ask-user grant is revoked" \
  "ok   the model called askfix in thread two after the restart"; do
  grep -qxF "$step" <<< "$out" || { echo "$out" | tail -5; echo "missing: $step"; exit 1; }
done
grep -q '^ok   the daemon dropped thread two'"'"'s plugin-askfix ACP registration' <<< "$out" || { echo "$out" | tail -5; echo "missing: the daemon dropped the revoked registration"; exit 1; }
grep -q '^PASS ask smoke' <<< "$out" || { echo "$out" | tail -5; echo "missing: PASS ask smoke"; exit 1; }
[ $code -eq 0 ] || { echo "ask_smoke exited $code"; exit 1; }
echo ok
