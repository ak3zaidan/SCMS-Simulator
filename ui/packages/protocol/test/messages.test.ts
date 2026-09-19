/** §3.5–§3.11 and the content conformance items of §10.4. */

import { describe, expect, it } from "vitest";

import {
  ByeReason,
  ChannelId,
  MsgType,
  ProvenanceFlags,
  RpcErrorCode,
  SENTINEL_U16,
  SENTINEL_U32,
  SENTINEL_U64,
  StringTable,
  TELEMETRY_RECORD_BYTES_V1,
  decodeBye,
  decodeError,
  decodeEvent,
  decodeMessage,
  decodeMetricSample,
  decodeProvenance,
  decodeStrTable,
  decodeTelemetry,
  decodeWorldChunk,
  encodeByeBody,
  encodeErrorBody,
  encodeEventBody,
  encodeMetricSampleBody,
  encodeProvenanceBody,
  encodeStrTable,
  encodeTelemetryBody,
  encodeWorldChunkBody,
  frameOf,
  strTableStrings,
  viewFrame,
  type NodeTelemetryInit,
} from "../src/index.js";
import { telemetryRow as telemetry } from "./helpers/build-frames.js";

describe("§2.5 — the symbol table", () => {
  it("round-trips through encode and decode, including the padding rule", () => {
    const strings = ["", "abc", "", "a much longer string to force padding", "x"];
    const bytes = encodeStrTable(strings);
    expect(bytes.byteLength % 4).toBe(0);
    const view = decodeStrTable(bytes.buffer as ArrayBuffer, 0);
    expect(view.count).toBe(5);
    expect(view.byteLength).toBe(bytes.byteLength);
    expect(strTableStrings(view)).toEqual(strings);
    expect(view.offsets[0]).toBe(0);
    expect(view.offsets[view.count]).toBe(view.blobBytes);
  });

  it("§10.4 C7 — is append-only within a connection and resets on a non-resumed Hello", () => {
    const table = new StringTable();
    expect(table.size).toBe(1);
    expect(table.get(0)).toBe("");
    table.reset(["", "a", "b"]);
    expect(table.size).toBe(3);
    const first = table.append(["c", "d"]);
    expect(first).toBe(3);
    expect(table.get(3)).toBe("c");
    expect(table.get(4)).toBe("d");
    expect(table.get(99)).toBe(""); // unknown ids resolve to the empty string, never throw
    table.reset(["", "z"]);
    expect(table.size).toBe(2);
    expect(table.get(3)).toBe("");
  });
});

describe("§3.5 — Telemetry", () => {
  it("decodes every field of the 208-byte record", () => {
    const body = encodeTelemetryBody(5_000_000_000n, 1_000_000_000n, [telemetry(7), telemetry(9)]);
    expect(body.byteLength).toBe(32 + 208 * 2);
    const msg = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 42n, body)));
    expect(msg.simTimeNs).toBe(5_000_000_000n);
    expect(msg.windowNs).toBe(1_000_000_000n);
    expect(msg.nodeCount).toBe(2);
    expect(msg.recordSize).toBe(TELEMETRY_RECORD_BYTES_V1);
    expect(msg.recordSize).toBe(208);
    expect(msg.nodeIdAt(0)).toBe(7);
    expect(msg.nodeIdAt(1)).toBe(9);
    // The f32 columns come back at f32 precision, so the expectation is rounded the same way.
    const f32Fields = [
      "msgsInPerS", "msgsOutPerS", "verificationsPerS", "verifyWaitP50Ms", "verifyWaitP95Ms",
      "gnssHdop", "gnssSigmaM", "clockDriftPpm", "posErrorM", "airtimeMsPerS",
    ] as const;
    const expected: Record<string, unknown> = { ...telemetry(7) };
    for (const f of f32Fields) expected[f] = Math.fround(expected[f] as number);
    const r = msg.record(0);
    expect(r).toEqual(expected);
    expect(Object.keys(r).sort()).toEqual(Object.keys(telemetry(7)).sort()); // §10.4 C1: no field missing
    expect(msg.records()).toHaveLength(2);
    expect(() => msg.record(2)).toThrow(/out of range/);
  });

  it("§10.4 C2 — strides by the wire record_size, not by 208", () => {
    // A v1.1 writer appends fields into the reserved tail and raises record_size (§8.4).
    const v11Size = 240;
    const one = encodeTelemetryBody(1n, 1n, [telemetry(3)]);
    const grown = new Uint8Array(32 + v11Size * 2);
    grown.set(one.subarray(0, 32), 0);
    new DataView(grown.buffer).setUint32(24, v11Size, true);
    new DataView(grown.buffer).setUint32(16, 2, true);
    grown.set(one.subarray(32, 32 + 208), 32);
    const second = encodeTelemetryBody(1n, 1n, [telemetry(11)]);
    grown.set(second.subarray(32, 32 + 208), 32 + v11Size);
    // fill the new tail with values a v1 reader must ignore
    grown.fill(0xab, 32 + 208, 32 + v11Size);

    const msg = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 1n, grown)));
    expect(msg.recordSize).toBe(240);
    expect(msg.nodeIdAt(0)).toBe(3);
    expect(msg.nodeIdAt(1)).toBe(11); // only correct if the stride came off the wire
    expect(msg.record(1).nbrVerified).toBe(51);
  });

  it("carries the §0 sentinels unmodified", () => {
    const unknown: NodeTelemetryInit = {
      ...telemetry(1),
      nextTopupNs: SENTINEL_U64,
      dropCrlBacklog: SENTINEL_U32,
      dccState: SENTINEL_U16,
      gnssSigmaM: Number.NaN,
    };
    const msg = decodeTelemetry(viewFrame(frameOf(MsgType.Telemetry, 0n, encodeTelemetryBody(0n, 0n, [unknown]))));
    const r = msg.record(0);
    expect(r.nextTopupNs).toBe(SENTINEL_U64);
    expect(r.dropCrlBacklog).toBe(SENTINEL_U32);
    expect(r.dccState).toBe(SENTINEL_U16);
    expect(Number.isNaN(r.gnssSigmaM)).toBe(true);
  });
});

describe("§3.6 — Event", () => {
  const nodeTx = (nodeId: number, msgId: number): Uint8Array => {
    const b = new Uint8Array(40);
    const dv = new DataView(b.buffer);
    dv.setUint32(0, nodeId, true);
    dv.setUint32(4, msgId, true);
    dv.setUint32(8, 311, true);
    dv.setFloat32(12, 0.48, true);
    dv.setUint16(16, 1, true); // BSM
    dv.setInt16(18, 2000, true);
    dv.setUint16(20, 172, true);
    dv.setUint8(22, 5);
    dv.setUint8(23, 0);
    dv.setUint8(24, 1);
    dv.setUint8(25, 0);
    dv.setUint16(26, 213, true);
    b.set([1, 2, 3, 4, 5, 6, 7, 8], 28);
    dv.setUint32(36, 4242, true);
    return b;
  };
  const phyRx = (rxNode: number, txNode: number): Uint8Array => {
    const b = new Uint8Array(48);
    const dv = new DataView(b.buffer);
    dv.setBigUint64(0, 100n, true);
    dv.setBigUint64(8, 400n, true);
    dv.setUint32(16, rxNode, true);
    dv.setUint32(20, txNode, true);
    dv.setUint32(24, 77, true);
    dv.setFloat32(28, -78.5, true);
    dv.setFloat32(32, 14.25, true);
    dv.setFloat32(36, 122.5, true);
    dv.setUint8(40, 0);
    dv.setUint8(41, 0);
    dv.setUint8(42, 0);
    return b;
  };

  it("sorts the index by (sim_time_ns, channel_id) and 8-aligns every payload (§10.4 C4)", () => {
    const body = encodeEventBody(0n, 1_000_000n, [
      { simTimeNs: 900n, channelId: ChannelId.PHY_RX, payload: phyRx(5, 6) },
      { simTimeNs: 100n, channelId: ChannelId.PHY_RX, payload: phyRx(1, 2) },
      { simTimeNs: 100n, channelId: ChannelId.NODE_TX, payload: nodeTx(1, 55) },
    ]);
    const msg = decodeEvent(viewFrame(frameOf(MsgType.Event, 3n, body)));
    expect(msg.count).toBe(3);
    expect(Array.from(msg.index.simTimeNs)).toEqual([100n, 100n, 900n]);
    expect(Array.from(msg.index.channelId)).toEqual([ChannelId.NODE_TX, ChannelId.PHY_RX, ChannelId.PHY_RX]);
    for (let i = 0; i < msg.count; i++) {
      expect(msg.index.payloadOff[i] % 8).toBe(0);
      expect(msg.index.payloadLen[i] % 8).toBe(0);
    }
  });

  it("decodes the documented payload layouts", () => {
    const body = encodeEventBody(0n, 10n, [
      { simTimeNs: 1n, channelId: ChannelId.NODE_TX, payload: nodeTx(42, 900) },
      { simTimeNs: 2n, channelId: ChannelId.PHY_RX, payload: phyRx(7, 42) },
    ]);
    const msg = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, body)));
    const tx = msg.payload(0);
    expect(tx.channel).toBe("node.tx");
    if (tx.channel === "node.tx") {
      expect(tx.nodeId).toBe(42);
      expect(tx.msgId).toBe(900);
      expect(tx.bytesOnAir).toBe(311);
      expect(tx.airtimeMs).toBeCloseTo(0.48, 5);
      expect(tx.msgType).toBe(1);
      expect(tx.txPowerCdbm).toBe(2000);
      expect(tx.channelNumber).toBe(172);
      expect(tx.mcs).toBe(5);
      expect(tx.payloadBytes).toBe(213);
      expect(Array.from(tx.pseudonymDigest)).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
      expect(tx.certId).toBe(4242);
    }
    const rx = msg.payload(1);
    expect(rx.channel).toBe("phy.rx");
    if (rx.channel === "phy.rx") {
      expect(rx.rxNode).toBe(7);
      expect(rx.txNode).toBe(42);
      expect(rx.msgId).toBe(77);
      expect(rx.rssiDbm).toBeCloseTo(-78.5, 5);
      expect(rx.sinrDb).toBeCloseTo(14.25, 5);
      expect(rx.distanceM).toBeCloseTo(122.5, 5);
      expect(rx.outcome).toBe(0);
    }
  });

  it("§10.4 C3 — skips an unknown channel_id by payload_len without desynchronising", () => {
    const future = new Uint8Array(24).fill(0x5a);
    const body = encodeEventBody(0n, 10n, [
      { simTimeNs: 1n, channelId: 4242, payload: future }, // a plug-in channel from §3.6.2's 1000+ range
      { simTimeNs: 2n, channelId: ChannelId.NODE_TX, payload: nodeTx(1, 2) },
    ]);
    const msg = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, body)));
    const unknown = msg.payload(0);
    expect(unknown.channel).toBe("unknown");
    if (unknown.channel === "unknown") {
      expect(unknown.channelId).toBe(4242);
      expect(unknown.bytes.byteLength).toBe(24);
    }
    // the known record after it still decodes correctly
    const tx = msg.payload(1);
    expect(tx.channel).toBe("node.tx");
    if (tx.channel === "node.tx") expect(tx.msgId).toBe(2);
  });

  it("handles an empty batch", () => {
    const msg = decodeEvent(viewFrame(frameOf(MsgType.Event, 0n, encodeEventBody(0n, 1n, []))));
    expect(msg.count).toBe(0);
    expect(msg.payloads.byteLength).toBe(0);
  });
});

describe("§3.7 — MetricSample", () => {
  it("decodes samples and their visibility tag", () => {
    const body = encodeMetricSampleBody(9_000_000_000n, 1_000_000_000n, [
      { value: 0.9712, strMetric: 20, dimKey: 0, nodeId: SENTINEL_U32, count: 4821, agg: 5, visibility: 1, provId: 3 },
      { value: 137.5, strMetric: 21, dimKey: 7, nodeId: 42, count: 10, agg: 3, visibility: 4, provId: 0 },
    ]);
    expect(body.byteLength).toBe(32 + 32 * 2);
    const msg = decodeMetricSample(viewFrame(frameOf(MsgType.MetricSample, 7n, body)));
    expect(msg.simTimeNs).toBe(9_000_000_000n);
    expect(msg.binWidthNs).toBe(1_000_000_000n);
    expect(msg.recordSize).toBe(32);
    expect(msg.sampleCount).toBe(2);
    const s0 = msg.sample(0);
    expect(s0.value).toBeCloseTo(0.9712, 12);
    expect(s0.nodeId).toBe(SENTINEL_U32);
    expect(s0.agg).toBe(5); // ratio
    expect(s0.visibility).toBe(1); // NODE
    expect(msg.sample(1).nodeId).toBe(42);
    expect(msg.samples()).toHaveLength(2);
  });
});

describe("§3.8 — Provenance", () => {
  it("decodes entries, the dimension dictionary and the symbol-table extension", () => {
    const body = encodeProvenanceBody(
      1_000_000_000n,
      [
        { provId: 1, strModelId: 17, strModelVersion: 18, strParamSetId: 19, strCardUrl: 20, family: 3, subjectKind: 4 },
        { provId: 2, strModelId: 21, strModelVersion: 18, strParamSetId: 22, strCardUrl: 23, family: 1, subjectKind: 2 },
      ],
      [{ dimKey: 7, strDims: 24 }],
      ["radio/propagation/log-distance-shadowing", "1.2.0", "b3:9f2c1e", "/cards/prop.html", "ma/pipeline/simple", "b3:aa11", "/cards/ma.html", "dist_bin=75,rat=dsrc"],
      ProvenanceFlags.FINAL,
    );
    const msg = decodeProvenance(viewFrame(frameOf(MsgType.Provenance, 12n, body)));
    expect(msg.simTimeNs).toBe(1_000_000_000n);
    expect(msg.flags).toBe(ProvenanceFlags.FINAL);
    expect(msg.entries.count).toBe(2);
    expect(Array.from(msg.entries.provId)).toEqual([1, 2]);
    expect(Array.from(msg.entries.family)).toEqual([3, 1]);
    expect(Array.from(msg.entries.subjectKind)).toEqual([4, 2]);
    expect(msg.dims.count).toBe(1);
    expect(Array.from(msg.dims.dimKey)).toEqual([7]);
    expect(msg.stringExtension[0]).toBe("radio/propagation/log-distance-shadowing");
    expect(msg.stringExtension[7]).toBe("dist_bin=75,rat=dsrc");

    // The connection table appends them, so §3.8's "first entry takes the current table size" holds.
    const table = new StringTable();
    table.reset(new Array<string>(17).fill("").map((_, i) => (i === 0 ? "" : `s${i}`)));
    expect(table.append(msg.stringExtension)).toBe(17);
    expect(table.get(17)).toBe("radio/propagation/log-distance-shadowing");
    expect(table.get(24)).toBe("dist_bin=75,rat=dsrc");
  });
});

describe("§3.9–§3.11 — WorldChunk, Error, Bye", () => {
  it("decodes a WorldChunk", () => {
    const hash = new Uint8Array(32).fill(7);
    const payload = new Uint8Array(1000).fill(3);
    const msg = decodeWorldChunk(
      viewFrame(frameOf(MsgType.WorldChunk, 0n, encodeWorldChunkBody({
        worldHash: hash, totalBytes: 2000n, chunkIndex: 0, chunkCount: 2, format: 0, payload,
      }), 0x0010)),
    );
    expect(msg.chunkIndex).toBe(0);
    expect(msg.chunkCount).toBe(2);
    expect(msg.totalBytes).toBe(2000n);
    expect(msg.format).toBe(0);
    expect(msg.payload.byteLength).toBe(1000);
    expect(msg.continued).toBe(true);
    expect(Array.from(msg.worldHash.subarray(0, 4))).toEqual([7, 7, 7, 7]);
  });

  it("decodes an Error, resolving its string extension", () => {
    const table = new StringTable();
    table.reset(["", "a", "b"]);
    const body = encodeErrorBody({
      simTimeNs: 42n, code: RpcErrorCode.UNSUPPORTED_VERSION, fatal: true,
      message: "server speaks major 2 only", detail: '{"server":"2.0","supported_major":[2]}',
      firstStringId: table.size,
    });
    const msg = decodeError(viewFrame(frameOf(MsgType.Error, 100n, body)), table);
    expect(msg.code).toBe(-32050);
    expect(msg.fatal).toBe(true);
    expect(msg.message).toBe("server speaks major 2 only");
    expect(msg.detail).toBe('{"server":"2.0","supported_major":[2]}');
  });

  it("decodes a Bye", () => {
    const table = new StringTable();
    const body = encodeByeBody({
      simTimeNs: 600_000_000_000n, canonicalFrames: 6001n, reason: ByeReason.RUN_COMPLETE,
      detail: "run finished", firstStringId: table.size,
    });
    const msg = decodeBye(viewFrame(frameOf(MsgType.Bye, 6001n, body)), table);
    expect(msg.reason).toBe(ByeReason.RUN_COMPLETE);
    expect(msg.canonicalFrames).toBe(6001n);
    expect(msg.detail).toBe("run finished");
  });

  it("the dispatcher routes every message type of §2.4", () => {
    const kinds = [
      [MsgType.Telemetry, encodeTelemetryBody(0n, 0n, []), "telemetry"],
      [MsgType.Event, encodeEventBody(0n, 0n, []), "event"],
      [MsgType.MetricSample, encodeMetricSampleBody(0n, 0n, []), "metric"],
      [MsgType.Provenance, encodeProvenanceBody(0n, []), "provenance"],
      [MsgType.Bye, encodeByeBody({ simTimeNs: 0n, canonicalFrames: 0n, reason: 2, firstStringId: 0 }), "bye"],
    ] as const;
    for (const [type, body, kind] of kinds) {
      expect(decodeMessage(frameOf(type, 0n, body)).kind).toBe(kind);
    }
  });
});
