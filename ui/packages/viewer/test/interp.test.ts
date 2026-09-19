import { describe, expect, it } from "vitest";
import { PoseInterpolator, lerpAngle } from "../src/interp.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";

describe("lerpAngle", () => {
  it("takes the short way round the wrap", () => {
    expect(lerpAngle(0.1, 6.0, 0.5)).toBeCloseTo(-0.0915, 3);
    expect(lerpAngle(0, Math.PI / 2, 0.5)).toBeCloseTo(Math.PI / 4, 6);
    // Crossing the ±π seam the short way lands on the seam itself; −π and π are the same angle.
    expect(Math.abs(lerpAngle(-Math.PI + 0.1, Math.PI - 0.1, 0.5))).toBeCloseTo(Math.PI, 6);
  });
});

describe("PoseInterpolator", () => {
  const grid = makeGridWorld({ blocks: 4, blockM: 120 });

  it("interpolates between the two most recent snapshots", () => {
    const stream = new SyntheticStream(64, grid);
    stream.keyframe();
    const interp = new PoseInterpolator({ delaySteps: 1, maxDelaySeconds: 0.35 });
    interp.capture(stream.poses, 0);
    const p0 = Float32Array.from(stream.poses.positions.subarray(0, 192));
    stream.advance(0.1);
    stream.delta();
    interp.capture(stream.poses, 0.1);
    const p1 = Float32Array.from(stream.poses.positions.subarray(0, 192));

    // Render one whole step behind the newest snapshot: alpha ≈ 0 at t = 0.1.
    let info = interp.sample(0.1);
    expect(info.alpha).toBeGreaterThanOrEqual(0);
    expect(info.alpha).toBeLessThan(0.15);
    expect(interp.outPosition[0]).toBeCloseTo(p0[0], 3);

    // Half a step later, roughly half way between the two.
    info = interp.sample(0.15);
    expect(info.alpha).toBeGreaterThan(0.35);
    expect(info.alpha).toBeLessThan(0.65);
    const mid = (p0[0] + p1[0]) / 2;
    expect(Math.abs(interp.outPosition[0] - mid)).toBeLessThan(Math.abs(p1[0] - p0[0]) * 0.2 + 0.01);

    // A whole step later: at the newest snapshot.
    info = interp.sample(0.2);
    expect(info.alpha).toBeGreaterThan(0.9);
    expect(interp.outPosition[0]).toBeCloseTo(p1[0], 2);
  });

  it("glides to a stop past the newest snapshot and never reverses (Q7)", () => {
    const stream = new SyntheticStream(32, grid);
    stream.keyframe();
    const interp = new PoseInterpolator({ maxExtrapolationSeconds: 0.12, stallSeconds: 0.4 });
    interp.capture(stream.poses, 0);
    stream.advance(0.1);
    stream.delta();
    interp.capture(stream.poses, 0.1);

    // Sample continuously across the extrapolation limit and the stall boundary. The old
    // behaviour was a step: `alphaCap` fell from 2.2 to 1.0 the instant `silence` crossed
    // `stallSeconds`, snapping every actor 1.2 steps *backwards* in one frame. Nothing may move
    // backwards now, and alpha may never exceed 1 + maxExtrapolationSteps.
    interp.sample(0.1);
    let prev = Float32Array.from(interp.outPosition.subarray(0, 96));
    let worstBackwards = 0;
    for (let t = 0.1 + 1 / 60; t < 2; t += 1 / 60) {
      const info = interp.sample(t);
      expect(info.alpha).toBeLessThanOrEqual(2.2 + 1e-6);
      for (let i = 0; i < 32; i++) {
        const dx = interp.outPosition[i * 3] - prev[i * 3];
        const dy = interp.outPosition[i * 3 + 1] - prev[i * 3 + 1];
        const h = interp.outHeading[i];
        const along = dx * Math.cos(h) + dy * Math.sin(h);
        if (-along > worstBackwards) worstBackwards = -along;
      }
      prev = Float32Array.from(interp.outPosition.subarray(0, 96));
    }
    expect(worstBackwards).toBeLessThan(1e-4);
    expect(interp.lastSample.stalled).toBe(true);

    // And it comes to rest: after 2 s of silence the pose is already within millimetres of its
    // resting place, 30 s more silence moves it no further, and the resting place is exactly the
    // 1.2-step budget past the newest snapshot — no further, whatever the silence.
    const resting = Float32Array.from(interp.outPosition.subarray(0, 96));
    const alphaAt2s = interp.lastSample.alpha;
    for (let t = 2; t < 32; t += 0.5) interp.sample(t);
    let drift = 0;
    for (let i = 0; i < 96; i++) drift = Math.max(drift, Math.abs(interp.outPosition[i] - resting[i]));
    let fastest = 0;
    for (let i = 0; i < 32; i++) fastest = Math.max(fastest, interp.outSpeed[i]);
    // eslint-disable-next-line no-console
    console.log(
      `extrapolation ease: alpha ${alphaAt2s.toFixed(4)} after 2 s of silence → `
      + `${interp.lastSample.alpha.toFixed(4)} after 32 s, ${(drift * 1000).toFixed(1)} mm of further `
      + `drift (a 1/60 s frame of the fastest actor is ${(fastest / 60).toFixed(3)} m)`,
    );
    // The ease converges on the budget asymptotically, so "at rest" is a bound, not an equality:
    // 30 s more silence may add a few centimetres and no more, and never leaves the budget.
    expect(alphaAt2s).toBeGreaterThan(2.1);
    expect(interp.lastSample.alpha).toBeLessThanOrEqual(2.2 + 1e-9);
    expect(drift).toBeLessThan(fastest / 60 / 4);
  });

  it("snaps rather than interpolates when a slot changes actor or teleports", () => {
    const stream = new SyntheticStream(8, grid);
    stream.keyframe();
    const interp = new PoseInterpolator({ teleportMetres: 40 });
    interp.capture(stream.poses, 0);
    // A seek: the whole crowd jumps far, delivered as a fresh keyframe.
    stream.advance(60);
    stream.nextGop();
    stream.keyframe();
    interp.capture(stream.poses, 0.1);
    const info = interp.sample(0.15);
    expect(info.snapped).toBeGreaterThan(0);
    for (let i = 0; i < 8; i++) {
      expect(interp.outPosition[i * 3]).toBeCloseTo(stream.poses.positions[i * 3], 3);
    }
  });

  it("clears to the live count, not to capacity (Q18)", () => {
    // Capacity comes from `Hello.actor_capacity`, so a 20,480-slot run with 8 live actors used to
    // write 20,472 zeros a frame. Correctness first: a shrinking count must still clear.
    const interp = new PoseInterpolator({ capacity: 20_480 });
    const many = new SyntheticStream(300, grid);
    many.keyframe();
    interp.capture(many.poses, 0);
    many.advance(0.1);
    many.delta();
    interp.capture(many.poses, 0.1);
    interp.sample(0.1);
    expect(interp.count).toBe(300);

    const few = new SyntheticStream(8, grid);
    few.keyframe();
    interp.reset();
    interp.capture(few.poses, 1);
    few.advance(0.1);
    few.delta();
    interp.capture(few.poses, 1.1);
    interp.sample(1.1);
    expect(interp.count).toBe(8);
    for (let i = 8; i < 320; i++) expect(interp.outOccupied[i]).toBe(0);

    // Now the O(count) property itself, deterministically: a sentinel planted far above the live
    // count survives the next sample, because that sample has no business touching it. Before the
    // fix the loop ran to capacity and wiped it.
    interp.outOccupied[10_000] = 7;
    interp.sample(1.11);
    expect(interp.outOccupied[10_000]).toBe(7);
    interp.outOccupied[10_000] = 0;

    // The guard is the *quantity* the fix was about, counted exactly: how many occupancy slots one
    // `sample()` writes above the live count. This used to be asserted as a wall-clock ratio
    // (`large < small * 2`), which is not a sound test: the ratio is 0.91x-1.17x on an idle
    // machine but reached 2.18x with four packages testing concurrently, because the absolute
    // times inflate about threefold under load while the fixed per-call overhead does not. It
    // failed 2 of 7 workspace runs. The count below is the same property, load-independent, and it
    // discriminates far better than the ratio ever did: 0 against 20,280.
    const census = new PoseInterpolator({ capacity: 20_480 });
    const live = new SyntheticStream(200, grid);
    live.keyframe();
    census.capture(live.poses, 0);
    live.advance(0.1);
    live.delta();
    census.capture(live.poses, 0.1);
    census.sample(0.1);
    expect(census.count).toBe(200);
    expect(census.capacity).toBe(20_480); // `Hello.actor_capacity`, not the live count.

    const SENTINEL = 0xab;
    census.outOccupied.fill(SENTINEL, 200);
    const frames = 12;
    for (let i = 1; i <= frames; i++) census.sample(0.1 + i / 60);
    let touched = 0;
    for (let i = 200; i < census.capacity; i++) if (census.outOccupied[i] !== SENTINEL) touched++;
    // Before the fix both `sample()` and `#copySnapshot` looped to capacity, so every one of these
    // frames wrote all 20,280 of them.
    expect(census.capacity - 200).toBe(20_280);
    expect(touched).toBe(0);

    // Measured too, the way the review measured it — reported, not asserted, because a wall-clock
    // number on a shared machine is evidence and not a bound.
    const time = (capacity: number, liveActors: number): number => {
      const it = new PoseInterpolator({ capacity });
      const stream = new SyntheticStream(liveActors, grid);
      stream.keyframe();
      it.capture(stream.poses, 0);
      stream.advance(0.1);
      stream.delta();
      it.capture(stream.poses, 0.1);
      const N = 20_000;
      for (let i = 0; i < 4000; i++) it.sample(0.1 + i * 1e-4); // JIT warm-up
      const t0 = performance.now();
      for (let i = 0; i < N; i++) it.sample(0.1 + i * 1e-4);
      return ((performance.now() - t0) / N) * 1000;
    };
    const large = time(20_480, 200);
    const small = time(256, 200);
    // eslint-disable-next-line no-console
    console.log(
      `sample() with 200 live actors: ${small.toFixed(2)} us at capacity 256 vs `
      + `${large.toFixed(2)} us at capacity 20,480 = ${(large / small).toFixed(2)}x `
      + `(review measured 2.35 vs 14.52 us, 6.2x); slots written above the live count over `
      + `${frames} frames at capacity 20,480: ${touched} of ${census.capacity - 200}`,
    );
  });

  it("grows without losing what it already held", () => {
    const interp = new PoseInterpolator({ capacity: 16 });
    const stream = new SyntheticStream(400, grid);
    stream.keyframe();
    interp.capture(stream.poses, 0);
    expect(interp.capacity).toBeGreaterThanOrEqual(400);
    interp.sample(0);
    expect(interp.count).toBe(400);
    expect(interp.outActorId[399]).toBe(400);
  });
});
