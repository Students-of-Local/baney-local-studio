#!/usr/bin/env bash
set -euo pipefail
apps=true
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ]; then
  if changed=$(git diff --name-only --no-renames HEAD^1 HEAD) && ! grep -qvE '^"?(browser|docs)/' <<<"$changed"; then
    apps=false
  fi
fi
printf 'apps=%s\n' "$apps" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
