/**
 * Decoder and pose-buffer hardening — the wire-conformance findings S5..S13 of
 * docs/design/findings/ui-review-register.md, each tested against the clause it comes from.
 *
 * These are all "a non-conforming producer is accepted silently" defects: the writer in
 * `src/encode.ts` honours every rule below, so every frame here is built with the real encoder and
 * then patched byte by byte to break exactly one MUST.
 */

import { describe, expect, it } from "vitest";

import {
  ChannelId,
  EVENT_PAYLOAD_BYTES,
  EVENT_PREFIX_BYTES,
  FRAME_HEADER_BYTES,
  FrameFlags,
  HELLO_OFFSETS,
  MAX_ACTOR_SLOTS,
  MovedFlags,
  MsgType,
  PoseBuffer,
  ProtocolError,
  SlotTable,
  assertLittleEndianHost,
  DELTA_OFFSETS,
  EVENT_OFFSETS,
  KEYFRAME_OFFSETS,
  METRIC_OFFSETS,
  PROVENANCE_OFFSETS,
  TELEMETRY_OFFSETS,
  decodeBye,
  decodeDelta,
  decodeError,
  decodeEvent,
  decodeEventPayload,
  decodeHello,
  decodeKeyframe,
  decodeMetricSample,
  decodeProvenance,
  decodeTelemetry,
  decodeStrTable,
  encodeByeBody,
  encodeErrorBody,
  encodeEventBody,
  encodeHelloBody,
  encodeMetricSampleBody,
  encodeProvenanceBody,
  encodeTelemetryBody,
  encodeStrTable,
  eventChannelName,
  deltaFrame,
  frameOf,
  helloFrame,
  keyframeFrame,
  isLittleEndianHost,
  serialiseFrameHeader,
  StringTable,
  strTableStrings,
  viewFrame,
} from "../src/index.js";
import { buildDelta, buildKeyframe, telemetryRow } from "./helpers/build-frames.js";
import { SPEC_DELTA_HEX, SPEC_KEYFRAME_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

/** Catch a {@link ProtocolError} and hand back its code, or `null` when nothing was thrown. */
function codeOf(fn: () => unknown): string | null {
  try {
    fn();
    return null;
  } catch (err) {
    if (err instanceof ProtocolError) return err.code;
    throw new Error(`expected a ProtocolError, got ${String(err)} (${(err as Error)?.constructor?.name})`);
  }
}

const HELLO_BASE = {
  helloFlags: 0x01,
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
  strings: ["", "engine", "scn"],
  strEngineVersion: 1,
  strScenarioName: 2,
  strRunLabel: 0,
  strSessionToken: 0,
};

const NODE_ROW = {
  nodeId: 7,
  actorId: 0xffffffff,
  posXM: 1,
  posYM: 2,
  posZM: 3,
  strLabel: 1,
  strProfileId: 2,
  flags: 0,
  kind: 2,
  classIdx: 0xff,
};

// ---------------------------------------------------------------------------
// S6 — §2.2 `off_* = 0` means "section absent"; §3.1
// ---------------------------------------------------------------------------

describe("§2.2 / §3.1 — decodeHello honours the section-absent sentinel", () => {
  /** A Hello frame with one prefix `u32` patched. */
  function patchedHello(field: number, value: number, init: Parameters<typeof encodeHelloBody>[0]): ArrayBuffer {
    const body = encodeHelloBody(init);
    new DataView(body.buffer, body.byteOffset).setUint32(field, value, true);
    return frameOf(MsgType.Hello, 0n, body);
  }

  it("node_count > 0 with off_nodes = 0 is a protocol error, not a table over the prefix", () => {
    const frame = patchedHello(HELLO_OFFSETS.offNodes, 0, { ...HELLO_BASE, nodes: [NODE_ROW], classes: [], channels: [] });
    // Before this check the table was built over the 256-byte prefix and node_id[0] decoded as 1 —
    // the u32 formed by version_major = 1 and version_minor = 0.
    expect(codeOf(() => decodeHello(viewFrame(frame)))).toBe("bad_offset");
  });

  it("class_count and channel_count get the same treatment", () => {
    const classes = [{ strName: 1, lengthM: 4, widthM: 2, heightM: 1.5, colorRgba: 0, category: 0 }];
    const channels = [{ strId: 1, channelId: ChannelId.NODE_TX, visibility: 1, enabled: 1 }];
    expect(
      codeOf(() => decodeHello(viewFrame(patchedHello(HELLO_OFFSETS.offClasses, 0, { ...HELLO_BASE, nodes: [], classes, channels: [] })))),
    ).toBe("bad_offset");
    expect(
      codeOf(() => decodeHello(viewFrame(patchedHello(HELLO_OFFSETS.offChannels, 0, { ...HELLO_BASE, nodes: [], classes: [], channels })))),
    ).toBe("bad_offset");
  });

  it("off_world_ref = 0 is a protocol error, because §3.1 always includes the world reference", () => {
    const frame = patchedHello(HELLO_OFFSETS.offWorldRef, 0, { ...HELLO_BASE, nodes: [], classes: [], channels: [] });
    expect(codeOf(() => decodeHello(viewFrame(frame)))).toBe("bad_offset");
  });

  it("a zero count with a zero offset is an empty table, exactly as the encoder writes it", () => {
    const hello = decodeHello(viewFrame(helloFrame({ ...HELLO_BASE, nodes: [], classes: [], channels: [] }, 0n)));
    expect(hello.nodes.count).toBe(0);
    expect(hello.classes.count).toBe(0);
    expect(hello.channels.count).toBe(0);
    expect(hello.worldRef.mode).toBe(2);
  });
});

// ---------------------------------------------------------------------------
// S6 (continued) — §2.2 in the pose decoders: Keyframe (§3.3), Delta (§3.4)
// ---------------------------------------------------------------------------

/**
 * `decodeHello` was fixed for the §2.2 sentinel; its siblings were not. They guarded with
 * `count === 0 || off === 0`, which folds "the section is absent" and "the section is declared but
 * unreachable" into the same answer: an EMPTY block. That is the worse of the two failure modes,
 * because nothing is reported at all — take the §9.2 keyframe, zero `off_actors`, and the frame
 * still says `actor_count = 3` on the wire while the client decodes zero actors and drives the
 * pose buffer's count to zero. The whole world silently disappears.
 *
 * Every frame below is a §9 specification vector, patched byte by byte in exactly one `u32`, with
 * one encoder-built Delta for the three §3.4 blocks the §9.3 vector legitimately leaves absent.
 */
describe("§2.2 / §3.3 / §3.4 — the pose decoders honour the section-absent sentinel too", () => {
  /** A §9 specification frame with one body `u32` zeroed. */
  function zeroed(hex: string, bodyOffset: number): ArrayBuffer {
    const frame = hexToArrayBuffer(hex);
    new DataView(frame, FRAME_HEADER_BYTES).setUint32(bodyOffset, 0, true);
    return frame;
  }

  /** The `u32` a body offset holds, read straight off the wire. */
  const wire = (frame: ArrayBuffer, bodyOffset: number): number =>
    new DataView(frame, FRAME_HEADER_BYTES).getUint32(bodyOffset, true);

  /** Catch a {@link ProtocolError} and hand back its code and its Appendix A close code. */
  function failure(fn: () => unknown): { code: string; closeCode: number; field?: string } {
    try {
      fn();
      throw new Error("expected a ProtocolError, nothing was thrown");
    } catch (err) {
      if (!(err instanceof ProtocolError)) throw new Error(`expected a ProtocolError, got ${String(err)}`);
      return { code: err.code, closeCode: err.closeCode, field: err.detail.field };
    }
  }

  it("the §9.2 keyframe this suite patches really does carry three actors and one signal", () => {
    // The control: without a patch the vector decodes, so every failure below is the patch's doing.
    const kf = decodeKeyframe(viewFrame(hexToArrayBuffer(SPEC_KEYFRAME_HEX)));
    expect(kf.actors.count).toBe(3);
    expect(kf.signals.count).toBe(1);
    const poses = new PoseBuffer(8);
    poses.applyKeyframe(kf);
    expect(poses.count).toBe(3);
  });

  it("§3.3.2 — actor_count = 3 with off_actors = 0 is a protocol error, not an empty world", () => {
    const frame = zeroed(SPEC_KEYFRAME_HEX, KEYFRAME_OFFSETS.offActors);
    // The frame still declares three actors: this is the exact "silently decodes to nothing" case.
    expect(wire(frame, KEYFRAME_OFFSETS.actorCount)).toBe(3);
    expect(failure(() => decodeKeyframe(viewFrame(frame)))).toEqual({
      code: "bad_offset",
      closeCode: 1002,
      field: "Keyframe.actors",
    });
  });

  it("§3.3.3 — signal_count = 1 with off_signals = 0 is a protocol error", () => {
    const frame = zeroed(SPEC_KEYFRAME_HEX, KEYFRAME_OFFSETS.offSignals);
    expect(wire(frame, KEYFRAME_OFFSETS.signalCount)).toBe(1);
    expect(failure(() => decodeKeyframe(viewFrame(frame)))).toEqual({
      code: "bad_offset",
      closeCode: 1002,
      field: "Keyframe.signals",
    });
  });

  it("the §9.3 delta this suite patches really does carry a moved row, a lane and a signal", () => {
    const d = decodeDelta(viewFrame(hexToArrayBuffer(SPEC_DELTA_HEX)));
    expect(d.moved.count).toBe(1);
    expect(d.lanes.length).toBe(1);
    expect(d.signals.count).toBe(1);
    // And the three genuinely absent blocks stay empty, which is what the sentinel is *for*.
    expect(wire(hexToArrayBuffer(SPEC_DELTA_HEX), DELTA_OFFSETS.offAbs)).toBe(0);
    expect(d.absolute.count).toBe(0);
    expect(d.spawns.count).toBe(0);
    expect(d.despawns.count).toBe(0);
  });

  it("§3.4.2 / §3.4.4 / §3.4.7 — the three §9.3 blocks each reject a zeroed offset", () => {
    for (const [off, count, field] of [
      [DELTA_OFFSETS.offMoved, DELTA_OFFSETS.movedCount, "Delta.moved"],
      [DELTA_OFFSETS.offLanes, DELTA_OFFSETS.laneCount, "Delta.lanes"],
      [DELTA_OFFSETS.offSignals, DELTA_OFFSETS.signalCount, "Delta.signals"],
    ] as const) {
      const frame = zeroed(SPEC_DELTA_HEX, off);
      expect(wire(frame, count)).toBeGreaterThan(0);
      expect(failure(() => decodeDelta(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field });
    }
  });

  it("§3.4.3 / §3.4.5 / §3.4.6 — so do the absolute, spawn and despawn blocks", () => {
    // §9.3 has no absolute, spawn or despawn rows, so this one is built with the real encoder and
    // then patched the same way. The row values are the §9.3 moved row's, re-expressed.
    const full = deltaFrame(
      {
        simTimeNs: 1_100_000_000n,
        gopIndex: 1,
        stepIndex: 1,
        moved: [{ slot: 0, dxMm: 0, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 1778, accelCq: 32, state: 0x08, verifiedNeighbors: 7, mflags: MovedFlags.ABSOLUTE }],
        absolute: [{ xMm: 512345, yMm: 496790, zCm: 15 }],
        spawns: [{ slot: 3, actorId: 3, nodeId: 0xffffffff, xMm: 1000, yMm: 2000, laneId: 42, zCm: 15, headingBrad: 0, speedCq: 0, cause: 0, classIdx: 0, state: 0, verifiedNeighbors: 0 }],
        despawns: [{ slot: 2, cause: 1 }],
        signals: [{ signalId: 7, timeToChangeDs: 128, phase: 3 }],
      },
      11n,
    );
    const d = decodeDelta(viewFrame(full));
    expect([d.absolute.count, d.spawns.count, d.despawns.count]).toEqual([1, 1, 1]);

    for (const [off, count, field] of [
      [DELTA_OFFSETS.offAbs, DELTA_OFFSETS.absCount, "Delta.abs"],
      [DELTA_OFFSETS.offSpawns, DELTA_OFFSETS.spawnCount, "Delta.spawns"],
      [DELTA_OFFSETS.offDespawns, DELTA_OFFSETS.despawnCount, "Delta.despawns"],
    ] as const) {
      const frame = full.slice(0);
      new DataView(frame, FRAME_HEADER_BYTES).setUint32(off, 0, true);
      expect(wire(frame, count)).toBe(1);
      expect(failure(() => decodeDelta(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field });
    }
  });

  it("a zero count with a zero offset is still an empty block, exactly as the encoder writes it", () => {
    // The other half of §2.2: the sentinel must keep meaning "absent" for a genuinely empty
    // section, or every keyframe with no signals and every delta with no spawns would be refused.
    const kf = decodeKeyframe(viewFrame(keyframeFrame({ simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 1, actors: [], signals: [] }, 0n)));
    expect(kf.actors.count).toBe(0);
    expect(kf.signals.count).toBe(0);
    const d = decodeDelta(viewFrame(deltaFrame({ simTimeNs: 0n, gopIndex: 1, stepIndex: 1 }, 1n)));
    expect([d.moved.count, d.absolute.count, d.lanes.length, d.spawns.count, d.despawns.count, d.signals.count]).toEqual([0, 0, 0, 0, 0, 0]);
  });
});

// ---------------------------------------------------------------------------
// S6 (swept) — §2.2 in the four remaining decoders: Event, Provenance,
// Telemetry, MetricSample
// ---------------------------------------------------------------------------

/**
 * The rest of the module, so the sweep is not half applied.
 *
 * §9 carries annotated hex only for `Hello` (§9.1), `Keyframe` (§9.2) and `Delta` (§9.3) — there is
 * no specification vector for any of the four message types below — so every frame here is built
 * with the real encoder in `src/encode.ts` and then patched in exactly one prefix `u32`, which is
 * the convention the rest of this file already follows. The payloads inside the `Event` frames are
 * real §3.6.4 / §3.6.5 records at their documented sizes.
 *
 * `Telemetry` and `MetricSample` reached here by a different route than the rest: they never used
 * the `count === 0 || off === 0` spelling, they went straight to `checkSection(v, 0, total)`, which
 * *passes* — `0 + total` is inside the body — and hands back the body origin. So instead of
 * decoding to nothing they decoded their records over their own 32-byte prefix. Probed on the
 * pre-fix decoder: a one-node `Telemetry` with `off_records = 0` reported `node_count = 1` and
 * `node_id[0] = 0x0`, and a one-sample `MetricSample` with `off_samples = 0` reported
 * `value = 4.94e-315` and `str_metric = 1000000000` — the prefix's own `sim_time_ns` and
 * `bin_width_ns` read as a sample record — with no error either time.
 */
describe("§2.2 — Event, Provenance, Telemetry and MetricSample honour the sentinel too", () => {
  /** Encode a body, patch one prefix `u32`, and hand back a whole frame. */
  function patched(msgType: number, body: Uint8Array, field: number, value: number): ArrayBuffer {
    const copy = body.slice();
    new DataView(copy.buffer, copy.byteOffset).setUint32(field, value, true);
    return frameOf(msgType, 0n, copy);
  }
  const wire = (frame: ArrayBuffer, off: number): number => new DataView(frame, FRAME_HEADER_BYTES).getUint32(off, true);

  function failure(fn: () => unknown): { code: string; closeCode: number; field?: string } {
    try {
      fn();
      throw new Error("expected a ProtocolError, nothing was thrown");
    } catch (err) {
      if (!(err instanceof ProtocolError)) throw new Error(`expected a ProtocolError, got ${String(err)}`);
      return { code: err.code, closeCode: err.closeCode, field: err.detail.field };
    }
  }

  const eventBody = (): Uint8Array =>
    encodeEventBody(0n, 1_000_000n, [
      { simTimeNs: 0n, channelId: ChannelId.NODE_TX, payload: new Uint8Array(EVENT_PAYLOAD_BYTES[ChannelId.NODE_TX]) },
      { simTimeNs: 1000n, channelId: ChannelId.PHY_RX, payload: new Uint8Array(EVENT_PAYLOAD_BYTES[ChannelId.PHY_RX]) },
    ]);
  const provBody = (): Uint8Array =>
    encodeProvenanceBody(
      0n,
      [{ provId: 3, strModelId: 1, strModelVersion: 2, strParamSetId: 0, strCardUrl: 0, family: 0, subjectKind: 4 }],
      [{ dimKey: 9, strDims: 1 }],
      ["dist_bin=75,rat=dsrc"],
    );
  const telemetryBody = (): Uint8Array => encodeTelemetryBody(1_000_000_000n, 1_000_000_000n, [telemetryRow(7)]);
  const metricBody = (): Uint8Array =>
    encodeMetricSampleBody(1_000_000_000n, 1_000_000_000n, [
      { value: 0.97, strMetric: 11, dimKey: 0, nodeId: 0xffffffff, count: 1, agg: 0, visibility: 0, provId: 3 },
    ]);

  it("the four encoder-built frames this block patches really do carry their sections", () => {
    const ev = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, eventBody())));
    expect(ev.count).toBe(2);
    expect(ev.payloads.byteLength).toBe(EVENT_PAYLOAD_BYTES[ChannelId.NODE_TX] + EVENT_PAYLOAD_BYTES[ChannelId.PHY_RX]);
    const pv = decodeProvenance(viewFrame(frameOf(MsgType.Provenance, 0n, provBody())));
    expect(pv.entries.count).toBe(1);
    expect(pv.dims.count).toBe(1);
    const tl = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 0n, telemetryBody())));
    expect(tl.nodeCount).toBe(1);
    expect(tl.nodeIdAt(0)).toBe(7);
    const ms = decodeMetricSample(viewFrame(frameOf(MsgType.MetricSample, 0n, metricBody())));
    expect(ms.sampleCount).toBe(1);
    expect(ms.sample(0).strMetric).toBe(11);
  });

  it("§3.6.1 — event_count = 2 with off_index = 0 is a protocol error", () => {
    const frame = patched(MsgType.Event, eventBody(), EVENT_OFFSETS.offIndex, 0);
    expect(wire(frame, EVENT_OFFSETS.eventCount)).toBe(2);
    expect(failure(() => decodeEvent(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field: "Event.index" });
  });

  it("§3.6.1 — payload_bytes > 0 with off_payloads = 0 is a protocol error, not a read of the prefix", () => {
    // The worst of the family: `payloads` came back empty, but `payloadView(i)` still passed its
    // `off + len <= payload_bytes` check against the *declared* size and then read from body
    // offset 0 — decoding the 32-byte Event prefix as a §3.6 payload.
    const frame = patched(MsgType.Event, eventBody(), EVENT_OFFSETS.offPayloads, 0);
    expect(wire(frame, EVENT_OFFSETS.payloadBytes)).toBeGreaterThan(0);
    expect(failure(() => decodeEvent(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field: "Event.payloads" });
  });

  it("§3.8 — entry_count and dim_count both get the same treatment", () => {
    for (const [off, count, field] of [
      [PROVENANCE_OFFSETS.offEntries, PROVENANCE_OFFSETS.entryCount, "Provenance.entries"],
      [PROVENANCE_OFFSETS.offDims, PROVENANCE_OFFSETS.dimCount, "Provenance.dims"],
    ] as const) {
      const frame = patched(MsgType.Provenance, provBody(), off, 0);
      expect(wire(frame, count)).toBe(1);
      expect(failure(() => decodeProvenance(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field });
    }
  });

  it("§3.5.1 — node_count = 1 with off_records = 0 no longer reads the record out of the prefix", () => {
    const frame = patched(MsgType.Telemetry, telemetryBody(), TELEMETRY_OFFSETS.offRecords, 0);
    expect(wire(frame, TELEMETRY_OFFSETS.nodeCount)).toBe(1);
    expect(failure(() => decodeTelemetry(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field: "Telemetry.records" });
  });

  it("§3.7 — sample_count = 1 with off_samples = 0 likewise", () => {
    const frame = patched(MsgType.MetricSample, metricBody(), METRIC_OFFSETS.offSamples, 0);
    expect(wire(frame, METRIC_OFFSETS.sampleCount)).toBe(1);
    expect(failure(() => decodeMetricSample(viewFrame(frame)))).toEqual({ code: "bad_offset", closeCode: 1002, field: "MetricSample.samples" });
  });

  it("genuinely empty sections still decode to empty, in all four", () => {
    const ev = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, encodeEventBody(0n, 0n, []))));
    expect([ev.count, ev.payloads.byteLength]).toEqual([0, 0]);
    const pv = decodeProvenance(viewFrame(frameOf(MsgType.Provenance, 0n, encodeProvenanceBody(0n, [], [], []))));
    expect([pv.entries.count, pv.dims.count, pv.stringExtension.length]).toEqual([0, 0, 0]);
    const tl = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 0n, encodeTelemetryBody(0n, 0n, []))));
    expect(tl.nodeCount).toBe(0);
    const ms = decodeMetricSample(viewFrame(frameOf(MsgType.MetricSample, 0n, encodeMetricSampleBody(0n, 0n, []))));
    expect(ms.sampleCount).toBe(0);
    // §3.6.1 — E events whose payloads are all empty: `off_payloads` is non-zero but the region is
    // of no bytes, which is why the payload region is keyed on `payload_bytes` and not on `E`.
    const empty = encodeEventBody(0n, 1n, [{ simTimeNs: 0n, channelId: 9999, payload: new Uint8Array(0) }]);
    expect(new DataView(empty.buffer, empty.byteOffset).getUint32(EVENT_OFFSETS.offPayloads, true)).toBeGreaterThan(0);
    expect(new DataView(empty.buffer, empty.byteOffset).getUint32(EVENT_OFFSETS.payloadBytes, true)).toBe(0);
    const ep = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, empty)));
    expect(ep.count).toBe(1);
    expect(ep.payloads.byteLength).toBe(0);
  });

  it("§3.8 — off_strings = 0 stays legitimate: it is the only way to say `no extension`", () => {
    // The one zero offset in this module that must NOT become an error. §3.8's prefix table spells
    // it out — "off_strings — symbol-table extension, or `0`" — and there is no count beside it:
    // a StrTable carries its own `n` inside the section, so 0 is how absence is expressed. The
    // same holds for Hello.off_strings (§3.1.7), Error and Bye (§3.10, §3.11).
    const body = encodeProvenanceBody(0n, [{ provId: 3, strModelId: 0, strModelVersion: 0, strParamSetId: 0, strCardUrl: 0, family: 0, subjectKind: 4 }], [], []);
    expect(new DataView(body.buffer, body.byteOffset).getUint32(PROVENANCE_OFFSETS.offStrings, true)).toBe(0);
    const pv = decodeProvenance(viewFrame(frameOf(MsgType.Provenance, 0n, body)));
    expect(pv.stringExtension).toEqual([]);
    expect(pv.entries.count).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// S5 — §3.6.1 / §3.6.4–§3.6.17 fixed record sizes
// ---------------------------------------------------------------------------

describe("§3.6.4–§3.6.17 — an event payload shorter than its record is rejected, typed", () => {
  /** An Event frame carrying the given payloads, through the real encoder. */
  function eventFrame(events: readonly { channelId: number; payload: Uint8Array }[]): ArrayBuffer {
    return frameOf(
      MsgType.Event,
      0n,
      encodeEventBody(
        0n,
        1_000_000n,
        events.map((e, i) => ({ simTimeNs: BigInt(i), channelId: e.channelId, payload: e.payload })),
      ),
    );
  }

  it("the per-channel record sizes are the ones §3.6 states", () => {
    // Transcribed from the §3.6.4–§3.6.17 headings, not from the decoder.
    expect(EVENT_PAYLOAD_BYTES).toMatchObject({
      [ChannelId.NODE_TX]: 40,
      [ChannelId.PHY_RX]: 48,
      [ChannelId.NODE_VERIFY]: 48,
      [ChannelId.SEC_CERT]: 40,
      [ChannelId.DET_OBSERVATION]: 32,
      [ChannelId.APP_WARNING]: 32,
      [ChannelId.PROTO_REVOCATION]: 32,
      [ChannelId.GT_KINEMATICS]: 56,
      [ChannelId.GT_ATTACK_ACTION]: 32,
      [ChannelId.MAC_CBR]: 16,
      [ChannelId.NET_FRAG]: 24,
      [ChannelId.NODE_NEIGHBOR]: 32,
      [ChannelId.PROTO_MSG]: 32,
      [ChannelId.MA_REPORT]: 40,
      [ChannelId.MA_CASE]: 40,
      [ChannelId.MA_DECISION]: 40,
    });
    expect(eventChannelName(ChannelId.PHY_RX)).toBe("phy.rx");
  });

  it("an 8-byte phy.rx payload throws a ProtocolError, not a bare RangeError", () => {
    const msg = decodeEvent(viewFrame(eventFrame([{ channelId: ChannelId.PHY_RX, payload: new Uint8Array(8) }])));
    let caught: unknown = null;
    try {
      msg.payload(0);
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(ProtocolError);
    expect((caught as ProtocolError).code).toBe("bad_length");
    expect((caught as ProtocolError).closeCode).toBe(1002);
    expect((caught as ProtocolError).detail.field).toBe("phy.rx");
  });

  it("every known channel rejects a one-byte payload with bad_length", () => {
    for (const id of Object.keys(EVENT_PAYLOAD_BYTES).map(Number)) {
      const msg = decodeEvent(viewFrame(eventFrame([{ channelId: id, payload: new Uint8Array(1) }])));
      expect(codeOf(() => msg.payload(0)), eventChannelName(id)).toBe("bad_length");
    }
  });

  it("a short node.tx payload never returns bytes from the neighbouring payload as a HashedId8", () => {
    // 30 bytes pads to 32, so `pseudonym_digest` at +28 would have read 28..36 — four bytes of the
    // next payload — and returned them before a later field threw.
    const short = new Uint8Array(30).fill(0x11);
    const neighbour = new Uint8Array(40).fill(0x22);
    const msg = decodeEvent(
      viewFrame(eventFrame([{ channelId: ChannelId.NODE_TX, payload: short }, { channelId: ChannelId.NODE_TX, payload: neighbour }])),
    );
    expect(codeOf(() => msg.payload(0))).toBe("bad_length");
    // The well-formed neighbour still decodes, and its digest is its own bytes.
    const ok = msg.payload(1);
    expect(ok.channel).toBe("node.tx");
    expect(Array.from((ok as { pseudonymDigest: Uint8Array }).pseudonymDigest)).toEqual(new Array(8).fill(0x22));
  });

  it("an unknown channel still falls through to UnknownPayload (§3.6.1, §8.4)", () => {
    const msg = decodeEvent(viewFrame(eventFrame([{ channelId: 1234, payload: new Uint8Array(3).fill(9) }])));
    const p = msg.payload(0);
    expect(p.channel).toBe("unknown");
    expect((p as { channelId: number }).channelId).toBe(1234);
  });

  it("decodeEventPayload is safe when called directly on a truncated view", () => {
    const dv = new DataView(new ArrayBuffer(16));
    expect(codeOf(() => decodeEventPayload(ChannelId.DET_OBSERVATION, dv))).toBe("bad_length");
  });
});

// ---------------------------------------------------------------------------
// S9 — §3.6.1 alignment and ordering MUSTs, §10.4 C4
// ---------------------------------------------------------------------------

describe("§3.6.1 / §10.4 C4 — the reader verifies what the writer honours", () => {
  /** Two 8-byte-padded payloads on `mac.cbr` (16 bytes each), times 1 and 2. */
  function twoEventFrame(): ArrayBuffer {
    return frameOf(
      MsgType.Event,
      0n,
      encodeEventBody(0n, 10n, [
        { simTimeNs: 1n, channelId: ChannelId.MAC_CBR, payload: new Uint8Array(16) },
        { simTimeNs: 2n, channelId: ChannelId.MAC_CBR, payload: new Uint8Array(16) },
      ]),
    );
  }
  const bodyDv = (frame: ArrayBuffer): DataView => new DataView(frame, FRAME_HEADER_BYTES);

  it("rejects an index that is not sorted by (sim_time_ns, channel_id)", () => {
    const frame = twoEventFrame();
    // sim_time_ns column starts at off_index (= 32); make entry 0 later than entry 1.
    bodyDv(frame).setBigUint64(EVENT_PREFIX_BYTES + 0, 99n, true);
    expect(codeOf(() => decodeEvent(viewFrame(frame)))).toBe("bad_state");
  });

  it("rejects equal times whose channel ids descend", () => {
    const frame = frameOf(
      MsgType.Event,
      0n,
      encodeEventBody(0n, 10n, [
        { simTimeNs: 5n, channelId: ChannelId.MAC_CBR, payload: new Uint8Array(16) },
        { simTimeNs: 5n, channelId: ChannelId.NET_FRAG, payload: new Uint8Array(24) },
      ]),
    );
    const dv = bodyDv(frame);
    const chan = EVENT_PREFIX_BYTES + 14 * 2; // channel_id column, E = 2
    dv.setUint16(chan + 0, ChannelId.NET_FRAG, true);
    dv.setUint16(chan + 2, ChannelId.MAC_CBR, true);
    expect(codeOf(() => decodeEvent(viewFrame(frame)))).toBe("bad_state");
  });

  it("rejects a payload_off that is not a multiple of 8", () => {
    const frame = twoEventFrame();
    // payload_off column is at off_index + 8·E; entry 0 sits at +0.
    bodyDv(frame).setUint32(EVENT_PREFIX_BYTES + 8 * 2, 4, true);
    const msg = decodeEvent(viewFrame(frame));
    expect(codeOf(() => msg.payloadView(0))).toBe("misaligned");
    expect(codeOf(() => msg.payload(0))).toBe("misaligned");
    // The conforming entry is untouched.
    expect(msg.payloadView(1).byteLength).toBe(16);
  });

  it("rejects an off_payloads that is not 8-aligned", () => {
    const frame = twoEventFrame();
    const dv = bodyDv(frame);
    const offPayloads = dv.getUint32(24, true);
    dv.setUint32(24, offPayloads + 4, true); // 8-aligned + 4
    dv.setUint32(28, 16, true); // shrink payload_bytes so the section still fits the body
    expect(codeOf(() => decodeEvent(viewFrame(frame)))).toBe("misaligned");
  });

  it("the encoder's own output passes all three checks", () => {
    const msg = decodeEvent(viewFrame(twoEventFrame()));
    expect(msg.count).toBe(2);
    expect(msg.index.payloadOff[1] % 8).toBe(0);
    expect(msg.payloadView(1).byteLength).toBe(16);
  });
});

// ---------------------------------------------------------------------------
// S10 — §2.5 StrTable endpoint invariants and the body bound
// ---------------------------------------------------------------------------

describe("§2.5 — the StrTable endpoint invariants are asserted, not inferred", () => {
  const tableOf = (strings: readonly string[]): { bytes: Uint8Array; dv: DataView } => {
    const bytes = encodeStrTable(strings);
    return { bytes, dv: new DataView(bytes.buffer, bytes.byteOffset) };
  };

  it("offsets[n] must equal blob_bytes — a table one byte short is rejected, not silently truncated", () => {
    const { bytes, dv } = tableOf(["", "alpha", "omega"]);
    const n = dv.getUint32(0, true);
    const blobBytes = dv.getUint32(4, true);
    dv.setUint32(8 + 4 * n, blobBytes - 1, true); // still non-decreasing, so the old code accepted it
    expect(codeOf(() => decodeStrTable(bytes.buffer as ArrayBuffer, bytes.byteOffset))).toBe("bad_offset");
  });

  it("offsets[0] must be 0 — a consistently shifted table is rejected, not silently missing a byte", () => {
    const { bytes, dv } = tableOf(["", "alpha", "omega"]);
    dv.setUint32(8, 1, true); // offsets[0] = 1: the first byte of the blob would be dropped
    expect(codeOf(() => decodeStrTable(bytes.buffer as ArrayBuffer, bytes.byteOffset))).toBe("bad_offset");
  });

  it("a conforming table still decodes, including the empty one", () => {
    const { bytes } = tableOf(["", "alpha", "omega"]);
    expect(strTableStrings(decodeStrTable(bytes.buffer as ArrayBuffer, bytes.byteOffset))).toEqual(["", "alpha", "omega"]);
    const empty = encodeStrTable([]);
    expect(decodeStrTable(empty.buffer as ArrayBuffer, empty.byteOffset).count).toBe(0);
  });

  it("a table is bounded by body_len, not by the frame buffer's trailing bytes", () => {
    // §2.1 lets a frame carry trailing bytes beyond body_len and §8 says readers ignore them, so a
    // table that only fits when those bytes are counted is truncated.
    const body = encodeByeBody({ simTimeNs: 0n, canonicalFrames: 1n, reason: 0, detail: "goodbye", firstStringId: 1 });
    const frame = new ArrayBuffer(FRAME_HEADER_BYTES + body.byteLength);
    serialiseFrameHeader(frame, 0, { msgType: MsgType.Bye, bodyLen: body.byteLength - 4, seq: 0n });
    new Uint8Array(frame, FRAME_HEADER_BYTES).set(body);
    expect(codeOf(() => decodeBye(viewFrame(frame)))).toBe("truncated");

    // The same frame with an honest body_len decodes.
    const honest = frameOf(MsgType.Bye, 0n, body);
    expect(decodeBye(viewFrame(honest)).stringExtension).toEqual(["goodbye"]);
  });
});

// ---------------------------------------------------------------------------
// S11 — §2.5 / §3.10 / §3.11: what an Error or Bye extension does to the id space
// ---------------------------------------------------------------------------

describe("§2.5 / §3.10 / §3.11 — the Error/Bye symbol-table rule this client implements", () => {
  it("an Error extension takes ids from the current table size, exactly as Provenance does", () => {
    // §2.5 names only `Provenance` as appending, yet §3.10 and §3.11 both end in "a symbol-table
    // extension". This pins the reading the client implements so a change is deliberate; the
    // specification itself still needs the erratum (register finding S11).
    const table = new StringTable();
    table.reset(["", "engine", "scn"]);
    expect(table.size).toBe(3);
    const body = encodeErrorBody({ simTimeNs: 5n, code: -32000, fatal: false, message: "engine aborted", detail: "{}", firstStringId: table.size });
    const err = decodeError(viewFrame(frameOf(MsgType.Error, 0n, body)), table);
    expect(err.message).toBe("engine aborted");
    expect(err.detail).toBe("{}");
    expect(err.stringExtension).toEqual(["engine aborted", "{}"]);
    // Ids below the base still resolve out of the connection table.
    expect(table.get(1)).toBe("engine");
    // And appending the extension continues the id space, never reassigning (§2.5).
    expect(table.append(err.stringExtension as string[])).toBe(3);
    expect(table.size).toBe(5);
    expect(table.get(3)).toBe("engine aborted");
  });
});

// ---------------------------------------------------------------------------
// S12 — §10.1 F4 / §2.2: the zero-copy path is host-endian
// ---------------------------------------------------------------------------

describe("§2.2 / §10.1 F4 — the zero-copy path requires a little-endian host", () => {
  it("detects the host byte order and lets a little-endian host through", () => {
    expect(isLittleEndianHost()).toBe(true);
    expect(() => assertLittleEndianHost()).not.toThrow();
    // viewFrame asserts it before building any typed-array view.
    expect(() => viewFrame(frameOf(MsgType.Bye, 0n, encodeByeBody({ simTimeNs: 0n, canonicalFrames: 0n, reason: 0, firstStringId: 1 })))).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// S13 — §1.3 rule 1 / §10.1 F7: Hello is never compressed
// ---------------------------------------------------------------------------

describe("§1.3 rule 1 / §10.1 F7 — Hello is never compressed", () => {
  it("helloFrame masks FLAG_COMPRESSED out of the flags a caller passes", () => {
    const frame = helloFrame({ ...HELLO_BASE, nodes: [], classes: [], channels: [] }, 0n, FrameFlags.COMPRESSED | FrameFlags.RESYNC);
    const flags = new DataView(frame).getUint16(12, true);
    expect(flags & FrameFlags.COMPRESSED).toBe(0);
    expect(flags & FrameFlags.RESYNC).toBe(FrameFlags.RESYNC);
    expect(decodeHello(viewFrame(frame)).versionMajor).toBe(1);
  });

  it("viewFrame rejects a Hello that claims to be compressed, decompressor or not", () => {
    const body = encodeHelloBody({ ...HELLO_BASE, nodes: [], classes: [], channels: [] });
    const frame = frameOf(MsgType.Hello, 0n, body, FrameFlags.COMPRESSED);
    expect(codeOf(() => viewFrame(frame))).toBe("bad_state");
    expect(codeOf(() => viewFrame(frame, { decompress: (bytes) => bytes }))).toBe("bad_state");
    // A non-Hello frame with the same flag still takes the §2.6 path.
    const other = frameOf(MsgType.Bye, 0n, new Uint8Array(4), FrameFlags.COMPRESSED);
    expect(codeOf(() => viewFrame(other))).toBe("compressed_unsupported");
  });
});

// ---------------------------------------------------------------------------
// S7 — §3.4 / §1.5: a Delta is applied all-or-nothing
// ---------------------------------------------------------------------------

describe("§3.4 / §1.5 — a Delta is never partially applied", () => {
  /** Two actors at x = 512345 mm, z = 15 cm, in GOP 1. */
  function seeded(): PoseBuffer {
    const poses = new PoseBuffer(8);
    poses.applyKeyframe(
      buildKeyframe({
        simTimeNs: 1_000_000_000n,
        originXM: 0,
        originYM: 0,
        originZM: 0,
        gopIndex: 1,
        actors: [0, 1].map((i) => ({
          actorId: i, xMm: 512345, yMm: 1000, laneId: 42, zCm: 15, headingBrad: 0, speedCq: 0, accelCq: 0,
          classIdx: 0, state: 8, verifiedNeighbors: 0, flags8: 0,
        })),
        signals: [],
      }),
    );
    return poses;
  }

  const movedRow = (slot: number, dxMm: number, mflags: number) => ({
    slot, dxMm, dyMm: 0, dzMm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, state: 8, verifiedNeighbors: 0, mflags,
  });

  it("MFLAG_LANE_CHANGED with an empty lane block leaves the buffer exactly as it was", () => {
    const poses = seeded();
    const before = { x0: poses.xMm[0], x1: poses.xMm[1], z0: poses.zMm[0], lane0: poses.laneId[0], step: poses.stepIndex, t: poses.simTimeNs };
    const delta = buildDelta({
      simTimeNs: 1_100_000_000n,
      gopIndex: 1,
      stepIndex: 1,
      // Row 0 is a plain move whose write the old code had already committed (512345 -> 512346)
      // before row 1 threw; row 1 asks for a lane the delta does not carry.
      moved: [movedRow(0, 1, 0), movedRow(1, 1, MovedFlags.LANE_CHANGED)],
      lanes: [],
    });
    expect(codeOf(() => poses.applyDelta(delta))).toBe("bad_state");
    expect(poses.xMm[0]).toBe(before.x0);
    expect(poses.xMm[1]).toBe(before.x1);
    expect(poses.zMm[0]).toBe(before.z0);
    expect(poses.laneId[0]).toBe(before.lane0);
    expect(poses.stepIndex).toBe(before.step);
    expect(poses.simTimeNs).toBe(before.t);
  });

  it("MFLAG_ABSOLUTE with an empty absolute block leaves the buffer exactly as it was", () => {
    const poses = seeded();
    const before = poses.xMm[0];
    const delta = buildDelta({
      simTimeNs: 1_100_000_000n,
      gopIndex: 1,
      stepIndex: 1,
      moved: [movedRow(0, 1, 0), movedRow(1, 0, MovedFlags.ABSOLUTE)],
      absolute: [],
    });
    expect(codeOf(() => poses.applyDelta(delta))).toBe("bad_state");
    expect(poses.xMm[0]).toBe(before);
    expect(poses.stepIndex).toBe(0);
  });

  it("a moved slot beyond the buffer leaves the earlier rows untouched", () => {
    const poses = seeded();
    const before = poses.xMm[0];
    const delta = buildDelta({
      simTimeNs: 1_100_000_000n,
      gopIndex: 1,
      stepIndex: 1,
      moved: [movedRow(0, 1, 0), movedRow(4_000, 1, 0)],
    });
    expect(codeOf(() => poses.applyDelta(delta))).toBe("bad_state");
    expect(poses.xMm[0]).toBe(before);
    expect(poses.stepIndex).toBe(0);
  });

  it("and a conforming delta with both blocks still applies in full", () => {
    const poses = seeded();
    const applied = poses.applyDelta(
      buildDelta({
        simTimeNs: 1_100_000_000n,
        gopIndex: 1,
        stepIndex: 1,
        moved: [movedRow(0, 1, MovedFlags.LANE_CHANGED), movedRow(1, 0, MovedFlags.ABSOLUTE)],
        absolute: [{ xMm: 7000, yMm: 8000, zCm: 20 }],
        lanes: [99],
      }),
    );
    expect(applied).toEqual({ applied: true, moved: 2, spawned: 0, despawned: 0 });
    expect(poses.xMm[0]).toBe(512346);
    expect(poses.laneId[0]).toBe(99);
    expect(poses.xMm[1]).toBe(7000);
    expect(poses.zMm[1]).toBe(200);
    expect(poses.stepIndex).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// S8 — §3.4.5 / §3.1.1: an untrusted slot id must not drive allocation
// ---------------------------------------------------------------------------

describe("§3.4.5 / §3.1.1 — a wire slot id cannot drive unbounded allocation", () => {
  const spawnRow = (slot: number) => ({
    slot, actorId: 5, nodeId: 0xffffffff, xMm: 0, yMm: 0, laneId: 0xffffffff, zCm: 0,
    headingBrad: 0, speedCq: 0, cause: 0xffff, classIdx: 0, state: 8, verifiedNeighbors: 0,
  });

  function seededBuffer(): PoseBuffer {
    const poses = new PoseBuffer(1024);
    poses.applyKeyframe(
      buildKeyframe({
        simTimeNs: 0n, originXM: 0, originYM: 0, originZM: 0, gopIndex: 0,
        actors: [{ actorId: 0, xMm: 0, yMm: 0, laneId: 0xffffffff, zCm: 0, headingBrad: 0, speedCq: 0, accelCq: 0, classIdx: 0, state: 8, verifiedNeighbors: 0, flags8: 0 }],
        signals: [],
      }),
    );
    return poses;
  }

  it("PoseBuffer refuses a spawn slot of 3,000,000,000 without allocating", () => {
    const poses = seededBuffer();
    const capacityBefore = poses.capacity;
    const delta = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1, spawns: [spawnRow(3_000_000_000)] });
    expect(codeOf(() => poses.applyDelta(delta))).toBe("bad_offset");
    // Measured before the bound existed: capacity grew to 4,294,967,296 (~116 GB of typed arrays)
    // and applyDelta returned applied = true.
    expect(poses.capacity).toBe(capacityBefore);
    expect(poses.stepIndex).toBe(0);
  });

  it("the hard ceiling is 1 << 20 slots even with no Hello", () => {
    const poses = seededBuffer();
    expect(poses.slotLimit).toBe(MAX_ACTOR_SLOTS);
    const delta = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1, spawns: [spawnRow(MAX_ACTOR_SLOTS)] });
    expect(codeOf(() => poses.applyDelta(delta))).toBe("bad_offset");
    expect(poses.capacity).toBe(1024);
  });

  it("Hello.actor_capacity tightens the bound, with the allocated capacity as the floor", () => {
    const poses = seededBuffer();
    poses.setSlotBound(4096);
    expect(poses.slotLimit).toBe(4096);
    const far = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1, spawns: [spawnRow(500_000)] });
    expect(codeOf(() => poses.applyDelta(far))).toBe("bad_offset");
    // A slot inside the bound is still honoured, and still grows the buffer.
    const near = buildDelta({ simTimeNs: 1n, gopIndex: 0, stepIndex: 1, spawns: [spawnRow(4_000)] });
    expect(poses.applyDelta(near).applied).toBe(true);
    expect(poses.capacity).toBeGreaterThanOrEqual(4001);
    expect(poses.occupied[4_000]).toBe(1);
  });

  it("SlotTable.adoptSpawn has the same bound", () => {
    const slots = new SlotTable(1024);
    expect(slots.slotLimit).toBe(MAX_ACTOR_SLOTS);
    expect(codeOf(() => slots.adoptSpawn(3_000_000_000, 5))).toBe("bad_offset");
    expect(slots.capacity).toBe(1024);
    slots.setSlotBound(4096);
    expect(codeOf(() => slots.adoptSpawn(500_000, 5))).toBe("bad_offset");
    slots.adoptSpawn(4_000, 5);
    expect(slots.actorIdOf(4_000)).toBe(5);
  });
});
