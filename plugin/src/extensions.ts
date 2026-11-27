import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { basename, join } from "node:path";
import { type Plan, read, stamp, write } from "./files.ts";

export type ExtOptions = { action: "list" | "add" | "remove"; name?: string | undefined; command?: string | undefined; args: string[]; t3Home?: string | undefined; ompDir?: string | undefined; dryRun: boolean };
export type ExtResult = { code: number; lines: string[]; report: Record<string, unknown>; targets: number };
type Json = Record<string, unknown>;
type Target = { id: string; kind: "omp"; path: string } | { id: string; kind: "claude"; bin: string | null; home: string; settings: string };
type Instance = { driver?: unknown; enabled?: unknown; config?: { commandPath?: unknown; homePath?: unknown; binaryPath?: unknown } };

const BUILTINS: Record<string, { server: string; binary: string; args: string[]; find: () => string | null }> = {
  browser: { server: "local-studio", binary: "local-studio-browser", args: ["mcp"], find: () => Bun.which("local-studio-browser") ?? [join(homedir(), ".local-studio", "browser", "local-studio-browser")].find((p) => existsSync(p)) ?? null },
  skills: { server: "local-studio-skills", binary: "local-studio", args: ["skills", "mcp"], find: () => Bun.which("local-studio") },
};
export const NAME = /^[a-z][a-z0-9-]{0,39}$/;
export const builtinBinary = (name: string): string | null => BUILTINS[name]?.find() ?? null;

const text = (v: unknown): string => (typeof v === "string" ? v : "");
const same = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);
const obj = (parent: Json, key: string): Json => (parent[key] && typeof parent[key] === "object" && !Array.isArray(parent[key]) ? (parent[key] as Json) : (parent[key] = {}));
const claude = (t: Extract<Target, { kind: "claude" }>, args: string[]) => {
  const r = Bun.spawnSync([t.bin ?? "claude", ...args], { env: { ...process.env, CLAUDE_CONFIG_DIR: t.home || undefined }, stdout: "pipe", stderr: "pipe" });
  return { code: r.exitCode, out: `${r.stdout.toString()}\n${r.stderr.toString()}`, err: r.stderr.toString().trim().split("\n")[0] ?? "" };
};
const claudeGet = (t: Extract<Target, { kind: "claude" }>, server: string): { command: string; args: string } | null => {
  const r = t.bin ? claude(t, ["mcp", "get", server]) : null;
  if (r?.code !== 0) return null;
  const field = (k: string) => new RegExp(`^\\s*${k}: ?(.*)$`, "m").exec(r.out)?.[1]?.trim() ?? "";
  return { command: field("Command"), args: field("Args") };
};
export const runExtensions = async (o: ExtOptions): Promise<ExtResult> => {
  const t3Home = o.t3Home ?? process.env.T3CODE_HOME ?? join(homedir(), ".t3");
  const ompDir = o.ompDir ?? process.env.PI_CODING_AGENT_DIR ?? join(homedir(), ".omp", "agent");
  const recordPath = join(t3Home, "userdata", "localstudio-extensions.json");
  const lines: string[] = [];
  const skipped: string[] = [];
  const docs = new Map<string, { before: string | null; data: Json; changed: boolean }>();
  const doc = (path: string) => {
    const before = docs.has(path) ? null : read(path);
    const data = docs.has(path) || before === null ? {} : (() => { try { return JSON.parse(before) as unknown; } catch (e) { throw new Error(`${path} is not valid JSON (${e instanceof Error ? e.message : String(e)}); fix it and run again`); } })();
    if (!data || typeof data !== "object" || Array.isArray(data)) throw new Error(`${path} does not hold a JSON object; fix it and run again`);
    if (!docs.has(path)) docs.set(path, { before, data: data as Json, changed: false });
    return docs.get(path)!;
  };
  const settingsText = read(join(t3Home, "userdata", "settings.json"));
  const instances = Object.entries((((settingsText === null ? {} : JSON.parse(settingsText)) as Json).providerInstances ?? {}) as Record<string, Instance>);
  const targets: Target[] = [];
  for (const [id, inst] of instances) {
    if (inst.enabled === false) continue;
    if (inst.driver === "acpRegistry" && basename(text(inst.config?.commandPath)) === "omp") {
      if (!targets.some((t) => t.kind === "omp")) targets.push({ id: "omp", kind: "omp", path: join(ompDir, "mcp.json") });
    } else if (inst.driver === "claudeAgent") {
      const home = text(inst.config?.homePath);
      targets.push({ id, kind: "claude", bin: text(inst.config?.binaryPath) || Bun.which("claude"), home, settings: join(home || join(homedir(), ".claude"), "settings.json") });
    } else skipped.push(`skipped: ${id} (${text(inst.driver) || "unknown"} not supported yet)`);
  }
  lines.push(...skipped);
  const stop = (code: number, message: string): ExtResult => ({ code, lines: [`localstudio: ${message}`], report: { ok: false, error: message }, targets: 0 });
  try { for (const path of [recordPath, ...targets.map((t) => (t.kind === "omp" ? t.path : t.settings))]) doc(path); } catch (e) { return stop(1, e instanceof Error ? e.message : String(e)); }
  const record = doc(recordPath);
  const spec = (name: string) => {
    const b = BUILTINS[name];
    const bin = b?.find() ?? null;
    if (b) return { server: b.server, command: bin, args: b.args, missing: bin ? null : b.binary };
    const r = record.data[name] as { command?: unknown; args?: unknown } | undefined;
    return { server: name, command: o.command ?? (text(r?.command) || null), args: o.command ? o.args : Array.isArray(r?.args) ? r.args.map(String) : [], missing: null };
  };
  if (o.action === "list") {
    const extensions = [...Object.keys(BUILTINS), ...Object.keys(record.data).filter((n) => !BUILTINS[n])].map((name) => {
      const s = spec(name);
      const states: Record<string, string> = {};
      for (const t of targets) {
        const present = t.kind === "omp" ? Boolean(((doc(t.path).data.mcpServers ?? {}) as Json)[s.server]) : Boolean(claudeGet(t, s.server));
        states[t.id] = present ? "added" : (t.kind === "claude" && !t.bin) || s.missing ? "binary not found" : "missing";
      }
      lines.push(`${name} (${s.server}): ${targets.map((t) => `${t.id} ${states[t.id]}`).join(", ") || "no targets"}`);
      return { name, server: s.server, targets: states };
    });
    return { code: 0, lines, report: { ok: true, extensions, skipped }, targets: targets.length };
  }
  const name = o.name ?? "";
  const s = spec(name);
  if (o.action === "add" && s.missing) return stop(5, `${name} needs ${s.missing}; install it first`);
  const foreign = !BUILTINS[name] && !(name in record.data);
  if (foreign && targets.some((t) => (t.kind === "omp" ? s.server in obj(doc(t.path).data, "mcpServers") : claudeGet(t, s.server)))) return stop(1, `an MCP server named ${name} is set up outside localstudio extensions, so it is left alone`);
  if (foreign && o.action === "remove") return { code: 0, lines: [...lines, `${name} is not set up, so there is nothing to remove`], report: { ok: true, action: o.action, name, changed: [] }, targets: targets.length };
  const want = { type: "stdio", command: s.command ?? "", args: s.args, enabled: true };
  const allow = `mcp__${s.server}__*`;
  const errors: string[] = [];
  const verb = (does: string, would: string) => (o.dryRun ? would : does);
  for (const t of targets) {
    if (t.kind === "omp") {
      const d = doc(t.path);
      const servers = obj(d.data, "mcpServers");
      const add = o.action === "add";
      d.changed ||= add ? !same(servers[s.server], want) : s.server in servers;
      lines.push(d.changed ? `omp: ${verb(add ? "set" : "removed", add ? "would set" : "would remove")} mcpServers.${s.server} in ${t.path}` : `omp: ${s.server} already ${add ? "added" : "absent"}`);
      if (add) servers[s.server] = want; else delete servers[s.server];
      continue;
    }
    if (!t.bin) { lines.push(`skipped: ${t.id} (claude binary not found)`); continue; }
    const got = claudeGet(t, s.server);
    if (o.action === "remove" && !got) lines.push(`${t.id}: ${s.server} already absent`);
    else if (o.action === "add" && got?.command === want.command && got.args === want.args.join(" ")) lines.push(`${t.id}: ${s.server} already added`);
    else {
      lines.push(`${t.id}: ${verb("ran", "would run")} claude mcp ${o.action === "add" ? "add-json" : "remove"} ${s.server} --scope user${t.home ? ` with CLAUDE_CONFIG_DIR=${t.home}` : ""}`);
      if (!o.dryRun) {
        const addJson = (command: string, args: string[]) => claude(t, ["mcp", "add-json", s.server, JSON.stringify({ type: "stdio", command, args }), "--scope", "user"]);
        const rm = got ? claude(t, ["mcp", "remove", s.server, "--scope", "user"]) : null;
        if (rm && rm.code !== 0) { errors.push(`${t.id}: claude mcp remove failed: ${rm.err}`); continue; }
        const add = o.action === "add" ? addJson(want.command, want.args) : null;
        if (add && add.code !== 0) {
          const back = got?.command ? addJson(got.command, got.args ? got.args.split(" ") : []).code === 0 : false;
          errors.push(`${t.id}: claude mcp add-json failed: ${add.err}${got ? (back ? "; the old server is back" : "; the old server could not be put back") : ""}`); continue;
        }
      }
    }
    const d = doc(t.settings);
    const perms = obj(d.data, "permissions");
    const list = Array.isArray(perms.allow) ? (perms.allow as unknown[]) : [];
    if (o.action === "add" ? !list.includes(allow) : list.includes(allow)) { perms.allow = o.action === "add" ? [...list, allow] : list.filter((a) => a !== allow); d.changed = true; }
  }
  if (!BUILTINS[name]) {
    const entry = { command: want.command, args: want.args };
    record.changed = o.action === "add" ? !same(record.data[name], entry) : name in record.data;
    if (o.action === "add") record.data[name] = entry; else delete record.data[name];
  }
  const plans: Plan[] = [...docs].filter(([, d]) => d.changed).map(([path, d]) => ({ path, before: d.before, after: `${JSON.stringify(d.data, null, 2)}\n` }));
  const when = stamp();
  for (const p of plans) { lines.push(`${verb("writing", "would write")} ${p.path}${p.before === null ? " (new)" : `, backup ${p.path}.bak-localstudio-${when}`}`); if (!o.dryRun) write(p, when); }
  if (!plans.length) lines.push("no file to change");
  lines.push(...errors.map((e) => `error: ${e}`));
  return { code: errors.length ? 1 : 0, lines, report: { ok: !errors.length, action: o.action, name, server: s.server, targets: targets.map((t) => t.id), skipped, errors, changed: plans.map((p) => p.path), ...(o.dryRun ? { dryRun: true } : {}) }, targets: targets.length };
};
