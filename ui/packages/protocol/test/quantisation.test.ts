/**
 * §3.2 quantisation and the conformance checks of §10.3:
 * Q1 (exact quantisation for the §9 vectors), Q2 `no_quantisation_drift`, Q3 `teleport_escape`.
 */

import { describe, expect, it } from "vitest";

import {
  ACCEL_SCALE,
  BRAD_PER_TURN,
  DELTA_ESCAPE_MM,
  MM_PER_CM,
  MM_PER_M,
  MovedFlags,
  PoseBuffer,
  SPEED_SCALE,
  dequantiseAccelMps2,
  dequantiseHeadingRad,
  dequantiseHeightM,
  dequantisePositionM,
  dequantiseSpeedMps,
  needsAbsoluteEscape,
  quantiseAccelCq,
  quantiseHeadingBrad,
  quantiseHeightCm,
  quantisePositionMm,
  quantiseSpeedCq,
  roundHalfAwayFromZero,
} from "../src/index.js";
import { buildDelta, buildKeyframe } from "./helpers/build-frames.js";

describe("§3.2 — the normative quantisation rule", () => {
  it("rounds half away from zero, not half up", () => {
    expect(roundHalfAwayFromZero(0.5)).toBe(1);
    expect(roundHalfAwayFromZero(-0.5)).toBe(-1);
    expect(roundHalfAwayFromZero(1.5)).toBe(2);
    expect(roundHalfAwayFromZero(-1.5)).toBe(-2);
    expect(roundHalfAwayFromZero(2.4)).toBe(2);
    expect(roundHalfAwayFromZero(-2.4)).toBe(-2);
    // Math.round would give -0 here, which is the whole reason for the helper.
    expect(Math.round(-0.5)).not.toBe(-1);
  });

  it("reproduces the §9 worked example exactly", () => {
    expect(quantisePositionMm(12.345 - -500)).toBe(512345);
    expect(quantisePositionMm(-3.21 - -500)).toBe(496790);
    expect(quantiseHeightCm(0.15 - 0)).toBe(15);
    expect(quantiseHeadingBrad(0)).toBe(0);
    expect(quantiseHeadingBrad(Math.PI)).toBe(32768);
    expect(quantiseHeadingBrad(Math.PI / 2)).toBe(16384);
    expect(quantiseSpeedCq(13.89)).toBe(1778);
    expect(quantiseSpeedCq(11)).toBe(1408);
    expect(quantiseAccelCq(0.5)).toBe(32);
    expect(quantiseAccelCq(-1.2)).toBe(-77);
    expect(quantisePositionMm(-20 - -500)).toBe(480000);
    expect(quantisePositionMm(0.5 - -500)).toBe(500500);
    expect(quantisePositionMm(3 - -500)).toBe(503000);
    expect(quantisePositionMm(41.25 - -500)).toBe(541250);
  });

  it("wraps headings by construction — no ±π branch", () => {
    expect(quantiseHeadingBrad(2 * Math.PI)).toBe(0);
    expect(quantiseHeadingBrad(-Math.PI / 2)).toBe(49152);
    expect(quantiseHeadingBrad(5 * 2 * Math.PI + Math.PI)).toBe(32768);
    for (const brad of [0, 1, 16384, 32768, 49152, 65535]) {
      expect(quantiseHeadingBrad(dequantiseHeadingRad(brad))).toBe(brad);
    }
  });

  it("clamps at the type bounds instead of wrapping", () => {
    expect(quantiseSpeedCq(1e6)).toBe(32767);
    expect(quantiseSpeedCq(-1e6)).toBe(-32768);
    expect(quantiseHeightCm(1e9)).toBe(32767);
    expect(quantisePositionMm(1e12)).toBe(2147483647);
  });

  it("round-trips inside half a quantisation step", () => {
    const halfStepM = 0.5 / MM_PER_M;
    const halfStepZ = 0.5 / 100;
    const halfStepHeading = Math.PI / BRAD_PER_TURN;
    const halfStepSpeed = 0.5 / SPEED_SCALE;
    const halfStepAccel = 0.5 / ACCEL_SCALE;
    let seed = 0x1234_5678;
    const rand = (): number => {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      return seed / 0x7fffffff;
    };
    for (let i = 0; i < 20_000; i++) {
      const x = (rand() - 0.5) * 4000;
      const z = (rand() - 0.5) * 200;
      const hdg = rand() * 2 * Math.PI;
      const spd = (rand() - 0.5) * 100;
      const acc = (rand() - 0.5) * 20;
      expect(Math.abs(dequantisePositionM(quantisePositionMm(x), 0) - x)).toBeLessThanOrEqual(halfStepM + 1e-9);
      expect(Math.abs(dequantiseHeightM(quantiseHeightCm(z), 0) - z)).toBeLessThanOrEqual(halfStepZ + 1e-9);
      const dh = Math.abs(dequantiseHeadingRad(quantiseHeadingBrad(hdg)) - hdg);
      expect(Math.min(dh, 2 * Math.PI - dh)).toBeLessThanOrEqual(halfStepHeading + 1e-9);
      expect(Math.abs(dequantiseSpeedMps(quantiseSpeedCq(spd)) - spd)).toBeLessThanOrEqual(halfStepSpeed + 1e-9);
      expect(Math.abs(dequantiseAccelMps2(quantiseAccelCq(acc)) - acc)).toBeLessThanOrEqual(halfStepAccel + 1e-9);
    }
  });
});

describe("§10.3 Q2 no_quantisation_drift — 10,000 delta steps", () => {
  it("keyframe + all deltas reproduces the producer's quantised state exactly, with ≤ 1 mm deviation", () => {
    const STEPS = 10_000;
    const originX = -500;
    const originY = -500;
    const originZ = 0;

    // The producer's true, continuous trajectory: a curving path so dx and dy are never round.
    const truth = (step: number): { x: number; y: number; z: number; hdg: number; spd: number } => {
      const t = step * 0.1;
      return {
        x: 12.345 + 13.8901 * t * Math.cos(0.0007 * t),
        y: -3.21 + 13.8901 * t * Math.sin(0.0007 * t),
        // §3.2/§3.4.2: the amplitude must be tens of centimetres, otherwise z_cm never changes,
        // dz_mm is identically 0 and the delta z path is not exercised at all (the bug this guards).
        z: 1.47 + 0.83 * Math.sin(0.5 * t),
        hdg: 0.0007 * t,
        spd: 13.89 + 0.37 * Math.sin(0.013 * t),
      };
    };

    // Producer state: the previously **transmitted quantised** value (§3.2 delta reference rule).
    const p0 = truth(0);
    let refX = quantisePositionMm(p0.x - originX);
    let refY = quantisePositionMm(p0.y - originY);
    // §3.4.2 `dz_mm` is millimetres, so the producer's z reference is its quantised z in
    // millimetres: `z_cm · 10`. The client must hold the same unit (`PoseBuffer.zMm`).
    let refZ = quantiseHeightCm(p0.z - originZ) * MM_PER_CM;

    const poses = new PoseBuffer(4);
    poses.applyKeyframe(
      buildKeyframe({
        simTimeNs: 0n, originXM: originX, originYM: originY, originZM: originZ, gopIndex: 0,
        actors: [{ actorId: 7, xMm: refX, yMm: refY, zCm: refZ / MM_PER_CM, laneId: 1,
          headingBrad: quantiseHeadingBrad(p0.hdg), speedCq: quantiseSpeedCq(p0.spd), accelCq: 0,
          classIdx: 0, state: 0x08, verifiedNeighbors: 3 }],
        signals: [],
      }),
    );

    let maxDeviationMm = 0;
    let maxErrorVsTruthM = 0;
    let maxZDeviationMm = 0;
    let stepsWithVerticalMotion = 0;
    const zBuckets = new Set<number>();
    let maxZErrorVsTruthM = 0;

    for (let step = 1; step <= STEPS; step++) {
      const t = truth(step);
      const qx = quantisePositionMm(t.x - originX);
      const qy = quantisePositionMm(t.y - originY);
      const qzCm = quantiseHeightCm(t.z - originZ);
      const qzMm = qzCm * MM_PER_CM;

      const dx = qx - refX;
      const dy = qy - refY;
      const dz = qzMm - refZ;
      if (dz !== 0) stepsWithVerticalMotion++;
      zBuckets.add(qzCm);
      expect(needsAbsoluteEscape(dx, dy, dz)).toBe(false);

      poses.applyDelta(
        buildDelta({
          simTimeNs: BigInt(step) * 100_000_000n, gopIndex: 0, stepIndex: step,
          moved: [{ slot: 0, dxMm: dx, dyMm: dy, dzMm: dz, headingBrad: quantiseHeadingBrad(t.hdg),
            speedCq: quantiseSpeedCq(t.spd), accelCq: 0, state: 0x08, verifiedNeighbors: 3, mflags: 0 }],
        }),
      );

      // The producer's reference is its own quantised value, never the true one.
      refX = qx;
      refY = qy;
      refZ = qzMm;

      // Client state must equal producer state bit for bit at every step.
      expect(poses.xMm[0]).toBe(qx);
      expect(poses.yMm[0]).toBe(qy);
      expect(poses.zMm[0]).toBe(qzMm);
      expect(poses.zCm[0]).toBe(qzCm);

      maxDeviationMm = Math.max(maxDeviationMm, Math.abs(poses.xMm[0] - qx), Math.abs(poses.yMm[0] - qy));
      maxZDeviationMm = Math.max(maxZDeviationMm, Math.abs(poses.zMm[0] - qzMm));
      const pos = poses.positionOf(0);
      maxErrorVsTruthM = Math.max(maxErrorVsTruthM, Math.abs(pos.x - t.x), Math.abs(pos.y - t.y));
      maxZErrorVsTruthM = Math.max(maxZErrorVsTruthM, Math.abs(pos.z - t.z));
    }

    // Q2: "10,000 steps, assert max deviation ≤ 1 mm".
    expect(maxDeviationMm).toBe(0);
    expect(maxZDeviationMm).toBe(0);
    expect(maxErrorVsTruthM).toBeLessThanOrEqual(0.0005 + 1e-9); // half a 1 mm step, and it never grows
    expect(maxZErrorVsTruthM).toBeLessThanOrEqual(0.005 + 1e-9); // half a 1 cm step on z, and it never grows
    // The vertical axis must actually move, or this test proves nothing about `dz_mm`.
    expect(stepsWithVerticalMotion).toBeGreaterThan(STEPS * 0.9);
    expect(zBuckets.size).toBeGreaterThan(100);
    expect(poses.stepIndex).toBe(STEPS);
  });

  it("naive delta coding against the true value would drift — this is what the rule prevents", () => {
    // Same trajectory, but the producer references its *unquantised* state, as §3.2 forbids.
    let clientMm = 0;
    let maxErr = 0;
    for (let step = 1; step <= 10_000; step++) {
      const trueM = step * 0.0001234; // a displacement that never lands on a whole millimetre
      const prevM = (step - 1) * 0.0001234;
      const dWrong = quantisePositionMm(trueM - prevM); // quantise the *difference*: rounding each step
      clientMm += dWrong;
      maxErr = Math.max(maxErr, Math.abs(clientMm / MM_PER_M - trueM));
    }
    expect(maxErr).toBeGreaterThan(0.001); // more than a millimetre: drift has accumulated
  });
});

describe("§3.2/§3.4.2 — `dz_mm` is millimetres, and the z reference is millimetres too", () => {
  /** One actor at (0, 0, `zCm`) about a zero origin. */
  const keyframeAtZ = (zCm: number): ReturnType<typeof buildKeyframe> =>
    buildKeyframe({
      simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0,
      actors: [{ actorId: 1, xMm: 0, yMm: 0, zCm, laneId: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 }],
      signals: [],
    });

  /** One moved row for slot 0 carrying nothing but a displacement. */
  const step = (n: number, dxMm: number, dyMm: number, dzMm: number): ReturnType<typeof buildDelta> =>
    buildDelta({
      simTimeNs: BigInt(n) * 100_000_000n, gopIndex: 0, stepIndex: n,
      moved: [{ slot: 0, dxMm, dyMm, dzMm, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: 0 }],
    });

  it("moves z by exactly 0.100 m for dz_mm = +100 — not by 1.000 m", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15)); // z = 0.15 m (§9 worked example)
    expect(poses.positionOf(0).z).toBeCloseTo(0.15, 12);

    expect(poses.applyDelta(step(1, 0, 0, 100)).applied).toBe(true);

    expect(poses.zMm[0]).toBe(250);
    expect(poses.zCm[0]).toBe(25);
    expect(poses.positionOf(0).z).toBeCloseTo(0.25, 12);
    expect(poses.positions[2]).toBeCloseTo(0.25, 6);
    // The 10x bug this guards against: dz_mm added to a centimetre accumulator gave 1.15 m.
    expect(poses.positionOf(0).z).not.toBeCloseTo(1.15, 6);
  });

  it("treats dx, dy and dz as the same unit — one millimetre", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(0));
    expect(poses.applyDelta(step(1, 100, 100, 100)).applied).toBe(true);
    const p = poses.positionOf(0);
    expect(p.x).toBeCloseTo(0.1, 12);
    expect(p.y).toBeCloseTo(0.1, 12);
    expect(p.z).toBeCloseTo(0.1, 12);
  });

  it("applies a negative dz_mm in millimetres", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15));
    expect(poses.applyDelta(step(1, 0, 0, -150)).applied).toBe(true);
    expect(poses.zMm[0]).toBe(0);
    expect(poses.zCm[0]).toBe(0);
    expect(poses.positionOf(0).z).toBeCloseTo(0, 12);
  });

  it("accumulates sub-centimetre deltas without drift (§3.2 delta reference rule)", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15)); // 150 mm
    for (let n = 1; n <= 10; n++) {
      expect(poses.applyDelta(step(n, 0, 0, 3)).applied).toBe(true);
      expect(poses.zMm[0]).toBe(150 + 3 * n); // millimetre-exact, never rounded away
    }
    expect(poses.zMm[0]).toBe(180);
    expect(poses.zCm[0]).toBe(18);
    expect(poses.positionOf(0).z).toBeCloseTo(0.18, 12);
  });

  it("mirrors zCm from zMm with §3.2 half-away-from-zero rounding", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15));
    expect(poses.applyDelta(step(1, 0, 0, 15)).applied).toBe(true); // 165 mm
    expect(poses.zMm[0]).toBe(165);
    expect(poses.zCm[0]).toBe(17); // 16.5 rounds away from zero
    expect(poses.positionOf(0).z).toBeCloseTo(0.165, 12); // the metre value keeps the millimetre
  });

  it("re-seeds the millimetre reference from the absolute block (§3.4.3, z_cm)", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15));
    poses.applyDelta(
      buildDelta({
        simTimeNs: 100_000_000n, gopIndex: 0, stepIndex: 1,
        moved: [{ slot: 0, dxMm: 0, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: MovedFlags.ABSOLUTE }],
        absolute: [{ xMm: 0, yMm: 0, zCm: 120 }], // 1.20 m
      }),
    );
    expect(poses.zMm[0]).toBe(120 * MM_PER_CM);
    expect(poses.positionOf(0).z).toBeCloseTo(1.2, 12);
    // …and the next millimetre delta is applied against it, still in millimetres.
    expect(poses.applyDelta(step(2, 0, 0, -200)).applied).toBe(true);
    expect(poses.zMm[0]).toBe(1000);
    expect(poses.zCm[0]).toBe(100);
    expect(poses.positionOf(0).z).toBeCloseTo(1.0, 12);
  });

  it("re-seeds the millimetre reference from a spawn row (§3.4.5, z_cm)", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(15));
    poses.applyDelta(
      buildDelta({
        simTimeNs: 100_000_000n, gopIndex: 0, stepIndex: 1,
        spawns: [{ slot: 1, actorId: 9, nodeId: 4, xMm: 0, yMm: 0, laneId: 0, zCm: 30, headingBrad: 0, speedCq: 0, cause: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 }],
      }),
    );
    expect(poses.zMm[1]).toBe(300);
    expect(poses.positionOf(1).z).toBeCloseTo(0.3, 12);
    poses.applyDelta(
      buildDelta({
        simTimeNs: 200_000_000n, gopIndex: 0, stepIndex: 2,
        moved: [{ slot: 1, dxMm: 0, dyMm: 0, dzMm: 100, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: 0 }],
      }),
    );
    expect(poses.zMm[1]).toBe(400);
    expect(poses.positionOf(1).z).toBeCloseTo(0.4, 12);
  });

  it("keeps the millimetre z reference across a capacity growth", () => {
    const poses = new PoseBuffer(1);
    poses.applyKeyframe(keyframeAtZ(15));
    expect(poses.applyDelta(step(1, 0, 0, 7)).applied).toBe(true);
    poses.ensureCapacity(64);
    expect(poses.zMm[0]).toBe(157);
    expect(poses.applyDelta(step(2, 0, 0, 3)).applied).toBe(true);
    expect(poses.zMm[0]).toBe(160);
    expect(poses.zCm[0]).toBe(16);
  });

  it("agrees with the §3.2 escape threshold, which is 32,000 mm on all three axes", () => {
    expect(needsAbsoluteEscape(0, 0, DELTA_ESCAPE_MM)).toBe(false);
    expect(needsAbsoluteEscape(0, 0, DELTA_ESCAPE_MM + 1)).toBe(true);
    // 32,000 mm of z fits an i16 delta and, applied, is 32 m of height — not 320 m.
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(keyframeAtZ(0));
    expect(poses.applyDelta(step(1, 0, 0, DELTA_ESCAPE_MM)).applied).toBe(true);
    expect(poses.positionOf(0).z).toBeCloseTo(32, 12);
  });
});

describe("§3.2 escape hatch / §10.3 Q3 teleport_escape", () => {
  it("flags a displacement over 32,000 mm", () => {
    expect(needsAbsoluteEscape(0, 0, 0)).toBe(false);
    expect(needsAbsoluteEscape(DELTA_ESCAPE_MM, -DELTA_ESCAPE_MM, 0)).toBe(false);
    expect(needsAbsoluteEscape(DELTA_ESCAPE_MM + 1, 0, 0)).toBe(true);
    expect(needsAbsoluteEscape(0, -(DELTA_ESCAPE_MM + 1), 0)).toBe(true);
    expect(needsAbsoluteEscape(0, 0, DELTA_ESCAPE_MM + 1)).toBe(true);
  });

  it("MFLAG_ABSOLUTE makes the client take the absolute block and ignore dx/dy/dz", () => {
    const poses = new PoseBuffer(4);
    poses.applyKeyframe(
      buildKeyframe({
        simTimeNs: 0n, originXM: -500, originYM: -500, originZM: 0, gopIndex: 3,
        actors: [
          { actorId: 0, xMm: 512345, yMm: 496790, zCm: 15, laneId: 42, headingBrad: 0, speedCq: 1778, accelCq: 32, classIdx: 0, state: 0x08, verifiedNeighbors: 7 },
          { actorId: 1, xMm: 480000, yMm: 500500, zCm: 15, laneId: 43, headingBrad: 32768, speedCq: 1408, accelCq: -77, classIdx: 1, state: 0x08, verifiedNeighbors: 5 },
        ],
        signals: [],
      }),
    );

    // Slot 0 teleports 900 m east; slot 1 makes an ordinary 1.4 m step in the same delta.
    const teleportX = quantisePositionMm(912.345 - -500);
    const teleportY = quantisePositionMm(-3.21 - -500);
    const res = poses.applyDelta(
      buildDelta({
        simTimeNs: 100_000_000n, gopIndex: 3, stepIndex: 1,
        moved: [
          { slot: 0, dxMm: 0, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 1778, accelCq: 32, state: 0x08, verifiedNeighbors: 7, mflags: MovedFlags.ABSOLUTE },
          { slot: 1, dxMm: -1400, dyMm: 0, dzMm: 0, headingBrad: 32768, speedCq: 1408, accelCq: -77, state: 0x08, verifiedNeighbors: 5, mflags: 0 },
        ],
        absolute: [{ xMm: teleportX, yMm: teleportY, zCm: 15 }],
      }),
    );

    expect(res.applied).toBe(true);
    expect(poses.xMm[0]).toBe(teleportX);
    expect(poses.positionOf(0).x).toBeCloseTo(912.345, 9);
    expect(poses.positionOf(0).y).toBeCloseTo(-3.21, 9);
    expect(poses.positionOf(0).z).toBeCloseTo(0.15, 9);
    // the ordinary row in the same delta is unaffected by the escape
    expect(poses.xMm[1]).toBe(480000 - 1400);
    expect(poses.positionOf(1).x).toBeCloseTo(-21.4, 9);
  });

  it("several absolute rows are consumed in moved-row order", () => {
    const poses = new PoseBuffer(4);
    poses.applyKeyframe(
      buildKeyframe({
        simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0,
        actors: [
          { actorId: 0, xMm: 0, yMm: 0, zCm: 0, laneId: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 },
          { actorId: 1, xMm: 0, yMm: 0, zCm: 0, laneId: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 },
          { actorId: 2, xMm: 0, yMm: 0, zCm: 0, laneId: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 },
        ],
        signals: [],
      }),
    );
    poses.applyDelta(
      buildDelta({
        simTimeNs: 100_000_000n, gopIndex: 0, stepIndex: 1,
        moved: [
          { slot: 0, dxMm: 0, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: MovedFlags.ABSOLUTE },
          { slot: 1, dxMm: 5, dyMm: 5, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: 0 },
          { slot: 2, dxMm: 0, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: MovedFlags.ABSOLUTE },
        ],
        absolute: [
          { xMm: 111_000, yMm: 222_000, zCm: 1 },
          { xMm: 333_000, yMm: 444_000, zCm: 2 },
        ],
      }),
    );
    expect([poses.xMm[0], poses.yMm[0], poses.zCm[0]]).toEqual([111_000, 222_000, 1]);
    expect([poses.xMm[1], poses.yMm[1]]).toEqual([5, 5]);
    expect([poses.xMm[2], poses.yMm[2], poses.zCm[2]]).toEqual([333_000, 444_000, 2]);
  });
});

describe("§3.4 — a delta is only meaningful against its own GOP", () => {
  const kf = buildKeyframe({
    simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 5,
    actors: [{ actorId: 0, xMm: 1000, yMm: 2000, zCm: 3, laneId: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0 }],
    signals: [],
  });
  const moved = [{ slot: 0, dxMm: 10, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags: 0 }];

  it("refuses a delta before any keyframe", () => {
    const poses = new PoseBuffer(2);
    expect(poses.applyDelta(buildDelta({ simTimeNs: 1n, gopIndex: 5, stepIndex: 1, moved }))).toEqual({
      applied: false, reason: "no-keyframe",
    });
  });

  it("refuses a delta from another GOP", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(kf);
    expect(poses.applyDelta(buildDelta({ simTimeNs: 1n, gopIndex: 6, stepIndex: 1, moved }))).toEqual({
      applied: false, reason: "gop-mismatch",
    });
    expect(poses.xMm[0]).toBe(1000); // untouched
  });

  it("refuses a delta whose predecessor was dropped (§10.2 H10)", () => {
    const poses = new PoseBuffer(2);
    poses.applyKeyframe(kf);
    expect(poses.applyDelta(buildDelta({ simTimeNs: 1n, gopIndex: 5, stepIndex: 1, moved })).applied).toBe(true);
    expect(poses.applyDelta(buildDelta({ simTimeNs: 3n, gopIndex: 5, stepIndex: 3, moved }))).toEqual({
      applied: false, reason: "step-gap",
    });
    expect(poses.xMm[0]).toBe(1010); // only step 1 applied
  });
});
