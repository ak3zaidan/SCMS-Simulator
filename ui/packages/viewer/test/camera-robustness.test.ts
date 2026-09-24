/**
 * "Sometimes the camera gets broken and it blacks out or goes into the ground", and "the cars
 * sometimes go through buildings" — the halves of both that need no GPU.
 *
 * The browser half (pixels, the real Studio, random sequences of user actions) is
 * `apps/studio/e2e/camera-fuzz.spec.ts`.
 */

import { describe, expect, it } from "vitest";
import { Vector3 } from "three";
import { Viewer } from "../src/scene.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";
import { placedPoses } from "./support/one-actor.js";

const CANVAS = { width: 1280, height: 800, clientWidth: 1280, clientHeight: 800 } as unknown as ViewerCanvas;
const grid = makeGridWorld({ blocks: 6, blockM: 120, buildingsPerBlock: 2 });

function viewer(): Viewer {
  const v = new Viewer({ canvas: CANVAS, autoStart: false, createRenderer: (c) => new NullRenderer(c) });
  v.setWorld(grid.world);
  return v;
}

/** A vehicle on the street along x = axes[2] + 1.65 heading north. */
function street(): { x: number; y: number } {
  return { x: grid.axes[2] + 1.65, y: grid.axes[1] + 30 };
}

function finite(v: Vector3): boolean {
  return Number.isFinite(v.x) && Number.isFinite(v.y) && Number.isFinite(v.z);
}

describe("camera robustness", () => {
  it("never puts the chase camera under the road, whatever the orbit pitch", () => {
    const v = viewer();
    const { x, y } = street();
    v.capture(placedPoses([{ actorId: 1, x, y, headingRad: Math.PI / 2, speedMps: 0 }], 0n), 0);
    v.step(1 / 60);
    v.flyTo(1, "chase", true);
    // Drag the orbit down as far as it goes, and zoom out: the old controller allowed −20° at up
    // to 400 m, which put the camera 137 m under the road looking up at the ground's underside.
    const target = new EventTarget();
    v.cameras.attachInput(target);
    const ev = (type: string, init: Record<string, number | boolean>): Event => Object.assign(new Event(type), init);
    target.dispatchEvent(ev("pointerdown", { clientX: 100, clientY: 100, button: 0, shiftKey: false }));
    target.dispatchEvent(ev("pointermove", { clientX: 100, clientY: -2000 }));
    target.dispatchEvent(ev("pointerup", {}));
    for (let i = 0; i < 30; i++) target.dispatchEvent(ev("wheel", { deltaY: 400 }));
    for (let i = 0; i < 240; i++) v.step(1 / 60);
    expect(finite(v.camera.position)).toBe(true);
    expect(v.camera.position.z, "camera height above the vehicle's road").toBeGreaterThanOrEqual(0.5);
    v.dispose();
  });

  it("refuses a non-finite pose rather than drawing a NaN camera", () => {
    const v = viewer();
    v.cameras.follow(5);
    v.cameras.setFollowPose(10, 10, 0, 0, 5);
    v.cameras.setMode("chase", true);
    v.cameras.setFollowPose(Number.NaN, 10, 0, 0, 5);
    v.cameras.setFollowPose(10, 10, 0, Number.NaN, Number.POSITIVE_INFINITY);
    for (let i = 0; i < 10; i++) v.cameras.update(1 / 60);
    expect(finite(v.camera.position)).toBe(true);
    expect(finite(v.cameras.look)).toBe(true);
    // And a camera already broken from outside is put back.
    v.camera.position.set(Number.NaN, 0, 0);
    v.cameras.update(1 / 60);
    expect(finite(v.camera.position)).toBe(true);
    expect(v.cameras.recoveries).toBeGreaterThan(0);
    v.dispose();
  });

  it("orbits round a U-turn instead of cutting through the car", () => {
    const v = viewer();
    const { x, y } = street();
    let clock = 0;
    const at = (k: number, h: number): void => {
      v.capture(placedPoses([{ actorId: 2, x, y, headingRad: h, speedMps: 0 }], BigInt(k) * 100_000_000n), clock);
    };
    at(0, Math.PI / 2);
    v.step(1 / 60);
    v.flyTo(2, "chase", true);
    for (let i = 0; i < 60; i++) v.step(1 / 60);
    // Heading flips by 180° in one step.
    let closest = Infinity;
    for (let k = 1; k < 40; k++) {
      at(k, k < 3 ? Math.PI / 2 : -Math.PI / 2);
      for (let f = 0; f < 6; f++) {
        v.step(1 / 60);
        clock += 1 / 60;
        const d = Math.hypot(v.camera.position.x - x, v.camera.position.y - y);
        closest = Math.min(closest, d);
      }
    }
    // A Cartesian lerp between the two chase positions passes over the car (0 m horizontally).
    expect(closest).toBeGreaterThan(4);
    v.dispose();
  });

  it("frames the chase subject inside the part of the viewport a panel does not cover", () => {
    const v = viewer();
    const { x, y } = street();
    v.capture(placedPoses([{ actorId: 3, x, y, headingRad: Math.PI / 2, speedMps: 0 }], 0n), 0);
    v.step(1 / 60);
    v.flyTo(3, "chase", true);
    for (let i = 0; i < 120; i++) v.step(1 / 60);
    const project = (): number => {
      const p = new Vector3(x, y, 0.7).project(v.camera);
      return ((1 - p.y) / 2) * 800; // CSS px from the top
    };
    const free = project();
    // A HUD over the bottom 45 % of an 800 px viewport, as the Studio's floating panel can be.
    v.setViewInsets({ bottom: 360 });
    for (let i = 0; i < 5; i++) v.step(1 / 60);
    const inset = project();
    expect(free, "without insets the subject is where the HUD would be").toBeGreaterThan(440);
    expect(inset, "with the inset it is above the panel").toBeLessThan(440);
    v.dispose();
  });

  it("hides the building a followed vehicle drives through, and keeps the camera behind it", () => {
    const v = viewer();
    const w = v.worldRenderer;
    // Put the vehicle inside a building footprint — a road under a building.
    const b = grid.world.buildings;
    const ring = grid.world.ringPoints;
    const off = b.ringOff[0];
    const cx = (ring.x[off] + ring.x[off + 2]) / 2;
    const cy = (ring.y[off] + ring.y[off + 2]) / 2;
    const top = b.baseZM[0] + b.heightM[0];
    let clock = 0;
    for (let k = 0; k < 4; k++) {
      v.capture(placedPoses([{ actorId: 4, x: cx, y: cy + k * 0.2, headingRad: Math.PI / 2, speedMps: 2 }], BigInt(k) * 100_000_000n), clock);
      clock += 0.1;
    }
    v.step(1 / 60);
    v.flyTo(4, "chase", true);
    for (let i = 0; i < 120; i++) v.step(1 / 60);
    expect(w.ghostBuilding, "the building around the followed vehicle is ghosted").toBe(0);
    // The old rule lifted the camera onto the roof (top + 3 m), where the roof hides the car.
    expect(v.camera.position.z).toBeLessThan(top);
    // Leaving the mode restores it.
    v.setCameraMode("map", true);
    v.step(1 / 60);
    expect(w.ghostBuilding).toBe(-1);
    v.dispose();
  });

  it("puts the near plane where the plan view can resolve the road layers", () => {
    const v = viewer();
    const stream = new SyntheticStream(20, grid);
    stream.keyframe();
    v.capture(stream.poses, 0);
    v.cameras.altitudeM = 1400;
    v.cameras.snap();
    v.step(1 / 60);
    const near = v.camera.near;
    // 24-bit depth at 1,400 m: the gap between the junction and road layers is 6 cm.
    const resolution = (1400 * 1400) / (near * 2 ** 24);
    expect(resolution, `depth resolution at the ground with near = ${near.toFixed(2)} m`).toBeLessThan(0.01);
    v.dispose();
  });

  it("draws each building's real footprint at every distance", () => {
    const v = viewer();
    // Far away: the old far LOD was the footprint's axis-aligned box.
    v.cameras.altitudeM = 3000;
    v.cameras.snap();
    v.step(1 / 60);
    const w = v.worldRenderer;
    // A point inside a building's bounding box but outside its footprint must not be a building.
    // The fixture's footprints are axis-aligned rectangles, so check through the geometry API: the
    // report counts one geometry per building, not three.
    expect(w.report.buildingBackend).toBe("batched");
    const perBuilding = w.report.buildingVertices / grid.world.buildings.count;
    // Walls (4 per edge) + roof (1 per corner) + parapet (4 per edge) for a 4-corner footprint.
    expect(perBuilding).toBeLessThanOrEqual(4 * 4 + 4 + 4 * 4);
    v.dispose();
  });
});
