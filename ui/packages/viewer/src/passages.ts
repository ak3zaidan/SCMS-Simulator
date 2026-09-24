/**
 * Openings where a road runs through a building.
 *
 * Manhattan's map puts roads through buildings on purpose: Park Avenue's two portals in the
 * Helmsley Building (`tunnel=building_passage`), the Park Avenue Viaduct around Grand Central,
 * hotel forecourts and garage ramps under towers. The engine marks those lanes as passages and
 * drives its vehicles through them (`v2xw_world::Passage`); drawn as solid prisms, the buildings
 * showed a car driving into a wall. Every place a motor lane's centreline crosses a footprint's
 * wall at the height of that wall is a portal, and a dark opening is drawn in the wall there,
 * as wide as the lane and as tall as the road clearance, so a car visibly enters an opening.
 *
 * The world payload (`vwp-world/1`) carries no passage table, so the portals are found from the
 * geometry it does carry. That is the same test the engine's importer applies — a lane inside a
 * footprint within the building's vertical extent — and after the importer's classification
 * every such lane *is* a passage, so the two agree. Hiding the building the followed vehicle is
 * inside ({@link WorldRenderer.setGhostBuildings}) remains as the chase camera's fallback.
 */

import type { VwpWorld } from "@vwp/protocol";

/** The road clearance an opening is drawn to, metres: AASHTO's 4.9 m (16 ft), the engine's too. */
export const PORTAL_CLEARANCE_M = 4.9;

/** Lane types (vwp-v1 Appendix A) that carry motor traffic: drive, bus, parking, junction-internal. */
const MOTOR_TYPES = new Set([0, 3, 4, 5]);

/** One opening in one building wall. */
export interface Portal {
  /** Building index (not id). */
  readonly building: number;
  /** Lane index (not id). */
  readonly lane: number;
  /** Where the lane's centreline crosses the wall, ENU metres. */
  readonly x: number;
  readonly y: number;
  /** The road surface there. */
  readonly z: number;
  /** Unit vector along the wall, counter-clockwise round the footprint. */
  readonly ex: number;
  readonly ey: number;
  /** Half the opening's width, metres. */
  readonly halfWidth: number;
  /** The opening's height above the road, metres. */
  readonly height: number;
}

/** Proper segment intersection: the parameter along p→p2 and q→q2, or null. */
function cross(
  px: number, py: number, p2x: number, p2y: number,
  qx: number, qy: number, q2x: number, q2y: number,
): [number, number] | null {
  const rx = p2x - px;
  const ry = p2y - py;
  const sx = q2x - qx;
  const sy = q2y - qy;
  const den = rx * sy - ry * sx;
  if (den === 0 || !Number.isFinite(den)) return null;
  const qpx = qx - px;
  const qpy = qy - py;
  const t = (qpx * sy - qpy * sx) / den;
  const u = (qpx * ry - qpy * rx) / den;
  if (t < 0 || t > 1 || u < 0 || u > 1) return null;
  return [t, u];
}

/** Every portal in `world`: each crossing of a motor lane's centreline with a building's wall. */
export function findPortals(world: VwpWorld): Portal[] {
  const b = world.buildings;
  const ring = world.ringPoints;
  const lanes = world.lanes;
  const pts = world.lanePoints;
  // Building bounding boxes, and a coarse grid over them.
  const B = b.count;
  const box = new Float64Array(B * 4);
  const CELL = 50;
  const grid = new Map<number, number[]>();
  const key = (gx: number, gy: number): number => gx * 100_003 + gy;
  for (let i = 0; i < B; i++) {
    const off = b.ringOff[i];
    const n = b.ringCount[i];
    let x0 = Infinity;
    let y0 = Infinity;
    let x1 = -Infinity;
    let y1 = -Infinity;
    for (let k = 0; k < n; k++) {
      x0 = Math.min(x0, ring.x[off + k]);
      y0 = Math.min(y0, ring.y[off + k]);
      x1 = Math.max(x1, ring.x[off + k]);
      y1 = Math.max(y1, ring.y[off + k]);
    }
    box[i * 4] = x0;
    box[i * 4 + 1] = y0;
    box[i * 4 + 2] = x1;
    box[i * 4 + 3] = y1;
    if (n < 3) continue;
    for (let gx = Math.floor(x0 / CELL); gx <= Math.floor(x1 / CELL); gx++) {
      for (let gy = Math.floor(y0 / CELL); gy <= Math.floor(y1 / CELL); gy++) {
        const k = key(gx, gy);
        const list = grid.get(k);
        if (list) list.push(i);
        else grid.set(k, [i]);
      }
    }
  }
  const out: Portal[] = [];
  const seen = new Set<string>();
  for (let l = 0; l < lanes.count; l++) {
    if (!MOTOR_TYPES.has(lanes.laneType[l])) continue;
    const off = lanes.pointOff[l];
    const n = lanes.pointCount[l];
    const halfWidth = Math.max(0.8, lanes.widthM[l] / 2 + 0.3);
    for (let s = 0; s + 1 < n; s++) {
      const ax = pts.x[off + s];
      const ay = pts.y[off + s];
      const az = pts.z[off + s];
      const bx = pts.x[off + s + 1];
      const by = pts.y[off + s + 1];
      const bz = pts.z[off + s + 1];
      const candidates = new Set<number>();
      for (let gx = Math.floor(Math.min(ax, bx) / CELL); gx <= Math.floor(Math.max(ax, bx) / CELL); gx++) {
        for (let gy = Math.floor(Math.min(ay, by) / CELL); gy <= Math.floor(Math.max(ay, by) / CELL); gy++) {
          for (const i of grid.get(key(gx, gy)) ?? []) candidates.add(i);
        }
      }
      for (const i of candidates) {
        if (Math.max(ax, bx) < box[i * 4] || Math.min(ax, bx) > box[i * 4 + 2]
          || Math.max(ay, by) < box[i * 4 + 1] || Math.min(ay, by) > box[i * 4 + 3]) continue;
        const roff = b.ringOff[i];
        const rn = b.ringCount[i];
        const base = b.baseZM[i];
        const top = base + Math.max(1, b.heightM[i]);
        for (let k = 0; k < rn; k++) {
          const cx = ring.x[roff + k];
          const cy = ring.y[roff + k];
          const dx = ring.x[roff + ((k + 1) % rn)];
          const dy = ring.y[roff + ((k + 1) % rn)];
          const hit = cross(ax, ay, bx, by, cx, cy, dx, dy);
          if (!hit) continue;
          const [t, u] = hit;
          const z = az + (bz - az) * t;
          // The road must meet the building's volume: a tunnel under it or a bridge over it
          // has no opening in its wall.
          if (!(top > z && base < z + PORTAL_CLEARANCE_M)) continue;
          const x = ax + (bx - ax) * t;
          const y = ay + (by - ay) * t;
          const tag = `${i}:${l}:${Math.round(x * 10)}:${Math.round(y * 10)}`;
          if (seen.has(tag)) continue;
          seen.add(tag);
          const el = Math.hypot(dx - cx, dy - cy) || 1;
          out.push({
            building: i, lane: l, x, y, z,
            ex: (dx - cx) / el, ey: (dy - cy) / el,
            halfWidth,
            height: Math.max(0.5, Math.min(PORTAL_CLEARANCE_M, top - z)),
          });
          void u;
        }
      }
    }
  }
  return out;
}
