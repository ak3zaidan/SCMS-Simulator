/**
 * A `ViewerRenderer` that does everything except talk to a GPU.
 *
 * Node has no WebGL context, so the smoke test substitutes this. It is not a no-op: it walks the
 * scene the way the real renderer's projection pass does and counts the drawables that *would* be
 * submitted, honouring `visible`, `InstancedMesh.count` and `LineSegments` draw ranges. That makes
 * the draw-call number in the performance test a real measurement of the scene graph the viewer
 * built, even though nothing is rasterised.
 */

import type { Camera, Scene } from "three";
import { BatchedMesh, InstancedMesh, Line, Mesh, Points } from "three";
import type { RendererInfoLike, ViewerRenderer } from "../../src/types.js";

interface MutableInfo {
  render: { calls: number; triangles: number; frame: number; lines: number; points: number };
  memory: { geometries: number; textures: number };
  autoReset: boolean;
  reset(): void;
}

/** Counts what a real renderer would submit, without a GL context. */
export class NullRenderer implements ViewerRenderer {
  readonly domElement: HTMLCanvasElement;
  readonly info: RendererInfoLike;
  #info: MutableInfo;
  #width = 0;
  #height = 0;
  #pixelRatio = 1;
  #renders = 0;
  #disposed = false;

  constructor(canvas?: unknown) {
    this.domElement = (canvas ?? {}) as HTMLCanvasElement;
    this.#info = {
      render: { calls: 0, triangles: 0, frame: 0, lines: 0, points: 0 },
      memory: { geometries: 0, textures: 0 },
      autoReset: true,
      reset: () => {
        this.#info.render.calls = 0;
        this.#info.render.triangles = 0;
        this.#info.render.lines = 0;
        this.#info.render.points = 0;
      },
    };
    this.info = this.#info;
  }

  /** Frames rendered. */
  get renders(): number {
    return this.#renders;
  }

  get pixelRatio(): number {
    return this.#pixelRatio;
  }

  get size(): { width: number; height: number } {
    return { width: this.#width, height: this.#height };
  }

  get disposed(): boolean {
    return this.#disposed;
  }

  setSize(width: number, height: number): void {
    this.#width = width;
    this.#height = height;
  }

  setPixelRatio(value: number): void {
    this.#pixelRatio = value;
  }

  dispose(): void {
    this.#disposed = true;
  }

  render(scene: Scene, camera: Camera): void {
    this.#renders++;
    this.#info.render.frame++;
    this.#info.reset();
    camera.updateMatrixWorld();
    scene.updateMatrixWorld(true);
    let geometries = 0;
    scene.traverseVisible((o) => {
      if (o instanceof BatchedMesh) {
        this.#info.render.calls++;
        geometries++;
        return;
      }
      if (o instanceof InstancedMesh) {
        if (o.count > 0) {
          this.#info.render.calls++;
          const idx = o.geometry.getIndex();
          const tri = idx ? idx.count / 3 : (o.geometry.getAttribute("position")?.count ?? 0) / 3;
          this.#info.render.triangles += tri * o.count;
        }
        geometries++;
        return;
      }
      if (o instanceof Line) {
        const range = o.geometry.drawRange;
        if (range.count > 0) {
          this.#info.render.calls++;
          this.#info.render.lines += range.count / 2;
        }
        geometries++;
        return;
      }
      if (o instanceof Points) {
        this.#info.render.calls++;
        geometries++;
        return;
      }
      if (o instanceof Mesh) {
        this.#info.render.calls++;
        const idx = o.geometry.getIndex();
        this.#info.render.triangles += (idx ? idx.count : (o.geometry.getAttribute("position")?.count ?? 0)) / 3;
        geometries++;
      }
    });
    this.#info.memory.geometries = geometries;
  }
}
