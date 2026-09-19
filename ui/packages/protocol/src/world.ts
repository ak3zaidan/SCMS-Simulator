/**
 * The world payload `vwp-world/1` — docs/protocol/vwp-v1.md §4.
 *
 * Fetched with one HTTP `GET /world/{hash}.vwb` (§3.1.6) or delivered as `WorldChunk` frames
 * (§3.9). Same conventions as §2: little-endian, every scalar array aligned to its element size,
 * `off_* = 0` means "section absent", offsets are body-relative and the body starts at file
 * offset 16.
 *
 * Section decoders build typed-array views in place — nothing is copied. Array-of-structs sections
 * (junctions, signals, sites, crossings, landuse) expose strided views over the same bytes plus an
 * `at(i)` accessor.
 */

import { ProtocolError } from "./frame.js";
import { bytesToHex, decodeStrTable, sectionCount, strTableStrings } from "./messages.js";

/** §4.1 — `magic` = `0x444C5756`; wire bytes `56 57 4C 44` = `V W L D`. */
export const VWB_MAGIC = 0x444c5756;
/** §4.1 — `version` = 1. */
export const VWB_VERSION = 1;
/** §4.1 — the file header is 16 bytes; the body starts at file offset 16. */
export const VWB_HEADER_BYTES = 16;
/** §4 — the media type of the binary form. */
export const VWB_CONTENT_TYPE = "application/vnd.v2xw.world.v1";

/** §4.1 — file-header field offsets. */
export const VWB_HEADER_OFFSETS = {
  /** `u32` @0 */ magic: 0,
  /** `u16` @4 */ version: 4,
  /** `u16` @6 */ reserved: 6,
  /** `u32` @8 — uncompressed body bytes */ bodyLen: 8,
  /** `u16` @12 — bit0 zstd */ flags: 12,
  /** `u16` @14 */ reserved2: 14,
} as const;

/** §4.1 — `flags` bit 0. */
export const VWB_FLAG_ZSTD = 0x0001;

/** §4.2 — the 192-byte directory prefix, body-relative. */
export const VWB_DIRECTORY_BYTES = 192;

/** §4.2 — directory field offsets, body-relative. */
export const VWB_DIRECTORY_OFFSETS = {
  /** `u8[32]` @0 — SHA-256 of the body */ contentHash: 0,
  /** `f64` @32 */ originLatDeg: 32,
  /** `f64` @40 */ originLonDeg: 40,
  /** `f64` @48 */ originAltM: 48,
  /** `f64` @56 */ bboxMinXM: 56,
  /** `f64` @64 */ bboxMinYM: 64,
  /** `f64` @72 */ bboxMaxXM: 72,
  /** `f64` @80 */ bboxMaxYM: 80,
  /** `f32` @88 */ bboxMinZM: 88,
  /** `f32` @92 */ bboxMaxZM: 92,
  /** `u32` @96 */ laneCount: 96,
  /** `u32` @100 */ lanePointTotal: 100,
  /** `u32` @104 */ buildingCount: 104,
  /** `u32` @108 */ ringPointTotal: 108,
  /** `u32` @112 */ offLanes: 112,
  /** `u32` @116 */ offLanePoints: 116,
  /** `u32` @120 */ offBuildings: 120,
  /** `u32` @124 */ offRingPoints: 124,
  /** `u32` @128 */ junctionCount: 128,
  /** `u32` @132 */ offJunctions: 132,
  /** `u32` @136 */ signalCount: 136,
  /** `u32` @140 */ offSignals: 140,
  /** `u32` @144 */ siteCount: 144,
  /** `u32` @148 */ offSites: 148,
  /** `u32` @152 */ offStrings: 152,
  /** `u32` @156 */ crossingCount: 156,
  /** `u32` @160 */ offCrossings: 160,
  /** `u32` @164 */ landuseCount: 164,
  /** `u32` @168 */ offLanduse: 168,
  /** `u32` @172 */ offProvenanceJson: 172,
  /** `u32` @176 */ provenanceJsonBytes: 176,
} as const;

/** §4.3 — bytes per lane row. */
export const VWB_LANE_STRIDE = 36;
/** §4.4 — bytes per building row. */
export const VWB_BUILDING_STRIDE = 28;
/** §4.5 — bytes per junction record. */
export const VWB_JUNCTION_STRIDE = 24;
/** §4.5 — bytes per signal record. */
export const VWB_SIGNAL_STRIDE = 28;
/** §4.5 — bytes per site record. */
export const VWB_SITE_STRIDE = 32;
/** §4.5 — bytes per crossing record. */
export const VWB_CROSSING_STRIDE = 28;
/** §4.5 — bytes per landuse record. */
export const VWB_LANDUSE_STRIDE = 16;

/** §4.3 / Appendix A — `LaneType`. */
export const LANE_TYPES = ["drive", "bike", "sidewalk", "bus", "parking", "junction-internal", "crossing"] as const;
export type LaneType = (typeof LANE_TYPES)[number];

/** §4.3 — `allowed_classes` bitmask. */
export const ALLOWED_CLASSES = ["car", "truck", "bus", "moto", "bicycle", "pedestrian", "emergency", "rail"] as const;
export type AllowedClass = (typeof ALLOWED_CLASSES)[number];

/** §4.4 — building `material`. */
export const BUILDING_MATERIALS = ["unknown", "concrete", "brick", "glass", "wood", "metal"] as const;
export type BuildingMaterial = (typeof BUILDING_MATERIALS)[number];

/** §4.4 — building `lod_hint`. */
export const LOD_HINTS = ["box", "box+roof", "detailed"] as const;
export type LodHint = (typeof LOD_HINTS)[number];

/** §4.5 / Appendix A — `JunctionControl`. */
export const JUNCTION_CONTROLS = ["none", "priority", "signal", "stop", "yield", "roundabout"] as const;
export type JunctionControl = (typeof JUNCTION_CONTROLS)[number];

/** §4.5 — signal `kind`. */
export const SIGNAL_KINDS = ["vehicle", "pedestrian", "bicycle", "transit"] as const;
export type SignalKind = (typeof SIGNAL_KINDS)[number];

/** §4.5 — site `kind`. */
export const SITE_KINDS = ["rsu", "cell", "other"] as const;
export type SiteKind = (typeof SITE_KINDS)[number];

/**
 * §4.5 — landuse `class`, in the specification's code order: `0` urban, `1` suburban,
 * `2` rural, `3` highway, **`4` water, `5` park**, `6` industrial.
 *
 * The order of the last two matters and used to be wrong here: this table listed `park`
 * at 4 and `water` at 5, so every payload the engine wrote decoded water as park and park
 * as water, and Central Park rendered as open water. §4.5 is normative and the Rust crate
 * follows it, so this table is what was wrong, not the payload. `world.test.ts` pins each
 * name against the code §4.5 documents it with, so the table cannot drift again.
 */
export const LANDUSE_CLASSES = ["urban", "suburban", "rural", "highway", "water", "park", "industrial"] as const;
export type LanduseClass = (typeof LANDUSE_CLASSES)[number];

const enumName = <T extends readonly string[]>(names: T, i: number): T[number] => (names[i] ?? names[0]) as T[number];

/** Decode an `allowed_classes` bitmask into class names (§4.3). */
export function allowedClassNames(mask: number): AllowedClass[] {
  const out: AllowedClass[] = [];
  for (let b = 0; b < ALLOWED_CLASSES.length; b++) if ((mask & (1 << b)) !== 0) out.push(ALLOWED_CLASSES[b]);
  return out;
}

/** Encode class names into an `allowed_classes` bitmask (§4.3). */
export function allowedClassMask(names: readonly string[]): number {
  let mask = 0;
  for (const n of names) {
    const b = ALLOWED_CLASSES.indexOf(n as AllowedClass);
    if (b >= 0) mask |= 1 << b;
  }
  return mask;
}

/** §4.3 — lane table, struct-of-arrays. */
export interface WorldLanes {
  readonly count: number;
  readonly laneId: Uint32Array;
  /** index of the first centreline point in the point arrays */ readonly pointOff: Uint32Array;
  /** ≥ 2 */ readonly pointCount: Uint32Array;
  readonly edgeId: Uint32Array;
  /** `0xFFFFFFFF` unless internal to a junction */ readonly junctionId: Uint32Array;
  readonly strName: Uint32Array;
  readonly widthM: Float32Array;
  readonly speedLimitMps: Float32Array;
  readonly allowedClasses: Uint16Array;
  readonly laneType: Uint8Array;
  /** 0 = rightmost in the direction of travel */ readonly indexInEdge: Uint8Array;
}

/** §4.3 — lane centreline points, three parallel `f32` arrays in travel order. */
export interface WorldLanePoints {
  readonly count: number;
  readonly x: Float32Array;
  readonly y: Float32Array;
  readonly z: Float32Array;
}

/** §4.4 — building table, struct-of-arrays. */
export interface WorldBuildings {
  readonly count: number;
  readonly buildingId: Uint32Array;
  readonly ringOff: Uint32Array;
  /** ≥ 3 */ readonly ringCount: Uint32Array;
  readonly heightM: Float32Array;
  readonly baseZM: Float32Array;
  readonly strName: Uint32Array;
  readonly material: Uint8Array;
  readonly lodHint: Uint8Array;
  /** storeys, `0xFFFF` unknown */ readonly levels: Uint16Array;
}

/** §4.4 — outer-ring points, counter-clockwise and **not** closed. */
export interface WorldRingPoints {
  readonly count: number;
  readonly x: Float32Array;
  readonly y: Float32Array;
}

/** §4.5 — one junction. */
export interface WorldJunction {
  readonly junctionId: number;
  readonly strName: number;
  readonly xM: number;
  readonly yM: number;
  readonly zM: number;
  readonly control: number;
  readonly laneCount: number;
}
/** §4.5 — one signal head. */
export interface WorldSignal {
  readonly signalId: number;
  readonly junctionId: number;
  readonly laneId: number;
  readonly xM: number;
  readonly yM: number;
  readonly zM: number;
  readonly kind: number;
  readonly group: number;
}
/** §4.5 — one infrastructure site (RSU, cell). */
export interface WorldSite {
  readonly siteId: number;
  /** `0xFFFFFFFF` if unassigned */ readonly nodeId: number;
  readonly xM: number;
  readonly yM: number;
  readonly zM: number;
  readonly antennaHeightM: number;
  readonly antennaGainDbi: number;
  readonly kind: number;
}
/** §4.5 — one pedestrian crossing. */
export interface WorldCrossing {
  readonly crossingId: number;
  readonly junctionId: number;
  readonly x1M: number;
  readonly y1M: number;
  readonly x2M: number;
  readonly y2M: number;
  readonly widthM: number;
}
/** §4.5 — one landuse zone; its ring lives in the building ring arrays. */
export interface WorldLanduse {
  readonly landuseId: number;
  readonly ringOff: number;
  readonly ringCount: number;
  readonly classIdx: number;
}

/** An array-of-structs section: strided typed views over the same bytes, plus a record accessor. */
export interface AosSection<T> {
  readonly count: number;
  readonly stride: number;
  readonly words: Uint32Array;
  readonly floats: Float32Array;
  readonly bytes: Uint8Array;
  readonly halves: Uint16Array;
  at(i: number): T;
}

/** §4 — a decoded `vwp-world/1` payload. */
export interface VwpWorld {
  readonly contentHash: string;
  readonly origin: { readonly latDeg: number; readonly lonDeg: number; readonly altM: number };
  readonly bbox: {
    readonly minXM: number; readonly minYM: number; readonly maxXM: number; readonly maxYM: number;
    readonly minZM: number; readonly maxZM: number;
  };
  readonly lanes: WorldLanes;
  readonly lanePoints: WorldLanePoints;
  readonly buildings: WorldBuildings;
  readonly ringPoints: WorldRingPoints;
  readonly junctions: AosSection<WorldJunction>;
  readonly signals: AosSection<WorldSignal>;
  readonly sites: AosSection<WorldSite>;
  readonly crossings: AosSection<WorldCrossing>;
  readonly landuse: AosSection<WorldLanduse>;
  readonly strings: readonly string[];
  /** §4.5 — the serialised `WorldProvenance`, already parsed. */ readonly provenance: WorldProvenanceJson | null;
  /** Resolve a world-scoped string id. */ str(id: number): string;
}

/** §4.5 — the `WorldProvenance` document carried as UTF-8 JSON. */
export interface WorldProvenanceJson {
  readonly source?: string;
  readonly bbox?: readonly number[];
  readonly imported_at?: string;
  readonly tool_versions?: Readonly<Record<string, string>>;
  readonly transformations?: readonly string[];
  readonly licence?: string;
  readonly dropped?: Readonly<Record<string, number>>;
}

function requireRange(buffer: ArrayBuffer, abs: number, bytes: number, what: string): number {
  if (abs < 0 || bytes < 0 || abs + bytes > buffer.byteLength) {
    throw new ProtocolError("bad_offset", `${what}: [${abs}, ${abs + bytes}) is outside the ${buffer.byteLength}-byte world payload`, {
      offset: abs, field: what,
    });
  }
  return abs;
}

/**
 * §4.3 / §4.4 — every row that indexes another section must lie inside it.
 *
 * `lanes.point_off/point_count` index the lane-point arrays and `buildings.ring_off/ring_count`
 * and `landuse.ring_off/ring_count` index the ring-point arrays, but nothing checked either pair
 * against the directory's `lane_point_total` / `ring_point_total`. A typed array reads out of
 * range as `undefined` rather than throwing, so an overrunning row produced *shape*, not an error:
 * probed on the real Manhattan payload with `point_off[0]` pushed 1,000 past the total,
 * `worldToJson` emitted `centreline: [null, null, null, …]` and the renderer would have built
 * NaN geometry from it. The directory arrives over HTTP, so it is untrusted input.
 */
function checkRowRanges(offs: Uint32Array, counts: Uint32Array, total: number, what: string): void {
  for (let i = 0; i < offs.length; i++) {
    const off = offs[i];
    const n = counts[i];
    if (off + n > total) {
      throw new ProtocolError(
        "bad_offset",
        `${what}[${i}] spans [${off}, ${off + n}) but only ${total} points were placed`,
        { offset: off, expected: total, actual: off + n, field: what },
      );
    }
  }
}

/**
 * Build an array-of-structs section view. `count` must already be 0 when the directory said the
 * section is absent (`off_* = 0`); a `base` of 0 is legitimate for a standalone buffer, so it is
 * not itself taken to mean "absent".
 */
function aos<T>(
  buffer: ArrayBuffer, base: number, count: number, stride: number, what: string,
  read: (s: AosSectionBase, i: number) => T,
): AosSection<T> {
  if (count === 0) {
    return {
      count: 0, stride,
      words: new Uint32Array(0), floats: new Float32Array(0), bytes: new Uint8Array(0), halves: new Uint16Array(0),
      at: (i: number) => {
        throw new ProtocolError("bad_offset", `${what}[${i}] out of range (section is empty)`, { offset: i });
      },
    };
  }
  const abs = requireRange(buffer, base, count * stride, what);
  if (abs % 4 !== 0) throw new ProtocolError("misaligned", `${what} at ${abs} is not 4-aligned`, { offset: abs });
  const section: AosSectionBase = {
    words: new Uint32Array(buffer, abs, (count * stride) / 4),
    floats: new Float32Array(buffer, abs, (count * stride) / 4),
    bytes: new Uint8Array(buffer, abs, count * stride),
    halves: new Uint16Array(buffer, abs, (count * stride) / 2),
    stride,
  };
  return {
    count, stride,
    words: section.words, floats: section.floats, bytes: section.bytes, halves: section.halves,
    at: (i: number) => {
      if (i < 0 || i >= count) throw new ProtocolError("bad_offset", `${what}[${i}] out of range (count = ${count})`, { offset: i });
      return read(section, i);
    },
  };
}

interface AosSectionBase {
  readonly words: Uint32Array;
  readonly floats: Float32Array;
  readonly bytes: Uint8Array;
  readonly halves: Uint16Array;
  readonly stride: number;
}

/**
 * Parse a `vwp-world/1` binary payload (§4).
 *
 * `payload` is the whole file, file header included. A zstd-compressed body (flags bit 0) needs a
 * decompressor passed in `options.decompress`, for the same reason as §2.6.
 */
export function decodeWorld(
  payload: ArrayBuffer,
  options: { readonly decompress?: (compressed: Uint8Array, uncompressedLen: number) => Uint8Array } = {},
): VwpWorld {
  if (payload.byteLength < VWB_HEADER_BYTES) {
    throw new ProtocolError("truncated", `world payload is ${payload.byteLength} bytes, the file header alone is 16`, {
      actual: payload.byteLength,
    });
  }
  const fh = new DataView(payload, 0, VWB_HEADER_BYTES);
  const magic = fh.getUint32(VWB_HEADER_OFFSETS.magic, true);
  if (magic !== VWB_MAGIC) {
    throw new ProtocolError("bad_magic", `bad world magic 0x${magic.toString(16).padStart(8, "0")}, expected 0x444c5756 ("VWLD")`, {
      expected: VWB_MAGIC, actual: magic,
    });
  }
  const version = fh.getUint16(VWB_HEADER_OFFSETS.version, true);
  if (version !== VWB_VERSION) {
    throw new ProtocolError("bad_version", `unsupported vwp-world version ${version}`, { expected: VWB_VERSION, actual: version });
  }
  const bodyLen = fh.getUint32(VWB_HEADER_OFFSETS.bodyLen, true);
  const flags = fh.getUint16(VWB_HEADER_OFFSETS.flags, true);

  let body: ArrayBuffer;
  if ((flags & VWB_FLAG_ZSTD) !== 0) {
    if (!options.decompress) {
      throw new ProtocolError("compressed_unsupported", "world payload is zstd-compressed but no decompressor was supplied");
    }
    const out = options.decompress(new Uint8Array(payload, VWB_HEADER_BYTES), bodyLen);
    body = out.byteOffset === 0 && out.byteLength === out.buffer.byteLength ? (out.buffer as ArrayBuffer) : (out.slice().buffer as ArrayBuffer);
  } else {
    // §4.1 — `body_len` is a declared extent and the payload arrives over HTTP, so it is checked
    // against the bytes actually served before it is used. `ArrayBuffer.slice` clamps silently, so
    // without this a file declaring a body longer than itself decoded happily from a short body and
    // every section bound was then measured against the wrong length. (Trailing bytes past the
    // declared body are left to the §4.2 digest, which covers everything after the file header and
    // so refuses a padded payload in `verifyWorldPayload` before decoding is reached.)
    if (VWB_HEADER_BYTES + bodyLen > payload.byteLength) {
      throw new ProtocolError(
        "truncated",
        `world header declares a ${bodyLen}-byte body but the payload is ${payload.byteLength} bytes (${payload.byteLength - VWB_HEADER_BYTES} after the §4.1 header)`,
        { expected: VWB_HEADER_BYTES + bodyLen, actual: payload.byteLength, field: "world.body_len" },
      );
    }
    // Re-home the body at offset 0 so every body-relative offset is also buffer-aligned (§2.2).
    body = payload.slice(VWB_HEADER_BYTES, VWB_HEADER_BYTES + bodyLen);
  }
  if (body.byteLength < VWB_DIRECTORY_BYTES) {
    throw new ProtocolError("truncated", `world body is ${body.byteLength} bytes, the directory alone is 192`, { actual: body.byteLength });
  }

  const D = VWB_DIRECTORY_OFFSETS;
  const dv = new DataView(body);
  const u32 = (o: number): number => dv.getUint32(o, true);
  const f64 = (o: number): number => dv.getFloat64(o, true);

  const offLanes = u32(D.offLanes);
  const offLanePoints = u32(D.offLanePoints);
  const offBuildings = u32(D.offBuildings);
  const offRingPoints = u32(D.offRingPoints);
  // §2.2 / §4 — `off_* = 0` means the section is absent. This used to *derive* the count from the
  // offset — `off === 0 ? 0 : count` — which reads the sentinel backwards: a payload declaring 920
  // lanes with `off_lanes = 0` decoded to zero lanes and rendered an empty city, with no error and
  // nothing to notice. The §4.2 digest cannot catch it either, since the payload's own
  // `content_hash` is computed over the same bad bytes. Validated in the same direction as every
  // §3 decoder instead: zero count means absent, non-zero count with a zero offset is invalid.
  const L = sectionCount(u32(D.laneCount), offLanes, "world.lanes");
  const lanePointTotal = sectionCount(u32(D.lanePointTotal), offLanePoints, "world.lane_points");
  const B = sectionCount(u32(D.buildingCount), offBuildings, "world.buildings");
  const ringPointTotal = sectionCount(u32(D.ringPointTotal), offRingPoints, "world.ring_points");
  const offStrings = u32(D.offStrings);
  const offProv = u32(D.offProvenanceJson);
  const provBytes = u32(D.provenanceJsonBytes);

  // §2.2 — a column base must be a multiple of its element size. `aos()` already checked this;
  // these helpers did not, and `new Uint32Array(body, 193, n)` throws a bare `RangeError` rather
  // than a `ProtocolError`, so a one-byte-misaligned `off_lanes` escaped the decoder's error
  // contract entirely and reached the caller as "start offset of Uint32Array should be a multiple
  // of 4". Bounds passing is not the same as the offset being usable.
  const colBase = (off: number, n: number, elem: number, what: string): number => {
    const abs = requireRange(body, off, n * elem, what);
    if (abs % elem !== 0) {
      throw new ProtocolError("misaligned", `${what}: byte offset ${abs} is not a multiple of ${elem} (§2.2)`, {
        offset: abs, expected: elem, field: what,
      });
    }
    return abs;
  };
  const u32col = (off: number, n: number, what: string): Uint32Array =>
    n === 0 ? new Uint32Array(0) : new Uint32Array(body, colBase(off, n, 4, what), n);
  const f32col = (off: number, n: number, what: string): Float32Array =>
    n === 0 ? new Float32Array(0) : new Float32Array(body, colBase(off, n, 4, what), n);
  const u16col = (off: number, n: number, what: string): Uint16Array =>
    n === 0 ? new Uint16Array(0) : new Uint16Array(body, colBase(off, n, 2, what), n);
  const u8col = (off: number, n: number, what: string): Uint8Array =>
    n === 0 ? new Uint8Array(0) : new Uint8Array(body, requireRange(body, off, n, what), n);

  const lanes: WorldLanes = {
    count: L,
    laneId: u32col(offLanes + 0 * L, L, "world.lanes.lane_id"),
    pointOff: u32col(offLanes + 4 * L, L, "world.lanes.point_off"),
    pointCount: u32col(offLanes + 8 * L, L, "world.lanes.point_count"),
    edgeId: u32col(offLanes + 12 * L, L, "world.lanes.edge_id"),
    junctionId: u32col(offLanes + 16 * L, L, "world.lanes.junction_id"),
    strName: u32col(offLanes + 20 * L, L, "world.lanes.str_name"),
    widthM: f32col(offLanes + 24 * L, L, "world.lanes.width_m"),
    speedLimitMps: f32col(offLanes + 28 * L, L, "world.lanes.speed_limit_mps"),
    allowedClasses: u16col(offLanes + 32 * L, L, "world.lanes.allowed_classes"),
    laneType: u8col(offLanes + 34 * L, L, "world.lanes.lane_type"),
    indexInEdge: u8col(offLanes + 35 * L, L, "world.lanes.index_in_edge"),
  };

  const T = lanePointTotal;
  const lanePoints: WorldLanePoints = {
    count: T,
    x: f32col(offLanePoints + 0 * T, T, "world.lane_points.x"),
    y: f32col(offLanePoints + 4 * T, T, "world.lane_points.y"),
    z: f32col(offLanePoints + 8 * T, T, "world.lane_points.z"),
  };

  const buildings: WorldBuildings = {
    count: B,
    buildingId: u32col(offBuildings + 0 * B, B, "world.buildings.building_id"),
    ringOff: u32col(offBuildings + 4 * B, B, "world.buildings.ring_off"),
    ringCount: u32col(offBuildings + 8 * B, B, "world.buildings.ring_count"),
    heightM: f32col(offBuildings + 12 * B, B, "world.buildings.height_m"),
    baseZM: f32col(offBuildings + 16 * B, B, "world.buildings.base_z_m"),
    strName: u32col(offBuildings + 20 * B, B, "world.buildings.str_name"),
    material: u8col(offBuildings + 24 * B, B, "world.buildings.material"),
    lodHint: u8col(offBuildings + 25 * B, B, "world.buildings.lod_hint"),
    levels: u16col(offBuildings + 26 * B, B, "world.buildings.levels"),
  };

  const R = ringPointTotal;
  const ringPoints: WorldRingPoints = {
    count: R,
    x: f32col(offRingPoints + 0 * R, R, "world.ring_points.x"),
    y: f32col(offRingPoints + 4 * R, R, "world.ring_points.y"),
  };

  const junctions = aos<WorldJunction>(body, u32(D.offJunctions), sectionCount(u32(D.junctionCount), u32(D.offJunctions), "world.junctions"), VWB_JUNCTION_STRIDE, "world.junctions", (s, i) => {
    const w = i * 6;
    const b = i * VWB_JUNCTION_STRIDE;
    return {
      junctionId: s.words[w], strName: s.words[w + 1],
      xM: s.floats[w + 2], yM: s.floats[w + 3], zM: s.floats[w + 4],
      control: s.bytes[b + 20], laneCount: s.halves[i * 12 + 11],
    };
  });

  const signals = aos<WorldSignal>(body, u32(D.offSignals), sectionCount(u32(D.signalCount), u32(D.offSignals), "world.signals"), VWB_SIGNAL_STRIDE, "world.signals", (s, i) => {
    const w = i * 7;
    const b = i * VWB_SIGNAL_STRIDE;
    return {
      signalId: s.words[w], junctionId: s.words[w + 1], laneId: s.words[w + 2],
      xM: s.floats[w + 3], yM: s.floats[w + 4], zM: s.floats[w + 5],
      kind: s.bytes[b + 24], group: s.halves[i * 14 + 13],
    };
  });

  const sites = aos<WorldSite>(body, u32(D.offSites), sectionCount(u32(D.siteCount), u32(D.offSites), "world.sites"), VWB_SITE_STRIDE, "world.sites", (s, i) => {
    const w = i * 8;
    const b = i * VWB_SITE_STRIDE;
    return {
      siteId: s.words[w], nodeId: s.words[w + 1],
      xM: s.floats[w + 2], yM: s.floats[w + 3], zM: s.floats[w + 4],
      antennaHeightM: s.floats[w + 5], antennaGainDbi: s.floats[w + 6],
      kind: s.bytes[b + 28],
    };
  });

  const crossings = aos<WorldCrossing>(body, u32(D.offCrossings), sectionCount(u32(D.crossingCount), u32(D.offCrossings), "world.crossings"), VWB_CROSSING_STRIDE, "world.crossings", (s, i) => {
    const w = i * 7;
    return {
      crossingId: s.words[w], junctionId: s.words[w + 1],
      x1M: s.floats[w + 2], y1M: s.floats[w + 3], x2M: s.floats[w + 4], y2M: s.floats[w + 5],
      widthM: s.floats[w + 6],
    };
  });

  const landuse = aos<WorldLanduse>(body, u32(D.offLanduse), sectionCount(u32(D.landuseCount), u32(D.offLanduse), "world.landuse"), VWB_LANDUSE_STRIDE, "world.landuse", (s, i) => {
    const w = i * 4;
    const b = i * VWB_LANDUSE_STRIDE;
    return { landuseId: s.words[w], ringOff: s.words[w + 1], ringCount: s.words[w + 2], classIdx: s.bytes[b + 12] };
  });

  checkRowRanges(lanes.pointOff, lanes.pointCount, lanePoints.count, "world.lanes.point_off");
  checkRowRanges(buildings.ringOff, buildings.ringCount, ringPoints.count, "world.buildings.ring_off");
  // Landuse rings share the ring-point arrays with buildings (see `encodeWorld`'s `writeRing`).
  for (let i = 0; i < landuse.count; i++) {
    const off = landuse.words[i * 4 + 1];
    const n = landuse.words[i * 4 + 2];
    if (off + n > ringPoints.count) {
      throw new ProtocolError(
        "bad_offset",
        `world.landuse.ring_off[${i}] spans [${off}, ${off + n}) but only ${ringPoints.count} ring points were placed`,
        { offset: off, expected: ringPoints.count, actual: off + n, field: "world.landuse.ring_off" },
      );
    }
  }

  const strings = offStrings === 0 ? [""] : strTableStrings(decodeStrTable(body, requireRange(body, offStrings, 8, "world.strings")));
  // §4.2 — `provenance_json_bytes` is the count and `off_provenance_json` the offset, so the same
  // rule applies: a payload declaring a provenance block it does not place used to drop it silently,
  // and the "why was this world built this way" panel simply showed nothing.
  let provenance: WorldProvenanceJson | null = null;
  if (sectionCount(provBytes, offProv, "world.provenance") > 0) {
    const raw = new Uint8Array(body, requireRange(body, offProv, provBytes, "world.provenance"), provBytes);
    try {
      provenance = JSON.parse(new TextDecoder().decode(raw)) as WorldProvenanceJson;
    } catch {
      provenance = null;
    }
  }

  return {
    contentHash: bytesToHex(new Uint8Array(body, D.contentHash, 32)),
    origin: { latDeg: f64(D.originLatDeg), lonDeg: f64(D.originLonDeg), altM: f64(D.originAltM) },
    bbox: {
      minXM: f64(D.bboxMinXM), minYM: f64(D.bboxMinYM), maxXM: f64(D.bboxMaxXM), maxYM: f64(D.bboxMaxYM),
      minZM: dv.getFloat32(D.bboxMinZM, true), maxZM: dv.getFloat32(D.bboxMaxZM, true),
    },
    lanes, lanePoints, buildings, ringPoints, junctions, signals, sites, crossings, landuse,
    strings,
    provenance,
    str: (id: number) => strings[id] ?? "",
  };
}

/**
 * §4.2 / conformance W1, W3 — the **payload digest** of a world payload.
 *
 * Settled by the §4.2 amendment of 2026-09-18, which is what this function has always computed:
 *
 * > A writer MUST zero the 32 bytes at body offset 0, hash the **whole body** including those 32
 * > zero bytes, and then write the digest into them. A reader MUST verify by copying the body,
 * > zeroing the same 32 bytes, and hashing the copy.
 *
 * The specification now keeps two digests apart by name, and they are different numbers:
 *
 * - the **payload digest** — this one. It is the `{hash}` in `GET /world/{hash}.vwb`, the value
 *   stored at `content_hash` (§4.2), the value `Hello.world_hash` carries and the value §3.9 checks
 *   `WorldChunk` payloads against. `WorldPayload::content_hash` on the Rust side.
 * - the **geometry digest** — a hash of the engine's quantised model, independent of any wire
 *   format. `World::content_hash` on the Rust side. §3.1.1's gloss on `Hello.world_hash` names
 *   *that* one, which §4.2 records as a defect in §3.1.1: a server that sends the geometry digest
 *   produces a `Hello` whose `world_hash` resolves to no payload. Never verify against it.
 */
export async function computeWorldContentHash(payload: ArrayBuffer): Promise<string> {
  const body = new Uint8Array(payload.slice(VWB_HEADER_BYTES));
  body.fill(0, VWB_DIRECTORY_OFFSETS.contentHash, VWB_DIRECTORY_OFFSETS.contentHash + 32);
  const digest = await crypto.subtle.digest("SHA-256", body);
  return bytesToHex(new Uint8Array(digest));
}

/** Synchronous form of {@link computeWorldContentHash} for a host that has a SHA-256 to hand. */
export function computeWorldContentHashWith(payload: Uint8Array, sha256: (bytes: Uint8Array) => Uint8Array): string {
  const body = payload.slice(VWB_HEADER_BYTES);
  body.fill(0, VWB_DIRECTORY_OFFSETS.contentHash, VWB_DIRECTORY_OFFSETS.contentHash + 32);
  return bytesToHex(sha256(body).subarray(0, 32));
}

/** Lower-case hex of `Hello.world_hash`, `WorldChunk.world_hash` or an already-hex digest. */
function hashHex(expected: Uint8Array | string): string {
  return typeof expected === "string" ? expected.trim().toLowerCase() : bytesToHex(expected);
}

/**
 * §10.5 W3 — "verifies the world hash and refuses a mismatch".
 *
 * Takes the bytes actually served for `GET /world/{hash}.vwb` (or the concatenated §3.9
 * `WorldChunk` payloads, which §3.9 says are the same bytes), recomputes the §4.2 payload digest
 * and compares it against the digest the stream promised — `Hello.world_hash` (§3.1.1) or
 * `WorldChunk.world_hash` (§3.9), both of which carry that same payload digest, **not** the
 * geometry digest §3.1.1's gloss names.
 *
 * Returns the verified digest, so a caller can log or cache by it. Throws a typed
 * {@link ProtocolError} with code `hash_mismatch` otherwise, so the world is refused rather than
 * silently accepted: a mismatch means these are not the bytes this run was computed against.
 *
 * The in-body `content_hash` is checked in the same pass. On its own it proves nothing — it is
 * stored inside the very bytes being hashed, so a rewritten payload can carry a self-consistent
 * digest — but separating the two failures is the difference between "the server served the wrong
 * world" and "these bytes are damaged".
 */
export async function verifyWorldPayload(payload: ArrayBuffer, expected: Uint8Array | string): Promise<string> {
  return checkDigests(new Uint8Array(payload), expected, await computeWorldContentHash(payload));
}

/** Synchronous form of {@link verifyWorldPayload} for a host that has a SHA-256 to hand. */
export function verifyWorldPayloadWith(
  payload: Uint8Array,
  expected: Uint8Array | string,
  sha256: (bytes: Uint8Array) => Uint8Array,
): string {
  return checkDigests(payload, expected, computeWorldContentHashWith(payload, sha256));
}

/** Compare a recomputed §4.2 payload digest against the stored one and the promised one. */
function checkDigests(payload: Uint8Array, expected: Uint8Array | string, actual: string): string {
  const least = VWB_HEADER_BYTES + VWB_DIRECTORY_BYTES;
  if (payload.byteLength < least) {
    throw new ProtocolError(
      "truncated",
      `world payload is ${payload.byteLength} bytes, too short for the §4.1 header and the §4.2 directory`,
      { actual: payload.byteLength, expected: least, field: "world.payload" },
    );
  }
  const want = hashHex(expected);
  if (!/^[0-9a-f]{64}$/.test(want)) {
    throw new ProtocolError("bad_length", `the expected world hash is not a 64-character SHA-256 hex digest: "${want}"`, {
      actual: want, field: "Hello.world_hash",
    });
  }
  const at = VWB_HEADER_BYTES + VWB_DIRECTORY_OFFSETS.contentHash;
  const stored = bytesToHex(payload.subarray(at, at + 32));
  if (actual !== stored) {
    throw new ProtocolError(
      "hash_mismatch",
      `world payload §4.2 content_hash is ${stored} but the body hashes to ${actual}: the bytes are damaged`,
      { expected: stored, actual, field: "world.content_hash" },
    );
  }
  if (actual !== want) {
    throw new ProtocolError(
      "hash_mismatch",
      `the served world hashes to ${actual}, but the stream promised ${want} (§4.2 payload digest, §10.5 W3)`,
      { expected: want, actual, field: "Hello.world_hash" },
    );
  }
  return actual;
}

// ---------------------------------------------------------------------------
// §4.6 — the JSON mirror form
// ---------------------------------------------------------------------------

/** §4.6 — one lane in the JSON form; `centreline` is a flat `[x, y, z, x, y, z, …]` array. */
export interface WorldJsonLane {
  lane_id: number;
  edge_id: number;
  junction_id: number | null;
  name: string;
  width_m: number;
  speed_limit_mps: number;
  lane_type: LaneType;
  index_in_edge: number;
  allowed_classes: AllowedClass[];
  centreline: number[];
}
/** §4.6 — one building; `ring` is a flat `[x, y, x, y, …]` outer ring, CCW and not closed. */
export interface WorldJsonBuilding {
  building_id: number;
  height_m: number;
  base_z_m: number;
  levels: number | null;
  material: BuildingMaterial;
  lod_hint: LodHint;
  name: string;
  ring: number[];
}
/** §4.6 — one junction. */
export interface WorldJsonJunction {
  junction_id: number;
  name: string;
  x_m: number;
  y_m: number;
  z_m: number;
  control: JunctionControl;
  lane_count: number;
}
/** §4.6 — one signal head. */
export interface WorldJsonSignal {
  signal_id: number;
  junction_id: number;
  lane_id: number;
  x_m: number;
  y_m: number;
  z_m: number;
  kind: SignalKind;
  group: number;
}
/** §4.6 — one site. */
export interface WorldJsonSite {
  site_id: number;
  node_id: number | null;
  x_m: number;
  y_m: number;
  z_m: number;
  antenna_height_m: number;
  antenna_gain_dbi: number;
  kind: SiteKind;
}
/** §4.6 — one crossing. */
export interface WorldJsonCrossing {
  crossing_id: number;
  junction_id: number;
  x1_m: number;
  y1_m: number;
  x2_m: number;
  y2_m: number;
  width_m: number;
}
/** §4.6 — one landuse zone. */
export interface WorldJsonLanduse {
  landuse_id: number;
  class: LanduseClass;
  ring: number[];
}

/** §4.6 — the JSON transcription of a `vwp-world/1` payload. */
export interface VwpWorldJson {
  schema: "vwp-world/1";
  content_hash: string;
  origin: { lat_deg: number; lon_deg: number; alt_m: number };
  bbox: { min_x_m: number; min_y_m: number; max_x_m: number; max_y_m: number; min_z_m: number; max_z_m: number };
  lanes: WorldJsonLane[];
  buildings: WorldJsonBuilding[];
  junctions: WorldJsonJunction[];
  signals: WorldJsonSignal[];
  sites: WorldJsonSite[];
  crossings: WorldJsonCrossing[];
  landuse: WorldJsonLanduse[];
  provenance: WorldProvenanceJson | null;
}

/** Transcribe a decoded world into the §4.6 JSON form. */
export function worldToJson(w: VwpWorld): VwpWorldJson {
  const lanes: WorldJsonLane[] = [];
  for (let i = 0; i < w.lanes.count; i++) {
    const off = w.lanes.pointOff[i];
    const n = w.lanes.pointCount[i];
    const centreline: number[] = new Array<number>(n * 3);
    for (let k = 0; k < n; k++) {
      centreline[k * 3] = w.lanePoints.x[off + k];
      centreline[k * 3 + 1] = w.lanePoints.y[off + k];
      centreline[k * 3 + 2] = w.lanePoints.z[off + k];
    }
    lanes.push({
      lane_id: w.lanes.laneId[i],
      edge_id: w.lanes.edgeId[i],
      junction_id: w.lanes.junctionId[i] === 0xffffffff ? null : w.lanes.junctionId[i],
      name: w.str(w.lanes.strName[i]),
      width_m: w.lanes.widthM[i],
      speed_limit_mps: w.lanes.speedLimitMps[i],
      lane_type: enumName(LANE_TYPES, w.lanes.laneType[i]),
      index_in_edge: w.lanes.indexInEdge[i],
      allowed_classes: allowedClassNames(w.lanes.allowedClasses[i]),
      centreline,
    });
  }

  const ring = (off: number, n: number): number[] => {
    const out: number[] = new Array<number>(n * 2);
    for (let k = 0; k < n; k++) {
      out[k * 2] = w.ringPoints.x[off + k];
      out[k * 2 + 1] = w.ringPoints.y[off + k];
    }
    return out;
  };

  const buildings: WorldJsonBuilding[] = [];
  for (let i = 0; i < w.buildings.count; i++) {
    buildings.push({
      building_id: w.buildings.buildingId[i],
      height_m: w.buildings.heightM[i],
      base_z_m: w.buildings.baseZM[i],
      levels: w.buildings.levels[i] === 0xffff ? null : w.buildings.levels[i],
      material: enumName(BUILDING_MATERIALS, w.buildings.material[i]),
      lod_hint: enumName(LOD_HINTS, w.buildings.lodHint[i]),
      name: w.str(w.buildings.strName[i]),
      ring: ring(w.buildings.ringOff[i], w.buildings.ringCount[i]),
    });
  }

  const junctions: WorldJsonJunction[] = [];
  for (let i = 0; i < w.junctions.count; i++) {
    const j = w.junctions.at(i);
    junctions.push({
      junction_id: j.junctionId, name: w.str(j.strName), x_m: j.xM, y_m: j.yM, z_m: j.zM,
      control: enumName(JUNCTION_CONTROLS, j.control), lane_count: j.laneCount,
    });
  }

  const signals: WorldJsonSignal[] = [];
  for (let i = 0; i < w.signals.count; i++) {
    const s = w.signals.at(i);
    signals.push({
      signal_id: s.signalId, junction_id: s.junctionId, lane_id: s.laneId,
      x_m: s.xM, y_m: s.yM, z_m: s.zM, kind: enumName(SIGNAL_KINDS, s.kind), group: s.group,
    });
  }

  const sites: WorldJsonSite[] = [];
  for (let i = 0; i < w.sites.count; i++) {
    const s = w.sites.at(i);
    sites.push({
      site_id: s.siteId, node_id: s.nodeId === 0xffffffff ? null : s.nodeId,
      x_m: s.xM, y_m: s.yM, z_m: s.zM,
      antenna_height_m: s.antennaHeightM, antenna_gain_dbi: s.antennaGainDbi,
      kind: enumName(SITE_KINDS, s.kind),
    });
  }

  const crossings: WorldJsonCrossing[] = [];
  for (let i = 0; i < w.crossings.count; i++) {
    const c = w.crossings.at(i);
    crossings.push({
      crossing_id: c.crossingId, junction_id: c.junctionId,
      x1_m: c.x1M, y1_m: c.y1M, x2_m: c.x2M, y2_m: c.y2M, width_m: c.widthM,
    });
  }

  const landuse: WorldJsonLanduse[] = [];
  for (let i = 0; i < w.landuse.count; i++) {
    const l = w.landuse.at(i);
    landuse.push({ landuse_id: l.landuseId, class: enumName(LANDUSE_CLASSES, l.classIdx), ring: ring(l.ringOff, l.ringCount) });
  }

  return {
    schema: "vwp-world/1",
    content_hash: w.contentHash,
    origin: { lat_deg: w.origin.latDeg, lon_deg: w.origin.lonDeg, alt_m: w.origin.altM },
    bbox: {
      min_x_m: w.bbox.minXM, min_y_m: w.bbox.minYM, max_x_m: w.bbox.maxXM, max_y_m: w.bbox.maxYM,
      min_z_m: w.bbox.minZM, max_z_m: w.bbox.maxZM,
    },
    lanes, buildings, junctions, signals, sites, crossings, landuse,
    provenance: w.provenance,
  };
}

function packAos(count: number, stride: number, write: (dv: DataView, base: number, i: number) => void): ArrayBuffer {
  const buf = new ArrayBuffer(count * stride);
  const dv = new DataView(buf);
  for (let i = 0; i < count; i++) write(dv, i * stride, i);
  return buf;
}

/**
 * Build the same in-memory world from the §4.6 JSON form, so a client that fetched
 * `/world/{hash}.json` presents exactly the structure `decodeWorld` produces
 * (conformance `world_json_binary_parity`).
 */
export function worldFromJson(json: VwpWorldJson): VwpWorld {
  const strings: string[] = [""];
  const intern = (s: string): number => {
    if (s === "") return 0;
    const at = strings.indexOf(s);
    if (at >= 0) return at;
    strings.push(s);
    return strings.length - 1;
  };

  const L = json.lanes.length;
  const lanePointTotal = json.lanes.reduce((n, l) => n + l.centreline.length / 3, 0);
  const lanes: WorldLanes = {
    count: L,
    laneId: new Uint32Array(L), pointOff: new Uint32Array(L), pointCount: new Uint32Array(L),
    edgeId: new Uint32Array(L), junctionId: new Uint32Array(L), strName: new Uint32Array(L),
    widthM: new Float32Array(L), speedLimitMps: new Float32Array(L),
    allowedClasses: new Uint16Array(L), laneType: new Uint8Array(L), indexInEdge: new Uint8Array(L),
  };
  const lanePoints: WorldLanePoints = {
    count: lanePointTotal,
    x: new Float32Array(lanePointTotal), y: new Float32Array(lanePointTotal), z: new Float32Array(lanePointTotal),
  };
  let pcur = 0;
  json.lanes.forEach((l, i) => {
    const n = l.centreline.length / 3;
    lanes.laneId[i] = l.lane_id;
    lanes.pointOff[i] = pcur;
    lanes.pointCount[i] = n;
    lanes.edgeId[i] = l.edge_id;
    lanes.junctionId[i] = l.junction_id === null ? 0xffffffff : l.junction_id;
    lanes.strName[i] = intern(l.name);
    lanes.widthM[i] = l.width_m;
    lanes.speedLimitMps[i] = l.speed_limit_mps;
    lanes.allowedClasses[i] = allowedClassMask(l.allowed_classes);
    lanes.laneType[i] = Math.max(0, LANE_TYPES.indexOf(l.lane_type));
    lanes.indexInEdge[i] = l.index_in_edge;
    for (let k = 0; k < n; k++) {
      lanePoints.x[pcur + k] = l.centreline[k * 3];
      lanePoints.y[pcur + k] = l.centreline[k * 3 + 1];
      lanePoints.z[pcur + k] = l.centreline[k * 3 + 2];
    }
    pcur += n;
  });

  const B = json.buildings.length;
  const ringTotal =
    json.buildings.reduce((n, b) => n + b.ring.length / 2, 0) + json.landuse.reduce((n, l) => n + l.ring.length / 2, 0);
  const buildings: WorldBuildings = {
    count: B,
    buildingId: new Uint32Array(B), ringOff: new Uint32Array(B), ringCount: new Uint32Array(B),
    heightM: new Float32Array(B), baseZM: new Float32Array(B), strName: new Uint32Array(B),
    material: new Uint8Array(B), lodHint: new Uint8Array(B), levels: new Uint16Array(B),
  };
  const ringPoints: WorldRingPoints = { count: ringTotal, x: new Float32Array(ringTotal), y: new Float32Array(ringTotal) };
  let rcur = 0;
  json.buildings.forEach((b, i) => {
    const n = b.ring.length / 2;
    buildings.buildingId[i] = b.building_id;
    buildings.ringOff[i] = rcur;
    buildings.ringCount[i] = n;
    buildings.heightM[i] = b.height_m;
    buildings.baseZM[i] = b.base_z_m;
    buildings.strName[i] = intern(b.name);
    buildings.material[i] = Math.max(0, BUILDING_MATERIALS.indexOf(b.material));
    buildings.lodHint[i] = Math.max(0, LOD_HINTS.indexOf(b.lod_hint));
    buildings.levels[i] = b.levels === null ? 0xffff : b.levels;
    for (let k = 0; k < n; k++) {
      ringPoints.x[rcur + k] = b.ring[k * 2];
      ringPoints.y[rcur + k] = b.ring[k * 2 + 1];
    }
    rcur += n;
  });
  const landuseRingOffsets: number[] = [];
  json.landuse.forEach((l) => {
    const n = l.ring.length / 2;
    landuseRingOffsets.push(rcur);
    for (let k = 0; k < n; k++) {
      ringPoints.x[rcur + k] = l.ring[k * 2];
      ringPoints.y[rcur + k] = l.ring[k * 2 + 1];
    }
    rcur += n;
  });

  const jb = packAos(json.junctions.length, VWB_JUNCTION_STRIDE, (dv, b, i) => {
    const j = json.junctions[i];
    dv.setUint32(b + 0, j.junction_id, true);
    dv.setUint32(b + 4, intern(j.name), true);
    dv.setFloat32(b + 8, j.x_m, true);
    dv.setFloat32(b + 12, j.y_m, true);
    dv.setFloat32(b + 16, j.z_m, true);
    dv.setUint8(b + 20, Math.max(0, JUNCTION_CONTROLS.indexOf(j.control)));
    dv.setUint16(b + 22, j.lane_count, true);
  });
  const sb = packAos(json.signals.length, VWB_SIGNAL_STRIDE, (dv, b, i) => {
    const s = json.signals[i];
    dv.setUint32(b + 0, s.signal_id, true);
    dv.setUint32(b + 4, s.junction_id, true);
    dv.setUint32(b + 8, s.lane_id, true);
    dv.setFloat32(b + 12, s.x_m, true);
    dv.setFloat32(b + 16, s.y_m, true);
    dv.setFloat32(b + 20, s.z_m, true);
    dv.setUint8(b + 24, Math.max(0, SIGNAL_KINDS.indexOf(s.kind)));
    dv.setUint16(b + 26, s.group, true);
  });
  const tb = packAos(json.sites.length, VWB_SITE_STRIDE, (dv, b, i) => {
    const s = json.sites[i];
    dv.setUint32(b + 0, s.site_id, true);
    dv.setUint32(b + 4, s.node_id === null ? 0xffffffff : s.node_id, true);
    dv.setFloat32(b + 8, s.x_m, true);
    dv.setFloat32(b + 12, s.y_m, true);
    dv.setFloat32(b + 16, s.z_m, true);
    dv.setFloat32(b + 20, s.antenna_height_m, true);
    dv.setFloat32(b + 24, s.antenna_gain_dbi, true);
    dv.setUint8(b + 28, Math.max(0, SITE_KINDS.indexOf(s.kind)));
  });
  const cb = packAos(json.crossings.length, VWB_CROSSING_STRIDE, (dv, b, i) => {
    const c = json.crossings[i];
    dv.setUint32(b + 0, c.crossing_id, true);
    dv.setUint32(b + 4, c.junction_id, true);
    dv.setFloat32(b + 8, c.x1_m, true);
    dv.setFloat32(b + 12, c.y1_m, true);
    dv.setFloat32(b + 16, c.x2_m, true);
    dv.setFloat32(b + 20, c.y2_m, true);
    dv.setFloat32(b + 24, c.width_m, true);
  });
  const lb = packAos(json.landuse.length, VWB_LANDUSE_STRIDE, (dv, b, i) => {
    const l = json.landuse[i];
    dv.setUint32(b + 0, l.landuse_id, true);
    dv.setUint32(b + 4, landuseRingOffsets[i], true);
    dv.setUint32(b + 8, l.ring.length / 2, true);
    dv.setUint8(b + 12, Math.max(0, LANDUSE_CLASSES.indexOf(l.class)));
  });

  const junctions = aos<WorldJunction>(jb, 0, json.junctions.length, VWB_JUNCTION_STRIDE, "world.junctions", (s, i) => {
    const w = i * 6;
    return { junctionId: s.words[w], strName: s.words[w + 1], xM: s.floats[w + 2], yM: s.floats[w + 3], zM: s.floats[w + 4],
      control: s.bytes[i * VWB_JUNCTION_STRIDE + 20], laneCount: s.halves[i * 12 + 11] };
  });
  const signals = aos<WorldSignal>(sb, 0, json.signals.length, VWB_SIGNAL_STRIDE, "world.signals", (s, i) => {
    const w = i * 7;
    return { signalId: s.words[w], junctionId: s.words[w + 1], laneId: s.words[w + 2],
      xM: s.floats[w + 3], yM: s.floats[w + 4], zM: s.floats[w + 5],
      kind: s.bytes[i * VWB_SIGNAL_STRIDE + 24], group: s.halves[i * 14 + 13] };
  });
  const sites = aos<WorldSite>(tb, 0, json.sites.length, VWB_SITE_STRIDE, "world.sites", (s, i) => {
    const w = i * 8;
    return { siteId: s.words[w], nodeId: s.words[w + 1], xM: s.floats[w + 2], yM: s.floats[w + 3], zM: s.floats[w + 4],
      antennaHeightM: s.floats[w + 5], antennaGainDbi: s.floats[w + 6], kind: s.bytes[i * VWB_SITE_STRIDE + 28] };
  });
  const crossings = aos<WorldCrossing>(cb, 0, json.crossings.length, VWB_CROSSING_STRIDE, "world.crossings", (s, i) => {
    const w = i * 7;
    return { crossingId: s.words[w], junctionId: s.words[w + 1], x1M: s.floats[w + 2], y1M: s.floats[w + 3],
      x2M: s.floats[w + 4], y2M: s.floats[w + 5], widthM: s.floats[w + 6] };
  });
  const landuse = aos<WorldLanduse>(lb, 0, json.landuse.length, VWB_LANDUSE_STRIDE, "world.landuse", (s, i) => {
    const w = i * 4;
    return { landuseId: s.words[w], ringOff: s.words[w + 1], ringCount: s.words[w + 2], classIdx: s.bytes[i * VWB_LANDUSE_STRIDE + 12] };
  });

  return {
    contentHash: json.content_hash,
    origin: { latDeg: json.origin.lat_deg, lonDeg: json.origin.lon_deg, altM: json.origin.alt_m },
    bbox: {
      minXM: json.bbox.min_x_m, minYM: json.bbox.min_y_m, maxXM: json.bbox.max_x_m, maxYM: json.bbox.max_y_m,
      minZM: json.bbox.min_z_m, maxZM: json.bbox.max_z_m,
    },
    lanes, lanePoints, buildings, ringPoints, junctions, signals, sites, crossings, landuse,
    strings,
    provenance: json.provenance,
    str: (id: number) => strings[id] ?? "",
  };
}
