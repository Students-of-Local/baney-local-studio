#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
CATALOG=$(cd "$(dirname "$0")/../.." && pwd)/examples/plugins/catalog.toml
SCRATCH=$2
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
failures=0

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }

S=$SCRATCH/catalog
rm -rf "$S"
mkdir -p "$S/h" "$S/d" "$S/a"
echo '{}' > "$S/a/settings.json"

ls_() { HOME=$S/h LOCAL_STUDIO_DATA_DIR=$S/d EUKHE_CODING_AGENT_DIR=$S/a "$BIN" "$@"; }

caps() { tr -d ' "[]' | tr ',' '\n' | grep -v '^$' | sort | tr '\n' ' '; }

listed() {
  awk -v id="$1" '
    /^\[\[/ { inside = 0 }
    $0 == "id = \"" id "\"" { inside = 1 }
    inside && /^capabilities/ { sub(/^capabilities *= */, ""); print; exit }
  ' "$CATALOG" | caps
}

ids=$(sed -n 's/^id = "\(.*\)"$/\1/p' "$CATALOG")
[ -n "$ids" ] || { echo "FAIL no catalog entries in $CATALOG"; exit 1; }

for id in $ids; do
  out=$(ls_ plugin install "$id" 2>&1)
  code=$?
  echo "$out" | sed 's/^/  /'
  want=$(listed "$id")
  manifest=$S/d/plugins/$id/plugin.toml
  got=$( [ -f "$manifest" ] && sed -n 's/^capabilities *= *//p' "$manifest" | caps )
  echo "  $id: catalog lists [${want% }], plugin.toml asks for [${got% }]"
  if [ "$code" = 0 ] && echo "$out" | grep -q "from catalog $id" && [ -f "$manifest" ] && [ "$want" = "$got" ]; then
    pass "catalog install of $id matches its catalog entry"
  else
    fail "catalog install of $id matches its catalog entry"
  fi
done

echo "failures: $failures"
exit "$failures"
