/**
 * Golden interop tests against the worked example of docs/protocol/vwp-v1.md §9.
 *
 * The three frames in `vectors/spec-vectors.ts` are the specification's own annotated hex dumps,
 * extracted byte for byte. If a decoder offset drifts from a §3 table, one of these fails.
 */

import { describe, expect, it } from "vitest";

import {
  ActorState,
  ByeReason,
  HelloFlags,
  MsgType,
  MovedFlags,
  PoseBuffer,
  SlotTable,
  VWP_MAGIC,
  bytesToHex,
  decodeDelta,
  decodeHello,
  decodeKeyframe,
  decodeMessage,
  dequantiseAccelMps2,
  dequantiseHeadingRad,
  dequantiseSpeedMps,
  dequantiseTimeToChangeS,
  formatUuid,
  parseFrameHeader,
  viewFrame,
} from "../src/index.js";
import { SPEC_DELTA_HEX, SPEC_HELLO_HEX, SPEC_KEYFRAME_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

const helloFrame = (): ArrayBuffer => hexToArrayBuffer(SPEC_HELLO_HEX);
const keyframeFrame = (): ArrayBuffer => hexToArrayBuffer(SPEC_KEYFRAME_HEX);
const deltaFrame = (): ArrayBuffer => hexToArrayBuffer(SPEC_DELTA_HEX);

describe("§9.1 Hello — 796 bytes (24 header + 772 body)", () => {
  const frame = helloFrame();

  it("is exactly 796 bytes and its header matches the §2.1 table", () => {
    expect(frame.byteLength).toBe(796);
    const h = parseFrameHeader(frame);
    expect(h.magic).toBe(VWP_MAGIC);
    expect(h.magic).toBe(0x31505756);
    expect(h.version).toBe(1);
    expect(h.msgType).toBe(MsgType.Hello);
    expect(h.bodyLen).toBe(772);
    expect(24 + h.bodyLen).toBe(frame.byteLength);
    expect(h.flags).toBe(0x0000);
    expect(h.reserved).toBe(0);
    expect(h.seq).toBe(0n);
  });

  it("the wire bytes of magic spell V W P 1", () => {
    expect(Array.from(new Uint8Array(frame, 0, 4))).toEqual([0x56, 0x57, 0x50, 0x31]);
    expect(new TextDecoder().decode(new Uint8Array(frame, 0, 4))).toBe("VWP1");
  });

  it("decodes every field of the §3.1.1 prefix to the values §9 states", () => {
    const hello = decodeHello(viewFrame(frame));
    expect(hello.versionMajor).toBe(1);
    expect(hello.versionMinor).toBe(0);
    expect(hello.helloFlags).toBe(HelloFlags.LIVE);
    expect(formatUuid(hello.runId)).toBe("0189d4c7-9f3a-7b21-8e44-5c6d7e8f9a0b");
    expect(bytesToHex(hello.scenarioHash)).toBe("bc46db1c20e90e5621c377aabafa5632de6412a22984f57d80e3f6babdfae184");
    expect(bytesToHex(hello.worldHash)).toBe("d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988");
    expect(hello.t0WallNs).toBe(1804143600000000000n);
    expect(hello.simDurationNs).toBe(600_000_000_000n);
    expect(hello.mobilityStepNs).toBe(100_000_000n);
    expect(hello.keyframePeriodNs).toBe(1_000_000_000n);
    expect(hello.telemetryPeriodNs).toBe(1_000_000_000n);
    expect(hello.metricPeriodNs).toBe(1_000_000_000n);
    expect(hello.resumeSeq).toBe(0n);
    expect(hello.simTimeNs).toBe(0n);
    expect(hello.originLatDeg).toBeCloseTo(52.5163, 10);
    expect(hello.originLonDeg).toBeCloseTo(13.3777, 10);
    expect(hello.originAltM).toBe(34);
    expect([hello.bboxMinXM, hello.bboxMinYM, hello.bboxMaxXM, hello.bboxMaxYM]).toEqual([-500, -500, 500, 500]);
    expect(hello.actorCapacity).toBe(4096);
  });

  it("decodes the node, class and channel tables (§3.1.3–§3.1.5)", () => {
    const hello = decodeHello(viewFrame(frame));

    expect(hello.nodes.count).toBe(3);
    expect(Array.from(hello.nodes.nodeId)).toEqual([0, 1, 2]);
    expect(Array.from(hello.nodes.actorId)).toEqual([0, 1, 0xffffffff]);
    expect(Array.from(hello.nodes.posXM)).toEqual([0, 0, 12]);
    expect(Array.from(hello.nodes.posYM)).toEqual([0, 0, -8]);
    expect(Array.from(hello.nodes.posZM)).toEqual([0, 0, 6]);
    expect(Array.from(hello.nodes.flags)).toEqual([1, 1, 1]);
    expect(Array.from(hello.nodes.kind)).toEqual([0, 0, 2]);
    expect(Array.from(hello.nodes.classIdx)).toEqual([0, 1, 0xff]);
    expect(hello.nodes.strLabel.length).toBe(3);
    expect([...hello.nodes.strLabel].map((id) => hello.strings[id])).toEqual(["veh_0000", "veh_0001", "rsu_north"]);
    expect([...hello.nodes.strProfileId].map((id) => hello.strings[id])).toEqual([
      "obu/cohda-mk5", "obu/cohda-mk5", "rsu/cohda-mk5-rsu",
    ]);

    expect(hello.classes.count).toBe(3);
    expect([...hello.classes.strName].map((id) => hello.strings[id])).toEqual(["car", "truck", "pedestrian"]);
    expect(Array.from(hello.classes.lengthM)).toEqual([4.5, 12, 0.5]);
    expect(hello.classes.widthM[0]).toBeCloseTo(1.8, 5);
    expect(hello.classes.widthM[1]).toBeCloseTo(2.55, 5);
    expect(hello.classes.widthM[2]).toBe(0.5);
    expect(hello.classes.heightM[0]).toBe(1.5);
    expect(hello.classes.heightM[1]).toBeCloseTo(3.6, 5);
    expect(hello.classes.heightM[2]).toBe(1.75);
    expect(Array.from(hello.classes.colorRgba).map((c) => c.toString(16))).toEqual(["3b82f6ff", "f59e0bff", "10b981ff"]);
    expect(Array.from(hello.classes.category)).toEqual([0, 0, 1]);
    expect(Array.from(hello.classes.reserved16)).toEqual([0, 0, 0]);
    expect(Array.from(hello.classes.reserved8)).toEqual([0, 0, 0]);

    expect(hello.channels.count).toBe(3);
    expect([...hello.channels.strId].map((id) => hello.strings[id])).toEqual(["node.tx", "phy.rx", "gt.kinematics"]);
    expect(Array.from(hello.channels.channelId)).toEqual([10, 11, 1]);
    expect(Array.from(hello.channels.visibility)).toEqual([1, 3, 0]);
    expect(Array.from(hello.channels.enabled)).toEqual([1, 1, 1]);
  });

  it("decodes the world reference and the symbol table (§3.1.6, §2.5)", () => {
    const hello = decodeHello(viewFrame(frame));
    expect(hello.worldRef.mode).toBe(0);
    expect(hello.worldRef.format).toBe(0);
    expect(hello.worldRef.payloadBytes).toBe(1_482_960);
    expect(hello.strings[hello.worldRef.strUrl]).toBe(
      "/world/d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988.vwb",
    );

    expect(hello.strings.length).toBe(17);
    expect(hello.strings[0]).toBe("");
    expect(hello.strings).toEqual([
      "", "v2xw 0.4.0+9f0649d", "single-intersection", "demo", "s_7f3a9c21",
      "/world/d172872bfdd10998babc1497334713546bbbb7dffa1b3b90fddb2d17d641e988.vwb",
      "veh_0000", "veh_0001", "rsu_north", "obu/cohda-mk5", "rsu/cohda-mk5-rsu",
      "car", "truck", "pedestrian", "node.tx", "phy.rx", "gt.kinematics",
    ]);
    expect(hello.engineVersion).toBe("v2xw 0.4.0+9f0649d");
    expect(hello.scenarioName).toBe("single-intersection");
    expect(hello.runLabel).toBe("demo");
    expect(hello.sessionToken).toBe("s_7f3a9c21");
  });

  it("reads back the section offsets §9 quotes", () => {
    const dv = new DataView(helloFrame(), 24);
    expect(dv.getUint32(220, true)).toBe(256); // off_nodes
    expect(dv.getUint32(224, true)).toBe(352); // off_classes
    expect(dv.getUint32(228, true)).toBe(424); // off_channels
    expect(dv.getUint32(232, true)).toBe(448); // off_world_ref
    expect(dv.getUint32(236, true)).toBe(464); // off_strings
  });
});

describe("§9.2 Keyframe — 180 bytes (24 header + 156 body), 3 actors", () => {
  const frame = keyframeFrame();

  it("is exactly 180 bytes with the header §9.2 shows", () => {
    expect(frame.byteLength).toBe(180);
    const h = parseFrameHeader(frame);
    expect(h.msgType).toBe(MsgType.Keyframe);
    expect(h.bodyLen).toBe(156);
    expect(h.seq).toBe(10n);
    expect(h.flags).toBe(0);
  });

  it("body size matches the §9.2 formula 64 + 28·3 + 8·1 = 156", () => {
    expect(64 + 28 * 3 + 8 * 1).toBe(156);
  });

  it("decodes the §3.3.1 prefix", () => {
    const kf = decodeKeyframe(viewFrame(frame));
    expect(kf.simTimeNs).toBe(1_000_000_000n);
    expect(kf.originXM).toBe(-500);
    expect(kf.originYM).toBe(-500);
    expect(kf.originZM).toBe(0);
    expect(kf.actors.count).toBe(3);
    expect(kf.signals.count).toBe(1);
    expect(kf.gopIndex).toBe(1);
    expect(kf.profile).toBe(0);
    const dv = new DataView(frame, 24);
    expect(dv.getUint32(40, true)).toBe(64); // off_actors
    expect(dv.getUint32(44, true)).toBe(148); // off_signals
  });

  it("produces exactly the columns §9.4 says both reference decoders produce", () => {
    const kf = decodeKeyframe(viewFrame(frame));
    const a = kf.actors;
    expect(Array.from(a.actorId)).toEqual([0, 1, 2]);
    expect(Array.from(a.xMm)).toEqual([512345, 480000, 503000]);
    expect(Array.from(a.yMm)).toEqual([496790, 500500, 541250]);
    expect(Array.from(a.laneId)).toEqual([42, 43, 4294967295]);
    expect(Array.from(a.zCm)).toEqual([15, 15, 10]);
    expect(Array.from(a.headingBrad)).toEqual([0, 32768, 16384]);
    expect(Array.from(a.speedCq)).toEqual([1778, 1408, 0]);
    expect(Array.from(a.accelCq)).toEqual([32, -77, 0]);
    expect(Array.from(a.classIdx)).toEqual([0, 1, 2]);
    expect(Array.from(a.state)).toEqual([0x08, 0x09, 0x00]);
    expect(Array.from(a.verifiedNeighbors)).toEqual([7, 5, 0]);
    expect(Array.from(a.flags8)).toEqual([0, 0, 0]);
  });

  it("decodes signal 7 as phase 3 with 12.8 s to change (§3.3.3)", () => {
    const kf = decodeKeyframe(viewFrame(frame));
    expect(Array.from(kf.signals.signalId)).toEqual([7]);
    expect(Array.from(kf.signals.timeToChangeDs)).toEqual([128]);
    expect(dequantiseTimeToChangeS(kf.signals.timeToChangeDs[0])).toBeCloseTo(12.8, 10);
    expect(Array.from(kf.signals.phase)).toEqual([3]);
    expect(Array.from(kf.signals.reserved)).toEqual([0]);
  });

  it("reconstructs the metre poses of the §9 actor table", () => {
    const kf = decodeKeyframe(viewFrame(frame));
    const poses = new PoseBuffer(8);
    poses.applyKeyframe(kf);

    expect(poses.positionOf(0).x).toBeCloseTo(12.345, 9);
    expect(poses.positionOf(0).y).toBeCloseTo(-3.21, 9);
    expect(poses.positionOf(0).z).toBeCloseTo(0.15, 9);
    expect(poses.headingOf(0)).toBe(0);
    expect(poses.speedOf(0)).toBeCloseTo(13.890625, 9); // 1778 / 128, the §3.2 worked example
    expect(poses.accelOf(0)).toBe(0.5);

    expect(poses.positionOf(1).x).toBeCloseTo(-20, 9);
    expect(poses.positionOf(1).y).toBeCloseTo(0.5, 9);
    expect(poses.headingOf(1)).toBeCloseTo(Math.PI, 9);
    expect(poses.speedOf(1)).toBe(11);
    expect(poses.accelOf(1)).toBeCloseTo(-1.203125, 9); // -77 / 64

    expect(poses.positionOf(2).x).toBeCloseTo(3, 9);
    expect(poses.positionOf(2).y).toBeCloseTo(41.25, 9);
    expect(poses.positionOf(2).z).toBeCloseTo(0.1, 9);
    expect(poses.headingOf(2)).toBeCloseTo(Math.PI / 2, 9);
    expect(poses.speedOf(2)).toBe(0);

    // The Float32Array the renderer consumes carries the same values.
    expect(poses.positions[0]).toBeCloseTo(12.345, 4);
    expect(poses.positions[1]).toBeCloseTo(-3.21, 4);
    expect(poses.positions[2]).toBeCloseTo(0.15, 4);
    expect(poses.headings[1]).toBeCloseTo(Math.PI, 5);

    // §3.3.4 — benign is the absence of bits 0–2.
    expect(poses.isBenign(0)).toBe(true);
    expect(poses.isBenign(1)).toBe(false); // ST_ATTACKER
    expect(kf.actors.state[1] & ActorState.ATTACKER).toBe(ActorState.ATTACKER);
    expect(kf.actors.state[0] & ActorState.EQUIPPED).toBe(ActorState.EQUIPPED);
  });

  it("agrees with the §9 quantisation worked example for slot 0", () => {
    const kf = decodeKeyframe(viewFrame(frame));
    // SPEC ERRATUM: §3.2's worked example annotates `x_mm = 512345 -> 0x0007D179`, but
    // 512345 is 0x0007D159. The §9.2 hex dump carries the correct bytes `59 d1 07 00`, and the
    // decimal 512345 is consistent everywhere else, so the hex annotation is the typo.
    expect(kf.actors.xMm[0]).toBe(512345);
    expect(kf.actors.xMm[0]).toBe(0x0007d159);
    expect(Array.from(new Uint8Array(frame, 24 + 64 + 4 * 3, 4))).toEqual([0x59, 0xd1, 0x07, 0x00]);
    expect(kf.actors.yMm[0]).toBe(0x00079496);
    expect(kf.actors.zCm[0]).toBe(0x000f);
    expect(dequantiseHeadingRad(kf.actors.headingBrad[1])).toBeCloseTo(3.141593, 5);
    expect(dequantiseSpeedMps(kf.actors.speedCq[0])).toBeCloseTo(13.890625, 9);
    expect(dequantiseAccelMps2(kf.actors.accelCq[1])).toBeCloseTo(-1.203125, 9);
  });
});

describe("§9.3 Delta — 120 bytes (24 header + 96 body), 1 moved actor", () => {
  const frame = deltaFrame();

  it("is exactly 120 bytes with the header §9.3 shows", () => {
    expect(frame.byteLength).toBe(120);
    const h = parseFrameHeader(frame);
    expect(h.msgType).toBe(MsgType.Delta);
    expect(h.bodyLen).toBe(96);
    expect(h.seq).toBe(11n);
  });

  it("body size matches the §9.3 formula 64 + 20 + 4 + 8 = 96", () => {
    expect(64 + 20 * 1 + 0 + 4 * 1 + 0 + 0 + 8 * 1).toBe(96);
  });

  it("decodes the §3.4.1 prefix, including the absent sections", () => {
    const d = decodeDelta(viewFrame(frame));
    expect(d.simTimeNs).toBe(1_100_000_000n);
    expect(d.gopIndex).toBe(1);
    expect(d.stepIndex).toBe(1);
    expect(d.moved.count).toBe(1);
    expect(d.absolute.count).toBe(0);
    expect(d.lanes.length).toBe(1);
    expect(d.spawns.count).toBe(0);
    expect(d.despawns.count).toBe(0);
    expect(d.signals.count).toBe(1);
    const dv = new DataView(frame, 24);
    expect(dv.getUint32(40, true)).toBe(64); // off_moved
    expect(dv.getUint32(44, true)).toBe(0); // off_abs — absent
    expect(dv.getUint32(48, true)).toBe(84); // off_lanes
    expect(dv.getUint32(52, true)).toBe(0); // off_spawns — absent
    expect(dv.getUint32(56, true)).toBe(0); // off_despawns — absent
    expect(dv.getUint32(60, true)).toBe(88); // off_signals
  });

  it("decodes the moved row (§3.4.2)", () => {
    const d = decodeDelta(viewFrame(frame));
    expect(Array.from(d.moved.slot)).toEqual([0]);
    expect(Array.from(d.moved.dxMm)).toEqual([1389]);
    expect(Array.from(d.moved.dyMm)).toEqual([0]);
    expect(Array.from(d.moved.dzMm)).toEqual([0]);
    expect(Array.from(d.moved.headingBrad)).toEqual([0]);
    expect(Array.from(d.moved.speedCq)).toEqual([1784]);
    expect(Array.from(d.moved.accelCq)).toEqual([32]);
    expect(Array.from(d.moved.state)).toEqual([0x08]);
    expect(Array.from(d.moved.verifiedNeighbors)).toEqual([8]);
    expect(Array.from(d.moved.mflags)).toEqual([MovedFlags.LANE_CHANGED]);
    expect(Array.from(d.moved.reserved)).toEqual([0]);
    expect(Array.from(d.lanes)).toEqual([44]);
    expect(Array.from(d.signals.signalId)).toEqual([7]);
    expect(Array.from(d.signals.timeToChangeDs)).toEqual([118]);
    expect(Array.from(d.signals.phase)).toEqual([3]);
  });

  it("applied to the keyframe, reproduces the §9.3 'Applying the delta' table", () => {
    const kf = decodeKeyframe(viewFrame(keyframeFrame()));
    const d = decodeDelta(viewFrame(frame));
    const poses = new PoseBuffer(8);
    poses.applyKeyframe(kf);
    const result = poses.applyDelta(d);

    expect(result).toEqual({ applied: true, moved: 1, spawned: 0, despawned: 0 });
    expect(poses.xMm[0]).toBe(512345 + 1389);
    expect(poses.xMm[0]).toBe(513734);
    expect(poses.positionOf(0).x).toBeCloseTo(13.734, 9);
    expect(poses.yMm[0]).toBe(496790);
    expect(poses.positionOf(0).y).toBeCloseTo(-3.21, 9);
    expect(poses.positionOf(0).z).toBeCloseTo(0.15, 9);
    expect(poses.headingOf(0)).toBe(0);
    expect(poses.speedOf(0)).toBe(13.9375);
    expect(poses.accelOf(0)).toBe(0.5);
    expect(poses.state[0]).toBe(0x08);
    expect(poses.verifiedNeighbors[0]).toBe(8);
    expect(poses.laneId[0]).toBe(44);

    // slots 1 and 2 unchanged
    expect(poses.positionOf(1).x).toBeCloseTo(-20, 9);
    expect(poses.positionOf(1).y).toBeCloseTo(0.5, 9);
    expect(poses.positionOf(2).x).toBeCloseTo(3, 9);
    expect(poses.positionOf(2).y).toBeCloseTo(41.25, 9);

    // signal 7: phase 3, 11.8 s to change
    expect(dequantiseTimeToChangeS(d.signals.timeToChangeDs[0])).toBeCloseTo(11.8, 10);
    expect(d.signals.phase[0]).toBe(3);

    expect(poses.simTimeNs).toBe(1_100_000_000n);
    expect(poses.stepIndex).toBe(1);
    expect(poses.gopIndex).toBe(1);
  });

  it("the slot table adopts the keyframe's dense actor_id column (§3.3.1)", () => {
    const kf = decodeKeyframe(viewFrame(keyframeFrame()));
    const slots = new SlotTable(8);
    slots.adoptKeyframe(kf.actors.actorId, kf.gopIndex);
    expect(slots.count).toBe(3);
    expect(slots.actorIdOf(0)).toBe(0);
    expect(slots.slotOf(2)).toBe(2);
    expect(slots.occupiedSlots()).toEqual([0, 1, 2]);
  });
});

describe("the dispatcher decodes each vector by msg_type", () => {
  it("routes Hello, Keyframe and Delta", () => {
    expect(decodeMessage(helloFrame()).kind).toBe("hello");
    expect(decodeMessage(keyframeFrame()).kind).toBe("keyframe");
    expect(decodeMessage(deltaFrame()).kind).toBe("delta");
  });

  it("exposes ByeReason exactly as Appendix A numbers it", () => {
    expect(ByeReason.RUN_COMPLETE).toBe(0);
    expect(ByeReason.CLIENT_REQUESTED).toBe(1);
    expect(ByeReason.SERVER_SHUTDOWN).toBe(2);
    expect(ByeReason.ERROR).toBe(3);
    expect(ByeReason.SUPERSEDED).toBe(4);
  });
});
