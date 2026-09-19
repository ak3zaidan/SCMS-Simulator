/**
 * Click-to-select.
 *
 * 09-ui §4 suggests `three-mesh-bvh` for picking. This implementation uses a **uniform grid over the
 * actor positions plus an exact ray/OBB test** instead, and the reason is the data, not a dislike of
 * the library: a BVH accelerates rays against a *static triangle mesh*, but the actors are 5,000
 * instances that all move every frame, so a BVH over them would have to be refit every frame — and
 * `three-mesh-bvh`'s own `InstancedMesh` support still walks every instance's bounds on the CPU. The
 * grid here is built **lazily, on the first pick after the poses changed**, which for an interactive
 * click means once per click rather than once per frame, in O(live actors) with no allocation after
 * warm-up. The narrow phase is an exact oriented-box test against the class dimensions from `Hello`,
 * which is more accurate than a triangle hit on the LOD-2 box the renderer may actually be drawing.
 *
 * If a future need appears for ray tests against the *static* world (roads, building shells at
 * metre accuracy), that is where `three-mesh-bvh` would earn its place; {@link Picker.pickGround}
 * currently answers with an analytic plane intersection, which is what the map view needs.
 */

import { Vector3 } from "three";
import type { PerspectiveCamera } from "three";
import type { ActorClassDef, PickResult } from "./types.js";
import type { WorldRenderer } from "./world-render.js";

/** Tuning for {@link Picker}. */
export interface PickerOptions {
  readonly camera: PerspectiveCamera;
  /** Class dimensions, the same table the actor renderer uses. */
  readonly classes: readonly ActorClassDef[];
  readonly world?: WorldRenderer | null;
  /** Broad-phase cell edge in metres. Default 24. */
  readonly cellSizeM?: number;
  /** Stop walking the ray after this distance. Default 5,000 m. */
  readonly maxDistanceM?: number;
  /** Pick radius around an RSU mast head, metres. Default 3. */
  readonly siteRadiusM?: number;
}

/** The pose columns the picker tests against. */
export interface PickerPoses {
  readonly position: Float32Array;
  readonly heading: Float32Array;
  readonly occupied: Uint8Array;
  readonly actorId: Uint32Array;
  readonly classIdx: Uint8Array;
  readonly count: number;
}

const MAX_CELLS_WALKED = 4096;

/**
 * Ray-picks actors, infrastructure sites and the ground plane.
 *
 * ```ts
 * picker.setPoses(interp);            // once per frame, O(1)
 * const hit = picker.pickAtPixel(ev.offsetX, ev.offsetY, w, h);
 * if (hit?.kind === "actor") camera.flyTo(hit.actorId, "chase");
 * ```
 */
export class Picker {
  #camera: PerspectiveCamera;
  #classes: ActorClassDef[];
  #world: WorldRenderer | null;
  #cell: number;
  #maxDistance: number;
  #siteRadius: number;

  #pos: Float32Array = new Float32Array(0);
  #head: Float32Array = new Float32Array(0);
  #occ: Uint8Array = new Uint8Array(0);
  #ids: Uint32Array = new Uint32Array(0);
  #cls: Uint8Array = new Uint8Array(0);
  #count = 0;
  #dirty = true;

  #gridMinX = 0;
  #gridMinY = 0;
  #gridW = 0;
  #gridH = 0;
  #gridStart = new Int32Array(0);
  #gridItems = new Int32Array(0);
  #gridCounts = new Int32Array(0);
  #zMin = 0;
  #zMax = 0;

  #origin = new Vector3();
  #dir = new Vector3();
  #scratch = new Vector3();

  constructor(options: PickerOptions) {
    this.#camera = options.camera;
    this.#classes = [...options.classes];
    this.#world = options.world ?? null;
    this.#cell = Math.max(2, options.cellSizeM ?? 24);
    this.#maxDistance = options.maxDistanceM ?? 5000;
    this.#siteRadius = options.siteRadiusM ?? 3;
  }

  /** Replace the class table (a new `Hello`). */
  setClasses(classes: readonly ActorClassDef[]): void {
    this.#classes = [...classes];
  }

  /** Attach the world, enabling site and ground picks. */
  setWorld(world: WorldRenderer | null): void {
    this.#world = world;
  }

  /** Point the picker at a different camera. */
  setCamera(camera: PerspectiveCamera): void {
    this.#camera = camera;
  }

  /**
   * Hand over this frame's interpolated poses. O(1): it only stores the references and marks the
   * broad-phase grid stale, so calling it every frame costs nothing when nobody clicks.
   */
  setPoses(poses: PickerPoses): void {
    this.#pos = poses.position;
    this.#head = poses.heading;
    this.#occ = poses.occupied;
    this.#ids = poses.actorId;
    this.#cls = poses.classIdx;
    this.#count = poses.count;
    this.#dirty = true;
  }

  /** Force a rebuild on the next pick. */
  invalidate(): void {
    this.#dirty = true;
  }

  /** Cells the broad-phase grid currently holds, for tests. */
  get gridCells(): number {
    return this.#gridW * this.#gridH;
  }

  /** Pick from a pixel in the canvas. `(0, 0)` is the top-left corner. */
  pickAtPixel(px: number, py: number, width: number, height: number): PickResult {
    const ndcX = (px / Math.max(1, width)) * 2 - 1;
    const ndcY = -((py / Math.max(1, height)) * 2 - 1);
    return this.pickAtNdc(ndcX, ndcY);
  }

  /** Pick from normalised device coordinates, both in `[-1, 1]`. */
  pickAtNdc(ndcX: number, ndcY: number): PickResult {
    this.#buildRay(ndcX, ndcY);
    const actor = this.#pickActor();
    const site = this.#pickSite();
    if (actor && (!site || actor.distanceM <= site.distanceM)) return actor;
    if (site) return site;
    return this.pickGroundFromRay();
  }

  /** Where the ray through a pixel meets the ground plane, or null if it never does. */
  pickGround(ndcX: number, ndcY: number): PickResult {
    this.#buildRay(ndcX, ndcY);
    return this.pickGroundFromRay();
  }

  /** The ground hit for the ray built by the last {@link pickAtNdc} or {@link pickGround}. */
  pickGroundFromRay(): PickResult {
    const groundZ = this.#world?.world?.bbox.minZM ?? 0;
    const dz = this.#dir.z;
    if (Math.abs(dz) < 1e-6) return null;
    const t = (groundZ - this.#origin.z) / dz;
    if (t <= 0 || t > this.#maxDistance) return null;
    return {
      kind: "ground",
      distanceM: t,
      point: {
        x: this.#origin.x + this.#dir.x * t,
        y: this.#origin.y + this.#dir.y * t,
        z: groundZ,
      },
    };
  }

  #buildRay(ndcX: number, ndcY: number): void {
    this.#camera.updateMatrixWorld();
    this.#origin.setFromMatrixPosition(this.#camera.matrixWorld);
    this.#dir.set(ndcX, ndcY, 0.5).unproject(this.#camera).sub(this.#origin).normalize();
  }

  #ensureGrid(): void {
    if (!this.#dirty) return;
    this.#dirty = false;
    const n = this.#count;
    if (n === 0) {
      this.#gridW = 0;
      this.#gridH = 0;
      return;
    }
    let minX = Infinity;
    let minY = Infinity;
    let maxX = -Infinity;
    let maxY = -Infinity;
    let minZ = Infinity;
    let maxZ = -Infinity;
    for (let i = 0; i < n; i++) {
      if (this.#occ[i] === 0) continue;
      const p = i * 3;
      const x = this.#pos[p];
      const y = this.#pos[p + 1];
      const z = this.#pos[p + 2];
      if (x < minX) minX = x;
      if (x > maxX) maxX = x;
      if (y < minY) minY = y;
      if (y > maxY) maxY = y;
      if (z < minZ) minZ = z;
      if (z > maxZ) maxZ = z;
    }
    if (!Number.isFinite(minX)) {
      this.#gridW = 0;
      this.#gridH = 0;
      return;
    }
    const cell = this.#cell;
    this.#gridMinX = minX - cell;
    this.#gridMinY = minY - cell;
    this.#gridW = Math.max(1, Math.ceil((maxX - minX) / cell) + 3);
    this.#gridH = Math.max(1, Math.ceil((maxY - minY) / cell) + 3);
    this.#zMin = minZ - 1;
    this.#zMax = maxZ + 6;

    const cells = this.#gridW * this.#gridH;
    if (this.#gridStart.length < cells + 1) this.#gridStart = new Int32Array(cells + 1);
    else this.#gridStart.fill(0, 0, cells + 1);
    if (this.#gridCounts.length < cells) this.#gridCounts = new Int32Array(cells);

    let live = 0;
    for (let i = 0; i < n; i++) {
      if (this.#occ[i] === 0) continue;
      const c = this.#cellOf(this.#pos[i * 3], this.#pos[i * 3 + 1]);
      if (c < 0) continue;
      this.#gridStart[c + 1]++;
      live++;
    }
    for (let i = 0; i < cells; i++) this.#gridStart[i + 1] += this.#gridStart[i];
    if (this.#gridItems.length < live) this.#gridItems = new Int32Array(Math.max(live, 64));
    this.#gridCounts.set(this.#gridStart.subarray(0, cells));
    for (let i = 0; i < n; i++) {
      if (this.#occ[i] === 0) continue;
      const c = this.#cellOf(this.#pos[i * 3], this.#pos[i * 3 + 1]);
      if (c < 0) continue;
      this.#gridItems[this.#gridCounts[c]++] = i;
    }
  }

  #cellOf(x: number, y: number): number {
    const ix = Math.floor((x - this.#gridMinX) / this.#cell);
    const iy = Math.floor((y - this.#gridMinY) / this.#cell);
    if (ix < 0 || iy < 0 || ix >= this.#gridW || iy >= this.#gridH) return -1;
    return iy * this.#gridW + ix;
  }

  /**
   * Broad phase: clip the ray to the slab of z the actors occupy, then walk the grid cells the
   * clipped segment crosses with a 2D DDA. Bounded by {@link MAX_CELLS_WALKED} so a grazing ray in a
   * huge world cannot turn a click into a stall.
   */
  #pickActor(): PickResult {
    this.#ensureGrid();
    if (this.#gridW === 0) return null;

    const o = this.#origin;
    const d = this.#dir;
    // Clip against the z slab.
    let t0 = 0;
    let t1 = this.#maxDistance;
    if (Math.abs(d.z) < 1e-9) {
      if (o.z < this.#zMin || o.z > this.#zMax) return null;
    } else {
      const ta = (this.#zMin - o.z) / d.z;
      const tb = (this.#zMax - o.z) / d.z;
      t0 = Math.max(t0, Math.min(ta, tb));
      t1 = Math.min(t1, Math.max(ta, tb));
      if (t1 <= t0) return null;
    }

    const cell = this.#cell;
    const sx = o.x + d.x * t0;
    const sy = o.y + d.y * t0;
    const ex = o.x + d.x * t1;
    const ey = o.y + d.y * t1;

    let best = -1;
    let bestT = Infinity;

    const test = (ix: number, iy: number): void => {
      if (ix < 0 || iy < 0 || ix >= this.#gridW || iy >= this.#gridH) return;
      const c = iy * this.#gridW + ix;
      const from = this.#gridStart[c];
      const to = this.#gridStart[c + 1];
      for (let k = from; k < to; k++) {
        const slot = this.#gridItems[k];
        const t = this.#rayBox(slot);
        if (t >= 0 && t < bestT) {
          bestT = t;
          best = slot;
        }
      }
    };

    let ix = Math.floor((sx - this.#gridMinX) / cell);
    let iy = Math.floor((sy - this.#gridMinY) / cell);
    const ixEnd = Math.floor((ex - this.#gridMinX) / cell);
    const iyEnd = Math.floor((ey - this.#gridMinY) / cell);

    const dxWorld = ex - sx;
    const dyWorld = ey - sy;
    const stepX = dxWorld > 0 ? 1 : dxWorld < 0 ? -1 : 0;
    const stepY = dyWorld > 0 ? 1 : dyWorld < 0 ? -1 : 0;
    const invDx = dxWorld !== 0 ? 1 / Math.abs(dxWorld) : Infinity;
    const invDy = dyWorld !== 0 ? 1 / Math.abs(dyWorld) : Infinity;
    const nextBoundaryX = this.#gridMinX + (ix + (stepX > 0 ? 1 : 0)) * cell;
    const nextBoundaryY = this.#gridMinY + (iy + (stepY > 0 ? 1 : 0)) * cell;
    let tMaxX = stepX === 0 ? Infinity : Math.abs(nextBoundaryX - sx) * invDx;
    let tMaxY = stepY === 0 ? Infinity : Math.abs(nextBoundaryY - sy) * invDy;
    const tDeltaX = stepX === 0 ? Infinity : cell * invDx;
    const tDeltaY = stepY === 0 ? Infinity : cell * invDy;

    // Test a 1-cell halo so an actor whose centre sits in a neighbouring cell is still found.
    let walked = 0;
    for (;;) {
      for (let oy = -1; oy <= 1; oy++) for (let ox = -1; ox <= 1; ox++) test(ix + ox, iy + oy);
      if ((ix === ixEnd && iy === iyEnd) || ++walked >= MAX_CELLS_WALKED) break;
      if (tMaxX < tMaxY) {
        if (tMaxX > 1) break;
        ix += stepX;
        tMaxX += tDeltaX;
      } else {
        if (tMaxY > 1) break;
        iy += stepY;
        tMaxY += tDeltaY;
      }
      if (stepX === 0 && stepY === 0) break;
    }

    if (best < 0) return null;
    return {
      kind: "actor",
      actorId: this.#ids[best],
      slot: best,
      distanceM: bestT,
      point: {
        x: o.x + d.x * bestT,
        y: o.y + d.y * bestT,
        z: o.z + d.z * bestT,
      },
    };
  }

  /**
   * Exact ray/oriented-box test for one slot. The box is the class's `length × width × height`,
   * centred half a height above the pose origin and yawed by the heading — the same box the
   * instanced mesh draws into. Returns the entry distance, or −1 for a miss.
   */
  #rayBox(slot: number): number {
    let ci = this.#cls[slot];
    if (ci >= this.#classes.length) ci = 0;
    const def = this.#classes[ci];
    if (!def) return -1;
    const p = slot * 3;
    const cx = this.#pos[p];
    const cy = this.#pos[p + 1];
    const cz = this.#pos[p + 2] + def.heightM / 2;
    const h = this.#head[slot];
    const c = Math.cos(h);
    const s = Math.sin(h);

    // Ray into the box's local frame: translate then rotate by −h about +z.
    const ox = this.#origin.x - cx;
    const oy = this.#origin.y - cy;
    const oz = this.#origin.z - cz;
    const lox = ox * c + oy * s;
    const loy = -ox * s + oy * c;
    const ldx = this.#dir.x * c + this.#dir.y * s;
    const ldy = -this.#dir.x * s + this.#dir.y * c;
    const ldz = this.#dir.z;

    // A little slack so a thin pedestrian is still clickable.
    const hx = Math.max(0.4, def.lengthM / 2);
    const hy = Math.max(0.4, def.widthM / 2);
    const hz = Math.max(0.4, def.heightM / 2);

    let tMin = 0;
    let tMax = this.#maxDistance;
    // x slab
    if (Math.abs(ldx) < 1e-9) {
      if (lox < -hx || lox > hx) return -1;
    } else {
      const inv = 1 / ldx;
      let t1 = (-hx - lox) * inv;
      let t2 = (hx - lox) * inv;
      if (t1 > t2) { const t = t1; t1 = t2; t2 = t; }
      if (t1 > tMin) tMin = t1;
      if (t2 < tMax) tMax = t2;
      if (tMin > tMax) return -1;
    }
    // y slab
    if (Math.abs(ldy) < 1e-9) {
      if (loy < -hy || loy > hy) return -1;
    } else {
      const inv = 1 / ldy;
      let t1 = (-hy - loy) * inv;
      let t2 = (hy - loy) * inv;
      if (t1 > t2) { const t = t1; t1 = t2; t2 = t; }
      if (t1 > tMin) tMin = t1;
      if (t2 < tMax) tMax = t2;
      if (tMin > tMax) return -1;
    }
    // z slab
    if (Math.abs(ldz) < 1e-9) {
      if (oz < -hz || oz > hz) return -1;
    } else {
      const inv = 1 / ldz;
      let t1 = (-hz - oz) * inv;
      let t2 = (hz - oz) * inv;
      if (t1 > t2) { const t = t1; t1 = t2; t2 = t; }
      if (t1 > tMin) tMin = t1;
      if (t2 < tMax) tMax = t2;
      if (tMin > tMax) return -1;
    }
    return tMin >= 0 ? tMin : -1;
  }

  /** Nearest RSU/cell mast head the ray passes within {@link PickerOptions.siteRadiusM} of. */
  #pickSite(): PickResult {
    const world = this.#world;
    if (!world || world.siteCount === 0) return null;
    const p = world.sitePositions;
    const o = this.#origin;
    const d = this.#dir;
    const r2 = this.#siteRadius * this.#siteRadius;
    let best = -1;
    let bestT = Infinity;
    for (let i = 0; i < world.siteCount; i++) {
      const sx = p[i * 3] - o.x;
      const sy = p[i * 3 + 1] - o.y;
      const sz = p[i * 3 + 2] - o.z;
      const t = sx * d.x + sy * d.y + sz * d.z;
      if (t <= 0 || t > this.#maxDistance || t >= bestT) continue;
      const ex = sx - d.x * t;
      const ey = sy - d.y * t;
      const ez = sz - d.z * t;
      if (ex * ex + ey * ey + ez * ez > r2) continue;
      bestT = t;
      best = i;
    }
    if (best < 0) return null;
    this.#scratch.set(o.x + d.x * bestT, o.y + d.y * bestT, o.z + d.z * bestT);
    return {
      kind: "site",
      siteId: world.siteIds[best],
      nodeId: world.siteNodeIds[best],
      distanceM: bestT,
      point: { x: this.#scratch.x, y: this.#scratch.y, z: this.#scratch.z },
    };
  }
}
