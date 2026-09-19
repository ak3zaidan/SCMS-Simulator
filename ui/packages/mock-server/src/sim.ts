/**
 * The mock run: actors driving the generated grid, a signal plan, and the canonical frame stream.
 *
 * Everything the specification makes normative is implemented here rather than approximated:
 *
 * - §3.2 delta references are the **previously transmitted quantised** values, kept per slot in
 *   `refXMm`/`refYMm`/`refZCm`, so a client that applies keyframe + deltas reproduces this state
 *   exactly and quantisation error never accumulates (conformance Q2).
 * - §3.2's escape hatch sets `MFLAG_ABSOLUTE` whenever a step would move a slot more than
 *   32,000 mm — which is what a respawn or a `run.seek` does (conformance Q3).
 * - §3.3.1 slots are the lowest free slot at spawn and are not reused until one full keyframe
 *   period after despawn (conformance Q4, Q5).
 * - §5.2's blanking is applied at the producer, per connection profile, before serialisation.
 */

import {
  ActorState,
  type ActorRowInit,
  type DespawnRowInit,
  type EventRecordInit,
  type MetricRecordInit,
  type MovedRowInit,
  type NodeTelemetryInit,
  type SignalRowInit,
  type SpawnRowInit,
  ChannelId,
  DELTA_ESCAPE_MM,
  MM_PER_CM,
  MovedFlags,
  SENTINEL_U16,
  SENTINEL_U32,
  SENTINEL_U64,
  SlotTable,
  encodeDeltaBody,
  encodeKeyframeBody,
  needsAbsoluteEscape,
  quantiseAccelCq,
  quantiseHeadingBrad,
  quantiseHeightCm,
  quantisePositionMm,
  quantiseSpeedCq,
} from "@vwp/protocol";

import type { GeneratedWorld } from "./manhattan.js";
import { mulberry32, randInt, randRange } from "./rng.js";

/** Node ids `0..999` are reserved for infrastructure sites; actor nodes start here. */
export const ACTOR_NODE_BASE = 1000;

/** `full` keeps ground truth; `node` is the blind-evaluation profile of §5. */
export type Profile = "full" | "node";

/** Actor classes this fixture drives, matching the `Hello` class table it publishes. */
export const ACTOR_CLASSES = [
  { name: "car", lengthM: 4.5, widthM: 1.8, heightM: 1.5, colorRgba: 0x3b82f6ff, category: 0, share: 0.62 },
  { name: "taxi", lengthM: 4.8, widthM: 1.85, heightM: 1.55, colorRgba: 0xfacc15ff, category: 0, share: 0.14 },
  { name: "truck", lengthM: 9.5, widthM: 2.5, heightM: 3.4, colorRgba: 0xf59e0bff, category: 0, share: 0.08 },
  { name: "bus", lengthM: 12.2, widthM: 2.55, heightM: 3.2, colorRgba: 0xef4444ff, category: 0, share: 0.05 },
  { name: "moto", lengthM: 2.1, widthM: 0.8, heightM: 1.4, colorRgba: 0xa855f7ff, category: 0, share: 0.05 },
  { name: "bicycle", lengthM: 1.8, widthM: 0.6, heightM: 1.6, colorRgba: 0x22c55eff, category: 1, share: 0.04 },
  { name: "pedestrian", lengthM: 0.5, widthM: 0.5, heightM: 1.75, colorRgba: 0x10b981ff, category: 1, share: 0.02 },
] as const;

/** §3.3.3 phases this fixture uses, out of SAE J2735 `MovementPhaseState`. */
export const PHASE_STOP_AND_REMAIN = 3;
export const PHASE_PROTECTED_GREEN = 6;
export const PHASE_PROTECTED_CLEARANCE = 8;

/** Signal plan: a 60 s cycle, avenues (group 0) first, then cross-streets (group 1). */
const CYCLE_S = 60;
const PLAN: readonly { untilS: number; group0: number; group1: number }[] = [
  { untilS: 30, group0: PHASE_PROTECTED_GREEN, group1: PHASE_STOP_AND_REMAIN },
  { untilS: 34, group0: PHASE_PROTECTED_CLEARANCE, group1: PHASE_STOP_AND_REMAIN },
  { untilS: 56, group0: PHASE_STOP_AND_REMAIN, group1: PHASE_PROTECTED_GREEN },
  { untilS: 60, group0: PHASE_STOP_AND_REMAIN, group1: PHASE_PROTECTED_CLEARANCE },
];

/** The phase of both signal groups, and the seconds until the next change, at a sim time. */
export function signalPlanAt(simTimeNs: bigint): { group0: number; group1: number; secondsToChange: number } {
  const t = Number(simTimeNs / 1_000_000n) / 1000;
  const inCycle = ((t % CYCLE_S) + CYCLE_S) % CYCLE_S;
  for (const slice of PLAN) {
    if (inCycle < slice.untilS) {
      return { group0: slice.group0, group1: slice.group1, secondsToChange: slice.untilS - inCycle };
    }
  }
  const last = PLAN[PLAN.length - 1];
  return { group0: last.group0, group1: last.group1, secondsToChange: 0 };
}

/** One simulated actor. */
interface Actor {
  actorId: number;
  nodeId: number;
  slot: number;
  classIdx: number;
  laneIdx: number;
  /** Metres travelled along the current lane. */ s: number;
  speedMps: number;
  cruiseMps: number;
  accelMps2: number;
  state: number;
  verifiedNeighbors: number;
  equipped: boolean;
  alive: boolean;
  /**
   * While an actor crosses a junction it travels a short straight connector from the end of the
   * lane it left to the start of the lane it joined, so its position stays continuous. A real
   * engine has these as junction-internal lanes (§4.3 `lane_type = 5`); this fixture keeps them
   * server-side because §4's decision 21 keeps lane connectivity out of the payload anyway.
   */
  connector: { fromX: number; fromY: number; toX: number; toY: number; lengthM: number; s: number } | null;
}

/** Per-slot record of what was last **transmitted**, which is what deltas are relative to (§3.2). */
interface SlotRef {
  actorId: number;
  xMm: number;
  yMm: number;
  zCm: number;
  headingBrad: number;
  speedCq: number;
  accelCq: number;
  state: number;
  verifiedNeighbors: number;
  laneId: number;
  classIdx: number;
  occupied: boolean;
}

/** Options for {@link MockRun}. */
export interface MockRunOptions {
  readonly world: GeneratedWorld;
  readonly actors: number;
  readonly seed?: number;
  readonly mobilityStepNs?: bigint;
  readonly keyframePeriodNs?: bigint;
  readonly telemetryPeriodNs?: bigint;
  readonly metricPeriodNs?: bigint;
  readonly durationNs?: bigint;
}

/** A frame the run produced, ready to be wrapped in a §2.1 header by the server. */
export interface ProducedBody {
  readonly body: Uint8Array;
  readonly profile: Profile;
}

/** The run's advertised timing, which `Hello` publishes. */
export interface RunTiming {
  readonly mobilityStepNs: bigint;
  readonly keyframePeriodNs: bigint;
  readonly telemetryPeriodNs: bigint;
  readonly metricPeriodNs: bigint;
  readonly durationNs: bigint;
}

/**
 * The mock engine's run state: actors, signals, slots and the quantised reference state the
 * delta encoder needs.
 */
export class MockRun {
  readonly world: GeneratedWorld;
  readonly timing: RunTiming;
  readonly slots = new SlotTable(1024);

  #rand: () => number;
  #actors: Actor[] = [];
  #refs: SlotRef[] = [];
  #simTimeNs = 0n;
  #gopIndex = -1;
  #stepIndex = 0;
  #nextActorId = 0;
  #pendingSpawns: SpawnRowInit[] = [];
  #pendingDespawns: DespawnRowInit[] = [];
  #lastSignalPhases: { group0: number; group1: number } | null = null;
  #lastSignalTtcDs = -1;
  #msgId = 1;

  constructor(options: MockRunOptions) {
    this.world = options.world;
    this.#rand = mulberry32(options.seed ?? 1337);
    this.timing = {
      mobilityStepNs: options.mobilityStepNs ?? 100_000_000n,
      keyframePeriodNs: options.keyframePeriodNs ?? 1_000_000_000n,
      telemetryPeriodNs: options.telemetryPeriodNs ?? 1_000_000_000n,
      metricPeriodNs: options.metricPeriodNs ?? 1_000_000_000n,
      durationNs: options.durationNs ?? 3_600_000_000_000n,
    };
    for (let i = 0; i < options.actors; i++) this.#spawnActor(true);
  }

  /** Current sim time. */
  get simTimeNs(): bigint {
    return this.#simTimeNs;
  }

  /** GOP the stream is in (the keyframe ordinal), or −1 before the first keyframe. */
  get gopIndex(): number {
    return this.#gopIndex;
  }

  /** `step_index` within the current GOP. */
  get stepIndex(): number {
    return this.#stepIndex;
  }

  /** Live actors. */
  get actorCount(): number {
    return this.#actors.filter((a) => a.alive).length;
  }

  /** Slot high-water mark + 1 — the `actor_count` a keyframe carries (§3.3.1). */
  get slotCount(): number {
    return this.slots.count;
  }

  /** Every live actor's node id, for the `Hello` node table and `inspect.node`. */
  nodeIds(): number[] {
    return this.#actors.filter((a) => a.alive && a.equipped).map((a) => a.nodeId);
  }

  /** Look up an actor by node id. */
  actorForNode(nodeId: number): { actorId: number; classIdx: number; state: number; xM: number; yM: number; speedMps: number } | null {
    const actor = this.#actors.find((a) => a.alive && a.nodeId === nodeId);
    if (!actor) return null;
    const p = this.#positionOf(actor);
    return { actorId: actor.actorId, classIdx: actor.classIdx, state: actor.state, xM: p.x, yM: p.y, speedMps: actor.speedMps };
  }

  #classFor(rand: number): number {
    let acc = 0;
    for (let i = 0; i < ACTOR_CLASSES.length; i++) {
      acc += ACTOR_CLASSES[i].share;
      if (rand < acc) return i;
    }
    return 0;
  }

  #spawnActor(initial: boolean): void {
    const lanes = this.world.lanes;
    if (lanes.length === 0) return;
    const laneIdx = randInt(this.#rand, 0, lanes.length);
    const lane = lanes[laneIdx];
    const classIdx = this.#classFor(this.#rand());
    const actorId = this.#nextActorId++;
    const slot = this.slots.assign(actorId);
    // §5.2's last rule only bites when some actors are unequipped, so some are.
    const equipped = this.#rand() > 0.12;
    let state = equipped ? ActorState.EQUIPPED : 0;
    if (equipped && this.#rand() < 0.03) state |= ActorState.ATTACKER;
    if (equipped && this.#rand() < 0.02) state |= ActorState.REPORTED;
    if (equipped && this.#rand() < 0.008) state |= ActorState.REVOKED;
    if (equipped && this.#rand() < 0.05) state |= ActorState.GNSS_DEGRADED;
    const cruise = lane.speedLimitMps * randRange(this.#rand, 0.75, 1.08) * (classIdx === 6 ? 0.15 : classIdx === 5 ? 0.45 : 1);
    const actor: Actor = {
      actorId,
      nodeId: equipped ? ACTOR_NODE_BASE + actorId : SENTINEL_U32,
      slot,
      classIdx,
      laneIdx,
      s: randRange(this.#rand, 0, lane.lengthM),
      speedMps: cruise * randRange(this.#rand, 0.4, 1),
      cruiseMps: cruise,
      accelMps2: 0,
      state,
      verifiedNeighbors: equipped ? randInt(this.#rand, 0, 40) : 0,
      equipped,
      alive: true,
      connector: null,
    };
    this.#actors.push(actor);
    this.#ensureRef(slot);
    if (!initial) {
      const p = this.#positionOf(actor);
      this.#pendingSpawns.push({
        slot,
        actorId,
        nodeId: actor.nodeId,
        xMm: this.#qx(p.x),
        yMm: this.#qy(p.y),
        laneId: lane.laneId,
        zCm: this.#qz(0.15),
        headingBrad: quantiseHeadingBrad(lane.headingRad),
        speedCq: quantiseSpeedCq(actor.speedMps),
        cause: 2, // respawn
        classIdx,
        state,
        verifiedNeighbors: actor.verifiedNeighbors,
      });
    }
  }

  #ensureRef(slot: number): void {
    while (this.#refs.length <= slot) {
      this.#refs.push({
        actorId: SENTINEL_U32, xMm: 0, yMm: 0, zCm: 0, headingBrad: 0, speedCq: 0, accelCq: 0,
        state: 0, verifiedNeighbors: 0, laneId: SENTINEL_U32, classIdx: 0, occupied: false,
      });
    }
  }

  #qx = (xM: number): number => quantisePositionMm(xM - this.world.bboxM.minX);
  #qy = (yM: number): number => quantisePositionMm(yM - this.world.bboxM.minY);
  #qz = (zM: number): number => quantiseHeightCm(zM);

  #positionOf(actor: Actor): { x: number; y: number; headingRad: number } {
    const c = actor.connector;
    if (c !== null) {
      const t = c.lengthM > 0 ? Math.min(1, c.s / c.lengthM) : 1;
      return {
        x: c.fromX + (c.toX - c.fromX) * t,
        y: c.fromY + (c.toY - c.fromY) * t,
        headingRad: Math.atan2(c.toY - c.fromY, c.toX - c.fromX),
      };
    }
    const lane = this.world.lanes[actor.laneIdx];
    const t = lane.lengthM > 0 ? Math.min(1, actor.s / lane.lengthM) : 0;
    return {
      x: lane.x0 + (lane.x1 - lane.x0) * t,
      y: lane.y0 + (lane.y1 - lane.y0) * t,
      headingRad: lane.headingRad,
    };
  }

  /** Advance the world by one mobility step. */
  advance(): void {
    const dt = Number(this.timing.mobilityStepNs) / 1e9;
    this.#simTimeNs += this.timing.mobilityStepNs;
    const plan = signalPlanAt(this.#simTimeNs);
    const lanes = this.world.lanes;

    for (const actor of this.#actors) {
      if (!actor.alive) continue;
      const lane = lanes[actor.laneIdx];
      const phase = lane.signalGroup === 0 ? plan.group0 : plan.group1;
      const mustStop = phase !== PHASE_PROTECTED_GREEN;
      const distanceToStop = lane.lengthM - 6 - actor.s;

      let target = Math.min(actor.cruiseMps, lane.speedLimitMps * 1.1);
      if (mustStop && distanceToStop < 45) {
        // Brake smoothly for a stop line that is not green.
        target = distanceToStop <= 1 ? 0 : Math.min(target, Math.max(0, distanceToStop / 3));
      }
      const before = actor.speedMps;
      const accelLimit = target > actor.speedMps ? 1.8 : 3.4;
      const delta = target - actor.speedMps;
      actor.speedMps += Math.sign(delta) * Math.min(Math.abs(delta), accelLimit * dt);
      if (actor.speedMps < 0) actor.speedMps = 0;
      actor.accelMps2 = (actor.speedMps - before) / dt;

      let travel = actor.speedMps * dt;
      if (actor.connector !== null) {
        actor.connector.s += travel;
        if (actor.connector.s < actor.connector.lengthM) continue;
        travel = actor.connector.s - actor.connector.lengthM;
        actor.connector = null;
        actor.s = 0;
      }

      actor.s += travel;
      if (actor.s >= lane.lengthM) {
        // Cross the junction onto a successor, preferring to continue straight.
        const overshoot = actor.s - lane.lengthM;
        const successors = lane.successors;
        if (successors.length === 0) {
          actor.s = lane.lengthM;
          actor.speedMps = 0;
        } else {
          let chosen = successors[0];
          let bestTurn = Infinity;
          for (const id of successors) {
            const turn = Math.abs(normaliseAngle(lanes[id].headingRad - lane.headingRad));
            if (turn < bestTurn) {
              bestTurn = turn;
              chosen = id;
            }
          }
          if (this.#rand() > 0.68 && successors.length > 1) chosen = successors[randInt(this.#rand, 0, successors.length)];
          const next = lanes[chosen];
          const gap = Math.hypot(next.x0 - lane.x1, next.y0 - lane.y1);
          actor.laneIdx = chosen;
          if (gap > 0.05) {
            // Cross the junction on a connector instead of jumping to the new lane's start.
            actor.connector = { fromX: lane.x1, fromY: lane.y1, toX: next.x0, toY: next.y0, lengthM: gap, s: Math.min(overshoot, gap) };
            actor.s = 0;
          } else {
            actor.s = Math.min(overshoot, next.lengthM);
          }
          actor.cruiseMps = lanes[chosen].speedLimitMps * randRange(this.#rand, 0.75, 1.08) * (actor.classIdx === 6 ? 0.15 : actor.classIdx === 5 ? 0.45 : 1);
        }
      }

      if (actor.equipped) {
        // A little churn so the UI has something to render on the state bits.
        if (this.#rand() < 0.12) {
          actor.verifiedNeighbors = Math.max(0, Math.min(255, actor.verifiedNeighbors + randInt(this.#rand, -2, 3)));
        }
        if (this.#rand() < 0.5) actor.state |= ActorState.TRANSMITTING;
        else actor.state &= ~ActorState.TRANSMITTING;
        if (this.#rand() < 0.002) actor.state ^= ActorState.WARNING_ACTIVE;
      }
    }

    // Trip ends: a small share of actors finish and respawn elsewhere, exercising §3.4.5/§3.4.6
    // and the slot-reuse rule.
    for (const actor of this.#actors) {
      if (!actor.alive) continue;
      if (this.#rand() < 0.0004) {
        actor.alive = false;
        this.#pendingDespawns.push({ slot: actor.slot, cause: 0 });
        this.slots.release(actor.slot);
        this.#spawnActor(false);
      }
    }
    this.#actors = this.#actors.filter((a) => a.alive || this.#pendingDespawns.some((d) => d.slot === a.slot));
  }

  /** Signal rows for every signal head (a keyframe carries all of them, §3.3.3). */
  #allSignalRows(): SignalRowInit[] {
    const plan = signalPlanAt(this.#simTimeNs);
    const ttcDs = Math.max(0, Math.min(65_534, Math.round(plan.secondsToChange * 10)));
    return this.world.signals.map((s) => ({
      signalId: s.signalId,
      timeToChangeDs: ttcDs,
      phase: s.group === 0 ? plan.group0 : plan.group1,
    }));
  }

  /**
   * Signal rows for a delta. §3.4.7 says only signals whose `phase` or `time_to_change_ds` changed
   * appear; the countdown changes every step, so this fixture emits the whole set on a phase change
   * and every fifth step otherwise, which keeps the delta small at the cost of a countdown that can
   * be up to 400 ms stale in the client.
   */
  #deltaSignalRows(): SignalRowInit[] {
    const plan = signalPlanAt(this.#simTimeNs);
    const phaseChanged =
      this.#lastSignalPhases === null ||
      this.#lastSignalPhases.group0 !== plan.group0 ||
      this.#lastSignalPhases.group1 !== plan.group1;
    const ttcDs = Math.max(0, Math.min(65_534, Math.round(plan.secondsToChange * 10)));
    const periodic = this.#stepIndex % 5 === 0;
    if (!phaseChanged && !periodic) return [];
    if (ttcDs === this.#lastSignalTtcDs && !phaseChanged) return [];
    this.#lastSignalPhases = { group0: plan.group0, group1: plan.group1 };
    this.#lastSignalTtcDs = ttcDs;
    return this.#allSignalRows();
  }

  #blankLane(profile: Profile, laneId: number): number {
    return profile === "node" ? SENTINEL_U32 : laneId;
  }
  #blankAccel(profile: Profile, accelCq: number): number {
    return profile === "node" ? 0 : accelCq;
  }
  #blankState(profile: Profile, state: number): number {
    return profile === "node" ? state & ~ActorState.ATTACKER : state;
  }
  #blankCause(profile: Profile, cause: number): number {
    return profile === "node" ? SENTINEL_U16 : cause;
  }

  /**
   * Build the keyframe body for a profile and, for the `full` profile, latch the per-slot reference
   * state that the following deltas are relative to.
   */
  buildKeyframeBody(profile: Profile, latch: boolean): Uint8Array {
    const A = this.slots.count;
    const rows: ActorRowInit[] = new Array<ActorRowInit>(A);
    for (let slot = 0; slot < A; slot++) {
      this.#ensureRef(slot);
      rows[slot] = {
        actorId: SENTINEL_U32, xMm: 0, yMm: 0, laneId: SENTINEL_U32, zCm: 0, headingBrad: 0,
        speedCq: 0, accelCq: 0, classIdx: 0, state: 0, verifiedNeighbors: 0,
      };
    }
    for (const actor of this.#actors) {
      if (!actor.alive) continue;
      // §5.2: in the node profile an unequipped actor's slot is left empty.
      if (profile === "node" && !actor.equipped) continue;
      const lane = this.world.lanes[actor.laneIdx];
      const p = this.#positionOf(actor);
      const row: ActorRowInit = {
        actorId: actor.actorId,
        xMm: this.#qx(p.x),
        yMm: this.#qy(p.y),
        laneId: this.#blankLane(profile, lane.laneId),
        zCm: this.#qz(0.15),
        headingBrad: quantiseHeadingBrad(p.headingRad),
        speedCq: quantiseSpeedCq(actor.speedMps),
        accelCq: this.#blankAccel(profile, quantiseAccelCq(actor.accelMps2)),
        classIdx: actor.classIdx,
        state: this.#blankState(profile, actor.state),
        verifiedNeighbors: actor.verifiedNeighbors,
      };
      rows[actor.slot] = row;
      if (latch) {
        const ref = this.#refs[actor.slot];
        ref.actorId = actor.actorId;
        ref.xMm = this.#qx(p.x);
        ref.yMm = this.#qy(p.y);
        ref.zCm = this.#qz(0.15);
        ref.headingBrad = row.headingBrad;
        ref.speedCq = row.speedCq;
        ref.accelCq = quantiseAccelCq(actor.accelMps2);
        ref.state = actor.state;
        ref.verifiedNeighbors = actor.verifiedNeighbors;
        ref.laneId = lane.laneId;
        ref.classIdx = actor.classIdx;
        ref.occupied = true;
      }
    }
    if (latch) {
      for (let slot = 0; slot < A; slot++) {
        if (rows[slot].actorId === SENTINEL_U32) this.#refs[slot].occupied = false;
      }
      this.#gopIndex += 1;
      this.#stepIndex = 0;
      this.#pendingSpawns = [];
      this.#pendingDespawns = [];
      this.slots.onKeyframe(this.#gopIndex);
    }
    return encodeKeyframeBody({
      simTimeNs: this.#simTimeNs,
      originXM: Math.floor(this.world.bboxM.minX),
      originYM: Math.floor(this.world.bboxM.minY),
      originZM: 0,
      gopIndex: Math.max(0, this.#gopIndex),
      profile: profile === "node" ? 1 : 0,
      actors: rows,
      signals: this.#allSignalRows(),
    });
  }

  /** Build the delta body for a profile; `latch` updates the §3.2 reference state. */
  buildDeltaBody(profile: Profile, latch: boolean): Uint8Array {
    const moved: MovedRowInit[] = [];
    const absolute: { xMm: number; yMm: number; zCm: number }[] = [];
    const lanes: number[] = [];
    const stepIndex = latch ? this.#stepIndex + 1 : this.#stepIndex + 1;

    const byslot = [...this.#actors].filter((a) => a.alive).sort((a, b) => a.slot - b.slot);
    for (const actor of byslot) {
      if (profile === "node" && !actor.equipped) continue;
      this.#ensureRef(actor.slot);
      const ref = this.#refs[actor.slot];
      const lane = this.world.lanes[actor.laneIdx];
      const p = this.#positionOf(actor);
      const xMm = this.#qx(p.x);
      const yMm = this.#qy(p.y);
      const zCm = this.#qz(0.15);
      const headingBrad = quantiseHeadingBrad(p.headingRad);
      const speedCq = quantiseSpeedCq(actor.speedMps);
      const accelCq = quantiseAccelCq(actor.accelMps2);

      if (!ref.occupied) continue; // freshly spawned slots are carried by the spawn block instead

      const dxMm = xMm - ref.xMm;
      const dyMm = yMm - ref.yMm;
      // §3.4.2 `dz_mm` is MILLIMETRES, while the reference `z_cm` is centimetres (§3.3.2): the
      // delta is the change of the previously transmitted quantised z expressed in millimetres.
      const dzMm = (zCm - ref.zCm) * MM_PER_CM;
      const changed =
        dxMm !== 0 || dyMm !== 0 || dzMm !== 0 ||
        headingBrad !== ref.headingBrad || speedCq !== ref.speedCq || accelCq !== ref.accelCq ||
        actor.state !== ref.state || actor.verifiedNeighbors !== ref.verifiedNeighbors ||
        lane.laneId !== ref.laneId;
      if (!changed) continue;

      let mflags = 0;
      // §3.2 escape hatch — a step beyond ±32,000 mm goes in the absolute block.
      const escape = needsAbsoluteEscape(dxMm, dyMm, dzMm);
      if (escape) {
        mflags |= MovedFlags.ABSOLUTE;
        absolute.push({ xMm, yMm, zCm });
      }
      if (lane.laneId !== ref.laneId && profile !== "node") {
        mflags |= MovedFlags.LANE_CHANGED;
        lanes.push(lane.laneId);
      }
      moved.push({
        slot: actor.slot,
        dxMm: escape ? 0 : dxMm,
        dyMm: escape ? 0 : dyMm,
        dzMm: escape ? 0 : Math.max(-DELTA_ESCAPE_MM, Math.min(DELTA_ESCAPE_MM, dzMm)),
        headingBrad,
        speedCq,
        accelCq: this.#blankAccel(profile, accelCq),
        state: this.#blankState(profile, actor.state),
        verifiedNeighbors: actor.verifiedNeighbors,
        mflags,
      });

      if (latch) {
        ref.xMm = xMm;
        ref.yMm = yMm;
        ref.zCm = zCm;
        ref.headingBrad = headingBrad;
        ref.speedCq = speedCq;
        ref.accelCq = accelCq;
        ref.state = actor.state;
        ref.verifiedNeighbors = actor.verifiedNeighbors;
        ref.laneId = lane.laneId;
      }
    }

    const spawns = this.#pendingSpawns
      .filter((s) => profile !== "node" || s.nodeId !== SENTINEL_U32)
      .map((s) => ({
        ...s,
        laneId: this.#blankLane(profile, s.laneId),
        cause: this.#blankCause(profile, s.cause),
        state: this.#blankState(profile, s.state),
      }));
    const despawns = this.#pendingDespawns.map((d) => ({ ...d, cause: this.#blankCause(profile, d.cause) }));

    const body = encodeDeltaBody({
      simTimeNs: this.#simTimeNs,
      gopIndex: Math.max(0, this.#gopIndex),
      stepIndex,
      moved,
      absolute,
      lanes: profile === "node" ? [] : lanes,
      spawns,
      despawns,
      signals: this.#deltaSignalRows(),
    });

    if (latch) {
      for (const s of this.#pendingSpawns) {
        this.#ensureRef(s.slot);
        const ref = this.#refs[s.slot];
        ref.actorId = s.actorId;
        ref.xMm = s.xMm;
        ref.yMm = s.yMm;
        ref.zCm = s.zCm;
        ref.headingBrad = s.headingBrad;
        ref.speedCq = s.speedCq;
        ref.accelCq = 0;
        ref.state = s.state;
        ref.verifiedNeighbors = s.verifiedNeighbors;
        ref.laneId = s.laneId;
        ref.classIdx = s.classIdx;
        ref.occupied = true;
      }
      for (const d of this.#pendingDespawns) {
        this.#ensureRef(d.slot);
        this.#refs[d.slot].occupied = false;
      }
      this.#actors = this.#actors.filter((a) => a.alive);
      this.#pendingSpawns = [];
      this.#pendingDespawns = [];
      this.#stepIndex = stepIndex;
    }
    return body;
  }

  /**
   * Jump the whole run to a sim time (`run.seek`). Actors are advanced analytically, which moves
   * them far more than 32,000 mm in one step — exactly the case §3.2's escape hatch exists for.
   */
  seekTo(targetNs: bigint): void {
    const steps = Number((targetNs - this.#simTimeNs) / this.timing.mobilityStepNs);
    if (steps <= 0) {
      this.#simTimeNs = targetNs;
      return;
    }
    const capped = Math.min(steps, 6000);
    for (let i = 0; i < capped; i++) this.advance();
    this.#simTimeNs = targetNs;
  }

  /** §3.5.2 — a plausible telemetry record for a node, with every field populated. */
  telemetryFor(nodeId: number, windowNs: bigint): NodeTelemetryInit {
    const rand = mulberry32(nodeId * 2654435761 + Number(this.#simTimeNs / 1_000_000_000n));
    const actor = this.#actors.find((a) => a.alive && a.nodeId === nodeId);
    const attacker = actor !== undefined && (actor.state & ActorState.ATTACKER) !== 0;
    const degraded = actor !== undefined && (actor.state & ActorState.GNSS_DEGRADED) !== 0;
    const neighbours = actor?.verifiedNeighbors ?? randInt(rand, 0, 40);
    const msgsIn = 40 + neighbours * 9.6 + randRange(rand, -8, 8);
    const isRsu = nodeId < ACTOR_NODE_BASE;
    return {
      storageUsedB: BigInt(Math.round(randRange(rand, 1.5e6, 7e6))),
      storageTotalB: isRsu ? 268_435_456n : 67_108_864n,
      nextTopupNs: this.#simTimeNs + BigInt(Math.round(randRange(rand, 60, 3600)) * 1_000_000_000),
      crlBytes: BigInt(Math.round(randRange(rand, 2e4, 3e5))),
      outboxBytes: BigInt(randInt(rand, 0, 8192)),
      clockOffsetNs: BigInt(Math.round(randRange(rand, -2e6, 2e6))),
      nodeId,
      ramUsedKib: randInt(rand, 1800, 9000),
      ramTotalKib: isRsu ? 262_144 : 65_536,
      dropRxOverflow: randInt(rand, 0, 40),
      dropVerifyPolicySkip: randInt(rand, 0, 600),
      dropVerifyOverflow: randInt(rand, 0, 25),
      dropTxOverflow: randInt(rand, 0, 5),
      dropReassemblyTimeout: randInt(rand, 0, 4),
      dropCrlBacklog: 0,
      certStored: randInt(rand, 8, 40),
      crlEntries: randInt(rand, 100, 4000),
      outboxMsgs: randInt(rand, 0, 12),
      peerCacheEntries: Math.max(neighbours, randInt(rand, 20, 220)),
      p2pcdRequests: randInt(rand, 0, 12),
      fullCertMsgs: randInt(rand, 0, 40),
      msgsInPerS: msgsIn,
      msgsOutPerS: isRsu ? randRange(rand, 8, 14) : 10,
      verificationsPerS: msgsIn * randRange(rand, 0.7, 0.98),
      verifyWaitP50Ms: randRange(rand, 0.3, 4),
      verifyWaitP95Ms: randRange(rand, 3, 22),
      gnssHdop: degraded ? randRange(rand, 2.5, 8) : randRange(rand, 0.6, 1.4),
      gnssSigmaM: degraded ? randRange(rand, 4, 25) : randRange(rand, 0.7, 2.4),
      clockDriftPpm: randRange(rand, -8, 8),
      posErrorM: degraded ? randRange(rand, 3, 18) : randRange(rand, 0.2, 2.2),
      airtimeMsPerS: randRange(rand, 5, 90),
      cpuUtilPm: randInt(rand, 120, 880),
      hsmUtilPm: randInt(rand, 40, 700),
      qRxP50: randInt(rand, 0, 12),
      qRxP95: randInt(rand, 6, 60),
      qVerifyP50: randInt(rand, 0, 20),
      qVerifyP95: randInt(rand, 8, 90),
      qAppP50: randInt(rand, 0, 5),
      qAppP95: randInt(rand, 1, 14),
      qTxP50: randInt(rand, 0, 3),
      qTxP95: randInt(rand, 0, 8),
      qCrlP50: 0,
      qCrlP95: randInt(rand, 0, 3),
      dccState: randInt(rand, 0, 5),
      cbrPm: randInt(rand, 80, 720),
      txPowerCdbm: randInt(rand, 1000, 2300),
      nbrTotal: Math.min(65535, neighbours + randInt(rand, 0, 25)),
      nbrVerified: neighbours,
      nbrUnverified: randInt(rand, 0, 20),
      nbrRevoked: randInt(rand, 0, 3),
      certActive: randInt(rand, 1, 20),
      crlExpansionPm: randInt(rand, 0, 1000),
      unverifiedRatioPm: randInt(rand, 0, 300),
      gnssFix: degraded ? 1 : 2,
      nodeState: attacker ? 6 : 2,
      verifyPolicy: 2,
    };
  }

  /** §3.5.2 — the node-profile blanking of a telemetry record (§5.2). */
  static blankTelemetryForNodeProfile(record: NodeTelemetryInit): NodeTelemetryInit {
    return {
      ...record,
      clockOffsetNs: 0n,
      posErrorM: Number.NaN,
      nodeState: record.nodeState === 6 ? 2 : record.nodeState,
    };
  }

  /** §3.6 — a batch of event records on the requested channels, for this mobility step. */
  buildEvents(channels: ReadonlySet<number>, profile: Profile, maxEvents: number): EventRecordInit[] {
    if (channels.size === 0 || maxEvents === 0) return [];
    const out: EventRecordInit[] = [];
    const t = this.#simTimeNs;
    const equipped = this.#actors.filter((a) => a.alive && a.equipped);
    if (equipped.length === 0) return out;
    const sampleSize = Math.max(1, Math.min(equipped.length, Math.floor(maxEvents / 4)));
    const start = randInt(this.#rand, 0, equipped.length);

    for (let n = 0; n < sampleSize && out.length < maxEvents; n++) {
      const actor = equipped[(start + n) % equipped.length];
      const msgId = this.#msgId++;

      if (channels.has(ChannelId.NODE_TX)) {
        const b = new Uint8Array(40);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.nodeId, true);
        dv.setUint32(4, msgId, true);
        dv.setUint32(8, 300 + randInt(this.#rand, 0, 120), true);
        dv.setFloat32(12, randRange(this.#rand, 0.3, 0.7), true);
        dv.setUint16(16, actor.classIdx >= 5 ? 6 : 1, true); // PSM for VRUs, BSM otherwise
        dv.setInt16(18, 2000, true);
        dv.setUint16(20, 172, true);
        dv.setUint8(22, 5);
        dv.setUint8(23, 0);
        dv.setUint8(24, 1);
        dv.setUint8(25, this.#rand() < 0.1 ? 1 : 0);
        dv.setUint16(26, 200 + randInt(this.#rand, 0, 80), true);
        for (let k = 0; k < 8; k++) dv.setUint8(28 + k, randInt(this.#rand, 0, 256));
        dv.setUint32(36, 1000 + (actor.actorId % 20), true);
        out.push({ simTimeNs: t, channelId: ChannelId.NODE_TX, payload: b });
      }

      if (channels.has(ChannelId.PHY_RX) && out.length < maxEvents) {
        const peer = equipped[(start + n + 1) % equipped.length];
        const pa = this.#positionOf(actor);
        const pb = this.#positionOf(peer);
        const distance = Math.hypot(pa.x - pb.x, pa.y - pb.y);
        const b = new Uint8Array(48);
        const dv = new DataView(b.buffer);
        dv.setBigUint64(0, t, true);
        dv.setBigUint64(8, t + 400_000n, true);
        dv.setUint32(16, peer.nodeId, true);
        dv.setUint32(20, profile === "node" ? SENTINEL_U32 : actor.nodeId, true);
        dv.setUint32(24, msgId, true);
        dv.setFloat32(28, -40 - 20 * Math.log10(Math.max(1, distance)), true);
        dv.setFloat32(32, Math.max(-5, 30 - 12 * Math.log10(Math.max(1, distance))), true);
        dv.setFloat32(36, profile === "node" ? Number.NaN : distance, true);
        dv.setUint8(40, distance > 600 ? 1 : this.#rand() < 0.04 ? 2 : 0);
        dv.setUint8(41, distance > 600 ? 9 : 0);
        dv.setUint8(42, profile === "node" ? 0xff : distance > 200 ? 1 : 0);
        out.push({ simTimeNs: t, channelId: ChannelId.PHY_RX, payload: b });
      }

      if (channels.has(ChannelId.MAC_CBR) && out.length < maxEvents && n % 8 === 0) {
        const b = new Uint8Array(16);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.nodeId, true);
        dv.setFloat32(4, randRange(this.#rand, 0.08, 0.72), true);
        dv.setUint16(8, 172, true);
        dv.setUint16(10, randInt(this.#rand, 0, 5), true);
        dv.setInt16(12, 2000, true);
        out.push({ simTimeNs: t, channelId: ChannelId.MAC_CBR, payload: b });
      }

      if (channels.has(ChannelId.APP_WARNING) && out.length < maxEvents && this.#rand() < 0.02) {
        const b = new Uint8Array(32);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.nodeId, true);
        dv.setUint32(4, 1, true); // str id of "fcw", appended by the server's symbol table
        for (let k = 0; k < 8; k++) dv.setUint8(8 + k, randInt(this.#rand, 0, 256));
        dv.setFloat32(16, randRange(this.#rand, 0.8, 4.5), true);
        dv.setFloat32(20, randRange(this.#rand, 8, 60), true);
        dv.setUint8(24, 0);
        dv.setUint8(25, randInt(this.#rand, 1, 4));
        dv.setUint8(26, profile === "node" ? 0 : this.#rand() < 0.7 ? 1 : 2);
        dv.setUint32(28, profile === "node" ? SENTINEL_U32 : actor.actorId, true);
        out.push({ simTimeNs: t, channelId: ChannelId.APP_WARNING, payload: b });
      }

      if (channels.has(ChannelId.DET_OBSERVATION) && out.length < maxEvents && this.#rand() < 0.015) {
        const b = new Uint8Array(32);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.nodeId, true);
        dv.setUint32(4, 2, true);
        for (let k = 0; k < 8; k++) dv.setUint8(8 + k, randInt(this.#rand, 0, 256));
        dv.setFloat32(16, randRange(this.#rand, 0.35, 0.99), true);
        dv.setUint32(20, profile === "node" ? SENTINEL_U32 : actor.actorId, true);
        dv.setUint16(24, randInt(this.#rand, 1, 30), true);
        dv.setUint8(26, randInt(this.#rand, 0, 5));
        dv.setUint32(28, 1, true); // prov_id 1, delivered in the Provenance frame
        out.push({ simTimeNs: t, channelId: ChannelId.DET_OBSERVATION, payload: b });
      }

      if (channels.has(ChannelId.SEC_CERT) && out.length < maxEvents && this.#rand() < 0.01) {
        const b = new Uint8Array(40);
        const dv = new DataView(b.buffer);
        dv.setBigUint64(0, t, true);
        dv.setBigUint64(8, t + 604_800_000_000_000n, true);
        dv.setUint32(16, actor.nodeId, true);
        dv.setUint32(20, 1000 + (actor.actorId % 20), true);
        for (let k = 0; k < 8; k++) dv.setUint8(24 + k, randInt(this.#rand, 0, 256));
        dv.setUint8(32, 0); // change
        dv.setUint8(33, 0); // pseudonym / AT
        dv.setUint16(34, randInt(this.#rand, 0, 200), true);
        dv.setUint16(36, randInt(this.#rand, 0, 20), true);
        dv.setUint16(38, 1, true);
        out.push({ simTimeNs: t, channelId: ChannelId.SEC_CERT, payload: b });
      }

      if (channels.has(ChannelId.PROTO_REVOCATION) && out.length < maxEvents && this.#rand() < 0.003) {
        const stage = randInt(this.#rand, 5, 11); // node profile withholds stages <= 4 (§5.2)
        const b = new Uint8Array(32);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.nodeId, true);
        dv.setUint32(4, 500 + (actor.actorId % 40), true);
        for (let k = 0; k < 8; k++) dv.setUint8(8 + k, randInt(this.#rand, 0, 256));
        dv.setBigUint64(16, BigInt(randInt(this.#rand, 1000, 300_000)), true);
        dv.setUint32(24, stage >= 7 ? actor.nodeId : SENTINEL_U32, true);
        dv.setUint8(28, stage);
        dv.setUint8(29, 0);
        dv.setUint16(30, randInt(this.#rand, 1, 5000), true);
        out.push({ simTimeNs: t, channelId: ChannelId.PROTO_REVOCATION, payload: b });
      }

      if (channels.has(ChannelId.GT_KINEMATICS) && profile === "full" && out.length < maxEvents && n % 16 === 0) {
        const lane = this.world.lanes[actor.laneIdx];
        const p = this.#positionOf(actor);
        const b = new Uint8Array(56);
        const dv = new DataView(b.buffer);
        dv.setUint32(0, actor.actorId, true);
        dv.setUint32(4, lane.laneId, true);
        dv.setFloat32(8, p.x, true);
        dv.setFloat32(12, p.y, true);
        dv.setFloat32(16, 0.15, true);
        dv.setFloat32(20, Math.cos(p.headingRad) * actor.speedMps, true);
        dv.setFloat32(24, Math.sin(p.headingRad) * actor.speedMps, true);
        dv.setFloat32(28, 0, true);
        dv.setFloat32(32, Math.cos(p.headingRad) * actor.accelMps2, true);
        dv.setFloat32(36, Math.sin(p.headingRad) * actor.accelMps2, true);
        dv.setFloat32(40, 0, true);
        dv.setFloat32(44, p.headingRad, true);
        dv.setFloat32(48, 0, true);
        dv.setFloat32(52, actor.s, true);
        out.push({ simTimeNs: t, channelId: ChannelId.GT_KINEMATICS, payload: b });
      }
    }
    return out.slice(0, maxEvents);
  }

  /** §3.7 — a metric bin. `strIds` maps metric name to its symbol-table id. */
  buildMetrics(strIds: ReadonlyMap<string, number>, profile: Profile): MetricRecordInit[] {
    const rand = mulberry32(Number(this.#simTimeNs / 1_000_000_000n) + 91);
    const live = this.#actors.filter((a) => a.alive);
    const equipped = live.filter((a) => a.equipped);
    const meanSpeed = live.length === 0 ? 0 : live.reduce((n, a) => n + a.speedMps, 0) / live.length;
    const rows: { name: string; value: number; agg: number; visibility: number; nodeId?: number; provId: number }[] = [
      { name: "pdr", value: randRange(rand, 0.86, 0.98), agg: 5, visibility: 1, provId: 1 },
      { name: "cbr", value: randRange(rand, 0.12, 0.55), agg: 1, visibility: 1, provId: 1 },
      { name: "msgs_per_s", value: equipped.length * 10, agg: 6, visibility: 1, provId: 1 },
      { name: "verify_wait_p95_ms", value: randRange(rand, 4, 24), agg: 3, visibility: 1, provId: 1 },
      { name: "nbr_verified_mean", value: equipped.length === 0 ? 0 : equipped.reduce((n, a) => n + a.verifiedNeighbors, 0) / equipped.length, agg: 1, visibility: 1, provId: 2 },
      { name: "crl_bytes_p95", value: randRange(rand, 5e4, 3e5), agg: 3, visibility: 1, provId: 1 },
      // GT metrics: §5.2 says a node-profile stream must not carry visibility 0 samples.
      { name: "mean_speed", value: meanSpeed, agg: 1, visibility: 0, provId: 2 },
      { name: "ttc_min", value: randRange(rand, 0.9, 6), agg: 8, visibility: 0, provId: 2 },
      { name: "det_precision", value: randRange(rand, 0.55, 0.95), agg: 5, visibility: 0, provId: 2 },
    ];
    return rows
      .filter((r) => profile !== "node" || r.visibility !== 0)
      .map((r) => ({
        value: r.value,
        strMetric: strIds.get(r.name) ?? 0,
        dimKey: 0,
        nodeId: r.nodeId ?? SENTINEL_U32,
        count: Math.max(1, live.length),
        agg: r.agg,
        visibility: r.visibility,
        provId: r.provId,
      }));
  }

  /** Names of every metric {@link buildMetrics} can emit, for the symbol table and the catalogue. */
  static metricNames(): string[] {
    return [
      "pdr", "cbr", "msgs_per_s", "verify_wait_p95_ms", "nbr_verified_mean", "crl_bytes_p95",
      "mean_speed", "ttc_min", "det_precision",
    ];
  }

  /** The sentinel for "no time" — re-exported so callers do not import it separately. */
  static readonly NO_TIME = SENTINEL_U64;
}

function normaliseAngle(a: number): number {
  let x = a;
  while (x > Math.PI) x -= 2 * Math.PI;
  while (x < -Math.PI) x += 2 * Math.PI;
  return x;
}
