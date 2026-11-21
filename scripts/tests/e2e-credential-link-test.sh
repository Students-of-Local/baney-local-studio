#!/usr/bin/env bash
# A scratch agent dir left by the old scheme held auth.json as a link to the real ~/.eukhe/auth.json.
# The examples must replace such links, never write through them. Usage: e2e-credential-link-test.sh <smoke binary>
set -uo pipefail
smoke=${1:?usage: e2e-credential-link-test.sh <smoke binary>}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/run/agent"
printf 'real sign-in\n' > "$scratch/credential"
printf 'real catalog\n' > "$scratch/catalog"
ln -s "$scratch/credential" "$scratch/run/agent/auth.json"
ln "$scratch/catalog" "$scratch/run/agent/models.json"
# No Eukhe at this path, so smoke stops right after it writes its scratch agent files.
LOCAL_STUDIO_SMOKE_MODEL=ollama/none LOCAL_STUDIO_SMOKE_BASE_URL=http://127.0.0.1:9/v1 \
  timeout 60 "$smoke" "$scratch/no-eukhe" "$scratch/run" > "$scratch/out" 2>&1
fail=0
[ "$(cat "$scratch/credential")" = 'real sign-in' ] || { echo "FAIL the auth.json link's target was overwritten: $(cat "$scratch/credential")"; fail=1; }
[ "$(cat "$scratch/catalog")" = 'real catalog' ] || { echo "FAIL the models.json hard link's target was overwritten"; fail=1; }
[ ! -L "$scratch/run/agent/auth.json" ] && [ "$(cat "$scratch/run/agent/auth.json")" = '{}' ] || { echo "FAIL auth.json is not a fresh {} file"; fail=1; }
[ "$fail" = 0 ] || { sed 's/^/  smoke: /' "$scratch/out"; exit 1; }
echo ok
