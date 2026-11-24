#!/usr/bin/env bash
set -euo pipefail
cd "${1:-$(git rev-parse --show-toplevel)}/core/src/tunnel/worker"
err() { echo "relay-bundle: $*" >&2; exit 1; }

digest() { if command -v sha256sum > /dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }

grep -qxE 'repo https://\S+ \S+' SOURCE || err "SOURCE names no repo"
grep -qxE 'commit [0-9a-f]{40}' SOURCE || err "SOURCE names no full commit"
for file in relay.js wrangler.jsonc; do
  want=$(awk -v file="$file" '$2 == file { print $1 }' SOURCE)
  [[ $want =~ ^[0-9a-f]{64}$ ]] || err "SOURCE has no SHA-256 for $file"
  got=$(digest "$file")
  [ "$got" = "$want" ] || err "$file is $got, not the $want SOURCE names"
done
echo "ok: the relay Worker files match SOURCE"
