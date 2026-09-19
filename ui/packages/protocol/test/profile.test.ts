/**
 * §5 visibility and the `NODE-only` profile — the checkers of register finding S14, which §10.6 V1
 * ("decodes the whole stream and asserts every GT field is at its sentinel and every GT channel is
 * absent") and §5.3 (HELLO_NODE_ONLY / Keyframe.profile / FLAG_NODE_ONLY agree) need.
 */

import { describe, expect, it } from "vitest";

import {
  ActorState,
  ChannelId,
  FrameFlags,
  GT_CHANNEL_IDS,
  HelloFlags,
  MovedFlags,
  MsgType,
  NodeFlags,
  SENTINEL_U16,
  SENTINEL_U32,
  SENTINEL_U8,
  checkNodeProfileLeakage,
  checkProfileConsistency,
  decodeEvent,
  decodeHello,
  decodeMetricSample,
  decodeTelemetry,
  encodeEventBody,
  encodeMetricSampleBody,
  encodeTelemetryBody,
  frameOf,
  helloFrame,
  viewFrame,
  type NodeTelemetryInit,
} from "../src/index.js";
import { buildDelta, buildKeyframe } from "./helpers/build-frames.js";

const HELLO_BASE = {
  helloFlags: HelloFlags.LIVE | HelloFlags.NODE_ONLY,
  runId: new Uint8Array(16),
  scenarioHash: new Uint8Array(32),
  worldHash: new Uint8Array(32),
  t0WallNs: 0n,
  simDurationNs: 0n,
  mobilityStepNs: 100_000_000n,
  keyframePeriodNs: 1_000_000_000n,
  telemetryPeriodNs: 1_000_000_000n,
  metricPeriodNs: 1_000_000_000n,
  resumeSeq: 0n,
  simTimeNs: 0n,
  originLatDeg: 0,
  originLonDeg: 0,
  originAltM: 0,
  bboxMinXM: 0,
  bboxMinYM: 0,
  bboxMaxXM: 1,
  bboxMaxYM: 1,
  actorCapacity: 16,
  worldRef: { mode: 2, format: 0, payloadBytes: 0, strUrl: 0 },
  strings: ["", "engine", "scn", "node.tx", "gt.kinematics"],
  strEngineVersion: 1,
  strScenarioName: 2,
  strRunLabel: 0,
  strSessionToken: 0,
};

const node = (flags: number) => ({
  nodeId: 7, actorId: 3, posXM: 0, posYM: 0, posZM: 0, strLabel: 1, strProfileId: 2, flags, kind: 0, classIdx: 0,
});

const hello = (over: Partial<Parameters<typeof helloFrame>[0]> = {}) =>
  decodeHello(viewFrame(helloFrame({ ...HELLO_BASE, nodes: [], classes: [], channels: [], ...over }, 0n)));

/** A §3.3.2 actor row that is clean under the node profile. */
const cleanActor = (actorId: number) => ({
  actorId, xMm: 1000, yMm: 2000, laneId: SENTINEL_U32, zCm: 15, headingBrad: 0, speedCq: 128,
  accelCq: 0, classIdx: 0, state: ActorState.EQUIPPED | ActorState.TRANSMITTING, verifiedNeighbors: 4, flags8: 0,
});

/** A §3.6.5 `phy.rx` payload with the GT fields blanked as §5.2 requires. */
function phyRx(txNode: number, distanceM: number, losClass: number): Uint8Array {
  const bytes = new Uint8Array(48);
  const dv = new DataView(bytes.buffer);
  dv.setBigUint64(0, 1n, true);
  dv.setBigUint64(8, 2n, true);
  dv.setUint32(16, 5, true);
  dv.setUint32(20, txNode, true);
  dv.setUint32(24, 9, true);
  dv.setFloat32(28, -80, true);
  dv.setFloat32(32, 12, true);
  dv.setFloat32(36, distanceM, true);
  dv.setUint8(40, 0);
  dv.setUint8(41, 0);
  dv.setUint8(42, losClass);
  return bytes;
}

const telemetry = (over: Partial<NodeTelemetryInit> = {}): NodeTelemetryInit => ({
  storageUsedB: 0n, storageTotalB: 0n, nextTopupNs: 0n, crlBytes: 0n, outboxBytes: 0n, clockOffsetNs: 0n,
  nodeId: 7, ramUsedKib: 0, ramTotalKib: 0, dropRxOverflow: 0, dropVerifyPolicySkip: 0, dropVerifyOverflow: 0,
  dropTxOverflow: 0, dropReassemblyTimeout: 0, dropCrlBacklog: 0, certStored: 0, crlEntries: 0, outboxMsgs: 0,
  peerCacheEntries: 0, p2pcdRequests: 0, fullCertMsgs: 0, msgsInPerS: 0, msgsOutPerS: 0, verificationsPerS: 0,
  verifyWaitP50Ms: 0, verifyWaitP95Ms: 0, gnssHdop: 0, gnssSigmaM: 0, clockDriftPpm: 0,
  posErrorM: Number.NaN, airtimeMsPerS: 0, cpuUtilPm: 0, hsmUtilPm: 0, qRxP50: 0, qRxP95: 0, qVerifyP50: 0,
  qVerifyP95: 0, qAppP50: 0, qAppP95: 0, qTxP50: 0, qTxP95: 0, qCrlP50: 0, qCrlP95: 0, dccState: 0, cbrPm: 0,
  txPowerCdbm: 0, nbrTotal: 0, nbrVerified: 0, nbrUnverified: 0, nbrRevoked: 0, certActive: 0,
  crlExpansionPm: 0, unverifiedRatioPm: 0, gnssFix: 2, nodeState: 2, verifyPolicy: 0, ...over,
});

const telemetryMsg = (records: readonly NodeTelemetryInit[]) =>
  decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 0n, encodeTelemetryBody(0n, 1_000_000_000n, records))));

describe("§5.2 / §10.6 V1 — checkNodeProfileLeakage names every GT field that is not blanked", () => {
  it("the GT channel set is §5.2's four channels", () => {
    expect([...GT_CHANNEL_IDS]).toEqual([ChannelId.GT_KINEMATICS, ChannelId.GT_ATTACK_ACTION, ChannelId.GT_SPAWN, ChannelId.GT_DESPAWN]);
  });

  it("Hello: IS_ATTACKER must be clear and GT channels must be absent from the table", () => {
    expect(checkNodeProfileLeakage(hello({ nodes: [node(NodeFlags.HAS_HSM)] }))).toEqual([]);
    expect(checkNodeProfileLeakage(hello({ nodes: [node(NodeFlags.IS_ATTACKER)] }))).toHaveLength(1);
    const withGt = hello({
      channels: [
        { strId: 3, channelId: ChannelId.NODE_TX, visibility: 1, enabled: 1 },
        { strId: 4, channelId: ChannelId.GT_KINEMATICS, visibility: 0, enabled: 0 },
      ],
    });
    // §5.2: "GT channels MUST be absent from the channel table (not merely `enabled = 0`)".
    expect(checkNodeProfileLeakage(withGt).join(" ")).toContain("gt.kinematics");
  });

  it("Keyframe: lane_id, accel_cq, ST_ATTACKER and unequipped occupied slots", () => {
    const clean = buildKeyframe({
      simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0, profile: 1,
      actors: [cleanActor(1), { ...cleanActor(SENTINEL_U32), state: 0 }], signals: [],
    });
    expect(checkNodeProfileLeakage(clean)).toEqual([]);

    const leaky = buildKeyframe({
      simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0, profile: 1,
      actors: [{ ...cleanActor(1), laneId: 42, accelCq: -64, state: ActorState.EQUIPPED | ActorState.ATTACKER }],
      signals: [],
    });
    const found = checkNodeProfileLeakage(leaky);
    expect(found.join("\n")).toContain("lane_id");
    expect(found.join("\n")).toContain("accel_cq");
    expect(found.join("\n")).toContain("ST_ATTACKER");
    expect(found).toHaveLength(3);

    const unequipped = buildKeyframe({
      simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0, profile: 1,
      actors: [{ ...cleanActor(1), state: 0 }], signals: [],
    });
    expect(checkNodeProfileLeakage(unequipped).join("\n")).toContain("ST_EQUIPPED");
  });

  it("Delta: the lane block, MFLAG_LANE_CHANGED, accel_cq, ST_ATTACKER and the spawn/despawn causes", () => {
    const moved = (mflags: number, accelCq: number, state: number) => ({
      slot: 0, dxMm: 1, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq, state, verifiedNeighbors: 0, mflags,
    });
    const clean = buildDelta({
      simTimeNs: 1n, gopIndex: 0, stepIndex: 1,
      moved: [moved(0, 0, ActorState.EQUIPPED)],
      spawns: [{ slot: 1, actorId: 2, nodeId: 3, xMm: 0, yMm: 0, laneId: SENTINEL_U32, zCm: 0, headingBrad: 0, speedCq: 0, cause: SENTINEL_U16, classIdx: 0, state: ActorState.EQUIPPED, verifiedNeighbors: 0 }],
      despawns: [{ slot: 2, cause: SENTINEL_U16 }],
    });
    expect(checkNodeProfileLeakage(clean)).toEqual([]);

    const leaky = buildDelta({
      simTimeNs: 1n, gopIndex: 0, stepIndex: 1,
      moved: [moved(MovedFlags.LANE_CHANGED, 32, ActorState.EQUIPPED | ActorState.ATTACKER)],
      lanes: [7],
      spawns: [{ slot: 1, actorId: 2, nodeId: 3, xMm: 0, yMm: 0, laneId: 9, zCm: 0, headingBrad: 0, speedCq: 0, cause: 0, classIdx: 0, state: ActorState.EQUIPPED, verifiedNeighbors: 0 }],
      despawns: [{ slot: 2, cause: 1 }],
    });
    const found = checkNodeProfileLeakage(leaky).join("\n");
    expect(found).toContain("accel_cq");
    expect(found).toContain("ST_ATTACKER");
    expect(found).toContain("MFLAG_LANE_CHANGED");
    expect(found).toContain("lane block");
    expect(found).toContain("spawns[0].lane_id");
    expect(found).toContain("spawns[0].cause");
    expect(found).toContain("despawns[0].cause");
  });

  it("Telemetry: clock_offset_ns, pos_error_m and the compromised node_state", () => {
    expect(checkNodeProfileLeakage(telemetryMsg([telemetry()]))).toEqual([]);
    const found = checkNodeProfileLeakage(telemetryMsg([telemetry({ clockOffsetNs: -1_250_000n, posErrorM: 0.62, nodeState: 6 })]));
    expect(found.join("\n")).toContain("clock_offset_ns");
    expect(found.join("\n")).toContain("pos_error_m");
    expect(found.join("\n")).toContain("node_state");
    expect(found).toHaveLength(3);
  });

  it("Event: a GT channel is named, and phy.rx's three GT fields are checked", () => {
    const eventFrame = (events: readonly { channelId: number; payload: Uint8Array }[]) =>
      decodeEvent(
        viewFrame(frameOf(MsgType.Event, 0n, encodeEventBody(0n, 10n, events.map((e, i) => ({ simTimeNs: BigInt(i), ...e }))))),
      );

    expect(checkNodeProfileLeakage(eventFrame([{ channelId: ChannelId.PHY_RX, payload: phyRx(SENTINEL_U32, Number.NaN, SENTINEL_U8) }]))).toEqual([]);

    const leaked = checkNodeProfileLeakage(eventFrame([{ channelId: ChannelId.PHY_RX, payload: phyRx(11, 42.5, 2) }]));
    expect(leaked.join("\n")).toContain("tx_node");
    expect(leaked.join("\n")).toContain("distance_m");
    expect(leaked.join("\n")).toContain("los_class");
    expect(leaked).toHaveLength(3);

    const gt = checkNodeProfileLeakage(eventFrame([{ channelId: ChannelId.GT_KINEMATICS, payload: new Uint8Array(56) }]));
    expect(gt).toHaveLength(1);
    expect(gt[0]).toContain("gt.kinematics");
  });

  it("Event: proto.revocation stages 0-4 are withheld, 5 and later are not (§5.3)", () => {
    const revocation = (stage: number): Uint8Array => {
      const bytes = new Uint8Array(32);
      new DataView(bytes.buffer).setUint8(28, stage);
      return bytes;
    };
    const frameFor = (stage: number) =>
      decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, encodeEventBody(0n, 10n, [{ simTimeNs: 0n, channelId: ChannelId.PROTO_REVOCATION, payload: revocation(stage) }]))));
    expect(checkNodeProfileLeakage(frameFor(4))).toHaveLength(1);
    expect(checkNodeProfileLeakage(frameFor(5))).toEqual([]);
  });

  it("MetricSample: a GT sample is a leak", () => {
    const metrics = (visibility: number) =>
      decodeMetricSample(
        viewFrame(
          frameOf(
            MsgType.MetricSample,
            0n,
            encodeMetricSampleBody(0n, 1_000_000_000n, [
              { value: 0.97, strMetric: 1, dimKey: 0, nodeId: SENTINEL_U32, count: 10, agg: 1, visibility, provId: 1 },
            ]),
          ),
        ),
      );
    expect(checkNodeProfileLeakage(metrics(1))).toEqual([]);
    expect(checkNodeProfileLeakage(metrics(0))).toHaveLength(1);
  });
});

describe("§5.3 — checkProfileConsistency cross-checks the three profile signals", () => {
  const nodeHello = hello();
  const fullHello = hello({ helloFlags: HelloFlags.LIVE });

  const keyframeWith = (profile: number, flags: number) =>
    buildKeyframe(
      { simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0, profile, actors: [cleanActor(1)], signals: [] },
      0n,
      flags,
    );

  it("accepts a consistent node-profile keyframe", () => {
    expect(checkProfileConsistency(nodeHello, keyframeWith(1, FrameFlags.NODE_ONLY))).toEqual([]);
  });

  it("accepts a consistent full-profile keyframe", () => {
    expect(checkProfileConsistency(fullHello, keyframeWith(0, 0))).toEqual([]);
  });

  it("catches a missing FLAG_NODE_ONLY on a canonical frame", () => {
    const found = checkProfileConsistency(nodeHello, keyframeWith(1, 0));
    expect(found).toHaveLength(1);
    expect(found[0]).toContain("FLAG_NODE_ONLY");
  });

  it("catches a Keyframe.profile that disagrees with the Hello", () => {
    const found = checkProfileConsistency(nodeHello, keyframeWith(0, FrameFlags.NODE_ONLY));
    expect(found).toHaveLength(1);
    expect(found[0]).toContain("Keyframe.profile");
  });

  it("catches a node-only flag on a full-profile stream", () => {
    const found = checkProfileConsistency(fullHello, keyframeWith(0, FrameFlags.NODE_ONLY));
    expect(found.join("\n")).toContain("FLAG_NODE_ONLY");
  });

  it("checks deltas too, which carry no profile field of their own", () => {
    const delta = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1 }, 1n, 0);
    expect(checkProfileConsistency(nodeHello, delta)).toHaveLength(1);
    const flagged = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1 }, 1n, FrameFlags.NODE_ONLY);
    expect(checkProfileConsistency(nodeHello, flagged)).toEqual([]);
  });
});
