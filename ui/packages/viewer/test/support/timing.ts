/**
 * The two timing experiments the UI review ran against `PoseInterpolator` (findings Q1 and Q2).
 *
 * Both drive the *real* interpolator from the *real* protocol encoders (`SyntheticStream` builds
 * `Keyframe`/`Delta` frames and applies them to a `PoseBuffer`), sample at 60 fps, and report the
 * per-frame motion of one tracked actor. The point of measuring motion rather than `alpha` is that
 * motion is what a viewer sees: a frozen frame followed by a teleport is the defect, whatever the
 * blend factor says.
 *
 * Timeline model: snapshot *k* carries sim time `k · interval` (the engine's cadence, exact by
 * construction — §3.4 steps are whole `mobility_step_ns`) and *arrives* at viewer clock
 * `k · interval + jitter_k`. That separation is the whole of Q1: the sim clock is authoritative and
 * the arrival clock is noise.
 */

import { PoseInterpolator, type PoseInterpolatorOptions } from "../../src/interp.js";
import { SyntheticStream, type GridWorld } from "./fixture.js";

/** What one run of the experiment measured, all distances in metres. */
export interface MotionStats {
  /** Frames sampled after the warm-up. */
  readonly frames: number;
  /** Frames whose tracked actor did not move at all (< 1 mm). */
  readonly frozen: number;
  readonly frozenPct: number;
  readonly minMotion: number;
  readonly maxMotion: number;
  readonly meanMotion: number;
  readonly medianMotion: number;
  /** `maxMotion / medianMotion`, the "teleport factor" the review quoted. */
  readonly jumpRatio: number;
  /** Largest backwards step along the tracked actor's heading (0 when it never reversed). */
  readonly maxReversalM: number;
  /** Highest `alpha` any sample used. */
  readonly maxAlpha: number;
  /** Speed of the tracked actor, m/s. */
  readonly speedMps: number;
}

/** A frame is "frozen" below this much motion; nominal motion at 60 fps is ~0.25 m. */
const FROZEN_M = 1e-3;

export interface MotionExperiment {
  readonly grid: GridWorld;
  /** Sim seconds between snapshots. */
  readonly intervalSeconds: number;
  /** Arrival jitter, ± this many seconds, deterministic. */
  readonly jitterSeconds?: number;
  /**
   * `"uniform"` draws each arrival's offset from a deterministic PRNG; `"alternating"` is the
   * worst case of the same bound — every other frame early then late, so two consecutive arrivals
   * are `interval - 2·jitter` apart. Default `"alternating"`, because that is the case a sampler
   * keyed off arrival time fails hardest on.
   */
  readonly jitterPattern?: "uniform" | "alternating";
  /** Snapshots delivered (the first two are warm-up and are not measured). */
  readonly snapshots?: number;
  readonly fps?: number;
  readonly interp?: PoseInterpolatorOptions;
  readonly actors?: number;
}

function median(values: number[]): number {
  if (values.length === 0) return 0;
  const s = [...values].sort((a, b) => a - b);
  return s[Math.floor(s.length / 2)];
}

/**
 * Run one experiment. Returns the per-frame motion statistics of the actor with the most clear road
 * ahead, chosen so the fixture's world-edge wrap (a legitimate teleport) cannot pollute the numbers.
 */
export function measureMotion(exp: MotionExperiment): MotionStats {
  const interval = exp.intervalSeconds;
  const jitter = exp.jitterSeconds ?? 0;
  const snapshots = exp.snapshots ?? 12;
  const fps = exp.fps ?? 60;
  const frameDt = 1 / fps;
  const stream = new SyntheticStream(exp.actors ?? 16, exp.grid);
  stream.keyframe();

  // Pick the actor with the most room before the fixture wraps it at the world edge.
  const half = ((exp.grid.options.blocks - 1) * exp.grid.options.blockM) / 2
    + exp.grid.options.blockM / 2;
  let slot = 0;
  let bestRoom = -Infinity;
  for (let i = 0; i < stream.poses.count; i++) {
    const x = stream.poses.positions[i * 3];
    const y = stream.poses.positions[i * 3 + 1];
    const h = stream.poses.headings[i];
    const cx = Math.cos(h);
    const cy = Math.sin(h);
    const room = Math.abs(cx) > 0.5 ? (cx > 0 ? half - x : x + half) : (cy > 0 ? half - y : y + half);
    if (room > bestRoom) {
      bestRoom = room;
      slot = i;
    }
  }
  const speed = stream.poses.speeds[slot];
  const travel = speed * interval * snapshots;
  if (travel > bestRoom) {
    throw new Error(`experiment would wrap the tracked actor (${travel.toFixed(0)} m of ${bestRoom.toFixed(0)} m)`);
  }

  const interp = new PoseInterpolator(exp.interp ?? {});
  interp.capture(stream.poses, 0);

  // Arrival schedule. Snapshot 0 is the keyframe already captured at clock 0.
  let seed = 0x9e3779b1;
  const rnd = (): number => {
    seed = (seed * 1664525 + 1013904223) >>> 0;
    return seed / 0x1_0000_0000;
  };
  const pattern = exp.jitterPattern ?? "alternating";
  const arrivals: number[] = [];
  for (let k = 1; k <= snapshots; k++) {
    const offset = jitter === 0
      ? 0
      : pattern === "alternating" ? (k % 2 === 0 ? jitter : -jitter) : (rnd() * 2 - 1) * jitter;
    arrivals.push(k * interval + offset);
  }

  const warmUntil = 2 * interval + jitter;
  const endClock = snapshots * interval;
  const motions: number[] = [];
  let px = interp.outPosition[slot * 3];
  let py = interp.outPosition[slot * 3 + 1];
  let havePrev = false;
  let maxAlpha = 0;
  let maxReversal = 0;
  let nextSnapshot = 0;
  const hx = Math.cos(stream.poses.headings[slot]);
  const hy = Math.sin(stream.poses.headings[slot]);

  for (let clock = 0; clock <= endClock; clock += frameDt) {
    while (nextSnapshot < arrivals.length && arrivals[nextSnapshot] <= clock) {
      // Advance the crowd to this snapshot's sim time, then emit it.
      stream.advance(interval);
      stream.delta();
      interp.capture(stream.poses, arrivals[nextSnapshot]);
      nextSnapshot++;
    }
    const info = interp.sample(clock);
    const x = interp.outPosition[slot * 3];
    const y = interp.outPosition[slot * 3 + 1];
    if (clock >= warmUntil && havePrev) {
      const dx = x - px;
      const dy = y - py;
      motions.push(Math.hypot(dx, dy));
      const along = dx * hx + dy * hy;
      if (along < -maxReversal) maxReversal = -along;
      if (info.alpha > maxAlpha) maxAlpha = info.alpha;
    }
    px = x;
    py = y;
    havePrev = true;
  }

  let sum = 0;
  let min = Infinity;
  let max = 0;
  let frozen = 0;
  for (const m of motions) {
    sum += m;
    if (m < min) min = m;
    if (m > max) max = m;
    if (m < FROZEN_M) frozen++;
  }
  const med = median(motions);
  return {
    frames: motions.length,
    frozen,
    frozenPct: (frozen / Math.max(1, motions.length)) * 100,
    minMotion: motions.length === 0 ? 0 : min,
    maxMotion: max,
    meanMotion: sum / Math.max(1, motions.length),
    medianMotion: med,
    jumpRatio: med > 0 ? max / med : 0,
    maxReversalM: maxReversal,
    maxAlpha,
    speedMps: speed,
  };
}

/** One line of the measurement table, for the test log. */
export function formatMotion(label: string, s: MotionStats): string {
  return `${label.padEnd(26)} motion ${s.minMotion.toFixed(4)}–${s.maxMotion.toFixed(4)} m `
    + `(median ${s.medianMotion.toFixed(4)}, ${s.jumpRatio.toFixed(1)}x), frozen `
    + `${s.frozen}/${s.frames} = ${s.frozenPct.toFixed(0)} %, reversal ${s.maxReversalM.toFixed(4)} m, `
    + `alpha ≤ ${s.maxAlpha.toFixed(3)}`;
}
