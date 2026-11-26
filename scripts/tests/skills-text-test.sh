#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
exec python3 "$HERE/skills-text.py" "${1:-$(cd "$HERE/../.." && pwd)/skills}"
