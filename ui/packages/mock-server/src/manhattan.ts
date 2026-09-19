/**
 * A synthetic but plausible Manhattan-like world in `vwp-world/1` (§4).
 *
 * The real midtown grid is rotated about 29° east of true north, avenues are ~270 m apart and
 * cross-streets ~80 m apart, so that is what this generates, clipped to the requested geodetic
 * bbox and projected to local ENU metres. It also keeps a directed lane graph the mobility model
 * drives actors along — §4's decision 21 keeps lane connectivity **out** of the payload, so the
 * graph stays server-side, exactly as a real engine's world cache would.
 *
 * Generation is deterministic in `seed` (conformance W5).
 */

import {
  allowedClassMask,
  encodeWorld,
  type WorldBuildingInit,
  type WorldInit,
  type WorldLaneInit,
} from "@vwp/protocol";

import { type EnuProjection, type GeoBbox, projectionForBbox } from "./geo.js";
import { mulberry32, randRange } from "./rng.js";

/** The real midtown-Manhattan bbox this fixture covers by default. */
export const MANHATTAN_BBOX: GeoBbox = [-73.99, 40.744, -73.968, 40.762];

/** Knobs for {@link generateManhattan}. */
export interface ManhattanOptions {
  readonly bbox?: GeoBbox;
  readonly seed?: number;
  /** Metres between avenues (measured across them). Midtown is ~274 m. */ readonly avenueSpacingM?: number;
  /** Metres between cross-streets. Midtown is ~80 m. */ readonly streetSpacingM?: number;
  /** Grid rotation, degrees east of north. Midtown is ~29°. */ readonly rotationDeg?: number;
  readonly laneWidthM?: number;
  readonly avenueLanesPerDirection?: number;
  readonly streetLanesPerDirection?: number;
  readonly avenueSpeedLimitMps?: number;
  readonly streetSpeedLimitMps?: number;
  /** Target number of extruded building footprints. */ readonly buildings?: number;
  readonly rsuCount?: number;
}

/** A lane as the mobility model uses it: a straight, directed segment with a successor list. */
export interface DirectedLane {
  readonly laneId: number;
  readonly edgeId: number;
  /** Intersection this lane leaves. */ readonly fromNode: number;
  /** Intersection this lane enters. */ readonly toNode: number;
  readonly x0: number;
  readonly y0: number;
  readonly x1: number;
  readonly y1: number;
  readonly lengthM: number;
  readonly headingRad: number;
  readonly speedLimitMps: number;
  /** `true` for an avenue (north-south-ish) lane, which matters for the signal plan. */ readonly isAvenue: boolean;
  /** Signal group controlling entry to `toNode`. */ readonly signalGroup: number;
  /** Lane index within its edge and direction, 0 = rightmost. */ readonly indexInEdge: number;
  /** Directed lanes an actor may continue onto at `toNode`. */ readonly successors: number[];
}

/** An intersection of the grid. */
export interface Junction {
  readonly junctionId: number;
  readonly xM: number;
  readonly yM: number;
  /** Avenue index in the grid. */ readonly i: number;
  /** Street index in the grid. */ readonly j: number;
}

/** The generated world: the wire payload, its ENU projection and the server-side lane graph. */
export interface GeneratedWorld {
  /** The `vwp-world/1` file, file header included, ready to serve from `GET /world/{hash}.vwb`. */
  readonly vwb: Uint8Array;
  readonly contentHashHex: string;
  readonly init: WorldInit;
  readonly projection: EnuProjection;
  readonly bboxM: { minX: number; minY: number; maxX: number; maxY: number; minZ: number; maxZ: number };
  readonly lanes: readonly DirectedLane[];
  readonly junctions: readonly Junction[];
  readonly signals: readonly { signalId: number; group: number; junctionId: number; laneId: number }[];
  readonly siteNodeIds: readonly number[];
  readonly counts: { lanes: number; buildings: number; junctions: number; signals: number; sites: number; bytes: number };
}

const AVENUE_NAMES = [
  "Twelfth Avenue", "Eleventh Avenue", "Tenth Avenue", "Ninth Avenue", "Eighth Avenue",
  "Seventh Avenue", "Avenue of the Americas", "Fifth Avenue", "Madison Avenue", "Park Avenue",
  "Lexington Avenue", "Third Avenue", "Second Avenue", "First Avenue",
];

/** Liang–Barsky: the parameter interval of `p + t·d` that lies inside the axis-aligned rectangle. */
function clipToRect(
  px: number, py: number, dx: number, dy: number,
  minX: number, minY: number, maxX: number, maxY: number,
): { t0: number; t1: number } | null {
  let t0 = -Infinity;
  let t1 = Infinity;
  const clip = (p: number, q: number): boolean => {
    if (p === 0) return q >= 0;
    const r = q / p;
    if (p < 0) {
      if (r > t1) return false;
      if (r > t0) t0 = r;
    } else {
      if (r < t0) return false;
      if (r < t1) t1 = r;
    }
    return true;
  };
  if (!clip(-dx, px - minX)) return null;
  if (!clip(dx, maxX - px)) return null;
  if (!clip(-dy, py - minY)) return null;
  if (!clip(dy, maxY - py)) return null;
  return t0 < t1 ? { t0, t1 } : null;
}

/**
 * Generate the world. `sha256` computes the §4.2 content hash (injected so the protocol package
 * keeps no runtime dependency).
 */
export function generateManhattan(options: ManhattanOptions, sha256: (bytes: Uint8Array) => Uint8Array): GeneratedWorld {
  const bbox = options.bbox ?? MANHATTAN_BBOX;
  const seed = options.seed ?? 20260918;
  const rand = mulberry32(seed);
  const avenueSpacing = options.avenueSpacingM ?? 274;
  const streetSpacing = options.streetSpacingM ?? 80;
  const rotation = ((options.rotationDeg ?? 29) * Math.PI) / 180;
  const laneWidth = options.laneWidthM ?? 3.25;
  const avenueLanes = options.avenueLanesPerDirection ?? 2;
  const streetLanes = options.streetLanesPerDirection ?? 1;
  const avenueSpeed = options.avenueSpeedLimitMps ?? 11.18; // 25 mph, the NYC default
  const streetSpeed = options.streetSpeedLimitMps ?? 8.94; // 20 mph
  const targetBuildings = options.buildings ?? 480;
  const rsuCount = options.rsuCount ?? 12;

  const projection = projectionForBbox(bbox);
  const far = projection.project(bbox[2], bbox[3]);
  const minX = 0;
  const minY = 0;
  const maxX = far.x;
  const maxY = far.y;

  // Grid basis: `u` runs along the avenues (north-north-east), `v` along the cross-streets.
  const u = { x: Math.sin(rotation), y: Math.cos(rotation) };
  const v = { x: Math.cos(rotation), y: -Math.sin(rotation) };
  const centre = { x: (minX + maxX) / 2, y: (minY + maxY) / 2 };

  // How far the rectangle reaches along each basis vector.
  const corners = [
    { x: minX, y: minY }, { x: maxX, y: minY }, { x: minX, y: maxY }, { x: maxX, y: maxY },
  ];
  let halfV = 0;
  let halfU = 0;
  for (const c of corners) {
    const dx = c.x - centre.x;
    const dy = c.y - centre.y;
    halfV = Math.max(halfV, Math.abs(dx * v.x + dy * v.y));
    halfU = Math.max(halfU, Math.abs(dx * u.x + dy * u.y));
  }
  const avenueCount = Math.max(2, Math.floor((2 * halfV) / avenueSpacing) + 1);
  const streetCount = Math.max(2, Math.floor((2 * halfU) / streetSpacing) + 1);

  const gridPoint = (i: number, j: number): { x: number; y: number } => {
    const p = (i - (avenueCount - 1) / 2) * avenueSpacing;
    const q = (j - (streetCount - 1) / 2) * streetSpacing;
    return { x: centre.x + p * v.x + q * u.x, y: centre.y + p * v.y + q * u.y };
  };

  // Intersections inside the bbox.
  const junctions: Junction[] = [];
  const junctionAt = new Map<string, number>();
  for (let i = 0; i < avenueCount; i++) {
    for (let j = 0; j < streetCount; j++) {
      const p = gridPoint(i, j);
      if (p.x < minX || p.x > maxX || p.y < minY || p.y > maxY) continue;
      const id = junctions.length;
      junctionAt.set(`${i}:${j}`, id);
      junctions.push({ junctionId: id, xM: p.x, yM: p.y, i, j });
    }
  }

  // Edges: consecutive kept intersections along an avenue, then along a street.
  interface Edge {
    readonly edgeId: number;
    readonly a: number;
    readonly b: number;
    readonly isAvenue: boolean;
    readonly name: string;
  }
  const edges: Edge[] = [];
  for (let i = 0; i < avenueCount; i++) {
    const name = AVENUE_NAMES[i % AVENUE_NAMES.length];
    let previous = -1;
    for (let j = 0; j < streetCount; j++) {
      const id = junctionAt.get(`${i}:${j}`);
      if (id === undefined) {
        previous = -1;
        continue;
      }
      if (previous >= 0) edges.push({ edgeId: edges.length, a: previous, b: id, isAvenue: true, name });
      previous = id;
    }
  }
  for (let j = 0; j < streetCount; j++) {
    const name = `West ${34 + j} Street`;
    let previous = -1;
    for (let i = 0; i < avenueCount; i++) {
      const id = junctionAt.get(`${i}:${j}`);
      if (id === undefined) {
        previous = -1;
        continue;
      }
      if (previous >= 0) edges.push({ edgeId: edges.length, a: previous, b: id, isAvenue: false, name });
      previous = id;
    }
  }

  // Lanes: two directions per edge, N lanes each, offset from the centreline. Right-hand traffic,
  // so a lane in the direction of travel sits to the right of the centreline.
  const strings: string[] = [""];
  const internString = (s: string): number => {
    if (s === "") return 0;
    const at = strings.indexOf(s);
    if (at >= 0) return at;
    strings.push(s);
    return strings.length - 1;
  };

  const laneInits: WorldLaneInit[] = [];
  const directed: DirectedLane[] = [];
  const lanesLeavingNode = new Map<number, number[]>();
  const driveClasses = allowedClassMask(["car", "truck", "bus", "moto", "emergency"]);

  for (const edge of edges) {
    const A = junctions[edge.a];
    const B = junctions[edge.b];
    const perDirection = edge.isAvenue ? avenueLanes : streetLanes;
    const speed = edge.isAvenue ? avenueSpeed : streetSpeed;
    const nameId = internString(edge.name);
    for (const forward of [true, false]) {
      const from = forward ? A : B;
      const to = forward ? B : A;
      const dx = to.xM - from.xM;
      const dy = to.yM - from.yM;
      const len = Math.hypot(dx, dy);
      if (len < 1) continue;
      const dirX = dx / len;
      const dirY = dy / len;
      // Right-hand normal of the direction of travel.
      const nx = dirY;
      const ny = -dirX;
      for (let k = 0; k < perDirection; k++) {
        const offset = laneWidth * (k + 0.5);
        const x0 = from.xM + nx * offset;
        const y0 = from.yM + ny * offset;
        const x1 = to.xM + nx * offset;
        const y1 = to.yM + ny * offset;
        const laneId = directed.length;
        // Signal group: avenues get the even phase, streets the odd one (§4.5 control = signal).
        const signalGroup = edge.isAvenue ? 0 : 1;
        directed.push({
          laneId, edgeId: edge.edgeId, fromNode: from.junctionId, toNode: to.junctionId,
          x0, y0, x1, y1, lengthM: len, headingRad: Math.atan2(dirY, dirX),
          speedLimitMps: speed, isAvenue: edge.isAvenue, signalGroup, indexInEdge: k, successors: [],
        });
        laneInits.push({
          laneId, edgeId: edge.edgeId, junctionId: 0xffffffff, strName: nameId,
          widthM: laneWidth, speedLimitMps: speed, allowedClasses: driveClasses,
          laneType: 0, indexInEdge: k,
          points: [
            [x0, y0, 0],
            [x1, y1, 0],
          ],
        });
        const list = lanesLeavingNode.get(from.junctionId) ?? [];
        list.push(laneId);
        lanesLeavingNode.set(from.junctionId, list);
      }
    }
  }

  // Successors: anything leaving the node we arrive at, except a U-turn back down the same edge.
  for (const lane of directed) {
    const leaving = lanesLeavingNode.get(lane.toNode) ?? [];
    for (const id of leaving) {
      const candidate = directed[id];
      if (candidate.edgeId === lane.edgeId) continue; // no U-turn
      lane.successors.push(id);
    }
    if (lane.successors.length === 0) {
      // A dead end at the bbox edge: allow the U-turn so actors never get stuck.
      for (const id of leaving) lane.successors.push(id);
    }
  }

  // Buildings: subdivide each block, inset by a setback, and extrude to a Manhattan-ish height.
  const buildings: WorldBuildingInit[] = [];
  const setback = laneWidth * (avenueLanes + 1) + 2;
  const blocks: { i: number; j: number }[] = [];
  for (let i = 0; i < avenueCount - 1; i++) {
    for (let j = 0; j < streetCount - 1; j++) {
      const ok = ["", "+1:0", "0:+1", "+1:+1"].every((_, n) => {
        const di = n === 1 || n === 3 ? 1 : 0;
        const dj = n === 2 || n === 3 ? 1 : 0;
        return junctionAt.has(`${i + di}:${j + dj}`);
      });
      if (ok) blocks.push({ i, j });
    }
  }
  const lotsPerBlock = Math.max(1, Math.min(6, Math.round(targetBuildings / Math.max(1, blocks.length))));
  let buildingId = 0;
  let maxHeight = 0;
  for (const block of blocks) {
    const p00 = gridPoint(block.i, block.j);
    const p10 = gridPoint(block.i + 1, block.j);
    const p01 = gridPoint(block.i, block.j + 1);
    // Block-local axes: `v` across the block (avenue to avenue), `u` along it (street to street).
    const acrossX = p10.x - p00.x;
    const acrossY = p10.y - p00.y;
    const alongX = p01.x - p00.x;
    const alongY = p01.y - p00.y;
    const acrossLen = Math.hypot(acrossX, acrossY);
    const alongLen = Math.hypot(alongX, alongY);
    if (acrossLen < 2 * setback + 8 || alongLen < 2 * setback + 8) continue;
    const a0 = setback / acrossLen;
    const a1 = 1 - setback / acrossLen;
    const b0 = setback / alongLen;
    const b1 = 1 - setback / alongLen;
    for (let lot = 0; lot < lotsPerBlock; lot++) {
      const s0 = a0 + ((a1 - a0) * lot) / lotsPerBlock + 0.004;
      const s1 = a0 + ((a1 - a0) * (lot + 1)) / lotsPerBlock - 0.004;
      const corner = (s: number, t: number): readonly [number, number] => [
        p00.x + acrossX * s + alongX * t,
        p00.y + acrossY * s + alongY * t,
      ];
      // Counter-clockwise, not closed (§4.4).
      const ring: (readonly [number, number])[] = [corner(s0, b0), corner(s1, b0), corner(s1, b1), corner(s0, b1)];
      const inside = ring.every(([x, y]) => x >= minX - 1 && x <= maxX + 1 && y >= minY - 1 && y <= maxY + 1);
      if (!inside) continue;
      const levels = Math.max(2, Math.round(randRange(rand, 3, 42)));
      const height = levels * randRange(rand, 3.2, 4.1);
      maxHeight = Math.max(maxHeight, height);
      buildings.push({
        buildingId: buildingId++, heightM: height, baseZM: 0, strName: 0,
        material: rand() < 0.45 ? 3 : 1, lodHint: 0, levels, ring,
      });
    }
  }

  // Signals: four heads per intersection, one per approach, grouped by avenue/street.
  const signalInits: { signalId: number; junctionId: number; laneId: number; xM: number; yM: number; zM: number; kind: number; group: number }[] = [];
  const signalMeta: { signalId: number; group: number; junctionId: number; laneId: number }[] = [];
  const approachesByNode = new Map<number, DirectedLane[]>();
  for (const lane of directed) {
    const list = approachesByNode.get(lane.toNode) ?? [];
    list.push(lane);
    approachesByNode.set(lane.toNode, list);
  }
  let signalId = 0;
  for (const j of junctions) {
    const approaches = approachesByNode.get(j.junctionId) ?? [];
    const seenEdges = new Set<number>();
    for (const lane of approaches) {
      if (seenEdges.has(lane.edgeId)) continue; // one head per approach, not per lane
      seenEdges.add(lane.edgeId);
      const stopX = lane.x1 - Math.cos(lane.headingRad) * 6;
      const stopY = lane.y1 - Math.sin(lane.headingRad) * 6;
      signalInits.push({
        signalId, junctionId: j.junctionId, laneId: lane.laneId,
        xM: stopX, yM: stopY, zM: 5.2, kind: 0, group: lane.signalGroup,
      });
      signalMeta.push({ signalId, group: lane.signalGroup, junctionId: j.junctionId, laneId: lane.laneId });
      signalId += 1;
    }
  }

  // RSU sites, spread over the grid; their node ids are allocated below the actor node ids.
  const sites: { siteId: number; nodeId: number; xM: number; yM: number; zM: number; antennaHeightM: number; antennaGainDbi: number; kind: number }[] = [];
  const siteNodeIds: number[] = [];
  const step = Math.max(1, Math.floor(junctions.length / Math.max(1, rsuCount)));
  for (let s = 0; s < rsuCount; s++) {
    const j = junctions[(s * step) % junctions.length];
    if (!j) continue;
    const nodeId = s; // site node ids occupy 0..999; actor nodes start at ACTOR_NODE_BASE
    sites.push({
      siteId: s, nodeId, xM: j.xM + 8, yM: j.yM + 8, zM: 0,
      antennaHeightM: 6, antennaGainDbi: 5, kind: 0,
    });
    siteNodeIds.push(nodeId);
  }

  // Crossings: one across each axis at every intersection.
  const crossings: { crossingId: number; junctionId: number; x1M: number; y1M: number; x2M: number; y2M: number; widthM: number }[] = junctions.flatMap((j, n) => [
    { crossingId: n * 2, junctionId: j.junctionId, x1M: j.xM - 9, y1M: j.yM - 9, x2M: j.xM + 9, y2M: j.yM - 9, widthM: 4 },
    { crossingId: n * 2 + 1, junctionId: j.junctionId, x1M: j.xM + 9, y1M: j.yM - 9, x2M: j.xM + 9, y2M: j.yM + 9, widthM: 4 },
  ]);

  // A park, because midtown has one.
  const parkX = minX + (maxX - minX) * 0.42;
  const parkY = minY + (maxY - minY) * 0.55;
  const landuse: { landuseId: number; classIdx: number; ring: (readonly [number, number])[] }[] = [
    {
      landuseId: 0, classIdx: 4,
      ring: [
        [parkX, parkY],
        [parkX + 180, parkY],
        [parkX + 180, parkY + 110],
        [parkX, parkY + 110],
      ],
    },
  ];

  const init: WorldInit = {
    originLatDeg: projection.originLatDeg,
    originLonDeg: projection.originLonDeg,
    originAltM: projection.originAltM,
    bboxMinXM: minX,
    bboxMinYM: minY,
    bboxMaxXM: maxX,
    bboxMaxYM: maxY,
    bboxMinZM: 0,
    bboxMaxZM: Math.max(10, Math.ceil(maxHeight)),
    lanes: laneInits,
    buildings,
    junctions: junctions.map((j) => ({
      junctionId: j.junctionId, strName: 0, xM: j.xM, yM: j.yM, zM: 0, control: 2, laneCount: (approachesByNode.get(j.junctionId) ?? []).length,
    })),
    signals: signalInits,
    sites,
    crossings,
    landuse,
    strings,
    provenanceJson: JSON.stringify({
      source: "synthetic",
      bbox: [...bbox],
      imported_at: new Date(0).toISOString(),
      tool_versions: { "vwp-mock-server": "0.1.0" },
      transformations: [
        "local-tangent-plane",
        `grid:${avenueSpacing}m-avenues/${streetSpacing}m-streets`,
        `rotation:${options.rotationDeg ?? 29}deg`,
        "height-default:3.2-4.1m/level",
      ],
      dropped: { building_holes: 0 },
      licence: "CC0-1.0",
      note: "synthetic Manhattan-like grid produced by the VWP mock server; not OSM data",
      seed,
    }),
  };

  const vwb = encodeWorld(init, sha256);
  const hashBytes = vwb.subarray(16, 48);
  const contentHashHex = Array.from(hashBytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");

  return {
    vwb,
    contentHashHex,
    init,
    projection,
    bboxM: { minX, minY, maxX, maxY, minZ: init.bboxMinZM, maxZ: init.bboxMaxZM },
    lanes: directed,
    junctions,
    signals: signalMeta,
    siteNodeIds,
    counts: {
      lanes: laneInits.length,
      buildings: buildings.length,
      junctions: junctions.length,
      signals: signalInits.length,
      sites: sites.length,
      bytes: vwb.byteLength,
    },
  };
}
