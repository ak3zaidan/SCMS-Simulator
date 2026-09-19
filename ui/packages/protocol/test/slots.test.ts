/** §3.3.1 — the actor-slot model, and the §10.3 Q4/Q5 conformance items. */

import { describe, expect, it } from "vitest";

import { EMPTY_SLOT, SENTINEL_U32, SlotTable } from "../src/index.js";

describe("§3.3.1 — slot assignment", () => {
  it("assigns the lowest free slot, deterministically", () => {
    const t = new SlotTable(4);
    expect(t.assign(100)).toBe(0);
    expect(t.assign(101)).toBe(1);
    expect(t.assign(102)).toBe(2);
    expect(t.count).toBe(3);
    expect(t.live).toBe(3);
    expect(t.assign(101)).toBe(1); // idempotent for an actor already placed
  });

  it("indexes densely: the actor_id column is slot-indexed with 0xFFFFFFFF for empty slots", () => {
    const t = new SlotTable(4);
    t.assign(100);
    t.assign(101);
    t.assign(102);
    t.release(1);
    const col = t.actorIdColumn();
    expect(col.length).toBe(3); // high-water mark + 1, per §3.3.1
    expect(Array.from(col)).toEqual([100, EMPTY_SLOT, 102]);
    expect(EMPTY_SLOT).toBe(SENTINEL_U32);
  });

  it("§10.3 Q5 — a slot is not reused until one full keyframe period after despawn", () => {
    const t = new SlotTable(4);
    t.onKeyframe(0);
    t.assign(100);
    t.assign(101);
    t.release(0); // despawn during GOP 0

    // Still inside GOP 0: the slot must not come back, so a new actor goes above the high-water mark.
    expect(t.assign(102)).toBe(2);

    t.onKeyframe(1); // one full keyframe period has passed
    expect(t.assign(103)).toBe(0); // now the slot is reusable
    expect(t.actorIdOf(0)).toBe(103);
  });

  it("grows past its initial capacity", () => {
    const t = new SlotTable(2);
    for (let i = 0; i < 100; i++) expect(t.assign(1000 + i)).toBe(i);
    expect(t.capacity).toBeGreaterThanOrEqual(100);
    expect(t.count).toBe(100);
    expect(t.slotOf(1099)).toBe(99);
  });

  it("adopts a keyframe's column and then a delta's spawns", () => {
    const t = new SlotTable(8);
    t.adoptKeyframe(new Uint32Array([7, EMPTY_SLOT, 9]), 4);
    expect(t.count).toBe(3);
    expect(t.actorIdOf(0)).toBe(7);
    expect(t.isOccupied(1)).toBe(false);
    expect(t.slotOf(9)).toBe(2);
    expect(t.occupiedSlots()).toEqual([0, 2]);

    // A spawn names the slot the server chose; the client must take it, not choose its own.
    t.adoptSpawn(1, 11);
    expect(t.actorIdOf(1)).toBe(11);
    expect(t.occupiedSlots()).toEqual([0, 1, 2]);
    t.adoptSpawn(5, 12);
    expect(t.count).toBe(6);
    expect(t.actorIdOf(5)).toBe(12);
    expect(t.actorIdOf(4)).toBe(EMPTY_SLOT);
  });

  it("resets completely on a non-resumed Hello", () => {
    const t = new SlotTable(4);
    t.assign(1);
    t.assign(2);
    t.reset();
    expect(t.count).toBe(0);
    expect(t.live).toBe(0);
    expect(t.slotOf(1)).toBeUndefined();
    expect(t.assign(9)).toBe(0);
  });
});
