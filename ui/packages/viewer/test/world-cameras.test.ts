import { describe, expect, it } from "vitest";
import { PerspectiveCamera, Vector3 } from "three";
import { CameraController } from "../src/cameras.js";
import { WorldRenderer } from "../src/world-render.js";
import { DARK_THEME, LIGHT_THEME } from "../src/theme.js";
import { makeGridWorld } from "./support/fixture.js";

const grid = makeGridWorld({ blocks: 6, blockM: 120, buildingsPerBlock: 2 });

function makeWorldRenderer(): WorldRenderer {
  const w = new WorldRenderer({ theme: DARK_THEME, tileSizeM: 300 });
  w.setWorld(grid.world);
  return w;
}

describe("WorldRenderer", () => {
  it("builds every §4 section into the scene graph", () => {
    const w = makeWorldRenderer();
    const r = w.report;
    expect(r.lanes).toBe(grid.world.lanes.count);
    expect(r.buildings).toBe(grid.world.buildings.count);
    expect(r.signals).toBe(grid.world.signals.count);
    expect(r.sites).toBe(grid.world.sites.count);
    expect(r.crossings).toBe(grid.world.crossings.count);
    expect(w.tiles.children.length).toBeGreaterThan(0);
    expect(w.markings.children.length).toBeGreaterThan(0);
    // Housings, lamps (three per head) and stop bars: three instanced drawables whatever the count.
    expect(w.signalsGroup.children.length).toBe(3);
    expect(w.signals.count).toBe(grid.world.signals.count);
    expect(w.sitesGroup.children.length).toBe(1);
    expect(w.siteCount).toBe(grid.world.sites.count);
    expect(w.sitePositions.length).toBe(grid.world.sites.count * 3);
    // BatchedMesh is the intended path; the merged fallback would report "merged".
    expect(r.buildingBackend).toBe("batched");
    expect(w.buildingBackendReason).toBeNull();
    w.dispose();
  });

  it("answers building-top queries from its occupancy grid", () => {
    const w = makeWorldRenderer();
    const b = grid.world.buildings;
    const ring = grid.world.ringPoints;
    const off = b.ringOff[0];
    const cx = (ring.x[off] + ring.x[off + 2]) / 2;
    const cy = (ring.y[off] + ring.y[off + 2]) / 2;
    expect(w.buildingTopAt(cx, cy)).toBeCloseTo(b.baseZM[0] + b.heightM[0], 3);
    // The middle of a junction is never inside a footprint.
    expect(w.buildingTopAt(grid.axes[2], grid.axes[2])).toBe(-Infinity);
    expect(w.buildingTopAt(1e9, 1e9)).toBe(-Infinity);
    w.dispose();
  });

  it("swaps building LOD as the camera moves and holds steady otherwise", () => {
    const w = makeWorldRenderer();
    const camera = new PerspectiveCamera(50, 1.6, 0.5, 8000);
    camera.up.set(0, 0, 1);
    camera.position.set(0, 0, 40);
    camera.updateMatrixWorld();
    const visible = w.updateLod(camera);
    expect(visible).toBe(grid.world.buildings.count);
    // A second call from the same place is a no-op and returns the cached count.
    expect(w.updateLod(camera)).toBe(visible);
    camera.position.set(0, 0, 3000);
    camera.updateMatrixWorld();
    expect(w.updateLod(camera)).toBe(visible);
    w.dispose();
  });

  it("drives the sun and sky from the time of day", () => {
    const w = makeWorldRenderer();
    w.setTimeOfDay(12);
    const noonIntensity = w.sun.intensity;
    const noonZ = w.sun.position.z;
    w.setTimeOfDay(0);
    expect(w.sun.intensity).toBeLessThan(noonIntensity);
    expect(w.sun.castShadow).toBe(false);
    w.setTimeOfDay(7);
    expect(w.sun.position.z).toBeLessThan(noonZ);
    expect(w.sun.intensity).toBeGreaterThan(0);
    // Wrapping is well defined.
    w.setTimeOfDay(36);
    expect(w.timeOfDay).toBe(12);
    w.dispose();
  });

  it("applies signal phases and survives a theme swap", () => {
    const w = makeWorldRenderer();
    w.updateSignalPhases({
      count: 3,
      signalId: Uint32Array.from([0, 1, 2]),
      timeToChangeDs: Uint16Array.from([10, 20, 30]),
      phase: Uint8Array.from([6, 3, 8]),
      reserved: new Uint8Array(3),
    });
    // Head 0 is green (its green lamp, index 2, lit) and head 1 red (its red lamp, index 0, lit).
    const sum = (c: [number, number, number] | null): number => (c ? c[0] + c[1] + c[2] : 0);
    expect(w.signals.headState(0)?.aspect.name).toBe("green");
    expect(w.signals.headState(1)?.aspect.name).toBe("red");
    expect(sum(w.signals.lampColor(0, 2))).toBeGreaterThan(sum(w.signals.lampColor(0, 0)) * 5);
    expect(sum(w.signals.lampColor(1, 0))).toBeGreaterThan(sum(w.signals.lampColor(1, 2)) * 5);
    w.setTheme(LIGHT_THEME);
    expect(w.report.lanes).toBe(grid.world.lanes.count);
    // The rebuild a theme swap does keeps what the lamps show.
    expect(w.signals.headState(0)?.aspect.name).toBe("green");
    w.dispose();
  });
});

describe("CameraController", () => {
  function make(): { camera: PerspectiveCamera; ctl: CameraController; world: WorldRenderer } {
    const camera = new PerspectiveCamera(45, 16 / 9, 0.35, 12_000);
    const world = makeWorldRenderer();
    const ctl = new CameraController({ camera, world });
    ctl.setViewportSize(1600, 900);
    return { camera, ctl, world };
  }

  it("puts the map camera above its target and the chase camera behind the vehicle", () => {
    const { camera, ctl, world } = make();
    ctl.focusOn(100, -50, 0);
    ctl.altitudeM = 500;
    ctl.setMode("map", true);
    expect(camera.position.z).toBeCloseTo(500, 3);
    // Above whatever focus survived the world clamp — the plan view is not allowed to point
    // somewhere that puts part of the frame outside the world. `ctl.target` is that focus.
    expect(Math.hypot(camera.position.x - ctl.target.x, camera.position.y - ctl.target.y)).toBeLessThan(0.2);

    ctl.follow(7);
    ctl.setFollowPose(0, 0, 0, 0, 12);
    ctl.setMode("chase", true);
    // Heading 0 is +x, so the chase camera sits at negative x, above the vehicle.
    expect(camera.position.x).toBeLessThan(-4);
    expect(Math.abs(camera.position.y)).toBeLessThan(0.5);
    expect(camera.position.z).toBeGreaterThan(1);

    ctl.setMode("dashboard", true);
    expect(camera.position.x).toBeGreaterThan(0);
    expect(camera.position.z).toBeGreaterThan(1);
    expect(camera.position.z).toBeLessThan(2.5);
    world.dispose();
  });

  it("watches from an RSU mast", () => {
    const { camera, ctl, world } = make();
    ctl.setFollowPose(50, 50, 0, 0, 0);
    ctl.viewFromSite(0, true);
    const p = world.sitePositions;
    expect(camera.position.x).toBeCloseTo(p[0], 3);
    expect(camera.position.y).toBeCloseTo(p[1], 3);
    expect(camera.position.z).toBeCloseTo(p[2] + 1.2, 3);
    expect(ctl.mode).toBe("rsu");
    world.dispose();
  });

  it("smooths at 1 − exp(−λ·dt) and is frame-rate independent", () => {
    const { camera, ctl, world } = make();
    ctl.focusOn(0, 0, 0);
    ctl.altitudeM = 100;
    ctl.setMode("map", true);
    const start = camera.position.z;
    ctl.altitudeM = 1100;
    ctl.update(0.1);
    const k = 1 - Math.exp(-0.1 * ctl.positionLambda);
    expect(camera.position.z).toBeCloseTo(start + (1100 - start) * k, 2);

    // One 0.2 s step and two 0.1 s steps must land in the same place.
    const a = new CameraController({ camera: new PerspectiveCamera(45, 1.6, 0.35, 12_000) });
    const b = new CameraController({ camera: new PerspectiveCamera(45, 1.6, 0.35, 12_000) });
    for (const c of [a, b]) {
      c.focusOn(0, 0, 0);
      c.altitudeM = 100;
      c.setMode("map", true);
      c.altitudeM = 1100;
    }
    a.update(0.2);
    b.update(0.1);
    b.update(0.1);
    expect(a.camera.position.z).toBeCloseTo(b.camera.position.z, 6);
    world.dispose();
  });

  it("round-trips fitExtent and reports a serialisable state", () => {
    const { ctl, world } = make();
    ctl.setMode("map", true);
    ctl.fitExtent(1200);
    expect(ctl.extentM).toBeCloseTo(1200, 3);
    const s = ctl.state();
    expect(s.mode).toBe("map");
    expect(s.fovDeg).toBeGreaterThan(0);
    expect(typeof s.position.x).toBe("number");
    expect(s.followActorId).toBeNull();
    world.dispose();
  });

  it("does not run the occlusion march at fly-down range", () => {
    const { ctl, world } = make();
    ctl.follow(7);
    ctl.setFollowPose(0, 0, 0, 0, 0);
    expect(ctl.setMode("chase", true)).toBe("chase");
    const b = grid.world.buildings;
    const ring = grid.world.ringPoints;
    const off = b.ringOff[0];
    const cx = (ring.x[off] + ring.x[off + 2]) / 2;
    const cy = (ring.y[off] + ring.y[off + 2]) / 2;

    // Far away and high: only the roof lift may apply, and here it does not.
    const far = new Vector3(cx, cy, 800);
    const look = new Vector3(cx, cy, 0);
    ctl.keepCameraOutsideBuildings(far, look);
    expect(far.z).toBe(800);

    // Inside the building: lifted above the roof.
    const inside = new Vector3(cx, cy, 1);
    ctl.keepCameraOutsideBuildings(inside, new Vector3(cx + 4, cy, 1));
    expect(inside.z).toBeGreaterThan(b.baseZM[0] + b.heightM[0]);
    world.dispose();
  });
});
