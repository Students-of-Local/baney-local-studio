#!/usr/bin/env bash
set -uo pipefail
[ $# -eq 2 ] || { echo "usage: $0 <local-studio binary> <scratch dir>"; exit 2; }
BIN=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
SCRATCH=$2
mkdir -p "$SCRATCH"
SCRATCH=$(cd "$SCRATCH" && pwd -P)
failures=0

pass() { echo "PASS $1"; }
fail() { echo "FAIL $1"; failures=$((failures + 1)); }
check() { if [ "$1" = 0 ]; then pass "$2"; else fail "$2"; fi; }

S=$SCRATCH/link
rm -rf "$S"
mkdir -p "$S/h/code" "$S/d/plugins" "$S/a" "$S/out"
H=$S/h
D=$S/d
A=$S/a
echo '{}' > "$A/settings.json"

ls_() { HOME=$H LOCAL_STUDIO_DATA_DIR=$D EUKHE_CODING_AGENT_DIR=$A "$BIN" "$@"; }

skills_only() {
  mkdir -p "$1/skills/hello"
  printf 'id = "%s"\nversion = "1.0.0"\ndescription = "Linked notes"\n' "$(basename "$1")" > "$1/plugin.toml"
  printf -- '---\nname: hello\ndescription: Says hello\n---\nSay hello.\n' > "$1/skills/hello/SKILL.md"
}

skills_only "$H/code/notes"
ln -s "$H/code/notes" "$D/plugins/notes"

mkdir -p "$H/code/capbox/bin"
printf 'id = "capbox"\nversion = "1.0.0"\ndescription = "Linked with a capability"\ncapabilities = ["mcp-server"]\n[mcp]\ncommand = "bin/server"\n' \
  > "$H/code/capbox/plugin.toml"
printf '#!/bin/sh\necho box\n' > "$H/code/capbox/bin/server"
chmod 755 "$H/code/capbox/bin/server"
ln -s "$H/code/capbox" "$D/plugins/capbox"

skills_only "$S/out/away"
ln -s "$S/out/away" "$D/plugins/away"

echo "== plugin list"
listed=$(ls_ plugin list 2>&1)
echo "$listed" | sed 's/^/  /'

echo "== a linked plugin with no capabilities inside home loads with its skills"
echo "$listed" | grep -q '^notes 1.0.0: Linked notes$' && echo "$listed" | grep -q '^  skill hello (plugin:notes)$' \
  && ! echo "$listed" | grep -q "^failed $D/plugins/notes:"
check $? "linked skills-only plugin inside home loads with its skills"

echo "== a linked plugin that asks for a capability is refused"
echo "$listed" | grep -A1 "^failed $D/plugins/capbox:" | grep -q 'symlink and the plugin asks for capabilities (mcp-server)' \
  && ! grep -q plugin-capbox "$A/settings.json"
check $? "linked plugin with a capability refused, no mcpServers entry"

echo "== a link that resolves outside home is refused"
echo "$listed" | grep -A1 "^failed $D/plugins/away:" | grep -q 'outside your home folder'
check $? "linked plugin outside home refused"

echo "== plugin list exits non-zero only for the two refused links"
echo "$listed" | grep -q '^error: 2 plugins failed to load$'
check $? "exactly two plugins failed to load"

echo "== an edit to the linked folder changes the hash"
before=$(ls_ plugin hash "$D/plugins/notes" 2>&1)
direct=$(ls_ plugin hash "$H/code/notes" 2>&1)
printf -- '---\nname: hello\ndescription: Says hello, edited\n---\nSay hello twice.\n' > "$H/code/notes/skills/hello/SKILL.md"
after=$(ls_ plugin hash "$D/plugins/notes" 2>&1)
echo "  through the link before: $before"
echo "  the target directly:     $direct"
echo "  through the link after:  $after"
case $before in sha256:*) [ "$before" = "$direct" ] && [ "$after" != "$before" ] && [ "${after#sha256:}" != "$after" ]; check $? "hash through the link follows the target and changes on edit";; *) fail "hash through the link follows the target and changes on edit";; esac
edited=$(ls_ plugin list 2>&1)
echo "$edited" | grep -q '^  skill hello (plugin:notes)$'
check $? "edited linked plugin still loads"

echo "== plugin validate accepts a symlinked argument"
ln -s "$H/code/notes" "$S/notes-link"
ln -s "$H/code/capbox" "$S/capbox-link"
out=$(ls_ plugin validate "$S/notes-link" 2>&1)
code=$?
echo "  validate notes-link: exit $code: $out" | head -2
[ $code = 0 ] && echo "$out" | grep -q '^OK notes 1.0.0 (1 skills, 0 prompts, 0 commands)$'
check $? "validate a symlinked skills-only argument"
out=$(ls_ plugin validate "$S/capbox-link" 2>&1)
code=$?
echo "  validate capbox-link: exit $code: $(echo "$out" | head -1)"
[ $code = 0 ] && echo "$out" | grep -q '^OK capbox 1.0.0'
check $? "validate a symlinked argument with a capability"

echo "failures: $failures"
exit "$failures"
