#!/usr/bin/env bun
import { install } from "./install.ts";
import { probe } from "./probe.ts";

const [command, ...rest] = process.argv.slice(2);
const flags = new Map<string, string | true>();
for (let i = 0; i < rest.length; i++) {
  const arg = rest[i] ?? "";
  if (!arg.startsWith("--")) usage(`unexpected argument ${arg}`);
  const name = arg.slice(2);
  if (["t3-home", "omp-dir", "controller"].includes(name)) {
    const value = rest[++i];
    if (value === undefined) usage(`--${name} needs a value`);
    flags.set(name, value);
  } else if (["dry-run", "json"].includes(name)) flags.set(name, true);
  else usage(`unknown flag ${arg}`);
}

function usage(message: string): never {
  console.error(`localstudio: ${message}\nusage: localstudio install [--t3-home <dir>] [--omp-dir <dir>] [--controller <url>] [--dry-run] [--json]\n       localstudio probe [--json]`);
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
} else usage(command ? `unknown command ${command}` : "no command");
