/**
 * The viewer: one scene, one renderer, one camera.
 *
 * 09-ui §3 is the whole reason this class looks the way it does. "The 2D map and the 3D world are the
 * same Three.js scene. The 2D view is a top-down camera; the 3D view is a chase/dashboard/free-fly
 * camera. The 'click and fly down' interaction is therefore a camera path, not a scene switch, and
 * every overlay exists in both views." So there is exactly one `THREE.Scene`, one `WebGLRenderer` and
 * one `PerspectiveCamera` here, and `map` is a camera mode rather than a separate renderer, canvas or
 * scene graph. Nothing in {@link Viewer} tears down or rebuilds anything when the mode changes.
 *
 * Render profile, from 09-ui §4: pixel ratio capped at 1.5, ACES filmic tone mapping, sRGB output,
 * 2,048² PCF soft shadows.
 *
 * The loop is a `requestAnimationFrame` callback with a fixed-step accumulator. Poses are sampled at
 * `fixedClock + residual`, so the pose clock advances in whole {@link ViewerOptions.fixedStepSeconds}
 * increments plus a sub-step residual, which is what makes 10 Hz deltas look like 60 fps motion
 * (`interp.ts`). Camera smoothing runs on the real frame `dt` because `1 − exp(−λ·dt)` is already
 * frame-rate independent — substepping it would only cost time.
 *
 * Headless use: pass {@link ViewerOptions.createRenderer} to substitute anything satisfying
 * {@link ViewerRenderer}, and drive {@link Viewer.renderFrame} by hand instead of {@link Viewer.start}.
 */

import {
  ACESFilmicToneMapping,
  Color,
  Fog,
  PCFSoftShadowMap,
  PerspectiveCamera,
  SRGBColorSpace,
  Scene,
  WebGLRenderer,
} from "three";
import type { HelloMessage, KeyframeMessage, PoseBuffer, VwpClientApi, VwpWorld } from "@vwp/protocol";
import { ActorRenderer, DEFAULT_ACTOR_CLASSES, classesFromHello } from "./actors.js";
import { CameraController, type CameraMode } from "./cameras.js";
import { PoseInterpolator, type PoseInterpolatorOptions } from "./interp.js";
import { OverlayManager } from "./overlays.js";
import { Picker } from "./picking.js";
import { FrameStats } from "./stats.js";
import { DARK_THEME, themeByName, type ViewerTheme } from "./theme.js";
import { WorldRenderer, type WorldRendererOptions } from "./world-render.js";
import type { ActorClassDef, FrameScheduler, PickResult, ViewerCanvas, ViewerRenderer } from "./types.js";

/** Options for {@link Viewer}. */
export interface ViewerOptions {
  /** Mount immediately on this canvas; otherwise call {@link Viewer.mount}. */
  readonly canvas?: ViewerCanvas;
  /** A theme, or the name of one (`"dark"`, `"light"`). Default dark. */
  readonly theme?: ViewerTheme | string;
  /** Build the renderer. Default is a `WebGLRenderer` with the 09-ui §4 profile. */
  readonly createRenderer?: (canvas: ViewerCanvas, options: ViewerOptions) => ViewerRenderer;
  /** Frame scheduler. Default is `requestAnimationFrame` + `performance.now`. */
  readonly scheduler?: FrameScheduler;
  /** Device pixel ratio ceiling. Default 1.5 (09-ui §4). */
  readonly pixelRatioCap?: number;
  readonly antialias?: boolean;
  readonly shadows?: boolean;
  /** Shadow map edge. Default 2048. */
  readonly shadowMapSize?: number;
  /** Fixed step for the pose clock, seconds. Default 1/60. */
  readonly fixedStepSeconds?: number;
  /** Fixed steps allowed per frame before the backlog is dropped. Default 6. */
  readonly maxSubSteps?: number;
  /** Longest frame `dt` honoured, seconds. Default 0.25. */
  readonly maxFrameSeconds?: number;
  /** Instance ceiling handed to the actor renderer. Default 20,000. */
  readonly maxActors?: number;
  /** Hours in `[0, 24)`. Default 11. */
  readonly timeOfDay?: number;
  /** Initial class table; replaced by `Hello`. */
  readonly classes?: readonly ActorClassDef[];
  /** Passed through to the world renderer. */
  readonly world?: Omit<WorldRendererOptions, "theme">;
  /** Passed through to the pose interpolator. */
  readonly interpolation?: PoseInterpolatorOptions;
  /** Camera near plane, metres. Default 0.35. */
  readonly nearM?: number;
  /** Camera far plane, metres. Default 12,000. */
  readonly farM?: number;
  /** Start the rAF loop on mount. Default true. */
  readonly autoStart?: boolean;
  /** Called after each fixed step; for deterministic per-step work. */
  readonly onFixedStep?: (stepSeconds: number, clockSeconds: number) => void;
  /** Called after each rendered frame. */
  readonly onFrame?: (dtSeconds: number, clockSeconds: number) => void;
}

/** What one frame did, returned by {@link Viewer.renderFrame}. */
export interface FrameReport {
  readonly dtSeconds: number;
  readonly clockSeconds: number;
  readonly fixedSteps: number;
  readonly actorsDrawn: number;
  readonly actorsCulled: number;
  readonly interpolationAlpha: number;
  readonly stalled: boolean;
}

const DEFAULT_SCHEDULER: FrameScheduler = {
  request: (cb) =>
    typeof requestAnimationFrame === "function"
      ? requestAnimationFrame(cb)
      : (setTimeout(() => cb(nowMs()), 16) as unknown as number),
  cancel: (h) => {
    if (typeof cancelAnimationFrame === "function") cancelAnimationFrame(h);
    else clearTimeout(h as unknown as ReturnType<typeof setTimeout>);
  },
  now: () => nowMs(),
};

function nowMs(): number {
  return typeof performance !== "undefined" ? performance.now() : Date.now();
}

function defaultCreateRenderer(canvas: ViewerCanvas, options: ViewerOptions): ViewerRenderer {
  const renderer = new WebGLRenderer({
    canvas,
    antialias: options.antialias ?? true,
    powerPreference: "high-performance",
    alpha: false,
    stencil: false,
  });
  renderer.outputColorSpace = SRGBColorSpace;
  renderer.toneMapping = ACESFilmicToneMapping;
  renderer.toneMappingExposure = 1.05;
  renderer.shadowMap.enabled = options.shadows ?? true;
  renderer.shadowMap.type = PCFSoftShadowMap;
  const dpr = typeof devicePixelRatio === "number" ? devicePixelRatio : 1;
  renderer.setPixelRatio(Math.min(dpr, options.pixelRatioCap ?? 1.5));
  // No cast: `WebGLRenderer` satisfies `ViewerRenderer` structurally, which is the point of the
  // interface — a headless or `OffscreenCanvas` substitute has to offer the same surface.
  return renderer;
}

/**
 * Owns the scene, the renderer and the frame loop; composes the world, actor, overlay, camera,
 * picking and statistics subsystems.
 */
export class Viewer {
  readonly scene = new Scene();
  readonly camera: PerspectiveCamera;
  readonly worldRenderer: WorldRenderer;
  readonly actors: ActorRenderer;
  readonly overlays: OverlayManager;
  readonly cameras: CameraController;
  readonly picker: Picker;
  readonly interpolator: PoseInterpolator;
  readonly stats = new FrameStats(240);

  #options: ViewerOptions;
  #theme: ViewerTheme;
  #scheduler: FrameScheduler;
  #renderer: ViewerRenderer | null = null;
  #canvas: ViewerCanvas | null = null;
  #handle: number | null = null;
  #running = false;
  #lastMs = 0;
  #startMs = 0;
  #fixedClock = 0;
  #accumulator = 0;
  #fixedStep: number;
  #maxSubSteps: number;
  #maxFrame: number;
  #pixelRatioCap: number;
  #width = 1280;
  #height = 720;
  #selectedActorId: number | null = null;
  #followSlot = -1;
  #detachClient: (() => void) | null = null;
  #lastReport: FrameReport = {
    dtSeconds: 0, clockSeconds: 0, fixedSteps: 0, actorsDrawn: 0, actorsCulled: 0,
    interpolationAlpha: 0, stalled: true,
  };

  constructor(options: ViewerOptions = {}) {
    this.#options = options;
    this.#theme = typeof options.theme === "string"
      ? themeByName(options.theme)
      : options.theme ?? DARK_THEME;
    this.#scheduler = options.scheduler ?? DEFAULT_SCHEDULER;
    this.#fixedStep = Math.max(1 / 480, options.fixedStepSeconds ?? 1 / 60);
    this.#maxSubSteps = Math.max(1, options.maxSubSteps ?? 6);
    this.#maxFrame = options.maxFrameSeconds ?? 0.25;
    this.#pixelRatioCap = options.pixelRatioCap ?? 1.5;

    this.scene.name = "vwp";
    this.scene.background = new Color(this.#theme.background);

    this.camera = new PerspectiveCamera(45, 16 / 9, options.nearM ?? 0.35, options.farM ?? 12_000);
    this.camera.name = "vwp/camera";
    this.camera.up.set(0, 0, 1);

    this.worldRenderer = new WorldRenderer({
      theme: this.#theme,
      shadows: options.shadows ?? true,
      shadowMapSize: options.shadowMapSize ?? 2048,
      timeOfDay: options.timeOfDay ?? 11,
      ...(options.world ?? {}),
    });
    this.actors = new ActorRenderer({
      classes: options.classes ?? DEFAULT_ACTOR_CLASSES,
      theme: this.#theme,
      maxActors: options.maxActors ?? 20_000,
    });
    this.overlays = new OverlayManager({ theme: this.#theme, world: this.worldRenderer });
    this.cameras = new CameraController({ camera: this.camera, world: this.worldRenderer });
    this.picker = new Picker({
      camera: this.camera,
      classes: this.actors.classes,
      world: this.worldRenderer,
    });
    this.interpolator = new PoseInterpolator(options.interpolation);

    this.scene.add(this.worldRenderer.group, this.actors.group, this.overlays.group);
    this.#startMs = this.#scheduler.now();

    if (options.canvas) this.mount(options.canvas);
  }

  // -------------------------------------------------------------------------------------------
  // Lifecycle
  // -------------------------------------------------------------------------------------------

  /** The renderer, once mounted. */
  get renderer(): ViewerRenderer | null {
    return this.#renderer;
  }

  /** The canvas, once mounted. */
  get canvas(): ViewerCanvas | null {
    return this.#canvas;
  }

  /** Whether the rAF loop is running. */
  get running(): boolean {
    return this.#running;
  }

  /**
   * The viewer's monotonic wall clock in seconds, used for frame statistics.
   *
   * It is **not** the clock poses are dated against: {@link capture} and
   * {@link PoseInterpolator.sample} both run on {@link renderClockSeconds}, so the interpolator's
   * arrival estimate and its sampling instant come from one clock even when a dropped frame backlog
   * makes the pose clock lag the wall clock.
   */
  get clockSeconds(): number {
    return (this.#scheduler.now() - this.#startMs) / 1000;
  }

  /** The fixed-step pose clock plus its sub-step residual. */
  get renderClockSeconds(): number {
    return this.#fixedClock + this.#accumulator;
  }

  /** The active theme. */
  get theme(): ViewerTheme {
    return this.#theme;
  }

  /** Report from the last {@link renderFrame}. */
  get lastFrame(): FrameReport {
    return this.#lastReport;
  }

  /** Attach to a canvas and create the renderer. */
  mount(canvas: ViewerCanvas): void {
    if (this.#canvas === canvas && this.#renderer) return;
    this.unmount();
    this.#canvas = canvas;
    const create = this.#options.createRenderer ?? defaultCreateRenderer;
    this.#renderer = create(canvas, this.#options);
    this.#renderer.setPixelRatio(
      Math.min(typeof devicePixelRatio === "number" ? devicePixelRatio : 1, this.#pixelRatioCap),
    );
    this.resize();
    if (this.#options.autoStart !== false) this.start();
  }

  /** Detach from the canvas, disposing the renderer. */
  unmount(): void {
    this.stop();
    if (this.#renderer) {
      this.#renderer.dispose();
      this.#renderer = null;
    }
    this.#canvas = null;
  }

  /**
   * Resize to explicit dimensions, or to the canvas's CSS box when none are given.
   * Updates the renderer, the camera aspect and the camera controller's pan scaling.
   */
  resize(width?: number, height?: number): void {
    let w = width;
    let h = height;
    if (w === undefined || h === undefined) {
      const c = this.#canvas as HTMLCanvasElement | null;
      w = c?.clientWidth || c?.width || this.#width;
      h = c?.clientHeight || c?.height || this.#height;
    }
    this.#width = Math.max(1, Math.floor(w));
    this.#height = Math.max(1, Math.floor(h));
    this.camera.aspect = this.#width / this.#height;
    this.camera.updateProjectionMatrix();
    this.cameras.setViewportSize(this.#width, this.#height);
    this.#renderer?.setSize(this.#width, this.#height, false);
  }

  /** Drawing size in CSS pixels. */
  get size(): { width: number; height: number } {
    return { width: this.#width, height: this.#height };
  }

  /** Start the `requestAnimationFrame` loop. */
  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#lastMs = this.#scheduler.now();
    const tick = (timeMs: number): void => {
      if (!this.#running) return;
      this.#handle = this.#scheduler.request(tick);
      this.renderFrame(timeMs);
    };
    this.#handle = this.#scheduler.request(tick);
  }

  /** Stop the loop. The scene stays mounted and can be stepped by hand. */
  stop(): void {
    this.#running = false;
    if (this.#handle !== null) {
      this.#scheduler.cancel(this.#handle);
      this.#handle = null;
    }
  }

  /** Release everything. The canvas belongs to the caller. */
  dispose(): void {
    this.detachClient();
    this.unmount();
    this.cameras.dispose();
    this.overlays.dispose();
    this.actors.dispose();
    this.worldRenderer.dispose();
    this.scene.clear();
    // `Scene.clear()` only removes children; the background colour and the fog are separate
    // references this viewer created and nothing else owns (Q8).
    this.scene.background = null;
    this.scene.fog = null;
  }

  // -------------------------------------------------------------------------------------------
  // Content
  // -------------------------------------------------------------------------------------------

  /** Build the static scene from a decoded `vwp-world/1` payload (§4) and frame the map on it. */
  setWorld(world: VwpWorld): void {
    this.worldRenderer.setWorld(world);
    this.overlays.setWorld(this.worldRenderer);
    this.picker.setWorld(this.worldRenderer);
    this.cameras.setWorld(this.worldRenderer);
    const b = world.bbox;
    this.cameras.focusOn((b.minXM + b.maxXM) / 2, (b.minYM + b.maxYM) / 2, b.minZM);
    this.cameras.fitExtent(Math.max(b.maxXM - b.minXM, b.maxYM - b.minYM) * 1.05);
    if (this.cameras.mode === "map") this.cameras.snap();
  }

  /** Adopt the class table, capacities and world origin a `Hello` announces (§3.1). */
  applyHello(hello: HelloMessage): void {
    const classes = classesFromHello(hello);
    if (classes.length > 0) {
      this.actors.setClasses(classes);
      this.picker.setClasses(classes);
    }
    if (hello.actorCapacity > 0) this.interpolator.ensureCapacity(hello.actorCapacity);
    // §3.1: the mobility step is the cadence deltas arrive at, so it is the interval the sampler
    // should start from rather than the hardcoded 10 Hz default (Q2).
    const stepSeconds = Number(hello.mobilityStepNs) / 1e9;
    if (stepSeconds > 0) this.interpolator.setNominalIntervalSeconds(stepSeconds);
    this.interpolator.reset();
  }

  /** Apply a keyframe's signal block; poses come through {@link capture}. */
  applyKeyframe(kf: KeyframeMessage): void {
    this.worldRenderer.updateSignalPhases(kf.signals);
  }

  /**
   * Snapshot the pose buffer. Call this once per applied keyframe or delta, **not** once per frame:
   * the interpolator fills the gaps between these calls.
   *
   * `clockSeconds` defaults to {@link renderClockSeconds} — the same clock {@link renderFrame}
   * samples on, so the interpolator never has to reconcile two clocks. The snapshot's own date comes
   * from `poses.simTimeNs`; this stamp only records when it arrived.
   */
  capture(poses: PoseBuffer, clockSeconds = this.renderClockSeconds): void {
    this.interpolator.capture(poses, clockSeconds);
  }

  /**
   * Wire the viewer to a `VwpClient` (or the worker client): `Hello` sets the class table, keyframes
   * update the signals and both keyframes and deltas snapshot the poses. Returns a detach function.
   */
  attachClient(client: VwpClientApi): () => void {
    this.detachClient();
    const offHello = client.onHello((hello) => {
      this.applyHello(hello);
      this.capture(client.poses);
    });
    const offKeyframe = client.onKeyframe((kf) => {
      this.applyKeyframe(kf);
      this.capture(client.poses);
    });
    const offDelta = client.onDelta(() => this.capture(client.poses));
    const detach = (): void => {
      offHello();
      offKeyframe();
      offDelta();
    };
    this.#detachClient = detach;
    return detach;
  }

  /** Undo {@link attachClient}. */
  detachClient(): void {
    if (this.#detachClient) {
      this.#detachClient();
      this.#detachClient = null;
    }
  }

  /** Swap the palette across every subsystem. */
  setTheme(theme: ViewerTheme | string): void {
    this.#theme = typeof theme === "string" ? themeByName(theme) : theme;
    (this.scene.background as Color | null)?.setHex(this.#theme.background);
    if (!(this.scene.background instanceof Color)) this.scene.background = new Color(this.#theme.background);
    this.worldRenderer.setTheme(this.#theme);
    this.actors.setTheme(this.#theme);
    this.overlays.setTheme(this.#theme);
  }

  /** Hours in `[0, 24)`; drives the sun, the sky gradient and the fill light. */
  setTimeOfDay(hours: number): void {
    this.worldRenderer.setTimeOfDay(hours);
  }

  /** Distance fog, off by default: at map altitudes it would grey out the whole plan view. */
  setFog(enabled: boolean, nearM?: number, farM?: number): void {
    if (!enabled) {
      this.scene.fog = null;
      return;
    }
    this.scene.fog = new Fog(this.#theme.fogColor, nearM ?? this.#theme.fogNear, farM ?? this.#theme.fogFar);
  }

  // -------------------------------------------------------------------------------------------
  // Selection and cameras
  // -------------------------------------------------------------------------------------------

  /** The selected actor id, or null. */
  get selectedActorId(): number | null {
    return this.#selectedActorId;
  }

  /** Select an actor (highlight colour) without moving the camera. */
  select(actorId: number | null): void {
    this.#selectedActorId = actorId;
    this.actors.selectedActorId = actorId ?? -1;
  }

  /**
   * The signature interaction: from the top-down map, click a vehicle and the camera flies down
   * into a chase view of it — one scene, one camera, no cut (09-ui §1.2, §3).
   */
  flyTo(actorId: number, mode: CameraMode = "chase", instant = false): void {
    this.select(actorId);
    this.cameras.flyTo(actorId, mode, instant);
  }

  /** Change camera mode, keeping whatever is being followed. */
  setCameraMode(mode: CameraMode, instant = false): void {
    this.cameras.setMode(mode, instant);
  }

  /** Pick whatever is under a canvas pixel. `(0, 0)` is the top-left corner. */
  pickAtPixel(px: number, py: number): PickResult {
    return this.picker.pickAtPixel(px, py, this.#width, this.#height);
  }

  /** Pick, then select and fly to an actor if one was hit. Returns what was hit. */
  clickAtPixel(px: number, py: number, mode: CameraMode = "chase"): PickResult {
    const hit = this.pickAtPixel(px, py);
    if (hit && hit.kind === "actor") this.flyTo(hit.actorId, mode);
    return hit;
  }

  /** The slot the followed actor occupies right now, or −1. */
  get followSlot(): number {
    return this.#followSlot;
  }

  // -------------------------------------------------------------------------------------------
  // The loop
  // -------------------------------------------------------------------------------------------

  /**
   * Advance and draw one frame. `timeMs` is a monotonic clock in milliseconds; when omitted the
   * scheduler's own clock is read. Safe to call by hand with the loop stopped — that is how the
   * headless test drives it.
   */
  renderFrame(timeMs: number = this.#scheduler.now()): FrameReport {
    // Frame statistics are measured on the scheduler's real clock, never on `timeMs`: a test or a
    // deterministic capture drives `timeMs` as a simulated clock, and mixing the two would produce
    // nonsense CPU times.
    this.stats.begin(this.#scheduler.now());

    let dt = (timeMs - this.#lastMs) / 1000;
    this.#lastMs = timeMs;
    if (!Number.isFinite(dt) || dt < 0) dt = 0;
    if (dt > this.#maxFrame) dt = this.#maxFrame;

    // Fixed-step pose clock: whole steps, then a residual used as the interpolation alpha.
    this.#accumulator += dt;
    let steps = 0;
    while (this.#accumulator >= this.#fixedStep && steps < this.#maxSubSteps) {
      this.#fixedClock += this.#fixedStep;
      this.#accumulator -= this.#fixedStep;
      steps++;
      this.#options.onFixedStep?.(this.#fixedStep, this.#fixedClock);
    }
    if (steps === this.#maxSubSteps && this.#accumulator > this.#fixedStep) {
      // Dropped backlog: a tab that was in the background, or a very long GC pause.
      this.#accumulator = 0;
    }
    const renderClock = this.#fixedClock + this.#accumulator;

    // 1. Poses.
    const sample = this.interpolator.sample(renderClock);

    // 2. Camera. Exponential smoothing on the true frame dt (frame-rate independent by construction).
    this.#followSlot = this.#resolveFollowSlot();
    if (this.#followSlot >= 0) {
      const p = this.#followSlot * 3;
      this.cameras.setFollowPose(
        this.interpolator.outPosition[p],
        this.interpolator.outPosition[p + 1],
        this.interpolator.outPosition[p + 2],
        this.interpolator.outHeading[this.#followSlot],
        this.interpolator.outSpeed[this.#followSlot],
      );
    } else if (this.cameras.followActorId !== null) {
      this.cameras.clearFollowPose();
    }
    this.cameras.update(dt);

    // 3. Static scene follow-ups.
    this.worldRenderer.followCamera(this.camera);
    this.worldRenderer.setShadowFocus(this.cameras.look.x, this.cameras.look.y, this.cameras.look.z);
    this.worldRenderer.updateLod(this.camera);

    // 4. Actors: cull, LOD, instance write.
    const actorStats = this.actors.update({
      position: this.interpolator.outPosition,
      heading: this.interpolator.outHeading,
      classIdx: this.interpolator.outClassIdx,
      state: this.interpolator.outState,
      occupied: this.interpolator.outOccupied,
      actorId: this.interpolator.outActorId,
      count: this.interpolator.count,
      camera: this.camera,
    });

    // 5. Overlays, which reuse the actor renderer's visible-slot list.
    this.overlays.update({
      camera: this.camera,
      timeSeconds: renderClock,
      position: this.interpolator.outPosition,
      state: this.interpolator.outState,
      occupied: this.interpolator.outOccupied,
      classIdx: this.interpolator.outClassIdx,
      count: this.interpolator.count,
      visibleSlots: this.actors.visibleSlots,
      visibleCount: this.actors.visibleCount,
    });

    // 6. Picking data: O(1), the grid is only built if somebody clicks.
    this.picker.setPoses({
      position: this.interpolator.outPosition,
      heading: this.interpolator.outHeading,
      occupied: this.interpolator.outOccupied,
      actorId: this.interpolator.outActorId,
      classIdx: this.interpolator.outClassIdx,
      count: this.interpolator.count,
    });

    this.stats.counters.actorInstances = actorStats.drawn;
    this.stats.counters.actorCulled = actorStats.culled;
    this.stats.counters.actorLive = actorStats.live;
    this.stats.counters.buildingsVisible = this.worldRenderer.buildingsVisible;

    this.stats.markCpu(this.#scheduler.now());
    this.#renderer?.render(this.scene, this.camera);
    this.stats.end(this.#scheduler.now(), this.#renderer?.info);

    this.#options.onFrame?.(dt, renderClock);
    this.#lastReport = {
      dtSeconds: dt,
      clockSeconds: renderClock,
      fixedSteps: steps,
      actorsDrawn: actorStats.drawn,
      actorsCulled: actorStats.culled,
      interpolationAlpha: sample.alpha,
      stalled: sample.stalled,
    };
    return this.#lastReport;
  }

  /** Advance by an explicit `dt`, for tests and deterministic captures. */
  step(dtSeconds: number): FrameReport {
    return this.renderFrame(this.#lastMs + dtSeconds * 1000);
  }

  #resolveFollowSlot(): number {
    const id = this.cameras.followActorId;
    if (id === null) return -1;
    const ids = this.interpolator.outActorId;
    const occ = this.interpolator.outOccupied;
    const n = this.interpolator.count;
    // The slot rarely moves, so check the cached one first.
    const cached = this.#followSlot;
    if (cached >= 0 && cached < n && occ[cached] === 1 && ids[cached] === (id >>> 0)) return cached;
    const target = id >>> 0;
    for (let i = 0; i < n; i++) {
      if (occ[i] === 1 && ids[i] === target) return i;
    }
    return -1;
  }
}
