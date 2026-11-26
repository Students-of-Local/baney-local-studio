#!/usr/bin/env bash
set -euo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$2"
exec python3 "$HERE/skills-recall.py" "$BIN" "$(cd "$HERE/../.." && pwd)/skills" "$HERE/skills-recall.tsv" "$(cd "$2" && pwd -P)"
