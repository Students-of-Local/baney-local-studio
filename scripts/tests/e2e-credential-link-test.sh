#!/usr/bin/env bash
# A scratch agent dir left by the old scheme held auth.json as a link to the real ~/.eukhe/auth.json.
# The examples must replace such links, never write through them. Usage: e2e-credential-link-test.sh <smoke binary>
set -uo pipefail
smoke=${1:?usage: e2e-credential-link-test.sh <smoke binary>}
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
# Runs smoke against <scratch>/<run>. With no Eukhe at the path, smoke either refuses the location at once
# or writes its scratch files and then retries the daemon until the timeout stops it.
run_smoke() {
  LOCAL_STUDIO_SMOKE_MODEL=ollama/none LOCAL_STUDIO_SMOKE_BASE_URL=http://127.0.0.1:9/v1 \
    timeout 20 "$smoke" "$scratch/no-eukhe" "$scratch/$1" > "$scratch/$1.out" 2>&1
}
fail=0

# 1. Stale links from the old scheme inside a scratch agent dir are replaced, never written through.
mkdir -p "$scratch/run/agent"
printf 'real sign-in\n' > "$scratch/credential"
printf 'real catalog\n' > "$scratch/catalog"
ln -s "$scratch/credential" "$scratch/run/agent/auth.json"
ln "$scratch/catalog" "$scratch/run/agent/models.json"
run_smoke run
[ "$(cat "$scratch/credential")" = 'real sign-in' ] || { echo "FAIL the auth.json link's target was overwritten: $(cat "$scratch/credential")"; fail=1; }
[ "$(cat "$scratch/catalog")" = 'real catalog' ] || { echo "FAIL the models.json hard link's target was overwritten"; fail=1; }
grep -q 'no key (auth.json is {})' "$scratch/run.out" || { echo "FAIL smoke never finished writing its scratch agent files"; fail=1; }
{ [ ! -L "$scratch/run/agent/auth.json" ] && [ "$(cat "$scratch/run/agent/auth.json")" = '{}' ]; } || { echo "FAIL auth.json is not a fresh {} file"; fail=1; }

# 2. An agent dir that links to a real one (~/.eukhe/agent here stands in) is refused before anything in it changes.
mkdir -p "$scratch/home/.eukhe/agent" "$scratch/linked"
printf '{"anthropic":{"key":"real"}}\n' > "$scratch/home/.eukhe/agent/auth.json"
printf 'real catalog\n' > "$scratch/home/.eukhe/agent/models.json"
ln -s "$scratch/home/.eukhe/agent" "$scratch/linked/agent"
run_smoke linked
[ "$(cat "$scratch/home/.eukhe/agent/auth.json")" = '{"anthropic":{"key":"real"}}' ] || { echo "FAIL the real auth.json behind a linked agent dir was replaced"; fail=1; }
[ "$(cat "$scratch/home/.eukhe/agent/models.json")" = 'real catalog' ] || { echo "FAIL the real models.json behind a linked agent dir was replaced"; fail=1; }

# 3. A real agent dir passed by mistake as the scratch dir is refused before anything in it changes.
run_smoke home/.eukhe
[ "$(cat "$scratch/home/.eukhe/agent/auth.json")" = '{"anthropic":{"key":"real"}}' ] || { echo "FAIL a real agent dir given as scratch lost its auth.json"; fail=1; }

# 4. A regular auth.json holding keys is never removed.
mkdir -p "$scratch/keys/agent"
printf '{"openai":{"key":"real"}}\n' > "$scratch/keys/agent/auth.json"
run_smoke keys
[ "$(cat "$scratch/keys/agent/auth.json")" = '{"openai":{"key":"real"}}' ] || { echo "FAIL an auth.json holding keys was replaced"; fail=1; }

[ "$fail" = 0 ] || { for out in "$scratch"/*.out; do sed "s|^|  $(basename "$out"): |" "$out"; done; exit 1; }
echo ok
