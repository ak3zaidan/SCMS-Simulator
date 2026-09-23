/**
 * Scene-correctness properties: the things that must be true of what is on screen, stated so that a
 * machine can check them.
 *
 * This is deliberately not a pixel diff against a golden image. A golden image goes red on every
 * legitimate change — a new road colour, a different building LOD — and says nothing about whether
 * the picture is *right*. What follows asserts properties instead: where the followed vehicle lands
 * in the frame, whether every live vehicle is drawn at map altitude, where the camera is allowed to
 * be, and whether the counters the interface prints agree with the scene they describe.
 *
 * Each `it` says in its name what must hold, and its body says why that is the right thing to want.
 * The rasterised half — "buildings are not the same colour as the sky" — cannot be checked without
 * a GPU and lives in `apps/studio/e2e/scene-validation.spec.ts`, which samples the real
 * framebuffer. What lives here is everything decidable from the scene graph and the camera.
 */

import { Vector3 } from "three";
import { describe, expect, it } from "vitest";

import { Viewer } from "../src/scene.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";
import { placedPoses } from "./support/one-actor.js";

const WIDTH = 1600;
const HEIGHT = 900;
const CANVAS = { width: WIDTH, height: HEIGHT, clientWidth: WIDTH, clientHeight: HEIGHT } as unknown as ViewerCanvas;

/**
 * The ground extent the Studio opens its map on, from `apps/studio/src/state/engine.ts`.
 *
 * Duplicated rather than imported: `@vwp/viewer` must not depend on the app that embeds it, and the
 * number is the *app's* choice of opening framing, which is exactly what these tests are about.
 */
const STUDIO_MAP_OPEN_EXTENT_M = 1400;

/**
 * The centre of the eastbound kerb lane of one street of `makeGridWorld({ blocks: 12 })`.
 *
 * Its streets sit on `axes[i] = -660 + 120 i`, so `axes[6] = 60`, and the first drive lane of the
 * `+x` direction is half a lane width off the centreline. A vehicle anywhere else is inside a city
 * block, where `CameraController.keepCameraOutsideBuildings` correctly lifts the chase camera onto
 * the roof — which would make every framing assertion below a measurement of a bug in the fixture.
 */
const LANE_Y = 60 + 1.65;

function makeViewer(): { viewer: Viewer; renderer: NullRenderer } {
  let renderer!: NullRenderer;
  const viewer = new Viewer({
    canvas: CANVAS,
    theme: "dark",
    autoStart: false,
    shadows: false,
    createRenderer: (canvas) => (renderer = new NullRenderer(canvas)),
  });
  return { viewer, renderer };
}

/** Run `n` frames of 1/60 s, which is what the loop would have done. */
function settle(viewer: Viewer, n = 180): void {
  for (let i = 0; i < n; i++) viewer.step(1 / 60);
}

/** Reproduce what `StudioEngine.#adoptWorld` does to the camera when a world arrives. */
function openAsStudioDoes(viewer: Viewer, bbox: { minXM: number; minYM: number; maxXM: number; maxYM: number }): void {
  const extent = Math.max(bbox.maxXM - bbox.minXM, bbox.maxYM - bbox.minYM);
  viewer.cameras.fitExtent(Math.min(extent * 1.05, STUDIO_MAP_OPEN_EXTENT_M));
  viewer.cameras.snap();
}

/** Where a world point lands on screen: NDC in [-1, 1] and pixels with y down from the top. */
function project(viewer: Viewer, x: number, y: number, z: number): {
  ndcX: number;
  ndcY: number;
  ndcZ: number;
  px: number;
  py: number;
  inFrame: boolean;
} {
  viewer.camera.updateMatrixWorld();
  const v = new Vector3(x, y, z).project(viewer.camera);
  return {
    ndcX: v.x,
    ndcY: v.y,
    ndcZ: v.z,
    px: ((v.x + 1) / 2) * WIDTH,
    py: ((1 - v.y) / 2) * HEIGHT,
    inFrame: Math.abs(v.x) <= 1 && Math.abs(v.y) <= 1 && v.z > -1 && v.z < 1,
  };
}

/** The slot a given actor id occupies in the interpolator's output, or −1. */
function slotOf(viewer: Viewer, actorId: number): number {
  const ids = viewer.interpolator.outActorId;
  const occ = viewer.interpolator.outOccupied;
  for (let i = 0; i < viewer.interpolator.count; i++) {
    if (occ[i] === 1 && ids[i] === (actorId >>> 0)) return i;
  }
  return -1;
}

function isVisibleSlot(viewer: Viewer, slot: number): boolean {
  const list = viewer.actors.visibleSlots;
  for (let i = 0; i < viewer.actors.visibleCount; i++) if (list[i] === slot) return true;
  return false;
}

// -----------------------------------------------------------------------------------------------
// 1. Every live vehicle is drawn at map altitude.
// -----------------------------------------------------------------------------------------------

describe("the aerial view draws the traffic", () => {
  /**
   * The owner's first report was "I don't really see any cars moving", on a run with exactly one
   * equipped vehicle in a three-kilometre world (`scenarios/phase1-manhattan.yaml`).
   *
   * This is that run's shape: a world whose bounding box is two kilometres across and one vehicle
   * a kilometre from its centre. The Studio opens the map focused on the *bounding box* centre at a
   * fixed 1,400 m extent, so the vehicle falls outside the frustum and the frustum test discards
   * it. Nothing is broken in the renderer; the view is simply pointed somewhere the traffic is not,
   * and the only visible symptom is an empty map.
   *
   * The property: when a run has live vehicles and the map view is the one the app opened on, every
   * live vehicle is drawn. An aerial view of a simulation that shows none of the simulation is
   * wrong however good its reasons.
   */
  it("draws the only vehicle in the run, even when it is far from the centre of the world", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 16, blockM: 120, buildingsPerBlock: 1 });
    viewer.setWorld(grid.world);
    openAsStudioDoes(viewer, grid.world.bbox);

    // In the eastbound lane of the street at y = 780 (`axes[14]` of a 16-block grid), a kilometre
    // from the bounding box centre — the same relationship phase1-manhattan's single vehicle has to
    // its own world.
    const poses = placedPoses([{ actorId: 7, x: 700, y: 781.65, headingRad: 0, speedMps: 9 }]);
    viewer.capture(poses);
    viewer.capture(poses);
    settle(viewer, 120);

    expect(viewer.cameras.mode).toBe("map");
    expect(viewer.actors.stats.live).toBe(1);
    expect(viewer.liveActorFraming()?.count).toBe(1);
    expect(viewer.actors.stats.drawn, "the one live vehicle is not drawn in the aerial view").toBe(1);
    viewer.dispose();
  });

  /**
   * The same property with a crowd, and with the framing the viewer itself offers
   * (`Viewer.frameActors`, which the `f` key is bound to). This is the control: it shows the
   * assertion above is about *where the camera points*, not about culling being broken.
   */
  it("draws all 200 vehicles when the map is framed on them", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 14, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(200, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    stream.advance(0.1);
    stream.delta();
    viewer.capture(stream.poses);
    viewer.frameActors();
    viewer.cameras.snap();
    settle(viewer, 120);

    const stats = viewer.actors.stats;
    expect(stats.live).toBe(200);
    expect(stats.drawn).toBe(200);
    expect(stats.culled).toBe(0);
    viewer.dispose();
  });

  /**
   * A vehicle mark is one instance, and the map must not double-draw or drop one. `visibleSlots` is
   * the list the overlays read, so a disagreement between it and the instance count would put a
   * state marker over empty road.
   */
  it("writes exactly one instance per drawn vehicle and one visible slot per instance", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(120, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.capture(stream.poses);
    viewer.frameActors();
    viewer.cameras.snap();
    settle(viewer, 60);

    expect(viewer.actors.visibleCount).toBe(viewer.actors.stats.drawn);
    const seen = new Set<number>();
    for (let i = 0; i < viewer.actors.visibleCount; i++) seen.add(viewer.actors.visibleSlots[i]);
    expect(seen.size).toBe(viewer.actors.visibleCount);
    viewer.dispose();
  });
});

// -----------------------------------------------------------------------------------------------
// 2. The chase view is behind a vehicle, and the vehicle is in the frame.
// -----------------------------------------------------------------------------------------------

describe("the chase view frames the vehicle it follows", () => {
  function chasing(): { viewer: Viewer; actorId: number; pos: [number, number, number] } {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    // In the eastbound lane of the street at y = 60 (`axes[6]` of a 12-block grid), heading +x, so
    // the geometry of the situation is known exactly — and on the road rather than inside a block,
    // which would send `keepCameraOutsideBuildings` up onto a roof and make the framing meaningless.
    const pos: [number, number, number] = [0, LANE_Y, 0];
    const poses = placedPoses([{ actorId: 42, x: pos[0], y: pos[1], z: pos[2], headingRad: 0, speedMps: 11 }]);
    viewer.capture(poses);
    viewer.capture(poses);
    viewer.cameras.flyTo(42, "chase", true);
    settle(viewer, 240);
    return { viewer, actorId: 42, pos };
  }

  /**
   * The owner's second report: "the camera is positioned weird and I don't really see any cars" —
   * two dark planes and a yellow line, no sense of being behind a vehicle.
   *
   * A chase view has one job: put the followed vehicle in the lower middle of the frame with the
   * road running away from it. So: the vehicle projects inside the frame, away from its edges, and
   * below the centre line — because a camera above and behind a car sees the car beneath the
   * horizon. If it projects off-screen, or above the centre, the camera is not behind the vehicle
   * whatever the mode says.
   */
  it("projects the followed vehicle into the lower middle of the frame", () => {
    const { viewer, pos } = chasing();
    const p = project(viewer, pos[0], pos[1], pos[2] + 0.7);

    expect(viewer.cameras.mode).toBe("chase");
    expect(p.inFrame, `followed vehicle projects off-screen at ndc ${p.ndcX.toFixed(2)}, ${p.ndcY.toFixed(2)}`).toBe(true);
    // Away from the edges: a vehicle half off the frame is not framed.
    expect(Math.abs(p.ndcX)).toBeLessThan(0.55);
    // Below the centre line, and not squashed into the very bottom edge.
    expect(p.py).toBeGreaterThan(HEIGHT * 0.5);
    expect(p.py).toBeLessThan(HEIGHT * 0.97);
    viewer.dispose();
  });

  /** And it is actually drawn: in chase view you are behind the car, so you can see the car. */
  it("draws the followed vehicle rather than hiding it", () => {
    const { viewer, actorId } = chasing();
    const slot = slotOf(viewer, actorId);
    expect(slot).toBeGreaterThanOrEqual(0);
    expect(viewer.actors.hiddenActorId).toBe(-1);
    expect(viewer.actors.stats.drawn).toBe(1);
    expect(isVisibleSlot(viewer, slot)).toBe(true);
    viewer.dispose();
  });

  /**
   * The dashboard view is the exception, and it must stay the exception: from the driver's seat the
   * camera is inside the body, so that one instance is deliberately not written. If this ever
   * stopped being mode-specific, chase view would lose its car — which is what `radios 0` taught us
   * to check for: an exception that silently widened.
   */
  it("hides the followed vehicle from the driver's seat, and only there", () => {
    const { viewer, actorId } = chasing();
    viewer.cameras.setMode("dashboard", true);
    settle(viewer, 60);
    expect(viewer.actors.hiddenActorId).toBe(actorId);
    expect(viewer.actors.stats.live).toBe(1);
    expect(viewer.actors.stats.drawn).toBe(0);

    viewer.cameras.setMode("chase", true);
    settle(viewer, 60);
    expect(viewer.actors.hiddenActorId).toBe(-1);
    expect(viewer.actors.stats.drawn).toBe(1);
    viewer.dispose();
  });

  /**
   * "Behind" is not a matter of taste: the camera must be on the opposite side of the vehicle from
   * its heading, above it, and near enough that the car is a car rather than a speck.
   */
  it("sits behind and above the vehicle, at a distance that reads as following it", () => {
    const { viewer, pos } = chasing();
    const s = viewer.cameras.state();
    const dx = s.position.x - pos[0];
    const dy = s.position.y - pos[1];
    // Heading 0 is +x, so "behind" is negative x.
    expect(dx).toBeLessThan(0);
    expect(Math.abs(dy)).toBeLessThan(3);
    expect(s.position.z - pos[2]).toBeGreaterThan(1);
    expect(s.position.z - pos[2]).toBeLessThan(15);
    const range = Math.hypot(dx, dy);
    expect(range).toBeGreaterThan(3);
    expect(range).toBeLessThan(40);
    viewer.dispose();
  });
});

// -----------------------------------------------------------------------------------------------
// 3. The camera is somewhere in the world, looking at what it says it is looking at.
// -----------------------------------------------------------------------------------------------

describe("the camera is where it claims to be", () => {
  it("keeps the map camera over the world and pointed at the ground", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 14, blockM: 120 });
    viewer.setWorld(grid.world);
    openAsStudioDoes(viewer, grid.world.bbox);
    settle(viewer, 60);

    const b = grid.world.bbox;
    const span = Math.max(b.maxXM - b.minXM, b.maxYM - b.minYM);
    const s = viewer.cameras.state();
    // Horizontally inside the world, allowing the framing's own half-extent as slack.
    expect(s.position.x).toBeGreaterThan(b.minXM - span);
    expect(s.position.x).toBeLessThan(b.maxXM + span);
    expect(s.position.y).toBeGreaterThan(b.minYM - span);
    expect(s.position.y).toBeLessThan(b.maxYM + span);
    // Above the tallest thing in it, and not in orbit: four world spans is already absurd.
    expect(s.position.z).toBeGreaterThan(b.maxZM);
    expect(s.position.z).toBeLessThan(span * 4);
    // Looking down at the ground it is framing, not at the horizon.
    expect(s.target.z).toBeLessThan(s.position.z);
    expect(Math.hypot(s.target.x - s.position.x, s.target.y - s.position.y)).toBeLessThan(span);
    viewer.dispose();
  });

  /**
   * In a follow mode the camera's target *is* the followed actor. If it drifts, the view swings for
   * no reason the viewer can explain, which is what "positioned weird" looks like from outside.
   */
  it("targets the followed actor, and reports that actor's id", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    const poses = placedPoses([{ actorId: 42, x: 58.35, y: 120, headingRad: Math.PI / 2, speedMps: 7 }]);
    viewer.capture(poses);
    viewer.capture(poses);
    viewer.cameras.flyTo(42, "chase", true);
    settle(viewer, 240);

    const slot = slotOf(viewer, 42);
    const p = slot * 3;
    const ax = viewer.interpolator.outPosition[p];
    const ay = viewer.interpolator.outPosition[p + 1];
    const s = viewer.cameras.state();
    expect(s.followActorId).toBe(42);
    expect(Math.hypot(s.target.x - ax, s.target.y - ay)).toBeLessThan(1.5);
    // The target rides at roughly eye height above the car, never below the road.
    expect(s.target.z).toBeGreaterThan(0);
    expect(s.target.z).toBeLessThan(4);
    viewer.dispose();
  });

  /**
   * A followed actor that leaves the run must not leave the camera pinned to the last place it was
   * seen with a stale follow id — the state the panel prints has to stop claiming a follow.
   */
  it("gives up the follow when the actor goes away", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    viewer.capture(placedPoses([{ actorId: 42, x: 0, y: LANE_Y }]));
    viewer.capture(placedPoses([{ actorId: 42, x: 1, y: LANE_Y }]));
    viewer.cameras.flyTo(42, "chase", true);
    settle(viewer, 60);
    expect(viewer.followSlot).toBeGreaterThanOrEqual(0);

    viewer.cameras.follow(null);
    settle(viewer, 60);
    expect(viewer.followSlot).toBe(-1);
    expect(viewer.cameras.state().followActorId).toBeNull();
    viewer.dispose();
  });
});

// -----------------------------------------------------------------------------------------------
// 4. Buildings are in front of the camera at street level.
// -----------------------------------------------------------------------------------------------

describe("buildings are part of the picture at street level", () => {
  /**
   * The unlit-buildings defect was found by looking at the top of the frame and seeing sky where
   * there should have been walls. Whether those walls are *lit* is a question for the framebuffer
   * and is asked in `apps/studio/e2e/scene-validation.spec.ts`; what can be settled here is whether
   * they are submitted at all and whether any of them is in front of the camera and above its
   * horizon. A street-level view with nothing above eye height is a view of an empty plain.
   */
  it("submits buildings and puts some of them above the horizon", () => {
    const { viewer, renderer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120, buildingsPerBlock: 2 });
    viewer.setWorld(grid.world);
    const poses = placedPoses([{ actorId: 1, x: 0, y: LANE_Y, headingRad: 0, speedMps: 8 }]);
    viewer.capture(poses);
    viewer.capture(poses);
    viewer.cameras.flyTo(1, "chase", true);
    settle(viewer, 240);

    expect(viewer.worldRenderer.buildingsVisible).toBeGreaterThan(0);
    expect(renderer.info.render.triangles).toBeGreaterThan(1000);

    // Count building roof corners that land in the upper half of the frame.
    const b = grid.world.buildings;
    const ring = grid.world.ringPoints;
    let aboveHorizon = 0;
    for (let i = 0; i < b.count; i++) {
      const off = b.ringOff[i];
      const n = b.ringCount[i];
      const topZ = b.baseZM[i] + b.heightM[i];
      for (let k = 0; k < n; k++) {
        const top = project(viewer, ring.x[off + k], ring.y[off + k], topZ);
        if (top.inFrame && top.py < HEIGHT * 0.5) aboveHorizon++;
      }
    }
    expect(aboveHorizon, "no building top is in the upper half of the street-level frame").toBeGreaterThan(0);
    viewer.dispose();
  });
});

// -----------------------------------------------------------------------------------------------
// 5. The counters agree with what they count.
// -----------------------------------------------------------------------------------------------

describe("the counters agree with the scene", () => {
  /**
   * `radios 0` beside `bytes_air 1468 B/s` is worse than a blank, because it is a claim. The
   * viewer's side of that class of defect is the drawn/live pair in the top bar: it comes from
   * `FrameStats.counters`, which `FrameStats.begin` zeroes at the top of every frame. If any path
   * through `renderFrame` returned before the counters were written, the read-out would print zero
   * while the scene was full — and only a test that compares the counter with its source would
   * notice.
   */
  it("reports the same drawn, culled and live numbers the actor renderer measured", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(150, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.frameActors();
    viewer.cameras.snap();

    for (let f = 0; f < 90; f++) {
      if (f % 6 === 0) {
        stream.advance(0.1);
        stream.delta();
        viewer.capture(stream.poses);
      }
      const report = viewer.step(1 / 60);
      const snap = viewer.stats.snapshot();
      const measured = viewer.actors.stats;
      expect(snap.actorInstances).toBe(measured.drawn);
      expect(snap.actorCulled).toBe(measured.culled);
      expect(snap.actorLive).toBe(measured.live);
      expect(report.actorsDrawn).toBe(measured.drawn);
      expect(report.actorsCulled).toBe(measured.culled);
      expect(snap.buildingsVisible).toBe(viewer.worldRenderer.buildingsVisible);
    }
    viewer.dispose();
  });

  /**
   * "Live" has to mean live. The counter is read off the interpolator's occupancy, and the only
   * thing that can make it right is comparing it with the occupancy the stream actually delivered.
   */
  it("counts as live exactly the occupied slots the stream delivered", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(77, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.capture(stream.poses);
    settle(viewer, 10);

    let occupied = 0;
    for (let i = 0; i < stream.poses.count; i++) if (stream.poses.occupied[i] === 1) occupied++;
    expect(occupied).toBe(77);
    expect(viewer.stats.snapshot().actorLive).toBe(occupied);

    // And with nothing on the wire it says nothing is live, rather than keeping the last number.
    viewer.interpolator.reset();
    settle(viewer, 10);
    expect(viewer.stats.snapshot().actorLive).toBe(0);
    expect(viewer.stats.snapshot().actorInstances).toBe(0);
    viewer.dispose();
  });

  /**
   * Drawn plus culled plus hidden plus dropped is live: the four outcomes are exhaustive, so a
   * vehicle that is neither drawn nor accounted for as skipped has gone missing silently. That is
   * the arithmetic that would have turned "no cars in the aerial view" from an opinion into a
   * number on the first look.
   */
  it("accounts for every live vehicle as drawn, culled, hidden or dropped", () => {
    const { viewer } = makeViewer();
    const grid = makeGridWorld({ blocks: 16, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(300, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.capture(stream.poses);

    for (const extent of [400, 1400, 4000]) {
      viewer.cameras.setMode("map", true);
      viewer.cameras.fitExtent(extent);
      viewer.cameras.snap();
      settle(viewer, 30);
      const s = viewer.actors.stats;
      const hidden = viewer.actors.hiddenActorId >= 0 ? 1 : 0;
      expect(s.drawn + s.culled + s.dropped + hidden, `extent ${extent} m loses vehicles`).toBe(s.live);
    }
    viewer.dispose();
  });
});
