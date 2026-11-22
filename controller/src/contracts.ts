import * as Schema from "effect/Schema";

export const LOCAL_STUDIO_CONTROLLER_HEADER = "x-local-studio-controller";

const OptionalString = Schema.NullOr(Schema.String);
const OptionalNumber = Schema.NullOr(Schema.Number);

export const LocalGpuTelemetry = Schema.Struct({
  index: Schema.Number,
  name: Schema.String,
  memoryUsedMiB: Schema.Number,
  memoryTotalMiB: Schema.Number,
  powerW: OptionalNumber,
  powerLimitW: OptionalNumber,
  temperatureC: OptionalNumber,
  utilization: OptionalNumber,
});
export type LocalGpuTelemetry = typeof LocalGpuTelemetry.Type;

export const LocalPcieLink = Schema.Struct({
  gen: OptionalNumber,
  width: OptionalNumber,
  maxGen: OptionalNumber,
  maxWidth: OptionalNumber,
  gbps: OptionalNumber,
  note: Schema.String,
});
export type LocalPcieLink = typeof LocalPcieLink.Type;

export const LocalGpu = Schema.Struct({
  ...LocalGpuTelemetry.fields,
  card: OptionalString,
  busy: Schema.Boolean,
  external: Schema.optionalKey(Schema.Boolean),
  busId: Schema.optionalKey(OptionalString),
  link: Schema.optionalKey(Schema.NullOr(LocalPcieLink)),
});
export type LocalGpu = typeof LocalGpu.Type;

export const LocalThunderbolt = Schema.Struct({
  uuid: Schema.String,
  name: Schema.String,
  vendor: OptionalString,
  generation: OptionalString,
  state: Schema.Literals(["pending", "authorizing", "authorized", "failed"]),
  message: OptionalString,
});
export type LocalThunderbolt = typeof LocalThunderbolt.Type;

export const LocalHotplug = Schema.Struct({
  at: Schema.String,
  change: Schema.Literals(["added", "removed"]),
  gpu: Schema.String,
  memoryTotalMiB: Schema.Number,
  external: Schema.Boolean,
  message: Schema.String,
  suggestions: Schema.Array(Schema.String),
});
export type LocalHotplug = typeof LocalHotplug.Type;

export const LocalHardware = Schema.Struct({
  cpus: Schema.Number,
  memTotalBytes: Schema.Number,
  memFreeBytes: Schema.Number,
  gpus: Schema.Array(LocalGpu),
  os: Schema.optionalKey(Schema.String),
  arch: Schema.optionalKey(Schema.String),
  egpuNote: Schema.optionalKey(OptionalString),
  thunderbolt: Schema.optionalKey(Schema.Array(LocalThunderbolt)),
  hotplug: Schema.optionalKey(Schema.Array(LocalHotplug)),
});
export type LocalHardware = typeof LocalHardware.Type;

export const LocalLoad = Schema.Struct({
  inFlight: Schema.Number,
  running: OptionalNumber,
  waiting: OptionalNumber,
  kvPercent: OptionalNumber,
  decodeTps: OptionalNumber,
});
export type LocalLoad = typeof LocalLoad.Type;

export const LocalEndpoint = Schema.Struct({
  id: Schema.String,
  controllerId: Schema.String,
  port: Schema.Number,
  pid: Schema.NullOr(Schema.Number),
  models: Schema.Array(Schema.String),
  live: Schema.Boolean,
  lastSeenAt: OptionalString,
  jobId: OptionalString,
  state: Schema.optionalKey(Schema.Literals(["loaded", "empty", "unreachable"])),
  checkedAt: Schema.optionalKey(OptionalString),
  failures: Schema.optionalKey(Schema.Number),
  error: Schema.optionalKey(OptionalString),
  latencyMs: Schema.optionalKey(OptionalNumber),
  lent: Schema.optionalKey(Schema.Boolean),
  load: Schema.optionalKey(Schema.NullOr(LocalLoad)),
});
export type LocalEndpoint = typeof LocalEndpoint.Type;

export const LocalUsage = Schema.Struct({
  model: Schema.String,
  requests: Schema.Number,
  lastAt: OptionalString,
  measuredRequests: Schema.optionalKey(Schema.Number),
  tokens: Schema.optionalKey(Schema.Record(Schema.String, Schema.Number)),
});
export type LocalUsage = typeof LocalUsage.Type;

export const LocalControllerLink = Schema.Struct({
  id: OptionalString,
  name: OptionalString,
  url: Schema.String,
  self: Schema.Boolean,
  reachable: Schema.Boolean,
  error: OptionalString,
  seenAt: Schema.optionalKey(OptionalString),
  usage: Schema.optionalKey(Schema.Array(LocalUsage)),
});
export type LocalControllerLink = typeof LocalControllerLink.Type;

export const LocalJob = Schema.Struct({
  id: Schema.String,
  recipeId: Schema.String,
  gpus: Schema.Array(Schema.Number),
  port: Schema.Number,
  container: Schema.String,
  phase: Schema.Literals([
    "pending",
    "pulling",
    "downloading",
    "starting",
    "ready",
    "failed",
    "stopped",
  ]),
  progress: Schema.NullOr(Schema.Number),
  message: OptionalString,
  startedAt: Schema.String,
  updatedAt: Schema.String,
  lent: Schema.optionalKey(Schema.Boolean),
  gpuUuids: Schema.optionalKey(Schema.Array(Schema.String)),
});
export type LocalJob = typeof LocalJob.Type;

const LocalControllerInfo = Schema.Struct({
  id: Schema.String,
  name: Schema.String,
  url: Schema.String,
  version: Schema.String,
});

export const LocalLendMode = Schema.Literals(["never", "idle", "always"]);
export type LocalLendMode = typeof LocalLendMode.Type;

export const LocalLendState = Schema.Literals(["off", "lent", "reclaimed"]);
export type LocalLendState = typeof LocalLendState.Type;

export const LocalPool = Schema.Struct({
  id: Schema.String,
  model: Schema.String,
  alias: Schema.String,
  port: Schema.Number,
  members: Schema.Array(Schema.Struct({ controllerId: Schema.String, name: Schema.String, endpoint: Schema.String })),
  phase: Schema.Literals(["starting", "ready", "failed", "stopped"]),
  message: OptionalString,
  argv: Schema.Array(Schema.String),
  startedAt: Schema.String,
});
export type LocalPool = typeof LocalPool.Type;

export const LocalNodeCluster = Schema.Struct({
  discover: Schema.Boolean,
  lend: LocalLendMode,
  state: LocalLendState,
  reason: Schema.String,
  since: Schema.String,
  rpc: Schema.Boolean,
  rpcEndpoint: OptionalString,
  gpus: Schema.Number,
  freeMiB: Schema.Number,
});
export type LocalNodeCluster = typeof LocalNodeCluster.Type;

export const LocalPowerLever = Schema.Literals(["none", "nvidia-limit", "duty-cycle"]);
export type LocalPowerLever = typeof LocalPowerLever.Type;

export const LocalPower = Schema.Struct({
  activeWatts: Schema.Number,
  awayWatts: OptionalNumber,
  present: Schema.NullOr(Schema.Boolean),
  presenceSource: Schema.String,
  capW: OptionalNumber,
  lever: LocalPowerLever,
  measured: Schema.Boolean,
  measuredW: OptionalNumber,
  helper: Schema.Literals(["installed", "missing", "unsupported"]),
  gpus: Schema.Array(Schema.Struct({ uuid: Schema.String, index: Schema.Number, lever: LocalPowerLever, limitW: OptionalNumber, originalW: OptionalNumber })),
});
export type LocalPower = typeof LocalPower.Type;

export const LocalCluster = Schema.Struct({
  ...LocalNodeCluster.fields,
  power: LocalPower,
  draining: Schema.Boolean,
  checks: Schema.Array(Schema.Struct({ name: Schema.String, ok: Schema.Boolean, detail: Schema.String })),
  discovered: Schema.Array(Schema.Struct({ id: Schema.String, name: Schema.String, url: Schema.String, at: Schema.String })),
  listen: Schema.Array(Schema.String),
  rpcServers: Schema.Array(Schema.Struct({ id: Schema.String, endpoint: Schema.String, forId: Schema.String, forName: Schema.String, startedAt: Schema.String, gpus: Schema.Array(Schema.Number) })),
  pools: Schema.Array(LocalPool),
  models: Schema.Array(Schema.Struct({ path: Schema.String, name: Schema.String, sizeBytes: Schema.Number, fitsOne: Schema.Boolean, fitsPool: Schema.Boolean })),
  machines: Schema.Array(Schema.Struct({ id: Schema.String, name: Schema.String, url: Schema.String, ...LocalNodeCluster.fields })),
});
export type LocalCluster = typeof LocalCluster.Type;

const Watts = Schema.Number.check(Schema.isInt(), Schema.isBetween({ minimum: 0, maximum: 2000 }));
const AwayWatts = Schema.Number.check(Schema.isInt(), Schema.isBetween({ minimum: 1, maximum: 2000 }));
export const LocalPowerRequest = Schema.Struct({
  activeWatts: Schema.optionalKey(Watts),
  awayWatts: Schema.optionalKey(Schema.NullOr(AwayWatts)),
});
export type LocalPowerRequest = typeof LocalPowerRequest.Type;

export const LocalClusterRequest = Schema.Struct({
  discover: Schema.optionalKey(Schema.Boolean),
  lend: Schema.optionalKey(LocalLendMode),
  power: Schema.optionalKey(LocalPowerRequest),
});
export const LocalHelloRequest = Schema.Struct({ id: Schema.String, nonce: Schema.String, target: Schema.String });
export const LocalRpcRequest = Schema.Struct({ id: Schema.String, name: Schema.String, url: Schema.String });
export const LocalReleaseRequest = Schema.Struct({ controllerId: Schema.String, reason: Schema.String });
export const LocalPoolRequest = Schema.Struct({
  model: Schema.String,
  alias: Schema.optionalKey(Schema.String),
  members: Schema.optionalKey(Schema.Array(Schema.String)),
  ctx: Schema.optionalKey(Schema.Number),
  dryRun: Schema.optionalKey(Schema.Boolean),
});
export type LocalPoolRequest = typeof LocalPoolRequest.Type;
export const LocalAuthorizeRequest = Schema.Struct({ uuid: Schema.String });

export const LocalSnapshot = Schema.Struct({
  controller: LocalControllerInfo,
  generatedAt: Schema.String,
  hardware: LocalHardware,
  auto: OptionalString,
  endpoints: Schema.Array(LocalEndpoint),
  models: Schema.Array(
    Schema.Struct({ id: Schema.String, live: Schema.Boolean, requests: Schema.Number }),
  ),
  controllers: Schema.Array(LocalControllerLink),
  jobs: Schema.Array(LocalJob),
  usage: Schema.Array(LocalUsage),
  cluster: Schema.optionalKey(LocalCluster),
});
export type LocalSnapshot = typeof LocalSnapshot.Type;

export const LocalRecipe = Schema.Struct({
  id: Schema.String,
  model: Schema.String,
  card: Schema.String,
  engine: Schema.String,
  cards: Schema.Number,
  ctx: Schema.Number,
  tps: Schema.NullOr(Schema.Number),
  fits: Schema.Boolean,
  blocked: OptionalString,
});
export type LocalRecipe = typeof LocalRecipe.Type;

export const LocalRecipes = Schema.Struct({ recipes: Schema.Array(LocalRecipe) });
export type LocalRecipes = typeof LocalRecipes.Type;

export const LocalRunRequest = Schema.Struct({
  gpus: Schema.optionalKey(Schema.Array(Schema.Number)),
  dryRun: Schema.optionalKey(Schema.Boolean),
  replace: Schema.optionalKey(Schema.Boolean),
  lend: Schema.optionalKey(Schema.Boolean),
});
export type LocalRunRequest = typeof LocalRunRequest.Type;

export const LocalPeerRequest = Schema.Struct({
  url: Schema.String,
  remove: Schema.optionalKey(Schema.Boolean),
});
export const LocalDeployRequest = Schema.Struct({
  ip: Schema.String,
  name: Schema.String,
  user: Schema.optionalKey(Schema.String),
});
export const LocalNameRequest = Schema.Struct({
  name: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(64)),
});

export const LocalTailnet = Schema.Struct({
  available: Schema.Boolean,
  devices: Schema.Array(
    Schema.Struct({
      name: Schema.String,
      ip: Schema.String,
      os: Schema.String,
      controller: Schema.NullOr(
        Schema.Struct({ id: Schema.String, name: Schema.String, url: Schema.String }),
      ),
      linked: Schema.Boolean,
    }),
  ),
});
export type LocalTailnet = typeof LocalTailnet.Type;

export const LocalNode = Schema.Struct({
  controller: LocalControllerInfo,
  endpoints: Schema.Array(LocalEndpoint),
  usage: Schema.Array(LocalUsage),
  peers: Schema.Array(Schema.String),
  cluster: Schema.optionalKey(LocalNodeCluster),
});
export type LocalNode = typeof LocalNode.Type;

export const LocalRegistryVariant = Schema.Struct({
  id: Schema.String,
  status: Schema.String,
  engine: Schema.String,
  hardwareId: Schema.String,
  hardwareCount: Schema.Number,
  format: OptionalString,
  precision: OptionalString,
  sizeGb: Schema.NullOr(Schema.Number),
  launchKind: Schema.String,
  evidence: Schema.Boolean,
  recommended: Schema.Boolean,
  fits: Schema.Boolean,
});
export type LocalRegistryVariant = typeof LocalRegistryVariant.Type;

export const LocalRegistryMatch = Schema.Struct({
  name: Schema.String,
  memoryGb: Schema.NullOr(Schema.Number),
  count: Schema.Number,
  hardwareId: OptionalString,
  hardwareName: OptionalString,
});
export type LocalRegistryMatch = typeof LocalRegistryMatch.Type;

export const LocalRegistry = Schema.Struct({
  commit: OptionalString,
  error: OptionalString,
  matches: Schema.Array(LocalRegistryMatch),
  total: Schema.Number,
  models: Schema.Array(
    Schema.Struct({ repo: Schema.String, variants: Schema.Array(LocalRegistryVariant) }),
  ),
  downloads: Schema.Record(Schema.String, Schema.String),
});
export type LocalRegistry = typeof LocalRegistry.Type;

export const LocalRegistryRecord = Schema.Struct({
  recipe: Schema.Unknown,
  modelInstance: Schema.Unknown,
  model: Schema.Unknown,
  hardware: Schema.Unknown,
  command: OptionalString,
  weights: Schema.NullOr(
    Schema.Struct({ repo: Schema.String, revision: OptionalString, include: OptionalString }),
  ),
});
export type LocalRegistryRecord = typeof LocalRegistryRecord.Type;

export const LocalDownloadRequest = Schema.Struct({ recipeId: Schema.String });
export const LocalShareRequest = Schema.Struct({
  port: Schema.Number,
  confirm: Schema.optionalKey(Schema.Boolean),
  dryRun: Schema.optionalKey(Schema.Boolean),
});
export type LocalShareRequest = typeof LocalShareRequest.Type;

export const LocalSharePreview = Schema.Struct({
  target: Schema.String,
  branch: Schema.String,
  title: Schema.String,
  body: Schema.String,
  files: Schema.Array(Schema.Struct({ path: Schema.String, record: Schema.Unknown })),
  reused: Schema.Array(Schema.String),
  redactions: Schema.Array(Schema.String),
  issues: Schema.Array(Schema.String),
  blockers: Schema.Array(Schema.String),
});
export type LocalSharePreview = typeof LocalSharePreview.Type;

export const LocalShareResult = Schema.Struct({
  url: OptionalString,
  commands: Schema.Array(Schema.String),
});
export type LocalShareResult = typeof LocalShareResult.Type;

export const LocalTelemetry = Schema.Struct({
  generatedAt: Schema.String,
  engines: Schema.Array(
    Schema.Struct({
      port: Schema.Number,
      models: Schema.Array(Schema.String),
      kind: OptionalString,
      loadedAt: OptionalString,
    }),
  ),
  live: Schema.Struct({
    decodeTps: OptionalNumber,
    running: OptionalNumber,
    waiting: OptionalNumber,
    kvPercent: OptionalNumber,
    ttftMs: OptionalNumber,
  }),
  peaks: Schema.Struct({ running: OptionalNumber, kvPercent: OptionalNumber, ttftBestMs: OptionalNumber }),
  gpus: Schema.Array(LocalGpuTelemetry),
  memory: Schema.Struct({ totalBytes: Schema.Number, freeBytes: Schema.Number }),
  history: Schema.optionalKey(
    Schema.Struct({
      decode: Schema.Struct({ start: Schema.String, stepSeconds: Schema.Number, values: Schema.Array(Schema.Number) }),
      days: Schema.Array(Schema.Struct({ day: Schema.String, prompt: Schema.Number, generation: Schema.Number })),
    }),
  ),
});
export type LocalTelemetry = typeof LocalTelemetry.Type;
