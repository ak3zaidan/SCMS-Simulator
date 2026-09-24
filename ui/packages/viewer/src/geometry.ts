/**
 * Procedural geometry helpers. Nothing here loads an external asset: every mesh the viewer draws is
 * built from numbers, which keeps the package a pure library and keeps the headless test honest.
 *
 * The central type is {@link MeshBuilder}: a growable position/normal/uv/colour/index accumulator.
 * `world-render.ts` merges a whole tile of roads into one builder and calls {@link MeshBuilder.toGeometry}
 * once, which is the "merged static geometry per tile" of 09-ui §4 without ever allocating the
 * thousands of intermediate `BufferGeometry` objects a `mergeGeometries` utility would need.
 */

import { BufferAttribute, BufferGeometry, Vector3 } from "three";
import type { ActorClassDef, LodLevel } from "./types.js";

/** Grow a `Float32Array` to at least `need` elements, preserving contents. */
function growF32(a: Float32Array, need: number): Float32Array {
  if (a.length >= need) return a;
  let n = Math.max(a.length * 2, 64);
  while (n < need) n *= 2;
  const next = new Float32Array(n);
  next.set(a, 0);
  return next;
}

function growU32(a: Uint32Array, need: number): Uint32Array {
  if (a.length >= need) return a;
  let n = Math.max(a.length * 2, 64);
  while (n < need) n *= 2;
  const next = new Uint32Array(n);
  next.set(a, 0);
  return next;
}

/** Options for a {@link MeshBuilder}. */
export interface MeshBuilderOptions {
  /** Emit a `uv` attribute. Default false. */
  readonly uv?: boolean;
  /** Emit a per-vertex `color` attribute. Default false. */
  readonly color?: boolean;
  /** Initial vertex capacity. */
  readonly vertexCapacity?: number;
  /** Initial index capacity. */
  readonly indexCapacity?: number;
}

/**
 * A growable triangle-soup accumulator: positions and normals always, uv and colour on request.
 * Reused across tiles by calling {@link reset}; the backing arrays are kept, so building a world
 * with 4,000 buildings allocates a handful of arrays rather than 4,000 geometries.
 */
export class MeshBuilder {
  positions: Float32Array;
  normals: Float32Array;
  uvs: Float32Array | null;
  colors: Float32Array | null;
  indices: Uint32Array;
  vertexCount = 0;
  indexCount = 0;

  constructor(options: MeshBuilderOptions = {}) {
    const v = Math.max(16, options.vertexCapacity ?? 256);
    const i = Math.max(16, options.indexCapacity ?? 512);
    this.positions = new Float32Array(v * 3);
    this.normals = new Float32Array(v * 3);
    this.uvs = options.uv ? new Float32Array(v * 2) : null;
    this.colors = options.color ? new Float32Array(v * 3) : null;
    this.indices = new Uint32Array(i);
  }

  /** True when nothing has been added since construction or the last {@link reset}. */
  get empty(): boolean {
    return this.indexCount === 0;
  }

  /** Make room for `vertices` more vertices and `indices` more indices without reallocating later. */
  reserve(vertices: number, indices: number): void {
    const v = (this.vertexCount + vertices) * 3;
    this.positions = growF32(this.positions, v);
    this.normals = growF32(this.normals, v);
    if (this.uvs) this.uvs = growF32(this.uvs, (this.vertexCount + vertices) * 2);
    if (this.colors) this.colors = growF32(this.colors, v);
    this.indices = growU32(this.indices, this.indexCount + indices);
  }

  /** Append one vertex; returns its index. */
  addVertex(
    x: number, y: number, z: number,
    nx: number, ny: number, nz: number,
    u = 0, v = 0,
    r = 1, g = 1, b = 1,
  ): number {
    const i = this.vertexCount;
    this.reserve(1, 0);
    const p = i * 3;
    this.positions[p] = x;
    this.positions[p + 1] = y;
    this.positions[p + 2] = z;
    this.normals[p] = nx;
    this.normals[p + 1] = ny;
    this.normals[p + 2] = nz;
    if (this.uvs) {
      this.uvs[i * 2] = u;
      this.uvs[i * 2 + 1] = v;
    }
    if (this.colors) {
      this.colors[p] = r;
      this.colors[p + 1] = g;
      this.colors[p + 2] = b;
    }
    this.vertexCount = i + 1;
    return i;
  }

  /** Append one triangle by vertex index. */
  addTriangle(a: number, b: number, c: number): void {
    this.reserve(0, 3);
    this.indices[this.indexCount++] = a;
    this.indices[this.indexCount++] = b;
    this.indices[this.indexCount++] = c;
  }

  /** Append a quad `a-b-c-d` (counter-clockwise) as two triangles. */
  addQuad(a: number, b: number, c: number, d: number): void {
    this.addTriangle(a, b, c);
    this.addTriangle(a, c, d);
  }

  /** Drop all content but keep the backing arrays. */
  reset(): void {
    this.vertexCount = 0;
    this.indexCount = 0;
  }

  /**
   * Snapshot the current content into an indexed `BufferGeometry`. The geometry owns copies, so the
   * builder may be {@link reset} and reused straight afterwards. Returns `null` when empty.
   */
  toGeometry(): BufferGeometry | null {
    if (this.indexCount === 0 || this.vertexCount === 0) return null;
    const g = new BufferGeometry();
    g.setAttribute("position", new BufferAttribute(this.positions.slice(0, this.vertexCount * 3), 3));
    g.setAttribute("normal", new BufferAttribute(this.normals.slice(0, this.vertexCount * 3), 3));
    if (this.uvs) g.setAttribute("uv", new BufferAttribute(this.uvs.slice(0, this.vertexCount * 2), 2));
    if (this.colors) g.setAttribute("color", new BufferAttribute(this.colors.slice(0, this.vertexCount * 3), 3));
    const idx = this.vertexCount > 65535
      ? new BufferAttribute(this.indices.slice(0, this.indexCount), 1)
      : new BufferAttribute(Uint16Array.from(this.indices.subarray(0, this.indexCount)), 1);
    g.setIndex(idx);
    g.computeBoundingSphere();
    g.computeBoundingBox();
    return g;
  }
}

/**
 * Add a flat ribbon of half-width `halfWidth` along a polyline, at `zOffset` above each point.
 * Vertices are mitred at interior joints, with the mitre length clamped to 4× the half-width so a
 * hairpin in an imported lane cannot produce a spike.
 *
 * Used for road surfaces, lane markings and crossings.
 */
export function addRibbon(
  b: MeshBuilder,
  xs: Float32Array, ys: Float32Array, zs: Float32Array | null,
  off: number, count: number,
  halfWidth: number, zOffset: number,
  r = 1, g = 1, bl = 1,
): void {
  if (count < 2) return;
  b.reserve(count * 2, (count - 1) * 6);
  let prevL = -1;
  let prevR = -1;
  for (let i = 0; i < count; i++) {
    const k = off + i;
    const x = xs[k];
    const y = ys[k];
    const z = (zs ? zs[k] : 0) + zOffset;

    // Direction: forward difference at the start, backward at the end, averaged inside.
    let dx: number;
    let dy: number;
    if (i === 0) {
      dx = xs[k + 1] - x;
      dy = ys[k + 1] - y;
    } else if (i === count - 1) {
      dx = x - xs[k - 1];
      dy = y - ys[k - 1];
    } else {
      const ax = x - xs[k - 1];
      const ay = y - ys[k - 1];
      const bx = xs[k + 1] - x;
      const by = ys[k + 1] - y;
      const la = Math.hypot(ax, ay) || 1;
      const lb = Math.hypot(bx, by) || 1;
      dx = ax / la + bx / lb;
      dy = ay / la + by / lb;
    }
    const len = Math.hypot(dx, dy) || 1;
    dx /= len;
    dy /= len;
    // Left normal in the ENU plane.
    let nx = -dy;
    let ny = dx;
    // Mitre scale: 1 / cos(half the turn). Clamped so hairpins do not spike.
    let scale = 1;
    if (i > 0 && i < count - 1) {
      const ax = x - xs[k - 1];
      const ay = y - ys[k - 1];
      const la = Math.hypot(ax, ay) || 1;
      const cos = (ax / la) * dx + (ay / la) * dy;
      scale = Math.min(4, 1 / Math.max(0.25, cos));
    }
    nx *= halfWidth * scale;
    ny *= halfWidth * scale;
    const v = i / (count - 1);
    const li = b.addVertex(x + nx, y + ny, z, 0, 0, 1, 0, v, r, g, bl);
    const ri = b.addVertex(x - nx, y - ny, z, 0, 0, 1, 1, v, r, g, bl);
    if (prevL >= 0) b.addQuad(prevL, prevR, ri, li);
    prevL = li;
    prevR = ri;
  }
}

/** Signed area of a ring in the xy plane; positive is counter-clockwise. */
export function ringSignedArea(xs: Float32Array, ys: Float32Array, off: number, count: number): number {
  let a = 0;
  for (let i = 0; i < count; i++) {
    const j = (i + 1) % count;
    a += xs[off + i] * ys[off + j] - xs[off + j] * ys[off + i];
  }
  return a * 0.5;
}

function triangleArea2(ax: number, ay: number, bx: number, by: number, cx: number, cy: number): number {
  return (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
}

function pointInTriangle(
  px: number, py: number,
  ax: number, ay: number, bx: number, by: number, cx: number, cy: number,
): boolean {
  const d1 = triangleArea2(ax, ay, bx, by, px, py);
  const d2 = triangleArea2(bx, by, cx, cy, px, py);
  const d3 = triangleArea2(cx, cy, ax, ay, px, py);
  const neg = d1 < 0 || d2 < 0 || d3 < 0;
  const pos = d1 > 0 || d2 > 0 || d3 > 0;
  return !(neg && pos);
}

/**
 * Ear-clip a simple polygon in the xy plane into `out` as local indices `0..count-1`, counter-clockwise.
 * Rings from §4.4 are CCW and not closed; a clockwise ring is flipped first. Falls back to a fan if
 * the ring is self-intersecting, which keeps a bad import from breaking the whole world build.
 *
 * O(n²), which is right for footprints of 4–40 points and avoids a dependency.
 */
export function earClip(
  xs: Float32Array, ys: Float32Array, off: number, count: number, out: number[],
): void {
  out.length = 0;
  if (count < 3) return;
  const ccw = ringSignedArea(xs, ys, off, count) >= 0;
  const idx: number[] = new Array<number>(count);
  for (let i = 0; i < count; i++) idx[i] = ccw ? i : count - 1 - i;

  let guard = count * count + 16;
  while (idx.length > 3 && guard-- > 0) {
    let clipped = false;
    for (let i = 0; i < idx.length; i++) {
      const i0 = idx[(i + idx.length - 1) % idx.length];
      const i1 = idx[i];
      const i2 = idx[(i + 1) % idx.length];
      const ax = xs[off + i0];
      const ay = ys[off + i0];
      const bx = xs[off + i1];
      const by = ys[off + i1];
      const cx = xs[off + i2];
      const cy = ys[off + i2];
      if (triangleArea2(ax, ay, bx, by, cx, cy) <= 0) continue; // reflex or collinear
      let contains = false;
      for (let j = 0; j < idx.length && !contains; j++) {
        const m = idx[j];
        if (m === i0 || m === i1 || m === i2) continue;
        if (pointInTriangle(xs[off + m], ys[off + m], ax, ay, bx, by, cx, cy)) contains = true;
      }
      if (contains) continue;
      out.push(i0, i1, i2);
      idx.splice(i, 1);
      clipped = true;
      break;
    }
    if (!clipped) break; // self-intersecting; fall through to the fan below
  }
  if (idx.length === 3) {
    out.push(idx[0], idx[1], idx[2]);
  } else if (idx.length > 3) {
    out.length = 0;
    for (let i = 1; i + 1 < count; i++) out.push(ccw ? 0 : count - 1, ccw ? i : count - 1 - i, ccw ? i + 1 : count - 2 - i);
  }
}

/** Add a flat, upward-facing polygon at height `z`. `scratch` is reused to avoid an allocation. */
export function addPolygon(
  b: MeshBuilder,
  xs: Float32Array, ys: Float32Array, off: number, count: number, z: number,
  scratch: number[],
  r = 1, g = 1, bl = 1,
): void {
  earClip(xs, ys, off, count, scratch);
  if (scratch.length === 0) return;
  b.reserve(count, scratch.length);
  const base = b.vertexCount;
  for (let i = 0; i < count; i++) b.addVertex(xs[off + i], ys[off + i], z, 0, 0, 1, 0, 0, r, g, bl);
  for (let i = 0; i + 2 < scratch.length; i += 3) {
    b.addTriangle(base + scratch[i], base + scratch[i + 1], base + scratch[i + 2]);
  }
}

/** Add a filled disc in the xy plane. */
export function addDisc(
  b: MeshBuilder, cx: number, cy: number, z: number, radius: number, segments: number,
  r = 1, g = 1, bl = 1,
): void {
  const n = Math.max(3, segments | 0);
  b.reserve(n + 1, n * 3);
  const c = b.addVertex(cx, cy, z, 0, 0, 1, 0.5, 0.5, r, g, bl);
  const first = b.vertexCount;
  for (let i = 0; i < n; i++) {
    const a = (i / n) * Math.PI * 2;
    b.addVertex(cx + Math.cos(a) * radius, cy + Math.sin(a) * radius, z, 0, 0, 1, 0, 0, r, g, bl);
  }
  for (let i = 0; i < n; i++) b.addTriangle(c, first + i, first + ((i + 1) % n));
}

/**
 * View-independent shading baked into an extruded ring's vertex colours.
 *
 * At street level a directional sun gives a vertical wall almost nothing: at the default 11:00 the
 * sun sits 57° up, so `N·L` on a wall is small, and on a wall facing away it is zero. A city of
 * prisms then renders as one flat silhouette with no edges — which is exactly the "no buildings, no
 * sense of a street, no depth" the review saw. These three terms fix that in the vertex buffer, so
 * they cost nothing per frame and survive any sun position:
 *
 * - **tone** separates neighbours. One value per building, so two adjacent façades never share an
 *   edge you cannot see.
 * - **baseOcclusion** darkens the foot of every wall. This is a real effect — the sky is occluded
 *   at the bottom of a street canyon — and it is what makes a wall read as standing on a road
 *   rather than floating over it.
 * - **faceRelief** gives each face of a prism its own value from the face's azimuth alone, so the
 *   corner between two walls is always visible even when the sun lights neither.
 *
 * All three default to a no-op, so a caller that wants flat vertex colours gets byte-identical
 * geometry to before.
 */
export interface RingShading {
  /** Multiplies the whole building's colour. Default 1. */
  readonly tone?: number;
  /** Fraction of the wall colour removed at the base, `[0, 1)`. Default 0. */
  readonly baseOcclusion?: number;
  /** Value swing applied by wall azimuth, `[0, 1)`. Default 0. */
  readonly faceRelief?: number;
  /** Multiplies the near-LOD parapet band, which is what draws a roofline. Default 1. */
  readonly parapetGain?: number;
}

/**
 * The fixed relief direction, a unit vector in the ground plane.
 *
 * Deliberately *not* the sun: the sun moves with the time of day and goes below the horizon, and
 * the point of the relief term is that the corners of a building stay visible when it does. North
 * north-east is chosen so it disagrees with the 11:00 sun azimuth and therefore adds information
 * rather than doubling what the light already says.
 */
const RELIEF_X = 0.44;
const RELIEF_Y = 0.9;

/** `1 + faceRelief · relief(n)` for a wall whose outward normal is `(nx, ny)`. */
function reliefGain(nx: number, ny: number, amount: number): number {
  return amount <= 0 ? 1 : 1 + amount * (nx * RELIEF_X + ny * RELIEF_Y);
}

/**
 * Extrude a footprint ring into a closed solid: walls with outward normals plus a roof cap.
 *
 * `lod` 0 gives walls, a cap and a small parapet band; 1 gives walls and a flat cap; 2 replaces the
 * ring by its axis-aligned bounding box (the "box" LOD of 09-ui §4).
 *
 * `shading` bakes the view-independent form shading described on {@link RingShading}; omitting it
 * reproduces the flat-coloured geometry exactly.
 */
export function addExtrudedRing(
  b: MeshBuilder,
  xs: Float32Array, ys: Float32Array, off: number, count: number,
  baseZ: number, height: number,
  lod: LodLevel,
  scratch: number[],
  wallR = 1, wallG = 1, wallB = 1,
  roofR = 1, roofG = 1, roofB = 1,
  shading?: RingShading,
): void {
  if (count < 3 || height <= 0) return;
  const topZ = baseZ + height;
  const tone = shading?.tone ?? 1;
  const occl = Math.max(0, Math.min(0.95, shading?.baseOcclusion ?? 0));
  const relief = Math.max(0, Math.min(0.95, shading?.faceRelief ?? 0));
  const parapet = shading?.parapetGain ?? 1;
  // The base band keeps the *relative* colour of the three surface classes; it only lowers value.
  const footGain = tone * (1 - occl);

  if (lod === 2) {
    // One box, one colour: the relief term needs per-face normals it does not have. Split the
    // difference between the wall top and its shaded foot so an LOD switch is not a step change.
    const flat = tone * (1 - occl * 0.5);
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    for (let i = 0; i < count; i++) {
      const x = xs[off + i];
      const y = ys[off + i];
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
    }
    addBox(b, (minX + maxX) / 2, (minY + maxY) / 2, (baseZ + topZ) / 2,
      maxX - minX, maxY - minY, height, 0, wallR * flat, wallG * flat, wallB * flat);
    return;
  }

  const ccw = ringSignedArea(xs, ys, off, count) >= 0;
  b.reserve(count * 4 + count, count * 6 + count * 3);
  // Walls: one quad per edge, flat-shaded with the edge normal.
  for (let i = 0; i < count; i++) {
    const j = (i + 1) % count;
    const ai = ccw ? i : count - 1 - i;
    const aj = ccw ? j : (count - 1 - j + count) % count;
    const x0 = xs[off + ai];
    const y0 = ys[off + ai];
    const x1 = xs[off + aj];
    const y1 = ys[off + aj];
    const ex = x1 - x0;
    const ey = y1 - y0;
    const el = Math.hypot(ex, ey);
    if (el < 1e-6) continue;
    const nx = ey / el;
    const ny = -ex / el;
    const g = reliefGain(nx, ny, relief);
    // Top of the wall at full value, foot of it darkened: the gradient is what gives the wall
    // height and the road a shadow line to sit against.
    const tr = wallR * tone * g;
    const tg = wallG * tone * g;
    const tb = wallB * tone * g;
    const br = wallR * footGain * g;
    const bg = wallG * footGain * g;
    const bb = wallB * footGain * g;
    const v0 = b.addVertex(x0, y0, baseZ, nx, ny, 0, 0, 0, br, bg, bb);
    const v1 = b.addVertex(x1, y1, baseZ, nx, ny, 0, 1, 0, br, bg, bb);
    const v2 = b.addVertex(x1, y1, topZ, nx, ny, 0, 1, 1, tr, tg, tb);
    const v3 = b.addVertex(x0, y0, topZ, nx, ny, 0, 0, 1, tr, tg, tb);
    b.addQuad(v0, v1, v2, v3);
  }
  // Roof.
  addPolygon(b, xs, ys, off, count, topZ, scratch, roofR * tone, roofG * tone, roofB * tone);
  if (lod === 0) {
    // A 0.4 m parapet band so near buildings do not read as untextured prisms.
    const bandZ = topZ + 0.4;
    for (let i = 0; i < count; i++) {
      const j = (i + 1) % count;
      const ai = ccw ? i : count - 1 - i;
      const aj = ccw ? j : (count - 1 - j + count) % count;
      const x0 = xs[off + ai];
      const y0 = ys[off + ai];
      const x1 = xs[off + aj];
      const y1 = ys[off + aj];
      const ex = x1 - x0;
      const ey = y1 - y0;
      const el = Math.hypot(ex, ey);
      if (el < 1e-6) continue;
      const nx = ey / el;
      const ny = -ex / el;
      // The parapet is the one band that always catches light, so it is what draws the roofline
      // against the sky. Gaining it slightly is the cheapest silhouette there is.
      const k = tone * parapet * reliefGain(nx, ny, relief);
      const pr = roofR * k;
      const pg = roofG * k;
      const pb = roofB * k;
      const v0 = b.addVertex(x0, y0, topZ, nx, ny, 0, 0, 0, pr, pg, pb);
      const v1 = b.addVertex(x1, y1, topZ, nx, ny, 0, 1, 0, pr, pg, pb);
      const v2 = b.addVertex(x1, y1, bandZ, nx, ny, 0, 1, 1, pr, pg, pb);
      const v3 = b.addVertex(x0, y0, bandZ, nx, ny, 0, 0, 1, pr, pg, pb);
      b.addQuad(v0, v1, v2, v3);
    }
  }
}

/**
 * Face table for {@link addBox}: six faces of `nx, ny, nz` then four corners as
 * `sx, sy, sz` sign triples, counter-clockwise seen from outside.
 */
const BOX_FACES = new Float32Array([
  1, 0, 0, 1, -1, -1, 1, 1, -1, 1, 1, 1, 1, -1, 1,
  -1, 0, 0, -1, 1, -1, -1, -1, -1, -1, -1, 1, -1, 1, 1,
  0, 1, 0, 1, 1, -1, -1, 1, -1, -1, 1, 1, 1, 1, 1,
  0, -1, 0, -1, -1, -1, 1, -1, -1, 1, -1, 1, -1, -1, 1,
  0, 0, 1, -1, -1, 1, 1, -1, 1, 1, 1, 1, -1, 1, 1,
  0, 0, -1, -1, 1, -1, 1, 1, -1, 1, -1, -1, -1, -1, -1,
]);

/** Add an axis-aligned box centred at `(cx, cy, cz)`, optionally yawed about +z by `yaw` radians. */
export function addBox(
  b: MeshBuilder,
  cx: number, cy: number, cz: number,
  sx: number, sy: number, sz: number,
  yaw = 0,
  r = 1, g = 1, bl = 1,
): void {
  const hx = sx / 2;
  const hy = sy / 2;
  const hz = sz / 2;
  const c = Math.cos(yaw);
  const s = Math.sin(yaw);
  b.reserve(24, 36);
  for (let f = 0; f < 6; f++) {
    const o = f * 15;
    const lnx = BOX_FACES[o];
    const lny = BOX_FACES[o + 1];
    const nz = BOX_FACES[o + 2];
    const nx = lnx * c - lny * s;
    const ny = lnx * s + lny * c;
    const base = b.vertexCount;
    for (let k = 0; k < 4; k++) {
      const q = o + 3 + k * 3;
      const lx = BOX_FACES[q] * hx;
      const ly = BOX_FACES[q + 1] * hy;
      const lz = BOX_FACES[q + 2] * hz;
      b.addVertex(cx + lx * c - ly * s, cy + lx * s + ly * c, cz + lz, nx, ny, nz, 0, 0, r, g, bl);
    }
    b.addQuad(base, base + 1, base + 2, base + 3);
  }
}

/** Add a cylinder along +z. */
export function addCylinder(
  b: MeshBuilder, cx: number, cy: number, z0: number, z1: number, radius: number, segments: number,
  r = 1, g = 1, bl = 1,
): void {
  const n = Math.max(3, segments | 0);
  b.reserve(n * 4 + 2, n * 12);
  for (let i = 0; i < n; i++) {
    const a0 = (i / n) * Math.PI * 2;
    const a1 = ((i + 1) / n) * Math.PI * 2;
    const x0 = cx + Math.cos(a0) * radius;
    const y0 = cy + Math.sin(a0) * radius;
    const x1 = cx + Math.cos(a1) * radius;
    const y1 = cy + Math.sin(a1) * radius;
    const v0 = b.addVertex(x0, y0, z0, Math.cos(a0), Math.sin(a0), 0, 0, 0, r, g, bl);
    const v1 = b.addVertex(x1, y1, z0, Math.cos(a1), Math.sin(a1), 0, 1, 0, r, g, bl);
    const v2 = b.addVertex(x1, y1, z1, Math.cos(a1), Math.sin(a1), 0, 1, 1, r, g, bl);
    const v3 = b.addVertex(x0, y0, z1, Math.cos(a0), Math.sin(a0), 0, 0, 1, r, g, bl);
    b.addQuad(v0, v1, v2, v3);
  }
  const top = b.addVertex(cx, cy, z1, 0, 0, 1, 0.5, 0.5, r, g, bl);
  const ringStart = b.vertexCount;
  for (let i = 0; i < n; i++) {
    const a = (i / n) * Math.PI * 2;
    b.addVertex(cx + Math.cos(a) * radius, cy + Math.sin(a) * radius, z1, 0, 0, 1, 0, 0, r, g, bl);
  }
  for (let i = 0; i < n; i++) b.addTriangle(top, ringStart + i, ringStart + ((i + 1) % n));
}

/**
 * Build a recognisable vehicle silhouette for one class and LOD, in the actor local frame:
 * **+x forward, +y left, +z up**, origin on the ground at the centre of the wheelbase, so an instance
 * matrix is `translate(pose) · rotateZ(heading)` and nothing else.
 *
 * LOD 0 has a body, a set-back cabin, a windscreen wedge and four wheels; LOD 1 drops the wheels and
 * merges the windscreen into the cabin; LOD 2 is a single box. Pedestrians and bicycles get their own
 * proportions so the silhouette still reads at a distance.
 */
export function buildActorGeometry(def: ActorClassDef, lod: LodLevel): BufferGeometry {
  const b = new MeshBuilder({ color: true, vertexCapacity: 256, indexCapacity: 512 });
  const L = Math.max(0.3, def.lengthM);
  const W = Math.max(0.2, def.widthM);
  const H = Math.max(0.3, def.heightM);

  if (lod === 2) {
    addBox(b, 0, 0, H / 2, L, W, H, 0, 1, 1, 1);
    const g2 = b.toGeometry();
    if (!g2) throw new Error("vehicle LOD2 geometry is empty");
    return g2;
  }

  if (def.category === 1 && isRiddenVru(def.name)) {
    buildCyclist(b, L, W, H, lod);
    const gc = b.toGeometry();
    if (!gc) throw new Error("cyclist geometry is empty");
    return gc;
  }

  if (def.category === 1) {
    // A person on foot: legs apart, a torso a little narrower than the shoulders, arms at the
    // sides and a head. Reads as a person from the chase camera and from above; the legs are split
    // across the body's width so the silhouette is not a post.
    buildPedestrian(b, L, W, H, lod);
    const gv = b.toGeometry();
    if (!gv) throw new Error("VRU geometry is empty");
    return gv;
  }

  const isTwoWheeler = def.name === "moto" || def.name === "bicycle";
  const wheelR = isTwoWheeler ? Math.min(0.35, H * 0.3) : Math.min(0.36, H * 0.28);
  const bodyZ0 = isTwoWheeler ? wheelR * 0.8 : wheelR * 0.75;
  const bodyH = isTwoWheeler ? H * 0.42 : H * 0.48;
  const cabinH = H - bodyZ0 - bodyH;

  // Body.
  addBox(b, 0, 0, bodyZ0 + bodyH / 2, L, W, bodyH, 0, 1, 1, 1);

  // Cabin, set back from the nose. Buses and trucks keep a full-width box; cars get a narrower one.
  if (cabinH > 0.05) {
    const cabinL = def.name === "bus" ? L * 0.9 : def.name === "truck" ? L * 0.34 : L * 0.5;
    const cabinX = def.name === "truck" ? L * 0.28 : def.name === "bus" ? 0 : -L * 0.05;
    const cabinW = isTwoWheeler ? W * 0.7 : W * 0.88;
    addBox(b, cabinX, 0, bodyZ0 + bodyH + cabinH / 2, cabinL, cabinW, cabinH, 0, 0.55, 0.62, 0.72);
    if (lod === 0 && !isTwoWheeler && def.name !== "bus") {
      // Windscreen wedge: a thin darker slab leaning over the nose end of the cabin.
      addBox(b, cabinX + cabinL / 2, 0, bodyZ0 + bodyH + cabinH * 0.55,
        cabinL * 0.18, cabinW * 0.94, cabinH * 0.8, 0, 0.3, 0.38, 0.5);
    }
  }

  if (lod === 0) {
    const axleX = L * 0.33;
    const wheelY = W / 2 - Math.min(0.12, W * 0.08);
    const wheelW = Math.min(0.26, W * 0.16);
    const ys = isTwoWheeler ? [0] : [wheelY, -wheelY];
    for (const wy of ys) {
      for (const wx of [axleX, -axleX]) {
        addBox(b, wx, wy, wheelR, wheelR * 2, wheelW, wheelR * 2, 0, 0.12, 0.12, 0.14);
      }
    }
    if (def.name === "emergency") {
      // Light bar, so the class is recognisable without a texture.
      addBox(b, -L * 0.05, 0, H + 0.08, L * 0.18, W * 0.7, 0.16, 0, 1, 0.25, 0.2);
    }
  }

  const g = b.toGeometry();
  if (!g) throw new Error(`actor geometry for class ${def.name} is empty`);
  return g;
}

/** True for a vulnerable-road-user class that rides something: a bicycle or a scooter. */
export function isRiddenVru(name: string): boolean {
  return name === "bicycle" || name === "scooter" || name === "cyclist";
}

/**
 * A person on foot, `H` tall, facing +x: two legs, a torso, two arms and a head. The class table's
 * width is shoulder to shoulder and its length front to back, as `VehicleClass::Pedestrian` sizes
 * the body the mobility model moves.
 */
function buildPedestrian(b: MeshBuilder, L: number, W: number, H: number, lod: LodLevel): void {
  const legH = H * 0.47;
  const torsoH = H * 0.33;
  const headR = Math.min(W * 0.32, H * 0.075);
  const torsoW = W * 0.72;
  const depth = Math.min(L, W) * 0.55;
  // Trousers, darker than the torso so the gait reads.
  addBox(b, 0, -torsoW * 0.26, legH / 2, depth * 0.8, torsoW * 0.4, legH, 0, 0.32, 0.36, 0.48);
  addBox(b, 0, torsoW * 0.26, legH / 2, depth * 0.8, torsoW * 0.4, legH, 0, 0.32, 0.36, 0.48);
  // Torso: takes the class colour.
  addBox(b, 0, 0, legH + torsoH / 2, depth, torsoW, torsoH, 0, 1, 1, 1);
  if (lod === 0) {
    // Arms hang beside the torso.
    const armH = torsoH * 0.95;
    const armZ = legH + torsoH - armH / 2;
    addBox(b, 0, -(torsoW / 2 + W * 0.08), armZ, depth * 0.6, W * 0.14, armH, 0, 0.85, 0.85, 0.9);
    addBox(b, 0, torsoW / 2 + W * 0.08, armZ, depth * 0.6, W * 0.14, armH, 0, 0.85, 0.85, 0.9);
    // Neck and head, skin-toned.
    const z0 = legH + torsoH;
    addCylinder(b, 0, 0, z0, z0 + headR * 0.5, headR * 0.45, 6, 0.9, 0.78, 0.66);
    addCylinder(b, 0, 0, z0 + headR * 0.4, z0 + headR * 2.4, headR, 10, 0.93, 0.8, 0.68);
  } else {
    addBox(b, 0, 0, legH + torsoH + headR, depth, headR * 1.6, headR * 2, 0, 0.93, 0.8, 0.68);
  }
}

/**
 * A rider on a bicycle, facing +x: two wheels, the frame between them, and the rider sitting on
 * it with the torso leaning forward to the bars. `L` is the bicycle's length (wheel to wheel), `H`
 * the rider's height on it.
 */
function buildCyclist(b: MeshBuilder, L: number, W: number, H: number, lod: LodLevel): void {
  const wheelR = Math.min(0.34, L * 0.21);
  const axle = L / 2 - wheelR;
  const tyre = Math.min(0.06, W * 0.1);
  // Wheels: thin vertical discs, dark tyres.
  for (const x of [axle, -axle]) {
    if (lod === 0) {
      // Four boxes rotated around the hub make a readable wheel from the side.
      for (let k = 0; k < 4; k++) {
        const a = (k * Math.PI) / 4;
        // Each spoke pair spans the wheel's diameter and never reaches below the road.
        addBox(b, x, 0, wheelR, Math.max(wheelR * 2 * Math.abs(Math.cos(a)), tyre), tyre,
          Math.max(wheelR * 2 * Math.abs(Math.sin(a)), tyre), 0, 0.1, 0.1, 0.12);
      }
    } else {
      addBox(b, x, 0, wheelR, wheelR * 2, tyre, wheelR * 2, 0, 0.1, 0.1, 0.12);
    }
  }
  // Frame: the top tube and the down tube, at hub-to-saddle height.
  const frameZ = wheelR * 1.25;
  addBox(b, 0, 0, frameZ, axle * 2, tyre * 1.2, tyre * 1.5, 0, 0.55, 0.6, 0.65);
  addBox(b, -axle * 0.15, 0, (wheelR + frameZ + wheelR * 0.9) / 2, tyre * 1.5, tyre * 1.2,
    frameZ + wheelR * 0.9 - wheelR, 0, 0.55, 0.6, 0.65);
  // Handlebars at the front.
  const barZ = wheelR * 2.25;
  addBox(b, axle * 0.85, 0, barZ, tyre * 1.5, Math.min(W, 0.6), tyre * 1.5, 0, 0.2, 0.2, 0.22);
  // Rider: legs down to the pedals, a torso leaning forward, a head.
  const saddleZ = wheelR * 2.1;
  const legW = Math.min(W * 0.25, 0.16);
  addBox(b, -axle * 0.1, -legW, (wheelR + saddleZ) / 2, 0.14, legW, saddleZ - wheelR, 0, 0.32, 0.36, 0.48);
  addBox(b, -axle * 0.1, legW, (wheelR + saddleZ) / 2, 0.14, legW, saddleZ - wheelR, 0, 0.32, 0.36, 0.48);
  const torsoH = Math.max(0.3, H - saddleZ - 0.28);
  // The torso takes the class colour: lean it by setting it forward of the saddle.
  addBox(b, axle * 0.2, 0, saddleZ + torsoH / 2, axle * 0.7, Math.min(W * 0.7, 0.4), torsoH, 0, 1, 1, 1);
  if (lod === 0) {
    const headR = 0.11;
    const hx = axle * 0.4;
    addCylinder(b, hx, 0, saddleZ + torsoH, saddleZ + torsoH + headR * 2, headR, 10, 0.93, 0.8, 0.68);
    // Arms from the shoulders to the bars.
    addBox(b, (axle * 0.4 + axle * 0.85) / 2, -0.18, (saddleZ + torsoH * 0.85 + barZ) / 2,
      axle * 0.5, 0.08, 0.08, 0, 0.85, 0.85, 0.9);
    addBox(b, (axle * 0.4 + axle * 0.85) / 2, 0.18, (saddleZ + torsoH * 0.85 + barZ) / 2,
      axle * 0.5, 0.08, 0.08, 0, 0.85, 0.85, 0.9);
  }
}

/** A ring in the xy plane, `innerRadius..1`, for the transmission-pulse overlay. */
export function ringGeometry(innerRadius: number, segments: number): BufferGeometry {
  const b = new MeshBuilder({ uv: true, vertexCapacity: segments * 2 + 2, indexCapacity: segments * 6 });
  const n = Math.max(6, segments | 0);
  for (let i = 0; i <= n; i++) {
    const a = (i / n) * Math.PI * 2;
    const c = Math.cos(a);
    const s = Math.sin(a);
    b.addVertex(c * innerRadius, s * innerRadius, 0, 0, 0, 1, 0, 0);
    b.addVertex(c, s, 0, 0, 0, 1, 1, 0);
  }
  for (let i = 0; i < n; i++) {
    const a = i * 2;
    b.addQuad(a, a + 1, a + 3, a + 2);
  }
  const g = b.toGeometry();
  if (!g) throw new Error("ring geometry is empty");
  return g;
}

/**
 * A filled unit-radius disc in the xy plane, for a mark whose instance matrix is a pure
 * scale-and-translate.
 *
 * The companion to {@link ringGeometry}, which is the same circle with its middle taken out: the
 * aerial vehicle mark switches between the two as a vehicle grows past its own mark, so the pair
 * has to share a radius convention — both are one metre across before the instance scale.
 *
 * `perInstanceColor` is not a style choice; see {@link withUnitVertexColors}.
 */
export function discGeometry(segments: number, perInstanceColor = false): BufferGeometry {
  const n = Math.max(6, segments | 0);
  const b = new MeshBuilder({ uv: true, color: perInstanceColor, vertexCapacity: n + 1, indexCapacity: n * 3 });
  addDisc(b, 0, 0, 0, 1, n);
  const g = b.toGeometry();
  if (!g) throw new Error("disc geometry is empty");
  return g;
}

/**
 * Give a geometry the unit-white `color` attribute that `InstancedMesh.setColorAt` needs to have
 * any effect — the trap this function exists to name.
 *
 * three applies an instance colour only inside `#ifdef USE_COLOR`, and `USE_COLOR` comes from
 * `material.vertexColors` (three 0.186.0, `color_vertex.glsl.js` and `color_fragment.glsl.js`:
 * `vColor` is declared and multiplied into `diffuseColor` only under `USE_COLOR`, and
 * `USE_INSTANCING_COLOR` on its own multiplies a varying nothing ever reads). So per-instance
 * colour requires `vertexColors: true`, and `vertexColors: true` requires a `color` attribute —
 * without one WebGL supplies the missing attribute as `(0, 0, 0, 1)` and `vColor.rgb *= color`
 * zeroes it. The failure is silent and total: the mesh draws, at the right size, in the right
 * place, **black**. That is exactly how the first version of the aerial vehicle mark shipped, and
 * on a dark basemap a black dot is indistinguishable from no dot at all.
 *
 * `buildActorGeometry` avoids it by building with `color: true`; anything reused from a geometry
 * that was not needs this.
 */
export function withUnitVertexColors(geometry: BufferGeometry): BufferGeometry {
  if (geometry.getAttribute("color")) return geometry;
  const n = geometry.getAttribute("position").count;
  const colors = new Float32Array(n * 3).fill(1);
  geometry.setAttribute("color", new BufferAttribute(colors, 3));
  return geometry;
}

/** Marker shapes for the state overlay; shape redundancy for the colour-blind palette (09-ui §10). */
export type MarkerShape = "triangle" | "square" | "diamond" | "cross";

/** A flat marker in the xy plane, one metre across, for billboarded state markers. */
export function markerGeometry(shape: MarkerShape): BufferGeometry {
  const b = new MeshBuilder({ vertexCapacity: 16, indexCapacity: 24 });
  switch (shape) {
    case "triangle": {
      const v0 = b.addVertex(0, 0.6, 0, 0, 0, 1);
      const v1 = b.addVertex(-0.55, -0.4, 0, 0, 0, 1);
      const v2 = b.addVertex(0.55, -0.4, 0, 0, 0, 1);
      b.addTriangle(v0, v1, v2);
      break;
    }
    case "square": {
      const v0 = b.addVertex(-0.5, -0.5, 0, 0, 0, 1);
      const v1 = b.addVertex(0.5, -0.5, 0, 0, 0, 1);
      const v2 = b.addVertex(0.5, 0.5, 0, 0, 0, 1);
      const v3 = b.addVertex(-0.5, 0.5, 0, 0, 0, 1);
      b.addQuad(v0, v1, v2, v3);
      break;
    }
    case "diamond": {
      const v0 = b.addVertex(0, -0.62, 0, 0, 0, 1);
      const v1 = b.addVertex(0.62, 0, 0, 0, 0, 1);
      const v2 = b.addVertex(0, 0.62, 0, 0, 0, 1);
      const v3 = b.addVertex(-0.62, 0, 0, 0, 0, 1);
      b.addQuad(v0, v1, v2, v3);
      break;
    }
    case "cross": {
      const t = 0.18;
      const a0 = b.addVertex(-0.6, -t, 0, 0, 0, 1);
      const a1 = b.addVertex(0.6, -t, 0, 0, 0, 1);
      const a2 = b.addVertex(0.6, t, 0, 0, 0, 1);
      const a3 = b.addVertex(-0.6, t, 0, 0, 0, 1);
      b.addQuad(a0, a1, a2, a3);
      const c0 = b.addVertex(-t, -0.6, 0, 0, 0, 1);
      const c1 = b.addVertex(t, -0.6, 0, 0, 0, 1);
      const c2 = b.addVertex(t, 0.6, 0, 0, 0, 1);
      const c3 = b.addVertex(-t, 0.6, 0, 0, 0, 1);
      b.addQuad(c0, c1, c2, c3);
      break;
    }
  }
  const g = b.toGeometry();
  if (!g) throw new Error("marker geometry is empty");
  return g;
}

/** Scratch vector shared by module-level helpers that need one; never escapes a call. */
export const SCRATCH_VEC = new Vector3();
