import { readdirSync, readFileSync, realpathSync } from "node:fs";
import { arch, cpus, freemem, platform, totalmem } from "node:os";
import { basename, dirname, join } from "node:path";
import * as Effect from "effect/Effect";
import type { LocalGpu, LocalHardware, LocalHotplug, LocalPcieLink, LocalThunderbolt } from "./contracts.ts";
import { exec, fail, lastLine } from "./core.ts";

export type RawGpu = Omit<LocalGpu, "card"> & { uuid: string; pids: number[] };

const QUERY = "index,uuid,name,memory.used,memory.total,power.draw,power.limit,temperature.gpu,utilization.gpu,pci.bus_id,pcie.link.gen.current,pcie.link.gen.max,pcie.link.width.current,pcie.link.width.max";
const LANE_GBPS: Record<number, number> = { 1: 0.25, 2: 0.5, 3: 0.985, 4: 1.969, 5: 3.938, 6: 7.563 };
const sysfs = () => process.env.LOCAL_STUDIO_SYSFS ?? "/sys";

const reading = (s: string | undefined): number | null => {
  const n = Number.parseFloat(s ?? "");
  return Number.isFinite(n) ? n : null;
};

const read = (path: string): string | null => {
  try {
    return readFileSync(path, "utf8").trim();
  } catch {
    return null;
  }
};

const real = (path: string): string | null => {
  try {
    return realpathSync(path);
  } catch {
    return null;
  }
};

const list = (dir: string): string[] => {
  try {
    return readdirSync(dir);
  } catch {
    return [];
  }
};

const PCI_DIR = /^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$/;

const pciId = (busId: string): string | null => {
  const m = /^([0-9a-f]+):([0-9a-f]{2}):([0-9a-f]{2})\.([0-7])$/i.exec(busId.trim());
  return m ? `${(m[1] ?? "").slice(-4).padStart(4, "0")}:${m[2]}:${m[3]}.${m[4]}`.toLowerCase() : null;
};

const thunderboltBridges = (): string[] =>
  list(join(sysfs(), "bus/thunderbolt/devices"))
    .filter((d) => /^domain\d+$/.test(d))
    .flatMap((d) => {
      const domain = real(join(sysfs(), "bus/thunderbolt/devices", d));
      const port = domain ? dirname(dirname(domain)) : null;
      const up = port && PCI_DIR.test(basename(port)) ? dirname(port) : null;
      return up && PCI_DIR.test(basename(up)) ? [up] : [];
    });

export const isExternal = (busId: string): boolean => {
  const id = pciId(busId);
  const path = id ? real(join(sysfs(), "bus/pci/devices", id)) : null;
  if (!path) return false;
  for (let p = path; PCI_DIR.test(basename(p)); p = dirname(p)) {
    if (read(join(p, "removable")) === "removable") return true;
  }
  return thunderboltBridges().some((b) => path.startsWith(`${b}/`));
};

export const pcieLink = (gen: number | null, width: number | null, maxGen: number | null, maxWidth: number | null, external: boolean): LocalPcieLink | null => {
  const g = maxGen ?? gen;
  const w = maxWidth ?? width;
  if (!g || !w) return null;
  const gbps = Math.round((LANE_GBPS[g] ?? LANE_GBPS[6] ?? 0) * w * 10) / 10;
  const narrow = width && maxWidth && width < maxWidth ? `, running at x${width}` : "";
  const fast = external
    ? `about ${gbps} GB/s over the cable${narrow}. Loading a model takes longer than on an internal slot; once it is loaded, decoding speed is nearly the same.`
    : `about ${gbps} GB/s${narrow}.`;
  return { gen, width, maxGen, maxWidth, gbps, note: `PCIe ${g}.0 x${w}, ${fast}` };
};

export const readGpuList: Effect.Effect<RawGpu[] | null> = Effect.gen(function* () {
  const q = yield* exec(["nvidia-smi", `--query-gpu=${QUERY}`, "--format=csv,noheader,nounits"], 8_000);
  const rows = q.stdout.split("\n").filter((l) => /^\s*\d+\s*,\s*GPU-/.test(l));
  const said = `${q.stdout}${q.stderr}`;
  if (q.code !== 0 && !rows.length && !/GPU is lost|Unable to determine the device handle|fallen off the bus/i.test(said)) return /no devices were found/i.test(said) ? [] : null;
  const apps = (yield* exec(["nvidia-smi", "--query-compute-apps=gpu_uuid,pid", "--format=csv,noheader"], 8_000)).stdout
    .split("\n")
    .map((l) => l.split(",").map((s) => s.trim()))
    .filter(([uuid]) => uuid);
  return rows.map((l) => {
    const [index = "", uuid = "", name = "", used = "", total = "", power = "", limit = "", temp = "", util = "", bus = "", gen = "", maxGen = "", width = "", maxWidth = ""] = l.split(",").map((s) => s.trim());
    const pids = apps.filter(([u]) => u === uuid).flatMap(([, pid]) => (Number(pid) > 0 ? [Number(pid)] : []));
    const busId = pciId(bus) ? bus : null;
    const external = busId ? isExternal(busId) : false;
    return {
      index: Number(index) || 0, uuid, name, memoryUsedMiB: Number(used) || 0, memoryTotalMiB: Number(total) || 0,
      powerW: reading(power), powerLimitW: reading(limit), temperatureC: reading(temp), utilization: reading(util),
      busy: pids.length > 0 || Number(used) > 2048,
      pids, busId, external,
      link: pcieLink(reading(gen), reading(width), reading(maxGen), reading(maxWidth), external),
    };
  });
});

export const readGpus: Effect.Effect<RawGpu[]> = Effect.map(readGpuList, (g) => g ?? []);

const boltDevices = (out: string): LocalThunderbolt[] => {
  const blocks: Record<string, string>[] = [];
  for (const line of out.split("\n")) {
    const head = /^\s*[●○*o]\s+(.+?)\s*$/.exec(line);
    if (head) {
      blocks.push({ title: head[1] ?? "" });
      continue;
    }
    const kv = /^[\s│├└─|`-]*([a-z-]+):\s+(.*?)\s*$/.exec(line);
    const last = blocks.at(-1);
    if (kv && last && !(kv[1]! in last)) last[kv[1]!] = kv[2] ?? "";
  }
  return blocks.flatMap((b) => {
    if (!b.uuid || b.type === "host") return [];
    const status = b.status ?? "";
    const state = status === "authorized" ? "authorized" : status === "authorizing" ? "authorizing" : status === "auth-error" ? "failed" : status === "connected" ? "pending" : null;
    return state ? [{ uuid: b.uuid, name: b.name || b.title || "Thunderbolt device", vendor: b.vendor || null, generation: b.generation || null, state, message: state === "failed" ? "Approving this device failed. Unplug it, plug it back in and try again." : null }] : [];
  });
};

const sysfsDevices = (): LocalThunderbolt[] =>
  list(join(sysfs(), "bus/thunderbolt/devices")).flatMap((d) => {
    const dir = join(sysfs(), "bus/thunderbolt/devices", d);
    const uuid = read(join(dir, "unique_id"));
    const authorized = read(join(dir, "authorized"));
    if (!uuid || authorized === null || /^domain|-0$/.test(d)) return [];
    return [{ uuid, name: read(join(dir, "device_name")) ?? "Thunderbolt device", vendor: read(join(dir, "vendor_name")), generation: read(join(dir, "generation")) ? `Thunderbolt ${read(join(dir, "generation"))}` : null, state: authorized === "0" ? "pending" : "authorized", message: null }] as LocalThunderbolt[];
  });

export const readThunderbolt: Effect.Effect<LocalThunderbolt[]> = Effect.gen(function* () {
  if (platform() !== "linux" && !process.env.LOCAL_STUDIO_SYSFS) return [];
  if (Bun.which("boltctl")) {
    const r = yield* exec(["boltctl", "list", "--all"], 5_000);
    if (r.code === 0) return boltDevices(r.stdout);
  }
  return sysfsDevices();
});

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export const authorize = (uuid: string) =>
  Effect.gen(function* () {
    if (!UUID.test(uuid)) return yield* fail(400, "BAD_UUID", "that is not a Thunderbolt device id");
    if (!Bun.which("boltctl")) return yield* fail(409, "BOLT_MISSING", "boltctl is not installed. Install the bolt package, or approve the device in your system's Thunderbolt settings.");
    const r = yield* exec(["boltctl", "enroll", "--policy", "auto", uuid], 30_000);
    if (r.code === 0) return { uuid, state: "authorized" as const };
    const why = `${r.stderr}\n${r.stdout}`;
    if (/polkit|not authorized|AccessDenied|authenticat|permission/i.test(why))
      return yield* fail(403, "NEEDS_PASSWORD", `Your system wants an administrator password to approve this device. Run "boltctl enroll --policy auto ${uuid}" in a terminal, or approve it in Settings, Privacy, Thunderbolt.`);
    if (/already/i.test(why)) {
      const again = yield* exec(["boltctl", "authorize", uuid], 30_000);
      if (again.code === 0 || /already authorized/i.test(`${again.stderr}${again.stdout}`)) return { uuid, state: "authorized" as const };
      return yield* fail(500, "AUTHORIZE_FAILED", `boltctl could not approve the device: ${lastLine(again)}`);
    }
    return yield* fail(500, "AUTHORIZE_FAILED", `boltctl could not approve the device: ${lastLine(r)}`);
  });

export const egpuNote = (): string | null =>
  platform() === "darwin" && arch() === "arm64" ? "Apple Silicon Macs cannot use an external GPU. Use a GPU on another machine in your fleet instead." : null;

export const powerSource: Effect.Effect<"ac" | "battery" | "unknown"> = Effect.gen(function* () {
  if (platform() === "darwin" && !process.env.LOCAL_STUDIO_SYSFS) {
    const r = yield* exec(["pmset", "-g", "batt"], 5_000);
    return r.code !== 0 ? "unknown" : /'Battery Power'/.test(r.stdout) ? "battery" : "ac";
  }
  const dir = join(sysfs(), "class/power_supply");
  const supplies = list(dir)
    .filter((d) => read(join(dir, d, "scope")) !== "Device")
    .map((d) => ({ type: read(join(dir, d, "type")), online: read(join(dir, d, "online")) }));
  if (!supplies.some((s) => s.type === "Battery")) return "ac";
  return supplies.some((s) => s.type !== "Battery" && s.online === "1") ? "ac" : "battery";
});

export const hardware = (gpus: LocalGpu[], thunderbolt: LocalThunderbolt[], hotplug: LocalHotplug[]): LocalHardware => ({
  cpus: cpus().length, memTotalBytes: totalmem(), memFreeBytes: freemem(), gpus,
  os: platform(), arch: arch(), egpuNote: egpuNote(), thunderbolt, hotplug,
});
