import { existsSync, readFileSync, realpathSync, rmSync } from "node:fs";
import { arch, platform } from "node:os";
import { join } from "node:path";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import type { LocalPowerLever } from "./contracts.ts";
import { decodeJson, exec, home, writeJson } from "./core.ts";
import type { RawGpu } from "./hardware.ts";

const PERIOD_MS = 200;
const CONTAINER_TICKS = 10;
const APPLE_GPU_W = 30;
const MIN_FRACTION = 0.1;
// How long a cached helper check or a failed sudo stays trusted. The smoke test shortens it.
const HELPER_TTL_MS = Number(process.env.LOCAL_STUDIO_TEST_POWER_TTL_MS) || 60_000;
const LIMITS_TTL_MS = 30_000;
// A GPU with no adjustable limit and no readable draw gets a fixed share of this nominal wattage.
const NOMINAL_GPU_W = 150;
const UNMEASURED_AFTER_MS = 2_000;

const Restore = Schema.Record(Schema.String, Schema.Struct({ index: Schema.Number, originalW: Schema.Number }));
type Restore = Record<string, { index: number; originalW: number }>;

export const restorePath = () => join(home, "power-restore.json");
export const pausedPath = () => join(home, "power-paused.json");

// What the duty cycle may have left stopped or paused if the controller dies mid-cycle. Each pid carries
// its command name, so a reused pid that now belongs to something else is never continued.
const Paused = Schema.Struct({ pids: Schema.Array(Schema.Struct({ pid: Schema.Number, comm: Schema.String })), containers: Schema.Array(Schema.String) });

const commOf = (pid: number): string | null => {
  try {
    if (platform() === "linux") return readFileSync(`/proc/${pid}/comm`, "utf8").trim() || null;
    const r = Bun.spawnSync(["ps", "-o", "comm=", "-p", String(pid)], { stdout: "pipe", stderr: "ignore" });
    return r.exitCode === 0 ? r.stdout.toString().trim().split("/").pop() || null : null;
  } catch {
    return null;
  }
};

const readRestore = (): Restore => {
  try {
    return { ...decodeJson(Restore, readFileSync(restorePath(), "utf8")) };
  } catch {
    return {};
  }
};

const saveRestore = (r: Restore) => {
  if (Object.keys(r).length) writeJson(restorePath(), r);
  else if (existsSync(restorePath())) rmSync(restorePath(), { force: true });
};

interface Limits {
  uuid: string;
  index: number;
  limitW: number | null;
  minW: number | null;
  maxW: number | null;
}

interface GpuLever {
  uuid: string;
  index: number;
  lever: LocalPowerLever;
  limitW: number | null;
  originalW: number | null;
}

export interface PowerDeps {
  gpus: () => RawGpu[];
  pids: () => number[];
  containers: () => string[];
}

const num = (s: string | undefined): number | null => {
  const n = Number.parseFloat(s ?? "");
  return Number.isFinite(n) ? n : null;
};

const watts = (w: number): string => (Number.isInteger(w) ? String(w) : w.toFixed(2));

const clamp = (x: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, x));

// The resolved path, because the sudoers rule names the real file (see `local-studio fleet power-helper`).
const nvsmi = (): string | null => {
  const found = Bun.which("nvidia-smi");
  if (!found) return null;
  try {
    return realpathSync(found);
  } catch {
    return found;
  }
};

const queryLimits = async (): Promise<Limits[] | null> => {
  const bin = nvsmi();
  if (!bin) return null;
  const r = await Effect.runPromise(exec([bin, "--query-gpu=uuid,index,power.limit,power.min_limit,power.max_limit", "--format=csv,noheader,nounits"], 8_000));
  if (r.code !== 0) return null;
  return r.stdout.split("\n").map((l) => l.split(",").map((s) => s.trim())).filter(([uuid]) => uuid?.startsWith("GPU-")).map(([uuid = "", index = "", limit, min, max]) => ({
    uuid, index: Number(index) || 0, limitW: num(limit), minW: num(min), maxW: num(max),
  }));
};

const setLimit = async (index: number, w: number): Promise<string | null> => {
  const bin = nvsmi();
  if (!bin) return "nvidia-smi is not installed";
  const r = await Effect.runPromise(exec(["sudo", "-n", bin, "-i", String(index), "-pl", watts(w)], 20_000));
  return r.code === 0 ? null : `${r.stderr}\n${r.stdout}`.trim() || `exit ${r.code}`;
};

const sudoNote = (why: string): string =>
  /password|a terminal is required|not allowed|may not run/i.test(why)
    ? 'sudo needs a password to set the NVIDIA power limit, so it pauses fleet work in short bursts instead. Run "local-studio fleet power-helper" to let it set the limit.'
    : `Setting the NVIDIA power limit failed (${why.split("\n").pop()}), so it pauses fleet work in short bursts instead.`;

export const makePower = (d: PowerDeps) => {
  let cap: number | null = null;
  let note: string | null = null;
  let sudoFailed: { why: string; at: number } | null = null;
  let limitsCache: { at: number; value: Limits[] | null } = { at: 0, value: null };
  let helperCache: { at: number; value: "installed" | "missing" | "unsupported" } | null = null;
  let helperBusy = false;
  const perGpu = new Map<string, GpuLever>();
  let chain: Promise<unknown> = Promise.resolve();

  let duty = false;
  let dutyCap = 0;
  let dutyUuids: string[] = [];
  let dutySource: "nvidia" | "macmon" | "fixed" = "fixed";
  let dutyNominal = NOMINAL_GPU_W;
  let dutyAt = 0;
  let unmeasured = false;
  let pausedKey = "";
  let fraction = 1;
  let looping: Promise<void> | null = null;
  const stopped = new Set<number>();
  const paused = new Set<string>();
  const docker = new Set<Promise<unknown>>();
  let stream: Bun.Subprocess | null = null;
  const draws = new Map<string, number>();
  let drawAt = 0;

  const serial = <A>(f: () => Promise<A>): Promise<A> => {
    const next = chain.then(f, f);
    chain = next.catch(() => undefined);
    return next;
  };

  const limits = async (fresh = false) => {
    if (fresh || Date.now() - limitsCache.at > LIMITS_TTL_MS) limitsCache = { at: Date.now(), value: await queryLimits() };
    return limitsCache.value;
  };

  const signal = (pid: number, sig: "SIGSTOP" | "SIGCONT") => {
    try {
      process.kill(pid, sig);
      return true;
    } catch {
      return false;
    }
  };

  const contAll = () => {
    for (const pid of stopped) signal(pid, "SIGCONT");
    stopped.clear();
  };

  const dockerCall = (verb: "pause" | "unpause", name: string) => {
    const p = Effect.runPromise(exec(["docker", verb, name], 15_000));
    docker.add(p);
    void p.finally(() => docker.delete(p));
    return p;
  };

  const measuredNow = (): number | null => {
    if (Date.now() - drawAt > 1_500) return null;
    if (dutySource === "macmon") return draws.get("apple") ?? null;
    const vals = dutyUuids.flatMap((u) => (draws.has(u) ? [draws.get(u)!] : []));
    return vals.length ? vals.reduce((a, b) => a + b, 0) : null;
  };

  const readLines = async (proc: Bun.Subprocess, onLine: (l: string) => void) => {
    const out = proc.stdout;
    if (!out || typeof out === "number") return;
    const decoder = new TextDecoder();
    let buf = "";
    for await (const chunk of out as ReadableStream<Uint8Array>) {
      buf += decoder.decode(chunk, { stream: true });
      let i = buf.indexOf("\n");
      while (i >= 0) {
        onLine(buf.slice(0, i));
        buf = buf.slice(i + 1);
        i = buf.indexOf("\n");
      }
    }
  };

  const startStream = () => {
    if (stream || dutySource === "fixed") return;
    const argv = dutySource === "nvidia"
      ? [nvsmi() ?? "nvidia-smi", "--query-gpu=uuid,power.draw", "--format=csv,noheader,nounits", `-lms`, String(PERIOD_MS)]
      : ["macmon", "pipe", "-i", String(PERIOD_MS)];
    try {
      stream = Bun.spawn(argv, { stdin: "ignore", stdout: "pipe", stderr: "ignore" });
    } catch {
      stream = null;
      return;
    }
    const proc = stream;
    void readLines(proc, (line) => {
      if (dutySource === "nvidia") {
        const [uuid = "", w] = line.split(",").map((s) => s.trim());
        const v = num(w);
        if (uuid.startsWith("GPU-") && v !== null) {
          draws.set(uuid, v);
          drawAt = Date.now();
        }
      } else {
        let v: number | null = null;
        try {
          v = num(String((JSON.parse(line.trim() || "{}") as { gpu_power?: unknown }).gpu_power ?? ""));
        } catch {
          v = null; // a banner or partial line, not a reading
        }
        if (v !== null) {
          draws.set("apple", v);
          drawAt = Date.now();
        }
      }
    }).catch(() => undefined).finally(() => {
      if (stream === proc) stream = null;
      if (proc.exitCode === null) proc.kill("SIGTERM");
    });
  };

  const stopStream = () => {
    const s = stream;
    stream = null;
    if (s && s.exitCode === null) s.kill("SIGTERM");
    draws.clear();
    drawAt = 0;
  };

  const loop = async () => {
    let tick = 0;
    while (duty) {
      if (dutySource === "fixed") fraction = clamp(dutyCap / APPLE_GPU_W, MIN_FRACTION, 1);
      else {
        const m = measuredNow();
        if (m !== null) {
          unmeasured = false;
          fraction = clamp(fraction + 0.3 * fraction * (dutyCap - m) / Math.max(m, 1), MIN_FRACTION, 1);
        } else if (Date.now() - dutyAt > UNMEASURED_AFTER_MS) {
          unmeasured = true;
          fraction = clamp(dutyCap / dutyNominal, MIN_FRACTION, 1);
        }
      }
      const f = fraction;
      contAll();
      const slot = tick % CONTAINER_TICKS;
      const runTicks = Math.max(1, Math.round(CONTAINER_TICKS * f));
      const names = d.containers();
      if (f < 0.999) record(d.pids(), names);
      if (slot === 0) for (const n of [...paused]) {
        paused.delete(n);
        void dockerCall("unpause", n);
      }
      if (slot === runTicks && runTicks < CONTAINER_TICKS) for (const n of names) if (!paused.has(n)) {
        paused.add(n);
        void dockerCall("pause", n);
      }
      tick++;
      const on = Math.round(PERIOD_MS * f);
      await Bun.sleep(on);
      if (!duty) break;
      if (f < 0.999) {
        for (const pid of d.pids()) if (signal(pid, "SIGSTOP")) stopped.add(pid);
        await Bun.sleep(PERIOD_MS - on);
      }
    }
    contAll();
  };

  const record = (pids: number[], containers: string[]) => {
    const key = `${pids.join(",")}|${containers.join(",")}`;
    if (key === pausedKey) return;
    pausedKey = key;
    const list = pids.flatMap((pid) => {
      const comm = commOf(pid);
      return comm ? [{ pid, comm }] : [];
    });
    if (list.length || containers.length) writeJson(pausedPath(), { pids: list, containers });
    else rmSync(pausedPath(), { force: true });
  };

  const continueLeftovers = async () => {
    if (!existsSync(pausedPath())) return;
    let rec: typeof Paused.Type | undefined;
    try {
      rec = decodeJson(Paused, readFileSync(pausedPath(), "utf8"));
    } catch {
      rec = undefined;
    }
    for (const { pid, comm } of rec?.pids ?? []) if (commOf(pid) === comm) signal(pid, "SIGCONT");
    await Promise.all((rec?.containers ?? []).map((n) => Effect.runPromise(exec(["docker", "unpause", n], 15_000))));
    rmSync(pausedPath(), { force: true });
  };

  const startDuty = (share: number, uuids: string[], source: "nvidia" | "macmon" | "fixed", nominal = NOMINAL_GPU_W) => {
    dutyCap = share;
    dutyUuids = uuids;
    dutyNominal = Math.max(1, nominal);
    if (duty && dutySource !== source) stopStream();
    dutySource = source;
    startStream();
    if (!duty) {
      duty = true;
      dutyAt = Date.now();
      unmeasured = false;
      fraction = source === "fixed" ? clamp(share / APPLE_GPU_W, MIN_FRACTION, 1) : 1;
      looping = loop();
    }
  };

  const stopDuty = async () => {
    duty = false;
    contAll();
    await looping;
    looping = null;
    contAll();
    await Promise.all([...docker]);
    await Promise.all([...paused].map((n) => dockerCall("unpause", n)));
    paused.clear();
    stopStream();
    unmeasured = false;
    if (pausedKey) {
      pausedKey = "";
      rmSync(pausedPath(), { force: true });
    }
  };

  const restoreLimits = async (): Promise<string | null> => {
    const rec = readRestore();
    const uuids = Object.keys(rec);
    if (!uuids.length) return null;
    const now = await limits(true);
    const problems: string[] = [];
    for (const uuid of uuids) {
      const r = rec[uuid]!;
      const index = now ? now.find((l) => l.uuid === uuid)?.index : r.index;
      if (index === undefined) continue;
      const failed = await setLimit(index, r.originalW);
      if (failed) problems.push(`GPU ${index}: ${failed.split("\n").pop()}`);
      else delete rec[uuid];
    }
    saveRestore(rec);
    for (const [uuid, g] of perGpu) if (g.lever === "nvidia-limit" && !rec[uuid]) perGpu.delete(uuid);
    limitsCache.at = 0;
    return problems.length ? `Could not restore the original NVIDIA power limit (${problems.join("; ")}); it stays recorded in ${restorePath()}.` : null;
  };

  const release = async () => {
    cap = null;
    await stopDuty();
    note = await restoreLimits();
    perGpu.clear();
  };

  // `away` is true only when presence was read and nobody is there. An NVIDIA power limit is board-wide: set
  // while the owner is present (or presence is unknown) it would throttle their own games and apps too, so then
  // only the duty cycle, which pauses nothing but the processes the controller started, holds the cap.
  const apply = async (want: number, fleetUuids: string[], away: boolean) => {
    cap = want;
    // Nothing borrowed yet: no lever. Limiting idle GPUs would only slow the owner's own work.
    if (!fleetUuids.length && !d.pids().length && !d.containers().length) {
      await stopDuty();
      note = await restoreLimits();
      perGpu.clear();
      return;
    }
    const nv = d.gpus().filter((g) => !g.stale);
    if (!nv.length) {
      perGpu.clear();
      if (platform() === "darwin" && arch() === "arm64" && Bun.which("macmon")) {
        note = null;
        startDuty(want, [], "macmon");
      } else {
        const pct = Math.round(clamp(want / APPLE_GPU_W, MIN_FRACTION, 1) * 100);
        note = `It can't read this machine's power draw (macmon is not installed), so it runs fleet work ${pct}% of the time instead.`;
        startDuty(want, [], "fixed");
      }
      return;
    }
    if (sudoFailed && Date.now() - sudoFailed.at > HELPER_TTL_MS) sudoFailed = null;
    const targets = nv.filter((g) => !fleetUuids.length || fleetUuids.includes(g.uuid));
    const list = targets.length ? targets : nv;
    const each = Math.floor(want / list.length);
    const lim = await limits();
    const nominal = (g: RawGpu) => {
      const l = lim?.find((x) => x.uuid === g.uuid);
      return l?.limitW ?? l?.maxW ?? NOMINAL_GPU_W;
    };
    const notes: string[] = [];
    const dutyFor: RawGpu[] = [];
    for (const g of list) {
      const l = lim?.find((x) => x.uuid === g.uuid);
      if (!l || l.limitW === null || l.minW === null || l.maxW === null) {
        dutyFor.push(g);
        notes.push(`${g.name} does not report an adjustable power limit, so it pauses fleet work in short bursts instead.`);
        continue;
      }
      if (!away) {
        dutyFor.push(g);
        continue;
      }
      const w = clamp(each, Math.ceil(l.minW), Math.floor(l.maxW));
      const floor = each < l.minW ? `${g.name} can't go below ${watts(l.minW)} W.` : null;
      const had = perGpu.get(g.uuid);
      if (had?.lever === "nvidia-limit" && had.limitW === w) {
        if (floor) notes.push(floor);
        continue;
      }
      // sudo failed lately: a GPU already held at a limit stays there rather than being capped twice.
      if (sudoFailed) {
        if (had?.lever !== "nvidia-limit") dutyFor.push(g);
        continue;
      }
      const rec = readRestore();
      const fresh = !rec[g.uuid];
      if (fresh) {
        // Read the limit now, not from the cache: a cached value may be a cap this controller set.
        const now = (await limits(true))?.find((x) => x.uuid === g.uuid)?.limitW ?? l.limitW;
        rec[g.uuid] = { index: l.index, originalW: now };
        saveRestore(rec);
      }
      const failed = await setLimit(l.index, w);
      if (failed) {
        sudoFailed = { why: failed, at: Date.now() };
        if (fresh && had?.lever !== "nvidia-limit") {
          delete rec[g.uuid];
          saveRestore(rec);
        }
        if (had?.lever !== "nvidia-limit") dutyFor.push(g);
        continue;
      }
      limitsCache.at = 0;
      if (floor) notes.push(floor);
      perGpu.set(g.uuid, { uuid: g.uuid, index: l.index, lever: "nvidia-limit", limitW: w, originalW: rec[g.uuid]!.originalW });
    }
    if (sudoFailed && dutyFor.length && away) notes.unshift(sudoNote(sudoFailed.why));
    // Every recorded GPU that is not held at a limit right now gets its own limit back: one that left fleet
    // work, or one that moved to the duty cycle because the owner came back.
    const held = new Set(list.filter((g) => !dutyFor.includes(g) && perGpu.get(g.uuid)?.lever === "nvidia-limit").map((g) => g.uuid));
    const rec = readRestore();
    for (const [uuid, r] of Object.entries(rec)) {
      if (held.has(uuid)) continue;
      const index = nv.find((g) => g.uuid === uuid)?.index ?? r.index;
      const failed = await setLimit(index, r.originalW);
      if (failed) notes.push(`Could not restore GPU ${index}'s original power limit; it stays recorded in ${restorePath()}.`);
      else {
        delete rec[uuid];
        limitsCache.at = 0;
      }
    }
    saveRestore(rec);
    for (const uuid of [...perGpu.keys()]) if (!held.has(uuid)) perGpu.delete(uuid);
    for (const g of dutyFor) perGpu.set(g.uuid, { uuid: g.uuid, index: g.index, lever: "duty-cycle", limitW: null, originalW: null });
    note = notes.length ? [...new Set(notes)].join(" ") : null;
    if (dutyFor.length) startDuty(each * dutyFor.length, dutyFor.map((g) => g.uuid), "nvidia", dutyFor.reduce((n, g) => n + nominal(g), 0));
    else await stopDuty();
  };

  const refreshHelper = async () => {
    if (helperBusy) return;
    helperBusy = true;
    try {
      const bin = nvsmi();
      const has = d.gpus().length > 0 && !!bin;
      if (platform() !== "linux" || !has) {
        helperCache = { at: Date.now(), value: "unsupported" };
        return;
      }
      const r = await Effect.runPromise(exec(["sudo", "-n", "-l", bin!, "-i", "0", "-pl", "100"], 10_000));
      const value = r.code === 0 ? "installed" : "missing";
      // The helper was just installed: try the NVIDIA limit again on the next tick instead of duty-cycling on.
      if (value === "installed" && helperCache?.value !== "installed") sudoFailed = null;
      helperCache = { at: Date.now(), value };
    } finally {
      helperBusy = false;
    }
  };

  const helper = (): "installed" | "missing" | "unsupported" => {
    if (!helperCache || Date.now() - helperCache.at > HELPER_TTL_MS) void refreshHelper();
    return helperCache?.value ?? (d.gpus().length && platform() === "linux" ? "missing" : "unsupported");
  };

  const overall = (): LocalPowerLever => {
    if (cap === null) return "none";
    if (duty) return "duty-cycle";
    return [...perGpu.values()].some((g) => g.lever === "nvidia-limit") ? "nvidia-limit" : "none";
  };

  return {
    set: (want: number | null, fleetUuids: string[], away = false) => serial(() => (want === null ? (cap === null && !duty && !Object.keys(readRestore()).length ? Promise.resolve() : release()) : apply(want, fleetUuids, away))),
    stop: () => serial(release),
    recover: () => serial(async () => {
      await continueLeftovers();
      note = await restoreLimits();
    }),
    retry: () => {
      sudoFailed = null;
      helperCache = null;
    },
    resume: (pid: number) => {
      if (stopped.delete(pid)) signal(pid, "SIGCONT");
    },
    note: () => {
      if (!(duty && unmeasured)) return note;
      const pct = Math.round(fraction * 100);
      const why = `It can't read the GPU's power draw, so it runs fleet work ${pct}% of the time instead.`;
      return note ? `${note} ${why}` : why;
    },
    helper,
    // The most the fleet could draw here: the sum of each NVIDIA GPU's max_limit, or a nominal Apple GPU.
    // The app uses it as the top of its power sliders.
    maxW: (): number | null => {
      const nv = d.gpus().filter((g) => !g.stale);
      if (!nv.length) return platform() === "darwin" && arch() === "arm64" ? APPLE_GPU_W : null;
      if (Date.now() - limitsCache.at > LIMITS_TTL_MS) void serial(() => limits());
      const max = nv.flatMap((g) => {
        const w = limitsCache.value?.find((l) => l.uuid === g.uuid)?.maxW;
        return w ? [w] : [];
      });
      return max.length ? max.reduce((a, b) => a + b, 0) : null;
    },
    view: (fleetUuids: string[]) => {
      const m = duty ? measuredNow() : null;
      const gpus = d.gpus().filter((g) => !fleetUuids.length || fleetUuids.includes(g.uuid));
      const draw = gpus.flatMap((g) => (g.powerW === null ? [] : [g.powerW]));
      return {
        capW: cap,
        lever: overall(),
        measured: !(duty && (dutySource === "fixed" || unmeasured)) && (d.gpus().length > 0 || dutySource === "macmon"),
        measuredW: m ?? (draw.length ? Math.round(draw.reduce((a, b) => a + b, 0) * 10) / 10 : null),
        gpus: [...perGpu.values()],
      };
    },
  };
};
