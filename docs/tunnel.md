# One tunnel per machine

Decision, 2026-11-21 (Claude, tunnels lead, with Donovan's answers). Donovan, 2026-11-21: "1 tunnel for cua and bua, and also this tunnel is the same localstudio will use for syncing across devices." Then: the tunnel is Local Studio's, and Polycarp's name leaves it.

A machine has at most one public route: one cloudflared process with one ingress, or one `tailscale serve` config on port 443. Local Studio owns it on every route. It carries both products:

| Paths | Goes to |
| --- | --- |
| `/v1/health`, `/v1/ws` | local-studio-serve: the tunnel listener (18094) on a Cloudflare route, the local listener (18093) on the tailnet route (docs/remote.md, Tunnel listener contract) |
| `/mcp`, `/.well-known/oauth-protected-resource`, `/.well-known/oauth-protected-resource/mcp`, `/.well-known/oauth-authorization-server`, `/.well-known/oauth-authorization-server/mcp`, `/authorize`, `/token`, `/register`, `/revoke` | Tychicus serve (8787): browser use and computer use |
| anything else | 404 |

These are the eleven paths of baney75/local-studio-tunnels docs/INTERFACE.md. One hostname serves both products, split by these exact paths: the tailnet route has one name per machine, and on the hosted route a second name would halve the zone's tunnel cap.

## The route record

`<data dir>/tunnel/route.json`, where the data dir is `$LOCAL_STUDIO_DATA_DIR` or `~/.local-studio`. Mode 0600 in a 0700 folder. Local Studio writes it; Tychicus reads it.

```json
{
  "v": 1,
  "owner": "local-studio",
  "route": "cloudflare",
  "hostname": "studio.example.com",
  "aliases": [],
  "ports": { "mcp": 8787, "serve": 18094 }
}
```

- `route` is `hosted` (Local Studio's waitlist service), `cloudflare` (the person's own account and domain), or `tailnet` (`tailscale serve`, no Funnel).
- `aliases` are extra hostnames on the same tunnel with the same routes, kept only while a connector moves to `hostname`.
- Ports are loopback, 1024 to 65535, different from each other, and never 18091 (the controller).

## Local Studio sets it up

```
local-studio tunnel setup --cloudflare <name>   the person's own Cloudflare account
local-studio tunnel setup --tailnet             tailnet only, no Funnel
local-studio tunnel request                     the hosted route (waitlist)
local-studio tunnel adopt                       take over a Tychicus tunnel already on this machine
local-studio tunnel status                      the route, its health and the next step
local-studio tunnel off                         stop and remove the route; the Cloudflare tunnel and DNS stay
```

### Dev channel only, for now

Donovan, 2026-11-21: tunnel setup ships on the dev channel only (docs/channels.md). On stable and nightly, `setup`, `adopt` and `request` don't run and the settings screen shows a waitlist signup instead. The gate reads `local_studio_core::CHANNEL`, which builds stamp from `LOCAL_STUDIO_BUILD_CHANNEL` and local builds set to `dev`. `status` and `off` work on every channel, so a machine that moves off dev can still see and remove its route. The waitlist is a signup-only Worker on a free workers.dev address that stores emails and holds no Cloudflare API token; it moves to the hosted route's own account when that exists.

### Agents can run it

An agent must be able to set a tunnel up for a person. Every `tunnel` command takes `--json`, never prompts when stdin isn't a terminal, and returns one state with the next step: `ready`, `needs_signin` (the person signs in to Cloudflare or Tailscale; the state carries the URL), `needs_dns`, `waitlist` (stable or nightly; carries the signup URL), or `error` with a code. `--yes` accepts the steps that change the machine, such as adopting a Tychicus tunnel. Signing in stays with the person: an agent hands over the URL and polls `status --json`.

### Going live

Before a route carries a path, Local Studio checks auth there (baney75/local-studio-tunnels docs/DESIGN.md, Going live): `/mcp` answers 401 with `WWW-Authenticate` naming the resource metadata, and `/v1/health` answers 200 for the route's hostname and 403 for any other Host. It registers the hostname with `local-studio-serve allow-tunnel-host <name>`. The Cloudflare route uses the pinned, verified cloudflared, the same release and digests as Tychicus's download, and reuses Tychicus's copy when its digest matches.

## Tychicus joins

`tychicus remote join` reads the record, stops any tunnel of its own without deleting it, and serves on `ports.mcp` with `https://<hostname>` as its public address, answering OAuth on each alias too. On a machine with Local Studio, `tychicus remote setup` runs `local-studio tunnel setup` and then joins. On a machine without Local Studio, Tychicus keeps its own `remote setup` with the same exact paths.

`local-studio tunnel adopt` moves a Tychicus tunnel to Local Studio without a gap: it keeps the tunnel and its credentials, adds the new hostname, keeps the old one as an alias, starts Local Studio's service, then runs `tychicus remote join`. Any failure puts Tychicus's service back.

## Not yet

- The hosted route waits on Donovan's choice of a Cloudflare account and domain (`STABLE_SERVICE`).
- Tychicus's Funnel route (port 8443) opens the whole port, so it can't keep to exact paths; Local Studio doesn't use Funnel.
