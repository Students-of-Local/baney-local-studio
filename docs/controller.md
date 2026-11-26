# Local controller and fleet

The controller is a small Bun service (`controller/`). It watches the inference servers on one machine and serves them to the rest of your machines. Each machine runs its own controller. Controllers that share a fleet key link into one graph.

## Where it runs

| Situation    | How it runs                                                                                                                                                                                                                                                                                                                        |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| User service | Installed by a tailnet deploy (below). On Linux it is the systemd unit `local-studio-controller` with linger enabled, falling back to a background process when no user systemd is available. On macOS it is the LaunchAgent `ai.localstudio.controller`. The binary is `~/.local/bin/local-studio-controller` and it binds the machine's Tailscale IPv4. |
| Development  | `bun controller/src/main.ts`; `cd controller && bun run typecheck && bun run test` checks it.                                                                                                                                                                                                                                       |
| Manual       | Download `local-studio-controller-<os>-<arch>` from a release and run it.                                                                                                                                                                                                                                                          |

## Configuration

`~/.local-studio-controller/config.json` is written on first start with mode 0600. Older installs that keep their data in `~/.local-studio-t3` keep working: when only that directory has a `config.json`, the controller reads and writes it in place.

| Field                   | Meaning                                                                          |
| ----------------------- | -------------------------------------------------------------------------------- |
| `id`, `name`            | Machine identity shown in the UI                                                 |
| `url`                   | The URL other controllers use to reach this one                                  |
| `fleetKey`              | Shared secret; every request except `/api/health` must send it as a bearer token |
| `peers`                 | URLs of linked controllers                                                       |
| `linkPeers`             | Optional. Machines reached over a desktop link; a node poll they answer 503 `busy` is not a miss. See [docs/fleet-links.md](fleet-links.md) |
| `linkToken`             | Optional. The bearer sent to link peers instead of `fleetKey`; dropped with the last link peer |
| `excludePorts`          | Local ports never treated as inference servers (18095, the link proxy, always is) |
| `enginePorts`           | More local ports to ask for models; see [Discovery](#discovery)                  |
| `engineKeys`            | API keys for local engines, by port                                              |
| `registry`, `modelsDir` | Recipe registry checkout and weights directory                                   |
| `cluster`               | Optional. Tailnet discovery, GPU lending and pooled models; see [Clustering](#clustering) |

The following environment variables change the defaults:

| Variable                          | Default                                                                    |
| --------------------------------- | -------------------------------------------------------------------------- |
| `LOCAL_STUDIO_CONTROLLER_HOME`    | `~/.local-studio-controller`                                               |
| `LOCAL_STUDIO_CONTROLLER_PORT`    | `18091`                                                                    |
| `LOCAL_STUDIO_CONTROLLER_HOST`    | `127.0.0.1`                                                                |

To link machines by hand:

1. Bind each controller to its tailnet address with `LOCAL_STUDIO_CONTROLLER_HOST`.
2. Set `url` to that address.
3. Give every machine the same `fleetKey`.
4. Link the peers with `POST /api/peers`.

## Installing on a tailnet machine

`GET /api/tailnet` lists online Linux and macOS devices from `tailscale status`, with or without a controller. `POST /api/tailnet/deploy` installs one, from your machine's controller:

1. `ssh <tailnet-ip> sh -s` with `BatchMode`, so key-based access is required. A new host key is accepted.
2. It refuses if anything answers at the machine's port 18091, or if `~/.local-studio-controller/config.json`, `~/.local-studio-t3/config.json` or `~/.local/bin/local-studio-controller` exists there. An existing controller is never replaced.
3. It sends its own code over that ssh connection; nothing is downloaded. Where the machine has [Bun](https://bun.sh) 1.3 or newer, that is one script run by it. Without Bun, a machine of the same system and processor gets a compiled binary, and any other is refused before anything is written. A controller that is itself a compiled binary sends that binary, and only to the same kind of machine. A machine that cannot run what it was sent is refused. A refused or failed install removes the upload and leaves no config.
4. It writes a config with your fleet key and the device's tailnet URL. The key is never on a command line.
5. It installs and starts the user service.
6. It waits until `/api/health` answers, then adds the device to your peers.

To remove a controller, stop the user service on that machine and delete `~/.local/bin/local-studio-controller` and `~/.local-studio-controller`. Then unlink the peer with `POST /api/peers`.

## API

All routes except `/api/health` and `/api/cluster/hello` require the fleet key. A request without the key is answered only under a host name that is an IP address, a name without dots, a `.ts.net` name, or the host in `url`; any other gets 421.

| Route                                                                                             | Purpose                                                               |
| ------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------- |
| `GET /api/health`                                                                                 | Identity and version                                                  |
| `GET /v1/models`, `POST /v1/chat/completions`, `/v1/completions`, `/v1/messages`, `/v1/responses` | Gateway across the whole fleet; `auto` selects the busiest live model |
| `GET /api/snapshot`, `/api/node`, `/api/recipes`                                                  | State for the Local settings page                                     |
| `GET /api/telemetry`, `/api/telemetry?history=1`                                                  | Live engine and GPU figures for the Local page; history adds series   |
| `GET /api/tailnet`, `POST /api/tailnet/deploy`                                                    | Tailnet scan and install                                              |
| `POST /api/recipes/<id>/run`, `/api/runs/<id>/stop`, `/api/ports/<port>/stop`                     | Launch and stop engines                                               |
| `POST /api/name`, `/api/peers`                                                                    | Rename, link and unlink                                               |
| `GET /api/registry`, `/api/registry/records/<id>`, `POST /api/registry/download`                  | Registry models matched to this machine's hardware, records, weights  |
| `GET /api/registry/share?port=<port>`, `POST /api/registry/share`                                 | Preview and share a running server's config to local-ai-registry      |
| `GET /api/cluster`, `POST /api/cluster`                                                           | Lending state, checks, power budget, machines and pools; set `discover`, `lend` and `power` |
| `POST /api/cluster/pool`, `/api/cluster/pool/<id>/stop`                                           | Launch (or `dryRun`) and stop a pooled model                          |
| `POST /api/cluster/rpc`, `/api/cluster/rpc/stop`, `/api/cluster/release`                          | Controller to controller: lend an `rpc-server`, stop it, take it back |
| `POST /api/cluster/hello`                                                                         | Discovery handshake; needs no key, answers only with a keyed proof    |
| `POST /api/thunderbolt/authorize`                                                                 | Approve a pending Thunderbolt or USB4 device with `boltctl`           |
| `POST /api/hardware/gpus/<id>/eject`, `/api/hardware/gpus/<id>/eject/cancel`                      | Safely eject an external GPU, or cancel and use it again              |

Registry launches use pinned container images and weight revisions on free matching NVIDIA GPUs, on ports 18100 to 18299. They never evict running engines.

A launch may set only the container options in `controller/src/registry.ts` (`ALLOWED_FLAGS`: memory, CPU, ulimit, user, workdir and shared memory settings, `--init`, and the `IPC_LOCK` and `SYS_NICE` capabilities), each with one value of a fixed form; anything else makes the recipe read-only. The controller sets the network, ports, GPUs, name and restart policy itself. A recipe that asks for the host's IPC namespace gets its own with a 16 GB `/dev/shm`, or the size the recipe names.

The registry backs captured fleet configurations up next to the catalog. They are listed but stay read-only, as do host-specific, privileged and multi-machine launches; the API says why instead of quietly rewriting them. Missing weights or scripts are reported, never invented. Cancelling a request is forwarded to the engine, but an engine that ignores client disconnects (the tested SGLang build, for example) may keep generating. `auto` ranks successful requests these controllers have observed, not an engine's earlier traffic.

`GET /api/registry` reads `data/registry` from the same checkout. It matches each machine's GPUs, or an Apple Silicon chip and its unified memory, to registry hardware records within 1 GB, groups recipes by Hugging Face repository, and keeps one variant per hardware, engine, format and precision. `GET /api/registry/records/<id>` returns the full recipe, model instance, model and hardware records, and `POST /api/registry/download` runs `hf download` at the pinned revision on that machine.

`GET /api/registry/share?port=<port>` builds registry records from a running server's launch command or container, sends one short validation request, scrubs credentials, home paths, hostnames, private addresses and device IDs, and validates the records against the registry's JSON Schemas at the checked-out commit. Nothing is sent until `POST /api/registry/share`; the controller then forks `0xSero/local-ai-registry` and opens the pull request with your `gh` CLI login. `"dryRun": true` lists the `gh` calls without running them.

## Discovery

Every scan (about every 10 seconds) the controller lists the machine's TCP listeners and asks the ones that can be an engine for `GET /v1/models`, with the port's `engineKeys` entry as a bearer token. Those are the ports of launched jobs, the ports in `enginePorts` and `engineKeys`, the usual engine ports (`PORTS` in `controller/src/discovery.ts`), and any port whose process is a known server such as vLLM, SGLang, llama.cpp or Ollama (`SERVERS` there, matched on the first words of its command line). Nothing is sent to any other listener. An engine in a container, or run by another user, shows no process: if its port is not a usual one, add it to `enginePorts`. New ports get 1.5 seconds to answer; ports that already answered get 10 seconds, so a busy engine that answers slowly stays loaded.

| Endpoint `state` | Meaning                                                                                                     |
| ---------------- | ----------------------------------------------------------------------------------------------------------- |
| `loaded`         | The engine listed models. It stays `loaded` through two failed checks in a row (`failures`, `error` set)    |
| `empty`          | The engine answered with an empty model list                                                                |
| `unreachable`    | Three checks in a row failed (timeout, refused, 401/403, 5xx). The last models stay listed until forgotten |

`lastSeenAt` is the last answer, `checkedAt` the last check, `latencyMs` the last answer's time. A port that no longer listens is dropped at once; an unreachable one after 10 minutes. Only answers that are clearly not an inference server (404, non-JSON, another controller) are skipped for 5 minutes, never a port that already answered or a launched job's port. If the listener scan itself fails, the previous state is kept. A peer that misses `/api/node` (8 seconds) keeps its last state, flagged with the error, for two more walks before it shows as unreachable.

The Local page also lists the OpenAI-compatible custom endpoints in Eukhe's `models.json` whose host is private (loopback, private or tailnet addresses, `.local`, `.ts.net` and similar). Local Studio's core asks each one for `/models` itself with the key from Eukhe's `auth.json` or `models.json`, so the key never reaches a controller. An endpoint on a machine that runs a controller joins that machine; otherwise it shows as its own machine with its models, reachability and response time, and the page says that hardware and decode figures need a controller there.

## Telemetry

Every 5 seconds the controller reads `nvidia-smi` (memory, power draw and limit, temperature, utilization per GPU) and the Prometheus `/metrics` of each live engine on the machine. It understands vLLM (`vllm:`), SGLang (`sglang:`) and llama.cpp (`llamacpp:`, started with `--metrics`) names, summed across labels:

| Figure        | Metric                                                                                            |
| ------------- | ------------------------------------------------------------------------------------------------- |
| Decode tok/s  | Rate of `generation_tokens_total` / `tokens_predicted_total` over the last 16 seconds             |
| First token   | `time_to_first_token_seconds` sum ÷ count over the last hour (24 hours when idle); best is the lowest minute |
| In flight     | `num_requests_running` / `num_running_reqs` / `requests_processing`; peak over 24 hours           |
| KV cache      | `kv_cache_usage_perc` (or `gpu_cache_usage_perc`) / `token_usage` / `kv_cache_usage_ratio`; peak over 24 hours |
| Tokens per day | `prompt_tokens_total` (read) and `generation_tokens_total` (written), per UTC day                |

`telemetry.json` in the controller's data directory keeps per-minute figures for 24 hours, per-day token counts for 800 days, and the last counter values per engine process, so a controller restart counts what the engine produced meanwhile and an engine restart is not counted twice. Engines without `/metrics` (Ollama, LM Studio, ExLlama) show their GPUs but no engine figures.

## External GPUs

On Linux the controller marks a GPU as external when its PCI device sits behind a port the kernel calls `removable`, or under the upstream bridge of a discrete Thunderbolt or USB4 controller. Each GPU reports its PCIe link (`gen`, `width` and their maximums) with a plain bandwidth note. A PCIe 3.0 x4 enclosure moves about 3.9 GB/s, so loading a model takes longer than on an internal slot, while decoding speed is nearly the same.

`hardware.thunderbolt` lists Thunderbolt and USB4 devices from `boltctl list --all` (or sysfs when bolt is missing). A device the system has not approved shows as `pending`. `POST /api/thunderbolt/authorize` with its `uuid` runs `boltctl enroll --policy auto <uuid>`. If polkit wants an administrator password the call returns 403 `NEEDS_PASSWORD` and says how to approve it by hand.

### Ejecting an external GPU

Each external GPU in `hardware.gpus` carries `eject`: `state` is `ready`, `busy` (with `blockers`, the programs outside Local Studio on it), `needs_root` (with the `command` to run) or `unsupported`, plus `draining`. `POST /api/hardware/gpus/<id>/eject` takes the GPU's index, UUID or PCI bus id and:

1. Marks the GPU draining. Until the eject ends or is cancelled, a load on it answers 409 `DRAINING`, and the machine lends and pools nothing.
2. Unloads every model the controller started on it (registry runs and pooled models), through the same stop as an unload.
3. Lists every program still on it, graphics clients such as Xorg included, with `nvidia-smi -q -d PIDS -i <bus id>`. Any left were started elsewhere, so it answers `busy` with their pid, name and MiB and never signals them. It also answers `busy` while the GPU drives a display, or while persistence mode holds the driver open (with `sudo nvidia-smi -i <bus id> -pm 0` as the `command`).
4. With nothing left and the eject not cancelled meanwhile, writes `1` to `/sys/bus/pci/devices/<addr>/remove` from a child process with a 30 second limit and answers `removed`. When that file is not writable it answers `needs_root` with `echo 1 | sudo tee /sys/bus/pci/devices/<addr>/remove`, and the GPU stays draining until it disappears or the eject is cancelled. `boltctl` cannot deauthorize a device without root, so Thunderbolt authorization is left alone.

An internal GPU answers 404 `NOT_EXTERNAL`. An Apple silicon Mac and Windows answer `unsupported`; an Intel Mac runs `SafeEjectGPU Eject` when it has the tool. `POST .../eject/cancel` clears draining. From a terminal: `local-studio hardware eject <id> [--json]` and `local-studio hardware eject --cancel <id>`; the first exits 1 unless the GPU was removed.

`cd controller && node test/eject.mjs` checks this against a real controller with a stub `nvidia-smi` and `docker` on `PATH` and a scratch sysfs (`LOCAL_STUDIO_SYSFS`); `LOCAL_STUDIO_TEST_PLATFORM=darwin-arm64` stands in for an Apple silicon Mac. With `LOCAL_STUDIO_CLI=<target>/debug/local-studio` it also drives the CLI.

The controller reads the GPU list every 2 seconds, so a GPU that is plugged in or pulled shows within about 2 seconds. A removed GPU fails every job on it with "<GPU> was unplugged." and stops any `rpc-server` lent from it. `hardware.hotplug` keeps the last eight changes for an hour; an added GPU lists up to five recipes and models that fit its memory.

Apple Silicon Macs cannot use an external GPU, so `hardware.egpuNote` points the user to a GPU on another machine in the fleet.

## Clustering

Clustering is off until a machine opts in with a `cluster` block in `config.json`:

| Field         | Default        | Meaning                                                                                   |
| ------------- | -------------- | ----------------------------------------------------------------------------------------- |
| `discover`    | `false`        | Find other controllers on the tailnet and link those that share the fleet key             |
| `lend`        | `"never"`      | `never`, `idle` (lend when every check passes; see [Power budget](#power-budget) for lending while the owner is present) or `always` |
| `power`       | `{ "activeWatts": 0, "awayWatts": null }` | Watt caps for fleet GPU work while the owner is present and while away; see [Power budget](#power-budget) |
| `ports`       | `[18091]`      | Controller ports to probe on each online tailnet device                                   |
| `idleSeconds` | `120`          | How long every check must pass before an `idle` machine lends                             |
| `drainSeconds`| `5`            | How long a reclaim waits for requests in flight before it stops lent work                 |
| `failoverMs`  | `60000`        | How long a streaming request waits for headers before it moves to the next machine        |
| `rpcPort`     | `50052`        | Tailnet port for the lent `rpc-server`                                                    |
| `rpcServer`, `llamaServer` | `rpc-server`, `llama-server` | llama.cpp binaries                                                 |

### Discovery

Every 15 seconds a discovering controller asks `tailscale status --json` for online devices and probes `GET /api/health` on each of `ports`. When a controller answers with `cluster: true`, the prober sends `POST /api/cluster/hello` with a random nonce and the address it called. The other side answers only if that address is exactly `http://<its own address>:<its port>`, with an HMAC of the nonce, its id and that address, each field separate, under the fleet key. Ids hold only letters, digits, `_`, `.` and `-`; a controller with any other id is never linked by discovery. The proof format changed on 2026-11-24, so a controller older than that is not discovered by a newer one, or the other way around, until both are updated; existing links keep working. A controller with another key fails the proof and is never linked, and the address check stops one controller from relaying another's proof. Found controllers are added to `peers`. A controller you unlink with `POST /api/peers` is remembered in `cluster.ignored` and is not linked again by discovery until you add it back. When the controller listens on loopback, discovery also opens a listener on the machine's tailnet IPv4 address.

### Lending

Every 2 seconds a lending machine runs its checks:

| Check          | Passes when                                                                                  |
| -------------- | -------------------------------------------------------------------------------------------- |
| GPU programs   | No GPU compute process outside the fleet (the controller's engines, jobs and lent servers, found by process tree, container id or the `LOCAL_STUDIO_FLEET_OWNER` tag) |
| GPU use        | No GPU is 10% or more busy without a fleet process on it                                     |
| Power          | The machine is on AC power                                                                   |
| parish-media   | Where `~/.local/bin/parish-media` exists, its `status` is ready, or the only programs it lists are fleet processes and the machine is on AC with no media jobs |
| Owner          | Nobody is present (below), or someone is and `power.activeWatts` is above 0                  |

The Owner check reads presence. On a Mac, someone is present when the keyboard or mouse was touched in the last minute (`HIDIdleTime`). On Linux, someone is present when logind has an active local graphical user session (`x11`, `wayland` or `mir`, class `user`, not remote) whose idle hint is off or turned on less than a minute ago; greeter and lock-screen sessions don't count, a locked session (`LockedHint`) counts as away, and a machine with no graphical user session counts as away. Some desktops never set the idle hint (Hyprland, many window managers): logind then says "not idle" from login onward, so the controller reports presence as unknown (`present: null`) rather than present, and says so in `presenceSource`. Where presence can't be read the check is left out, and the away cap applies.

`idle` lends once every check has passed for `idleSeconds` and reclaims the moment one fails. `always` lends whenever the machine has a GPU and is on AC power. On a reclaim the controller marks itself draining, tells the machines it lends to, waits up to `drainSeconds` for requests in flight, then stops only what it started for the fleet: lent `rpc-server` processes and jobs launched with `lend: true`. A lent job is failed with "<machine> took its GPU back: <reason>". The `reason` field says in plain words why a machine is or is not lending.

### Power budget

`cluster.power` caps how many watts fleet work may draw:

| Field         | Default | Meaning                                                                                      |
| ------------- | ------- | -------------------------------------------------------------------------------------------- |
| `activeWatts` | `0`     | Cap while the owner is present. `0` means the machine does not lend while someone is using it |
| `awayWatts`   | `null`  | Cap while the owner is away; `null` means no cap                                             |

Both are whole watts up to 2000; `activeWatts` may be 0, `awayWatts` starts at 1 (use `lend: "never"` to stop lending). Set them with `POST /api/cluster` and `{ "power": { "activeWatts": 60, "awayWatts": null } }`; fields left out keep their value. With `lend: "idle"` and `activeWatts` above 0, the Owner check passes while someone is present and the machine lends under `activeWatts`; GPU programs, GPU use, parish-media and battery still block and still reclaim at once. `always` uses `awayWatts` while the owner is away, `activeWatts` while present when it is above 0, and otherwise no cap. A machine on battery never lends.

The controller caps fleet work with one of two levers, chosen per GPU:

- `nvidia-limit`: only while the owner is known to be away (`present: false`), because the limit is board-wide: set while someone is at the machine it would also throttle their own games and apps, which `nvidia-smi` doesn't list as compute processes. While the owner is present, or presence is unknown, every GPU uses the duty cycle, and a GPU held at a limit gets its original back first. On an NVIDIA GPU it runs `sudo -n nvidia-smi -i <index> -pl <watts>`, the cap split evenly across the GPUs in use and clamped to `power.min_limit` and `power.max_limit`. Before the first change it records the GPU's limit by uuid in `<data root>/power-restore.json` (mode 0600). It puts the original back when the machine reclaims, when the cap goes away, on SIGTERM or SIGINT, and at start (after a crash), then drops the record. `sudo` needs a rule that allows `nvidia-smi` without a password: `local-studio fleet power-helper --yes` writes `/etc/sudoers.d/local-studio-power`, which allows only `<nvidia-smi path> -i <GPU index> -pl <watts>` for the user who ran it. The path is the first `nvidia-smi` on `PATH` resolved through symlinks, and the helper refuses unless root owns that file and every directory above it and no other user or group can write any of them, because a rule for a file the user could replace would hand root to anything running as the user. The rule never passes through a file the user can write: it is checked with `visudo -cf -` on stdin, piped by `sudo install` into `/etc/sudoers.d/.local-studio-power.new` (sudo skips names with a dot), checked again there with `sudo visudo -cf`, then moved into place. The controller calls the same resolved path. When `sudo -n` fails, or a GPU reports no adjustable limit, that GPU falls back to the duty cycle and `reason` says why. It tries `sudo` again a minute later, as soon as the helper reads `installed`, and whenever the budget changes. While no fleet work is running on the machine, no limit is set.
- `duty-cycle`: needs no root. Every 200 ms the controller lets the processes it started for lending run for part of the period and stops them (`SIGSTOP`, then `SIGCONT`) for the rest: lent `rpc-server` processes, by the PIDs it spawned. Lent jobs, which are containers, are paused with `docker pause` and `docker unpause` on a 2 second period. It never signals a process it did not start. On NVIDIA it reads `power.draw` from `nvidia-smi -lms 200` and steers the share of time that fleet work runs so the draw stays at or under the cap. When `nvidia-smi` gives no draw reading for 2 seconds, it runs fleet work `cap / limit` of the time (the GPU's power limit, or 150 W when it reports none), reports `measured: false`, and says so in `reason`. On Apple Silicon it reads `gpu_power` from `macmon pipe` when `macmon` is on `PATH`; without it there is no watt reading, so it runs fleet work `cap / 30 W` of the time (10% to 100%), reports `measured: false`, and says so in `reason`. Every stopped process is continued on reclaim, when the cap goes away, and on exit. While it stops or pauses anything it keeps their PIDs (each with its command name) and container names in `<data root>/power-paused.json`, so after a crash or `SIGKILL` the next start continues each PID whose command name still matches and unpauses each container, then drops the file.

`GET /api/cluster` reports the budget under `power`:

| Field            | Meaning                                                                                  |
| ---------------- | ---------------------------------------------------------------------------------------- |
| `activeWatts`, `awayWatts` | The configured caps                                                            |
| `present`        | `true`, `false`, or `null` when presence can't be read or lending is off                 |
| `presenceSource` | How presence was read, such as `macOS input idle time` or `logind session 2 (wayland)`   |
| `capW`           | The cap in force now, or `null`                                                          |
| `lever`          | `nvidia-limit`, `duty-cycle` or `none`                                                   |
| `measured`, `measuredW` | Whether draw is measured, and the fleet GPUs' draw now in watts (or `null`)       |
| `helper`         | `installed` when `sudo -n` may run `nvidia-smi -pl`, `missing`, or `unsupported` (not Linux with NVIDIA) |
| `maxW`           | The most fleet work could draw here: the GPUs' summed `power.max_limit`, 30 W on Apple Silicon, or `null` |
| `gpus`           | Per GPU: `uuid`, `index`, `lever`, `limitW` and `originalW`                              |

`reason` stays plain, for example "In use, so it lends at up to 60 W." `LOCAL_STUDIO_TEST_PRESENT` is for checks only: `1` or `0`, or the path of a file holding `1` or `0`, replaces the presence reading.

### Routing

The gateway ranks every live copy of a model by load: lent machines that took their GPU back last, then requests queued or running (from the engine's metrics or the controller's own count), KV cache use, decode speed, local before remote, and fewer hops. A streaming request moves to the next copy on any error, on a 502, 503 or 504, or when no headers arrive within `failoverMs`. A non-streaming request moves only when no engine could have started it: a connection that was refused or never opened, a 503 from a local engine, or a peer answer marked `x-local-studio-retry: 1`. Any other error or timeout returns 502 or 504 at once, so a generation never runs twice. A response that moved carries `x-local-studio-failover`.

### Pooled models

`GET /api/cluster` lists the GGUF models in `modelsDir` and the Hugging Face cache, with `fitsOne` (fits the largest single machine's free GPU memory) and `fitsPool` (fits the pooled free memory of lending machines). `POST /api/cluster/pool` with a GGUF `model` asks every lending machine (or `members`) for an `rpc-server`, then runs `llama-server -m <model> --rpc <endpoints> -ngl 999 --metrics` on a port in 18100 to 18299. `"dryRun": true` returns the argv without starting anything. The pooled model is served through the gateway like any local engine.

Each lent `rpc-server` listens on loopback only. A gate on the tailnet IP at `rpcPort` forwards connections only from the address of the controller that asked for that server and drops every other address, because `rpc-server` has no authentication of its own. Programs run by other users on the lending machine can still reach the loopback port, so lend only from machines whose local accounts you trust. When a member reclaims, becomes unreachable or stops lending, the pool is torn down and shows why. When the controller gets SIGTERM or SIGINT it stops the `rpc-server` and pooled `llama-server` processes it started before it exits.

