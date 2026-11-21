#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <smoke binary> <eukhe> <scratch dir>"; exit 2; }
: "${LOCAL_STUDIO_SMOKE_MODEL:?set LOCAL_STUDIO_SMOKE_MODEL to the local model as <provider>/<model>}"
case $LOCAL_STUDIO_SMOKE_MODEL in anthropic/*) echo "LOCAL_STUDIO_SMOKE_MODEL is an Eukhe Claude model; the core checks run a local one"; exit 2 ;; esac
out=$("$1" "$2" "$3" 2>&1)
code=$?
if grep -qF "Eukhe's Claude models are hidden" <<< "$out"; then
  echo "$out" | grep -F "Eukhe's Claude models are hidden" | head -3
  echo "the core smoke reached Eukhe's Claude sign-in refusal"
  exit 1
fi
grep -q '^PASS' <<< "$out" || { echo "$out" | tail -5; echo "missing: PASS"; exit 1; }
[ $code -eq 0 ] || { echo "smoke exited $code"; exit 1; }
echo ok
