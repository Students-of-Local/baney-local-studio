#!/usr/bin/env bash
set -euo pipefail
script=$(git rev-parse --show-toplevel)/scripts/ci-apps.sh
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
git init -q -b main .
git config user.email ci@example.com
git config user.name ci
mkdir -p browser docs core
touch browser/a docs/a core/a README.md
git add -A
git commit -qm base
fail=0

check() {
  local want=$1 event=$2 what=$3 edit=$4 got
  git checkout -q -B pr main
  eval "$edit"
  git add -A
  git commit -qm "$what" --allow-empty
  git checkout -q -B merge main
  git merge -q --no-ff -m merge pr >/dev/null
  : >out
  GITHUB_EVENT_NAME=$event GITHUB_OUTPUT=$work/out "$script" >/dev/null
  got=$(cat out)
  rm out
  if [ "$got" = "apps=$want" ]; then
    echo "ok   $what ($event): $got"
  else
    echo "FAIL $what ($event): $got, expected apps=$want" >&2
    fail=1
  fi
}

check false pull_request "browser only" 'echo b >browser/a'
check false pull_request "browser and docs" 'echo b >browser/a; echo b >docs/a'
check false pull_request "a browser file with a name git quotes" 'echo b >"browser/é"'
check true pull_request "core" 'echo b >core/a'
check true pull_request "browser and core" 'echo b >browser/a; echo b >core/a'
check true pull_request "README" 'echo b >README.md'
check true pull_request "a file moved from core to browser" 'git mv core/a browser/b'
check true pull_request "a folder that only starts like browser" 'mkdir browser-x; echo b >browser-x/a'
check true pull_request "a core file named like a browser path" 'mkdir -p core/browser; echo b >core/browser/a'
check true push "browser only" 'echo b >browser/a'
check true "" "browser only, outside GitHub" 'echo b >browser/a'
exit "$fail"
