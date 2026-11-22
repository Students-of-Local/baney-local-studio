#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 3 ] || { echo "usage: $0 <harness_queue_smoke binary> <eukhe> <scratch dir>"; exit 2; }
out=$("$1" "$2" "$3" 2>&1)
code=$?
steps=()
for cli in claude codex; do
  a=".$cli-a" b=".$cli-b" label="$cli-account"
  steps+=(
    "ok   $label: a new thread starts on the first account $a"
    "ok   $label: a turn that hits the usage limit on $a is answered on $b"
    "ok   $label: the transcript says the thread moved from $a to $b"
    "ok   $label: the thread now runs on $b"
    "ok   $label: the session file is in $b"
    "ok   $label: the switch raised no Failed notification"
    "ok   $label: a new thread after the switch starts on $b"
    "ok   $label: with every account limited the error names when the first one resets"
    "ok   $label: with every account limited a Failed notification names the reset"
  )
done
steps+=("PASS harness_queue_smoke")
for step in "${steps[@]}"; do
  grep -qxF "$step" <<< "$out" || { echo "$out" | grep -E '^(MISS|Error|- )' | head -20; echo "missing: $step"; exit 1; }
done
[ $code -eq 0 ] || { echo "harness_queue_smoke exited $code"; exit 1; }
echo ok
