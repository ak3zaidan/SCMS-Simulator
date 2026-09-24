import { describe, expect, it } from "vitest";
import { worldFromJson } from "@vwp/protocol";
import type { VwpWorldJson, WorldJsonLane } from "@vwp/protocol";
import { PORTAL_CLEARANCE_M, findPortals } from "../src/passages.js";

/** A 20 x 20 m, 30 m tall building centred on the origin, and the lanes given. */
function world(lanes: WorldJsonLane[], baseZ = 0): VwpWorldJson {
  return {
    schema: "vwp-world/1",
    content_hash: "0".repeat(64),
    origin: { lat_deg: 0, lon_deg: 0, alt_m: 0 },
    bbox: { min_x_m: -100, min_y_m: -100, max_x_m: 100, max_y_m: 100, min_z_m: -10, max_z_m: 40 },
    lanes,
    buildings: [{
      building_id: 0, height_m: 30, base_z_m: baseZ, levels: null, material: "concrete", lod_hint: "box",
      name: "", ring: [-10, -10, 10, -10, 10, 10, -10, 10],
    }],
    junctions: [], signals: [], sites: [], crossings: [], landuse: [], provenance: null,
  };
}

function lane(id: number, type: WorldJsonLane["lane_type"], z: number, y = 0): WorldJsonLane {
  return {
    lane_id: id, edge_id: id, junction_id: null, name: "", width_m: 3.5, speed_limit_mps: 11,
    lane_type: type, index_in_edge: 0, allowed_classes: ["car"],
    centreline: [-50, y, z, 50, y, z],
  };
}

describe("findPortals", () => {
  it("opens both walls where a drive lane runs through a building at street level", () => {
    const portals = findPortals(worldFromJson(world([lane(0, "drive", 0)])));
    expect(portals).toHaveLength(2);
    const xs = portals.map((p) => Math.round(p.x)).sort((a, b) => a - b);
    expect(xs).toEqual([-10, 10]);
    for (const p of portals) {
      expect(p.building).toBe(0);
      expect(p.height).toBeCloseTo(PORTAL_CLEARANCE_M, 6);
      // As wide as the lane, and the wall it sits in runs north-south here.
      expect(p.halfWidth).toBeGreaterThanOrEqual(1.75);
      expect(Math.abs(p.ex)).toBeLessThan(1e-6);
      expect(Math.abs(p.ey)).toBeCloseTo(1, 6);
    }
  });

  it("opens nothing for a tunnel under the building, a footway, or a lane that misses it", () => {
    const w = world([lane(0, "drive", -6), lane(1, "sidewalk", 0), lane(2, "drive", 0, 30)]);
    expect(findPortals(worldFromJson(w))).toHaveLength(0);
  });

  it("opens a viaduct's way through a building it enters at height", () => {
    const portals = findPortals(worldFromJson(world([lane(0, "junction-internal", 6)])));
    expect(portals).toHaveLength(2);
    expect(portals[0].z).toBeCloseTo(6, 5);
  });
});
