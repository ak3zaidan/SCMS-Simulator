/**
 * The encoder is checked against the specification, not against the decoder: build the §9 example
 * from the field values §9 states in prose, and assert the bytes equal the §9 hex dumps exactly.
 *
 * This is what makes the offset constants trustworthy. A transposed offset that the decoder and the
 * encoder shared would still pass a round-trip test; it cannot pass this one.
 */

import { describe, expect, it } from "vitest";

import {
  ActorState,
  MovedFlags,
  deltaFrame,
  helloFrame,
  keyframeFrame,
  quantiseAccelCq,
  quantiseHeadingBrad,
  quantiseHeightCm,
  quantisePositionMm,
  quantiseSpeedCq,
  quantiseTimeToChangeDs,
} from "../src/index.js";
import { SPEC_DELTA_HEX, SPEC_HELLO_HEX, SPEC_KEYFRAME_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

const hex = (buf: ArrayBuffer): string =>
  Array.from(new Uint8Array(buf))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

const fromHex = (s: string): Uint8Array => new Uint8Array(hexToArrayBuffer(s));

// §9 — the strings of the example's symbol table, in id order.
const WORLD_URL = "/world/d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988.vwb";
const STRINGS = [
  "", "v2xw 0.4.0+9f0649d", "single-intersection", "demo", "s_7f3a9c21", WORLD_URL,
  "veh_0000", "veh_0001", "rsu_north", "obu/cohda-mk5", "rsu/cohda-mk5-rsu",
  "car", "truck", "pedestrian", "node.tx", "phy.rx", "gt.kinematics",
] as const;

describe("§9.1 — encoding Hello reproduces the specification's 796 bytes", () => {
  it("byte-for-byte", () => {
    const frame = helloFrame(
      {
        helloFlags: 0x0000_0001, // HELLO_LIVE
        runId: fromHex("0189d4c79f3a7b218e445c6d7e8f9a0b"),
        scenarioHash: fromHex("bc46db1c20e90e5621c377aabafa5632de6412a22984f57d80e3f6babdfae184"),
        worldHash: fromHex("d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988"),
        t0WallNs: 1804143600000000000n,
        simDurationNs: 600_000_000_000n,
        mobilityStepNs: 100_000_000n,
        keyframePeriodNs: 1_000_000_000n,
        telemetryPeriodNs: 1_000_000_000n,
        metricPeriodNs: 1_000_000_000n,
        resumeSeq: 0n,
        simTimeNs: 0n,
        originLatDeg: 52.5163,
        originLonDeg: 13.3777,
        originAltM: 34,
        bboxMinXM: -500,
        bboxMinYM: -500,
        bboxMaxXM: 500,
        bboxMaxYM: 500,
        actorCapacity: 4096,
        nodes: [
          { nodeId: 0, actorId: 0, posXM: 0, posYM: 0, posZM: 0, strLabel: 6, strProfileId: 9, flags: 1, kind: 0, classIdx: 0 },
          { nodeId: 1, actorId: 1, posXM: 0, posYM: 0, posZM: 0, strLabel: 7, strProfileId: 9, flags: 1, kind: 0, classIdx: 1 },
          { nodeId: 2, actorId: 0xffffffff, posXM: 12, posYM: -8, posZM: 6, strLabel: 8, strProfileId: 10, flags: 1, kind: 2, classIdx: 0xff },
        ],
        classes: [
          { strName: 11, lengthM: 4.5, widthM: 1.8, heightM: 1.5, colorRgba: 0x3b82f6ff, category: 0 },
          { strName: 12, lengthM: 12, widthM: 2.55, heightM: 3.6, colorRgba: 0xf59e0bff, category: 0 },
          { strName: 13, lengthM: 0.5, widthM: 0.5, heightM: 1.75, colorRgba: 0x10b981ff, category: 1 },
        ],
        channels: [
          { strId: 14, channelId: 10, visibility: 1, enabled: 1 },
          { strId: 15, channelId: 11, visibility: 3, enabled: 1 },
          { strId: 16, channelId: 1, visibility: 0, enabled: 1 },
        ],
        worldRef: { mode: 0, format: 0, payloadBytes: 1_482_960, strUrl: 5 },
        strings: STRINGS,
        strEngineVersion: 1,
        strScenarioName: 2,
        strRunLabel: 3,
        strSessionToken: 4,
      },
      0n,
    );
    expect(frame.byteLength).toBe(796);
    expect(hex(frame)).toBe(SPEC_HELLO_HEX.replace(/\s+/g, ""));
  });
});

describe("§9.2 — encoding Keyframe reproduces the specification's 180 bytes", () => {
  it("byte-for-byte, quantising the metre values of the §9 actor table", () => {
    const originX = -500;
    const originY = -500;
    const originZ = 0;
    const actor = (
      actorId: number, x: number, y: number, z: number, laneId: number,
      headingRad: number, speedMps: number, accelMps2: number, classIdx: number,
      state: number, verifiedNeighbors: number,
    ) => ({
      actorId,
      xMm: quantisePositionMm(x - originX),
      yMm: quantisePositionMm(y - originY),
      laneId,
      zCm: quantiseHeightCm(z - originZ),
      headingBrad: quantiseHeadingBrad(headingRad),
      speedCq: quantiseSpeedCq(speedMps),
      accelCq: quantiseAccelCq(accelMps2),
      classIdx,
      state,
      verifiedNeighbors,
    });

    const frame = keyframeFrame(
      {
        simTimeNs: 1_000_000_000n,
        originXM: originX,
        originYM: originY,
        originZM: originZ,
        gopIndex: 1,
        profile: 0,
        actors: [
          actor(0, 12.345, -3.21, 0.15, 42, 0, 13.89, 0.5, 0, ActorState.EQUIPPED, 7),
          actor(1, -20, 0.5, 0.15, 43, Math.PI, 11, -1.2, 1, ActorState.EQUIPPED | ActorState.ATTACKER, 5),
          actor(2, 3, 41.25, 0.1, 0xffffffff, Math.PI / 2, 0, 0, 2, 0, 0),
        ],
        signals: [{ signalId: 7, timeToChangeDs: quantiseTimeToChangeDs(12.8), phase: 3 }],
      },
      10n,
    );
    expect(frame.byteLength).toBe(180);
    expect(hex(frame)).toBe(SPEC_KEYFRAME_HEX.replace(/\s+/g, ""));
  });
});

describe("§9.3 — encoding Delta reproduces the specification's 120 bytes", () => {
  it("byte-for-byte", () => {
    const frame = deltaFrame(
      {
        simTimeNs: 1_100_000_000n,
        gopIndex: 1,
        stepIndex: 1,
        moved: [
          {
            slot: 0,
            dxMm: 1389,
            dyMm: 0,
            dzMm: 0,
            headingBrad: 0,
            speedCq: 1784,
            accelCq: 32,
            state: ActorState.EQUIPPED,
            verifiedNeighbors: 8,
            mflags: MovedFlags.LANE_CHANGED,
          },
        ],
        lanes: [44],
        signals: [{ signalId: 7, timeToChangeDs: quantiseTimeToChangeDs(11.8), phase: 3 }],
      },
      11n,
    );
    expect(frame.byteLength).toBe(120);
    expect(hex(frame)).toBe(SPEC_DELTA_HEX.replace(/\s+/g, ""));
  });
});
