/**
 * Pose interpolation.
 *
 * Deltas arrive at the mobility step (10 Hz in the reference scenarios, `Hello.mobility_step_ns`,
 * §3.1) but the renderer runs at 60 fps. 09-ui §2 calls for "the render loop samples the ring at its
 * own cadence and interpolates between the two latest samples"; this is that, without the
 * `SharedArrayBuffer` ring (the viewer is handed a `PoseBuffer` that the client or the worker has
 * already brought up to date, and takes its own snapshot of it).
 *
 * Two snapshots are kept and swapped, so steady-state capture and sampling allocate nothing.
 *
 * ## The clock (this is the whole design)
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
 * the error. Network jitter is one-sided noise on top of a transport delay, so the minimum is the
 * estimate that jitter cannot move, and `rate` (sim seconds per clock second, which is exactly
 * `run.speed`) is measured over a multi-second baseline so slow playback is followed without ever
 * being confused for a stall.
 *
 * The render clock then *follows* that line rather than taking its value: it advances at `rate · dt`
 * with a bounded correction towards the estimate, so a re-anchor, a new rate or a re-measured
 * interval changes the clock's speed by a few per cent and never its position.
 *
 * ## Delay, extrapolation and the stall
 *
 * Every window is a multiple of the **measured snapshot interval**, never an absolute number of
 * seconds: a 2 s cadence (0.05x playback, a keyframe-only run, a congested link) has to interpolate
 * exactly as well as a 0.1 s one, and an absolute cap smaller than the interval guarantees the
 * opposite — the sampler runs out of segment, freezes, then jumps. The absolute options survive only
 * as an outer safety bound far above any expected cadence.
 *
 * Outside the segment the two snapshots span the sampler extrapolates, but it *glides to a stop*
 * rather than being clipped: the render clock's advance decays linearly to zero and the total
 * overshoot converges to {@link PoseInterpolatorOptions.maxExtrapolationSteps} intervals. Being
 * continuous, and monotone by construction, this can never snap an actor backwards — a paused
 * engine or a dropped socket ends with every actor standing still a bounded distance past its last
 * reported pose, which is the failure mode the clamp exists to prevent. The same ease applies below
 * the older snapshot, so an early arrival cannot drag the clock forward either.
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
   * extrapolation for a jitter-free stream. Default 1.0.
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
   * rather than interpolated (a `run.seek`, a GOP origin change, or a slot reused after despawn).
   * Metres. Default 40.
   */
  readonly teleportMetres?: number;
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
}

/** What one {@link PoseInterpolator.sample} produced, for the caller's bookkeeping. */
export interface SampleInfo {
  /** Blend factor actually used; > 1 means the sampler extrapolated. */
  readonly alpha: number;
  /** Render delay applied, seconds. */
  readonly delaySeconds: number;
  /** Measured interval between the two snapshots, seconds of **sim** time. */
  readonly intervalSeconds: number;
  /** True when the stream is considered stalled. */
  readonly stalled: boolean;
  /** Slot high-water mark of the sampled output. */
  readonly count: number;
  /** Number of slots whose actor id changed between the snapshots and were therefore snapped. */
  readonly snapped: number;
  /** The sim time this sample rendered, seconds. */
  readonly renderSimSeconds: number;
  /** Estimated sim seconds per viewer-clock second (1 at normal speed, 0.5 at 0.5x playback). */
  readonly rate: number;
}

function makeSnapshot(capacity: number): PoseSnapshot {
  return {
    clockSeconds: 0,
    simSeconds: 0,
    count: 0,
    position: new Float32Array(capacity * 3),
    heading: new Float32Array(capacity),
    speed: new Float32Array(capacity),
    accel: new Float32Array(capacity),
    actorId: new Uint32Array(capacity).fill(0xffffffff),
    classIdx: new Uint8Array(capacity),
    state: new Uint8Array(capacity),
    occupied: new Uint8Array(capacity),
    valid: false,
  };
}

function growSnapshot(s: PoseSnapshot, capacity: number): void {
  if (s.position.length >= capacity * 3) return;
  const pos = new Float32Array(capacity * 3);
  pos.set(s.position, 0);
  s.position = pos;
  const grow1 = (a: Float32Array): Float32Array => {
    const n = new Float32Array(capacity);
    n.set(a, 0);
    return n;
  };
  s.heading = grow1(s.heading);
  s.speed = grow1(s.speed);
  s.accel = grow1(s.accel);
  const id = new Uint32Array(capacity).fill(0xffffffff);
  id.set(s.actorId, 0);
  s.actorId = id;
  const growU8 = (a: Uint8Array): Uint8Array => {
    const n = new Uint8Array(capacity);
    n.set(a, 0);
    return n;
  };
  s.classIdx = growU8(s.classIdx);
  s.state = growU8(s.state);
  s.occupied = growU8(s.occupied);
}

/** Shortest-arc interpolation between two angles in radians. */
export function lerpAngle(a: number, b: number, t: number): number {
  let d = b - a;
  const TAU = Math.PI * 2;
  d -= Math.floor(d / TAU + 0.5) * TAU;
  return a + d * t;
}

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

/**
 * How far past the newest snapshot a render clock that wants to be `over` seconds past it is
 * actually allowed to be, given an overshoot budget of `limit` seconds.
 *
 * Full rate up to `0.7 · limit` — ordinary jitter-driven extrapolation must not be throttled, or the
 * throttling itself becomes motion — then the advance rate decays linearly to zero at `1.3 · limit`
 * of *wanted* overshoot, which at 60 fps is a stop spread over four frames rather than one. This is that rate's integral, so it is continuous, `C¹` at both knees,
 * monotonically non-decreasing (a pose can never be pulled backwards, Q7) and saturates at exactly
 * `limit`: a dropped socket leaves every actor standing still `limit` seconds past its last
 * reported pose, never further.
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
 * Keeps the two most recent pose snapshots and produces a smooth pose for any render time between
 * them. All output arrays are preallocated and reused; {@link sample} allocates nothing.
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

  #prev: PoseSnapshot;
  #next: PoseSnapshot;
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
  readonly clockGain: number;

  constructor(options: PoseInterpolatorOptions = {}) {
    this.#capacity = Math.max(16, options.capacity ?? 1024);
    this.delaySteps = options.delaySteps ?? 1;
    this.maxDelaySteps = options.maxDelaySteps ?? 2;
    this.maxDelaySeconds = options.maxDelaySeconds ?? 5;
    this.maxExtrapolationSteps = options.maxExtrapolationSteps ?? 1.2;
    this.maxExtrapolationSeconds = options.maxExtrapolationSeconds ?? 5;
    this.stallSteps = options.stallSteps ?? 3;
    this.stallSeconds = options.stallSeconds ?? 0.4;
    this.teleportMetres = options.teleportMetres ?? 40;
    this.clockGain = Math.min(1, Math.max(0, options.clockGain ?? 0.05));
    this.#nominalInterval = Math.max(1e-3, options.intervalSeconds ?? 0.1);
    this.#intervalSeconds = this.#nominalInterval;
    this.#prev = makeSnapshot(this.#capacity);
    this.#next = makeSnapshot(this.#capacity);
    this.outPosition = new Float32Array(this.#capacity * 3);
    this.outHeading = new Float32Array(this.#capacity);
    this.outSpeed = new Float32Array(this.#capacity);
    this.outActorId = new Uint32Array(this.#capacity).fill(0xffffffff);
    this.outClassIdx = new Uint8Array(this.#capacity);
    this.outState = new Uint8Array(this.#capacity);
    this.outOccupied = new Uint8Array(this.#capacity);
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

  /** True once two snapshots exist and sampling is meaningful. */
  get ready(): boolean {
    return this.#next.valid;
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
    if (!this.#prev.valid) this.#intervalSeconds = this.#nominalInterval;
  }

  /**
   * Grow every array to hold `capacity` slots. Called from {@link capture}; growth doubles, so a
   * stream that settles at a stable actor count stops reallocating after the first few keyframes.
   */
  ensureCapacity(capacity: number): void {
    if (capacity <= this.#capacity) return;
    let n = this.#capacity;
    while (n < capacity) n *= 2;
    growSnapshot(this.#prev, n);
    growSnapshot(this.#next, n);
    const pos = new Float32Array(n * 3);
    pos.set(this.outPosition, 0);
    this.outPosition = pos;
    const head = new Float32Array(n);
    head.set(this.outHeading, 0);
    this.outHeading = head;
    const spd = new Float32Array(n);
    spd.set(this.outSpeed, 0);
    this.outSpeed = spd;
    const aid = new Uint32Array(n).fill(0xffffffff);
    aid.set(this.outActorId, 0);
    this.outActorId = aid;
    const cls = new Uint8Array(n);
    cls.set(this.outClassIdx, 0);
    this.outClassIdx = cls;
    const st = new Uint8Array(n);
    st.set(this.outState, 0);
    this.outState = st;
    const occ = new Uint8Array(n);
    occ.set(this.outOccupied, 0);
    this.outOccupied = occ;
    this.#capacity = n;
  }

  /** Forget both snapshots and the clock estimate (a resync, a seek, or a fresh `Hello`). */
  reset(): void {
    this.#prev.valid = false;
    this.#next.valid = false;
    this.#prev.count = 0;
    this.#next.count = 0;
    this.#outCount = 0;
    this.outOccupied.fill(0);
    this.outActorId.fill(0xffffffff);
    this.#dirtyTo = 0;
    this.#intervalSeconds = this.#nominalInterval;
    this.#resetClock();
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
    if (!this.#anchored) return this.#next.valid ? this.#next.simSeconds : 0;
    return this.#anchorSim + (clockSeconds - this.#anchorClock) * this.#rate;
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
    const older = this.#prev;
    const newer = this.#next;
    // Swap: the old `next` becomes `prev`, and we overwrite the old `prev` in place.
    this.#prev = newer;
    this.#next = older;
    const s = this.#next;
    const simSeconds = Number(poses.simTimeNs) / 1e9;

    if (this.#prev.valid) {
      const dSim = simSeconds - this.#prev.simSeconds;
      const dClock = clockSeconds - this.#prev.clockSeconds;
      if (dSim < 0 || dSim >= 5) {
        // A seek, a fresh GOP origin or a replay restart: the clock map means nothing now.
        this.#resetClock();
        this.#intervalSeconds = this.#nominalInterval;
        this.#intervalSamples = 0;
      } else if (dSim > 1e-4) {
        // The interval is measured in SIM time (§3.4: whole `mobility_step_ns` steps), so arrival
        // jitter cannot shrink it and slow playback cannot stretch it. The first few intervals are
        // taken outright — the sim cadence is exact, and starting from a wrong nominal value is
        // itself a source of frozen frames — and afterwards a shorter interval wins immediately
        // (the cadence is the minimum; a longer gap is a dropped or merged frame) while a longer
        // one is followed slowly.
        if (this.#intervalSamples < 3 || dSim < this.#intervalSeconds) this.#intervalSeconds = dSim;
        else this.#intervalSeconds += (dSim - this.#intervalSeconds) * 0.2;
        this.#intervalSamples++;
      }
    }
    this.#updateRate(clockSeconds, simSeconds);
    this.#updateClockAnchor(clockSeconds, simSeconds);

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
   * the ring, not between adjacent snapshots.
   *
   * Adjacent snapshots are the wrong baseline: with a ±40 ms jitter on a 100 ms cadence the
   * per-pair ratio swings between 0.55 and 5, and no amount of averaging makes that estimate good
   * enough, because a rate that is wrong by a few per cent makes the clock map fall behind between
   * anchors and the poses stutter. Over a multi-second baseline the same jitter is a couple of per
   * cent, and a short baseline is blended with the estimate already held.
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
    // Retire history older than the window. The window is a duration *and* a number of intervals:
    // arrival jitter is a fraction of the interval, so the baseline has to span several intervals
    // before the ratio of two jittered endpoints means anything.
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
   * Fill `out*` with the pose at `clockSeconds`, interpolating between the two snapshots.
   * Returns diagnostics; the arrays are the ones exposed as `outPosition` and friends.
   */
  sample(clockSeconds: number): SampleInfo {
    const prev = this.#prev;
    const next = this.#next;

    if (!next.valid) {
      this.#clearTo(0);
      this.#outCount = 0;
      this.#renderSim = Number.NaN;
      this.#lastInfo = {
        alpha: 0, delaySeconds: 0, intervalSeconds: this.#intervalSeconds, stalled: true, count: 0,
        snapped: 0, renderSimSeconds: 0, rate: this.#rate,
      };
      return this.#lastInfo;
    }

    const interval = Math.max(1e-3, this.#intervalSeconds);
    // Arrival time is used for exactly one thing: deciding the stream has gone quiet. A stall is
    // relative to the cadence, or a 2 s cadence would read as permanently stalled.
    const silence = clockSeconds - next.clockSeconds;
    const stalled = silence > Math.max(this.stallSeconds, this.stallSteps * interval);

    if (!prev.valid) {
      this.#copySnapshot(next);
      this.#renderSim = next.simSeconds;
      this.#lastInfo = {
        alpha: 1, delaySeconds: 0, intervalSeconds: interval, stalled, count: next.count, snapped: 0,
        renderSimSeconds: this.#renderSim, rate: this.#rate,
      };
      return this.#lastInfo;
    }

    const span = Math.max(1e-6, next.simSeconds - prev.simSeconds);
    // Every window is a multiple of the measured interval; the absolute values are only an outer
    // safety bound (Q2: an absolute cap below the interval breaks every slow stream). The delay is
    // also capped by the segment actually held — two snapshots is all there is, and a delay longer
    // than the gap between them would render before the older one on every frame.
    const delay = Math.min(
      interval * this.delaySteps,
      interval * this.maxDelaySteps,
      this.maxDelaySeconds,
      span,
    );
    const extrapolationLimit = Math.min(
      interval * this.maxExtrapolationSteps,
      this.maxExtrapolationSeconds,
    );

    // Where the sim clock says we should be rendering, one render delay in the past.
    const target = this.simAtClock(clockSeconds) - delay;
    let renderSim = this.#advanceRenderClock(clockSeconds, target);
    // Outside the segment the two snapshots span, glide to a stop instead of being clipped (Q7).
    // Symmetric on purpose: a segment is a constant-velocity model of the motion, and sampling a
    // little before the older snapshot is exactly as well-founded as sampling a little past the
    // newer one. Clamping the clock to the segment instead would drag it — a forward *jump* of up
    // to a whole interval on the frame a snapshot arrives early.
    const over = renderSim - next.simSeconds;
    if (over > 0) {
      renderSim = next.simSeconds + extrapolationEase(over, extrapolationLimit);
    } else {
      const under = prev.simSeconds - renderSim;
      if (under > 0) renderSim = prev.simSeconds - extrapolationEase(under, extrapolationLimit);
    }
    // Monotone: the pose decelerates to a freeze, it is never dragged backwards (Q7). Not on the
    // first sample after a reset, where there is no previous value to be monotone against.
    const previous = this.#renderSim;
    if (Number.isFinite(previous) && !(renderSim >= previous)) renderSim = previous;
    this.#renderSim = renderSim;

    let alpha = (renderSim - prev.simSeconds) / span;
    // Defence in depth: two snapshots at the same sim time make `span` meaningless.
    const alphaCap = 1 + this.maxExtrapolationSteps;
    if (alpha > alphaCap) alpha = alphaCap;
    else if (alpha < -this.maxExtrapolationSteps) alpha = -this.maxExtrapolationSteps;

    const count = Math.max(prev.count, next.count);
    this.#outCount = count;
    let snapped = 0;

    const pPos = prev.position;
    const nPos = next.position;
    const pHead = prev.heading;
    const nHead = next.heading;
    const oPos = this.outPosition;
    const oHead = this.outHeading;
    const teleport2 = this.teleportMetres * this.teleportMetres;

    for (let i = 0; i < count; i++) {
      const live = i < next.count ? next.occupied[i] : 0;
      this.outOccupied[i] = live;
      if (!live) {
        this.outActorId[i] = 0xffffffff;
        continue;
      }
      const id = next.actorId[i];
      this.outActorId[i] = id;
      this.outClassIdx[i] = next.classIdx[i];
      this.outState[i] = next.state[i];
      this.outSpeed[i] = next.speed[i];

      const p = i * 3;
      const continuous = i < prev.count && prev.occupied[i] === 1 && prev.actorId[i] === id;
      if (!continuous) {
        oPos[p] = nPos[p];
        oPos[p + 1] = nPos[p + 1];
        oPos[p + 2] = nPos[p + 2];
        oHead[i] = nHead[i];
        snapped++;
        continue;
      }
      const ax = pPos[p];
      const ay = pPos[p + 1];
      const az = pPos[p + 2];
      const bx = nPos[p];
      const by = nPos[p + 1];
      const bz = nPos[p + 2];
      const dx = bx - ax;
      const dy = by - ay;
      const dz = bz - az;
      if (dx * dx + dy * dy + dz * dz > teleport2) {
        oPos[p] = bx;
        oPos[p + 1] = by;
        oPos[p + 2] = bz;
        oHead[i] = nHead[i];
        snapped++;
        continue;
      }
      oPos[p] = ax + dx * alpha;
      oPos[p + 1] = ay + dy * alpha;
      oPos[p + 2] = az + dz * alpha;
      oHead[i] = lerpAngle(pHead[i], nHead[i], alpha);
    }
    this.#clearTo(count);

    this.#lastInfo = {
      alpha, delaySeconds: delay, intervalSeconds: interval, stalled, count, snapped,
      renderSimSeconds: renderSim, rate: this.#rate,
    };
    return this.#lastInfo;
  }

  /**
   * Advance the render clock towards `target`, at the stream's rate ± {@link SLEW_FRACTION}.
   *
   * This is the whole of the jitter rejection. The clock never *takes* the estimate's value, it
   * only ever runs forward at `rate · dt` with a bounded correction towards it, so an estimator
   * that jumps — a re-anchor, a new rate, a changed delay, a re-measured interval — changes the
   * render clock's rate by at most a few per cent and never its position. Monotone by construction,
   * because the correction can never exceed the nominal advance.
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
    const err = target - this.#renderSim;
    return this.#renderSim + nominal + Math.max(-bound, Math.min(bound, err));
  }

  /**
   * Zero the occupancy of every slot this interpolator wrote above `count`, and nothing else.
   *
   * Capacity comes from `Hello.actor_capacity`, not from the live actor count, so clearing to
   * capacity means a 20,000-slot run with 20 actors pays for 20,000 stores a frame (Q18). Slots
   * above the high-water mark were never written, so they are already zero.
   */
  #clearTo(count: number): void {
    if (this.#dirtyTo > count) this.outOccupied.fill(0, count, this.#dirtyTo);
    this.#dirtyTo = count;
  }

  #copySnapshot(s: PoseSnapshot): void {
    const n = s.count;
    this.#outCount = n;
    if (n > 0) {
      this.outPosition.set(s.position.subarray(0, n * 3), 0);
      this.outHeading.set(s.heading.subarray(0, n), 0);
      this.outSpeed.set(s.speed.subarray(0, n), 0);
      this.outActorId.set(s.actorId.subarray(0, n), 0);
      this.outClassIdx.set(s.classIdx.subarray(0, n), 0);
      this.outState.set(s.state.subarray(0, n), 0);
      this.outOccupied.set(s.occupied.subarray(0, n), 0);
    }
    this.#clearTo(n);
  }
}
