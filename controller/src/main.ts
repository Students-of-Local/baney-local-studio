import { timingSafeEqual } from "node:crypto";
import { readFileSync } from "node:fs";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import { clusterSettings, makeCluster } from "./cluster.ts";
import { LOCAL_STUDIO_CONTROLLER_HEADER, LocalAuthorizeRequest, LocalClusterRequest, LocalDeployRequest, LocalDownloadRequest, type LocalGpu, LocalHelloRequest, type LocalHotplug, type LocalNode, LocalPeerRequest, LocalPoolRequest, type LocalRecipes, LocalReleaseRequest, LocalRpcRequest, LocalRunRequest, type LocalSharePreview, LocalShareRequest, LocalNameRequest, type LocalSnapshot, type LocalThunderbolt } from "./contracts.ts";
import { decodeJson, errorResponse, exec, fail, type HttpError, httpError, json, loadConfig, now, port, saveConfig, VERSION } from "./core.ts";
import { type Endpoint, listeners, makeScanner } from "./discovery.ts";
import { autoModel, HOP_HEADER, inFlight, listModels, passthrough, routes } from "./gateway.ts";
import { graphUsage, makeGraph, makeUsage, normUrl } from "./graph.ts";
import { authorize, hardware, type RawGpu, readGpuList, readGpus, readThunderbolt } from "./hardware.ts";
import { makeJobs } from "./jobs.ts";
import { browse, detect, download, type Library, loadLibrary, matchHardware, record } from "./library.ts";
import { type Catalog, loadCatalog, matchCard, toLocalRecipe } from "./registry.ts";
import { createPr, previewShare, secretValues } from "./share.ts";
import { deployController, scanTailnet } from "./tailnet.ts";
import { makeTelemetry } from "./telemetry.ts";

let config = loadConfig();
let catalog: Catalog = { info: { commit: null, error: "loading" }, cards: {}, recipes: new Map(), archived: [] };
let rawGpus: RawGpu[] = [];
let known: Map<string, RawGpu> | null = null;
let thunderbolt: LocalThunderbolt[] = [];
let hotplug: LocalHotplug[] = [];
let library: Library | null = null;
const host = process.env.LOCAL_STUDIO_CONTROLLER_HOST ?? "127.0.0.1";
const shares = new Map<number, { preview: LocalSharePreview; at: number }>();
const usage = makeUsage();
const telemetry = makeTelemetry();

const gpus = (): LocalGpu[] => {
  const reserved = jobs.reserved();
  return rawGpus.map(({ uuid: _uuid, pids: _pids, ...g }) => ({ ...g, card: matchCard(catalog, g.name, g.memoryTotalMiB), busy: g.busy || reserved.has(g.index) }));
};

const byCard = (all: boolean): Map<string, number[]> => {
  const out = new Map<string, number[]>();
  for (const g of gpus()) if (g.card && (all || !g.busy)) out.set(g.card, [...(out.get(g.card) ?? []), g.index]);
  return out;
};
const freeGpus = () => byCard(false);

const jobs = makeJobs({
  config: () => config,
  catalog: () => catalog,
  freeGpus,
  listening: () => Effect.map(listeners, (ls) => new Set([...(ls ?? []).map((l) => l.port), ...cluster.poolPorts()])),
  uuids: (indices) => indices.flatMap((i) => rawGpus.filter((g) => g.index === i).map((g) => g.uuid)),
  indexOf: (uuid) => rawGpus.find((g) => g.uuid === uuid)?.index ?? null,
});
const scanner = makeScanner(config.id, (p) => jobs.jobPort(p) ?? cluster.poolPort(p));

const localEndpoints = (): Endpoint[] => {
  const lent = new Set(jobs.active().filter((j) => j.lent).map((j) => j.id));
  const loads = telemetry.loads();
  return scanner.current().map((ep) => ({
    ...ep,
    ...(ep.jobId && lent.has(ep.jobId) ? { lent: true } : {}),
    load: { inFlight: inFlight(ep.id), running: null, waiting: null, kvPercent: null, decodeTps: null, ...loads.get(ep.port) },
  }));
};

const selfNode = (): LocalNode => ({
  controller: { id: config.id, name: config.name, url: config.url, version: VERSION },
  endpoints: localEndpoints().map(({ base: _base, ...ep }) => ep),
  usage: usage.list(),
  peers: config.peers,
  cluster: cluster.node(),
});
const graph = makeGraph(() => config, selfNode);
const cluster = makeCluster({
  config: () => config,
  save: (c) => {
    config = c;
    saveConfig(c);
  },
  gpus: () => rawGpus,
  graph: () => graph.get(),
  invalidate: () => graph.invalidate(),
  jobs,
  listenHost: host,
  port,
  takenPorts: () => Effect.map(listeners, (ls) => new Set([...(ls ?? []).map((l) => l.port), ...jobs.active().map((j) => j.port)])),
});
const gw = { config: () => config, local: localEndpoints, usage: () => usage.list(), graph: () => graph.get(), record: usage.record, cluster: () => cluster.node(), failoverMs: () => cluster.failoverMs(), refuse: (ep: Endpoint) => cluster.refuse(ep) };

const excluded = (): Set<number> => {
  const out = new Set([port, ...config.excludePorts, ...cluster.rpcPorts()]);
  for (const p of config.peers) {
    const u = URL.parse(p);
    if (u && ["127.0.0.1", "localhost", "::1", "[::1]"].includes(u.hostname)) out.add(Number(u.port || 80));
  }
  return out;
};

const tick = Effect.suspend(() => scanner.scan(excluded(), config.engineKeys));

const suggest = (g: RawGpu): string[] => {
  const card = matchCard(catalog, g.name, g.memoryTotalMiB);
  const fromCatalog = card ? [...catalog.recipes].map(([k, e]) => toLocalRecipe(k, e, freeGpus(), byCard(true))).filter((r) => r.card === card && r.cards <= 1 && r.fits).map((r) => `${r.model} (${r.engine})`) : [];
  const fromLibrary = library ? browse(library, matchHardware(library, [{ name: g.name, memoryGb: g.memoryTotalMiB / 1024, count: 1 }]), false).models.slice(0, 3).map((m) => m.repo) : [];
  return [...new Set([...fromCatalog, ...fromLibrary])].slice(0, 5);
};

const note = (e: Omit<LocalHotplug, "at">) => {
  const cutoff = Date.now() - 3600_000;
  hotplug = [{ ...e, at: now() }, ...hotplug.filter((h) => Date.parse(h.at) > cutoff)].slice(0, 8);
};

const watchHardware = Effect.gen(function* () {
  const next = yield* readGpuList;
  thunderbolt = yield* readThunderbolt;
  if (!next) return;
  const prev = known;
  rawGpus = next;
  known = new Map(next.map((g) => [g.uuid, g]));
  if (!prev) return;
  for (const [uuid, g] of prev) {
    if (known.has(uuid)) continue;
    const why = `${g.name} was unplugged.`;
    for (const j of jobs.onGpu(uuid, g.index)) yield* Effect.result(jobs.stop(j.id, why));
    yield* Effect.promise(() => cluster.gpuGone(uuid, why));
    note({ change: "removed", gpu: g.name, memoryTotalMiB: g.memoryTotalMiB, external: !!g.external, message: why, suggestions: [] });
  }
  for (const g of next) {
    if (prev.has(g.uuid)) continue;
    const ideas = suggest(g);
    const gb = Math.round(g.memoryTotalMiB / 1024);
    const how = g.external ? `${g.name} connected over Thunderbolt or USB4${g.link ? `: ${g.link.note}` : "."}` : `${g.name} is ready.`;
    note({ change: "added", gpu: g.name, memoryTotalMiB: g.memoryTotalMiB, external: !!g.external, message: ideas.length ? how : `${how} No registry recipe fits its ${gb} GB yet.`, suggestions: ideas });
  }
});

const snapshot = async (): Promise<LocalSnapshot> => {
  const g = await graph.get();
  const live = await routes(gw);
  const totals = graphUsage(usage.list(), g);
  const endpoints = [...selfNode().endpoints, ...g.nodes.flatMap((n) => n.node.endpoints)];
  const ids = new Set([...endpoints.flatMap((e) => e.models), ...totals.keys()]);
  return {
    controller: selfNode().controller,
    generatedAt: now(),
    hardware: hardware(gpus(), thunderbolt, hotplug),
    auto: await autoModel(gw),
    endpoints,
    models: [...ids].sort().map((id) => ({ id, live: live.has(id), requests: totals.get(id)?.requests ?? 0 })),
    controllers: g.links,
    jobs: jobs.list(),
    usage: [...totals.values()],
    cluster: await cluster.view(),
  };
};

const recipes = (archived: boolean): LocalRecipes => {
  const all = byCard(true);
  const list = [...catalog.recipes].map(([k, e]) => toLocalRecipe(k, e, freeGpus(), all));
  return { recipes: archived ? [...list, ...catalog.archived].filter((r) => r.cards <= (all.get(r.card)?.length ?? 0)) : list };
};

const containerOf = (pid: number | null): string | null => {
  try {
    return pid ? (/([0-9a-f]{64})/.exec(readFileSync(`/proc/${pid}/cgroup`, "utf8"))?.[1] ?? null) : null;
  } catch {
    return null;
  }
};

const unload = (p: number) =>
  Effect.gen(function* () {
    const ep = scanner.current().find((e) => e.port === p && e.live);
    if (!ep) return yield* fail(404, "NOT_RUNNING", `nothing is serving on port ${p}`);
    const pool = cluster.poolPort(p);
    if (pool) return yield* cluster.stopPool(pool);
    if (ep.jobId) return yield* jobs.stop(ep.jobId);
    const published = (yield* exec(["docker", "ps", "-q", "--filter", `publish=${p}`], 10_000)).stdout.split("\n").filter(Boolean);
    const ids = [...published, containerOf(ep.pid)].filter((id): id is string => !!id);
    const pid = ep.pid;
    if (ids.length) {
      const r = yield* exec(["docker", "stop", "-t", "30", ...ids], 90_000);
      if (r.code !== 0) return yield* fail(500, "UNLOAD_FAILED", r.stderr.trim().slice(0, 300));
    } else if (pid) yield* Effect.try({ try: () => process.kill(pid, "SIGTERM"), catch: (e) => httpError(500, "UNLOAD_FAILED", String(e)) });
    else return yield* fail(409, "UNLOAD_UNSUPPORTED", `can't find the process serving port ${p}`);
    yield* tick;
    return { port: p };
  });

const load = (recipeId: string, req: LocalRunRequest) =>
  Effect.gen(function* () {
    if (req.lend && clusterSettings(config).lend !== "never" && cluster.state() !== "lent") return yield* fail(409, "OWNER_ACTIVE", `${config.name} is not lending its GPU right now.`);
    if (req.replace && !req.dryRun) {
      for (const ep of scanner.current().filter((e) => e.live)) yield* unload(ep.port);
      const entry = catalog.recipes.get(recipeId);
      for (let i = 0; entry && i < 60 && (freeGpus().get(entry.card)?.length ?? 0) < (entry.launch.cards ?? 1); i++) {
        yield* Effect.sleep("2 seconds");
        rawGpus = yield* readGpus;
      }
    }
    return yield* jobs.run(recipeId, req);
  });

const lib = () => (library ? Effect.succeed(library) : fail(503, "REGISTRY_LOADING", catalog.info.error ?? "the registry is still loading"));
const matches = (l: Library) => Effect.map(detect(rawGpus), (d) => matchHardware(l, d));

const share = (port: number) =>
  Effect.gen(function* () {
    const ep = scanner.current().find((e) => e.port === port && e.live);
    if (!ep) return yield* fail(404, "NOT_RUNNING", `nothing is serving on port ${port}`);
    const recipeId = jobs.list().find((j) => j.id === ep.jobId)?.recipeId;
    const l = yield* lib();
    const preview = yield* previewShare(l, ep, (recipeId && catalog.recipes.get(recipeId)) || null, yield* matches(l), secretValues([config.fleetKey, ...Object.values(config.engineKeys)]), config.engineKeys[String(port)]);
    shares.set(port, { preview, at: Date.now() });
    return preview;
  });

const publish = (req: LocalShareRequest) =>
  Effect.gen(function* () {
    const cached = shares.get(req.port);
    if (!cached || Date.now() - cached.at > 15 * 60_000) return yield* fail(409, "PREVIEW_REQUIRED", "preview the share first");
    if (cached.preview.blockers.length || cached.preview.issues.length) return yield* fail(409, "SHARE_BLOCKED", [...cached.preview.blockers, ...cached.preview.issues].join("; "));
    if (!req.dryRun && !req.confirm) return yield* fail(400, "CONFIRMATION_REQUIRED", "sharing needs explicit confirmation");
    return yield* createPr(cached.preview, req.dryRun === true);
  });

const body = async <S extends Schema.Top>(req: Request, schema: S): Promise<S["Type"]> => {
  const text = await req.text();
  const value = decodeJson(schema, text || "{}");
  if (value === undefined) throw httpError(400, "BAD_REQUEST", "request body is not JSON matching the schema");
  return value;
};

const authorized = (req: Request): boolean => {
  const bearer = /^Bearer\s+(.+)$/i.exec(req.headers.get("authorization") ?? "")?.[1];
  const key = Buffer.from(bearer ?? req.headers.get("x-api-key") ?? req.headers.get("x-local-studio-key") ?? "");
  const want = Buffer.from(config.fleetKey);
  return key.length === want.length && timingSafeEqual(key, want);
};

const run = <A>(e: Effect.Effect<A, HttpError>) => Effect.runPromise(e);

const saved = <A>(next: typeof config, result: A) => {
  config = next;
  saveConfig(config);
  graph.invalidate();
  return json(result);
};

const handle = async (req: Request, peer: string | null): Promise<Response> => {
  const url = new URL(req.url);
  const p = url.pathname;
  const post = req.method === "POST";
  const match = (re: RegExp) => (post ? re.exec(p)?.[1] : undefined);
  if (req.method === "GET" && p === "/api/health") return json({ ok: true, id: config.id, name: config.name, version: VERSION, api: 2, cluster: clusterSettings(config).discover });
  if (post && p === "/api/cluster/hello") {
    if (!clusterSettings(config).discover || Number(req.headers.get("content-length") ?? 0) > 4096) return json({ error: { code: "NOT_FOUND", message: `${req.method} ${p}` } }, 404);
    return json(await run(cluster.hello(await body(req, LocalHelloRequest))));
  }
  if (!authorized(req)) return json({ error: { code: "UNAUTHORIZED", message: "fleet key required" } }, 401);
  if (req.method === "GET" && (p === "/v1/models" || p === "/models")) return listModels(gw);
  if (post && /^\/v1\/(chat\/completions|completions|messages|responses)$/.test(p)) return passthrough(gw, req, p);
  if (req.headers.get(HOP_HEADER)) return json({ error: { code: "NOT_FOUND", message: p } }, 404);
  if (req.method === "GET" && p === "/api/node") return json(selfNode());
  if (req.method === "GET" && p === "/api/snapshot") return json(await snapshot());
  if (req.method === "GET" && p === "/api/telemetry") return json(await Effect.runPromise(telemetry.view(rawGpus, jobs.list(), url.searchParams.get("history") === "1")));
  if (req.method === "GET" && p === "/api/recipes") return json(recipes(url.searchParams.get("archived") === "1"));
  if (req.method === "GET" && p === "/api/tailnet") return json(await Effect.runPromise(scanTailnet(new Set((await graph.get()).links.map((l) => normUrl(l.url))))));
  if (req.method === "GET" && p === "/api/registry") return json(await run(Effect.gen(function* () {
    const l = yield* lib();
    return browse(l, yield* matches(l), url.searchParams.get("all") === "1");
  })));
  const recordId = req.method === "GET" ? /^\/api\/registry\/records\/([^/]+)$/.exec(p)?.[1] : undefined;
  if (recordId) return json(await run(Effect.flatMap(lib(), (l) => record(l, decodeURIComponent(recordId)))));
  if (post && p === "/api/registry/download") {
    const { recipeId } = await body(req, LocalDownloadRequest);
    return json(await run(Effect.flatMap(lib(), (l) => download(l, recipeId))));
  }
  if (req.method === "GET" && p === "/api/registry/share") return json(await run(share(Number(url.searchParams.get("port")))));
  if (post && p === "/api/registry/share") return json(await run(publish(await body(req, LocalShareRequest))));
  const recipe = match(/^\/api\/recipes\/(.+)\/run$/);
  if (recipe) return json(await run(load(decodeURIComponent(recipe), await body(req, LocalRunRequest))));
  const unloadPort = match(/^\/api\/ports\/(\d+)\/stop$/);
  if (unloadPort) return json(await run(unload(Number(unloadPort))));
  const stopId = match(/^\/api\/runs\/([\w-]+)\/stop$/);
  if (stopId) return json(await run(jobs.stop(stopId)));
  if (post && p === "/api/tailnet/deploy") {
    const u = normUrl(await run(deployController(config, await body(req, LocalDeployRequest))));
    return saved({ ...config, peers: [...new Set([...config.peers.map(normUrl), u])] }, { url: u });
  }
  if (req.method === "GET" && p === "/api/cluster") return json(await cluster.view());
  if (post && p === "/api/cluster") {
    await cluster.configure(await body(req, LocalClusterRequest));
    graph.invalidate();
    return json(await cluster.view());
  }
  if (post && p === "/api/cluster/rpc") return json(await run(cluster.startRpc(await body(req, LocalRpcRequest), peer)));
  if (post && p === "/api/cluster/rpc/stop") return json(await run(cluster.stopRpcFor((await body(req, LocalRpcRequest)).id)));
  if (post && p === "/api/cluster/release") {
    const r = await body(req, LocalReleaseRequest);
    return json(await run(cluster.release(r.controllerId, r.reason)));
  }
  if (post && p === "/api/cluster/pool") return json(await run(cluster.launchPool(await body(req, LocalPoolRequest))));
  const poolId = match(/^\/api\/cluster\/pool\/([\w-]+)\/stop$/);
  if (poolId) return json(await run(cluster.stopPool(poolId)));
  if (post && p === "/api/thunderbolt/authorize") {
    if (!/^(::ffff:)?127\.|^::1$/.test(peer ?? "")) return json({ error: { code: "LOCAL_ONLY", message: `Approve a Thunderbolt device on ${config.name} itself. Approving one gives it direct memory access, so it is never done over the network.` } }, 403);
    const r = await run(authorize((await body(req, LocalAuthorizeRequest)).uuid));
    thunderbolt = await Effect.runPromise(readThunderbolt);
    return json(r);
  }
  if (post && p === "/api/name") {
    const { name } = await body(req, LocalNameRequest);
    return saved({ ...config, name }, { name });
  }
  if (post && p === "/api/peers") {
    const b = await body(req, LocalPeerRequest);
    const u = normUrl(b.url);
    if (!/^https?:\/\/[^/]+$/.test(u)) throw httpError(400, "BAD_URL", "peer url must be http(s)://host:port");
    if (u === normUrl(config.url)) throw httpError(400, "SELF_PEER", "a controller cannot peer with itself");
    const peers = b.remove ? config.peers.filter((x) => normUrl(x) !== u) : [...new Set([...config.peers.map(normUrl), u])];
    const id = b.remove ? (await graph.get()).links.find((l) => normUrl(l.url) === u && !l.self)?.id : undefined;
    const was = (config.cluster?.ignored ?? []).filter((x) => x !== u);
    const ignored = b.remove ? [...new Set([...was, u, ...(id ? [id] : [])])] : was;
    return saved({ ...config, peers, ...(config.cluster || b.remove ? { cluster: { ...config.cluster, ignored } } : {}) }, { peers });
  }
  return json({ error: { code: "NOT_FOUND", message: `${req.method} ${p}` } }, 404);
};

const serve = (hostname: string) =>
  Bun.serve({
    port,
    hostname,
    idleTimeout: 0,
    maxRequestBodySize: 256 * 1024 * 1024,
    fetch: async (req, server) => {
      const res = await handle(req, server.requestIP(req)?.address ?? null).catch(errorResponse);
      res.headers.set(LOCAL_STUDIO_CONTROLLER_HEADER, config.id);
      return res;
    },
  });
serve(host);

let tailnetServer: { ip: string; server: ReturnType<typeof serve> } | null = null;
const listenTailnet = async () => {
  const want = clusterSettings(config).discover && /^(127\.|localhost$)/.test(host) ? await cluster.tailnet() : null;
  if (tailnetServer && tailnetServer.ip !== want) {
    void tailnetServer.server.stop();
    cluster.listening(`http://${tailnetServer.ip}:${port}`, false);
    tailnetServer = null;
  }
  if (!want || tailnetServer || want === host) return;
  try {
    tailnetServer = { ip: want, server: serve(want) };
    cluster.listening(`http://${want}:${port}`, true);
  } catch (e) {
    console.error(`could not listen on the tailnet address ${want}:${port}: ${String(e)}`);
  }
};

for (const signal of ["SIGTERM", "SIGINT"] as const) process.on(signal, () => void cluster.shutdown().finally(() => process.exit(0)));

const loop = async (ms: number, f: () => Promise<unknown>) => {
  for (;;) {
    await f().catch((e) => console.error(String(e)));
    await Bun.sleep(ms);
  }
};

console.log(`local-controller ${config.id} listening on ${host}:${port}`);
void loop(30 * 60_000, async () => {
  catalog = await Effect.runPromise(loadCatalog(config.registry, true));
  if (catalog.info.commit && catalog.info.commit !== library?.commit) library = await Effect.runPromise(loadLibrary(config.registry.dir, catalog.info.commit));
  console.log(`registry ${catalog.info.commit ?? "-"} ${catalog.recipes.size} recipes, ${catalog.archived.length} archived records, ${library?.rows.length ?? 0} indexed${catalog.info.error ? ` (${catalog.info.error})` : ""}`);
});
void Effect.runPromise(jobs.reconcile);
void loop(10_000, () => Effect.runPromise(scanner.scan(excluded(), config.engineKeys)));
void loop(5_000, () => Effect.runPromise(telemetry.sample(scanner.current(), config.engineKeys)));
void loop(2_000, async () => {
  await Effect.runPromise(watchHardware);
  await cluster.tick();
  await cluster.watchPools();
});
void loop(15_000, async () => {
  await listenTailnet();
  await cluster.discover();
});
