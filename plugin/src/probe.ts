import { existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, statfsSync, statSync, writeFileSync } from "node:fs";
import { arch, freemem, homedir, platform, totalmem } from "node:os";
import { basename, dirname, join } from "node:path";

export type Gpu = { id: string; name: string; vendor: string; backend: string; vramGb: number | null; vramFreeGb: number | null; pci: string | null; removable: boolean; card: string | null };
type Card = { match?: { backend?: string; name?: string; names?: string[]; vramGb?: number } };

const CATALOG = "https://local.sybilsolutions.ai/api/v2/catalog.json";
const gb = (bytes: number): number => Math.round((bytes / 2 ** 30) * 10) / 10;
const PCI_DIR = /^[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$/;

const run = (cmd: string[]): string | null => {
  if (!Bun.which(cmd[0] ?? "")) return null;
  const r = Bun.spawnSync(cmd, { stdout: "pipe", stderr: "ignore" });
  return r.exitCode === 0 ? r.stdout.toString() : null;
};
const read = (path: string): string | null => { try { return readFileSync(path, "utf8").trim(); } catch { return null; } };
const real = (path: string): string | null => { try { return realpathSync(path); } catch { return null; } };
const list = (dir: string): string[] => { try { return readdirSync(dir); } catch { return []; } };

const pciId = (bus: string): string | null => {
  const m = /^([0-9a-f]+):([0-9a-f]{2}):([0-9a-f]{2})\.([0-7])$/i.exec(bus.trim());
  return m ? `${(m[1] ?? "").slice(-4).padStart(4, "0")}:${m[2]}:${m[3]}.${m[4]}`.toLowerCase() : null;
};

const removable = (pci: string | null): boolean => {
  const path = pci && platform() === "linux" ? real(join("/sys/bus/pci/devices", pci)) : null;
  if (!path) return false;
  for (let p = path; PCI_DIR.test(basename(p)); p = dirname(p)) if (read(join(p, "removable")) === "removable") return true;
  return list("/sys/bus/thunderbolt/devices").filter((d) => /^domain\d+$/.test(d)).some((d) => {
    const port = dirname(dirname(real(join("/sys/bus/thunderbolt/devices", d)) ?? "/"));
    const up = PCI_DIR.test(basename(port)) ? dirname(port) : "";
    return PCI_DIR.test(basename(up)) && path.startsWith(`${up}/`);
  });
};

const nvidia = (): Omit<Gpu, "card">[] =>
  (run(["nvidia-smi", "--query-gpu=index,name,memory.total,memory.free,pci.bus_id", "--format=csv,noheader,nounits"]) ?? "").split("\n").flatMap((line) => {
    const [index, name, total, free, bus] = line.split(",").map((s) => s.trim());
    if (!index || !name) return [];
    const pci = pciId(bus ?? "");
    return [{ id: `nvidia:${index}`, name, vendor: "nvidia", backend: "nvidia", vramGb: gb(Number(total) * 2 ** 20), vramFreeGb: gb(Number(free) * 2 ** 20), pci, removable: removable(pci) }];
  });

const amd = (): Omit<Gpu, "card">[] => {
  const out = run(["rocm-smi", "--showproductname", "--showmeminfo", "vram", "--json"]);
  if (!out) return [];
  const cards = Object.entries(JSON.parse(out) as Record<string, Record<string, string>>).filter(([k]) => /^card\d+$/.test(k));
  return cards.map(([k, v]) => {
    const total = Number(v["VRAM Total Memory (B)"]);
    const used = Number(v["VRAM Total Used Memory (B)"]);
    const pci = pciId(v["PCI Bus"] ?? "");
    return { id: `amd:${k.slice(4)}`, name: v["Card Series"] ?? v["Card series"] ?? v["Card model"] ?? k, vendor: "amd", backend: "amd-rocm", vramGb: total ? gb(total) : null, vramFreeGb: total && !Number.isNaN(used) ? gb(total - used) : null, pci, removable: removable(pci) };
  });
};

const intel = (): Omit<Gpu, "card">[] =>
  (run(["lspci", "-D", "-mm"]) ?? "").split("\n").flatMap((line, i) => {
    const f = [...line.matchAll(/"([^"]*)"/g)].map((m) => m[1] ?? "");
    const pci = pciId(line.split(" ")[0] ?? "");
    if (!pci || !/VGA|3D|Display/.test(f[0] ?? "") || !/Intel/.test(f[1] ?? "") || !/Arc/.test(f[2] ?? "")) return [];
    const drm = list(join("/sys/bus/pci/devices", pci, "drm")).find((d) => /^card\d+$/.test(d));
    const total = Number(read(join("/sys/bus/pci/devices", pci, "drm", drm ?? "-", "device", "tile0", "vram0", "size")) ?? NaN);
    return [{ id: `intel:${i}`, name: (/\[(Arc[^\]]*)\]/.exec(f[2] ?? "")?.[1] ?? f[2] ?? "Intel Arc").trim(), vendor: "intel", backend: "intel-xpu", vramGb: total > 0 ? gb(total) : null, vramFreeGb: null, pci, removable: removable(pci) }];
  });

const mac = (): Omit<Gpu, "card">[] => {
  const out = run(["system_profiler", "SPDisplaysDataType", "-json"]);
  const mem = Number(run(["sysctl", "-n", "hw.memsize"]) ?? 0);
  const items = out ? ((JSON.parse(out) as { SPDisplaysDataType?: Record<string, string>[] }).SPDisplaysDataType ?? []) : [];
  return items.map((d, i) => {
    const apple = /^Apple/.test(d.sppci_model ?? "");
    const vram = /(\d+)\s*GB/.exec(d.spdisplays_vram ?? d.spdisplays_vram_shared ?? "")?.[1];
    return { id: `metal:${i}`, name: d.sppci_model ?? "GPU", vendor: apple ? "apple" : (d.spdisplays_vendor ?? "").replace(/^sppci_vendor_/, "").toLowerCase() || "unknown", backend: "metal", vramGb: apple ? gb(mem) : vram ? Number(vram) : null, vramFreeGb: apple ? gb(freemem()) : null, pci: null, removable: false };
  });
};

const catalog = async (warnings: string[]): Promise<Record<string, Card> | null> => {
  const dir = join(homedir(), ".cache", "localstudio");
  const path = join(dir, "catalog.json");
  const fresh = existsSync(path) && Date.now() - statSync(path).mtimeMs < 86_400_000;
  if (!fresh) {
    try {
      const res = await fetch(CATALOG, { signal: AbortSignal.timeout(15_000) });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const text = await res.text();
      if (!(JSON.parse(text) as { cards?: unknown }).cards) throw new Error("no cards in the catalog");
      mkdirSync(dir, { recursive: true, mode: 0o700 });
      writeFileSync(path, text);
    } catch (e) {
      warnings.push(`could not fetch ${CATALOG} (${e instanceof Error ? e.message : String(e)})${existsSync(path) ? "; using the cached copy" : "; cards not matched"}`);
    }
  }
  try {
    return (JSON.parse(readFileSync(path, "utf8")) as { cards: Record<string, Card> }).cards;
  } catch {
    return null;
  }
};

const norm = (s: string): string => s.toLowerCase().replace(/nvidia|geforce|intel|amd|radeon|generation|workstation|edition|\d+\s*gb/g, "").replace(/[^a-z0-9]/g, "");

const matchCard = (cards: Record<string, Card>, g: Omit<Gpu, "card">): string | null =>
  Object.entries(cards)
    .filter(([, c]) => c.match?.backend === g.backend && ((c.match.names ?? []).includes(norm(g.name)) || norm(c.match.name ?? "") === norm(g.name)))
    .map(([id, c]) => ({ id, vram: c.match?.vramGb ?? 0, gap: Math.abs((c.match?.vramGb ?? 0) - (g.vramGb ?? 0)) }))
    .filter((h) => g.vramGb === null || h.gap <= Math.max(1, h.vram * 0.05))
    .sort((a, b) => a.gap - b.gap)[0]?.id ?? null;

export const probe = async () => {
  const warnings: string[] = [];
  const found = platform() === "darwin" ? mac() : [...nvidia(), ...amd(), ...intel()];
  const cards = found.length ? await catalog(warnings) : null;
  const gpus: Gpu[] = found.map((g) => ({ ...g, card: cards ? matchCard(cards, g) : null }));
  const path = process.env.HF_HOME || join(homedir(), ".cache", "huggingface");
  let probePath = path;
  while (!existsSync(probePath) && dirname(probePath) !== probePath) probePath = dirname(probePath);
  const fs = statfsSync(probePath);
  return { os: platform(), arch: arch(), ramTotalGb: gb(totalmem()), ramFreeGb: gb(freemem()), disk: { path, freeGb: gb(fs.bavail * fs.bsize) }, gpus, warnings };
};
