import { readFileSync } from "node:fs";
import { catalog, gb, matchCard, probe, type Catalog, type Gpu } from "./probe.ts";

type Device = Omit<Awaited<ReturnType<typeof probe>>, "warnings">;
type Pick = { source: "registry"; label: "Proven"; model: string | null; quant: string | null; engine: string | null; tps: number | null; prefillTps: number | null; proofDate: string | null; recipe: string };
type Result = { source: "hf"; repo: string; format: string; quant: string | null; sizeGb: number | null; gated: boolean; licenseUrl: string | null; label: string | null; why: string; tps: null };
type Repo = { id: string; tags?: string[]; gated?: boolean | string; siblings?: { rfilename: string; size?: number }[] };

const r1 = (n: number): number => Math.round(n * 10) / 10;
const message = (e: unknown): string => (e instanceof Error ? e.message : String(e));

const loadDevice = async (warnings: string[]): Promise<Device> => {
  const file = process.env.LOCALSTUDIO_PROBE_JSON;
  if (!file) {
    const { warnings: w, ...device } = await probe();
    warnings.push(...w);
    return device;
  }
  const { warnings: _, ...device } = JSON.parse(readFileSync(file, "utf8")) as Device & { warnings?: string[] };
  return device;
};

const picksFor = (cat: Catalog, gpus: Gpu[], query: string): { pick: Pick; sizeGb: number | null }[] => {
  const counts = new Map<string, number>();
  for (const g of gpus) if (g.card) counts.set(g.card, (counts.get(g.card) ?? 0) + 1);
  return [...counts].flatMap(([id, n]) => {
    const setup = (cat.cards[id]?.setups ?? []).filter((s) => s.machines === 1 && s.cards <= n).sort((a, b) => b.cards - a.cards)[0];
    return (setup?.picks ?? []).flatMap((key) => {
      const r = cat.recipes?.[key];
      const m = cat.models?.[r?.model ?? ""];
      if (query && ![key, r?.model, m?.name, m?.hf].some((s) => s?.toLowerCase().includes(query))) return [];
      const proof = r?.proof?.find((p) => !p.failed);
      const build = r?.weights ? cat.builds?.[r.weights] : undefined;
      const pick: Pick = { source: "registry", label: "Proven", model: r?.model ?? null, quant: build?.format ?? null, engine: r?.engine ?? null, tps: proof?.tps ?? null, prefillTps: proof?.prefill ?? null, proofDate: proof?.at ?? null, recipe: key };
      return [{ pick, sizeGb: build?.size_gb ?? null }];
    });
  });
};

const hfToken = (): string | null => {
  if (process.env.HF_TOKEN) return process.env.HF_TOKEN;
  if (!Bun.which("hf")) return null;
  const r = Bun.spawnSync(["hf", "auth", "token"], { stdout: "pipe", stderr: "ignore" });
  const line = r.exitCode === 0 ? r.stdout.toString().trim().split("\n").pop()?.trim() : undefined;
  return line && /^hf_[A-Za-z0-9]+$/.test(line) ? line : null;
};

const QUANT = /(?:^|[-_.])((?:UD-)?(?:I?Q\d|TQ\d)[A-Za-z0-9_]*|BF16|F16|F32|MXFP4[A-Za-z0-9_]*|\d+(?:\.\d+)?bpw)$/i;

type FileSet = { format: string; quant: string | null; stem: string; bytes: number | null };

const SHARD = /-\d{5}-of-\d{5}\.safetensors$/i;

const fileSets = (repo: Repo): FileSet[] => {
  const tags = repo.tags ?? [];
  const mlx = tags.includes("mlx") || /mlx/i.test(repo.id);
  const exl3 = tags.includes("exl3") || /exl3/i.test(repo.id);
  const sets = new Map<string, FileSet>();
  const add = (key: string, set: Omit<FileSet, "bytes">, size: number | undefined) => {
    const s = sets.get(key) ?? { ...set, bytes: 0 };
    s.bytes = s.bytes === null || size === undefined ? null : s.bytes + size;
    sets.set(key, s);
  };
  const folders = new Map<string, { rfilename: string; size?: number }[]>();
  for (const f of repo.siblings ?? []) {
    const name = f.rfilename.split("/").pop() ?? f.rfilename;
    if (/\.gguf$/i.test(name)) {
      if (/mmproj/i.test(name)) continue;
      const key = f.rfilename.replace(/-\d{5}-of-\d{5}\.gguf$/i, ".gguf");
      const stem = (key.split("/").pop() ?? key).replace(/\.gguf$/i, "");
      add(`gguf:${key}`, { format: "gguf", quant: QUANT.exec(stem)?.[1] ?? stem, stem }, f.size);
    } else if (/\.safetensors$/i.test(name) && !/^adapter_model/i.test(name)) {
      const folder = f.rfilename.includes("/") ? f.rfilename.slice(0, f.rfilename.lastIndexOf("/")) : "";
      folders.set(folder, [...(folders.get(folder) ?? []), f]);
    }
  }
  const format = mlx ? "mlx" : exl3 ? "exl3" : "safetensors";
  const named = mlx ? (/\d+[-_]?bit/i.exec(repo.id)?.[0] ?? null) : exl3 ? (/\d+(?:\.\d+)?_?bpw/i.exec(repo.id)?.[0] ?? null) : null;
  for (const [folder, files] of folders) {
    const shards = files.filter((f) => SHARD.test(f.rfilename));
    for (const f of shards.length ? shards : files) add(`st:${folder}`, { format, quant: named ?? (folder || null), stem: folder }, f.size);
  }
  const all = [...sets.values()];
  return all.map((s) => (s.stem && all.some((o) => o !== s && o.quant === s.quant) ? { ...s, quant: s.stem } : s));
};

const fit = (sizeGb: number | null, format: string, device: Device): { label: string | null; why: string } => {
  if (sizeGb === null) return { label: null, why: "Hugging Face did not give every file's size" };
  const groups = new Map<string, { name: string; apple: boolean; gpus: { usable: number; total: number; free: number | null }[] }>();
  for (const g of device.gpus) {
    const apple = g.vendor === "apple";
    const group = groups.get(`${g.vendor}/${g.backend}`) ?? { name: g.name, apple, gpus: [] };
    if (apple && group.gpus.length) continue;
    const total = apple ? device.ramTotalGb : (g.vramGb ?? 0);
    group.gpus.push({ usable: apple ? 0.75 * total : 0.9 * total, total, free: g.vramFreeGb });
    groups.set(`${g.vendor}/${g.backend}`, group);
  }
  const sum = (xs: number[]): number => xs.reduce((a, b) => a + b, 0);
  const best = [...groups.values()].map((g) => ({ ...g, gpus: g.gpus.sort((a, b) => b.usable - a.usable) })).sort((a, b) => sum(b.gpus.map((x) => x.usable)) - sum(a.gpus.map((x) => x.usable)))[0] ?? { name: "no GPU", apple: false, gpus: [] };
  const need = sizeGb * 1.1 + 1;
  const all = sum(best.gpus.map((x) => x.usable));
  let n = 0;
  while (n < best.gpus.length && sum(best.gpus.slice(0, n).map((x) => x.usable)) < need) n++;
  const used = best.gpus.slice(0, Math.max(n, 1));
  const usable = sum(used.map((x) => x.usable));
  const where = `${best.name}${used.length > 1 ? `, split across ${used.length} GPUs` : ""}`;
  const free = used.every((x) => x.free !== null) ? sum(used.map((x) => x.free ?? 0)) : null;
  const busy = used.length && free !== null && free < Math.min(need, usable) ? `; ${r1(sum(used.map((x) => x.total)) - free)} GB in use now` : "";
  const ram = format === "gguf" && !best.apple ? 0.8 * device.ramFreeGb : 0;
  const disk = sizeGb > device.disk.freeGb ? `; the files need ${r1(sizeGb)} GB of disk and ${device.disk.path} has ${device.disk.freeGb} GB free` : "";
  if (need <= all) return { label: "Fits", why: `needs ${r1(need)} GB of the ${r1(usable)} GB usable on ${where}${busy}${disk}` };
  if (ram && need <= all + ram) return { label: "Offload", why: `needs ${r1(need)} GB: ${r1(all)} GB on ${where} and ${r1(need - all)} GB of the ${r1(ram)} GB usable RAM${busy}${disk}` };
  return { label: "Won't fit", why: `needs ${r1(need)} GB, ${r1(need - all - ram)} GB more than the ${r1(all + ram)} GB usable on ${where}${ram ? " plus RAM" : ""}${busy}${disk}` };
};

const searchHf = async (query: string, limit: number, device: Device, warnings: string[]): Promise<Result[]> => {
  const base = (process.env.HF_ENDPOINT || "https://huggingface.co").replace(/\/+$/, "");
  const token = hfToken();
  const get = async (path: string): Promise<unknown> => {
    const res = await fetch(base + path, { headers: token ? { authorization: `Bearer ${token}` } : {}, signal: AbortSignal.timeout(20_000) });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.json();
  };
  let found: Repo[];
  try {
    found = (await get(`/api/models?search=${encodeURIComponent(query)}&limit=${limit}&full=true`)) as Repo[];
  } catch (e) {
    warnings.push(`could not search Hugging Face at ${base} (${message(e)}), so there are no Hugging Face results`);
    return [];
  }
  const repos = await Promise.all(
    found.map((r) =>
      (get(`/api/models/${r.id.split("/").map(encodeURIComponent).join("/")}?blobs=true`) as Promise<Repo>).catch((e) => {
        warnings.push(`could not read ${r.id} from Hugging Face (${message(e)})`);
        return null;
      }),
    ),
  );
  return repos.flatMap((repo) => {
    if (!repo) return [];
    const gated = repo.gated === true || repo.gated === "auto" || repo.gated === "manual";
    return fileSets(repo).map((s) => {
      const sizeGb = s.bytes === null ? null : gb(s.bytes);
      return { source: "hf", repo: repo.id, format: s.format, quant: s.quant, sizeGb, gated, licenseUrl: gated ? `${base}/${repo.id}` : null, ...fit(sizeGb, s.format, device), tps: null };
    });
  });
};

export const find = async (opts: { query: string | undefined; ctx: number; limit: number; json: boolean }): Promise<number> => {
  const warnings: string[] = [];
  const device = await loadDevice(warnings);
  const cat = await catalog(warnings);
  const gpus = device.gpus.map((g) => ({ ...g, card: g.card === undefined ? (cat ? matchCard(cat.cards, g) : null) : g.card }));
  const cards = [...new Set(gpus.flatMap((g) => g.card ?? []))];
  if (cat && !cards.length) warnings.push(device.gpus.length ? `no registry card matches ${device.gpus.map((g) => `${g.name} (${g.vramGb ?? "?"} GB)`).join(", ")}, so there are no registry picks` : "no GPU found, so there are no registry picks");
  const picks = cat ? picksFor(cat, gpus, opts.query?.toLowerCase() ?? "") : [];
  const results = opts.query ? await searchHf(opts.query, opts.limit, device, warnings) : [];
  const unique = [...new Set(warnings)];
  if (opts.json) console.log(JSON.stringify({ ok: true, device: { ...device, gpus }, cards, picks: picks.map((p) => p.pick), results, warnings: unique }, null, 2));
  else {
    for (const { pick: p, sizeGb } of picks) console.log([p.label, p.model ?? p.recipe, p.quant ?? "-", sizeGb === null ? "? GB" : `${sizeGb} GB`, p.tps === null ? "tps unknown" : `${p.tps} tok/s`].join("\t"));
    for (const r of results) console.log([r.label ?? "Unknown size", r.repo, r.quant ?? r.format, r.sizeGb === null ? "? GB" : `${r.sizeGb} GB`, "est. pending"].join("\t"));
    for (const w of unique) console.error(`warning: ${w}`);
  }
  return 0;
};
