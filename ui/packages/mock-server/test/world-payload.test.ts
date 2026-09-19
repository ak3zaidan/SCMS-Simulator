/**
 * §4 world-payload hardening, against the payload the server actually serves.
 *
 * The world arrives over HTTP (`GET /world/{hash}.vwb`, §4) or as §3.9 `WorldChunk` frames, so from
 * the client's side its 192-byte directory is untrusted input. The decoder used to read the §2.2
 * sentinel **backwards** for every section: `off_* === 0 ? 0 : count`, which derives the count from
 * the offset instead of validating one against the other. A payload declaring 920 lanes with
 * `off_lanes = 0` decoded to zero lanes and rendered an empty city, with no error anywhere.
 *
 * The §4.2 digest check cannot catch that — a mis-built payload's own `content_hash` is computed
 * over the same bad bytes, so it verifies cleanly and then renders nothing. A check that passes on
 * corrupt input is worse than no check, so the decoder has to be the one that refuses.
 *
 * These tests live here rather than in `@vwp/protocol` because this is where the real payload is:
 * `generateManhattan` with the server's own default options produces the exact bytes
 * `MockEngineServer` serves and the Studio's end-to-end suite renders. Every case below takes those
 * bytes and patches exactly one directory field. Only the last test is encoder-built, and says why.
 */

import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";

import {
  ProtocolError,
  VWB_DIRECTORY_OFFSETS as D,
  VWB_HEADER_BYTES,
  VWB_HEADER_OFFSETS as H,
  computeWorldContentHash,
  decodeWorld,
  encodeWorld,
  verifyWorldPayload,
  worldToJson,
} from "@vwp/protocol";

import { MANHATTAN_BBOX, generateManhattan } from "../src/manhattan.js";

const sha256 = (bytes: Uint8Array): Uint8Array => new Uint8Array(createHash("sha256").update(bytes).digest());

// The server's own defaults (`MockEngineServer`: `bbox ?? MANHATTAN_BBOX`, `seed ?? 20260918`), so
// this is the payload it serves, not a payload shaped like it.
const world = generateManhattan({ bbox: MANHATTAN_BBOX, seed: 20260918 }, sha256);
const SERVED = world.vwb;

/** A fresh copy of the served bytes, as their own `ArrayBuffer`. */
const served = (): ArrayBuffer =>
  SERVED.buffer.slice(SERVED.byteOffset, SERVED.byteOffset + SERVED.byteLength) as ArrayBuffer;

/** The served bytes with one `u32` of the §4.2 directory replaced. */
function patchDirectory(field: number, value: number): ArrayBuffer {
  const buf = served();
  new DataView(buf, VWB_HEADER_BYTES).setUint32(field, value, true);
  return buf;
}

/** What the directory says, read straight off the served bytes. */
const directory = (buf: ArrayBuffer, field: number): number =>
  new DataView(buf, VWB_HEADER_BYTES).getUint32(field, true);

function failure(fn: () => unknown): { code: string; closeCode: number; field?: string } {
  try {
    fn();
    throw new Error("expected a ProtocolError, nothing was thrown");
  } catch (err) {
    if (!(err instanceof ProtocolError)) {
      throw new Error(`expected a ProtocolError, got ${(err as Error)?.constructor?.name}: ${String(err)}`);
    }
    return { code: err.code, closeCode: err.closeCode, field: err.detail.field };
  }
}

describe("§4 — the served world payload is a known-good control", () => {
  it("decodes with every section populated, which is what makes the patches below meaningful", () => {
    const w = decodeWorld(served());
    expect(w.lanes.count).toBe(world.counts.lanes);
    expect(w.buildings.count).toBe(world.counts.buildings);
    expect(w.junctions.count).toBe(world.counts.junctions);
    expect(w.signals.count).toBe(world.counts.signals);
    expect(w.sites.count).toBe(world.counts.sites);
    expect(w.crossings.count).toBeGreaterThan(0);
    expect(w.landuse.count).toBeGreaterThan(0);
    expect(w.lanePoints.count).toBeGreaterThan(0);
    expect(w.ringPoints.count).toBeGreaterThan(0);
    expect(w.provenance).not.toBeNull();
    // And the whole point of the exercise: it is self-consistent, so nothing below is caught by
    // accident, and its centrelines are real numbers rather than the nulls an overrun produces.
    expect(worldToJson(w).lanes[0].centreline.every((n) => Number.isFinite(n))).toBe(true);
  });

  it("§10.5 W3 — it verifies against its own §4.2 payload digest", async () => {
    await expect(computeWorldContentHash(served())).resolves.toBe(world.contentHashHex);
    await expect(verifyWorldPayload(served(), world.contentHashHex)).resolves.toBe(world.contentHashHex);
  });
});

describe("§2.2 / §4 — a declared section with a zero offset is refused, not silently emptied", () => {
  // Each row: the offset to zero, the count beside it, and the field the error must name.
  const SECTIONS = [
    { field: "world.lanes", off: D.offLanes, count: D.laneCount },
    { field: "world.lane_points", off: D.offLanePoints, count: D.lanePointTotal },
    { field: "world.buildings", off: D.offBuildings, count: D.buildingCount },
    { field: "world.ring_points", off: D.offRingPoints, count: D.ringPointTotal },
    { field: "world.junctions", off: D.offJunctions, count: D.junctionCount },
    { field: "world.signals", off: D.offSignals, count: D.signalCount },
    { field: "world.sites", off: D.offSites, count: D.siteCount },
    { field: "world.crossings", off: D.offCrossings, count: D.crossingCount },
    { field: "world.landuse", off: D.offLanduse, count: D.landuseCount },
    { field: "world.provenance", off: D.offProvenanceJson, count: D.provenanceJsonBytes },
  ] as const;

  it.each(SECTIONS)("$field: its offset zeroed while its count stays non-zero -> bad_offset", ({ field, off, count }) => {
    const buf = patchDirectory(off, 0);
    // The payload still declares the rows: this is the "decodes to nothing" case, not "is absent".
    expect(directory(buf, count)).toBeGreaterThan(0);
    expect(directory(buf, off)).toBe(0);
    expect(failure(() => decodeWorld(buf))).toEqual({ code: "bad_offset", closeCode: 1002, field });
  });

  it("covers every §4.2 section that has a count beside an offset, so none is left unswept", () => {
    expect(SECTIONS.map((s) => s.field)).toEqual([
      "world.lanes", "world.lane_points", "world.buildings", "world.ring_points", "world.junctions",
      "world.signals", "world.sites", "world.crossings", "world.landuse", "world.provenance",
    ]);
  });
});

describe("§4 — the other directory fields the decoder used to trust", () => {
  it("§4.1 — a `body_len` longer than the bytes served is refused, not silently clamped", () => {
    // `ArrayBuffer.slice` clamps, so the decoder used to carry on against a body shorter than the
    // one declared and measure every section bound against the wrong length. Bounds passing is not
    // the same as the extent being right.
    const buf = patchDirectoryHeader(SERVED.byteLength);
    expect(failure(() => decodeWorld(buf))).toMatchObject({ code: "truncated", field: "world.body_len" });
  });

  it("§2.2 — a misaligned column base is a typed error, not a bare RangeError", () => {
    // `new Uint32Array(body, 193, n)` throws `RangeError: start offset of Uint32Array should be a
    // multiple of 4`, which is not a ProtocolError, so it escaped the decoder's error contract and
    // reached the caller untyped. `aos()` already checked this; the column helpers did not.
    const base = directory(served(), D.offLanes);
    expect(base % 4).toBe(0);
    expect(failure(() => decodeWorld(patchDirectory(D.offLanes, base + 1)))).toMatchObject({
      code: "misaligned",
      closeCode: 1002,
    });
  });

  it("§4.3 / §4.4 — a row indexing past its point array is refused rather than decoding to nulls", () => {
    // Typed arrays read out of range as `undefined` instead of throwing, so an overrunning
    // `point_off` produced shape, not an error: `worldToJson` emitted `centreline: [null, null, …]`
    // and the renderer would have built NaN geometry from it.
    const lanes = decodeWorld(served()).lanes;
    const total = directory(served(), D.lanePointTotal);

    const overrunLane = served();
    const lv = new DataView(overrunLane, VWB_HEADER_BYTES);
    lv.setUint32(directory(overrunLane, D.offLanes) + 4 * lanes.count, total + 1000, true); // point_off[0]
    expect(failure(() => decodeWorld(overrunLane))).toMatchObject({
      code: "bad_offset",
      field: "world.lanes.point_off",
    });

    const buildings = decodeWorld(served()).buildings;
    const ringTotal = directory(served(), D.ringPointTotal);
    const overrunBuilding = served();
    const bv = new DataView(overrunBuilding, VWB_HEADER_BYTES);
    bv.setUint32(directory(overrunBuilding, D.offBuildings) + 4 * buildings.count, ringTotal, true); // ring_off[0]
    expect(failure(() => decodeWorld(overrunBuilding))).toMatchObject({
      code: "bad_offset",
      field: "world.buildings.ring_off",
    });

    // Landuse rings share the ring-point arrays with buildings, and were unchecked in the same way.
    const overrunLanduse = served();
    const uv = new DataView(overrunLanduse, VWB_HEADER_BYTES);
    uv.setUint32(directory(overrunLanduse, D.offLanduse) + 4, ringTotal + 1, true); // landuse[0].ring_off
    expect(failure(() => decodeWorld(overrunLanduse))).toMatchObject({
      code: "bad_offset",
      field: "world.landuse.ring_off",
    });
  });
});

describe("§2.2 / §4 — the other half of the sentinel still holds", () => {
  it("a genuinely empty section decodes to empty, in every section that can be", () => {
    // `generateManhattan` always emits every section, so this one payload is encoder-built: a real
    // world can have no sites, no crossings, no land use and no provenance block, and
    // `encodeWorld` writes `off_* = 0` for each of them. Refusing those would refuse real worlds.
    const sparse = encodeWorld(
      {
        originLatDeg: 40.75, originLonDeg: -73.98, originAltM: 10,
        bboxMinXM: -100, bboxMinYM: -100, bboxMaxXM: 100, bboxMaxYM: 100, bboxMinZM: 0, bboxMaxZM: 20,
        lanes: [{
          laneId: 1, edgeId: 1, junctionId: 0xffffffff, strName: 0, widthM: 3.2, speedLimitMps: 13.9,
          allowedClasses: 1, laneType: 0, indexInEdge: 0, points: [[-50, 0, 0], [50, 0, 0]],
        }],
        buildings: [], junctions: [], signals: [], sites: [], crossings: [], landuse: [],
        strings: [""], provenanceJson: "",
      },
      sha256,
    );
    const buf = sparse.buffer.slice(sparse.byteOffset, sparse.byteOffset + sparse.byteLength) as ArrayBuffer;
    for (const off of [D.offBuildings, D.offRingPoints, D.offJunctions, D.offSignals, D.offSites, D.offCrossings, D.offLanduse, D.offProvenanceJson]) {
      expect(directory(buf, off)).toBe(0);
    }
    const w = decodeWorld(buf);
    expect(w.lanes.count).toBe(1);
    expect(w.lanePoints.count).toBe(2);
    expect([w.buildings.count, w.ringPoints.count, w.junctions.count, w.signals.count, w.sites.count, w.crossings.count, w.landuse.count])
      .toEqual([0, 0, 0, 0, 0, 0, 0]);
    expect(w.provenance).toBeNull();
  });
});

/** The served bytes with the §4.1 file header's `body_len` replaced. */
function patchDirectoryHeader(value: number): ArrayBuffer {
  const buf = served();
  new DataView(buf).setUint32(H.bodyLen, value, true);
  return buf;
}
