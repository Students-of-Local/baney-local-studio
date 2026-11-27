#!/usr/bin/env bun
import { NAME, runExtensions } from "./extensions.ts";
import { find } from "./find.ts";
import { install } from "./install.ts";
import { probe } from "./probe.ts";

const [command, ...rest] = process.argv.slice(2);
const flags = new Map<string, string | true>();
const args: string[] = [];
const extArgs: string[] = [];
for (let i = 0; i < rest.length; i++) {
  const arg = rest[i] ?? "";
  if (!arg.startsWith("--")) {
    if ((command !== "find" || args.length) && (command !== "extensions" || args.length > 1)) usage(`unexpected argument ${arg}`);
    args.push(arg);
    continue;
  }
  const name = arg.slice(2);
  if (name === "arg" && command === "extensions") {
    const value = rest[++i];
    if (value === undefined) usage("--arg needs a value");
    extArgs.push(value);
  } else if (["t3-home", "omp-dir", "controller", "ctx", "limit", "command"].includes(name)) {
    const value = rest[++i];
    if (value === undefined) usage(`--${name} needs a value`);
    flags.set(name, value);
  } else if (["dry-run", "json"].includes(name)) flags.set(name, true);
  else usage(`unknown flag ${arg}`);
}

function usage(message: string): never {
  console.error(`localstudio: ${message}\nusage: localstudio install [--t3-home <dir>] [--omp-dir <dir>] [--controller <url>] [--dry-run] [--json]\n       localstudio probe [--json]\n       localstudio find [query] [--ctx N] [--limit N] [--json]\n       localstudio extensions list [--json]\n       localstudio extensions add <name> [--command <path> [--arg <a>]...] [--t3-home <dir>] [--omp-dir <dir>] [--dry-run] [--json]\n       localstudio extensions remove <name> [--t3-home <dir>] [--omp-dir <dir>] [--dry-run] [--json]`);
  process.exit(2);
}

const text = (name: string): string | undefined => {
  const v = flags.get(name);
  return typeof v === "string" ? v : undefined;
};

if (command === "install") {
  process.exitCode = await install({ t3Home: text("t3-home"), ompDir: text("omp-dir"), controller: text("controller"), dryRun: flags.has("dry-run"), json: flags.has("json") });
} else if (command === "probe") {
  if ([...flags.keys()].some((k) => k !== "json")) usage("probe takes only --json");
  const result = await probe();
  if (flags.has("json")) console.log(JSON.stringify(result, null, 2));
  else {
    console.log(`${result.os} ${result.arch}, RAM ${result.ramFreeGb} of ${result.ramTotalGb} GB free, disk ${result.disk.freeGb} GB free at ${result.disk.path}`);
    for (const g of result.gpus) console.log(`${g.id}: ${g.name}, ${g.vramGb ?? "?"} GB (${g.vramFreeGb ?? "?"} free), ${g.backend}${g.removable ? ", removable" : ""}, card ${g.card ?? "none"}`);
    for (const w of result.warnings) console.error(`warning: ${w}`);
  }
} else if (command === "find") {
  if ([...flags.keys()].some((k) => !["ctx", "limit", "json"].includes(k))) usage("find takes only --ctx, --limit and --json");
  const count = (name: string, fallback: number): number => {
    const v = text(name);
    if (v === undefined) return fallback;
    if (!/^[1-9]\d*$/.test(v)) usage(`--${name} needs a whole number above 0`);
    return Number(v);
  };
  process.exitCode = await find({ query: args[0], ctx: count("ctx", 8192), limit: count("limit", 10), json: flags.has("json") });
} else if (command === "extensions") {
  const [action, name] = args;
  if (action !== "list" && action !== "add" && action !== "remove") usage("extensions needs list, add or remove");
  if (action === "list" ? name !== undefined : name === undefined) usage(`extensions ${action} ${action === "list" ? "takes no name" : "needs a name"}`);
  if (name !== undefined && !NAME.test(name)) usage(`extension names match ${NAME.source}`);
  const builtin = name === "browser" || name === "skills";
  if (action === "add" && !builtin && !flags.has("command")) usage(`${name} is not built in, so add needs --command`);
  if ((action !== "add" || builtin) && (flags.has("command") || extArgs.length)) usage("--command and --arg are only for adding a custom extension");
  if ([...flags.keys()].some((k) => ["controller", "ctx", "limit"].includes(k))) usage("extensions takes only --t3-home, --omp-dir, --command, --arg, --dry-run and --json");
  const r = await runExtensions({ action, name, command: text("command"), args: extArgs, t3Home: text("t3-home"), ompDir: text("omp-dir"), dryRun: flags.has("dry-run") });
  if (flags.has("json")) console.log(JSON.stringify(r.report));
  else for (const line of r.lines) (line.startsWith("error") || r.code ? console.error : console.log)(line);
  process.exitCode = r.code;
} else usage(command ? `unknown command ${command}` : "no command");
