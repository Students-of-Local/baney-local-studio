import * as Schema from "effect/Schema";
import { LOCAL_STUDIO_CONTROLLER_HEADER, type LocalNodeCluster, type LocalUsage } from "./contracts.ts";
import { type Config, decodeJson } from "./core.ts";
import type { Endpoint } from "./discovery.ts";
import { type Graph, graphUsage, liveModels, pickAuto, type Route } from "./graph.ts";
import { observeUsage } from "./usage.ts";

export const HOP_HEADER = "x-local-studio-hop";
const PIN_HEADER = "x-local-studio-endpoint";
const FORWARD = ["content-type", "accept", "anthropic-version", "anthropic-beta", "openai-beta", "openai-organization", "openai-project", "x-request-id", "idempotency-key", "user-agent"];
const DROP = new Set(["content-length", "content-encoding", "transfer-encoding", "connection", "keep-alive", "upgrade", "proxy-authenticate", "trailer", "te"]);
const RETRY = new Set([502, 503, 504]);
const RETRY_HEADER = "x-local-studio-retry";
const CONNECT = /refused|ECONNREFUSED|ConnectionRefused|FailedToOpenSocket|EHOSTUNREACH|ENETUNREACH|ENOTFOUND|EAI_AGAIN|unable to connect/i;
const HEADERS_MS = 30 * 60_000;
const Body = Schema.Record(Schema.String, Schema.Unknown);

export interface GatewayDeps {
  config: () => Config;
  local: () => Endpoint[];
  usage: () => LocalUsage[];
  graph: () => Promise<Graph>;
  record: (model: string, tokens?: number | null) => void;
  cluster: () => LocalNodeCluster | null;
  failoverMs: () => number;
  refuse: (endpoint: Endpoint) => string | null;
}

const inflight = new Map<string, number>();
let lentHops = 0;
const bump = (id: string, by: number) => {
  const n = (inflight.get(id) ?? 0) + by;
  if (n > 0) inflight.set(id, n);
  else inflight.delete(id);
};

export const inFlight = (id: string): number => inflight.get(id) ?? 0;
export const lentInFlight = (): number => lentHops;

const apiError = (status: number, type: string, message: string): Response => Response.json({ error: { type, code: type, message } }, { status });

export const routes = async (d: GatewayDeps, hop = false) => liveModels(d.local(), hop ? { links: [], nodes: [] } : await d.graph(), d.cluster());

export const autoModel = async (d: GatewayDeps): Promise<string | null> => {
  const g = await d.graph();
  return pickAuto(liveModels(d.local(), g), graphUsage(d.usage(), g));
};

export const rank = (r: Route): number[] => {
  const l = r.endpoint.load;
  const queue = Math.max((l?.running ?? 0) + (l?.waiting ?? 0), l?.inFlight ?? 0, inFlight(r.endpoint.id));
  const away = r.endpoint.lent && r.cluster?.state === "reclaimed" ? 1 : 0;
  return [away, queue, Math.round((l?.kvPercent ?? 0) / 10), r.peer ? 1 : 0, r.depth, -Math.round(l?.decodeTps ?? 0)];
};

const byLoad = (a: Route, b: Route): number => {
  const [x, y] = [rank(a), rank(b)];
  return x.reduce((d, v, i) => d || v - (y[i] ?? 0), 0);
};

export const listModels = async (d: GatewayDeps): Promise<Response> => {
  const live = await routes(d);
  const data = ["auto", ...[...live.keys()].sort()].map((id) => ({
    id,
    object: "model",
    created: 0,
    owned_by: id === "auto" ? "local-studio" : [...new Set(live.get(id)?.map((r) => r.endpoint.controllerId))].join(","),
  }));
  return Response.json({ object: "list", data });
};

export const passthrough = async (d: GatewayDeps, req: Request, path: string): Promise<Response> => {
  const raw = await req.text();
  const body = decodeJson(Body, raw);
  if (!body || Array.isArray(body)) return apiError(400, "invalid_request_error", "request body must be a JSON object");
  const requested = body.model;
  if (typeof requested !== "string" || !requested) return apiError(400, "invalid_request_error", "model is required");
  const hop = req.headers.get(HOP_HEADER) === "1";
  const live = await routes(d, hop);
  const model = requested === "auto" && !hop ? await autoModel(d) : requested;
  if (!model) return apiError(503, "no_live_models", "no live models are reachable for auto");
  const pin = req.headers.get(PIN_HEADER);
  const candidates = (live.get(model) ?? []).filter((r) => !pin || r.endpoint.id === pin).sort(byLoad);
  if (!candidates.length) return apiError(404, "model_not_found", `model ${model} is not live on any reachable endpoint`);
  const payload = model === requested ? raw : JSON.stringify({ ...body, model });
  const cfg = d.config();
  const stream = body.stream === true;
  let lastError = "";
  let unsent = true;
  for (const [i, r] of candidates.entries()) {
    const more = i < candidates.length - 1;
    const local = r.peer ? null : d.local().find((e) => e.id === r.endpoint.id) ?? null;
    const refused = hop && local ? d.refuse(local) : null;
    if (refused) {
      lastError = refused;
      continue;
    }
    const headers = new Headers(FORWARD.flatMap((h) => { const v = req.headers.get(h); return v ? [[h, v] as [string, string]] : []; }));
    const key = r.peer ? cfg.fleetKey : cfg.engineKeys[String(r.endpoint.port)];
    if (key) headers.set("authorization", `Bearer ${key}`);
    if (r.peer) {
      headers.set(HOP_HEADER, "1");
      headers.set(PIN_HEADER, r.endpoint.id);
    }
    const counted = hop && !!local?.lent;
    const ac = new AbortController();
    const wait = stream && more ? d.failoverMs() : HEADERS_MS;
    const timer = setTimeout(() => ac.abort(new Error(`no answer within ${Math.round(wait / 100) / 10} s`)), wait);
    bump(r.endpoint.id, 1);
    if (counted) lentHops++;
    let done = false;
    const release = () => {
      if (done) return;
      done = true;
      bump(r.endpoint.id, -1);
      if (counted) lentHops--;
    };
    let res: Response;
    try {
      res = await fetch(`${r.base}${path}${new URL(req.url).search}`, { method: "POST", headers, body: payload, signal: AbortSignal.any([req.signal, ac.signal]), redirect: "manual", decompress: true } as RequestInit);
    } catch (e) {
      release();
      if (req.signal.aborted) return new Response(null, { status: 499 });
      const timedOut = ac.signal.aborted;
      lastError = timedOut ? String(ac.signal.reason instanceof Error ? ac.signal.reason.message : ac.signal.reason) : e instanceof Error ? e.message : String(e);
      const connect = !timedOut && CONNECT.test(`${(e as { code?: string } | null)?.code ?? ""} ${lastError}`);
      if (connect || stream) {
        if (!connect) unsent = false;
        continue;
      }
      return apiError(timedOut ? 504 : 502, "upstream_error", `${r.endpoint.id} did not finish: ${lastError}`);
    } finally {
      clearTimeout(timer);
    }
    const safe = !r.peer ? res.status === 503 : res.headers.get(RETRY_HEADER) === "1";
    if (more && RETRY.has(res.status) && (stream || safe)) {
      release();
      if (!safe) unsent = false;
      lastError = `HTTP ${res.status} from ${r.endpoint.id}`;
      await res.body?.cancel().catch(() => undefined);
      continue;
    }
    const out = new Headers([...res.headers].filter(([k]) => !DROP.has(k.toLowerCase()) && k.toLowerCase() !== RETRY_HEADER));
    out.set(PIN_HEADER, r.endpoint.id);
    out.set(LOCAL_STUDIO_CONTROLLER_HEADER, cfg.id);
    if (i > 0) out.set("x-local-studio-failover", String(i));
    if (safe && unsent && RETRY.has(res.status)) out.set(RETRY_HEADER, "1");
    const count = res.ok && !r.peer;
    const usage = count ? observeUsage(res.headers.get("content-type") ?? "", path === "/v1/messages") : null;
    const reader = res.body?.getReader();
    const piped = reader
      ? new ReadableStream<Uint8Array>({
          async pull(ctrl) {
            try {
              const { done, value } = await reader.read();
              if (!done) {
                usage?.push(value);
                return ctrl.enqueue(value);
              }
              release();
              if (count && !req.signal.aborted) d.record(model, usage?.finish());
              ctrl.close();
            } catch (e) {
              release();
              ctrl.error(e);
            }
          },
          cancel(reason) {
            release();
            return reader.cancel(reason);
          },
        })
      : null;
    req.signal.addEventListener("abort", release, { once: true });
    if (!piped) {
      release();
      if (count) d.record(model);
    }
    return new Response(piped, { status: res.status, statusText: res.statusText, headers: out });
  }
  const failed = apiError(502, "upstream_unreachable", `no endpoint for ${model} answered: ${lastError}`);
  if (unsent) failed.headers.set(RETRY_HEADER, "1");
  return failed;
};
