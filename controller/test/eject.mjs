import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";

assert.notEqual(process.getuid?.(), 0, "run as a normal user: root can write a read-only remove file");
const root = mkdtempSync(join(tmpdir(), "local-studio-eject-"));
const key = randomBytes(24).toString("hex");
const auth = { authorization: `Bearer ${key}`, "content-type": "application/json" };
const children = [];
const state = join(root, "state");
const apps = join(state, "apps");
const pinned = join(state, "pinned");
const sys = join(root, "sys");
const INTERNAL = "00000000:01:00.0";
const EXTERNAL = "00000000:0B:00.0";
const remove = join(sys, "bus/pci/devices/0000:0b:00.0/remove");
const git = (...args) => execFileSync("git", args, { cwd: root, stdio: "pipe", timeout: 10_000 });
const freePort = () =>
  new Promise((resolve) => {
    const s = createServer().listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => resolve(port));
    });
  });
const waitFor = async (fn, label, ms = 30_000) => {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      if (await fn()) return;
    } catch (error) {
      last = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out: ${label}`, { cause: last });
};
const call = (port, path, body) =>
  fetch(`http://127.0.0.1:${port}${path}`, { headers: auth, ...(body === undefined ? {} : { method: "POST", body: JSON.stringify(body) }), signal: AbortSignal.timeout(60_000) });
const snapshot = async (port) => (await call(port, "/api/snapshot")).json();
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

const start = async (name, env = {}) => {
  const port = await freePort();
  const home = join(root, name);
  mkdirSync(home);
  writeFileSync(
    join(home, "config.json"),
    JSON.stringify({ id: name, name, url: `http://127.0.0.1:${port}`, fleetKey: key, peers: [], excludePorts: [], enginePorts: [], engineKeys: {}, registry: { url: root, dir: root, ref: commit }, modelsDir: join(home, "models") }),
    { mode: 0o600 },
  );
  writeFileSync(
    join(home, "jobs.json"),
    JSON.stringify([{ id: "job1", recipeId: "plain", gpus: [1], gpuUuids: ["GPU-ext"], port: 18150, container: "local-studio-job1", phase: "ready", progress: 1, message: null, startedAt: "2026-01-01T00:00:00Z", updatedAt: "2026-01-01T00:00:00Z" }]),
  );
  const child = spawn("bun", [new URL("../src/main.ts", import.meta.url).pathname], {
    env: { ...process.env, ...env, PATH: `${join(root, "bin")}:${process.env.PATH}`, EJECT_STATE: state, LOCAL_STUDIO_SYSFS: sys, LOCAL_STUDIO_CONTROLLER_HOME: home, LOCAL_STUDIO_CONTROLLER_PORT: String(port), LOCAL_STUDIO_CONTROLLER_HOST: "127.0.0.1" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  for (const s of [child.stdout, child.stderr]) s.on("data", (c) => (log += c));
  children.push(child);
  await waitFor(async () => {
    if (child.exitCode !== null) throw new Error(log);
    const s = await snapshot(port);
    const r = await (await call(port, "/api/recipes")).json();
    return s.hardware.gpus.length === 2 && r.recipes.some((x) => x.id === "plain");
  }, `${name} sees both GPUs and the registry`);
  return port;
};

let commit;
try {
  mkdirSync(state);
  mkdirSync(join(root, "bin"));
  writeFileSync(apps, "GPU-ext,00000000:0B:00.0,4242,vllm,20480,local-studio-job1\n");
  writeFileSync(pinned, "Disabled, Disabled\n");
  writeFileSync(
    join(root, "bin/nvidia-smi"),
    [
      "#!/bin/sh",
      'case "$1" in',
      `--query-gpu=display_active,persistence_mode) cat "$EJECT_STATE/pinned" ;;`,
      `--query-gpu=*) printf '0, GPU-int, NVIDIA GeForce RTX 4090, 900, 24564, 30.1, 450.00, 40, 0, ${INTERNAL}, 4, 4, 16, 16\\n1, GPU-ext, NVIDIA GeForce RTX 3090, 900, 24576, 20.5, 350.00, 38, 0, ${EXTERNAL}, 3, 3, 4, 4\\n' ;;`,
      `--query-compute-apps=gpu_uuid,pid) awk -F, '$7!="G" {print $1", "$3}' "$EJECT_STATE/apps" ;;`,
      `-q) awk -F, -v bus="$5" '$2==bus {print "        Process ID                        : "$3; print "            Type                          : "($7=="" ? "C" : $7); print "            Name                          : /usr/bin/"$4; print "            Used GPU Memory               : "$5" MiB"}' "$EJECT_STATE/apps" ;;`,
      "esac",
      "",
    ].join("\n"),
    { mode: 0o755 },
  );
  writeFileSync(
    join(root, "bin/docker"),
    [
      "#!/bin/sh",
      'echo "$*" >> "$EJECT_STATE/docker.log"',
      'case "$1 $2" in',
      '"inspect -f") case "$3" in *Labels*) echo "job1 " ;; *) echo "true 0" ;; esac ;;',
      `"rm -f") awk -F, -v c="$3" '$6!=c' "$EJECT_STATE/apps" > "$EJECT_STATE/apps.next" && mv "$EJECT_STATE/apps.next" "$EJECT_STATE/apps" ;;`,
      "esac",
      "",
    ].join("\n"),
    { mode: 0o755 },
  );
  const bridge = join(sys, "devices/pci0000:00/0000:00:07.0");
  for (const [id, external] of [["0000:01:00.0", false], ["0000:0b:00.0", true]]) {
    const dir = join(bridge, id);
    mkdirSync(dir, { recursive: true });
    if (external) writeFileSync(join(dir, "removable"), "removable\n");
    writeFileSync(join(dir, "remove"), "");
    mkdirSync(join(sys, "bus/pci/devices"), { recursive: true });
    symlinkSync(dir, join(sys, "bus/pci/devices", id));
  }
  mkdirSync(join(root, "dist"));
  const recipe = { model: "plain", card: "unknown", engine: "vllm", weights: "baked-into-image", proof: [], launch: { port: 8000, ctx: 4096, image: `registry.example/engine@sha256:${"a".repeat(64)}`, weights: { repo: "org/model", revision: "b".repeat(40), at: "/model" } } };
  writeFileSync(join(root, "dist/catalog.json"), JSON.stringify({ cards: {}, recipes: { plain: recipe } }));
  git("init", "-q");
  git("add", "-f", "dist/catalog.json");
  git("-c", "user.name=Eject Smoke", "-c", "user.email=eject@localhost", "commit", "-qm", "fixture");
  commit = git("rev-parse", "HEAD").toString().trim();

  const port = await start("linux");
  const eject = (id) => call(port, `/api/hardware/gpus/${id}/eject`, {});
  const cancel = async (id) => (await call(port, `/api/hardware/gpus/${id}/eject/cancel`, {})).json();
  const dryRun = () => call(port, "/api/recipes/plain/run", { dryRun: true, gpus: [1] });
  const gpu = async (i) => (await snapshot(port)).hardware.gpus.find((g) => g.index === i);

  const internal = await gpu(0);
  assert.equal(internal.external, false);
  assert.equal("eject" in internal, false, "an internal GPU has no eject field");
  const refused = await eject(0);
  assert.equal(refused.status, 404);
  assert.equal((await refused.json()).error.code, "NOT_EXTERNAL");
  assert.equal((await gpu(1)).eject.state, "ready", "the controller's own model does not block eject");
  console.log("internal GPU: no eject field, POST answers 404 NOT_EXTERNAL");

  let r = await (await eject(1)).json();
  assert.equal(r.state, "removed", JSON.stringify(r));
  assert.deepEqual(r.unloaded, ["job1"]);
  assert.match(readFileSync(join(state, "docker.log"), "utf8"), /^rm -f local-studio-job1$/m, "the job went through the normal stop");
  assert.equal((await snapshot(port)).jobs.find((j) => j.id === "job1").phase, "stopped");
  assert.equal(readFileSync(remove, "utf8"), "1");
  console.log("controller's model on the GPU: unloaded through jobs.stop, then removed");
  assert.equal((await cancel(1)).draining, false);

  writeFileSync(remove, "");
  r = await (await eject(1)).json();
  assert.equal(r.state, "removed", JSON.stringify(r));
  assert.deepEqual(r.unloaded, []);
  assert.equal(readFileSync(remove, "utf8"), "1");
  console.log("nothing loaded, writable sysfs: removed, 1 written to the remove file");
  await cancel(1);

  writeFileSync(remove, "");
  const foreign = spawn("sleep", ["600"], { stdio: "ignore" });
  children.push(foreign);
  writeFileSync(apps, `GPU-ext,${EXTERNAL},${foreign.pid},sleep,512,\n`);
  r = await (await eject(1)).json();
  assert.equal(r.state, "busy", JSON.stringify(r));
  assert.deepEqual(r.processes, [{ pid: foreign.pid, name: "sleep", usedMiB: 512 }]);
  assert.equal(r.draining, true);
  assert(alive(foreign.pid), "the foreign process is untouched");
  assert.equal(readFileSync(remove, "utf8"), "", "nothing was written while busy");
  await waitFor(async () => (await gpu(1)).eject.blockers.some((b) => b.pid === foreign.pid), "snapshot lists the blocker");
  const shown = await gpu(1);
  assert.equal(shown.eject.state, "busy");
  assert.equal(shown.eject.draining, true);
  assert.equal(shown.busy, true);
  console.log(`foreign process: busy with pid ${foreign.pid} (sleep, 512 MiB), process alive, GPU draining`);

  const blocked = await dryRun();
  assert.equal(blocked.status, 409);
  assert.equal((await blocked.json()).error.code, "DRAINING");
  console.log("load while draining: 409 DRAINING");

  assert.equal((await cancel(1)).draining, false);
  assert.equal((await gpu(1)).eject.draining, false);
  const loaded = await dryRun();
  assert.equal(loaded.status, 200, await loaded.clone().text());
  assert((await loaded.json()).argv.includes("docker"));
  assert(alive(foreign.pid));
  console.log("cancel: draining cleared, the load goes through");
  foreign.kill("SIGTERM");

  const xorg = spawn("sleep", ["600"], { stdio: "ignore" });
  children.push(xorg);
  writeFileSync(apps, `GPU-ext,${EXTERNAL},${xorg.pid},Xorg,4,,G\n`);
  r = await (await eject(1)).json();
  assert.equal(r.state, "busy", JSON.stringify(r));
  assert.deepEqual(r.processes, [{ pid: xorg.pid, name: "Xorg", usedMiB: 4 }]);
  assert.equal(readFileSync(remove, "utf8"), "");
  await cancel(1);
  xorg.kill("SIGTERM");
  writeFileSync(apps, "");
  console.log(`graphics client: busy with pid ${xorg.pid} (Xorg), not in the compute apps`);

  writeFileSync(pinned, "Enabled, Disabled\n");
  r = await (await eject(1)).json();
  assert.equal(r.state, "busy", JSON.stringify(r));
  assert.match(r.message, /drives a display/);
  await cancel(1);
  writeFileSync(pinned, "Disabled, Enabled\n");
  r = await (await eject(1)).json();
  assert.equal(r.state, "busy", JSON.stringify(r));
  assert.equal(r.command, `sudo nvidia-smi -i ${EXTERNAL} -pm 0`);
  assert.equal(readFileSync(remove, "utf8"), "", "nothing was written while the driver is pinned");
  await cancel(1);
  writeFileSync(pinned, "Disabled, Disabled\n");
  console.log("display or persistence mode on the GPU: busy, nothing written");

  chmodSync(remove, 0o444);
  r = await (await eject(1)).json();
  assert.equal(r.state, "needs_root", JSON.stringify(r));
  assert.equal(r.command, `echo 1 | sudo tee ${remove}`);
  assert.equal(r.draining, true);
  assert.equal(readFileSync(remove, "utf8"), "");
  assert.equal((await dryRun()).status, 409, "still draining after needs_root");
  console.log(`sysfs not writable: needs_root with "${r.command}", GPU stays draining`);
  await cancel(1);

  const mac = await start("mac", { LOCAL_STUDIO_TEST_PLATFORM: "darwin-arm64" });
  const onMac = await (await call(mac, "/api/hardware/gpus/1/eject", {})).json();
  assert.equal(onMac.state, "unsupported", JSON.stringify(onMac));
  assert.equal((await snapshot(mac)).hardware.gpus.find((g) => g.index === 1).eject.state, "unsupported");
  console.log("macOS arm64: unsupported");
  console.log("PASS eject: internal, removed, unload first, busy, draining refusal, cancel, graphics client, display and persistence, needs_root, macOS arm64.");
} finally {
  for (const child of children) if (child.exitCode === null && child.signalCode === null) child.kill("SIGTERM");
  await Promise.all(children.map((child) => (child.exitCode === null && child.signalCode === null ? once(child, "exit") : undefined)));
  rmSync(root, { recursive: true, force: true });
}
