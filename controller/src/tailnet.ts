import { randomBytes } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import type { LocalTailnet } from "./contracts.ts";
import { type Config, decodeJson, exec, fail, fetchJson, httpError, lastLine, LEGACY_DIR } from "./core.ts";
import { normUrl } from "./graph.ts";

const Peer = Schema.Struct({ HostName: Schema.String, OS: Schema.String, Online: Schema.Boolean, TailscaleIPs: Schema.Array(Schema.String) });
const Status = Schema.Struct({ Peer: Schema.optionalKey(Schema.NullOr(Schema.Record(Schema.String, Peer))) });
const decodeHealth = Schema.decodeUnknownOption(Schema.Struct({ id: Schema.String, name: Schema.String }));

const BINS = ["tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"];

export const onlinePeers: Effect.Effect<(typeof Peer.Type)[] | null> = Effect.gen(function* () {
  for (const bin of BINS) {
    const r = yield* exec([bin, "status", "--json"], 5_000);
    const status = r.code === 0 ? decodeJson(Status, r.stdout) : undefined;
    if (status) return Object.values(status.Peer ?? {}).filter((p) => p.Online && p.TailscaleIPs[0]);
  }
  return null;
});

export const tailnetIPv4: Effect.Effect<string | null> = Effect.gen(function* () {
  for (const bin of BINS) {
    const r = yield* exec([bin, "ip", "-4"], 5_000);
    const ip = r.code === 0 ? r.stdout.trim().split("\n")[0]?.trim() : "";
    if (ip && /^\d+\.\d+\.\d+\.\d+$/.test(ip)) return ip;
  }
  return null;
});

export const scanTailnet = (linked: Set<string>): Effect.Effect<LocalTailnet> =>
  Effect.gen(function* () {
    const peers = yield* onlinePeers;
    if (!peers) return { available: false, devices: [] };
    const devices = yield* Effect.forEach(
      peers,
      (p) =>
        Effect.gen(function* () {
          const ip = p.TailscaleIPs.find((a) => !a.includes(":")) ?? p.TailscaleIPs[0] ?? "";
          const url = `http://${ip}:18091`;
          const res = yield* Effect.option(fetchJson(`${url}/api/health`, 1_500));
          const health = res._tag === "Some" && res.value.status === 200 ? decodeHealth(res.value.body) : null;
          return { name: p.HostName, ip, os: p.OS, controller: health?._tag === "Some" ? { ...health.value, url } : null, linked: linked.has(normUrl(url)) };
        }),
      { concurrency: 16 },
    );
    return { available: true, devices: devices.sort((a, b) => a.name.localeCompare(b.name)) };
  });

const quote = (v: string) => `'${v.replaceAll("'", `'\\''`)}'`;
const PROBE = `D="$HOME/.local-studio-controller"; L="$HOME/${LEGACY_DIR}"
if [ -e "$D/config.json" ] || [ -e "$L/config.json" ] || curl -s -m 2 -o /dev/null "http://$HOST:18091/api/health"; then echo "a controller is already set up on this machine; connect it instead" >&2; exit 3; fi
echo "$(uname -s)-$(uname -m)" | tr A-Z a-z | sed 's/x86_64/x64/;s/aarch64/arm64/'
for b in "$(command -v bun)" "$HOME/.bun/bin/bun" /opt/homebrew/bin/bun /usr/local/bin/bun; do if [ -x "$b" ]; then echo "$b"; break; fi; done`;
const UPLOAD = 'umask 077; D="$HOME/.local-studio-controller"; mkdir -p "$D" && cat > "$D/upload"';
const SCRIPT = `set -e
D="$HOME/.local-studio-controller"; B="$HOME/.local/bin/local-studio-controller"
mkdir -p "$HOME/.local/bin" && chmod 700 "$D" "$D/upload" && mv "$D/upload" "$B"
umask 077 && printf '%s\\n' "$CFG" > "$D/config.json"
if [ "$(uname -s)" = Linux ]; then
  mkdir -p "$HOME/.config/systemd/user"
  printf '[Unit]\\nDescription=Local Studio controller\\n[Service]\\nEnvironment=LOCAL_STUDIO_CONTROLLER_HOST=%s\\nExecStart=%s\\nRestart=always\\n[Install]\\nWantedBy=default.target\\n' "$HOST" "$B" > "$HOME/.config/systemd/user/local-studio-controller.service"
  loginctl enable-linger "$(id -un)" 2>/dev/null || true
  export XDG_RUNTIME_DIR="\${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  if systemctl --user daemon-reload 2>/dev/null; then systemctl --user enable local-studio-controller && systemctl --user restart local-studio-controller
  else pkill -f "$B" 2>/dev/null || true; LOCAL_STUDIO_CONTROLLER_HOST="$HOST" setsid nohup "$B" > "$D/controller.log" 2>&1 < /dev/null & fi
else
  P="$HOME/Library/LaunchAgents/ai.localstudio.controller.plist" && mkdir -p "$(dirname "$P")"
  printf '<?xml version="1.0" encoding="UTF-8"?>\\n<plist version="1.0"><dict><key>Label</key><string>ai.localstudio.controller</string><key>ProgramArguments</key><array><string>%s</string></array><key>EnvironmentVariables</key><dict><key>LOCAL_STUDIO_CONTROLLER_HOST</key><string>%s</string></dict><key>RunAtLoad</key><true/><key>KeepAlive</key><true/></dict></plist>\\n' "$B" "$HOST" > "$P"
  launchctl bootout "gui/$(id -u)/ai.localstudio.controller" 2>/dev/null || true
  launchctl bootstrap "gui/$(id -u)" "$P" || launchctl bootstrap "user/$(id -u)" "$P"
fi`;

const program = (platform: string, bun: string | undefined, target: string) =>
  Effect.gen(function* () {
    const same = platform === `${process.platform}-${process.arch}`;
    if (Bun.main.startsWith("/$bunfs/")) {
      if (!same) return yield* fail(502, "DEPLOY_FAILED", `${target} is ${platform} and this controller was built for ${process.platform}-${process.arch}; deploy from a machine of the same kind`);
      return yield* Effect.promise(() => Bun.file(process.execPath).bytes());
    }
    if (bun) {
      const built = yield* Effect.tryPromise({ try: () => Bun.build({ entrypoints: [Bun.main], target: "bun" }), catch: (e) => httpError(500, "BUILD_FAILED", String(e)) });
      return new TextEncoder().encode(`#!${bun}\n${yield* Effect.promise(() => built.outputs[0]!.text())}`);
    }
    if (!same) return yield* fail(502, "DEPLOY_FAILED", `${target} is ${platform}: install Bun there first (bun.sh)`);
    const dir = mkdtempSync(join(tmpdir(), "local-studio-deploy-"));
    const out = join(dir, "controller");
    const r = yield* exec([process.execPath, "build", "--compile", Bun.main, "--outfile", out], 120_000);
    const bytes = r.code === 0 ? yield* Effect.promise(() => Bun.file(out).bytes()) : null;
    rmSync(dir, { recursive: true, force: true });
    return bytes ?? (yield* fail(500, "BUILD_FAILED", lastLine(r)));
  });

export const deployController = (config: Config, req: { ip: string; name: string; user?: string }) =>
  Effect.gen(function* () {
    if (!/^[\d.]+$/.test(req.ip) || !/^[\w.-]*$/.test(req.user ?? "")) return yield* fail(400, "BAD_TARGET", "deploy needs a tailnet IPv4 and a plain user name");
    const url = `http://${req.ip}:18091`;
    const name = req.name.replace(/[^\w.-]/g, "-") || req.ip;
    const cfg = JSON.stringify({ id: `${name}-${randomBytes(3).toString("hex")}`, name, url, fleetKey: config.fleetKey, peers: [] });
    const target = req.user ? `${req.user}@${req.ip}` : req.ip;
    const ssh = (command: string, stdin: string | Uint8Array) =>
      Effect.flatMap(exec(["ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new", target, command], 180_000, { stdin }), (r) =>
        r.code === 0 ? Effect.succeed(r.stdout) : fail(502, "DEPLOY_FAILED", `${target}: ${lastLine(r)}`),
      );
    const vars = (v: Record<string, string>) => Object.entries(v).map(([k, value]) => `${k}=${quote(value)}`).join("\n");
    const [platform, bun] = (yield* ssh("sh -s", `${vars({ HOST: req.ip })}\n${PROBE}\n`)).trim().split("\n");
    yield* ssh(UPLOAD, yield* program(platform ?? "", bun, target));
    yield* ssh("sh -s", `${vars({ CFG: cfg, HOST: req.ip })}\n${SCRIPT}\n`);
    for (let i = 0; i < 20; i++) {
      const h = yield* Effect.option(fetchJson(`${url}/api/health`, 1_500));
      if (h._tag === "Some" && h.value.status === 200) return url;
      yield* Effect.sleep("1 second");
    }
    return yield* fail(504, "DEPLOY_TIMEOUT", `installed on ${target} but nothing answers at ${url}`);
  });
