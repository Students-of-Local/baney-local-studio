import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";

const LINK_PORT = 18095;
const root = mkdtempSync(join(tmpdir(), "local-studio-link-peers-"));
const key = randomBytes(24).toString("hex");
const machine = randomBytes(16).toString("hex");
const linkUrl = `http://127.0.0.1:${LINK_PORT}/m/${machine}`;
const auth = { authorization: `Bearer ${key}`, "content-type": "application/json" };
const servers = [];
const children = [];
const hits = { link: [], tailnet: [], trap: 0, control: 0 };
const state = { nodeDelay: 400, tailnetRunning: 0 };

const listen = async (server, port = 0) => {
  servers.push(server);
  server.listen(port, "127.0.0.1");
  await once(server, "listening");
  return server.address().port;
};
const waitFor = async (fn, label, ms = 35_000) => {
  const end = Date.now() + ms;
  let last;
  while (Date.now() < end) {
    try {
      if (await fn()) return;
    } catch (error) {
      last = error;
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out: ${label}`, { cause: last });
};
const read = async (req) => {
  let raw = "";
  for await (const chunk of req) raw += chunk;
  return raw;
};
const json = (res, status, value) => {
  res.writeHead(status, { "content-type": "application/json" });
  res.end(JSON.stringify(value));
};
const endpoint = (controllerId, port, models, latencyMs, running) => ({
  id: `${controllerId}:${port}`,
  controllerId,
  port,
  pid: null,
  models,
  live: true,
  lastSeenAt: null,
  jobId: null,
  latencyMs,
  load: { inFlight: 0, running, waiting: 0, kvPercent: 0, decodeTps: 0 },
});
const lending = () => ({
  discover: true,
  lend: "always",
  state: "lent",
  reason: "lending",
  since: new Date().toISOString(),
  rpc: true,
  rpcEndpoint: "127.0.0.1:50052",
  gpus: 1,
  freeMiB: 24_000,
});

try {
  const trap = await listen(
    createServer((req, res) => {
      hits.trap++;
      json(res, 404, {});
    }),
  );
  const control = await listen(
    createServer((req, res) => {
      hits.control++;
      json(res, 404, {});
    }),
  );
  const tailnet = await listen(
    createServer(async (req, res) => {
      const raw = await read(req);
      if (req.url === "/api/node")
        return json(res, 200, {
          controller: { id: "tailnet-t", name: "Tailnet T", url: `http://127.0.0.1:${tailnet}`, version: "0.1.0" },
          endpoints: [endpoint("tailnet-t", 9001, ["shared"], 7, state.tailnetRunning)],
          usage: [],
          peers: [`http://127.0.0.1:${control}`],
          cluster: lending(),
        });
      if (req.url === "/v1/chat/completions") {
        hits.tailnet.push({ raw, headers: req.headers });
        return json(res, 200, { served: "tailnet" });
      }
      json(res, 404, {});
    }),
  );
  const proxy = createServer(async (req, res) => {
    const raw = await read(req);
    hits.link.push({ method: req.method, url: req.url, headers: req.headers, raw });
    if (req.url === `/m/${machine}/api/node`) {
      await new Promise((resolve) => setTimeout(resolve, state.nodeDelay));
      return json(res, 200, {
        controller: { id: "link-a", name: "Linked A", url: "http://10.9.9.9:18091", version: "0.1.0" },
        endpoints: [endpoint("link-a", 9000, ["link-only", "shared"], 5, 0)],
        usage: [],
        peers: [`http://127.0.0.1:${trap}`, "http://10.9.9.10:18091"],
        cluster: lending(),
      });
    }
    if (req.url === `/m/${machine}/v1/chat/completions`) return json(res, 200, { served: "link" });
    json(res, 404, {});
  });
  try {
    await listen(proxy, LINK_PORT);
  } catch (error) {
    throw new Error(`port ${LINK_PORT} must be free for this test (the desktop link proxy's port)`, { cause: error });
  }
  const scratch = createServer();
  const port = await listen(scratch);
  await new Promise((resolve) => scratch.close(resolve));
  const home = join(root, "controller");
  mkdirSync(home);
  const llama = join(root, "llama-server");
  writeFileSync(llama, "#!/bin/sh\nexit 1\n", { mode: 0o755 });
  const gguf = join(root, "model.gguf");
  writeFileSync(gguf, "");
  const tailnetUrl = `http://127.0.0.1:${tailnet}`;
  writeFileSync(
    join(home, "config.json"),
    JSON.stringify({
      id: "ctrl-b",
      name: "Controller B",
      url: `http://127.0.0.1:${port}`,
      fleetKey: key,
      peers: [tailnetUrl],
      excludePorts: [trap, control, tailnet],
      engineKeys: {},
      registry: { url: join(root, "no-registry"), dir: join(root, "registry"), ref: "origin/main" },
      modelsDir: join(root, "models"),
      cluster: { llamaServer: llama },
    }),
    { mode: 0o600 },
  );
  const child = spawn("bun", [new URL("../src/main.ts", import.meta.url).pathname], {
    env: {
      ...process.env,
      HOME: root,
      LOCAL_STUDIO_CONTROLLER_HOME: home,
      LOCAL_STUDIO_CONTROLLER_PORT: String(port),
      LOCAL_STUDIO_CONTROLLER_HOST: "127.0.0.1",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let log = "";
  for (const stream of [child.stdout, child.stderr]) stream.on("data", (chunk) => (log += chunk));
  children.push(child);
  const call = (path, body) =>
    fetch(`http://127.0.0.1:${port}${path}`, {
      headers: auth,
      ...(body === undefined ? {} : { method: "POST", body: JSON.stringify(body) }),
      signal: AbortSignal.timeout(10_000),
    });
  const get = async (path) => (await call(path)).json();
  const config = () => JSON.parse(readFileSync(join(home, "config.json"), "utf8"));
  await waitFor(async () => {
    if (child.exitCode !== null) throw new Error(log);
    return (await call("/api/health")).ok;
  }, "controller health");

  for (const url of [
    `https://127.0.0.1:${LINK_PORT}/m/${machine}`,
    `http://localhost:${LINK_PORT}/m/${machine}`,
    `http://10.0.0.1:${LINK_PORT}/m/${machine}`,
    `http://127.0.0.1:${LINK_PORT + 1}/m/${machine}`,
    `http://127.0.0.1:${LINK_PORT}/m/`,
    `http://127.0.0.1:${LINK_PORT}/m/${machine}/v1`,
    `http://127.0.0.1:${LINK_PORT}/m/${machine}?x=1`,
    `http://127.0.0.1:${LINK_PORT}/n/${machine}`,
    `http://127.0.0.1:${LINK_PORT}`,
    tailnetUrl,
  ]) {
    const r = await call("/api/peers", { url, via: "link" });
    assert.equal(r.status, 400, `via link accepted ${url}`);
  }
  assert.equal((await call("/api/peers", { url: linkUrl })).status, 400, "a link url was accepted as a tailnet peer");
  assert.equal((await call("/api/peers", { url: linkUrl, via: "tunnel" })).status, 400);
  assert.equal(config().linkPeers, undefined, "a refused link peer was stored");
  const added = await call("/api/peers", { url: `${linkUrl}/`, via: "link" });
  assert.equal(added.status, 200);
  assert.deepEqual((await added.json()).linkPeers, [linkUrl]);
  assert.deepEqual(config().linkPeers, [linkUrl]);
  assert.deepEqual(config().peers, [tailnetUrl], "the link peer leaked into peers");
  assert.deepEqual((await get("/api/node")).peers, [tailnetUrl], "the link peer is advertised to other controllers");

  await waitFor(async () => {
    const models = await get("/v1/models");
    return models.data.some((m) => m.id === "link-only" && m.owned_by === "link-a");
  }, "the gateway lists the linked machine's model");
  const nodeReads = hits.link.filter((h) => h.url === `/m/${machine}/api/node`);
  assert(nodeReads.length > 0, "the link peer's /api/node was never read");
  for (const h of nodeReads) {
    assert.equal(h.method, "GET");
    assert.equal(h.headers.authorization, `Bearer ${key}`, "the link peer was not read with this machine's fleet key");
  }

  await waitFor(async () => hits.control > 0, "a tailnet peer's own peers are crawled (control)");
  assert.equal(hits.trap, 0, "the controller crawled a peer listed by a link peer");
  const crawled = hits.link.filter((h) => h.url !== `/m/${machine}/api/node` && h.url !== `/m/${machine}/v1/chat/completions`);
  assert.deepEqual(crawled.map((h) => h.url), [], "the controller asked the link for something besides /api/node");

  const snap = await get("/api/snapshot");
  const linked = snap.controllers.find((c) => c.id === "link-a");
  assert.equal(linked?.via, "link");
  assert.equal(linked?.url, linkUrl);
  assert.equal(snap.controllers.find((c) => c.id === "tailnet-t")?.via, undefined);
  const linkEndpoint = snap.endpoints.find((e) => e.id === "link-a:9000");
  assert(linkEndpoint.latencyMs >= state.nodeDelay, `latencyMs ${linkEndpoint.latencyMs} was not measured through the link`);
  assert.equal(snap.endpoints.find((e) => e.id === "tailnet-t:9001").latencyMs, 7);

  const cluster = await get("/api/cluster");
  assert(cluster.machines.some((m) => m.id === "tailnet-t"), "the tailnet peer is not offered for lending (control)");
  assert(!cluster.machines.some((m) => m.id === "link-a"), "the link peer is offered for lending");
  const pool = await call("/api/cluster/pool", { model: gguf, members: ["link-a"] });
  assert.equal(pool.status, 409);
  assert.equal((await pool.json()).error.code, "NO_LENDERS");
  const dry = await call("/api/cluster/pool", { model: gguf, dryRun: true });
  assert.equal(dry.status, 200);
  assert.deepEqual((await dry.json()).members, ["Tailnet T"], "a dry-run pool did not pick exactly the tailnet lender");
  assert(!hits.link.some((h) => h.url.includes("/api/cluster")), "the link peer got a cluster or rpc request");

  const chat = async () => {
    const r = await call("/v1/chat/completions", { model: "shared", messages: [] });
    assert.equal(r.status, 200);
    return (await r.json()).served;
  };
  assert.equal(await chat(), "tailnet", "at equal load the link copy ranked before the tailnet copy");
  state.tailnetRunning = 3;
  await waitFor(async () => (await chat()) === "link", "the link copy serves once the tailnet copy is busier");
  const routed = hits.link.findLast((h) => h.url === `/m/${machine}/v1/chat/completions`);
  assert.equal(routed.headers.authorization, `Bearer ${key}`);
  assert.equal(routed.headers["x-local-studio-hop"], "1");
  assert.equal(routed.headers["x-local-studio-endpoint"], "link-a:9000");
  const only = await call("/v1/chat/completions", { model: "link-only", messages: [] });
  assert.equal((await only.json()).served, "link");

  const removed = await call("/api/peers", { url: linkUrl, via: "link", remove: true });
  assert.equal(removed.status, 200);
  assert.deepEqual(config().linkPeers, []);
  assert.deepEqual(config().peers, [tailnetUrl]);
  assert(!(config().cluster?.ignored ?? []).includes(linkUrl), "removing a link peer touched the tailnet ignore list");
  await waitFor(async () => !(await get("/v1/models")).data.some((m) => m.id === "link-only"), "a removed link peer leaves the gateway");
  console.log(
    "PASS link peers: exact link urls only, a separate linkPeers list, leaf crawl, no pool, rpc or lending, ranked after tailnet copies at equal load, latency through the link.",
  );
} finally {
  for (const child of children) child.kill("SIGTERM");
  await Promise.all(children.map((child) => (child.exitCode === null ? once(child, "exit") : undefined)));
  for (const server of servers) {
    server.closeAllConnections();
    server.close();
  }
  rmSync(root, { recursive: true, force: true });
}
