#!/usr/bin/env bash
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
bun=$(command -v bun)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/ls-find-test.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
export HOME=$scratch/home HF_HOME=$scratch/hf
unset HF_TOKEN HF_ENDPOINT LOCALSTUDIO_PROBE_JSON
mkdir -p "$HOME"
failed=0
result() { if [ "$1" = 0 ]; then echo "pass: $2"; else echo "FAIL: $2"; failed=1; fi; }
closed_port() { "$bun" -e 'const s=Bun.listen({hostname:"127.0.0.1",port:0,socket:{data(){}}});console.log(s.port);s.stop(true)'; }
device() {
  printf '{"os":"linux","arch":"x64","ramTotalGb":%s,"ramFreeGb":%s,"disk":{"path":"%s","freeGb":500},"gpus":[' "$2" "$2" "$scratch/hf" > "$scratch/$1.json"
  local sep="" i=0
  for _ in $(seq "$5"); do
    printf '%s{"id":"nvidia:%s","name":"%s","vendor":"nvidia","backend":"nvidia","vramGb":%s,"vramFreeGb":%s,"pci":null,"removable":false}' "$sep" "$i" "$3" "$4" "$4" >> "$scratch/$1.json"
    sep=, i=$((i + 1))
  done
  echo ']}' >> "$scratch/$1.json"
}
find_json() { local probe=$1; shift; LOCALSTUDIO_PROBE_JSON=${probe:+$scratch/$probe.json} "$bun" "$root/plugin/src/cli.ts" find "$@" --json; }
check() { "$bun" -e "
const fs = require('fs'), r = JSON.parse(fs.readFileSync(process.argv[1], 'utf8'));
const cat = fs.existsSync(process.argv[2]) ? JSON.parse(fs.readFileSync(process.argv[2], 'utf8')) : null;
const eq = (a, b) => JSON.stringify(a) === JSON.stringify(b);
const must = (ok, what) => { if (!ok) { console.error('  ' + what); process.exit(1); } };
const label = (s, gpu, ram, gguf) => s * 1.1 + 1 <= 0.9 * gpu ? 'Fits' : gguf && s * 1.1 + 1 <= 0.9 * gpu + 0.8 * ram ? 'Offload' : \"Won't fit\";
$1" "$scratch/out.json" "$HOME/.cache/localstudio/catalog.json"; }

find_json "" > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit ' + $code);
const gpus = r.device.gpus, cards = gpus.filter((g) => g.card);
must(r.ok && eq(r.cards, [...new Set(cards.map((g) => g.card))]), 'cards ' + JSON.stringify(r.cards));
if (!cards.length) must(r.picks.length === 0 && r.warnings.length === 1 && (gpus.length ? gpus.every((g) => r.warnings[0].includes(g.name)) : r.warnings[0].includes('no GPU')), JSON.stringify(r.warnings));"
result $? "this machine's own probe: picks and warnings follow its matched cards (exit 0)"

device laptop 32 "NVIDIA GeForce RTX 4080 Laptop GPU" 12 1
find_json laptop > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit');
must(eq(r.picks, []) && eq(r.cards, []) && r.warnings.length === 1 && r.warnings[0].includes('RTX 4080 Laptop GPU'), JSON.stringify(r.warnings));"
result $? "RTX 4080 Laptop 12 GB, no card: no picks, one warning naming the GPU, exit 0"

for setup in "rtx4090:NVIDIA GeForce RTX 4090:24:1:rtx-4090-24gb" "rtx3090x2:NVIDIA GeForce RTX 3090:24:2:rtx-3090-24gb"; do
  IFS=: read -r name gpu vram n card <<< "$setup"
  device "$name" 64 "$gpu" "$vram" "$n"
  find_json "$name" > "$scratch/out.json"; code=$?
  check "must($code === 0, 'exit');
const s = cat.cards['$card'].setups.find((x) => x.cards === $n && x.machines === 1);
must(eq(r.cards, ['$card']), 'cards ' + JSON.stringify(r.cards));
must(s.picks.length > 0 && eq(r.picks.map((p) => p.recipe), s.picks), 'order ' + JSON.stringify(r.picks.map((p) => p.recipe)));
for (const p of r.picks) {
  const rec = cat.recipes[p.recipe], proof = rec.proof.find((x) => !x.failed);
  must(p.source === 'registry' && p.label === 'Proven' && p.tps === proof.tps && p.prefillTps === (proof.prefill ?? null) && p.proofDate === proof.at && p.model === rec.model && p.engine === rec.engine, JSON.stringify(p));
  must(eq(Object.keys(p), ['source', 'label', 'model', 'quant', 'engine', 'tps', 'prefillTps', 'proofDate', 'recipe']), 'keys');
}"
  result $? "$n x $gpu: the $card setup's picks in catalog order, Proven, with the proof's tps"
done

find_json rtx4090 qwen3 > "$scratch/out.json"; code=$?
LOCALSTUDIO_PROBE_JSON=$scratch/rtx4090.json "$bun" "$root/plugin/src/cli.ts" find qwen3 > "$scratch/out.txt" 2>/dev/null
check "must($code === 0, 'exit');
must(r.picks.length > 0 && r.picks.every((p) => p.model.toLowerCase().includes('qwen3')), 'picks ' + JSON.stringify(r.picks.map((p) => p.model)));
must(r.results.length > 0 && r.results.some((x) => x.sizeGb <= 18 && x.label === 'Fits'), 'no Fits at or under 18 GB');
for (const x of r.results) must(x.source === 'hf' && x.tps === null && x.label === label(x.sizeGb, 24, 64, x.format === 'gguf'), JSON.stringify(x));
const lines = fs.readFileSync(process.argv[1].replace(/json\$/, 'txt'), 'utf8').trim().split('\n');
must(lines.length === r.picks.length + r.results.length, 'one line per entry');
must(lines.slice(0, r.picks.length).every((l) => l.startsWith('Proven\t')) && lines.slice(r.picks.length).every((l) => l.endsWith('est. pending')), 'picks first in text');"
result $? "find qwen3 on a 24 GB card: matching picks first, then HF results, a quant under 19 GB Fits"

repo=bartowski/Llama-3.3-70B-Instruct-GGUF
for ram in 64 8; do
  device "ram$ram" "$ram" "NVIDIA GeForce RTX 4080" 16 1
  find_json "ram$ram" "$repo" > "$scratch/out.json"; code=$?
  check "must($code === 0, 'exit');
const q4 = r.results.find((x) => x.repo === '$repo' && x.quant === 'Q4_K_M');
must(q4 && q4.format === 'gguf', 'no Q4_K_M set');
const want = $ram === 64 ? 'Offload' : \"Won't fit\";
must(q4.label === want, q4.label + ': ' + q4.why);
if (want !== 'Offload') must(q4.why.includes((Math.round((q4.sizeGb * 1.1 + 1 - 14.4 - 6.4) * 10) / 10) + ' GB more'), 'gap not in why: ' + q4.why);"
  result $? "70B Q4_K_M GGUF on a 16 GB card with $ram GB RAM free: $([ "$ram" = 64 ] && echo Offload || echo "Won't fit, gap named")"
done

curl -sf "https://huggingface.co/api/models/$repo?blobs=true" > "$scratch/repo.json"
check "must($code === 0, 'exit');
const parts = JSON.parse(fs.readFileSync('$scratch/repo.json', 'utf8')).siblings.filter((f) => /Q5_K_M-0000\d-of-00002\.gguf\$/.test(f.rfilename));
const sets = r.results.filter((x) => x.repo === '$repo' && x.quant === 'Q5_K_M');
must(parts.length === 2 && sets.length === 1, parts.length + ' parts, ' + sets.length + ' sets');
must(sets[0].sizeGb === Math.round(parts.reduce((a, f) => a + f.size, 0) / 2 ** 30 * 10) / 10, 'size ' + sets[0].sizeGb);"
result $? "split GGUF Q5_K_M (2 parts): one set, sizeGb the sum of the parts"

gated=meta-llama/Llama-3.1-8B-Instruct
find_json rtx4090 "$gated" > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit');
const g = r.results.filter((x) => x.repo === '$gated');
must(g.length > 0 && g.every((x) => x.gated === true && x.licenseUrl === 'https://huggingface.co/$gated'), JSON.stringify(g));
must(!fs.existsSync('$HF_HOME') && !fs.existsSync('$HOME/.cache/huggingface'), 'something was written under the Hugging Face cache');"
result $? "gated $gated: gated true, licenseUrl set, nothing under the HF cache"

port=$(closed_port)
HF_ENDPOINT=http://127.0.0.1:$port find_json rtx4090 qwen3 > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit');
must(eq(r.results, []) && r.picks.length > 0 && r.warnings.some((w) => w.includes('Hugging Face') && w.includes('127.0.0.1:$port')), JSON.stringify(r.warnings));"
result $? "HF_ENDPOINT on a closed port: exit 0, picks kept, no results, an HF warning"

"$bun" -e 'const t = Date.now() / 1000 - 172800; require("fs").utimesSync(process.argv[1], t, t)' "$HOME/.cache/localstudio/catalog.json"
HTTPS_PROXY=http://127.0.0.1:$port find_json rtx4090 qwen3 > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit');
must(r.picks.length > 0 && eq(r.results, []) && r.warnings.length === 2 && r.warnings.some((w) => w.includes('cached copy')), JSON.stringify(r.warnings));"
result $? "offline with a stale cached catalog: picks from the cache, no results, two warnings"

rm -rf "$HOME/.cache"
HTTPS_PROXY=http://127.0.0.1:$port find_json rtx4090 qwen3 > "$scratch/out.json"; code=$?
check "must($code === 0, 'exit');
must(eq(r.picks, []) && eq(r.results, []) && r.warnings.length === 2, JSON.stringify(r.warnings));"
result $? "offline with no catalog: exit 0, empty lists, two warnings"

[ "$failed" = 0 ] && echo "plugin-find: all cases passed" || { echo "plugin-find: some cases failed"; exit 1; }
