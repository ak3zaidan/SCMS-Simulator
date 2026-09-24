/**
 * The JSON-RPC 2.0 control surface — docs/protocol/vwp-v1.md §6.
 *
 * Text frames on the same WebSocket carry JSON-RPC 2.0, single objects only (§6.1: batch arrays are
 * rejected with −32600). This file hand-transcribes the 31 methods plus `rpc.discover` of §6.15 and
 * the 8 server→client notifications of §6.14 into a typed method map, so a caller gets its params
 * and result checked at compile time. Nothing here is invented: every method, param and result
 * property comes from a schema in §6.
 *
 * Note on `SimTimeNs`: §6.5 types it as an integer up to `u64::MAX`, but it travels as a JSON
 * number, so values above 2^53 − 1 (≈ 104 days of simulated time in nanoseconds) cannot be
 * represented exactly. These types use `number`, which is what `JSON.parse` produces.
 */

// ---------------------------------------------------------------------------
// §6.5 — shared schema definitions
// ---------------------------------------------------------------------------

/** §6.5 — nanoseconds since `t0`. */
export type SimTimeNs = number;
/** §6.5 — 36-character UUID string. */
export type RunId = string;
/** §6.5 */
export type NodeId = number;
/** §6.5 */
export type ActorId = number;
/** §6.5 */
export type LaneId = number;
/** §6.5 — 64 lower-case hex characters. */
export type Sha256Hex = string;
/** §6.5 */
export type Visibility = "GT" | "NODE" | "PUBLIC" | "MIXED" | "DERIVED" | "META";
/** §6.5 */
export type RunState = "idle" | "loading" | "running" | "paused" | "seeking" | "finished" | "error";
/** §6.5 */
export type ChannelName = string;
/** §6.5 */
export type CameraMode = "map" | "chase" | "dashboard" | "free" | "rsu" | "jump";
/** §6.5 */
export interface Vec3 {
  x: number;
  y: number;
  z: number;
}
/** §6.5 — what a value on screen refers to, for `explain`. */
export interface ValueRef {
  kind: "metric" | "node_field" | "actor_field" | "event" | "link" | "entity" | "channel" | "world" | "overlay";
  id?: string;
  node?: NodeId;
  actor?: ActorId;
  t_ns?: SimTimeNs;
  prov_id?: number;
}
/** §6.5 — a resolved provenance record. */
export interface ProvenanceInfo {
  prov_id: number;
  model_id: string;
  model_version: string;
  param_set_id: string;
  family?: string;
  card_url?: string;
  parameters?: Record<string, unknown>;
  sources?: Record<string, unknown>[];
  equations?: Record<string, unknown>[];
  assumptions?: string[];
  validation?: Record<string, unknown>;
}
/** §6.5 */
export interface ValidationError {
  path: string;
  message: string;
  hint?: string;
  severity?: "error" | "warning";
}
/** §6.5 */
export interface WorldImportResult {
  world_hash: Sha256Hex;
  url?: string;
  bbox_m: { min_x?: number; min_y?: number; max_x?: number; max_y?: number };
  origin?: { lat_deg?: number; lon_deg?: number; alt_m?: number };
  lanes: number;
  buildings: number;
  junctions: number;
  signals?: number;
  bytes: number;
  cached: boolean;
  licence?: string;
  warnings?: ValidationError[];
  provenance?: Record<string, unknown>;
}
/** §6.5 — any operation that can exceed 2 s returns this instead of a final result (§6.11). */
export interface Job {
  job_id: string;
  state: "queued" | "running" | "done" | "failed" | "cancelled";
  progress?: number;
  message?: string;
  outputs?: string[];
}

// ---------------------------------------------------------------------------
// §6.4 — error codes
// ---------------------------------------------------------------------------

/** §6.4 — the complete error-code table. */
export const RpcErrorCode = {
  PARSE_ERROR: -32700,
  INVALID_REQUEST: -32600,
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  INTERNAL_ERROR: -32603,
  RUN_NOT_FOUND: -32000,
  RUN_ALREADY_RUNNING: -32001,
  RUN_NOT_RUNNING: -32002,
  SEEK_OUT_OF_RANGE: -32003,
  SCENARIO_INVALID: -32004,
  WORLD_NOT_FOUND: -32005,
  UNKNOWN_ID: -32006,
  UNKNOWN_METRIC: -32007,
  EXPORT_FAILED: -32008,
  NOT_SUPPORTED_HERE: -32009,
  BUSY: -32010,
  EXPERIMENT_NOT_FOUND: -32011,
  PLUGIN_DRIFT: -32012,
  IO_ERROR: -32013,
  VISIBILITY_DENIED: -32040,
  UNAUTHORIZED: -32041,
  RATE_LIMITED: -32042,
  UNSUPPORTED_VERSION: -32050,
} as const;
export type RpcErrorCode = (typeof RpcErrorCode)[keyof typeof RpcErrorCode];

// ---------------------------------------------------------------------------
// §6.6 — run control
// ---------------------------------------------------------------------------

/** §6.6 `run.start` params. */
export interface RunStartParams {
  scenario?: string | Record<string, unknown>;
  seed?: number;
  speed?: number;
  paused?: boolean;
  record?: boolean;
  record_path?: string;
  label?: string;
}
/** §6.6 `run.start` result. */
export interface RunStartResult {
  run_id: RunId;
  state: RunState;
  world_hash: Sha256Hex;
  scenario_hash: Sha256Hex;
  recording_path?: string;
  t_end_ns?: SimTimeNs;
  /** How many runs this engine process has started; every `run.start` moves it. */
  generation?: number;
}
/** §6.6 `run.pause` / `run.resume` params (empty object). */
export type RunPauseParams = Record<string, never>;
/** §6.6 `run.pause` / `run.resume` result. */
export interface RunPauseResult {
  state: RunState;
  t_ns: SimTimeNs;
}
/** §6.6 `run.step` params. */
export interface RunStepParams {
  unit?: "step" | "event" | "keyframe" | "second";
  count?: number;
}
/** §6.6 `run.step` result. */
export interface RunStepResult {
  state: RunState;
  t_ns: SimTimeNs;
  stepped: number;
  last_event?: { channel?: ChannelName; priority?: number; seq?: number };
}
/** §6.6 `run.seek` params — exactly one of `t_ns`, `fraction`, `event`. */
export interface RunSeekParams {
  t_ns?: SimTimeNs;
  fraction?: number;
  event?: { channel: ChannelName; direction?: "next" | "prev"; node?: NodeId };
  pause_after?: boolean;
}
/** §6.6 `run.seek` result. The keyframe and deltas arrive **before** this reply (§6.6, R4). */
export interface RunSeekResult {
  t_ns: SimTimeNs;
  keyframe_seq: number;
  deltas_applied: number;
  elapsed_ms: number;
  state?: RunState;
}
/** §6.6 `run.speed` params. */
export interface RunSpeedParams {
  speed: number;
  sync?: "free" | "client";
}
/** §6.6 `run.speed` result. */
export interface RunSpeedResult {
  speed: number;
  sync: "free" | "client";
}
/** §6.6 `run.stop` params. */
export interface RunStopParams {
  finalize_exports?: boolean;
}
/** §6.6 `run.stop` result. */
export interface RunStopResult {
  state: RunState;
  t_ns: SimTimeNs;
  recording_path?: string;
  digest?: Sha256Hex;
  files?: { path: string; sha256: Sha256Hex; bytes: number }[];
}
/** §6.6 `run.status` params. */
export interface RunStatusParams {
  run_id?: RunId;
}
/** §6.6 `run.status` result. */
export interface RunStatusResult {
  run_id: RunId;
  state: RunState;
  t_ns: SimTimeNs;
  t_end_ns: SimTimeNs;
  speed: number;
  sync?: "free" | "client";
  profile: "full" | "node";
  live: boolean;
  wall_elapsed_s?: number;
  realtime_factor?: number;
  actors?: number;
  nodes?: number;
  events_per_s?: number;
  seq?: number;
  dropped?: { delta?: number; event?: number; telemetry?: number; metric?: number };
  manifest?: Record<string, unknown>;
  warnings?: ValidationError[];
  /** How many runs this engine process has started; every `run.start` moves it. */
  generation?: number;
  /** The running scenario's digest. */
  scenario_hash?: Sha256Hex;
  /** The digest of the scenario held for the next run, when `scenario.set` staged one. */
  staged_hash?: Sha256Hex | null;
  /** Engine facts beyond the schema: kernel threads, the run's output digest, retention. */
  engine?: {
    kernel_threads?: number;
    kernel_threads_started?: number;
    runs_started?: number;
    output_digest?: string | null;
    digest_steps?: number;
    failure?: string | null;
    /** The scenario timeline's items fired by the stream position (03-interfaces §13). */
    timeline?: ScenarioEventRecord[];
    [key: string]: unknown;
  };
}

/**
 * One scenario timeline item as the engine fired it: the `scenario.event` record
 * (03-interfaces §14), as `run.status` reports it in `engine.timeline`.
 */
export interface ScenarioEventRecord {
  /** When it took effect, simulated ns. */
  t: number;
  /** Its position in the scenario's `events`. */
  index: number;
  /** Its `type`, e.g. `closure`. */
  kind: string;
  /** `start`, or `end` when its `until` arrived. */
  phase: "start" | "end";
  /** What it did, as a sentence. */
  effect: string;
  lanes?: number[];
  multiplier?: number;
  path?: string;
  /** The value a `param.change` set, as JSON text. */
  value?: string;
  populations?: number[];
}

// ---------------------------------------------------------------------------
// §6.7 — view control (connection-scoped)
// ---------------------------------------------------------------------------

/** §6.7 `view.follow` params — also the subscription control for `Telemetry`. */
export interface ViewFollowParams {
  node?: NodeId;
  actor?: ActorId;
  camera?: CameraMode;
  clear?: boolean;
  telemetry?: boolean;
  radius_m?: number;
}
/** §6.7 `view.follow` result. */
export interface ViewFollowResult {
  following: number | null;
  camera?: CameraMode;
  subscribed_nodes: NodeId[];
}
/** §6.7 `view.camera` params. */
export interface ViewCameraParams {
  mode: CameraMode;
  target?: Vec3;
  position?: Vec3;
  fov_deg?: number;
  projection?: "perspective" | "orthographic";
  extent_m?: number;
  node?: NodeId;
  animate_ms?: number;
}
/** §6.7 `view.camera` result. */
export interface ViewCameraResult {
  mode: CameraMode;
  position: Vec3;
  target: Vec3;
  fov_deg: number;
  projection: "perspective" | "orthographic";
}
/** §6.7 — the overlay catalogue names. Those ending `_gt` have `visibility: "GT"`. */
export const OVERLAY_NAMES = [
  "tx_pulses", "links", "cbr_heatmap", "coverage", "attackers_gt", "revoked", "reported", "detections",
  "backend_flows", "focus_region", "lane_markings", "buildings", "labels", "trajectories_gt",
  "belief_vs_truth_gt", "signal_state", "rsu_range", "density",
] as const;
export type OverlayName = (typeof OVERLAY_NAMES)[number];
/** §6.7 `overlay.set` params. */
export interface OverlaySetParams {
  overlays?: Partial<Record<OverlayName, boolean>>;
  opacity?: Partial<Record<OverlayName, number>>;
  list?: boolean;
}
/** §6.7 `overlay.set` result. */
export interface OverlaySetResult {
  overlays: Partial<Record<OverlayName, boolean>>;
  catalogue?: {
    name: string;
    visibility: Visibility;
    available: boolean;
    description?: string;
    needs_channels?: ChannelName[];
  }[];
}

// ---------------------------------------------------------------------------
// §6.8 — inspection, §6.9 — explain
// ---------------------------------------------------------------------------

/** §6.8 `inspect.node` params. */
export interface InspectNodeParams {
  node: NodeId;
  t_ns?: SimTimeNs;
  include?: ("telemetry" | "stores" | "queues" | "neighbors" | "certs" | "crl" | "gnss" | "clock" | "apps" | "detectors" | "provenance" | "messages")[];
  limit?: number;
}
/**
 * §6.8 — one neighbour-table row. The spec's row names a certificate `digest` and a `verify_state`;
 * the live engine answers from its link history instead, with the sending `node`, whether its last
 * frame was `heard` or `lost`, and the last RSSI. Both shapes are typed, so a client shows either.
 */
export interface InspectNeighbor {
  digest?: string;
  verify_state?: "unverified" | "verified" | "failed" | "revoked";
  node?: NodeId;
  state?: "heard" | "lost";
  rssi_dbm?: number | null;
  last_seen_ns: SimTimeNs;
  distance_m?: number;
  relevance?: number;
  messages?: number;
}
/** What one broadcast message said: its wire fields decoded, and the kinematic claim receivers are handed. */
export interface InspectMessageContent {
  msg_count?: number;
  temp_id?: string;
  sec_mark_ms?: number;
  lat_deg?: number;
  lon_deg?: number;
  elev_m?: number;
  speed_mps?: number;
  heading_deg?: number;
  part_ii?: number;
  claimed_x_m?: number;
  claimed_y_m?: number;
  claimed_speed_mps?: number;
  claimed_heading_rad?: number;
}
/** One frame the node put on the air (`inspect.node` `messages.sent`). */
export interface InspectSentMessage {
  t_ns: SimTimeNs;
  msg?: number | null;
  msg_type?: string | null;
  bytes_on_wire: number;
  payload_bytes?: number | null;
  envelope_bytes?: number | null;
  cert_bytes?: number | null;
  net_header_bytes?: number | null;
  link_bytes?: number | null;
  airtime_us?: number | null;
  power_dbm?: number | null;
  signer?: "certificate" | "digest" | "self-signed" | null;
  t_generated_ns?: SimTimeNs | null;
  t_signed_ns?: SimTimeNs | null;
  channel?: number | null;
  pseudonym?: string | null;
  content?: InspectMessageContent | null;
}
/** One reception at the node, followed to its fate (`inspect.node` `messages.received`). */
export interface InspectReceivedMessage {
  t_ns: SimTimeNs;
  msg?: number | null;
  from?: NodeId | null;
  msg_type?: string | null;
  outcome?: "delivered" | "lost" | "in-flight";
  cause?: string | null;
  verification?: string | null;
  rssi_dbm?: number | null;
  sinr_db?: number | null;
  dist_m?: number | null;
  bytes_on_wire?: number | null;
  e2e_ms?: number | null;
  stages_ms?: Record<string, number>;
}
/** `inspect.node`'s `messages` section: the followed node's recent traffic, oldest first. */
export interface InspectMessages {
  sent: InspectSentMessage[];
  received: InspectReceivedMessage[];
}
/** §6.8 `inspect.node` result. `additionalProperties: true`, so unknown keys are kept (R9). */
export interface InspectNodeResult {
  node: NodeId;
  t_ns: SimTimeNs;
  kind: "obu" | "vru-device" | "rsu" | "base-station" | "router" | "backend-entity" | "other";
  label?: string;
  profile_id: string;
  actor?: ActorId;
  telemetry?: Record<string, unknown>;
  queues?: Record<string, { depth?: number; p50?: number; p95?: number; policy?: string; drops?: Record<string, unknown> }>;
  stores?: Record<string, unknown>;
  neighbors?: InspectNeighbor[];
  messages?: InspectMessages;
  certs?: Record<string, unknown>[];
  crl?: Record<string, unknown>;
  gnss?: Record<string, unknown>;
  clock?: Record<string, unknown>;
  apps?: Record<string, unknown>[];
  detectors?: Record<string, unknown>[];
  provenance?: ProvenanceInfo[];
  [key: string]: unknown;
}
/** §6.8 `inspect.link` params — either `tx`+`rx` or `link`. */
export interface InspectLinkParams {
  tx?: NodeId;
  rx?: NodeId;
  link?: string;
  t_ns?: SimTimeNs;
  window_ns?: SimTimeNs;
}
/** §6.8 `inspect.link` result. */
export interface InspectLinkResult {
  kind: "radio" | "backhaul" | "uu" | "backend-net";
  t_ns: SimTimeNs;
  distance_m?: number;
  los?: { class?: "LOS" | "NLOSb" | "NLOSv" | "NLOSt" | "NLOSbv"; walls_crossed?: number; obstructed_len_m?: number };
  path_loss_db?: number;
  shadowing_db?: number;
  fading_db?: number;
  rx_power_dbm?: number;
  sinr_db?: number;
  pdr?: number;
  pir_p95_s?: number;
  frames?: number;
  bytes?: number;
  latency_ms?: { p50?: number; p95?: number };
  bandwidth_mbps?: number;
  provenance?: ProvenanceInfo[];
  [key: string]: unknown;
}
/** §6.8 `inspect.entity` params. */
export interface InspectEntityParams {
  entity: string;
  t_ns?: SimTimeNs;
  limit?: number;
}
/** §6.8 `inspect.entity` result. */
export interface InspectEntityResult {
  entity: string;
  t_ns: SimTimeNs;
  role: string;
  node?: NodeId;
  state: Record<string, unknown>;
  queue?: Record<string, unknown>;
  storage_bytes?: number;
  open_cases?: number;
  decisions?: number;
  flows?: Record<string, unknown>[];
  provenance?: ProvenanceInfo[];
  [key: string]: unknown;
}
/** §6.9 `explain` params. */
export interface ExplainParams {
  subject: ValueRef;
  depth?: number;
  format?: "json" | "markdown";
}
/** §6.9 `explain` result. */
export interface ExplainResult {
  subject: ValueRef;
  value?: unknown;
  unit?: string;
  chain: ProvenanceInfo[];
  definition_md?: string;
  markdown?: string;
  caveats?: string[];
}

// ---------------------------------------------------------------------------
// §6.10 — scenario
// ---------------------------------------------------------------------------

/** §6.10 `scenario.get` params. */
export interface ScenarioGetParams {
  path?: string;
  resolved?: boolean;
  with_schema?: boolean;
}
/** §6.10 `scenario.get` result. */
export interface ScenarioGetResult {
  scenario: unknown;
  hash: Sha256Hex;
  schema?: Record<string, unknown>;
  /** The running scenario's digest, when `scenario` is a staged one that differs from it. */
  running_hash?: Sha256Hex;
  /** What `scenario.set` is holding for the next run, or `null`. */
  staged?: { hash: Sha256Hex; changed: string[]; valid: boolean; errors?: ValidationError[] } | null;
  /** The published settings surface: one row per editable leaf, with its implementation status. */
  fields?: Record<string, unknown>[];
  /** The implementation-status vocabulary the `fields` rows use. */
  statuses?: { id: string; label: string; note: string }[];
  groups?: { name: string; description: string; sections: string[] }[];
}
/** RFC 6902 patch operation, as `scenario.set` accepts it. */
export interface JsonPatchOp {
  op: "add" | "remove" | "replace" | "move" | "copy" | "test";
  path: string;
  from?: string;
  value?: unknown;
}
/** §6.10 `scenario.set` params — either `scenario` or `patch`. */
export interface ScenarioSetParams {
  scenario?: Record<string, unknown>;
  patch?: JsonPatchOp[];
  validate?: boolean;
  apply_live?: boolean;
}
/** §6.10 `scenario.set` result. */
export interface ScenarioSetResult {
  hash: Sha256Hex;
  valid: boolean;
  errors?: ValidationError[];
  applied_live?: string[];
  requires_restart?: string[];
}
/** §6.10 `scenario.validate` params. */
export interface ScenarioValidateParams {
  scenario?: Record<string, unknown>;
  strict?: boolean;
}
/** §6.10 `scenario.validate` result. */
export interface ScenarioValidateResult {
  valid: boolean;
  errors: ValidationError[];
  warnings: ValidationError[];
  resolved_tiers?: Record<string, "abstract" | "medium" | "high">;
  estimated_cost?: { actors?: number; nodes?: number; realtime_factor?: number; recording_mb_per_sim_min?: number };
}
/** §6.10 `scenario.save` params. */
export interface ScenarioSaveParams {
  path: string;
  scenario?: Record<string, unknown>;
  overwrite?: boolean;
  format?: "yaml" | "json";
}
/** §6.10 `scenario.save` result. */
export interface ScenarioSaveResult {
  path: string;
  hash: Sha256Hex;
  bytes: number;
}
/** §6.10 `scenario.load` params. */
export interface ScenarioLoadParams {
  path: string;
  validate?: boolean;
}
/** §6.10 `scenario.load` result. */
export interface ScenarioLoadResult {
  hash: Sha256Hex;
  valid: boolean;
  scenario: Record<string, unknown>;
  errors?: ValidationError[];
}
/** §6.10 `scenario.list` params. */
export interface ScenarioListParams {
  kind?: "presets" | "saved" | "runs" | "all";
  prefix?: string;
  limit?: number;
}
/** §6.10 `scenario.list` result. */
export interface ScenarioListResult {
  items: {
    id: string;
    kind: "preset" | "saved" | "run";
    name?: string;
    description?: string;
    tags?: string[];
    path?: string;
    hash?: Sha256Hex;
    modified?: string;
  }[];
}

// ---------------------------------------------------------------------------
// §6.11 — world
// ---------------------------------------------------------------------------

/** §6.11 `world.import_osm` params — either `bbox` or `file`. */
export interface WorldImportOsmParams {
  bbox?: [number, number, number, number];
  file?: string;
  buildings?: boolean;
  terrain?: boolean | string;
  simplify_tolerance_m?: number;
  default_levels_height_m?: number;
  lane_inference?: "osm2streets" | "sumo-netconvert";
  cache?: boolean;
}
/** §6.11 `world.generate` params. */
export interface WorldGenerateParams {
  kind: "grid" | "manhattan" | "highway" | "ring" | "intersection" | "custom";
  size_m?: [number, number];
  block_m?: number;
  lanes_per_direction?: number;
  lane_width_m?: number;
  speed_limit_mps?: number;
  buildings?: { density?: number; height_m?: [number, number]; setback_m?: number };
  signals?: boolean;
  seed?: number;
  params?: Record<string, unknown>;
}

// ---------------------------------------------------------------------------
// §6.12 — events, metrics, export
// ---------------------------------------------------------------------------

/** §6.12 `events.set` params. No channel is subscribed by default (§6.12 decision 28). */
export interface EventsSetParams {
  subscribe?: ChannelName[];
  unsubscribe?: ChannelName[];
  only?: ChannelName[];
  filter?: { nodes?: NodeId[]; follow_radius_m?: number; min_severity?: number; sample_1_in?: number };
  max_events_per_step?: number;
  list?: boolean;
}
/** §6.12 `events.set` result. */
export interface EventsSetResult {
  subscribed: { channel: ChannelName; channel_id: number; visibility: Visibility; est_rate_per_s?: number }[];
  available?: Record<string, unknown>[];
}
/** §6.12 `metrics.query` params. */
export interface MetricsQueryParams {
  metrics?: string[];
  t_from_ns?: SimTimeNs;
  t_to_ns?: SimTimeNs;
  bin_ns?: SimTimeNs;
  group_by?: ("t" | "node" | "class" | "dist_bin" | "density_bin" | "region" | "protocol" | "rat" | "tier" | "run" | "cause")[];
  where?: Record<string, string | number | (string | number)[]>;
  runs?: RunId[];
  agg?: "sum" | "mean" | "p50" | "p95" | "p99" | "ratio" | "rate" | "max" | "min";
  format?: "json" | "arrow";
  limit?: number;
}
/** §6.12 `metrics.query` result. */
export interface MetricsQueryResult {
  columns: { name: string; type: "int" | "float" | "string" | "time_ns"; unit?: string; visibility?: Visibility }[];
  rows: (string | number | boolean | null)[][];
  truncated?: boolean;
  arrow_url?: string;
  catalogue?: {
    name: string;
    unit: string;
    dims: string[];
    agg: string;
    visibility: Visibility;
    definition_md?: string;
    source?: Record<string, unknown>;
    /** The metric a series is a view of: its own name for a headline, the metric's for `x.p95` or `x[label]`. */
    base?: string;
  }[];
  provenance?: ProvenanceInfo[];
}
/** §6.12 `metrics.plot` params. */
export interface MetricsPlotParams {
  metric: string | string[];
  x?: "t" | "dist_bin" | "density_bin" | "node" | "class" | "run" | "config";
  by?: string[];
  runs?: RunId[];
  kind?: "line" | "scatter" | "bar" | "box" | "heatmap" | "cdf";
  preset?: string;
  ci?: number;
  render?: "figure" | "svg" | "png" | "csv";
  width_px?: number;
  height_px?: number;
}
/** §6.12 `metrics.plot` result — `figure` is Plotly figure JSON. */
export interface MetricsPlotResult {
  figure: Record<string, unknown>;
  url?: string;
  manifest_hash?: Sha256Hex;
  provenance?: ProvenanceInfo[];
}
/** §6.12 `export.dataset` params. */
export interface ExportDatasetParams {
  exporter: "ma-dataset" | "receiver-logs" | "telemetry" | "net-trace" | "backend-log" | "metrics";
  out_dir?: string;
  opts?: Record<string, unknown>;
  visibility?: "node" | "gt" | "both";
  t_from_ns?: SimTimeNs;
  t_to_ns?: SimTimeNs;
  compress?: "none" | "zstd" | "gzip";
}
/** §6.12 `export.dataset` result when it completes inside 2 s. */
export interface ExportDatasetFiles {
  files: { path: string; sha256: Sha256Hex; bytes: number; schema: string; visibility: Visibility; rows?: number }[];
  digest: Sha256Hex;
  datasheet?: string;
  leakage_lint?: { passed?: boolean; findings?: Record<string, unknown>[] };
}
/** §6.12 `export.recording` params. */
export interface ExportRecordingParams {
  path?: string;
  t_from_ns?: SimTimeNs;
  t_to_ns?: SimTimeNs;
  channels?: ChannelName[];
  profile?: "full" | "node";
  compression?: "zstd" | "lz4" | "none";
  chunk_mb?: number;
}
/** §6.12 `export.recording` result when it completes inside 2 s. */
export interface ExportRecordingResult {
  path: string;
  sha256: Sha256Hex;
  bytes: number;
  messages: number;
  channels: string[];
  keyframes?: number;
  t_from_ns: SimTimeNs;
  t_to_ns: SimTimeNs;
  profile?: "full" | "node";
}

// ---------------------------------------------------------------------------
// §6.13 — experiments and meta
// ---------------------------------------------------------------------------

/** §6.13 `experiment.define` params. */
export interface ExperimentDefineParams {
  name: string;
  base?: string | Record<string, unknown>;
  sweep: Record<string, (string | number | boolean)[]>;
  seeds?: { count?: number; master?: number };
  replications_policy?: { ci?: number; method?: "bootstrap" | "t" | "none" };
  outputs?: string[];
  resources?: { parallel?: number; tier_overrides_for_large_n?: Record<string, string> };
}
/** §6.13 `experiment.define` result. */
export interface ExperimentDefineResult {
  experiment_id: string;
  cells: number;
  cell_keys?: Record<string, unknown>[];
  estimated_wall_s: number;
  warnings?: ValidationError[];
}
/** §6.13 `experiment.run` params. */
export interface ExperimentRunParams {
  experiment_id: string;
  resume?: boolean;
  cells?: number[];
  runner?: "local" | "slurm" | "k8s";
}
/** §6.13 `experiment.run` result — a {@link Job} plus experiment counters. */
export interface ExperimentRunResult extends Job {
  experiment_id?: string;
  cells_total?: number;
  cells_skipped?: number;
}
/** §6.13 `experiment.status` params. */
export interface ExperimentStatusParams {
  experiment_id?: string;
  job_id?: string;
  include_cells?: boolean;
}
/** §6.13 `experiment.status` result. */
export interface ExperimentStatusResult {
  experiment_id: string;
  state: "defined" | "queued" | "running" | "done" | "failed" | "cancelled";
  cells_total: number;
  cells_done: number;
  cells_failed?: number;
  progress?: number;
  eta_s?: number;
  outputs?: string[];
  experiment_manifest_hash?: Sha256Hex;
  cells?: {
    index: number;
    key: Record<string, unknown>;
    state: "pending" | "running" | "done" | "failed" | "skipped";
    run_id?: RunId;
    manifest_hash?: Sha256Hex;
    recording?: string;
    error?: string;
  }[];
}
/** §6.13 `rpc.discover` params. */
export interface RpcDiscoverParams {
  method?: string;
}
/** §6.13 `rpc.discover` result — an OpenRPC 1.3.2 document. */
export interface RpcDiscoverResult {
  openrpc: string;
  info: Record<string, unknown>;
  methods: Record<string, unknown>[];
  components?: Record<string, unknown>;
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// §6.15 — the method inventory: 31 methods + rpc.discover = 32
// ---------------------------------------------------------------------------

/** §6.15 — every method, with its params and result type. */
export interface VwpMethods {
  "run.start": { params: RunStartParams; result: RunStartResult };
  "run.pause": { params: RunPauseParams; result: RunPauseResult };
  "run.resume": { params: RunPauseParams; result: RunPauseResult };
  "run.step": { params: RunStepParams; result: RunStepResult };
  "run.seek": { params: RunSeekParams; result: RunSeekResult };
  "run.speed": { params: RunSpeedParams; result: RunSpeedResult };
  "run.stop": { params: RunStopParams; result: RunStopResult };
  "run.status": { params: RunStatusParams; result: RunStatusResult };
  "view.follow": { params: ViewFollowParams; result: ViewFollowResult };
  "view.camera": { params: ViewCameraParams; result: ViewCameraResult };
  "overlay.set": { params: OverlaySetParams; result: OverlaySetResult };
  "inspect.node": { params: InspectNodeParams; result: InspectNodeResult };
  "inspect.link": { params: InspectLinkParams; result: InspectLinkResult };
  "inspect.entity": { params: InspectEntityParams; result: InspectEntityResult };
  explain: { params: ExplainParams; result: ExplainResult };
  "scenario.get": { params: ScenarioGetParams; result: ScenarioGetResult };
  "scenario.set": { params: ScenarioSetParams; result: ScenarioSetResult };
  "scenario.validate": { params: ScenarioValidateParams; result: ScenarioValidateResult };
  "scenario.save": { params: ScenarioSaveParams; result: ScenarioSaveResult };
  "scenario.load": { params: ScenarioLoadParams; result: ScenarioLoadResult };
  "scenario.list": { params: ScenarioListParams; result: ScenarioListResult };
  "world.import_osm": { params: WorldImportOsmParams; result: Job | WorldImportResult };
  "world.generate": { params: WorldGenerateParams; result: Job | WorldImportResult };
  "events.set": { params: EventsSetParams; result: EventsSetResult };
  "metrics.query": { params: MetricsQueryParams; result: MetricsQueryResult };
  "metrics.plot": { params: MetricsPlotParams; result: MetricsPlotResult };
  "export.dataset": { params: ExportDatasetParams; result: Job | ExportDatasetFiles };
  "export.recording": { params: ExportRecordingParams; result: Job | ExportRecordingResult };
  "experiment.define": { params: ExperimentDefineParams; result: ExperimentDefineResult };
  "experiment.run": { params: ExperimentRunParams; result: ExperimentRunResult };
  "experiment.status": { params: ExperimentStatusParams; result: ExperimentStatusResult };
  "rpc.discover": { params: RpcDiscoverParams; result: RpcDiscoverResult };
}

/** Every method name of §6.15. */
export type VwpMethodName = keyof VwpMethods;

/** §6.15 — the method names as a runtime array (32 entries), in the order the table lists them. */
export const VWP_METHODS: readonly VwpMethodName[] = [
  "run.start", "run.pause", "run.resume", "run.step", "run.seek", "run.speed", "run.stop", "run.status",
  "view.follow", "view.camera", "overlay.set",
  "inspect.node", "inspect.link", "inspect.entity", "explain",
  "scenario.get", "scenario.set", "scenario.validate", "scenario.save", "scenario.load", "scenario.list",
  "world.import_osm", "world.generate",
  "events.set",
  "metrics.query", "metrics.plot",
  "export.dataset", "export.recording",
  "experiment.define", "experiment.run", "experiment.status",
  "rpc.discover",
] as const;

/** Params of a given method. */
export type ParamsOf<M extends VwpMethodName> = VwpMethods[M]["params"];
/** Result of a given method. */
export type ResultOf<M extends VwpMethodName> = VwpMethods[M]["result"];

// ---------------------------------------------------------------------------
// §6.14 — server → client notifications
// ---------------------------------------------------------------------------

/** §6.14 `run.state`. */
export interface RunStateNotification {
  state: RunState;
  t_ns: SimTimeNs;
  run_id: RunId;
  reason?: string;
}
/** §1.5 / §6.14 `stream.drop` — a backpressure drop; binary frames stay byte-identical. */
export interface StreamDropNotification {
  seq_first: number;
  seq_last: number;
  dropped: { delta?: number; event?: number; telemetry?: number; metric?: number };
  resync_seq: number;
}
/** §6.14 `job.progress`. */
export interface JobProgressNotification {
  job_id: string;
  progress: number;
  message?: string;
}
/** §6.14 `job.done`. */
export interface JobDoneNotification {
  job_id: string;
  state: "done" | "failed" | "cancelled";
  outputs?: unknown[];
  error?: string;
}
/** §6.14 `view.changed`. */
export interface ViewChangedNotification {
  mode: CameraMode;
  position?: Vec3;
  target?: Vec3;
  fov_deg?: number;
  following?: number | null;
}
/** §6.14 `log`. */
export interface LogNotification {
  level: string;
  target?: string;
  message: string;
  t_ns?: SimTimeNs;
}
/** §6.14 `validation`. */
export interface ValidationNotification {
  errors: ValidationError[];
  warnings: ValidationError[];
}
/** §6.14 `experiment.progress`. */
export interface ExperimentProgressNotification {
  experiment_id: string;
  cells_done: number;
  cells_total: number;
  eta_s?: number;
}

/** §6.14 — the eight server→client notifications. */
export interface VwpNotifications {
  "run.state": RunStateNotification;
  "stream.drop": StreamDropNotification;
  "job.progress": JobProgressNotification;
  "job.done": JobDoneNotification;
  "view.changed": ViewChangedNotification;
  log: LogNotification;
  validation: ValidationNotification;
  "experiment.progress": ExperimentProgressNotification;
}

/** Every notification name of §6.14. */
export type VwpNotificationName = keyof VwpNotifications;

/** §6.14 — the notification names as a runtime array (8 entries). */
export const VWP_NOTIFICATIONS: readonly VwpNotificationName[] = [
  "run.state", "stream.drop", "job.progress", "job.done", "view.changed", "log", "validation", "experiment.progress",
] as const;

// ---------------------------------------------------------------------------
// §6.1 — the wire envelope and the client
// ---------------------------------------------------------------------------

/** §6.1 — a JSON-RPC 2.0 request or notification as it goes on the wire. */
export interface JsonRpcRequest {
  jsonrpc: "2.0";
  method: string;
  params?: unknown;
  /** absent for a notification */ id?: string | number;
}
/** §6.4 — a JSON-RPC error object. */
export interface JsonRpcErrorObject {
  code: number;
  message: string;
  data?: unknown;
}
/** §6.1 — a JSON-RPC 2.0 response. */
export interface JsonRpcResponse {
  jsonrpc: "2.0";
  id: string | number | null;
  result?: unknown;
  error?: JsonRpcErrorObject;
}
/** Anything the server may send on the text channel. */
export type JsonRpcIncoming = JsonRpcResponse | JsonRpcRequest;

/** A JSON-RPC error raised by a call (§6.4). */
export class JsonRpcError extends Error {
  override readonly name = "JsonRpcError";
  readonly code: number;
  readonly data: unknown;
  readonly method: string;

  constructor(method: string, error: JsonRpcErrorObject) {
    super(`${method} failed: ${error.message} (${error.code})`);
    this.code = error.code;
    this.data = error.data;
    this.method = method;
  }
}

/** Handler for one notification method. */
export type NotificationHandler<N extends VwpNotificationName> = (params: VwpNotifications[N]) => void;
/** Handler for a notification method this client does not know (§10.7 R9: tolerate them). */
export type UnknownNotificationHandler = (method: string, params: unknown) => void;

/** How {@link JsonRpcClient} puts text on the wire. */
export type JsonRpcSend = (text: string) => void;

/** Options for {@link JsonRpcClient}. */
export interface JsonRpcClientOptions {
  /** Milliseconds before an outstanding request rejects; 0 disables the timeout. Default 30000. */
  readonly timeoutMs?: number;
  /** Called for a notification whose method is not one of the eight in §6.14. */
  readonly onUnknownNotification?: UnknownNotificationHandler;
}

interface Pending {
  readonly method: string;
  readonly resolve: (value: unknown) => void;
  readonly reject: (reason: Error) => void;
  readonly timer: ReturnType<typeof setTimeout> | null;
}

/**
 * A typed JSON-RPC 2.0 client for the text channel of a VWP connection (§6).
 *
 * It correlates responses to requests by `id`, exposes the 32 methods of §6.15 with their param and
 * result types, and dispatches the 8 notifications of §6.14. It never sends a batch array (§6.1).
 */
export class JsonRpcClient {
  #send: JsonRpcSend;
  #nextId = 1;
  #pending = new Map<string | number, Pending>();
  #handlers = new Map<string, Set<(params: unknown) => void>>();
  #timeoutMs: number;
  #onUnknown: UnknownNotificationHandler | undefined;

  constructor(send: JsonRpcSend, options: JsonRpcClientOptions = {}) {
    this.#send = send;
    this.#timeoutMs = options.timeoutMs ?? 30_000;
    this.#onUnknown = options.onUnknownNotification;
  }

  /** Replace the transport (used after a reconnect). */
  setTransport(send: JsonRpcSend): void {
    this.#send = send;
  }

  /** Number of requests awaiting a reply. */
  get pendingCount(): number {
    return this.#pending.size;
  }

  /**
   * Call a method and await its typed result.
   *
   * `options.timeoutMs` overrides the client's timeout for this one call — for a call the server
   * reports progress on while it works (a `run.seek` past what a live kernel has produced sends
   * `job.progress` until it lands), where the default would give up on a call that is going well.
   */
  request<M extends VwpMethodName>(
    method: M,
    params: ParamsOf<M>,
    options: { readonly timeoutMs?: number } = {},
  ): Promise<ResultOf<M>> {
    const id = this.#nextId++;
    const payload: JsonRpcRequest = { jsonrpc: "2.0", method, params, id };
    const timeoutMs = options.timeoutMs ?? this.#timeoutMs;
    return new Promise<ResultOf<M>>((resolve, reject) => {
      const timer =
        timeoutMs > 0
          ? setTimeout(() => {
              this.#pending.delete(id);
              reject(new Error(`${method} timed out after ${timeoutMs} ms`));
            }, timeoutMs)
          : null;
      this.#pending.set(id, {
        method,
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });
      try {
        this.#send(JSON.stringify(payload));
      } catch (err) {
        this.#pending.delete(id);
        if (timer) clearTimeout(timer);
        reject(err instanceof Error ? err : new Error(String(err)));
      }
    });
  }

  /** Send a notification (no `id`, no reply expected, §6.1). */
  notify<M extends VwpMethodName>(method: M, params: ParamsOf<M>): void {
    const payload: JsonRpcRequest = { jsonrpc: "2.0", method, params };
    this.#send(JSON.stringify(payload));
  }

  /** Subscribe to one of the eight server→client notifications (§6.14). Returns an unsubscribe. */
  on<N extends VwpNotificationName>(method: N, handler: NotificationHandler<N>): () => void {
    let set = this.#handlers.get(method);
    if (!set) {
      set = new Set();
      this.#handlers.set(method, set);
    }
    const wrapped = handler as (params: unknown) => void;
    set.add(wrapped);
    return () => {
      set?.delete(wrapped);
    };
  }

  /**
   * Feed one text frame from the socket. Returns the parsed message, or `null` when the text was
   * not valid JSON-RPC (the caller may then close with −32700 semantics).
   */
  handleText(text: string): JsonRpcIncoming | null {
    let parsed: unknown;
    try {
      parsed = JSON.parse(text);
    } catch {
      return null;
    }
    if (Array.isArray(parsed)) return null; // §6.1: batches are not supported
    if (typeof parsed !== "object" || parsed === null) return null;
    const msg = parsed as JsonRpcResponse & JsonRpcRequest;
    if (typeof msg.method === "string") {
      this.#dispatchNotification(msg.method, msg.params);
      return msg as JsonRpcRequest;
    }
    if (msg.id !== undefined && msg.id !== null) {
      const pending = this.#pending.get(msg.id);
      if (pending) {
        this.#pending.delete(msg.id);
        if (pending.timer) clearTimeout(pending.timer);
        if (msg.error) pending.reject(new JsonRpcError(pending.method, msg.error));
        else pending.resolve(msg.result);
      }
      return msg as JsonRpcResponse;
    }
    return null;
  }

  #dispatchNotification(method: string, params: unknown): void {
    const set = this.#handlers.get(method);
    if (set && set.size > 0) {
      for (const h of set) h(params);
      return;
    }
    // §10.7 R9: unknown notification methods are tolerated, not errors.
    this.#onUnknown?.(method, params);
  }

  /** Reject every outstanding request, e.g. when the socket closed. */
  rejectAll(reason: Error): void {
    for (const [, pending] of this.#pending) {
      if (pending.timer) clearTimeout(pending.timer);
      pending.reject(reason);
    }
    this.#pending.clear();
  }
}

/**
 * §6.2 — the same methods over `POST /rpc`, for CLI, notebooks and CI. Connection-scoped methods
 * (`view.follow`, `view.camera`, `overlay.set`) return −32009 there.
 */
export async function rpcOverHttp<M extends VwpMethodName>(
  endpoint: string,
  method: M,
  params: ParamsOf<M>,
  init: RequestInit = {},
): Promise<ResultOf<M>> {
  const body: JsonRpcRequest = { jsonrpc: "2.0", method, params, id: 1 };
  const res = await fetch(endpoint, {
    ...init,
    method: "POST",
    headers: { "content-type": "application/json", ...(init.headers ?? {}) },
    body: JSON.stringify(body),
  });
  if (!res.ok) throw new Error(`POST ${endpoint} returned HTTP ${res.status}`);
  const parsed = (await res.json()) as JsonRpcResponse;
  if (parsed.error) throw new JsonRpcError(method, parsed.error);
  return parsed.result as ResultOf<M>;
}
