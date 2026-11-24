import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { randomBytes } from "node:crypto";
import { once } from "node:events";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";

if (process.platform !== "linux") {
  console.log("SKIP deploy: it runs the installed controller on 127.0.0.2, which only Linux answers");
  process.exit(0);
}
const TARGET = "127.0.0.2";
const DEPLOY_PORT = 18091;
const root = mkdtempSync(join(tmpdir(), "local-studio-deploy-"));
const key = randomBytes(24).toString("hex");
const headers = { authorization: `Bearer ${key}`, "content-type": "application/json" };
const far = join(root, "far");
const farBin = join(root, "far-bin");
const nearBin = join(root, "near-bin");
const log = join(root, "commands.log");
for (const dir of [far, farBin, nearBin]) mkdirSync(dir);
const script = (path, text) => {
  writeFileSync(path, `#!/bin/sh\n${text}\n`);
  chmodSync(path, 0o755);
};
script(join(nearBin, "ssh"), `for command; do :; done\nprintf '%s\\n' "$command" >> "${log}"\nexec env -i HOME="${far}" PATH="${farBin}:/usr/bin:/bin" /bin/sh -c "$command"`);
script(join(farBin, "curl"), `printf 'curl %s\\n' "$*" >> "${log}"\nexit 7`);
script(join(farBin, "uname"), `case "$1" in -s) cat "${root}/os" ;; -m) cat "${root}/arch" ;; esac`);
for (const name of ["systemctl", "loginctl", "launchctl"]) script(join(farBin, name), `printf '${name} %s\\n' "$*" >> "${log}"\nexit 1`);
script(join(farBin, "setsid"), `shift\nprintf '%s\\n' "$$" >> "${root}/pids"\nexec "$@"`);
symlinkSync(execFileSync("which", ["bun"]).toString().trim(), join(farBin, "bun"));
const arch = { x64: "x86_64", arm64: "aarch64" }[process.arch];
const machine = (os, cpu) => {
  writeFileSync(join(root, "os"), `${os}\n`);
  writeFileSync(join(root, "arch"), `${cpu}\n`);
};
const scratch = createServer().listen(0, "127.0.0.1");
await once(scratch, "listening");
const port = scratch.address().port;
await new Promise((resolve) => scratch.close(resolve));
const home = join(root, "controller");
mkdirSync(home);
writeFileSync(join(home, "config.json"), JSON.stringify({ id: "near", name: "Near", url: `http://127.0.0.1:${port}`, fleetKey: key, peers: [], registry: { url: join(root, "none"), dir: join(root, "registry"), ref: "none" } }), { mode: 0o600 });
const child = spawn("bun", [new URL("../src/main.ts", import.meta.url).pathname], {
  env: { ...process.env, HOME: root, PATH: `${nearBin}:${process.env.PATH}`, LOCAL_STUDIO_CONTROLLER_HOME: home, LOCAL_STUDIO_CONTROLLER_PORT: String(port), LOCAL_STUDIO_CONTROLLER_HOST: "127.0.0.1" },
  stdio: ["ignore", "ignore", "inherit"],
});
const call = (path, body) => fetch(`http://127.0.0.1:${port}${path}`, { headers, method: body ? "POST" : "GET", ...(body ? { body: JSON.stringify(body) } : {}), signal: AbortSignal.timeout(170_000) });
const deploy = async () => {
  const r = await call("/api/tailnet/deploy", { ip: TARGET, name: "far box" });
  return { status: r.status, body: await r.json() };
};
const stopped = async () => {
  for (const pid of existsSync(join(root, "pids")) ? readFileSync(join(root, "pids"), "utf8").trim().split("\n").map(Number) : []) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {}
    while (existsSync(`/proc/${pid}`)) await new Promise((resolve) => setTimeout(resolve, 50));
  }
  rmSync(join(root, "pids"), { force: true });
  rmSync(far, { recursive: true, force: true });
  mkdirSync(far);
};
const installed = join(far, ".local/bin/local-studio-controller");
const took = async (label) => {
  const got = await deploy();
  assert.deepEqual([got.status, got.body.url], [200, `http://${TARGET}:${DEPLOY_PORT}`], `${label}: ${JSON.stringify(got.body)}`);
  const health = await (await fetch(`http://${TARGET}:${DEPLOY_PORT}/api/health`)).json();
  assert.equal(health.name, "far-box");
  const config = join(far, ".local-studio-controller/config.json");
  assert.equal(JSON.parse(readFileSync(config, "utf8")).fleetKey, key);
  assert.equal(statSync(config).mode & 0o777, 0o600);
  assert.equal(statSync(installed).mode & 0o777, 0o700);
  assert.deepEqual(readdirSync(join(far, ".local-studio-controller")).filter((name) => name === "upload"), []);
  const commands = readFileSync(log, "utf8");
  assert(!commands.includes(key), "the fleet key is on a command line");
  assert.deepEqual([...new Set(commands.match(/^curl .*$/gm))], [`curl -s -m 2 -o /dev/null http://${TARGET}:${DEPLOY_PORT}/api/health`], "something was downloaded");
};

try {
  for (let i = 0; ; i++) {
    const up = await call("/api/health").then((r) => r.ok, () => false);
    if (up) break;
    assert(i < 100, "the controller did not start");
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  machine("Linux", arch);
  await took("with Bun on the machine");
  assert.equal(readFileSync(installed, "utf8").split("\n")[0], `#!${join(farBin, "bun")}`);
  const again = await deploy();
  assert.equal(again.status, 502);
  assert.match(again.body.error.message, /already set up/);
  await stopped();

  rmSync(join(farBin, "bun"));
  await took("the same kind of machine, no Bun");
  assert.equal(readFileSync(installed).subarray(1, 4).toString(), "ELF");
  await stopped();

  machine("Darwin", "arm64");
  const refused = await deploy();
  assert.equal(refused.status, 502);
  assert.match(refused.body.error.message, /install Bun there first/);
  assert.deepEqual(readdirSync(far), [], "a refused machine was changed");
  console.log("PASS deploy: own code over ssh, nothing downloaded, the fleet key only on stdin, an existing controller and a machine without Bun refused untouched.");
} finally {
  await stopped();
  child.kill("SIGTERM");
  if (child.exitCode === null && child.signalCode === null) await once(child, "exit");
  rmSync(root, { recursive: true, force: true });
}
