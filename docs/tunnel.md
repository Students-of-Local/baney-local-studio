# One tunnel per machine

Decision, 2026-11-21 (Claude, tunnels lead, with Donovan's answers). Donovan, 2026-11-21: "1 tunnel for cua and bua, and also this tunnel is the same localstudio will use for syncing across devices." Then: the tunnel is Local Studio's, and Polycarp's name leaves it.

A machine has at most one public route: one cloudflared process with one ingress, or one `tailscale serve` config on port 443. Local Studio owns it on every route. It carries both products:

| Paths | Goes to |
| --- | --- |
| `/v1/health`, `/v1/ws` | local-studio-serve: the tunnel listener (18094) on a Cloudflare route, the local listener (18093) on the tailnet route (docs/remote.md, Tunnel listener contract) |
| `/mcp`, `/.well-known/oauth-protected-resource`, `/.well-known/oauth-protected-resource/mcp`, `/.well-known/oauth-authorization-server`, `/.well-known/oauth-authorization-server/mcp`, `/authorize`, `/token`, `/register`, `/revoke` | Tychicus serve (8787): browser use and computer use |
| anything else | 404 |

These are the eleven paths of baney75/local-studio-tunnels docs/INTERFACE.md. One hostname serves both products, split by these exact paths: the tailnet route has one name per machine.

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

- `route` is `cloudflare` (the person's own account: a tunnel on their domain, or their own relay, whose hostname is its `workers.dev` name) or `tailnet` (`tailscale serve`, no Funnel). Setup never writes `hosted` any more: that was the retired hosted route on Donovan's account. A route.json that still says `hosted` is shown by `status` and removed by `off`, and setup refuses next to it until it is off.
- `aliases` are extra hostnames on the same tunnel with the same routes, kept only while a connector moves to `hostname`.
- Ports are loopback, 1024 to 65535, different from each other, and never 18091 (the controller).

## Local Studio sets it up

```
local-studio tunnel byo                                             the steps for a tunnel on the person's own Cloudflare account
local-studio tunnel setup --relay [<name>] [--account <id>]         a relay Worker on the person's own Cloudflare account, no domain
local-studio tunnel setup --cloudflare <name> [--alias <name>]...   the person's own Cloudflare account
local-studio tunnel setup --tailnet                                 tailnet only, no Funnel
local-studio tunnel adopt [--hostname <name>]                       take over a Tychicus tunnel already on this machine
local-studio tunnel status                                          the route, its health and the next step
local-studio tunnel off                                             stop and remove the route; the Cloudflare tunnel, DNS and relay Worker stay
```

`setup` also takes `--serve <port>` and `--mcp <port>` (defaults 18094 on Cloudflare, 18093 on the tailnet, and 8787).

### Your own relay, step by step

The first choice `byo` gives, and the only one on the open internet that needs no domain. The relay is a small Worker on the person's own free Cloudflare account, at `https://<name>.<subdomain>.workers.dev`. This machine dials out to it and answers its requests, so nothing listens on the internet here and nothing runs on anyone else's account.

1. Install Node.js 22 or later (https://nodejs.org/en/download) if `npx` isn't there.
2. Run `local-studio tunnel setup --relay --yes`. The first time it answers `needs_signin` with `npx --yes wrangler@4.163.0 login`.
3. Run that command and sign in to Cloudflare, or make a free account, in the browser it opens.
4. Run the setup command again. With more than one account it answers `needs_account`, and with an account that never used Workers `needs_subdomain` with the link to pick one; run it again after. It deploys the relay, starts a login service, checks the public address, and answers `ready`.
5. `local-studio tunnel status` shows it any time. `local-studio tunnel off` removes it from this machine and gives the command that deletes the Worker.

### Your own Cloudflare tunnel, step by step

For a domain the person owns, after the relay in what `local-studio tunnel byo` prints:

1. Make a free Cloudflare account at https://dash.cloudflare.com/sign-up and add a domain you own to it (https://developers.cloudflare.com/fundamentals/manage-domains/add-site/). Cloudflare must run the domain's DNS. The free plan is enough.
2. Run `local-studio tunnel setup --cloudflare studio.YOUR-DOMAIN --yes`, with your domain in place of `YOUR-DOMAIN`. It downloads the pinned cloudflared and, the first time, answers `needs_signin` with a `cloudflared tunnel login` command.
3. Run that command, sign in to Cloudflare in the browser it opens, and pick your domain. cloudflared writes `~/.cloudflared/cert.pem`; Local Studio checks only that the file exists.
4. Run the setup command again. It creates the tunnel `local-studio-<machine>`, its DNS record and a login service, checks the public address, and answers `ready` with the MCP address (`https://studio.YOUR-DOMAIN/mcp`) and the phone address (`wss://studio.YOUR-DOMAIN/v1/ws`).
5. `local-studio tunnel status` shows it any time; `local-studio tunnel off` removes it from this machine and leaves the tunnel and DNS record in your Cloudflare account.

**Or let an agent do it.** Tell a coding agent such as Claude Code or Codex: "Set up my Local Studio tunnel: run local-studio tunnel byo --json and follow it." The guide's JSON carries that sentence as `agentPrompt`. The agent runs the steps in order, since each answers a state with the next command (below). It sets up the relay unless the person wants their own domain, which they add to Cloudflare themselves (step 1). It stops at `needs_signin`, hands the person the `wrangler login` or `cloudflared tunnel login` command (signing in to Cloudflare stays with the person), and once they say they signed in, runs the setup command again.

A machine that already runs a Tychicus tunnel uses `local-studio tunnel adopt` instead. `local-studio tunnel setup --tailnet` keeps Local Studio on the tailnet only. Windows has no tunnel service yet, so there `byo` points at the tailnet route.

### Your own relay

`setup --relay [<name>] [--account <id>]` deploys the Worker vendored in `core/src/tunnel/worker/` (byte for byte from baney75/local-studio-tunnels; `SOURCE` names the commit and digests, and `scripts/tests/relay-bundle-test.sh` checks them). The name defaults to `local-studio-<machine>`: 1 to 63 of `a-z`, `0-9` and `-`. Every wrangler run is `npx --yes wrangler@4.163.0` (`LS_NPX` names another `npx`) in `<data dir>/tunnel/relay/` (0700), which holds only the two Worker files and wrangler's output file, with `WRANGLER_SEND_METRICS=false`. Local Studio never opens wrangler's own files and never runs its login.

Setup runs `whoami --json`, `deploy --name <name>`, then `secret put RELAY_KEY --name <name>` with a new 32-byte key on stdin. Only then does it write `<data dir>/tunnel/relay.json` (0600: `v`, `url`, `name`, `accountId`, `key`), the one place the key is kept. The service of the Cloudflare route (same label and unit) runs `local-studio tunnel relay run`, and the workers.dev name goes into `allowTunnelHosts`. Setup polls `https://<host>/_relay/health` until it says this machine is connected (a new name can take minutes, so up to 5 minutes, every 5 seconds), restarting the service when the relay doesn't know the new key yet, and then runs the checks below. route.json is written last. If any of that fails, the service, relay.json and the host allowance go; the Worker stays and refuses every connector until setup runs again. `LS_RELAY_WAIT_MS` shortens the wait for checks only. Setup again on a live relay sets a new key and restarts the service.

It answers `needs_node`, `needs_signin`, `needs_account` (`accounts`, and `nextCommand` with `--account`), `needs_subdomain` (`url`) or `ready` with `relay: {name, accountId}`, and the errors `bad_name`, `bad_account`, `no_account`, `relay_whoami`, `relay_deploy`, `relay_key`, `service_failed` and `going_live`.

`tunnel relay run` connects to the relay and stays in the foreground until SIGTERM or SIGINT. It writes `<data dir>/tunnel/relay-state.json` (`state`, `at`, `pid`): `connecting`, `connected`, `disconnected` (it dials again every 1 to 30 seconds), `replaced` (another machine took the relay over), `unauthorized`, `outdated`, `bad_config` or `stopped`. After `replaced`, `unauthorized`, `outdated` and `bad_config` it stays up without dialing. `status` adds `relay: {state, since}`, with `stopped` for a process that is gone, and a message with the next step when it isn't connected.

### Your own Cloudflare account

`setup --cloudflare` uses the pinned cloudflared. Without `~/.cloudflared/cert.pem` (only its presence is checked; it is never read) it answers `needs_signin` with the command `<cloudflared> tunnel login` for the person to run. Then it reuses or creates the tunnel `local-studio-<machine>` (a tunnel of that name whose credentials aren't on this machine is refused as `tunnel_elsewhere`), runs `cloudflared tunnel route dns` for the hostname and each alias (a name taken or put on another zone answers `needs_dns`), and writes `<data dir>/tunnel/cloudflared.yml` (0600): for each hostname the eleven paths as anchored `path:` rules (`^/mcp$`, `^/\.well-known/oauth-protected-resource$`), then `http_status:404`. A per-user service runs `cloudflared tunnel --no-autoupdate --config <that yml> run`: launchd `com.localstudio.tunnel` in `~/Library/LaunchAgents` on macOS, `local-studio-tunnel.service` under `systemctl --user` on Linux. Windows has no service yet, so `--cloudflare` answers `unsupported` there. The credentials stay where cloudflared wrote them. Before any service change, `setup`, `adopt` and `off` refuse with `unsafe_service_path`, changing nothing, when the real launchctl or systemctl would run while `HOME` isn't the account's own home, or a stand-in (`LS_LAUNCHCTL`, `LS_SYSTEMCTL`) would run while it is.

Once the service runs, setup adds each hostname to `allowTunnelHosts` in `serve.json` (docs/remote.md) and checks the public hostname: `/v1/health` must answer 200, and `/mcp` must answer 401 with `WWW-Authenticate` naming `resource_metadata` when Tychicus serve listens on `ports.mcp` (`checks.mcp` is `not_listening` otherwise, which doesn't fail setup). The checks retry for about a minute while DNS settles. When this computer's own resolver has no answer for a name yet (a stale negative answer can outlive its TTL in a forwarding resolver such as Tailscale MagicDNS, or in the macOS mDNSResponder cache), each check looks the name up over DNS over HTTPS (`https://cloudflare-dns.com/dns-query`, then `https://dns.google/resolve`, JSON API, A and AAAA) and runs again against those addresses with the real name for TLS and Host. `checks.dns` is then `public_only` instead of `local`, and the message says this computer's DNS still has the old answer, which clears on its own. When neither lookup has an address, the check fails naming both. `LS_TUNNEL_CHECK_URL` skips this; `LS_TUNNEL_DOH` (comma separated base URLs) and `LS_TUNNEL_CHECK_HTTP_PORT` (plain HTTP on that port instead of HTTPS on 443) are for checks only. If they fail, the service is stopped and removed, so nothing stays exposed, and the answer is `error` with code `going_live`. route.json is written last.

### Tailnet

`setup --tailnet` reads `tailscale status --json` (not running or signed out answers `needs_signin`, with Tailscale's sign-in URL when it gives one) and mounts each of the eleven paths on https 443 with `tailscale serve --bg --yes --https=443 --set-path=<path> http://127.0.0.1:<port><path>`: `/v1` paths to the local listener (18093), the rest to `ports.mcp`. It never runs Funnel, and refuses, changing nothing, when Funnel is on for 443, 443 forwards raw TCP, or a path already goes somewhere else. It records the paths it added in `<data dir>/tunnel/tailnet.json`; `off` removes exactly those. route.json carries `ports.serve` 18093 on this route. Then it runs `local-studio-serve allow-host <name> --json` so the local listener answers the tailnet name, and checks `/v1/health` and `/mcp` on that name. When that local-studio-serve has no `allow-host` yet, setup still finishes the rest, skips the health check (`checks.health` is `unchecked`), writes route.json, and answers `needs_serve_allow_host` with that command as `next`.

### Adopting a Tychicus tunnel

`adopt` reads only Tychicus's `remote.json` and `cloudflared.yml` for the tunnel id, the credentials file path and the old hostname. Without `--yes`, or without a terminal, it answers `confirm` with the plan and changes nothing. With it: the DNS record for `--hostname` (when it differs), Local Studio's yml with the new hostname and the old one as an alias on the same tunnel and credentials file, Local Studio's service, a check of `/mcp` on the old hostname, then Tychicus's `com.tychicus.cloudflared` (or `tychicus-cloudflared.service`) is disabled and stopped (`launchctl disable` then `bootout`, or `systemctl --user disable --now`), keeping its files, so it doesn't start again at login next to Local Studio's connector. With Local Studio's connector alone, `/v1/health` is checked on both names and `/mcp` on the old one. route.json comes next, then `tychicus remote join --json`, then `/mcp` on every name. When Tychicus can't join, or its serve still doesn't answer `/mcp` on every name after the join, the answer is `needs_tychicus_join` with that command and the working tunnel stays. Any failure before route.json removes Local Studio's service and yml, enables and starts Tychicus's service again if it was stopped, and answers `error` with code `adopt_rolled_back`. A DNS record already added stays.

While both connectors run, Cloudflare sends each request to either one, and Tychicus's connector answers 404 for names and paths its own config lacks, so before the switch only what both serve is checked: `/mcp` on the old hostname, which keeps answering throughout. Tychicus serve answers 421 for any Host that isn't its public address or an alias in its remote.json, and the new hostname is only there once `tychicus remote join` has run, so `/mcp` on the new name is checked after the join.

### off

`off` works on every channel. It removes Local Studio's service and yml, relay.json, relay-state.json and the relay folder, the tailnet paths it added, and route.json, takes each name out of `allowTunnelHosts` in `serve.json` (a tailnet name stays in `allowHosts`), and stops a hosted cloudflared. The Cloudflare tunnel, its DNS records and the tailnet node stay, and so does a relay's Worker, whose delete command is in the message. A route run by Tychicus is refused with `not_ours`.

### Every channel, no waitlist

Donovan, 2026-11-24: "NO more waitlist." `setup`, `adopt`, `status` and `off` run the same on stable, nightly and dev, and nothing answers `waitlist`. Everyone is directed to set up a relay on their own free Cloudflare account, a tunnel on a domain they own (`local-studio tunnel byo` prints the steps for both, or hands them to an agent), or Tailscale (`setup --tailnet`). `local-studio tunnel request` and the hosted route are gone. The hosted client commands (`up`, `down`, `release`, `ports`) remain only for a machine that already holds a hosted tunnel's keys.

### Agents can run it

An agent must be able to set a tunnel up for a person. Every `tunnel` command takes `--json`, never prompts when stdin isn't a terminal, and returns one state with the next step: `byo` (the guide; `byo --json` carries the steps as `plan`, the relay setup command as `command`, the domain one as `template` and the sentence for an agent as `agentPrompt`), `none` (no route yet), `ready`, `needs_signin` (the person signs in to Cloudflare or Tailscale; the state carries the URL or the command), `needs_node`, `needs_account`, `needs_subdomain` (the relay), `needs_dns`, `confirm` (adopt's plan, waiting for `--yes`), `needs_tychicus_join`, `needs_serve_allow_host` (tailnet: run the command it carries), `off`, or `error` with a code (exit 1). `status` with no route answers `none` with `nextCommand` `local-studio tunnel byo`, and its message names the tailnet choice too. `--yes` accepts the steps that change the machine, such as adopting a Tychicus tunnel. Signing in stays with the person: an agent hands over the URL and polls `status --json`.

### Going live

Before a route carries a path, Local Studio checks auth there (baney75/local-studio-tunnels docs/DESIGN.md, Going live): `/mcp` answers 401 with `WWW-Authenticate` naming the resource metadata, and `/v1/health` answers 200 for the route's hostname and 403 for any other Host. It adds the hostname to `allowTunnelHosts` in `serve.json`. The Cloudflare route uses the pinned, verified cloudflared, the same release and digests as Tychicus's download, and reuses Tychicus's copy when its digest matches.

## Tychicus joins

`tychicus remote join` reads the record, stops any tunnel of its own without deleting it, and serves on `ports.mcp` with `https://<hostname>` as its public address, answering OAuth on each alias too. On a machine with Local Studio, `tychicus remote setup` runs `local-studio tunnel setup` and then joins. On a machine without Local Studio, Tychicus keeps its own `remote setup` with the same exact paths.

`local-studio tunnel adopt` moves a Tychicus tunnel to Local Studio without a gap: it keeps the tunnel and its credentials, adds the new hostname, keeps the old one as an alias, starts Local Studio's service, then runs `tychicus remote join`. Any failure puts Tychicus's service back.

## Not yet

- The app has no tunnel screen yet: setup runs from the CLI, or from an agent through `--json`.
- Tychicus's Funnel route (port 8443) opens the whole port, so it can't keep to exact paths; Local Studio doesn't use Funnel.
