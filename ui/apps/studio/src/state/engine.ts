/**
 * The imperative half of the Studio.
 *
 * React owns none of the hot path. This module owns the `VwpClient` (docs/protocol/vwp-v1.md §1),
 * the `Viewer`, the decoded world, the provenance dictionary (§3.8), the telemetry ring buffers and
 * the metric history; it pushes a small, throttled projection of that state into the Zustand store
 * so the panels re-render at a human rate (09-ui §4: "HUD updates 5 Hz for DOM, 20 Hz for
 * sparklines") while poses, deltas and instance writes stay on the 60 fps path inside `@vwp/viewer`.
 *
 * The engine serves the built Studio in production (09-ui §9), so every endpoint is same-origin and
 * relative: `/vwp/v1`, `/rpc`, `/world/{hash}.vwb`. `vite.config.ts` proxies those three in dev.
 */

import {
  ChannelId,
  VwpClient,
  bytesToHex,
  computeWorldContentHash,
  decodeWorld,
  formatUuid,
  verifyWorldPayload,
  ProtocolError,
  type ByeMessage,
  type DeltaMessage,
  type ErrorMessage,
  type EventMessage,
  type HelloMessage,
  type MetricSampleMessage,
  type NodeTelemetry,
  type ProvenanceMessage,
  type TelemetryMessage,
  type VwpMethodName,
  type VwpWorld,
  type WorldChunkMessage,
  type ParamsOf,
  type ResultOf,
} from "@vwp/protocol";
import { CAMERA_MODES, Viewer, type CameraMode, type OverlayEntry } from "@vwp/viewer";
import type { OverlayName } from "@vwp/protocol";

import { MetricHistory, SeriesRing } from "../lib/history.js";
import {
  LocalReplay,
  loadReplayBindings,
  type ReplayBindings,
  type ReplayModuleLocation,
} from "../lib/replay.js";
import { SPARKLINE_SERIES } from "../lib/telemetry.js";
import { hex, shortDigest } from "../lib/format.js";
import { resolveEngineUrl } from "../lib/target.js";
import { studioTheme, type ThemeName } from "../lib/theme.js";
import {
  MAX_MARKS,
  useStudio,
  type LogLine,
  type NodeInfo,
  type ProvEntry,
  type PseudonymInfo,
  type TimelineMark,
} from "./store.js";

/**
 * Channels the Studio subscribes to on connect (§6.12 `events.set`).
 *
 * Exactly the four §6.12 names: "The Studio subscribes `app.warning`, `det.observation`,
 * `proto.revocation` and `sec.cert` at start-up, and subscribes the high-rate channels only for the
 * followed node." The three high-rate ones — `node.tx`, `phy.rx`, `mac.cbr` — used to be in this
 * list, which against the fixture engine cost a readable pulse overlay and against a real engine is
 * the "millions of `phy.rx` records per simulated second" §6.12 warns about. They are now
 * subscribed by {@link StudioEngine.setFollowChannels} when something is followed and dropped when
 * the selection is cleared; see that method for why the node restriction is not applied with them.
 */
const DEFAULT_CHANNELS = [
  "sec.cert",
  "det.observation",
  "app.warning",
  "proto.revocation",
] as const;

/**
 * Channels subscribed only while a node is followed (§6.12).
 *
 * `node.tx` is also where the followed node's pseudonym digest comes from (§3.6.4), so following a
 * node is what makes the HUD's pseudonym line work — the subscription and the HUD field arrive
 * together rather than one being useful without the other.
 */
const FOLLOW_CHANNELS = ["node.tx", "phy.rx", "mac.cbr"] as const;

/**
 * Overlays enabled the moment a world is on screen.
 *
 * `reported`, `revoked` and `detections` are in the set because they are the three channels of
 * `StateMarkerOverlay`, and that overlay *is* the shape redundancy 09-ui §10 requires alongside the
 * colour-blind-safe state palette ("colour-blind-safe categorical palette for actor states …
 * with shape redundancy"). Leaving them opt-in meant a colour-blind user got colour only until
 * they found the overlay menu. `attackers_gt` stays out: it is ground truth, so `lockGroundTruth`
 * has to be able to refuse it (09-ui §6, blind evaluation), and the `reported` channel is the
 * non-GT sibling that keeps a shape channel alive while the lock is on.
 */
export const DEFAULT_OVERLAYS: readonly OverlayName[] = [
  "lane_markings", "signal_state", "tx_pulses", "reported", "revoked", "detections",
];

/**
 * Pulses drawn per `Event` frame, and their radius.
 *
 * The mock engine emits a `node.tx` for every equipped node every mobility step — 2,000 a second at
 * 200 actors — and one expanding 60 m ring each would bury the scene. The budget samples the frame
 * evenly instead, which keeps the overlay readable in map mode and out of the way in chase mode.
 */
const PULSE_BUDGET = 40;
const PULSE_RADIUS_M = 40;
const LINK_BUDGET = 1500;

/**
 * Ground extent the map camera opens on, metres.
 *
 * `Viewer.setWorld` frames the whole `vwp-world/1` bbox, which for a 3 km² import puts the street
 * grid below one pixel per lane. Opening on a readable block scale and letting the user zoom out is
 * the friendlier default; the full extent is one scroll away.
 */
const MAP_OPEN_EXTENT_M = 1400;

const SPARK_KEYS = SPARKLINE_SERIES.map((s) => s.key);

/** How often the DOM-side projection of the hot state is refreshed (09-ui §4). */
const STORE_HZ = 5;

/**
 * The methods that act on a connection's own view and are refused over `POST /rpc` (§6.2, −32009).
 *
 * The list is `crates/v2xw-server`'s `rpc::CONNECTION_SCOPED`, and it is short for a reason: every
 * other method is answerable without a socket, which is what {@link StudioEngine.request}'s HTTP
 * fallback relies on. `run.seek` is the near miss — it is not on this list, but it streams its
 * frames on the connection and the server refuses it over HTTP with a message saying so, so the
 * interface treats a scrub as needing an open stream.
 */
const CONNECTION_SCOPED: readonly VwpMethodName[] = ["view.follow", "view.camera", "overlay.set"];

/**
 * How many metric names the store projection keeps.
 *
 * `MetricSample.str_metric` (§3.7) is a wire-supplied string id, so the name set is engine- and
 * scenario-controlled. `metricProvenance` and `metricDims` are plain objects rebuilt on publish,
 * which is O(k), so k has to be bounded; the oldest name is evicted first.
 */
const METRIC_KEY_LIMIT = 2000;

/** One link record the `links` overlay draws, pooled so an `Event` frame allocates nothing. */
interface LinkRecord {
  ax: number; ay: number; az: number;
  bx: number; by: number; bz: number;
  gt: boolean; strength: number;
}

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

/** The one engine instance the app talks to. */
export class StudioEngine {
  client: VwpClient | null = null;
  viewer: Viewer | null = null;
  world: VwpWorld | null = null;

  /** §3.8 — `prov_id` → the model that produced the value. */
  readonly provenance = new Map<number, ProvEntry>();
  /** §3.8 — `dim_key` → the canonical `"k=v,k=v"` string. */
  readonly dims = new Map<number, string>();
  /** §3.1.3 — the node table, by node id. */
  readonly nodes = new Map<number, NodeInfo>();
  /** actor id → node id, so a click in the viewport becomes a `view.follow` (§6.7). */
  readonly nodeByActor = new Map<number, number>();
  /** The last `Telemetry` record per node (§3.5). */
  readonly telemetry = new Map<number, NodeTelemetry>();
  /** The sparkline ring for the followed node. */
  readonly spark = new SeriesRing(SPARK_KEYS, 300);
  /** Every metric sample seen, for the plots strip (§3.7). */
  readonly metrics = new MetricHistory(900);

  #detachViewer: (() => void) | null = null;
  #storeTimer: ReturnType<typeof setInterval> | null = null;
  #statusTimer: ReturnType<typeof setInterval> | null = null;
  #followedNode: number | null = null;
  #worldChunks: Uint8Array[] = [];
  #worldChunkBytes = 0;
  /** §3.1.1 — the §4.2 payload digest this run promised, lower-case hex; what §10.5 W3 checks. */
  #promisedWorldHash: string | null = null;
  /** The base URL the current connection was opened against; `""` before the first connect. */
  #baseUrl = "";
  #dirty = true;
  /** Set once the user touches the buildings toggle, after which the camera stops driving it. */
  #buildingsUserSet = false;
  #lastSimTimeNs = 0;
  #frameCounts = { keyframe: 0, delta: 0, telemetry: 0, event: 0, metric: 0 };

  /** A pool, reused in place: `#pendingLinkCount` records are live, the rest are spare capacity. */
  #pendingLinks: LinkRecord[] = [];
  #pendingLinkCount = 0;
  /** Scratch for `#nodePositionInto`, so the event loop allocates no tuples. */
  readonly #posA = new Float64Array(3);
  readonly #posB = new Float64Array(3);

  // --- the throttled store projection (09-ui §4) ------------------------------------------------
  // Metric provenance, metric dimensions, timeline marks and the followed node's pseudonym all
  // arrive at stream rate — thousands of rows a second — and all four used to be written straight
  // into the React store from the stream handler, which is exactly the 5 Hz promise this module's
  // header makes. They are accumulated here and published by `flushProjection()` instead.
  #metricProv = new Map<string, number>();
  #metricDims = new Map<string, string>();
  #metricProjectionDirty = false;
  #pendingMarks: TimelineMark[] = [];
  #pendingPseudonyms: PseudonymInfo[] = [];

  // ---------------------------------------------------------------------------------------------
  // Connection
  // ---------------------------------------------------------------------------------------------

  /**
   * Open the VWP connection (§1.3) and bring the whole app up behind it.
   *
   * `baseUrl` is whatever `lib/target.ts` resolved — the page's own origin behind the Vite proxy and
   * in the deployed build, or an absolute origin when the user pinned one with `?engine=`. It is
   * remembered because `Hello.world_ref.str_url` is root-relative (§3.1.6): a pinned engine's world
   * has to be fetched from *that* origin, not from the page's.
   */
  /**
   * The connection attempt currently in flight, and the origin it is for.
   *
   * This exists because `#connectOnce` *starts* by tearing the previous client down, which is a
   * teardown and not a guard. Two overlapping calls therefore had the second one close the first
   * one's socket while it was still handshaking, and the browser reports exactly that: "WebSocket
   * is closed before the connection is established", surfacing as a 1006 with no Hello. React's
   * StrictMode double-invoke is the common way in, but any second caller does it — a reconnect
   * landing on top of the mount, or the user pressing Run while the first attempt is still open.
   *
   * Whether it bit was a race on how long the `/healthz` probe took, which is why it looked
   * intermittent: fast probe, the two calls overlap and it fails; slow probe, the first finishes
   * first and it works.
   */
  #connectInFlight: { url: string; promise: Promise<HelloMessage> } | null = null;

  /**
   * Open the VWP connection, coalescing concurrent callers.
   *
   * A second call for the *same* origin joins the attempt already running instead of destroying
   * it. A call for a *different* origin is a deliberate change of engine, so it supersedes.
   */
  async connect(baseUrl = window.location.origin): Promise<HelloMessage> {
    const inFlight = this.#connectInFlight;
    if (inFlight && inFlight.url === baseUrl) return inFlight.promise;
    const promise = this.#connectOnce(baseUrl).finally(() => {
      if (this.#connectInFlight?.promise === promise) this.#connectInFlight = null;
    });
    this.#connectInFlight = { url: baseUrl, promise };
    return promise;
  }

  async #connectOnce(baseUrl: string): Promise<HelloMessage> {
    this.disconnect();
    const store = useStudio.getState();
    store.setConnection("connecting");
    this.#baseUrl = baseUrl;

    const client = new VwpClient({ url: baseUrl, compress: "none", autoReconnect: true });
    this.client = client;

    client.onState((state) => useStudio.getState().setConnection(state));
    client.onHello((hello) => this.handleHello(hello));
    client.onKeyframe(() => {
      this.#frameCounts.keyframe++;
      this.#lastSimTimeNs = Number(client.poses.simTimeNs);
      this.#dirty = true;
    });
    client.onDelta((delta) => this.#onDelta(delta));
    client.onTelemetry((t) => this.#onTelemetry(t));
    client.onEvent((e) => this.handleEvent(e));
    client.onMetric((m) => this.handleMetric(m));
    client.onProvenance((p) => this.#onProvenance(p));
    client.onWorldChunk((c) => this.#onWorldChunk(c));
    client.onStreamError((e) => this.#onStreamError(e));
    client.onBye((b) => this.#onBye(b));
    client.onDrop((d) => this.#log("warn", "stream", `backpressure drop: ${JSON.stringify(d.dropped ?? {})} — resync at ${String(d.resync_seq ?? "?")}`));
    client.onGap((g) => this.#log("warn", "stream", `seq gap: expected ${g.expected}, got ${g.received} (${g.missing} missing)`));
    client.on("protocolerror", (err) => this.#log("error", "protocol", `${err.code}: ${err.message}`));
    client.onRpcNotification("run.state", (p) => {
      useStudio.getState().setRun({ state: p.state, tNs: p.t_ns });
      this.#log("info", "run", `state → ${p.state}`);
    });
    client.onRpcNotification("log", (p) => this.#log(p.level === "error" ? "error" : p.level === "warn" ? "warn" : "info", p.target ?? "engine", p.message));
    client.onRpcNotification("view.changed", (p) => {
      if (typeof p.mode === "string" && (CAMERA_MODES as readonly string[]).includes(p.mode)) {
        useStudio.getState().setCameraMode(p.mode as CameraMode);
      }
    });
    client.onRpcNotification("validation", (p) => {
      useStudio.getState().setValidation({ valid: (p.errors ?? []).length === 0, errors: p.errors ?? [], warnings: p.warnings ?? [] });
    });

    // Both timers start *before* the handshake is awaited, and both survive it failing. A page
    // whose socket will not open can still reach the engine over HTTP, and the 2 s `run.status`
    // poll is what turns that into a sentence the user can act on instead of a dead Connect button.
    this.#storeTimer = setInterval(() => this.flushProjection(), 1000 / STORE_HZ);
    this.#statusTimer = setInterval(() => void this.refreshStatus(), 2000);
    void this.refreshStatus();

    const hello = await client.connect();

    // §6.12 — nothing is emitted on a channel until it is subscribed.
    try {
      const res = await client.request("events.set", { subscribe: [...DEFAULT_CHANNELS], max_events_per_step: 2000 });
      this.#log("info", "events", `subscribed ${res.subscribed.map((s) => s.channel).join(", ")}`);
    } catch (err) {
      this.#log("warn", "events", `events.set failed: ${errText(err)}`);
    }

    void this.refreshStatus();
    void this.refreshRpcMethods();
    void this.refreshScenario();
    void this.refreshOverlayCatalogue();
    return hello;
  }

  // ---------------------------------------------------------------------------------------------
  // The no-engine replay path (09-ui §7)
  // ---------------------------------------------------------------------------------------------

  /**
   * A recording open in this page, read by `crates/v2xw-wasm`, with no engine behind it.
   *
   * This is the path a reviewer uses to check somebody else's result: open the `.mcap`, scrub it,
   * read the poses. It drives the *main* viewport, not the comparison pane — a reviewer with a
   * recording and no server should get the Studio, not half of it.
   *
   * It coexists with a connection rather than replacing it: a connection can be open at the same
   * time (that is how a recording is compared against a live run), and the replay's poses are what
   * the viewer shows only while no stream is feeding it.
   */
  replay: LocalReplay | null = null;

  #replayBindings: ReplayBindings | null = null;
  /** True once a world has been adopted from a file rather than verified against a `Hello`. */
  #worldUnverified = false;

  /** Whether a local recording is open and indexed. */
  get replayOpen(): boolean {
    return this.replay !== null && this.replay.isOpen;
  }

  /**
   * Open a recording from a local file and show its first frame.
   *
   * Nothing is uploaded and nothing is started: the file is read into WebAssembly memory and the
   * chunk index is read from its footer (§7.3). The world is a separate question — §7.1 keeps it
   * out of a recording — so the scene stays empty until one is supplied by
   * {@link openLocalWorld} or by a connection.
   */
  async openLocalRecording(
    file: { name?: string; arrayBuffer(): Promise<ArrayBuffer> },
    location?: ReplayModuleLocation,
  ): Promise<void> {
    this.#log("info", "replay", `opening ${file.name ?? "recording"} with the WebAssembly reader`);
    // Held in a local: the `new LocalReplay()` between the `??=` and the use invalidates the
    // narrowing TypeScript would otherwise carry on the private field.
    const bindings = (this.#replayBindings ??= await loadReplayBindings(location));
    const replay = new LocalReplay();
    const span = await replay.openBlob(file, bindings);
    this.replay?.close();
    this.replay = replay;
    this.#log(
      "info",
      "replay",
      `${file.name ?? "recording"}: ${span.startNs} … ${span.endNs} ns of simulated time`,
    );
    // The viewer is fed from the replay's own pose buffer while a recording drives the view, so the
    // live stream must not also be writing into the interpolator.
    this.#detachViewer?.();
    this.#detachViewer = null;
    await this.seekLocalReplay(span.startNs);
  }

  /**
   * Adopt a `vwp-world/1` payload from a local file, for a recording that has no engine to serve
   * one.
   *
   * There is no `Hello.world_hash` to check this against — §7.1 keeps `Hello` out of a recording —
   * so the §10.5 W3 comparison that {@link loadWorldPayload} performs has nothing to compare with.
   * Rather than skip the digest, it is computed and logged: the reviewer can check it against the
   * run manifest by eye, and the log line says in those words that the check was not automatic.
   */
  async openLocalWorld(file: { name?: string; arrayBuffer(): Promise<ArrayBuffer> }): Promise<boolean> {
    try {
      const payload = await file.arrayBuffer();
      const digest = await computeWorldContentHash(payload);
      const world = decodeWorld(payload);
      this.#adoptWorld(world, payload.byteLength);
      this.#worldUnverified = true;
      this.#log(
        "warn",
        "world",
        `adopted ${file.name ?? "world"} with payload digest ${digest} — a recording carries no ` +
          `Hello.world_hash (§7.1), so nothing verified this is the world the run was computed on`,
      );
      const replay = useStudio.getState().replay;
      if (replay !== null) useStudio.getState().setReplay({ ...replay, worldUnverified: true });
      return true;
    } catch (err) {
      this.#log("error", "world", `world file refused: ${errText(err)}`);
      return false;
    }
  }

  /**
   * Seek the local recording and put its state on screen.
   *
   * §7.3's seek reads one chunk and one keyframe period of deltas, which is why the scrub bar can
   * drive this directly rather than debouncing to a coarse grid.
   */
  async seekLocalReplay(tNs: number): Promise<void> {
    const replay = this.replay;
    if (replay === null || !replay.isOpen) return;
    const position = await replay.seekToNs(tNs);
    const viewer = this.viewer;
    if (viewer) {
      viewer.interpolator.reset();
      if (replay.signals) viewer.worldRenderer.applySignalKeyframe(replay.signals);
      // Twice: the interpolator samples between two snapshots, and one leaves it nothing to
      // interpolate from, so a seeked recording would render an empty scene until the next seek.
      viewer.capture(replay.poses);
      viewer.capture(replay.poses);
    }
    let live = 0;
    for (let slot = 0; slot < replay.poses.count; slot++) if (replay.poses.occupied[slot] === 1) live++;
    const span = replay.span;
    const store = useStudio.getState();
    store.setRun({
      state: "paused",
      tNs: position.tNs,
      tEndNs: span?.endNs ?? position.tNs,
      actors: live,
      runId: replay.label,
      live: false,
    });
    store.setTelemetry(null, null, position.tNs);
    store.setReplay({
      label: replay.label,
      startNs: span?.startNs ?? 0,
      endNs: span?.endNs ?? position.tNs,
      tNs: position.tNs,
      chunksRead: position.chunksRead,
      requests: position.requests,
      worldUnverified: this.#worldUnverified,
    });
    if (position.refused.length > 0) {
      this.#log(
        "warn",
        "replay",
        `${position.refused.length} of ${position.deltas} recorded deltas were refused: ` +
          `${position.refused.map((r) => (r.applied ? "applied" : r.reason)).join(", ")}`,
      );
    }
    this.#dirty = true;
  }

  /** Close the local recording. The last frame stays on screen. */
  closeLocalReplay(): void {
    this.replay?.close();
    this.replay = null;
    useStudio.getState().setReplay(null);
    // Hand the viewer back to the stream, if one is open.
    this.attachViewer();
  }

  /** Tear the connection down; the viewer stays mounted. */
  disconnect(): void {
    this.#connectInFlight = null;
    if (this.#storeTimer) clearInterval(this.#storeTimer);
    if (this.#statusTimer) clearInterval(this.#statusTimer);
    this.#storeTimer = null;
    this.#statusTimer = null;
    this.#detachViewer?.();
    this.#detachViewer = null;
    this.client?.close(1000, "studio closed");
    this.client = null;
    this.#followedNode = null;
    this.telemetry.clear();
    this.spark.reset();
    this.metrics.reset();
    this.provenance.clear();
    this.dims.clear();
    this.nodes.clear();
    this.nodeByActor.clear();
    this.#metricProv.clear();
    this.#metricDims.clear();
    this.#metricProjectionDirty = false;
    this.#pendingMarks = [];
    this.#pendingPseudonyms.length = 0;
    this.#pendingLinkCount = 0;
  }

  /**
   * Typed JSON-RPC (§6), over whichever transport is up.
   *
   * Two transports, one method surface. §6.2 serves the same methods over `POST /rpc`, and routing
   * *every* call that way when the socket is down is not a convenience, it is what makes a finished
   * run legible: the server hangs up on a run that has ended, so by the time the page renders, the
   * socket each panel's call went through is gone. That is why the Studio showed a scenario form
   * with sixteen empty fields, no metric catalogue, no overlay catalogue and no run status — five
   * separate "the engine published nothing" messages for one closed socket, when every one of those
   * answers was available over HTTP the whole time.
   *
   * {@link CONNECTION_SCOPED} is the exception, and it is the whole exception: those three act on a
   * connection's own view and are refused over HTTP with −32009. Rather than send a call that
   * cannot succeed, this says what is missing.
   */
  async request<M extends VwpMethodName>(method: M, params: ParamsOf<M>): Promise<ResultOf<M>> {
    const client = this.client;
    if (this.streaming && client) {
      useStudio.getState().noteRpcCall(method);
      try {
        return await client.request(method, params);
      } catch (err) {
        this.#log("error", "rpc", `${method}: ${errText(err)}`);
        throw err;
      }
    }
    if (CONNECTION_SCOPED.includes(method)) {
      const err = new Error(`${method} needs an open stream — press Connect first`);
      this.#log("warn", "rpc", err.message);
      throw err;
    }
    try {
      return await this.requestHttp(method, params);
    } catch (err) {
      this.#log("error", "rpc", `${method} over HTTP: ${errText(err)}`);
      throw err;
    }
  }

  /**
   * Reopen the stream on the target this engine last connected to.
   *
   * Separate from {@link connect} so a panel can reconnect without re-resolving the target: the
   * probe belongs to the app shell, which owns `?engine=` and the candidate list.
   */
  async reopenStream(): Promise<void> {
    await this.connect(this.#baseUrl === "" ? window.location.origin : this.#baseUrl);
    this.attachViewer();
  }

  // ---------------------------------------------------------------------------------------------
  // Viewer
  // ---------------------------------------------------------------------------------------------

  /** Create (or re-mount) the viewer on a canvas and wire the stream into it. */
  mountViewer(canvas: HTMLCanvasElement, theme: ThemeName): Viewer {
    if (!this.viewer) {
      this.viewer = new Viewer({
        theme: studioTheme(theme),
        timeOfDay: 11,
        maxActors: 20_000,
        autoStart: true,
      });
    }
    this.viewer.mount(canvas);
    this.viewer.setTheme(studioTheme(theme));
    if (this.world) this.viewer.setWorld(this.world);
    if (this.client) {
      this.#detachViewer?.();
      this.#detachViewer = this.viewer.attachClient(this.client);
      const hello = this.client.hello;
      if (hello) this.viewer.applyHello(hello);
      // The stream may already be running: seed the interpolator with what is in the pose buffer.
      if (this.client.poses.hasKeyframe) this.viewer.capture(this.client.poses);
    }
    for (const name of DEFAULT_OVERLAYS) this.viewer.overlays.set(name, true);
    this.viewer.overlays.setOpacity("tx_pulses", 0.3);
    this.viewer.overlays.setOpacity("links", 0.6);
    this.#applyModeOverlays(this.viewer.cameras.mode);
    useStudio.getState().setOverlays(this.viewer.overlays.states());
    return this.viewer;
  }

  /**
   * Attach the live stream to an already-mounted viewer. React mounts the viewport before the
   * connection opens, so this runs again once `Hello` has landed — and it re-captures the pose
   * buffer, because a keyframe that arrived before the wiring existed is the only one a paused run
   * will ever send (§1.3 rule 4), and without the capture the scene would stay empty.
   */
  attachViewer(): void {
    if (!this.viewer || !this.client) return;
    this.#detachViewer?.();
    this.#detachViewer = this.viewer.attachClient(this.client);
    const hello = this.client.hello;
    if (hello) this.viewer.applyHello(hello);
    if (this.world) this.viewer.setWorld(this.world);
    if (this.client.poses.hasKeyframe) this.viewer.capture(this.client.poses);
  }

  /** The overlay catalogue the viewer can actually draw (09-ui §6), in `overlay.set {list}` shape. */
  overlayCatalogue(): OverlayEntry[] {
    return this.viewer?.overlays.catalogue() ?? [];
  }

  /**
   * The plan view is a plan view: extruded building volumes hide the street grid, the lane markings
   * and the actors when the camera looks straight down, so `buildings` is off in `map` mode and on
   * in every 3D mode — until the user touches the toggle, after which their choice sticks.
   */
  #applyModeOverlays(mode: CameraMode): void {
    if (this.#buildingsUserSet || !this.viewer) return;
    this.viewer.overlays.set("buildings", mode !== "map");
    useStudio.getState().setOverlays(this.viewer.overlays.states());
  }

  /** Toggle one overlay locally and mirror it to the engine (§6.7). */
  async setOverlay(name: OverlayName, enabled: boolean): Promise<void> {
    if (name === "buildings") this.#buildingsUserSet = true;
    const viewer = this.viewer;
    const applied = viewer ? viewer.overlays.set(name, enabled) : enabled;
    useStudio.getState().setOverlays(viewer?.overlays.states() ?? { [name]: applied });
    if (!this.client) return;
    try {
      await this.request("overlay.set", { overlays: { [name]: applied } });
    } catch {
      /* the log line is written by request() */
    }
  }

  /** Lock every `*_gt` overlay off — blind evaluation (09-ui §6). */
  lockGroundTruth(locked: boolean): void {
    this.viewer?.overlays.lockGroundTruth(locked);
    useStudio.getState().setOverlays(this.viewer?.overlays.states() ?? {});
    useStudio.getState().setGroundTruthLocked(locked);
  }

  /**
   * Camera mode, mirrored to the engine so a copilot sees it (§6.7 `view.camera`).
   *
   * The viewer refuses a street-level mode it cannot draw — `chase` and `dashboard` need a vehicle
   * to sit behind, and with none in the stream they used to give a view of an arbitrary city block
   * with no car in it. It adopts the nearest vehicle where it can; where it cannot, the mode it
   * applied is not the one that was asked for, and the chip, the overlays and the engine are all
   * told the truth rather than the request.
   */
  setCameraMode(mode: CameraMode): void {
    const applied = this.viewer?.setCameraMode(mode) ?? mode;
    if (applied !== mode) {
      this.#log(
        "warn",
        "camera",
        `${mode} needs a vehicle to follow and the stream has none, so the view stayed on the ${applied}. Click a vehicle first.`,
      );
    }
    this.#applyModeOverlays(applied);
    useStudio.getState().setCameraMode(applied);
    // The viewer may have adopted a vehicle to make the mode possible; keep the follow chip and
    // the telemetry subscription in step with what the camera is actually on.
    const adopted = this.viewer?.cameras.followActorId ?? null;
    if (adopted !== null && useStudio.getState().selectedActor !== adopted) {
      void this.selectActor(adopted, applied);
      return;
    }
    const state = this.viewer?.cameras.state();
    if (!this.client || !state) return;
    void this.request("view.camera", {
      mode: applied,
      position: state.position,
      target: state.target,
      fov_deg: state.fovDeg,
    }).catch(() => undefined);
  }

  /**
   * Select an actor: fly the camera down to it (09-ui §3) and subscribe its node to `Telemetry`
   * (§6.7 — "`view.follow` is the subscription control for `Telemetry`").
   */
  async selectActor(actorId: number | null, mode: CameraMode = "chase"): Promise<void> {
    const store = useStudio.getState();
    if (actorId === null) {
      this.viewer?.select(null);
      this.viewer?.cameras.follow(null);
      this.#followedNode = null;
      store.setSelection(null, null);
      if (this.client) await this.request("view.follow", { clear: true }).catch(() => undefined);
      await this.setFollowChannels(false);
      return;
    }
    const applied = this.viewer?.flyTo(actorId, mode) ?? mode;
    this.#applyModeOverlays(applied);
    useStudio.getState().setCameraMode(applied);
    const nodeId = this.nodeByActor.get(actorId) ?? null;
    this.#followedNode = nodeId;
    this.spark.reset();
    store.setSelection(actorId, nodeId);
    if (!this.client) return;
    try {
      const res = await this.request("view.follow", { actor: actorId, ...(nodeId !== null ? { node: nodeId } : {}), camera: mode, telemetry: true });
      if (typeof res.following === "number") {
        this.#followedNode = res.following;
        useStudio.getState().setSelection(actorId, res.following);
      }
    } catch {
      /* logged by request() */
    }
    await this.setFollowChannels(true);
    void this.inspectFollowed();
  }

  /**
   * Subscribe or drop the high-rate event channels of §6.12.
   *
   * §6.12's decision note says the Studio "subscribes the high-rate channels only for the followed
   * node", and this is as close to that as VWP v1 lets a client get: the *channels* are subscribed
   * only while something is followed, but the node restriction is deliberately **not** applied.
   * `events.set`'s `filter.nodes` is connection-scoped, not per-channel — `Session::set_events` in
   * `crates/v2xw-server` keeps one `event_nodes` set for the whole connection — so filtering to the
   * followed node would also silence `det.observation`, `proto.revocation` and `app.warning` for
   * every other node, and those three are what the scrub bar's event markers are made of. The rate
   * is bounded by `max_events_per_step` instead, which §6.12 defines as a deterministic sample and
   * is the mechanism that actually protects the connection.
   */
  async setFollowChannels(enabled: boolean): Promise<void> {
    if (!this.client) return;
    try {
      await this.request(
        "events.set",
        enabled
          ? { subscribe: [...FOLLOW_CHANNELS], max_events_per_step: 2000 }
          : { unsubscribe: [...FOLLOW_CHANNELS] },
      );
    } catch {
      /* logged by request(); the low-rate subscription is unaffected either way */
    }
  }

  /** Follow a node that has no actor (an RSU site picked in the viewport). */
  async selectNode(nodeId: number): Promise<void> {
    this.#followedNode = nodeId;
    this.spark.reset();
    this.viewer?.cameras.setMode("rsu");
    this.#applyModeOverlays("rsu");
    useStudio.getState().setCameraMode("rsu");
    useStudio.getState().setSelection(null, nodeId);
    if (!this.client) return;
    await this.request("view.follow", { node: nodeId, telemetry: true }).catch(() => undefined);
    await this.setFollowChannels(true);
    void this.inspectFollowed();
  }

  /** §6.8 — pull the inspector payload for the followed node. */
  async inspectFollowed(): Promise<void> {
    const node = this.#followedNode;
    if (node === null || !this.client) return;
    try {
      const res = await this.request("inspect.node", {
        node,
        include: ["telemetry", "stores", "queues", "neighbors", "certs", "crl", "provenance"],
        limit: 50,
      });
      useStudio.getState().setInspect(res);
    } catch {
      useStudio.getState().setInspect(null);
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Run control (§6.6)
  // ---------------------------------------------------------------------------------------------

  /**
   * One JSON-RPC call over HTTP `POST /rpc`, for when there is no stream to carry it.
   *
   * The engine answers the same method set over HTTP as over the socket, and that is the whole
   * point of this method: after a run ends the engine closes the stream, and a page that could only
   * talk over the socket had no way to ask what had happened or to start another run. It showed the
   * word "closed" and a Connect button instead. With this, "Run again" works from a closed page.
   */
  async requestHttp<M extends VwpMethodName>(
    method: M,
    params: ParamsOf<M>,
    options: { readonly quiet?: boolean } = {},
  ): Promise<ResultOf<M>> {
    const base = this.#baseUrl === "" ? window.location.origin : this.#baseUrl;
    // The 2 s status poll runs through here whenever the socket is down, and a poll is not a call
    // the user made: logging it would bury every real call in the Calls-made list.
    if (options.quiet !== true) useStudio.getState().noteRpcCall(method);
    const res = await fetch(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: Date.now(), method, params }),
    });
    if (!res.ok) throw new Error(`the engine answered ${res.status} for ${method}`);
    const body = (await res.json()) as { result?: unknown; error?: { code?: number; message?: string } };
    if (body.error) throw new Error(body.error.message ?? `error ${String(body.error.code)} from ${method}`);
    return body.result as ResultOf<M>;
  }

  /**
   * Whether the socket is up and carrying frames.
   *
   * The distinction the interface hangs on: a client object exists long after its socket has gone,
   * so `this.client !== null` is not "connected".
   */
  get streaming(): boolean {
    return this.client !== null && this.client.state === "streaming";
  }

  /**
   * Start, or rewind and start again — one coherent action whatever state the run is in.
   *
   * `run.start` on its own is not enough, because each of the engine's refusals is a state check
   * and the sequence has to satisfy all of them:
   *
   *  1. **Pause first if the run is going.** `run.start` on a running run is refused outright
   *     (`RunAlreadyRunning`), so "Run again" mid-run would fail with a JSON-RPC error.
   *  2. **`run.start {paused: true}`**, which rewinds — `LiveEngine::restart` clears the timeline,
   *     the history and the cursor, and deliberately keeps the run id and the symbol table — and
   *     leaves the clock at zero rather than moving.
   *  3. **Reopen the stream if it is down**, which it is in the case this exists for: a run that
   *     ended closed its socket. Asking for `paused` in step 2 is what makes this safe. Started
   *     running, a run at high speed (or at `speed: 0`, unthrottled) can reach its end before the
   *     socket is back, and the user would press "Run again" and be handed another finished run —
   *     the original complaint, reproduced by its own fix.
   *  4. **`run.resume`**, legal now because the run is paused, and only now with a viewer attached
   *     to receive the first keyframe.
   *
   * Returns the state the engine reports at the end, so a caller can say what happened.
   */
  async startRun(): Promise<string> {
    if (useStudio.getState().run.state === "running") await this.request("run.pause", {});
    const speed = useStudio.getState().run.speed;
    await this.request("run.start", { paused: true, ...(Number.isFinite(speed) ? { speed } : {}) });
    if (!this.streaming) await this.reopenStream();
    const res = await this.request("run.resume", {});
    await this.refreshStatus();
    return res.state;
  }

  /**
   * `run.status`, polled so the scrub bar has a bound even while paused.
   *
   * Falls back to HTTP whenever the socket is not streaming. Without that fallback the run state
   * froze at whatever it was when the stream died, which is how a finished run came to be reported
   * as a closed socket and nothing else: the engine was answering `state: "finished"` the whole
   * time, over a transport the page was not using.
   */
  async refreshStatus(): Promise<void> {
    try {
      const s = this.streaming && this.client
        ? await this.client.request("run.status", {})
        : await this.requestHttp("run.status", {}, { quiet: true });
      useStudio.getState().setRun({
        state: s.state,
        tNs: s.t_ns,
        tEndNs: s.t_end_ns,
        speed: s.speed,
        actors: s.actors ?? 0,
        nodes: s.nodes ?? 0,
        runId: s.run_id,
        profile: s.profile,
        live: s.live,
      });
    } catch {
      /* a poll failure is not worth a log line */
    }
  }

  /** §6.15 — the tool surface for the copilot panel, straight from the engine. */
  async refreshRpcMethods(): Promise<void> {
    try {
      const doc = await this.request("rpc.discover", {});
      const methods = (doc.methods ?? [])
        .map((m) => ({
          name: String((m as { name?: unknown }).name ?? ""),
          summary: typeof (m as { summary?: unknown }).summary === "string" ? (m as { summary: string }).summary : "",
        }))
        .filter((m) => m.name !== "");
      useStudio.getState().setRpcMethods(methods, String((doc.info as { title?: unknown } | undefined)?.title ?? "engine"));
    } catch {
      useStudio.getState().setRpcMethods([], "unavailable");
    }
  }

  /** §6.10 — the scenario document and, when the engine publishes one, its JSON Schema. */
  async refreshScenario(): Promise<void> {
    try {
      const res = await this.request("scenario.get", { with_schema: true, resolved: true });
      useStudio.getState().setScenario(res.scenario, res.hash, res.schema ?? null);
    } catch (err) {
      this.#log("warn", "scenario", `scenario.get failed: ${errText(err)}`);
    }
    try {
      const list = await this.request("scenario.list", { kind: "all", limit: 100 });
      useStudio.getState().setScenarioList(list.items ?? []);
    } catch {
      useStudio.getState().setScenarioList([]);
    }
  }

  /** §6.7 — the engine's overlay catalogue, merged with what this build can draw. */
  async refreshOverlayCatalogue(): Promise<void> {
    if (!this.client) return;
    try {
      const res = await this.client.request("overlay.set", { list: true });
      useStudio.getState().setServerOverlays(res.catalogue ?? []);
    } catch {
      useStudio.getState().setServerOverlays([]);
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Stream handlers
  // ---------------------------------------------------------------------------------------------

  /**
   * §3.1 — a `Hello`: the node table, the class and channel tables, and the world reference.
   *
   * Public for the same reason as {@link handleEvent} — the only other way to reach it is a live
   * socket, and `test/projection.test.ts` drives the world-verification path through it.
   */
  handleHello(hello: HelloMessage): void {
    const strings = this.client?.strings;
    // §10.5 W3 — every world this run adopts must hash to this, whether it arrives over HTTP or as
    // §3.9 chunks. Recorded before either path can start.
    this.#promisedWorldHash = bytesToHex(hello.worldHash);
    this.nodes.clear();
    this.nodeByActor.clear();
    // `setHello` empties the timeline, so anything still queued belongs to the previous run.
    this.#pendingMarks = [];
    this.#pendingPseudonyms.length = 0;
    const n = hello.nodes;
    for (let i = 0; i < n.count; i++) {
      const nodeId = n.nodeId[i];
      const actorId = n.actorId[i];
      const info: NodeInfo = {
        nodeId,
        actorId: actorId === 0xffffffff ? null : actorId,
        label: strings?.get(n.strLabel[i]) ?? "",
        profileId: strings?.get(n.strProfileId[i]) ?? "",
        kind: n.kind[i],
        flags: n.flags[i],
        classIdx: n.classIdx[i] === 0xff ? null : n.classIdx[i],
        x: n.posXM[i],
        y: n.posYM[i],
        z: n.posZM[i],
      };
      this.nodes.set(nodeId, info);
      if (info.actorId !== null) this.nodeByActor.set(info.actorId, nodeId);
    }

    const classNames: string[] = [];
    for (let i = 0; i < hello.classes.count; i++) classNames.push(strings?.get(hello.classes.strName[i]) ?? `class ${i}`);
    const channels: { name: string; id: number; visibility: number; enabled: boolean }[] = [];
    for (let i = 0; i < hello.channels.count; i++) {
      channels.push({
        name: strings?.get(hello.channels.strId[i]) ?? "",
        id: hello.channels.channelId[i],
        visibility: hello.channels.visibility[i],
        enabled: hello.channels.enabled[i] === 1,
      });
    }

    useStudio.getState().setHello({
      runId: formatUuid(hello.runId),
      engineVersion: hello.engineVersion,
      scenarioName: hello.scenarioName,
      runLabel: hello.runLabel,
      worldHash: bytesToHex(hello.worldHash),
      scenarioHash: bytesToHex(hello.scenarioHash),
      flags: hello.helloFlags,
      simDurationNs: Number(hello.simDurationNs),
      mobilityStepNs: Number(hello.mobilityStepNs),
      keyframePeriodNs: Number(hello.keyframePeriodNs),
      telemetryPeriodNs: Number(hello.telemetryPeriodNs),
      actorCapacity: hello.actorCapacity,
      nodeCount: hello.nodes.count,
      classNames,
      channels,
      origin: { lat: hello.originLatDeg, lon: hello.originLonDeg, alt: hello.originAltM },
      bbox: { minX: hello.bboxMinXM, minY: hello.bboxMinYM, maxX: hello.bboxMaxXM, maxY: hello.bboxMaxYM },
      versionMajor: hello.versionMajor,
      versionMinor: hello.versionMinor,
    });

    this.#log("info", "vwp", `Hello v${hello.versionMajor}.${hello.versionMinor} from ${hello.engineVersion}: ${hello.nodes.count} nodes, ${hello.classes.count} classes`);

    // §3.1.6 — mode 0 fetches the world over HTTP by content hash; mode 1 streams WorldChunks.
    if (hello.worldRef.mode === 0) {
      const url = strings?.get(hello.worldRef.strUrl) ?? "";
      if (url !== "") void this.#fetchWorld(url);
    } else if (hello.worldRef.mode === 1) {
      this.#worldChunks = [];
      this.#worldChunkBytes = 0;
    }
  }

  /**
   * Fetch the world payload named by `Hello.world_ref` (§3.1.6 mode 0).
   *
   * The URL is resolved against the base the connection was opened on, because §3.1.6's URL is
   * root-relative and assumes the engine and the page are one origin. They are behind the proxy and
   * in the deployed build; they are not when the engine is pinned to another port, and a bare
   * `fetch("/world/…")` then asks the page's own origin for a world it has never heard of. Both
   * servers also set `cross-origin-resource-policy: same-origin` on every response (§1.1), so a
   * genuinely cross-origin fetch is refused by the browser however the URL is written — that case
   * is reported by `lib/target.ts` up front rather than as a mystery here.
   */
  async #fetchWorld(path: string): Promise<void> {
    const url = resolveEngineUrl(this.#baseUrl, path);
    try {
      const res = await fetch(url, { cache: "force-cache" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      await this.loadWorldPayload(await res.arrayBuffer(), url);
    } catch (err) {
      this.#log("error", "world", `world fetch failed (${url}): ${errText(err)}`);
    }
  }

  /**
   * §10.5 W3 — verify served world bytes against `Hello.world_hash`, then adopt them.
   *
   * The §4.2 payload digest is recomputed over the bytes that actually arrived and compared with
   * what `Hello` promised (§3.1.1) before anything is decoded, so a world this run was not computed
   * against is refused rather than silently rendered. A mismatch is a typed `ProtocolError`
   * (`hash_mismatch`), reported on the `world` log target and returned as `false`; the previously
   * adopted world, if any, is left alone.
   *
   * Both delivery paths land here: `world_ref.mode = 0` over HTTP and the `WorldChunk` stream of
   * §3.9, whose concatenated payloads §3.9 defines as the same bytes.
   */
  async loadWorldPayload(payload: ArrayBuffer, source: string): Promise<boolean> {
    const promised = this.#promisedWorldHash;
    if (promised === null) {
      this.#log("error", "world", `world payload from ${source} arrived before Hello; refused (§10.5 W3)`);
      return false;
    }
    try {
      const digest = await verifyWorldPayload(payload, promised);
      this.#adoptWorld(decodeWorld(payload), payload.byteLength);
      this.#log("info", "world", `world ${shortDigest(digest)} verified against Hello.world_hash (§10.5 W3)`);
      return true;
    } catch (err) {
      const why = err instanceof ProtocolError ? `${err.code}: ${err.message}` : errText(err);
      this.#log("error", "world", `world refused (${source}): ${why}`);
      return false;
    }
  }

  #adoptWorld(world: VwpWorld, sourceBytes: number): void {
    this.world = world;
    const viewer = this.viewer;
    if (viewer) {
      viewer.setWorld(world);
      const extent = Math.max(world.bbox.maxXM - world.bbox.minXM, world.bbox.maxYM - world.bbox.minYM);
      viewer.cameras.fitExtent(Math.min(extent * 1.05, MAP_OPEN_EXTENT_M));
      if (viewer.cameras.mode === "map") viewer.cameras.snap();
      this.#applyModeOverlays(viewer.cameras.mode);
    }
    const report = this.viewer?.worldRenderer.report;
    useStudio.getState().setWorldSummary({
      lanes: world.lanes.count,
      buildings: world.buildings.count,
      junctions: world.junctions.count,
      signals: world.signals.count,
      sites: world.sites.count,
      crossings: world.crossings.count,
      landuse: world.landuse.count,
      bytes: sourceBytes,
      buildMs: report?.buildMs ?? 0,
      buildingBackend: report?.buildingBackend ?? "none",
      drawables: report?.drawables ?? 0,
    });
    this.#log("info", "world", `world ready: ${world.lanes.count} lanes, ${world.buildings.count} buildings, ${world.sites.count} sites`);
    if (this.viewer) {
      for (const name of DEFAULT_OVERLAYS) this.viewer.overlays.set(name, true);
      this.#applyModeOverlays(this.viewer.cameras.mode);
      useStudio.getState().setOverlays(this.viewer.overlays.states());
    }
  }

  #onDelta(delta: DeltaMessage): void {
    this.#frameCounts.delta++;
    this.#lastSimTimeNs = Number(delta.simTimeNs);
    this.#dirty = true;
  }

  #onTelemetry(msg: TelemetryMessage): void {
    this.#frameCounts.telemetry++;
    for (let i = 0; i < msg.nodeCount; i++) {
      const rec = msg.record(i);
      this.telemetry.set(rec.nodeId, rec);
      if (rec.nodeId === this.#followedNode) {
        this.spark.push(
          Number(msg.simTimeNs) / 1e9,
          SPARKLINE_SERIES.map((s) => s.pick(rec)),
        );
      }
    }
    this.#dirty = true;
  }

  /**
   * §3.6 — one `Event` batch: the overlays it feeds, and the timeline marks it contributes.
   *
   * Public because it is the stream projection, and the only other way to reach it is a live
   * socket; `test/projection.test.ts` delivers synthetic batches through it.
   *
   * Which overlays are on is decided *before* the loop. Building 1,500 link records and 4,000
   * position tuples and then discovering at the end that the `links` overlay is off — which it is
   * unless the user switches it on — was ~5,500 objects of garbage per frame for nothing.
   */
  handleEvent(msg: EventMessage): void {
    this.#frameCounts.event++;
    const viewer = this.viewer;
    const followed = this.#followedNode;
    const wantLinks = viewer?.overlays.isEnabled("links") ?? false;
    const wantPulses = viewer?.overlays.isEnabled("tx_pulses") ?? false;
    // `node.tx` still has to be decoded when a node is followed: that is where its pseudonym
    // digest comes from (§3.6.4).
    const wantNodeTx = wantPulses || followed !== null;
    this.#pendingLinkCount = 0;
    let txSeen = 0;
    let txDrawn = 0;
    const txStride = Math.max(1, Math.ceil(msg.count / PULSE_BUDGET));

    for (let i = 0; i < msg.count; i++) {
      const channelId = msg.index.channelId[i];
      const tNs = Number(msg.index.simTimeNs[i]);

      // The overlays that are fed from the event stream (09-ui §6).
      if (channelId === ChannelId.NODE_TX) {
        if (!wantNodeTx) continue;
        const p = msg.payload(i);
        if (p.channel === "node.tx") {
          if (wantPulses && viewer) {
            const sample = txSeen++ % txStride === 0 && txDrawn < PULSE_BUDGET;
            if (sample && this.#nodePositionInto(p.nodeId, this.#posA)) {
              const pos = this.#posA;
              viewer.overlays.pulses.emit(pos[0], pos[1], pos[2] + 1.2, PULSE_RADIUS_M, viewer.renderClockSeconds);
              txDrawn++;
            }
          }
          if (p.nodeId === followed) {
            this.#pendingPseudonyms.push({ digest: hex(p.pseudonymDigest), i: null, j: null, source: "node.tx" });
          }
        }
      } else if (channelId === ChannelId.PHY_RX) {
        if (!wantLinks || this.#pendingLinkCount >= LINK_BUDGET) continue;
        const p = msg.payload(i);
        if (p.channel === "phy.rx" && p.outcome === 0) {
          const a = this.#posA;
          const b = this.#posB;
          if (this.#nodePositionInto(p.txNode, a) && this.#nodePositionInto(p.rxNode, b)) {
            const l = this.#linkSlot();
            l.ax = a[0]; l.ay = a[1]; l.az = a[2] + 1;
            l.bx = b[0]; l.by = b[1]; l.bz = b[2] + 1;
            l.gt = false; l.strength = 1;
          }
        }
      } else if (channelId === ChannelId.SEC_CERT) {
        const p = msg.payload(i);
        if (p.channel === "sec.cert" && p.nodeId === followed) {
          this.#pendingPseudonyms.push({ digest: hex(p.digest), i: p.indexI, j: p.indexJ, source: "sec.cert" });
          this.#mark({ tNs, channel: "sec.cert", nodeId: p.nodeId, label: `pseudonym ${shortDigest(hex(p.digest))}` });
        }
      } else if (channelId === ChannelId.DET_OBSERVATION) {
        const p = msg.payload(i);
        if (p.channel === "det.observation") {
          this.#mark({ tNs, channel: "det.observation", nodeId: p.nodeId, label: `detection score ${p.score.toFixed(2)}`, provId: p.provId });
        }
      } else if (channelId === ChannelId.PROTO_REVOCATION) {
        const p = msg.payload(i);
        if (p.channel === "proto.revocation") {
          this.#mark({ tNs, channel: "proto.revocation", nodeId: p.nodeId, label: `revocation stage ${p.stage}` });
        }
      } else if (channelId === ChannelId.APP_WARNING) {
        const p = msg.payload(i);
        if (p.channel === "app.warning") {
          this.#mark({ tNs, channel: "app.warning", nodeId: p.nodeId, label: `warning (ttc ${p.ttcS.toFixed(1)} s)` });
        }
      }
    }

    if (viewer && this.#pendingLinkCount > 0) {
      viewer.overlays.links.begin();
      for (let i = 0; i < this.#pendingLinkCount; i++) {
        const l = this.#pendingLinks[i];
        viewer.overlays.links.add(l.ax, l.ay, l.az, l.bx, l.by, l.bz, l.gt, l.strength);
      }
      viewer.overlays.links.end();
    }
    this.#dirty = true;
  }

  /**
   * §3.7 — one `MetricSample` frame into the metric history and the provenance projection.
   *
   * Public for the same reason as {@link handleEvent}. Nothing here touches the React store: a
   * per-node or per-link metric carries a different `dim_key` on every sample, so writing
   * `metricDims` from here rewrote the store thousands of times a second to end up with a single
   * key. The maps are published by `flushProjection()`, once, and only when they changed.
   */
  handleMetric(msg: MetricSampleMessage): void {
    this.#frameCounts.metric++;
    const strings = this.client?.strings;
    const t = Number(msg.simTimeNs) / 1e9;
    for (let i = 0; i < msg.sampleCount; i++) {
      const s = msg.sample(i);
      const name = strings?.get(s.strMetric) ?? `metric ${s.strMetric}`;
      if (name === "") continue;
      this.metrics.push(name, t, s.value);
      if (s.provId !== 0 && this.#metricProv.get(name) !== s.provId) {
        this.#noteMetric(this.#metricProv, name, s.provId);
      }
      // §3.8 — `dim_key` indexes the dimension dictionary the Provenance frame carries.
      if (s.dimKey !== 0) {
        const dims = this.dims.get(s.dimKey);
        if (dims !== undefined && dims !== "" && this.#metricDims.get(name) !== dims) {
          this.#noteMetric(this.#metricDims, name, dims);
        }
      }
    }
    this.#dirty = true;
  }

  /** Record one metric attribute, evicting the oldest name once the ceiling is reached. */
  #noteMetric<V>(map: Map<string, V>, name: string, value: V): void {
    if (!map.has(name) && map.size >= METRIC_KEY_LIMIT) {
      const oldest = map.keys().next().value;
      if (oldest !== undefined) map.delete(oldest);
    }
    map.set(name, value);
    this.#metricProjectionDirty = true;
  }

  /**
   * Queue one scrub-bar mark, bounded by what the store would keep anyway (`MAX_MARKS`).
   *
   * The trim drops the oldest half in one `slice` rather than `shift()`-ing per push, so the cost
   * stays amortised O(1) even if a whole flush interval's worth of `det.observation` events lands
   * at the configured `max_events_per_step`.
   */
  #mark(m: TimelineMark): void {
    this.#pendingMarks.push(m);
    if (this.#pendingMarks.length > MAX_MARKS * 2) {
      this.#pendingMarks = this.#pendingMarks.slice(-MAX_MARKS);
    }
  }

  /** The next pooled link record, appending only while the pool is still growing. */
  #linkSlot(): LinkRecord {
    const i = this.#pendingLinkCount++;
    let l = this.#pendingLinks[i];
    if (l === undefined) {
      l = { ax: 0, ay: 0, az: 0, bx: 0, by: 0, bz: 0, gt: false, strength: 1 };
      this.#pendingLinks[i] = l;
    }
    return l;
  }

  #onProvenance(msg: ProvenanceMessage): void {
    const strings = this.client?.strings;
    if ((msg.flags & 0x1) !== 0) this.provenance.clear();
    for (let i = 0; i < msg.entries.count; i++) {
      this.provenance.set(msg.entries.provId[i], {
        provId: msg.entries.provId[i],
        modelId: strings?.get(msg.entries.strModelId[i]) ?? "",
        modelVersion: strings?.get(msg.entries.strModelVersion[i]) ?? "",
        paramSetId: strings?.get(msg.entries.strParamSetId[i]) ?? "",
        cardUrl: strings?.get(msg.entries.strCardUrl[i]) ?? "",
        family: msg.entries.family[i],
        subjectKind: msg.entries.subjectKind[i],
      });
    }
    for (let i = 0; i < msg.dims.count; i++) {
      this.dims.set(msg.dims.dimKey[i], strings?.get(msg.dims.strDims[i]) ?? "");
    }
    useStudio.getState().setProvenanceCount(this.provenance.size);
    this.#log("info", "provenance", `${msg.entries.count} entries, ${msg.dims.count} dimension keys`);
  }

  /**
   * §3.9 — the world arrives inline when `Hello.world_ref.mode = 1` (static/WASM hosting, no engine
   * HTTP server). Chunks are contiguous and all but the last carry `FLAG_CONTINUED`.
   */
  #onWorldChunk(msg: WorldChunkMessage): void {
    if (msg.chunkIndex === 0) {
      this.#worldChunks = [];
      this.#worldChunkBytes = 0;
    }
    this.#worldChunks.push(msg.payload.slice());
    this.#worldChunkBytes += msg.payload.byteLength;
    if (msg.continued) return;
    const joined = new Uint8Array(this.#worldChunkBytes);
    let at = 0;
    for (const chunk of this.#worldChunks) {
      joined.set(chunk, at);
      at += chunk.byteLength;
    }
    this.#worldChunks = [];
    this.#worldChunkBytes = 0;
    // §3.9 — "the client MUST verify SHA-256(concat(payloads)) == world_hash and MUST discard the
    // world on mismatch", which is the same check §10.5 W3 asks of the HTTP path.
    void this.loadWorldPayload(joined.buffer as ArrayBuffer, `WorldChunk x${msg.chunkCount}`);
  }

  #onStreamError(msg: ErrorMessage): void {
    this.#log(msg.fatal ? "error" : "warn", "engine", `${msg.message}${msg.detail ? ` — ${msg.detail}` : ""} (code ${msg.code})`);
  }

  #onBye(msg: ByeMessage): void {
    // Appendix A's order, which is not the order this line used to assume: reason 0 is
    // RUN_COMPLETE and reason 3 is ERROR. Reversed, the log said "Bye: error" every time a run
    // ended normally — the one line that could have explained the closed stream, saying the
    // opposite of what happened.
    const reasons = ["run complete", "client requested stop", "server shutdown", "error", "superseded"];
    this.#log("info", "vwp", `Bye: ${reasons[msg.reason] ?? `reason ${msg.reason}`}${msg.detail ? ` — ${msg.detail}` : ""}`);
  }

  // ---------------------------------------------------------------------------------------------
  // Helpers
  // ---------------------------------------------------------------------------------------------

  /**
   * Where a node is right now: a mobile node from the live pose buffer, a static one from `Hello`.
   *
   * Fills a caller-supplied scratch buffer rather than returning a tuple — this runs twice per
   * `phy.rx` event, so up to 4,000 times per `Event` frame at the configured
   * `max_events_per_step`, and a fresh `[x, y, z]` each time is the one allocation the viewer
   * package's own hot paths go to some length to avoid.
   */
  #nodePositionInto(nodeId: number, out: Float64Array): boolean {
    const info = this.nodes.get(nodeId);
    if (!info) return false;
    if (info.actorId !== null && this.client) {
      const slot = this.client.slots.slotOf(info.actorId);
      if (slot === undefined) return false;
      const p = this.client.poses.positionOf(slot);
      out[0] = p.x;
      out[1] = p.y;
      out[2] = p.z;
      return true;
    }
    out[0] = info.x;
    out[1] = info.y;
    out[2] = info.z;
    return true;
  }

  /** Resolve a `prov_id` (§3.8) — the "why" tab's first stop, with no round trip. */
  resolveProvenance(provId: number): ProvEntry | null {
    return this.provenance.get(provId) ?? null;
  }

  /** The telemetry record of the followed node, or null. */
  followedTelemetry(): NodeTelemetry | null {
    if (this.#followedNode === null) return null;
    return this.telemetry.get(this.#followedNode) ?? null;
  }

  /** The node the connection is subscribed to (§6.7). */
  get followedNode(): number | null {
    return this.#followedNode;
  }

  #log(level: LogLine["level"], target: string, message: string): void {
    useStudio.getState().addLog({ level, target, message, at: nowMs() });
  }

  /**
   * Push the throttled projection of the hot state into the store — the 5 Hz beat of 09-ui §4.
   *
   * This is the *only* place the stream handlers are allowed to reach React. Every setter it calls
   * returns the previous state when the content is unchanged (see `store.ts`), so a flush with
   * nothing new behind it notifies once, for `bumpSeries`, which is the sparkline/plot redraw tick.
   */
  flushProjection(): void {
    if (!this.#dirty) return;
    this.#dirty = false;
    const store = useStudio.getState();
    const followed = this.#followedNode;
    const rec = followed === null ? null : this.telemetry.get(followed) ?? null;
    store.setTelemetry(rec, followed, this.#lastSimTimeNs);
    store.setFrameCounts({ ...this.#frameCounts });
    if (this.#pendingMarks.length > 0) {
      store.addTimelineMarks(this.#pendingMarks);
      this.#pendingMarks = [];
    }
    if (this.#pendingPseudonyms.length > 0) {
      // Replayed in arrival order: `sec.cert` carries the i/j indices a later `node.tx` with the
      // same digest must not overwrite, and the store's own rule handles that.
      for (const p of this.#pendingPseudonyms) store.notePseudonym(p);
      this.#pendingPseudonyms.length = 0;
    }
    if (this.#metricProjectionDirty) {
      this.#metricProjectionDirty = false;
      store.setMetricProjection(Object.fromEntries(this.#metricProv), Object.fromEntries(this.#metricDims));
    }
    const snap = this.viewer?.stats.snapshot();
    if (snap) {
      store.setStats({
        fps: snap.fps,
        fpsAverage: snap.fpsAverage,
        frameMs: snap.frameMs,
        p95Ms: snap.p95Ms,
        cpuMs: snap.cpuMs,
        drawCalls: snap.drawCalls,
        triangles: snap.triangles,
        actorInstances: snap.actorInstances,
        actorCulled: snap.actorCulled,
        actorLive: snap.actorLive,
        buildingsVisible: snap.buildingsVisible,
      });
    }
    store.bumpSeries();
  }
}

function errText(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}

/** The process-wide engine. */
export const engine = new StudioEngine();
