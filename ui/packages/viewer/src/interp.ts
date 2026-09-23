/**
 * Pose interpolation.
 *
 * Deltas arrive at the mobility step (10 Hz in the reference scenarios, `Hello.mobility_step_ns`,
 * §3.1) but the renderer runs at 60 fps. 09-ui §2 calls for "the render loop samples the ring at its
 * own cadence and interpolates between the two latest samples"; this is that, without the
 * `SharedArrayBuffer` ring (the viewer is handed a `PoseBuffer` that the client or the worker has
 * already brought up to date, and takes its own snapshot of it).
 *
 * A short **history** of snapshots is kept ({@link HISTORY}), not just the latest two, and the
 * sampler evaluates the segment that actually brackets the render time. With only two snapshots a
 * frame that arrived while the render clock was still inside the previous segment forced the
 * sampler onto the new segment with a negative blend factor — an extrapolation *backwards along the
 * new chord*, which is a visible snap at every keyframe the render clock had not yet reached. The
 * buffers are preallocated and rotated, so steady-state capture and sampling allocate nothing.
 *
 * ## The motion model
 *
 * Between two snapshots the position is a **cubic Hermite** curve whose end tangents are the
 * velocities the engine reported (`speed` along `heading`, §3.3.2), so the path leaves each
 * snapshot in the direction the vehicle is pointing and at the speed it is going. A linear blend
 * has a velocity discontinuity at every snapshot — ten times a second — which is exactly the
 * stepping a followed vehicle shows in a chase camera, and it cuts every turn along its chord. The
 * tangents are *limited* against the chord (Fritsch & Carlson 1980, "Monotone Piecewise Cubic
 * Interpolation", SIAM J. Numer. Anal. 17(2): a tangent of at most three chords keeps a monotone
 * segment monotone), so a speed that disagrees with the positions — a vehicle reported at 11 m/s
 * that did not move — can never make the curve overshoot or loop.
 *
 * Heading is a monotone cubic on the unwrapped angle, with tangents from the neighbouring
 * snapshots where they exist (Catmull–Rom), so the yaw rate is continuous too; it crosses the ±180°
 * seam the short way.
 *
 * Any remaining discontinuity at a snapshot boundary — the sampler had to extrapolate past the
 * newest snapshot and the next one disagrees — is **blended out** rather than shown: the difference
 * between what was on screen and what the new data says for the same instant is carried as a
 * per-actor offset that decays with a {@link ERROR_BLEND_SECONDS} time constant. Offsets larger
 * than {@link PoseInterpolatorOptions.blendMaxMetres} are real jumps and are snapped.
 *
 * ## The clock
 *
 * A snapshot is dated by **sim time** — `PoseBuffer.simTimeNs`, §3.3.1/§3.4.1, the only authoritative
 * clock in the system. The engine is forbidden from reading a wall clock anywhere (ADR 0004) for
 * exactly this reason, and the viewer honours the same discipline as far as the data allows: the
 * instant a frame *arrived* is used for one thing only, deciding that the stream has gone quiet.
 *
 * Sampling therefore needs a map from the viewer's monotonic clock to sim time. That map is a
 * straight line, `sim ≈ anchorSim + (clock − anchorClock) · rate`, maintained in {@link
 * PoseInterpolator.capture} by a minimum-delay filter: a frame that arrives *earlier* relative to
 * its sim time than the line predicts re-anchors it immediately (it is the least-delayed evidence
 * available), while a later one only drags the line by {@link PoseInterpolatorOptions.clockGain} of
 * the error. `rate` (sim seconds per clock second, which is exactly `run.speed`) is measured over a
 * multi-second baseline so slow playback is followed without ever being confused for a stall.
 *
 * The render clock then *follows* that line rather than taking its value: it advances at `rate · dt`
 * with a bounded correction towards the estimate, so a re-anchor, a new rate or a re-measured
 * interval changes the clock's speed by a few per cent and never its position.
 *
 * ## Discontinuities
 *
 * A snapshot dated *before* the newest one, or far after it, is a seek, a rewind or a new run: the
 * history is dropped and the new snapshot starts a fresh one. (The two-snapshot sampler instead
 * blended across the seek with a 1e-6 s span and a clamped blend factor of −1.2, putting every actor
 * that had moved less than the teleport distance up to 1.2 chords *away from both* poses.) A
 * snapshot at the *same* sim time as the newest — a keyframe that repeats the step a delta already
 * carried, or a delta the pose buffer refused — replaces it in place.
 *
 * ## Delay, extrapolation and the stall
 *
 * Every window is a multiple of the **measured snapshot interval**, never an absolute number of
 * seconds: a 2 s cadence (0.05x playback, a keyframe-only run, a congested link) has to interpolate
 * exactly as well as a 0.1 s one. Outside the history the sampler dead-reckons from the newest
 * snapshot's velocity, but it *glides to a stop*: the render clock's advance decays linearly to zero
 * and the total overshoot converges to {@link PoseInterpolatorOptions.maxExtrapolationSteps}
 * intervals. When the owner knows the stream is paused or finished ({@link
 * PoseInterpolator.setHeld}) there is no extrapolation at all: the scene shows exactly the newest
 * snapshot, which is the state the engine is actually in.
 */

import { ACCEL_SCALE } from "@vwp/protocol";
import type { PoseBuffer } from "@vwp/protocol";

/** A copy of the pose buffer at one instant, plus the clock readings that date it. */
export interface PoseSnapshot {
  /**
   * Viewer clock (seconds) at which this snapshot arrived. Used to estimate the clock map and to
   * detect silence — never to position an actor.
   */
  clockSeconds: number;
  /** `PoseBuffer.simTimeNs` converted to seconds. The authoritative date of this snapshot. */
  simSeconds: number;
  /** Slot high-water mark; slots `0..count-1` are meaningful. */
  count: number;
  /** 3 floats per slot, ENU metres. */
  position: Float32Array;
  /** radians, CCW from +x. */
  heading: Float32Array;
  /** m/s along `heading`. */
  speed: Float32Array;
  /** m/s² along `heading`. */
  accel: Float32Array;
  actorId: Uint32Array;
  classIdx: Uint8Array;
  state: Uint8Array;
  occupied: Uint8Array;
  /** False before the first {@link PoseInterpolator.capture}. */
  valid: boolean;
}

/** Tuning for {@link PoseInterpolator}. */
export interface PoseInterpolatorOptions {
  /**
   * How far behind the newest snapshot to render, as a multiple of the measured snapshot interval.
   * 1.0 means "render exactly one step in the past", which guarantees interpolation rather than
   * extrapolation for a perfectly punctual stream. Default 1.25: the extra quarter step (25 ms at
   * 10 Hz) is the margin that lets a frame arrive a little late — a GC pause, a busy main thread —
   * and still be interpolated towards rather than extrapolated past.
   */
  readonly delaySteps?: number;
  /** Ceiling on the render delay, as a multiple of the measured interval. Default 2. */
  readonly maxDelaySteps?: number;
  /**
   * Outer safety bound on the render delay, seconds — deliberately far above any expected cadence,
   * because a cap *below* the snapshot interval is the Q2 defect. Default 5.
   */
  readonly maxDelaySeconds?: number;
  /**
   * How far past the newest snapshot the sampler may extrapolate, as a multiple of the measured
   * interval. Default 1.2 (0.12 s at the reference 10 Hz mobility step).
   */
  readonly maxExtrapolationSteps?: number;
  /** Outer safety bound on extrapolation, seconds. Default 5. */
  readonly maxExtrapolationSeconds?: number;
  /**
   * Silence after which the stream is reported stalled, as a multiple of the measured interval.
   * Default 3. The larger of this and {@link stallSeconds} wins.
   */
  readonly stallSteps?: number;
  /** Silence after which the stream is reported stalled, seconds. Default 0.4. */
  readonly stallSeconds?: number;
  /**
   * A slot that moved further than this between two snapshots is treated as a teleport and snapped
   * rather than interpolated (a GOP origin change, or a slot reused after despawn). The effective
   * threshold also grows with the distance the reported speed covers in the interval, so a slow
   * stream's legitimate motion is never mistaken for one. Metres. Default 40.
   */
  readonly teleportMetres?: number;
  /**
   * Largest on-screen correction blended out rather than snapped when new data disagrees with what
   * was drawn, metres. Default 4.
   */
  readonly blendMaxMetres?: number;
  /** Initial slot capacity. Default 1024. */
  readonly capacity?: number;
  /**
   * Nominal snapshot interval in **sim** seconds, used until two snapshots have been seen and
   * restored by {@link PoseInterpolator.reset}. Seed it from `Hello.mobility_step_ns`. Default 0.1.
   */
  readonly intervalSeconds?: number;
  /**
   * How hard a late arrival drags the clock map, per snapshot, in `[0, 1]`. Small means jitter is
   * rejected; 0 means the map only ever follows the earliest arrival. Default 0.05.
   */
  readonly clockGain?: number;
  /**
   * `"hermite"` (default) or `"linear"`, the pre-2026-09 chord blend, kept so the two can be
   * measured against each other.
   */
  readonly curve?: "hermite" | "linear";
}

/** What one {@link PoseInterpolator.sample} produced, for the caller's bookkeeping. */
export interface SampleInfo {
  /** Blend factor within the segment used; > 1 means the sampler extrapolated past the newest. */
  readonly alpha: number;
  /** Render delay applied, seconds. */
  readonly delaySeconds: number;
  /** Measured interval between snapshots, seconds of **sim** time. */
  readonly intervalSeconds: number;
  /** True when the stream is considered stalled. */
  readonly stalled: boolean;
  /** Slot high-water mark of the sampled output. */
  readonly count: number;
  /** Number of slots that were snapped rather than interpolated this sample. */
  readonly snapped: number;
  /** The sim time this sample rendered, seconds. */
  readonly renderSimSeconds: number;
  /** Estimated sim seconds per viewer-clock second (1 at normal speed, 0.5 at 0.5x playback). */
  readonly rate: number;
}

/** Snapshots kept. Four spans three intervals: enough for a render delay of up to two intervals plus jitter. */
export const HISTORY = 4;

/** Time constant of the correction blend, seconds of viewer clock. */
export const ERROR_BLEND_SECONDS = 0.12;

const NO_ACTOR = 0xffffffff;

function makeSnapshot(capacity: number): PoseSnapshot {
  return {
    clockSeconds: 0,
    simSeconds: 0,
    count: 0,
    position: new Float32Array(capacity * 3),
    heading: new Float32Array(capacity),
    speed: new Float32Array(capacity),
    accel: new Float32Array(capacity),
    actorId: new Uint32Array(capacity).fill(NO_ACTOR),
    classIdx: new Uint8Array(capacity),
    state: new Uint8Array(capacity),
    occupied: new Uint8Array(capacity),
    valid: false,
  };
}

function growF32(a: Float32Array, n: number): Float32Array {
  const out = new Float32Array(n);
  out.set(a.subarray(0, Math.min(a.length, n)), 0);
  return out;
}

function growU8(a: Uint8Array, n: number): Uint8Array {
  const out = new Uint8Array(n);
  out.set(a.subarray(0, Math.min(a.length, n)), 0);
  return out;
}

function growSnapshot(s: PoseSnapshot, capacity: number): void {
  if (s.position.length >= capacity * 3) return;
  s.position = growF32(s.position, capacity * 3);
  s.heading = growF32(s.heading, capacity);
  s.speed = growF32(s.speed, capacity);
  s.accel = growF32(s.accel, capacity);
  const id = new Uint32Array(capacity).fill(NO_ACTOR);
  id.set(s.actorId, 0);
  s.actorId = id;
  s.classIdx = growU8(s.classIdx, capacity);
  s.state = growU8(s.state, capacity);
  s.occupied = growU8(s.occupied, capacity);
}

const TAU = Math.PI * 2;

/** `a` wrapped into `(−π, π]`. */
export function wrapAngle(a: number): number {
  return a - Math.floor(a / TAU + 0.5) * TAU;
}

/** Shortest-arc interpolation between two angles in radians. */
export function lerpAngle(a: number, b: number, t: number): number {
  return a + wrapAngle(b - a) * t;
}

/**
 * A scalar tangent limited so a cubic through a segment of rise `delta` stays monotone (Fritsch &
 * Carlson 1980): zero when it points against the segment, and at most three times the rise.
 */
function limitTangent(m: number, delta: number): number {
  if (delta === 0) return 0;
  if (m * delta <= 0) return 0;
  const cap = 3 * Math.abs(delta);
  return Math.abs(m) > cap ? Math.sign(m) * cap : m;
}

/**
 * The same limit for a 2-D tangent against a 2-D chord, written into `TAN`: zero when the tangent
 * points away from the chord, and at most three chords long. Applied to the vector rather than per
 * axis — per axis, a vehicle crossing the top of a curve (tangent x changing sign within a step)
 * had one component zeroed and turned the corner with a kink.
 */
const TAN = new Float64Array(2);
function limitTangent2(mx: number, my: number, cx: number, cy: number): void {
  const dot = mx * cx + my * cy;
  if (!(dot > 0)) {
    TAN[0] = 0;
    TAN[1] = 0;
    return;
  }
  const m2 = mx * mx + my * my;
  const cap2 = 9 * (cx * cx + cy * cy);
  const k = m2 > cap2 ? Math.sqrt(cap2 / m2) : 1;
  TAN[0] = mx * k;
  TAN[1] = my * k;
}

/** Largest yaw rate dead reckoning will extrapolate with, rad/s: a car at walking pace on a 3 m radius. */
const MAX_YAW_RATE = 2;

/**
 * How much of a frame's advance may go into correcting the clock estimate. 0.1 means the render
 * clock runs at 90–110 % of the stream's rate while it converges — imperceptible, and never a stop.
 */
const SLEW_FRACTION = 0.1;

/** Snapshots of arrival history kept for the rate estimate. */
const RATE_RING = 128;
/** History older than this is retired, so the rate follows a speed change within a few seconds. */
const RATE_WINDOW_SECONDS = 4;
/** …and never shorter than this many snapshot intervals, whatever the cadence. */
const RATE_WINDOW_STEPS = 12;
/** A baseline this long makes the rate estimate trustworthy on its own; shorter ones are blended. */
const RATE_BASELINE_SECONDS = 2;
/** …and this many intervals, so jittered endpoints cannot dominate the ratio. */
const RATE_BASELINE_STEPS = 6;

/** Where the extrapolation ease starts to decelerate, as a fraction of the overshoot budget. */
const EASE_FULL_RATE = 0.7;
/** Where it comes to a complete stop, as a fraction of the overshoot budget. */
const EASE_STOP = 1.3;

/** A snapshot dated this far past the newest is a seek, not a gap. Seconds of sim time. */
const DISCONTINUITY_SECONDS = 5;
/** …or this many measured intervals, whichever is larger. */
const DISCONTINUITY_STEPS = 20;
/**
 * A render clock this far behind its target — a forward seek within the discontinuity window —
 * is re-seated rather than slewed, which at 10 % would take ten times the gap to catch up.
 */
const CATCH_UP_SECONDS = 0.5;
const CATCH_UP_STEPS = 4;

/**
 * How far past the newest snapshot a render clock that wants to be `over` seconds past it is
 * actually allowed to be, given an overshoot budget of `limit` seconds.
 *
 * Full rate up to `0.7 · limit` — ordinary jitter-driven extrapolation must not be throttled, or the
 * throttling itself becomes motion — then the advance rate decays linearly to zero at `1.3 · limit`
 * of *wanted* overshoot, which at 60 fps is a stop spread over four frames rather than one. This is
 * that rate's integral, so it is continuous, `C¹` at both knees, monotonically non-decreasing (a
 * pose can never be pulled backwards, Q7) and saturates at exactly `limit`.
 */
export function extrapolationEase(over: number, limit: number): number {
  if (!(over > 0)) return over;
  if (limit <= 0) return 0;
  const a = EASE_FULL_RATE * limit;
  if (over <= a) return over;
  const b = EASE_STOP * limit;
  if (over >= b) return limit;
  const u = over - a;
  return a + u - (u * u) / (2 * (b - a));
}

/**
 * Keeps the most recent pose snapshots and produces a smooth pose for any render time. All output
 * arrays are preallocated and reused; {@link sample} allocates nothing.
 */
export class PoseInterpolator {
  /**
   * Interpolated output, indexed by slot. These references are **replaced** when
   * {@link ensureCapacity} grows the interpolator, so read them through the instance each frame
   * rather than caching them across frames.
   */
  outPosition: Float32Array;
  outHeading: Float32Array;
  outSpeed: Float32Array;
  outActorId: Uint32Array;
  outClassIdx: Uint8Array;
  outState: Uint8Array;
  outOccupied: Uint8Array;

  /** Snapshot ring; `#ring[#head]` is the newest, `#size` of them are valid. */
  #ring: PoseSnapshot[];
  #head = 0;
  #size = 0;
  #capacity: number;
  #nominalInterval: number;
  #intervalSeconds: number;
  #outCount = 0;
  /** Highest slot index written since it was last cleared, so clearing is O(live), not O(capacity). */
  #dirtyTo = 0;
  // The clock map: sim ≈ #anchorSim + (clock − #anchorClock) · #rate.
  #anchored = false;
  #anchorSim = 0;
  #anchorClock = 0;
  #rate = 1;
  /** Ring of `(arrival clock, sim time)` pairs, the baseline the rate is measured over. */
  #rateClock = new Float64Array(RATE_RING);
  #rateSim = new Float64Array(RATE_RING);
  #rateHead = 0;
  #rateCount = 0;
  #intervalSamples = 0;
  #lastSampleClock = Number.NaN;
  /** Sim time the last {@link sample} rendered; `NaN` until the first one after a reset. */
  #renderSim = Number.NaN;
  #held = false;

  // Correction blend (see the header): per-slot offset added to the raw pose, decaying to zero.
  #errPos: Float32Array;
  #errHead: Float32Array;
  /** Actor each offset belongs to; an offset never follows a slot to a different actor. */
  #errId: Uint32Array;
  /** Set by {@link capture}: the next sample re-bases the offsets at the previous render time. */
  #rebaseAtPrevious = false;
  /** Set by a hold change or a catch-up: the next sample re-bases at its own render time. */
  #rebaseAtCurrent = false;
  #anyError = false;
  /** Set when a discontinuity dropped the history: the next sample reports every live slot snapped. */
  #snapAll = false;
  /** Scratch for one slot's raw pose. */
  #raw = new Float64Array(4);

  #lastInfo: SampleInfo = {
    alpha: 0, delaySeconds: 0, intervalSeconds: 0.1, stalled: false, count: 0, snapped: 0,
    renderSimSeconds: 0, rate: 1,
  };

  readonly delaySteps: number;
  readonly maxDelaySteps: number;
  readonly maxDelaySeconds: number;
  readonly maxExtrapolationSteps: number;
  readonly maxExtrapolationSeconds: number;
  readonly stallSteps: number;
  readonly stallSeconds: number;
  readonly teleportMetres: number;
  readonly blendMaxMetres: number;
  readonly clockGain: number;
  readonly curve: "hermite" | "linear";

  constructor(options: PoseInterpolatorOptions = {}) {
    this.#capacity = Math.max(16, options.capacity ?? 1024);
    this.delaySteps = options.delaySteps ?? 1.25;
    this.maxDelaySteps = options.maxDelaySteps ?? 2;
    this.maxDelaySeconds = options.maxDelaySeconds ?? 5;
    this.maxExtrapolationSteps = options.maxExtrapolationSteps ?? 1.2;
    this.maxExtrapolationSeconds = options.maxExtrapolationSeconds ?? 5;
    this.stallSteps = options.stallSteps ?? 3;
    this.stallSeconds = options.stallSeconds ?? 0.4;
    this.teleportMetres = options.teleportMetres ?? 40;
    this.blendMaxMetres = options.blendMaxMetres ?? 4;
    this.clockGain = Math.min(1, Math.max(0, options.clockGain ?? 0.05));
    this.curve = options.curve ?? "hermite";
    this.#nominalInterval = Math.max(1e-3, options.intervalSeconds ?? 0.1);
    this.#intervalSeconds = this.#nominalInterval;
    this.#ring = [];
    for (let i = 0; i < HISTORY; i++) this.#ring.push(makeSnapshot(this.#capacity));
    this.outPosition = new Float32Array(this.#capacity * 3);
    this.outHeading = new Float32Array(this.#capacity);
    this.outSpeed = new Float32Array(this.#capacity);
    this.outActorId = new Uint32Array(this.#capacity).fill(NO_ACTOR);
    this.outClassIdx = new Uint8Array(this.#capacity);
    this.outState = new Uint8Array(this.#capacity);
    this.outOccupied = new Uint8Array(this.#capacity);
    this.#errPos = new Float32Array(this.#capacity * 3);
    this.#errHead = new Float32Array(this.#capacity);
    this.#errId = new Uint32Array(this.#capacity).fill(NO_ACTOR);
  }

  /** Slot capacity of the interpolator's own arrays. */
  get capacity(): number {
    return this.#capacity;
  }

  /** Slot high-water mark of the last {@link sample}. */
  get count(): number {
    return this.#outCount;
  }

  /** Diagnostics from the last {@link sample}. */
  get lastSample(): SampleInfo {
    return this.#lastInfo;
  }

  /** Smoothed interval between snapshots, seconds of sim time. */
  get intervalSeconds(): number {
    return this.#intervalSeconds;
  }

  /** Estimated sim seconds per viewer-clock second — the stream's playback rate. */
  get rate(): number {
    return this.#rate;
  }

  /** The sim time the last {@link sample} rendered, seconds. */
  get renderSimSeconds(): number {
    return this.#renderSim;
  }

  /** Sim time of the newest snapshot held, or `NaN` when none is. */
  get newestSimSeconds(): number {
    return this.#size > 0 ? this.#ring[this.#head].simSeconds : Number.NaN;
  }

  /** Number of snapshots currently held (at most {@link HISTORY}). */
  get snapshotCount(): number {
    return this.#size;
  }

  /** True once a snapshot exists and sampling is meaningful. */
  get ready(): boolean {
    return this.#size > 0;
  }

  /** Whether the sampler is holding the newest snapshot; see {@link setHeld}. */
  get held(): boolean {
    return this.#held;
  }

  /**
   * Tell the sampler the stream is not advancing on purpose — the run is paused, finished or
   * stopped — so it shows exactly the newest snapshot instead of dead-reckoning past it. A paused
   * engine is *in* the newest state; drawing every vehicle a metre further on is drawing a state
   * that never existed (measured: 1.34 m on Manhattan after a paused seek). Releasing the hold
   * restarts the clock map, because the arrival history spans the pause.
   */
  setHeld(held: boolean): void {
    if (held === this.#held) return;
    this.#held = held;
    this.#rebaseAtCurrent = true;
    if (!held) {
      const keep = this.#renderSim;
      this.#resetClock();
      // Resume from where the picture is, not from a fresh estimate one delay in the past.
      this.#renderSim = keep;
    }
  }

  /**
   * The nominal snapshot interval, in sim seconds: what the interval estimate starts from and what
   * {@link reset} restores. Seed it from `Hello.mobility_step_ns` (§3.1) rather than leaving it at
   * the 10 Hz default.
   */
  get nominalIntervalSeconds(): number {
    return this.#nominalInterval;
  }

  setNominalIntervalSeconds(seconds: number): void {
    if (!Number.isFinite(seconds) || seconds <= 0) return;
    this.#nominalInterval = Math.max(1e-3, seconds);
    if (this.#size < 2) this.#intervalSeconds = this.#nominalInterval;
  }

  /**
   * Grow every array to hold `capacity` slots. Called from {@link capture}; growth doubles, so a
   * stream that settles at a stable actor count stops reallocating after the first few keyframes.
   */
  ensureCapacity(capacity: number): void {
    if (capacity <= this.#capacity) return;
    let n = this.#capacity;
    while (n < capacity) n *= 2;
    for (const s of this.#ring) growSnapshot(s, n);
    this.outPosition = growF32(this.outPosition, n * 3);
    this.outHeading = growF32(this.outHeading, n);
    this.outSpeed = growF32(this.outSpeed, n);
    const aid = new Uint32Array(n).fill(NO_ACTOR);
    aid.set(this.outActorId, 0);
    this.outActorId = aid;
    this.outClassIdx = growU8(this.outClassIdx, n);
    this.outState = growU8(this.outState, n);
    this.outOccupied = growU8(this.outOccupied, n);
    this.#errPos = growF32(this.#errPos, n * 3);
    this.#errHead = growF32(this.#errHead, n);
    const eid = new Uint32Array(n).fill(NO_ACTOR);
    eid.set(this.#errId, 0);
    this.#errId = eid;
    this.#capacity = n;
  }

  /** Forget every snapshot and the clock estimate (a resync, a seek, or a fresh `Hello`). */
  reset(): void {
    this.#dropHistory();
    this.#outCount = 0;
    this.outOccupied.fill(0);
    this.outActorId.fill(NO_ACTOR);
    this.#dirtyTo = 0;
    this.#intervalSeconds = this.#nominalInterval;
    this.#resetClock();
    this.#clearErrors();
  }

  #dropHistory(): void {
    for (const s of this.#ring) {
      s.valid = false;
      s.count = 0;
    }
    this.#size = 0;
  }

  #clearErrors(): void {
    this.#errId.fill(NO_ACTOR);
    this.#anyError = false;
    this.#rebaseAtPrevious = false;
    this.#rebaseAtCurrent = false;
  }

  #resetClock(): void {
    this.#anchored = false;
    this.#anchorSim = 0;
    this.#anchorClock = 0;
    this.#rate = 1;
    this.#rateHead = 0;
    this.#rateCount = 0;
    this.#intervalSamples = 0;
    this.#lastSampleClock = Number.NaN;
    this.#renderSim = Number.NaN;
  }

  /** The clock map: the sim time the stream is believed to have reached at `clockSeconds`. */
  simAtClock(clockSeconds: number): number {
    if (!this.#anchored) return this.#size > 0 ? this.#ring[this.#head].simSeconds : 0;
    return this.#anchorSim + (clockSeconds - this.#anchorClock) * this.#rate;
  }

  /** The `k`-th newest snapshot (0 = newest); only meaningful for `k < #size`. */
  #at(k: number): PoseSnapshot {
    return this.#ring[(this.#head - k + HISTORY * 2) % HISTORY];
  }

  /**
   * Take a snapshot of `poses` as it stands now. Call this once per applied keyframe or delta, not
   * once per frame: the interpolator's whole job is to fill the gaps between these calls.
   *
   * `clockSeconds` is the viewer's monotonic clock, the same one passed to {@link sample}. It dates
   * the *arrival*; the snapshot itself is dated by `poses.simTimeNs`.
   */
  capture(poses: PoseBuffer, clockSeconds: number): void {
    const count = poses.count;
    this.ensureCapacity(Math.max(count, 1));
    const simSeconds = Number(poses.simTimeNs) / 1e9;

    let target: PoseSnapshot;
    if (this.#size > 0) {
      const newest = this.#ring[this.#head];
      const dSim = simSeconds - newest.simSeconds;
      const jump = Math.max(DISCONTINUITY_SECONDS, DISCONTINUITY_STEPS * this.#intervalSeconds);
      if (Math.abs(dSim) <= 1e-6) {
        // The same instant again: a keyframe repeating a step, or a refused delta. Replace the
        // newest in place, keeping its (earliest) arrival time; the clock map learns nothing new.
        this.#fill(newest, poses, count, newest.clockSeconds, simSeconds);
        this.#rebaseAtPrevious = true;
        return;
      }
      if (dSim < 0 || dSim >= jump) {
        // A seek, a rewind, a fresh GOP origin or a replay restart: the history describes a
        // different stretch of the run and the clock map means nothing now.
        this.#dropHistory();
        this.#resetClock();
        this.#intervalSeconds = this.#nominalInterval;
        this.#clearErrors();
        this.#snapAll = true;
      } else {
        // The interval is measured in SIM time (§3.4: whole `mobility_step_ns` steps), so arrival
        // jitter cannot shrink it and slow playback cannot stretch it. The first few intervals are
        // taken outright, afterwards a shorter interval wins immediately (the cadence is the
        // minimum; a longer gap is a dropped or merged frame) while a longer one is followed slowly.
        if (this.#intervalSamples < 3 || dSim < this.#intervalSeconds) this.#intervalSeconds = dSim;
        else this.#intervalSeconds += (dSim - this.#intervalSeconds) * 0.2;
        this.#intervalSamples++;
        this.#rebaseAtPrevious = true;
      }
    }
    this.#head = (this.#head + 1) % HISTORY;
    target = this.#ring[this.#head];
    if (this.#size < HISTORY) this.#size++;
    this.#updateRate(clockSeconds, simSeconds);
    this.#updateClockAnchor(clockSeconds, simSeconds);
    this.#fill(target, poses, count, clockSeconds, simSeconds);
  }

  #fill(s: PoseSnapshot, poses: PoseBuffer, count: number, clockSeconds: number, simSeconds: number): void {
    s.clockSeconds = clockSeconds;
    s.simSeconds = simSeconds;
    s.count = count;
    s.valid = true;
    if (count > 0) {
      s.position.set(poses.positions.subarray(0, count * 3), 0);
      s.heading.set(poses.headings.subarray(0, count), 0);
      s.speed.set(poses.speeds.subarray(0, count), 0);
      s.actorId.set(poses.actorId.subarray(0, count), 0);
      s.classIdx.set(poses.classIdx.subarray(0, count), 0);
      s.state.set(poses.state.subarray(0, count), 0);
      s.occupied.set(poses.occupied.subarray(0, count), 0);
      for (let i = 0; i < count; i++) s.accel[i] = poses.accelCq[i] / ACCEL_SCALE;
    }
    // Slots above the new high-water mark are not live.
    if (s.occupied.length > count) s.occupied.fill(0, count);
  }

  /**
   * Measure `rate` — sim seconds per clock second, i.e. `run.speed` — over the longest baseline in
   * the ring, not between adjacent snapshots: with a ±40 ms jitter on a 100 ms cadence the per-pair
   * ratio swings between 0.55 and 5, while over a multi-second baseline the same jitter is a couple
   * of per cent. A short baseline is blended with the estimate already held.
   */
  #updateRate(clockSeconds: number, simSeconds: number): void {
    const i = (this.#rateHead + this.#rateCount) % RATE_RING;
    if (this.#rateCount < RATE_RING) {
      this.#rateCount++;
    } else {
      this.#rateHead = (this.#rateHead + 1) % RATE_RING;
    }
    this.#rateClock[i] = clockSeconds;
    this.#rateSim[i] = simSeconds;
    const window = Math.max(RATE_WINDOW_SECONDS, RATE_WINDOW_STEPS * this.#intervalSeconds);
    while (this.#rateCount > 2 && clockSeconds - this.#rateClock[this.#rateHead] > window) {
      this.#rateHead = (this.#rateHead + 1) % RATE_RING;
      this.#rateCount--;
    }
    if (this.#rateCount < 2) return;
    const h = this.#rateHead;
    const baseline = clockSeconds - this.#rateClock[h];
    if (!(baseline > 1e-3)) return;
    const measured = (simSeconds - this.#rateSim[h]) / baseline;
    if (!Number.isFinite(measured) || measured <= 0) return;
    const trusted = Math.max(RATE_BASELINE_SECONDS, RATE_BASELINE_STEPS * this.#intervalSeconds);
    const w = Math.min(1, baseline / trusted);
    const blended = this.#rate * (1 - w) + measured * w;
    this.#rate = Math.min(64, Math.max(1 / 64, blended));
  }

  /**
   * Minimum-delay clock filter. An arrival that beats the line re-anchors it outright, because a
   * frame can only ever be *late*; a late one moves it by `clockGain` of the error, which is what
   * lets the line follow a genuine slowdown (or a rate estimate that started out wrong).
   */
  #updateClockAnchor(clockSeconds: number, simSeconds: number): void {
    if (!this.#anchored || !Number.isFinite(clockSeconds)) {
      this.#anchored = true;
      this.#anchorSim = simSeconds;
      this.#anchorClock = clockSeconds;
      return;
    }
    const before = this.#anchorSim + (clockSeconds - this.#anchorClock) * this.#rate;
    const error = simSeconds - before;
    this.#anchorClock = clockSeconds;
    this.#anchorSim = error > 0 ? simSeconds : before + error * this.clockGain;
  }

  /**
   * Fill `out*` with the pose at `clockSeconds`. Returns diagnostics; the arrays are the ones
   * exposed as `outPosition` and friends.
   */
  sample(clockSeconds: number): SampleInfo {
    const frameDt = Number.isFinite(this.#lastSampleClock)
      ? Math.max(0, Math.min(0.5, clockSeconds - this.#lastSampleClock))
      : 0;

    if (this.#size === 0) {
      this.#clearTo(0);
      this.#outCount = 0;
      this.#renderSim = Number.NaN;
      this.#lastSampleClock = clockSeconds;
      this.#lastInfo = {
        alpha: 0, delaySeconds: 0, intervalSeconds: this.#intervalSeconds, stalled: true, count: 0,
        snapped: 0, renderSimSeconds: 0, rate: this.#rate,
      };
      return this.#lastInfo;
    }

    const newest = this.#ring[this.#head];
    const oldest = this.#at(this.#size - 1);
    const interval = Math.max(1e-3, this.#intervalSeconds);
    // Arrival time is used for exactly one thing: deciding the stream has gone quiet. A stall is
    // relative to the cadence, or a 2 s cadence would read as permanently stalled.
    const silence = clockSeconds - newest.clockSeconds;
    const stalled = silence > Math.max(this.stallSeconds, this.stallSteps * interval);

    const span = newest.simSeconds - oldest.simSeconds;
    const delay = Math.min(interval * this.delaySteps, interval * this.maxDelaySteps, this.maxDelaySeconds, span);
    const extrapolationLimit = this.#held
      ? 0
      : Math.min(interval * this.maxExtrapolationSteps, this.maxExtrapolationSeconds);

    // Where the render clock goes this frame.
    const previousRender = this.#renderSim;
    let renderSim: number;
    if (this.#held) {
      this.#lastSampleClock = clockSeconds;
      renderSim = newest.simSeconds;
    } else if (this.#size === 1) {
      this.#lastSampleClock = clockSeconds;
      renderSim = newest.simSeconds;
    } else {
      const target = this.simAtClock(clockSeconds) - delay;
      renderSim = this.#advanceRenderClock(clockSeconds, target);
      const catchUp = Math.max(CATCH_UP_SECONDS, CATCH_UP_STEPS * interval);
      if (
        Number.isFinite(previousRender)
        && target - renderSim > catchUp
        && newest.simSeconds - renderSim > catchUp
      ) {
        // Far behind data that has already arrived: a forward seek inside the discontinuity window.
        // Slewing at 10 % would lag by the whole gap for ten times its length; re-seat instead and
        // let the blend (or the snap) take the difference. Never on a stall — there the target runs
        // ahead of the data and the glide below is the right answer.
        renderSim = Math.min(target, newest.simSeconds);
        this.#rebaseAtCurrent = true;
      }
      // Outside the history, glide to a stop instead of being clipped (Q7).
      const over = renderSim - newest.simSeconds;
      if (over > 0) {
        renderSim = newest.simSeconds + extrapolationEase(over, extrapolationLimit);
      } else {
        const under = oldest.simSeconds - renderSim;
        if (under > 0) renderSim = oldest.simSeconds - extrapolationEase(under, extrapolationLimit);
      }
      // Monotone: the pose decelerates to a freeze, it is never dragged backwards (Q7). Not on the
      // first sample after a reset, and not across a re-seat, which is a deliberate jump.
      if (Number.isFinite(previousRender) && !this.#rebaseAtCurrent && !(renderSim >= previousRender)) {
        renderSim = previousRender;
      }
    }
    this.#renderSim = renderSim;

    // Which segment brackets the render time: `a` older, `b` newer (`b` === newest when past it).
    const bk = this.#bracket(renderSim);
    const b = this.#at(bk);
    const a = this.#size > 1 ? this.#at(bk + 1) : b;
    const segSpan = b.simSeconds - a.simSeconds;
    let alpha = segSpan > 1e-9 ? (renderSim - a.simSeconds) / segSpan : 1;
    const alphaCap = 1 + this.maxExtrapolationSteps;
    if (alpha > alphaCap) alpha = alphaCap;
    else if (alpha < -this.maxExtrapolationSteps) alpha = -this.maxExtrapolationSteps;

    const count = newest.count;
    this.#outCount = count;

    // Re-base the correction blend where the underlying function changed under the picture.
    const rebaseAt = this.#rebaseAtCurrent
      ? renderSim
      : this.#rebaseAtPrevious && Number.isFinite(previousRender) ? previousRender : Number.NaN;
    this.#rebaseAtCurrent = false;
    this.#rebaseAtPrevious = false;
    const rebase = Number.isFinite(rebaseAt) && this.#outCount > 0;
    const rebaseBk = rebase ? this.#bracket(rebaseAt) : 0;
    const decay = frameDt > 0 ? Math.exp(-frameDt / ERROR_BLEND_SECONDS) : 1;
    const blendMax2 = this.blendMaxMetres * this.blendMaxMetres;
    const raw = this.#raw;
    const snapAll = this.#snapAll;
    this.#snapAll = false;
    let snapped = 0;
    let anyError = false;

    const oPos = this.outPosition;
    const oHead = this.outHeading;
    const ePos = this.#errPos;
    const eHead = this.#errHead;
    const eId = this.#errId;

    for (let i = 0; i < count; i++) {
      const live = newest.occupied[i];
      if (!live) {
        this.outOccupied[i] = 0;
        this.outActorId[i] = NO_ACTOR;
        eId[i] = NO_ACTOR;
        continue;
      }
      const id = newest.actorId[i];
      const wasShown = this.outOccupied[i] === 1 && this.outActorId[i] === id;
      const p = i * 3;

      if (rebase && wasShown) {
        // What is on screen now, against what the new data says for the same instant.
        const shownX = oPos[p];
        const shownY = oPos[p + 1];
        const shownZ = oPos[p + 2];
        const shownH = oHead[i];
        if (this.#evaluate(i, id, rebaseAt, rebaseBk, raw) >= 0) {
          const dx = shownX - raw[0];
          const dy = shownY - raw[1];
          const dz = shownZ - raw[2];
          if (dx * dx + dy * dy + dz * dz <= blendMax2) {
            ePos[p] = dx;
            ePos[p + 1] = dy;
            ePos[p + 2] = dz;
            eHead[i] = wrapAngle(shownH - raw[3]);
            eId[i] = id;
          } else {
            eId[i] = NO_ACTOR;
          }
        }
      }

      this.outOccupied[i] = 1;
      this.outActorId[i] = id;
      this.outClassIdx[i] = newest.classIdx[i];
      this.outState[i] = newest.state[i];
      this.outSpeed[i] = newest.speed[i];

      let kind = this.#evaluate(i, id, renderSim, bk, raw);
      if (snapAll) kind = 1;
      if (kind === 1) snapped++;
      let x = raw[0];
      let y = raw[1];
      let z = raw[2];
      let h = raw[3];
      if (eId[i] === id) {
        if (kind === 1) {
          // A snap is a real discontinuity; never smear it.
          eId[i] = NO_ACTOR;
        } else {
          const ex = ePos[p] * decay;
          const ey = ePos[p + 1] * decay;
          const ez = ePos[p + 2] * decay;
          const eh = eHead[i] * decay;
          if (ex * ex + ey * ey + ez * ez < 1e-8 && Math.abs(eh) < 1e-5) {
            eId[i] = NO_ACTOR;
          } else {
            ePos[p] = ex;
            ePos[p + 1] = ey;
            ePos[p + 2] = ez;
            eHead[i] = eh;
            x += ex;
            y += ey;
            z += ez;
            h += eh;
            anyError = true;
          }
        }
      }
      oPos[p] = x;
      oPos[p + 1] = y;
      oPos[p + 2] = z;
      oHead[i] = wrapAngle(h);
    }
    this.#anyError = anyError;
    this.#clearTo(count);

    this.#lastInfo = {
      alpha, delaySeconds: delay, intervalSeconds: interval, stalled, count, snapped,
      renderSimSeconds: renderSim, rate: this.#rate,
    };
    return this.#lastInfo;
  }

  /** Whether any correction blend is still decaying (diagnostic). */
  get blending(): boolean {
    return this.#anyError;
  }

  /**
   * The raw pose of slot `i` (actor `id`) at sim time `t` from the history, written into `out` as
   * `[x, y, z, heading]`. Returns 0 when interpolated or extrapolated, 1 when snapped (no continuous
   * pair holds this actor, or it teleported), −1 when no snapshot holds it at all.
   */
  #evaluate(i: number, id: number, t: number, bracket: number, out: Float64Array): number {
    const size = this.#size;
    let bk = bracket;
    let b = this.#at(bk);
    // The actor may have appeared after `b`: use the earliest snapshot that holds it, snapped.
    if (!(i < b.count && b.occupied[i] === 1 && b.actorId[i] === id)) {
      for (let k = bk - 1; k >= 0; k--) {
        const s = this.#at(k);
        if (i < s.count && s.occupied[i] === 1 && s.actorId[i] === id) {
          b = s;
          bk = k;
          break;
        }
      }
      if (!(i < b.count && b.occupied[i] === 1 && b.actorId[i] === id)) return -1;
      this.#write(b, i, out);
      return t < b.simSeconds - 1e-9 ? 1 : this.#deadReckon(b, i, t, 0, out);
    }
    const aOk = size > bk + 1;
    const a = aOk ? this.#at(bk + 1) : b;
    const continuous = aOk && i < a.count && a.occupied[i] === 1 && a.actorId[i] === id;
    if (!continuous) {
      // Spawned at `b` (or the history is one deep): hold `b`, or dead-reckon forward from it.
      this.#write(b, i, out);
      if (t > b.simSeconds) return this.#deadReckon(b, i, t, 0, out);
      return aOk && t < b.simSeconds - 1e-9 ? 1 : 0;
    }

    const p = i * 3;
    const T = b.simSeconds - a.simSeconds;
    const ax = a.position[p];
    const ay = a.position[p + 1];
    const az = a.position[p + 2];
    const bx = b.position[p];
    const by = b.position[p + 1];
    const bz = b.position[p + 2];
    const cx = bx - ax;
    const cy = by - ay;
    const cz = bz - az;
    const chord2 = cx * cx + cy * cy + cz * cz;
    const vmax = Math.max(Math.abs(a.speed[i]), Math.abs(b.speed[i]));
    const tele = Math.max(this.teleportMetres, 1.5 * vmax * T + 2);
    if (chord2 > tele * tele || !(T > 1e-9)) {
      this.#write(b, i, out);
      return 1;
    }

    if (t >= b.simSeconds || t <= a.simSeconds) {
      // Outside the history: dead-reckon with a constant turn rate and speed (the CTRV model,
      // Schubert, Richter & Wanielik 2008, "Comparison and evaluation of advanced motion models for
      // vehicle tracking", FUSION), which continues the Hermite curve's end tangent and keeps a
      // turning vehicle on its arc rather than sending it straight on into the kerb.
      const omega = Math.max(-MAX_YAW_RATE, Math.min(MAX_YAW_RATE, wrapAngle(b.heading[i] - a.heading[i]) / T));
      const from = t >= b.simSeconds ? b : a;
      this.#write(from, i, out);
      return this.#deadReckon(from, i, t, omega, out);
    }

    const u = (t - a.simSeconds) / T;
    const ha = a.heading[i];
    const dh = wrapAngle(b.heading[i] - ha);
    // A segment whose length disagrees with the speeds reported at its ends is not motion the
    // tangents describe — measured on manhattan-5min, 2.7 % of steps: a vehicle held still for half
    // a second while reporting 8.5 m/s, then moved 5.6 m in one step. A Hermite curve through such
    // a step compresses the jump into the middle of the interval (peak 1.5x the chord speed); a
    // constant speed along the chord is the least violent faithful rendering of it.
    const chord = Math.sqrt(chord2);
    const expected = 0.5 * (Math.abs(a.speed[i]) + Math.abs(b.speed[i])) * T;
    const consistent = Math.abs(chord - expected) <= Math.max(0.3, 0.35 * expected);
    if (this.curve === "linear" || !consistent) {
      out[0] = ax + cx * u;
      out[1] = ay + cy * u;
      out[2] = az + cz * u;
      out[3] = ha + dh * u;
      return 0;
    }

    // Position: cubic Hermite with the reported velocities as tangents, limited against the chord
    // (Fritsch–Carlson) so an inconsistent speed can neither overshoot nor loop.
    const sa = a.speed[i] * T;
    const sb = b.speed[i] * T;
    const hb = b.heading[i];
    limitTangent2(Math.cos(ha) * sa, Math.sin(ha) * sa, cx, cy);
    const m0x = TAN[0];
    const m0y = TAN[1];
    limitTangent2(Math.cos(hb) * sb, Math.sin(hb) * sb, cx, cy);
    const m1x = TAN[0];
    const m1y = TAN[1];
    const u2 = u * u;
    const u3 = u2 * u;
    const h00 = 2 * u3 - 3 * u2 + 1;
    const h10 = u3 - 2 * u2 + u;
    const h01 = -2 * u3 + 3 * u2;
    const h11 = u3 - u2;
    out[0] = h00 * ax + h10 * m0x + h01 * bx + h11 * m1x;
    out[1] = h00 * ay + h10 * m0y + h01 * by + h11 * m1y;
    out[2] = az + cz * u;

    // Heading: monotone cubic on the unwrapped angle, Catmull–Rom tangents from the neighbours.
    let ta = dh;
    let tb = dh;
    const olderK = bk + 2;
    if (olderK < size) {
      const o = this.#at(olderK);
      if (i < o.count && o.occupied[i] === 1 && o.actorId[i] === id && a.simSeconds - o.simSeconds > 1e-9) {
        const prevRate = wrapAngle(ha - o.heading[i]) / (a.simSeconds - o.simSeconds);
        ta = 0.5 * (prevRate * T + dh);
      }
    }
    if (bk >= 1) {
      const n = this.#at(bk - 1);
      if (i < n.count && n.occupied[i] === 1 && n.actorId[i] === id && n.simSeconds - b.simSeconds > 1e-9) {
        const nextRate = wrapAngle(n.heading[i] - hb) / (n.simSeconds - b.simSeconds);
        tb = 0.5 * (dh + nextRate * T);
      }
    }
    ta = limitTangent(ta, dh);
    tb = limitTangent(tb, dh);
    out[3] = ha + h10 * ta + h01 * dh + h11 * tb;
    return 0;
  }

  /**
   * Index (0 = newest) of the newer snapshot of the segment that brackets `t`: the newest when `t`
   * is past it, the second oldest when `t` is before the oldest.
   */
  #bracket(t: number): number {
    let bk = 0;
    for (let k = 0; k < this.#size - 1; k++) {
      bk = k;
      if (this.#at(k + 1).simSeconds <= t) break;
    }
    return bk;
  }

  /** Copy snapshot `s`'s pose of slot `i` into `out`. */
  #write(s: PoseSnapshot, i: number, out: Float64Array): void {
    const p = i * 3;
    out[0] = s.position[p];
    out[1] = s.position[p + 1];
    out[2] = s.position[p + 2];
    out[3] = s.heading[i];
  }

  /**
   * Move `out` (already `s`'s pose) to time `t` — forwards or backwards — at `s`'s speed and the
   * yaw rate `omega`. Returns 0.
   */
  #deadReckon(s: PoseSnapshot, i: number, t: number, omega: number, out: Float64Array): number {
    const dt = t - s.simSeconds;
    if (dt === 0) return 0;
    const v = s.speed[i];
    const h0 = s.heading[i];
    const turn = omega * dt;
    if (Math.abs(turn) < 1e-4) {
      out[0] += Math.cos(h0) * v * dt;
      out[1] += Math.sin(h0) * v * dt;
    } else {
      const r = v / omega;
      const h1 = h0 + turn;
      out[0] += r * (Math.sin(h1) - Math.sin(h0));
      out[1] += r * (Math.cos(h0) - Math.cos(h1));
      out[3] = h1;
    }
    return 0;
  }

  /**
   * Advance the render clock towards `target`, at the stream's rate ± {@link SLEW_FRACTION}.
   *
   * This is the whole of the jitter rejection. The clock never *takes* the estimate's value, it
   * only ever runs forward at `rate · dt` with a bounded correction towards it, so an estimator
   * that jumps changes the render clock's rate by at most a few per cent and never its position.
   */
  #advanceRenderClock(clockSeconds: number, target: number): number {
    const last = this.#lastSampleClock;
    this.#lastSampleClock = clockSeconds;
    if (!Number.isFinite(this.#renderSim)) return target;
    let dt = clockSeconds - last;
    if (!Number.isFinite(dt) || dt < 0) dt = 0;
    if (dt > 0.5) dt = 0.5;
    const nominal = this.#rate * dt;
    const bound = SLEW_FRACTION * nominal;
    // The error is measured against where the nominal advance lands, not against the previous
    // position: measured against the previous position it includes the nominal advance itself, and
    // the clock settles one whole frame *ahead* of its target — 17 ms at 60 fps, 83 ms at the 12 fps
    // a software renderer manages, which on a 100 ms cadence is most of the interpolation margin
    // spent on extrapolating.
    const advanced = this.#renderSim + nominal;
    const err = target - advanced;
    return advanced + Math.max(-bound, Math.min(bound, err));
  }

  /**
   * Zero the occupancy of every slot this interpolator wrote above `count`, and nothing else.
   *
   * Capacity comes from `Hello.actor_capacity`, not from the live actor count, so clearing to
   * capacity means a 20,000-slot run with 20 actors pays for 20,000 stores a frame (Q18).
   */
  #clearTo(count: number): void {
    if (this.#dirtyTo > count) {
      this.outOccupied.fill(0, count, this.#dirtyTo);
      this.outActorId.fill(NO_ACTOR, count, this.#dirtyTo);
      this.#errId.fill(NO_ACTOR, count, this.#dirtyTo);
    }
    this.#dirtyTo = count;
  }
}
