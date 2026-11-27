import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { builtinBinary, runExtensions } from "./extensions.ts";
import { type Plan, read, stamp, write } from "./files.ts";

type Options = { t3Home: string | undefined; ompDir: string | undefined; controller: string | undefined; dryRun: boolean; json: boolean };
type Model = { id: string; contextWindow?: number };

const sq = (s: string): string => `'${s.replaceAll("'", `'\\''`)}'`;

const controllerConfig = (): string => {
  const set = process.env.LOCAL_STUDIO_CONTROLLER_HOME;
  if (set) return join(set, "config.json");
  const fresh = join(homedir(), ".local-studio-controller");
  const legacy = join(homedir(), ".local-studio-t3", "config.json");
  return !existsSync(fresh) && existsSync(legacy) ? legacy : join(fresh, "config.json");
};

const fleetKey = (path: string): string | null => {
  try {
    const key = (JSON.parse(readFileSync(path, "utf8")) as { fleetKey?: unknown }).fleetKey;
    return typeof key === "string" && key ? key : null;
  } catch {
    return null;
  }
};

const speaksAcp = async (omp: string): Promise<boolean> => {
  const child = Bun.spawn([omp, "acp"], { stdin: "pipe", stdout: "pipe", stderr: "ignore" });
  child.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: 1, clientCapabilities: {} } })}\n`);
  child.stdin.flush();
  const timer = setTimeout(() => child.kill(), 20_000);
  let buffer = "";
  let ok = false;
  try {
    for await (const chunk of child.stdout as unknown as AsyncIterable<Uint8Array>) {
      buffer += new TextDecoder().decode(chunk);
      const lines = buffer.split("\n");
      buffer = lines.pop() ?? "";
      const reply = lines.map((l) => { try { return JSON.parse(l) as { id?: unknown; result?: { agentInfo?: { name?: unknown } } }; } catch { return null; } }).find((m) => m?.id === 1);
      if (reply) {
        ok = reply.result?.agentInfo?.name === "omp";
        break;
      }
    }
  } finally {
    clearTimeout(timer);
    child.kill();
    await child.exited;
  }
  return ok;
};

const listModels = async (base: string, key: string | null): Promise<Model[] | string> => {
  try {
    const res = await fetch(`${base}/v1/models`, { headers: key ? { authorization: `Bearer ${key}` } : {}, signal: AbortSignal.timeout(10_000) });
    if (!res.ok) return `the controller at ${base} answered ${res.status} for /v1/models${res.status === 401 ? " (no fleet key found in its config.json)" : ""}`;
    const body = (await res.json()) as { data?: Record<string, unknown>[] };
    return (body.data ?? []).flatMap((m) => {
      if (typeof m.id !== "string") return [];
      const ctx = [m.context_length, m.max_model_len, m.context_window].find((v) => typeof v === "number" && v > 0) as number | undefined;
      return [ctx ? { id: m.id, contextWindow: ctx } : { id: m.id }];
    });
  } catch (e) {
    return `the controller at ${base} is unreachable (${e instanceof Error ? e.message : String(e)})`;
  }
};

export const install = async (o: Options): Promise<number> => {
  const say = (line: string) => { if (!o.json) console.log(line); };
  const stop = (code: number, message: string): number => {
    if (o.json) console.log(JSON.stringify({ ok: false, error: message }));
    else console.error(`localstudio: ${message}`);
    return code;
  };
  const omp = Bun.which("omp");
  if (!omp) return stop(3, "omp is not on PATH; install it from https://github.com/can1357/oh-my-pi and run install again");
  if (!(await speaksAcp(resolve(omp)))) return stop(4, `${omp} did not answer an ACP initialize as omp within 20 s`);
  const t3Home = o.t3Home ?? process.env.T3CODE_HOME ?? join(homedir(), ".t3");
  const ompDir = o.ompDir ?? process.env.PI_CODING_AGENT_DIR ?? join(homedir(), ".omp", "agent");
  const base = (o.controller ?? "http://127.0.0.1:18091").replace(/\/+$/, "");
  const settingsPath = join(t3Home, "userdata", "settings.json");
  const modelsPath = join(ompDir, "models.yml");
  const warnings: string[] = [];

  const settingsText = read(settingsPath);
  const settings = (settingsText === null ? {} : JSON.parse(settingsText)) as Record<string, unknown>;
  const instances = (settings.providerInstances ?? {}) as Record<string, unknown>;
  const prior = ((instances.localstudio as { config?: { commandArgs?: unknown } } | undefined)?.config?.commandArgs ?? []) as unknown[];
  const kept = prior.includes("acp") ? prior.slice(prior.indexOf("acp") + 1).filter((a): a is string => typeof a === "string") : [];
  settings.providerInstances = { ...instances, localstudio: { driver: "acpRegistry", displayName: "Local Studio", enabled: true, config: { source: "local", commandPath: resolve(omp), commandArgs: ["acp", ...(kept.length ? kept : ["--approval-mode=yolo"])], customModels: [] } } };

  const keyPath = controllerConfig();
  const listed = await listModels(base, fleetKey(keyPath));
  const models = typeof listed === "string" ? [] : listed;
  if (typeof listed === "string") warnings.push(`${listed}; the localstudio provider was written with no models`);
  const modelsText = read(modelsPath);
  const config = ((modelsText === null ? null : Bun.YAML.parse(modelsText)) ?? {}) as Record<string, unknown>;
  const providers = (config.providers ?? {}) as Record<string, unknown>;
  const reader = 'process.stdout.write(JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).fleetKey)';
  config.providers = { ...providers, localstudio: { baseUrl: `${base}/v1`, api: "openai-completions", apiKey: `!${sq(process.execPath)} -e ${sq(reader)} ${sq(keyPath)}`, models } };

  const plans: Plan[] = [
    { path: settingsPath, before: settingsText, after: `${JSON.stringify(settings, null, 2)}\n` },
    { path: modelsPath, before: modelsText, after: `${Bun.YAML.stringify(config, null, 2).replace(/[ \t]+$/gm, "").replace(/\n*$/, "\n")}` },
  ];
  const changed = plans.filter((p) => p.before !== p.after);
  const when = stamp();
  for (const p of changed) {
    say(`${o.dryRun ? "would write" : "writing"} ${p.path}${p.before === null ? " (new)" : `, backup ${p.path}.bak-localstudio-${when}`}`);
    if (!o.dryRun) write(p, when);
  }
  if (!changed.length) say("nothing to change");
  say(`localstudio provider: ${models.length} model(s) from ${base}/v1`);
  let browser = "localstudio extensions add browser: skipped, local-studio-browser not found; run it after installing the browser";
  if (builtinBinary("browser")) {
    try {
      const r = await runExtensions({ action: "add", name: "browser", args: [], t3Home, ompDir, dryRun: o.dryRun });
      browser = `localstudio extensions add browser: ${r.code ? `failed (${r.lines.filter((l) => l.startsWith("error")).join("; ")})` : `${o.dryRun ? "would add" : "added"} for ${r.targets} target(s)`}`;
    } catch (e) {
      browser = `localstudio extensions add browser: failed (${e instanceof Error ? e.message : String(e)})`;
    }
  }
  say(browser);
  for (const w of warnings) say(`warning: ${w}`);
  if (o.json) console.log(JSON.stringify({ ok: true, t3Settings: settingsPath, ompModels: modelsPath, provider: "localstudio", models: models.map((m) => m.id), warnings, browser, changed: changed.map((p) => p.path), ...(o.dryRun ? { dryRun: true } : {}) }));
  return 0;
};
