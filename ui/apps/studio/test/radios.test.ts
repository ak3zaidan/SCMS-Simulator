/**
 * The node table follows the stream: radios that spawn after t = 0 are known, radios that leave are
 * not counted, and the inspector's "radios" is that table.
 *
 * Two defects the wave A integrator saw, one cause. The page filled its node table from `Hello` and
 * nothing else, and `Hello` names the radios that existed when the connection opened (§3.1.3):
 *
 *  * a vehicle that spawned later had no row, so the inspector printed "actor id n/a" for it;
 *  * the inspector's radio count was `run.status.nodes`, a polled engine figure that counts only
 *    vehicles, so it read 178 beside a stream carrying 191 radios (the thirteen roadside units).
 *
 * A `Delta` spawn row names the node it carries (§3.4.5) and a keyframe says which actors exist
 * (§3.3), so the table can follow the stream, and now does.
 */

import { beforeEach, describe, expect, it } from "vitest";

import type { DeltaMessage } from "@vwp/protocol";

import { StudioEngine } from "../src/state/engine.js";
import { useStudio } from "../src/state/store.js";
import { addNode } from "./support/fakes.js";

const NONE = 0xffffffff;

function delta(
  spawns: { slot: number; actor: number; node: number }[],
  despawns: number[] = [],
): Pick<DeltaMessage, "spawns" | "despawns"> {
  return {
    spawns: {
      count: spawns.length,
      slot: Uint32Array.from(spawns.map((s) => s.slot)),
      actorId: Uint32Array.from(spawns.map((s) => s.actor)),
      nodeId: Uint32Array.from(spawns.map((s) => s.node)),
      classIdx: Uint8Array.from(spawns.map(() => 0)),
    } as unknown as DeltaMessage["spawns"],
    despawns: {
      count: despawns.length,
      slot: Uint32Array.from(despawns),
    } as unknown as DeltaMessage["despawns"],
  };
}

describe("the node table follows the stream", () => {
  let engine: StudioEngine;
  beforeEach(() => {
    engine = new StudioEngine();
    useStudio.setState({ radios: 0 });
    // What Hello said: two roadside units and one vehicle.
    addNode(engine, 0, null);
    addNode(engine, 1, null);
    addNode(engine, 5, 40);
  });

  it("knows the actor of a radio that spawned after the connection opened", () => {
    engine.handleDelta(delta([{ slot: 3, actor: 900, node: 57 }]));
    expect(engine.nodes.get(57)?.actorId).toBe(900);
    expect(engine.nodeByActor.get(900)).toBe(57);
  });

  it("does not add a row for an unequipped vehicle", () => {
    engine.handleDelta(delta([{ slot: 4, actor: 901, node: NONE }]));
    expect(engine.nodes.size).toBe(3);
    expect(engine.nodeByActor.has(901)).toBe(false);
  });

  it("counts the radios the stream carries, roadside units included, as they come and go", () => {
    engine.flushProjection();
    expect(useStudio.getState().radios).toBe(3);
    engine.handleDelta(delta([{ slot: 3, actor: 900, node: 57 }, { slot: 4, actor: 902, node: 58 }]));
    engine.flushProjection();
    expect(useStudio.getState().radios).toBe(5);
    engine.handleDelta(delta([], [3]));
    engine.flushProjection();
    expect(useStudio.getState().radios).toBe(4);
    expect(engine.nodes.has(57)).toBe(false);
    expect(engine.nodeByActor.has(900)).toBe(false);
  });

  it("a keyframe drops the radios of actors that are no longer there", () => {
    engine.handleDelta(delta([{ slot: 3, actor: 900, node: 57 }]));
    // Slot 0 holds actor 40 (node 5); actor 900 has gone.
    engine.handleKeyframe({
      actors: { count: 2, actorId: Uint32Array.from([40, NONE]) } as unknown as Parameters<StudioEngine["handleKeyframe"]>[0]["actors"],
    });
    expect([...engine.nodes.keys()].sort((a, b) => a - b)).toEqual([0, 1, 5]);
    // A despawn row for a slot the keyframe filled is traced through it.
    engine.handleDelta(delta([], [0]));
    expect(engine.nodes.has(5)).toBe(false);
  });
});
