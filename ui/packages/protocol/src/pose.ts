/**
 * Pose quantisation and dequantisation — docs/protocol/vwp-v1.md §3.2, §3.3, §3.4.
 *
 * The rules this file implements, exactly as the specification states them:
 *
 * - **Keyframe** x, y are `i32` **millimetres** about `Keyframe.origin_{x,y}_m`; z is `i16`
 *   **centimetres** about `origin_z_m`.
 * - **Delta** dx, dy, dz are `i16` millimetres **about the previously delivered quantised value**
 *   of the same field in the same GOP — never about the engine's true state. That is what stops
 *   quantisation error accumulating (§3.2 "Delta reference rule", conformance `no_quantisation_drift`).
 * - The z delta is `dz_mm`, **millimetres**, while the absolute z of a keyframe, a spawn and the
 *   delta absolute block is `z_cm`, **centimetres** (§3.2 table, §3.3.2 col 5, §3.4.2 col 4,
 *   §3.4.3, §3.4.5 col 7). The two units meet in the delta reference rule, so this client keeps its
 *   z reference in **millimetres** ({@link PoseBuffer.zMm}) and seeds it with `z_cm · 10` from every
 *   absolute source; `zCm` is a derived mirror. Adding `dz_mm` to a centimetre accumulator would be
 *   a 10x error on every vertical delta. `ui/README.md` § "Known spec issues" records the erratum
 *   this resolution depends on.
 * - A step whose |dx|, |dy| or |dz| would exceed 32,000 mm sets `MFLAG_ABSOLUTE` and carries the
 *   absolute pose in the delta's absolute block instead (§3.2 "Escape hatch", `teleport_escape`).
 * - Heading is `u16` **binary radians**: `rad = brad · 2π / 65536`.
 * - Speed is `i16` at **1/128 m/s**; acceleration is `i16` at **1/64 m/s²**.
 */

import { ProtocolError, SENTINEL_U32 } from "./frame.js";
import { ActorState, type DeltaMessage, type KeyframeMessage, MovedFlags } from "./messages.js";

/** §3.2 — absolute x, y are millimetres. */
export const MM_PER_M = 1000;
/** §3.2 — absolute z is centimetres. */
export const CM_PER_M = 100;
/** §3.2/§3.4.2 — millimetres per centimetre: `dz_mm` against an absolute `z_cm`. */
export const MM_PER_CM = 10;
/** §3.2 — headings are binary radians: a full turn is 65536. */
export const BRAD_PER_TURN = 65536;
/** §3.2 — speed scale, 1/128 m/s. */
export const SPEED_SCALE = 128;
/** §3.2 — acceleration scale, 1/64 m/s². */
export const ACCEL_SCALE = 64;
/** §3.2 — signal time-to-change is deciseconds. */
export const DS_PER_S = 10;
/** §3.2 — a delta component beyond this many mm must use the absolute escape. */
export const DELTA_ESCAPE_MM = 32_000;

/**
 * Hard ceiling on the slot ids this client will honour — the same `1 << 20` the connection state
 * machine already applies to `Hello.actor_capacity`-driven growth.
 *
 * §3.1.1 calls `actor_capacity` "max concurrent actor slots for the run; a preallocation hint", and
 * nothing in §3.4.5 asks a client to honour a slot id above it. Without a ceiling an untrusted
 * `slot` from one malformed `Delta` drives the doubling loop in {@link PoseBuffer.ensureCapacity}:
 * `slot = 3_000_000_000` allocates 14 typed-array columns plus a 3-wide `Float32Array` for
 * 4,294,967,296 slots (~116 GB), i.e. a memory-exhaustion vector from a single frame.
 */
export const MAX_ACTOR_SLOTS = 1 << 20;

/** Radians per binary radian. */
export const RAD_PER_BRAD = (2 * Math.PI) / BRAD_PER_TURN;

const I32_MIN = -2_147_483_648;
const I32_MAX = 2_147_483_647;
const I16_MIN = -32_768;
const I16_MAX = 32_767;

/** Clamp to `i16` instead of letting an `Int16Array` store wrap. */
function clampI16(v: number): number {
  return v < I16_MIN ? I16_MIN : v > I16_MAX ? I16_MAX : v;
}

/**
 * §3.2 — `round_half_away_from_zero`. `Math.round` rounds half **up** (−0.5 → −0), which would
 * disagree with the Rust producer on exactly the negative half-way values, so it is not used.
 */
export function roundHalfAwayFromZero(v: number): number {
  return v >= 0 ? Math.floor(v + 0.5) : Math.ceil(v - 0.5);
}

/** §3.2 — `q(v, scale) = clamp(round_half_away_from_zero(v * scale), TYPE_MIN, TYPE_MAX)`. */
export function quantise(v: number, scale: number, min: number, max: number): number {
  const q = roundHalfAwayFromZero(v * scale);
  return q < min ? min : q > max ? max : q;
}

/** Quantise an x or y offset from the keyframe origin, in metres, to `i32` millimetres. */
export function quantisePositionMm(metresFromOrigin: number): number {
  return quantise(metresFromOrigin, MM_PER_M, I32_MIN, I32_MAX);
}

/** Quantise a z offset from the keyframe origin, in metres, to `i16` centimetres. */
export function quantiseHeightCm(metresFromOrigin: number): number {
  return quantise(metresFromOrigin, CM_PER_M, I16_MIN, I16_MAX);
}

/**
 * §3.2 — `heading_brad = ((round_half_away_from_zero(rad · 65536 / 2π) % 65536) + 65536) % 65536`.
 * Binary radians wrap by construction, so there is no ±π branch.
 */
export function quantiseHeadingBrad(rad: number): number {
  const raw = roundHalfAwayFromZero((rad * BRAD_PER_TURN) / (2 * Math.PI));
  return ((raw % BRAD_PER_TURN) + BRAD_PER_TURN) % BRAD_PER_TURN;
}

/** Quantise a speed in m/s to `i16` at 1/128 m/s. */
export function quantiseSpeedCq(mps: number): number {
  return quantise(mps, SPEED_SCALE, I16_MIN, I16_MAX);
}

/** Quantise a longitudinal acceleration in m/s² to `i16` at 1/64 m/s². */
export function quantiseAccelCq(mps2: number): number {
  return quantise(mps2, ACCEL_SCALE, I16_MIN, I16_MAX);
}

/** Quantise a time-to-change in seconds to `u16` deciseconds (`0xFFFF` = unknown). */
export function quantiseTimeToChangeDs(seconds: number): number {
  const q = roundHalfAwayFromZero(seconds * DS_PER_S);
  return q < 0 ? 0 : q > 65_534 ? 65_534 : q;
}

/** Millimetres about the keyframe origin back to ENU metres. */
export function dequantisePositionM(mm: number, originM: number): number {
  return originM + mm / MM_PER_M;
}

/** Centimetres about the keyframe origin back to ENU metres. */
export function dequantiseHeightM(cm: number, originM: number): number {
  return originM + cm / CM_PER_M;
}

/** §3.2 — `rad = brad · 2π / 65536`. */
export function dequantiseHeadingRad(brad: number): number {
  return brad * RAD_PER_BRAD;
}

/** 1/128 m/s back to m/s. */
export function dequantiseSpeedMps(cq: number): number {
  return cq / SPEED_SCALE;
}

/** 1/64 m/s² back to m/s². */
export function dequantiseAccelMps2(cq: number): number {
  return cq / ACCEL_SCALE;
}

/** Deciseconds back to seconds; `0xFFFF` (unknown) yields `NaN`. */
export function dequantiseTimeToChangeS(ds: number): number {
  return ds === 0xffff ? Number.NaN : ds / DS_PER_S;
}

/**
 * §3.2 — does this step need the `MFLAG_ABSOLUTE` escape? True when any component of the
 * quantised displacement exceeds ±32,000 mm (a teleport, a `run.seek`, a mobility command).
 */
export function needsAbsoluteEscape(dxMm: number, dyMm: number, dzMm: number): boolean {
  return Math.abs(dxMm) > DELTA_ESCAPE_MM || Math.abs(dyMm) > DELTA_ESCAPE_MM || Math.abs(dzMm) > DELTA_ESCAPE_MM;
}

/** Why {@link PoseBuffer.applyDelta} refused a delta. */
export type DeltaRejectReason = "no-keyframe" | "gop-mismatch" | "step-gap";

/** Outcome of {@link PoseBuffer.applyDelta}. */
export type DeltaApplyResult =
  | { readonly applied: true; readonly moved: number; readonly spawned: number; readonly despawned: number }
  | { readonly applied: false; readonly reason: DeltaRejectReason };

/**
 * The client's mirror of the server's quantised actor state, per §3.3/§3.4.
 *
 * It keeps the **quantised** value of every field per actor slot, exactly as the producer does, so
 * `Keyframe + all deltas of the GOP` reproduces the server's state bit for bit and no error
 * accumulates. Alongside it maintains dequantised `Float32Array`s a renderer can upload or read
 * directly — `positions` is `[x, y, z]` per slot in world-local ENU metres, `headings` is radians.
 */
export class PoseBuffer {
  #capacity: number;
  #slotBound = MAX_ACTOR_SLOTS;
  #count = 0;
  #gopIndex = -1;
  #stepIndex = 0;
  #hasKeyframe = false;
  #simTimeNs = 0n;

  /** Quantisation origin of the current GOP (§3.3.1). */
  originXM = 0;
  originYM = 0;
  originZM = 0;

  /** `0xFFFFFFFF` marks an empty slot (§3.3.1). */ actorId: Uint32Array;
  xMm: Int32Array;
  yMm: Int32Array;
  /**
   * Height in **millimetres** above `originZM` — the authoritative z reference, because `dz_mm`
   * (§3.4.2) is millimetres while every absolute z on the wire is `z_cm` (§3.3.2, §3.4.3, §3.4.5).
   * Seeded as `z_cm · 10`, advanced by `dz_mm`.
   */
  zMm: Int32Array;
  /** Height in centimetres, **derived** from {@link zMm} — the unit the wire uses for absolute z. */
  zCm: Int16Array;
  headingBrad: Uint16Array;
  speedCq: Int16Array;
  accelCq: Int16Array;
  laneId: Uint32Array;
  classIdx: Uint8Array;
  state: Uint8Array;
  verifiedNeighbors: Uint8Array;

  /** Dequantised world-local ENU metres, 3 floats (x, y, z) per slot. */ positions: Float32Array;
  /** Dequantised heading in radians, 1 float per slot. */ headings: Float32Array;
  /** Dequantised speed in m/s, 1 float per slot. */ speeds: Float32Array;
  /** 1 where the slot holds a live actor. */ occupied: Uint8Array;

  constructor(capacity = 1024) {
    this.#capacity = Math.max(1, capacity);
    const c = this.#capacity;
    this.actorId = new Uint32Array(c).fill(SENTINEL_U32);
    this.xMm = new Int32Array(c);
    this.yMm = new Int32Array(c);
    this.zMm = new Int32Array(c);
    this.zCm = new Int16Array(c);
    this.headingBrad = new Uint16Array(c);
    this.speedCq = new Int16Array(c);
    this.accelCq = new Int16Array(c);
    this.laneId = new Uint32Array(c).fill(SENTINEL_U32);
    this.classIdx = new Uint8Array(c);
    this.state = new Uint8Array(c);
    this.verifiedNeighbors = new Uint8Array(c);
    this.positions = new Float32Array(c * 3);
    this.headings = new Float32Array(c);
    this.speeds = new Float32Array(c);
    this.occupied = new Uint8Array(c);
  }

  /** Allocated slot capacity. */
  get capacity(): number {
    return this.#capacity;
  }

  /**
   * The highest slot id + 1 a wire frame may name (§3.4.5, §3.1.1), capped at
   * {@link MAX_ACTOR_SLOTS}. Seeded from `Hello.actor_capacity` via {@link setSlotBound}; never
   * below the currently allocated capacity, so a keyframe that legitimately grew the buffer cannot
   * make its own occupied slots unreachable.
   */
  get slotLimit(): number {
    return Math.max(this.#slotBound, this.#capacity);
  }

  /**
   * Set the wire-slot bound from `Hello.actor_capacity` (§3.1.1). Clamped to
   * {@link MAX_ACTOR_SLOTS}; a later `Hello` replaces it, and {@link slotLimit} never drops below
   * the capacity already allocated.
   */
  setSlotBound(actorCapacity: number): void {
    if (!Number.isFinite(actorCapacity) || actorCapacity <= 0) return;
    this.#slotBound = Math.min(MAX_ACTOR_SLOTS, Math.floor(actorCapacity));
  }

  /** §3.4.5 / §3.1.1 — reject a wire slot id beyond {@link slotLimit} before anything is allocated. */
  #checkSlot(slot: number, what: string): void {
    const limit = this.slotLimit;
    if (!Number.isInteger(slot) || slot < 0 || slot >= limit) {
      throw new ProtocolError("bad_offset", `Delta ${what} slot ${slot} is at or beyond the actor-capacity bound ${limit} (§3.1.1)`, {
        offset: slot,
        expected: limit,
        actual: slot,
        field: what,
      });
    }
  }

  /** Slot high-water mark + 1, as the last keyframe or delta left it. */
  get count(): number {
    return this.#count;
  }

  /** GOP the buffer is currently inside, or −1 before the first keyframe. */
  get gopIndex(): number {
    return this.#gopIndex;
  }

  /** Last applied `step_index` within the GOP; 0 right after a keyframe. */
  get stepIndex(): number {
    return this.#stepIndex;
  }

  /** Sim time of the last applied frame. */
  get simTimeNs(): bigint {
    return this.#simTimeNs;
  }

  /** False until a keyframe has seeded the buffer; deltas are refused until then (§3.4). */
  get hasKeyframe(): boolean {
    return this.#hasKeyframe;
  }

  /** Grow to hold at least `capacity` slots, preserving contents. */
  ensureCapacity(capacity: number): void {
    if (capacity <= this.#capacity) return;
    let next = this.#capacity;
    while (next < capacity) next *= 2;
    const old = this.#capacity;

    const actorId = new Uint32Array(next);
    actorId.set(this.actorId, 0);
    actorId.fill(SENTINEL_U32, old);
    const laneId = new Uint32Array(next);
    laneId.set(this.laneId, 0);
    laneId.fill(SENTINEL_U32, old);
    const xMm = new Int32Array(next);
    xMm.set(this.xMm, 0);
    const yMm = new Int32Array(next);
    yMm.set(this.yMm, 0);
    const zMm = new Int32Array(next);
    zMm.set(this.zMm, 0);
    const zCm = new Int16Array(next);
    zCm.set(this.zCm, 0);
    const headingBrad = new Uint16Array(next);
    headingBrad.set(this.headingBrad, 0);
    const speedCq = new Int16Array(next);
    speedCq.set(this.speedCq, 0);
    const accelCq = new Int16Array(next);
    accelCq.set(this.accelCq, 0);
    const classIdx = new Uint8Array(next);
    classIdx.set(this.classIdx, 0);
    const state = new Uint8Array(next);
    state.set(this.state, 0);
    const verifiedNeighbors = new Uint8Array(next);
    verifiedNeighbors.set(this.verifiedNeighbors, 0);
    const positions = new Float32Array(next * 3);
    positions.set(this.positions, 0);
    const headings = new Float32Array(next);
    headings.set(this.headings, 0);
    const speeds = new Float32Array(next);
    speeds.set(this.speeds, 0);
    const occupied = new Uint8Array(next);
    occupied.set(this.occupied, 0);

    this.actorId = actorId;
    this.laneId = laneId;
    this.xMm = xMm;
    this.yMm = yMm;
    this.zMm = zMm;
    this.zCm = zCm;
    this.headingBrad = headingBrad;
    this.speedCq = speedCq;
    this.accelCq = accelCq;
    this.classIdx = classIdx;
    this.state = state;
    this.verifiedNeighbors = verifiedNeighbors;
    this.positions = positions;
    this.headings = headings;
    this.speeds = speeds;
    this.occupied = occupied;
    this.#capacity = next;
  }

  /** Drop all stream state (a non-resumed `Hello`, §1.4 case 2). */
  reset(): void {
    this.actorId.fill(SENTINEL_U32);
    this.laneId.fill(SENTINEL_U32);
    this.occupied.fill(0);
    this.#count = 0;
    this.#gopIndex = -1;
    this.#stepIndex = 0;
    this.#hasKeyframe = false;
    this.#simTimeNs = 0n;
  }

  /** Recompute the dequantised mirrors of one slot. */
  #refresh(slot: number): void {
    const p = slot * 3;
    this.positions[p] = this.originXM + this.xMm[slot] / MM_PER_M;
    this.positions[p + 1] = this.originYM + this.yMm[slot] / MM_PER_M;
    const zMm = this.zMm[slot];
    this.zCm[slot] = clampI16(roundHalfAwayFromZero(zMm / MM_PER_CM));
    this.positions[p + 2] = this.originZM + zMm / MM_PER_M;
    this.headings[slot] = this.headingBrad[slot] * RAD_PER_BRAD;
    this.speeds[slot] = this.speedCq[slot] / SPEED_SCALE;
  }

  /**
   * Seed the buffer from a `Keyframe` (§3.3). A keyframe is an idempotent snapshot: every slot
   * `0..actor_count-1` is overwritten and every slot above it is released.
   */
  applyKeyframe(kf: KeyframeMessage): void {
    const A = kf.actors.count;
    this.ensureCapacity(Math.max(A, 1));
    this.originXM = kf.originXM;
    this.originYM = kf.originYM;
    this.originZM = kf.originZM;
    const a = kf.actors;
    for (let s = 0; s < A; s++) {
      const id = a.actorId[s];
      this.actorId[s] = id;
      this.xMm[s] = a.xMm[s];
      this.yMm[s] = a.yMm[s];
      this.zMm[s] = a.zCm[s] * MM_PER_CM;
      this.headingBrad[s] = a.headingBrad[s];
      this.speedCq[s] = a.speedCq[s];
      this.accelCq[s] = a.accelCq[s];
      this.laneId[s] = a.laneId[s];
      this.classIdx[s] = a.classIdx[s];
      this.state[s] = a.state[s];
      this.verifiedNeighbors[s] = a.verifiedNeighbors[s];
      this.occupied[s] = id === SENTINEL_U32 ? 0 : 1;
      this.#refresh(s);
    }
    for (let s = A; s < this.#count; s++) {
      this.occupied[s] = 0;
      this.actorId[s] = SENTINEL_U32;
    }
    this.#count = A;
    this.#gopIndex = kf.gopIndex;
    this.#stepIndex = 0;
    this.#hasKeyframe = true;
    this.#simTimeNs = kf.simTimeNs;
  }

  /**
   * Apply a `Delta` (§3.4). Refused — without mutating anything — when the buffer holds no
   * keyframe, when `gop_index` does not match, or when a `step_index` was skipped: §3.4 says a
   * client that sees a gap MUST drop deltas until the next `Keyframe`, because a surviving delta
   * after a dropped one would be applied against the wrong base.
   *
   * **All-or-nothing.** A malformed delta (a row with `MFLAG_ABSOLUTE` or `MFLAG_LANE_CHANGED`
   * whose block is short, a slot beyond the buffer, a slot beyond the §3.1.1 actor-capacity bound)
   * throws a {@link ProtocolError} from a validation pre-pass, leaving the buffer exactly as the
   * previous frame left it — including `stepIndex` and `simTimeNs`. §1.5 and §3.4 require deltas
   * never to be partially applied; a half-applied one leaves the ring against a base no frame
   * describes.
   */
  applyDelta(d: DeltaMessage): DeltaApplyResult {
    if (!this.#hasKeyframe) return { applied: false, reason: "no-keyframe" };
    if (d.gopIndex !== this.#gopIndex) return { applied: false, reason: "gop-mismatch" };
    if (d.stepIndex !== this.#stepIndex + 1) return { applied: false, reason: "step-gap" };

    const sp = d.spawns;
    const m = d.moved;

    // -----------------------------------------------------------------------
    // Validation pre-pass. §1.5/§3.4: "deltas are never partially applied" — a delta is meaningful
    // only against its own GOP, so a throw from inside the apply loops would leave the buffer in a
    // state neither the keyframe nor keyframe+delta describes, and the next delta would be applied
    // on top of it. Everything that can be refused is refused here, before a single column is
    // written; the loops below cannot throw.
    // -----------------------------------------------------------------------

    // §3.4.5 / §3.1.1 — an untrusted slot id must not drive allocation.
    let growTo = 0;
    for (let i = 0; i < sp.count; i++) {
      const s = sp.slot[i];
      this.#checkSlot(s, "spawns");
      if (s + 1 > growTo) growTo = s + 1;
    }
    // The bound moved rows are checked against: the current capacity, or the capacity the spawns
    // above will grow it to.
    const bound = Math.max(this.#capacity, growTo);
    let absNeeded = 0;
    let laneNeeded = 0;
    for (let i = 0; i < m.count; i++) {
      const s = m.slot[i];
      this.#checkSlot(s, "moved");
      if (s >= bound) {
        throw new ProtocolError("bad_state", `Delta moves slot ${s} beyond capacity ${bound}`, { offset: s });
      }
      const mf = m.mflags[i];
      if ((mf & MovedFlags.ABSOLUTE) !== 0) absNeeded++;
      if ((mf & MovedFlags.LANE_CHANGED) !== 0) laneNeeded++;
    }
    if (absNeeded > d.absolute.count) {
      throw new ProtocolError(
        "bad_state",
        `Delta sets MFLAG_ABSOLUTE on ${absNeeded} moved rows but the absolute block holds only ${d.absolute.count} entries (§3.4.3)`,
        { expected: absNeeded, actual: d.absolute.count, field: "Delta.absolute" },
      );
    }
    if (laneNeeded > d.lanes.length) {
      throw new ProtocolError(
        "bad_state",
        `Delta sets MFLAG_LANE_CHANGED on ${laneNeeded} moved rows but the lane block holds only ${d.lanes.length} entries (§3.4.4)`,
        { expected: laneNeeded, actual: d.lanes.length, field: "Delta.lanes" },
      );
    }

    // Spawns first: a spawn may occupy a slot beyond the current high-water mark.
    for (let i = 0; i < sp.count; i++) {
      const s = sp.slot[i];
      this.ensureCapacity(s + 1);
      this.actorId[s] = sp.actorId[i];
      this.xMm[s] = sp.xMm[i];
      this.yMm[s] = sp.yMm[i];
      this.zMm[s] = sp.zCm[i] * MM_PER_CM;
      this.headingBrad[s] = sp.headingBrad[i];
      this.speedCq[s] = sp.speedCq[i];
      this.accelCq[s] = 0;
      this.laneId[s] = sp.laneId[i];
      this.classIdx[s] = sp.classIdx[i];
      this.state[s] = sp.state[i];
      this.verifiedNeighbors[s] = sp.verifiedNeighbors[i];
      this.occupied[s] = 1;
      if (s + 1 > this.#count) this.#count = s + 1;
      this.#refresh(s);
    }

    let absCursor = 0;
    let laneCursor = 0;
    for (let i = 0; i < m.count; i++) {
      const s = m.slot[i];
      const mf = m.mflags[i];
      if ((mf & MovedFlags.ABSOLUTE) !== 0) {
        // §3.4.3 — teleport escape: ignore dx/dy/dz, take the absolute pose (x/y mm, z cm).
        this.xMm[s] = d.absolute.xMm(absCursor);
        this.yMm[s] = d.absolute.yMm(absCursor);
        this.zMm[s] = d.absolute.zCm(absCursor) * MM_PER_CM;
        absCursor++;
      } else {
        // §3.4.2 — dx/dy/dz are millimetres about the previously delivered quantised value;
        // z is accumulated in millimetres because `dz_mm` is millimetres, not centimetres.
        this.xMm[s] += m.dxMm[i];
        this.yMm[s] += m.dyMm[i];
        this.zMm[s] += m.dzMm[i];
      }
      if ((mf & MovedFlags.LANE_CHANGED) !== 0) {
        this.laneId[s] = d.lanes[laneCursor++];
      }
      this.headingBrad[s] = m.headingBrad[i];
      this.speedCq[s] = m.speedCq[i];
      this.accelCq[s] = m.accelCq[i];
      this.state[s] = m.state[i];
      this.verifiedNeighbors[s] = m.verifiedNeighbors[i];
      this.occupied[s] = 1;
      this.#refresh(s);
    }

    const dp = d.despawns;
    for (let i = 0; i < dp.count; i++) {
      const s = dp.slot[i];
      if (s < this.#capacity) {
        this.occupied[s] = 0;
        this.actorId[s] = SENTINEL_U32;
        this.laneId[s] = SENTINEL_U32;
      }
    }

    this.#stepIndex = d.stepIndex;
    this.#simTimeNs = d.simTimeNs;
    return { applied: true, moved: m.count, spawned: sp.count, despawned: dp.count };
  }

  /** World-local ENU position of a slot, in metres, at full `f64` precision. */
  positionOf(slot: number): { x: number; y: number; z: number } {
    return {
      x: this.originXM + this.xMm[slot] / MM_PER_M,
      y: this.originYM + this.yMm[slot] / MM_PER_M,
      z: this.originZM + this.zMm[slot] / MM_PER_M,
    };
  }

  /** Heading of a slot in radians (0 = east, counter-clockwise). */
  headingOf(slot: number): number {
    return this.headingBrad[slot] * RAD_PER_BRAD;
  }

  /** Speed of a slot in m/s. */
  speedOf(slot: number): number {
    return this.speedCq[slot] / SPEED_SCALE;
  }

  /** Longitudinal acceleration of a slot in m/s² (GT; `0` under the `node` profile). */
  accelOf(slot: number): number {
    return this.accelCq[slot] / ACCEL_SCALE;
  }

  /** §3.3.4 — the actor is neither an attacker, nor reported, nor revoked. */
  isBenign(slot: number): boolean {
    return (this.state[slot] & (ActorState.ATTACKER | ActorState.REPORTED | ActorState.REVOKED)) === 0;
  }

  /** Slots currently holding a live actor. */
  occupiedSlots(): number[] {
    const out: number[] = [];
    for (let s = 0; s < this.#count; s++) if (this.occupied[s] === 1) out.push(s);
    return out;
  }
}
