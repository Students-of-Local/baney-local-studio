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
run_install() { PATH="$1" "$bun" "$root/plugin/src/cli.ts" install --t3-home "$scratch/t3" --omp-dir "$scratch/omp" "${@:2}"; }
check() { "$bun" -e "$1" "$settings" "$models" "$scratch/bin/omp" "${@:2}" || fail "${*:2}"; }

cat > "$settings" <<'EOF'
{"linkedEnvironmentsEnabled":true,"providerInstances":{"omp":{"driver":"acpRegistry","displayName":"My omp","enabled":true,"config":{"source":"local","commandPath":"/opt/omp","commandArgs":["acp"],"customModels":["x"]}}}}
EOF
printf 'providers:\n  other:\n    baseUrl: http://127.0.0.1:9/v1\n    api: openai-completions\n    apiKey: none\n    models:\n      - id: other-model\n        contextWindow: 4096\n' > "$models"
chmod 640 "$settings" "$models"
cp "$settings" "$scratch/settings.orig"; cp "$models" "$scratch/models.orig"

out=$(run_install "$scratch/bin:/usr/bin:/bin" --controller http://127.0.0.1:9 --json) || fail "case 1 install exited $?"
check '
const fs = require("fs"), [s, m, omp, origS, origM, out] = process.argv.slice(1);
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const S = JSON.parse(fs.readFileSync(s, "utf8")), O = JSON.parse(fs.readFileSync(origS, "utf8"));
const M = Bun.YAML.parse(fs.readFileSync(m, "utf8")), OM = Bun.YAML.parse(fs.readFileSync(origM, "utf8"));
const r = JSON.parse(out);
const want = { driver: "acpRegistry", displayName: "Local Studio", enabled: true, config: { source: "local", commandPath: omp, commandArgs: ["acp"], customModels: [] } };
if (!eq(S.providerInstances.localstudio, want)) throw new Error("localstudio instance " + JSON.stringify(S.providerInstances.localstudio));
if (!eq(S.providerInstances.omp, O.providerInstances.omp) || S.linkedEnvironmentsEnabled !== true || Object.keys(S).length !== 2) throw new Error("other settings changed");
if (!eq(M.providers.other, OM.providers.other)) throw new Error("other omp provider changed");
const p = M.providers.localstudio;
if (p.baseUrl !== "http://127.0.0.1:9/v1" || p.api !== "openai-completions" || !p.apiKey.startsWith("!") || !eq(p.models, [])) throw new Error("localstudio provider " + JSON.stringify(p));
if (!r.ok || r.provider !== "localstudio" || r.t3Settings !== s || r.ompModels !== m || !eq(r.models, []) || r.warnings.length !== 1 || r.changed.length !== 2) throw new Error("json " + out);
if (!fs.readFileSync(s, "utf8").endsWith("}\n")) throw new Error("no trailing newline");
' "$scratch/settings.orig" "$scratch/models.orig" "$out" "case 1 contents"
for pair in "$settings:$scratch/settings.orig" "$models:$scratch/models.orig"; do
  f=${pair%%:*}
  [ "$(mode "$f")" = 640 ] || fail "case 1: $f mode $(mode "$f")"
  b=("$f".bak-localstudio-*)
  [ ${#b[@]} = 1 ] && cmp -s "${b[0]}" "${pair#*:}" || fail "case 1: one backup of $f equal to the original"
done
pass "case 1: localstudio instance and provider written, other entries kept, backups made, modes kept"

before=$(sums "$scratch/t3"; sums "$scratch/omp")
out=$(run_install "$scratch/bin:/usr/bin:/bin" --controller http://127.0.0.1:9 --json) || fail "case 2 install exited $?"
[ "$before" = "$(sums "$scratch/t3"; sums "$scratch/omp")" ] || fail "case 2: files changed on a second run"
grep -q '"changed":\[\]' <<< "$out" || fail "case 2: $out"
pass "case 2: second install changes nothing and makes no backup"

set +e; run_install "$scratch/bunonly" --controller http://127.0.0.1:9 >/dev/null 2>&1; code=$?; set -e
[ "$code" = 3 ] || fail "omp missing: exit $code"
[ "$before" = "$(sums "$scratch/t3"; sums "$scratch/omp")" ] || fail "omp missing: files changed"
pass "omp missing: exit 3, files untouched"

run_install "$scratch/bin:/usr/bin:/bin" --controller http://127.0.0.1:8 --dry-run >/dev/null || fail "dry run exited $?"
[ "$before" = "$(sums "$scratch/t3"; sums "$scratch/omp")" ] || fail "dry run changed files"
PATH="$scratch/bin:/usr/bin:/bin" "$bun" "$root/plugin/src/cli.ts" install --t3-home "$scratch/fresh-t3" --omp-dir "$scratch/fresh-omp" --dry-run >/dev/null || fail "fresh dry run"
[ ! -e "$scratch/fresh-t3" ] && [ ! -e "$scratch/fresh-omp" ] || fail "dry run created files"
pass "--dry-run: nothing written"

if (exec 3<>/dev/tcp/127.0.0.1/18091) 2>/dev/null; then
  echo "note: port 18091 is in use, so the controller part is skipped"
else
  (cd "$root/controller" && "$bun" install --frozen-lockfile >/dev/null)
  stub_port=$("$bun" -e 'const s=Bun.serve({port:0,fetch:()=>new Response("")});console.log(s.port);s.stop()')
  STUB_PORT=$stub_port "$bun" -e 'Bun.serve({hostname:"127.0.0.1",port:Number(process.env.STUB_PORT),fetch:(r)=>new URL(r.url).pathname.endsWith("/models")?Response.json({object:"list",data:[{id:"stub-model",object:"model"}]}):new Response("{}",{status:404})})' &
  pids+=($!)
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
