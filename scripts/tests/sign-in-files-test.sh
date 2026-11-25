#!/usr/bin/env bash
set -euo pipefail
root=${1:-$(git rev-parse --show-toplevel)}
cd "$root"
fail=0
err() { echo "sign-in-files: $*" >&2; fail=1; }

allowed() {
  cat <<'LIST'
core/src/harness/accounts.rs|name for the existence checks below|const CLAUDE_SIGNED_IN: &str = ".claude.json";
core/src/harness/accounts.rs|name for the existence checks below|const CLAUDE_SIGNED_IN: &str = ".credentials.json";
core/src/harness/accounts.rs|name for the existence checks below|Harness::ClaudeCode => Some(("CLAUDE_CONFIG_DIR", ".claude", CLAUDE_SIGNED_IN)),
core/src/harness/accounts.rs|name for the existence checks below|Harness::Codex => Some(("CODEX_HOME", ".codex", "auth.json")),
core/src/harness/accounts.rs|takes the folder names only|let (variable, base, _) = names(harness)?;
core/src/harness/accounts.rs|takes the folder name only|let (_, base, _) = names(harness)?;
core/src/harness/accounts.rs|takes the name for the existence check below|let Some((_, base, marker)) = names(harness) else { return accounts };
core/src/harness/accounts.rs|existence only|names(harness).is_some_and(|(_, _, marker)| std::fs::symlink_metadata(dir.join(marker)).is_ok())
core/src/harness/accounts.rs|existence only|.filter(|dir| dir.is_dir() && dir.join(marker).exists())
core/src/workspace/t3/source.rs|T3 Code's data files, named in PRIVACY.md|.find(|dir| MARKERS.iter().any(|marker| dir.join(marker).is_file()))
core/src/tunnel/setup.rs|existence only|if !creds.join("cert.pem").exists() {
core/src/workspace/providers.rs|Eukhe's own file, named in PRIVACY.md|let auth = read_object(&agent.join("auth.json")).unwrap_or_else(|error| {
core/src/workspace/providers.rs|Eukhe's own file, named in PRIVACY.md|let lock = path.with_file_name("auth.json.lock");
core/src/workspace/providers.rs|Eukhe's own file, named in PRIVACY.md|let path = agent.join("auth.json");
core/src/workspace/providers.rs|Eukhe's own file, named in PRIVACY.md|.and_then(|agent| read_object(&agent.join("auth.json")).ok())
core/src/workspace/providers.rs|Eukhe's own file, named in PRIVACY.md|blocking(move || Ok(stored_key(&read_object(&agent.join("auth.json"))?, &provider))).await?
core/src/scratch.rs|smoke guard on a scratch Eukhe folder|let auth = agent.join("auth.json");
core/src/scratch.rs|smoke guard on a scratch Eukhe folder|std::fs::read(agent.join("auth.json"))
macos/Sources/App/ProvidersSmoke.swift|smoke on a scratch Eukhe folder|let auth = (agent as NSString).appendingPathComponent("auth.json")
macos/Sources/App/ProvidersSmoke.swift|smoke on a scratch Eukhe folder|for name in ["models.json", "auth.json"] {
core/src/plugins/compat.rs|words that get a plugin refused|"find-generic-password",
core/src/plugins/compat.rs|words that get a plugin refused|"find-internet-password",
core/src/plugins/compat.rs|words that get a plugin refused|"dump-keychain",
core/src/plugins/compat.rs|words that get a plugin refused|"cookies.sqlite",
ios/Packages/LocalStudioRemote/Sources/LocalStudioRemote/CredentialStore.swift|the app's own keychain items|guard SecItemCopyMatching(request as CFDictionary, &result) == errSecSuccess, let data = result as? Data else {
LIST
}

claude='credentials\.json|\.claude\.json|CLAUDE_SIGNED_IN|names\(harness\)|join\(marker\)'
others='["'\''/]auth\.json(\.lock)?["'\''`]|\.codex/auth\.json|["'\''/]hosts\.ya?ml|\bauth["'\'', ]+token|auth"\)\.arg\("token|show-token|cloud-auth-token|cert\.pem|\.netrc|\.git-credentials'
keychain='get_(generic|internet)_password|security_framework|keyring::|Claude Code-credentials|find-(generic|internet)-password|dump-keychain|SecKeychain|SecItemCopyMatching|secret-tool|libsecret|org\.freedesktop\.secrets|CryptUnprotectData'
browser='Cookies["'\''/]|cookies\.(sqlite|binarycookies)|Login Data|logins\.json|key4\.db'

found=$(git ls-files -co --exclude-standard -- core remote macos linux ios kit controller |
  grep -vE '(^|/)(tests?|Tests|examples|fixtures|docs)/|/tests\.rs$|^ios/(tools|LocalStudioSmoke)/|\.(md|lock|png|icns|mp3|svg|pbxproj|car)$' |
  while read -r file; do
    { grep -aE "$claude|$others|$keychain|$browser" "$file" || true; } | sed -E "s#^[[:space:]]+##; s#^#$file|#"
  done)

while IFS= read -r use; do
  [ -n "$use" ] || continue
  allowed | sed -E 's#^([^|]*)\|[^|]*\|#\1|#' | grep -qxF -- "$use" ||
    err "${use%%|*} names another tool's sign-in: ${use#*|}"
done <<< "$found"

while IFS='|' read -r file reason line; do
  [ -n "$reason" ] || err "the allowlist entry for $file has no reason: $line"
  grep -qxF -- "$file|$line" <<< "$found" || err "stale allowlist entry: $file no longer has: $line"
done < <(allowed)

[ "$fail" = 0 ] && echo "ok: shipped code opens no other tool's sign-in files"
exit "$fail"
