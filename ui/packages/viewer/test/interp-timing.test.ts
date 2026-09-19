/**
 * The Q1/Q2/Q7 timing regressions.
 *
 * Q1: pose interpolation must be keyed off `simTimeNs` (the authoritative clock the engine stamps
 * every frame with) and not off the wall-clock instant a frame happened to arrive, so arrival
 * jitter cannot turn into actor motion.
 *
 * Q2: the render delay and the extrapolation ceiling must be multiples of the *measured* snapshot
 * interval, so a slow stream (0.1x–0.5x playback, a keyframe-only run, a congested link) is
 * interpolated exactly like a fast one.
 *
 * Q7: the extrapolation ceiling must be continuous — no frame may snap an actor backwards.
 *
 * Every number asserted here is a measurement from `measureMotion`, and the review's own
 * measurements are quoted beside each one.
 */

import { describe, expect, it } from "vitest";
import { PoseInterpolator } from "../src/interp.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";
import { formatMotion, measureMotion } from "./support/timing.js";

const grid = makeGridWorld({ blocks: 24, blockM: 200 });

describe("PoseInterpolator — timing (Q1, Q2, Q7)", () => {
  it("rejects arrival jitter because it interpolates on sim time (Q1)", () => {
    // The review's experiment: sim cadence exactly 0.1 s, arrival jitter ±40 ms.
    const jittered = measureMotion({
      grid, intervalSeconds: 0.1, jitterSeconds: 0.04, jitterPattern: "alternating", snapshots: 40,
    });
    const random = measureMotion({
      grid, intervalSeconds: 0.1, jitterSeconds: 0.04, jitterPattern: "uniform", snapshots: 40,
    });
    const regular = measureMotion({ grid, intervalSeconds: 0.1, jitterSeconds: 0, snapshots: 40 });

    // eslint-disable-next-line no-console
    console.log([
      "",
      "--- Q1: ±40 ms arrival jitter on an exact 0.1 s sim cadence -------------",
      formatMotion("±40 ms, worst case", jittered),
      formatMotion("±40 ms, uniform", random),
      formatMotion("no jitter", regular),
      `nominal per-frame move ${(regular.speedMps / 60).toFixed(4)} m at ${regular.speedMps.toFixed(2)} m/s`,
      "",
    ].join("\n"));

    // Measured before the fix: 0.0000–15.1205 m, ~15,000x max/min, frames frozen then teleporting.
    expect(jittered.frozen).toBe(0);
    expect(jittered.minMotion).toBeGreaterThan(0);
    // Jitter must not widen the motion band by more than a few per cent over the jitter-free run.
    expect(jittered.maxMotion).toBeLessThan(regular.maxMotion * 1.1 + 1e-3);
    expect(jittered.jumpRatio).toBeLessThan(1.5);
    // And the motion must stay at the nominal 60 fps step of the tracked actor.
    expect(jittered.medianMotion).toBeCloseTo(jittered.speedMps / 60, 2);
    expect(random.frozen).toBe(0);
    expect(random.jumpRatio).toBeLessThan(1.5);
  });

  it("interpolates a slow stream as well as a fast one (Q2)", () => {
    const rows = [0.1, 0.2, 0.35, 0.5, 1.0, 2.0].map((interval) => ({
      interval,
      stats: measureMotion({ grid, intervalSeconds: interval, snapshots: 12 }),
    }));

    // eslint-disable-next-line no-console
    console.log([
      "",
      "--- Q2: snapshot interval sweep, 60 fps sampling ------------------------",
      ...rows.map((r) => formatMotion(`interval ${r.interval.toFixed(2)} s`, r.stats)),
      "",
    ].join("\n"));

    // Measured before the fix: 0 % frozen up to 0.35 s, then 13 % at 0.5 s (3.175 m jump),
    // 57 % at 1.0 s (9.755 m), 82 % at 2.0 s (24.762 m).
    for (const r of rows) {
      expect(r.stats.frozen, `interval ${r.interval}`).toBe(0);
      expect(r.stats.jumpRatio, `interval ${r.interval}`).toBeLessThan(1.5);
      expect(r.stats.medianMotion, `interval ${r.interval}`).toBeCloseTo(r.stats.speedMps / 60, 2);
    }
  });

  it("scales the extrapolation budget with the interval too (Q2)", () => {
    // A slow stream with proportional jitter is what separates a *relative* extrapolation budget
    // from an absolute one: the sampler has to run up to two jitter widths past the newest
    // snapshot, which at a 1 s cadence is far beyond any fixed 0.12 s ceiling.
    const jittered = [0.5, 1.0, 2.0].map((interval) => ({
      interval,
      stats: measureMotion({
        grid, intervalSeconds: interval, jitterSeconds: interval * 0.1, snapshots: 12,
      }),
    }));

    // eslint-disable-next-line no-console
    console.log([
      "",
      "--- Q2: slow stream with ±10 % arrival jitter ---------------------------",
      ...jittered.map((r) => formatMotion(`interval ${r.interval.toFixed(2)} s`, r.stats)),
      "",
    ].join("\n"));

    for (const r of jittered) {
      expect(r.stats.frozen, `interval ${r.interval}`).toBe(0);
      expect(r.stats.maxReversalM, `interval ${r.interval}`).toBeLessThan(1e-4);
      expect(r.stats.jumpRatio, `interval ${r.interval}`).toBeLessThan(1.5);
    }
  });

  it("never snaps an actor backwards at the stall boundary (Q7)", () => {
    // Two snapshots, then silence across the extrapolation limit and the stall boundary.
    const stats = measureMotion({
      grid, intervalSeconds: 0.1, snapshots: 2, fps: 60,
      interp: { maxExtrapolationSeconds: 0.12, stallSeconds: 0.4 },
    });
    // A dedicated run of the boundary itself: capture twice, then sample across the stall edge.
    const interp = new PoseInterpolator({ maxExtrapolationSeconds: 0.12, stallSeconds: 0.4 });
    const stream = new SyntheticStream(8, grid);
    stream.keyframe();
    interp.capture(stream.poses, 0);
    stream.advance(0.1);
    stream.delta();
    interp.capture(stream.poses, 0.1);

    let worstBack = 0;
    let worstFrame = 0;
    const prev = new Float32Array(24);
    interp.sample(0.1);
    prev.set(interp.outPosition.subarray(0, 24));
    // Track one actor's per-frame motion so the *shape* of the stop can be asserted, not just its
    // sign: a hard clip goes from full speed to zero in a single frame, an ease does not.
    const profile: number[] = [];
    for (let t = 0.1 + 1 / 60; t < 1.2; t += 1 / 60) {
      const info = interp.sample(t);
      let frame = 0;
      for (let i = 0; i < 8; i++) {
        const dx = interp.outPosition[i * 3] - prev[i * 3];
        const dy = interp.outPosition[i * 3 + 1] - prev[i * 3 + 1];
        const h = interp.outHeading[i];
        const along = dx * Math.cos(h) + dy * Math.sin(h);
        if (-along > worstBack) worstBack = -along;
        frame = Math.max(frame, Math.hypot(dx, dy));
      }
      profile.push(Math.hypot(
        interp.outPosition[0] - prev[0],
        interp.outPosition[1] - prev[1],
      ));
      if (frame > worstFrame) worstFrame = frame;
      prev.set(interp.outPosition.subarray(0, 24));
      expect(info.alpha).toBeLessThanOrEqual(2.2 + 1e-6);
    }
    const nominal = Math.max(...profile);
    const partial = profile.filter((m) => m > nominal * 0.05 && m < nominal * 0.95).length;

    // eslint-disable-next-line no-console
    console.log([
      "",
      "--- Q7: crossing the stall boundary ------------------------------------",
      `worst backwards step ${worstBack.toFixed(4)} m, worst single-frame move ${worstFrame.toFixed(4)} m`,
      `deceleration profile (m/frame): ${profile.filter((m) => m > 1e-6).map((m) => m.toFixed(3)).join(" ")}`,
      `(review measured a 1.801 m backwards snap, 7.2x the nominal 0.2502 m frame)`,
      "",
    ].join("\n"));

    // Measured before the fix: 1.801 m backwards in one frame.
    expect(worstBack).toBeLessThan(1e-4);
    expect(stats.maxReversalM).toBeLessThan(1e-4);
    // And the stop is a glide, not a clip: the review's version went from a full 0.25 m frame to a
    // 1.8 m lurch backwards in one step, and a version that merely clipped the ceiling would go
    // from full speed to zero in one frame. Several frames of decreasing partial motion is the
    // shape an ease produces, and the only shape that reads as deceleration on screen.
    expect(partial).toBeGreaterThanOrEqual(3);
    const tail = profile.slice(profile.findIndex((m) => m < nominal * 0.95));
    let worstDrop = 0;
    for (let i = 1; i < tail.length; i++) {
      expect(tail[i], `frame ${i} of the stop`).toBeLessThanOrEqual(tail[i - 1] + 1e-4);
      worstDrop = Math.max(worstDrop, tail[i - 1] - tail[i]);
    }
    // eslint-disable-next-line no-console
    console.log(
      `  largest single-frame deceleration ${(worstDrop / nominal * 100).toFixed(0)} % of a nominal `
      + `frame (a clipped ceiling drops ~73 %, a step to a full stop 100 %)`,
    );
    expect(worstDrop).toBeLessThan(nominal * 0.4);
  });
});
