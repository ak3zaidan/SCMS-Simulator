/**
 * The street-level cameras: what they refuse to draw, where they stand, and how they get there.
 *
 * Every check here corresponds to a frame that was captured from the running Studio and looked at.
 * The headline one is the first: asking for `chase` with nothing followed used to put the camera at
 * the *plan view's focus* — which at start-up is the centre of the world — at street level, inside
 * whatever city block happened to be there. On `scenarios/phase1-manhattan.yaml` that was
 * (921, 1000, 20) looking at a point 8 m away while the only vehicle in the run was at
 * (1295, 2016): two dark planes, one lane marking, and no car. The owner's screenshot of it is the
 * reason this file exists.
 */

import { describe, expect, it } from "vitest";
import { PerspectiveCamera, Vector3 } from "three";
import { CameraController } from "../src/cameras.js";
import { WorldRenderer } from "../src/world-render.js";
import { Viewer } from "../src/scene.js";
import { DARK_THEME } from "../src/theme.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";

const grid = makeGridWorld({ blocks: 8, blockM: 120, buildingsPerBlock: 2 });
const CANVAS = { width: 1600, height: 900, clientWidth: 1600, clientHeight: 900 } as unknown as ViewerCanvas;

function make(): { camera: PerspectiveCamera; ctl: CameraController; world: WorldRenderer } {
  const camera = new PerspectiveCamera(45, 16 / 9, 0.35, 12_000);
  const world = new WorldRenderer({ theme: DARK_THEME, tileSizeM: 300 });
  world.setWorld(grid.world);
  const ctl = new CameraController({ camera, world });
  ctl.setViewportSize(1600, 900);
  return { camera, ctl, world };
}

function makeViewer(opts: { autoFrameActors?: boolean } = {}): Viewer {
  return new Viewer({
    canvas: CANVAS,
    theme: "dark",
    autoStart: false,
    shadows: false,
    autoFrameActors: opts.autoFrameActors,
    createRenderer: (canvas) => new NullRenderer(canvas),
  });
}

describe("the street-level modes refuse to draw a frame that means nothing", () => {
  it("will not enter chase or dashboard with no vehicle to sit behind", () => {
    const { ctl, world } = make();
    ctl.focusOn(400, 400, 0);
    ctl.setMode("map", true);

    expect(ctl.hasFollowSubject).toBe(false);
    expect(ctl.setMode("chase")).toBe("map");
    expect(ctl.mode).toBe("map");
    expect(ctl.rejectedMode).toBe("chase");

    expect(ctl.setMode("dashboard")).toBe("map");
    expect(ctl.rejectedMode).toBe("dashboard");

    // `jump` is the same claim in one keystroke.
    expect(ctl.setMode("jump")).toBe("map");

    // With a subject, all three are granted and the refusal is forgotten.
    ctl.follow(3);
    ctl.setFollowPose(500, 500, 0, 0, 10);
    expect(ctl.setMode("chase")).toBe("chase");
    expect(ctl.rejectedMode).toBeNull();
    world.dispose();
  });

  it("will not watch from an RSU mast a world does not have", () => {
    const camera = new PerspectiveCamera(45, 16 / 9, 0.35, 12_000);
    const bare = new CameraController({ camera });
    expect(bare.setMode("rsu")).toBe("map");
    expect(bare.rejectedMode).toBe("rsu");

    const { ctl, world } = make();
    expect(world.siteCount).toBeGreaterThan(0);
    expect(ctl.setMode("rsu")).toBe("rsu");
    world.dispose();
  });

  it("holds the last known pose when the followed vehicle stops streaming", () => {
    const { camera, ctl, world } = make();
    // The plan view is pointing somewhere else entirely — the case that broke.
    ctl.focusOn(200, 200, 0);
    ctl.follow(9);
    ctl.setFollowPose(700, 700, 0, 0, 0);
    ctl.setMode("chase", true);
    const behind = camera.position.clone();
    expect(Math.hypot(behind.x - 700, behind.y - 700)).toBeLessThan(15);

    // The run ends, or the vehicle despawns.
    ctl.clearFollowPose();
    ctl.focusOn(200, 200, 0);
    for (let i = 0; i < 120; i++) ctl.update(1 / 60);

    // Still behind the vehicle's last known position, not at the map focus.
    expect(Math.hypot(camera.position.x - 700, camera.position.y - 700)).toBeLessThan(15);
    expect(Math.hypot(camera.position.x - 200, camera.position.y - 200)).toBeGreaterThan(100);
    world.dispose();
  });
});

describe("the plan view stays inside the world", () => {
  it("clamps the focus so no part of the frame is off-world", () => {
    const { ctl, world } = make();
    const b = grid.world.bbox;
    ctl.setMode("map", true);
    ctl.fitExtent(400);
    const halfV = ctl.extentM / 2;
    const halfH = halfV * ctl.camera.aspect;

    // Drive the focus hard past the north-east corner, as a followed vehicle at the edge does.
    ctl.focusOn(b.maxXM + 500, b.maxYM + 500, 0);
    ctl.update(1 / 60);
    expect(ctl.target.x + halfH).toBeLessThanOrEqual(b.maxXM + 1e-6);
    expect(ctl.target.y + halfV).toBeLessThanOrEqual(b.maxYM + 1e-6);

    ctl.focusOn(b.minXM - 500, b.minYM - 500, 0);
    ctl.update(1 / 60);
    expect(ctl.target.x - halfH).toBeGreaterThanOrEqual(b.minXM - 1e-6);
    expect(ctl.target.y - halfV).toBeGreaterThanOrEqual(b.minYM - 1e-6);
    world.dispose();
  });

  it("centres a world smaller than the frame rather than showing void on one side", () => {
    const { ctl, world } = make();
    const b = grid.world.bbox;
    ctl.setMode("map", true);
    ctl.fitExtent((b.maxYM - b.minYM) * 4);
    ctl.focusOn(b.maxXM, b.maxYM, 0);
    ctl.update(1 / 60);
    expect(ctl.target.x).toBeCloseTo((b.minXM + b.maxXM) / 2, 3);
    expect(ctl.target.y).toBeCloseTo((b.minYM + b.maxYM) / 2, 3);
    world.dispose();
  });
});

describe("the map-to-chase flight", () => {
  /** Fly a controller from a plan view down to a parked vehicle, sampling every frame. */
  function fly(step: number, frames: number): {
    ctl: CameraController; path: Vector3[]; start: Vector3; vehicle: Vector3; world: WorldRenderer;
  } {
    const { ctl, world } = make();
    const b = grid.world.bbox;
    const vehicle = new Vector3((b.minXM + b.maxXM) / 2, (b.minYM + b.maxYM) / 2, 0);
    ctl.altitudeM = 1700;
    ctl.focusOn(vehicle.x, vehicle.y, 0);
    ctl.setMode("map", true);
    // The plan view's focus is clamped into the world, so read the real start pose rather than
    // assuming the requested one.
    const start = ctl.camera.position.clone();
    expect(start.z).toBeGreaterThan(1600);
    ctl.follow(1);
    ctl.setFollowPose(vehicle.x, vehicle.y, 0, 0, 0);
    ctl.setMode("chase");
    const path: Vector3[] = [];
    for (let i = 0; i < frames; i++) {
      ctl.update(step);
      path.push(ctl.camera.position.clone());
    }
    return { ctl, path, start, vehicle, world };
  }

  it("travels continuously instead of snapping, and lands within its budget", () => {
    const { ctl, path, start, vehicle, world } = fly(1 / 60, 240);
    let maxJump = 0;
    let prev = start;
    for (const p of path) {
      maxJump = Math.max(maxJump, p.distanceTo(prev));
      prev = p;
    }
    // The bare exponential lerp starts at λ·distance — 4 × 1,700 m/s, which is 113 m in one 60 Hz
    // frame and was measured at 305 m over the first three frames of the real fly-down. An eased
    // 2.4 s descent peaks at 1.875·D/T, about 24 m per frame, in the middle rather than at the
    // start.
    expect(maxJump).toBeLessThan(30);
    // Eased in: the first frame is a move, not a jump.
    expect(path[0].distanceTo(start)).toBeLessThan(1);
    // Eased out: the last frames are still, not asymptotically creeping.
    expect(path[239].distanceTo(path[200])).toBeLessThan(0.5);
    // And it has arrived: 2.4 s is 144 frames.
    expect(ctl.camera.position.z).toBeLessThan(12);
    expect(Math.hypot(ctl.camera.position.x - vehicle.x, ctl.camera.position.y - vehicle.y)).toBeLessThan(15);
    world.dispose();
  });

  it("is frame-rate independent: one 0.2 s step lands where two 0.1 s steps do", () => {
    const a = fly(0.2, 12);
    const b = fly(0.1, 24);
    expect(a.ctl.camera.position.x).toBeCloseTo(b.ctl.camera.position.x, 6);
    expect(a.ctl.camera.position.y).toBeCloseTo(b.ctl.camera.position.y, 6);
    expect(a.ctl.camera.position.z).toBeCloseTo(b.ctl.camera.position.z, 6);
    a.world.dispose();
    b.world.dispose();
  });

  it("does not let street-level fog erase the city on the way down", () => {
    const viewer = makeViewer({ autoFrameActors: false });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(40, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.cameras.focusOn(480, 480, 0);
    viewer.cameras.altitudeM = 1700;
    viewer.cameras.setMode("map", true);
    viewer.renderFrame(16);

    viewer.flyTo(stream.poses.actorId[0], "chase");
    let t = 16;
    let minFarWhileHigh = Infinity;
    let highestAltitudeSeen = 0;
    for (let i = 0; i < 200; i++) {
      t += 1000 / 60;
      viewer.renderFrame(t);
      const fog = viewer.scene.fog as { far?: number } | null;
      const height = viewer.camera.position.z;
      if (height > 200) {
        highestAltitudeSeen = Math.max(highestAltitudeSeen, height);
        minFarWhileHigh = Math.min(minFarWhileHigh, fog?.far ?? Infinity);
      }
    }
    expect(highestAltitudeSeen).toBeGreaterThan(500);
    // A fog total at 1,200 m, applied from a camera 1,400 m up, is a grey wipe over the middle of
    // the transition — the captured frames showed exactly that. The cue has to follow the camera.
    expect(minFarWhileHigh).toBeGreaterThan(5_000);

    // And at the bottom it is the authored street-level cue again.
    const settled = viewer.scene.fog as { near: number; far: number };
    expect(viewer.camera.position.z).toBeLessThan(12);
    expect(settled.far).toBeLessThan(2_000);
    viewer.dispose();
  });
});

describe("the depth cue is applied only when it changes", () => {
  /** Count writes to `scene.fog` over `frames` steady frames in `mode`. */
  function fogWrites(mode: "map" | "chase", frames: number): number {
    const viewer = makeViewer({ autoFrameActors: false });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(20, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);
    if (mode === "chase") {
      viewer.flyTo(stream.poses.actorId[0], "chase", true);
      for (let i = 0; i < 90; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));
    } else {
      viewer.setCameraMode("map", true);
      for (let i = 0; i < 30; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));
    }
    let writes = 0;
    let held: unknown = viewer.scene.fog;
    Object.defineProperty(viewer.scene, "fog", {
      configurable: true,
      get: () => held,
      set: (v: unknown) => { writes++; held = v; },
    });
    let t = 3000;
    for (let i = 0; i < frames; i++) { t += 1000 / 60; viewer.renderFrame(t); }
    // Read the count *before* disposing: `dispose` clears the fog, which is a write of its own.
    const total = writes;
    viewer.dispose();
    return total;
  }

  it("does not touch scene.fog on every frame of a steady plan view", () => {
    // `scene.fog` going null-to-non-null recompiles every material that reads it, so a steady
    // camera must not be writing it at all. The guard used a bare `< scale × 0.02`, which for the
    // plan view's scale of 0 is `|0 − 0| < 0` — false — so it re-applied on all 120 frames.
    expect(fogWrites("map", 120)).toBe(0);
  });

  it("does not touch scene.fog on every frame of a settled chase view", () => {
    expect(fogWrites("chase", 120)).toBe(0);
  });
});

describe("the viewer supplies a subject rather than a meaningless view", () => {
  it("adopts the vehicle nearest the plan view when chase is asked for cold", () => {
    const viewer = makeViewer({ autoFrameActors: false });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(60, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);
    expect(viewer.cameras.followActorId).toBeNull();

    expect(viewer.setCameraMode("chase")).toBe("chase");
    const adopted = viewer.cameras.followActorId;
    expect(adopted).not.toBeNull();
    expect(viewer.selectedActorId).toBe(adopted);

    // It is a real vehicle, and the camera settles behind it rather than in a random block.
    for (let i = 0; i < 240; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));
    const slot = viewer.followSlot;
    expect(slot).toBeGreaterThanOrEqual(0);
    const ax = viewer.interpolator.outPosition[slot * 3];
    const ay = viewer.interpolator.outPosition[slot * 3 + 1];
    expect(Math.hypot(viewer.camera.position.x - ax, viewer.camera.position.y - ay)).toBeLessThan(20);
    viewer.dispose();
  });

  it("refuses chase when the stream has no vehicle at all, and says which mode it refused", () => {
    const viewer = makeViewer({ autoFrameActors: false });
    viewer.setWorld(grid.world);
    viewer.renderFrame(16);
    expect(viewer.setCameraMode("chase")).toBe("map");
    expect(viewer.cameras.mode).toBe("map");
    expect(viewer.cameras.rejectedMode).toBe("chase");
    viewer.dispose();
  });
});

describe("the followed vehicle is skipped only when the camera is inside it", () => {
  /** Step the viewer to a settled pose in `mode` following the first live actor. */
  function settled(mode: "chase" | "dashboard" | "free"): Viewer {
    const viewer = makeViewer({ autoFrameActors: false });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(40, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);
    viewer.flyTo(stream.poses.actorId[0], mode === "free" ? "dashboard" : mode, true);
    for (let i = 0; i < 60; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));
    if (mode === "free") {
      // `free` seeds from wherever the camera is — the driver's seat.
      viewer.setCameraMode("free", true);
      for (let i = 0; i < 30; i++) viewer.renderFrame(1200 + (i + 1) * (1000 / 60));
    }
    return viewer;
  }

  it("skips it from the driver's seat and from a free camera seeded there", () => {
    for (const mode of ["dashboard", "free"] as const) {
      const viewer = settled(mode);
      expect(viewer.cameras.mode).toBe(mode);
      expect(viewer.actors.hiddenActorId).toBe(viewer.cameras.followActorId);
      viewer.dispose();
    }
  });

  it("draws it in a chase view, where the camera is behind the car and not in it", () => {
    const viewer = settled("chase");
    expect(viewer.cameras.mode).toBe("chase");
    expect(viewer.actors.hiddenActorId).toBe(-1);
    viewer.dispose();
  });
});

describe("the plan view opens on the traffic", () => {
  it("centres on the vehicles once, keeping the opening zoom", () => {
    const viewer = makeViewer();
    viewer.setWorld(grid.world);
    const b = grid.world.bbox;
    // The opening framing is the world's centre, as `setWorld` leaves it.
    viewer.cameras.fitExtent(400);
    viewer.cameras.focusOn((b.minXM + b.maxXM) / 2, (b.minYM + b.maxYM) / 2, 0);
    const openExtent = viewer.cameras.extentM;

    // Traffic confined to one corner, as a scenario's single vehicle is.
    const stream = new SyntheticStream(24, grid);
    stream.keyframe();
    for (let i = 0; i < stream.poses.count; i++) {
      if (stream.poses.occupied[i] !== 1) continue;
      stream.poses.positions[i * 3] = b.maxXM - 40 - (i % 4) * 3;
      stream.poses.positions[i * 3 + 1] = b.maxYM - 40 - ((i >> 2) % 4) * 3;
    }
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);

    const f = viewer.liveActorFraming();
    expect(f).not.toBeNull();
    expect(viewer.cameras.target.x).toBeGreaterThan((b.minXM + b.maxXM) / 2);
    expect(viewer.cameras.target.y).toBeGreaterThan((b.minYM + b.maxYM) / 2);
    // The zoom is untouched: only the centre moved.
    expect(viewer.cameras.extentM).toBeCloseTo(openExtent, 6);
    viewer.dispose();
  });

  it("still centres when the traffic arrives while the camera is at street level", () => {
    const viewer = makeViewer();
    viewer.setWorld(grid.world);
    const b = grid.world.bbox;
    viewer.cameras.fitExtent(400);

    // A restored camera mode, or a copilot's `view.camera`, puts the camera at street level before
    // any pose has arrived. There is no second `setWorld` after this, so the one-shot recentre has
    // to survive being asked for while the plan view is not on screen.
    viewer.cameras.follow(1);
    viewer.cameras.setFollowPose(0, 0, 0, 0, 0);
    expect(viewer.setCameraMode("chase", true)).toBe("chase");
    for (let i = 0; i < 60; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));

    // Now the traffic arrives, in one corner.
    const stream = new SyntheticStream(24, grid);
    stream.keyframe();
    for (let i = 0; i < stream.poses.count; i++) {
      if (stream.poses.occupied[i] !== 1) continue;
      stream.poses.positions[i * 3] = b.maxXM - 40;
      stream.poses.positions[i * 3 + 1] = b.maxYM - 40;
    }
    viewer.capture(stream.poses, 1.2);
    for (let i = 0; i < 60; i++) viewer.renderFrame(1200 + (i + 1) * (1000 / 60));

    // Drop the follow and put the focus back at the world's centre — what a stream resync's
    // `setWorld` does. Following a vehicle would drag the focus along on its own and hide whether
    // the recentre ever ran.
    viewer.select(null);
    viewer.cameras.follow(null);
    viewer.cameras.focusOn((b.minXM + b.maxXM) / 2, (b.minYM + b.maxYM) / 2, 0);

    // Coming back to the plan view must find the traffic, not the world's empty middle.
    viewer.setCameraMode("map");
    for (let i = 0; i < 300; i++) viewer.renderFrame(3000 + (i + 1) * (1000 / 60));
    expect(viewer.cameras.target.x).toBeGreaterThan((b.minXM + b.maxXM) / 2 + 50);
    expect(viewer.cameras.target.y).toBeGreaterThan((b.minYM + b.maxYM) / 2 + 50);
    viewer.dispose();
  });

  it("brings the focus back when something else moves it, not just once at the start", () => {
    const viewer = makeViewer();
    viewer.setWorld(grid.world);
    const b = grid.world.bbox;
    viewer.cameras.fitExtent(400);
    const centre = { x: (b.minXM + b.maxXM) / 2, y: (b.minYM + b.maxYM) / 2 };
    const stream = new SyntheticStream(24, grid);
    stream.keyframe();
    for (let i = 0; i < stream.poses.count; i++) {
      if (stream.poses.occupied[i] !== 1) continue;
      stream.poses.positions[i * 3] = b.maxXM - 40;
      stream.poses.positions[i * 3 + 1] = b.maxYM - 40;
    }
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);
    expect(viewer.cameras.target.x).toBeGreaterThan(centre.x + 50);

    // Now something puts the focus back on the world's centre with nothing to re-arm a one-shot —
    // which is what a stream resync's `setWorld` does, and what left the only vehicle off the top
    // of the frame again with no way back.
    viewer.cameras.focusOn(centre.x, centre.y, 0);
    let t = 16;
    for (let i = 0; i < 90; i++) { t += 1000 / 60; viewer.renderFrame(t); }
    expect(viewer.cameras.target.x).toBeGreaterThan(centre.x + 50);
    expect(viewer.cameras.target.y).toBeGreaterThan(centre.y + 50);
    viewer.dispose();
  });

  it("does not cut the flight back from chase to the map", () => {
    const viewer = makeViewer();
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(24, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.flyTo(stream.poses.actorId[0], "chase", true);
    for (let i = 0; i < 60; i++) viewer.renderFrame(16 + (i + 1) * (1000 / 60));
    // Re-arm the recentre, which used to `snap()` on the next frame and end the flight instantly.
    viewer.setWorld(grid.world);
    viewer.setCameraMode("map");
    const low = viewer.camera.position.z;
    viewer.renderFrame(2000);
    expect(viewer.camera.position.z - low).toBeLessThan(50);
    expect(viewer.cameras.isFlying).toBe(true);
    viewer.dispose();
  });

  it("does not move a camera the user has already moved", () => {
    const viewer = makeViewer();
    viewer.setWorld(grid.world);
    // A drag, as `attachInput` reports it.
    const events: Record<string, (e: Event) => void> = {};
    viewer.cameras.attachInput({
      addEventListener: (t: string, fn: unknown) => { events[t] = fn as (e: Event) => void; },
      removeEventListener: () => undefined,
    });
    events.pointerdown?.({ clientX: 10, clientY: 10, shiftKey: false, button: 0 } as unknown as Event);
    expect(viewer.cameras.userHasMoved).toBe(true);

    viewer.cameras.focusOn(100, 100, 0);
    const stream = new SyntheticStream(24, grid);
    stream.keyframe();
    viewer.capture(stream.poses, 0);
    viewer.renderFrame(16);
    // Untouched but for the world clamp, which is not a recentre on the traffic.
    expect(viewer.cameras.target.x).toBeLessThan(400);
    viewer.dispose();
  });
});
