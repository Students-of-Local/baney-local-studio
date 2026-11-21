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
| iPhone app against real serve | Not run | Needs a simulator run of `ios/tools/smoke.sh` |
| Machines UI (macOS, Linux) | Snapshots only | Settings › Connections, pairing sheet, sidebar rows |
| Mac to Parish1 over the tailnet | Not run yet | Waits for serve as a service (#41) |
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
