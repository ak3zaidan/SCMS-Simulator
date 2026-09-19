/** §4 — the `vwp-world/1` payload, including the §10.5 W4 `world_json_binary_parity` check. */

import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";

import {
  ALLOWED_CLASSES,
  BUILDING_MATERIALS,
  JUNCTION_CONTROLS,
  LANDUSE_CLASSES,
  LANE_TYPES,
  LOD_HINTS,
  ProtocolError,
  SIGNAL_KINDS,
  SITE_KINDS,
  VWB_MAGIC,
  allowedClassMask,
  allowedClassNames,
  computeWorldContentHashWith,
  decodeWorld,
  encodeWorld,
  verifyWorldPayload,
  verifyWorldPayloadWith,
  worldFromJson,
  worldToJson,
  type WorldInit,
} from "../src/index.js";

const sha256 = (bytes: Uint8Array): Uint8Array => new Uint8Array(createHash("sha256").update(bytes).digest());

const init: WorldInit = {
  originLatDeg: 52.5163,
  originLonDeg: 13.3777,
  originAltM: 34,
  bboxMinXM: -500,
  bboxMinYM: -500,
  bboxMaxXM: 500,
  bboxMaxYM: 500,
  bboxMinZM: -2,
  bboxMaxZM: 61.5,
  lanes: [
    {
      laneId: 42, edgeId: 7, junctionId: 0xffffffff, strName: 1, widthM: 3.25, speedLimitMps: 13.89,
      allowedClasses: allowedClassMask(["car", "truck", "bus", "moto", "emergency"]), laneType: 0, indexInEdge: 0,
      points: [
        [-120, -3.2, 0.15],
        [-60, -3.2, 0.140625],
        [12.375, -3.25, 0.15],
      ],
    },
    {
      laneId: 43, edgeId: 7, junctionId: 3, widthM: 3.25, strName: 1, speedLimitMps: 13.89,
      allowedClasses: allowedClassMask(["car"]), laneType: 5, indexInEdge: 1,
      points: [
        [12.375, -6.5, 0.15],
        [40, -6.5, 0.15],
      ],
    },
  ],
  buildings: [
    {
      buildingId: 3, heightM: 21.5, baseZM: 0, strName: 2, material: 1, lodHint: 0, levels: 6,
      ring: [
        [10, 20],
        [40, 20],
        [40, 55],
        [10, 55],
      ],
    },
    {
      buildingId: 4, heightM: 9.25, baseZM: 0.5, strName: 0, material: 2, lodHint: 1, levels: 0xffff,
      ring: [
        [-80, -40],
        [-50, -40],
        [-50, -10],
      ],
    },
  ],
  junctions: [{ junctionId: 1, strName: 3, xM: 0, yM: 0, zM: 0.125, control: 2, laneCount: 12 }],
  signals: [{ signalId: 7, junctionId: 1, laneId: 42, xM: -8, yM: -6, zM: 5.25, kind: 0, group: 2 }],
  sites: [{ siteId: 0, nodeId: 2, xM: 12, yM: -8, zM: 0, antennaHeightM: 6, antennaGainDbi: 5, kind: 0 }],
  crossings: [{ crossingId: 0, junctionId: 1, x1M: -6, y1M: -10, x2M: 6, y2M: -10, widthM: 4 }],
  landuse: [
    {
      landuseId: 0, classIdx: 4,
      ring: [
        [100, 100],
        [200, 100],
        [200, 200],
      ],
    },
  ],
  strings: ["", "Unter den Linden", "Hotel Adlon", "Pariser Platz"],
  provenanceJson: JSON.stringify({
    source: "osm",
    bbox: [13.37, 52.512, 13.385, 52.521],
    imported_at: "2026-09-14T11:02:31Z",
    tool_versions: { "v2xw-world": "0.4.0", "osm2streets-rs": "0.3.1" },
    transformations: ["local-tangent-plane", "simplify:0.25m", "height-default:3m/level"],
    dropped: { building_holes: 41 },
    licence: "ODbL-1.0",
  }),
};

describe("§4 — the binary world payload", () => {
  const file = encodeWorld(init, sha256);

  it("has the §4.1 file header with magic V W L D", () => {
    expect(new DataView(file.buffer, file.byteOffset).getUint32(0, true)).toBe(VWB_MAGIC);
    expect(Array.from(file.subarray(0, 4))).toEqual([0x56, 0x57, 0x4c, 0x44]);
    expect(new TextDecoder().decode(file.subarray(0, 4))).toBe("VWLD");
    expect(new DataView(file.buffer, file.byteOffset).getUint16(4, true)).toBe(1);
    expect(new DataView(file.buffer, file.byteOffset).getUint32(8, true)).toBe(file.byteLength - 16);
  });

  it("§10.5 W1/W3 — the stored content hash is the one a client recomputes", () => {
    const world = decodeWorld(file.buffer.slice(file.byteOffset, file.byteOffset + file.byteLength) as ArrayBuffer);
    expect(world.contentHash).toMatch(/^[0-9a-f]{64}$/);
    expect(computeWorldContentHashWith(file, sha256)).toBe(world.contentHash);
  });

  it("decodes every section into typed arrays", () => {
    const world = decodeWorld(file.buffer.slice(file.byteOffset, file.byteOffset + file.byteLength) as ArrayBuffer);
    expect(world.origin).toEqual({ latDeg: 52.5163, lonDeg: 13.3777, altM: 34 });
    expect(world.bbox.minXM).toBe(-500);
    expect(world.bbox.maxYM).toBe(500);
    expect(world.bbox.minZM).toBe(-2);
    expect(world.bbox.maxZM).toBe(61.5);

    expect(world.lanes.count).toBe(2);
    expect(Array.from(world.lanes.laneId)).toEqual([42, 43]);
    expect(Array.from(world.lanes.pointOff)).toEqual([0, 3]);
    expect(Array.from(world.lanes.pointCount)).toEqual([3, 2]);
    expect(Array.from(world.lanes.junctionId)).toEqual([0xffffffff, 3]);
    expect(Array.from(world.lanes.laneType)).toEqual([0, 5]);
    expect(Array.from(world.lanes.indexInEdge)).toEqual([0, 1]);
    expect(world.str(world.lanes.strName[0])).toBe("Unter den Linden");
    expect(allowedClassNames(world.lanes.allowedClasses[0])).toEqual(["car", "truck", "bus", "moto", "emergency"]);

    expect(world.lanePoints.count).toBe(5);
    expect(world.lanePoints.x[0]).toBe(-120);
    expect(world.lanePoints.y[0]).toBeCloseTo(-3.2, 5);
    expect(world.lanePoints.z[1]).toBe(0.140625);
    expect(world.lanePoints.x[3]).toBe(12.375);

    expect(world.buildings.count).toBe(2);
    expect(Array.from(world.buildings.ringOff)).toEqual([0, 4]);
    expect(Array.from(world.buildings.ringCount)).toEqual([4, 3]);
    expect(world.buildings.heightM[0]).toBe(21.5);
    expect(world.buildings.baseZM[1]).toBe(0.5);
    expect(Array.from(world.buildings.levels)).toEqual([6, 0xffff]);
    expect(Array.from(world.buildings.material)).toEqual([1, 2]);
    expect(world.str(world.buildings.strName[0])).toBe("Hotel Adlon");
    expect(world.ringPoints.count).toBe(4 + 3 + 3); // buildings then landuse share the arrays
    expect([world.ringPoints.x[0], world.ringPoints.y[0]]).toEqual([10, 20]);

    const j = world.junctions.at(0);
    expect(j.junctionId).toBe(1);
    expect(j.control).toBe(2);
    expect(j.laneCount).toBe(12);
    expect(j.zM).toBe(0.125);
    expect(world.str(j.strName)).toBe("Pariser Platz");

    const s = world.signals.at(0);
    expect([s.signalId, s.junctionId, s.laneId, s.group, s.kind]).toEqual([7, 1, 42, 2, 0]);
    expect([s.xM, s.yM, s.zM]).toEqual([-8, -6, 5.25]);

    const site = world.sites.at(0);
    expect([site.siteId, site.nodeId, site.kind]).toEqual([0, 2, 0]);
    expect([site.antennaHeightM, site.antennaGainDbi]).toEqual([6, 5]);

    const c = world.crossings.at(0);
    expect([c.x1M, c.y1M, c.x2M, c.y2M, c.widthM]).toEqual([-6, -10, 6, -10, 4]);

    const l = world.landuse.at(0);
    expect([l.landuseId, l.classIdx, l.ringOff, l.ringCount]).toEqual([0, 4, 7, 3]);

    expect(world.provenance?.source).toBe("osm");
    expect(world.provenance?.licence).toBe("ODbL-1.0");
    expect(world.provenance?.dropped?.building_holes).toBe(41);
  });

  it("rejects a bad magic and a bad version", () => {
    const bad = file.slice();
    new DataView(bad.buffer, bad.byteOffset).setUint32(0, 0x11223344, true);
    expect(() => decodeWorld(bad.buffer.slice(bad.byteOffset, bad.byteOffset + bad.byteLength) as ArrayBuffer)).toThrow(ProtocolError);
    const badVersion = file.slice();
    new DataView(badVersion.buffer, badVersion.byteOffset).setUint16(4, 2, true);
    expect(() =>
      decodeWorld(badVersion.buffer.slice(badVersion.byteOffset, badVersion.byteOffset + badVersion.byteLength) as ArrayBuffer),
    ).toThrow(/version/);
  });

  it("§10.5 W4 world_json_binary_parity — the JSON and binary forms carry the same content", () => {
    const binary = decodeWorld(file.buffer.slice(file.byteOffset, file.byteOffset + file.byteLength) as ArrayBuffer);
    const json = worldToJson(binary);
    expect(json.schema).toBe("vwp-world/1");
    const rebuilt = worldFromJson(json);

    expect(rebuilt.contentHash).toBe(binary.contentHash);
    expect(rebuilt.origin).toEqual(binary.origin);
    expect(rebuilt.bbox).toEqual(binary.bbox);
    expect(rebuilt.lanes.count).toBe(binary.lanes.count);
    expect(Array.from(rebuilt.lanes.laneId)).toEqual(Array.from(binary.lanes.laneId));
    expect(Array.from(rebuilt.lanes.pointCount)).toEqual(Array.from(binary.lanes.pointCount));
    expect(Array.from(rebuilt.lanes.allowedClasses)).toEqual(Array.from(binary.lanes.allowedClasses));
    expect(Array.from(rebuilt.lanes.laneType)).toEqual(Array.from(binary.lanes.laneType));
    expect(Array.from(rebuilt.lanePoints.x)).toEqual(Array.from(binary.lanePoints.x));
    expect(Array.from(rebuilt.lanePoints.y)).toEqual(Array.from(binary.lanePoints.y));
    expect(Array.from(rebuilt.lanePoints.z)).toEqual(Array.from(binary.lanePoints.z));
    expect(Array.from(rebuilt.buildings.heightM)).toEqual(Array.from(binary.buildings.heightM));
    expect(Array.from(rebuilt.buildings.levels)).toEqual(Array.from(binary.buildings.levels));
    expect(Array.from(rebuilt.ringPoints.x)).toEqual(Array.from(binary.ringPoints.x));
    expect(rebuilt.junctions.at(0)).toMatchObject({ junctionId: 1, control: 2, laneCount: 12 });
    expect(rebuilt.signals.at(0)).toEqual(binary.signals.at(0));
    expect(rebuilt.sites.at(0)).toEqual(binary.sites.at(0));
    expect(rebuilt.crossings.at(0)).toEqual(binary.crossings.at(0));
    expect(rebuilt.landuse.at(0)).toEqual(binary.landuse.at(0));
    expect(rebuilt.str(rebuilt.lanes.strName[0])).toBe("Unter den Linden");
    expect(rebuilt.provenance).toEqual(binary.provenance);

    // Round-tripping the JSON through text changes nothing.
    const reparsed = worldFromJson(JSON.parse(JSON.stringify(json)) as typeof json);
    expect(worldToJson(reparsed)).toEqual(json);
  });

  // Every enum table in §4, pinned name by name against the code the specification
  // documents it with. LANDUSE_CLASSES was wrong here — it had park at 4 and water at 5,
  // the other way round from the normative §4.5 table the engine writes — so every
  // land-use zone in every payload decoded as the wrong class, and nothing caught it
  // because no test tied a name to a code. These lists are transcribed from §4.3, §4.4
  // and §4.5 (Appendix A repeats them); if the protocol changes, this test is the place
  // the change is made, deliberately.
  it("§4.3/§4.4/§4.5 — every enum table is in the specification's code order", () => {
    // §4.5: `u8 class` @12 — `0` urban, `1` suburban, `2` rural, `3` highway,
    // `4` water, `5` park, `6` industrial.
    expect(LANDUSE_CLASSES[0]).toBe("urban");
    expect(LANDUSE_CLASSES[1]).toBe("suburban");
    expect(LANDUSE_CLASSES[2]).toBe("rural");
    expect(LANDUSE_CLASSES[3]).toBe("highway");
    expect(LANDUSE_CLASSES[4]).toBe("water");
    expect(LANDUSE_CLASSES[5]).toBe("park");
    expect(LANDUSE_CLASSES[6]).toBe("industrial");
    expect(LANDUSE_CLASSES.length).toBe(7);
    expect(LANDUSE_CLASSES.indexOf("water")).toBe(4);
    expect(LANDUSE_CLASSES.indexOf("park")).toBe(5);

    // §4.3: `u8 lane_type`.
    expect(Array.from(LANE_TYPES)).toEqual([
      "drive", "bike", "sidewalk", "bus", "parking", "junction-internal", "crossing",
    ]);
    // §4.4: `u8 material`, `u8 lod_hint`.
    expect(Array.from(BUILDING_MATERIALS)).toEqual(["unknown", "concrete", "brick", "glass", "wood", "metal"]);
    expect(Array.from(LOD_HINTS)).toEqual(["box", "box+roof", "detailed"]);
    // §4.5: `u8 control`, `u8 kind` (signal), `u8 kind` (site).
    expect(Array.from(JUNCTION_CONTROLS)).toEqual(["none", "priority", "signal", "stop", "yield", "roundabout"]);
    expect(Array.from(SIGNAL_KINDS)).toEqual(["vehicle", "pedestrian", "bicycle", "transit"]);
    expect(Array.from(SITE_KINDS)).toEqual(["rsu", "cell", "other"]);
  });

  // The class byte survives a decode and a JSON round trip as the same *name*, so a
  // renderer keyed by name paints the class the engine meant. The fixture's zone is
  // class 4, which §4.5 defines as water.
  it("§4.5/§4.6 — a class-4 zone decodes as water and round-trips as water", () => {
    const binary = decodeWorld(file.buffer.slice(file.byteOffset, file.byteOffset + file.byteLength) as ArrayBuffer);
    expect(binary.landuse.at(0).classIdx).toBe(4);
    const json = worldToJson(binary);
    expect(json.landuse[0].class).toBe("water");
    expect(worldFromJson(json).landuse.at(0).classIdx).toBe(4);
  });

  it("§4.3 — the allowed-classes bitmask is the documented bit order", () => {
    expect(ALLOWED_CLASSES).toEqual(["car", "truck", "bus", "moto", "bicycle", "pedestrian", "emergency", "rail"]);
    expect(allowedClassMask(["car"])).toBe(1);
    expect(allowedClassMask(["truck"])).toBe(2);
    expect(allowedClassMask(["bus"])).toBe(4);
    expect(allowedClassMask(["moto"])).toBe(8);
    expect(allowedClassMask(["bicycle"])).toBe(16);
    expect(allowedClassMask(["pedestrian"])).toBe(32);
    expect(allowedClassMask(["emergency"])).toBe(64);
    expect(allowedClassMask(["rail"])).toBe(128);
    expect(allowedClassNames(0xff)).toEqual([...ALLOWED_CLASSES]);
  });
});

/**
 * §10.5 W3 — "verifies the world hash and refuses a mismatch".
 *
 * §4.2's amendment of 2026-09-18 settled what the digest is (the body with its own 32 bytes
 * zeroed) and which of the two digests the specification now names `Hello.world_hash` carries (the
 * **payload** digest, not the geometry digest §3.1.1's gloss points at). What was still missing was
 * the verification itself: nothing recomputed the digest of the bytes actually served and compared
 * it against what the stream promised, so W3 was unimplemented and a client would have adopted any
 * world the server happened to hand it.
 */
describe("§4.2 / §10.5 W3 — the served world is verified against Hello.world_hash", () => {
  const file = encodeWorld(init, sha256);
  const asBuffer = (bytes: Uint8Array): ArrayBuffer =>
    bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
  /** `Hello.world_hash` as it arrives on the wire: 32 raw bytes (§3.1.1). */
  const helloWorldHash = (hex: string): Uint8Array =>
    Uint8Array.from(hex.match(/.{2}/g)!.map((h) => Number.parseInt(h, 16)));
  const stored = computeWorldContentHashWith(file, sha256);

  it("accepts the payload whose digest is the one the stream promised", async () => {
    // Both call shapes: the 32 raw bytes of Hello.world_hash and the hex of the /world/{hash} URL.
    await expect(verifyWorldPayload(asBuffer(file), helloWorldHash(stored))).resolves.toBe(stored);
    await expect(verifyWorldPayload(asBuffer(file), stored)).resolves.toBe(stored);
    expect(verifyWorldPayloadWith(file, helloWorldHash(stored), sha256)).toBe(stored);
    // And the digest verified is the §4.2 one, not some fourth rule: it is what the body stores.
    expect(stored).toBe(decodeWorld(asBuffer(file)).contentHash);
  });

  it("refuses a payload the stream did not promise, with a typed hash_mismatch", async () => {
    // A different world: same shape, one building moved. The bytes are internally consistent — the
    // §4.2 content_hash matches its own body — so only the comparison against Hello catches it.
    const other = encodeWorld({ ...init, bboxMaxXM: 501 }, sha256);
    const otherHash = computeWorldContentHashWith(other, sha256);
    expect(otherHash).not.toBe(stored);

    const err = await verifyWorldPayload(asBuffer(other), helloWorldHash(stored)).then(
      () => null,
      (e: unknown) => e,
    );
    expect(err).toBeInstanceOf(ProtocolError);
    expect((err as ProtocolError).code).toBe("hash_mismatch");
    expect((err as ProtocolError).closeCode).toBe(1002);
    expect((err as ProtocolError).detail).toMatchObject({ expected: stored, actual: otherHash, field: "Hello.world_hash" });
    expect(() => verifyWorldPayloadWith(other, helloWorldHash(stored), sha256)).toThrow(ProtocolError);
  });

  it("refuses a payload whose bytes were altered after it was hashed", async () => {
    // One flipped bit anywhere in the body: the recomputed digest no longer matches the body's own
    // content_hash, which says "damaged", not "wrong world".
    const damaged = file.slice();
    damaged[damaged.length - 40] ^= 0x01;
    const err = await verifyWorldPayload(asBuffer(damaged), helloWorldHash(stored)).then(() => null, (e: unknown) => e);
    expect(err).toBeInstanceOf(ProtocolError);
    expect((err as ProtocolError).code).toBe("hash_mismatch");
    expect((err as ProtocolError).detail.field).toBe("world.content_hash");
  });

  it("refuses a truncated payload and a malformed expected hash, typed rather than thrown raw", async () => {
    await expect(verifyWorldPayload(asBuffer(file.subarray(0, 100)), stored)).rejects.toMatchObject({ code: "truncated" });
    await expect(verifyWorldPayload(asBuffer(file), "not-a-digest")).rejects.toMatchObject({ code: "bad_length" });
    // A geometry digest sent where a payload digest belongs is exactly the §4.2 recorded defect:
    // well-formed, 64 hex characters, and not the digest of any payload — it must be refused.
    await expect(verifyWorldPayload(asBuffer(file), "0".repeat(64))).rejects.toMatchObject({
      code: "hash_mismatch",
      detail: { field: "Hello.world_hash" },
    });
  });
});
