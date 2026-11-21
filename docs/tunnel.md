# One tunnel per machine

Decision, 2026-11-21 (Claude, tunnels lead). Donovan, 2026-11-21: "1 tunnel for cua and bua, and also this tunnel is the same localstudio will use for syncing across devices."

A machine has at most one public route: one cloudflared process with one ingress, or one `tailscale serve` config on port 443. That route carries both products:

| Paths | Goes to |
| --- | --- |
| `/v1/health`, `/v1/ws` | local-studio-serve: the tunnel listener (18094) on a Cloudflare route, the local listener (18093) on the tailnet route (docs/remote.md) |
| `/mcp`, `/.well-known/oauth-protected-resource`, `/.well-known/oauth-protected-resource/mcp`, `/.well-known/oauth-authorization-server`, `/.well-known/oauth-authorization-server/mcp`, `/authorize`, `/token`, `/register`, `/revoke` | Tychicus serve (8787): browser use and computer use |
| anything else | 404 |

These are the eleven paths of baney75/local-studio-tunnels docs/INTERFACE.md, on every route.

## The route record

`<data dir>/tunnel/route.json`, where the data dir is `$LOCAL_STUDIO_DATA_DIR` or `~/.local-studio`. Mode 0600 in a 0700 folder.

```json
{
  "v": 1,
  "owner": "tychicus",
  "route": "cloudflare",
  "hostname": "tychicus.example.com",
  "ports": { "mcp": 8787, "serve": 18094 }
}
```

- `owner` is `tychicus` or `local-studio`: the product that runs the tunnel process.
- `route` is `hosted` (Local Studio's waitlist service), `cloudflare` (the person's own account), or `tailnet` (`tailscale serve`, no Funnel).
- `ports.mcp` is Tychicus serve. `ports.serve` is where `/v1` goes; absent means Local Studio hasn't joined, and the owner routes no `/v1` paths.
- Ports are loopback, 1024 to 65535, different from each other, and never 18091 (the controller).

The owner writes the record when it sets the route up and removes it when it tears the route down. The owner renders its ingress from the record every time its tunnel starts, so a change to the record takes effect on the next start.

## Who owns which route

Whichever product sets up first owns the route it set up:

- **hosted**: Local Studio core (`core/src/tunnel.rs`). The service sets the ingress from the record's two ports; `PUT /v1/tunnel/ports` changes them.
- **cloudflare** and **tailnet**: Tychicus `remote setup --cloudflare <name>` or `--tailnet`. Tychicus already has the pinned and verified cloudflared, the Cloudflare sign-in, DNS and the service units. `local-studio tunnel byo` runs it.
- Tychicus's Funnel route (`--tailscale`, port 8443) opens the whole port, so it can't keep to exact paths. Local Studio doesn't join it; docs/remote.md keeps Funnel off the local listener.

## The second product joins

- Local Studio finds a Tychicus record: `local-studio tunnel attach` sets `ports.serve` and runs `tychicus remote reload`, which re-renders the ingress and restarts the tunnel. On failure the record goes back as it was. `local-studio-serve` then runs with `--allow-tunnel-host <hostname>` (Cloudflare) or `--allow-host <hostname>` (tailnet).
- Tychicus finds a Local Studio hosted record: `remote setup` starts no tunnel. It saves the `custom` route with `https://<hostname>` and serves on `ports.mcp`.
- Neither starts a second tunnel: `local-studio tunnel request` refuses while a Tychicus route exists, and Tychicus refuses `--cloudflare` or `--tailnet` while a hosted route exists.

Both products check auth before a route carries their paths (baney75/local-studio-tunnels docs/DESIGN.md, Going live): `/mcp` answers 401 with `WWW-Authenticate` naming the resource metadata, and `/v1/health` answers for the route's hostname and 403 for any other Host.

## Not yet

- Local Studio without Tychicus can't make a bring-your-own route: `tunnel byo` needs the Tychicus plugin.
- The hosted route waits on Donovan's choice of a Cloudflare account and domain (`STABLE_SERVICE`).
