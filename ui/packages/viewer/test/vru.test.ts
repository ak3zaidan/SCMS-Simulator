/**
 * Pedestrians and cyclists as the viewer draws them.
 *
 * Two promises. Up close each is drawn as itself — a person on foot, or a rider on a bicycle with
 * two wheels under them — and not as a generic block. From map altitude each is a dot in the same
 * state colours as a vehicle's, but smaller ({@link VRU_MARK_SCALE}), so a crowd on a pavement
 * reads differently from traffic on the road and the legend can say so.
 */

import { Box3 } from "three";
import { describe, expect, it } from "vitest";

import { DEFAULT_ACTOR_CLASSES } from "../src/actors.js";
import { buildActorGeometry, isRiddenVru } from "../src/geometry.js";
import { VRU_MARK_SCALE } from "../src/overlays.js";
import { Viewer } from "../src/scene.js";
import type { ViewerCanvas } from "../src/types.js";
import { NullRenderer } from "./support/null-renderer.js";
import { makeGridWorld } from "./support/fixture.js";
import { placedPoses } from "./support/one-actor.js";

const WIDTH = 1600;
const HEIGHT = 900;
const CANVAS = { width: WIDTH, height: HEIGHT, clientWidth: WIDTH, clientHeight: HEIGHT } as unknown as ViewerCanvas;

function classNamed(name: string) {
  const d = DEFAULT_ACTOR_CLASSES.find((c) => c.name === name);
  if (!d) throw new Error(`no default class ${name}`);
  return d;
}

function bounds(name: string): Box3 {
  const g = buildActorGeometry(classNamed(name), 0);
  g.computeBoundingBox();
  const b = g.boundingBox;
  if (!b) throw new Error("no bounding box");
  return b;
}

describe("a pedestrian and a cyclist are drawn as themselves", () => {
  it("draws a person as tall as the class says and no longer than a stride", () => {
    const def = classNamed("pedestrian");
    const b = bounds("pedestrian");
    expect(b.max.z, "the figure's head is at the class height").toBeGreaterThan(def.heightM * 0.9);
    expect(b.max.z).toBeLessThan(def.heightM * 1.1);
    expect(b.max.x - b.min.x, "a person is not a car-length slab").toBeLessThan(0.6);
  });

  it("draws a cyclist on a bicycle: wheels at the ground, a bicycle's length, a rider above", () => {
    expect(isRiddenVru("bicycle")).toBe(true);
    expect(isRiddenVru("pedestrian")).toBe(false);
    const def = classNamed("bicycle");
    const b = bounds("bicycle");
    // Its wheels touch the road, and it is as long as the bicycle, not as a person.
    expect(b.min.z).toBeLessThan(0.05);
    expect(b.max.x - b.min.x).toBeGreaterThan(def.lengthM * 0.85);
    // The rider's head is well above the wheels, at about the class height.
    expect(b.max.z).toBeGreaterThan(def.heightM * 0.85);
    // And it is a different figure from the pedestrian's.
    const ped = buildActorGeometry(classNamed("pedestrian"), 0);
    const bike = buildActorGeometry(def, 0);
    expect(bike.getAttribute("position").count).not.toBe(ped.getAttribute("position").count);
  });
});

describe("a pedestrian's aerial mark is smaller than a vehicle's, in the same colours", () => {
  it("draws the pedestrian's dot at VRU_MARK_SCALE of the car's", () => {
    const viewer = new Viewer({
      canvas: CANVAS, theme: "dark", autoStart: false, shadows: false,
      createRenderer: (canvas) => new NullRenderer(canvas),
    });
    viewer.setWorld(makeGridWorld({ blocks: 12, blockM: 120 }).world);
    const car = DEFAULT_ACTOR_CLASSES.findIndex((c) => c.name === "car");
    const ped = DEFAULT_ACTOR_CLASSES.findIndex((c) => c.name === "pedestrian");
    const poses = placedPoses([
      { actorId: 1, x: 0, y: 61.65, classIdx: car },
      { actorId: 2, x: 60, y: 61.65, classIdx: ped, speedMps: 1.3 },
    ]);
    viewer.capture(poses);
    viewer.capture(poses);
    viewer.cameras.setMode("map", true);
    viewer.cameras.focusOn(30, 61.65, 0);
    viewer.cameras.altitudeM = 1600;
    viewer.cameras.snap();
    for (let i = 0; i < 90; i++) viewer.step(1 / 60);
    const dot = viewer.overlays.locators.group.children.find((c) => c.name.includes("locator-dot")) as unknown as {
      count: number;
      instanceMatrix: { array: Float32Array };
    };
    expect(dot.count, "both are dots at map altitude").toBe(2);
    const m = dot.instanceMatrix.array;
    const radiusAt = (x: number): number => {
      for (let i = 0; i < dot.count; i++) {
        if (Math.abs(m[i * 16 + 12] - x) < 1) return m[i * 16];
      }
      throw new Error(`no dot at x = ${x}`);
    };
    expect(radiusAt(60) / radiusAt(0)).toBeCloseTo(VRU_MARK_SCALE, 3);
    viewer.dispose();
  });
});
