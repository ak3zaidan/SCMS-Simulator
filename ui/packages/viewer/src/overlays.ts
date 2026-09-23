/**
 * Overlays: independently toggleable, each cheap, each drawing in one call.
 *
 * The catalogue is `OVERLAY_NAMES` from `@vwp/protocol` (§6.7), so `overlay.set` maps one-to-one onto
 * {@link OverlayManager.set} and the Studio needs no translation table. 09-ui §4 sets the technique
 * for each one and §6 adds the rule that ground-truth overlays are labelled `GT` and can be locked
 * off for blind evaluation — {@link OverlayManager.lockGroundTruth} is that lock, and while it is on
 * a GT overlay cannot be enabled at all.
 *
 * - **tx_pulses** — instanced expanding rings, one shared `InstancedBufferGeometry`, expansion and
 *   fade entirely in the vertex shader from a `uTime` uniform. The CPU only appends `(origin, start,
 *   radius, colour)` into a **ring buffer**: emitting is O(1) whether the buffer is full or not, and
 *   retiring an expired pulse is O(1) too, because pulses expire in the order they were added. An
 *   earlier version memmoved the whole live range on every emit once the buffer filled, which is
 *   O(capacity) per emit and measured 19x slower at the default capacity (finding Q11).
 * - **links** — one `LineSegments` over a preallocated position/colour buffer with `setDrawRange`.
 * - **cbr_heatmap** — a `DataTexture` on a ground quad, sampled through a ramp in the fragment
 *   shader. No per-pixel CPU work; the engine's per-cell channel accounting goes straight in.
 * - **coverage / rsu_range** — instanced soft rings at RSU antenna positions.
 * - **attackers_gt / reported / revoked / detections** — instanced billboard markers above actors,
 *   each state with its own *shape* as well as its own colour (09-ui §10 asks for shape redundancy
 *   alongside the colour-blind-safe palette).
 * - **lane_markings / buildings / signal_state** — visibility flips on groups `world-render.ts`
 *   already owns; they cost nothing to toggle.
 */

import {
  AdditiveBlending,
  BufferAttribute,
  BufferGeometry,
  Color,
  DataTexture,
  DoubleSide,
  DynamicDrawUsage,
  Group,
  InstancedBufferAttribute,
  InstancedBufferGeometry,
  InstancedMesh,
  LineBasicMaterial,
  LineSegments,
  LinearFilter,
  Matrix4,
  Mesh,
  MeshBasicMaterial,
  PlaneGeometry,
  RGBAFormat,
  ShaderMaterial,
  UnsignedByteType,
  Vector3,
  type Camera,
} from "three";
import { ActorState, OVERLAY_NAMES, type OverlayName } from "@vwp/protocol";
import { ACTOR_STATE_COLOR_KEYS, actorStateColorIndex } from "./actors.js";
import { VEHICLE_MARK_ANGULAR_RADIUS } from "./types.js";
import {
  MeshBuilder, addBox, discGeometry, markerGeometry, ringGeometry, withUnitVertexColors,
} from "./geometry.js";
import type { ViewerTheme } from "./theme.js";
import type { WorldRenderer } from "./world-render.js";

/** A mutable upload range, allocated once and re-pushed every frame. */
interface UploadRange {
  start: number;
  count: number;
}

/** Allocate `n` upload ranges up front. */
function makeRanges(n: number): UploadRange[] {
  const out: UploadRange[] = [];
  for (let i = 0; i < n; i++) out.push({ start: 0, count: 0 });
  return out;
}

/**
 * Mark the used prefix of a dynamic attribute for upload without touching the rest of the buffer.
 *
 * `range` is the caller's preallocated object: `BufferAttribute.addUpdateRange` pushes a fresh
 * `{start, count}` literal on every call (three 0.186.0, BufferAttribute.js:181), which is a real
 * per-frame allocation in the render loop (finding Q17). three clears `updateRanges` after each
 * upload, so the same object is pushed again rather than kept in place.
 */
function publishRange(
  attr: BufferAttribute | InstancedBufferAttribute,
  elements: number,
  range: UploadRange,
): void {
  attr.clearUpdateRanges();
  range.start = 0;
  range.count = elements;
  attr.updateRanges.push(range);
  attr.needsUpdate = true;
}

/**
 * Publish a ring buffer's live span, which is one range or two when it wraps.
 *
 * `stride` is components per element.
 */
function publishRing(
  attr: BufferAttribute | InstancedBufferAttribute,
  head: number,
  count: number,
  capacity: number,
  stride: number,
  a: UploadRange,
  b: UploadRange,
): void {
  attr.clearUpdateRanges();
  if (count <= 0) return;
  const tail = head + count;
  if (tail <= capacity) {
    a.start = head * stride;
    a.count = count * stride;
    attr.updateRanges.push(a);
  } else {
    a.start = head * stride;
    a.count = (capacity - head) * stride;
    b.start = 0;
    b.count = (tail - capacity) * stride;
    attr.updateRanges.push(a);
    attr.updateRanges.push(b);
  }
  attr.needsUpdate = true;
}

/** Overlays whose data is ground truth (§5.1: the name ends `_gt`). */
export const GROUND_TRUTH_OVERLAYS: ReadonlySet<OverlayName> = new Set<OverlayName>(
  OVERLAY_NAMES.filter((n) => n.endsWith("_gt")),
);

/** True when an overlay's data is ground truth and must carry the `GT` tag. */
export function isGroundTruthOverlay(name: OverlayName): boolean {
  return GROUND_TRUTH_OVERLAYS.has(name);
}

/** A human label for an overlay, with the `GT` tag appended where 09-ui §6 requires it. */
export function overlayLabel(name: OverlayName): string {
  const base = name.replace(/_gt$/, "").replace(/_/g, " ");
  const pretty = base.charAt(0).toUpperCase() + base.slice(1);
  return isGroundTruthOverlay(name) ? `${pretty} (GT)` : pretty;
}

/** One row of the overlay catalogue, shaped for §6.7 `overlay.set {list: true}`. */
export interface OverlayEntry {
  readonly name: OverlayName;
  readonly groundTruth: boolean;
  /** False when this build has no implementation for it (the Studio greys it out). */
  readonly available: boolean;
  readonly enabled: boolean;
  readonly opacity: number;
  readonly label: string;
}

/** Per-frame inputs for {@link OverlayManager.update}. */
export interface OverlayUpdateContext {
  readonly camera: Camera;
  /** Viewer clock in seconds; the pulse shader's `uTime`. */
  readonly timeSeconds: number;
  /** Interpolated actor poses, indexed by slot. */
  readonly position: Float32Array;
  readonly state: Uint8Array;
  readonly occupied: Uint8Array;
  readonly classIdx: Uint8Array;
  readonly count: number;
  /** Slots the actor renderer actually drew, so markers reuse its culling. */
  readonly visibleSlots: Int32Array;
  readonly visibleCount: number;
  /** Height above the actor origin to float a marker, metres. */
  readonly markerHeightM?: number;
  /** `actor_id` per slot, for matching the selection. Omitted, nothing counts as selected. */
  readonly actorId?: Uint32Array;
  /** The selected actor's id, or null. */
  readonly selectedActorId?: number | null;
  /** Live actors in the stream. Omitted, {@link count} stands in. */
  readonly liveCount?: number;
  /**
   * Whether the ground-truth `ATTACKER` bit may colour a vehicle mark. Omitted, it may.
   *
   * It is passed per frame rather than stored because the authority is
   * `ActorRenderer.showGroundTruth` — 09-ui §6 lets a blind evaluation lock ground truth off, and
   * a mark that kept its own copy of that switch could paint an attacker vermillion on a map whose
   * legend has no attacker row.
   */
  readonly showGroundTruth?: boolean;
}

// ---------------------------------------------------------------------------------------------
// Transmission pulses
// ---------------------------------------------------------------------------------------------

const PULSE_VERTEX = /* glsl */ `
attribute vec3 aOrigin;
attribute float aStart;
attribute float aRadius;
attribute vec3 aColor;
uniform float uTime;
uniform float uLife;
varying float vAge;
varying vec3 vColor;
varying vec2 vUv;
void main() {
  // A ring buffer's live span does not have to start at instance 0, and there is no instance
  // offset in WebGL 1, so expired slots are drawn and must cost nothing: collapse them to their
  // origin (zero-area triangles, no fragments) rather than rasterising a full-size invisible ring.
  float t = (uTime - aStart) / uLife;
  float alive = step(0.0, t) * (1.0 - step(1.0, t));
  float age = clamp(t, 0.0, 1.0);
  vAge = age;
  vColor = aColor;
  vUv = uv;
  vec3 p = aOrigin + vec3(position.xy * (aRadius * age * alive), 0.0);
  gl_Position = projectionMatrix * modelViewMatrix * vec4(p, 1.0);
}
`;

const PULSE_FRAGMENT = /* glsl */ `
uniform float uOpacity;
varying float vAge;
varying vec3 vColor;
varying vec2 vUv;
void main() {
  float edge = smoothstep(0.0, 0.35, vUv.x) * (1.0 - smoothstep(0.65, 1.0, vUv.x));
  float fade = 1.0 - vAge;
  float a = edge * fade * fade * uOpacity;
  if (a < 0.004) discard;
  gl_FragColor = vec4(vColor, a);
}
`;

/** Expanding rings marking transmissions, animated entirely on the GPU. */
export class TxPulseOverlay {
  readonly object: Mesh<InstancedBufferGeometry, ShaderMaterial>;
  readonly capacity: number;
  /** Seconds a pulse takes to expand from nothing to its full radius and fade out. */
  lifeSeconds: number;

  #origin: Float32Array;
  #start: Float32Array;
  #radius: Float32Array;
  #color: Float32Array;
  /** Index of the oldest live pulse. */
  #head = 0;
  #count = 0;
  #ranges = makeRanges(8);
  #geometry: InstancedBufferGeometry;
  #aOrigin: InstancedBufferAttribute;
  #aStart: InstancedBufferAttribute;
  #aRadius: InstancedBufferAttribute;
  #aColor: InstancedBufferAttribute;
  #base: BufferGeometry;
  #scratch = new Color();

  constructor(theme: ViewerTheme, capacity = 4096, lifeSeconds = 0.9) {
    this.capacity = Math.max(16, capacity);
    this.lifeSeconds = lifeSeconds;
    const cap = this.capacity;
    this.#origin = new Float32Array(cap * 3);
    // Every slot starts expired, so a slot that has never been written draws nothing whatever the
    // clock reads (including a clock that has been reset behind an already-written start time).
    this.#start = new Float32Array(cap).fill(-1e9);
    this.#radius = new Float32Array(cap);
    this.#color = new Float32Array(cap * 3);

    this.#base = ringGeometry(0.72, 48);
    const g = new InstancedBufferGeometry();
    const pos = this.#base.getAttribute("position");
    const uv = this.#base.getAttribute("uv");
    g.setAttribute("position", pos);
    if (uv) g.setAttribute("uv", uv);
    const index = this.#base.getIndex();
    if (index) g.setIndex(index);
    this.#aOrigin = new InstancedBufferAttribute(this.#origin, 3).setUsage(DynamicDrawUsage);
    this.#aStart = new InstancedBufferAttribute(this.#start, 1).setUsage(DynamicDrawUsage);
    this.#aRadius = new InstancedBufferAttribute(this.#radius, 1).setUsage(DynamicDrawUsage);
    this.#aColor = new InstancedBufferAttribute(this.#color, 3).setUsage(DynamicDrawUsage);
    g.setAttribute("aOrigin", this.#aOrigin);
    g.setAttribute("aStart", this.#aStart);
    g.setAttribute("aRadius", this.#aRadius);
    g.setAttribute("aColor", this.#aColor);
    g.instanceCount = 0;
    this.#geometry = g;

    const material = new ShaderMaterial({
      name: "tx-pulses",
      uniforms: {
        uTime: { value: 0 },
        uLife: { value: lifeSeconds },
        uOpacity: { value: 0.75 },
      },
      vertexShader: PULSE_VERTEX,
      fragmentShader: PULSE_FRAGMENT,
      transparent: true,
      depthWrite: false,
      blending: AdditiveBlending,
      side: DoubleSide,
      toneMapped: false,
    });
    this.object = new Mesh(this.#geometry, material);
    this.object.name = "overlay/tx-pulses";
    this.object.frustumCulled = false;
    this.object.renderOrder = 10;
    this.setTheme(theme);
  }

  /** Live pulses. */
  get count(): number {
    return this.#count;
  }

  /** Index of the oldest live pulse in the ring; exposed for tests and diagnostics. */
  get head(): number {
    return this.#head;
  }

  setTheme(theme: ViewerTheme): void {
    this.#scratch.setHex(theme.pulse);
  }

  set opacity(v: number) {
    this.object.material.uniforms.uOpacity.value = v;
  }

  get opacity(): number {
    return this.object.material.uniforms.uOpacity.value as number;
  }

  /**
   * Emit a pulse. `colorHex` defaults to the theme's pulse colour. When the buffer is full the
   * oldest pulse is dropped, which is the right trade: a burst of transmissions should not stall.
   */
  emit(x: number, y: number, z: number, radiusM: number, timeSeconds: number, colorHex?: number): void {
    let i: number;
    if (this.#count >= this.capacity) {
      // Full: overwrite the oldest and advance the head. O(1) — no memmove of the live range.
      i = this.#head;
      this.#head = (this.#head + 1) % this.capacity;
    } else {
      i = (this.#head + this.#count) % this.capacity;
      this.#count++;
    }
    this.#origin[i * 3] = x;
    this.#origin[i * 3 + 1] = y;
    this.#origin[i * 3 + 2] = z;
    this.#start[i] = timeSeconds;
    this.#radius[i] = radiusM;
    if (colorHex !== undefined) this.#scratch.setHex(colorHex);
    this.#color[i * 3] = this.#scratch.r;
    this.#color[i * 3 + 1] = this.#scratch.g;
    this.#color[i * 3 + 2] = this.#scratch.b;
  }

  /** Drop every pulse. */
  clear(): void {
    this.#count = 0;
    this.#head = 0;
    this.#start.fill(-1e9);
    this.#geometry.instanceCount = 0;
  }

  /**
   * Retire expired pulses and publish the buffers.
   *
   * Retirement walks the head forward over expired entries, so it is O(retired) rather than
   * O(live), and it must run even while the overlay is hidden — otherwise a hidden overlay that is
   * still being emitted into stays permanently full (finding Q11).
   */
  update(timeSeconds: number): void {
    const cutoff = timeSeconds - this.lifeSeconds;
    while (this.#count > 0 && this.#start[this.#head] <= cutoff) {
      this.#head = (this.#head + 1) % this.capacity;
      this.#count--;
    }
    if (this.#count === 0) this.#head = 0;
    const n = this.#count;
    // No instance offset exists in WebGL, so the draw has to cover the live span from index 0; the
    // slots before the head are expired and the vertex shader collapses them to nothing.
    this.#geometry.instanceCount = n === 0 ? 0 : Math.min(this.capacity, this.#head + n);
    const u = this.object.material.uniforms;
    u.uTime.value = timeSeconds;
    u.uLife.value = this.lifeSeconds;
    if (n === 0) return;
    const r = this.#ranges;
    publishRing(this.#aOrigin, this.#head, n, this.capacity, 3, r[0], r[1]);
    publishRing(this.#aStart, this.#head, n, this.capacity, 1, r[2], r[3]);
    publishRing(this.#aRadius, this.#head, n, this.capacity, 1, r[4], r[5]);
    publishRing(this.#aColor, this.#head, n, this.capacity, 3, r[6], r[7]);
  }

  dispose(): void {
    this.#geometry.dispose();
    this.#base.dispose();
    this.object.material.dispose();
    this.object.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// Communication links
// ---------------------------------------------------------------------------------------------

/** Line segments between communicating nodes, over one preallocated buffer. */
export class LinkOverlay {
  readonly object: LineSegments<BufferGeometry, LineBasicMaterial>;
  readonly capacity: number;

  #positions: Float32Array;
  #colors: Float32Array;
  #posAttr: BufferAttribute;
  #colorAttr: BufferAttribute;
  #count = 0;
  #ranges = makeRanges(2);
  #color = new Color();
  #gtColor = new Color();

  constructor(theme: ViewerTheme, capacity = 8192) {
    this.capacity = Math.max(16, capacity);
    this.#positions = new Float32Array(this.capacity * 6);
    this.#colors = new Float32Array(this.capacity * 6);
    const g = new BufferGeometry();
    this.#posAttr = new BufferAttribute(this.#positions, 3).setUsage(DynamicDrawUsage) as BufferAttribute;
    this.#colorAttr = new BufferAttribute(this.#colors, 3).setUsage(DynamicDrawUsage) as BufferAttribute;
    g.setAttribute("position", this.#posAttr);
    g.setAttribute("color", this.#colorAttr);
    g.setDrawRange(0, 0);
    const material = new LineBasicMaterial({
      name: "links", vertexColors: true, transparent: true, opacity: 0.55, depthWrite: false, toneMapped: false,
    });
    this.object = new LineSegments(g, material);
    this.object.name = "overlay/links";
    this.object.frustumCulled = false;
    this.object.renderOrder = 9;
    this.setTheme(theme);
  }

  setTheme(theme: ViewerTheme): void {
    this.#color.setHex(theme.link);
    this.#gtColor.setHex(theme.linkGt);
  }

  set opacity(v: number) {
    this.object.material.opacity = v;
  }

  get opacity(): number {
    return this.object.material.opacity;
  }

  /** Segments currently drawn. */
  get count(): number {
    return this.#count;
  }

  /** Start a new set of links. */
  begin(): void {
    this.#count = 0;
  }

  /**
   * Add one link. `groundTruth` picks the GT-tagged colour, so a link whose transmitter identity
   * came from the GT channel is visually distinct from one derived from received messages.
   */
  add(
    x1: number, y1: number, z1: number,
    x2: number, y2: number, z2: number,
    groundTruth = false,
    strength = 1,
  ): boolean {
    if (this.#count >= this.capacity) return false;
    const i = this.#count++;
    const p = i * 6;
    const P = this.#positions;
    P[p] = x1; P[p + 1] = y1; P[p + 2] = z1;
    P[p + 3] = x2; P[p + 4] = y2; P[p + 5] = z2;
    const c = groundTruth ? this.#gtColor : this.#color;
    const C = this.#colors;
    const s = Math.max(0, Math.min(1, strength));
    C[p] = c.r * s; C[p + 1] = c.g * s; C[p + 2] = c.b * s;
    C[p + 3] = c.r * s; C[p + 4] = c.g * s; C[p + 5] = c.b * s;
    return true;
  }

  /** Publish the batch. */
  end(): void {
    const verts = this.#count * 2;
    this.object.geometry.setDrawRange(0, verts);
    if (verts === 0) return;
    publishRange(this.#posAttr, verts * 3, this.#ranges[0]);
    publishRange(this.#colorAttr, verts * 3, this.#ranges[1]);
  }

  dispose(): void {
    this.object.geometry.dispose();
    this.object.material.dispose();
    this.object.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// Channel-busy-ratio heatmap
// ---------------------------------------------------------------------------------------------

const HEATMAP_FRAGMENT = /* glsl */ `
uniform sampler2D uData;
uniform float uOpacity;
varying vec2 vUv;
vec3 ramp(float t) {
  // Perceptually ordered blue → cyan → yellow → red; dark end stays dark so an empty map is quiet.
  vec3 a = vec3(0.09, 0.18, 0.42);
  vec3 b = vec3(0.10, 0.62, 0.68);
  vec3 c = vec3(0.92, 0.82, 0.24);
  vec3 d = vec3(0.86, 0.25, 0.12);
  return t < 0.4 ? mix(a, b, t / 0.4) : t < 0.75 ? mix(b, c, (t - 0.4) / 0.35) : mix(c, d, (t - 0.75) / 0.25);
}
void main() {
  float v = texture2D(uData, vUv).r;
  if (v <= 0.002) discard;
  gl_FragColor = vec4(ramp(clamp(v, 0.0, 1.0)), uOpacity * clamp(v * 1.6, 0.0, 1.0));
}
`;

const HEATMAP_VERTEX = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
}
`;

/** A per-cell scalar field (channel busy ratio, density) drawn as a texture on a ground quad. */
export class HeatmapOverlay {
  readonly object: Mesh<PlaneGeometry, ShaderMaterial>;
  #texture: DataTexture;
  #data: Uint8Array;
  #width: number;
  #height: number;

  constructor(width = 128, height = 128) {
    this.#width = Math.max(2, width);
    this.#height = Math.max(2, height);
    this.#data = new Uint8Array(this.#width * this.#height * 4);
    this.#texture = new DataTexture(this.#data, this.#width, this.#height, RGBAFormat, UnsignedByteType);
    this.#texture.minFilter = LinearFilter;
    this.#texture.magFilter = LinearFilter;
    this.#texture.needsUpdate = true;
    const material = new ShaderMaterial({
      name: "cbr-heatmap",
      uniforms: { uData: { value: this.#texture }, uOpacity: { value: 0.55 } },
      vertexShader: HEATMAP_VERTEX,
      fragmentShader: HEATMAP_FRAGMENT,
      transparent: true,
      depthWrite: false,
      toneMapped: false,
    });
    this.object = new Mesh(new PlaneGeometry(1, 1), material);
    this.object.name = "overlay/heatmap";
    this.object.renderOrder = 2;
    this.object.matrixAutoUpdate = false;
  }

  get width(): number {
    return this.#width;
  }

  get height(): number {
    return this.#height;
  }

  set opacity(v: number) {
    this.object.material.uniforms.uOpacity.value = v;
  }

  get opacity(): number {
    return this.object.material.uniforms.uOpacity.value as number;
  }

  /** Place the quad over a world bounding box at height `z`. */
  fitTo(minX: number, minY: number, maxX: number, maxY: number, z: number): void {
    this.object.geometry.dispose();
    this.object.geometry = new PlaneGeometry(Math.max(1, maxX - minX), Math.max(1, maxY - minY));
    this.object.position.set((minX + maxX) / 2, (minY + maxY) / 2, z);
    this.object.updateMatrix();
  }

  /** Change the grid resolution; the previous contents are dropped. */
  resize(width: number, height: number): void {
    if (width === this.#width && height === this.#height) return;
    this.#width = Math.max(2, width);
    this.#height = Math.max(2, height);
    this.#data = new Uint8Array(this.#width * this.#height * 4);
    this.#texture.dispose();
    this.#texture = new DataTexture(this.#data, this.#width, this.#height, RGBAFormat, UnsignedByteType);
    this.#texture.minFilter = LinearFilter;
    this.#texture.magFilter = LinearFilter;
    this.#texture.needsUpdate = true;
    this.object.material.uniforms.uData.value = this.#texture;
  }

  /** Set one cell from a value in `[0, 1]`. */
  setCell(ix: number, iy: number, value01: number): void {
    if (ix < 0 || iy < 0 || ix >= this.#width || iy >= this.#height) return;
    const v = Math.round(Math.max(0, Math.min(1, value01)) * 255);
    const o = (iy * this.#width + ix) * 4;
    this.#data[o] = v;
    this.#data[o + 3] = 255;
  }

  /** Bulk-load the field, row-major, `width · height` values in `[0, 1]`. */
  setField(values: ArrayLike<number>): void {
    const n = Math.min(values.length, this.#width * this.#height);
    for (let i = 0; i < n; i++) {
      const v = Math.round(Math.max(0, Math.min(1, values[i])) * 255);
      this.#data[i * 4] = v;
      this.#data[i * 4 + 3] = 255;
    }
  }

  /** Zero the field. */
  clear(): void {
    this.#data.fill(0);
  }

  /** Upload whatever has been written since the last commit. */
  commit(): void {
    this.#texture.needsUpdate = true;
  }

  dispose(): void {
    this.#texture.dispose();
    this.object.geometry.dispose();
    this.object.material.dispose();
    this.object.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// RSU coverage
// ---------------------------------------------------------------------------------------------

const COVERAGE_VERTEX = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  #ifdef USE_INSTANCING
  gl_Position = projectionMatrix * modelViewMatrix * instanceMatrix * vec4(position, 1.0);
  #else
  gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0);
  #endif
}
`;

const COVERAGE_FRAGMENT = /* glsl */ `
uniform vec3 uColor;
uniform float uOpacity;
varying vec2 vUv;
void main() {
  float edge = smoothstep(0.0, 0.25, vUv.x);
  gl_FragColor = vec4(uColor, uOpacity * (0.25 + 0.75 * edge));
}
`;

/** Soft rings at RSU antennas showing nominal coverage radius. */
export class CoverageOverlay {
  readonly object: InstancedMesh<BufferGeometry, ShaderMaterial>;
  readonly capacity: number;
  #geometry: BufferGeometry;
  #matrix = new Matrix4();
  #count = 0;

  constructor(theme: ViewerTheme, capacity = 512) {
    this.capacity = Math.max(1, capacity);
    this.#geometry = ringGeometry(0.88, 64);
    const material = new ShaderMaterial({
      name: "coverage",
      uniforms: { uColor: { value: new Color(theme.coverage) }, uOpacity: { value: 0.45 } },
      vertexShader: COVERAGE_VERTEX,
      fragmentShader: COVERAGE_FRAGMENT,
      transparent: true,
      depthWrite: false,
      side: DoubleSide,
      toneMapped: false,
    });
    this.object = new InstancedMesh<BufferGeometry, ShaderMaterial>(this.#geometry, material, this.capacity);
    this.object.name = "overlay/coverage";
    this.object.frustumCulled = false;
    this.object.renderOrder = 3;
    this.object.count = 0;
  }

  setTheme(theme: ViewerTheme): void {
    (this.object.material.uniforms.uColor.value as Color).setHex(theme.coverage);
  }

  set opacity(v: number) {
    this.object.material.uniforms.uOpacity.value = v;
  }

  get opacity(): number {
    return this.object.material.uniforms.uOpacity.value as number;
  }

  /** Circles currently drawn. */
  get count(): number {
    return this.#count;
  }

  /** Place one circle per site at `radiusM`. `z` is the ring's height above the terrain. */
  setSites(positions: Float32Array, siteCount: number, radiusM: number, z = 0.2): void {
    const n = Math.min(siteCount, this.capacity);
    for (let i = 0; i < n; i++) {
      this.#matrix.identity();
      const e = this.#matrix.elements;
      e[0] = radiusM;
      e[5] = radiusM;
      e[12] = positions[i * 3];
      e[13] = positions[i * 3 + 1];
      e[14] = z;
      this.object.setMatrixAt(i, this.#matrix);
    }
    this.#count = n;
    this.object.count = n;
    this.object.instanceMatrix.needsUpdate = true;
  }

  dispose(): void {
    this.object.dispose();
    this.#geometry.dispose();
    this.object.material.dispose();
    this.object.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// Actor state markers
// ---------------------------------------------------------------------------------------------

/** The four marker channels, each with its own shape as well as its own colour (09-ui §10). */
export type MarkerChannel = "attacker" | "reported" | "revoked" | "detection";

const MARKER_CHANNELS: readonly MarkerChannel[] = ["attacker", "reported", "revoked", "detection"];

/**
 * Channel → index, so the per-actor loop in {@link StateMarkerOverlay.update} does a property
 * lookup rather than `MARKER_CHANNELS.indexOf(ch)`: a linear string search per visible actor per
 * frame, 300,000 a second at the 5,000-actor budget (finding Q19).
 */
const CHANNEL_INDEX: Readonly<Record<MarkerChannel, number>> = {
  attacker: 0, reported: 1, revoked: 2, detection: 3,
};

/**
 * Billboarded markers floating above actors. One `InstancedMesh` per channel, written from the slots
 * the actor renderer already decided were visible, so no second culling pass happens.
 */
export class StateMarkerOverlay {
  readonly group = new Group();
  readonly capacity: number;

  #meshes: InstancedMesh<BufferGeometry, MeshBasicMaterial>[] = [];
  #geometries: BufferGeometry[] = [];
  #enabled = new Uint8Array(MARKER_CHANNELS.length);
  #counts = new Int32Array(MARKER_CHANNELS.length);
  #right = new Vector3();
  #up = new Vector3();
  #fwd = new Vector3();
  #matrix = new Matrix4();
  #ranges = makeRanges(MARKER_CHANNELS.length);
  #sizeM = 2.2;

  constructor(theme: ViewerTheme, capacity = 4096) {
    this.capacity = Math.max(16, capacity);
    this.group.name = "overlay/state-markers";
    const shapes: Record<MarkerChannel, "triangle" | "diamond" | "cross" | "square"> = {
      attacker: "triangle", reported: "diamond", revoked: "cross", detection: "square",
    };
    for (const ch of MARKER_CHANNELS) {
      const g = markerGeometry(shapes[ch]);
      this.#geometries.push(g);
      const m = new MeshBasicMaterial({
        name: `marker-${ch}`, transparent: true, opacity: 0.95, depthTest: true, depthWrite: false,
        side: DoubleSide, toneMapped: false,
      });
      const mesh = new InstancedMesh<BufferGeometry, MeshBasicMaterial>(g, m, this.capacity);
      mesh.name = `overlay/marker-${ch}`;
      mesh.frustumCulled = false;
      mesh.renderOrder = 11;
      mesh.count = 0;
      mesh.visible = false;
      this.#meshes.push(mesh);
      this.group.add(mesh);
    }
    this.setTheme(theme);
  }

  /** Marker size in metres. */
  get sizeM(): number {
    return this.#sizeM;
  }

  set sizeM(v: number) {
    this.#sizeM = Math.max(0.2, v);
  }

  setTheme(theme: ViewerTheme): void {
    const map: Record<MarkerChannel, number> = {
      attacker: theme.actorState.attacker,
      reported: theme.actorState.reported,
      revoked: theme.actorState.revoked,
      detection: theme.groundTruthTag,
    };
    for (let i = 0; i < MARKER_CHANNELS.length; i++) this.#meshes[i].material.color.setHex(map[MARKER_CHANNELS[i]]);
  }

  /** Turn one channel on or off. */
  setChannel(channel: MarkerChannel, enabled: boolean): void {
    const i = CHANNEL_INDEX[channel] ?? -1;
    if (i < 0) return;
    this.#enabled[i] = enabled ? 1 : 0;
    if (!enabled) {
      this.#meshes[i].visible = false;
      this.#meshes[i].count = 0;
    }
  }

  /** Whether a channel is on. */
  isChannelEnabled(channel: MarkerChannel): boolean {
    const i = CHANNEL_INDEX[channel] ?? -1;
    return i >= 0 && this.#enabled[i] === 1;
  }

  setOpacity(channel: MarkerChannel, v: number): void {
    const i = CHANNEL_INDEX[channel] ?? -1;
    if (i >= 0) this.#meshes[i].material.opacity = v;
  }

  /**
   * Rewrite every enabled channel from this frame's visible slots.
   *
   * `predicate` decides which channel a slot belongs to, so the caller can drive `detection` from
   * something other than the pose buffer's state bits.
   */
  update(
    ctx: OverlayUpdateContext,
    predicate: (slot: number, state: number) => MarkerChannel | null,
  ): void {
    let any = false;
    for (let i = 0; i < this.#enabled.length; i++) if (this.#enabled[i] === 1) any = true;
    if (!any) {
      for (let i = 0; i < this.#meshes.length; i++) {
        this.#meshes[i].count = 0;
        this.#meshes[i].visible = false;
      }
      return;
    }

    // One billboard basis for the whole frame: camera right and up in world space.
    const e = ctx.camera.matrixWorld.elements;
    this.#right.set(e[0], e[1], e[2]).normalize();
    this.#up.set(e[4], e[5], e[6]).normalize();
    this.#fwd.set(e[8], e[9], e[10]).normalize();

    this.#counts.fill(0);

    const s = this.#sizeM;
    const lift = ctx.markerHeightM ?? 3.2;
    const m = this.#matrix;
    const me = m.elements;
    me[3] = 0; me[7] = 0; me[11] = 0; me[15] = 1;
    me[0] = this.#right.x * s; me[1] = this.#right.y * s; me[2] = this.#right.z * s;
    me[4] = this.#up.x * s; me[5] = this.#up.y * s; me[6] = this.#up.z * s;
    me[8] = this.#fwd.x * s; me[9] = this.#fwd.y * s; me[10] = this.#fwd.z * s;

    for (let k = 0; k < ctx.visibleCount; k++) {
      const slot = ctx.visibleSlots[k];
      const ch = predicate(slot, ctx.state[slot]);
      if (ch === null) continue;
      const ci = CHANNEL_INDEX[ch] ?? -1;
      if (ci < 0 || this.#enabled[ci] !== 1) continue;
      const n = this.#counts[ci];
      if (n >= this.capacity) continue;
      const p = slot * 3;
      me[12] = ctx.position[p];
      me[13] = ctx.position[p + 1];
      me[14] = ctx.position[p + 2] + lift;
      this.#meshes[ci].setMatrixAt(n, m);
      this.#counts[ci] = n + 1;
    }

    for (let i = 0; i < this.#meshes.length; i++) {
      const mesh = this.#meshes[i];
      const n = this.#counts[i];
      mesh.count = n;
      mesh.visible = n > 0 && this.#enabled[i] === 1;
      if (n > 0) publishRange(mesh.instanceMatrix, n * 16, this.#ranges[i]);
    }
  }

  dispose(): void {
    for (const mesh of this.#meshes) {
      mesh.dispose();
      mesh.material.dispose();
    }
    for (const g of this.#geometries) g.dispose();
    this.#meshes = [];
    this.#geometries = [];
    this.group.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// Actor locators — the aerial vehicle mark
// ---------------------------------------------------------------------------------------------

/**
 * Bounding radius assumed for a vehicle when no `Hello` class table has arrived, metres.
 *
 * Half the body diagonal of the 5.0 × 1.8 × 1.5 m passenger car that every class table so far
 * opens with, which is the right thing to be wrong about: it is what the ratio rule compares the
 * mark against, and guessing "car" keeps a mark on screen until the real table replaces the guess.
 */
const DEFAULT_VEHICLE_RADIUS_M = 2.75;

/**
 * The mark that makes a vehicle visible — and clickable — from map altitude.
 *
 * ## The measurement this exists for
 *
 * `scenarios/phase1-manhattan.yaml` has one equipped vehicle in a two-kilometre world. Opened on
 * the map the Studio sits 1,690 m up, where one screen pixel is 1.55 m of street, so a 5.0 × 1.8 m
 * car covers **3 × 1 pixels**. Driving the page with Playwright and projecting the pose by hand
 * gave the numbers: the vehicle rendered as a sub-pixel smear, and a click within three pixels of
 * its own centre missed it, because {@link Picker} tests the true 5 × 1.8 m box. "I don't really
 * see any cars moving" was not a rendering fault at all — it was a scale fault, and no amount of
 * colour or shading fixes a shape that is smaller than a pixel.
 *
 * ## The rule: one constant solid angle
 *
 * The mark's radius is a fixed **angular** size — {@link angularRadius} radians, scaled by camera
 * distance — so it occupies the same patch of screen at 40 m and at 4 km. That is what stops it
 * both from vanishing when you zoom out and from swamping the street when you zoom in, and it is
 * the same number {@link Picker} grows its hit box to, so **what you can see is what you can
 * click**. One constant, two users: a mark you cannot hit is as useless as a car you cannot see.
 *
 * ## What it draws, and why it changes shape
 *
 * Comparing the vehicle's own angular radius with the mark's decides the form, so the mark is
 * always the thing the vehicle is not:
 *
 * | vehicle radius ÷ mark radius | drawn                            | because                                  |
 * | ---------------------------- | -------------------------------- | ---------------------------------------- |
 * | below {@link ringRatio}      | filled dot over a contrast halo  | the vehicle is sub-pixel; the dot **is** the vehicle |
 * | up to {@link hideRatio}      | a ring around the vehicle        | the vehicle is legible — do not cover it |
 * | above {@link hideRatio}      | nothing (unless it is selected)  | the vehicle is the largest thing in frame |
 *
 * So a dot at map altitude, a ring at the street corner, and a bare car from the chase camera —
 * with no distance threshold anywhere in it, which is what made the previous version fail: it
 * suppressed every mark below 45 m *and* above 64 live actors, and the two rules between them meant
 * a 200-vehicle run drew no marks at all at any zoom (`drawn: 16, culled: 184`, `locators: 0`).
 *
 * ## Colour
 *
 * The dot and the ring carry **state**, through {@link actorStateColorIndex} — the same decision
 * {@link ActorRenderer} paints instances with and {@link ActorRenderer.legend} publishes. At map
 * altitude the mark is the only thing on screen, so a neutral mark would mean the legend describes
 * colours the aerial view never shows. The halo behind the dot is the theme background, which is
 * what keeps a dark dot legible on the light theme and a bright one from glaring on the dark theme;
 * it carries no meaning and so takes no palette entry.
 *
 * The selected vehicle additionally keeps a ring and a vertical stem at every distance, because
 * "which one am I following" has to be answerable from anywhere, and there is only ever one of it.
 */
export class ActorLocatorOverlay {
  readonly group = new Group();

  /**
   * Angular radius of the mark, radians — {@link VEHICLE_MARK_ANGULAR_RADIUS}, which is the same
   * constant {@link Picker} grows its actor hit box to. Change it here and the target moves with
   * it only if you change it there too, which is why the default is shared rather than repeated.
   */
  angularRadius = VEHICLE_MARK_ANGULAR_RADIUS;
  /** Floor on the mark's *drawn* world radius, metres, so a drawn mark cannot collapse. */
  minRadiusM = 1.1;
  /**
   * Vehicle-to-mark radius ratio at which the filled dot opens into a ring.
   *
   * For the 2.8 m radius of a passenger car this puts the changeover at about 460 m of camera
   * distance, where the dot is ~10 px across and the car itself ~11 px long: the mark opens up
   * exactly as the thing underneath it becomes worth looking at.
   */
  ringRatio = 0.9;
  /**
   * …and at which the mark gives up, the vehicle being plainly visible by itself. About 130 m for a
   * car, where its 5 m body is some 26 px long.
   */
  hideRatio = 3.2;
  /** Stem height for the selected vehicle, as a fraction of camera distance. */
  selectedStemPerDistance = 0.055;
  selectedStemMinM = 6;
  /**
   * Most contrast halos drawn at once. Default 2,048.
   *
   * The halo is legibility, not information: it is what keeps a lone dot readable against a pale
   * road or a dark one. Past a couple of thousand dots they are packed tightly enough to define
   * each other's edges, and every halo is a second instance to write — measured at 5,000 vehicles,
   * halos were a third of the mark's whole per-frame cost. So the *marks* are never capped, only
   * their halos, and the cap is on the cosmetic half alone.
   */
  haloLimit = 2048;

  #capacity: number;
  #maxCapacity: number;
  #halo: InstancedMesh<BufferGeometry, MeshBasicMaterial>;
  #dot: InstancedMesh<BufferGeometry, MeshBasicMaterial>;
  #ring: InstancedMesh<BufferGeometry, MeshBasicMaterial>;
  #stem: InstancedMesh<BufferGeometry, MeshBasicMaterial>;
  #discGeom: BufferGeometry;
  #ringGeom: BufferGeometry;
  #stemGeom: BufferGeometry;
  #theme: ViewerTheme;
  #enabled = true;
  #opacity = 1;
  #color = new Color();
  /** Packed state colours, in {@link ACTOR_STATE_COLOR_KEYS} order. */
  #stateColor = new Float32Array(5 * 3);
  /** Per-class bounding radius in metres, from `Hello`; empty means "assume a car". */
  #classRadius = new Float32Array(0);
  /** Colour index last written into each dot/ring instance slot; `0xff` means "never written". */
  #dotKey: Uint8Array;
  #ringKey: Uint8Array;
  #dotColorDirty = true;
  #ringColorDirty = true;
  #ranges = makeRanges(8);
  #drawn = 0;
  #dots = 0;
  #rings = 0;

  constructor(theme: ViewerTheme, capacity = 256, maxCapacity = 20_000) {
    this.#theme = theme;
    this.#capacity = Math.max(1, Math.min(capacity, maxCapacity));
    this.#maxCapacity = Math.max(this.#capacity, maxCapacity);
    this.group.name = "overlay/actor-locators";

    // Both carry a unit `color` attribute: `withUnitVertexColors` explains why nothing per-instance
    // colour is written reaches the screen without one.
    this.#discGeom = discGeometry(28, true);
    this.#ringGeom = withUnitVertexColors(ringGeometry(0.66, 28));
    // A unit stem: 1 m square, base at z = 0, top at z = 1, so its instance matrix is a pure
    // scale-and-translate and the writer never has to build a rotation.
    const stemBuilder = new MeshBuilder({ vertexCapacity: 24, indexCapacity: 36 });
    addBox(stemBuilder, 0, 0, 0.5, 1, 1, 1);
    const stemGeom = stemBuilder.toGeometry();
    if (!stemGeom) throw new Error("locator stem geometry is empty");
    this.#stemGeom = stemGeom;

    this.#halo = this.#makeMesh(this.#discGeom, "locator-halo", this.#capacity, 0.85, 10, false);
    this.#dot = this.#makeMesh(this.#discGeom, "locator-dot", this.#capacity, 0.95, 11, true);
    this.#ring = this.#makeMesh(this.#ringGeom, "locator-ring", this.#capacity, 0.9, 12, true);
    this.#stem = this.#makeMesh(this.#stemGeom, "locator-stem-selected", 1, 0.45, 13, false);
    this.#dotKey = new Uint8Array(this.#capacity).fill(0xff);
    this.#ringKey = new Uint8Array(this.#capacity).fill(0xff);
    this.setTheme(theme);
  }

  /** Marks drawn on the last {@link update} — dots plus rings, the selected stem excluded. */
  get drawn(): number {
    return this.#drawn;
  }

  /** Filled dots on the last {@link update}: vehicles that are sub-pixel without the mark. */
  get dots(): number {
    return this.#dots;
  }

  /** Rings on the last {@link update}: vehicles legible enough not to be covered up. */
  get rings(): number {
    return this.#rings;
  }

  /** Instances each of the dot, halo and ring meshes can hold before the next growth. */
  get capacity(): number {
    return this.#capacity;
  }

  get enabled(): boolean {
    return this.#enabled;
  }

  set enabled(v: boolean) {
    this.#enabled = v;
    if (!v) this.#hide();
  }

  /**
   * Per-class bounding radii in metres, which decide when a vehicle has outgrown its mark.
   *
   * Without them every actor is assumed to be a car, and a pedestrian then loses its dot at the
   * distance a bus would — the opposite of what the ratio rule is for.
   */
  setClassRadii(radii: readonly number[] | Float32Array): void {
    if (this.#classRadius.length !== radii.length) this.#classRadius = new Float32Array(radii.length);
    for (let i = 0; i < radii.length; i++) this.#classRadius[i] = radii[i];
  }

  setTheme(theme: ViewerTheme): void {
    this.#theme = theme;
    const s = theme.actorState;
    for (let i = 0; i < ACTOR_STATE_COLOR_KEYS.length; i++) {
      this.#color.setHex(s[ACTOR_STATE_COLOR_KEYS[i]]);
      this.#stateColor[i * 3] = this.#color.r;
      this.#stateColor[i * 3 + 1] = this.#color.g;
      this.#stateColor[i * 3 + 2] = this.#color.b;
    }
    // The halo is the page behind the mark, not a fifth state: it is there so a dot painted in a
    // palette colour clears its background in both themes.
    this.#halo.material.color.setHex(theme.background);
    this.#stem.material.color.setHex(s.selected);
    this.#invalidateColors();
  }

  setOpacity(v: number): void {
    this.#opacity = Math.max(0, Math.min(1, v));
    this.#halo.material.opacity = 0.85 * this.#opacity;
    this.#dot.material.opacity = 0.95 * this.#opacity;
    this.#ring.material.opacity = 0.9 * this.#opacity;
    this.#stem.material.opacity = 0.45 * this.#opacity;
  }

  #invalidateColors(): void {
    this.#dotKey.fill(0xff);
    this.#ringKey.fill(0xff);
    this.#dotColorDirty = true;
    this.#ringColorDirty = true;
  }

  #makeMesh(
    geom: BufferGeometry, name: string, n: number, opacity: number, order: number, perInstance: boolean,
  ): InstancedMesh<BufferGeometry, MeshBasicMaterial> {
    const material = new MeshBasicMaterial({
      name, transparent: true, opacity: opacity * this.#opacity, side: DoubleSide, toneMapped: false,
      vertexColors: perInstance,
      // No depth test: a mark a building hides is a mark that has failed at the one job it has. No
      // depth *write* either, so it never occludes the city behind it.
      depthTest: false, depthWrite: false, fog: false,
    });
    const mesh = new InstancedMesh<BufferGeometry, MeshBasicMaterial>(geom, material, n);
    mesh.name = `overlay/${name}`;
    mesh.frustumCulled = false;
    mesh.renderOrder = order;
    mesh.count = 0;
    mesh.visible = false;
    mesh.matrixAutoUpdate = false;
    mesh.instanceMatrix.setUsage(DynamicDrawUsage);
    // Every one of these marks is a pure scale-and-translate about +z, so twelve of the sixteen
    // matrix elements are the same on every instance for the life of the mesh. Writing them once
    // here leaves the per-frame loop five stores per instance instead of `setMatrixAt`'s sixteen —
    // the same reason `actors.ts` writes `instanceMatrix.array` by hand instead of composing a
    // `Matrix4`. `zScale` is 1 for the flat discs and rings; the stem overwrites element 10.
    const m = mesh.instanceMatrix.array as Float32Array;
    for (let i = 0; i < n; i++) {
      m[i * 16 + 10] = 1;
      m[i * 16 + 15] = 1;
    }
    if (perInstance) {
      // Touch the colour attribute once so it exists and `setColorAt` never allocates in the loop.
      this.#color.setRGB(1, 1, 1);
      mesh.setColorAt(0, this.#color);
      if (mesh.instanceColor) mesh.instanceColor.setUsage(DynamicDrawUsage);
    }
    this.group.add(mesh);
    return mesh;
  }

  /**
   * Double the capacity of the three per-actor meshes.
   *
   * `InstancedMesh` cannot be resized, so fresh ones replace them. Unlike the actor renderer's
   * growth nothing has to be copied across: this runs *before* the write loop, from
   * {@link update}'s own count of the slots it is about to draw, so there are no written instances
   * to lose. Doubling makes it amortised — a stream that settles at a stable vehicle count stops
   * growing after the first few frames, which is what keeps the per-frame allocation budget.
   */
  #ensureCapacity(needed: number): void {
    if (needed <= this.#capacity || this.#capacity >= this.#maxCapacity) return;
    let next = this.#capacity;
    while (next < needed && next < this.#maxCapacity) next *= 2;
    next = Math.min(next, this.#maxCapacity);
    for (const mesh of [this.#halo, this.#dot, this.#ring]) {
      this.group.remove(mesh);
      mesh.dispose();
      mesh.material.dispose();
    }
    this.#capacity = next;
    this.#halo = this.#makeMesh(this.#discGeom, "locator-halo", next, 0.85, 10, false);
    this.#dot = this.#makeMesh(this.#discGeom, "locator-dot", next, 0.95, 11, true);
    this.#ring = this.#makeMesh(this.#ringGeom, "locator-ring", next, 0.9, 12, true);
    this.#dotKey = new Uint8Array(next).fill(0xff);
    this.#ringKey = new Uint8Array(next).fill(0xff);
    this.#dotColorDirty = true;
    this.#ringColorDirty = true;
    this.#halo.material.color.setHex(this.#theme.background);
  }

  /** Rewrite the marks from this frame's visible slots. */
  update(ctx: OverlayUpdateContext): void {
    this.#drawn = 0;
    this.#dots = 0;
    this.#rings = 0;
    if (!this.#enabled || ctx.visibleCount === 0) {
      this.#hide();
      return;
    }
    this.#ensureCapacity(ctx.visibleCount);

    const ids = ctx.actorId;
    const selected = ctx.selectedActorId ?? null;
    const gt = ctx.showGroundTruth !== false;
    const cls = ctx.classIdx;
    const st = ctx.state;
    const radii = this.#classRadius;
    const nRadii = radii.length;
    const stateColor = this.#stateColor;
    const col = this.#color;
    const cap = this.#capacity;

    const e = ctx.camera.matrixWorld.elements;
    const camX = e[12];
    const camY = e[13];
    const camZ = e[14];

    // The three raw instance-matrix buffers. Only elements 0, 5, 12, 13 and 14 are ever written
    // per frame; `#makeMesh` set 10 and 15 to 1 for every instance when the mesh was built.
    const haloM = this.#halo.instanceMatrix.array as Float32Array;
    const dotM = this.#dot.instanceMatrix.array as Float32Array;
    const ringM = this.#ring.instanceMatrix.array as Float32Array;
    const haloLimit = this.haloLimit;

    let nDot = 0;
    let nRing = 0;
    let nHalo = 0;
    let nStem = 0;

    for (let k = 0; k < ctx.visibleCount; k++) {
      const slot = ctx.visibleSlots[k];
      const p = slot * 3;
      const x = ctx.position[p];
      const y = ctx.position[p + 1];
      const z = ctx.position[p + 2];
      const dx = x - camX;
      const dy = y - camY;
      const dz = z - camZ;
      const dist = Math.sqrt(dx * dx + dy * dy + dz * dz);
      let c = cls[slot];
      if (c >= nRadii) c = 0;
      const vehicleR = nRadii > 0 ? radii[c] : DEFAULT_VEHICLE_RADIUS_M;
      // The *unclamped* angular radius decides the form, and the clamped one decides what is drawn.
      // Using the clamped radius for both was a real bug: `minRadiusM` of 1.1 m holds the ratio for
      // a 2.8 m car below 2.6 however close the camera gets, so `hideRatio` could never fire and
      // every nearby vehicle kept a ring — clutter at street level, which is the opposite of what
      // the rule is for. The floor exists so a drawn mark cannot collapse; it must not also decide
      // whether there is one.
      const angularR = dist * this.angularRadius;
      const markR = Math.max(this.minRadiusM, angularR);
      const ratio = angularR > 0 ? vehicleR / angularR : Infinity;
      const isSelected = selected !== null && ids !== undefined && ids[slot] === (selected >>> 0);

      // The vehicle is already the biggest thing in the frame; a mark over it would hide what the
      // viewer came to look at. This is the one rule, and the selected vehicle obeys it too: from
      // the chase camera you are looking straight at the car you are following, so "which one is
      // it" is not a question, and a 6 m pillar on its roof is just something in the way.
      if (ratio >= this.hideRatio) continue;

      if (isSelected && nStem === 0) {
        const w = Math.max(0.3, markR * 0.28);
        const h = Math.max(this.selectedStemMinM, dist * this.selectedStemPerDistance);
        const sm = this.#stem.instanceMatrix.array as Float32Array;
        sm[0] = w; sm[5] = w; sm[10] = h;
        sm[12] = x; sm[13] = y; sm[14] = z + 0.6;
        nStem = 1;
      }

      const ci = actorStateColorIndex(st[slot], isSelected, gt);

      if (ratio < this.ringRatio) {
        if (nDot >= cap) continue;
        const i = nDot++;
        const o = i * 16;
        dotM[o] = markR; dotM[o + 5] = markR;
        dotM[o + 12] = x; dotM[o + 13] = y; dotM[o + 14] = z + 0.14;
        if (nHalo < haloLimit) {
          // A little wider and behind, so the dot clears whatever is under it in either theme.
          const h = nHalo++ * 16;
          const r = markR * 1.55;
          haloM[h] = r; haloM[h + 5] = r;
          haloM[h + 12] = x; haloM[h + 13] = y; haloM[h + 14] = z + 0.10;
        }
        if (this.#dotKey[i] !== ci) {
          this.#dotKey[i] = ci;
          this.#dotColorDirty = true;
          col.setRGB(stateColor[ci * 3], stateColor[ci * 3 + 1], stateColor[ci * 3 + 2]);
          this.#dot.setColorAt(i, col);
        }
        this.#dots++;
      }

      if (ratio >= this.ringRatio || isSelected) {
        if (nRing >= cap) continue;
        const i = nRing++;
        const o = i * 16;
        // Around the vehicle, never over it: at least a third wider than the body it encircles.
        const r = isSelected
          ? Math.max(markR * 1.5, vehicleR * 1.6)
          : Math.max(markR, vehicleR * 1.35);
        ringM[o] = r; ringM[o + 5] = r;
        ringM[o + 12] = x; ringM[o + 13] = y; ringM[o + 14] = z + 0.16;
        if (this.#ringKey[i] !== ci) {
          this.#ringKey[i] = ci;
          this.#ringColorDirty = true;
          col.setRGB(stateColor[ci * 3], stateColor[ci * 3 + 1], stateColor[ci * 3 + 2]);
          this.#ring.setColorAt(i, col);
        }
        this.#rings++;
      }

      this.#drawn++;
    }

    this.#publish(this.#halo, nHalo, 0);
    this.#publish(this.#dot, nDot, 1);
    this.#publish(this.#ring, nRing, 2);
    this.#publish(this.#stem, nStem, 3);
    if (this.#dotColorDirty && nDot > 0 && this.#dot.instanceColor) {
      publishRange(this.#dot.instanceColor, nDot * 3, this.#ranges[4]);
      this.#dotColorDirty = false;
    }
    if (this.#ringColorDirty && nRing > 0 && this.#ring.instanceColor) {
      publishRange(this.#ring.instanceColor, nRing * 3, this.#ranges[5]);
      this.#ringColorDirty = false;
    }
  }

  #publish(mesh: InstancedMesh<BufferGeometry, MeshBasicMaterial>, n: number, rangeIndex: number): void {
    mesh.count = n;
    mesh.visible = n > 0;
    if (n > 0) publishRange(mesh.instanceMatrix, n * 16, this.#ranges[rangeIndex]);
  }

  #hide(): void {
    for (const mesh of [this.#halo, this.#dot, this.#ring, this.#stem]) {
      mesh.count = 0;
      mesh.visible = false;
    }
  }

  dispose(): void {
    for (const mesh of [this.#halo, this.#dot, this.#ring, this.#stem]) {
      mesh.dispose();
      mesh.material.dispose();
    }
    this.#discGeom.dispose();
    this.#ringGeom.dispose();
    this.#stemGeom.dispose();
    this.group.removeFromParent();
  }
}

// ---------------------------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------------------------

/** Options for {@link OverlayManager}. */
export interface OverlayManagerOptions {
  readonly theme: ViewerTheme;
  readonly world?: WorldRenderer | null;
  readonly pulseCapacity?: number;
  readonly linkCapacity?: number;
  readonly markerCapacity?: number;
  readonly heatmapSize?: number;
  /** Nominal RSU coverage radius in metres for the `coverage`/`rsu_range` overlays. Default 350. */
  readonly coverageRadiusM?: number;
  /**
   * Starting instance capacity of the aerial vehicle mark. Default 256, doubling as needed.
   *
   * It is a *starting* capacity, not a ceiling: the previous fixed 64 doubled as a suppression rule
   * — above 64 live vehicles no mark was drawn at all — and a 200-vehicle run therefore showed
   * nothing anywhere on the map. Every drawn vehicle now gets a mark; this only decides how many
   * frames of doubling it takes to get there.
   */
  readonly locatorCapacity?: number;
  /** Hard ceiling on marks, matching the actor renderer's `maxActors`. Default 20,000. */
  readonly locatorMaxCapacity?: number;
}

/**
 * Overlays that start enabled.
 *
 * The three world-owned ones match what `world-render.ts` builds. The three state-marker channels
 * are on because 09-ui §10 asks for shape redundancy *alongside* the colour-blind-safe palette, and
 * an overlay that has to be switched on is not redundancy — on first load, and after every
 * reconnect, actor state would be carried by instance colour alone (finding Q14). None of the three
 * is ground truth (§5.1: the name would end `_gt`), so the blind-evaluation lock does not apply to
 * them; `attackers_gt` stays off, which is exactly the overlay the lock exists for.
 */
const DEFAULT_ENABLED: readonly OverlayName[] = [
  "lane_markings", "buildings", "signal_state", "reported", "revoked", "detections",
];

/** Overlays this build actually implements. The rest are catalogued as unavailable. */
const IMPLEMENTED: ReadonlySet<OverlayName> = new Set<OverlayName>([
  "tx_pulses", "links", "cbr_heatmap", "coverage", "rsu_range", "attackers_gt", "revoked", "reported",
  "detections", "lane_markings", "buildings", "signal_state", "density",
]);

/**
 * Owns every overlay and maps §6.7's `overlay.set` onto them.
 *
 * Add {@link group} to the scene once. Call {@link update} after the actor renderer, so the marker
 * overlay can reuse its visible-slot list.
 */
export class OverlayManager {
  readonly group = new Group();
  readonly pulses: TxPulseOverlay;
  readonly links: LinkOverlay;
  readonly heatmap: HeatmapOverlay;
  readonly coverage: CoverageOverlay;
  readonly markers: StateMarkerOverlay;
  /** Locator beacons; see {@link ActorLocatorOverlay}. Not a catalogue entry — it is always on. */
  readonly locators: ActorLocatorOverlay;

  #theme: ViewerTheme;
  #world: WorldRenderer | null;
  #enabled = new Map<OverlayName, boolean>();
  #opacity = new Map<OverlayName, number>();
  #gtLocked = false;
  #coverageRadius: number;

  constructor(options: OverlayManagerOptions) {
    this.#theme = options.theme;
    this.#world = options.world ?? null;
    this.#coverageRadius = options.coverageRadiusM ?? 350;
    this.group.name = "overlays";

    this.pulses = new TxPulseOverlay(this.#theme, options.pulseCapacity ?? 4096);
    this.links = new LinkOverlay(this.#theme, options.linkCapacity ?? 8192);
    this.heatmap = new HeatmapOverlay(options.heatmapSize ?? 128, options.heatmapSize ?? 128);
    this.coverage = new CoverageOverlay(this.#theme, 512);
    this.markers = new StateMarkerOverlay(this.#theme, options.markerCapacity ?? 4096);
    this.locators = new ActorLocatorOverlay(
      this.#theme, options.locatorCapacity ?? 256, options.locatorMaxCapacity ?? 20_000);

    this.group.add(this.heatmap.object, this.coverage.object, this.links.object,
      this.pulses.object, this.markers.group, this.locators.group);

    for (const name of OVERLAY_NAMES) {
      this.#enabled.set(name, false);
      this.#opacity.set(name, 1);
    }
    for (const name of DEFAULT_ENABLED) this.#enabled.set(name, true);
    this.#applyAll();
  }

  /** The world whose groups the passive overlays toggle. */
  setWorld(world: WorldRenderer | null): void {
    this.#world = world;
    if (world) {
      const b = world.world?.bbox;
      if (b) this.heatmap.fitTo(b.minXM, b.minYM, b.maxXM, b.maxYM, b.minZM + 0.35);
      this.coverage.setSites(world.sitePositions, world.siteCount, this.#coverageRadius,
        (b?.minZM ?? 0) + 0.25);
    }
    this.#applyAll();
  }

  /** Nominal RSU coverage radius, metres. */
  get coverageRadiusM(): number {
    return this.#coverageRadius;
  }

  set coverageRadiusM(v: number) {
    this.#coverageRadius = Math.max(1, v);
    const w = this.#world;
    if (w) {
      this.coverage.setSites(w.sitePositions, w.siteCount, this.#coverageRadius,
        (w.world?.bbox.minZM ?? 0) + 0.25);
    }
  }

  setTheme(theme: ViewerTheme): void {
    this.#theme = theme;
    this.pulses.setTheme(theme);
    this.links.setTheme(theme);
    this.coverage.setTheme(theme);
    this.markers.setTheme(theme);
    this.locators.setTheme(theme);
  }

  /**
   * Lock ground-truth overlays off (09-ui §6, blind evaluation). While locked, `set(name, true)` on
   * a GT overlay is refused and reported back as `false`.
   */
  lockGroundTruth(locked: boolean): void {
    this.#gtLocked = locked;
    if (locked) {
      for (const name of GROUND_TRUTH_OVERLAYS) this.#enabled.set(name, false);
    }
    this.#applyAll();
  }

  /** Whether GT overlays are locked off. */
  get groundTruthLocked(): boolean {
    return this.#gtLocked;
  }

  /** Turn one overlay on or off. Returns the state actually applied. */
  set(name: OverlayName, enabled: boolean): boolean {
    if (enabled && this.#gtLocked && isGroundTruthOverlay(name)) return false;
    if (enabled && !IMPLEMENTED.has(name)) return false;
    this.#enabled.set(name, enabled);
    this.#applyAll();
    return enabled;
  }

  /** Apply a `Partial<Record<OverlayName, boolean>>`, as §6.7 `overlay.set` delivers it. */
  setMany(overlays: Partial<Record<OverlayName, boolean>>): Partial<Record<OverlayName, boolean>> {
    const out: Partial<Record<OverlayName, boolean>> = {};
    for (const key of Object.keys(overlays) as OverlayName[]) {
      const want = overlays[key];
      if (want === undefined) continue;
      out[key] = this.set(key, want);
    }
    return out;
  }

  /** Whether an overlay is on. */
  isEnabled(name: OverlayName): boolean {
    return this.#enabled.get(name) === true;
  }

  /** Set an overlay's opacity in `[0, 1]`. */
  setOpacity(name: OverlayName, value: number): void {
    const v = Math.max(0, Math.min(1, value));
    this.#opacity.set(name, v);
    switch (name) {
      case "tx_pulses": this.pulses.opacity = v * 0.75; break;
      case "links": this.links.opacity = v * 0.55; break;
      case "cbr_heatmap":
      case "density": this.heatmap.opacity = v * 0.55; break;
      case "coverage":
      case "rsu_range": this.coverage.opacity = v * 0.45; break;
      case "attackers_gt": this.markers.setOpacity("attacker", v); break;
      case "reported": this.markers.setOpacity("reported", v); break;
      case "revoked": this.markers.setOpacity("revoked", v); break;
      case "detections": this.markers.setOpacity("detection", v); break;
      default: break;
    }
  }

  /** The catalogue §6.7 `overlay.set {list: true}` returns. */
  catalogue(): OverlayEntry[] {
    return OVERLAY_NAMES.map((name) => ({
      name,
      groundTruth: isGroundTruthOverlay(name),
      available: IMPLEMENTED.has(name) && !(this.#gtLocked && isGroundTruthOverlay(name)),
      enabled: this.#enabled.get(name) === true,
      opacity: this.#opacity.get(name) ?? 1,
      label: overlayLabel(name),
    }));
  }

  /** The `Partial<Record<OverlayName, boolean>>` §6.7 expects back. */
  states(): Partial<Record<OverlayName, boolean>> {
    const out: Partial<Record<OverlayName, boolean>> = {};
    for (const name of OVERLAY_NAMES) out[name] = this.#enabled.get(name) === true;
    return out;
  }

  #applyAll(): void {
    this.pulses.object.visible = this.isEnabled("tx_pulses");
    this.links.object.visible = this.isEnabled("links");
    this.heatmap.object.visible = this.isEnabled("cbr_heatmap") || this.isEnabled("density");
    this.coverage.object.visible = this.isEnabled("coverage") || this.isEnabled("rsu_range");
    this.markers.setChannel("attacker", this.isEnabled("attackers_gt"));
    this.markers.setChannel("reported", this.isEnabled("reported"));
    this.markers.setChannel("revoked", this.isEnabled("revoked"));
    this.markers.setChannel("detection", this.isEnabled("detections"));
    const w = this.#world;
    if (w) {
      w.markings.visible = this.isEnabled("lane_markings");
      w.buildingsGroup.visible = this.isEnabled("buildings");
      w.signalsGroup.visible = this.isEnabled("signal_state");
    }
  }

  /**
   * Advance the per-frame overlays. Call after the actor renderer's update.
   *
   * The pulse buffer is retired every frame whether or not the overlay is visible: the engine keeps
   * emitting into it from `#onEvent` regardless, and an unretired buffer stays full for the rest of
   * the session, so every later emit drops the oldest pulse and `count` never moves (finding Q11).
   */
  update(ctx: OverlayUpdateContext): void {
    this.pulses.update(ctx.timeSeconds);
    if (this.markers.group.visible) this.markers.update(ctx, defaultMarkerPredicate);
    this.locators.update(ctx);
  }

  dispose(): void {
    this.pulses.dispose();
    this.links.dispose();
    this.heatmap.dispose();
    this.coverage.dispose();
    this.markers.dispose();
    this.locators.dispose();
    this.group.removeFromParent();
  }
}

/** ATTACKER → attacker, REVOKED → revoked, REPORTED → reported; §3.3.4 bits 0–2 in priority order. */
function defaultMarkerPredicate(_slot: number, state: number): MarkerChannel | null {
  if (state & ActorState.REVOKED) return "revoked";
  if (state & ActorState.ATTACKER) return "attacker";
  if (state & ActorState.REPORTED) return "reported";
  return null;
}
