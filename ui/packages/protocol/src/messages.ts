/**
 * VWP v1 message decoders — docs/protocol/vwp-v1.md §3.
 *
 * Every decoder is **zero-parse**: it builds typed-array views directly over the received
 * `ArrayBuffer` at the offsets §3 documents (§2.2 guarantees each scalar array is aligned to its
 * element size relative to the body, and the 24-byte header keeps that alignment frame-relative).
 * Nothing is copied element by element. Strings are the one exception — they are UTF-8 blobs and
 * are decoded on demand by {@link StringTable}.
 *
 * Every offset below is a named constant transcribed from a §3 table so it can be diffed against
 * the specification.
 */

import {
  FRAME_HEADER_BYTES,
  MsgType,
  ProtocolError,
  FrameFlags,
  assertLittleEndianHost,
  parseFrameHeader,
  type FrameHeader,
} from "./frame.js";

// ---------------------------------------------------------------------------
// Frame view: header + body location, with optional zstd decompression (§2.6)
// ---------------------------------------------------------------------------

/**
 * A zstd (RFC 8878) decompressor. The protocol package deliberately bundles none: a client that
 * cannot decompress connects with `?compress=none` (§1.1). Inject `fzstd` or a WASM build here.
 */
export type ZstdDecompress = (compressed: Uint8Array, uncompressedLen: number) => Uint8Array;

/** Options accepted by {@link viewFrame} and {@link decodeMessage}. */
export interface DecodeOptions {
  readonly decompress?: ZstdDecompress;
}

/**
 * A validated frame: its header plus the location of its **uncompressed** body.
 *
 * For an uncompressed frame `buffer` is the received frame itself and `bodyOffset` is 24, so all
 * views are built in place with no copy. For a compressed frame the body has been decompressed
 * into a fresh buffer and `bodyOffset` is 0 — §2.2 notes both cases satisfy the same alignment rule.
 */
export interface FrameView {
  readonly header: FrameHeader;
  readonly buffer: ArrayBuffer;
  readonly bodyOffset: number;
  readonly bodyLen: number;
}

/** Validate a frame header and locate (decompressing if needed) its body. */
export function viewFrame(frame: ArrayBuffer, options: DecodeOptions = {}): FrameView {
  // §2.2 — everything below builds typed-array views in place, which use the host byte order.
  assertLittleEndianHost();
  const header = parseFrameHeader(frame, 0);
  const rest = frame.byteLength - FRAME_HEADER_BYTES;
  if ((header.flags & FrameFlags.COMPRESSED) !== 0) {
    // §1.3 rule 1 / §10.1 F7 — `Hello` is never compressed, whatever the flags claim.
    if (header.msgType === MsgType.Hello) {
      throw new ProtocolError("bad_state", "Hello carries FLAG_COMPRESSED, but §1.3 rule 1 says Hello is never compressed", {
        msgType: header.msgType,
        field: "flags",
      });
    }
    if (!options.decompress) {
      throw new ProtocolError(
        "compressed_unsupported",
        "frame has FLAG_COMPRESSED but no zstd decompressor was supplied (connect with compress=none, §1.1)",
        { msgType: header.msgType },
      );
    }
    const out = options.decompress(new Uint8Array(frame, FRAME_HEADER_BYTES, rest), header.bodyLen);
    if (out.byteLength !== header.bodyLen) {
      throw new ProtocolError("bad_length", `decompressed body is ${out.byteLength} bytes, body_len says ${header.bodyLen}`, {
        expected: header.bodyLen,
        actual: out.byteLength,
      });
    }
    // Re-home into an ArrayBuffer whose offset 0 is the body start, per §2.2.
    const owned = out.byteOffset === 0 && out.byteLength === out.buffer.byteLength ? (out.buffer as ArrayBuffer) : (out.slice().buffer as ArrayBuffer);
    return { header, buffer: owned, bodyOffset: 0, bodyLen: header.bodyLen };
  }
  if (rest < header.bodyLen) {
    throw new ProtocolError("bad_length", `frame carries ${rest} body bytes, body_len says ${header.bodyLen}`, {
      expected: header.bodyLen,
      actual: rest,
    });
  }
  return { header, buffer: frame, bodyOffset: FRAME_HEADER_BYTES, bodyLen: header.bodyLen };
}

// ---------------------------------------------------------------------------
// Section helpers — bounds and alignment checks (§2.2)
// ---------------------------------------------------------------------------

function bodyView(v: FrameView): DataView {
  return new DataView(v.buffer, v.bodyOffset, v.bodyLen);
}

function checkSection(v: FrameView, off: number, bytes: number, what: string): number {
  if (off < 0 || bytes < 0 || off + bytes > v.bodyLen) {
    throw new ProtocolError("bad_offset", `${what}: section [${off}, ${off + bytes}) is outside the ${v.bodyLen}-byte body`, {
      offset: off,
      field: what,
      expected: v.bodyLen,
    });
  }
  return v.bodyOffset + off;
}

function align(abs: number, elem: number, what: string): void {
  if (abs % elem !== 0) {
    throw new ProtocolError("misaligned", `${what}: byte offset ${abs} is not a multiple of ${elem} (§2.2)`, {
      offset: abs,
      field: what,
      expected: elem,
    });
  }
}

function u8s(v: FrameView, off: number, n: number, what: string): Uint8Array {
  return new Uint8Array(v.buffer, checkSection(v, off, n, what), n);
}
function u16s(v: FrameView, off: number, n: number, what: string): Uint16Array {
  const abs = checkSection(v, off, n * 2, what);
  align(abs, 2, what);
  return new Uint16Array(v.buffer, abs, n);
}
function i16s(v: FrameView, off: number, n: number, what: string): Int16Array {
  const abs = checkSection(v, off, n * 2, what);
  align(abs, 2, what);
  return new Int16Array(v.buffer, abs, n);
}
function u32s(v: FrameView, off: number, n: number, what: string): Uint32Array {
  const abs = checkSection(v, off, n * 4, what);
  align(abs, 4, what);
  return new Uint32Array(v.buffer, abs, n);
}
function i32s(v: FrameView, off: number, n: number, what: string): Int32Array {
  const abs = checkSection(v, off, n * 4, what);
  align(abs, 4, what);
  return new Int32Array(v.buffer, abs, n);
}
function f32s(v: FrameView, off: number, n: number, what: string): Float32Array {
  const abs = checkSection(v, off, n * 4, what);
  align(abs, 4, what);
  return new Float32Array(v.buffer, abs, n);
}
function u64s(v: FrameView, off: number, n: number, what: string): BigUint64Array {
  const abs = checkSection(v, off, n * 8, what);
  align(abs, 8, what);
  return new BigUint64Array(v.buffer, abs, n);
}

// ---------------------------------------------------------------------------
// §2.5 — the symbol table
// ---------------------------------------------------------------------------

/** §2.5 — byte offsets inside a serialised `StrTable`. */
export const STRTABLE_OFFSETS = {
  /** `u32` @0 — number of strings */ n: 0,
  /** `u32` @4 — UTF-8 bytes excluding padding */ blobBytes: 4,
  /** `u32[n+1]` @8 */ offsets: 8,
} as const;

/** A `StrTable` located in a buffer, not yet decoded to JS strings. */
export interface StrTableView {
  readonly count: number;
  readonly blobBytes: number;
  readonly offsets: Uint32Array;
  readonly blob: Uint8Array;
  /** Total serialised size: `8 + 4(n+1) + ceil4(blob_bytes)`. */
  readonly byteLength: number;
}

const ceil4 = (n: number): number => (n + 3) & ~3;

/**
 * Decode a `StrTable` (§2.5) located at `byteOffset` in `buffer`.
 *
 * `endOffset` bounds the table: pass the end of the **enclosing body** (`bodyOffset + body_len`) so
 * a table cannot read into trailing bytes beyond `body_len` (§2.1 lets a frame carry them and §8
 * requires readers to ignore them). It defaults to the end of the buffer.
 *
 * §2.5 states three invariants on `offsets`: `offsets[0] = 0`, `offsets[n] = blob_bytes` and
 * non-decreasing. The first two are asserted here (the third is asserted per entry by
 * {@link strTableStrings}), because a monotonic table with `offsets[n] = blob_bytes − 1` would
 * otherwise silently truncate the last string, and `offsets[0] > 0` would silently drop the first
 * bytes of the blob.
 */
export function decodeStrTable(buffer: ArrayBuffer, byteOffset: number, endOffset?: number): StrTableView {
  const end = Math.min(endOffset ?? buffer.byteLength, buffer.byteLength);
  if (byteOffset % 4 !== 0) {
    throw new ProtocolError("misaligned", `StrTable at ${byteOffset} is not 4-aligned (§2.5)`, { offset: byteOffset });
  }
  if (byteOffset + 8 > end) {
    throw new ProtocolError("truncated", "StrTable header runs past the end of the body", { offset: byteOffset });
  }
  const dv = new DataView(buffer, byteOffset);
  const count = dv.getUint32(STRTABLE_OFFSETS.n, true);
  const blobBytes = dv.getUint32(STRTABLE_OFFSETS.blobBytes, true);
  const byteLength = 8 + 4 * (count + 1) + ceil4(blobBytes);
  if (byteOffset + byteLength > end) {
    throw new ProtocolError("truncated", `StrTable of ${byteLength} bytes runs past the end of the body`, {
      offset: byteOffset,
      expected: byteLength,
    });
  }
  const offsets = new Uint32Array(buffer, byteOffset + STRTABLE_OFFSETS.offsets, count + 1);
  if (offsets[0] !== 0) {
    throw new ProtocolError("bad_offset", `StrTable offsets[0] is ${offsets[0]}, §2.5 requires 0`, {
      offset: byteOffset,
      expected: 0,
      actual: offsets[0],
      field: "StrTable.offsets[0]",
    });
  }
  if (offsets[count] !== blobBytes) {
    throw new ProtocolError("bad_offset", `StrTable offsets[${count}] is ${offsets[count]}, §2.5 requires blob_bytes (${blobBytes})`, {
      offset: byteOffset,
      expected: blobBytes,
      actual: offsets[count],
      field: `StrTable.offsets[${count}]`,
    });
  }
  const blob = new Uint8Array(buffer, byteOffset + 8 + 4 * (count + 1), blobBytes);
  return { count, blobBytes, offsets, blob, byteLength };
}

const UTF8 = new TextDecoder("utf-8", { fatal: false });

/** Decode every string of a {@link StrTableView} (id order, id 0 first). */
export function strTableStrings(view: StrTableView): string[] {
  const out: string[] = new Array<string>(view.count);
  for (let i = 0; i < view.count; i++) {
    const a = view.offsets[i];
    const b = view.offsets[i + 1];
    if (b < a || b > view.blobBytes) {
      throw new ProtocolError("bad_offset", `StrTable offsets[${i}]=${a}..${b} are not non-decreasing within ${view.blobBytes} blob bytes`, {
        offset: i,
      });
    }
    out[i] = UTF8.decode(view.blob.subarray(a, b));
  }
  return out;
}

/**
 * The per-connection, append-only symbol table of §2.5.
 *
 * Id `0` is always the empty string. `Hello` establishes ids `0..n-1`; each `Provenance` frame may
 * append, its first entry taking the id equal to the current table size. A non-resumed `Hello`
 * resets it — call {@link reset}.
 */
export class StringTable {
  #strings: string[] = [""];

  /** Number of ids currently defined. */
  get size(): number {
    return this.#strings.length;
  }

  /** Resolve a string id. Unknown ids resolve to `""` rather than throwing, per §0's sentinel style. */
  get(id: number): string {
    return this.#strings[id] ?? "";
  }

  /** Replace the whole table (a non-resumed `Hello`, §1.4 case 2). */
  reset(strings: readonly string[] = [""]): void {
    this.#strings = strings.length > 0 ? [...strings] : [""];
    if (this.#strings[0] !== "") this.#strings[0] = "";
  }

  /** Append an extension table (§3.8); returns the id the first appended entry took. */
  append(strings: readonly string[]): number {
    const first = this.#strings.length;
    this.#strings.push(...strings);
    return first;
  }

  /** A snapshot copy, id order. */
  toArray(): string[] {
    return [...this.#strings];
  }
}

// ---------------------------------------------------------------------------
// §3.1 — Hello (0x0001)
// ---------------------------------------------------------------------------

/** §3.1.1 — the 256-byte `Hello` prefix. */
export const HELLO_PREFIX_BYTES = 256;

/** §3.1.1 — body-relative byte offsets of the `Hello` prefix fields. */
export const HELLO_OFFSETS = {
  /** `u16` @0 */ versionMajor: 0,
  /** `u16` @2 */ versionMinor: 2,
  /** `u32` @4 */ helloFlags: 4,
  /** `u8[16]` @8 — UUIDv7, raw big-endian bytes (RFC 9562) */ runId: 8,
  /** `u8[32]` @24 */ scenarioHash: 24,
  /** `u8[32]` @56 */ worldHash: 56,
  /** `i64` @88 */ t0WallNs: 88,
  /** `u64` @96 */ simDurationNs: 96,
  /** `u64` @104 */ mobilityStepNs: 104,
  /** `u64` @112 */ keyframePeriodNs: 112,
  /** `u64` @120 */ telemetryPeriodNs: 120,
  /** `u64` @128 */ metricPeriodNs: 128,
  /** `u64` @136 */ resumeSeq: 136,
  /** `u64` @144 */ simTimeNs: 144,
  /** `f64` @152 */ originLatDeg: 152,
  /** `f64` @160 */ originLonDeg: 160,
  /** `f64` @168 */ originAltM: 168,
  /** `f64` @176 */ bboxMinXM: 176,
  /** `f64` @184 */ bboxMinYM: 184,
  /** `f64` @192 */ bboxMaxXM: 192,
  /** `f64` @200 */ bboxMaxYM: 200,
  /** `u32` @208 */ actorCapacity: 208,
  /** `u32` @212 */ nodeCount: 212,
  /** `u16` @216 */ classCount: 216,
  /** `u16` @218 */ channelCount: 218,
  /** `u32` @220 */ offNodes: 220,
  /** `u32` @224 */ offClasses: 224,
  /** `u32` @228 */ offChannels: 228,
  /** `u32` @232 */ offWorldRef: 232,
  /** `u32` @236 */ offStrings: 236,
  /** `u32` @240 */ strEngineVersion: 240,
  /** `u32` @244 */ strScenarioName: 244,
  /** `u32` @248 */ strRunLabel: 248,
  /** `u32` @252 */ strSessionToken: 252,
} as const;

/** §3.1.2 — `hello_flags` bits. */
export const HelloFlags = {
  LIVE: 0x0000_0001,
  REPLAY: 0x0000_0002,
  NODE_ONLY: 0x0000_0004,
  WORLD_INLINE: 0x0000_0008,
  PAUSED: 0x0000_0010,
  RESUMED: 0x0000_0020,
  SEEKABLE: 0x0000_0040,
  WRITABLE: 0x0000_0080,
} as const;
export type HelloFlag = (typeof HelloFlags)[keyof typeof HelloFlags];

/** §3.1.3 — `nodes.flags` bits. */
export const NodeFlags = {
  HAS_HSM: 1 << 0,
  /** ground truth */ IS_ATTACKER: 1 << 1,
  IS_BACKEND: 1 << 2,
  HAS_BACKHAUL: 1 << 3,
  HAS_UU: 1 << 4,
} as const;

/** §3.1.3 — node table, one column per field, each of length `node_count`. */
export interface HelloNodeTable {
  readonly count: number;
  readonly nodeId: Uint32Array;
  /** `0xFFFFFFFF` if the node is not mounted on an actor. */ readonly actorId: Uint32Array;
  readonly posXM: Float32Array;
  readonly posYM: Float32Array;
  readonly posZM: Float32Array;
  readonly strLabel: Uint32Array;
  readonly strProfileId: Uint32Array;
  readonly flags: Uint16Array;
  /** `NodeKind`: 0 obu, 1 vru-device, 2 rsu, 3 base-station, 4 router, 5 backend-entity, 6 other. */
  readonly kind: Uint8Array;
  /** `0xFF` if not an actor. */ readonly classIdx: Uint8Array;
}

/** §3.1.4 — actor-class table. */
export interface HelloClassTable {
  readonly count: number;
  readonly strName: Uint32Array;
  readonly lengthM: Float32Array;
  readonly widthM: Float32Array;
  readonly heightM: Float32Array;
  /** `0xRRGGBBAA` stored little-endian. */ readonly colorRgba: Uint32Array;
  readonly reserved16: Uint16Array;
  /** `ActorCategory`: 0 vehicle, 1 vru, 2 infrastructure, 3 other. */ readonly category: Uint8Array;
  readonly reserved8: Uint8Array;
}

/** §3.1.5 — channel table. */
export interface HelloChannelTable {
  readonly count: number;
  readonly strId: Uint32Array;
  readonly channelId: Uint16Array;
  /** `Visibility`: 0 GT, 1 NODE, 2 PUBLIC, 3 MIXED, 4 DERIVED, 5 META. */ readonly visibility: Uint8Array;
  readonly enabled: Uint8Array;
}

/** §3.1.6 — world reference (16 bytes). */
export interface HelloWorldRef {
  /** `WorldRefMode`: 0 HTTP GET by content hash, 1 inline `WorldChunk`, 2 client already has it. */
  readonly mode: number;
  /** 0 `vwp-world/1` binary (`.vwb`), 1 JSON. */ readonly format: number;
  readonly payloadBytes: number;
  readonly strUrl: number;
}

/** §3.1.6 — byte offsets within the 16-byte world reference. */
export const WORLD_REF_OFFSETS = {
  /** `u8` @0 */ mode: 0,
  /** `u8` @1 */ format: 1,
  /** `u16` @2 */ reserved: 2,
  /** `u32` @4 */ payloadBytes: 4,
  /** `u32` @8 */ strUrl: 8,
  /** `u32` @12 */ reserved32: 12,
} as const;

/** §3.1 — a decoded `Hello`. */
export interface HelloMessage {
  readonly kind: "hello";
  readonly header: FrameHeader;
  readonly versionMajor: number;
  readonly versionMinor: number;
  readonly helloFlags: number;
  /** Raw 16 bytes; use {@link formatUuid} for the canonical text form. */ readonly runId: Uint8Array;
  readonly scenarioHash: Uint8Array;
  readonly worldHash: Uint8Array;
  readonly t0WallNs: bigint;
  readonly simDurationNs: bigint;
  readonly mobilityStepNs: bigint;
  readonly keyframePeriodNs: bigint;
  readonly telemetryPeriodNs: bigint;
  readonly metricPeriodNs: bigint;
  readonly resumeSeq: bigint;
  readonly simTimeNs: bigint;
  readonly originLatDeg: number;
  readonly originLonDeg: number;
  readonly originAltM: number;
  readonly bboxMinXM: number;
  readonly bboxMinYM: number;
  readonly bboxMaxXM: number;
  readonly bboxMaxYM: number;
  readonly actorCapacity: number;
  readonly nodes: HelloNodeTable;
  readonly classes: HelloClassTable;
  readonly channels: HelloChannelTable;
  readonly worldRef: HelloWorldRef;
  /** Strings established by this `Hello`, id order (id 0 = `""`). */ readonly strings: readonly string[];
  readonly strEngineVersion: number;
  readonly strScenarioName: number;
  readonly strRunLabel: number;
  readonly strSessionToken: number;
  /** `strings[str_engine_version]`, resolved for convenience. */ readonly engineVersion: string;
  readonly scenarioName: string;
  readonly runLabel: string;
  readonly sessionToken: string;
}

/** Format 16 raw UUID bytes as the canonical 36-character text form. */
export function formatUuid(bytes: Uint8Array): string {
  const hex = bytesToHex(bytes);
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20, 32)}`;
}

/** Lower-case hex of a byte array (content hashes, digests). */
export function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) out += bytes[i].toString(16).padStart(2, "0");
  return out;
}

/**
 * §2.2 — resolve one section's row count against its `off_*` sentinel.
 *
 * "`0` means section absent (a section can never legitimately start at 0, because every body begins
 * with a fixed prefix)". So a zero count is an empty section, and a **non-zero** count with a zero
 * offset is malformed: decoding it anyway builds the section's views over the body prefix and
 * reports plausible-looking garbage — or, worse, silently reports nothing at all while the frame
 * still declares rows on the wire.
 *
 * Every count/offset pair in the module goes through here, and so does every one in `world.ts`:
 * §4 says the world payload uses "the same conventions as §2", and one definition of the rule is
 * the point — this sweep exists because the rule had been applied in some decoders and not others.
 */
export function sectionCount(count: number, off: number, what: string): number {
  if (count === 0) return 0;
  if (off === 0) {
    throw new ProtocolError("bad_offset", `${what}: count is ${count} but its off_* is 0 (§2.2 section-absent sentinel)`, {
      expected: count,
      actual: 0,
      field: what,
    });
  }
  return count;
}

/** Decode a `Hello` frame (§3.1). */
export function decodeHello(v: FrameView): HelloMessage {
  const dv = bodyView(v);
  const O = HELLO_OFFSETS;
  if (v.bodyLen < HELLO_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Hello body is ${v.bodyLen} bytes, the prefix alone is 256`, { actual: v.bodyLen });
  }
  const nodeCount = dv.getUint32(O.nodeCount, true);
  const classCount = dv.getUint16(O.classCount, true);
  const channelCount = dv.getUint16(O.channelCount, true);
  const offNodes = dv.getUint32(O.offNodes, true);
  const offClasses = dv.getUint32(O.offClasses, true);
  const offChannels = dv.getUint32(O.offChannels, true);
  const offWorldRef = dv.getUint32(O.offWorldRef, true);
  const offStrings = dv.getUint32(O.offStrings, true);

  const N = sectionCount(nodeCount, offNodes, "Hello.nodes");
  const nodes: HelloNodeTable = {
    count: N,
    nodeId: u32s(v, offNodes + 0 * N, N, "Hello.nodes.node_id"),
    actorId: u32s(v, offNodes + 4 * N, N, "Hello.nodes.actor_id"),
    posXM: f32s(v, offNodes + 8 * N, N, "Hello.nodes.pos_x_m"),
    posYM: f32s(v, offNodes + 12 * N, N, "Hello.nodes.pos_y_m"),
    posZM: f32s(v, offNodes + 16 * N, N, "Hello.nodes.pos_z_m"),
    strLabel: u32s(v, offNodes + 20 * N, N, "Hello.nodes.str_label"),
    strProfileId: u32s(v, offNodes + 24 * N, N, "Hello.nodes.str_profile_id"),
    flags: u16s(v, offNodes + 28 * N, N, "Hello.nodes.flags"),
    kind: u8s(v, offNodes + 30 * N, N, "Hello.nodes.kind"),
    classIdx: u8s(v, offNodes + 31 * N, N, "Hello.nodes.class_idx"),
  };

  const C = sectionCount(classCount, offClasses, "Hello.classes");
  const classes: HelloClassTable = {
    count: C,
    strName: u32s(v, offClasses + 0 * C, C, "Hello.classes.str_name"),
    lengthM: f32s(v, offClasses + 4 * C, C, "Hello.classes.length_m"),
    widthM: f32s(v, offClasses + 8 * C, C, "Hello.classes.width_m"),
    heightM: f32s(v, offClasses + 12 * C, C, "Hello.classes.height_m"),
    colorRgba: u32s(v, offClasses + 16 * C, C, "Hello.classes.color_rgba"),
    reserved16: u16s(v, offClasses + 20 * C, C, "Hello.classes.reserved16"),
    category: u8s(v, offClasses + 22 * C, C, "Hello.classes.category"),
    reserved8: u8s(v, offClasses + 23 * C, C, "Hello.classes.reserved8"),
  };

  const K = sectionCount(channelCount, offChannels, "Hello.channels");
  const channels: HelloChannelTable = {
    count: K,
    strId: u32s(v, offChannels + 0 * K, K, "Hello.channels.str_id"),
    channelId: u16s(v, offChannels + 4 * K, K, "Hello.channels.channel_id"),
    visibility: u8s(v, offChannels + 6 * K, K, "Hello.channels.visibility"),
    enabled: u8s(v, offChannels + 7 * K, K, "Hello.channels.enabled"),
  };

  // §3.1 always includes the 16-byte world reference, so off_world_ref = 0 is never legitimate.
  if (offWorldRef === 0) {
    throw new ProtocolError("bad_offset", "Hello.world_ref: off_world_ref is 0, but §3.1 always includes the world reference (§2.2)", {
      field: "Hello.world_ref",
    });
  }
  const wrAbs = checkSection(v, offWorldRef, 16, "Hello.world_ref");
  const wr = new DataView(v.buffer, wrAbs, 16);
  const worldRef: HelloWorldRef = {
    mode: wr.getUint8(WORLD_REF_OFFSETS.mode),
    format: wr.getUint8(WORLD_REF_OFFSETS.format),
    payloadBytes: wr.getUint32(WORLD_REF_OFFSETS.payloadBytes, true),
    strUrl: wr.getUint32(WORLD_REF_OFFSETS.strUrl, true),
  };

  const strings = offStrings === 0 ? [""] : strTableStrings(decodeStrTable(v.buffer, checkSection(v, offStrings, 8, "Hello.strings"), v.bodyOffset + v.bodyLen));
  const str = (id: number): string => strings[id] ?? "";

  return {
    kind: "hello",
    header: v.header,
    versionMajor: dv.getUint16(O.versionMajor, true),
    versionMinor: dv.getUint16(O.versionMinor, true),
    helloFlags: dv.getUint32(O.helloFlags, true),
    runId: u8s(v, O.runId, 16, "Hello.run_id"),
    scenarioHash: u8s(v, O.scenarioHash, 32, "Hello.scenario_hash"),
    worldHash: u8s(v, O.worldHash, 32, "Hello.world_hash"),
    t0WallNs: dv.getBigInt64(O.t0WallNs, true),
    simDurationNs: dv.getBigUint64(O.simDurationNs, true),
    mobilityStepNs: dv.getBigUint64(O.mobilityStepNs, true),
    keyframePeriodNs: dv.getBigUint64(O.keyframePeriodNs, true),
    telemetryPeriodNs: dv.getBigUint64(O.telemetryPeriodNs, true),
    metricPeriodNs: dv.getBigUint64(O.metricPeriodNs, true),
    resumeSeq: dv.getBigUint64(O.resumeSeq, true),
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    originLatDeg: dv.getFloat64(O.originLatDeg, true),
    originLonDeg: dv.getFloat64(O.originLonDeg, true),
    originAltM: dv.getFloat64(O.originAltM, true),
    bboxMinXM: dv.getFloat64(O.bboxMinXM, true),
    bboxMinYM: dv.getFloat64(O.bboxMinYM, true),
    bboxMaxXM: dv.getFloat64(O.bboxMaxXM, true),
    bboxMaxYM: dv.getFloat64(O.bboxMaxYM, true),
    actorCapacity: dv.getUint32(O.actorCapacity, true),
    nodes,
    classes,
    channels,
    worldRef,
    strings,
    strEngineVersion: dv.getUint32(O.strEngineVersion, true),
    strScenarioName: dv.getUint32(O.strScenarioName, true),
    strRunLabel: dv.getUint32(O.strRunLabel, true),
    strSessionToken: dv.getUint32(O.strSessionToken, true),
    engineVersion: str(dv.getUint32(O.strEngineVersion, true)),
    scenarioName: str(dv.getUint32(O.strScenarioName, true)),
    runLabel: str(dv.getUint32(O.strRunLabel, true)),
    sessionToken: str(dv.getUint32(O.strSessionToken, true)),
  };
}

// ---------------------------------------------------------------------------
// §3.3 — Keyframe (0x0002)
// ---------------------------------------------------------------------------

/** §3.3.1 — the 64-byte `Keyframe` prefix. */
export const KEYFRAME_PREFIX_BYTES = 64;

/** §3.3.1 — body-relative byte offsets of the `Keyframe` prefix fields. */
export const KEYFRAME_OFFSETS = {
  /** `u64` @0 */ simTimeNs: 0,
  /** `f64` @8 */ originXM: 8,
  /** `f64` @16 */ originYM: 16,
  /** `f64` @24 */ originZM: 24,
  /** `u32` @32 — slot high-water mark + 1 */ actorCount: 32,
  /** `u32` @36 */ signalCount: 36,
  /** `u32` @40 */ offActors: 40,
  /** `u32` @44 */ offSignals: 44,
  /** `u32` @48 */ gopIndex: 48,
  /** `u16` @52 — 0 full, 1 node-only */ profile: 52,
  /** `u16` @54 */ reserved: 54,
  /** `u64` @56 */ reserved64: 56,
} as const;

/** §3.3.2 — bytes per actor row. */
export const KEYFRAME_ACTOR_STRIDE = 28;
/** §3.3.3 / §3.4.7 — bytes per signal row. */
export const SIGNAL_STRIDE = 8;

/** §3.3.4 — the `state` byte. */
export const ActorState = {
  /** ground truth */ ATTACKER: 0x01,
  REPORTED: 0x02,
  REVOKED: 0x04,
  EQUIPPED: 0x08,
  TRANSMITTING: 0x10,
  PARKED: 0x20,
  GNSS_DEGRADED: 0x40,
  WARNING_ACTIVE: 0x80,
} as const;

/** §3.3.4 — "benign" is the absence of bits 0–2 (conformance Q6). */
export function isBenign(state: number): boolean {
  return (state & (ActorState.ATTACKER | ActorState.REPORTED | ActorState.REVOKED)) === 0;
}

/** §3.3.2 — actor block, struct-of-arrays, indexed by slot. */
export interface KeyframeActorBlock {
  readonly count: number;
  /** `0xFFFFFFFF` = empty slot. */ readonly actorId: Uint32Array;
  /** mm east of `origin_x_m`. */ readonly xMm: Int32Array;
  /** mm north of `origin_y_m`. */ readonly yMm: Int32Array;
  /** GT; `0xFFFFFFFF` = off-lane/unknown. */ readonly laneId: Uint32Array;
  /** cm up from `origin_z_m`. */ readonly zCm: Int16Array;
  /** binary radians, ENU, 0 = east, CCW. */ readonly headingBrad: Uint16Array;
  /** 1/128 m/s along `heading`. */ readonly speedCq: Int16Array;
  /** GT; 1/64 m/s², longitudinal. */ readonly accelCq: Int16Array;
  readonly classIdx: Uint8Array;
  readonly state: Uint8Array;
  readonly verifiedNeighbors: Uint8Array;
  readonly flags8: Uint8Array;
}

/** §3.3.3 — signal block. */
export interface SignalBlock {
  readonly count: number;
  readonly signalId: Uint32Array;
  /** deciseconds, `0xFFFF` unknown. */ readonly timeToChangeDs: Uint16Array;
  /** SAE J2735 `MovementPhaseState` 0–9. */ readonly phase: Uint8Array;
  readonly reserved: Uint8Array;
}

/** §3.3 — a decoded `Keyframe`. */
export interface KeyframeMessage {
  readonly kind: "keyframe";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly originXM: number;
  readonly originYM: number;
  readonly originZM: number;
  readonly gopIndex: number;
  /** 0 full, 1 node-only. */ readonly profile: number;
  readonly actors: KeyframeActorBlock;
  readonly signals: SignalBlock;
  /** `FLAG_RESYNC` was set on the frame header (§2.3). */ readonly resync: boolean;
  /** `FLAG_SEEK_RESULT` was set on the frame header. */ readonly seekResult: boolean;
}

function decodeSignalBlock(v: FrameView, off: number, count: number, what: string): SignalBlock {
  const S = sectionCount(count, off, what);
  if (S === 0) {
    return { count: 0, signalId: new Uint32Array(0), timeToChangeDs: new Uint16Array(0), phase: new Uint8Array(0), reserved: new Uint8Array(0) };
  }
  return {
    count: S,
    signalId: u32s(v, off + 0 * S, S, `${what}.signal_id`),
    timeToChangeDs: u16s(v, off + 4 * S, S, `${what}.time_to_change_ds`),
    phase: u8s(v, off + 6 * S, S, `${what}.phase`),
    reserved: u8s(v, off + 7 * S, S, `${what}.reserved`),
  };
}

/** Decode a `Keyframe` frame (§3.3). */
export function decodeKeyframe(v: FrameView): KeyframeMessage {
  const dv = bodyView(v);
  const O = KEYFRAME_OFFSETS;
  if (v.bodyLen < KEYFRAME_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Keyframe body is ${v.bodyLen} bytes, the prefix alone is 64`, { actual: v.bodyLen });
  }
  const signalCount = dv.getUint32(O.signalCount, true);
  const offActors = dv.getUint32(O.offActors, true);
  const offSignals = dv.getUint32(O.offSignals, true);
  const A = sectionCount(dv.getUint32(O.actorCount, true), offActors, "Keyframe.actors");

  const actors: KeyframeActorBlock =
    A === 0
      ? {
          count: 0,
          actorId: new Uint32Array(0),
          xMm: new Int32Array(0),
          yMm: new Int32Array(0),
          laneId: new Uint32Array(0),
          zCm: new Int16Array(0),
          headingBrad: new Uint16Array(0),
          speedCq: new Int16Array(0),
          accelCq: new Int16Array(0),
          classIdx: new Uint8Array(0),
          state: new Uint8Array(0),
          verifiedNeighbors: new Uint8Array(0),
          flags8: new Uint8Array(0),
        }
      : {
          count: A,
          actorId: u32s(v, offActors + 0 * A, A, "Keyframe.actors.actor_id"),
          xMm: i32s(v, offActors + 4 * A, A, "Keyframe.actors.x_mm"),
          yMm: i32s(v, offActors + 8 * A, A, "Keyframe.actors.y_mm"),
          laneId: u32s(v, offActors + 12 * A, A, "Keyframe.actors.lane_id"),
          zCm: i16s(v, offActors + 16 * A, A, "Keyframe.actors.z_cm"),
          headingBrad: u16s(v, offActors + 18 * A, A, "Keyframe.actors.heading_brad"),
          speedCq: i16s(v, offActors + 20 * A, A, "Keyframe.actors.speed_cq"),
          accelCq: i16s(v, offActors + 22 * A, A, "Keyframe.actors.accel_cq"),
          classIdx: u8s(v, offActors + 24 * A, A, "Keyframe.actors.class_idx"),
          state: u8s(v, offActors + 25 * A, A, "Keyframe.actors.state"),
          verifiedNeighbors: u8s(v, offActors + 26 * A, A, "Keyframe.actors.verified_neighbors"),
          flags8: u8s(v, offActors + 27 * A, A, "Keyframe.actors.flags8"),
        };

  return {
    kind: "keyframe",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    originXM: dv.getFloat64(O.originXM, true),
    originYM: dv.getFloat64(O.originYM, true),
    originZM: dv.getFloat64(O.originZM, true),
    gopIndex: dv.getUint32(O.gopIndex, true),
    profile: dv.getUint16(O.profile, true),
    actors,
    signals: decodeSignalBlock(v, offSignals, signalCount, "Keyframe.signals"),
    resync: (v.header.flags & FrameFlags.RESYNC) !== 0,
    seekResult: (v.header.flags & FrameFlags.SEEK_RESULT) !== 0,
  };
}

// ---------------------------------------------------------------------------
// §3.4 — Delta (0x0003)
// ---------------------------------------------------------------------------

/** §3.4.1 — the 64-byte `Delta` prefix. */
export const DELTA_PREFIX_BYTES = 64;

/** §3.4.1 — body-relative byte offsets of the `Delta` prefix fields. */
export const DELTA_OFFSETS = {
  /** `u64` @0 */ simTimeNs: 0,
  /** `u32` @8 */ gopIndex: 8,
  /** `u32` @12 — 1-based within the GOP */ stepIndex: 12,
  /** `u32` @16 */ movedCount: 16,
  /** `u32` @20 */ absCount: 20,
  /** `u32` @24 */ laneCount: 24,
  /** `u32` @28 */ spawnCount: 28,
  /** `u32` @32 */ despawnCount: 32,
  /** `u32` @36 */ signalCount: 36,
  /** `u32` @40 */ offMoved: 40,
  /** `u32` @44 */ offAbs: 44,
  /** `u32` @48 */ offLanes: 48,
  /** `u32` @52 */ offSpawns: 52,
  /** `u32` @56 */ offDespawns: 56,
  /** `u32` @60 */ offSignals: 60,
} as const;

/** §3.4.2 — bytes per moved row. */
export const DELTA_MOVED_STRIDE = 20;
/** §3.4.3 — bytes per absolute-block entry. */
export const DELTA_ABS_STRIDE = 12;
/** §3.4.5 — bytes per spawn row. */
export const DELTA_SPAWN_STRIDE = 36;
/** §3.4.6 — bytes per despawn row. */
export const DELTA_DESPAWN_STRIDE = 8;

/** §3.4.2.1 — `mflags` bits. */
export const MovedFlags = {
  /** ignore `dx/dy/dz`; this row has an entry in the absolute block */ ABSOLUTE: 0x01,
  /** this row has an entry in the lane block */ LANE_CHANGED: 0x02,
} as const;

/** §3.4.2 — moved block, struct-of-arrays. */
export interface DeltaMovedBlock {
  readonly count: number;
  /** strictly ascending */ readonly slot: Uint32Array;
  /** mm change since the previous frame of the GOP */ readonly dxMm: Int16Array;
  readonly dyMm: Int16Array;
  readonly dzMm: Int16Array;
  /** absolute */ readonly headingBrad: Uint16Array;
  /** absolute, 1/128 m/s */ readonly speedCq: Int16Array;
  /** absolute, 1/64 m/s², GT */ readonly accelCq: Int16Array;
  /** absolute */ readonly state: Uint8Array;
  /** absolute */ readonly verifiedNeighbors: Uint8Array;
  readonly mflags: Uint8Array;
  readonly reserved: Uint8Array;
}

/**
 * §3.4.3 — absolute block: an **array of structs** of 12 bytes
 * (`i32 x_mm`, `i32 y_mm`, `i16 z_cm`, `u16 reserved`), in the order of the moved rows that set
 * `MFLAG_ABSOLUTE`. The two typed views below are strided over the same bytes, no copy.
 */
export interface DeltaAbsoluteBlock {
  readonly count: number;
  /** stride 3: `x = words[3i]`, `y = words[3i + 1]`. */ readonly words: Int32Array;
  /** stride 6: `z_cm = halves[6i + 4]`. */ readonly halves: Int16Array;
  xMm(i: number): number;
  yMm(i: number): number;
  zCm(i: number): number;
}

/** §3.4.5 — spawn block, struct-of-arrays. */
export interface DeltaSpawnBlock {
  readonly count: number;
  readonly slot: Uint32Array;
  readonly actorId: Uint32Array;
  /** `0xFFFFFFFF` if unequipped. */ readonly nodeId: Uint32Array;
  /** absolute, about the GOP keyframe origin. */ readonly xMm: Int32Array;
  readonly yMm: Int32Array;
  /** GT */ readonly laneId: Uint32Array;
  readonly zCm: Int16Array;
  readonly headingBrad: Uint16Array;
  readonly speedCq: Int16Array;
  /** GT; 0 demand, 1 scenario-event, 2 respawn, 3 handover-in, `0xFFFF` unknown. */ readonly cause: Uint16Array;
  readonly classIdx: Uint8Array;
  readonly state: Uint8Array;
  readonly verifiedNeighbors: Uint8Array;
  readonly reserved: Uint8Array;
}

/** §3.4.6 — despawn block. */
export interface DeltaDespawnBlock {
  readonly count: number;
  readonly slot: Uint32Array;
  /** GT; 0 trip-end, 1 left-map, 2 parked, 3 scenario-event, 4 error, `0xFFFF` unknown. */ readonly cause: Uint16Array;
  readonly reserved: Uint16Array;
}

/** §3.4 — a decoded `Delta`. */
export interface DeltaMessage {
  readonly kind: "delta";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly gopIndex: number;
  readonly stepIndex: number;
  readonly moved: DeltaMovedBlock;
  readonly absolute: DeltaAbsoluteBlock;
  /** GT; `u32` lane ids in the order of the moved rows that set `MFLAG_LANE_CHANGED`. */ readonly lanes: Uint32Array;
  readonly spawns: DeltaSpawnBlock;
  readonly despawns: DeltaDespawnBlock;
  readonly signals: SignalBlock;
}

const EMPTY_ABS: DeltaAbsoluteBlock = {
  count: 0,
  words: new Int32Array(0),
  halves: new Int16Array(0),
  xMm: () => 0,
  yMm: () => 0,
  zCm: () => 0,
};

/** Decode a `Delta` frame (§3.4). */
export function decodeDelta(v: FrameView): DeltaMessage {
  const dv = bodyView(v);
  const O = DELTA_OFFSETS;
  if (v.bodyLen < DELTA_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Delta body is ${v.bodyLen} bytes, the prefix alone is 64`, { actual: v.bodyLen });
  }
  const signalCount = dv.getUint32(O.signalCount, true);
  const offMoved = dv.getUint32(O.offMoved, true);
  const offAbs = dv.getUint32(O.offAbs, true);
  const offLanes = dv.getUint32(O.offLanes, true);
  const offSpawns = dv.getUint32(O.offSpawns, true);
  const offDespawns = dv.getUint32(O.offDespawns, true);
  const offSignals = dv.getUint32(O.offSignals, true);
  // §2.2 — every count is resolved against its own offset before a single view is built, so a
  // declared-but-unreachable block is a protocol error and never an empty one.
  const M = sectionCount(dv.getUint32(O.movedCount, true), offMoved, "Delta.moved");
  const absCount = sectionCount(dv.getUint32(O.absCount, true), offAbs, "Delta.abs");
  const laneCount = sectionCount(dv.getUint32(O.laneCount, true), offLanes, "Delta.lanes");
  const P = sectionCount(dv.getUint32(O.spawnCount, true), offSpawns, "Delta.spawns");
  const D = sectionCount(dv.getUint32(O.despawnCount, true), offDespawns, "Delta.despawns");

  const moved: DeltaMovedBlock =
    M === 0
      ? {
          count: 0,
          slot: new Uint32Array(0),
          dxMm: new Int16Array(0),
          dyMm: new Int16Array(0),
          dzMm: new Int16Array(0),
          headingBrad: new Uint16Array(0),
          speedCq: new Int16Array(0),
          accelCq: new Int16Array(0),
          state: new Uint8Array(0),
          verifiedNeighbors: new Uint8Array(0),
          mflags: new Uint8Array(0),
          reserved: new Uint8Array(0),
        }
      : {
          count: M,
          slot: u32s(v, offMoved + 0 * M, M, "Delta.moved.slot"),
          dxMm: i16s(v, offMoved + 4 * M, M, "Delta.moved.dx_mm"),
          dyMm: i16s(v, offMoved + 6 * M, M, "Delta.moved.dy_mm"),
          dzMm: i16s(v, offMoved + 8 * M, M, "Delta.moved.dz_mm"),
          headingBrad: u16s(v, offMoved + 10 * M, M, "Delta.moved.heading_brad"),
          speedCq: i16s(v, offMoved + 12 * M, M, "Delta.moved.speed_cq"),
          accelCq: i16s(v, offMoved + 14 * M, M, "Delta.moved.accel_cq"),
          state: u8s(v, offMoved + 16 * M, M, "Delta.moved.state"),
          verifiedNeighbors: u8s(v, offMoved + 17 * M, M, "Delta.moved.verified_neighbors"),
          mflags: u8s(v, offMoved + 18 * M, M, "Delta.moved.mflags"),
          reserved: u8s(v, offMoved + 19 * M, M, "Delta.moved.reserved"),
        };

  let absolute: DeltaAbsoluteBlock = EMPTY_ABS;
  if (absCount > 0) {
    const words = i32s(v, offAbs, absCount * 3, "Delta.abs");
    const halves = i16s(v, offAbs, absCount * 6, "Delta.abs.z_cm");
    absolute = {
      count: absCount,
      words,
      halves,
      xMm: (i: number) => words[i * 3],
      yMm: (i: number) => words[i * 3 + 1],
      zCm: (i: number) => halves[i * 6 + 4],
    };
  }

  const spawns: DeltaSpawnBlock =
    P === 0
      ? {
          count: 0,
          slot: new Uint32Array(0),
          actorId: new Uint32Array(0),
          nodeId: new Uint32Array(0),
          xMm: new Int32Array(0),
          yMm: new Int32Array(0),
          laneId: new Uint32Array(0),
          zCm: new Int16Array(0),
          headingBrad: new Uint16Array(0),
          speedCq: new Int16Array(0),
          cause: new Uint16Array(0),
          classIdx: new Uint8Array(0),
          state: new Uint8Array(0),
          verifiedNeighbors: new Uint8Array(0),
          reserved: new Uint8Array(0),
        }
      : {
          count: P,
          slot: u32s(v, offSpawns + 0 * P, P, "Delta.spawns.slot"),
          actorId: u32s(v, offSpawns + 4 * P, P, "Delta.spawns.actor_id"),
          nodeId: u32s(v, offSpawns + 8 * P, P, "Delta.spawns.node_id"),
          xMm: i32s(v, offSpawns + 12 * P, P, "Delta.spawns.x_mm"),
          yMm: i32s(v, offSpawns + 16 * P, P, "Delta.spawns.y_mm"),
          laneId: u32s(v, offSpawns + 20 * P, P, "Delta.spawns.lane_id"),
          zCm: i16s(v, offSpawns + 24 * P, P, "Delta.spawns.z_cm"),
          headingBrad: u16s(v, offSpawns + 26 * P, P, "Delta.spawns.heading_brad"),
          speedCq: i16s(v, offSpawns + 28 * P, P, "Delta.spawns.speed_cq"),
          cause: u16s(v, offSpawns + 30 * P, P, "Delta.spawns.cause"),
          classIdx: u8s(v, offSpawns + 32 * P, P, "Delta.spawns.class_idx"),
          state: u8s(v, offSpawns + 33 * P, P, "Delta.spawns.state"),
          verifiedNeighbors: u8s(v, offSpawns + 34 * P, P, "Delta.spawns.verified_neighbors"),
          reserved: u8s(v, offSpawns + 35 * P, P, "Delta.spawns.reserved"),
        };

  const despawns: DeltaDespawnBlock =
    D === 0
      ? { count: 0, slot: new Uint32Array(0), cause: new Uint16Array(0), reserved: new Uint16Array(0) }
      : {
          count: D,
          slot: u32s(v, offDespawns + 0 * D, D, "Delta.despawns.slot"),
          cause: u16s(v, offDespawns + 4 * D, D, "Delta.despawns.cause"),
          reserved: u16s(v, offDespawns + 6 * D, D, "Delta.despawns.reserved"),
        };

  return {
    kind: "delta",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    gopIndex: dv.getUint32(O.gopIndex, true),
    stepIndex: dv.getUint32(O.stepIndex, true),
    moved,
    absolute,
    lanes: laneCount === 0 ? new Uint32Array(0) : u32s(v, offLanes, laneCount, "Delta.lanes"),
    spawns,
    despawns,
    signals: decodeSignalBlock(v, offSignals, signalCount, "Delta.signals"),
  };
}

// ---------------------------------------------------------------------------
// §3.5 — Telemetry (0x0004)
// ---------------------------------------------------------------------------

/** §3.5.1 — the 32-byte `Telemetry` prefix. */
export const TELEMETRY_PREFIX_BYTES = 32;

/** §3.5.1 — body-relative byte offsets of the `Telemetry` prefix fields. */
export const TELEMETRY_OFFSETS = {
  /** `u64` @0 — end of the sampling window */ simTimeNs: 0,
  /** `u64` @8 */ windowNs: 8,
  /** `u32` @16 */ nodeCount: 16,
  /** `u32` @20 — MUST be 8-aligned */ offRecords: 20,
  /** `u32` @24 — 208 in v1 */ recordSize: 24,
  /** `u32` @28 */ reserved: 28,
} as const;

/** §3.5.2 — `record_size` in v1. Readers MUST stride by the wire value, not this (conformance C2). */
export const TELEMETRY_RECORD_BYTES_V1 = 208;

/** §3.5.2 — byte offsets within one 208-byte `NodeTelemetry` record. */
export const NODE_TELEMETRY_OFFSETS = {
  /** `u64` @0 */ storageUsedB: 0,
  /** `u64` @8 */ storageTotalB: 8,
  /** `u64` @16 — `u64::MAX` = none */ nextTopupNs: 16,
  /** `u64` @24 */ crlBytes: 24,
  /** `u64` @32 */ outboxBytes: 32,
  /** `i64` @40 — GT */ clockOffsetNs: 40,
  /** `u32` @48 */ nodeId: 48,
  /** `u32` @52 */ ramUsedKib: 52,
  /** `u32` @56 */ ramTotalKib: 56,
  /** `u32` @60 */ dropRxOverflow: 60,
  /** `u32` @64 */ dropVerifyPolicySkip: 64,
  /** `u32` @68 */ dropVerifyOverflow: 68,
  /** `u32` @72 */ dropTxOverflow: 72,
  /** `u32` @76 */ dropReassemblyTimeout: 76,
  /** `u32` @80 */ dropCrlBacklog: 80,
  /** `u32` @84 */ certStored: 84,
  /** `u32` @88 */ crlEntries: 88,
  /** `u32` @92 */ outboxMsgs: 92,
  /** `u32` @96 */ peerCacheEntries: 96,
  /** `u32` @100 */ p2pcdRequests: 100,
  /** `u32` @104 */ fullCertMsgs: 104,
  /** `f32` @108 */ msgsInPerS: 108,
  /** `f32` @112 */ msgsOutPerS: 112,
  /** `f32` @116 */ verificationsPerS: 116,
  /** `f32` @120 */ verifyWaitP50Ms: 120,
  /** `f32` @124 */ verifyWaitP95Ms: 124,
  /** `f32` @128 */ gnssHdop: 128,
  /** `f32` @132 */ gnssSigmaM: 132,
  /** `f32` @136 */ clockDriftPpm: 136,
  /** `f32` @140 — GT */ posErrorM: 140,
  /** `f32` @144 */ airtimeMsPerS: 144,
  /** `u16` @148 */ cpuUtilPm: 148,
  /** `u16` @150 */ hsmUtilPm: 150,
  /** `u16` @152 */ qRxP50: 152,
  /** `u16` @154 */ qRxP95: 154,
  /** `u16` @156 */ qVerifyP50: 156,
  /** `u16` @158 */ qVerifyP95: 158,
  /** `u16` @160 */ qAppP50: 160,
  /** `u16` @162 */ qAppP95: 162,
  /** `u16` @164 */ qTxP50: 164,
  /** `u16` @166 */ qTxP95: 166,
  /** `u16` @168 */ qCrlP50: 168,
  /** `u16` @170 */ qCrlP95: 170,
  /** `u16` @172 */ dccState: 172,
  /** `u16` @174 */ cbrPm: 174,
  /** `i16` @176 — centi-dBm */ txPowerCdbm: 176,
  /** `u16` @178 */ nbrTotal: 178,
  /** `u16` @180 */ nbrVerified: 180,
  /** `u16` @182 */ nbrUnverified: 182,
  /** `u16` @184 */ nbrRevoked: 184,
  /** `u16` @186 */ certActive: 186,
  /** `u16` @188 */ crlExpansionPm: 188,
  /** `u16` @190 */ unverifiedRatioPm: 190,
  /** `u8` @192 */ gnssFix: 192,
  /** `u8` @193 — 6 (compromised) is GT */ nodeState: 193,
  /** `u8` @194 */ verifyPolicy: 194,
  /** `u8` @195 */ reserved8: 195,
  /** `u8[12]` @196 */ reserved: 196,
} as const;

/** §3.5.2 — one decoded `NodeTelemetry` record. Every field of the 208-byte layout is present. */
export interface NodeTelemetry {
  readonly storageUsedB: bigint;
  readonly storageTotalB: bigint;
  readonly nextTopupNs: bigint;
  readonly crlBytes: bigint;
  readonly outboxBytes: bigint;
  readonly clockOffsetNs: bigint;
  readonly nodeId: number;
  readonly ramUsedKib: number;
  readonly ramTotalKib: number;
  readonly dropRxOverflow: number;
  readonly dropVerifyPolicySkip: number;
  readonly dropVerifyOverflow: number;
  readonly dropTxOverflow: number;
  readonly dropReassemblyTimeout: number;
  readonly dropCrlBacklog: number;
  readonly certStored: number;
  readonly crlEntries: number;
  readonly outboxMsgs: number;
  readonly peerCacheEntries: number;
  readonly p2pcdRequests: number;
  readonly fullCertMsgs: number;
  readonly msgsInPerS: number;
  readonly msgsOutPerS: number;
  readonly verificationsPerS: number;
  readonly verifyWaitP50Ms: number;
  readonly verifyWaitP95Ms: number;
  readonly gnssHdop: number;
  readonly gnssSigmaM: number;
  readonly clockDriftPpm: number;
  readonly posErrorM: number;
  readonly airtimeMsPerS: number;
  readonly cpuUtilPm: number;
  readonly hsmUtilPm: number;
  readonly qRxP50: number;
  readonly qRxP95: number;
  readonly qVerifyP50: number;
  readonly qVerifyP95: number;
  readonly qAppP50: number;
  readonly qAppP95: number;
  readonly qTxP50: number;
  readonly qTxP95: number;
  readonly qCrlP50: number;
  readonly qCrlP95: number;
  readonly dccState: number;
  readonly cbrPm: number;
  readonly txPowerCdbm: number;
  readonly nbrTotal: number;
  readonly nbrVerified: number;
  readonly nbrUnverified: number;
  readonly nbrRevoked: number;
  readonly certActive: number;
  readonly crlExpansionPm: number;
  readonly unverifiedRatioPm: number;
  readonly gnssFix: number;
  readonly nodeState: number;
  readonly verifyPolicy: number;
}

/** §3.5 — a decoded `Telemetry` frame. Records are read on demand, striding by the wire `record_size`. */
export interface TelemetryMessage {
  readonly kind: "telemetry";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly windowNs: bigint;
  readonly nodeCount: number;
  readonly recordSize: number;
  /** The whole record region, for callers that want to forward it untouched. */ readonly raw: Uint8Array;
  /** Decode record `i` (0-based). */ record(i: number): NodeTelemetry;
  /** `node_id` of record `i` without decoding the rest. */ nodeIdAt(i: number): number;
  records(): NodeTelemetry[];
}

function readNodeTelemetry(dv: DataView, base: number): NodeTelemetry {
  const T = NODE_TELEMETRY_OFFSETS;
  return {
    storageUsedB: dv.getBigUint64(base + T.storageUsedB, true),
    storageTotalB: dv.getBigUint64(base + T.storageTotalB, true),
    nextTopupNs: dv.getBigUint64(base + T.nextTopupNs, true),
    crlBytes: dv.getBigUint64(base + T.crlBytes, true),
    outboxBytes: dv.getBigUint64(base + T.outboxBytes, true),
    clockOffsetNs: dv.getBigInt64(base + T.clockOffsetNs, true),
    nodeId: dv.getUint32(base + T.nodeId, true),
    ramUsedKib: dv.getUint32(base + T.ramUsedKib, true),
    ramTotalKib: dv.getUint32(base + T.ramTotalKib, true),
    dropRxOverflow: dv.getUint32(base + T.dropRxOverflow, true),
    dropVerifyPolicySkip: dv.getUint32(base + T.dropVerifyPolicySkip, true),
    dropVerifyOverflow: dv.getUint32(base + T.dropVerifyOverflow, true),
    dropTxOverflow: dv.getUint32(base + T.dropTxOverflow, true),
    dropReassemblyTimeout: dv.getUint32(base + T.dropReassemblyTimeout, true),
    dropCrlBacklog: dv.getUint32(base + T.dropCrlBacklog, true),
    certStored: dv.getUint32(base + T.certStored, true),
    crlEntries: dv.getUint32(base + T.crlEntries, true),
    outboxMsgs: dv.getUint32(base + T.outboxMsgs, true),
    peerCacheEntries: dv.getUint32(base + T.peerCacheEntries, true),
    p2pcdRequests: dv.getUint32(base + T.p2pcdRequests, true),
    fullCertMsgs: dv.getUint32(base + T.fullCertMsgs, true),
    msgsInPerS: dv.getFloat32(base + T.msgsInPerS, true),
    msgsOutPerS: dv.getFloat32(base + T.msgsOutPerS, true),
    verificationsPerS: dv.getFloat32(base + T.verificationsPerS, true),
    verifyWaitP50Ms: dv.getFloat32(base + T.verifyWaitP50Ms, true),
    verifyWaitP95Ms: dv.getFloat32(base + T.verifyWaitP95Ms, true),
    gnssHdop: dv.getFloat32(base + T.gnssHdop, true),
    gnssSigmaM: dv.getFloat32(base + T.gnssSigmaM, true),
    clockDriftPpm: dv.getFloat32(base + T.clockDriftPpm, true),
    posErrorM: dv.getFloat32(base + T.posErrorM, true),
    airtimeMsPerS: dv.getFloat32(base + T.airtimeMsPerS, true),
    cpuUtilPm: dv.getUint16(base + T.cpuUtilPm, true),
    hsmUtilPm: dv.getUint16(base + T.hsmUtilPm, true),
    qRxP50: dv.getUint16(base + T.qRxP50, true),
    qRxP95: dv.getUint16(base + T.qRxP95, true),
    qVerifyP50: dv.getUint16(base + T.qVerifyP50, true),
    qVerifyP95: dv.getUint16(base + T.qVerifyP95, true),
    qAppP50: dv.getUint16(base + T.qAppP50, true),
    qAppP95: dv.getUint16(base + T.qAppP95, true),
    qTxP50: dv.getUint16(base + T.qTxP50, true),
    qTxP95: dv.getUint16(base + T.qTxP95, true),
    qCrlP50: dv.getUint16(base + T.qCrlP50, true),
    qCrlP95: dv.getUint16(base + T.qCrlP95, true),
    dccState: dv.getUint16(base + T.dccState, true),
    cbrPm: dv.getUint16(base + T.cbrPm, true),
    txPowerCdbm: dv.getInt16(base + T.txPowerCdbm, true),
    nbrTotal: dv.getUint16(base + T.nbrTotal, true),
    nbrVerified: dv.getUint16(base + T.nbrVerified, true),
    nbrUnverified: dv.getUint16(base + T.nbrUnverified, true),
    nbrRevoked: dv.getUint16(base + T.nbrRevoked, true),
    certActive: dv.getUint16(base + T.certActive, true),
    crlExpansionPm: dv.getUint16(base + T.crlExpansionPm, true),
    unverifiedRatioPm: dv.getUint16(base + T.unverifiedRatioPm, true),
    gnssFix: dv.getUint8(base + T.gnssFix),
    nodeState: dv.getUint8(base + T.nodeState),
    verifyPolicy: dv.getUint8(base + T.verifyPolicy),
  };
}

/**
 * Decode `NodeTelemetry` record `index` out of a raw record region, striding by the wire
 * `recordSize` (conformance C2). Exported so a worker can rehydrate a transferred region.
 */
export function readNodeTelemetryRecord(raw: Uint8Array, index: number, recordSize: number): NodeTelemetry {
  const dv = new DataView(raw.buffer, raw.byteOffset, raw.byteLength);
  return readNodeTelemetry(dv, index * recordSize);
}

/** Decode a `Telemetry` frame (§3.5). */
export function decodeTelemetry(v: FrameView): TelemetryMessage {
  const dv = bodyView(v);
  const O = TELEMETRY_OFFSETS;
  if (v.bodyLen < TELEMETRY_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Telemetry body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  }
  const nodeCount = dv.getUint32(O.nodeCount, true);
  const offRecords = dv.getUint32(O.offRecords, true);
  const recordSize = dv.getUint32(O.recordSize, true);
  if (recordSize < TELEMETRY_RECORD_BYTES_V1 && nodeCount > 0) {
    throw new ProtocolError("bad_length", `Telemetry.record_size ${recordSize} is smaller than the v1 record (208)`, {
      actual: recordSize,
      expected: TELEMETRY_RECORD_BYTES_V1,
    });
  }
  // §2.2 — the record block is one more section behind an `off_*` sentinel. Without this check a
  // frame declaring N nodes with `off_records = 0` passes `checkSection(…, 0, total)` (0 + total is
  // inside the body) and reads its 208-byte records over the 32-byte prefix: probed before the fix,
  // one node decoded with node_id 0x0 out of the prefix, and no error was raised.
  const total = sectionCount(nodeCount, offRecords, "Telemetry.records") * recordSize;
  const absRecords = nodeCount === 0 ? v.bodyOffset : checkSection(v, offRecords, total, "Telemetry.records");
  const recDv = new DataView(v.buffer, absRecords, total);
  const bounded = (i: number): number => {
    if (i < 0 || i >= nodeCount) {
      throw new ProtocolError("bad_offset", `Telemetry record ${i} out of range (node_count = ${nodeCount})`, { offset: i });
    }
    return i * recordSize;
  };
  return {
    kind: "telemetry",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    windowNs: dv.getBigUint64(O.windowNs, true),
    nodeCount,
    recordSize,
    raw: new Uint8Array(v.buffer, absRecords, total),
    record: (i: number) => readNodeTelemetry(recDv, bounded(i)),
    nodeIdAt: (i: number) => recDv.getUint32(bounded(i) + NODE_TELEMETRY_OFFSETS.nodeId, true),
    records: () => {
      const out: NodeTelemetry[] = new Array<NodeTelemetry>(nodeCount);
      for (let i = 0; i < nodeCount; i++) out[i] = readNodeTelemetry(recDv, i * recordSize);
      return out;
    },
  };
}

// ---------------------------------------------------------------------------
// §3.6 — Event (0x0005)
// ---------------------------------------------------------------------------

/** §3.6.1 — the 32-byte `Event` prefix. */
export const EVENT_PREFIX_BYTES = 32;

/** §3.6.1 — body-relative byte offsets of the `Event` prefix fields. */
export const EVENT_OFFSETS = {
  /** `u64` @0 — inclusive lower bound of the batch */ tStartNs: 0,
  /** `u64` @8 — inclusive upper bound */ tEndNs: 8,
  /** `u32` @16 */ eventCount: 16,
  /** `u32` @20 — MUST be 8-aligned */ offIndex: 20,
  /** `u32` @24 — MUST be 8-aligned */ offPayloads: 24,
  /** `u32` @28 */ payloadBytes: 28,
} as const;

/** §3.6.1 — bytes per index entry. */
export const EVENT_INDEX_STRIDE = 16;

/** §3.6.2 — core channel ids. */
export const ChannelId = {
  GT_KINEMATICS: 1,
  GT_ATTACK_ACTION: 2,
  GT_SPAWN: 3,
  GT_DESPAWN: 4,
  NODE_TX: 10,
  PHY_RX: 11,
  MAC_CBR: 12,
  NET_FRAG: 13,
  NODE_VERIFY: 14,
  NODE_TELEMETRY: 15,
  NODE_NEIGHBOR: 16,
  SEC_CERT: 20,
  PROTO_MSG: 21,
  PROTO_REVOCATION: 22,
  DET_OBSERVATION: 30,
  MA_REPORT: 31,
  MA_CASE: 32,
  MA_DECISION: 33,
  APP_WARNING: 40,
  METRIC_SAMPLE: 50,
  SNAPSHOT_KEYFRAME: 60,
  SNAPSHOT_DELTA: 61,
  MANIFEST: 70,
} as const;
export type ChannelId = (typeof ChannelId)[keyof typeof ChannelId];

/** §3.6.3 — `MsgType` enum used by several payloads (0 other … 18 provisioning-response). */
export const V2xMsgType = {
  OTHER: 0, BSM: 1, CAM: 2, DENM: 3, SPAT: 4, MAP: 5, PSM: 6, VAM: 7, CPM: 8, SRM: 9, SSM: 10,
  WSA: 11, CRL: 12, MISBEHAVIOR_REPORT: 13, P2PCD_REQUEST: 14, P2PCD_RESPONSE: 15, CTL_ECTL: 16,
  PROVISIONING_REQUEST: 17, PROVISIONING_RESPONSE: 18,
} as const;

/** §3.6.4 — `node.tx` (channel 10), 40 bytes. */
export interface NodeTxPayload {
  readonly channel: "node.tx";
  readonly nodeId: number; readonly msgId: number; readonly bytesOnAir: number; readonly airtimeMs: number;
  readonly msgType: number; readonly txPowerCdbm: number; readonly channelNumber: number; readonly mcs: number;
  readonly accessCategory: number; readonly dccState: number; readonly signerIdType: number;
  readonly payloadBytes: number; readonly pseudonymDigest: Uint8Array; readonly certId: number;
}
/** §3.6.5 — `phy.rx` (channel 11), 48 bytes. */
export interface PhyRxPayload {
  readonly channel: "phy.rx";
  readonly tStartNs: bigint; readonly tEndNs: bigint; readonly rxNode: number;
  /** GT; `0xFFFFFFFF` in the `node` profile. */ readonly txNode: number;
  readonly msgId: number; readonly rssiDbm: number; readonly sinrDb: number;
  /** GT; `NaN` in the `node` profile. */ readonly distanceM: number;
  readonly outcome: number; readonly cause: number;
  /** GT; `0xFF` in the `node` profile. */ readonly losClass: number;
}
/** §3.6.6 — `node.verify` (channel 14), 48 bytes. */
export interface NodeVerifyPayload {
  readonly channel: "node.verify";
  readonly tEnqueueNs: bigint; readonly tStartNs: bigint; readonly tDoneNs: bigint;
  readonly nodeId: number; readonly msgId: number; readonly costUs: number; readonly primitive: number;
  readonly outcome: number; readonly policyDecision: number; readonly policyReason: number;
  readonly whereRun: number; readonly queueDepthAtEnqueue: number;
}
/** §3.6.7 — `sec.cert` (channel 20), 40 bytes. */
export interface SecCertPayload {
  readonly channel: "sec.cert";
  readonly validFromNs: bigint; readonly validUntilNs: bigint; readonly nodeId: number; readonly certId: number;
  readonly digest: Uint8Array; readonly event: number; readonly certKind: number;
  readonly indexI: number; readonly indexJ: number; readonly count: number;
}
/** §3.6.8 — `det.observation` (channel 30), 32 bytes. */
export interface DetObservationPayload {
  readonly channel: "det.observation";
  readonly nodeId: number; readonly strDetector: number; readonly subjectDigest: Uint8Array;
  readonly score: number;
  /** GT; `0xFFFFFFFF` in the `node` profile. */ readonly subjectActorId: number;
  readonly evidenceCount: number; readonly detectorKind: number; readonly provId: number;
}
/** §3.6.9 — `app.warning` (channel 40), 32 bytes. */
export interface AppWarningPayload {
  readonly channel: "app.warning";
  readonly nodeId: number; readonly strApp: number; readonly subjectDigest: Uint8Array;
  readonly ttcS: number; readonly distanceM: number; readonly kind: number; readonly severity: number;
  /** GT; `0` in the `node` profile. */ readonly truth: number;
  /** GT; `0xFFFFFFFF` in the `node` profile. */ readonly subjectActorId: number;
}
/** §3.6.10 — `proto.revocation` (channel 22), 32 bytes, PUBLIC. */
export interface ProtoRevocationPayload {
  readonly channel: "proto.revocation";
  readonly subjectNodeId: number; readonly revocationId: number; readonly subjectDigest: Uint8Array;
  readonly sizeBytes: bigint; readonly nodeId: number; readonly stage: number;
  readonly mechanism: number; readonly entries: number;
}
/** §3.6.11 — `gt.kinematics` (channel 1), 56 bytes, GT. */
export interface GtKinematicsPayload {
  readonly channel: "gt.kinematics";
  readonly actorId: number; readonly laneId: number;
  readonly posXM: number; readonly posYM: number; readonly posZM: number;
  readonly velXMps: number; readonly velYMps: number; readonly velZMps: number;
  readonly accXMps2: number; readonly accYMps2: number; readonly accZMps2: number;
  readonly headingRad: number; readonly yawRateRadS: number; readonly laneSM: number;
}
/** §3.6.12 — `gt.attack.action` (channel 2), 32 bytes, GT. */
export interface GtAttackActionPayload {
  readonly channel: "gt.attack.action";
  readonly actorId: number; readonly nodeId: number; readonly msgId: number; readonly strAttackId: number;
  readonly fieldsChanged: number; readonly magnitude: number; readonly action: number;
  readonly coalitionId: number; readonly provId: number;
}
/** §3.6.13 — `mac.cbr` (channel 12), 16 bytes. */
export interface MacCbrPayload {
  readonly channel: "mac.cbr";
  readonly nodeId: number; readonly cbr: number; readonly channelNumber: number;
  readonly dccState: number; readonly txPowerCdbm: number;
}
/** §3.6.14 — `net.frag` (channel 13), 24 bytes. */
export interface NetFragPayload {
  readonly channel: "net.frag";
  readonly nodeId: number; readonly sduId: number; readonly sduBytes: number;
  readonly fragmentsTotal: number; readonly fragmentsReceived: number; readonly msgType: number;
  readonly outcome: number; readonly direction: number;
}
/** §3.6.15 — `node.neighbor` (channel 16), 32 bytes. */
export interface NodeNeighborPayload {
  readonly channel: "node.neighbor";
  readonly nodeId: number; readonly peerDigest: Uint8Array; readonly relevance: number;
  readonly tableSizeAfter: number; readonly op: number; readonly verifyState: number;
  /** GT; `0xFFFFFFFF` in the `node` profile. */ readonly peerActorId: number;
  readonly provId: number;
}
/** §3.6.16 — `proto.msg` (channel 21), 32 bytes. */
export interface ProtoMsgPayload {
  readonly channel: "proto.msg";
  readonly fromNode: number; readonly toNode: number; readonly strFlow: number; readonly bytes: number;
  readonly flowInstanceId: number; readonly step: number; readonly transport: number;
  readonly outcome: number; readonly latencyMs: number; readonly provId: number;
}
/** §3.6.17 — `ma.report` / `ma.case` / `ma.decision` (channels 31/32/33), 40 bytes. */
export interface MaRecordPayload {
  readonly channel: "ma.report" | "ma.case" | "ma.decision";
  readonly caseId: number; readonly reportId: number; readonly subjectDigest: Uint8Array;
  readonly reporterNodeId: number; readonly confidence: number; readonly strPipeline: number;
  /** GT; `0xFFFFFFFF` in the `node` profile. */ readonly subjectActorId: number;
  readonly reportsInCase: number; readonly kind: number; readonly reporterKind: number; readonly provId: number;
}
/** A payload on a channel this decoder does not know (§3.6.1: skip it by `payload_len`). */
export interface UnknownPayload {
  readonly channel: "unknown";
  readonly channelId: number;
  readonly bytes: Uint8Array;
}

/** Any decoded event payload. */
export type EventPayload =
  | NodeTxPayload | PhyRxPayload | NodeVerifyPayload | SecCertPayload | DetObservationPayload
  | AppWarningPayload | ProtoRevocationPayload | GtKinematicsPayload | GtAttackActionPayload
  | MacCbrPayload | NetFragPayload | NodeNeighborPayload | ProtoMsgPayload | MaRecordPayload | UnknownPayload;

/**
 * §3.6.4–§3.6.17 — the fixed record size of every channel this build decodes, transcribed from the
 * section headings ("`node.tx` (channel 10) — 40 bytes" and so on).
 *
 * A payload shorter than its channel's record is malformed: §3.6.1 sizes every payload by the
 * record and pads it to a multiple of 8. Decoding it anyway reads whatever follows in the payload
 * region, so {@link decodeEventPayload} rejects it with a typed {@link ProtocolError} instead.
 * Unknown channels have no entry here and keep falling through to {@link UnknownPayload}.
 */
export const EVENT_PAYLOAD_BYTES: Readonly<Record<number, number>> = {
  /** §3.6.11 */ [ChannelId.GT_KINEMATICS]: 56,
  /** §3.6.12 */ [ChannelId.GT_ATTACK_ACTION]: 32,
  /** §3.6.4 */ [ChannelId.NODE_TX]: 40,
  /** §3.6.5 */ [ChannelId.PHY_RX]: 48,
  /** §3.6.13 */ [ChannelId.MAC_CBR]: 16,
  /** §3.6.14 */ [ChannelId.NET_FRAG]: 24,
  /** §3.6.6 */ [ChannelId.NODE_VERIFY]: 48,
  /** §3.6.15 */ [ChannelId.NODE_NEIGHBOR]: 32,
  /** §3.6.7 */ [ChannelId.SEC_CERT]: 40,
  /** §3.6.16 */ [ChannelId.PROTO_MSG]: 32,
  /** §3.6.10 */ [ChannelId.PROTO_REVOCATION]: 32,
  /** §3.6.8 */ [ChannelId.DET_OBSERVATION]: 32,
  /** §3.6.17 */ [ChannelId.MA_REPORT]: 40,
  /** §3.6.17 */ [ChannelId.MA_CASE]: 40,
  /** §3.6.17 */ [ChannelId.MA_DECISION]: 40,
  /** §3.6.9 */ [ChannelId.APP_WARNING]: 32,
};

/** §3.6.2 — the channel name for a core channel id, for error messages and inspectors. */
export const EVENT_CHANNEL_NAMES: Readonly<Record<number, string>> = {
  [ChannelId.GT_KINEMATICS]: "gt.kinematics",
  [ChannelId.GT_ATTACK_ACTION]: "gt.attack.action",
  [ChannelId.GT_SPAWN]: "gt.spawn",
  [ChannelId.GT_DESPAWN]: "gt.despawn",
  [ChannelId.NODE_TX]: "node.tx",
  [ChannelId.PHY_RX]: "phy.rx",
  [ChannelId.MAC_CBR]: "mac.cbr",
  [ChannelId.NET_FRAG]: "net.frag",
  [ChannelId.NODE_VERIFY]: "node.verify",
  [ChannelId.NODE_TELEMETRY]: "node.telemetry",
  [ChannelId.NODE_NEIGHBOR]: "node.neighbor",
  [ChannelId.SEC_CERT]: "sec.cert",
  [ChannelId.PROTO_MSG]: "proto.msg",
  [ChannelId.PROTO_REVOCATION]: "proto.revocation",
  [ChannelId.DET_OBSERVATION]: "det.observation",
  [ChannelId.MA_REPORT]: "ma.report",
  [ChannelId.MA_CASE]: "ma.case",
  [ChannelId.MA_DECISION]: "ma.decision",
  [ChannelId.APP_WARNING]: "app.warning",
  [ChannelId.METRIC_SAMPLE]: "metric.sample",
  [ChannelId.SNAPSHOT_KEYFRAME]: "snapshot.keyframe",
  [ChannelId.SNAPSHOT_DELTA]: "snapshot.delta",
  [ChannelId.MANIFEST]: "manifest",
};

/** §3.6.2 — the channel name, or `channel <id>` for a plug-in channel this build does not know. */
export function eventChannelName(channelId: number): string {
  return EVENT_CHANNEL_NAMES[channelId] ?? `channel ${channelId}`;
}

/**
 * Read a `HashedId8` (§3.6.2) from inside the payload.
 *
 * Bounded by the **declared payload**, not by the underlying `ArrayBuffer`: a plain
 * `new Uint8Array(dv.buffer, dv.byteOffset + off, 8)` is bounded only by the buffer, so a short
 * payload would silently return bytes belonging to the next payload as a pseudonym digest.
 */
function digest8(dv: DataView, off: number): Uint8Array {
  if (off < 0 || off + 8 > dv.byteLength) {
    throw new ProtocolError("bad_length", `HashedId8 at +${off} needs 8 bytes but the payload is ${dv.byteLength} bytes (§3.6.2)`, {
      offset: off,
      expected: off + 8,
      actual: dv.byteLength,
      field: "HashedId8",
    });
  }
  return new Uint8Array(dv.buffer, dv.byteOffset + off, 8);
}

/**
 * Decode one event payload by channel id (§3.6.4–§3.6.17).
 *
 * `dv` must start at the payload. A channel this build does not know yields {@link UnknownPayload}
 * rather than an error — §3.6.1 and §8.4 require readers to skip unknown channels by `payload_len`.
 */
export function decodeEventPayload(channelId: number, dv: DataView): EventPayload {
  // §3.6.4–§3.6.17 — a known channel's payload is a fixed-size record; a shorter one is malformed.
  // Checked here so the failure is a typed ProtocolError the client can close the socket on (1002),
  // not a bare RangeError escaping the message handler.
  const needed = EVENT_PAYLOAD_BYTES[channelId];
  if (needed !== undefined && dv.byteLength < needed) {
    throw new ProtocolError(
      "bad_length",
      `Event payload for ${eventChannelName(channelId)} is ${dv.byteLength} bytes, §3.6 defines a ${needed}-byte record`,
      { expected: needed, actual: dv.byteLength, field: eventChannelName(channelId) },
    );
  }
  switch (channelId) {
    case ChannelId.NODE_TX:
      return { channel: "node.tx", nodeId: dv.getUint32(0, true), msgId: dv.getUint32(4, true),
        bytesOnAir: dv.getUint32(8, true), airtimeMs: dv.getFloat32(12, true), msgType: dv.getUint16(16, true),
        txPowerCdbm: dv.getInt16(18, true), channelNumber: dv.getUint16(20, true), mcs: dv.getUint8(22),
        accessCategory: dv.getUint8(23), dccState: dv.getUint8(24), signerIdType: dv.getUint8(25),
        payloadBytes: dv.getUint16(26, true), pseudonymDigest: digest8(dv, 28), certId: dv.getUint32(36, true) };
    case ChannelId.PHY_RX:
      return { channel: "phy.rx", tStartNs: dv.getBigUint64(0, true), tEndNs: dv.getBigUint64(8, true),
        rxNode: dv.getUint32(16, true), txNode: dv.getUint32(20, true), msgId: dv.getUint32(24, true),
        rssiDbm: dv.getFloat32(28, true), sinrDb: dv.getFloat32(32, true), distanceM: dv.getFloat32(36, true),
        outcome: dv.getUint8(40), cause: dv.getUint8(41), losClass: dv.getUint8(42) };
    case ChannelId.NODE_VERIFY:
      return { channel: "node.verify", tEnqueueNs: dv.getBigUint64(0, true), tStartNs: dv.getBigUint64(8, true),
        tDoneNs: dv.getBigUint64(16, true), nodeId: dv.getUint32(24, true), msgId: dv.getUint32(28, true),
        costUs: dv.getFloat32(32, true), primitive: dv.getUint16(36, true), outcome: dv.getUint8(38),
        policyDecision: dv.getUint8(39), policyReason: dv.getUint8(40), whereRun: dv.getUint8(41),
        queueDepthAtEnqueue: dv.getUint16(42, true) };
    case ChannelId.SEC_CERT:
      return { channel: "sec.cert", validFromNs: dv.getBigUint64(0, true), validUntilNs: dv.getBigUint64(8, true),
        nodeId: dv.getUint32(16, true), certId: dv.getUint32(20, true), digest: digest8(dv, 24),
        event: dv.getUint8(32), certKind: dv.getUint8(33), indexI: dv.getUint16(34, true),
        indexJ: dv.getUint16(36, true), count: dv.getUint16(38, true) };
    case ChannelId.DET_OBSERVATION:
      return { channel: "det.observation", nodeId: dv.getUint32(0, true), strDetector: dv.getUint32(4, true),
        subjectDigest: digest8(dv, 8), score: dv.getFloat32(16, true), subjectActorId: dv.getUint32(20, true),
        evidenceCount: dv.getUint16(24, true), detectorKind: dv.getUint8(26), provId: dv.getUint32(28, true) };
    case ChannelId.APP_WARNING:
      return { channel: "app.warning", nodeId: dv.getUint32(0, true), strApp: dv.getUint32(4, true),
        subjectDigest: digest8(dv, 8), ttcS: dv.getFloat32(16, true), distanceM: dv.getFloat32(20, true),
        kind: dv.getUint8(24), severity: dv.getUint8(25), truth: dv.getUint8(26), subjectActorId: dv.getUint32(28, true) };
    case ChannelId.PROTO_REVOCATION:
      return { channel: "proto.revocation", subjectNodeId: dv.getUint32(0, true), revocationId: dv.getUint32(4, true),
        subjectDigest: digest8(dv, 8), sizeBytes: dv.getBigUint64(16, true), nodeId: dv.getUint32(24, true),
        stage: dv.getUint8(28), mechanism: dv.getUint8(29), entries: dv.getUint16(30, true) };
    case ChannelId.GT_KINEMATICS:
      return { channel: "gt.kinematics", actorId: dv.getUint32(0, true), laneId: dv.getUint32(4, true),
        posXM: dv.getFloat32(8, true), posYM: dv.getFloat32(12, true), posZM: dv.getFloat32(16, true),
        velXMps: dv.getFloat32(20, true), velYMps: dv.getFloat32(24, true), velZMps: dv.getFloat32(28, true),
        accXMps2: dv.getFloat32(32, true), accYMps2: dv.getFloat32(36, true), accZMps2: dv.getFloat32(40, true),
        headingRad: dv.getFloat32(44, true), yawRateRadS: dv.getFloat32(48, true), laneSM: dv.getFloat32(52, true) };
    case ChannelId.GT_ATTACK_ACTION:
      return { channel: "gt.attack.action", actorId: dv.getUint32(0, true), nodeId: dv.getUint32(4, true),
        msgId: dv.getUint32(8, true), strAttackId: dv.getUint32(12, true), fieldsChanged: dv.getUint32(16, true),
        magnitude: dv.getFloat32(20, true), action: dv.getUint8(24), coalitionId: dv.getUint8(25),
        provId: dv.getUint32(28, true) };
    case ChannelId.MAC_CBR:
      return { channel: "mac.cbr", nodeId: dv.getUint32(0, true), cbr: dv.getFloat32(4, true),
        channelNumber: dv.getUint16(8, true), dccState: dv.getUint16(10, true), txPowerCdbm: dv.getInt16(12, true) };
    case ChannelId.NET_FRAG:
      return { channel: "net.frag", nodeId: dv.getUint32(0, true), sduId: dv.getUint32(4, true),
        sduBytes: dv.getUint32(8, true), fragmentsTotal: dv.getUint16(12, true),
        fragmentsReceived: dv.getUint16(14, true), msgType: dv.getUint16(16, true),
        outcome: dv.getUint8(18), direction: dv.getUint8(19) };
    case ChannelId.NODE_NEIGHBOR:
      return { channel: "node.neighbor", nodeId: dv.getUint32(0, true), peerDigest: digest8(dv, 8),
        relevance: dv.getFloat32(16, true), tableSizeAfter: dv.getUint16(20, true), op: dv.getUint8(22),
        verifyState: dv.getUint8(23), peerActorId: dv.getUint32(24, true), provId: dv.getUint32(28, true) };
    case ChannelId.PROTO_MSG:
      return { channel: "proto.msg", fromNode: dv.getUint32(0, true), toNode: dv.getUint32(4, true),
        strFlow: dv.getUint32(8, true), bytes: dv.getUint32(12, true), flowInstanceId: dv.getUint32(16, true),
        step: dv.getUint16(20, true), transport: dv.getUint8(22), outcome: dv.getUint8(23),
        latencyMs: dv.getFloat32(24, true), provId: dv.getUint32(28, true) };
    case ChannelId.MA_REPORT:
    case ChannelId.MA_CASE:
    case ChannelId.MA_DECISION:
      return { channel: channelId === ChannelId.MA_REPORT ? "ma.report" : channelId === ChannelId.MA_CASE ? "ma.case" : "ma.decision",
        caseId: dv.getUint32(0, true), reportId: dv.getUint32(4, true), subjectDigest: digest8(dv, 8),
        reporterNodeId: dv.getUint32(16, true), confidence: dv.getFloat32(20, true), strPipeline: dv.getUint32(24, true),
        subjectActorId: dv.getUint32(28, true), reportsInCase: dv.getUint16(32, true), kind: dv.getUint8(34),
        reporterKind: dv.getUint8(35), provId: dv.getUint32(36, true) };
    default:
      return { channel: "unknown", channelId, bytes: new Uint8Array(dv.buffer, dv.byteOffset, dv.byteLength) };
  }
}

/** §3.6 — a decoded `Event` batch. Payloads are resolved on demand. */
export interface EventMessage {
  readonly kind: "event";
  readonly header: FrameHeader;
  readonly tStartNs: bigint;
  readonly tEndNs: bigint;
  readonly count: number;
  /** Index columns, sorted by `(sim_time_ns, channel_id)` ascending. */
  readonly index: {
    readonly simTimeNs: BigUint64Array;
    /** byte offset relative to `off_payloads`, a multiple of 8. */ readonly payloadOff: Uint32Array;
    /** including padding to 8. */ readonly payloadLen: Uint16Array;
    readonly channelId: Uint16Array;
  };
  /** The whole payload region, for forwarding it untouched (e.g. across a worker boundary). */
  readonly payloads: Uint8Array;
  /** A `DataView` over event `i`'s payload region. */ payloadView(i: number): DataView;
  /** Decode event `i`'s payload by its channel id. */ payload(i: number): EventPayload;
}

/** Decode an `Event` frame (§3.6). */
export function decodeEvent(v: FrameView): EventMessage {
  const dv = bodyView(v);
  const O = EVENT_OFFSETS;
  if (v.bodyLen < EVENT_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Event body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  }
  const offIndex = dv.getUint32(O.offIndex, true);
  const offPayloads = dv.getUint32(O.offPayloads, true);
  const E = sectionCount(dv.getUint32(O.eventCount, true), offIndex, "Event.index");
  // §2.2 — the payload region is sized by `payload_bytes`, not by `event_count`: E events whose
  // `payload_len` is all zero is a region of no bytes, and the encoder still writes a non-zero
  // `off_payloads` for it. Keyed on its own extent, the sentinel says what it should.
  const payloadBytes = sectionCount(dv.getUint32(O.payloadBytes, true), offPayloads, "Event.payloads");

  const index =
    E === 0
      ? { simTimeNs: new BigUint64Array(0), payloadOff: new Uint32Array(0), payloadLen: new Uint16Array(0), channelId: new Uint16Array(0) }
      : {
          simTimeNs: u64s(v, offIndex + 0 * E, E, "Event.index.sim_time_ns"),
          payloadOff: u32s(v, offIndex + 8 * E, E, "Event.index.payload_off"),
          payloadLen: u16s(v, offIndex + 12 * E, E, "Event.index.payload_len"),
          channelId: u16s(v, offIndex + 14 * E, E, "Event.index.channel_id"),
        };

  // §3.6.1 — `off_payloads` MUST be 8-aligned. `off_index` gets the same check from u64s() above.
  const payloadsAbs = payloadBytes === 0 ? v.bodyOffset : checkSection(v, offPayloads, payloadBytes, "Event.payloads");
  if (payloadBytes > 0) align(payloadsAbs, 8, "Event.off_payloads");

  // §3.6.1 / §10.4 C4 — the index MUST be sorted by (sim_time_ns, channel_id) ascending. A reader
  // that trusts it silently mis-orders a timeline, so check it once per frame (O(E), no allocation).
  for (let i = 1; i < E; i++) {
    const tPrev = index.simTimeNs[i - 1];
    const tHere = index.simTimeNs[i];
    if (tHere < tPrev || (tHere === tPrev && index.channelId[i] < index.channelId[i - 1])) {
      throw new ProtocolError(
        "bad_state",
        `Event index entry ${i} (t=${tHere}, channel=${index.channelId[i]}) precedes entry ${i - 1} (t=${tPrev}, channel=${index.channelId[i - 1]}); §3.6.1 requires ascending (sim_time_ns, channel_id)`,
        { offset: i, field: "Event.index" },
      );
    }
  }

  const payloadView = (i: number): DataView => {
    if (i < 0 || i >= E) throw new ProtocolError("bad_offset", `Event ${i} out of range (event_count = ${E})`, { offset: i });
    const off = index.payloadOff[i];
    const len = index.payloadLen[i];
    if (off + len > payloadBytes) {
      throw new ProtocolError("bad_offset", `Event ${i} payload [${off}, ${off + len}) exceeds payload_bytes ${payloadBytes}`, { offset: off });
    }
    // §3.6.1 — every `payload_off` MUST be a multiple of 8; otherwise the f32/u64 fields inside the
    // payload are read from misaligned offsets and decode to plausible nonsense.
    if (off % 8 !== 0) {
      throw new ProtocolError("misaligned", `Event ${i} payload_off ${off} is not a multiple of 8 (§3.6.1)`, {
        offset: off,
        expected: 8,
        field: "Event.index.payload_off",
      });
    }
    return new DataView(v.buffer, payloadsAbs + off, len);
  };

  return {
    kind: "event",
    header: v.header,
    tStartNs: dv.getBigUint64(O.tStartNs, true),
    tEndNs: dv.getBigUint64(O.tEndNs, true),
    count: E,
    index,
    payloads: new Uint8Array(v.buffer, payloadsAbs, payloadBytes),
    payloadView,
    payload: (i: number) => decodeEventPayload(index.channelId[i], payloadView(i)),
  };
}

// ---------------------------------------------------------------------------
// §3.7 — MetricSample (0x0006)
// ---------------------------------------------------------------------------

/** §3.7 — the 32-byte `MetricSample` prefix. */
export const METRIC_PREFIX_BYTES = 32;

/** §3.7 — body-relative byte offsets of the `MetricSample` prefix fields. */
export const METRIC_OFFSETS = {
  /** `u64` @0 — **end** of the time bin */ simTimeNs: 0,
  /** `u64` @8 */ binWidthNs: 8,
  /** `u32` @16 */ sampleCount: 16,
  /** `u32` @20 — MUST be 8-aligned */ offSamples: 20,
  /** `u32` @24 — 32 in v1 */ recordSize: 24,
  /** `u32` @28 */ reserved: 28,
} as const;

/** §3.7 — `record_size` in v1; readers stride by the wire value. */
export const METRIC_RECORD_BYTES_V1 = 32;

/** §3.7 — byte offsets within one 32-byte sample record. */
export const METRIC_RECORD_OFFSETS = {
  /** `f64` @0 */ value: 0,
  /** `u32` @8 */ strMetric: 8,
  /** `u32` @12 — `0` = no dimensions */ dimKey: 12,
  /** `u32` @16 — `0xFFFFFFFF` if not per-node */ nodeId: 16,
  /** `u32` @20 */ count: 20,
  /** `u16` @24 — `MetricAgg` */ agg: 24,
  /** `u8` @26 — 0 GT, 1 NODE, 2 PUBLIC, 3 DERIVED */ visibility: 26,
  /** `u8` @27 */ reserved: 27,
  /** `u32` @28 — `0` none */ provId: 28,
} as const;

/** §3.7 — one decoded metric sample. */
export interface MetricRecord {
  readonly value: number;
  readonly strMetric: number;
  readonly dimKey: number;
  readonly nodeId: number;
  readonly count: number;
  readonly agg: number;
  readonly visibility: number;
  readonly provId: number;
}

/** §3.7 — a decoded `MetricSample` frame. */
export interface MetricSampleMessage {
  readonly kind: "metric";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly binWidthNs: bigint;
  readonly sampleCount: number;
  readonly recordSize: number;
  /** The whole record region, for forwarding it untouched. */ readonly raw: Uint8Array;
  sample(i: number): MetricRecord;
  samples(): MetricRecord[];
}

/** Decode a `MetricSample` frame (§3.7). */
export function decodeMetricSample(v: FrameView): MetricSampleMessage {
  const dv = bodyView(v);
  const O = METRIC_OFFSETS;
  if (v.bodyLen < METRIC_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `MetricSample body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  }
  const M = dv.getUint32(O.sampleCount, true);
  const offSamples = dv.getUint32(O.offSamples, true);
  const recordSize = dv.getUint32(O.recordSize, true);
  if (M > 0 && recordSize < METRIC_RECORD_BYTES_V1) {
    throw new ProtocolError("bad_length", `MetricSample.record_size ${recordSize} is smaller than the v1 record (32)`, {
      actual: recordSize, expected: METRIC_RECORD_BYTES_V1,
    });
  }
  // §2.2 — same sentinel, same failure: probed before the fix, a one-sample frame with
  // `off_samples = 0` decoded value 4.94e-315 and str_metric 1000000000 — the prefix's
  // `sim_time_ns` and `bin_width_ns` read as a sample — and raised nothing.
  const total = sectionCount(M, offSamples, "MetricSample.samples") * recordSize;
  const abs = M === 0 ? v.bodyOffset : checkSection(v, offSamples, total, "MetricSample.samples");
  const rec = new DataView(v.buffer, abs, total);
  const R = METRIC_RECORD_OFFSETS;
  const read = (i: number): MetricRecord => {
    if (i < 0 || i >= M) throw new ProtocolError("bad_offset", `MetricSample ${i} out of range (sample_count = ${M})`, { offset: i });
    const b = i * recordSize;
    return {
      value: rec.getFloat64(b + R.value, true),
      strMetric: rec.getUint32(b + R.strMetric, true),
      dimKey: rec.getUint32(b + R.dimKey, true),
      nodeId: rec.getUint32(b + R.nodeId, true),
      count: rec.getUint32(b + R.count, true),
      agg: rec.getUint16(b + R.agg, true),
      visibility: rec.getUint8(b + R.visibility),
      provId: rec.getUint32(b + R.provId, true),
    };
  };
  return {
    kind: "metric",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    binWidthNs: dv.getBigUint64(O.binWidthNs, true),
    sampleCount: M,
    recordSize,
    raw: new Uint8Array(v.buffer, abs, total),
    sample: read,
    samples: () => {
      const out: MetricRecord[] = new Array<MetricRecord>(M);
      for (let i = 0; i < M; i++) out[i] = read(i);
      return out;
    },
  };
}

// ---------------------------------------------------------------------------
// §3.8 — Provenance (0x0007)
// ---------------------------------------------------------------------------

/** §3.8 — the 32-byte `Provenance` prefix. */
export const PROVENANCE_PREFIX_BYTES = 32;

/** §3.8 — body-relative byte offsets of the `Provenance` prefix fields. */
export const PROVENANCE_OFFSETS = {
  /** `u64` @0 */ simTimeNs: 0,
  /** `u32` @8 */ entryCount: 8,
  /** `u32` @12 */ offEntries: 12,
  /** `u32` @16 */ dimCount: 16,
  /** `u32` @20 */ offDims: 20,
  /** `u32` @24 — symbol-table extension, or `0` */ offStrings: 24,
  /** `u32` @28 */ flags: 28,
} as const;

/** §3.8 — `Provenance.flags` bits. */
export const ProvenanceFlags = { REPLACE_ALL: 1 << 0, FINAL: 1 << 1 } as const;

/** §3.8 — entry block, struct-of-arrays. */
export interface ProvenanceEntries {
  readonly count: number;
  readonly provId: Uint32Array;
  readonly strModelId: Uint32Array;
  readonly strModelVersion: Uint32Array;
  readonly strParamSetId: Uint32Array;
  readonly strCardUrl: Uint32Array;
  readonly family: Uint16Array;
  /** 0 value, 1 node, 2 link, 3 actor, 4 metric, 5 channel, 6 world. */ readonly subjectKind: Uint16Array;
}

/** §3.8 — dimension dictionary. */
export interface ProvenanceDims {
  readonly count: number;
  readonly dimKey: Uint32Array;
  readonly strDims: Uint32Array;
}

/** §3.8 — a decoded `Provenance` frame. */
export interface ProvenanceMessage {
  readonly kind: "provenance";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly flags: number;
  readonly entries: ProvenanceEntries;
  readonly dims: ProvenanceDims;
  /** Strings appended by this frame, in id order; the first takes id `table_size_before`. */
  readonly stringExtension: readonly string[];
}

/** Decode a `Provenance` frame (§3.8). */
export function decodeProvenance(v: FrameView): ProvenanceMessage {
  const dv = bodyView(v);
  const O = PROVENANCE_OFFSETS;
  if (v.bodyLen < PROVENANCE_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `Provenance body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  }
  const offEntries = dv.getUint32(O.offEntries, true);
  const offDims = dv.getUint32(O.offDims, true);
  const offStrings = dv.getUint32(O.offStrings, true);
  const P = sectionCount(dv.getUint32(O.entryCount, true), offEntries, "Provenance.entries");
  const Dk = sectionCount(dv.getUint32(O.dimCount, true), offDims, "Provenance.dims");

  const entries: ProvenanceEntries =
    P === 0
      ? { count: 0, provId: new Uint32Array(0), strModelId: new Uint32Array(0), strModelVersion: new Uint32Array(0),
          strParamSetId: new Uint32Array(0), strCardUrl: new Uint32Array(0), family: new Uint16Array(0), subjectKind: new Uint16Array(0) }
      : {
          count: P,
          provId: u32s(v, offEntries + 0 * P, P, "Provenance.entries.prov_id"),
          strModelId: u32s(v, offEntries + 4 * P, P, "Provenance.entries.str_model_id"),
          strModelVersion: u32s(v, offEntries + 8 * P, P, "Provenance.entries.str_model_version"),
          strParamSetId: u32s(v, offEntries + 12 * P, P, "Provenance.entries.str_param_set_id"),
          strCardUrl: u32s(v, offEntries + 16 * P, P, "Provenance.entries.str_card_url"),
          family: u16s(v, offEntries + 20 * P, P, "Provenance.entries.family"),
          subjectKind: u16s(v, offEntries + 22 * P, P, "Provenance.entries.subject_kind"),
        };

  const dims: ProvenanceDims =
    Dk === 0
      ? { count: 0, dimKey: new Uint32Array(0), strDims: new Uint32Array(0) }
      : {
          count: Dk,
          dimKey: u32s(v, offDims + 0 * Dk, Dk, "Provenance.dims.dim_key"),
          strDims: u32s(v, offDims + 4 * Dk, Dk, "Provenance.dims.str_dims"),
        };

  return {
    kind: "provenance",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    flags: dv.getUint32(O.flags, true),
    entries,
    dims,
    stringExtension: offStrings === 0 ? [] : strTableStrings(decodeStrTable(v.buffer, checkSection(v, offStrings, 8, "Provenance.strings"), v.bodyOffset + v.bodyLen)),
  };
}

// ---------------------------------------------------------------------------
// §3.9 — WorldChunk (0x0008)
// ---------------------------------------------------------------------------

/** §3.9 — the 64-byte `WorldChunk` prefix. */
export const WORLD_CHUNK_PREFIX_BYTES = 64;

/** §3.9 — body-relative byte offsets of the `WorldChunk` prefix fields. */
export const WORLD_CHUNK_OFFSETS = {
  /** `u8[32]` @0 — MUST equal `Hello.world_hash` */ worldHash: 0,
  /** `u64` @32 */ totalBytes: 32,
  /** `u32` @40 — 0-based */ chunkIndex: 40,
  /** `u32` @44 */ chunkCount: 44,
  /** `u32` @48 */ offPayload: 48,
  /** `u32` @52 */ payloadLen: 52,
  /** `u8` @56 — 0 binary, 1 JSON */ format: 56,
  /** `u32` @60 */ reserved32: 60,
} as const;

/** §3.9 — a decoded `WorldChunk`. */
export interface WorldChunkMessage {
  readonly kind: "world-chunk";
  readonly header: FrameHeader;
  readonly worldHash: Uint8Array;
  readonly totalBytes: bigint;
  readonly chunkIndex: number;
  readonly chunkCount: number;
  readonly format: number;
  readonly payload: Uint8Array;
  /** `FLAG_CONTINUED` was set: more chunks follow. */ readonly continued: boolean;
}

/** Decode a `WorldChunk` frame (§3.9). */
export function decodeWorldChunk(v: FrameView): WorldChunkMessage {
  const dv = bodyView(v);
  const O = WORLD_CHUNK_OFFSETS;
  if (v.bodyLen < WORLD_CHUNK_PREFIX_BYTES) {
    throw new ProtocolError("truncated", `WorldChunk body is ${v.bodyLen} bytes, the prefix alone is 64`, { actual: v.bodyLen });
  }
  const offPayload = dv.getUint32(O.offPayload, true);
  const payloadLen = dv.getUint32(O.payloadLen, true);
  return {
    kind: "world-chunk",
    header: v.header,
    worldHash: u8s(v, O.worldHash, 32, "WorldChunk.world_hash"),
    totalBytes: dv.getBigUint64(O.totalBytes, true),
    chunkIndex: dv.getUint32(O.chunkIndex, true),
    chunkCount: dv.getUint32(O.chunkCount, true),
    format: dv.getUint8(O.format),
    payload: payloadLen === 0 ? new Uint8Array(0) : u8s(v, offPayload, payloadLen, "WorldChunk.payload"),
    continued: (v.header.flags & FrameFlags.CONTINUED) !== 0,
  };
}

// ---------------------------------------------------------------------------
// §3.10 — Error (0x00FE), §3.11 — Bye (0x00FF)
// ---------------------------------------------------------------------------

/** §3.10 — body-relative byte offsets of the 32-byte `Error` prefix. */
export const ERROR_OFFSETS = {
  /** `u64` @0 */ simTimeNs: 0,
  /** `i32` @8 — JSON-RPC error numbering (§6.4) */ code: 8,
  /** `u32` @12 */ offStrings: 12,
  /** `u32` @16 */ strMessage: 16,
  /** `u32` @20 */ strDetail: 20,
  /** `u8` @24 — 1 means a `Bye` follows */ fatal: 24,
  /** `u32` @28 */ reserved32: 28,
} as const;

/** §3.10 — a decoded `Error` frame. */
export interface ErrorMessage {
  readonly kind: "error";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly code: number;
  readonly fatal: boolean;
  readonly message: string;
  readonly detail: string;
  readonly stringExtension: readonly string[];
}

/**
 * Decode an `Error` frame (§3.10). `str_message` and `str_detail` are ids in the **connection**
 * symbol table, extended by this frame's own table; pass the live table to resolve them.
 */
export function decodeError(v: FrameView, table?: StringTable): ErrorMessage {
  const dv = bodyView(v);
  const O = ERROR_OFFSETS;
  if (v.bodyLen < 32) throw new ProtocolError("truncated", `Error body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  const offStrings = dv.getUint32(O.offStrings, true);
  const ext = offStrings === 0 ? [] : strTableStrings(decodeStrTable(v.buffer, checkSection(v, offStrings, 8, "Error.strings"), v.bodyOffset + v.bodyLen));
  const strMessage = dv.getUint32(O.strMessage, true);
  const strDetail = dv.getUint32(O.strDetail, true);
  const base = table ? table.size : 0;
  const resolve = (id: number): string => (table && id < base ? table.get(id) : (ext[id - base] ?? ""));
  return {
    kind: "error",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    code: dv.getInt32(O.code, true),
    fatal: dv.getUint8(O.fatal) === 1,
    message: resolve(strMessage),
    detail: resolve(strDetail),
    stringExtension: ext,
  };
}

/** §3.11 — body-relative byte offsets of the 32-byte `Bye` prefix. */
export const BYE_OFFSETS = {
  /** `u64` @0 */ simTimeNs: 0,
  /** `u64` @8 */ canonicalFrames: 8,
  /** `u8` @16 — `ByeReason` */ reason: 16,
  /** `u32` @20 */ offStrings: 20,
  /** `u32` @24 */ strDetail: 24,
  /** `u32` @28 */ reserved32: 28,
} as const;

/** §3.11 / Appendix A — `ByeReason`. */
export const ByeReason = {
  RUN_COMPLETE: 0,
  CLIENT_REQUESTED: 1,
  SERVER_SHUTDOWN: 2,
  ERROR: 3,
  SUPERSEDED: 4,
} as const;
export type ByeReason = (typeof ByeReason)[keyof typeof ByeReason];

/** §3.11 — a decoded `Bye` frame. */
export interface ByeMessage {
  readonly kind: "bye";
  readonly header: FrameHeader;
  readonly simTimeNs: bigint;
  readonly canonicalFrames: bigint;
  readonly reason: number;
  readonly detail: string;
  readonly stringExtension: readonly string[];
}

/** Decode a `Bye` frame (§3.11). */
export function decodeBye(v: FrameView, table?: StringTable): ByeMessage {
  const dv = bodyView(v);
  const O = BYE_OFFSETS;
  if (v.bodyLen < 32) throw new ProtocolError("truncated", `Bye body is ${v.bodyLen} bytes, the prefix alone is 32`, { actual: v.bodyLen });
  const offStrings = dv.getUint32(O.offStrings, true);
  const ext = offStrings === 0 ? [] : strTableStrings(decodeStrTable(v.buffer, checkSection(v, offStrings, 8, "Bye.strings"), v.bodyOffset + v.bodyLen));
  const strDetail = dv.getUint32(O.strDetail, true);
  const base = table ? table.size : 0;
  return {
    kind: "bye",
    header: v.header,
    simTimeNs: dv.getBigUint64(O.simTimeNs, true),
    canonicalFrames: dv.getBigUint64(O.canonicalFrames, true),
    reason: dv.getUint8(O.reason),
    detail: table && strDetail < base ? table.get(strDetail) : (ext[strDetail - base] ?? ""),
    stringExtension: ext,
  };
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/** A frame whose `msg_type` this build does not know. §2.1: readers MUST ignore it, not fail. */
export interface UnknownMessage {
  readonly kind: "unknown";
  readonly header: FrameHeader;
  readonly body: Uint8Array;
}

/** Every server-to-client message VWP v1 defines. */
export type VwpMessage =
  | HelloMessage | KeyframeMessage | DeltaMessage | TelemetryMessage | EventMessage
  | MetricSampleMessage | ProvenanceMessage | WorldChunkMessage | ErrorMessage | ByeMessage
  | UnknownMessage;

/** Decode an already-validated {@link FrameView} into the message its `msg_type` names. */
export function decodeFrameView(v: FrameView, table?: StringTable): VwpMessage {
  switch (v.header.msgType) {
    case MsgType.Hello: return decodeHello(v);
    case MsgType.Keyframe: return decodeKeyframe(v);
    case MsgType.Delta: return decodeDelta(v);
    case MsgType.Telemetry: return decodeTelemetry(v);
    case MsgType.Event: return decodeEvent(v);
    case MsgType.MetricSample: return decodeMetricSample(v);
    case MsgType.Provenance: return decodeProvenance(v);
    case MsgType.WorldChunk: return decodeWorldChunk(v);
    case MsgType.Error: return decodeError(v, table);
    case MsgType.Bye: return decodeBye(v, table);
    default:
      return { kind: "unknown", header: v.header, body: new Uint8Array(v.buffer, v.bodyOffset, v.bodyLen) };
  }
}

/** Validate and decode a whole binary frame in one call. */
export function decodeMessage(frame: ArrayBuffer, options: DecodeOptions = {}, table?: StringTable): VwpMessage {
  return decodeFrameView(viewFrame(frame, options), table);
}
