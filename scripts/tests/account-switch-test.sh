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
    "ok   $label: a turn on $b is answered"
    "ok   $label: switching back to $a keeps the turn done on $b"
    "ok   $label: the thread runs on $a again"
    "ok   $label: with every account limited the error names when the first one resets"
    "ok   $label: with every account limited a Failed notification names the reset"
  )
done
for case in "claude .claude-b after a message Stop dropped" "claude .claude-b after a steered follow-up" "claude .claude-b after an effort change during the turn" "claude .claude-b after a limit named only in the result text" "codex .codex-b after a held message Codex refused"; do
  read -r cli b rest <<< "$case"
  steps+=(
    "ok   $cli-account: $rest: the limit turn's own message is sent again and answered on $b"
    "ok   $cli-account: $rest: the thread runs on $b"
  )
done
steps+=("PASS harness_queue_smoke")
for step in "${steps[@]}"; do
  grep -qxF "$step" <<< "$out" || { echo "$out" | grep -E '^(MISS|Error|- )' | head -20; echo "missing: $step"; exit 1; }
done
[ $code -eq 0 ] || { echo "harness_queue_smoke exited $code"; exit 1; }
echo ok
