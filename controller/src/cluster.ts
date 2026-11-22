import { createHmac, randomBytes, timingSafeEqual } from "node:crypto";
import { closeSync, existsSync, openSync, readdirSync, readFileSync, statSync } from "node:fs";
import { createServer, connect, type Server, type Socket } from "node:net";
import { arch, homedir, platform } from "node:os";
import { basename, join } from "node:path";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import type { LocalCluster, LocalClusterRequest, LocalLendMode, LocalLendState, LocalNodeCluster, LocalPool, LocalPoolRequest } from "./contracts.ts";
import { type Config, decodeJson, exec, fail, fetchJson, home, now } from "./core.ts";
import type { Endpoint } from "./discovery.ts";
import { lentInFlight } from "./gateway.ts";
import { type Graph, normUrl } from "./graph.ts";
import { type Presence, presence, powerSource, type RawGpu } from "./hardware.ts";
import type { makeJobs } from "./jobs.ts";
import { makePower } from "./power.ts";
import { onlinePeers, tailnetIPv4 } from "./tailnet.ts";

type Jobs = ReturnType<typeof makeJobs>;

export interface ClusterDeps {
  config: () => Config;
  save: (c: Config) => void;
  gpus: () => RawGpu[];
  graph: () => Promise<Graph>;
  invalidate: () => void;
  jobs: Jobs;
  listenHost: string;
  port: number;
  takenPorts: () => Effect.Effect<Set<number>>;
}

interface Check {
  name: string;
  ok: boolean;
  detail: string;
}

interface Rpc {
  id: string;
  proc: Bun.Subprocess;
  gate: Server;
  conns: Set<Socket>;
  endpoint: string;
  forId: string;
  forName: string;
  forUrl: string;
  startedAt: string;
  gpus: number[];
  gpuIds: string[];
  internal: number;
  quietSince: number | null;
}

const RPC_QUIET_MS = 120_000;

const netOf = (ip: string): string =>
  /^127\./.test(ip) || ip === "::1" ? "loopback" : /^100\.(6[4-9]|[7-9]\d|1[01]\d|12[0-7])\./.test(ip) || /^fd7a:115c:a1e0:/i.test(ip) ? "tailnet" : "other";

interface Pool {
  info: LocalPool;
  proc: Bun.Subprocess | null;
  urls: Map<string, string>;
}

const decodeHealth = Schema.decodeUnknownOption(Schema.Struct({ id: Schema.String, name: Schema.String, cluster: Schema.optionalKey(Schema.Boolean) }));
const decodeHello = Schema.decodeUnknownOption(Schema.Struct({ id: Schema.String, name: Schema.String, proof: Schema.String }));
const decodeRpc = Schema.decodeUnknownOption(Schema.Struct({ endpoint: Schema.String }));
const MediaStatus = Schema.Struct({
  ac_online: Schema.optionalKey(Schema.NullOr(Schema.Boolean)),
  compute_pids: Schema.optionalKey(Schema.Array(Schema.Union([Schema.String, Schema.Number]))),
  active_jobs: Schema.optionalKey(Schema.Array(Schema.Unknown)),
  not_ready: Schema.optionalKey(Schema.NullOr(Schema.String)),
});
const POOL_PORTS: [number, number] = [18100, 18299];
const MEDIA = () => join(homedir(), ".local/bin/parish-media");

// awayWatts 0 by hand would read as "lend at 0 W" while the GPU's floor still lets it draw; treat it as the
// lowest cap, which the lever clamps to the GPU's minimum and says so.
const awayCap = (w: number | null | undefined): number | null => (w === null || w === undefined ? null : Math.max(1, w));

export const clusterSettings = (c: Config) => ({
  discover: c.cluster?.discover ?? false,
  lend: (c.cluster?.lend ?? "never") as LocalLendMode,
  ports: c.cluster?.ports?.length ? [...c.cluster.ports] : [18091],
  idleSeconds: c.cluster?.idleSeconds ?? 120,
  drainSeconds: c.cluster?.drainSeconds ?? 5,
  failoverMs: c.cluster?.failoverMs ?? 60_000,
  rpcPort: c.cluster?.rpcPort ?? 50052,
  rpcServer: c.cluster?.rpcServer ?? "rpc-server",
  llamaServer: c.cluster?.llamaServer ?? "llama-server",
  power: { activeWatts: Math.max(0, c.cluster?.power?.activeWatts ?? 0), awayWatts: awayCap(c.cluster?.power?.awayWatts) },
});

const proof = (key: string, nonce: string, id: string, target: string): string => createHmac("sha256", key).update(`${nonce}|${id}|${normUrl(target)}`).digest("hex");

const same = (a: string, b: string): boolean => {
  const [x, y] = [Buffer.from(a), Buffer.from(b)];
  return x.length === y.length && timingSafeEqual(x, y);
};

const plainIp = (ip: string | undefined): string => (ip ?? "").replace(/^::ffff:/, "");

const ppid = (pid: number): number | null => {
  try {
    const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
    return Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1]) || null;
  } catch {
    return null;
  }
};

const command = (pid: number): string => {
  try {
    return readFileSync(`/proc/${pid}/comm`, "utf8").trim();
  } catch {
    return "a program";
  }
};

const FLEET_ENV = "LOCAL_STUDIO_FLEET_OWNER";

const taggedFor = (pid: number, owner: string): boolean => {
  try {
    return readFileSync(`/proc/${pid}/environ`, "utf8").split("\0").includes(`${FLEET_ENV}=${owner}`);
  } catch {
    return false;
  }
};

const containerOf = (pid: number): string | null => {
  try {
    return /([0-9a-f]{64})/.exec(readFileSync(`/proc/${pid}/cgroup`, "utf8"))?.[1] ?? null;
  } catch {
    return null;
  }
};

const freePort = (): Promise<number> =>
  new Promise((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const a = s.address();
      const p = typeof a === "object" && a ? a.port : 0;
      s.close(() => resolve(p));
    });
  });

const accepts = (port: number): Promise<boolean> =>
  new Promise((resolve) => {
    const s = connect(port, "127.0.0.1");
    s.once("connect", () => {
      s.destroy();
      resolve(true);
    });
    s.once("error", () => resolve(false));
  });

const kill = async (proc: Bun.Subprocess) => {
  if (proc.exitCode !== null || proc.signalCode) return;
  proc.kill("SIGTERM");
  const done = await Promise.race([proc.exited.then(() => true), Bun.sleep(3_000).then(() => false)]);
  if (!done) proc.kill("SIGKILL");
};

const tail = (path: string): string => {
  try {
    return readFileSync(path, "utf8").trim().split("\n").slice(-3).join(" ").slice(-400);
  } catch {
    return "";
  }
};

const ggufs = (dirs: string[]): { path: string; name: string; sizeBytes: number }[] => {
  const out: { path: string; name: string; sizeBytes: number }[] = [];
  const walk = (dir: string, depth: number) => {
    if (depth > 6 || out.length >= 200) return;
    let entries: string[] = [];
    try {
      entries = readdirSync(dir);
    } catch {
      return;
    }
    for (const e of entries) {
      const p = join(dir, e);
      let st;
      try {
        st = statSync(p);
      } catch {
        continue;
      }
      if (st.isDirectory()) walk(p, depth + 1);
      else if (/\.gguf$/i.test(e) && !/mmproj/i.test(e) && !(/-\d{5}-of-\d{5}\.gguf$/i.test(e) && !/-00001-of-\d{5}\.gguf$/i.test(e))) {
        const parts = /-00001-of-(\d{5})\.gguf$/i.exec(e);
        let size = st.size;
        if (parts) for (let i = 2; i <= Number(parts[1]); i++) size += (() => { try { return statSync(p.replace(/-00001-of-/, `-${String(i).padStart(5, "0")}-of-`)).size; } catch { return 0; } })();
        out.push({ path: p, name: e.replace(/(-00001-of-\d{5})?\.gguf$/i, ""), sizeBytes: size });
      }
    }
  };
  for (const d of dirs) walk(d, 0);
  return out.sort((a, b) => a.name.localeCompare(b.name));
};

export const makeCluster = (d: ClusterDeps) => {
  let state: LocalLendState = "off";
  let reason = "Lending is off for this machine.";
  let base = reason;
  const off: Presence = { present: null, source: "not checked while lending is off", idleSeconds: null };
  let owner: Presence = off;
  let since = now();
  let checks: Check[] = [];
  let idleSince: number | null = null;
  let draining = false;
  let reclaiming: Promise<void> | null = null;
  let tailnetIp: string | null = null;
  let tailnetAt = 0;
  let models: { path: string; name: string; sizeBytes: number }[] = [];
  let modelsAt = 0;
  let containers = { at: 0, key: "", ids: new Set<string>() };
  const discovered = new Map<string, { id: string; name: string; url: string; at: string }>();
  const listen = new Set<string>();
  const rpcs = new Map<string, Rpc>();
  const pools = new Map<string, Pool>();
  const claims = new Set<{ ids: string[]; port: number }>();
  const set = () => clusterSettings(d.config());
  const lever = makePower({
    gpus: d.gpus,
    pids: () => [...rpcs.values()].flatMap((r) => (r.proc.exitCode === null && !r.proc.signalCode ? [r.proc.pid] : [])),
    containers: () => d.jobs.active().filter((j) => j.lent).map((j) => j.container),
  });
  void lever.recover();
  const fleetGpus = (): string[] => [...new Set([...[...rpcs.values()].flatMap((r) => r.gpuIds), ...d.jobs.active().filter((j) => j.lent).flatMap((j) => j.gpuUuids ?? [])])];

  const ip = async (): Promise<string | null> => {
    if (Date.now() - tailnetAt > 60_000) {
      tailnetIp = await Effect.runPromise(tailnetIPv4);
      tailnetAt = Date.now();
    }
    return tailnetIp;
  };

  const ownHosts = (): Set<string> => new Set([d.listenHost, ...(tailnetIp ? [tailnetIp] : []), URL.parse(d.config().url)?.hostname ?? ""].filter(Boolean));

  const spawnedPids = (): Set<number> => new Set([...[...rpcs.values()].map((r) => r.proc.pid), ...[...pools.values()].flatMap((p) => (p.proc ? [p.proc.pid] : []))]);

  const fleetContainers = async (miss: boolean): Promise<Set<string>> => {
    const names = d.jobs.active().filter((j) => j.lent).map((j) => j.container);
    if (!names.length) return new Set();
    const key = names.join(",");
    const age = Date.now() - containers.at;
    if (key !== containers.key || age > 30_000 || (miss && age > 2_000)) {
      const r = await Effect.runPromise(exec(["docker", "inspect", "-f", "{{.Id}}", ...names], 10_000));
      containers = { at: Date.now(), key, ids: new Set(r.stdout.split("\n").map((s) => s.trim()).filter((s) => /^[0-9a-f]{64}$/.test(s))) };
    }
    return containers.ids;
  };

  const isFleet = (pid: number, roots: Set<number>, ids: Set<string>): boolean => {
    const c = containerOf(pid);
    if (c && ids.has(c)) return true;
    if (taggedFor(pid, d.config().id)) return true;
    for (let p: number | null = pid, i = 0; p && p > 1 && i < 64; p = ppid(p), i++) if (roots.has(p)) return true;
    return false;
  };

  const media = async (fleet: (pid: number) => boolean): Promise<Check | null> => {
    if (!existsSync(MEDIA())) return null;
    const r = await Effect.runPromise(exec([MEDIA(), "status"], 10_000));
    const s = r.code === 0 ? decodeJson(MediaStatus, r.stdout) : undefined;
    if (!s) return { name: "parish-media", ok: false, detail: `parish-media status did not answer: ${r.stderr.trim().split("\n").pop() || r.code}` };
    const pids = (s.compute_pids ?? []).map(Number).filter((p) => p > 0);
    const ok = !s.not_ready || (s.ac_online !== false && !(s.active_jobs ?? []).length && pids.length > 0 && pids.every(fleet));
    return { name: "parish-media", ok, detail: ok ? "parish-media says the GPU is free." : `parish-media: ${s.not_ready}` };
  };

  const ownerCheck = async (): Promise<Check | null> => {
    owner = await Effect.runPromise(presence);
    if (owner.present === null) return null;
    const where = platform() === "darwin" && !process.env.LOCAL_STUDIO_SYSFS ? "this Mac" : "this machine";
    if (!owner.present) return { name: "Owner", ok: true, detail: owner.idleSeconds === null ? `Nobody is at ${where}'s screen.` : `Nobody has used ${where} for ${Math.floor(owner.idleSeconds / 60)} min.` };
    const w = set().power.activeWatts;
    return w > 0 ? { name: "Owner", ok: true, detail: `Someone is using ${where}, so it lends at up to ${w} W.` } : { name: "Owner", ok: false, detail: `Someone is using ${where}.` };
  };

  const capFor = (s: ReturnType<typeof set>): number | null => (owner.present === true ? (s.power.activeWatts > 0 ? s.power.activeWatts : null) : s.power.awayWatts);

  const lentReason = (s: ReturnType<typeof set>, head: string): string => {
    const cap = capFor(s);
    if (owner.present === true && cap !== null) return `In use, so it lends at up to ${cap} W.`;
    return cap === null ? `${head}.` : `${head}, at up to ${cap} W.`;
  };

  const evaluate = async (): Promise<Check[]> => {
    const gpus = d.gpus();
    const roots = spawnedPids();
    let ids = await fleetContainers(false);
    if (gpus.some((g) => g.pids.some((p) => { const c = containerOf(p); return !!c && !ids.has(c); }))) ids = await fleetContainers(true);
    const fleet = (pid: number) => isFleet(pid, roots, ids);
    const foreign = gpus.flatMap((g) => g.pids.filter((p) => !fleet(p)).map((p) => ({ g, p })));
    const busy = gpus.filter((g) => (g.utilization ?? 0) >= 10 && !g.pids.some(fleet));
    const power = await Effect.runPromise(powerSource);
    const out: Check[] = [
      { name: "GPU programs", ok: !foreign.length, detail: foreign.length ? `${command(foreign[0]!.p)} (pid ${foreign[0]!.p}) is using ${foreign[0]!.g.name}.` : "No programs outside the fleet are using the GPU." },
      { name: "GPU use", ok: !busy.length, detail: busy.length ? `${busy[0]!.name} is ${busy[0]!.utilization}% busy with work outside the fleet.` : "The GPU is not busy with other work." },
      { name: "Power", ok: power !== "battery", detail: power === "battery" ? "Running on battery." : "Plugged in." },
    ];
    const stale = gpus.find((g) => g.stale);
    if (stale) out.push({ name: "GPU readings", ok: false, detail: `A GPU fell off the bus, so nvidia-smi no longer reports ${stale.name}. This machine can't tell whether its owner is using it until it restarts.` });
    for (const extra of await Promise.all([media(fleet), ownerCheck()])) if (extra) out.push(extra);
    return out;
  };

  const hasGpu = () => d.gpus().length > 0 || (platform() === "darwin" && arch() === "arm64");

  const move = (next: LocalLendState, why: string) => {
    if (next !== state) since = now();
    const was = state;
    state = next;
    reason = why;
    base = why;
    if (was === "lent" && next !== "lent") void reclaim(why);
  };

  const tick = async () => {
    await decide();
    const s = set();
    await lever.set(state === "lent" ? capFor(s) : null, fleetGpus(), owner.present === false);
    const note = lever.note();
    if (note && (state === "lent" || /restore/.test(note))) reason = `${base} ${note}`;
  };

  const decide = async () => {
    const s = set();
    for (const r of [...rpcs.values()]) {
      if (r.conns.size) r.quietSince = null;
      else if (Date.now() - (r.quietSince ??= Date.now()) > RPC_QUIET_MS) {
        void post(r.forUrl, "/api/cluster/release", { controllerId: d.config().id, reason: `${d.config().name} stopped an rpc-server that nobody used for 2 minutes.` }, 3_000).catch(() => undefined);
        await stopRpc(r.id);
      }
    }
    if (s.lend !== "never") await ip();
    if (s.lend === "never") {
      checks = [];
      idleSince = null;
      owner = off;
      return move("off", "Lending is off for this machine.");
    }
    if (!hasGpu()) {
      checks = [];
      owner = off;
      return move("off", "This machine has no GPU to lend.");
    }
    if (s.lend === "always") {
      checks = await evaluate();
      const battery = checks.find((c) => c.name === "Power" && !c.ok);
      if (battery) return move("reclaimed", battery.detail);
      return move("lent", lentReason(s, "Lent to the fleet all the time"));
    }
    checks = await evaluate();
    const bad = checks.find((c) => !c.ok);
    if (bad) {
      idleSince = null;
      return move("reclaimed", bad.detail);
    }
    idleSince ??= Date.now();
    const idle = Math.floor((Date.now() - idleSince) / 1000);
    if (idle >= s.idleSeconds || state === "lent") return move("lent", lentReason(s, "Idle, so its GPU is lent to the fleet"));
    return move("reclaimed", `Idle for ${idle} s; it lends its GPU after ${s.idleSeconds} s idle.`);
  };

  const post = (url: string, path: string, body: unknown, timeoutMs: number) =>
    fetch(`${url}${path}`, { method: "POST", headers: { authorization: `Bearer ${d.config().fleetKey}`, "content-type": "application/json" }, body: JSON.stringify(body), signal: AbortSignal.timeout(timeoutMs), redirect: "manual" });

  const stopRpc = async (id: string) => {
    const r = rpcs.get(id);
    if (!r) return;
    rpcs.delete(id);
    lever.resume(r.proc.pid);
    for (const c of r.conns) c.destroy();
    r.gate.close();
    await kill(r.proc);
  };

  const reclaim = (why: string): Promise<void> => {
    reclaiming ??= (async () => {
      draining = true;
      try {
        await lever.set(null, []);
        await Promise.all([...rpcs.values()].map((r) => post(r.forUrl, "/api/cluster/release", { controllerId: d.config().id, reason: `${d.config().name} took its GPU back: ${why}` }, 3_000).catch(() => undefined)));
        const end = Date.now() + set().drainSeconds * 1000;
        while (Date.now() < end && (lentInFlight() > 0 || [...rpcs.values()].some((r) => r.conns.size > 0))) await Bun.sleep(200);
        await Promise.all([...rpcs.keys()].map(stopRpc));
        for (const j of d.jobs.active().filter((j) => j.lent)) await Effect.runPromise(Effect.result(d.jobs.stop(j.id, `${d.config().name} took its GPU back: ${why}`)));
      } finally {
        draining = false;
        reclaiming = null;
      }
    })();
    return reclaiming;
  };

  const startRpc = (from: { id: string; name: string; url: string }, peer: string | null) =>
    Effect.gen(function* () {
      const s = set();
      if (state !== "lent" || draining) return yield* fail(409, "NOT_LENDING", `${d.config().name} is not lending its GPU: ${reason}`);
      const borrower = plainIp(peer ?? "");
      if (!borrower) return yield* fail(400, "NO_PEER", "could not tell which machine asked for the GPU");
      const existing = [...rpcs.values()][0];
      if (existing && existing.forId === from.id) return { id: existing.id, endpoint: existing.endpoint };
      if (existing) return yield* fail(409, "RPC_BUSY", `${d.config().name} is already lending its GPU to ${existing.forName}.`);
      const bin = Bun.which(s.rpcServer);
      if (!bin) return yield* fail(409, "RPC_MISSING", `rpc-server from llama.cpp is not installed on ${d.config().name}.`);
      const host = yield* Effect.promise(ip);
      if (!host) return yield* fail(409, "NO_TAILNET", `${d.config().name} has no tailnet address, so it cannot lend its GPU safely.`);
      if (netOf(borrower) !== netOf(host)) return yield* fail(400, "NOT_TAILNET", `${from.name} reached ${d.config().name} at ${borrower}, which is not a tailnet address. Link it by its tailnet address to pool GPUs.`);
      const internal = yield* Effect.promise(freePort);
      const free = d.gpus().filter((g) => !g.pids.length);
      const gpus = free.map((g) => g.index);
      if (d.gpus().length && !gpus.length) return yield* fail(409, "GPU_BUSY", `Every GPU on ${d.config().name} is already in use.`);
      const id = randomBytes(5).toString("hex");
      const log = join(home, `rpc-${id}.log`);
      const fd = openSync(log, "w", 0o600);
      const proc = Bun.spawn([bin, "-H", "127.0.0.1", "-p", String(internal)], {
        stdin: "ignore", stdout: fd, stderr: fd,
        env: { ...process.env, [FLEET_ENV]: d.config().id, CUDA_DEVICE_ORDER: "PCI_BUS_ID", ...(d.gpus().length ? { CUDA_VISIBLE_DEVICES: gpus.join(",") } : {}) },
      });
      closeSync(fd);
      const ready = Date.now() + 15_000;
      while (Date.now() < ready && proc.exitCode === null && !(yield* Effect.promise(() => accepts(internal)))) yield* Effect.sleep("200 millis");
      if (proc.exitCode !== null || !(yield* Effect.promise(() => accepts(internal)))) {
        yield* Effect.promise(() => kill(proc));
        return yield* fail(500, "RPC_FAILED", `rpc-server did not start on ${d.config().name}: ${tail(log)}`);
      }
      const conns = new Set<Socket>();
      const gate = createServer({ pauseOnConnect: true }, (sock) => {
        if (plainIp(sock.remoteAddress) !== borrower) return sock.destroy();
        const up = connect(internal, "127.0.0.1");
        conns.add(sock);
        const close = () => {
          conns.delete(sock);
          sock.destroy();
          up.destroy();
        };
        up.on("error", close).on("close", close);
        sock.on("error", close).on("close", close);
        up.setNoDelay(true);
        sock.setNoDelay(true);
        sock.pipe(up).pipe(sock);
        sock.resume();
      });
      const bound = yield* Effect.promise(() => new Promise<string | null>((resolve) => {
        gate.once("error", (e) => resolve(String(e)));
        gate.listen(s.rpcPort, host, () => resolve(null));
      }));
      if (bound) {
        yield* Effect.promise(() => kill(proc));
        return yield* fail(409, "RPC_PORT", `port ${s.rpcPort} on ${host} is taken: ${bound}`);
      }
      if (state !== "lent" || draining) {
        gate.close();
        yield* Effect.promise(() => kill(proc));
        return yield* fail(409, "NOT_LENDING", `${d.config().name} took its GPU back: ${reason}`);
      }
      const back = URL.parse(from.url);
      const forUrl = `${back?.protocol ?? "http:"}//${borrower.includes(":") ? `[${borrower}]` : borrower}:${back?.port || "18091"}`;
      const r: Rpc = { id, proc, gate, conns, endpoint: `${host}:${s.rpcPort}`, forId: from.id, forName: from.name, forUrl, startedAt: now(), gpus, gpuIds: free.map((g) => g.uuid), internal, quietSince: Date.now() };
      rpcs.set(id, r);
      void proc.exited.then(() => {
        if (rpcs.get(id) === r) {
          rpcs.delete(id);
          gate.close();
          void post(r.forUrl, "/api/cluster/release", { controllerId: d.config().id, reason: `rpc-server on ${d.config().name} stopped: ${tail(log)}` }, 3_000).catch(() => undefined);
        }
      });
      return { id, endpoint: r.endpoint };
    });

  const stopRpcFor = (forId: string) =>
    Effect.promise(async () => {
      const ids = [...rpcs.values()].filter((r) => r.forId === forId).map((r) => r.id);
      await Promise.all(ids.map(stopRpc));
      return { stopped: ids };
    });

  const teardown = async (id: string, why: string | null) => {
    const p = pools.get(id);
    if (!p || p.info.phase === "stopped" || p.info.phase === "failed") return;
    p.info = { ...p.info, phase: why ? "failed" : "stopped", message: why };
    if (p.proc) await kill(p.proc);
    const me = d.config();
    await Promise.all([...p.urls.values()].map((u) => post(u, "/api/cluster/rpc/stop", { id: me.id, name: me.name, url: me.url }, 5_000).catch(() => undefined)));
  };

  const machines = async () => {
    const g = await d.graph();
    return g.nodes.flatMap((n) => (n.node.cluster ? [{ id: n.node.controller.id, name: n.node.controller.name, url: n.url, ...n.node.cluster }] : []));
  };

  const watchPools = async () => {
    const live = [...pools.values()].filter((p) => p.info.phase === "starting" || p.info.phase === "ready");
    if (!live.length) return;
    const g = await d.graph();
    for (const p of live) {
      for (const m of p.info.members) {
        const n = g.nodes.find((x) => x.node.controller.id === m.controllerId);
        const link = g.links.find((l) => l.id === m.controllerId);
        if (!n || !link?.reachable) await teardown(p.info.id, `${m.name} can no longer be reached.`);
        else if (n.node.cluster && n.node.cluster.state !== "lent") await teardown(p.info.id, `${m.name} took its GPU back: ${n.node.cluster.reason}`);
      }
    }
  };

  const launchPool = (req: LocalPoolRequest) =>
    Effect.gen(function* () {
      const s = set();
      const me = d.config();
      if (!/\.gguf$/i.test(req.model) || !existsSync(req.model)) return yield* fail(400, "MODEL_MISSING", `no GGUF model at ${req.model} on ${me.name}`);
      const bin = Bun.which(s.llamaServer);
      if (!bin) return yield* fail(409, "LLAMA_MISSING", `llama-server from llama.cpp is not installed on ${me.name}.`);
      const all = yield* Effect.promise(machines);
      const busy = new Set([...[...pools.values()].filter((p) => p.info.phase === "starting" || p.info.phase === "ready").flatMap((p) => p.info.members.map((m) => m.controllerId)), ...[...claims].flatMap((c) => c.ids)]);
      const lenders = all.filter((m) => m.state === "lent" && m.rpc && m.rpcEndpoint && (m.gpus === 0 || m.freeMiB > 0) && !busy.has(m.id) && (!req.members?.length || req.members.includes(m.id)));
      if (!lenders.length) {
        const why = all.map((m) => `${m.name}: ${busy.has(m.id) ? "already serves a pooled model from this machine; stop that one first" : m.reason}`).join("; ");
        return yield* fail(409, "NO_LENDERS", `No machine in the fleet is lending its GPU right now${why ? ` (${why})` : ""}.`);
      }
      const claim = { ids: lenders.map((m) => m.id), port: 0 };
      claims.add(claim);
      return yield* grant(req, bin, lenders, claim).pipe(Effect.ensuring(Effect.sync(() => claims.delete(claim))));
    });

  const grant = (req: LocalPoolRequest, bin: string, lenders: Awaited<ReturnType<typeof machines>>, claim: { ids: string[]; port: number }) =>
    Effect.gen(function* () {
      const me = d.config();
      const taken = yield* d.takenPorts();
      for (const p of pools.values()) if (p.info.phase === "starting" || p.info.phase === "ready") taken.add(p.info.port);
      for (const c of claims) taken.add(c.port);
      let port = POOL_PORTS[0];
      while (port <= POOL_PORTS[1] && taken.has(port)) port++;
      if (port > POOL_PORTS[1]) return yield* fail(503, "NO_PORT", "no free port in 18100-18299");
      claim.port = port;
      const alias = req.alias?.trim() || basename(req.model).replace(/(-00001-of-\d{5})?\.gguf$/i, "");
      const argvFor = (eps: string[]) => [bin, "-m", req.model, "--rpc", eps.join(","), "--host", "127.0.0.1", "--port", String(port), "-ngl", "999", "--metrics", "--alias", alias, ...(req.ctx ? ["-c", String(req.ctx)] : [])];
      if (req.dryRun) return { pool: null, argv: argvFor(lenders.map((m) => m.rpcEndpoint!)), members: lenders.map((m) => m.name) };
      const granted = yield* Effect.forEach(lenders, (m) =>
        Effect.promise(async () => {
          const r = await post(m.url, "/api/cluster/rpc", { id: me.id, name: me.name, url: me.url }, 30_000).catch((e) => String(e));
          if (typeof r === "string") return { m, error: r };
          const body = await r.json().catch(() => null);
          const ok = r.ok ? decodeRpc(body) : null;
          return ok?._tag === "Some" ? { m, endpoint: ok.value.endpoint } : { m, error: (body as { error?: { message?: string } } | null)?.error?.message ?? `HTTP ${r.status}` };
        }), { concurrency: 8 });
      const ok = granted.flatMap((g) => ("endpoint" in g && g.endpoint ? [{ m: g.m, endpoint: g.endpoint }] : []));
      for (const g of granted) if ("error" in g) void post(g.m.url, "/api/cluster/rpc/stop", { id: me.id, name: me.name, url: me.url }, 5_000).catch(() => undefined);
      if (!ok.length) return yield* fail(502, "RPC_UNAVAILABLE", granted.map((g) => `${g.m.name}: ${"error" in g ? g.error : ""}`).join("; "));
      const id = randomBytes(5).toString("hex");
      const argv = argvFor(ok.map((g) => g.endpoint));
      const log = join(home, `pool-${id}.log`);
      const fd = openSync(log, "w", 0o600);
      const proc = Bun.spawn(argv, { stdin: "ignore", stdout: fd, stderr: fd, env: { ...process.env, [FLEET_ENV]: me.id } });
      closeSync(fd);
      const info: LocalPool = { id, model: req.model, alias, port, members: ok.map((g) => ({ controllerId: g.m.id, name: g.m.name, endpoint: g.endpoint })), phase: "starting", message: null, argv, startedAt: now() };
      const pool: Pool = { info, proc, urls: new Map(ok.map((g) => [g.m.id, g.m.url])) };
      pools.set(id, pool);
      void proc.exited.then((code) => {
        if (pool.info.phase === "starting" || pool.info.phase === "ready") void teardown(id, `llama-server exited (${code}): ${tail(log)}`);
      });
      void (async () => {
        const end = Date.now() + 30 * 60_000;
        while (Date.now() < end && pool.info.phase === "starting") {
          const r = await Effect.runPromise(Effect.option(fetchJson(`http://127.0.0.1:${port}/v1/models`, 3_000)));
          if (r._tag === "Some" && r.value.status === 200 && pool.info.phase === "starting") pool.info = { ...pool.info, phase: "ready" };
          else await Bun.sleep(1_000);
        }
        if (pool.info.phase === "starting") await teardown(id, "llama-server did not become ready within 30 minutes.");
      })();
      return { pool: info, argv, members: info.members.map((m) => m.name) };
    });

  const discover = async () => {
    const s = set();
    if (!s.discover) return;
    const me = d.config();
    const peers = await Effect.runPromise(onlinePeers);
    if (!peers) return;
    await ip();
    const own = ownHosts();
    const ignored = new Set(me.cluster?.ignored ?? []);
    const known = new Set([...me.peers.map(normUrl), ...ignored]);
    const targets = [...new Set(peers.flatMap((p) => p.TailscaleIPs.filter((a) => !a.includes(":")).slice(0, 1)))]
      .flatMap((host) => s.ports.map((p) => `http://${host}:${p}`))
      .filter((u) => !known.has(u) && !(own.has(URL.parse(u)?.hostname ?? "") && URL.parse(u)?.port === String(d.port)));
    const found = await Promise.all(targets.map(async (url) => {
      const h = await Effect.runPromise(Effect.option(fetchJson(`${url}/api/health`, 1_500)));
      const health = h._tag === "Some" && h.value.status === 200 ? decodeHealth(h.value.body) : null;
      if (health?._tag !== "Some" || !health.value.cluster || health.value.id === me.id) return null;
      const nonce = randomBytes(24).toString("base64url");
      const r = await fetch(`${url}/api/cluster/hello`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ id: me.id, nonce, target: url }), signal: AbortSignal.timeout(3_000), redirect: "manual" }).catch(() => null);
      const hello = r?.ok ? decodeHello(await r.json().catch(() => null)) : null;
      if (hello?._tag !== "Some" || hello.value.id !== health.value.id || ignored.has(hello.value.id)) return null;
      return same(hello.value.proof, proof(me.fleetKey, nonce, hello.value.id, url)) ? { id: hello.value.id, name: hello.value.name, url, at: now() } : null;
    }));
    const fresh = found.filter((f): f is NonNullable<typeof f> => !!f);
    if (!fresh.length) return;
    for (const f of fresh) discovered.set(f.id, f);
    const c = d.config();
    d.save({ ...c, peers: [...new Set([...c.peers.map(normUrl), ...fresh.map((f) => f.url)])] });
    d.invalidate();
  };

  const hello = (req: { id: string; nonce: string; target: string }) =>
    Effect.gen(function* () {
      if (!set().discover) return yield* fail(404, "NOT_FOUND", "POST /api/cluster/hello");
      const me = d.config();
      const t = URL.parse(req.target);
      if (!t || t.port !== String(d.port) || !ownHosts().has(t.hostname) || req.nonce.length < 16 || req.nonce.length > 128 || req.id === me.id)
        return yield* fail(400, "BAD_HELLO", "that address is not this controller");
      return { id: me.id, name: me.name, proof: proof(me.fleetKey, req.nonce, me.id, req.target) };
    });

  const node = (): LocalNodeCluster => {
    const s = set();
    const free = d.gpus().filter((g) => !g.pids.length).reduce((n, g) => n + g.memoryTotalMiB - g.memoryUsedMiB, 0);
    return {
      discover: s.discover, lend: s.lend, state, reason, since,
      rpc: s.lend !== "never" && !!Bun.which(s.rpcServer),
      rpcEndpoint: tailnetIp ? `${tailnetIp}:${s.rpcPort}` : null,
      gpus: d.gpus().length,
      freeMiB: free,
    };
  };

  const view = async (): Promise<LocalCluster> => {
    if (Date.now() - modelsAt > 60_000) {
      models = ggufs([d.config().modelsDir, join(process.env.HF_HOME ?? join(homedir(), ".cache", "huggingface"), "hub")]);
      modelsAt = Date.now();
    }
    const ms = await machines();
    const one = Math.max(node().freeMiB, ...ms.map((m) => m.freeMiB)) * 2 ** 20;
    const pooled = (node().freeMiB + ms.filter((m) => m.state === "lent").reduce((n, m) => n + m.freeMiB, 0)) * 2 ** 20;
    const s = set();
    const p = lever.view(fleetGpus());
    return {
      ...node(),
      power: {
        activeWatts: s.power.activeWatts,
        awayWatts: s.power.awayWatts,
        present: owner.present,
        presenceSource: owner.source,
        capW: state === "lent" ? p.capW : null,
        lever: state === "lent" ? p.lever : "none",
        measured: p.measured,
        measuredW: state === "lent" ? p.measuredW : null,
        helper: lever.helper(),
        maxW: lever.maxW(),
        gpus: p.gpus,
      },
      draining,
      checks,
      discovered: [...discovered.values()],
      listen: [...listen],
      rpcServers: [...rpcs.values()].map((r) => ({ id: r.id, endpoint: r.endpoint, forId: r.forId, forName: r.forName, startedAt: r.startedAt, gpus: r.gpus })),
      pools: [...pools.values()].map((p) => p.info).sort((a, b) => b.startedAt.localeCompare(a.startedAt)),
      models: models.map((m) => ({ ...m, fitsOne: m.sizeBytes * 1.1 <= one, fitsPool: m.sizeBytes * 1.1 <= pooled })),
      machines: ms,
    };
  };

  const configure = (req: typeof LocalClusterRequest.Type) => {
    const c = d.config();
    const power = req.power ? { ...c.cluster?.power, ...req.power } : undefined;
    d.save({ ...c, cluster: { ...c.cluster, ...(req.discover === undefined ? {} : { discover: req.discover }), ...(req.lend ? { lend: req.lend } : {}), ...(power ? { power } : {}) } });
    if (req.power) lever.retry();
    if (req.lend) idleSince = null;
    return tick().then(() => node());
  };

  return {
    tick,
    discover,
    hello,
    node,
    view,
    configure,
    startRpc,
    stopRpcFor,
    launchPool,
    stopPool: (id: string) => Effect.gen(function* () {
      const p = pools.get(id);
      if (!p) return yield* fail(404, "POOL_NOT_FOUND", `no pooled model ${id}`);
      yield* Effect.promise(() => teardown(id, null));
      return p.info;
    }),
    release: (controllerId: string, why: string) => Effect.promise(async () => {
      const hit = [...pools.values()].filter((p) => p.info.members.some((m) => m.controllerId === controllerId) && (p.info.phase === "starting" || p.info.phase === "ready"));
      await Promise.all(hit.map((p) => teardown(p.info.id, why)));
      return { released: hit.map((p) => p.info.id) };
    }),
    watchPools,
    rpcPorts: () => [set().rpcPort, ...[...rpcs.values()].map((r) => r.internal)],
    shutdown: async () => {
      await lever.stop();
      await Promise.all([...pools.keys()].map((id) => teardown(id, null)));
      await Promise.all([...rpcs.keys()].map(stopRpc));
    },
    poolPorts: () => [...pools.values()].filter((p) => p.info.phase === "starting" || p.info.phase === "ready").map((p) => p.info.port),
    poolPort: (port: number) => [...pools.values()].find((p) => p.info.port === port && (p.info.phase === "starting" || p.info.phase === "ready"))?.info.id ?? null,
    gpuGone: async (uuid: string, why: string) => {
      for (const r of [...rpcs.values()].filter((r) => r.gpuIds.includes(uuid))) {
        await post(r.forUrl, "/api/cluster/release", { controllerId: d.config().id, reason: why }, 3_000).catch(() => undefined);
        await stopRpc(r.id);
      }
    },
    refuse: (ep: Endpoint): string | null => (ep.lent && (draining || (state !== "lent" && set().lend !== "never")) ? `${d.config().name} took its GPU back: ${reason}` : null),
    tailnet: ip,
    ownHosts,
    listening: (url: string, on: boolean) => (on ? listen.add(url) : listen.delete(url)),
    failoverMs: () => set().failoverMs,
    state: () => state,
  };
};
