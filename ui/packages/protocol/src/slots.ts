/**
 * The actor-slot model — docs/protocol/vwp-v1.md §3.3.1 and Appendix D.2.
 *
 * > Slots are assigned by the server at spawn as the **lowest free slot** (deterministic), and are
 * > released **one full keyframe period** after despawn so that a late delta cannot be misapplied.
 * > Keyframe actor rows are a **dense array indexed by slot**; empty slots carry
 * > `actor_id = 0xFFFFFFFF` and zeros elsewhere.
 *
 * The client keeps this table to map slots to actor ids and back (selection, follow, labels); the
 * mock server uses the same class to assign them, which is what makes the two agree.
 */

import { ProtocolError, SENTINEL_U32 } from "./frame.js";
import { MAX_ACTOR_SLOTS } from "./pose.js";

/** §3.3.1 — the `actor_id` of an empty slot. */
export const EMPTY_SLOT = SENTINEL_U32;

/** A slot pending release, with the keyframe ordinal at which it may be reused. */
interface PendingRelease {
  readonly slot: number;
  readonly releaseAfterGop: number;
}

/**
 * Dense, deterministic actor-slot allocation.
 *
 * `assign` always returns the lowest free slot. `release` marks a slot for reuse **after** the next
 * keyframe boundary: call {@link onKeyframe} with each keyframe's `gop_index` and the table frees
 * the slots whose keyframe period has elapsed.
 */
export class SlotTable {
  #actorIdBySlot: Uint32Array;
  #slotByActor = new Map<number, number>();
  #free: number[] = [];
  #pending: PendingRelease[] = [];
  #highWater = 0;
  #gopIndex = -1;
  #slotBound = MAX_ACTOR_SLOTS;

  constructor(capacity = 1024) {
    this.#actorIdBySlot = new Uint32Array(Math.max(1, capacity)).fill(EMPTY_SLOT);
  }

  /** Allocated capacity; grows on demand. */
  get capacity(): number {
    return this.#actorIdBySlot.length;
  }

  /** Slot high-water mark + 1 — the `actor_count` a keyframe would carry (§3.3.1). */
  get count(): number {
    return this.#highWater;
  }

  /**
   * The highest slot id + 1 a wire frame may name (§3.4.5, §3.1.1), capped at
   * {@link MAX_ACTOR_SLOTS} and never below the allocated capacity. Seeded from
   * `Hello.actor_capacity` by {@link setSlotBound}.
   */
  get slotLimit(): number {
    return Math.max(this.#slotBound, this.#actorIdBySlot.length);
  }

  /**
   * Set the wire-slot bound from `Hello.actor_capacity` (§3.1.1). Clamped to
   * {@link MAX_ACTOR_SLOTS}; a later `Hello` replaces it, and {@link slotLimit} never drops below
   * the capacity already allocated.
   */
  setSlotBound(actorCapacity: number): void {
    if (!Number.isFinite(actorCapacity) || actorCapacity <= 0) return;
    this.#slotBound = Math.min(MAX_ACTOR_SLOTS, Math.floor(actorCapacity));
  }

  /** Number of slots currently holding a live actor. */
  get live(): number {
    return this.#slotByActor.size;
  }

  /** Grow the table, preserving contents. */
  ensureCapacity(capacity: number): void {
    if (capacity <= this.#actorIdBySlot.length) return;
    let next = this.#actorIdBySlot.length;
    while (next < capacity) next *= 2;
    const grown = new Uint32Array(next).fill(EMPTY_SLOT);
    grown.set(this.#actorIdBySlot, 0);
    this.#actorIdBySlot = grown;
  }

  /** Assign the lowest free slot to `actorId`. Returns the slot. */
  assign(actorId: number): number {
    const existing = this.#slotByActor.get(actorId);
    if (existing !== undefined) return existing;
    let slot: number;
    if (this.#free.length > 0) {
      this.#free.sort((a, b) => a - b);
      slot = this.#free.shift() as number;
    } else {
      slot = this.#highWater;
      this.#highWater += 1;
    }
    this.ensureCapacity(slot + 1);
    this.#actorIdBySlot[slot] = actorId;
    this.#slotByActor.set(actorId, slot);
    if (slot + 1 > this.#highWater) this.#highWater = slot + 1;
    return slot;
  }

  /**
   * Mark a slot despawned. The slot keeps its identity until one full keyframe period has passed
   * (§3.3.1), so a delta that was in flight cannot be applied to a different actor.
   */
  release(slot: number): void {
    const actorId = this.#actorIdBySlot[slot];
    if (actorId === EMPTY_SLOT) return;
    this.#slotByActor.delete(actorId);
    this.#actorIdBySlot[slot] = EMPTY_SLOT;
    this.#pending.push({ slot, releaseAfterGop: this.#gopIndex + 1 });
  }

  /** Announce a keyframe boundary; frees slots whose keyframe period has elapsed. */
  onKeyframe(gopIndex: number): void {
    this.#gopIndex = gopIndex;
    const stillPending: PendingRelease[] = [];
    for (const p of this.#pending) {
      if (gopIndex >= p.releaseAfterGop) this.#free.push(p.slot);
      else stillPending.push(p);
    }
    this.#pending = stillPending;
  }

  /** The actor in a slot, or `0xFFFFFFFF` if the slot is empty. */
  actorIdOf(slot: number): number {
    return slot < this.#actorIdBySlot.length ? this.#actorIdBySlot[slot] : EMPTY_SLOT;
  }

  /** The slot holding an actor, or `undefined`. */
  slotOf(actorId: number): number | undefined {
    return this.#slotByActor.get(actorId);
  }

  /** Is this slot currently occupied? */
  isOccupied(slot: number): boolean {
    return this.actorIdOf(slot) !== EMPTY_SLOT;
  }

  /** Slots holding a live actor, ascending. */
  occupiedSlots(): number[] {
    const out: number[] = [];
    for (let s = 0; s < this.#highWater; s++) if (this.#actorIdBySlot[s] !== EMPTY_SLOT) out.push(s);
    return out;
  }

  /** A dense `actor_id` column for slots `0..count-1`, as a keyframe carries it. */
  actorIdColumn(): Uint32Array {
    return this.#actorIdBySlot.slice(0, this.#highWater);
  }

  /** Rebuild the table from a keyframe's `actor_id` column (the client side of the model). */
  adoptKeyframe(actorIds: Uint32Array, gopIndex: number): void {
    this.ensureCapacity(Math.max(actorIds.length, 1));
    this.#actorIdBySlot.fill(EMPTY_SLOT);
    this.#slotByActor.clear();
    this.#free = [];
    this.#pending = [];
    this.#highWater = actorIds.length;
    for (let s = 0; s < actorIds.length; s++) {
      const id = actorIds[s];
      this.#actorIdBySlot[s] = id;
      if (id !== EMPTY_SLOT) this.#slotByActor.set(id, s);
      else this.#free.push(s);
    }
    this.#gopIndex = gopIndex;
  }

  /**
   * Record a spawn announced by a `Delta` (§3.4.5), which names the slot the server chose.
   *
   * The slot comes straight off the wire, so it is bounded by {@link slotLimit} before it can drive
   * {@link ensureCapacity}: §3.1.1 makes `actor_capacity` the run's slot ceiling, and without the
   * check one malformed frame allocates a multi-gigabyte `Uint32Array`.
   */
  adoptSpawn(slot: number, actorId: number): void {
    const limit = this.slotLimit;
    if (!Number.isInteger(slot) || slot < 0 || slot >= limit) {
      throw new ProtocolError("bad_offset", `Delta spawns slot ${slot} is at or beyond the actor-capacity bound ${limit} (§3.1.1)`, {
        offset: slot,
        expected: limit,
        actual: slot,
        field: "SlotTable.adoptSpawn",
      });
    }
    this.ensureCapacity(slot + 1);
    this.#actorIdBySlot[slot] = actorId;
    this.#slotByActor.set(actorId, slot);
    this.#free = this.#free.filter((s) => s !== slot);
    if (slot + 1 > this.#highWater) this.#highWater = slot + 1;
  }

  /** Forget everything (a non-resumed `Hello`, §1.4 case 2). */
  reset(): void {
    this.#actorIdBySlot.fill(EMPTY_SLOT);
    this.#slotByActor.clear();
    this.#free = [];
    this.#pending = [];
    this.#highWater = 0;
    this.#gopIndex = -1;
  }
}
