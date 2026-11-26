#!/usr/bin/env bash
set -euo pipefail
# e2e builds into its own target dir in this checkout, never a CARGO_TARGET_DIR shared with other
# worktrees, so another build can't swap the examples between this build and the runs.
target=$(git rev-parse --show-toplevel)/target/e2e
[ "${1:-}" = --target ] && { echo "$target"; exit 0; }
: "${LOCAL_STUDIO_SMOKE_MODEL:?set LOCAL_STUDIO_SMOKE_MODEL to the local model as <provider>/<model>, e.g. ollama/fv-smoke-lfm3-1.7b (docs/e2e.md)}"
case $LOCAL_STUDIO_SMOKE_MODEL in */?*) ;; *) echo "LOCAL_STUDIO_SMOKE_MODEL=$LOCAL_STUDIO_SMOKE_MODEL is not <provider>/<model>" >&2; exit 2 ;; esac
command -v "${NU:-nu}" > /dev/null || { echo "nu is not on PATH; set NU to its path" >&2; exit 2; }
command -v bun > /dev/null || { echo "bun is not on PATH; local_smoke needs it" >&2; exit 2; }
model=${LOCAL_STUDIO_SMOKE_MODEL#*/}
base=${LOCAL_STUDIO_SMOKE_BASE_URL:-http://127.0.0.1:11434/v1}
context_window=${LOCAL_STUDIO_SMOKE_CONTEXT:-32768}
window=${OLLAMA_WINDOW:-$(command -v ollama-window || echo "$HOME/.local/bin/ollama-window")}
window_who=${E2E_WINDOW_WHO:-local-studio-e2e}
window_minutes=${E2E_WINDOW_MINUTES:-60}
window_wait=${E2E_WINDOW_WAIT_MINUTES:-180}
remote_ip=${E2E_REMOTE_IP:-}
[ -n "$remote_ip" ] || remote_ip=$( (ip -4 route get 1.1.1.1 2> /dev/null || true) | awk '{for (i = 1; i < NF; i++) if ($i == "src") { print $(i + 1); exit }}')
[ -n "$remote_ip" ] || remote_ip=$(ipconfig getifaddr en0 2> /dev/null || true)
[ -n "$remote_ip" ] || { echo "no non-loopback IPv4 address for local_smoke's remote stub; set E2E_REMOTE_IP" >&2; exit 2; }
context=local-studio/e2e
repo=${REPO:-baney75/local-studio}
examples=(smoke workspace_smoke working_smoke durable_smoke order_smoke open_smoke stuck_smoke providers_smoke local_smoke plugin_smoke ask_smoke tools_smoke claude_signin_smoke harness_queue_smoke tunnel_setup_smoke relay_setup_smoke harness_claude_smoke harness_codex_smoke t3_import_smoke update_smoke usage_days_smoke skills_smoke)
helpers=(provider_stub fake_claude fake_codex consent_driver)
post=
[ "${1:-}" = --post ] && post=1 && shift
commit=${1:-$(git rev-parse HEAD)}
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$commit^{commit}")" ] || { echo "check out $commit first" >&2; exit 2; }
commit=$(git rev-parse HEAD)
[ -z "$(git status --porcelain --untracked-files=all)" ] || { echo "the checkout is not clean (uncommitted or untracked files):" >&2; git status --short --untracked-files=all >&2; exit 2; }
root=$(pwd)

for file in core/examples/*.rs; do
  name=$(basename "$file" .rs)
  [[ " ${examples[*]} ${helpers[*]} " == *" $name "* ]] || grep -q -- "--example $name\b" .github/workflows/ci.yml || { echo "core/examples/$name.rs is run by neither scripts/e2e.sh nor CI; add it" >&2; exit 2; }
done

status() {
  [ -n "$post" ] || return 0
  gh api "repos/$repo/statuses/$commit" -f state="$1" -f context="$context" -f description="${2:0:140}" > /dev/null
}

claimed=
keeper=
claim() {
  [ -x "$window" ] || return 0
  if [ -z "$claimed" ]; then
    echo "== waiting up to $window_wait min for the GPU window ($window)"
    timeout "$((window_wait * 60))" "$window" wait "$window_minutes" "$model" "$window_who" || { echo "the GPU window stayed busy for $window_wait min: $("$window" status)" >&2; return 1; }
    claimed=1
    # One example can outlast the claim (15 min a model step), so keep it fresh in the background too.
    (while sleep 600; do "$window" claim "$window_minutes" "$model" "$window_who" > /dev/null || true; done) &
    keeper=$!
  else
    "$window" claim "$window_minutes" "$model" "$window_who" > /dev/null || { echo "lost the GPU window: $("$window" status)" >&2; return 1; }
  fi
}

timings=()
summary() {
  echo "== timings (local model $LOCAL_STUDIO_SMOKE_MODEL on $(hostname -s))"
  for line in "${timings[@]}"; do echo "   $line"; done
}

scratch=$(mktemp -d)
bin=
cleanup() {
  for socket in "$scratch"/*/d.sock; do
    [ -S "$socket" ] && [ -n "$bin" ] && "$bin/smoke" --stop "$socket" || true
  done
  [ -n "$keeper" ] && kill "$keeper" 2> /dev/null || true
  [ -n "$claimed" ] && "$window" release "$window_who" > /dev/null || true
  rm -rf "$scratch" 2> /dev/null || { sleep 5; rm -rf "$scratch" || true; }
}
trap cleanup EXIT
eukhe=$(realpath "$("${NU:-nu}" -c 'use eukhe.nu; eukhe binary . target/eukhe')")
# Always build from the checked commit, so a status never rests on examples from another commit,
# then run copies from scratch.
CARGO_TARGET_DIR=$target cargo build -j3 -p local-studio-core --examples --bin local-studio --features link
(cd controller && bun install --frozen-lockfile > /dev/null)
echo "== cluster_smoke"
(cd controller && E2E_REMOTE_IP=$remote_ip node test/cluster_smoke.mjs) || { status failure "cluster_smoke failed on $(hostname -s)"; exit 1; }
bin=$scratch/bin
mkdir -p "$bin"
for example in "${examples[@]}" "${helpers[@]}"; do
  [ -x "$target/debug/examples/$example" ] || { echo "$target/debug/examples/$example is missing after the build" >&2; exit 2; }
  cp "$target/debug/examples/$example" "$bin/"
done
cp "$target/debug/local-studio" "$bin/"
export PLUGIN_SMOKE_MODEL=${PLUGIN_SMOKE_MODEL:-$model}
printf '{"providers":{"local-fleet":{"name":"local-fleet","baseUrl":"%s","api":"openai-completions","apiKey":"none","models":[{"id":"%s","contextWindow":%s}]}}}\n' "$base" "$PLUGIN_SMOKE_MODEL" "$context_window" > "$scratch/plugin-models.json"
claim || { status error "the GPU window on $(hostname -s) stayed busy"; exit 1; }
status pending "local model $LOCAL_STUDIO_SMOKE_MODEL: core e2e running on $(hostname -s)"
started=$SECONDS
for example in "${examples[@]}"; do
  echo "== $example"
  dir="$scratch/$example"
  case $example in
    local_smoke) args=("$eukhe" "$dir" "$remote_ip" "$remote_ip") ;;
    tunnel_setup_smoke | relay_setup_smoke | t3_import_smoke | skills_smoke) args=("$bin/local-studio" "$dir") ;;
    update_smoke) args=("$dir") ;;
    plugin_smoke) args=("$eukhe" "$dir" "$scratch/plugin-models.json" "$root/examples/plugins/smoothstone") ;;
    *) args=("$eukhe" "$dir") ;;
  esac
  claim || { status error "lost the GPU window on $(hostname -s) before $example"; exit 1; }
  from=$SECONDS
  if ! (cd "$bin" && "./$example" "${args[@]}"); then
    timings+=("$example failed after $((SECONDS - from)) s")
    summary
    status failure "local model $LOCAL_STUDIO_SMOKE_MODEL: $example failed on $(hostname -s)"
    exit 1
  fi
  timings+=("$example passed in $((SECONDS - from)) s")
done
timings+=("all ${#examples[@]} passed in $((SECONDS - started)) s")
summary
status success "local model $LOCAL_STUDIO_SMOKE_MODEL: all ${#examples[@]} core e2e checks passed on $(hostname -s)"
echo "e2e ok for $commit"
