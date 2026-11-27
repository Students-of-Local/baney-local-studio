import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { basename, join, resolve } from "node:path";
import { builtinBinary, runExtensions } from "./extensions.ts";
import { type Plan, read, stamp, write } from "./files.ts";

type Options = { t3Home: string | undefined; ompDir: string | undefined; controller: string | undefined; dryRun: boolean; json: boolean };
type Model = { id: string; contextWindow?: number };
type Instance = { driver?: unknown; enabled?: unknown; [k: string]: unknown; config?: { commandPath?: unknown; commandArgs?: unknown; [k: string]: unknown } };

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
  const instances = { ...((settings.providerInstances ?? {}) as Record<string, Instance>) };
  const isOmp = (i: Instance | undefined): boolean => i?.driver === "acpRegistry" && typeof i.config?.commandPath === "string" && basename(i.config.commandPath) === "omp";
  const argsOf = (i: Instance | undefined): string[] => (Array.isArray(i?.config?.commandArgs) ? i.config.commandArgs.filter((a): a is string => typeof a === "string") : []);
  const modeOf = (args: string[]): string | undefined => args.find((a) => a.startsWith("--approval-mode="));
  const old = instances.localstudio;
  let carried: string | undefined;
  let removed = false;
  let kept = false;
  if (old !== undefined && isOmp(old)) {
    carried = modeOf(argsOf(old));
    delete instances.localstudio;
    removed = true;
  } else if (old !== undefined) warnings.push(`left providerInstances.localstudio (${typeof old.driver === "string" ? old.driver : "unknown"}) as it is`);
  let key = Object.keys(instances).find((k) => k !== "localstudio" && instances[k]?.enabled !== false && isOmp(instances[k]));
  if (key === undefined) {
    let n = 1;
    key = "omp" in instances ? "omp-local" : "omp";
    while (key in instances) key = `omp-local-${++n}`;
    instances[key] = { driver: "acpRegistry", displayName: "omp", enabled: true, config: { source: "local", commandPath: resolve(omp), commandArgs: ["--mode", "acp", carried ?? "--approval-mode=yolo"], customModels: [] } };
  } else if (carried === "--approval-mode=yolo" && !modeOf(argsOf(instances[key]))) {
    kept = true;
  } else if (carried && !modeOf(argsOf(instances[key]))) {
    const entry = instances[key] as Instance & { config: NonNullable<Instance["config"]> };
    instances[key] = { ...entry, config: { ...entry.config, commandArgs: [...(Array.isArray(entry.config.commandArgs) ? entry.config.commandArgs : []), carried] } };
  }
  const args = argsOf(instances[key]);
  const parsed = settingsText === null ? null : JSON.stringify(settings);
  settings.providerInstances = instances;
  const settingsAfter = parsed !== null && parsed === JSON.stringify(settings) ? settingsText : `${JSON.stringify(settings, null, 2)}\n`;

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
    { path: settingsPath, before: settingsText, after: settingsAfter ?? "" },
    { path: modelsPath, before: modelsText, after: `${Bun.YAML.stringify(config, null, 2).replace(/[ \t]+$/gm, "").replace(/\n*$/, "\n")}` },
  ];
  const changed = plans.filter((p) => p.before !== p.after);
  const when = stamp();
  if (removed) say(`${o.dryRun ? "would remove" : "removed"} the separate "Local Studio" omp entry; its threads continue under omp`);
  for (const p of changed) {
    say(`${o.dryRun ? "would write" : "writing"} ${p.path}${p.before === null ? " (new)" : `, backup ${p.path}.bak-localstudio-${when}`}`);
    if (!o.dryRun) write(p, when);
  }
  if (!changed.length) say("nothing to change");
  say(`localstudio provider: ${models.length} model(s) from ${base}/v1`);
  say(`local models added to: omp (${key})`);
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
  if (args.includes("--approval-mode=yolo")) say(`the ${key} instance runs omp with --approval-mode=yolo, so tool calls run without a prompt; to be asked, change it to --approval-mode=write or --approval-mode=always-ask in its commandArgs, and install keeps it`);
  if (kept) say("the old Local Studio entry ran omp with --approval-mode=yolo; your omp entry keeps its own approval mode; to run it without prompts, add --approval-mode=yolo after --mode acp in its commandArgs");
  for (const w of warnings) say(`warning: ${w}`);
  if (o.json) console.log(JSON.stringify({ ok: true, t3Settings: settingsPath, ompModels: modelsPath, provider: "localstudio", harnesses: [{ name: "omp", instance: key }], models: models.map((m) => m.id), warnings, browser, changed: changed.map((p) => p.path), ...(o.dryRun ? { dryRun: true } : {}) }));
  return 0;
};
