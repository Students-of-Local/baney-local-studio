#!/usr/bin/env bash
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 <local-studio binary>"; exit 2; }
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
scratch=$(mktemp -d)
trap 'rm -r "$scratch"' EXIT
mkdir "$scratch/h" "$scratch/d"
run() { HOME=$scratch/h LOCAL_STUDIO_DATA_DIR=$scratch/d "$bin" threads "$@"; }
expect() { [ "$1" = "$2" ] || { echo "FAIL $3: $1"; exit 1; }; echo "ok   $3"; }

expect "$(run --json)" "[]" "no state file lists nothing"
[ -z "$(ls -A "$scratch/d")" ] || { echo "FAIL listing wrote $(ls -A "$scratch/d")"; exit 1; }
cat > "$scratch/d/state.json" <<'JSON'
{"version": 1, "projects": [{"path": "/p/app", "name": "app"}], "threads": {
  "old": {"project": "/p/app", "harness": "claudeCode", "nativeSession": "s", "title": "Fix the build\nand more", "activityAt": 10},
  "new": {"cwd": "/p/app/wt", "harness": "codex", "activityAt": 20},
  "eukhe": {"project": "/p/app", "title": "in Eukhe", "activityAt": 30},
  "put-away": {"project": "/p/app", "harness": "codex", "archived": true, "activityAt": 40},
  "kept-local": {"project": "/p/app", "harness": "codex", "localOnly": "ledger", "activityAt": 50}
}}
JSON
before=$(cksum < "$scratch/d/state.json")
expect "$(run --json)" '[{"id":"new","title":"New thread","project":"/p/app/wt","harness":"codex","modified":20},{"id":"old","title":"Fix the build","project":"/p/app","harness":"claude-code","modified":10}]' \
  "Claude Code and Codex threads, newest first, without archived and local-only ones"
expect "$(run | wc -l | tr -d ' ')" 2 "without --json, one line for each"
expect "$(cksum < "$scratch/d/state.json") $(ls -A "$scratch/d")" "$before state.json" "listing changes no file"
echo '{"version": 1, "thr' > "$scratch/d/state.json"
if run --json > "$scratch/out" 2> "$scratch/err"; then echo "FAIL a broken state file was listed"; exit 1; fi
expect "$(cat "$scratch/out")" "" "a broken state file prints no list and fails"
echo "PASS threads list"
