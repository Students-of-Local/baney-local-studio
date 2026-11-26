import { accessSync, constants, readFileSync, writeFileSync } from "node:fs";
import { arch, platform } from "node:os";
import { join } from "node:path";
import * as Effect from "effect/Effect";
import type { LocalEject, LocalEjectProcess } from "./contracts.ts";
import { exec, fail, type HttpError, httpError, lastLine } from "./core.ts";
import { pciId, type RawGpu, sysfs } from "./hardware.ts";

export interface EjectResult {
  gpu: number | null;
  name: string | null;
  state: "removed" | "busy" | "needs_root" | "unsupported";
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

const computeApps = (busId: string) =>
  Effect.gen(function* () {
    const r = yield* exec(["nvidia-smi", "--query-compute-apps=pid,process_name,used_memory", "--format=csv,noheader", "-i", busId], 8_000);
    if (r.code !== 0) return yield* fail(500, "NVIDIA_SMI_FAILED", `nvidia-smi could not list the programs on the GPU: ${lastLine(r)}`);
    return r.stdout.split("\n").flatMap((line): LocalEjectProcess[] => {
      const [pid = "", name = "", used = ""] = line.split(",").map((s) => s.trim());
      const n = Number(pid);
      const mib = Number.parseFloat(used);
      return n > 0 ? [{ pid: n, name: name.split("/").pop() || comm(n), usedMiB: Number.isFinite(mib) ? mib : null }] : [];
    });
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
      let processes = yield* computeApps(g.busId);
      for (let i = 0; unloaded.length && processes.length && i < 20; i++) {
        yield* Effect.sleep("1 second");
        processes = yield* computeApps(g.busId);
      }
      if (processes.length)
        return done(g, { state: "busy", draining: true, unloaded, processes, command: null, message: `${g.name} is still in use by ${processes.map((p) => `${p.name} (pid ${p.pid})`).join(", ")}. Local Studio does not stop programs it did not start. Quit them and eject again, or cancel to use the GPU again.` });
      if (!writable(remove))
        return done(g, { state: "needs_root", draining: true, unloaded, processes: [], command: rootCommand(remove), message: `Nothing is using ${g.name} now. Removing it from the PCI bus needs an administrator: run the command, then unplug it. Local Studio loads nothing on it until then.` });
      yield* Effect.try({ try: () => writeFileSync(remove, "1"), catch: (e) => httpError(500, "EJECT_FAILED", `could not write ${remove}: ${String(e)}`) });
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
