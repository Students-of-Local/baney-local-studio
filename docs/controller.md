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
| `excludePorts`          | Local ports never treated as inference servers                                   |
| `engineKeys`            | API keys for local engines, by port                                              |
| `registry`, `modelsDir` | Recipe registry checkout and weights directory                                   |
| `cluster`               | Optional. Tailnet discovery, GPU lending and pooled models; see [Clustering](#clustering) |

The following environment variables change the defaults:

| Variable                          | Default                                                                    |
| --------------------------------- | -------------------------------------------------------------------------- |
| `LOCAL_STUDIO_CONTROLLER_HOME`    | `~/.local-studio-controller`                                               |
| `LOCAL_STUDIO_CONTROLLER_PORT`    | `18091`                                                                    |
| `LOCAL_STUDIO_CONTROLLER_HOST`    | `127.0.0.1`                                                                |
| `LOCAL_STUDIO_CONTROLLER_RELEASE` | `https://github.com/sybil-solutions/local-studio/releases/latest/download` |

To link machines by hand:

1. Bind each controller to its tailnet address with `LOCAL_STUDIO_CONTROLLER_HOST`.
2. Set `url` to that address.
3. Give every machine the same `fleetKey`.
4. Link the peers with `POST /api/peers`.

## Installing on a tailnet machine

`GET /api/tailnet` lists online Linux and macOS devices from `tailscale status`, with or without a controller. `POST /api/tailnet/deploy` installs one, from your machine's controller:

1. `ssh <tailnet-ip> sh -s` with `BatchMode`, so key-based access is required. A new host key is accepted.
2. On the remote machine, it refuses if `~/.local-studio-controller/config.json` or `~/.local-studio-t3/config.json` exists or anything answers on port 18091. An existing controller is never replaced.
3. It downloads the matching binary from `LOCAL_STUDIO_CONTROLLER_RELEASE`.
4. It writes a config with your fleet key and the device's tailnet URL.
5. It installs and starts the user service.
6. It waits until `/api/health` answers, then adds the device to your peers.

To remove a controller, stop the user service on that machine and delete `~/.local/bin/local-studio-controller` and `~/.local-studio-controller`. Then unlink the peer with `POST /api/peers`.

## API

All routes except `/api/health` require the fleet key.

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
| `GET /api/cluster`, `POST /api/cluster`                                                           | Lending state, checks, machines and pools; set `discover` and `lend`  |
| `POST /api/cluster/pool`, `/api/cluster/pool/<id>/stop`                                           | Launch (or `dryRun`) and stop a pooled model                          |
| `POST /api/cluster/rpc`, `/api/cluster/rpc/stop`, `/api/cluster/release`                          | Controller to controller: lend an `rpc-server`, stop it, take it back |
| `POST /api/cluster/hello`                                                                         | Discovery handshake; needs no key, answers only with a keyed proof    |
| `POST /api/thunderbolt/authorize`                                                                 | Approve a pending Thunderbolt or USB4 device with `boltctl`           |

Registry launches use pinned container images and weight revisions on free matching NVIDIA GPUs, on ports 18100 to 18299. They never evict running engines.

The registry backs captured fleet configurations up next to the catalog. They are listed but stay read-only, as do host-specific, privileged and multi-machine launches; the API says why instead of quietly rewriting them. Missing weights or scripts are reported, never invented. Cancelling a request is forwarded to the engine, but an engine that ignores client disconnects (the tested SGLang build, for example) may keep generating. `auto` ranks successful requests these controllers have observed, not an engine's earlier traffic.

`GET /api/registry` reads `data/registry` from the same checkout. It matches each machine's GPUs, or an Apple Silicon chip and its unified memory, to registry hardware records within 1 GB, groups recipes by Hugging Face repository, and keeps one variant per hardware, engine, format and precision. `GET /api/registry/records/<id>` returns the full recipe, model instance, model and hardware records, and `POST /api/registry/download` runs `hf download` at the pinned revision on that machine.

`GET /api/registry/share?port=<port>` builds registry records from a running server's launch command or container, sends one short validation request, scrubs credentials, home paths, hostnames, private addresses and device IDs, and validates the records against the registry's JSON Schemas at the checked-out commit. Nothing is sent until `POST /api/registry/share`; the controller then forks `0xSero/local-ai-registry` and opens the pull request with your `gh` CLI login. `"dryRun": true` lists the `gh` calls without running them.

## Discovery

Every scan (about every 10 seconds) the controller lists the machine's TCP listeners and asks each one for `GET /v1/models`, with the port's `engineKeys` entry as a bearer token. New ports get 1.5 seconds to answer; ports that already answered get 10 seconds, so a busy engine that answers slowly stays loaded.

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

The controller reads the GPU list every 2 seconds, so a GPU that is plugged in or pulled shows within about 2 seconds. A removed GPU fails every job on it with "<GPU> was unplugged." and stops any `rpc-server` lent from it. `hardware.hotplug` keeps the last eight changes for an hour; an added GPU lists up to five recipes and models that fit its memory.

Apple Silicon Macs cannot use an external GPU, so `hardware.egpuNote` points the user to a GPU on another machine in the fleet.

## Clustering

Clustering is off until a machine opts in with a `cluster` block in `config.json`:

| Field         | Default        | Meaning                                                                                   |
| ------------- | -------------- | ----------------------------------------------------------------------------------------- |
| `discover`    | `false`        | Find other controllers on the tailnet and link those that share the fleet key             |
| `lend`        | `"never"`      | `never`, `idle` (lend only while the owner is away) or `always`                           |
| `ports`       | `[18091]`      | Controller ports to probe on each online tailnet device                                   |
| `idleSeconds` | `120`          | How long every check must pass before an `idle` machine lends                             |
| `drainSeconds`| `5`            | How long a reclaim waits for requests in flight before it stops lent work                 |
| `failoverMs`  | `60000`        | How long a streaming request waits for headers before it moves to the next machine        |
| `rpcPort`     | `50052`        | Tailnet port for the lent `rpc-server`                                                    |
| `rpcServer`, `llamaServer` | `rpc-server`, `llama-server` | llama.cpp binaries                                                 |

### Discovery

Every 15 seconds a discovering controller asks `tailscale status --json` for online devices and probes `GET /api/health` on each of `ports`. When a controller answers with `cluster: true`, the prober sends `POST /api/cluster/hello` with a random nonce and the address it called. The other side answers only if that address is its own, with an HMAC of the nonce, its id and that address under the fleet key. A controller with another key fails the proof and is never linked, and the address check stops one controller from relaying another's proof. Found controllers are added to `peers`. A controller you unlink with `POST /api/peers` is remembered in `cluster.ignored` and is not linked again by discovery until you add it back. When the controller listens on loopback, discovery also opens a listener on the machine's tailnet IPv4 address.

### Lending

Every 2 seconds a lending machine runs its checks:

| Check          | Passes when                                                                                  |
| -------------- | -------------------------------------------------------------------------------------------- |
| GPU programs   | No GPU compute process outside the fleet (the controller's engines, jobs and lent servers, found by process tree, container id or the `LOCAL_STUDIO_FLEET_OWNER` tag) |
| GPU use        | No GPU is 10% or more busy without a fleet process on it                                     |
| Power          | The machine is on AC power                                                                   |
| parish-media   | Where `~/.local/bin/parish-media` exists, its `status` is ready, or the only programs it lists are fleet processes and the machine is on AC with no media jobs |
| Owner          | On a Mac, nobody has touched the keyboard or mouse for a minute                              |

`idle` lends once every check has passed for `idleSeconds` and reclaims the moment one fails. `always` lends whenever the machine has a GPU. On a reclaim the controller marks itself draining, tells the machines it lends to, waits up to `drainSeconds` for requests in flight, then stops only what it started for the fleet: lent `rpc-server` processes and jobs launched with `lend: true`. A lent job is failed with "<machine> took its GPU back: <reason>". The `reason` field says in plain words why a machine is or is not lending.

### Routing

The gateway ranks every live copy of a model by load: lent machines that took their GPU back last, then requests queued or running (from the engine's metrics or the controller's own count), KV cache use, decode speed, local before remote, and fewer hops. A streaming request moves to the next copy on any error, on a 502, 503 or 504, or when no headers arrive within `failoverMs`. A non-streaming request moves only when no engine could have started it: a connection that was refused or never opened, a 503 from a local engine, or a peer answer marked `x-local-studio-retry: 1`. Any other error or timeout returns 502 or 504 at once, so a generation never runs twice. A response that moved carries `x-local-studio-failover`.

### Pooled models

`GET /api/cluster` lists the GGUF models in `modelsDir` and the Hugging Face cache, with `fitsOne` (fits the largest single machine's free GPU memory) and `fitsPool` (fits the pooled free memory of lending machines). `POST /api/cluster/pool` with a GGUF `model` asks every lending machine (or `members`) for an `rpc-server`, then runs `llama-server -m <model> --rpc <endpoints> -ngl 999 --metrics` on a port in 18100 to 18299. `"dryRun": true` returns the argv without starting anything. The pooled model is served through the gateway like any local engine.

Each lent `rpc-server` listens on loopback only. A gate on the tailnet IP at `rpcPort` forwards connections only from the address of the controller that asked for that server and drops every other address, because `rpc-server` has no authentication of its own. Programs run by other users on the lending machine can still reach the loopback port, so lend only from machines whose local accounts you trust. When a member reclaims, becomes unreachable or stops lending, the pool is torn down and shows why. When the controller gets SIGTERM or SIGINT it stops the `rpc-server` and pooled `llama-server` processes it started before it exits.

