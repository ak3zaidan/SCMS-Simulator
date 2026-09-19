import { describe, expect, it } from "vitest";
import { Box3, Vector3 } from "three";
import {
  MeshBuilder, addBox, addExtrudedRing, addPolygon, addRibbon, buildActorGeometry, earClip,
  markerGeometry, ringGeometry, ringSignedArea,
} from "../src/geometry.js";
import { DEFAULT_ACTOR_CLASSES } from "../src/actors.js";
import { pointInRing } from "../src/world-render.js";

describe("MeshBuilder", () => {
  it("grows, snapshots and resets without losing its backing arrays", () => {
    const b = new MeshBuilder({ color: true, vertexCapacity: 4, indexCapacity: 4 });
    addBox(b, 0, 0, 0, 2, 2, 2);
    expect(b.vertexCount).toBe(24);
    expect(b.indexCount).toBe(36);
    const g = b.toGeometry();
    expect(g).not.toBeNull();
    expect(g?.getAttribute("position").count).toBe(24);
    expect(g?.getAttribute("color")).toBeTruthy();
    const positions = b.positions;
    b.reset();
    expect(b.empty).toBe(true);
    addBox(b, 5, 0, 0, 1, 1, 1);
    expect(b.positions).toBe(positions); // reused, not reallocated
    g?.dispose();
  });

  it("builds a ribbon whose width matches the request", () => {
    const b = new MeshBuilder({ color: true });
    const xs = Float32Array.from([0, 10, 20]);
    const ys = Float32Array.from([0, 0, 0]);
    addRibbon(b, xs, ys, null, 0, 3, 2, 0.1);
    const g = b.toGeometry();
    expect(g).not.toBeNull();
    const box = new Box3().setFromBufferAttribute(g!.getAttribute("position") as never);
    expect(box.min.y).toBeCloseTo(-2, 5);
    expect(box.max.y).toBeCloseTo(2, 5);
    expect(box.min.z).toBeCloseTo(0.1, 5);
    g?.dispose();
  });
});

describe("earClip", () => {
  it("triangulates a convex ring", () => {
    const xs = Float32Array.from([0, 10, 10, 0]);
    const ys = Float32Array.from([0, 0, 10, 10]);
    const out: number[] = [];
    earClip(xs, ys, 0, 4, out);
    expect(out.length).toBe(6);
    expect(ringSignedArea(xs, ys, 0, 4)).toBeCloseTo(100, 6);
  });

  it("triangulates a concave ring and covers its area", () => {
    // An L shape, counter-clockwise.
    const xs = Float32Array.from([0, 10, 10, 4, 4, 0]);
    const ys = Float32Array.from([0, 0, 4, 4, 10, 10]);
    const out: number[] = [];
    earClip(xs, ys, 0, 6, out);
    expect(out.length).toBe(12); // n − 2 triangles
    let area = 0;
    for (let i = 0; i < out.length; i += 3) {
      const [a, b, c] = [out[i], out[i + 1], out[i + 2]];
      area += Math.abs((xs[b] - xs[a]) * (ys[c] - ys[a]) - (ys[b] - ys[a]) * (xs[c] - xs[a])) / 2;
    }
    expect(area).toBeCloseTo(10 * 4 + 4 * 6, 4);
  });

  it("handles a clockwise ring by flipping it", () => {
    const xs = Float32Array.from([0, 0, 10, 10]);
    const ys = Float32Array.from([0, 10, 10, 0]);
    expect(ringSignedArea(xs, ys, 0, 4)).toBeLessThan(0);
    const out: number[] = [];
    earClip(xs, ys, 0, 4, out);
    expect(out.length).toBe(6);
  });
});

describe("addExtrudedRing", () => {
  it("produces a solid of the requested height at each LOD", () => {
    const xs = Float32Array.from([0, 20, 20, 0]);
    const ys = Float32Array.from([0, 0, 15, 15]);
    const scratch: number[] = [];
    for (const lod of [0, 1, 2] as const) {
      const b = new MeshBuilder({ color: true });
      addExtrudedRing(b, xs, ys, 0, 4, 2, 30, lod, scratch);
      const g = b.toGeometry();
      expect(g).not.toBeNull();
      const box = new Box3().setFromBufferAttribute(g!.getAttribute("position") as never);
      expect(box.min.z).toBeCloseTo(2, 5);
      // LOD 0 adds a 0.4 m parapet band.
      expect(box.max.z).toBeCloseTo(lod === 0 ? 32.4 : 32, 5);
      expect(box.min.x).toBeCloseTo(0, 5);
      expect(box.max.x).toBeCloseTo(20, 5);
      g?.dispose();
    }
  });
});

describe("pointInRing", () => {
  it("classifies points against a footprint", () => {
    const xs = Float32Array.from([0, 20, 20, 0]);
    const ys = Float32Array.from([0, 0, 15, 15]);
    expect(pointInRing(xs, ys, 0, 4, 10, 7)).toBe(true);
    expect(pointInRing(xs, ys, 0, 4, -1, 7)).toBe(false);
    expect(pointInRing(xs, ys, 0, 4, 10, 20)).toBe(false);
  });
});

describe("buildActorGeometry", () => {
  it("fits the class dimensions, forward along +x", () => {
    for (const def of DEFAULT_ACTOR_CLASSES) {
      for (const lod of [0, 1, 2] as const) {
        const g = buildActorGeometry(def, lod);
        const box = new Box3().setFromBufferAttribute(g.getAttribute("position") as never);
        const size = box.getSize(new Vector3());
        expect(size.x).toBeLessThanOrEqual(def.lengthM + 0.5);
        expect(size.y).toBeLessThanOrEqual(def.widthM + 0.5);
        expect(box.min.z).toBeGreaterThanOrEqual(-0.01); // sits on the ground
        expect(box.max.z).toBeLessThanOrEqual(def.heightM + 0.3);
        expect(g.getIndex()).not.toBeNull();
        g.dispose();
      }
    }
  });

  it("gives the near LOD more geometry than the far one", () => {
    const car = DEFAULT_ACTOR_CLASSES[0];
    const near = buildActorGeometry(car, 0);
    const mid = buildActorGeometry(car, 1);
    const far = buildActorGeometry(car, 2);
    const count = (g: { getIndex(): { count: number } | null }): number => g.getIndex()?.count ?? 0;
    expect(count(near)).toBeGreaterThan(count(mid));
    expect(count(mid)).toBeGreaterThan(count(far));
    expect(count(far)).toBe(36); // one box
    near.dispose();
    mid.dispose();
    far.dispose();
  });
});

describe("overlay geometries", () => {
  it("builds rings and every marker shape", () => {
    const r = ringGeometry(0.7, 16);
    expect(r.getAttribute("uv")).toBeTruthy();
    expect(r.getIndex()?.count).toBe(16 * 6);
    r.dispose();
    for (const shape of ["triangle", "square", "diamond", "cross"] as const) {
      const g = markerGeometry(shape);
      expect(g.getAttribute("position").count).toBeGreaterThan(2);
      g.dispose();
    }
  });
});

describe("addPolygon", () => {
  it("lays a flat polygon at the requested height", () => {
    const b = new MeshBuilder({ color: true });
    const xs = Float32Array.from([0, 10, 10, 0]);
    const ys = Float32Array.from([0, 0, 10, 10]);
    addPolygon(b, xs, ys, 0, 4, 1.5, []);
    const g = b.toGeometry();
    const box = new Box3().setFromBufferAttribute(g!.getAttribute("position") as never);
    expect(box.min.z).toBeCloseTo(1.5, 6);
    expect(box.max.z).toBeCloseTo(1.5, 6);
    g?.dispose();
  });
});
