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
  const env: Record<string, string | undefined> = { ...process.env };
  delete env.CLAUDE_CONFIG_DIR;
  if (t.home) env.CLAUDE_CONFIG_DIR = t.home;
  const r = Bun.spawnSync([t.bin ?? "claude", ...args], { env, stdout: "pipe", stderr: "pipe" });
  return { code: r.exitCode, out: `${r.stdout.toString()}\n${r.stderr.toString()}`, err: r.stderr.toString().trim().split("\n")[0] ?? "" };
};

const claudeGet = (t: Extract<Target, { kind: "claude" }>, server: string): { command: string; args: string } | null => {
  if (!t.bin) return null;
  const r = claude(t, ["mcp", "get", server]);
  if (r.code !== 0) return null;
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
    if (!docs.has(path)) docs.set(path, { before, data: before === null ? {} : (JSON.parse(before) as Json), changed: false });
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
  if (o.action === "add" && s.missing) {
    const message = `${name} needs ${s.missing}; install it first`;
    return { code: 5, lines: [`localstudio: ${message}`], report: { ok: false, error: message }, targets: 0 };
  }
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
      if (d.changed) lines.push(`omp: ${verb(add ? "set" : "removed", add ? "would set" : "would remove")} mcpServers.${s.server} in ${t.path}`);
      else lines.push(`omp: ${s.server} already ${add ? "added" : "absent"}`);
      if (add) servers[s.server] = want;
      else delete servers[s.server];
      continue;
    }
    if (!t.bin) {
      lines.push(`skipped: ${t.id} (claude binary not found)`);
      continue;
    }
    const got = o.action === "add" ? claudeGet(t, s.server) : null;
    if (o.action === "add" && got?.command === want.command && got.args === want.args.join(" ")) lines.push(`${t.id}: ${s.server} already added`);
    else {
      lines.push(`${t.id}: ${verb("ran", "would run")} claude mcp ${o.action === "add" ? "add-json" : "remove"} ${s.server} --scope user${t.home ? ` with CLAUDE_CONFIG_DIR=${t.home}` : ""}`);
      if (!o.dryRun) {
        const rm = claude(t, ["mcp", "remove", s.server, "--scope", "user"]);
        if (rm.code !== 0 && !/No MCP server named/.test(rm.out)) {
          errors.push(`${t.id}: claude mcp remove failed: ${rm.err}`);
          continue;
        }
        if (o.action === "add") {
          const add = claude(t, ["mcp", "add-json", s.server, JSON.stringify({ type: "stdio", command: want.command, args: want.args }), "--scope", "user"]);
          if (add.code !== 0) {
            errors.push(`${t.id}: claude mcp add-json failed: ${add.err}`);
            continue;
          }
        }
      }
    }
    const d = doc(t.settings);
    const perms = obj(d.data, "permissions");
    const list = Array.isArray(perms.allow) ? (perms.allow as unknown[]) : [];
    if (o.action === "add" ? !list.includes(allow) : list.includes(allow)) {
      perms.allow = o.action === "add" ? [...list, allow] : list.filter((a) => a !== allow);
      d.changed = true;
    }
  }

  if (!BUILTINS[name]) {
    const entry = { command: want.command, args: want.args };
    record.changed = o.action === "add" ? !same(record.data[name], entry) : name in record.data;
    if (o.action === "add") record.data[name] = entry;
    else delete record.data[name];
  }

  const plans: Plan[] = [...docs].filter(([, d]) => d.changed).map(([path, d]) => ({ path, before: d.before, after: `${JSON.stringify(d.data, null, 2)}\n` }));
  const when = stamp();
  for (const p of plans) {
    lines.push(`${verb("writing", "would write")} ${p.path}${p.before === null ? " (new)" : `, backup ${p.path}.bak-localstudio-${when}`}`);
    if (!o.dryRun) write(p, when);
  }
  if (!plans.length) lines.push("no file to change");
  lines.push(...errors.map((e) => `error: ${e}`));
  return { code: errors.length ? 1 : 0, lines, report: { ok: !errors.length, action: o.action, name, server: s.server, targets: targets.map((t) => t.id), skipped, errors, changed: plans.map((p) => p.path), ...(o.dryRun ? { dryRun: true } : {}) }, targets: targets.length };
};
