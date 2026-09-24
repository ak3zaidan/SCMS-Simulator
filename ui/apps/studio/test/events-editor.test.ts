/**
 * The event editor's "pick on the map": a click becomes the street under it.
 *
 * `nearestEdge` is what turns the viewport's ground point into a closure target. It has to skip
 * what a closure cannot close — junction connectors, footways, crossings — or a click at a corner
 * would close a turn instead of a street.
 */

import { describe, expect, it } from "vitest";

import { nearestEdge } from "../src/lib/events.js";

/** Three lanes along y = 0, y = 10 and y = 20; the middle one is a junction connector. */
function world(): Parameters<typeof nearestEdge>[0] {
  const lanes = {
    count: 3,
    laneId: Uint32Array.from([100, 101, 102]),
    pointOff: Uint32Array.from([0, 2, 4]),
    pointCount: Uint32Array.from([2, 2, 2]),
    edgeId: Uint32Array.from([7, 8, 9]),
    junctionId: Uint32Array.from([0xffffffff, 3, 0xffffffff]),
    strName: Uint32Array.from([1, 0, 2]),
    widthM: new Float32Array(3),
    speedLimitMps: new Float32Array(3),
    allowedClasses: new Uint16Array(3),
    laneType: Uint8Array.from([0, 5, 0]),
    indexInEdge: new Uint8Array(3),
  };
  const lanePoints = {
    count: 6,
    x: Float32Array.from([0, 100, 0, 100, 0, 100]),
    y: Float32Array.from([0, 0, 10, 10, 20, 20]),
    z: new Float32Array(6),
  };
  const strings = ["", "Main Street", "Side Street"];
  return { lanes, lanePoints, str: (id: number) => strings[id] ?? "" } as unknown as Parameters<typeof nearestEdge>[0];
}

describe("nearestEdge", () => {
  it("finds the street under a click, with its name", () => {
    expect(nearestEdge(world(), 50, 1)).toEqual({ edge: 7, name: "Main Street", distanceM: 1 });
    expect(nearestEdge(world(), 50, 19)?.edge).toBe(9);
  });

  it("never picks a junction connector, even when it is the nearest lane", () => {
    const hit = nearestEdge(world(), 50, 11);
    expect(hit?.edge).toBe(9);
    expect(hit?.distanceM).toBeCloseTo(9, 5);
  });
});
