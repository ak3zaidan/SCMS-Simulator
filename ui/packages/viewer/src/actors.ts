/**
 * Instanced actor rendering.
 *
 * 09-ui §4: "one `InstancedMesh` per (class × LOD level); capacity preallocated, `count` set to the
 * visible instances after CPU frustum culling; instances moved between LOD meshes by distance",
 * citing the measurement that count-based culling plus per-LOD meshes almost doubled the frame rate
 * on an integrated GPU. That is exactly the loop below:
 *
 * 1. one pass over the live slots computes distance and the LOD band, then tests a bounding sphere
 *    against the six frustum planes with plain arithmetic (no `Sphere`, no `Vector3`, no allocation);
 * 2. survivors are appended to their `(class, lod)` bucket — the 16 floats of the instance matrix are
 *    written straight into `instanceMatrix.array`, because the matrix is only a yaw about +z and a
 *    translation, so composing it through `Matrix4`/`Quaternion` would be pure overhead;
 * 3. each bucket's `count` is set to the number written and only that prefix of the buffer is
 *    uploaded, via `addUpdateRange`.
 *
 * Nothing in {@link ActorRenderer.update} allocates once the buckets have reached their steady-state
 * capacity: no `Object3D`, no `Matrix4`, no array, no closure — and no `{start, count}` either,
 * which is what `BufferAttribute.addUpdateRange` would push on every call, so each attribute owns
 * one range object that is mutated and re-pushed instead (three clears `updateRanges` after every
 * upload, so it has to be pushed again each frame).
 *
 * Instance *colours* are uploaded only when they change. A colour is a function of selection and of
 * the §3.3.4 state bits, neither of which changes most frames, unlike the matrices; each bucket
 * remembers the colour key it last wrote per instance slot and skips both the write and the upload
 * while they match. {@link ActorRenderer.legend} is the same decision, evaluated for a legend, so
 * the DOM and the scene cannot disagree about what a state looks like.
 */

import {
  Color,
  DynamicDrawUsage,
  Group,
  InstancedMesh,
  Matrix4,
  MeshLambertMaterial,
  type BufferGeometry,
  type Camera,
  type Material,
} from "three";
import { ActorState } from "@vwp/protocol";
import { buildActorGeometry } from "./geometry.js";
import type { ActorClassDef, LodLevel } from "./types.js";
import type { ActorStateColorKey, ViewerTheme } from "./theme.js";

/** Tuning for {@link ActorRenderer}. */
export interface ActorRendererOptions {
  /** Class table, normally derived from `Hello.classes` by {@link classesFromHello}. */
  readonly classes: readonly ActorClassDef[];
  readonly theme: ViewerTheme;
  /** Hard ceiling on drawn instances across all buckets. Default 20,000. */
  readonly maxActors?: number;
  /** Starting capacity of each `(class, LOD)` bucket. Default 128. */
  readonly initialCapacity?: number;
  /** `[LOD0→LOD1, LOD1→LOD2]` switch distances in metres. Default `[90, 400]`. */
  readonly lodDistancesM?: readonly [number, number];
  /** Extra metres added to every bounding radius before the frustum test. Default 1. */
  readonly cullMarginM?: number;
  /** Let LOD-0 actors cast shadows. Default false — 5,000 shadow casters is not a 60 fps budget. */
  readonly castShadows?: boolean;
  /**
   * Height added to every actor's z, metres. Default 0.1 — the height the world renderer draws the
   * road surface at above the lane centreline (`Z_ROAD` in `world-render.ts`), so a vehicle's tyres
   * sit on the asphalt instead of 10 cm into it.
   */
  readonly groundOffsetM?: number;
  /** Draw ground-truth-only state (the `ATTACKER` bit) in the actor colour. Default true. */
  readonly showGroundTruth?: boolean;
  /**
   * Paint actors with no state bit set in their *class* colour instead of `theme.actorState.benign`.
   *
   * Default false. 09-ui §10 names benign as one of the four categorical state colours, and the
   * legend and the `--state-benign` custom property are drawn from `theme.actorState.benign`, so
   * the scene has to use it too or the legend is a lie (finding Q13). Class identity is carried by
   * the per-class silhouette `buildActorGeometry` builds, and by the class colours
   * {@link ActorRenderer.legend} reports when this is on.
   */
  readonly colorBenignByClass?: boolean;
}

/**
 * The colour buckets an actor can be painted in, in the order `theme.actorState` declares them.
 * The index into this array *is* the index into the renderer's packed state-colour table.
 */
export const ACTOR_STATE_COLOR_KEYS: readonly ActorStateColorKey[] = [
  "benign", "attacker", "reported", "revoked", "selected",
];

/** Colour key for "this actor's class colour", one past the state keys. */
const CLASS_COLOR_KEY = ACTOR_STATE_COLOR_KEYS.length;

/**
 * Which colour bucket an actor falls into, as an index into {@link ACTOR_STATE_COLOR_KEYS}:
 * §3.3.4 bits 0–2, selection first, and `benign` when no bit is set.
 *
 * This is the single decision every place that paints an actor shares — {@link ActorRenderer.update}
 * writing instance colours, {@link ActorRenderer.legend} describing them, and the aerial vehicle
 * mark in `overlays.ts`, which is the *only* thing on screen at map altitude and would otherwise
 * have to re-derive the state colour for itself. A second copy of these four lines is how the
 * legend and the scene come to disagree, so there is one.
 *
 * It returns a number rather than a key because the callers index packed colour tables with it;
 * {@link actorColorKey} is the same answer named.
 */
export function actorStateColorIndex(
  state: number,
  selected: boolean,
  showGroundTruth = true,
): number {
  if (selected) return 4;
  if (state & ActorState.REVOKED) return 3;
  if (showGroundTruth && (state & ActorState.ATTACKER)) return 1;
  if (state & ActorState.REPORTED) return 2;
  return 0;
}

/**
 * Which colour bucket an actor falls into: §3.3.4 bits 0–2, selection first, and `benign` when no
 * bit is set. This is the single decision {@link ActorRenderer.update} and
 * {@link ActorRenderer.legend} share; nothing else may re-implement it.
 */
export function actorColorKey(
  state: number,
  selected: boolean,
  showGroundTruth = true,
): ActorStateColorKey {
  return ACTOR_STATE_COLOR_KEYS[actorStateColorIndex(state, selected, showGroundTruth)];
}

/** One row of {@link ActorRenderer.legend}: a colour the scene really draws, and what it means. */
export interface ActorLegendEntry {
  /** A state bucket, or `"class"` for a per-class row. */
  readonly kind: "state" | "class";
  /** The `theme.actorState` key, or the class name. */
  readonly key: string;
  readonly label: string;
  /** Packed `0xRRGGBB`, exactly the value written into `instanceColor`. */
  readonly color: number;
  /** Set on `kind: "class"` rows. */
  readonly classIndex?: number;
}

const STATE_LABELS: Readonly<Record<ActorStateColorKey, string>> = {
  benign: "Benign",
  attacker: "Attacker (GT)",
  reported: "Reported",
  revoked: "Revoked",
  selected: "Selected",
};

/** The per-frame inputs {@link ActorRenderer.update} reads. All arrays are indexed by slot. */
export interface ActorUpdateContext {
  readonly position: Float32Array;
  readonly heading: Float32Array;
  readonly classIdx: Uint8Array;
  readonly state: Uint8Array;
  readonly occupied: Uint8Array;
  readonly actorId: Uint32Array;
  /** Slot high-water mark. */
  readonly count: number;
  /** Camera whose `matrixWorld` and `projectionMatrix` are already up to date. */
  readonly camera: Camera;
  /** Set false to draw every live actor and measure the difference. Default true. */
  readonly cull?: boolean;
}

/** What one {@link ActorRenderer.update} did. */
export interface ActorUpdateStats {
  /** Live slots considered. */
  readonly live: number;
  /** Instances written into instance buffers. */
  readonly drawn: number;
  /** Live slots rejected by the frustum test. */
  readonly culled: number;
  /** Live slots rejected because `maxActors` was reached. */
  readonly dropped: number;
  /** Buckets with at least one instance — the actor share of the draw-call budget. */
  readonly buckets: number;
  readonly lod0: number;
  readonly lod1: number;
  readonly lod2: number;
}

interface Bucket {
  mesh: InstancedMesh<BufferGeometry, Material>;
  capacity: number;
  cursor: number;
  matrix: Float32Array;
  /** Slot that produced each written instance, for picking and hover read-back. */
  slots: Int32Array;
  /** Colour key last written into each instance slot; `0xff` means "never written". */
  colorKey: Uint8Array;
  /** Set when an instance colour actually changed, cleared when the colours are uploaded. */
  colorDirty: boolean;
  /** Preallocated upload ranges, mutated and re-pushed each frame instead of allocated. */
  readonly matrixRange: { start: number; count: number };
  readonly colorRange: { start: number; count: number };
  readonly classIndex: number;
  readonly lod: LodLevel;
}

/** Build the viewer's class table from a `Hello` class table (§3.1.4). */
export function classesFromHello(
  hello: {
    readonly classes: {
      readonly count: number;
      readonly strName: Uint32Array;
      readonly lengthM: Float32Array;
      readonly widthM: Float32Array;
      readonly heightM: Float32Array;
      readonly colorRgba: Uint32Array;
      readonly category: Uint8Array;
    };
    readonly strings: readonly string[];
  },
): ActorClassDef[] {
  const t = hello.classes;
  const out: ActorClassDef[] = [];
  for (let i = 0; i < t.count; i++) {
    const rgba = t.colorRgba[i] >>> 0;
    out.push({
      index: i,
      name: hello.strings[t.strName[i]] ?? `class_${i}`,
      lengthM: t.lengthM[i],
      widthM: t.widthM[i],
      heightM: t.heightM[i],
      // `color_rgba` is 0xRRGGBBAA; the viewer wants 0xRRGGBB.
      color: (rgba >>> 8) & 0xffffff,
      category: t.category[i],
    });
  }
  return out;
}

/** A sensible class table for worlds that arrive without a `Hello` (tests, static world viewing). */
export const DEFAULT_ACTOR_CLASSES: readonly ActorClassDef[] = [
  { index: 0, name: "car", lengthM: 4.5, widthM: 1.8, heightM: 1.5, color: 0x8fa6bd, category: 0 },
  { index: 1, name: "truck", lengthM: 10.0, widthM: 2.5, heightM: 3.4, color: 0xb08a58, category: 0 },
  { index: 2, name: "bus", lengthM: 12.0, widthM: 2.55, heightM: 3.2, color: 0xd08f3a, category: 0 },
  { index: 3, name: "moto", lengthM: 2.1, widthM: 0.8, heightM: 1.4, color: 0xa0d0c0, category: 0 },
  { index: 4, name: "bicycle", lengthM: 1.7, widthM: 0.6, heightM: 1.6, color: 0x7fc98a, category: 1 },
  { index: 5, name: "pedestrian", lengthM: 0.5, widthM: 0.5, heightM: 1.75, color: 0xe6d2a8, category: 1 },
  { index: 6, name: "emergency", lengthM: 5.4, widthM: 2.0, heightM: 2.2, color: 0xe05050, category: 0 },
  { index: 7, name: "rail", lengthM: 24.0, widthM: 3.0, heightM: 3.8, color: 0x9090a8, category: 0 },
];

/**
 * Draws every actor the stream reports, as instanced meshes grouped by class and LOD.
 *
 * The renderer owns one `Group`; add it to the scene once and never touch the children.
 */
export class ActorRenderer {
  /** The scene node holding every actor bucket. */
  readonly group = new Group();

  #classes: ActorClassDef[];
  #theme: ViewerTheme;
  #buckets: Bucket[] = [];
  #geometries: BufferGeometry[] = [];
  #materials: MeshLambertMaterial[] = [];
  #radius = new Float32Array(0);
  #halfHeight = new Float32Array(0);
  #classColor = new Float32Array(0);
  #stateColor = new Float32Array(5 * 3);
  #maxActors: number;
  #initialCapacity: number;
  #lod0 = 90;
  #lod1 = 400;
  #cullMargin: number;
  #groundOffset: number;
  #castShadows: boolean;
  #showGroundTruth: boolean;
  #benignByClass: boolean;
  #selectedActorId = -1;
  #hiddenActorId = -1;
  #color = new Color();
  #stats: ActorUpdateStats = {
    live: 0, drawn: 0, culled: 0, dropped: 0, buckets: 0, lod0: 0, lod1: 0, lod2: 0,
  };
  /** Plane coefficients `(nx, ny, nz, d)` × 6, refreshed each update. */
  #planes = new Float32Array(24);
  /** Scratch view-projection matrix; reused so {@link update} allocates nothing. */
  #viewProjection = new Matrix4();
  /** Slots drawn this frame, in bucket order; `overlays.ts` reuses them instead of re-culling. */
  readonly visibleSlots: Int32Array;
  #visibleCount = 0;

  constructor(options: ActorRendererOptions) {
    this.#theme = options.theme;
    this.#maxActors = Math.max(1, options.maxActors ?? 20_000);
    this.#initialCapacity = Math.max(8, options.initialCapacity ?? 128);
    this.#cullMargin = options.cullMarginM ?? 1;
    this.#groundOffset = options.groundOffsetM ?? 0.1;
    this.#castShadows = options.castShadows ?? false;
    this.#showGroundTruth = options.showGroundTruth ?? true;
    this.#benignByClass = options.colorBenignByClass ?? false;
    if (options.lodDistancesM) {
      this.#lod0 = options.lodDistancesM[0];
      this.#lod1 = options.lodDistancesM[1];
    }
    this.group.name = "actors";
    this.visibleSlots = new Int32Array(this.#maxActors);
    this.#classes = options.classes.length > 0 ? [...options.classes] : [...DEFAULT_ACTOR_CLASSES];
    this.#refreshStateColors();
    this.#rebuild();
  }

  /** The class table currently in use. */
  get classes(): readonly ActorClassDef[] {
    return this.#classes;
  }

  /** Stats from the last {@link update}. */
  get stats(): ActorUpdateStats {
    return this.#stats;
  }

  /** How many slots {@link visibleSlots} holds. */
  get visibleCount(): number {
    return this.#visibleCount;
  }

  /** `[LOD0→LOD1, LOD1→LOD2]` in metres. */
  get lodDistancesM(): readonly [number, number] {
    return [this.#lod0, this.#lod1];
  }

  set lodDistancesM(v: readonly [number, number]) {
    this.#lod0 = v[0];
    this.#lod1 = v[1];
  }

  /** Whether the ground-truth `ATTACKER` bit colours actors (09-ui §6: GT overlays can be locked off). */
  get showGroundTruth(): boolean {
    return this.#showGroundTruth;
  }

  set showGroundTruth(v: boolean) {
    if (v === this.#showGroundTruth) return;
    this.#showGroundTruth = v;
    this.#invalidateColors();
  }

  /** Whether benign actors are painted in their class colour instead of the benign state colour. */
  get colorBenignByClass(): boolean {
    return this.#benignByClass;
  }

  set colorBenignByClass(v: boolean) {
    if (v === this.#benignByClass) return;
    this.#benignByClass = v;
    this.#invalidateColors();
  }

  /** Actor id drawn in the selection colour, or −1. */
  get selectedActorId(): number {
    return this.#selectedActorId;
  }

  /**
   * One actor whose instance is not written this frame, or −1.
   *
   * This exists for the dashboard camera. The driver's eye sits 0.6 m ahead of the actor's origin
   * and a car is four metres long, so the camera is *inside* its own body: the view was a slab of
   * vehicle paint with the city visible above it. Culling the followed actor is what a driver's-eye
   * view means, and it is a slot skipped in a loop rather than a second pass.
   */
  get hiddenActorId(): number {
    return this.#hiddenActorId;
  }

  set hiddenActorId(id: number) {
    this.#hiddenActorId = id;
  }

  set selectedActorId(id: number) {
    this.#selectedActorId = id;
  }

  /**
   * Exactly the colours this renderer draws, as a legend.
   *
   * The Studio's `StateLegend`, the inspector chips and the `--state-*` CSS properties are all
   * meant to be this list (`apps/studio/src/lib/theme.ts`: "the DOM and the WebGL scene can never
   * drift apart"), so it is derived from the same {@link actorColorKey} decision and the same
   * colour tables the write loop uses — not re-declared.
   */
  legend(): ActorLegendEntry[] {
    const out: ActorLegendEntry[] = [];
    const s = this.#theme.actorState;
    for (const key of ACTOR_STATE_COLOR_KEYS) {
      if (key === "attacker" && !this.#showGroundTruth) continue;
      if (key === "benign" && this.#benignByClass) continue;
      out.push({ kind: "state", key, label: STATE_LABELS[key], color: s[key] });
    }
    if (this.#benignByClass) {
      for (let i = 0; i < this.#classes.length; i++) {
        const def = this.#classes[i];
        out.push({
          kind: "class",
          key: def.name,
          label: `${def.name} (benign)`,
          color: def.color !== 0 ? def.color : this.#theme.actorCategory[Math.min(def.category, 3)],
          classIndex: i,
        });
      }
    }
    return out;
  }

  /** Force a colour rewrite and upload on the next {@link update}. */
  #invalidateColors(): void {
    for (const b of this.#buckets) {
      b.colorKey.fill(0xff);
      b.colorDirty = true;
    }
  }

  /** Replace the class table (a new `Hello`). Rebuilds geometries and buckets. */
  setClasses(classes: readonly ActorClassDef[]): void {
    this.#classes = classes.length > 0 ? [...classes] : [...DEFAULT_ACTOR_CLASSES];
    this.#rebuild();
  }

  /** Swap the palette without rebuilding geometry. */
  setTheme(theme: ViewerTheme): void {
    this.#theme = theme;
    this.#refreshStateColors();
    this.#refreshClassColors();
    this.#invalidateColors();
  }

  #refreshStateColors(): void {
    const s = this.#theme.actorState;
    const keys = ACTOR_STATE_COLOR_KEYS.map((k) => s[k]);
    for (let i = 0; i < keys.length; i++) {
      this.#color.setHex(keys[i]);
      this.#stateColor[i * 3] = this.#color.r;
      this.#stateColor[i * 3 + 1] = this.#color.g;
      this.#stateColor[i * 3 + 2] = this.#color.b;
    }
  }

  #refreshClassColors(): void {
    const n = this.#classes.length;
    if (this.#classColor.length < n * 3) this.#classColor = new Float32Array(n * 3);
    for (let i = 0; i < n; i++) {
      const def = this.#classes[i];
      const hex = def.color !== 0 ? def.color : this.#theme.actorCategory[Math.min(def.category, 3)];
      this.#color.setHex(hex);
      this.#classColor[i * 3] = this.#color.r;
      this.#classColor[i * 3 + 1] = this.#color.g;
      this.#classColor[i * 3 + 2] = this.#color.b;
    }
  }

  #rebuild(): void {
    this.#disposeBuckets();
    const n = this.#classes.length;
    this.#radius = new Float32Array(n);
    this.#halfHeight = new Float32Array(n);
    this.#classColor = new Float32Array(n * 3);
    this.#refreshClassColors();
    this.#materials = [0, 1, 2].map((lod) =>
      new MeshLambertMaterial({
        vertexColors: true,
        // LOD 2 is a flat box seen from far away; flat shading keeps it from shimmering.
        flatShading: lod === 2,
        name: `actor-lod${lod}`,
      }),
    );
    this.#geometries = new Array<BufferGeometry>(n * 3);
    for (let c = 0; c < n; c++) {
      const def = this.#classes[c];
      this.#radius[c] = Math.hypot(def.lengthM, def.widthM, def.heightM) * 0.5 + this.#cullMargin;
      this.#halfHeight[c] = def.heightM * 0.5;
      for (let lod = 0; lod <= 2; lod++) {
        this.#geometries[c * 3 + lod] = buildActorGeometry(def, lod as LodLevel);
      }
      for (let lod = 0; lod <= 2; lod++) {
        this.#buckets.push(this.#makeBucket(c, lod as LodLevel, this.#initialCapacity));
      }
    }
  }

  #makeBucket(classIndex: number, lod: LodLevel, capacity: number): Bucket {
    const def = this.#classes[classIndex];
    const geometry = this.#geometries[classIndex * 3 + lod];
    const material = this.#materials[lod];
    const mesh = new InstancedMesh<BufferGeometry, Material>(geometry, material, capacity);
    mesh.name = `actors/${def.name}/lod${lod}`;
    // We do our own culling; three's bounding sphere would be stale the moment an instance moves.
    mesh.frustumCulled = false;
    mesh.matrixAutoUpdate = false;
    mesh.matrix.identity();
    mesh.count = 0;
    mesh.visible = false;
    mesh.castShadow = this.#castShadows && lod === 0;
    mesh.receiveShadow = false;
    // Touch the colour attribute once so it exists and `setColorAt` never allocates in the hot loop.
    this.#color.setRGB(1, 1, 1);
    mesh.setColorAt(0, this.#color);
    mesh.instanceMatrix.setUsage(DynamicDrawUsage);
    if (mesh.instanceColor) mesh.instanceColor.setUsage(DynamicDrawUsage);
    this.group.add(mesh);
    return {
      mesh,
      capacity,
      cursor: 0,
      matrix: mesh.instanceMatrix.array as Float32Array,
      slots: new Int32Array(capacity),
      colorKey: new Uint8Array(capacity).fill(0xff),
      colorDirty: true,
      matrixRange: { start: 0, count: 0 },
      colorRange: { start: 0, count: 0 },
      classIndex,
      lod,
    };
  }

  /**
   * Double a bucket's capacity. `InstancedMesh` cannot be resized, so a fresh one replaces it — and
   * the instances **already written this frame** are copied across, because growth happens in the
   * middle of {@link update}'s write loop and leaving them behind would flash that bucket's earlier
   * instances at the origin for one frame.
   *
   * Growth is amortised: a stream that settles at a stable actor count stops growing after the first
   * few frames, which the budget test asserts.
   */
  #grow(b: Bucket): void {
    const next = Math.min(this.#maxActors, b.capacity * 2);
    if (next <= b.capacity) return;
    const oldMesh = b.mesh;
    const oldMatrix = b.matrix;
    const oldColor = (oldMesh.instanceColor?.array ?? null) as Float32Array | null;
    const oldSlots = b.slots;
    const oldKeys = b.colorKey;
    const written = Math.min(b.cursor, b.capacity);

    this.group.remove(oldMesh);
    const fresh = this.#makeBucket(b.classIndex, b.lod, next);
    if (written > 0) {
      fresh.matrix.set(oldMatrix.subarray(0, written * 16), 0);
      const freshColor = (fresh.mesh.instanceColor?.array ?? null) as Float32Array | null;
      if (freshColor && oldColor) freshColor.set(oldColor.subarray(0, written * 3), 0);
      fresh.slots.set(oldSlots.subarray(0, written), 0);
      fresh.colorKey.set(oldKeys.subarray(0, written), 0);
    }
    oldMesh.dispose();

    b.mesh = fresh.mesh;
    b.capacity = next;
    b.matrix = fresh.matrix;
    b.slots = fresh.slots;
    b.colorKey = fresh.colorKey;
    // A brand-new attribute has never been uploaded, whatever the colours say.
    b.colorDirty = true;
    // `#makeBucket` only added the new mesh to the group; the bucket list itself is unchanged.
  }

  /**
   * Write one frame's worth of instances. Returns the same object every call — copy what you need.
   */
  update(ctx: ActorUpdateContext): ActorUpdateStats {
    const cam = ctx.camera;
    const cull = ctx.cull !== false;
    const buckets = this.#buckets;
    for (let i = 0; i < buckets.length; i++) buckets[i].cursor = 0;
    this.#visibleCount = 0;

    // Frustum planes, extracted by hand from the view-projection matrix so the test below can run on
    // plain numbers. Same algebra as `Frustum.setFromProjectionMatrix`.
    if (cull) this.#extractPlanes(cam);

    const camX = cam.matrixWorld.elements[12];
    const camY = cam.matrixWorld.elements[13];
    const camZ = cam.matrixWorld.elements[14];
    const lod0Sq = this.#lod0 * this.#lod0;
    const lod1Sq = this.#lod1 * this.#lod1;

    const pos = ctx.position;
    const head = ctx.heading;
    const cls = ctx.classIdx;
    const st = ctx.state;
    const occ = ctx.occupied;
    const ids = ctx.actorId;
    const nClasses = this.#classes.length;
    const planes = this.#planes;
    const lift = this.#groundOffset;
    const stateColor = this.#stateColor;
    const classColor = this.#classColor;
    const selected = this.#selectedActorId;
    const selectedU = selected >>> 0;
    const hidden = this.#hiddenActorId;
    const hiddenU = hidden >>> 0;
    const gt = this.#showGroundTruth;
    const benignByClass = this.#benignByClass;
    const col = this.#color;

    let live = 0;
    let drawn = 0;
    let culled = 0;
    let dropped = 0;
    let lod0n = 0;
    let lod1n = 0;
    let lod2n = 0;

    for (let s = 0; s < ctx.count; s++) {
      if (occ[s] === 0) continue;
      live++;
      // Still live and still pickable; just not drawn. See `hiddenActorId`.
      if (hidden >= 0 && ids[s] === hiddenU) continue;
      let c = cls[s];
      if (c >= nClasses) c = 0;
      const p = s * 3;
      const x = pos[p];
      const y = pos[p + 1];
      const z = pos[p + 2] + lift;
      const dx = x - camX;
      const dy = y - camY;
      const dz = z - camZ;
      const dist2 = dx * dx + dy * dy + dz * dz;

      const r = this.#radius[c];
      if (cull) {
        const cz = z + this.#halfHeight[c];
        let outside = false;
        for (let k = 0; k < 6; k++) {
          const o = k * 4;
          if (planes[o] * x + planes[o + 1] * y + planes[o + 2] * cz + planes[o + 3] < -r) {
            outside = true;
            break;
          }
        }
        if (outside) {
          culled++;
          continue;
        }
      }

      if (drawn >= this.#maxActors) {
        dropped++;
        continue;
      }

      const lod: LodLevel = dist2 < lod0Sq ? 0 : dist2 < lod1Sq ? 1 : 2;
      if (lod === 0) lod0n++;
      else if (lod === 1) lod1n++;
      else lod2n++;

      const b = buckets[c * 3 + lod];
      if (b.cursor >= b.capacity) {
        if (b.capacity >= this.#maxActors) {
          dropped++;
          continue;
        }
        this.#grow(b);
      }

      const i = b.cursor++;
      const m = b.matrix;
      const o = i * 16;
      const h = head[s];
      const cosH = Math.cos(h);
      const sinH = Math.sin(h);
      // Column-major, identical to what `Matrix4.toArray` would write for
      // makeRotationZ(h) then setPosition(x, y, z).
      m[o] = cosH; m[o + 1] = sinH; m[o + 2] = 0; m[o + 3] = 0;
      m[o + 4] = -sinH; m[o + 5] = cosH; m[o + 6] = 0; m[o + 7] = 0;
      m[o + 8] = 0; m[o + 9] = 0; m[o + 10] = 1; m[o + 11] = 0;
      m[o + 12] = x; m[o + 13] = y; m[o + 14] = z; m[o + 15] = 1;

      // Colour by state (§3.3.4 bits 0–2), selection first. Same order as `actorColorKey`, which
      // `legend()` reports from — keep the two in step. Written, and uploaded, only on a change:
      // the matrices move every frame but a colour is a function of selection and state bits.
      const state = st[s];
      let ci = actorStateColorIndex(state, selected >= 0 && ids[s] === selectedU, gt);
      if (ci === 0 && benignByClass) ci = CLASS_COLOR_KEY;
      if (b.colorKey[i] !== ci) {
        b.colorKey[i] = ci;
        b.colorDirty = true;
        if (ci === CLASS_COLOR_KEY) {
          col.setRGB(classColor[c * 3], classColor[c * 3 + 1], classColor[c * 3 + 2]);
        } else {
          col.setRGB(stateColor[ci * 3], stateColor[ci * 3 + 1], stateColor[ci * 3 + 2]);
        }
        b.mesh.setColorAt(i, col);
      }

      b.slots[i] = s;
      this.visibleSlots[this.#visibleCount++] = s;
      drawn++;
    }

    let usedBuckets = 0;
    for (let k = 0; k < buckets.length; k++) {
      const b = buckets[k];
      const n = b.cursor;
      b.mesh.count = n;
      b.mesh.visible = n > 0;
      if (n > 0) {
        usedBuckets++;
        const im = b.mesh.instanceMatrix;
        im.clearUpdateRanges();
        b.matrixRange.start = 0;
        b.matrixRange.count = n * 16;
        // Not `addUpdateRange`, which allocates a fresh `{start, count}` on every call
        // (three 0.186.0, BufferAttribute.js:181). three clears the array after each upload, so
        // the same object is re-pushed rather than kept.
        im.updateRanges.push(b.matrixRange);
        im.needsUpdate = true;
        const ic = b.mesh.instanceColor;
        if (ic && b.colorDirty) {
          ic.clearUpdateRanges();
          b.colorRange.start = 0;
          b.colorRange.count = n * 3;
          ic.updateRanges.push(b.colorRange);
          ic.needsUpdate = true;
          b.colorDirty = false;
        }
      }
    }

    this.#stats = {
      live, drawn, culled, dropped, buckets: usedBuckets, lod0: lod0n, lod1: lod1n, lod2: lod2n,
    };
    return this.#stats;
  }

  /** The bucket a `(class, lod)` pair draws into, for tests and the picker. */
  bucketAt(classIndex: number, lod: LodLevel): {
    readonly mesh: InstancedMesh<BufferGeometry, Material>;
    readonly count: number;
    readonly capacity: number;
  } | null {
    const b = this.#buckets[classIndex * 3 + lod];
    return b ? { mesh: b.mesh, count: b.cursor, capacity: b.capacity } : null;
  }

  /** Total instance capacity currently allocated across all buckets. */
  get allocatedCapacity(): number {
    let n = 0;
    for (const b of this.#buckets) n += b.capacity;
    return n;
  }

  #extractPlanes(cam: Camera): void {
    const m = this.#viewProjection.multiplyMatrices(cam.projectionMatrix, cam.matrixWorldInverse).elements;
    const p = this.#planes;
    const set = (k: number, a: number, b: number, c: number, d: number): void => {
      const inv = 1 / (Math.hypot(a, b, c) || 1);
      p[k * 4] = a * inv;
      p[k * 4 + 1] = b * inv;
      p[k * 4 + 2] = c * inv;
      p[k * 4 + 3] = d * inv;
    };
    set(0, m[3] - m[0], m[7] - m[4], m[11] - m[8], m[15] - m[12]);
    set(1, m[3] + m[0], m[7] + m[4], m[11] + m[8], m[15] + m[12]);
    set(2, m[3] + m[1], m[7] + m[5], m[11] + m[9], m[15] + m[13]);
    set(3, m[3] - m[1], m[7] - m[5], m[11] - m[9], m[15] - m[13]);
    set(4, m[3] - m[2], m[7] - m[6], m[11] - m[10], m[15] - m[14]);
    set(5, m[3] + m[2], m[7] + m[6], m[11] + m[10], m[15] + m[14]);
  }

  #disposeBuckets(): void {
    for (const b of this.#buckets) {
      this.group.remove(b.mesh);
      b.mesh.dispose();
    }
    this.#buckets = [];
    for (const g of this.#geometries) g.dispose();
    this.#geometries = [];
    for (const m of this.#materials) m.dispose();
    this.#materials = [];
  }

  /** Release every GPU resource this renderer owns. */
  dispose(): void {
    this.#disposeBuckets();
    this.group.removeFromParent();
  }
}
