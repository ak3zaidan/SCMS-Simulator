/** §2.1–§2.4 framing, and the conformance items of §10.1. */

import { describe, expect, it } from "vitest";

import {
  CANONICAL_FLAG_MASK,
  FRAME_HEADER_BYTES,
  FrameFlags,
  MsgType,
  ProtocolError,
  TRANSPORT_FLAG_MASK,
  VWP_MAGIC,
  VWP_SUBPROTOCOL,
  VWP_VERSION,
  canonicalFlags,
  decodeKeyframe,
  decodeMessage,
  encodeFrame,
  isCanonicalMsgType,
  msgTypeName,
  parseFrameHeader,
  serialiseFrameHeader,
  viewFrame,
} from "../src/index.js";
import { SPEC_KEYFRAME_HEX, hexToArrayBuffer } from "./vectors/spec-vectors.js";

const goodFrame = (): ArrayBuffer => hexToArrayBuffer(SPEC_KEYFRAME_HEX);

describe("§2.1 — the 24-byte header", () => {
  it("round-trips through serialise and parse", () => {
    const body = new Uint8Array(64);
    const frame = encodeFrame({ msgType: MsgType.Keyframe, seq: 123456789012345n, flags: FrameFlags.RESYNC }, body);
    expect(frame.byteLength).toBe(FRAME_HEADER_BYTES + 64);
    const h = parseFrameHeader(frame);
    expect(h.magic).toBe(VWP_MAGIC);
    expect(h.version).toBe(VWP_VERSION);
    expect(h.msgType).toBe(MsgType.Keyframe);
    expect(h.bodyLen).toBe(64);
    expect(h.flags).toBe(FrameFlags.RESYNC);
    expect(h.reserved).toBe(0); // §10.1 F6 — reserved bytes are written as zero
    expect(h.seq).toBe(123456789012345n);
  });

  it("writes little-endian magic whose bytes are V W P 1", () => {
    const buf = new ArrayBuffer(FRAME_HEADER_BYTES);
    serialiseFrameHeader(buf, 0, { msgType: MsgType.Hello, bodyLen: 0, seq: 0n });
    expect(Array.from(new Uint8Array(buf, 0, 4))).toEqual([0x56, 0x57, 0x50, 0x31]);
  });

  it("§10.1 F1 — rejects a frame whose magic is wrong, with close code 1002", () => {
    const frame = goodFrame();
    new DataView(frame).setUint32(0, 0xdeadbeef, true);
    let caught: ProtocolError | null = null;
    try {
      parseFrameHeader(frame);
    } catch (err) {
      caught = err as ProtocolError;
    }
    expect(caught).toBeInstanceOf(ProtocolError);
    expect(caught?.code).toBe("bad_magic");
    expect(caught?.closeCode).toBe(1002);
    expect(caught?.detail.expected).toBe(VWP_MAGIC);
    expect(caught?.detail.actual).toBe(0xdeadbeef);
  });

  it("rejects a big-endian magic, i.e. a byte-order mistake on the writer's side", () => {
    const frame = goodFrame();
    new DataView(frame).setUint32(0, VWP_MAGIC, false);
    expect(() => parseFrameHeader(frame)).toThrow(ProtocolError);
  });

  it("§8.2 / §10.9 N3 — rejects an unsupported major version, with close code 4406", () => {
    for (const version of [0, 2, 3, 0xffff]) {
      const frame = goodFrame();
      new DataView(frame).setUint16(4, version, true);
      let caught: ProtocolError | null = null;
      try {
        parseFrameHeader(frame);
      } catch (err) {
        caught = err as ProtocolError;
      }
      expect(caught?.code).toBe("bad_version");
      expect(caught?.closeCode).toBe(4406);
      expect(caught?.detail.actual).toBe(version);
    }
  });

  it("rejects a truncated header and a body shorter than body_len", () => {
    expect(() => parseFrameHeader(new ArrayBuffer(23))).toThrow(ProtocolError);
    const frame = goodFrame();
    new DataView(frame).setUint32(8, 10_000, true); // body_len far beyond the frame
    expect(() => viewFrame(frame)).toThrow(/body_len/);
  });

  it("§10.1 F9 — ignores unknown flag bits instead of failing", () => {
    const frame = goodFrame();
    new DataView(frame).setUint16(12, 0xff00, true); // bits 8..15 are reserved
    const msg = decodeMessage(frame);
    expect(msg.kind).toBe("keyframe");
    expect(msg.header.flags).toBe(0xff00);
  });

  it("§10.1 F2 — an unknown msg_type decodes to `unknown` and keeps the body", () => {
    const frame = goodFrame();
    new DataView(frame).setUint16(6, 0x00aa, true);
    const msg = decodeMessage(frame);
    expect(msg.kind).toBe("unknown");
    if (msg.kind === "unknown") expect(msg.body.byteLength).toBe(156);
    expect(msgTypeName(0x00aa)).toBe("Unknown(0x00aa)");
  });

  it("§2.6 — a compressed body without a decompressor is a typed error, not a crash", () => {
    const frame = goodFrame();
    new DataView(frame).setUint16(12, FrameFlags.COMPRESSED, true);
    let caught: ProtocolError | null = null;
    try {
      viewFrame(frame);
    } catch (err) {
      caught = err as ProtocolError;
    }
    expect(caught?.code).toBe("compressed_unsupported");
  });

  it("§2.6 — a supplied decompressor is used and its length is checked", () => {
    const original = goodFrame();
    const body = new Uint8Array(original, FRAME_HEADER_BYTES, 156);
    const fake = encodeFrame({ msgType: MsgType.Keyframe, seq: 10n, flags: FrameFlags.COMPRESSED, bodyLen: 156 }, new Uint8Array([1, 2, 3]));
    const msg = decodeMessage(fake, { decompress: () => body.slice() });
    expect(msg.kind).toBe("keyframe");
    if (msg.kind === "keyframe") expect(Array.from(msg.actors.xMm)).toEqual([512345, 480000, 503000]);
    expect(() => decodeMessage(fake, { decompress: () => new Uint8Array(4) })).toThrow(/body_len/);
  });

  it("§2.3 — the canonical/transport flag split is exactly what §7.2 depends on", () => {
    expect(CANONICAL_FLAG_MASK).toBe(0x000c);
    expect(TRANSPORT_FLAG_MASK).toBe(0x0033);
    expect(FrameFlags.NODE_ONLY | FrameFlags.END_OF_RUN).toBe(CANONICAL_FLAG_MASK);
    expect(FrameFlags.COMPRESSED | FrameFlags.RESYNC | FrameFlags.CONTINUED | FrameFlags.SEEK_RESULT).toBe(TRANSPORT_FLAG_MASK);
    expect(canonicalFlags(FrameFlags.RESYNC | FrameFlags.NODE_ONLY)).toBe(FrameFlags.NODE_ONLY);
  });

  it("§2.4 — canonical frames are 0x0002..0x0008; Hello, Error and Bye are not", () => {
    expect(isCanonicalMsgType(MsgType.Hello)).toBe(false);
    expect(isCanonicalMsgType(MsgType.Keyframe)).toBe(true);
    expect(isCanonicalMsgType(MsgType.WorldChunk)).toBe(true);
    expect(isCanonicalMsgType(MsgType.Error)).toBe(false);
    expect(isCanonicalMsgType(MsgType.Bye)).toBe(false);
    expect(MsgType.Error).toBe(0x00fe);
    expect(MsgType.Bye).toBe(0x00ff);
  });

  it("§1.1 — the subprotocol token carries the major version", () => {
    expect(VWP_SUBPROTOCOL).toBe("vwp.v1");
  });

  it("§2.2 — a misaligned body offset is refused rather than silently mis-viewed", () => {
    // A body at an odd offset cannot carry aligned i32/i16 columns; the decoder must say so
    // instead of constructing a wrong view or throwing a raw RangeError.
    const frame = goodFrame();
    const shifted = new ArrayBuffer(frame.byteLength + 2);
    new Uint8Array(shifted, 2).set(new Uint8Array(frame));
    let caught: ProtocolError | null = null;
    try {
      decodeKeyframe({ header: parseFrameHeader(shifted, 2), buffer: shifted, bodyOffset: 26, bodyLen: 156 });
    } catch (err) {
      caught = err as ProtocolError;
    }
    expect(caught).toBeInstanceOf(ProtocolError);
    expect(caught?.code).toBe("misaligned");
  });
});
