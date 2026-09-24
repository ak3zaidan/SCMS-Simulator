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
  Color,
  Fog,
  NeutralToneMapping,
  PCFSoftShadowMap,
  PerspectiveCamera,
  SRGBColorSpace,
  Scene,
  WebGLRenderer,
} from "three";
import type {
  DeltaMessage, HelloMessage, KeyframeMessage, PoseBuffer, SignalBlock, VwpClientApi, VwpWorld,
} from "@vwp/protocol";
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
  /**
   * Fade the far distance into the sky colour in the street-level camera modes. Default true.
   *
   * Fog is what gives a street view a horizon and a sense of scale. It is deliberately *not*
   * applied in `map`, where it would grey out the plan view — which is why it used to be off
   * everywhere, and why a chase camera had 3 km of city in perfect focus and no depth at all.
   * {@link Viewer.setFog} takes over from this permanently; {@link Viewer.setDepthCueing} hands it
   * back.
   */
  readonly depthCueing?: boolean;
  /** Distance at which street-level fog begins, metres. Default 140. */
  readonly depthCueNearM?: number;
  /** Distance at which street-level fog is total, metres. Default 1,200. */
  readonly depthCueFarM?: number;
  /**
   * Keep the plan view centred on the traffic while nothing is followed and the user has not moved
   * the camera. Default true.
   *
   * The map opens on the *world*, and the world is usually far larger than the traffic in it. On
   * `scenarios/phase1-manhattan.yaml` the one vehicle starts at (1306, 2036) while the opening
   * window covers y ∈ [300, 1700] — the vehicle is 336 m off the top of the frame, which is why
   * "I don't really see any cars" is the correct reading of a correctly rendered picture. Only the
   * centre moves; the zoom the opening view chose is left alone.
   *
   * This is a standing rule rather than a one-shot because a one-shot does not survive the app:
   * a stream resync re-adopts the world, and `setWorld` puts the focus back on the world's centre.
   * Measured on a mid-run reconnect, that left the only vehicle 200 m off the top of the frame
   * again with nothing left to correct it. It stands down the moment the user pans or zooms
   * ({@link CameraController.userHasMoved}), which is a better answer to "where should the plan
   * view point" than a bounding box is. A followed vehicle takes precedence too, though there it
   * is the ordering inside the frame loop that decides it — see `#trackTraffic`.
   */
  readonly autoFrameActors?: boolean;
  /** Start the rAF loop on mount. Default true. */
  readonly autoStart?: boolean;
  /** Called after each fixed step; for deterministic per-step work. */
  readonly onFixedStep?: (stepSeconds: number, clockSeconds: number) => void;
  /** Called after each rendered frame. */
  readonly onFrame?: (dtSeconds: number, clockSeconds: number) => void;
}

/** Where the live actors are, returned by {@link Viewer.liveActorFraming}. */
export interface ActorFraming {
  /** How many live actors the box covers. */
  readonly count: number;
  readonly centerX: number;
  readonly centerY: number;
  readonly centerZ: number;
  /** The larger horizontal span of the box, metres. */
  readonly extentM: number;
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

/**
 * Camera-to-subject distance the authored depth-cue distances belong to, metres.
 *
 * The chase camera sits about 9 m behind and 4 m above a vehicle, so ~10 m is "street level" and
 * the fog is used exactly as authored there. See `Viewer#syncDepthCueing`.
 */
const DEPTH_CUE_REFERENCE_M = 10;

/** Margin kept round the traffic when the opening plan view widens to hold it, metres. */
const OPEN_TRAFFIC_PADDING_M = 60;

/** How often the traffic's centre is recomputed for the plan view, hertz. See `Viewer#trackTraffic`. */
const AUTO_FRAME_HZ = 5;

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
  // Khronos PBR Neutral rather than ACES filmic. ACES has a heavy toe: a lit surface at 0.017 in
  // linear space came out at 0.008, so the dark theme's road and ground — authored as display
  // values and consumed as albedo — landed at about #141414 and the plan view read as a black void
  // with a few white strips in it. Neutral is near-identity through the midtones and only rolls off
  // the highlights, so a surface displays roughly the colour the theme asked for, which is the
  // right contract for an instrument. The bright crossings still do not clip.
  renderer.toneMapping = NeutralToneMapping;
  renderer.toneMappingExposure = 1;
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
  /** One reused `Fog`; toggling it is a reference swap, never an allocation in the render loop. */
  #fog: Fog | null = null;
  #depthCueing: boolean;
  #depthCueNear: number;
  #depthCueFar: number;
  /** The camera mode the fog state was last computed for; `null` forces a recompute. */
  #cuedMode: CameraMode | null = null;
  /** The depth-cue scale the fog distances were last written for; see `#syncDepthCueing`. */
  #cuedScale = 0;
  #autoFrameActors: boolean;
  /** Set until the plan view has been centred on the traffic once, which is done with a cut. */
  #autoFramePending = true;
  /** The plan-view extent the viewer itself last set, or null once someone else has zoomed. */
  #autoExtentM: number | null = null;
  /** Seconds since the traffic centre was last recomputed; see `#trackTraffic`. */
  #autoFrameAccum = 0;
  /** Scratch table of per-class bounding radii, rebuilt on each `Hello`. */
  #classRadii = new Float32Array(0);
  /** Set by {@link setFog}, after which the mode stops driving the fog. */
  #fogUserSet = false;
  #detachClient: (() => void) | null = null;
  /**
   * Signal blocks waiting for the render clock to reach their sim time. Poses are drawn about one
   * mobility step in the past (`interp.ts`); a lamp applied the moment its frame arrives would
   * change colour a step before the vehicles it controls reach the instant it changed at, which on
   * a stop line is the difference between a car crossing on green and on red.
   */
  #signalQueue: { simSeconds: number; keyframe: boolean; block: SignalBlock }[] = [];
  #lastSignalSim = Number.NaN;
  /** The `Hello` whose run the signal state belongs to; a re-attach with the same one keeps it. */
  #signalHello: HelloMessage | null = null;
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
    this.#depthCueing = options.depthCueing ?? true;
    this.#depthCueNear = options.depthCueNearM ?? 140;
    this.#depthCueFar = options.depthCueFarM ?? 1200;
    this.#autoFrameActors = options.autoFrameActors ?? true;

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
    this.cameras = new CameraController({
      camera: this.camera,
      world: this.worldRenderer,
      onFrameActors: () => { this.frameActors(); },
    });
    this.picker = new Picker({
      camera: this.camera,
      classes: this.actors.classes,
      world: this.worldRenderer,
    });
    this.interpolator = new PoseInterpolator(options.interpolation);
    // Before any `Hello`: the mark needs body sizes from the first frame, not from the first
    // connection, or a viewer opened on a recording would size every mark as a generic car.
    this.#publishClassRadii(this.actors.classes);

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
    this.#fog = null;
    this.#cuedMode = null;
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
    this.#autoExtentM = this.cameras.extentM;
    if (this.cameras.mode === "map") this.cameras.snap();
    // The opening framing is the world's, not the traffic's, so ask for the first recentre to be a
    // cut rather than a drift. `#trackTraffic` keeps it there afterwards.
    if (!this.cameras.userHasMoved) this.#autoFramePending = true;
  }

  /**
   * Set the extent the plan view opens on, metres, as the viewer's own framing: until the user
   * or a caller changes the zoom, the plan view may widen from here to keep every live vehicle in
   * frame (see `#trackTraffic`). A plain `cameras.fitExtent` is a caller's zoom, which the viewer
   * then leaves alone.
   */
  setOpeningExtent(extentM: number): void {
    this.cameras.fitExtent(extentM);
    this.#autoExtentM = this.cameras.extentM;
  }

  /** Adopt the class table, capacities and world origin a `Hello` announces (§3.1). */
  applyHello(hello: HelloMessage): void {
    const classes = classesFromHello(hello);
    if (classes.length > 0) {
      this.actors.setClasses(classes);
      this.picker.setClasses(classes);
      this.#publishClassRadii(classes);
    }
    if (hello.actorCapacity > 0) this.interpolator.ensureCapacity(hello.actorCapacity);
    // §3.1: the mobility step is the cadence deltas arrive at, so it is the interval the sampler
    // should start from rather than the hardcoded 10 Hz default (Q2).
    const stepSeconds = Number(hello.mobilityStepNs) / 1e9;
    if (stepSeconds > 0) this.interpolator.setNominalIntervalSeconds(stepSeconds);
    // §1.4 case 1: a resumed `Hello` continues the stream the scene is already drawing — the
    // replay that follows it is the frames that were missed, applied in order — so the
    // interpolation history, the lamps and the followed car all stay. `0x20` is HELLO_RESUMED
    // (§3.1.2), spelled as a number because this module imports the protocol's types only.
    if ((hello.helloFlags & 0x20) !== 0) {
      this.#signalHello = hello;
      return;
    }
    this.interpolator.reset();
    // A different Hello is a different run (or a non-resumed reconnect, §1.4 case 2): nothing the
    // lamps showed belongs to it. The *same* Hello re-applied — the Studio re-attaching the viewer
    // — keeps them, because a paused run sends its one keyframe once and never again.
    if (hello !== this.#signalHello) {
      // A different *run* — not a reconnect to the same one — has different vehicles under the
      // same ids and possibly a different world: a chase camera kept on the old subject would sit
      // wherever that id's last pose was, which in a new world can be the void.
      const previous = this.#signalHello;
      if (previous && !sameBytes(previous.runId, hello.runId)) {
        this.cameras.follow(null);
        this.select(null);
        if (this.cameras.mode !== "map") this.cameras.setMode("map", true);
      }
      this.#signalHello = hello;
      this.#signalQueue.length = 0;
      this.#lastSignalSim = Number.NaN;
      this.worldRenderer.signals.resetStates();
    }
  }

  /** Apply a keyframe's signal block, at the sim time the scene is drawn at; poses come through {@link capture}. */
  applyKeyframe(kf: KeyframeMessage): void {
    this.#queueSignals(Number(kf.simTimeNs) / 1e9, kf.signals, true);
  }

  /** Apply a delta's signal rows (§3.4.7: only the ones that changed), time-aligned like the poses. */
  applyDelta(delta: DeltaMessage): void {
    if (delta.signals.count === 0) return;
    this.#queueSignals(Number(delta.simTimeNs) / 1e9, delta.signals, false);
  }

  /**
   * Hold the scene at the newest snapshot (the run is paused, finished or stopped) or release it.
   * See {@link PoseInterpolator.setHeld}.
   */
  setStreamHeld(held: boolean): void {
    this.interpolator.setHeld(held);
  }

  #queueSignals(simSeconds: number, block: SignalBlock, keyframe: boolean): void {
    const copy: SignalBlock = {
      count: block.count,
      signalId: block.signalId.slice(0, block.count),
      timeToChangeDs: block.timeToChangeDs.slice(0, block.count),
      phase: block.phase.slice(0, block.count),
      reserved: block.reserved.slice(0, block.count),
    };
    const last = this.#lastSignalSim;
    // Earlier than what came before, or far later: a seek, a rewind or a new run. What is queued
    // belongs to a stretch of the run that will not be drawn, and a keyframe is the whole state.
    const discontinuous = Number.isFinite(last) && (simSeconds < last - 1e-6 || simSeconds > last + 5);
    this.#lastSignalSim = simSeconds;
    if (discontinuous) {
      this.#signalQueue.length = 0;
      if (keyframe) this.worldRenderer.applySignalKeyframe(copy);
      else this.worldRenderer.applySignalDelta(copy);
      return;
    }
    this.#signalQueue.push({ simSeconds, keyframe, block: copy });
    // A stalled render clock must not let the queue grow without bound.
    while (this.#signalQueue.length > 256) this.#applyQueuedSignal();
  }

  #applyQueuedSignal(): void {
    const q = this.#signalQueue.shift();
    if (!q) return;
    if (q.keyframe) this.worldRenderer.applySignalKeyframe(q.block);
    else this.worldRenderer.applySignalDelta(q.block);
  }

  /** Apply every queued signal block the render clock has reached. */
  #flushSignals(): void {
    const q = this.#signalQueue;
    if (q.length === 0) return;
    const at = this.interpolator.renderSimSeconds;
    // No render time yet (no poses): the lamps are all there is to draw, so draw them now.
    if (!Number.isFinite(at)) {
      while (q.length > 0) this.#applyQueuedSignal();
      return;
    }
    while (q.length > 0 && q[0].simSeconds <= at + 1e-6) this.#applyQueuedSignal();
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
    const offDelta = client.onDelta((delta) => {
      this.applyDelta(delta);
      this.capture(client.poses);
    });
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
    // The fog is a theme colour, so a swap has to repaint it or a light world fades into a dark sky.
    this.#cuedMode = null;
    if (this.#fogUserSet && this.#fog) this.#fog.color.setHex(this.#theme.fogColor);
    this.#syncDepthCueing();
  }

  /** Hours in `[0, 24)`; drives the sun, the sky gradient and the fill light. */
  setTimeOfDay(hours: number): void {
    this.worldRenderer.setTimeOfDay(hours);
  }

  /**
   * Set the fog by hand, and stop the camera mode driving it.
   *
   * Calling this is a statement that the caller wants a particular fog, so
   * {@link ViewerOptions.depthCueing} steps aside until {@link setDepthCueing} hands control back —
   * the same rule the Studio uses for the buildings overlay.
   */
  setFog(enabled: boolean, nearM?: number, farM?: number): void {
    this.#fogUserSet = true;
    this.#applyFog(enabled, this.#theme.fogColor, nearM ?? this.#theme.fogNear, farM ?? this.#theme.fogFar);
  }

  /**
   * Hand the fog back to the camera mode (see {@link ViewerOptions.depthCueing}), or turn automatic
   * depth cueing off entirely.
   */
  setDepthCueing(enabled: boolean, nearM?: number, farM?: number): void {
    this.#depthCueing = enabled;
    this.#fogUserSet = false;
    if (nearM !== undefined) this.#depthCueNear = Math.max(1, nearM);
    if (farM !== undefined) this.#depthCueFar = Math.max(this.#depthCueNear + 1, farM);
    this.#cuedMode = null;
    this.#syncDepthCueing();
  }

  /** Whether the camera mode is currently driving the fog. */
  get depthCueing(): boolean {
    return this.#depthCueing && !this.#fogUserSet;
  }

  /**
   * Put the fog where the camera mode wants it. O(1), and a no-op unless the mode changed, so
   * calling it every frame costs one comparison and never recompiles a shader.
   *
   * Toggling `scene.fog` between null and non-null *does* recompile every material that reads it,
   * which is why this is gated on the mode rather than on the camera's altitude: an altitude
   * threshold would recompile the world twice per wheel-click near the boundary.
   */
  #syncDepthCueing(): void {
    if (this.#fogUserSet || !this.#depthCueing) return;
    const mode = this.cameras.mode;
    const enabled = mode !== "map";
    // Aerial perspective is authored for a street-level camera, so its distances are scaled by how
    // far the camera actually is from what it is looking at. Without this the fly-down turned into
    // a grey wipe: the mode becomes `chase` on the first frame of the transition, the camera is
    // still 1,400 m up, and a fog that is total at 1,200 m erases the entire city for the middle
    // second of the signature interaction — measured, and visible in the captured frames.
    //
    // Only `near` and `far` move, which is a pair of float writes on the existing `Fog`. The
    // null/non-null toggle — the one thing that recompiles every material that reads fog — is
    // still gated on the mode, which is what the note above is really protecting.
    const dist = enabled ? this.cameras.look.distanceTo(this.camera.position) : 0;
    const scale = enabled ? Math.max(1, dist / DEPTH_CUE_REFERENCE_M) : 0;
    // `<=` and a floor, not `<` and a bare ratio: in `map` the scale is 0, so a bare
    // `|0 − 0| < 0 × 0.02` is false and this re-applied the fog on every single frame, breaking the
    // "no-op unless the mode changed" promise the note above makes. Caught by the heap delta in
    // `test/budget.test.ts`, not by reading it.
    const settled = mode === this.#cuedMode
      && Math.abs(scale - this.#cuedScale) <= Math.max(1e-6, this.#cuedScale * 0.02);
    if (settled) return;
    this.#cuedMode = mode;
    this.#cuedScale = scale;
    this.#applyFog(enabled, this.#theme.skyHorizon, this.#depthCueNear * scale, this.#depthCueFar * scale);
  }

  /** Reuse the one `Fog` instance; only the reference on the scene moves. */
  #applyFog(enabled: boolean, colorHex: number, nearM: number, farM: number): void {
    if (!enabled) {
      this.scene.fog = null;
      return;
    }
    let fog = this.#fog;
    if (!fog) {
      fog = new Fog(colorHex, nearM, farM);
      this.#fog = fog;
    } else {
      fog.color.setHex(colorHex);
      fog.near = nearM;
      fog.far = farM;
    }
    this.scene.fog = fog;
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
  flyTo(actorId: number, mode: CameraMode = "chase", instant = false): CameraMode {
    this.select(actorId);
    this.cameras.follow(actorId);
    // Seed the controller with this actor's *actual* pose before the mode changes, so the first
    // frame of the fly-down is aimed at the vehicle rather than at wherever the plan view was
    // pointing. Without it the street-level modes are refused for a frame, or aimed at the map
    // focus for one.
    this.#seedFollowPose(actorId);
    const applied = this.cameras.flyTo(actorId, mode, instant);
    this.#syncDepthCueing();
    return applied;
  }

  /**
   * Change camera mode, keeping whatever is being followed. Returns the mode actually applied.
   *
   * Asking for `chase` or `dashboard` with nothing followed used to give a street-level view of a
   * random city block — the camera fell back to the plan view's focus, which at start-up is the
   * centre of the *world*, a kilometre from the traffic. Two dark planes and one lane marking, no
   * vehicle. Since choosing `chase` says plainly what the user wants, the vehicle nearest the plan
   * view's focus is adopted; only when there is no live vehicle at all is the mode refused, and
   * then the returned mode differs from the requested one and
   * {@link CameraController.rejectedMode} says which was refused, so the caller can say why
   * instead of showing a meaningless frame.
   */
  setCameraMode(mode: CameraMode, instant = false): CameraMode {
    if (CameraController.needsFollowSubject(mode) && !this.cameras.hasFollowSubject) {
      const id = this.#adoptFollowSubject();
      if (id !== null) this.select(id);
    }
    const applied = this.cameras.setMode(mode, instant);
    this.#syncDepthCueing();
    return applied;
  }

  /**
   * Follow the live vehicle nearest the plan view's focus and seed its pose. Returns its id, or
   * null when the stream has no live actor to adopt.
   */
  #adoptFollowSubject(): number | null {
    const slot = this.#nearestLiveSlot(this.cameras.target.x, this.cameras.target.y);
    if (slot < 0) return null;
    const id = this.interpolator.outActorId[slot];
    this.cameras.follow(id);
    this.#pushFollowPose(slot);
    return id;
  }

  /**
   * Keep the part of the canvas under interface panels out of the camera's framing; see
   * {@link CameraController.setViewInsets}. CSS pixels.
   */
  setViewInsets(insets: { top?: number; right?: number; bottom?: number; left?: number }): void {
    this.cameras.setViewInsets(insets);
  }

  /** Hand the controller `actorId`'s current pose if it is in the stream. */
  #seedFollowPose(actorId: number): boolean {
    const ids = this.interpolator.outActorId;
    const occ = this.interpolator.outOccupied;
    const target = actorId >>> 0;
    for (let i = 0; i < this.interpolator.count; i++) {
      if (occ[i] === 1 && ids[i] === target) {
        this.#pushFollowPose(i);
        return true;
      }
    }
    return false;
  }

  /** Copy one slot's interpolated pose into the camera controller. */
  #pushFollowPose(slot: number): void {
    const p = slot * 3;
    this.cameras.setFollowPose(
      this.interpolator.outPosition[p],
      this.interpolator.outPosition[p + 1],
      this.interpolator.outPosition[p + 2],
      this.interpolator.outHeading[slot],
      this.interpolator.outSpeed[slot],
    );
  }

  /**
   * Where the live actors are, or null when the stream has none.
   *
   * `extentM` is the larger horizontal span of their bounding box, so it can go straight into
   * {@link CameraController.fitExtent}.
   */
  liveActorFraming(): ActorFraming | null {
    const pos = this.interpolator.outPosition;
    const occ = this.interpolator.outOccupied;
    const n = this.interpolator.count;
    let count = 0;
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    let sumZ = 0;
    for (let slot = 0; slot < n; slot++) {
      if (occ[slot] !== 1) continue;
      const p = slot * 3;
      const x = pos[p];
      const y = pos[p + 1];
      if (!Number.isFinite(x) || !Number.isFinite(y)) continue;
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
      sumZ += pos[p + 2];
      count++;
    }
    if (count === 0) return null;
    return {
      count,
      centerX: (minX + maxX) / 2,
      centerY: (minY + maxY) / 2,
      centerZ: sumZ / count,
      extentM: Math.max(maxX - minX, maxY - minY),
    };
  }

  /**
   * Frame the camera on the live actors and return how many it framed; 0 means there were none and
   * the camera did not move.
   *
   * This is the answer to the review's "with one actor in a square kilometre of city the user
   * cannot see it": the plan view opens on the whole world, a single vehicle is a few pixels
   * somewhere in it, and there is no way to ask where. The camera flies rather than cuts, so the
   * user keeps their bearings — and `minExtentM` stops a lone vehicle from zooming the map to a
   * 4 m window around its own roof.
   *
   * It does not change the camera mode. In the street-level modes the camera is already on an
   * actor, and the one thing a "show me the vehicles" control must not do is throw away the view
   * the user chose.
   */
  frameActors(paddingM = 90, minExtentM = 240): number {
    const f = this.liveActorFraming();
    if (!f) return 0;
    this.cameras.focusOn(f.centerX, f.centerY, f.centerZ);
    this.cameras.fitExtent(Math.max(minExtentM, f.extentM + paddingM * 2));
    // Nothing followed yet: adopt one, so the chase and dashboard modes and the follow chip have a
    // subject the moment the user asks for them.
    if (this.cameras.followActorId === null) {
      const slot = this.#nearestLiveSlot(f.centerX, f.centerY);
      if (slot >= 0) {
        const id = this.interpolator.outActorId[slot];
        this.select(id);
        this.cameras.follow(id);
      }
    }
    return f.count;
  }

  /** Slot of the live actor nearest `(x, y)`, or −1. */
  #nearestLiveSlot(x: number, y: number): number {
    const pos = this.interpolator.outPosition;
    const occ = this.interpolator.outOccupied;
    const n = this.interpolator.count;
    let best = -1;
    let bestD = Infinity;
    for (let slot = 0; slot < n; slot++) {
      if (occ[slot] !== 1) continue;
      const p = slot * 3;
      const dx = pos[p] - x;
      const dy = pos[p + 1] - y;
      const d = dx * dx + dy * dy;
      if (d < bestD) {
        bestD = d;
        best = slot;
      }
    }
    return best;
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

    // 1. Poses, and the signal states for the instant they are drawn at.
    const sample = this.interpolator.sample(renderClock);
    this.#flushSignals();
    this.worldRenderer.signals.update(renderClock);

    // 2. Camera. Exponential smoothing on the true frame dt (frame-rate independent by construction).
    this.#trackTraffic(dt);
    this.#followSlot = this.#resolveFollowSlot();
    this.#updateGhost();
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
    this.#syncClipPlanes();
    this.worldRenderer.fadeMarkings(this.camera.position.distanceTo(this.cameras.look), this.camera.fov, this.#height);
    this.#syncDepthCueing();
    this.worldRenderer.followCamera(this.camera);
    this.worldRenderer.setShadowFocus(this.cameras.look.x, this.cameras.look.y, this.cameras.look.z);
    this.worldRenderer.updateLod(this.camera);

    // 4. Actors: cull, LOD, instance write. When the camera is inside the followed car's own body,
    // that one instance is not written.
    this.actors.hiddenActorId = this.#cameraInsideFollowed() ? this.cameras.followActorId ?? -1 : -1;
    this.#syncActorLod();
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
      actorId: this.interpolator.outActorId,
      selectedActorId: this.#selectedActorId,
      liveCount: actorStats.live,
      // The aerial vehicle mark paints state, so it has to honour the same ground-truth lock the
      // instances do (09-ui §6). Read from the renderer rather than mirrored, so the two cannot
      // drift.
      showGroundTruth: this.actors.showGroundTruth,
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

  /**
   * Hide the building the followed vehicle is inside, if it is inside one below the roof — a road
   * through a building. See {@link WorldRenderer.setGhostBuilding}.
   */
  #updateGhost(): void {
    const slot = this.#followSlot;
    const w = this.worldRenderer;
    const mode = this.cameras.mode;
    if (!CameraController.needsFollowSubject(mode)) {
      if (w.ghostBuilding >= 0 || w.ghostBuilding2 >= 0) w.setGhostBuildings(-1, -1);
      return;
    }
    let vehicle = -1;
    if (slot >= 0) {
      const p = slot * 3;
      const x = this.interpolator.outPosition[p];
      const y = this.interpolator.outPosition[p + 1];
      const z = this.interpolator.outPosition[p + 2];
      const b = w.buildingIndexAt(x, y);
      if (b >= 0 && z < w.buildingTopOf(b)) vehicle = b;
    }
    // The camera's own building, when it is below that roof: it followed a vehicle through a
    // passage, and after a change of subject it is flying out of it. Hidden, not jumped over.
    // The same for the smoothed look target, which trails the vehicle out of a passage: the car is
    // out, the point the camera aims at is still inside, and the march from it met the wall.
    const inside = (x: number, y: number, z: number): number => {
      const b = w.buildingIndexAt(x, y);
      return b >= 0 && z < w.buildingTopOf(b) ? b : -1;
    };
    const ghosted = (b: number): boolean => b >= 0 && (b === w.ghostBuilding || b === w.ghostBuilding2);
    const c = this.camera.position;
    const l = this.cameras.look;
    const cb = inside(c.x, c.y, c.z);
    const lb = inside(l.x, l.y, l.z);
    let camera = ghosted(cb) ? cb : ghosted(lb) ? lb : -1;
    // And for the whole of a flight out of it: the camera looks back at where it was until it is
    // well on its way, and that roof then filled the frame (measured: σ 3.0, one flat colour).
    if (camera < 0 && this.cameras.inTransit) {
      if (w.ghostBuilding2 >= 0) camera = w.ghostBuilding2;
      else if (w.ghostBuilding >= 0 && w.ghostBuilding !== vehicle) camera = w.ghostBuilding;
    }
    w.setGhostBuildings(vehicle, camera);
  }

  /**
   * Put the near and far planes where the depth buffer can resolve the road's layers.
   *
   * A 24-bit depth buffer resolves `d² / (near · 2²⁴)` metres at distance `d`. With the near plane
   * at 0.35 m that is 0.38 m at the 1.4 km the plan view looks down from — six times the 6 cm
   * between the road, junction and crossing layers, so they z-fought across the whole map as it
   * moved. Nothing in the world is above its bounding box (plus the selected-vehicle stem), so a
   * camera above that can push its near plane to most of the gap: 0.2 mm at the same distance.
   * At street level the near plane follows the camera's height, capped at a metre.
   */
  #syncClipPlanes(): void {
    const cam = this.camera;
    const world = this.worldRenderer.world;
    const base = this.#options.nearM ?? 0.35;
    let near = base;
    let far = this.#options.farM ?? 12_000;
    if (world) {
      const look = this.cameras.look;
      const dist = cam.position.distanceTo(look);
      const top = Math.max(world.bbox.maxZM, look.z + dist * 0.08 + 12);
      const above = cam.position.z - top;
      if (above > 0) {
        near = Math.max(base, above * 0.8);
      } else if (this.cameras.mode !== "dashboard") {
        const height = cam.position.z - world.bbox.minZM;
        near = Math.min(1, Math.max(base, height * 0.2));
      }
      far = Math.max(far, this.worldRenderer.sky.scale.x * 1.05);
    }
    if (!Number.isFinite(near) || near <= 0) near = base;
    if (Math.abs(near - cam.near) > cam.near * 0.02 || far !== cam.far) {
      cam.near = near;
      cam.far = Math.max(far, near * 10);
      cam.updateProjectionMatrix();
    }
  }

  /**
   * Switch actor detail where the detail stops being visible, not at a fixed distance.
   *
   * At the old fixed 90 m a car's wheels (0.7 m, 6 px at 90 m in an 800 px, 55° view) popped in
   * and out in the middle of an ordinary street view. Here LOD 0 holds until half a metre of detail
   * is 1.5 px, and LOD 1 until a metre is: about 280 m and 560 m at that view, further in a taller
   * window, nearer in a wider field.
   */
  #syncActorLod(): void {
    const fovRad = (this.camera.fov * Math.PI) / 180;
    const pxPerRad = this.#height / Math.max(1e-3, fovRad);
    const d0 = Math.round(Math.min(600, Math.max(60, (0.5 * pxPerRad) / 1.5)));
    const d1 = Math.round(Math.min(2000, Math.max(d0 + 50, (1.0 * pxPerRad) / 1.5)));
    this.actors.setLodDistances(d0, d1);
  }

  /** Advance by an explicit `dt`, for tests and deterministic captures. */
  step(dtSeconds: number): FrameReport {
    return this.renderFrame(this.#lastMs + dtSeconds * 1000);
  }

  /**
   * Whether the camera is inside the followed vehicle's body, so that one instance must be skipped.
   *
   * This used to be `mode === "dashboard"`. That is the *usual* way to end up inside a car, not the
   * only one: `free` seeds itself from wherever the camera already is, so stepping from the
   * driver's seat into the free camera left the camera inside the body with the body still drawn —
   * a pale slab filling the lower half of the frame, captured from the running Studio and looked
   * at. Winding the chase distance down to its 2 m minimum does the same. A geometric test covers
   * all of them and costs one distance comparison per frame.
   */
  #cameraInsideFollowed(): boolean {
    const slot = this.#followSlot;
    if (slot < 0 || this.cameras.followActorId === null) return false;
    const p = slot * 3;
    const dx = this.camera.position.x - this.interpolator.outPosition[p];
    const dy = this.camera.position.y - this.interpolator.outPosition[p + 1];
    const dz = this.camera.position.z - this.interpolator.outPosition[p + 2];
    const def = this.actors.classes[this.interpolator.outClassIdx[slot]];
    // A camera on the skin of the body still sees its inside face, hence the margin.
    const reach = (def ? Math.max(def.lengthM, def.widthM) / 2 : 2.5) + 0.35;
    const height = (def ? def.heightM : 1.6) + 0.5;
    return dx * dx + dy * dy < reach * reach && dz > -0.5 && dz < height;
  }

  /**
   * Keep the plan view centred on the traffic (see {@link ViewerOptions.autoFrameActors}).
   *
   * Recomputed at {@link AUTO_FRAME_HZ}, not every frame: {@link liveActorFraming} is an O(n) pass
   * over the pose buffer and allocates its result, and the traffic's centre of mass does not move
   * fast enough to need 60 Hz. The first one cuts, so the opening picture is right rather than
   * drifting into place; after that the camera's own smoothing carries it.
   */
  #trackTraffic(dt: number): void {
    if (!this.#autoFrameActors || this.cameras.userHasMoved) return;
    // Following something, or not being in the plan view, means this cannot change the outcome:
    // `setFollowPose` runs later in the same frame and puts the focus on the followed vehicle, and
    // the other modes do not use the focus at all. So this is a cost guard, not a behaviour guard —
    // it is here to skip the O(n) `liveActorFraming` pass, which at 5,000 actors is not free.
    if (this.cameras.followActorId !== null || this.cameras.mode !== "map") return;
    if (!this.worldRenderer.world) return;
    this.#autoFrameAccum += dt;
    if (!this.#autoFramePending && this.#autoFrameAccum < 1 / AUTO_FRAME_HZ) return;
    this.#autoFrameAccum = 0;
    const f = this.liveActorFraming();
    if (!f) return; // no traffic yet — nothing to centre on
    // While the zoom is still the viewer's own opening framing, the plan view is widened — never
    // narrowed — just enough to hold every live vehicle, never wider than the world. Vehicle
    // marks are a constant angular size (`VEHICLE_MARK_ANGULAR_RADIUS`), so a wider view no longer
    // makes a vehicle smaller on screen; it stops the opening frame from leaving a quarter of the
    // traffic outside it (54 of 200 on the mock run at the fixed 1,400 m), and a vehicle entering
    // at the world's edge from being culled. Once anyone else sets the zoom — the user (checked
    // above), or a caller through `cameras` — the zoom is theirs and only the centre moves.
    this.cameras.focusOn(f.centerX, f.centerY, f.centerZ);
    const own = this.#autoExtentM;
    if (own !== null && Math.abs(this.cameras.extentM - own) <= 1e-6 * Math.max(1, own)) {
      const w = this.worldRenderer.world;
      const worldExtent = w ? Math.max(w.bbox.maxXM - w.bbox.minXM, w.bbox.maxYM - w.bbox.minYM) * 1.05 : Infinity;
      const want = Math.min(worldExtent, f.extentM + 2 * OPEN_TRAFFIC_PADDING_M);
      if (want > this.cameras.extentM) this.cameras.fitExtent(want);
      this.#autoExtentM = this.cameras.extentM;
    } else {
      this.#autoExtentM = null;
    }
    if (this.#autoFramePending) {
      // A snap mid-flight is a cut, and coming back from chase to the map *is* a flight.
      if (!this.cameras.isFlying) this.cameras.snap();
      this.#autoFramePending = false;
    }
  }

  /**
   * Hand the per-class bounding radii to the aerial vehicle mark.
   *
   * The mark decides between a dot, a ring and nothing by comparing the vehicle's own angular size
   * with its own, so it needs the body sizes `Hello` declares. Same radius the actor renderer culls
   * with — half the body diagonal — because the question both are asking is "how big does this
   * thing look".
   */
  #publishClassRadii(classes: readonly ActorClassDef[]): void {
    if (this.#classRadii.length !== classes.length) this.#classRadii = new Float32Array(classes.length);
    for (let i = 0; i < classes.length; i++) {
      const d = classes[i];
      this.#classRadii[i] = Math.hypot(d.lengthM, d.widthM, d.heightM) * 0.5;
    }
    this.overlays.locators.setClassRadii(this.#classRadii);
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

function sameBytes(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}
