#!/usr/bin/env bash
set -euo pipefail
apps=true
if [ "${GITHUB_EVENT_NAME:-}" = pull_request ] && git diff --quiet --no-renames HEAD^1 HEAD -- . ':!browser/' ':!docs/'; then
  apps=false
fi
printf 'apps=%s\n' "$apps" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
