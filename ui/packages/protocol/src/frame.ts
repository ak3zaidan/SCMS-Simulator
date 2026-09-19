/**
 * VWP v1 binary framing — docs/protocol/vwp-v1.md §2.
 *
 * Every binary WebSocket message is `header (24 B) || body`. All multi-byte scalars are
 * little-endian (§0). Offsets in this file are transcribed from the §2.1 table and are
 * exported so a reviewer can diff them against the specification line by line.
 */

/**
 * §2.1: `magic` is the `u32` `0x31505756`; its little-endian wire bytes are
 * `56 57 50 31` = ASCII `V W P 1`.
 */
export const VWP_MAGIC = 0x31505756;

/** §2.1 / §8.1: protocol major version carried in every frame header. */
export const VWP_VERSION = 1;

/** §2.1: the frame header is 24 bytes, fixed; the body starts at frame offset 24. */
export const FRAME_HEADER_BYTES = 24;

/** §1.1: the WebSocket subprotocol token, which carries the major version. */
export const VWP_SUBPROTOCOL = "vwp.v1";

/** §2.1 — byte offsets of the frame-header fields, relative to the start of the frame. */
export const FRAME_HEADER_OFFSETS = {
  /** `u32` @0 */ magic: 0,
  /** `u16` @4 */ version: 4,
  /** `u16` @6 */ msgType: 6,
  /** `u32` @8 — length of the **uncompressed** body */ bodyLen: 8,
  /** `u16` @12 */ flags: 12,
  /** `u16` @14 — MUST be 0 */ reserved: 14,
  /** `u64` @16 */ seq: 16,
} as const;

/** §2.4 — message type ids. */
export const MsgType = {
  Hello: 0x0001,
  Keyframe: 0x0002,
  Delta: 0x0003,
  Telemetry: 0x0004,
  Event: 0x0005,
  MetricSample: 0x0006,
  Provenance: 0x0007,
  WorldChunk: 0x0008,
  Error: 0x00fe,
  Bye: 0x00ff,
} as const;
export type MsgType = (typeof MsgType)[keyof typeof MsgType];

/** §2.3 — frame flags. */
export const FrameFlags = {
  /** bit 0 — body is a zstd frame (transport) */ COMPRESSED: 0x0001,
  /** bit 1 — discard interpolation state; this Keyframe re-seeds it (transport) */ RESYNC: 0x0002,
  /** bit 2 — produced under the `node` profile (canonical) */ NODE_ONLY: 0x0004,
  /** bit 3 — last canonical frame of the run (canonical) */ END_OF_RUN: 0x0008,
  /** bit 4 — one of several frames carrying one logical unit (transport) */ CONTINUED: 0x0010,
  /** bit 5 — this Keyframe answers a `run.seek` (transport) */ SEEK_RESULT: 0x0020,
} as const;
export type FrameFlag = (typeof FrameFlags)[keyof typeof FrameFlags];

/** §2.3 — the canonical/transport split that §7.2 byte-identity depends on. */
export const CANONICAL_FLAG_MASK = 0x000c;
/** §2.3 */
export const TRANSPORT_FLAG_MASK = 0x0033;

/** §0 — "absent" sentinels. */
export const SENTINEL_U32 = 0xffffffff;
/** §0 */
export const SENTINEL_U16 = 0xffff;
/** §0 */
export const SENTINEL_U8 = 0xff;
/** §0 — `u64::MAX` for times. */
export const SENTINEL_U64 = 0xffffffffffffffffn;

/** Machine-readable reason for a {@link ProtocolError}. */
export type ProtocolErrorCode =
  | "bad_magic"
  | "bad_version"
  | "truncated"
  | "bad_length"
  | "bad_offset"
  | "misaligned"
  | "unknown_msg_type"
  | "compressed_unsupported"
  | "bad_state"
  | "hash_mismatch";

/** Extra context attached to a {@link ProtocolError}; never `any`. */
export interface ProtocolErrorDetail {
  readonly expected?: number | bigint | string;
  readonly actual?: number | bigint | string;
  readonly offset?: number;
  readonly msgType?: number;
  readonly field?: string;
}

/**
 * A typed protocol violation. `closeCode` is the WebSocket close code Appendix A prescribes
 * for this class of failure (1002 for a malformed frame, 4406 for an unsupported version).
 */
export class ProtocolError extends Error {
  override readonly name = "ProtocolError";
  readonly code: ProtocolErrorCode;
  readonly detail: ProtocolErrorDetail;
  readonly closeCode: number;

  constructor(code: ProtocolErrorCode, message: string, detail: ProtocolErrorDetail = {}) {
    super(message);
    this.code = code;
    this.detail = detail;
    this.closeCode = code === "bad_version" ? 4406 : 1002;
  }
}

/**
 * §10.1 F4 / §2.2 — is this host little-endian?
 *
 * Every `DataView` access in this package passes `littleEndian = true`, so the *parsed* fields are
 * correct on any host. The zero-copy path of §2.2 is not: `new Int32Array(buffer, off, n)` uses the
 * **host** byte order, so on a big-endian host every pose column, signal id, lane id and telemetry
 * raw region would be byte-swapped while the prefixes stayed correct — silent corruption rather
 * than a rejection. F4 is a statement about the bytes on the wire, not about view construction.
 * {@link assertLittleEndianHost} turns that into a clean, typed failure.
 */
export function isLittleEndianHost(): boolean {
  return new Uint8Array(new Uint32Array([1]).buffer)[0] === 1;
}

let littleEndianChecked = false;

/**
 * Throw once if the host is big-endian, before any zero-copy typed-array view is built (§2.2).
 * Cheap: the check runs at most once per process.
 */
export function assertLittleEndianHost(): void {
  if (littleEndianChecked) return;
  if (!isLittleEndianHost()) {
    throw new ProtocolError(
      "bad_state",
      "this host is big-endian; VWP's zero-copy typed-array views (§2.2) require a little-endian host (§10.1 F4 describes the bytes on the wire, not view construction)",
      { field: "host_endianness" },
    );
  }
  littleEndianChecked = true;
}

/** §2.1 — a decoded frame header. */
export interface FrameHeader {
  readonly magic: number;
  readonly version: number;
  readonly msgType: number;
  readonly bodyLen: number;
  readonly flags: number;
  readonly reserved: number;
  readonly seq: bigint;
}

/** What {@link serialiseFrameHeader} and {@link encodeFrame} need; `reserved` is always written as 0. */
export interface FrameHeaderInit {
  readonly msgType: number;
  readonly bodyLen: number;
  readonly seq: bigint;
  readonly flags?: number;
  readonly version?: number;
}

function requireBytes(available: number, needed: number, what: string): void {
  if (available < needed) {
    throw new ProtocolError("truncated", `frame truncated: ${what} needs ${needed} bytes, have ${available}`, {
      expected: needed,
      actual: available,
      field: what,
    });
  }
}

/**
 * Parse and validate a 24-byte frame header (§2.1).
 *
 * Throws {@link ProtocolError} `bad_magic` when `magic !== 0x31505756` (the reader MUST reject
 * such a frame and close 1002) and `bad_version` when the major version is not one this client
 * speaks. §8.2 lets a client accept majors N and N−1; v1 is the first major, so only 1 is accepted.
 */
export function parseFrameHeader(frame: ArrayBuffer, byteOffset = 0): FrameHeader {
  requireBytes(frame.byteLength - byteOffset, FRAME_HEADER_BYTES, "header");
  const dv = new DataView(frame, byteOffset, FRAME_HEADER_BYTES);
  const o = FRAME_HEADER_OFFSETS;
  const magic = dv.getUint32(o.magic, true);
  if (magic !== VWP_MAGIC) {
    throw new ProtocolError("bad_magic", `bad magic 0x${magic.toString(16).padStart(8, "0")}, expected 0x31505756`, {
      expected: VWP_MAGIC,
      actual: magic,
      offset: byteOffset,
    });
  }
  const version = dv.getUint16(o.version, true);
  if (version !== VWP_VERSION) {
    throw new ProtocolError("bad_version", `unsupported protocol major version ${version}, this client speaks ${VWP_VERSION}`, {
      expected: VWP_VERSION,
      actual: version,
      offset: byteOffset,
    });
  }
  return {
    magic,
    version,
    msgType: dv.getUint16(o.msgType, true),
    bodyLen: dv.getUint32(o.bodyLen, true),
    flags: dv.getUint16(o.flags, true),
    reserved: dv.getUint16(o.reserved, true),
    seq: dv.getBigUint64(o.seq, true),
  };
}

/** Write a 24-byte frame header into `target` at `byteOffset` (§2.1). `reserved` is written as 0. */
export function serialiseFrameHeader(target: ArrayBuffer, byteOffset: number, init: FrameHeaderInit): void {
  requireBytes(target.byteLength - byteOffset, FRAME_HEADER_BYTES, "header");
  const dv = new DataView(target, byteOffset, FRAME_HEADER_BYTES);
  const o = FRAME_HEADER_OFFSETS;
  dv.setUint32(o.magic, VWP_MAGIC, true);
  dv.setUint16(o.version, init.version ?? VWP_VERSION, true);
  dv.setUint16(o.msgType, init.msgType, true);
  dv.setUint32(o.bodyLen, init.bodyLen, true);
  dv.setUint16(o.flags, init.flags ?? 0, true);
  dv.setUint16(o.reserved, 0, true);
  dv.setBigUint64(o.seq, init.seq, true);
}

/** Build a complete frame (`header || body`) in a fresh, 8-aligned ArrayBuffer. */
export function encodeFrame(init: Omit<FrameHeaderInit, "bodyLen"> & { bodyLen?: number }, body: Uint8Array): ArrayBuffer {
  const out = new ArrayBuffer(FRAME_HEADER_BYTES + body.byteLength);
  serialiseFrameHeader(out, 0, { ...init, bodyLen: init.bodyLen ?? body.byteLength });
  new Uint8Array(out, FRAME_HEADER_BYTES).set(body);
  return out;
}

/** §2.4 — canonical frames (`0x0002`–`0x0008`) consume a `seq`; `Hello`/`Error`/`Bye` do not. */
export function isCanonicalMsgType(msgType: number): boolean {
  return msgType >= MsgType.Keyframe && msgType <= MsgType.WorldChunk;
}

/** §7.2 — the canonical view of a frame's flags, with transport bits masked off. */
export function canonicalFlags(flags: number): number {
  return flags & CANONICAL_FLAG_MASK;
}

/** Convenience: is a flag bit set? */
export function hasFlag(flags: number, flag: number): boolean {
  return (flags & flag) !== 0;
}

/** Human-readable name for a message type, for logs and errors. */
export function msgTypeName(msgType: number): string {
  for (const [name, id] of Object.entries(MsgType)) {
    if (id === msgType) return name;
  }
  return `Unknown(0x${msgType.toString(16).padStart(4, "0")})`;
}
