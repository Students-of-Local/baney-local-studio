import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { connect, createServer as createTcp } from "node:net";
import { networkInterfaces, tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";

const root = mkdtempSync(join(tmpdir(), "ls-cluster-"));
const key = randomBytes(24).toString("hex");
const children = [];
const servers = [];
const strays = [];
const auth = { authorization: `Bearer ${key}`, "content-type": "application/json" };
const shared = `cluster-shared-${randomBytes(3).toString("hex")}`;
const lanIp =
  process.env.E2E_REMOTE_IP ||
  Object.values(networkInterfaces())
    .flat()
    .find((a) => a && a.family === "IPv4" && !a.internal)?.address;
const step = (s) => console.log(`== ${s}`);

const listen = async (server, host = "127.0.0.1") => {
  servers.push(server);
  server.listen(0, host);
  await once(server, "listening");
  return server.address().port;
};
const freePort = async () => {
  const s = createTcp();
  s.listen(0, "127.0.0.1");
  await once(s, "listening");
  const p = s.address().port;
  await new Promise((r) => s.close(r));
  return p;
};
const waitFor = async (fn, label, ms = 30_000) => {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      const v = await fn();
      if (v) return v;
    } catch (error) {
      last = error;
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`Timed out: ${label}`, { cause: last });
};
const call = (port, path, body, headers = auth, ms = 15_000) =>
  fetch(`http://127.0.0.1:${port}${path}`, {
    headers,
    ...(body === undefined ? {} : { method: "POST", body: JSON.stringify(body) }),
    signal: AbortSignal.timeout(ms),
  });
const snap = async (port) => (await call(port, "/api/snapshot")).json();
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};
const write = (path, text, mode = 0o644) => {
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, text, { mode });
};

try {
  assert(lanIp, "no non-loopback IPv4 address; set E2E_REMOTE_IP");
  const bin = join(root, "bin");
  const stubs = join(root, "stubs");
  mkdirSync(stubs, { recursive: true });
  write(
    join(bin, "nvidia-smi"),
    [
      "#!/bin/sh",
      'case "$*" in',
      '  *-pl*) echo "$*" >> "$FAKE_GPU_DIR/pl.log"; echo "Power limit set to $4 W."; exit 0 ;;',
      "esac",
      'case "$1" in',
      '  --query-gpu=uuid,index,power.limit,*) cat "$FAKE_GPU_DIR/limits.csv" 2>/dev/null ;;',
      '  --query-gpu=uuid,power.draw) if [ "$3" = "-lms" ]; then while :; do cat "$FAKE_GPU_DIR/draw.csv" 2>/dev/null; sleep 0.2; done; else cat "$FAKE_GPU_DIR/draw.csv"; fi ;;',
      '  --query-gpu=*) if [ -f "$FAKE_GPU_DIR/lostonly" ]; then echo "Unable to determine the device handle for GPU0000:2C:00.0: Unknown Error"; exit 15; fi; if [ -s "$FAKE_GPU_DIR/gpus.csv" ]; then cat "$FAKE_GPU_DIR/gpus.csv"; else echo "No devices were found"; exit 6; fi; if [ -f "$FAKE_GPU_DIR/lost" ]; then echo "Unable to determine the device handle for GPU0000:2C:00.0: Unknown Error"; exit 15; fi ;;',
      '  --query-compute-apps=*) cat "$FAKE_GPU_DIR/apps.csv" 2>/dev/null ;;',
      "esac",
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(bin, "sudo"),
    [
      "#!/bin/sh",
      'echo "$*" >> "$FAKE_GPU_DIR/sudo.log"',
      '[ -f "$FAKE_GPU_DIR/sudo-allow" ] || { echo "sudo: a password is required" >&2; exit 1; }',
      '[ "$1" = "-n" ] && shift',
      '[ "$1" = "-l" ] && exit 0',
      'exec "$@"',
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(bin, "tailscale"),
    [
      "#!/bin/sh",
      'case "$1" in',
      `  status) echo '{"Peer":{"k":{"HostName":"scratch","OS":"linux","Online":true,"TailscaleIPs":["127.0.0.1","fd7a::1"]}}}' ;;`,
      "  ip) echo 127.0.0.1 ;;",
      "esac",
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(bin, "boltctl"),
    [
      "#!/bin/sh",
      'tb="$LOCAL_STUDIO_SYSFS/bus/thunderbolt/devices"',
      'case "$1" in',
      "  list)",
      '    for d in "$tb"/0-1 "$tb"/0-3; do',
      '      [ -f "$d/unique_id" ] || continue',
      '      s=connected; [ "$(cat "$d/authorized")" = 1 ] && s=authorized',
      `      printf ' * %s %s\\n   |- type:          peripheral\\n   |- name:          %s\\n   |- vendor:        %s\\n   |- uuid:          %s\\n   |- generation:    Thunderbolt 3\\n   |- status:        %s\\n   |  |- domain:     domain0\\n   |  \`- authflags:  none\\n   \`- stored:        no\\n\\n' "$(cat "$d/vendor_name")" "$(cat "$d/device_name")" "$(cat "$d/device_name")" "$(cat "$d/vendor_name")" "$(cat "$d/unique_id")" "$s"`,
      "    done ;;",
      "  enroll)",
      '    for d in "$tb"/0-1 "$tb"/0-3; do',
      '      [ "$(cat "$d/unique_id")" = "$4" ] || continue',
      '      if [ -f "$d/polkit" ]; then echo "boltctl: error: GDBus.Error:org.freedesktop.DBus.Error.AccessDenied: Not authorized (polkit)" >&2; exit 1; fi',
      '      echo 1 > "$d/authorized"; echo "authorized $4"; exit 0',
      "    done; echo 'device not found' >&2; exit 1 ;;",
      "esac",
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(bin, "docker"),
    [
      "#!/bin/sh",
      'echo "$*" >> "$FAKE_GPU_DIR/docker.log"',
      'last=""; for a in "$@"; do last=$a; done',
      'case "$1 $2" in',
      '  "image inspect") exit 0 ;;',
      '  "ps -q") exit 0 ;;',
      "esac",
      'case "$1" in',
      "  run) echo 0123456789abcdef; exit 0 ;;",
      '  inspect) case "$3" in *Labels*) echo "${last#local-studio-}" ;; *) echo "true 0" ;; esac; exit 0 ;;',
      "  rm|logs|stop) exit 0 ;;",
      "esac",
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(stubs, "rpc-server"),
    [
      "#!/usr/bin/env node",
      'const net = require("node:net");',
      'const fs = require("node:fs");',
      "const a = process.argv.slice(2);",
      'const host = a[a.indexOf("-H") + 1];',
      'const port = Number(a[a.indexOf("-p") + 1]);',
      "fs.writeFileSync(`${process.env.STUB_DIR}/rpc-${process.pid}.json`, JSON.stringify({ pid: process.pid, argv: a, cuda: process.env.CUDA_VISIBLE_DEVICES ?? null, owner: process.env.FAKE_GPU_DIR }));",
      'net.createServer((s) => { s.write("rpc-stub\\n"); s.on("data", (d) => s.write(d)); s.on("error", () => {}); }).listen(port, host);',
      'process.on("SIGTERM", () => { fs.writeFileSync(`${process.env.STUB_DIR}/term-${process.pid}`, ""); process.exit(0); });',
      "",
    ].join("\n"),
    0o755,
  );
  write(
    join(stubs, "llama-server"),
    [
      "#!/usr/bin/env node",
      'const net = require("node:net");',
      'const http = require("node:http");',
      'const fs = require("node:fs");',
      "const a = process.argv.slice(2);",
      "const arg = (f) => a[a.indexOf(f) + 1];",
      'fs.writeFileSync(`${process.env.STUB_DIR}/llama-${process.pid}.json`, JSON.stringify({ pid: process.pid, argv: a }));',
      'const eps = arg("--rpc").split(",");',
      "const conns = [];",
      "let pending = eps.length;",
      "for (const ep of eps) {",
      '  const i = ep.lastIndexOf(":");',
      "  const s = net.connect(Number(ep.slice(i + 1)), ep.slice(0, i));",
      "  conns.push(s);",
      '  s.once("data", (d) => { if (!String(d).startsWith("rpc-stub")) process.exit(3); if (--pending === 0) serve(); });',
      '  s.on("error", () => process.exit(4));',
      '  s.on("close", () => process.exit(5));',
      "}",
      "const serve = () => http.createServer((req, res) => {",
      '  res.setHeader("content-type", "application/json");',
      '  if (req.url === "/v1/models") return res.end(JSON.stringify({ data: [{ id: arg("--alias") }] }));',
      '  let b = ""; req.on("data", (c) => (b += c)); req.on("end", () => res.end(JSON.stringify({ model: arg("--alias"), pooled: true, rpc: eps })));',
      '}).listen(Number(arg("--port")), arg("--host"));',
      'process.on("SIGTERM", () => process.exit(0));',
      "",
    ].join("\n"),
    0o755,
  );

  const engineState = new Map();
  const engine = async (name) => {
    engineState.set(name, { running: 0, mode: "ok", hits: 0 });
    const born = Date.now();
    return listen(
      createServer(async (req, res) => {
        const st = engineState.get(name);
        if (req.url === "/v1/models") {
          res.setHeader("content-type", "application/json");
          return res.end(JSON.stringify({ data: [{ id: shared }] }));
        }
        if (req.url === "/metrics") {
          res.setHeader("content-type", "text/plain; version=0.0.4");
          return res.end(
            [
              `vllm:generation_tokens_total{model_name="${shared}"} ${Math.floor((Date.now() - born) * 0.1)}`,
              `vllm:num_requests_running{model_name="${shared}"} ${st.running}`,
              `vllm:num_requests_waiting{model_name="${shared}"} 0`,
              `vllm:kv_cache_usage_perc{model_name="${shared}"} ${st.running / 10}`,
              "",
            ].join("\n"),
          );
        }
        let raw = "";
        for await (const chunk of req) raw += chunk;
        if (st.mode === "fail") {
          res.writeHead(503, { "content-type": "application/json" });
          return res.end('{"error":{"type":"overloaded"}}');
        }
        st.hits++;
        if (st.mode === "hang") return;
        if (st.mode === "reset") return req.socket.destroy();
        const body = JSON.parse(raw);
        if (body.stream) {
          res.writeHead(200, { "content-type": "text/event-stream" });
          return res.end(`data: {"engine":"${name}"}\n\ndata: [DONE]\n\n`);
        }
        res.setHeader("content-type", "application/json");
        res.end(JSON.stringify({ engine: name }));
      }),
    );
  };
  const e1 = await engine("e1");
  const e2 = await engine("e2");

  const names = ["a", "b", "c", "d"];
  const ports = {};
  for (const n of names) ports[n] = await freePort();
  const rpcPort = { b: await freePort(), c: await freePort() };
  const poolRange = Array.from({ length: 200 }, (_, i) => 18100 + i);
  const commit = (() => {
    const reg = join(root, "registry");
    write(
      join(reg, "dist/catalog.json"),
      JSON.stringify({
        cards: { rtx3090: { match: { backend: "nvidia", name: "NVIDIA GeForce RTX 3090", names: ["rtx3090"], vramGb: 24 } } },
        recipes: {
          "rtx3090/tiny": {
            model: "tiny-egpu-model",
            card: "rtx3090",
            engine: "vllm",
            weights: "baked-into-image",
            launch: { kind: "container", image: `example/engine@sha256:${"a".repeat(64)}`, port: 8000, ctx: 8192, cards: 1 },
            proof: [{ on: "rtx3090", tps: 40 }],
          },
        },
      }),
    );
    const git = (...args) => execFileSync("git", args, { cwd: reg, stdio: "pipe", timeout: 10_000 });
    git("init", "-q");
    git("add", "-f", "dist/catalog.json");
    git("-c", "user.name=Cluster Smoke", "-c", "user.email=smoke@localhost", "commit", "-qm", "fixture");
    return git("rev-parse", "HEAD").toString().trim();
  })();

  const dirs = {};
  for (const n of names) {
    const d = {
      home: join(root, `${n}-home`),
      data: join(root, `${n}-data`),
      sys: join(root, `${n}-sys`),
      gpu: join(root, `${n}-gpu`),
      models: join(root, `${n}-models`),
    };
    for (const p of Object.values(d)) mkdirSync(p, { recursive: true });
    dirs[n] = d;
    const ps = join(d.sys, "class/power_supply");
    write(join(ps, "AC/type"), "Mains\n");
    write(join(ps, "AC/online"), "1\n");
    write(join(ps, "BAT0/type"), "Battery\n");
    write(join(d.gpu, "apps.csv"), "");
    write(join(d.gpu, "gpus.csv"), "");
    write(join(d.gpu, "present"), "0\n");
  }
  const gpuRow = (i, uuid, name, used, total, util, bus, gen, width) =>
    `${i}, ${uuid}, ${name}, ${used}, ${total}, 50.0, 200.0, 40, ${util}, ${bus}, ${gen}, ${gen}, ${width}, ${width}\n`;
  const internalA = gpuRow(0, "GPU-a-0", "NVIDIA RTX A2000", 300, 6138, 0, "00000000:01:00.0", 4, 16);
  const egpuA = gpuRow(1, "GPU-a-egpu", "NVIDIA GeForce RTX 3090", 400, 24576, 0, "00000000:2C:00.0", 3, 4);
  write(join(dirs.a.gpu, "gpus.csv"), internalA);
  write(join(dirs.b.gpu, "gpus.csv"), gpuRow(0, "GPU-b-0", "NVIDIA GeForce RTX 4080", 500, 16376, 0, "00000000:01:00.0", 4, 16));
  write(join(dirs.c.gpu, "gpus.csv"), gpuRow(0, "GPU-c-0", "NVIDIA GeForce RTX 4070", 500, 12282, 0, "00000000:01:00.0", 4, 16));
  write(join(dirs.a.gpu, "limits.csv"), "GPU-a-0, 0, 140.00, 50.00, 200.00\n");
  write(join(dirs.a.gpu, "sudo-allow"), "");
  write(join(dirs.a.data, "power-restore.json"), JSON.stringify({ "GPU-a-0": { index: 0, originalW: 150 } }), 0o600);
  // A controller killed mid duty cycle leaves its lent process stopped. The record names it by command, so a
  // reused pid running something else (the decoy) is left alone.
  const leftover = spawn("sleep", ["120"], { stdio: "ignore" });
  const decoy = spawn("sleep", ["120"], { stdio: "ignore" });
  strays.push(leftover, decoy);
  process.kill(leftover.pid, "SIGSTOP");
  process.kill(decoy.pid, "SIGSTOP");
  write(join(dirs.a.data, "power-paused.json"), JSON.stringify({ pids: [{ pid: leftover.pid, comm: "sleep" }, { pid: decoy.pid, comm: "rpc-server" }], containers: [] }), 0o600);
  write(join(dirs.b.gpu, "limits.csv"), "GPU-b-0, 0, 200.00, 50.00, 320.00\n");
  write(join(dirs.b.gpu, "draw.csv"), "GPU-b-0, 30.00\n");
  const sysA = dirs.a.sys;
  const usb4 = "devices/pci0000:00/0000:00:1a.0/0000:07:00.0";
  mkdirSync(join(sysA, "bus/pci/devices"), { recursive: true });
  mkdirSync(join(sysA, "bus/thunderbolt/devices"), { recursive: true });
  for (const [id, parent] of [["0000:01:00.0", "devices/pci0000:00/0000:00:01.0"], ["0000:2c:00.0", `${usb4}/0000:08:01.0`], ["0000:09:00.0", `${usb4}/0000:08:00.0`]]) {
    const dev = join(sysA, parent, id);
    mkdirSync(dev, { recursive: true });
    write(join(dev, "removable"), "unknown\n");
    symlinkSync(dev, join(sysA, "bus/pci/devices", id));
  }
  mkdirSync(join(sysA, usb4, "0000:08:00.0/0000:09:00.0/domain0"), { recursive: true });
  symlinkSync(join(sysA, usb4, "0000:08:00.0/0000:09:00.0/domain0"), join(sysA, "bus/thunderbolt/devices/domain0"));
  const tbUuid = "00000000-1111-2222-3333-444444444444";
  const lockedUuid = "00000000-1111-2222-3333-555555555555";
  for (const [dev, uuid, name, polkit] of [["0-1", tbUuid, "Core X", false], ["0-3", lockedUuid, "Dock", true]]) {
    const d = join(sysA, "bus/thunderbolt/devices", dev);
    write(join(d, "unique_id"), `${uuid}\n`);
    write(join(d, "authorized"), "0\n");
    write(join(d, "device_name"), `${name}\n`);
    write(join(d, "vendor_name"), "Razer\n");
    if (polkit) write(join(d, "polkit"), "");
  }
  write(join(dirs.a.models, "tiny-pool.gguf"), "GGUF fixture");
  const media = (v) => write(join(dirs.b.home, "media.json"), JSON.stringify(v));
  media({ ac_online: true, compute_pids: [], active_jobs: [], not_ready: null });
  write(join(dirs.b.home, ".local/bin/parish-media"), `#!/bin/sh\ncat "$HOME/media.json"\n`, 0o755);

  const lend = { a: "never", b: "idle", c: "always", d: "always" };
  const allPorts = Object.values(ports);
  for (const n of names) {
    const d = dirs[n];
    const config = {
      id: `cluster-${n}`,
      name: `Machine ${n.toUpperCase()}`,
      url: `http://127.0.0.1:${ports[n]}`,
      fleetKey: n === "d" ? randomBytes(24).toString("hex") : key,
      peers: [],
      excludePorts: [
        ...allPorts,
        ...Object.values(rpcPort),
        ...(n === "a" ? [e2] : n === "b" ? [e1] : [e1, e2]),
        ...(n === "a" ? [] : poolRange),
      ],
      enginePorts: [e1, e2],
      engineKeys: {},
      registry: { url: join(root, "registry"), dir: join(root, "registry"), ref: commit },
      modelsDir: d.models,
      cluster: {
        discover: true,
        lend: lend[n],
        ports: allPorts,
        idleSeconds: 1,
        drainSeconds: 4,
        failoverMs: 1500,
        ...(rpcPort[n] ? { rpcPort: rpcPort[n] } : {}),
        rpcServer: join(stubs, "rpc-server"),
        llamaServer: join(stubs, "llama-server"),
      },
    };
    write(join(d.data, "config.json"), JSON.stringify(config), 0o600);
    const env = {
      ...process.env,
      HOME: d.home,
      HF_HOME: join(d.home, "hf"),
      LOCAL_STUDIO_CONTROLLER_HOME: d.data,
      LOCAL_STUDIO_CONTROLLER_PORT: String(ports[n]),
      LOCAL_STUDIO_CONTROLLER_HOST: "127.0.0.1",
      LOCAL_STUDIO_SYSFS: d.sys,
      LOCAL_STUDIO_TEST_PRESENT: join(d.gpu, "present"),
      LOCAL_STUDIO_TEST_POWER_TTL_MS: "3000",
      FAKE_GPU_DIR: d.gpu,
      STUB_DIR: stubs,
      PATH: `${bin}:${process.env.PATH}`,
    };
    const child = spawn("bun", [new URL("../src/main.ts", import.meta.url).pathname], { env, stdio: ["ignore", "pipe", "pipe"] });
    let log = "";
    for (const s of [child.stdout, child.stderr]) s.on("data", (c) => (log += c));
    child.log = () => log;
    children.push(child);
    await waitFor(async () => {
      if (child.exitCode !== null) throw new Error(log);
      return (await call(ports[n], "/api/health", undefined, {})).ok;
    }, `controller ${n} health`);
  }
  const { a: A, b: B, c: C, d: D } = ports;
  const plLog = (n) => (existsSync(join(dirs[n].gpu, "pl.log")) ? readFileSync(join(dirs[n].gpu, "pl.log"), "utf8").trim().split("\n") : []);

  step("a power limit left by a crash is restored at start");
  await waitFor(async () => plLog("a").includes("-i 0 -pl 150") && !existsSync(join(dirs.a.data, "power-restore.json")), "A restores GPU 0 to 150 W and drops the record", 10_000);
  step("a process left stopped by a crash is continued at start");
  const procState = (pid) => {
    if (process.platform === "linux") {
      const t = readFileSync(`/proc/${pid}/stat`, "utf8");
      return t.slice(t.lastIndexOf(")") + 2).split(" ")[0];
    }
    return execFileSync("ps", ["-o", "stat=", "-p", String(pid)]).toString().trim();
  };
  await waitFor(async () => !procState(leftover.pid).startsWith("T") && !existsSync(join(dirs.a.data, "power-paused.json")), "A continues the stopped leftover and drops the record", 10_000);
  assert(procState(decoy.pid).startsWith("T"), "a pid whose command changed is never continued");

  step("discovery over the tailnet with a fleet-key handshake");
  const started = Date.now();
  await waitFor(async () => {
    const links = (await snap(A)).controllers;
    return [B, C].every((p) => links.some((l) => l.url === `http://127.0.0.1:${p}` && l.reachable));
  }, "A discovers B and C", 45_000);
  await waitFor(async () => {
    const links = (await snap(B)).controllers;
    return [A, C].every((p) => links.some((l) => l.url === `http://127.0.0.1:${p}` && l.reachable));
  }, "B discovers A and C", 45_000);
  console.log(`   peered in ${Math.round((Date.now() - started) / 1000)} s`);
  const configA = JSON.parse(readFileSync(join(dirs.a.data, "config.json"), "utf8"));
  assert(!configA.peers.includes(`http://127.0.0.1:${D}`), "a controller with another fleet key must not be peered");
  assert.deepEqual(JSON.parse(readFileSync(join(dirs.d.data, "config.json"), "utf8")).peers, []);
  assert.equal((await snap(A)).cluster.discovered.some((x) => x.id === "cluster-d"), false);
  const relay = await call(B, "/api/cluster/hello", { id: "x", nonce: randomBytes(24).toString("base64url"), target: `http://127.0.0.1:${A}` }, { "content-type": "application/json" });
  assert.equal(relay.status, 400, "hello must refuse a target that is not its own address");
  const own = `http://127.0.0.1:${B}`;
  for (const target of [`${own}/`, `${own}/x`, `${own}?x`, `${own}#x`, `${own}/#|http://127.0.0.1:${A}`, `http://me@127.0.0.1:${B}`, `https://127.0.0.1:${B}`, `http://127.1:${B}`]) {
    const odd = await call(B, "/api/cluster/hello", { id: "x", nonce: randomBytes(24).toString("base64url"), target }, { "content-type": "application/json" });
    assert.equal(odd.status, 400, `hello must answer its exact address only, not ${target}`);
  }
  for (const nonce of ["short", "x".repeat(129), `${randomBytes(24).toString("base64url")}|`]) {
    const odd = await call(B, "/api/cluster/hello", { id: "x", nonce, target: own }, { "content-type": "application/json" });
    assert.equal(odd.status, 400, `hello must refuse the nonce ${nonce}`);
  }
  const padded = JSON.stringify({ id: "x", nonce: randomBytes(24).toString("base64url"), target: `http://127.0.0.1:${A}`, pad: "x".repeat(8192) });
  const chunked = await fetch(`http://127.0.0.1:${B}/api/cluster/hello`, { method: "POST", headers: { "content-type": "application/json" }, body: new Blob([padded]).stream(), duplex: "half" });
  assert.equal(chunked.status, 413, "hello must count the bytes of a chunked body");
  const health = await (await call(A, "/api/health", undefined, {})).json();
  assert.equal(health.cluster, true);

  step("Thunderbolt authorize and eGPU hot-plug");
  let s = await waitFor(async () => {
    const x = await snap(A);
    return x.hardware.thunderbolt.length ? x : null;
  }, "A reads its Thunderbolt devices", 10_000);
  const egpuNote = s.hardware.os === "darwin" && s.hardware.arch === "arm64";
  assert.equal(!!s.hardware.egpuNote, egpuNote, "Apple Silicon note matches the host");
  assert.equal(s.hardware.thunderbolt.find((t) => t.uuid === tbUuid)?.state, "pending");
  let r = await call(A, "/api/thunderbolt/authorize", { uuid: tbUuid });
  assert.equal(r.status, 200, await r.clone().text());
  s = await snap(A);
  assert.equal(s.hardware.thunderbolt.find((t) => t.uuid === tbUuid)?.state, "authorized");
  r = await call(A, "/api/thunderbolt/authorize", { uuid: lockedUuid });
  assert.equal(r.status, 403);
  assert.match((await r.json()).error.message, /administrator password/);
  assert.equal((await call(A, "/api/thunderbolt/authorize", { uuid: "../../x" })).status, 400);
  const plugged = Date.now();
  write(join(dirs.a.gpu, "gpus.csv"), internalA + egpuA);
  s = await waitFor(async () => {
    const x = await snap(A);
    return x.hardware.gpus.some((g) => g.name === "NVIDIA GeForce RTX 3090") && x.hardware.hotplug.some((h) => h.change === "added") ? x : null;
  }, "eGPU appears", 8_000);
  const appear = Date.now() - plugged;
  console.log(`   eGPU appeared in ${appear} ms`);
  assert(appear <= 5_000, `eGPU took ${appear} ms to appear`);
  const egpu = s.hardware.gpus.find((g) => g.name === "NVIDIA GeForce RTX 3090");
  assert.equal(egpu.external, true);
  assert.equal(s.hardware.gpus.find((g) => g.index === 0).external, false);
  assert.equal(egpu.link.maxGen, 3);
  assert.equal(egpu.link.maxWidth, 4);
  assert(Math.abs(egpu.link.gbps - 3.9) < 0.1, `bandwidth ${egpu.link.gbps}`);
  assert.match(egpu.link.note, /PCIe 3\.0 x4/);
  const added = s.hardware.hotplug.find((h) => h.change === "added");
  assert.equal(added.external, true);
  assert.match(added.message, /Thunderbolt or USB4/);
  await waitFor(async () => (await (await call(A, "/api/recipes")).json()).recipes.some((x) => x.id === "rtx3090/tiny" && x.fits), "eGPU recipe fits", 20_000);
  const job = await (await call(A, "/api/recipes/rtx3090%2Ftiny/run", {})).json();
  assert.equal(job.job.gpus[0], 1, JSON.stringify(job));
  const unplugged = Date.now();
  write(join(dirs.a.gpu, "lost"), "");
  write(join(dirs.a.gpu, "gpus.csv"), internalA);
  s = await waitFor(async () => {
    const x = await snap(A);
    return x.jobs.find((j) => j.id === job.job.id)?.phase === "failed" ? x : null;
  }, "job on the removed GPU fails", 8_000);
  console.log(`   eGPU removal failed its job in ${Date.now() - unplugged} ms`);
  assert(Date.now() - unplugged <= 5_000);
  assert.match(s.jobs.find((j) => j.id === job.job.id).message, /NVIDIA GeForce RTX 3090 was unplugged/);
  assert(s.hardware.hotplug.some((h) => h.change === "removed"));
  assert(!s.hardware.gpus.some((g) => g.external));
  rmSync(join(dirs.a.gpu, "lost"));
  write(join(dirs.a.gpu, "gpus.csv"), internalA + egpuA);
  s = await waitFor(async () => {
    const x = await snap(A);
    return x.hardware.hotplug.find((h) => h.change === "added" && h.suggestions.length) ? x : null;
  }, "suggestions for the new card", 8_000);
  assert(s.hardware.hotplug.find((h) => h.change === "added").suggestions.includes("tiny-egpu-model (vllm)"));
  write(join(dirs.a.gpu, "lostonly"), "");
  s = await waitFor(async () => {
    const x = await snap(A);
    return !x.hardware.gpus.some((g) => g.external) ? x : null;
  }, "a lost eGPU with no rows printed drops only that GPU", 8_000);
  assert(s.hardware.gpus.some((g) => g.index === 0 && !g.external), "the internal GPU stays listed when only the eGPU is lost");
  assert(!s.hardware.hotplug.some((h) => h.change === "removed" && !h.external), "the internal GPU is not reported unplugged");
  let set = await call(A, "/api/cluster", { lend: "idle" });
  assert(set.ok, await set.text());
  s = await waitFor(async () => {
    const x = await snap(A);
    return x.cluster.checks.some((c) => c.name === "GPU readings" && !c.ok) ? x : null;
  }, "frozen readings after a lost GPU fail the lending check", 10_000);
  assert.notEqual(s.cluster.state, "lent", "a machine with a lost GPU does not lend on stale readings");
  set = await call(A, "/api/cluster", { lend: "never" });
  assert(set.ok, await set.text());
  rmSync(join(dirs.a.gpu, "lostonly"));
  await waitFor(async () => ((await snap(A)).hardware.gpus.some((g) => g.external) ? true : null), "eGPU back after the lost read", 8_000);

  step("routing by load and failover");
  const pick = async (body = { model: shared, messages: [] }) => {
    const res = await call(C, "/v1/chat/completions", body);
    const text = await res.text();
    return { status: res.status, endpoint: res.headers.get("x-local-studio-endpoint"), failover: res.headers.get("x-local-studio-failover"), text };
  };
  const onA = `cluster-a:${e1}`;
  const onB = `cluster-b:${e2}`;
  engineState.get("e1").running = 6;
  await waitFor(async () => (await pick()).endpoint === onB, "busy A sends work to B");
  engineState.get("e1").running = 0;
  engineState.get("e2").running = 7;
  await waitFor(async () => (await pick()).endpoint === onA, "busy B sends work to A");
  const picks = await Promise.all(Array.from({ length: 4 }, () => pick()));
  assert(picks.every((p) => p.endpoint === onA && p.status === 200), JSON.stringify(picks));
  engineState.get("e1").mode = "fail";
  let p = await pick();
  assert.equal(p.status, 200, p.text);
  assert.equal(p.endpoint, onB);
  assert.equal(p.failover, "1");
  engineState.get("e1").mode = "hang";
  const t0 = Date.now();
  p = await pick({ model: shared, stream: true, messages: [] });
  const waited = Date.now() - t0;
  assert.equal(p.status, 200, p.text);
  assert.equal(p.endpoint, onB);
  assert.match(p.text, /"engine":"e2"/);
  assert(waited >= 1_400 && waited < 10_000, `failover after ${waited} ms`);
  console.log(`   stream failed over after ${waited} ms`);
  engineState.get("e1").mode = "reset";
  const before = engineState.get("e2").hits;
  p = await pick();
  assert.equal(p.status, 502, p.text);
  assert.equal(engineState.get("e2").hits, before, "a request the engine already took is never sent to a second copy");
  engineState.get("e1").mode = "ok";
  engineState.get("e2").running = 0;

  step("lending");
  await waitFor(async () => (await snap(B)).cluster.state === "lent", "B lends when idle", 15_000);
  s = await snap(B);
  assert.equal(s.cluster.lend, "idle");
  assert(s.cluster.checks.every((c) => c.ok), JSON.stringify(s.cluster.checks));
  assert(s.cluster.checks.some((c) => c.name === "parish-media"));
  await waitFor(async () => (await snap(C)).cluster.state === "lent", "C lends always");
  assert.equal((await snap(A)).cluster.state, "off");
  await waitFor(async () => (await snap(A)).cluster.machines.filter((m) => m.state === "lent").length === 2, "A sees two lenders");

  step("pooled model over llama.cpp RPC");
  const model = join(dirs.a.models, "tiny-pool.gguf");
  s = await snap(A);
  assert(s.cluster.models.some((m) => m.path === model), "A lists its GGUF models");
  const dry = await (await call(A, "/api/cluster/pool", { model, dryRun: true })).json();
  assert.deepEqual(dry.argv.slice(1, 4), ["-m", model, "--rpc"]);
  assert.deepEqual(dry.argv[4].split(",").sort(), [`127.0.0.1:${rpcPort.b}`, `127.0.0.1:${rpcPort.c}`].sort());
  assert.equal(dry.pool, null);
  const race = await Promise.all([1, 2].map(() => call(A, "/api/cluster/pool", { model, alias: "pooled-tiny" }, auth, 60_000)));
  assert.deepEqual(race.map((x) => x.status).sort(), [200, 409], "two launches at once get one pool");
  r = race.find((x) => x.status === 200);
  assert.match((await race.find((x) => x.status === 409).json()).error.message, /already serves a pooled model/);
  const launched = await r.json();
  assert.equal(r.status, 200, JSON.stringify(launched));
  const argv = launched.argv;
  const rpcArg = argv[argv.indexOf("--rpc") + 1].split(",").sort();
  assert.deepEqual(rpcArg, [`127.0.0.1:${rpcPort.b}`, `127.0.0.1:${rpcPort.c}`].sort());
  for (const [flag, value] of [["-m", model], ["--host", "127.0.0.1"], ["-ngl", "999"], ["--alias", "pooled-tiny"]]) assert.equal(argv[argv.indexOf(flag) + 1], value, flag);
  assert(argv.includes("--metrics"));
  await waitFor(async () => (await snap(A)).cluster.pools.find((x) => x.id === launched.pool.id)?.phase === "ready", "pool ready");
  const rpcs = readdirSync(stubs).filter((f) => /^rpc-\d+\.json$/.test(f)).map((f) => JSON.parse(readFileSync(join(stubs, f), "utf8")));
  assert.equal(rpcs.length, 2);
  for (const x of rpcs) assert.equal(x.argv[x.argv.indexOf("-H") + 1], "127.0.0.1", "rpc-server binds only loopback behind the gate");
  const rpcB = (await snap(B)).cluster.rpcServers[0];
  assert.equal(rpcB.forId, "cluster-a");
  assert.equal(rpcB.endpoint, `127.0.0.1:${rpcPort.b}`);
  const bStub = rpcs.find((x) => x.owner === dirs.b.gpu);
  assert(bStub && alive(bStub.pid), "B runs one rpc-server");
  r = await call(A, "/api/cluster/pool", { model, alias: "pooled-twice" }, auth, 30_000);
  assert.equal(r.status, 409, "a second pool on the same lenders is refused");
  assert.match((await r.json()).error.message, /already serves a pooled model/);
  assert.equal(bStub.cuda, "0");
  await waitFor(async () => {
    const res = await call(C, "/v1/chat/completions", { model: "pooled-tiny", messages: [] });
    return res.ok && (await res.json()).pooled === true;
  }, "pooled model answers through the fleet gateway", 25_000);
  const refused = await new Promise((resolve) => {
    const sock = connect({ host: "127.0.0.1", port: rpcPort.b, localAddress: lanIp });
    let got = "";
    sock.on("data", (d) => (got += d));
    sock.on("close", () => resolve(got));
    sock.on("error", () => resolve(got));
    setTimeout(() => {
      sock.destroy();
      resolve(`${got}(open)`);
    }, 3_000);
  });
  assert.equal(refused, "", `the gate must refuse a non-fleet address, got ${JSON.stringify(refused)}`);
  const bound =
    process.platform === "linux"
      ? execFileSync("ss", ["-H", "-ltn", `sport = :${rpcPort.b}`]).toString().trim().split("\n").map((l) => l.trim().split(/\s+/)[3])
      : execFileSync("lsof", ["-nP", `-iTCP:${rpcPort.b}`, "-sTCP:LISTEN", "-Fn"]).toString().split("\n").filter((l) => l.startsWith("n")).map((l) => l.slice(1));
  assert.deepEqual([...new Set(bound)], [`127.0.0.1:${rpcPort.b}`], "the gate listens only on the tailnet address");

  step("fleet GPU work does not count as the owner");
  write(join(dirs.b.gpu, "apps.csv"), `GPU-b-0, ${bStub.pid}\n`);
  media({ ac_online: true, compute_pids: [String(bStub.pid)], active_jobs: [], not_ready: `GPU is in use; compute=['${bStub.pid} (ggml-rpc-server)']` });
  await new Promise((r) => setTimeout(r, 5_000));
  s = await snap(B);
  assert.equal(s.cluster.state, "lent", JSON.stringify(s.cluster.checks));
  assert.equal(s.cluster.rpcServers.length, 1);

  step("power budget while the owner is present");
  const cl = async (port) => (await call(port, "/api/cluster")).json();
  const present = (v) => write(join(dirs.b.gpu, "present"), v ? "1\n" : "0\n");
  const restoreB = join(dirs.b.data, "power-restore.json");
  const stat = (pid) => {
    if (process.platform === "linux") {
      const t = readFileSync(`/proc/${pid}/stat`, "utf8");
      return t.slice(t.lastIndexOf(")") + 2).split(" ")[0];
    }
    return execFileSync("ps", ["-o", "stat=", "-p", String(pid)]).toString().trim();
  };
  const seen = async (pid, ms) => {
    const out = new Set();
    const end = Date.now() + ms;
    while (Date.now() < end && out.size < 2) {
      out.add(stat(pid).startsWith("T") ? "stopped" : "running");
      await new Promise((r) => setTimeout(r, 7));
    }
    return out;
  };
  s = await cl(B);
  assert.equal(s.state, "lent");
  assert.deepEqual(
    [s.power.activeWatts, s.power.awayWatts, s.power.present, s.power.presenceSource, s.power.capW, s.power.lever],
    [0, null, false, "test override", null, "none"],
    JSON.stringify(s.power),
  );
  assert.equal(s.power.helper, process.platform === "linux" ? "missing" : "unsupported");
  await waitFor(async () => (await cl(B)).power.maxW === 320, "B reports its GPU's 320 W max_limit as maxW", 10_000);
  for (const bad of [{ activeWatts: 2001 }, { activeWatts: 1.5 }, { awayWatts: -1 }, { awayWatts: 0 }, { activeWatts: null }, { activeWatts: "60" }]) {
    r = await call(B, "/api/cluster", { power: bad });
    assert.equal(r.status, 400, `${JSON.stringify(bad)} must be refused`);
  }
  write(join(dirs.b.gpu, "sudo-allow"), "");
  r = await call(B, "/api/cluster", { power: { activeWatts: 60 } });
  assert.equal(r.status, 200, await r.clone().text());
  s = await cl(B);
  assert.equal(s.power.activeWatts, 60);
  assert.equal(s.power.awayWatts, null);
  assert.deepEqual(JSON.parse(readFileSync(join(dirs.b.data, "config.json"), "utf8")).cluster.power, { activeWatts: 60 });
  const lentReason = s.reason;
  const plStart = plLog("b").length;
  present(true);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.state === "lent" && x.power.present === true && x.power.lever === "duty-cycle" ? x : null;
  }, "B keeps lending at 60 W through the duty cycle while its owner is present", 10_000);
  console.log(`   present: ${s.reason} (was: ${lentReason})`);
  assert.equal(s.reason, "In use, so it lends at up to 60 W.");
  assert.notEqual(s.reason, lentReason);
  assert.equal(s.power.capW, 60);
  assert.deepEqual(s.power.gpus, [{ uuid: "GPU-b-0", index: 0, lever: "duty-cycle", limitW: null, originalW: null }]);
  assert.equal(plLog("b").length, plStart, "a board-wide limit would throttle the owner's own work, so none is set while present");
  assert(!existsSync(restoreB));
  assert(s.checks.some((c) => c.name === "Owner" && c.ok && /up to 60 W/.test(c.detail)), JSON.stringify(s.checks));

  step("power budget while the owner is away uses the NVIDIA limit");
  r = await call(B, "/api/cluster", { power: { awayWatts: 100 } });
  assert.equal(r.status, 200, await r.clone().text());
  present(false);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.present === false && x.power.lever === "nvidia-limit" ? x : null;
  }, "away, B caps the GPU at 100 W through the NVIDIA limit", 10_000);
  assert.equal(s.reason, "Idle, so its GPU is lent to the fleet, at up to 100 W.");
  assert.deepEqual(s.power.gpus, [{ uuid: "GPU-b-0", index: 0, lever: "nvidia-limit", limitW: 100, originalW: 200 }]);
  assert.equal(plLog("b").at(-1), "-i 0 -pl 100");
  assert.deepEqual(JSON.parse(readFileSync(restoreB, "utf8")), { "GPU-b-0": { index: 0, originalW: 200 } });
  present(true);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.present === true && x.power.lever === "duty-cycle" && !existsSync(restoreB) ? x : null;
  }, "the owner comes back: the GPU gets its own limit back and the duty cycle takes over", 10_000);
  assert.equal(plLog("b").at(-1), "-i 0 -pl 200");
  r = await call(B, "/api/cluster", { power: { awayWatts: null } });
  assert.equal(r.status, 200);
  present(false);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.present === false && x.power.lever === "none" ? x : null;
  }, "away with no away cap pulls no lever", 10_000);
  assert.equal(s.reason, "Idle, so its GPU is lent to the fleet.");
  assert.equal(plLog("b").at(-1), "-i 0 -pl 200");
  assert(!existsSync(restoreB), "the restore record is removed once the original limit is back");
  rmSync(join(dirs.b.gpu, "sudo-allow"));
  write(join(dirs.b.gpu, "draw.csv"), "GPU-b-0, 120.00\n");
  r = await call(B, "/api/cluster", { power: { activeWatts: 40, awayWatts: 500 } });
  assert.equal(r.status, 200, await r.clone().text());
  present(true);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.present === true && x.power.capW === 40 && x.power.lever === "duty-cycle" && x.power.measuredW === 120 ? x : null;
  }, "present, B duty-cycles on measured draw", 10_000);
  console.log(`   duty cycle: ${s.reason}`);
  assert.equal(s.reason, "In use, so it lends at up to 40 W.");
  assert.equal(s.power.measured, true);
  assert(!existsSync(restoreB), "a refused limit leaves no restore record");
  const bystander = spawn("sleep", ["120"], { stdio: "ignore" });
  strays.push(bystander);
  const both = await waitFor(async () => ((await seen(bStub.pid, 3_000)).size === 2 ? true : null), "the lent rpc-server is stopped and continued", 10_000);
  assert(both);
  assert.deepEqual([...(await seen(bystander.pid, 1_000))], ["running"], "a process the controller did not start is never stopped");
  r = await call(B, "/api/cluster", { power: { awayWatts: null } });
  assert.equal(r.status, 200);
  present(false);
  await waitFor(async () => (await cl(B)).power.lever === "none", "budget removed", 10_000);
  const freed = await seen(bStub.pid, 1_500);
  assert.deepEqual([...freed], ["running"], "removing the budget continues the lent process");
  present(true);
  await waitFor(async () => (await cl(B)).power.lever === "duty-cycle", "duty cycle again", 10_000);
  await waitFor(async () => ((await seen(bStub.pid, 3_000)).has("stopped") ? true : null), "stopped again", 10_000);

  step("duty cycle with no readable draw uses a fixed share");
  write(join(dirs.b.gpu, "draw.csv"), "");
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.lever === "duty-cycle" && x.power.measured === false && /can't read the GPU's power draw, so it runs fleet work 20% of the time/.test(x.reason) ? x : null;
  }, "B duty-cycles at cap / limit when nvidia-smi reports no draw", 10_000);
  console.log(`   unmeasured: ${s.reason}`);
  await waitFor(async () => ((await seen(bStub.pid, 3_000)).size === 2 ? true : null), "the lent rpc-server is still stopped and continued", 10_000);
  write(join(dirs.b.gpu, "draw.csv"), "GPU-b-0, 120.00\n");

  step("installing the helper later switches to the NVIDIA limit without a new request");
  r = await call(B, "/api/cluster", { power: { awayWatts: 100 } });
  assert.equal(r.status, 200);
  present(false);
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.present === false && x.power.lever === "duty-cycle" && /sudo needs a password/.test(x.reason) ? x : null;
  }, "away without sudo, B falls back to the duty cycle and says why", 10_000);
  console.log(`   no helper: ${s.reason}`);
  write(join(dirs.b.gpu, "sudo-allow"), "");
  s = await waitFor(async () => {
    const x = await cl(B);
    return x.power.lever === "nvidia-limit" && x.power.helper === "installed" ? x : null;
  }, "B retries sudo and sets the limit", 15_000);
  assert.equal(plLog("b").at(-1), "-i 0 -pl 100");
  assert.deepEqual([...(await seen(bStub.pid, 1_500))], ["running"], "the duty cycle stops once the limit holds");

  step("reclaim when the owner returns");
  const owner = spawn("sleep", ["120"], { stdio: "ignore" });
  strays.push(owner);
  const tReclaim = Date.now();
  write(join(dirs.b.gpu, "apps.csv"), `GPU-b-0, ${bStub.pid}\nGPU-b-0, ${owner.pid}\n`);
  s = await waitFor(async () => {
    const b = await snap(B);
    const a = await snap(A);
    const pool = a.cluster.pools.find((x) => x.id === launched.pool.id);
    return b.cluster.state === "reclaimed" && !b.cluster.rpcServers.length && !alive(bStub.pid) && pool?.phase === "failed" ? { a, b, pool } : null;
  }, "reclaim", 12_000);
  const took = Date.now() - tReclaim;
  console.log(`   reclaimed in ${took} ms: ${s.b.cluster.reason}`);
  assert(took <= 10_000, `reclaim took ${took} ms`);
  assert.match(s.b.cluster.reason, new RegExp(`pid ${owner.pid}`));
  assert.match(s.pool.message, /Machine B took its GPU back/);
  const llama = readdirSync(stubs).filter((f) => /^llama-\d+\.json$/.test(f)).map((f) => JSON.parse(readFileSync(join(stubs, f), "utf8")));
  await waitFor(async () => llama.every((x) => !alive(x.pid)), "llama-server stopped");
  assert(alive(owner.pid), "the owner's process is never touched");
  assert(existsSync(join(stubs, `term-${bStub.pid}`)), "the lent rpc-server was continued before it was stopped, so its SIGTERM handler ran");
  s = await cl(B);
  assert.equal(s.power.lever, "none");
  assert.equal(s.power.capW, null);
  await waitFor(async () => (await snap(C)).cluster.rpcServers.length === 0, "C releases its rpc-server after the pool stops");

  step("battery");
  write(join(dirs.b.gpu, "apps.csv"), "");
  media({ ac_online: true, compute_pids: [], active_jobs: [], not_ready: null });
  await waitFor(async () => (await snap(B)).cluster.state === "lent", "B lends again", 15_000);
  const plBefore = plLog("b").length;
  await new Promise((r) => setTimeout(r, 3_000));
  s = await cl(B);
  assert.equal(s.power.lever, "none", "nothing is borrowed, so no lever is pulled");
  assert.equal(plLog("b").length, plBefore, "an idle lent GPU keeps its own power limit");
  write(join(dirs.b.sys, "class/power_supply/AC/online"), "0\n");
  await waitFor(async () => {
    const b = await snap(B);
    return b.cluster.state === "reclaimed" && /battery/.test(b.cluster.reason);
  }, "B reclaims on battery", 8_000);
  r = await call(B, "/api/cluster/rpc", { id: "cluster-a", name: "Machine A", url: `http://127.0.0.1:${A}` });
  assert.equal(r.status, 409);
  assert.match((await r.json()).error.message, /not lending/);

  step("an unlinked peer stays unlinked");
  r = await call(A, "/api/peers", { url: `http://127.0.0.1:${B}`, remove: true });
  assert.equal(r.status, 200);
  await new Promise((r) => setTimeout(r, 20_000));
  const after = JSON.parse(readFileSync(join(dirs.a.data, "config.json"), "utf8"));
  assert(!after.peers.includes(`http://127.0.0.1:${B}`), "discovery must not link a peer the user removed");
  assert(after.cluster.ignored.includes(`http://127.0.0.1:${B}`));

  console.log("PASS cluster_smoke: tailnet discovery with fleet-key handshake, unlink kept, Thunderbolt authorize, eGPU hot-plug and bandwidth, job failure on clean and surprise unplug, routing by load, failover on error and timeout, no second run of a taken request, lending checks, pooled llama.cpp RPC launch through the gate, reclaim within 10 s, battery, power budget (crash restore at start, stopped leftovers continued at start, NVIDIA limit set and restored, unreadable draw falls back to a fixed share, a later helper install takes over without a new request, no lever while nothing is borrowed, presence flips the reason, duty-cycle SIGSTOP/SIGCONT of the lent rpc-server only, SIGCONT on budget removal and reclaim).");
} catch (error) {
  for (const [i, c] of children.entries()) console.error(`--- controller ${["a", "b", "c", "d"][i]} log\n${c.log?.().slice(-3000)}`);
  throw error;
} finally {
  for (const child of children) child.kill("SIGTERM");
  await Promise.all(children.map((child) => (child.exitCode === null ? once(child, "exit") : undefined)));
  for (const f of existsSync(join(root, "stubs")) ? readdirSync(join(root, "stubs")) : []) {
    const m = /^(?:rpc|llama)-(\d+)\.json$/.exec(f);
    if (m && alive(Number(m[1]))) process.kill(Number(m[1]), "SIGKILL");
  }
  for (const s of strays) s.kill("SIGKILL");
  for (const server of servers) {
    server.closeAllConnections?.();
    server.close();
  }
  rmSync(root, { recursive: true, force: true });
}
