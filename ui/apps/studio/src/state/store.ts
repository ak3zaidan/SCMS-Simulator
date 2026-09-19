/**
 * The Studio's React-visible state.
 *
 * Everything here is a *projection*: the hot structures (pose buffer, interpolator, instance
 * matrices, telemetry rings) live in `engine.ts` and in `@vwp/viewer`, and are copied in at 5 Hz.
 * Nothing in this file is on a 60 fps path.
 *
 * The 5 Hz beat is unconditional, though, so a setter that always stores a fresh object re-renders
 * every subscriber 5x a second whether or not the numbers moved. The setters the flush drives —
 * `setStats`, `setFrameCounts`, `setRun`, `setTelemetry`, `setMetricProjection`,
 * `addTimelineMarks` — therefore return the previous state unchanged when the content matches;
 * Zustand skips the notification entirely in that case. `bumpSeries` is the deliberate exception:
 * it *is* the 5 Hz beat the sparklines and plots redraw on (09-ui §4).
 */

import { create } from "zustand";
import type {
  InspectNodeResult,
  NodeTelemetry,
  OverlayName,
  ValidationError,
  VwpConnectionState,
  RunState,
} from "@vwp/protocol";
import type { CameraMode } from "@vwp/viewer";
import type { ThemeName } from "../lib/theme.js";

/** §3.1.3 — one row of the `Hello` node table. */
export interface NodeInfo {
  readonly nodeId: number;
  readonly actorId: number | null;
  readonly label: string;
  readonly profileId: string;
  readonly kind: number;
  readonly flags: number;
  readonly classIdx: number | null;
  readonly x: number;
  readonly y: number;
  readonly z: number;
}

/** §3.8 — one resolved provenance entry. */
export interface ProvEntry {
  readonly provId: number;
  readonly modelId: string;
  readonly modelVersion: string;
  readonly paramSetId: string;
  readonly cardUrl: string;
  readonly family: number;
  readonly subjectKind: number;
}

/** What the connection `Hello` (§3.1) told us. */
export interface HelloSummary {
  readonly runId: string;
  readonly engineVersion: string;
  readonly scenarioName: string;
  readonly runLabel: string;
  readonly worldHash: string;
  readonly scenarioHash: string;
  readonly flags: number;
  readonly simDurationNs: number;
  readonly mobilityStepNs: number;
  readonly keyframePeriodNs: number;
  readonly telemetryPeriodNs: number;
  readonly actorCapacity: number;
  readonly nodeCount: number;
  readonly classNames: readonly string[];
  readonly channels: readonly { name: string; id: number; visibility: number; enabled: boolean }[];
  readonly origin: { lat: number; lon: number; alt: number };
  readonly bbox: { minX: number; minY: number; maxX: number; maxY: number };
  readonly versionMajor: number;
  readonly versionMinor: number;
}

/** §4 — what the decoded world contains. */
export interface WorldSummary {
  readonly lanes: number;
  readonly buildings: number;
  readonly junctions: number;
  readonly signals: number;
  readonly sites: number;
  readonly crossings: number;
  readonly landuse: number;
  readonly bytes: number;
  readonly buildMs: number;
  readonly buildingBackend: string;
  readonly drawables: number;
}

/** §6.6 `run.status`, plus what the notifications update. */
export interface RunInfo {
  readonly state: RunState;
  readonly tNs: number;
  readonly tEndNs: number;
  readonly speed: number;
  readonly actors: number;
  readonly nodes: number;
  readonly runId: string;
  readonly profile: "full" | "node";
  readonly live: boolean;
}

/** The pseudonym the followed node is currently using (§3.6.7 `sec.cert`, §3.6.4 `node.tx`). */
export interface PseudonymInfo {
  readonly digest: string;
  readonly i: number | null;
  readonly j: number | null;
  readonly source: "sec.cert" | "node.tx";
}

/** A marker on the scrub bar (09-ui §6 "scrub bar with event markers"). */
export interface TimelineMark {
  readonly tNs: number;
  readonly channel: string;
  readonly nodeId: number;
  readonly label: string;
  readonly provId?: number;
}

/** One line in the inspector's log tab. */
export interface LogLine {
  readonly level: "info" | "warn" | "error";
  readonly target: string;
  readonly message: string;
  readonly at: number;
}

/** What the HUD's performance strip shows, copied out of `FrameStats.snapshot()`. */
export interface StatsView {
  readonly fps: number;
  readonly fpsAverage: number;
  readonly frameMs: number;
  readonly p95Ms: number;
  readonly cpuMs: number;
  readonly drawCalls: number;
  readonly triangles: number;
  readonly actorInstances: number;
  readonly actorCulled: number;
  readonly actorLive: number;
  readonly buildingsVisible: number;
}

/** Frames received, by type (§2.4). */
export interface FrameCounts {
  readonly keyframe: number;
  readonly delta: number;
  readonly telemetry: number;
  readonly event: number;
  readonly metric: number;
}

/** An entry of `scenario.list` (§6.10). */
export interface ScenarioListItem {
  readonly id: string;
  readonly kind: string;
  readonly name?: string;
  readonly description?: string;
  readonly tags?: string[];
  readonly hash?: string;
}

/** One catalogue row of `overlay.set {list:true}` (§6.7). */
export interface ServerOverlay {
  readonly name: string;
  readonly visibility: string;
  readonly available: boolean;
  readonly description?: string;
  readonly needs_channels?: string[];
}

/** The validation outcome of `scenario.validate` (§6.10) or the `validation` notification. */
export interface ValidationView {
  readonly valid: boolean;
  readonly errors: readonly ValidationError[];
  readonly warnings: readonly ValidationError[];
}

/** What the inspector is explaining right now. */
export interface WhySubject {
  readonly kind: "metric" | "node_field" | "actor_field" | "event" | "link" | "entity" | "channel" | "world" | "overlay";
  readonly id: string;
  readonly label: string;
  readonly node?: number;
  readonly actor?: number;
  readonly provId?: number;
  readonly value?: string;
  readonly unit?: string;
}

const EMPTY_RUN: RunInfo = {
  state: "idle", tNs: 0, tEndNs: 0, speed: 1, actors: 0, nodes: 0, runId: "", profile: "full", live: true,
};

interface StudioState {
  connection: VwpConnectionState;
  hello: HelloSummary | null;
  world: WorldSummary | null;
  run: RunInfo;
  selectedActor: number | null;
  selectedNode: number | null;
  telemetry: NodeTelemetry | null;
  telemetryNode: number | null;
  simTimeNs: number;
  pseudonym: PseudonymInfo | null;
  inspect: InspectNodeResult | null;
  overlays: Partial<Record<OverlayName, boolean>>;
  serverOverlays: readonly ServerOverlay[];
  groundTruthLocked: boolean;
  cameraMode: CameraMode;
  theme: ThemeName;
  stats: StatsView | null;
  frames: FrameCounts;
  rpcMethods: readonly { name: string; summary: string }[];
  rpcTitle: string;
  rpcCalls: readonly { method: string; at: number }[];
  scenario: unknown;
  scenarioHash: string;
  scenarioSchema: Record<string, unknown> | null;
  scenarioList: readonly ScenarioListItem[];
  validation: ValidationView | null;
  timeline: readonly TimelineMark[];
  logs: readonly LogLine[];
  provenanceCount: number;
  metricProvenance: Readonly<Record<string, number>>;
  metricDims: Readonly<Record<string, string>>;
  why: WhySubject | null;
  inspectorTab: "state" | "why" | "log";
  hudDocked: boolean;
  seriesTick: number;

  setConnection: (s: VwpConnectionState) => void;
  setHello: (h: HelloSummary) => void;
  setWorldSummary: (w: WorldSummary) => void;
  setRun: (r: Partial<RunInfo>) => void;
  setSelection: (actorId: number | null, nodeId: number | null) => void;
  setTelemetry: (t: NodeTelemetry | null, node: number | null, simTimeNs: number) => void;
  notePseudonym: (p: PseudonymInfo) => void;
  setInspect: (r: InspectNodeResult | null) => void;
  setOverlays: (o: Partial<Record<OverlayName, boolean>>) => void;
  setServerOverlays: (o: readonly ServerOverlay[]) => void;
  setGroundTruthLocked: (v: boolean) => void;
  setCameraMode: (m: CameraMode) => void;
  setTheme: (t: ThemeName) => void;
  /** 5 Hz from `engine.flushProjection()`; keeps the previous object when the numbers match. */
  setStats: (s: StatsView) => void;
  /** 5 Hz from `engine.flushProjection()`; keeps the previous object when the counters match. */
  setFrameCounts: (f: FrameCounts) => void;
  setRpcMethods: (m: readonly { name: string; summary: string }[], title: string) => void;
  noteRpcCall: (method: string) => void;
  setScenario: (doc: unknown, hash: string, schema: Record<string, unknown> | null) => void;
  setScenarioList: (items: readonly ScenarioListItem[]) => void;
  setValidation: (v: ValidationView | null) => void;
  addTimelineMarks: (marks: readonly TimelineMark[]) => void;
  addLog: (line: LogLine) => void;
  setProvenanceCount: (n: number) => void;
  /**
   * Publish the metric → `prov_id` and metric → `dim_key` maps the engine accumulated (§3.7/§3.8).
   *
   * The engine hands over a fresh object only when its own map changed, so passing the same object
   * twice is a no-op and the slice keeps its identity.
   */
  setMetricProjection: (provenance: Readonly<Record<string, number>>, dims: Readonly<Record<string, string>>) => void;
  setWhy: (w: WhySubject | null) => void;
  setInspectorTab: (t: "state" | "why" | "log") => void;
  setHudDocked: (v: boolean) => void;
  bumpSeries: () => void;
}

const MAX_LOGS = 300;
export const MAX_MARKS = 600;

/**
 * Whether two `StatsView`s carry the same numbers.
 *
 * `engine.flushProjection()` runs at STORE_HZ whether or not anything moved, so returning a fresh
 * object every time made every `useStudio((s) => s.stats)` consumer — the topbar readout, the
 * viewport chip — re-render 5x a second forever. Field-by-field is cheaper than the reconciliation
 * it prevents.
 */
function sameStats(a: StatsView | null, b: StatsView): boolean {
  return (
    a !== null &&
    a.fps === b.fps && a.fpsAverage === b.fpsAverage && a.frameMs === b.frameMs && a.p95Ms === b.p95Ms &&
    a.cpuMs === b.cpuMs && a.drawCalls === b.drawCalls && a.triangles === b.triangles &&
    a.actorInstances === b.actorInstances && a.actorCulled === b.actorCulled &&
    a.actorLive === b.actorLive && a.buildingsVisible === b.buildingsVisible
  );
}

function sameFrames(a: FrameCounts, b: FrameCounts): boolean {
  return a.keyframe === b.keyframe && a.delta === b.delta && a.telemetry === b.telemetry &&
    a.event === b.event && a.metric === b.metric;
}

/** Whether `run` would be unchanged by `patch` — `run.status` is polled every 2 s (§6.6). */
function sameRun(a: RunInfo, patch: Partial<RunInfo>): boolean {
  for (const key of Object.keys(patch) as (keyof RunInfo)[]) {
    if (patch[key] !== undefined && patch[key] !== a[key]) return false;
  }
  return true;
}

export const useStudio = create<StudioState>((set) => ({
  connection: "idle",
  hello: null,
  world: null,
  run: EMPTY_RUN,
  selectedActor: null,
  selectedNode: null,
  telemetry: null,
  telemetryNode: null,
  simTimeNs: 0,
  pseudonym: null,
  inspect: null,
  overlays: {},
  serverOverlays: [],
  groundTruthLocked: false,
  cameraMode: "map",
  theme: "dark",
  stats: null,
  frames: { keyframe: 0, delta: 0, telemetry: 0, event: 0, metric: 0 },
  rpcMethods: [],
  rpcTitle: "",
  rpcCalls: [],
  scenario: null,
  scenarioHash: "",
  scenarioSchema: null,
  scenarioList: [],
  validation: null,
  timeline: [],
  logs: [],
  provenanceCount: 0,
  metricProvenance: {},
  metricDims: {},
  why: null,
  inspectorTab: "state",
  hudDocked: false,
  seriesTick: 0,

  setConnection: (s) => set({ connection: s }),
  setHello: (h) => set({ hello: h, timeline: [] }),
  setWorldSummary: (w) => set({ world: w }),
  setRun: (r) => set((state) => (sameRun(state.run, r) ? state : { run: { ...state.run, ...r } })),
  setSelection: (actorId, nodeId) => set({ selectedActor: actorId, selectedNode: nodeId, pseudonym: null, inspect: null }),
  setTelemetry: (t, node, simTimeNs) =>
    set((state) =>
      state.telemetry === t && state.telemetryNode === node && state.simTimeNs === simTimeNs
        ? state
        : { telemetry: t, telemetryNode: node, simTimeNs },
    ),
  notePseudonym: (p) =>
    set((state) => {
      // A `sec.cert` change carries the i/j indices; a `node.tx` digest only refreshes the digest.
      if (p.source === "node.tx" && state.pseudonym && state.pseudonym.digest === p.digest) return state;
      if (p.source === "node.tx" && state.pseudonym?.source === "sec.cert" && state.pseudonym.digest === p.digest) return state;
      return { pseudonym: p };
    }),
  setInspect: (r) => set({ inspect: r }),
  setOverlays: (o) => set((state) => ({ overlays: { ...state.overlays, ...o } })),
  setServerOverlays: (o) => set({ serverOverlays: o }),
  setGroundTruthLocked: (v) => set({ groundTruthLocked: v }),
  setCameraMode: (m) => set({ cameraMode: m }),
  setTheme: (t) => set({ theme: t }),
  setStats: (s) => set((state) => (sameStats(state.stats, s) ? state : { stats: s })),
  setFrameCounts: (f) => set((state) => (sameFrames(state.frames, f) ? state : { frames: f })),
  setRpcMethods: (m, title) => set({ rpcMethods: m, rpcTitle: title }),
  noteRpcCall: (method) =>
    set((state) => ({ rpcCalls: [{ method, at: Date.now() }, ...state.rpcCalls].slice(0, 40) })),
  setScenario: (doc, hash, schema) => set({ scenario: doc, scenarioHash: hash, scenarioSchema: schema }),
  setScenarioList: (items) => set({ scenarioList: items }),
  setValidation: (v) => set({ validation: v }),
  addTimelineMarks: (marks) =>
    set((state) => (marks.length === 0 ? state : { timeline: [...state.timeline, ...marks].slice(-MAX_MARKS) })),
  addLog: (line) => set((state) => ({ logs: [line, ...state.logs].slice(0, MAX_LOGS) })),
  setProvenanceCount: (n) => set({ provenanceCount: n }),
  setMetricProjection: (provenance, dims) =>
    set((state) =>
      state.metricProvenance === provenance && state.metricDims === dims
        ? state
        : { metricProvenance: provenance, metricDims: dims },
    ),
  setWhy: (w) => set({ why: w, inspectorTab: w ? "why" : "state" }),
  setInspectorTab: (t) => set({ inspectorTab: t }),
  setHudDocked: (v) => set({ hudDocked: v }),
  bumpSeries: () => set((state) => ({ seriesTick: state.seriesTick + 1 })),
}));
