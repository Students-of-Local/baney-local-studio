# Fleet over desktop links

Decision, 2026-11-22 (Claude, for #110). Donovan, 2026-11-22: peers reach each other over the built-in Tailscale, or over the person's own Cloudflare tunnels, and it must be super easy, fast and secure.

On a tailnet, controllers find each other and talk HTTP on port 18091 with the shared fleet key (docs/controller.md, Discovery). Off the tailnet there is no private network, and the controller never goes on the public tunnel (docs/tunnel.md): its API is broad (lending, pools, deploy over ssh), and one fleet key leaked from any machine would open every machine. So fleet traffic between machines that aren't on one tailnet rides the desktop link instead (docs/remote.md, Desktop links). That link is already end to end encrypted (Noise KK), keyed per device, and revocable on the serving machine, and it already crosses every route: the tailnet name, the person's own Cloudflare hostname, or the hosted route.

## What crosses a link

Routed requests only, in v1:

| From the client | The serving machine's controller sees |
| --- | --- |
| `GET /v1/models` | the same, with `x-local-studio-hop: 1` |
| `POST /v1/chat/completions`, `/v1/completions`, `/v1/messages`, `/v1/responses` | the same, with `x-local-studio-hop: 1` |
| `GET /api/node` | the same, with no hop header |

Anything else is refused by serve before it reaches the controller. The hop header is the controller's existing boundary: a hop request is served only by that machine's own engines, never forwarded again, honors the lending rules (a reclaimed lent engine refuses, a lent one counts the request), and every path but the model list and the four generation paths answers 404. So a linked machine can use the models the other machine runs and see its load, and nothing more: no lending changes, no pools, no deploy, no peers list edits, no registry downloads.

Layer splits (llama.cpp RPC pools, and later Fonsviva's node channel) never run over a link in v1. They need a raw, low-latency path that a Cloudflare route can't give, and the planner only splits over links it measures as fast enough. Two machines that want splits join one tailnet.

## Who may use it: the fleet scope

A paired device gets fleet access only when the serving machine's owner asks for it while minting the pairing:

```
local-studio fleet invite [--json]          on the machine that shares: mints a pairing with the fleet scope, prints the link
local-studio fleet join <link> [--json]     on the machine that uses it: pairs with that link and adds the machine as a fleet peer
```

The scope lives in the pending pairing on the serving machine (`devices.json`), never in the link, so the other side can't add it by editing the URL. When the handshake completes, that device's record gains `"scopes": ["fleet"]`. A phone paired the ordinary way has no scopes and gets `forbidden` from every fleet op. `local-studio-serve devices` lists the scope, and revoking the device (`local-studio-serve revoke <id>`) ends its fleet access with everything else. An agent can run both commands for a person: the only thing that crosses between the machines is the link, which the person copies or the agent passes.

`fleet join` without a link keeps today's meaning: turn on tailnet discovery.

## Wire

Two ops, added to protocol 2's op table (ops are additive: a server without them answers `not_remote`, and the client shows "update <machine> to use it in the fleet"):

- `fleet_http {method, path, query, headers, body, stream}` → answer `{status, headers, body}`, or with `stream: true` `{status, headers, stream: <id>}` followed by updates `{"type":"fleet_chunk","stream","data"}` and one `{"type":"fleet_end","stream","error"?}`, sent only to the device that asked.
- `fleet_cancel {stream}`: the client's caller went away; serve aborts its request to the controller.

`headers` passes only `content-type`, `accept` and `x-local-studio-endpoint`; serve sets `authorization: Bearer <its own fleet key>` (read from the controller's `config.json`, the same lookup `local-studio fleet` uses) and the hop header itself, and drops anything else. Bodies are base64 in the JSON frame, at most 6 MiB in each direction for a non-streaming request (413 otherwise; the JSON frame limit is 8 MiB). Each chunk carries what the controller's response stream gave, at most 64 KiB. Limits per device: 8 streams at once, 32 requests a minute beyond that are answered 429 (`remote/src/limits.rs` holds the numbers).

## The client side

The process that owns the data dir (the desktop app while it runs, otherwise `local-studio-serve`) already keeps every saved link connected (`core/src/link/client.rs`). For each linked machine whose pairing has the fleet scope it also serves a loopback proxy: one listener on `127.0.0.1:18095`, path prefix `/m/<machine id>`. A request there must carry this machine's own fleet key as a bearer token, like the controller's own API; the proxy strips it, sends `fleet_http` over the link, and streams the answer back. A closed client connection sends `fleet_cancel`. When the link is down the proxy answers 503 at once, so the gateway fails over to another copy.

`fleet join <link>` pairs, then adds `http://127.0.0.1:18095/m/<machine id>` to the controller's peers with `"via": "link"` (`POST /api/peers`). A peer URL is otherwise `http(s)://host:port` with no path; a `via: "link"` peer must instead be exactly `http://127.0.0.1:18095/m/<id>`, and the controller stores it in a separate `linkPeers` list so tailnet code never sees it. `forget_machine` removes that peer. The controller treats a link peer like any peer for routing, with three differences:

- It is a leaf: the graph reads its `/api/node` but never crawls the peers that node lists, since those addresses are only reachable from the other side.
- It never joins a pool, never gets an `rpc` request, and is never offered for lending.
- The gateway ranks it after tailnet copies of the same model at equal load, and its `latencyMs` is measured through the link.

## Not in v1

- Linking back: a link is one way. For each machine to use the other, both run `fleet invite` and `fleet join` once in each direction.
- The power budget governs fleet work this machine lends (rpc-servers and lent containers). A routed request to an engine this machine runs for itself is served as on the tailnet today; the Owner check and reclaim still stop lent engines.
- Sharing with another person's machines. The scope is the same, but the UI and docs assume one owner.

## Done check

Two scratch data dirs on one machine, no tailnet: B runs `local-studio fleet join` with A's `fleet invite` link over `wss://localhost` through link_smoke's test TLS terminator. B's gateway lists A's model, a streaming chat request to B is served by A's engine through the link with the hop header set, closing the client mid-stream cancels A's request, `POST /api/cluster` and `GET /api/snapshot` sent through B's proxy are refused by A's serve with `forbidden`, a phone paired the ordinary way gets `forbidden` from `fleet_http`, and revoking B on A makes B's next request fail over within one try.
