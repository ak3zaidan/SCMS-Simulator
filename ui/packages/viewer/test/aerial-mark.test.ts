/**
 * The aerial vehicle mark, and the promise it makes: **what you can see is what you can click.**
 *
 * The owner's report was "I don't really see any cars moving", on an aerial view of a run with one
 * equipped vehicle. Driving the real page with Playwright established that nothing was broken in
 * the renderer at all — the numbers were a vehicle 3 × 1 pixels across at the altitude the Studio
 * opens its map on, and a click within three pixels of its own centre missing it, because the pick
 * tests the true 5.0 × 1.8 m body. A shape smaller than a pixel cannot be fixed with colour.
 *
 * So the properties below are about *screen size*, measured in pixels off the real projection
 * matrix, not about whether a draw call happened. Each one is written so that the measurement it
 * makes is the thing the owner was complaining about.
 */

import { Vector3 } from "three";
import { describe, expect, it } from "vitest";

import { Viewer } from "../src/scene.js";
import { ACTOR_STATE_COLOR_KEYS, actorColorKey } from "../src/actors.js";
import { DARK_THEME } from "../src/theme.js";
import { VEHICLE_MARK_ANGULAR_RADIUS, type ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { SyntheticStream, makeGridWorld } from "./support/fixture.js";
import { placedPoses } from "./support/one-actor.js";

const WIDTH = 1600;
const HEIGHT = 900;
const CANVAS = { width: WIDTH, height: HEIGHT, clientWidth: WIDTH, clientHeight: HEIGHT } as unknown as ViewerCanvas;

/** The altitude `apps/studio/src/state/engine.ts` opens its map on: a 1,400 m extent at 45°. */
const STUDIO_OPEN_ALTITUDE_M = 700 / Math.tan((45 * Math.PI) / 180 / 2);

function makeViewer(): Viewer {
  return new Viewer({
    canvas: CANVAS, theme: "dark", autoStart: false, shadows: false,
    createRenderer: (canvas) => new NullRenderer(canvas),
  });
}

function settle(viewer: Viewer, n = 90): void {
  for (let i = 0; i < n; i++) viewer.step(1 / 60);
}

/** Metres per screen pixel at `distanceM`, from the camera's own vertical field of view. */
function metresPerPixel(viewer: Viewer, distanceM: number): number {
  return (2 * distanceM * Math.tan((viewer.camera.fov * Math.PI) / 360)) / HEIGHT;
}

/** The instanced mark meshes, by the suffix of their name. */
function markMesh(viewer: Viewer, kind: "dot" | "ring" | "halo" | "stem"): {
  count: number;
  matrix: Float32Array;
  color: Float32Array | null;
} {
  const found = viewer.overlays.locators.group.children.find((c) => c.name.includes(`locator-${kind}`));
  if (!found) throw new Error(`no locator-${kind} mesh`);
  const mesh = found as unknown as {
    count: number;
    instanceMatrix: { array: Float32Array };
    instanceColor: { array: Float32Array } | null;
  };
  return { count: mesh.count, matrix: mesh.instanceMatrix.array, color: mesh.instanceColor?.array ?? null };
}

/** A lone vehicle at the centre of a grid world, viewed from `altitudeM` straight down. */
function oneVehicleFrom(altitudeM: number, opts: { state?: number } = {}): Viewer {
  const viewer = makeViewer();
  const grid = makeGridWorld({ blocks: 12, blockM: 120 });
  viewer.setWorld(grid.world);
  const poses = placedPoses([
    { actorId: 7, x: 0, y: 61.65, headingRad: 0, speedMps: 9, state: opts.state ?? 0 },
  ]);
  viewer.capture(poses);
  viewer.capture(poses);
  viewer.cameras.setMode("map", true);
  viewer.cameras.focusOn(0, 61.65, 0);
  viewer.cameras.altitudeM = altitudeM;
  viewer.cameras.snap();
  settle(viewer);
  return viewer;
}

describe("a vehicle is visible from map altitude", () => {
  /**
   * The measurement that turns "I don't see any cars" into a number: at the altitude the Studio
   * opens on, how many pixels across is the largest thing drawn for one vehicle?
   *
   * The default car is 4.5 × 1.8 m, and at this altitude one pixel is over a metre and a half of
   * street, so on its own it draws as a **three-pixel-long, one-pixel-wide smear** — measured at
   * 3 × 1 px on the real page, whose canvas is shorter than this fixture's. That is below the size
   * at which anti-aliasing leaves anything you could call a shape, let alone aim at. The mark has
   * to clear a genuine see-and-click target, which is what the floor here is.
   */
  it("draws a mark several pixels across where the vehicle itself is a one-pixel smear", () => {
    const viewer = oneVehicleFrom(STUDIO_OPEN_ALTITUDE_M);
    const mPerPx = metresPerPixel(viewer, STUDIO_OPEN_ALTITUDE_M);

    // What the vehicle alone would give you: a smear thinner than a line.
    expect(4.5 / mPerPx, "the fixture no longer reproduces a sub-pixel vehicle").toBeLessThan(4);
    expect(1.8 / mPerPx, "the fixture no longer reproduces a sub-pixel vehicle").toBeLessThan(1.5);

    const dot = markMesh(viewer, "dot");
    expect(dot.count, "no aerial mark for the one vehicle in the run").toBe(1);
    const radiusPx = dot.matrix[0] / mPerPx;
    expect(radiusPx, `mark is ${(radiusPx * 2).toFixed(1)} px across, which is not a visible dot`)
      .toBeGreaterThan(4);
    // And a halo behind it, so the dot reads on a pale basemap as well as a dark one.
    expect(markMesh(viewer, "halo").count).toBe(1);
    viewer.dispose();
  });

  /**
   * "Constant angular size, so it neither vanishes when you zoom out nor swamps the street when you
   * zoom in." Stated as a measurement: the mark's radius in *pixels* is the same number at 200 m and
   * at 3 km. A mark defined in metres would differ by a factor of fifteen between these two.
   */
  it("keeps the same pixel radius across a fifteenfold change of altitude", () => {
    const radii: number[] = [];
    for (const altitude of [200, 700, 3000]) {
      const viewer = oneVehicleFrom(altitude);
      const dot = markMesh(viewer, "dot");
      const ring = markMesh(viewer, "ring");
      const drawn = dot.count > 0 ? dot.matrix[0] : ring.matrix[0];
      expect(dot.count + ring.count, `nothing drawn for the vehicle at ${altitude} m`).toBeGreaterThan(0);
      radii.push(drawn / metresPerPixel(viewer, altitude));
      viewer.dispose();
    }
    const [near, mid, far] = radii;
    // The 200 m case is in the ring band, where the radius is held at least a third wider than the
    // body so the ring surrounds the vehicle rather than covering it — so it is compared loosely.
    expect(near).toBeGreaterThan(4);
    expect(mid / far, `pixel radius drifted: ${mid.toFixed(2)} px vs ${far.toFixed(2)} px`)
      .toBeCloseTo(1, 2);
  });

  /**
   * The other half of the same rule. From the chase camera the vehicle fills a good part of the
   * frame, and a mark on top of it hides the thing the view exists to show. The previous
   * implementation expressed this as "no mark within 45 m", which is the same intent with a
   * hard-coded distance; it is a ratio now, so it is right for a pedestrian and a bus too.
   */
  it("draws no mark at all once the vehicle is plainly visible by itself", () => {
    const viewer = oneVehicleFrom(30);
    expect(markMesh(viewer, "dot").count).toBe(0);
    expect(markMesh(viewer, "ring").count).toBe(0);
    expect(viewer.overlays.locators.drawn).toBe(0);
    // …and the vehicle itself is being drawn, so this is a mark decision and not an empty scene.
    expect(viewer.actors.stats.drawn).toBe(1);
    viewer.dispose();
  });

  /**
   * In between, the mark is a ring *around* the vehicle. The property that makes it a ring rather
   * than a lid: its radius is larger than the body's own, so the vehicle shows through the middle.
   */
  it("surrounds the vehicle rather than covering it at intermediate range", () => {
    const viewer = oneVehicleFrom(250);
    const ring = markMesh(viewer, "ring");
    expect(ring.count).toBe(1);
    expect(markMesh(viewer, "dot").count).toBe(0);
    // Half the body diagonal of the default 4.5 × 1.8 × 1.5 m car.
    const bodyRadiusM = Math.hypot(4.5, 1.8, 1.5) * 0.5;
    expect(ring.matrix[0]).toBeGreaterThan(bodyRadiusM);
    viewer.dispose();
  });
});

describe("what you can see is what you can click", () => {
  /**
   * The pick and the mark share {@link VEHICLE_MARK_ANGULAR_RADIUS}, so the clickable target should
   * be the drawn target. The test walks outwards a pixel at a time from the vehicle's own projected
   * centre and finds the last offset that still picks it.
   *
   * Before the shared constant this number was **1**: a scan of a ±30 px grid around the centre
   * returned exactly one hit, at dead centre, and two runs of the same click script — same script,
   * same altitude — hit once and missed once.
   */
  it("picks the vehicle anywhere within the mark drawn over it", () => {
    const viewer = oneVehicleFrom(STUDIO_OPEN_ALTITUDE_M);
    const dot = markMesh(viewer, "dot");
    expect(dot.count).toBe(1);
    const mPerPx = metresPerPixel(viewer, STUDIO_OPEN_ALTITUDE_M);
    const markRadiusPx = dot.matrix[0] / mPerPx;

    viewer.camera.updateMatrixWorld();
    const centre = new Vector3(dot.matrix[12], dot.matrix[13], 0).project(viewer.camera);
    const cx = ((centre.x + 1) / 2) * WIDTH;
    const cy = ((1 - centre.y) / 2) * HEIGHT;

    let reach = -1;
    for (let d = 0; d < Math.ceil(markRadiusPx) + 8; d++) {
      const hit = viewer.pickAtPixel(cx + d, cy);
      if (hit?.kind === "actor" && hit.actorId === 7) reach = d;
      else break;
    }
    expect(reach, "the vehicle is not pickable even at its own centre").toBeGreaterThanOrEqual(0);
    // The reach is the mark's own radius, give or take the pixel the loop steps in.
    expect(reach + 1).toBeGreaterThanOrEqual(Math.floor(markRadiusPx));
    // And it is a target, not the whole map: well outside the mark is the ground.
    const far = viewer.pickAtPixel(cx + markRadiusPx * 4, cy);
    expect(far?.kind).not.toBe("actor");
    viewer.dispose();
  });

  /**
   * The risk the slack creates, and the reason the tie-break is not "nearest along the ray".
   *
   * At map altitude the slack is ~12 m, so neighbouring vehicles' hit boxes overlap and one ray
   * enters several of them. Ranking those by entry distance is close to arbitrary: measured on the
   * real page at 200 vehicles from 1,860 m, clicking each vehicle's own projected centre selected a
   * *different* vehicle 5–14 m away **38 times out of 200**. Following the wrong car is worse than
   * following none, because nothing on screen says it happened.
   *
   * With a body hit beating a slack hit, and perpendicular distance deciding between slack hits,
   * the same measurement gives 199 of 200 — the one exception being a pair a metre apart, where
   * there is no right answer.
   */
  it("picks the vehicle you pointed at, not its neighbour, in dense traffic", () => {
    const viewer = makeViewer();
    const grid = makeGridWorld({ blocks: 14, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(200, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    stream.advance(0.1);
    stream.delta();
    viewer.capture(stream.poses);
    viewer.frameActors(240, 900);
    viewer.cameras.snap();
    settle(viewer, 120);
    viewer.camera.updateMatrixWorld();

    const ip = viewer.interpolator;
    const classes = viewer.actors.classes;
    /** Whether slot `j`'s body footprint covers the point `(x, y)` — it is *over* that vehicle. */
    const covers = (j: number, x: number, y: number): boolean => {
      const def = classes[Math.min(ip.outClassIdx[j], classes.length - 1)];
      const h = ip.outHeading[j];
      const dx = x - ip.outPosition[j * 3];
      const dy = y - ip.outPosition[j * 3 + 1];
      const lx = dx * Math.cos(h) + dy * Math.sin(h);
      const ly = -dx * Math.sin(h) + dy * Math.cos(h);
      return Math.abs(lx) <= def.lengthM / 2 && Math.abs(ly) <= def.widthM / 2;
    };

    let tested = 0;
    let exact = 0;
    const wrong: string[] = [];
    for (let i = 0; i < ip.count; i++) {
      if (ip.outOccupied[i] !== 1) continue;
      const x = ip.outPosition[i * 3];
      const y = ip.outPosition[i * 3 + 1];
      const z = ip.outPosition[i * 3 + 2];
      const v = new Vector3(x, y, z).project(viewer.camera);
      if (Math.abs(v.x) > 1 || Math.abs(v.y) > 1) continue;
      // The fixture parks a bicycle inside a 24 m rail vehicle's body. Looking straight down at
      // that point you see the rail vehicle, and selecting it is the right answer — so those
      // vehicles are not part of this question.
      let buried = false;
      for (let j = 0; j < ip.count && !buried; j++) {
        if (j !== i && ip.outOccupied[j] === 1 && covers(j, x, y)) buried = true;
      }
      if (buried) continue;
      tested++;
      const hit = viewer.pickAtPixel(((v.x + 1) / 2) * WIDTH, ((1 - v.y) / 2) * HEIGHT);
      if (hit?.kind !== "actor") continue;
      if (hit.actorId === ip.outActorId[i]) exact++;
      else {
        const s = hit.slot;
        const d = Math.hypot(ip.outPosition[s * 3] - x, ip.outPosition[s * 3 + 1] - y);
        // Two vehicles within a metre of each other is a genuine coincidence with no right answer.
        if (d >= 1.5) wrong.push(`${d.toFixed(1)} m away (class ${ip.outClassIdx[s]})`);
      }
    }
    expect(tested, "the fixture put no clickable vehicles in frame").toBeGreaterThan(100);
    expect(wrong, "clicks that selected a vehicle other than the one under the cursor").toEqual([]);
    expect(exact / tested, `only ${exact} of ${tested} clicks selected the vehicle clicked on`)
      .toBeGreaterThan(0.97);
    viewer.dispose();
  });

  /**
   * The slack is angular, so it must not leak into street level, where the exact body box is the
   * right answer and a metre of slop would let you select the car in the next lane.
   */
  it("leaves the street-level hit box the size of the vehicle", () => {
    const viewer = makeViewer();
    const grid = makeGridWorld({ blocks: 12, blockM: 120 });
    viewer.setWorld(grid.world);
    viewer.capture(placedPoses([{ actorId: 7, x: 0, y: 61.65, headingRad: 0 }]));
    viewer.capture(placedPoses([{ actorId: 7, x: 0, y: 61.65, headingRad: 0 }]));
    viewer.cameras.flyTo(7, "chase", true);
    settle(viewer, 240);

    const camDistance = viewer.camera.position.distanceTo(new Vector3(0, 61.65, 0.75));
    const slackM = camDistance * VEHICLE_MARK_ANGULAR_RADIUS;
    expect(slackM, `chase-range pick slack is ${slackM.toFixed(2)} m, which is a lane's worth`)
      .toBeLessThan(0.25);
    viewer.dispose();
  });
});

describe("the mark is legible for a fleet, not just for one", () => {
  /**
   * The previous implementation suppressed every mark once the run had more than 64 live vehicles,
   * so a 200-vehicle run drew none at any zoom — measured on the real page as
   * `drawn: 16, culled: 184, locators: 0`. The mark is never suppressed by a count now; only the
   * cosmetic halo is capped.
   */
  it("marks all two hundred vehicles from map altitude", () => {
    const viewer = makeViewer();
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

    expect(viewer.actors.stats.drawn).toBe(200);
    expect(viewer.overlays.locators.drawn, "vehicles drawn but not marked").toBe(200);
    expect(markMesh(viewer, "dot").count + markMesh(viewer, "ring").count).toBe(200);
    viewer.dispose();
  });

  /** Growth is amortised and stops: a settled fleet must not reallocate the meshes every frame. */
  it("grows its capacity once and then stops", () => {
    const viewer = makeViewer();
    const grid = makeGridWorld({ blocks: 14, blockM: 120 });
    viewer.setWorld(grid.world);
    const stream = new SyntheticStream(900, grid);
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.capture(stream.poses);
    viewer.frameActors();
    viewer.cameras.snap();
    settle(viewer, 60);
    const settled = viewer.overlays.locators.capacity;
    expect(settled).toBeGreaterThanOrEqual(viewer.overlays.locators.drawn);
    settle(viewer, 120);
    expect(viewer.overlays.locators.capacity).toBe(settled);
    viewer.dispose();
  });
});

describe("the mark's colour is the legend's colour", () => {
  /**
   * At map altitude the mark is the only thing on screen big enough to have a colour, so if it
   * painted its own the legend would be describing colours the aerial view never shows. Both sides
   * go through `actorStateColorIndex`; this asserts they come out the same, per state.
   *
   * `radios 0` is the shape of defect this guards against: two places computing what should be one
   * number, and only one of them kept up to date.
   */
  it("paints each state in exactly the colour the legend publishes", () => {
    const states: Array<{ bits: number; key: string }> = [
      { bits: 0, key: "benign" },
      { bits: 1 << 0, key: "attacker" },
      { bits: 1 << 1, key: "reported" },
      { bits: 1 << 2, key: "revoked" },
    ];
    for (const { bits, key } of states) {
      expect(actorColorKey(bits, false, true), `state bits ${bits} are not ${key}`).toBe(key);
      const viewer = oneVehicleFrom(STUDIO_OPEN_ALTITUDE_M, { state: bits });
      const dot = markMesh(viewer, "dot");
      expect(dot.count, `no mark for state ${key}`).toBe(1);
      expect(dot.color).not.toBeNull();

      const legend = viewer.actors.legend().find((e) => e.kind === "state" && e.key === key);
      expect(legend, `no legend row for ${key}`).toBeDefined();
      const expected = DARK_THEME.actorState[key as keyof typeof DARK_THEME.actorState];
      expect(legend?.color).toBe(expected);

      // `instanceColor` is linear-sRGB working space; convert the legend's packed sRGB the same way
      // `Color.setHex` does, which is what the renderer wrote.
      const srgb = [(expected >> 16) & 0xff, (expected >> 8) & 0xff, expected & 0xff].map((v) => v / 255);
      const linear = srgb.map((c) => (c < 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
      for (let i = 0; i < 3; i++) {
        expect(dot.color![i], `${key} channel ${i}`).toBeCloseTo(linear[i], 3);
      }
      viewer.dispose();
    }
    expect(ACTOR_STATE_COLOR_KEYS).toContain("selected");
  });

  /**
   * The structural half of the same property, and the one that would actually have caught the bug.
   *
   * The first version of the mark wrote every instance colour correctly and drew **black**, because
   * three applies an instance colour only inside `#ifdef USE_COLOR`; `USE_COLOR` comes from
   * `material.vertexColors`, and `vertexColors` without a `color` attribute makes WebGL supply the
   * missing attribute as `(0, 0, 0, 1)`, so `vColor.rgb *= color` zeroes it. On a dark basemap a
   * black dot is indistinguishable from no dot at all.
   *
   * The test above cannot see that: it reads `instanceColor`, which is written either way — the
   * shader is what ignores it. Injecting the fault left it green. So the invariant is asserted
   * where it lives, on the pair of objects, across *every* mesh in the scene that colours its
   * instances — not just this overlay's.
   */
  it("gives every per-instance-coloured mesh the colour attribute its shader needs", () => {
    const viewer = oneVehicleFrom(STUDIO_OPEN_ALTITUDE_M);
    const stream = new SyntheticStream(40, makeGridWorld({ blocks: 8, blockM: 120 }));
    stream.keyframe();
    viewer.capture(stream.poses);
    viewer.capture(stream.poses);
    for (const entry of viewer.overlays.catalogue()) viewer.overlays.set(entry.name, true);
    settle(viewer, 30);

    const checked: string[] = [];
    viewer.scene.traverse((node) => {
      const o = node as unknown as {
        isInstancedMesh?: boolean;
        name: string;
        instanceColor: unknown;
        geometry: { getAttribute(n: string): unknown };
        material: { vertexColors?: boolean };
      };
      if (o.isInstancedMesh !== true) return;
      const usesInstanceColor = o.instanceColor != null;
      const vertexColors = o.material.vertexColors === true;
      if (!usesInstanceColor && !vertexColors) return;
      checked.push(o.name);
      expect(
        vertexColors,
        `${o.name} writes instance colours but its material has vertexColors off, so three never reads them`,
      ).toBe(true);
      expect(
        o.geometry.getAttribute("color"),
        `${o.name} has vertexColors on and no colour attribute: WebGL supplies (0,0,0) for the ` +
          "missing attribute and every instance draws black",
      ).toBeDefined();
    });
    expect(checked.length, "no per-instance-coloured mesh was examined").toBeGreaterThan(0);
    viewer.dispose();
  });
});
