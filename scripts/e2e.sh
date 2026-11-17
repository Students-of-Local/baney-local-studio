#!/usr/bin/env bash
set -euo pipefail
: "${LOCAL_STUDIO_SMOKE_AGENT:?set LOCAL_STUDIO_SMOKE_AGENT to the agent credentials dir (AGENTS.md)}"
context=local-studio/e2e
repo=${REPO:-baney75/local-studio}
post=
[ "${1:-}" = --post ] && post=1 && shift
commit=${1:-$(git rev-parse HEAD)}
[ "$(git rev-parse HEAD)" = "$(git rev-parse "$commit^{commit}")" ] || { echo "check out $commit first" >&2; exit 2; }
commit=$(git rev-parse HEAD)
git diff --quiet HEAD || { echo "the checkout has uncommitted changes" >&2; exit 2; }

status() {
  [ -n "$post" ] || return 0
  gh api "repos/$repo/statuses/$commit" -f state="$1" -f context="$context" -f description="$2" > /dev/null
}

scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
eukhe=$(nu -c 'use eukhe.nu; eukhe binary . target/eukhe')
status pending "core smokes running on $(hostname -s)"
cargo build -j3 -p local-studio-core --examples
for example in smoke workspace_smoke working_smoke durable_smoke order_smoke; do
  echo "== $example"
  if ! cargo run -j3 -q -p local-studio-core --example "$example" -- "$eukhe" "$scratch/$example"; then
    status failure "$example failed on $(hostname -s)"
    exit 1
  fi
done
status success "smoke, workspace, working, durable and order passed on $(hostname -s)"
echo "e2e ok for $commit"
