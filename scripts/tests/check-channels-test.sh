#!/usr/bin/env bash
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
check=$here/../check-channels.sh
status=0
out=$("$check" "$here/fixtures/good-channels" 2>&1) || { echo "good fixture failed: $out"; status=1; }
out=$("$check" "$here/fixtures/bad-channels" 2>&1) && { echo "bad fixture passed"; status=1; }
for want in "release.yml starts on a pushed tag" "stable.yml uses the stable environment but also runs on push" "docs/remote.md does not state min_protocol" "promote.yml pushes to the stable branch" "hotfix.yml pushes to the stable branch" "scripts/release.sh pipes into head or grep -q" "promote.yml builds stable with main's build.yml but does not refuse a commit whose build.yml differs" "build.yml stamps a build without exporting its version" "build.yml signs macOS with MAC_SIGNING_P12 but does not keep stable on notarization"; do
  grep -qF "$want" <<<"$out" || { echo "bad fixture did not report: $want"; status=1; }
done
exit $status
