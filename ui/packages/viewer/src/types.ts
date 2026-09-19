/**
 * Shared public types for `@vwp/viewer`.
 *
 * Coordinate convention (normative for this package): the scene is in the **same ENU metres the
 * world payload and the pose buffer use** — `x` east, `y` north, `z` up (docs/protocol/vwp-v1.md
 * §3.2, §4.2). Nothing is transposed on the way in, so a `PoseBuffer.positions` triple can be
 * written straight into an instance matrix. The cost is that the camera's `up` is `(0, 0, 1)`
 * rather than three.js's default `(0, 1, 0)`; {@link ENU_UP} is that vector and the viewer sets it
 * on every camera and light it owns. `Object3D.DEFAULT_UP` is never mutated — that is a global and
 * this is a library.
 *
 * Heading follows the engine: radians counter-clockwise from +x (east), so a model whose local
 * forward is +x needs only a rotation about +z (§3.2, and `atan2(dy, dx)` in the reference engine).
 */

import type { Camera, Scene, Vector3 } from "three";

/** The ENU up axis, `(0, 0, 1)`. Cloned on use; never handed out for mutation. */
export const ENU_UP: readonly [number, number, number] = [0, 0, 1];

/** The subset of `THREE.WebGLInfo` the viewer reads. `WebGLInfo` is assignable to it. */
export interface RendererInfoLike {
  readonly render: {
    readonly calls: number;
    readonly triangles: number;
    readonly frame: number;
    readonly lines: number;
    readonly points: number;
  };
  readonly memory: { readonly geometries: number; readonly textures: number };
  autoReset?: boolean;
  reset?(): void;
}

/**
 * The subset of `THREE.WebGLRenderer` the viewer drives. `WebGLRenderer` satisfies it structurally;
 * a headless test or an `OffscreenCanvas` worker can supply its own.
 */
export interface ViewerRenderer {
  render(scene: Scene, camera: Camera): void;
  setSize(width: number, height: number, updateStyle?: boolean): void;
  setPixelRatio(value: number): void;
  setScissorTest?(enable: boolean): void;
  dispose(): void;
  readonly info: RendererInfoLike;
  readonly domElement: HTMLCanvasElement;
}

/** A canvas the viewer can mount on. `OffscreenCanvas` is accepted where the platform has it. */
export type ViewerCanvas = HTMLCanvasElement | OffscreenCanvas;

/** Anything that can schedule a frame. Injected so tests can drive the loop by hand. */
export interface FrameScheduler {
  request(callback: (timeMs: number) => void): number;
  cancel(handle: number): void;
  /** Monotonic clock in milliseconds. */
  now(): number;
}

/** One actor class as the viewer renders it — dimensions and base colour come from `Hello` (§3.1.4). */
export interface ActorClassDef {
  /** Index into `Hello.classes`; also the `class_idx` carried by keyframe and delta rows. */
  readonly index: number;
  readonly name: string;
  readonly lengthM: number;
  readonly widthM: number;
  readonly heightM: number;
  /** Packed `0xRRGGBB` (the alpha byte of `Hello.classes.color_rgba` is dropped). */
  readonly color: number;
  /** `ActorCategory`: 0 vehicle, 1 vru, 2 infrastructure, 3 other. */
  readonly category: number;
}

/** Level of detail band. 0 is the closest and most detailed. */
export type LodLevel = 0 | 1 | 2;

/** The three LOD levels, in order. */
export const LOD_LEVELS: readonly LodLevel[] = [0, 1, 2];

/** What a click hit. */
export type PickResult =
  | {
      readonly kind: "actor";
      readonly actorId: number;
      readonly slot: number;
      readonly distanceM: number;
      readonly point: { readonly x: number; readonly y: number; readonly z: number };
    }
  | {
      readonly kind: "ground";
      readonly distanceM: number;
      readonly point: { readonly x: number; readonly y: number; readonly z: number };
    }
  | {
      readonly kind: "site";
      readonly siteId: number;
      readonly nodeId: number;
      readonly distanceM: number;
      readonly point: { readonly x: number; readonly y: number; readonly z: number };
    }
  | null;

/** A read-only 3-vector in ENU metres. */
export interface Vec3Like {
  readonly x: number;
  readonly y: number;
  readonly z: number;
}

/** Copy a `Vector3` into a plain record without allocating in the caller's hot path. */
export function vecToPlain(v: Vector3): { x: number; y: number; z: number } {
  return { x: v.x, y: v.y, z: v.z };
}
