# Local Studio Remote: the phone protocol

Status: accepted by the core lead on 2026-11-17 (PR #4) for building the phone and `local-studio serve`. The security section is a tier 3 change; the core lead runs its merge review.

## Goal

A phone (first the native iPhone app) sees every thread on every one of the user's machines, reads transcripts live, sends and queues messages, answers approvals and questions, starts threads, and hears when a thread needs the user. It reaches each machine over Tailscale (`tailscale serve`) and over a Cloudflare tunnel when one is set up. One wire for all of it, owned by the core, so the Mac, Linux and Windows apps can later attach to `serve` over it too.

## What the code already gives us

- `Studio` (core/src/studio.rs) and its `Workspace` (core/src/workspace) are command shaped: `send`, `abort`, `answer_approval`, `start_thread`, `load_diff` and the rest.
- `Update` (core/src/model.rs) is the event stream every GUI already renders.
- Eukhe's dashboard (eukhe-dashboard, protocol 3) solved loopback serving behind `tailscale serve`: a `Host` allowlist against DNS rebinding and an `Origin` check against cross-site sockets. This design keeps those and adds end to end encryption, because TLS ends at Cloudflare's edge on the tunnel route.

So: **serve `Studio` over a WebSocket**, encrypted end to end. Requests are `Studio` method names, events are `Update`s, both JSON. No second model.

## Core changes

1. **Watched threads, not one focus.** `Studio::select` keeps one `focused` thread and only that thread streams `Transcript`, `Thread`, `Approvals`, `Questions` and `Queue`. Add `watch_thread(thread)` and `unwatch_thread(thread)` with a reference count per thread; `select_thread` stays for GUI focus (it also marks read) and counts as one watch. Every `emit_*` that checks `focused == id` checks `watched.contains(id)` instead. The server routes thread-scoped updates only to connections watching that thread; global updates go to all.
2. **One Studio owner per data dir, on all three OSes.** `state.json` is written by atomic rename with no lock, so two Studios on one data dir lose each other's flags. Take an advisory lock on `<data>/studio.lock` with `fd-lock` (flock on macOS and Linux, `LockFileEx` on Windows). Whoever holds it serves remote clients: the desktop app when it runs (it starts the server module in process), or `local-studio serve`, run as a user service where no GUI runs: a launchd agent on macOS, a systemd user unit on Linux, a Task Scheduler logon task on Windows.
3. **`serde` behind a `wire` feature** on `Update` and everything it carries plus the request argument types: `#[serde(tag = "type", rename_all = "snake_case")]` on enums, `camelCase` fields. No binary data in JSON; images and files travel as blobs (below).

## Transport

- `local-studio serve [--port 18093] [--allow-host <name>]...` binds `127.0.0.1` only. `GET /v1/ws` upgrades to the WebSocket. `GET /v1/health` answers `{"ok":true,"protocol":N,"min_protocol":M}` without auth.
- **Host check.** The server checks the `Host` header, never `X-Forwarded-Host`. Allowed: `127.0.0.1:<port>`, `localhost:<port>` and each `--allow-host` name. Any other `Host` gets 403 on every path, `/v1/health` included.
- **Origin.** Native clients send no `Origin`. A request with an `Origin` must match its `Host` (browsers); a request without one is allowed.
- Tailscale: `tailscale serve --bg --https=443 http://127.0.0.1:18093`, `--allow-host <machine>.<tailnet>.ts.net`, and the phone connects to `wss://<machine>.<tailnet>.ts.net/v1/ws`.
- Cloudflare: the tunnels lead's hosted tunnel forwards a hostname to `127.0.0.1:18093`, and `--allow-host` names it. For rate limits the client address is `CF-Connecting-IP` when `Host` is a tunnel hostname (Cloudflare overwrites that header, so a client can't set it), and the socket peer otherwise.
- Binary WebSocket messages only after the opening header; no `permessage-deflate` (the payload is ciphertext). One socket per machine per phone.

## Security: end to end on every route

Every route, Tailscale and loopback included, runs the same encrypted channel inside the WebSocket, so there is one code path and nothing depends on which route a phone took. No reusable credential, token or signature crosses Cloudflare.

**Primitives.** Noise protocol framework, `X25519`, `ChaChaPoly`, `SHA256`: `Noise_XXpsk3_25519_ChaChaPoly_SHA256` for pairing and `Noise_KK_25519_ChaChaPoly_SHA256` for every later connection. The server uses the `snow` crate. The phone uses CryptoKit (`Curve25519.KeyAgreement`, `ChaChaPoly`, `SHA256`, `HMAC`) with the Noise state machine written to the spec, checked against `snow` end to end.

**Keys.**
- Each machine has one static X25519 key, created on first `serve` start, in `<data>/remote/static.key` (0600; on Windows an ACL for the user only).
- The phone makes a new static X25519 key for each machine it pairs with, so machines can't link one phone across them. It keeps that key, the machine's static public key, the machine id and its URLs in the Keychain as `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`, never synced. The Secure Enclave holds only P-256 keys, so it isn't used.
- The server stores each paired device in `<data>/remote/devices.json` (0600): device id, name, platform, app version, channel, the device's static public key, first and last seen. Nothing secret.

**Opening header.** The client's first WebSocket message is one text frame, also used as the Noise prologue so it can't be changed in flight:

```json
{"v": 1, "pattern": "KK", "device": "<uuid>", "protocol": 2, "min_protocol": 2}
{"v": 1, "pattern": "XXpsk3", "pair": "<pairing id>", "protocol": 2, "min_protocol": 2}
```

The prologue is the bytes `local-studio-remote/1\n` followed by that frame's exact UTF-8 bytes. If the protocol windows don't overlap, the server answers with a text frame `{"op":"incompatible","protocol":N,"min_protocol":M}` and closes before any handshake, so the phone can say which side to update.

**Pairing (`XXpsk3`).**
- `local-studio pair` (and "Pair a phone" in each desktop app) mints a pairing id (8 random bytes, hex) and a pairing secret (32 random bytes), valid 10 minutes and for one use. Minting a new one cancels any unused one. The desktop shows a QR code and a link: `localstudio://pair?url=wss%3A%2F%2Fstudio-mac.tail1234.ts.net%2Fv1%2Fws&name=studio-mac&id=<pairing id>#<secret, base64url>`. The secret sits in the fragment, so it never reaches a server log.
- The secret is the `psk`. Messages: `-> e`, `<- e, ee, s, es`, `-> s, se, psk`. The third message's payload is the device record: `{"id":"<uuid>","name":"My iPhone","platform":"ios","app":"0.4.0","channel":"nightly"}`. The phone trusts nothing from the server's second message until the handshake completes.
- On success the server burns the secret, stores the device with its static key, and the channel continues as a normal session (`hello` below). The phone stores the machine's static key. A wrong secret fails the handshake at the third message and counts as a failed attempt.

**Every later connection (`KK`).** Both static keys are known. Messages: `-> e, es, ss`, `<- e, ee, se`. Handshake payloads are empty; the first transport message is the phone's `hello`. An unknown or revoked device fails the handshake and gets a close with code 4401; the phone then shows "Pair again" for that machine.

**Records.** After the handshake every WebSocket message is one binary Noise transport message, at most 65,535 bytes. Its plaintext is a record:

| First byte | Meaning | Rest |
| --- | --- | --- |
| `0x01` | JSON frame, final chunk | UTF-8 JSON |
| `0x02` | JSON frame, more chunks follow | UTF-8 JSON |
| `0x03` | blob chunk | 16-byte blob id, 1 byte flags (bit 0: final), data |

A JSON frame is at most 8 MiB once joined; its chunks are sent back to back, never interleaved with another JSON frame. Blob chunks may interleave between JSON frames, so a photo upload never delays a ping. A record that fails to decrypt, or breaks these rules, closes the socket.

**Server limits** (the hosted route has no edge rate limit, since the big assistant sites share egress IPs):
- A socket whose opening header hasn't arrived within 5 s is closed; so is one whose handshake isn't complete within 10 s.
- At most 8 sockets at a time that haven't finished a handshake.
- 5 failed handshakes or bad headers from one client address within a minute refuse that address for 10 minutes (close code 4429).
- Each failure is logged with the address, the pattern and the reason, never a secret, key or payload.

**Revocation.** Desktop settings list paired devices and revoke one, which deletes its static key and closes its live sockets. "Revoke all devices" deletes every device; Local Studio calls it before bringing a tunnel back after a suspension for a second connector, and the phones pair again.

## Session

The phone's first transport frame:

```json
{"op": "hello", "device": {"name": "My iPhone", "app": "0.4.0", "channel": "nightly"}, "resume": {"seq": 412, "epoch": "<server epoch>", "watch": ["t1"]}}
```

The server answers:

```json
{"op": "hello", "protocol": 2, "epoch": "<id>", "machine": {"id": "…", "name": "studio-mac", "os": "macos", "version": "0.4.0", "channel": "nightly", "urls": ["wss://studio-mac.tail1234.ts.net/v1/ws", "wss://studio-mac.example-tunnel.net/v1/ws"]}, "resume": "delta", "snapshot": []}
```

- `protocol` is the highest version inside both windows; the socket speaks it until it closes.
- `machine.urls` lists every route the machine is reachable on now. The phone replaces its stored list with it, so a hostname that changed after 30 days down is learned on any route that still works.
- **Resume by `seq`.** `seq` counts updates per device and server `epoch` (a new epoch on every `serve` start). The server keeps each device's last 2,000 updates or 10 minutes, whichever is less. When `resume.epoch` matches and `resume.seq` is still in that buffer, the server answers `"resume": "delta"`, sends the missed updates in order, then live ones. Otherwise it answers `"resume": "snapshot"` with a full snapshot (`connection`, `projects`, `threads`, `models`, `theme`) and a `transcript` `reset` for each thread in `resume.watch`.
- **Reconnect.** After a drop the phone waits 1 s, doubling to 30 s, with full jitter, and tries its URLs in order of last success. It resets the wait after a socket stays up 60 s and reconnects at once when the app comes to the foreground or the network path changes.
- `ping`/`pong` every 20 s from the phone; the server closes a socket silent for 60 s.

## Frames

Client to server (requests run concurrently; `id` is the client's counter):

```json
{"id": 7, "op": "send", "args": {"request": "r7", "thread": "t1", "prompt": {"text": "ship it", "images": ["<blob id>"], "files": []}, "delivery": "queue"}}
```

The server answers each with `{"re": 7, "ok": true}` once accepted, or `{"re": 7, "error": {"code": "…", "message": "…"}}`. Outcomes arrive as updates. `request_done` and `request_failed` carry the `request` string, so a retried `send` is idempotent: the server drops a second `send` with a `request` it has already accepted.

Server to client: `{"op": "update", "seq": 413, "update": {"type": "transcript", "thread": "t1", "op": {"type": "insert", "index": 18, "item": {}}}}`.

**Transcript window.** `reset` carries at most the last 200 items plus `{"earlier": <count>}`; `load_earlier {"thread", "before": <index>, "limit"}` pages back.

## Blobs

Images, attachments and file contents never go in JSON. They travel as blob records inside the encrypted channel. They don't use a separate HTTP path, because TLS ends at Cloudflare and an HTTP side channel would skip the end to end encryption.

- **Upload.** The phone picks a random 16-byte id, sends `{"id": 8, "op": "blob_put", "args": {"blob": "<hex id>", "size": 183204, "mime": "image/heic"}}`, then the blob's chunks. The server acknowledges after the final chunk. A `send` names blobs by id. An unreferenced blob is dropped after 10 minutes; at most 25 MiB per blob and 100 MiB pending per device.
- **Download.** `load_message_image` and `read_file` answer with `{"type": "blob", "request": "…", "blob": "<hex id>", "size": …, "mime": "…"}` followed by the chunks. The phone caches by `(machine, thread, entry, index)`.

## Ops, frozen for protocol 2

Requests the phone may send. Each is a `Studio` or `Workspace` method under the same name, with its arguments as named fields. The server refuses any other op with error code `not_remote`; the phone never needs settings, provider keys, terminals, daemon control or project setup.

| Area | Ops |
| --- | --- |
| Session | `hello`, `ping`, `watch` `{thread}`, `unwatch` `{thread}`, `load_earlier` `{thread, before, limit}`, `blob_put` `{blob, size, mime}` |
| Turns | `send` `{request, thread, prompt, delivery}`, `abort` `{thread}`, `send_queued_now` `{thread, queued}`, `remove_queued` `{thread, queued}`, `answer_approval` `{thread, request, answer}`, `answer_questions` `{thread, request, answers}`, `compact` `{thread}` |
| Threads | `start_thread` `{draft, project, prompt, settings}`, `rename_thread` `{thread, title}`, `regenerate_title` `{thread}`, `fork_thread` `{thread}`, `set_archived` `{thread, archived}`, `set_pinned` `{thread, pinned}`, `set_settled` `{thread, settled}`, `snooze` `{thread, until}`, `mark_unread` `{thread, unread}`, `search_threads` `{request, query, limit}`, `list_archived` `{request}` |
| Agent | `load_models`, `set_model` `{thread, provider, model}`, `set_thinking_level` `{thread, level}`, `set_access_mode` `{thread, mode}`, `list_commands` `{request, project}`, `list_skills` `{request, project}`, `cancel_subagent` `{thread, child}` |
| Read only | `load_message_image` `{request, thread, entry, index}`, `git_status` `{project}`, `load_diff` `{request, project, scope}`, `list_pull_requests` `{request, project}`, `load_pull_request` `{request, project, number}`, `read_file` `{request, project, relative}` |

Updates the phone receives (`type`):

| Scope | Types |
| --- | --- |
| Global | `connection`, `projects`, `threads`, `thread_started`, `models`, `daemon_default`, `notification`, `theme`, `notice`, `request_done`, `request_failed` |
| Watched thread | `thread`, `transcript`, `approvals`, `questions`, `questions_settled`, `queue`, `subagents`, `worktree_setup` |
| Answers to a request | `thread_search`, `archived_threads`, `commands`, `skills`, `git_status`, `diff`, `pull_requests`, `pull_request_detail`, `blob` |

Adding an op or update type, or a field a client must understand, raises `protocol`. A server ignores unknown fields in requests, and a client ignores unknown update types and fields.

`local-studio-core`'s `remote_smoke` example starts `serve` on a scratch data dir, pairs a client through `XXpsk3`, reconnects through `KK`, and drives every op in the table once over a real socket against the real model, checking each reply and the updates it causes. It also checks the refusals: a wrong pairing secret, a revoked device, a `Host` not allowed, an op not in the table, and the rate limit.

## Theme

`{"type": "theme", "theme": {…}}` goes in every snapshot and on every change. The payload is the resolved theme for the `studio` surface in the shape of Polycarp's `theme.json` (polycarp spaces/spec/themes.md): `schema`, `mode`, `light` and `dark` with the twelve color roles as `#rrggbb` (background, surface, sunken, foreground, strong, muted, border, accent, selection, urgent, working, done) plus `on_accent`, and `radius_large`, `radius`, `radius_small`, `border_width`, `blur`. The server resolves it; the phone never reads TOML or a file. The phone maps the roles onto its own: background to canvas, surface to cards, sunken to code, foreground to text, muted to secondary text, border to hairlines, accent to the primary tint, urgent, working and done to the status colors.

## Many machines

The phone holds one socket per paired machine and merges what they send by `machine.id`. Each thread row carries its machine; Home shows every machine's threads together; a request goes to the socket of the thread's machine. A machine that is offline keeps its last known rows, marked with how old they are.

## Notifications when the phone is asleep

- `notification` updates (`turn_done`, `needs_approval`, `needs_answer`, `failed`) go to every connected client; the app shows them as local notifications while it has a socket.
- When the app is suspended, iOS keeps no socket. APNs needs a push key from the user's paid Apple Developer account and a relay that holds it. `serve` takes notify hooks (`remote.notify: [{"kind": "webhook", "url": …}]`) and posts a notification's kind and thread id to them, never message text. v1 uses an ntfy style webhook; APNs through the tunnels lead's hosted service is v2 and is the only piece that needs the paid account.

## Question cards

Local Studio has command approvals but no request where the agent asks the user a question with options, and Eukhe's daemon has only `shell_approval` (issue #19 tracks the upstream change). In T3 it is the request the owner answers most: 179 in 30 days against 18 command approvals. Shape, copying the shell approval flow: daemon frame `user_question {active_session_id, request: {request_id, questions: [{header, question, options: [{label, description}], multi_select}]}, agent}`, answer `answer_user_question {request_id, answers: {<question>: <answer>}}`, and `user_question_settled`. In core: `Update::Questions { thread, request, questions }`, `Update::QuestionsSettled { thread, request }` and `Studio::answer_questions(thread, request, answers)`, carried on this wire unchanged.

## Protocol window

Each side states the newest protocol it speaks (`protocol`) and the oldest it still accepts (`min_protocol`), and keeps `min_protocol` at `protocol - 1` once a version has shipped (docs/channels.md: both sides speak their protocol and the one before it). Protocol 1 was the unencrypted draft and never shipped, so the first release is protocol 2 with `min_protocol` 2. When the windows do not overlap, the server sends `incompatible` before the handshake and closes; the phone names the machine, its version and channel, and which side to update. `/v1/health` reports both numbers so the phone can say this before pairing.

## Channels

docs/channels.md is the scheme. The iPhone app installs side by side: `org.local.studio.ios` ("Local Studio"), `org.local.studio.ios.nightly` ("Local Studio Nightly"), `org.local.studio.ios.dev` ("Local Studio Dev"), with URL schemes `localstudio`, `localstudio-nightly` and `localstudio-dev`. Pairing links use the scheme of the channel the desktop app is on. The phone carries the desktop app's version and tags, so a phone and a desktop from the same channel always share a protocol. The server reports its version and channel in `hello`, and the phone shows them on the Machines sheet.

## Gaps against T3 Code

- **Question cards**, above.
- **Thread list paging.** `threads` sends every row. That's fine at hundreds of threads; T3 pages settled threads 25 at a time. Measure before adding paging.
