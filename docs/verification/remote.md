# Remote verification

This page records what has been checked against real binaries for Local Studio Remote ([docs/remote.md](../remote.md)): serve, desktop links between machines, and the iPhone client. Each entry gives the build, the host, the command and the result, so anyone can repeat it. A check that has not run says so.

Every run isolates its state. `HOME`, `EUKHE_DAEMON_SOCKET`, `EUKHE_CODING_AGENT_DIR` and `LOCAL_STUDIO_DATA_DIR` point into a scratch folder, and model credentials are linked, never copied.

## Status

| Area | State | Last check |
| --- | --- | --- |
| Protocol and serve, no model | Verified | remote_smoke non-model stages, abbey1 |
| Serve with a real model | Partly verified | remote_smoke model stage, abbey1, qwen3-vl:4b |
| Desktop link client | Verified | link_smoke, Claude Code haiku |
| iPhone client library against real serve | Verified | ServeInteropTests on the Mac host |
| iPhone app to Parish1 over the tailnet | Verified | `ios/tools/smoke.sh` in the simulator, Claude Code on Parish1 |
| Machines UI (macOS, Linux) | Snapshots only | Settings › Connections, pairing sheet, sidebar rows |
| Serve as a service on Linux | Deployed | Parish1, systemd user unit, `install --tailscale`, serve at a5f3c5b79 |
| Serve as a service on macOS, handoff with the app | Verified | The Mac, launchd agent, `install --tailscale` |
| The app serving while it runs | Verified | The Mac, app launched after `serve.json` existed |
| The app starting to serve when `serve.json` appears later | Verified | The Mac, installed app at a5f3c5b79 (#56) |
| Projects on a headless machine | Deployed | Parish1, `local-studio-serve project add` (#59) |
| Project requests answered by the app | Verified | The Mac, installed app at a5f3c5b79 |
| Mac to Parish1 over the tailnet, core link client | Verified | link_probe, Claude Code haiku on Parish1 |
| Mac to Parish1 over the tailnet, app screens | Not run | Polycarp computer use was down; steps below |
| Mac to Parish1 over the shared tunnel | Not run yet | Waits for the tunnel |

## Protocol and serve: remote_smoke

```sh
LOCAL_STUDIO_SMOKE_AGENT=$HOME/.cache/ls-creds REMOTE_SMOKE_MODEL=ollama/qwen3-vl:4b \
  target/debug/examples/remote_smoke <eukhe> "$(mktemp -d)"
```

It expects `local-studio-serve` at `<examples dir>/../local-studio-serve`. On abbey1, take the shared GPU first with `~/.local/bin/ollama-window wait <minutes> qwen3-vl:4b <who>`, then `release <who>` afterwards.

2026-11-21, abbey1, built from the head of baney75/local-studio-fork#63, Eukhe 0.10.73:

- Run 1, 195 ok: the non-model stages passed (`PASS remote smoke: non-model stages`), and so did `PASS model stage: a streamed turn with a real tool call over the wire`. These also passed over the wire: the once turn (124 s on this model), a retried send with the same request id running once, rename, pin, unread, snooze, archive and back, access mode, answer_approval, send_queued_now, remove_queued, cancel_subagent, load_diff and fork_thread. Then the outer `timeout 1500` stopped the run while it waited for the thread to go idle. That was a limit of the run, not a failed check.
- Run 2, 176 ok: the non-model stages passed again. The first model turn then hit the smoke's 2400 s bound: qwen3-vl:4b was still thinking about the `printf` it was asked to run, with no tool call yet, while another job shared the GPU.
- Not verified with a real model: the queue sleep and follow-up stage, and abort. They need a faster model than a 4B on a shared GPU.

## Desktop links: link_smoke

`remote/examples/link_smoke.rs` drives two desktops, A serving and B linked. It covers pairing, a real model turn with an approval answered from B, freeze and reconnect, a fake far side, revoke and forget. A runs `claude-code/haiku` on the claude CLI's own sign-in.

2026-11-21, Mac: 46 checks passed on the serve as a service branch, rebased onto #67.

## iPhone client against real serve: ServeInteropTests

The iPhone's `LocalStudioRemote` package also builds for macOS, so its interop test runs on the Mac host against a real serve:

```sh
local-studio-serve serve --port <port> --name interop-mac --eukhe <eukhe> --data-dir <scratch>/data &
link=$(local-studio-serve pair --url ws://127.0.0.1:<port>/v1/ws --name interop-mac --data-dir <scratch>/data | grep '://pair?')
cd ios/Packages/LocalStudioRemote && LOCAL_STUDIO_PAIR_LINK="$link" swift test --filter ServeInteropTests
```

2026-11-21, Mac, serve built from main at f36660ed7: `pairsReconnectsAndHoldsTheLimits` passed. The checks were:

- The client pairs with XXpsk3 and gets a snapshot.
- It reconnects with KK and gets a delta in the same epoch.
- A replayed KK handshake doesn't disturb the live socket and is closed with 1002.
- Serve refuses the flood of mid-handshake sockets.

This covers the phone's client library, not the app's screens.

## iPhone app to Parish1: smoke.sh

`ios/tools/smoke.sh` runs the app's UI test in the simulator. Given a pair link, it skips the fake studio and drives the real machine:

```sh
link=$(ssh <parish1> 'local-studio-serve pair --url wss://<parish1>.<tailnet>.ts.net/v1/ws --name iphone-smoke' | grep -o '[a-z-]*://pair?[^ ]*')
LOCAL_STUDIO_PAIR_LINK="$link" ios/tools/smoke.sh
ssh <parish1> 'local-studio-serve devices'
ssh <parish1> 'local-studio-serve revoke <device id>'
```

Revoke by the device id that `devices` prints; the app pairs under its own device name, not the `--name` given to `pair`.

2026-11-21, iPhone 17 Pro simulator (iOS 26.5), app built from main at d421e5628, against the Parish1 service over the tailnet. `testPairWatchSendApproveRelaunch` passed in 37.8 s:

- The app paired from the link and showed `link live` and the live thread list.
- It opened the first thread on Parish1, sent "Reply with the single word pong." and waited for an assistant reply and an idle thread (Claude Code, Supervised).
- It saved the cache, relaunched with a 6 s connect delay, showed the cached list first, then went live again.

| Timing | ms |
| --- | --- |
| Cold start to live list | 1526 |
| Send to first transcript update | 72 |
| Relaunch to cached list | 16 |
| Relaunch to live list (6 s connect delay) | 7106 |

`testIncompatibleMachine` skips without a fake studio. The device was revoked afterward, and Parish1 lists no paired devices.

## Serve on Parish1 and the Mac to Parish1 link

2026-11-21. Main at 6ef29c044; `local-studio-serve` is a release build from chapel1, and Parish1 runs Eukhe 0.10.73.

Install and read-back on Parish1:

```sh
local-studio-serve install --tailscale --name parish1 \
  --bin ~/.local/opt/local-studio/local-studio-serve --eukhe ~/.local/opt/local-studio/eukhe --json
local-studio-serve status --json
tailscale serve status
curl https://<parish1>.<tailnet>.ts.net/v1/health
```

- `install` wrote the `local-studio-serve.service` user unit, and `status --json` reported `serving` with the service as owner and `wss://<parish1>.<tailnet>.ts.net/v1/ws` as the effective URL.
- `tailscale serve` mounts only `/v1/health` and `/v1/ws` on 443. From the Mac, health over the tailnet answered `{"ok":true,"protocol":2,"min_protocol":2}`, and `/` returned 404.

A machine that only runs serve has no projects, and nothing can add one (#49). For this check, `state.json` was seeded by hand with one project, `~/local-studio-probe`, while the service was stopped. #49 replaced that step: run `local-studio-serve project add ~/local-studio-probe` while the service runs.

`remote/examples/link_probe.rs` drives the core link client that the desktop apps use, with no UI, against a real far machine:

```sh
scratch=$(mktemp -d)
LOCAL_STUDIO_PAIR_LINK="$(ssh parish1 'local-studio-serve pair' | grep '://pair?')" \
  target/debug/examples/link_probe <local eukhe> "$scratch"
ssh parish1 'local-studio-serve devices'
ssh parish1 'local-studio-serve revoke <the id of this Mac in that list>'
rm -rf "$scratch"
```

The probe forgets the far machine on success but can't revoke itself on the far side, and the scratch folder holds this side's pairing keys, so the last three steps run every time, pass or fail.

Run from the Mac over the tailnet, twice, both PASS. Numbers are from the second run, after the round 1 review fixes:

- `pair_machine` reached Connected in 1.5 s (4.0 s in the first run). The far machine reported linux, `0.1.0-dev.0+local.6ef29c0`, at `wss://<parish1>.<tailnet>.ts.net/v1/ws`; latency was 13 ms once the first ping came back (20 s).
- Parish1's project arrived as `@<machine>/<home>/local-studio-probe`.
- `start_thread` with Claude Code haiku answered in 52 ms with an `@<machine>/` thread id. A streaming transcript update arrived before the reply settled, and "far pong" settled 1.7 s after the start.
- A second send got "second" back in 1.0 s, again with a streaming update first. Parish1's serve log shows each send accepted and its first update out 0 ms later, and Parish1's `state.json` holds the threads with their working folder.
- `forget_machine` removed the machine locally. It does not revoke the desktop on the far side, so `local-studio-serve devices` still listed the Mac until `revoke` ran there.

The iPhone client library also paired with Parish1 over the tailnet, reconnected with KK, and had a replayed handshake refused (serve log). Its flood stage hung: it relies on the loopback-only slot and on fake `Tailscale-User-Login` headers, and over the tailnet every socket carries the real login and none is loopback. The stage works on loopback only.

## Serve and the desktop app on the Mac

2026-11-21, the Mac. The app is a Release build of #41's head, and serve is built from it, installed with:

```sh
local-studio-serve install --tailscale --name <mac> --bin ~/.local/opt/local-studio/local-studio-serve \
  --eukhe /Applications/LocalStudio.app/Contents/MacOS/eukhe --json
```

- With the app running, `install` wrote `org.local.studio.serve.plist` and loaded it, and mounted `/v1/health` and `/v1/ws` on 443. Serve logged that the app owns the data dir and waited. `status --json` reported `owned` by the app.
- App quit: serve took the lock about 1 s after the app exited (`status --json`: `serving`, owner `serve`). Health over the tailnet answered from the Mac and from Parish1.
- App launched: it sent `SIGUSR1`. Serve closed every remote socket with 1012, released the lock, and logged that the app took it over. The app owned the data dir about 40 s after launch, with the Mac's load average around 300, and serve went back to waiting.
- The relaunched app served in process: it listened on 18093, `status --json` reported `serving` with the app as owner, and health over the tailnet answered from the Mac and from Parish1.
- Gap (#53): the first app instance was already running when `install` wrote `serve.json`. It owned the lock but served nothing, because the app checks for `serve.json` only at launch. The Mac was unreachable until that app quit.

### Redeploy at a5f3c5b79 (#56 and #59)

2026-11-21. Serve and the app were rebuilt from main at a5f3c5b79.

- Parish1: the new serve binary was copied into place and the user unit restarted. `status --json` reported `serving`, and health answered over the tailnet. The hand-seeded project was replaced through the running service: `project remove ~/local-studio-probe` printed `removed`, `project add ~/local-studio-probe` printed `added`, and `project list` showed it, each exiting 0.
- The Mac, serve: the launchd agent was restarted on the new binary with the old app still open. It waited, and `status --json` reported `owned` by the app. `project list` printed `queued` and exited 2, since that app predates #59 and never answers (#62).
- The Mac, app: swapped while idle. While it was closed, serve owned the data dir and served. The new app took the data dir back and served on 18093.
- `project list` through the new app printed `listed` and exited 0. The Mac has no `state.json`, so 0 projects is the right answer.
- #53 on the installed app: with the app running, moving `serve.json` away made 18093 refuse within 1 s; restoring the same file made it serve again within 1.5 s. The launchd agent kept waiting throughout, and health over the tailnet answered afterward.

### Redeploy at 40ecae022 (#69)

2026-11-21. Serve was rebuilt from main at 40ecae022. The app was not rebuilt, since #69 changes only the CLI.

- Parish1: the new serve binary was copied into place and the user unit restarted. `status --json` reported `serving`, and `project list` printed `listed` with the probe project, exiting 0.
- The Mac: the launchd agent was restarted on the new binary with the app open. It waited while the app served, `status --json` named the app as the owner, `project list` printed `listed` and exited 0, and health on 18093 answered.
- The case #62 reported, an owner that holds the lock but never answers a list, was checked live before merge with a frozen serve (`kill -STOP`): `list` exited 0 with the projects from `state.json` and removed its own request (#69).

## Machines UI

Snapshots, light and dark:

- **macOS:** `LocalStudio +snapshot <dir> shell-sidebar-machines`, `pages-settings-connections` and `pages-pairing`.
- **Linux:** `local-studio --snapshot <dir> 6`, scenes 60 to 68.

Every machine state renders: connected with latency, connecting, offline, pair again, incompatible and refused. So do the pairing sheet's not-serving and loopback warnings.

Untested live. Steps, with serve running on machine A:

1. On A, open Settings › Connections › Pair a Device…. The QR code and countdown appear, and there is no not-serving warning.
2. Copy the link. On machine B, open Add a Machine…. The field prefills; press Pair. A shows as Connected with a latency.
3. A's threads appear in B's sidebar with A's name. Open one and send; the reply streams.
4. On A, revoke B. B's row turns to "Pair again".
5. On B, forget A. A's threads leave the sidebar.

With Parish1 as A, which has no desktop app, step 1 is `ssh parish1 'local-studio-serve pair'`, and step 4 is `local-studio-serve revoke <name>` on Parish1.
