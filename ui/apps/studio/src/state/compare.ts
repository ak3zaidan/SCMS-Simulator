/**
 * The comparison view's imperative half — Phase 5's "two runs side by side".
 *
 * 09-ui §6 asks for "two to four runs side by side with synchronised time, difference overlays for
 * metrics, and manifest diff". This builds the two-run case, which is the one a researcher actually
 * uses: a run against a baseline, or two recordings.
 *
 * # Why side B is its own connection, not a second `run_id`
 *
 * The obvious route is `metrics.query {runs: [...]}` (§6.12) and `experiment.compare`. Neither is
 * available: §6.15 reserves `experiment.compare` for a later minor version, and the real engine's
 * `metrics_query` (`crates/v2xw-server/src/rpc.rs`) ignores the `runs` argument because a server
 * serves exactly one run — `run.status` with a foreign `run_id` is `-32000 RUN_NOT_FOUND` there.
 * That is not a gap to work around; it is the architecture. One run per server means a second run
 * is a second *source*, and the browser can hold two:
 *
 *  * `"engine"` — a second VWP connection, to another `v2xw-server` on another port. This side
 *    carries everything a run carries, metric samples included, so the difference view is real.
 *    (`ReplayEngine` would let that second server replay a recording, but `v2xw-server`'s binary
 *    has no `--replay` flag yet — `serve_replay` is library-only — so today this means a second
 *    live run.)
 *  * `"replay"` — a recording opened from a local file and read by `crates/v2xw-wasm`, with no
 *    server at all (`lib/replay.ts`). Poses and signals, because §7.1 is what a recording holds;
 *    the difference view says so rather than showing zeros.
 *
 * # Synchronised time
 *
 * One scrub drives both, on *simulated* time, through {@link CompareController.seekTo}: side A gets
 * `run.seek` (§6.6) and side B gets either its own `run.seek` or a WebAssembly seek, which §7.3
 * costs one chunk and one keyframe period of deltas. `offsetNs` aligns two runs that do not share
 * a `t0`; it is zero for two runs of the same scenario.
 *
 * # The camera
 *
 * Side B has its own `Viewer` — two canvases, two scenes — and {@link mirrorCameraFrom} copies A's
 * `CameraState` onto it. Copying the *state* rather than the camera object is what makes the two
 * views comparable when the worlds differ in extent: the mode, the look-at target, the altitude,
 * the chase distance and the bearing all transfer, and each viewer keeps its own aspect ratio.
 */

import {
  VwpClient,
  bytesToHex,
  decodeWorld,
  verifyWorldPayload,
  type HelloMessage,
  type MetricSampleMessage,
  type VwpWorld,
} from "@vwp/protocol";
import { Viewer } from "@vwp/viewer";

import { MetricHistory } from "../lib/history.js";
import { LocalReplay, loadReplayBindings, type ReplayBindings, type ReplayModuleLocation } from "../lib/replay.js";
import { normaliseBase, resolveEngineUrl } from "../lib/target.js";
import { studioTheme, type ThemeName } from "../lib/theme.js";
import { engine } from "./engine.js";
import { useStudio, type CompareSideView, type MetricDiff } from "./store.js";

/** How often side B's summary and the difference table are republished (09-ui §4's 5 Hz). */
const PUBLISH_HZ = 5;

/** How many metric rows the difference view will build when the user has picked none. */
const MAX_AUTO_DIFF_ROWS = 40;

/** A difference smaller than this in relative terms is reported as "same". */
export const DIFF_EPSILON = 1e-9;

/** What side B is reading. */
export type CompareSource = "engine" | "replay";

/**
 * Build the difference table for one instant.
 *
 * Pure, and exported for `test/compare.test.ts`: this is the arithmetic the whole view rests on, and
 * it has three cases that are easy to get wrong — a metric only one side reports (a *shape*
 * difference, which must not read as a value of zero), a metric neither side has sampled yet at
 * this time, and a baseline of exactly zero (no relative difference exists).
 *
 * Rows come back sorted by metric name, so the table does not reorder itself between ticks.
 */
export function metricDifferences(
  names: readonly string[],
  a: MetricHistory,
  b: MetricHistory,
  tASeconds: number,
  tBSeconds: number,
  units: Readonly<Record<string, string>>,
): MetricDiff[] {
  const rows: MetricDiff[] = [];
  for (const metric of [...names].sort()) {
    const inA = a.has(metric);
    const inB = b.has(metric);
    const av = inA ? a.at(metric, tASeconds) : null;
    const bv = inB ? b.at(metric, tBSeconds) : null;
    const delta = av === null || bv === null ? null : bv - av;
    const relative = delta === null || av === null || Math.abs(av) < DIFF_EPSILON ? null : delta / Math.abs(av);
    rows.push({
      metric,
      a: av,
      b: bv,
      delta,
      relative,
      unit: units[metric] ?? "",
      oneSided: inA !== inB,
    });
  }
  return rows;
}

/** `[t, a, b, b − a]` for one metric, on side A's sample grid. */
export type DiffSeries = [number[], (number | null)[], (number | null)[], (number | null)[]];

/**
 * Align one metric's two histories onto side A's sample times.
 *
 * A's grid is the x axis because A is the run on screen; B is read at `t + offsetSeconds` with the
 * step rule {@link MetricHistory.at} follows. Where B has no sample yet the row is `null`, which
 * uPlot draws as a gap — a difference of zero would claim the two runs agreed at an instant one of
 * them has not reached.
 *
 * Pure and exported so `test/compare.test.ts` can pin the gap behaviour, which is the part a plot
 * makes invisible.
 */
export function alignedDiffSeries(
  metric: string,
  a: MetricHistory,
  b: MetricHistory,
  offsetSeconds: number,
): DiffSeries {
  // `UplotData` is `[number[], ...(number | null)[][]]`, so column 1 is the single y series
  // `MetricHistory.get` returns. Read by index with an explicit annotation rather than destructured:
  // the y column comes from the tuple's rest element, and being explicit about its type here is
  // cheaper than being surprised by it.
  const data = a.get(metric);
  const xs: number[] = data[0];
  const ys: (number | null)[] = data[1] ?? [];
  const av: (number | null)[] = new Array<number | null>(xs.length);
  const bv: (number | null)[] = new Array<number | null>(xs.length);
  const dv: (number | null)[] = new Array<number | null>(xs.length);
  for (let i = 0; i < xs.length; i++) {
    const left = ys[i];
    const right = b.at(metric, xs[i] + offsetSeconds);
    av[i] = left;
    bv[i] = right;
    dv[i] = left === null || right === null ? null : right - left;
  }
  return [xs, av, bv, dv];
}

/**
 * Side B of the comparison.
 *
 * One instance, like {@link engine}: the app has one comparison view, and two would mean two more
 * WebGL contexts.
 */
export class CompareController {
  /** Side B's scene. Created on first mount and kept for the life of the page. */
  viewer: Viewer | null = null;
  /** Side B's connection, when the source is another engine. */
  client: VwpClient | null = null;
  /** Side B's recording, when the source is a local file. */
  replay: LocalReplay | null = null;
  /** Side B's metric samples (§3.7), in the same store the plots strip uses for side A. */
  readonly metrics = new MetricHistory(900);

  #source: CompareSource | null = null;
  #label = "";
  #state: CompareSideView["state"] = "idle";
  #detail = "";
  #tNs = 0;
  #startNs = 0;
  #endNs = 0;
  #baseUrl = "";
  #world: VwpWorld | null = null;
  #worldFromA = false;
  #hello: HelloMessage | null = null;
  #bindings: ReplayBindings | null = null;
  #detachViewer: (() => void) | null = null;
  #timer: ReturnType<typeof setInterval> | null = null;
  #units: Readonly<Record<string, string>> = {};
  #seeking = false;

  // -------------------------------------------------------------------------------------------
  // Lifecycle
  // -------------------------------------------------------------------------------------------

  /** Side B's `Hello`, when it has one. */
  get hello(): HelloMessage | null {
    return this.#hello;
  }

  /** Which source is open, or `null`. */
  get source(): CompareSource | null {
    return this.#source;
  }

  /** Side B's base URL, for the `"engine"` source. */
  get baseUrl(): string {
    return this.#baseUrl;
  }

  /** Whether side B's world geometry was borrowed from side A. */
  get worldBorrowed(): boolean {
    return this.#worldFromA;
  }

  /**
   * Create (or re-mount) side B's viewer on a canvas.
   *
   * Idempotent, like `StudioEngine.mountViewer`: `Viewer.mount` returns early for a canvas it
   * already owns, which is what makes React 19 StrictMode's double-invoke harmless.
   */
  mountViewer(canvas: HTMLCanvasElement, theme: ThemeName): Viewer {
    if (!this.viewer) {
      this.viewer = new Viewer({
        theme: studioTheme(theme),
        timeOfDay: 11,
        // Side B is a comparison pane, not the main viewport: half the actor ceiling keeps the
        // instanced buffers off the main view's budget (09-ui §4).
        maxActors: 10_000,
        autoStart: true,
      });
    }
    this.viewer.mount(canvas);
    // `mount` starts the loop only when it actually created a renderer; a re-mount onto the canvas
    // it already owns returns early, so a pane that was stopped on unmount would come back frozen.
    this.viewer.start();
    this.viewer.setTheme(studioTheme(theme));
    if (this.#world) this.viewer.setWorld(this.#world);
    if (this.client) this.#attachClient(this.client);
    if (this.replay?.isOpen) this.#captureReplay();
    return this.viewer;
  }

  /** Start the 5 Hz publish tick. Safe to call twice. */
  start(): void {
    if (this.#timer !== null) return;
    this.#timer = setInterval(() => this.publish(), 1000 / PUBLISH_HZ);
  }

  /** Stop publishing. The viewer and the source stay open. */
  stop(): void {
    if (this.#timer !== null) clearInterval(this.#timer);
    this.#timer = null;
  }

  /** Close side B entirely. The viewer stays mounted, holding its last frame. */
  close(): void {
    this.#detachViewer?.();
    this.#detachViewer = null;
    this.client?.close(1000, "comparison closed");
    this.client = null;
    this.replay?.close();
    this.replay = null;
    this.metrics.reset();
    this.#source = null;
    this.#label = "";
    this.#state = "idle";
    this.#detail = "";
    this.#tNs = 0;
    this.#startNs = 0;
    this.#endNs = 0;
    this.#baseUrl = "";
    this.#hello = null;
    this.#world = null;
    this.#worldFromA = false;
    useStudio.getState().setCompare(null);
    useStudio.getState().setCompareDiffs([]);
  }

  // -------------------------------------------------------------------------------------------
  // Opening a side
  // -------------------------------------------------------------------------------------------

  /**
   * Open side B as a second VWP connection.
   *
   * Read-only by intent: no `view.follow`, no `view.camera`, no `events.set`. `Telemetry` and
   * `Event` frames are therefore never sent for this connection (§6.7 makes `view.follow` the
   * `Telemetry` subscription and §6.12 subscribes no event channel by default), which leaves
   * exactly what the comparison needs — keyframes, deltas and `MetricSample` — and keeps the
   * second connection off the first one's bandwidth.
   */
  async openEngine(baseUrl: string, pageOrigin: string): Promise<void> {
    this.close();
    const base = normaliseBase(baseUrl) || normaliseBase(pageOrigin);
    this.#source = "engine";
    this.#baseUrl = base;
    this.#label = base;
    this.#state = "opening";
    this.#detail = "connecting";
    this.publish();

    const client = new VwpClient({ url: base, compress: "none", autoReconnect: false, trackPoses: true });
    this.client = client;
    client.onMetric((m) => this.#onMetric(m));
    client.onKeyframe(() => {
      this.#tNs = Number(client.poses.simTimeNs);
    });
    client.onDelta(() => {
      this.#tNs = Number(client.poses.simTimeNs);
    });
    try {
      const hello = await client.connect();
      this.#hello = hello;
      this.#label = `${hello.scenarioName} · ${hello.runLabel || hello.engineVersion}`;
      this.#startNs = 0;
      this.#endNs = Number(hello.simDurationNs);
      this.#attachClient(client);
      this.viewer?.applyHello(hello);
      await this.#loadWorldFor(hello, base);
      const status = await client.request("run.status", {});
      this.#endNs = status.t_end_ns > 0 ? status.t_end_ns : this.#endNs;
      this.#tNs = status.t_ns;
      // A baseline is compared, not run: leave it paused so it cannot drift away from side A
      // between seeks. `run.pause` on an already-paused run is a no-op (§6.6).
      await client.request("run.pause", {}).catch(() => undefined);
      this.#state = "ready";
      this.#detail = `${hello.engineVersion} · run ${bytesToHex(hello.runId).slice(0, 8)}…`;
    } catch (err) {
      this.#state = "failed";
      this.#detail = err instanceof Error ? err.message : String(err);
    }
    this.publish();
  }

  /**
   * Open side B as a local recording, read by the WebAssembly reader.
   *
   * No server is involved: the file never leaves the page. This is the path a reviewer uses to
   * check somebody else's result (09-ui §7).
   */
  async openRecording(
    file: { name?: string; arrayBuffer(): Promise<ArrayBuffer> },
    location?: ReplayModuleLocation,
  ): Promise<void> {
    this.close();
    this.#source = "replay";
    this.#label = file.name ?? "recording";
    this.#state = "opening";
    this.#detail = "loading the WebAssembly reader";
    this.publish();
    try {
      // Held in a local: every call between the `??=` and the use — a constructor, `publish()` —
      // invalidates the narrowing TypeScript would otherwise carry on the private field.
      const bindings = (this.#bindings ??= await loadReplayBindings(location));
      const replay = new LocalReplay();
      this.replay = replay;
      this.#detail = "reading the chunk index";
      this.publish();
      const span = await replay.openBlob(file, bindings);
      this.#startNs = span.startNs;
      this.#endNs = span.endNs;
      // §7.1 keeps the world out of a recording, so borrow side A's when there is one. It is the
      // right geometry whenever the two runs share a world hash, and the panel says when it does
      // not rather than drawing one run's actors on another run's streets.
      this.#adoptWorldFromA();
      await this.seekTo(span.startNs);
      this.#state = "ready";
      this.#detail = `${(replay.position?.totalBytes ?? 0).toLocaleString("en-US")} bytes, ${replay.position?.chunksRead ?? 0} chunk(s) read`;
    } catch (err) {
      this.#state = "failed";
      this.#detail = err instanceof Error ? err.message : String(err);
    }
    this.publish();
  }

  /**
   * Open side B as a recording served over HTTP, range by range.
   *
   * The same reader as {@link openRecording}; the difference is that the bytes arrive as §7.3 asks
   * for them instead of all at once, so a multi-gigabyte recording opens in one round trip's worth
   * of bytes.
   */
  async openRecordingUrl(url: string, location?: ReplayModuleLocation): Promise<void> {
    this.close();
    this.#source = "replay";
    this.#label = url;
    this.#state = "opening";
    this.#detail = "loading the WebAssembly reader";
    this.publish();
    try {
      const bindings = (this.#bindings ??= await loadReplayBindings(location));
      const replay = new LocalReplay();
      this.replay = replay;
      const span = await replay.openUrl(url, bindings);
      this.#startNs = span.startNs;
      this.#endNs = span.endNs;
      this.#adoptWorldFromA();
      await this.seekTo(span.startNs);
      this.#state = "ready";
      this.#detail = `${replay.position?.requests ?? 0} range request(s), ${replay.position?.residentBytes ?? 0} bytes resident`;
    } catch (err) {
      this.#state = "failed";
      this.#detail = err instanceof Error ? err.message : String(err);
    }
    this.publish();
  }

  // -------------------------------------------------------------------------------------------
  // Synchronised time
  // -------------------------------------------------------------------------------------------

  /**
   * Put side B at `tANs + offsetNs`, clamped to what B can reach.
   *
   * Reentrancy is refused rather than queued: a scrub gesture fires faster than a seek completes,
   * and stacking them would make the last one to *return* win rather than the last one issued. The
   * scrub bar commits on release for the same reason (`TimeControls`).
   */
  async seekTo(tANs: number): Promise<void> {
    if (this.#seeking) return;
    const target = this.#clamp(tANs + useStudio.getState().compareSync.offsetNs);
    this.#seeking = true;
    try {
      if (this.replay?.isOpen) {
        const position = await this.replay.seekToNs(target);
        this.#tNs = position.tNs;
        this.#captureReplay();
      } else if (this.client) {
        const result = await this.client.request("run.seek", { t_ns: Math.round(target), pause_after: true });
        this.#tNs = result.t_ns;
        this.viewer?.interpolator.reset();
        if (this.client.poses.hasKeyframe) this.viewer?.capture(this.client.poses);
      }
    } catch (err) {
      this.#detail = `seek to ${target} ns failed: ${err instanceof Error ? err.message : String(err)}`;
    } finally {
      this.#seeking = false;
    }
    this.publish();
  }

  /** Copy side A's camera onto side B (09-ui §6 — "the same camera in both"). */
  mirrorCameraFrom(a: Viewer | null): void {
    const b = this.viewer;
    if (!a || !b) return;
    const state = a.cameras.state();
    b.cameras.setMode(state.mode);
    b.cameras.focusOn(state.target.x, state.target.y, state.target.z);
    b.cameras.altitudeM = state.altitudeM;
    b.cameras.distanceM = state.distanceM;
    b.cameras.bearingRad = state.bearingRad;
    // Following by actor id, not by slot: §3.3.1 slots are per-run and the same id is a different
    // vehicle in another run only if the runs are unrelated, in which case following nothing is the
    // honest result.
    b.cameras.follow(state.followActorId);
    b.cameras.snap();
  }

  /** Units for the difference table, from side A's `metrics.query` catalogue (§6.12). */
  setUnits(units: Readonly<Record<string, string>>): void {
    this.#units = units;
  }

  /** Borrow side A's decoded world for a recording that carries none (§7.1). */
  adoptWorldFromA(): void {
    this.#adoptWorldFromA();
    this.publish();
  }

  /**
   * Whether the two sides describe the same world.
   *
   * `null` when it cannot be known — a recording carries no `Hello`, so the reader has no world
   * hash to compare and "same world" is the user's assertion, not a checked fact.
   */
  worldMatchesA(): boolean | null {
    const a = useStudio.getState().hello;
    if (a === null || this.#hello === null) return null;
    return bytesToHex(this.#hello.worldHash) === a.worldHash;
  }

  /** The manifest fields worth putting beside each other (09-ui §6's "manifest diff"). */
  manifestRows(): readonly { readonly field: string; readonly a: string; readonly b: string; readonly same: boolean }[] {
    const a = useStudio.getState().hello;
    const b = this.#hello;
    const rows: { field: string; a: string; b: string; same: boolean }[] = [];
    const add = (field: string, left: string, right: string): void => {
      rows.push({ field, a: left, b: right, same: left === right && left !== "—" });
    };
    add("engine", a?.engineVersion ?? "—", b?.engineVersion ?? (this.#source === "replay" ? "recording" : "—"));
    add("scenario", a?.scenarioName ?? "—", b?.scenarioName ?? "—");
    add("run label", a?.runLabel ?? "—", b?.runLabel ?? this.#label);
    add("scenario hash", a?.scenarioHash ?? "—", b === null ? "—" : bytesToHex(b.scenarioHash));
    add("world hash", a?.worldHash ?? "—", b === null ? "—" : bytesToHex(b.worldHash));
    add("Δt_mob", a === null ? "—" : `${a.mobilityStepNs} ns`, b === null ? "—" : `${String(b.mobilityStepNs)} ns`);
    add("keyframe period", a === null ? "—" : `${a.keyframePeriodNs} ns`, b === null ? "—" : `${String(b.keyframePeriodNs)} ns`);
    add("actors", String(useStudio.getState().run.actors), String(this.#actorCount()));
    return rows;
  }

  // -------------------------------------------------------------------------------------------
  // Publishing
  // -------------------------------------------------------------------------------------------

  /** Push side B's summary and the difference table into the store. */
  publish(): void {
    const store = useStudio.getState();
    if (this.#source === null) {
      store.setCompare(null);
      store.setCompareDiffs([]);
      return;
    }
    store.setCompare({
      source: this.#source,
      label: this.#label,
      state: this.#state,
      detail: this.#detail,
      tNs: this.#tNs,
      startNs: this.#startNs,
      endNs: this.#endNs,
      actors: this.#actorCount(),
      hasMetrics: this.metrics.names().length > 0,
    });
    store.setCompareDiffs(this.#differences(store.compareMetrics, store.simTimeNs > 0 ? store.simTimeNs : store.run.tNs));
  }

  #differences(selected: readonly string[], tANs: number): MetricDiff[] {
    const names = selected.length > 0 ? selected : this.#autoNames();
    return metricDifferences(names, engine.metrics, this.metrics, tANs / 1e9, this.#tNs / 1e9, this.#units);
  }

  /** Every metric either side reports, capped so a wide catalogue cannot make the table unusable. */
  #autoNames(): readonly string[] {
    const set = new Set<string>();
    for (const name of engine.metrics.names()) set.add(name);
    for (const name of this.metrics.names()) set.add(name);
    return [...set].sort().slice(0, MAX_AUTO_DIFF_ROWS);
  }

  #actorCount(): number {
    if (this.replay?.isOpen) {
      let live = 0;
      const poses = this.replay.poses;
      for (let slot = 0; slot < poses.count; slot++) if (poses.occupied[slot] === 1) live++;
      return live;
    }
    if (this.client) {
      let live = 0;
      const poses = this.client.poses;
      for (let slot = 0; slot < poses.count; slot++) if (poses.occupied[slot] === 1) live++;
      return live;
    }
    return 0;
  }

  // -------------------------------------------------------------------------------------------
  // Internals
  // -------------------------------------------------------------------------------------------

  #clamp(tNs: number): number {
    const lo = this.#startNs;
    const hi = this.#endNs > lo ? this.#endNs : lo;
    if (!Number.isFinite(tNs)) return lo;
    return Math.min(hi, Math.max(lo, tNs));
  }

  #attachClient(client: VwpClient): void {
    const viewer = this.viewer;
    if (!viewer) return;
    this.#detachViewer?.();
    this.#detachViewer = viewer.attachClient(client);
    if (client.poses.hasKeyframe) viewer.capture(client.poses);
  }

  /** Snapshot the recording's poses into side B's interpolator, after a seek. */
  #captureReplay(): void {
    const viewer = this.viewer;
    const replay = this.replay;
    if (!viewer || !replay) return;
    // A seek is a discontinuity: interpolating across it would slide every actor from where it was
    // before the jump to where it is after, over the smoothing window.
    viewer.interpolator.reset();
    if (replay.signals) viewer.worldRenderer.updateSignalPhases(replay.signals);
    viewer.capture(replay.poses);
    // Captured twice on purpose: the interpolator samples *between* two snapshots, and one
    // snapshot leaves it with nothing to interpolate from, so a seeked recording would render
    // nothing until the next seek.
    viewer.capture(replay.poses);
  }

  #adoptWorldFromA(): void {
    const world = engine.world;
    if (!world) return;
    this.#world = world;
    this.#worldFromA = true;
    this.viewer?.setWorld(world);
  }

  /**
   * Fetch, verify and adopt side B's own world (§3.1.6 mode 0, §10.5 W3).
   *
   * The URL is joined against side B's base, not the page's: `world_ref.str_url` is root-relative
   * and side B is by definition somewhere else. The payload digest is checked against side B's own
   * `Hello.world_hash` before it is decoded, exactly as `StudioEngine.loadWorldPayload` does for
   * side A — a comparison against a world the baseline was not computed on is worse than no
   * comparison.
   */
  async #loadWorldFor(hello: HelloMessage, base: string): Promise<void> {
    if (hello.worldRef.mode !== 0) {
      // Mode 1 streams the world as §3.9 chunks. Side B does not assemble those: the comparison
      // pane can borrow side A's geometry, and saying so is better than a half-built scene.
      this.#adoptWorldFromA();
      return;
    }
    const path = this.client?.strings.get(hello.worldRef.strUrl) ?? "";
    if (path === "") {
      this.#adoptWorldFromA();
      return;
    }
    const url = resolveEngineUrl(base, path);
    try {
      const res = await fetch(url, { cache: "force-cache" });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const payload = await res.arrayBuffer();
      await verifyWorldPayload(payload, bytesToHex(hello.worldHash));
      const world = decodeWorld(payload);
      this.#world = world;
      this.#worldFromA = false;
      this.viewer?.setWorld(world);
    } catch (err) {
      this.#detail = `world fetch failed (${url}): ${err instanceof Error ? err.message : String(err)}`;
      this.#adoptWorldFromA();
    }
  }

  #onMetric(msg: MetricSampleMessage): void {
    const strings = this.client?.strings;
    const t = Number(msg.simTimeNs) / 1e9;
    for (let i = 0; i < msg.sampleCount; i++) {
      const sample = msg.sample(i);
      const name = strings?.get(sample.strMetric) ?? "";
      if (name === "") continue;
      this.metrics.push(name, t, sample.value);
    }
  }
}

/** The one comparison controller. */
export const compare = new CompareController();
