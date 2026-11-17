# Local Studio Remote: the phone protocol (proposal)

Status: proposal from the iPhone app lead, for the core lead to accept, change or reject. Nothing here is merged behavior yet.

## Goal

A phone (first the native iPhone app) sees every thread on every one of the user's machines, reads transcripts live, sends and queues messages, answers approvals, starts threads, and hears when a thread needs the user. It reaches each machine over Tailscale (`tailscale serve`) first and over a Cloudflare tunnel when one is set up. One wire for all of it, owned by the core, so the Mac, Linux and Windows apps can later use it too.

## What the code already gives us

- `Studio` (core/src/studio.rs) is command shaped: `send`, `abort`, `answer_approval`, `start_thread`, `send_queued_now`, `remove_queued`, `snooze`, `set_pinned`, `set_settled`, `set_model`, `set_thinking_level`, `set_access_mode`, `search_threads`, `load_models`, `list_archived`, `load_message_image` and the rest.
- `Update` (core/src/model.rs) is the event stream every GUI already renders: `Projects`, `Threads`, `Thread`, `Transcript { op: Reset | Insert | Update }`, `Approvals`, `Queue`, `Notification`, `Models`, `RequestDone`, `RequestFailed`, `Notice`, `Connection`.
- Eukhe's dashboard (eukhe-dashboard, protocol 3) already solved loopback serving behind `tailscale serve`: a 32-byte token in a 0600 file, the token in the URL fragment, the first socket frame pairs, a `Host` allowlist against DNS rebinding and an `Origin` check against cross-site sockets. Copy that design; do not invent a weaker one.

So the proposal is: **serve `Studio` over a WebSocket**. Requests are `Studio` methods, events are `Update`s, both as JSON. No second model.

## Two core changes it needs

1. **Watched threads, not one focus.** Today `Studio::select` keeps one `focused` thread and only that thread streams `Transcript`, `Thread`, `Approvals` and `Queue`. A phone and the desktop must not steal each other's focus. Add `watch_thread(thread)` and `unwatch_thread(thread)` with a reference count per thread; `select_thread` stays for GUI focus (it also marks read) and counts as one watch. Every `emit_*` that checks `focused == id` checks `watched.contains(id)` instead. The server tracks which connection watches which thread and routes thread-scoped updates only to those connections; global updates (`Threads`, `Projects`, `Notification`, `Models`, `Connection`, `Notice`) go to all.
2. **One Studio owner per data dir.** (Built: `Studio::new` takes the lock and a second Studio fails with a `StudioError` that names the holding process.) `state.json` is written by atomic rename with no lock, so two Studios on one data dir lose each other's flags. Add an advisory lock (`<data>/studio.lock`, `flock`). Whoever holds it serves remote clients: the desktop app when it runs (it starts the same server module in process), or `local-studio serve`, a headless binary run as a user service on machines with no GUI. Later the desktop apps can attach to `serve` over this same wire, the way Eukhe frontends attach to the daemon.
3. Add `serde::Serialize`/`Deserialize` to the wire types (`Update` and everything it carries, plus the request argument types) behind a `wire` feature, `#[serde(tag = "type", rename_all = "snake_case")]` on enums, `camelCase` fields, `Vec<u8>` image data as base64. Built: enums that carry data are tagged `{"type": …}`; enums without data are plain strings (`"queue"`, `"approve"`). Types that `state.json` already persisted keep their stored spelling so one serde derive serves both, so `AccessMode` is `"supervised"`, `"autoAcceptEdits"`, `"auto"`, `"fullAccess"` and the settings enums are camelCase too.

## Transport

- `local-studio-serve serve [--port 18093] [--allow-host <name>]... [--notify <url>]... [--eukhe <path>]` (crate `remote/`) binds `127.0.0.1` only. The WebSocket is `GET /v1/ws`; `GET /v1/health` answers `{"ok":true,"protocol":1,"min_protocol":1}` without auth for reachability probes. `local-studio-serve devices` lists paired phones and `local-studio-serve revoke <device id or name>` removes one.
- The `Host` must be a loopback name or an `--allow-host` name (any port), and so must the last `X-Forwarded-Host` value when one is present; anything else gets 403.
- Tailscale: `tailscale serve --bg --https=443 http://127.0.0.1:18093`, and `--allow-host <machine>.<tailnet>.ts.net`. The phone connects to `wss://<machine>.<tailnet>.ts.net/v1/ws`.
- Cloudflare: the tunnels lead's hosted tunnel forwards a hostname to the same port; `--allow-host` names it; `X-Forwarded-Host` is honored as in eukhe-dashboard's access rules.
- Native clients send no `Origin`. A request with an `Origin` must match its host (browsers); a request without one is allowed (only non-browser programs omit it, and they could forge any `Origin` anyway).
- No `permessage-deflate` in v1: tokio-tungstenite 0.28 does not implement it, and the 200-item transcript window keeps a thread open small. Add it when a measurement says frames are the bottleneck. One socket per machine per phone.

## Pairing and devices (better than one shared token)

- `local-studio-serve pair [--url <wss url>]` (and a "Pair a phone" sheet in each desktop app) mints a one-time pairing secret, valid 10 minutes, and shows a QR code and a link: `localstudio://pair?url=wss%3A%2F%2Fstudio-mac.tail1234.ts.net%2Fv1%2Fws&name=studio-mac#<secret>`. The scheme follows the build's channel (`localstudio-nightly`, `localstudio-dev`). Without `--url` it uses the machine's Tailscale name, else `ws://127.0.0.1:<port>/v1/ws`. Only a hash of the secret and its expiry are stored (`<data>/remote-pairing.json`, 0600).
- The phone's first frame: `{"op":"pair","secret":"…","device":{"id":"<uuid>","name":"My iPhone","platform":"ios","app":"0.1.0","channel":"nightly"}}`. The server answers `{"op":"paired","credential":"<64 hex>","machine":{…}}` and burns the secret.
- The server stores only a hash of each device credential in `<data>/remote-devices.json` (0600) with name, platform, first and last seen. Desktop settings list devices and revoke one. A revoked or unknown credential gets `{"op":"unpaired"}` and a close.
- Later connections open with `{"op":"hello","device":"<uuid>","credential":"…","protocol":1,"min_protocol":1}`. The phone keeps credentials in the Keychain (`kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`). After `paired` the phone may send this `hello` on the same socket. The server checks the stored credential again on every frame, so a revoke closes an open socket at its next frame (a ping at the latest).

## Frames

Client to server (requests run concurrently; `id` is the client's number):

```json
{"id": 7, "op": "send", "args": {"request": "r7", "thread": "t1", "prompt": {"text": "ship it", "images": [], "files": []}, "delivery": "queue"}}
{"id": 8, "op": "watch", "args": {"thread": "t1"}}
{"id": 9, "op": "answer_approval", "args": {"thread": "t1", "request": "a3", "answer": "approve"}}
```

`op` is the `Studio` method name. The server answers each with `{"re": 7, "ok": true}` once the command is accepted, or `{"re": 7, "error": "…"}`; outcomes then arrive as updates (`request_done`, `request_failed` carry the `request` string, which makes a retried send idempotent: the server drops a second `send` with a `request` it has already accepted).

Built in v1:

- A dropped retry answers `{"re": 7, "ok": true, "result": {"duplicate": true}}`. `start_thread` and `start_threads` are deduplicated the same way by their `draft`.
- Updates that carry a `request` or `draft` (`thread_started`, `request_done`, `usage`, `directory`, …) go only to the socket that sent it; with no known owner they go to all.
- `watch {"thread"}` and `unwatch {"thread"}` hold a server watch per socket; the first watcher of a thread calls `Studio::watch_thread`, the last one to leave `unwatch_thread`. A socket that closes drops its watches.
- `load_earlier {"thread", "before", "limit"}` answers inline: `{"re": 9, "ok": true, "result": {"thread", "start", "items"}}`, at most 500 items.
- The ops are the `Studio` methods a phone needs: projects, threads (start, send, abort, queue, approvals, rename, archive, pin, settle, snooze, unread, fork, compact, edit from here, restart), model and thinking and access settings, models, commands, skills, search, subagents, images, git status and branches, diffs, files, pull requests and usage. Left out on purpose: `select_thread` (desktop focus), terminals, provider keys and settings writes. An unknown op answers an error.

Server to client:

```json
{"op": "hello", "protocol": 1, "min_protocol": 1, "machine": {"id": "…", "name": "studio-mac", "os": "macos", "version": "0.3.0-nightly.20261118070000+1a2b3c4", "channel": "nightly"}, "device": {"id": "…", "name": "My iPhone"}, "snapshot": [ {"type": "connection", …}, {"type": "daemon", …}, {"type": "projects", …}, {"type": "threads", …}, {"type": "models", …} ]}
{"op": "update", "seq": 412, "update": {"type": "transcript", "thread": "t1", "op": {"type": "insert", "index": 18, "item": {…}}}}
```

- `seq` increases per connection. On reconnect the phone sends `hello` again with `"resume": {"watch": ["t1"]}`; v1 answers with a fresh snapshot and a `transcript` `reset` for each watched thread. Delta resume by `seq` is v2, once measured as needed.
- **Transcript window.** `reset` for a thread carries at most the last 200 items plus `{"earlier": <count>}`; `load_earlier {"thread", "before": <index>, "limit"}` pages back. Long threads must not cost the phone a megabyte on open.
- `ping`/`pong` every 20 s from the phone; the server closes a socket silent for 60 s.

## Notifications when the phone is asleep

- `Update::Notification` (`turn_done`, `needs_approval`, `failed`) goes to every connected client; the app shows it as a local notification while it has a socket.
- When the app is suspended, iOS keeps no socket. APNs needs a push key from the user's paid Apple Developer account and a relay that holds it. Proposal: `serve` takes a list of notify hooks (`remote.notify: [{"kind": "webhook", "url": …}]`) that it POSTs the same notification to. v1 uses an ntfy-style webhook: hooks come from `<data>/remote.json` (`{"notify": [{"kind": "webhook", "url": …}]}`) and `--notify <url>`, and each `notification` update is POSTed as `{"machine": {…}, "notification": {…}}`, best effort with a 5 s timeout; APNs through the tunnels lead's hosted service is v2 and is the only piece that needs the paid account.

## Gaps against T3 Code found while writing this

- **Question cards.** Local Studio has command approvals (`Approval`) but no "the agent asks the user a question with options" request, and Eukhe's daemon has only `shell_approval`. In T3 it is the request the owner answers most (179 in 30 days against 18 command approvals). Proposed shape, copying the shell approval flow: daemon frame `user_question {active_session_id, request: {request_id, questions: [{header, question, options: [{label, description}], multi_select}]}, agent}`, answer `answer_user_question {request_id, answers: {<question>: <answer>}}`, and `user_question_settled`; in core `Update::Questions { thread, questions }` and `Studio::answer_question(thread, request, answers)`, carried on this wire unchanged.
- **Thread list paging.** `Threads` sends every row. Fine at hundreds of threads; T3 pages settled threads 25 at a time. Measure before adding paging.

## Protocol window

Each side states the newest protocol it speaks (`protocol`) and the oldest it still accepts (`min_protocol`), and keeps `min_protocol` at `protocol - 1` once protocol 2 exists (docs/channels.md: both sides speak their protocol and the one before it). The server answers `hello` with the highest version inside both windows and speaks it for the life of the socket. When the windows do not overlap, the server sends `{"op":"incompatible","protocol":N,"min_protocol":M}` and closes; the phone names the machine, its version and channel, and which side to update. `/v1/health` reports both numbers so the phone can say this before pairing.

## Channels

docs/channels.md is the scheme. The iPhone app is side by side: `org.local.studio.ios` ("Local Studio"), `org.local.studio.ios.nightly` ("Local Studio Nightly"), `org.local.studio.ios.dev` ("Local Studio Dev"), URL schemes `localstudio`, `localstudio-nightly`, `localstudio-dev`. It carries the desktop app's version and tags, so a phone and a desktop from the same channel always share a protocol. The server reports its version and channel in `hello`; the phone shows them on the Machines sheet. `local-studio-serve` stamps both at build time: CI sets `LOCAL_STUDIO_VERSION` and `LOCAL_STUDIO_CHANNEL`; a local build reports `X.Y.Z-dev.0+local.<sha7>` on `dev`.
