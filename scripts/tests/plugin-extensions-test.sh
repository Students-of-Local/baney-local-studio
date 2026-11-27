#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
bun=$(command -v bun)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/ls-ext-test.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
fail() { echo "plugin-extensions: FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
sums() { find "$scratch/t3" "$scratch/omp" "$scratch/home" "$scratch/claude-b" -type f -print0 | sort -z | xargs -0 cksum; }
adds() { awk '/mcp add-json/ { n++ } END { print n + 0 }' "$STUB_LOG"; }

mkdir -p "$scratch/bin" "$scratch/nobrowser" "$scratch/home/.claude" "$scratch/t3/userdata" "$scratch/omp" "$scratch/claude-b" "$scratch/state"
export BUN_RUNTIME_TRANSPILER_CACHE_PATH=0 HOME=$scratch/home STUB_LOG=$scratch/claude.log STUB_STATE=$scratch/state BUN=$bun LOCAL_STUDIO_CONTROLLER_HOME=$scratch/controller
unset T3CODE_HOME PI_CODING_AGENT_DIR CLAUDE_CONFIG_DIR
touch "$STUB_LOG"
cat > "$scratch/bin/claude" <<'EOF'
#!/usr/bin/env bash
printf '%s\t%s\n' "${CLAUDE_CONFIG_DIR-unset}" "$*" >> "$STUB_LOG"
store="$STUB_STATE/$(printf '%s' "${CLAUDE_CONFIG_DIR-default}" | tr / _)"
mkdir -p "$store"
case "$1 $2" in
  "mcp get") [ -f "$store/$3" ] || { echo "No MCP server named \"$3\"." >&2; exit 1; }; cat "$store/$3" ;;
  "mcp add-json") "$BUN" -e 'const j = JSON.parse(process.argv[2]); console.log(`${process.argv[1]}:\n  Scope: User config\n  Type: stdio\n  Command: ${j.command}\n  Args: ${j.args.join(" ")}`)' "$3" "$4" > "$store/$3" ;;
  "mcp remove") [ -f "$store/$3" ] || { echo "No MCP server named \"$3\" in user scope" >&2; exit 1; }; rm "$store/$3" ;;
esac
EOF
printf '#!/bin/sh\nexit 0\n' > "$scratch/bin/local-studio-browser"
printf '#!/bin/sh\nread -r _\necho %s\nsleep 1\n' "'{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"agentInfo\":{\"name\":\"omp\"}}}'" > "$scratch/bin/omp"
chmod +x "$scratch/bin/claude" "$scratch/bin/local-studio-browser" "$scratch/bin/omp"
ln -s "$bun" "$scratch/bin/bun"
ln -s "$scratch/bin/claude" "$scratch/nobrowser/claude"
ln -s "$bun" "$scratch/nobrowser/bun"
path="$scratch/bin:/usr/bin:/bin"
settings=$scratch/t3/userdata/settings.json
mcp=$scratch/omp/mcp.json
record=$scratch/t3/userdata/localstudio-extensions.json
write_settings() {
  printf '{"providerInstances":{"localstudio":{"driver":"acpRegistry","enabled":true,"config":{"commandPath":"%s/bin/omp","commandArgs":["acp"]}},"claude-a":{"driver":"claudeAgent","enabled":true,"config":{"homePath":""}},"claude-b":{"driver":"claudeAgent","enabled":true,"config":{"homePath":"%s"}},"claude-off":{"driver":"claudeAgent","enabled":false,"config":{"homePath":"%s/claude-off"}},"codex":{"driver":"codex","enabled":true,"config":{}}}}\n' "$scratch" "$2" "$scratch" > "$1"
}
write_settings "$settings" "$scratch/claude-b"
printf '{"theme":"dark","mcpServers":{"polycarp":{"type":"stdio","command":"/opt/polycarp","args":["mcp","--x"],"enabled":true},"notion":{"type":"http","url":"http://127.0.0.1:9/mcp","headers":{"a":"b"}}}}\n' > "$mcp"
printf '{"model":"opus","permissions":{"allow":["Bash(ls)"],"deny":["Read(/x)"]}}\n' > "$scratch/claude-b/settings.json"
cp "$mcp" "$scratch/mcp.orig"
ext() { PATH="$1" "$bun" "$root/plugin/src/cli.ts" extensions "${@:2}"; }
js() { "$bun" -e "$1" "${@:2}" || fail "${@: -1}"; }
keep12='const fs = require("fs"), M = JSON.parse(fs.readFileSync(process.argv[1], "utf8")), O = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
if (JSON.stringify(M.mcpServers.polycarp) !== JSON.stringify(O.mcpServers.polycarp) || JSON.stringify(M.mcpServers.notion) !== JSON.stringify(O.mcpServers.notion) || M.theme !== "dark") throw new Error("other mcp.json keys changed");'

out=$(ext "$path" add browser --t3-home "$scratch/t3" --omp-dir "$scratch/omp") || fail "case 1 exited $?"
js 'const fs = require("fs"), [m, a, b, dir] = process.argv.slice(1);
const M = JSON.parse(fs.readFileSync(m, "utf8"));
if (JSON.stringify(M.mcpServers["local-studio"]) !== JSON.stringify({ type: "stdio", command: dir + "/bin/local-studio-browser", args: ["mcp"], enabled: true })) throw new Error("mcp.json " + JSON.stringify(M.mcpServers));
for (const p of [a, b]) if (!JSON.parse(fs.readFileSync(p, "utf8")).permissions.allow.includes("mcp__local-studio__*")) throw new Error("no allow in " + p);
const B = JSON.parse(fs.readFileSync(b, "utf8"));
if (JSON.stringify(B.permissions) !== JSON.stringify({ allow: ["Bash(ls)", "mcp__local-studio__*"], deny: ["Read(/x)"] }) || B.model !== "opus") throw new Error("claude-b settings " + JSON.stringify(B));' "$mcp" "$HOME/.claude/settings.json" "$scratch/claude-b/settings.json" "$scratch" "case 1 contents"
grep -qxF "unset	mcp add-json local-studio {\"type\":\"stdio\",\"command\":\"$scratch/bin/local-studio-browser\",\"args\":[\"mcp\"]} --scope user" "$STUB_LOG" || fail "case 1: no add-json without CLAUDE_CONFIG_DIR"
grep -qF "$scratch/claude-b	mcp add-json local-studio " "$STUB_LOG" || fail "case 1: no add-json with CLAUDE_CONFIG_DIR"
[ "$(adds)" = 2 ] || fail "case 1: $(adds) add-json calls"
pass "case 1: omp mcp.json, two claude add-json calls (one with CLAUDE_CONFIG_DIR), both allow entries"

js "$keep12" "$mcp" "$scratch/mcp.orig" "case 12 after add"
grep -qxF "skipped: codex (codex not supported yet)" <<< "$out" || fail "case 10: $out"
pass "case 10: codex listed as skipped, exit 0"

before=$(sums); n=$(adds); baks=$(find "$scratch" -name '*.bak-localstudio-*' | wc -l)
ext "$path" add browser --t3-home "$scratch/t3" --omp-dir "$scratch/omp" >/dev/null || fail "case 2 exited $?"
[ "$before" = "$(sums)" ] || fail "case 2: files changed"
[ "$baks" = "$(find "$scratch" -name '*.bak-localstudio-*' | wc -l)" ] || fail "case 2: new backups"
[ "$n" = "$(adds)" ] || fail "case 2: add-json ran again"
pass "case 2: second add changes no file, makes no backup, runs no add-json"

! grep -q claude-off "$STUB_LOG" && [ ! -e "$scratch/claude-off" ] || fail "case 3: disabled instance touched"
pass "case 3: disabled claudeAgent untouched"

mkdir -p "$scratch/fresh/home" "$scratch/fresh/t3/userdata"
write_settings "$scratch/fresh/t3/userdata/settings.json" "$scratch/fresh/claude-b"
n=$(wc -l < "$STUB_LOG")
out=$(HOME=$scratch/fresh/home ext "$path" add browser --dry-run --t3-home "$scratch/fresh/t3" --omp-dir "$scratch/fresh/omp") || fail "case 4 exited $?"
grep -q "would set mcpServers.local-studio" <<< "$out" && grep -q "would run claude mcp add-json" <<< "$out" && grep -q "would write" <<< "$out" || fail "case 4: $out"
[ "$(find "$scratch/fresh" -type f | wc -l)" = 1 ] && [ ! -e "$scratch/fresh/omp" ] && [ ! -e "$scratch/fresh/claude-b" ] || fail "case 4: files written"
! tail -n +"$((n + 1))" "$STUB_LOG" | grep -q 'add-json\|mcp remove' || fail "case 4: claude changed"
pass "case 4: --dry-run prints plans, writes nothing, runs no add-json or remove"

before=$(sums); n=$(wc -l < "$STUB_LOG")
set +e; out=$(ext "$scratch/nobrowser:/usr/bin:/bin" add browser --t3-home "$scratch/t3" --omp-dir "$scratch/omp" 2>&1); code=$?; set -e
[ "$code" = 5 ] && [ "$out" = "localstudio: browser needs local-studio-browser; install it first" ] || fail "case 5: exit $code: $out"
[ "$before" = "$(sums)" ] && [ "$n" = "$(wc -l < "$STUB_LOG")" ] || fail "case 5: something changed"
pass "case 5: no browser binary, exit 5, nothing written"

ext "$path" add ledger --command /bin/echo --arg hi --t3-home "$scratch/t3" --omp-dir "$scratch/omp" >/dev/null || fail "case 6 exited $?"
js 'const fs = require("fs"), [m, a, b, r] = process.argv.slice(1);
if (JSON.stringify(JSON.parse(fs.readFileSync(m, "utf8")).mcpServers.ledger) !== JSON.stringify({ type: "stdio", command: "/bin/echo", args: ["hi"], enabled: true })) throw new Error("mcp.json ledger");
for (const p of [a, b]) if (!JSON.parse(fs.readFileSync(p, "utf8")).permissions.allow.includes("mcp__ledger__*")) throw new Error("no ledger allow in " + p);
if (JSON.stringify(JSON.parse(fs.readFileSync(r, "utf8"))) !== JSON.stringify({ ledger: { command: "/bin/echo", args: ["hi"] } })) throw new Error("record");' "$mcp" "$HOME/.claude/settings.json" "$scratch/claude-b/settings.json" "$record" "case 6 contents"
[ "$(mode "$record")" = 600 ] || fail "case 6: record mode $(mode "$record")"
grep -qF "$scratch/claude-b	mcp add-json ledger {\"type\":\"stdio\",\"command\":\"/bin/echo\",\"args\":[\"hi\"]} --scope user" "$STUB_LOG" || fail "case 6: no ledger add-json"
pass "case 6: custom ledger in mcp.json, allowed, recorded at 0600"

before=$(sums); n=$(wc -l < "$STUB_LOG")
set +e; ext "$path" add Bad_Name --command /bin/echo --t3-home "$scratch/t3" --omp-dir "$scratch/omp" >/dev/null 2>&1; code=$?; set -e
[ "$code" = 2 ] && [ "$before" = "$(sums)" ] && [ "$n" = "$(wc -l < "$STUB_LOG")" ] || fail "case 7: exit $code or something changed"
pass "case 7: bad name, exit 2, nothing written"

out=$(ext "$path" list --json --t3-home "$scratch/t3" --omp-dir "$scratch/omp") || fail "case 8 exited $?"
js 'const r = JSON.parse(process.argv[1]), by = Object.fromEntries(r.extensions.map((e) => [e.name, e.targets]));
const ids = ["omp", "claude-a", "claude-b"];
for (const n of ["browser", "ledger"]) if (JSON.stringify(Object.keys(by[n]).sort()) !== JSON.stringify([...ids].sort()) || ids.some((t) => by[n][t] !== "added")) throw new Error(n + " " + JSON.stringify(by[n]));
if (ids.some((t) => by.skills[t] !== "binary not found")) throw new Error("skills " + JSON.stringify(by.skills));' "$out" "case 8: $out"
pass "case 8: list shows browser and ledger added everywhere, skills binary not found"

n=$(wc -l < "$STUB_LOG")
ext "$path" remove browser --t3-home "$scratch/t3" --omp-dir "$scratch/omp" >/dev/null || fail "case 9 exited $?"
js 'const fs = require("fs"), [m, a, b] = process.argv.slice(1), M = JSON.parse(fs.readFileSync(m, "utf8"));
if ("local-studio" in M.mcpServers || !M.mcpServers.ledger) throw new Error("mcp.json " + JSON.stringify(M));
const A = JSON.parse(fs.readFileSync(a, "utf8")).permissions.allow, B = JSON.parse(fs.readFileSync(b, "utf8")).permissions.allow;
if (JSON.stringify(A) !== JSON.stringify(["mcp__ledger__*"]) || JSON.stringify(B) !== JSON.stringify(["Bash(ls)", "mcp__ledger__*"])) throw new Error("allow " + JSON.stringify([A, B]));' "$mcp" "$HOME/.claude/settings.json" "$scratch/claude-b/settings.json" "case 9 contents"
new=$(tail -n +"$((n + 1))" "$STUB_LOG")
grep -qxF "unset	mcp remove local-studio --scope user" <<< "$new" && grep -qxF "$scratch/claude-b	mcp remove local-studio --scope user" <<< "$new" || fail "case 9: remove log $new"
pass "case 9: remove drops the mcp.json key, runs mcp remove per target, drops one allow entry"

js "$keep12" "$mcp" "$scratch/mcp.orig" "case 12 after remove"
pass "case 12: polycarp and notion kept after add and remove"

"$bun" -e 'const fs = require("fs"), p = process.argv[1], s = JSON.parse(fs.readFileSync(p, "utf8")); s.providerInstances.localstudio.config.commandArgs = ["acp", "--approval-mode=yolo"]; fs.writeFileSync(p, JSON.stringify(s))' "$settings"
out=$(PATH="$path" "$bun" "$root/plugin/src/cli.ts" install --t3-home "$scratch/t3" --omp-dir "$scratch/omp" --controller http://127.0.0.1:9) || fail "case 11 install exited $?"
js 'const s = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); if (JSON.stringify(s.providerInstances.localstudio.config.commandArgs) !== JSON.stringify(["acp", "--approval-mode=yolo"])) throw new Error(JSON.stringify(s.providerInstances.localstudio));' "$settings" "case 11 args"
grep -qx "localstudio extensions add browser: added for 3 target(s)" <<< "$out" || fail "case 11: no extensions line: $out"
[ "$("$bun" -e 'console.log(JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).mcpServers["local-studio"].command)' "$mcp")" = "$scratch/bin/local-studio-browser" ] || fail "case 11: install did not add browser"
pass "case 11: install keeps --approval-mode=yolo and adds the browser extension"

mkdir -p "$scratch/fresh2/t3/userdata" "$scratch/nob2"
ln -s "$bun" "$scratch/nob2/bun"; ln -s "$scratch/bin/omp" "$scratch/nob2/omp"
printf '{"providerInstances":{}}\n' > "$scratch/fresh2/t3/userdata/settings.json"
out=$(PATH="$scratch/nob2:/usr/bin:/bin" HOME=$scratch/fresh2 "$bun" "$root/plugin/src/cli.ts" install --t3-home "$scratch/fresh2/t3" --omp-dir "$scratch/fresh2/omp" --controller http://127.0.0.1:9) || fail "case 13 install exited $?"
js 'const s = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")); if (JSON.stringify(s.providerInstances.localstudio.config.commandArgs) !== JSON.stringify(["acp", "--approval-mode=yolo"])) throw new Error(JSON.stringify(s.providerInstances.localstudio));' "$scratch/fresh2/t3/userdata/settings.json" "case 13 args"
grep -qx "localstudio extensions add browser: skipped, local-studio-browser not found; run it after installing the browser" <<< "$out" && [ ! -e "$scratch/fresh2/omp/mcp.json" ] || fail "case 13: $out"
pass "case 13: a fresh install writes acp and --approval-mode=yolo, and with no browser names extensions add browser and exits 0"
echo "plugin-extensions: all cases passed"
