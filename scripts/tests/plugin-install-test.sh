#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
bun=$(command -v bun)
omp_bin=${OMP_BIN:-$(mise which omp 2>/dev/null || command -v omp)}
scratch=$(mktemp -d "${TMPDIR:-/tmp}/ls-plugin-test.XXXXXX")
pids=()
cleanup() { for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$scratch"; }
trap cleanup EXIT
fail() { echo "plugin-install: FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }
mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
sums() { find "$1" -type f -print0 | sort -z | xargs -0 cksum; }

mkdir -p "$scratch/bin" "$scratch/bunonly" "$scratch/home" "$scratch/t3/userdata" "$scratch/omp"
ln -s "$omp_bin" "$scratch/bin/omp"
ln -s "$bun" "$scratch/bin/bun"
ln -s "$bun" "$scratch/bunonly/bun"
export HOME=$scratch/home LOCAL_STUDIO_CONTROLLER_HOME=$scratch/controller
unset T3CODE_HOME PI_CODING_AGENT_DIR
settings=$scratch/t3/userdata/settings.json
models=$scratch/omp/models.yml
t3=$scratch/t3; ompd=$scratch/omp
run_install() { PATH="$1" "$bun" "$root/plugin/src/cli.ts" install --t3-home "$t3" --omp-dir "$ompd" "${@:2}"; }
check() { "$bun" -e "$1" "$t3/userdata/settings.json" "$ompd/models.yml" "$scratch/bin/omp" "${@:2}" || fail "${*:2}"; }
use() { t3=$scratch/$1/t3; ompd=$scratch/$1/omp; mkdir -p "$t3/userdata" "$ompd"; }
baks() { find "$t3" "$ompd" -name '*.bak-localstudio-*' | wc -l; }
setjson() { printf '%s\n' "$1" > "$t3/userdata/settings.json"; }
lsi='"localstudio":{"driver":"acpRegistry","displayName":"Local Studio","enabled":true,"config":{"source":"local","commandPath":"/usr/local/bin/omp","commandArgs":["acp","--approval-mode=yolo"],"customModels":[]}}'
p="$scratch/bin:/usr/bin:/bin"

stub_port=$("$bun" -e 'const s=Bun.serve({port:0,fetch:()=>new Response("")});console.log(s.port);s.stop()')
STUB_PORT=$stub_port "$bun" -e 'Bun.serve({hostname:"127.0.0.1",port:Number(process.env.STUB_PORT),fetch:(r)=>new URL(r.url).pathname.endsWith("/models")?Response.json({object:"list",data:[{id:"stub-model",object:"model"},{id:"stub-long",object:"model",context_length:32768}]}):new Response("{}",{status:404})})' &
pids+=($!)
for _ in $(seq 40); do curl -sf "http://127.0.0.1:$stub_port/v1/models" >/dev/null 2>&1 && break; sleep 0.25; done
stub="http://127.0.0.1:$stub_port"

setjson '{"linkedEnvironmentsEnabled":true,"providerInstances":{"omp":{"driver":"acpRegistry","displayName":"My omp","enabled":true,"config":{"source":"local","commandPath":"/opt/omp","commandArgs":["--mode","acp"],"customModels":["x"]}},'"$lsi"',"codex":{"driver":"codex","enabled":true,"config":{}}}}'
printf '%s\n' 'disabledProviders:' '  - nobody' 'providers:' '  synthetic:' '    baseUrl: http://127.0.0.1:9/v1' '    api: openai-completions' '    apiKey: SYNTHETIC_API_KEY' '    models:' '      - id: hf:one' '        contextWindow: 131072' '      - id: hf:two' '        contextWindow: 65536' '      - id: hf:three' > "$models"
chmod 640 "$settings" "$models"
cp "$settings" "$scratch/settings.orig"; cp "$models" "$scratch/models.orig"

out=$(run_install "$p" --controller "$stub" --dry-run) || fail "case 10 exited $?"
grep -qx 'would remove the separate "Local Studio" omp entry; its threads continue under omp' <<< "$out" && grep -q "^would write $settings" <<< "$out" && grep -q "^would write $models" <<< "$out" || fail "case 10: $out"
cmp -s "$settings" "$scratch/settings.orig" && cmp -s "$models" "$scratch/models.orig" && [ "$(baks)" = 0 ] || fail "case 10: files written"
pass "case 10: --dry-run on case 1 prints the plans and writes nothing"

out=$(run_install "$p" --controller "$stub") || fail "case 1 exited $?"
grep -qx 'removed the separate "Local Studio" omp entry; its threads continue under omp' <<< "$out" || fail "case 1: no removed line: $out"
grep -qx 'local models added to: omp (omp)' <<< "$out" || fail "case 1: no harness line: $out"
grep -qx "the old Local Studio entry ran omp with --approval-mode=yolo; your omp entry keeps its own approval mode; to run it without prompts, add --approval-mode=yolo after --mode acp in its commandArgs" <<< "$out" || fail "case 1: no kept note: $out"
! grep -q "instance runs omp with --approval-mode=yolo" <<< "$out" || fail "case 1: yolo notice: $out"
check '
const fs = require("fs"), [s, m, omp, origS, origM] = process.argv.slice(1);
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const S = JSON.parse(fs.readFileSync(s, "utf8")), O = JSON.parse(fs.readFileSync(origS, "utf8"));
const M = Bun.YAML.parse(fs.readFileSync(m, "utf8")), OM = Bun.YAML.parse(fs.readFileSync(origM, "utf8"));
if ("localstudio" in S.providerInstances) throw new Error("localstudio instance kept");
if (!eq(S.providerInstances.omp.config.commandArgs, ["--mode", "acp"])) throw new Error("omp args " + JSON.stringify(S.providerInstances.omp));
if (!eq(S.providerInstances.omp, O.providerInstances.omp) || !eq(S.providerInstances.codex, O.providerInstances.codex) || S.linkedEnvironmentsEnabled !== true || !eq(Object.keys(S), Object.keys(O)) || !eq(Object.keys(S.providerInstances), ["omp", "codex"])) throw new Error("other settings changed " + JSON.stringify(S));
if (!eq(M.providers.synthetic, OM.providers.synthetic) || M.providers.synthetic.models.length !== 3 || !eq(M.disabledProviders, OM.disabledProviders) || !eq(Object.keys(M).sort(), ["disabledProviders", "providers"])) throw new Error("case 7: synthetic or top level key changed " + JSON.stringify(M));
const p = M.providers.localstudio;
if (!p || !p.apiKey.startsWith("!") || !eq(p.models, [{ id: "stub-model" }, { id: "stub-long", contextWindow: 32768 }])) throw new Error("case 7: localstudio provider " + JSON.stringify(p));
if (!fs.readFileSync(s, "utf8").endsWith("}\n")) throw new Error("no trailing newline");
' "$scratch/settings.orig" "$scratch/models.orig" "case 1 contents"
for pair in "$settings:$scratch/settings.orig" "$models:$scratch/models.orig"; do
  f=${pair%%:*}
  [ "$(mode "$f")" = 640 ] || fail "case 1: $f mode $(mode "$f")"
  b=("$f".bak-localstudio-*)
  [ ${#b[@]} = 1 ] && cmp -s "${b[0]}" "${pair#*:}" || fail "case 1: one backup of $f equal to the original"
done
pass "case 1: localstudio instance removed, omp keeps its own args with a note, backups made, modes kept"
pass "case 7: synthetic (3 models) and disabledProviders equal, localstudio provider holds the stub's models"

before=$(sums "$t3"; sums "$ompd"); n=$(baks)
out=$(run_install "$p" --controller "$stub" --json) || fail "case 2 exited $?"
[ "$before" = "$(sums "$t3"; sums "$ompd")" ] && [ "$n" = "$(baks)" ] || fail "case 2: files changed on a second run"
check 'const r = JSON.parse(process.argv[4]); if (JSON.stringify(r.changed) !== "[]" || JSON.stringify(r.harnesses) !== JSON.stringify([{ name: "omp", instance: "omp" }]) || r.provider !== "localstudio" || r.warnings.length) throw new Error(process.argv[4]);' "$out" "case 2 json"
pass "case 2: second install changes nothing and makes no backup"

use c11
setjson '{"providerInstances":{"omp":{"driver":"acpRegistry","enabled":true,"config":{"commandPath":"/opt/omp","commandArgs":["--mode","acp"]}},'"$lsi"'}}'
out=$(run_install "$p" --controller "$stub" --json) || fail "case 11 exited $?"
check 'const r = JSON.parse(process.argv[4]); if (!r.ok || JSON.stringify(r.harnesses) !== JSON.stringify([{ name: "omp", instance: "omp" }]) || r.changed.length !== 2) throw new Error(process.argv[4]);' "$out" "case 11 json"
grep -q '"harnesses":\[{"name":"omp","instance":"omp"}\]' <<< "$out" || fail "case 11: $out"
pass "case 11: --json reports harnesses [{name omp, instance omp}]"

use c3
setjson '{"providerInstances":{"omp":{"driver":"acpRegistry","enabled":true,"config":{"commandPath":"/opt/omp","commandArgs":["--mode","acp","--approval-mode=write"]}},'"$lsi"'}}'
out=$(run_install "$p" --controller "$stub") || fail "case 3 exited $?"
check 'const S = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).providerInstances; if ("localstudio" in S || JSON.stringify(S.omp.config.commandArgs) !== JSON.stringify(["--mode", "acp", "--approval-mode=write"])) throw new Error(JSON.stringify(S));' "case 3 contents"
! grep -q "approval-mode=yolo" <<< "$out" || fail "case 3: yolo notice: $out"
pass "case 3: omp keeps --approval-mode=write"

use c3b
setjson '{"providerInstances":{"omp":{"driver":"acpRegistry","enabled":true,"config":{"commandPath":"/opt/omp","commandArgs":["--mode","acp"]}},'"${lsi/--approval-mode=yolo/--approval-mode=write}"'}}'
out=$(run_install "$p" --controller "$stub") || fail "case 3b exited $?"
check 'const S = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8")).providerInstances; if ("localstudio" in S || JSON.stringify(S.omp.config.commandArgs) !== JSON.stringify(["--mode", "acp", "--approval-mode=write"])) throw new Error(JSON.stringify(S));' "case 3b contents"
! grep -q "approval-mode=yolo" <<< "$out" || fail "case 3b: yolo line: $out"
pass "case 3b: an old entry with --approval-mode=write carries write onto an omp entry with none"

use c4
setjson '{"theme":"dark","providerInstances":{'"$lsi"'}}'
out=$(run_install "$p" --controller "$stub") || fail "case 4 exited $?"
check 'const fs = require("fs"), S = JSON.parse(fs.readFileSync(process.argv[1], "utf8")), omp = process.argv[3];
const want = { omp: { driver: "acpRegistry", displayName: "omp", enabled: true, config: { source: "local", commandPath: omp, commandArgs: ["--mode", "acp", "--approval-mode=yolo"], customModels: [] } } };
if (JSON.stringify(S.providerInstances) !== JSON.stringify(want) || S.theme !== "dark") throw new Error(JSON.stringify(S));' "case 4 contents"
grep -qx 'local models added to: omp (omp)' <<< "$out" || fail "case 4: $out"
pass "case 4: with no omp instance, install adds omp with --mode acp --approval-mode=yolo"

use c5
setjson '{"providerInstances":{"omp":{"driver":"codex","enabled":true,"config":{"binaryPath":"/opt/codex"}}}}'
cp "$t3/userdata/settings.json" "$scratch/c5.orig"
out=$(run_install "$p" --controller "$stub") || fail "case 5 exited $?"
check 'const fs = require("fs"), S = JSON.parse(fs.readFileSync(process.argv[1], "utf8")).providerInstances, O = JSON.parse(fs.readFileSync(process.argv[4], "utf8")).providerInstances;
if (JSON.stringify(S.omp) !== JSON.stringify(O.omp) || S["omp-local"]?.driver !== "acpRegistry" || S["omp-local"].config.commandPath !== process.argv[3] || JSON.stringify(Object.keys(S)) !== JSON.stringify(["omp", "omp-local"])) throw new Error(JSON.stringify(S));' "$scratch/c5.orig" "case 5 contents"
grep -qx 'local models added to: omp (omp-local)' <<< "$out" || fail "case 5: $out"
pass "case 5: key omp held by codex, so omp-local is added and codex is unchanged"

use c6
setjson '{"providerInstances":{"localstudio":{"driver":"claudeAgent","enabled":true,"config":{"homePath":"/x"}}}}'
cp "$t3/userdata/settings.json" "$scratch/c6.orig"
out=$(run_install "$p" --controller "$stub") || fail "case 6 exited $?"
grep -qx 'warning: left providerInstances.localstudio (claudeAgent) as it is' <<< "$out" || fail "case 6: no warning: $out"
! grep -q 'removed the separate' <<< "$out" || fail "case 6: removed line: $out"
check 'const fs = require("fs"), S = JSON.parse(fs.readFileSync(process.argv[1], "utf8")).providerInstances, O = JSON.parse(fs.readFileSync(process.argv[4], "utf8")).providerInstances;
if (JSON.stringify(S.localstudio) !== JSON.stringify(O.localstudio) || S.omp?.driver !== "acpRegistry" || S.omp.config.commandPath !== process.argv[3]) throw new Error(JSON.stringify(S));' "$scratch/c6.orig" "case 6 contents"
pass "case 6: a claudeAgent localstudio instance stays, with a warning, and omp is added"

use c8
setjson '{"providerInstances":{'"$lsi"'}}'
before=$(sums "$scratch/c8")
for h in claude omp,claude; do
  set +e; err=$(run_install "$p" --controller "$stub" --harness "$h" 2>&1 >/dev/null); code=$?; set -e
  [ "$code" = 2 ] && [ "$err" = "localstudio: Claude Code is not supported yet (Students-of-Local/localstudio-plugin#337)" ] || fail "case 8 ($h): exit $code: $err"
done
[ "$before" = "$(sums "$scratch/c8")" ] && [ ! -e "$ompd/models.yml" ] || fail "case 8: files written"
pass "case 8: --harness claude exits 2 with the #337 line and writes nothing"
set +e; err=$(run_install "$p" --controller "$stub" --harness bogus 2>&1 >/dev/null); code=$?; set -e
[ "$code" = 2 ] && [ "$err" = "localstudio: unknown harness bogus; known: omp" ] || fail "case 9: exit $code: $err"
[ "$before" = "$(sums "$scratch/c8")" ] && [ ! -e "$ompd/models.yml" ] || fail "case 9: files written"
out=$(run_install "$p" --controller "$stub" --harness omp --dry-run) || fail "--harness omp exited $?"
grep -qx 'local models added to: omp (omp)' <<< "$out" || fail "--harness omp: $out"
pass "case 9: --harness bogus exits 2 and writes nothing; --harness omp is accepted"

t3=$scratch/t3; ompd=$scratch/omp
mkdir -p "$scratch/extbin"
printf '#!/bin/sh\nexit 0\n' > "$scratch/extbin/local-studio-browser"; chmod +x "$scratch/extbin/local-studio-browser"
out=$(PATH="$scratch/extbin:$p" "$bun" "$root/plugin/src/cli.ts" extensions add browser --t3-home "$t3" --omp-dir "$ompd" --json) || fail "case 12 exited $?"
out2=$(PATH="$scratch/extbin:$p" "$bun" "$root/plugin/src/cli.ts" extensions add browser --t3-home "$t3" --omp-dir "$ompd" --json) || fail "case 12 second add exited $?"
check 'const fs = require("fs"), r = JSON.parse(process.argv[4]), r2 = JSON.parse(process.argv[5]), M = JSON.parse(fs.readFileSync(process.argv[6], "utf8"));
if (JSON.stringify(r.targets) !== JSON.stringify(["omp"]) || r.targets.includes("localstudio") || JSON.stringify(r2.changed) !== "[]") throw new Error(process.argv[4] + process.argv[5]);
if (JSON.stringify(Object.keys(M.mcpServers)) !== JSON.stringify(["local-studio"]) || M.mcpServers["local-studio"].command !== process.argv[7]) throw new Error(JSON.stringify(M));' "$out" "$out2" "$ompd/mcp.json" "$scratch/extbin/local-studio-browser" "case 12 contents"
pass "case 12: extensions add browser after case 1 adds the server to omp mcp.json once, no localstudio target"
before=$(sums "$scratch/t3"; sums "$scratch/omp")

set +e; run_install "$scratch/bunonly" --controller http://127.0.0.1:9 >/dev/null 2>&1; code=$?; set -e
[ "$code" = 3 ] || fail "omp missing: exit $code"
[ "$before" = "$(sums "$scratch/t3"; sums "$scratch/omp")" ] || fail "omp missing: files changed"
pass "omp missing: exit 3, files untouched"

PATH="$scratch/bin:/usr/bin:/bin" "$bun" "$root/plugin/src/cli.ts" install --t3-home "$scratch/fresh-t3" --omp-dir "$scratch/fresh-omp" --dry-run >/dev/null || fail "fresh dry run"
[ ! -e "$scratch/fresh-t3" ] && [ ! -e "$scratch/fresh-omp" ] || fail "dry run created files"
pass "--dry-run on a fresh home: nothing written"

if (exec 3<>/dev/tcp/127.0.0.1/18091) 2>/dev/null; then
  echo "note: port 18091 is in use, so the controller part is skipped"
else
  (cd "$root/controller" && "$bun" install --frozen-lockfile >/dev/null)
  busy=$( (ss -ltnH 2>/dev/null || true) | awk '{n=split($4,a,":"); print a[n]}' | grep -vx "$stub_port" | sort -un | paste -sd, -)
  mkdir -p "$LOCAL_STUDIO_CONTROLLER_HOME"
  printf '{"name":"plugin-test","fleetKey":"plugin-test-%s","excludePorts":[%s],"enginePorts":[%s],"registry":{"url":"","dir":"%s","ref":"none"},"modelsDir":"%s"}\n' "$$" "$busy" "$stub_port" "$scratch/registry" "$scratch/models" > "$LOCAL_STUDIO_CONTROLLER_HOME/config.json"
  chmod 600 "$LOCAL_STUDIO_CONTROLLER_HOME/config.json"
  (cd "$root/controller" && exec "$bun" src/main.ts) > "$scratch/controller.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 60); do
    curl -sf -H "authorization: Bearer plugin-test-$$" http://127.0.0.1:18091/v1/models 2>/dev/null | grep -q stub-model && break
    sleep 0.5
  done
  out=$(run_install "$scratch/bin:/usr/bin:/bin" --json) || fail "controller install exited $?"
  check '
const fs = require("fs"), M = Bun.YAML.parse(fs.readFileSync(process.argv[2], "utf8")), r = JSON.parse(process.argv[4]);
const ids = M.providers.localstudio.models.map((x) => x.id);
if (!ids.includes("stub-model") || M.providers.localstudio.baseUrl !== "http://127.0.0.1:18091/v1" || !r.models.includes("stub-model") || r.warnings.length) throw new Error(JSON.stringify({ ids, r }));
' "$out" "controller models"
  key_cmd=$("$bun" -e 'console.log(Bun.YAML.parse(require("fs").readFileSync(process.argv[1],"utf8")).providers.localstudio.apiKey.slice(1))' "$models")
  [ "$(sh -c "$key_cmd")" = "plugin-test-$$" ] || fail "the apiKey command does not give the controller's fleet key"
  PI_CODING_AGENT_DIR=$scratch/omp "$omp_bin" models localstudio 2>&1 | grep -q stub-model || fail "omp does not list localstudio/stub-model"
  pass "controller: its models land in omp's models.yml and omp lists them"
fi
echo "plugin-install: all cases passed"
