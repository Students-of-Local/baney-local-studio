#!/usr/bin/env bash
set -euo pipefail
root=${1:-$(git rev-parse --show-toplevel)}
cd "$root"
fail=0
err() { echo "privacy-hosts: $*" >&2; fail=1; }

allowed() {
  cat <<'LIST'
controller/src/share.ts|local-ai-registry.dev|JSON schema id, never fetched
core/src/tunnel/setup.rs|dash.cloudflare.com|printed for the user to open
core/src/tunnel/setup.rs|developers.cloudflare.com|printed for the user to open
core/src/tunnel/setup/relay.rs|dash.cloudflare.com|printed for the user to open
core/src/tunnel/setup/relay.rs|nodejs.org|printed for the user to open
core/src/tunnel/setup.rs|www.apple.com|property list DTD name
remote/src/service.rs|www.apple.com|property list DTD name
ios/LocalStudio/Info.plist|www.apple.com|property list DTD name
macos/LocalStudio-Info.plist|www.apple.com|property list DTD name
linux/src/fleet.rs|local-ai-registry.vercel.app|opens in the browser on a click
macos/Sources/App/StudioCommands.swift|local-ai-registry.vercel.app|opens in the browser on a click
macos/Sources/Features/Pages/Local/LocalFleetSections.swift|local-ai-registry.vercel.app|opens in the browser on a click
linux/src/scenes.rs|docs.gtk.org|sample text for screenshots
macos/Sources/Features/Conversation/Conversation+Snapshots.swift|developer.apple.com|sample text for screenshots
linux/src/dashboard_scenes.rs|server.tail1234.ts.net|made-up machine for screenshots
linux/src/fixtures.rs|laptop.tail1234.ts.net|made-up machine for screenshots
linux/src/fixtures.rs|studio.tail1234.ts.net|made-up machine for screenshots
linux/src/scenes_machines.rs|studio-mac.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Pages/Local/LocalDashboardFixtures.swift|server.tail1234.ts.net|made-up machine for screenshots
macos/Sources/Features/Pages/Pages+Snapshots.swift|studio-mac.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Sidebar/Sidebar+Snapshots.swift|chapel1.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Sidebar/Sidebar+Snapshots.swift|lab-box.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Sidebar/Sidebar+Snapshots.swift|old-laptop.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Sidebar/Sidebar+Snapshots.swift|parish1.tail0000.ts.net|made-up machine for screenshots
macos/Sources/Features/Sidebar/Sidebar+Snapshots.swift|studio-mini.tail0000.ts.net|made-up machine for screenshots
LIST
}

shipped() {
  git ls-files -co --exclude-standard -- core remote macos linux ios kit controller scripts/install.sh |
    grep -vE '(^|/)(tests?|Tests|examples|fixtures|docs)/|/tests\.rs$|^ios/(tools|LocalStudioSmoke)/|\.(md|lock|png|icns|mp3|svg|pbxproj|car)$|/\.swiftlint\.yml$'
}

local_host() {
  grep -qE '(^|\.)(localhost|invalid|example|test|lan|local|example\.(com|org|net))$|^(127|10|192\.168|172\.(1[6-9]|2[0-9]|3[01])|100\.(6[4-9]|[7-9][0-9]|1[01][0-9]|12[0-7])|0\.0)(\.[0-9]+)+$' <<< "$1"
}

named=$(grep -oE '`[a-z0-9-]+(\.[a-z0-9-]+)+`' PRIVACY.md | tr -d '`' | sort -u)
found=$(shipped | while read -r file; do
  { grep -aoiE '(https?|wss?|ssh|git)://([^][:space:]/@"'\''`<>{}()$\\]+@)?[a-z0-9-]+(\.[a-z0-9-]+)+|\bgit@[a-z0-9-]+(\.[a-z0-9-]+)+:' "$file" || true; } |
    sed -E 's#^[A-Za-z]+://##; s#^.*@##; s#:$##' | tr '[:upper:]' '[:lower:]' | sort -u | sed "s#^#$file|#"
done)
[ "$(wc -l <<< "$found")" -gt 10 ] || err "found almost no hosts, so the scan is broken"

while IFS='|' read -r file host; do
  local_host "$host" && continue
  grep -qxF -- "$host" <<< "$named" && continue
  allowed | cut -d'|' -f1,2 | grep -qxF -- "$file|$host" && continue
  err "$file names $host, which PRIVACY.md does not name in backticks"
done <<< "$found"

while IFS='|' read -r file host reason; do
  [ -n "$file" ] || continue
  [ -n "$reason" ] || err "the allowlist entry $file|$host has no reason"
  grep -qxF -- "$file|$host" <<< "$found" || err "stale allowlist entry: $file no longer names $host"
  if grep -qxF -- "$host" <<< "$named"; then err "stale allowlist entry: PRIVACY.md names $host"; fi
done < <(allowed)

[ "$fail" = 0 ] && echo "ok: every host shipped code names is in PRIVACY.md or allowed with a reason"
exit "$fail"
