/**
 * What a followed vehicle looks like between snapshots — the owner's "the cars aren't smooth".
 *
 * `interp-timing.test.ts` covers the clock (jitter, slow streams, the stall). This file covers the
 * *path*: a vehicle driving a curve at 10 Hz, sampled at 60 fps, against the exact curve it drove.
 * Every stream here goes through the real encoder (`placedPoses`), so the wire's millimetre and
 * binary-radian quantisation applies.
 *
 * Each property was shown to fail on the two-snapshot linear sampler this replaced (the numbers
 * quoted beside the assertions are that sampler's, measured by running this file against it).
 */

import { describe, expect, it } from "vitest";
import { PoseInterpolator, wrapAngle } from "../src/interp.js";
import { placedPoses, type PlacedActor } from "./support/one-actor.js";

const STEP = 0.1;
const FPS = 60;

/** A vehicle on a circle of radius `r` about `(cx, cy)`, counter-clockwise at `v` m/s. */
function onCircle(t: number, r = 15, v = 10, cx = 0, cy = 0, phase = 0): PlacedActor & { x: number; y: number; headingRad: number } {
  const w = v / r;
  const a = phase + w * t;
  return { actorId: 1, x: cx + r * Math.cos(a), y: cy + r * Math.sin(a), headingRad: wrapAngle(a + Math.PI / 2), speedMps: v };
}

interface PathRun {
  /** Largest distance from the true curve at the instant rendered, metres. */
  readonly maxError: number;
  /**
   * Largest frame-to-frame change of velocity, both measured against the *sim* time the frames
   * rendered, m/s². Sim time rather than frame time, so this is a property of the path and not of
   * the render clock's (bounded, separately tested) rate corrections.
   */
  readonly maxAccel: number;
  /** Largest single-frame heading change, radians. */
  readonly maxYawStep: number;
  /** Largest heading error against the truth, radians. */
  readonly maxHeadingError: number;
}

/**
 * Deliver `snapshots` samples of `truth` at 10 Hz with arrival jitter, sample at 60 fps, and compare
 * each rendered pose with `truth` at the sim time the sampler says it rendered.
 */
function drive(truth: (t: number) => PlacedActor & { x: number; y: number; headingRad: number }, jitter: number, snapshots = 40): PathRun {
  const interp = new PoseInterpolator();
  const arrivals: number[] = [];
  for (let k = 0; k <= snapshots; k++) arrivals.push(k * STEP + (k % 2 === 0 ? jitter : -jitter) + (k === 0 ? 0 : 0));
  let next = 0;
  let maxError = 0;
  let maxAccel = 0;
  let maxYawStep = 0;
  let maxHeadingError = 0;
  let prev: { x: number; y: number; s: number } | null = null;
  let prevV: { x: number; y: number } | null = null;
  let prevH: number | null = null;
  const dt = 1 / FPS;
  for (let clock = 0; clock < snapshots * STEP; clock += dt) {
    while (next < arrivals.length && arrivals[next] <= clock) {
      interp.capture(placedPoses([truth(next * STEP)], BigInt(Math.round(next * STEP * 1e9))), arrivals[next]);
      next++;
    }
    if (next === 0) continue;
    const info = interp.sample(clock);
    const x = interp.outPosition[0];
    const y = interp.outPosition[1];
    const h = interp.outHeading[0];
    if (clock < 3 * STEP) {
      prev = null;
      prevV = null;
      prevH = null;
      continue;
    }
    const want = truth(info.renderSimSeconds);
    maxError = Math.max(maxError, Math.hypot(x - want.x, y - want.y));
    maxHeadingError = Math.max(maxHeadingError, Math.abs(wrapAngle(h - want.headingRad)));
    if (prevH !== null) maxYawStep = Math.max(maxYawStep, Math.abs(wrapAngle(h - prevH)));
    const s = info.renderSimSeconds;
    if (prev && s - prev.s > 1e-4) {
      const ds = s - prev.s;
      const vx = (x - prev.x) / ds;
      const vy = (y - prev.y) / ds;
      if (prevV) maxAccel = Math.max(maxAccel, Math.hypot(vx - prevV.x, vy - prevV.y) / ds);
      prevV = { x: vx, y: vy };
    }
    prev = { x, y, s };
    prevH = h;
  }
  return { maxError, maxAccel, maxYawStep, maxHeadingError };
}

describe("PoseInterpolator — the path between snapshots", () => {
  it("follows a curve instead of its chords, with continuous velocity", () => {
    // 10 m/s on a 15 m radius: 6.7 m/s² of real centripetal acceleration, a city-corner turn.
    const run = drive((t) => onCircle(t), 0);
    // eslint-disable-next-line no-console
    console.log(`curve, no jitter: max error ${(run.maxError * 1000).toFixed(1)} mm, max apparent accel ${run.maxAccel.toFixed(1)} m/s², max yaw step ${(run.maxYawStep * 180 / Math.PI).toFixed(2)} deg`);
    // A linear blend cuts the corner along its chords and turns it in ten kinks a second: measured
    // on the old sampler, 8.7 mm off the curve and 81.7 m/s² of apparent acceleration.
    expect(run.maxError).toBeLessThan(0.006);
    expect(run.maxAccel).toBeLessThan(6.7 * 2);
    expect(run.maxHeadingError).toBeLessThan(0.02);
  });

  it("does not snap at a snapshot that arrives before the render clock reaches it", () => {
    // ±30 ms arrival jitter: every other snapshot lands while the render clock is still inside the
    // previous segment. The two-snapshot sampler then extrapolated backwards along the *new* chord:
    // measured 55.4 mm off the curve and 299.5 m/s² of apparent acceleration.
    const run = drive((t) => onCircle(t), 0.03);
    // eslint-disable-next-line no-console
    console.log(`curve, ±30 ms jitter: max error ${(run.maxError * 1000).toFixed(1)} mm, max apparent accel ${run.maxAccel.toFixed(1)} m/s²`);
    expect(run.maxError).toBeLessThan(0.01);
    expect(run.maxAccel).toBeLessThan(6.7 * 3);
  });

  it("turns the heading the short way across ±180 degrees", () => {
    // Through heading π and through heading 0, since which of the two is the seam depends on how the
    // wire's binary radians are decoded. One frame of a 0.67 rad/s turn is 0.011 rad; a wrap bug is
    // a jump of up to 6.28 rad (measured with the wrap removed: a 1.29 rad step).
    for (const start of [Math.PI - 0.4, -0.4]) {
      const run = drive((t) => onCircle(t, 15, 10, 0, 0, start - Math.PI / 2), 0.02, 20);
      expect(run.maxYawStep, `heading from ${start.toFixed(2)}`).toBeLessThan(0.03);
      expect(run.maxHeadingError, `heading from ${start.toFixed(2)}`).toBeLessThan(0.02);
    }
  });

  it("renders exactly the stream's pose after a seek backwards", () => {
    const interp = new PoseInterpolator();
    const at = (t: number, x: number): PlacedActor => ({ actorId: 7, x, y: 0, headingRad: 0, speedMps: 10 });
    interp.capture(placedPoses([at(10, 100)], 10_000_000_000n), 0);
    interp.capture(placedPoses([at(10.1, 101)], 10_100_000_000n), 0.1);
    for (let c = 0.1; c < 0.3; c += 1 / 60) interp.sample(c);
    // Seek to t = 7 s: the vehicle was 30 m back, inside the 40 m teleport distance, so nothing
    // flagged it as a jump. The old sampler rendered it at prev − 1.2·(next − prev): 36 m away.
    interp.capture(placedPoses([at(7, 70)], 7_000_000_000n), 0.31);
    const info = interp.sample(0.32);
    expect(info.snapped).toBe(1);
    expect(interp.outPosition[0]).toBeCloseTo(70, 2);
    // And it keeps showing it while the run stays paused there.
    for (let c = 0.32; c < 2; c += 1 / 60) interp.sample(c);
    expect(Math.abs(interp.outPosition[0] - 70)).toBeLessThan(1.3);
  });

  it("treats a second snapshot at the same instant as a replacement, not a zero-length segment", () => {
    const interp = new PoseInterpolator();
    const at = (x: number): PlacedActor => ({ actorId: 3, x, y: 0, headingRad: 0, speedMps: 10 });
    let worst = 0;
    let last = Number.NaN;
    let clock = 0;
    for (let k = 0; k < 20; k++) {
      const simNs = BigInt(k) * 100_000_000n;
      interp.capture(placedPoses([at(k)], simNs), k * STEP);
      // A keyframe repeating the step the delta already carried (§3.3 every second), or a delta the
      // pose buffer refused: same sim time, captured again.
      if (k % 5 === 4) interp.capture(placedPoses([at(k)], simNs), k * STEP + 0.01);
      for (; clock < (k + 1) * STEP; clock += 1 / FPS) {
        interp.sample(clock);
        const x = interp.outPosition[0];
        if (k > 2 && Number.isFinite(last)) worst = Math.max(worst, Math.abs(x - last));
        last = x;
      }
    }
    // Nominal motion is 10 m/s / 60 = 0.167 m a frame. The old sampler divided by a 1e-6 s span and
    // clamped: measured, a 1.00 m lurch in one frame.
    expect(worst).toBeLessThan((10 / FPS) * 1.3);
  });

  it("never overshoots when the reported speed disagrees with the positions", () => {
    // The engine reported 11 m/s while the vehicle did not move for four steps, then jumped 6 m
    // (manhattan-5min, vehicle 7, t = 32.3–32.8 s). A Hermite curve with unlimited tangents would
    // swing ahead and come back; the limited one stays between the two poses.
    const interp = new PoseInterpolator();
    const xs = [0, 1.1, 1.1, 1.1, 1.1, 7.2, 8.3, 9.4];
    let minX = Infinity;
    let maxX = -Infinity;
    let back = 0;
    let lastX = Number.NaN;
    let clock = 0;
    for (let k = 0; k < xs.length; k++) {
      interp.capture(placedPoses([{ actorId: 9, x: xs[k], y: 0, headingRad: 0, speedMps: 11 }], BigInt(k) * 100_000_000n), k * STEP);
      for (; clock < (k + 1) * STEP; clock += 1 / FPS) {
        interp.sample(clock);
        const x = interp.outPosition[0];
        if (k >= 2) {
          minX = Math.min(minX, x);
          maxX = Math.max(maxX, x);
          if (Number.isFinite(lastX)) back = Math.max(back, lastX - x);
        }
        lastX = x;
      }
    }
    expect(back).toBeLessThan(1e-3);
    // Held stream past the last snapshot may dead-reckon at most the extrapolation budget.
    expect(maxX).toBeLessThan(9.4 + 11 * STEP * 1.2 + 1e-3);
  });

  it("holds exactly the newest snapshot while the run is paused, and resumes without a jump", () => {
    const interp = new PoseInterpolator();
    const at = (k: number): PlacedActor => ({ actorId: 2, x: k, y: 0, headingRad: 0, speedMps: 10 });
    let clock = 0;
    for (let k = 0; k < 10; k++) {
      interp.capture(placedPoses([at(k)], BigInt(k) * 100_000_000n), k * STEP);
      for (; clock < (k + 1) * STEP; clock += 1 / FPS) interp.sample(clock);
    }
    // Paused: without the hold the sampler glides 1.2 steps past the last pose (measured on the old
    // sampler: 10.17 m for a vehicle that stopped at 9).
    interp.setHeld(true);
    let last = Number.NaN;
    let worstJump = 0;
    for (let i = 0; i < 120; i++, clock += 1 / FPS) {
      interp.sample(clock);
      if (Number.isFinite(last)) worstJump = Math.max(worstJump, Math.abs(interp.outPosition[0] - last));
      last = interp.outPosition[0];
    }
    expect(interp.outPosition[0]).toBeCloseTo(9, 3);
    // Getting there is blended, not a cut.
    expect(worstJump).toBeLessThan(0.2);
    // Resume: new snapshots arrive, the picture moves forward from where it is.
    interp.setHeld(false);
    let back = 0;
    for (let k = 10; k < 20; k++) {
      interp.capture(placedPoses([at(k)], BigInt(k) * 100_000_000n), clock);
      const end = clock + STEP;
      for (; clock < end; clock += 1 / FPS) {
        interp.sample(clock);
        back = Math.max(back, last - interp.outPosition[0]);
        last = interp.outPosition[0];
      }
    }
    expect(back).toBeLessThan(1e-3);
  });

  it("blends out a correction instead of snapping when extrapolation guessed wrong", () => {
    // Data arrives late (a stall of three steps), the sampler dead-reckons at 10 m/s, and the
    // vehicle had in fact braked to a stop. The picture must decelerate, not jump backwards (the old
    // sampler: a 0.54 m jump in one frame).
    const interp = new PoseInterpolator();
    let clock = 0;
    const xs = [0, 1, 2, 3, 3.5, 3.5, 3.5];
    const vs = [10, 10, 10, 10, 0, 0, 0];
    let last = Number.NaN;
    let worst = 0;
    for (let k = 0; k < xs.length; k++) {
      const arrive = k < 4 ? k * STEP : (k + 2) * STEP;
      while (clock < arrive) {
        interp.sample(clock);
        if (k > 1 && Number.isFinite(last)) worst = Math.max(worst, Math.abs(interp.outPosition[0] - last));
        last = interp.outPosition[0];
        clock += 1 / FPS;
      }
      interp.capture(placedPoses([{ actorId: 4, x: xs[k], y: 0, headingRad: 0, speedMps: vs[k] }], BigInt(k) * 100_000_000n), arrive);
    }
    for (let i = 0; i < 60; i++, clock += 1 / FPS) {
      interp.sample(clock);
      worst = Math.max(worst, Math.abs(interp.outPosition[0] - last));
      last = interp.outPosition[0];
    }
    expect(worst).toBeLessThan((10 / FPS) * 1.6);
    expect(interp.outPosition[0]).toBeCloseTo(3.5, 2);
  });
});
