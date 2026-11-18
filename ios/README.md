# Local Studio for iPhone

The native iPhone client for Local Studio Remote (`docs/remote.md`). `Packages/LocalStudioRemote` is the wire client and data layer; the app target is a plain debug shell until the design lands.

## Signing (once)

1. Open `ios/LocalStudio.xcodeproj` in Xcode.
2. Pick your team under Signing & Capabilities for the `LocalStudio` target and the `LocalStudioSmoke` target; if Apple says a bundle id is taken, change `LS_BUNDLE_PREFIX` in `project.yml` and run `xcodegen generate`.
3. Plug in your iPhone and select it as the run destination.
4. Choose the `LocalStudio Dev`, `LocalStudio Nightly` or `LocalStudio` scheme.
5. Press Run.

## Channels

Each channel is its own install with its own data and Keychain items (`docs/channels.md`).

| Scheme | Configuration | Bundle id | Name | URL scheme |
| --- | --- | --- | --- | --- |
| `LocalStudio Dev` | Dev | `org.local.studio.ios.dev` | Local Studio Dev | `localstudio-dev` |
| `LocalStudio Nightly` | Nightly | `org.local.studio.ios.nightly` | Local Studio Nightly | `localstudio-nightly` |
| `LocalStudio` | Release | `org.local.studio.ios` | Local Studio | `localstudio` |

`MARKETING_VERSION` is the workspace `Cargo.toml` base version. `CURRENT_PROJECT_VERSION` is `0` for local builds; release builds pass `CURRENT_PROJECT_VERSION=YYYYMMDD.HHMMSS` (UTC) and `LS_VERSION=<full SemVer channel string>` on the `xcodebuild` command line. Without `LS_VERSION`, the build stamps `X.Y.Z-dev.0+local.<sha7>` into the `LSVersion` Info.plist key, which the app sends as `app` in the pairing device record and in `hello`.

## Commands

From the repository root:

```sh
cd ios && xcodegen generate
xcodebuild -project ios/LocalStudio.xcodeproj -scheme "LocalStudio Dev" -destination 'generic/platform=iOS Simulator' build
ios/tools/smoke.sh
cd ios && swiftlint lint --strict
```

`project.yml` is the source of the Xcode project; edit it and run `xcodegen generate`, and commit both.

## Transport

`MachineConnection` keeps four layers apart. The socket moves opaque messages: one text opening header, then binary only. `SecureChannel` takes the header as the Noise prologue, runs the handshake (`XXpsk3` to pair, `KK` after) and seals and opens each message. `RecordReader` and `RecordWriter` carry the `0x01`, `0x02` and `0x03` records and join chunked JSON. `FrameCodec` turns JSON into frames. `PassThroughChannel` is the only `SecureChannel` today: it sends the handshake messages with the right shapes and no cryptography, so it pairs only with `fake-studio`, never with a real `serve`.

Each paired machine is one Keychain item (`AfterFirstUnlockThisDeviceOnly`, not synced) holding this phone's X25519 static key for that machine, the per machine device id, the machine's static public key, its id and its URLs with their last success times.

## Fake server and smoke

`bun ios/tools/fake-studio.ts --port 18094` speaks `docs/remote.md` deterministically over the pass through channel and prints its pairing link: the opening header, `XXpsk3` and `KK` handshake shapes, records with chunking, `hello` with `delta` or `snapshot` resume by `seq` and `epoch` (a snapshot carries the `seq` baseline), `watch`, a streamed reply, a tool call that waits for an approval, the queue, `load_earlier` answered inline over a 260 item thread, `{"duplicate":true}` for a repeated `send`, `busy` once 64 prompts wait, notifications, `ping`, `not_remote` and `/v1/health`, as `remote-serve` answers them. `--pair-id` and `--pair-secret` fix the pairing link; `--protocol N --min-protocol M` sets its protocol window; `--busy-once-after N` answers `busy` to send N + 1 once.

`SMOKE_GATE` (a command prefix, such as a build gate), `SMOKE_JOBS` and `SMOKE_DERIVED_DATA` pass through to `xcodebuild`. `ios/tools/smoke.sh` starts two fake servers (one speaking protocol 2, one speaking 3 to 4), then runs `LocalStudioSmoke` with `xcodebuild test` on an iPhone 17 Pro simulator. The test pairs, opens a thread, sends, sees the streamed reply, queues a message (the fake answers it `busy` once, so it goes through the Outbox retry), approves the tool call, pages back a long thread, kills and relaunches the app to see the cached list before it reconnects through `KK`, checks the incompatible machine, and prints the signpost timings JSON (also attached to the result bundle).

Against a real `local-studio serve` (once the Noise channel replaces the pass through), set `LOCAL_STUDIO_PAIR_LINK` to a fresh link from `local-studio pair`, or `LOCAL_STUDIO_REMOTE_URL`, `LOCAL_STUDIO_PAIR_ID` and `LOCAL_STUDIO_PAIR_SECRET`, before `ios/tools/smoke.sh`. The real run sends one short prompt to the first thread and approves any approval it asks for. Calling `xcodebuild test` directly needs the same values with a `TEST_RUNNER_` prefix.

App launch arguments for testing: `-LSReset YES` wipes machines, cache and credentials; `-LSPairLink <link>` pairs on launch; `-LSConnectDelay <seconds>` holds the first connection; `-LSSmoke YES` shows timings and skips the notification permission prompt.
