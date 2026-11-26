import { accessSync, constants, readFileSync } from "node:fs";
import { arch, platform } from "node:os";
import { join } from "node:path";
import * as Effect from "effect/Effect";
import type { LocalEject, LocalEjectProcess } from "./contracts.ts";
import { exec, fail, type HttpError, lastLine } from "./core.ts";
import { pciId, type RawGpu, sysfs } from "./hardware.ts";

export interface EjectResult {
  gpu: number | null;
  name: string | null;
  state: "removed" | "busy" | "needs_root" | "unsupported" | "cancelled";
  draining: boolean;
  unloaded: string[];
  processes: LocalEjectProcess[];
  command: string | null;
  message: string;
}

interface Deps {
  gpus: () => RawGpu[];
  ours: (g: RawGpu) => boolean;
  unload: (g: RawGpu) => Effect.Effect<string[], HttpError>;
}

const host = () => process.env.LOCAL_STUDIO_TEST_PLATFORM ?? `${platform()}-${arch()}`;

const comm = (pid: number): string => {
  try {
    return readFileSync(`/proc/${pid}/comm`, "utf8").trim();
  } catch {
    return "a program";
  }
};

const writable = (path: string): boolean => {
  try {
    accessSync(path, constants.W_OK);
    return true;
  } catch {
    return false;
  }
};

const unsupported = (): string | null => {
  const h = host();
  if (h === "darwin-arm64") return "Apple silicon Macs cannot use an external GPU, so there is nothing to eject.";
  if (h.startsWith("win32")) return "Local Studio cannot eject a GPU on Windows. Use Safely Remove Hardware in the taskbar.";
  if (!h.startsWith("linux") && !h.startsWith("darwin")) return `Local Studio cannot eject a GPU on ${h}.`;
  return null;
};

const removeFile = (g: RawGpu): string | null => {
  const id = g.busId ? pciId(g.busId) : null;
  return id ? join(sysfs(), "bus/pci/devices", id, "remove") : null;
};

const rootCommand = (path: string) => `echo 1 | sudo tee ${path}`;

const holders = (busId: string) =>
  Effect.gen(function* () {
    const r = yield* exec(["nvidia-smi", "-q", "-d", "PIDS", "-i", busId], 8_000);
    if (r.code !== 0) return yield* fail(500, "NVIDIA_SMI_FAILED", `nvidia-smi could not list the programs on the GPU: ${lastLine(r)}`);
    const found: { -readonly [K in keyof LocalEjectProcess]: LocalEjectProcess[K] }[] = [];
    for (const line of r.stdout.split("\n")) {
      const [key = "", value = ""] = line.split(/\s:\s/, 2).map((s) => s.trim());
      const last = found.at(-1);
      if (key === "Process ID" && Number(value) > 0) found.push({ pid: Number(value), name: comm(Number(value)), usedMiB: null });
      else if (last && key === "Name" && value) last.name = value.split("/").pop() || last.name;
      else if (last && key === "Used GPU Memory" && Number.isFinite(Number.parseFloat(value))) last.usedMiB = Number.parseFloat(value);
    }
    return found;
  });

const pinned = (busId: string) =>
  Effect.gen(function* () {
    const r = yield* exec(["nvidia-smi", "--query-gpu=display_active,persistence_mode", "--format=csv,noheader", "-i", busId], 8_000);
    if (r.code !== 0) return yield* fail(500, "NVIDIA_SMI_FAILED", `nvidia-smi could not read the GPU's display and persistence mode: ${lastLine(r)}`);
    const [display = "", persistence = ""] = r.stdout.trim().split(",").map((s) => s.trim());
    return { display: display === "Enabled", persistence: persistence === "Enabled" };
  });

export const makeEject = (d: Deps) => {
  const draining = new Set<string>();

  const find = (id: string): RawGpu | undefined => {
    const bus = pciId(id);
    return d.gpus().find((g) => String(g.index) === id || g.uuid === id || (!!bus && !!g.busId && pciId(g.busId) === bus));
  };

  const state = (g: RawGpu): LocalEject => {
    const on = draining.has(g.uuid);
    const why = unsupported();
    if (why) return { state: "unsupported", draining: on, blockers: [], command: null, message: why };
    const blockers = d.ours(g) ? [] : g.pids.map((pid) => ({ pid, name: comm(pid), usedMiB: null }));
    if (blockers.length) return { state: "busy", draining: on, blockers, command: null, message: `${blockers.length === 1 ? "A program" : `${blockers.length} programs`} outside Local Studio ${blockers.length === 1 ? "is" : "are"} using ${g.name}. Quit ${blockers.length === 1 ? "it" : "them"} first.` };
    const remove = removeFile(g);
    if (remove && !writable(remove)) return { state: "needs_root", draining: on, blockers: [], command: rootCommand(remove), message: "Removing the GPU from the PCI bus needs an administrator password." };
    return { state: "ready", draining: on, blockers: [], command: null, message: null };
  };

  const done = (g: RawGpu | null, r: Omit<EjectResult, "gpu" | "name">): EjectResult => ({ gpu: g?.index ?? null, name: g?.name ?? null, ...r });

  const eject = (id: string) =>
    Effect.gen(function* () {
      const why = unsupported();
      if (why) return done(null, { state: "unsupported", draining: false, unloaded: [], processes: [], command: null, message: why });
      if (host().startsWith("darwin")) {
        if (!Bun.which("SafeEjectGPU")) return done(null, { state: "unsupported", draining: false, unloaded: [], processes: [], command: null, message: "This Mac has no SafeEjectGPU tool. Eject the GPU from its menu bar icon." });
        const r = yield* exec(["SafeEjectGPU", "Eject"], 120_000);
        if (r.code !== 0) return yield* fail(500, "EJECT_FAILED", `SafeEjectGPU could not eject the GPU: ${lastLine(r)}`);
        return done(null, { state: "removed", draining: false, unloaded: [], processes: [], command: null, message: "macOS ejected the external GPU. You can unplug it now." });
      }
      const g = find(id);
      if (!g) return yield* fail(404, "GPU_NOT_FOUND", `no GPU ${id} on this machine`);
      const remove = removeFile(g);
      if (!g.external || !g.busId || !remove) return yield* fail(404, "NOT_EXTERNAL", `${g.name} is not an external GPU, so it cannot be ejected.`);
      draining.add(g.uuid);
      const unloaded = yield* d.unload(g);
      let processes = yield* holders(g.busId);
      for (let i = 0; unloaded.length && processes.length && i < 20; i++) {
        yield* Effect.sleep("1 second");
        processes = yield* holders(g.busId);
      }
      if (processes.length)
        return done(g, { state: "busy", draining: true, unloaded, processes, command: null, message: `${g.name} is still in use by ${processes.map((p) => `${p.name} (pid ${p.pid})`).join(", ")}. Local Studio does not stop programs it did not start. Quit them and eject again, or cancel to use the GPU again.` });
      const { display, persistence } = yield* pinned(g.busId);
      if (display)
        return done(g, { state: "busy", draining: true, unloaded, processes: [], command: null, message: `${g.name} drives a display. Move the display to another GPU, then eject again.` });
      if (persistence)
        return done(g, { state: "busy", draining: true, unloaded, processes: [], command: `sudo nvidia-smi -i ${g.busId} -pm 0`, message: `Persistence mode keeps the driver open on ${g.name}. Turn it off with the command, then eject again.` });
      if (!writable(remove))
        return done(g, { state: "needs_root", draining: true, unloaded, processes: [], command: rootCommand(remove), message: `Nothing is using ${g.name} now. Removing it from the PCI bus needs an administrator: run the command, then unplug it. Local Studio loads nothing on it until then.` });
      if (!draining.has(g.uuid))
        return done(g, { state: "cancelled", draining: false, unloaded, processes: [], command: null, message: `The eject of ${g.name} was cancelled, so it stays on the PCI bus.` });
      const w = yield* exec(["sh", "-c", 'printf 1 > "$1"', "sh", remove], 30_000);
      if (w.code === 124) return yield* fail(500, "EJECT_TIMEOUT", `the kernel did not finish removing ${g.name} within 30 seconds; something still holds the driver open`);
      if (w.code !== 0) return yield* fail(500, "EJECT_FAILED", `could not write ${remove}: ${lastLine(w)}`);
      return done(g, { state: "removed", draining: true, unloaded, processes: [], command: null, message: `${g.name} is off the PCI bus. You can unplug it now.` });
    });

  const cancel = (id: string) =>
    Effect.gen(function* () {
      const g = find(id);
      if (!g) return yield* fail(404, "GPU_NOT_FOUND", `no GPU ${id} on this machine`);
      draining.delete(g.uuid);
      return { gpu: g.index, name: g.name, draining: false };
    });

  return {
    state,
    eject,
    cancel,
    draining: (g: RawGpu) => draining.has(g.uuid),
    gone: (uuid: string) => draining.delete(uuid),
    any: () => draining.size > 0,
  };
};
