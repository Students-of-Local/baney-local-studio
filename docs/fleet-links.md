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
local-studio fleet invite [--url <wss url>] [--json]   on the machine that shares: mints a pairing with the fleet scope, prints the link
local-studio fleet join <link> [--json]                on the machine that uses it: pairs with that link and adds the machine as a fleet peer
```

The link points at this machine's public route when it has one (the `cloudflare` or `hosted` hostname in `<data dir>/tunnel/route.json`, as `wss://<hostname>/v1/ws`), then its tailnet name; `--url` overrides both and must be `wss://`. `local-studio-serve pair --fleet` mints the same pairing. The scope lives in the pending pairing on the serving machine (`devices.json`), never in the link, so the other side can't add it by editing the URL. When the handshake completes, that device's record gains `"scopes": ["fleet"]`. A phone paired the ordinary way has no scopes and gets `forbidden` from every fleet op. `local-studio-serve devices` lists the scope, and revoking the device (`local-studio-serve revoke <id>`) ends its fleet access with everything else. An agent can run both commands for a person: the only thing that crosses between the machines is the link, which the person copies or the agent passes.

`fleet join` without a link keeps today's meaning: turn on tailnet discovery.

## Wire

Two ops, added to protocol 2's op table (ops are additive: a server without them answers `not_remote`, and the client shows "update <machine> to use it in the fleet"):

- `fleet_http {method, path, query, headers, body, stream}` → answer `{status, headers, body}`, or with `stream` set `{status, headers, stream: <id>}` followed by `{"type":"fleet_chunk","stream","data"}` and one `{"type":"fleet_end","stream","error"?}`. `stream` is `true` (serve names the stream `~<n>`, which no client id can equal) or the client's own id of 1 to 64 letters, digits, `-` or `_`, unique among that device's open streams, so the client can cancel before the controller answers. Chunks and the end go only to the connection that asked, as `{"op":"fleet","update":{...}}` frames outside the update sequence: they are never buffered for resume, because the request ends with the connection.
- `fleet_cancel {stream}`: the client's caller went away; serve aborts its request to the controller. Closing the connection aborts every request it opened.

`headers` passes only `content-type`, `accept` and `x-local-studio-endpoint`; serve sets `authorization: Bearer <its own fleet key>` (read from the controller's `config.json`, the same lookup `local-studio fleet` uses) and the hop header itself, and drops anything else. Bodies are base64 in the JSON frame, which is at most 8 MiB. Base64 grows a body by a third and the envelope takes up to 4 KiB, so a non-streaming body may be at most 6,288,384 bytes (just under 6 MiB) in each direction: serve answers 413 past it, and the client side refuses a larger request body with 413 before sending it. Serve answers 504 when the controller sends no answer within 10 minutes or a body or stream goes 5 minutes without a byte. Each chunk carries what the controller's response stream gave, at most 64 KiB. Limits per device: 8 streams at once and 32 requests a minute; past either, 429 (`remote/src/limits.rs` holds the numbers).

## The client side

The process that owns the data dir (the desktop app while it runs, otherwise `local-studio-serve`) already keeps every saved link connected (`core/src/link/client.rs`). The client can't see a pairing's scope, so `fleet join` marks the saved link `"fleet": true` in `<data dir>/remote/links.json`, and only those links get a proxy route; a fleet invite pasted into the ordinary pairing sheet links the machine without one. While at least one fleet link is saved, that process serves a loopback proxy (`core/src/link/proxy.rs`): one listener on `127.0.0.1:18095`, path prefix `/m/<machine id>`. If the port is taken it warns once and keeps trying every 2 s. A request there must carry the fleet proxy token as a bearer token, compared in constant time; anything else, this machine's fleet key included, gets 401 and nothing is sent. The token is 32 random bytes from the OS, in hex, minted by the first `fleet join` and kept at 0600 in `<data dir>/remote/fleet-proxy-token` and, as `linkToken`, in the controller's `config.json` next to `linkPeers`. The controller sends it, never the fleet key, on every request to a link peer: anything else may hold port 18095 while the app and serve are closed, and the fleet key opens every controller that shares it, while the token opens only this proxy. The proxy stays off when it has no token. Forgetting the last fleet link deletes the token on both sides, so the next `fleet join` mints a new one. The proxy drops the token, sends `fleet_http` with only the allowed headers, and streams the answer back. Every POST goes as a stream with a name the proxy picks, so a closed client connection sends `fleet_cancel` even before the controller answers; GET answers come back whole.

| The proxy answers | When |
| --- | --- |
| whatever serve answered, 429 included | the request crossed the link |
| 401 `unauthorized` | the bearer is missing or not the fleet proxy token |
| 404 `not_found`, `unknown_machine` | the path has no `/m/<id>/` prefix, or no fleet link has that id |
| 413 `too_large` | the request body is over 6,288,384 bytes; a body up to four times that is read and dropped first so the client sees the answer |
| 503 `link_down`, with `x-local-studio-retry: 1` | the link is not connected (also after a revoke), so nothing was sent |
| 503 `link_lost` | the link dropped after the request was sent |
| 403 `forbidden`, 400 `bad_args` | serve refused the op |
| 502 `not_remote`, with `x-local-studio-retry: 1` | serve predates the fleet ops |

The 503 with the retry header lets the gateway fail over to another copy at once, streaming or not.

`fleet join <link>` asks the process that owns the data dir to pair, through the requests folder (`<data dir>/remote/requests/<id>.json` with `{"op":"fleet_join","link":...}`, answered in `<id>.done.json`), so the link lives in the process that runs the proxy. With nothing owning the data dir it refuses before using the invite. The desktop app answers that folder only while it serves phones in process (when `serve.json` exists); an app that doesn't leaves the command to time out after 30 s with a message saying so. It then adds `http://127.0.0.1:18095/m/<machine id>` to the controller's peers with `"via": "link"` and the proxy token as `token` (`POST /api/peers`; a link peer is refused without a token unless the controller already holds one). A peer URL is otherwise `http(s)://host:port` with no path; a `via: "link"` peer must instead be exactly `http://127.0.0.1:18095/m/<id>`, and the controller stores it in a separate `linkPeers` list so tailnet code never sees it. `forget_machine` removes that peer (`POST /api/peers` with `"via": "link", "remove": true`). The controller never scans port 18095 for engines. It treats a link peer like any peer for routing, with three differences:

- It is a leaf: the graph reads its `/api/node` but never crawls the peers that node lists, since those addresses are only reachable from the other side.
- It never joins a pool, never gets an `rpc` request, and is never offered for lending.
- The gateway ranks it after tailnet copies of the same model at equal load, and its `latencyMs` is measured through the link.

## Not in v1

- Linking back: a link is one way. For each machine to use the other, both run `fleet invite` and `fleet join` once in each direction.
- The power budget governs fleet work this machine lends (rpc-servers and lent containers). A routed request to an engine this machine runs for itself is served as on the tailnet today; the Owner check and reclaim still stop lent engines.
- Sharing with another person's machines. The scope is the same, but the UI and docs assume one owner.

## Done check

Two scratch data dirs on one machine, no tailnet: B runs `local-studio fleet join` with A's `fleet invite` link over `wss://localhost` through link_smoke's test TLS terminator. B's gateway lists A's model, a streaming chat request to B is served by A's engine through the link with the hop header set, closing the client mid-stream cancels A's request, `POST /api/cluster` and `GET /api/snapshot` sent through B's proxy are refused by A's serve with `forbidden`, a phone paired the ordinary way gets `forbidden` from `fleet_http`, and revoking B on A makes B's next request fail over within one try.

`fleet_link_client_smoke` runs it, with a stub controller in place of A's and a stub tailnet peer C for B to fail over to (AGENTS.md, checks).
