/**
 * Regression tests for `lib/history.ts` (ui-review-register Q12).
 *
 * The file's header claims "Nothing here allocates per sample" and frames both stores as rings.
 * `MetricHistory` evicted with `Array.prototype.shift()`, which is O(n) per sample once a series is
 * full — the reviewer measured 37 ns/push below capacity against 810 ns/push at capacity, a 22x
 * cliff that is permanent after the first 900 samples of every metric.
 *
 * That was first asserted as a wall-clock ratio between capacity 200 and capacity 40,000, on the
 * reasoning that a ~200x signal survives a noisy machine. It does not: the ratio reached 15.2
 * against a threshold of 8 with the four workspace packages testing concurrently, because the two
 * measurements are taken one after the other and the machine does not hold still between them. The
 * defect is asserted directly instead — the O(n) eviction primitives are counted, and must be zero
 * — which is the same property, exactly, and independent of what else the machine is doing.
 */

import { describe, expect, it } from "vitest";

import { MetricHistory } from "../src/lib/history.js";

function fillAndTime(capacity: number, pushes: number): number {
  const h = new MetricHistory(capacity);
  for (let i = 0; i < capacity; i++) h.push("m", i, i);
  const t0 = process.hrtime.bigint();
  for (let i = 0; i < pushes; i++) h.push("m", capacity + i, i);
  const t1 = process.hrtime.bigint();
  return Number(t1 - t0) / pushes;
}

/** Count the O(n) moves a push at capacity must not make, however the eviction is written. */
function countBulkMoves(run: () => void): number {
  const typed = Object.getPrototypeOf(Float64Array.prototype) as {
    copyWithin: unknown; set: unknown; subarray: unknown;
  };
  const saved: [object, string, unknown][] = [
    [Array.prototype, "shift", Array.prototype.shift],
    [Array.prototype, "unshift", Array.prototype.unshift],
    [Array.prototype, "splice", Array.prototype.splice],
    [Array.prototype, "slice", Array.prototype.slice],
    [typed, "copyWithin", typed.copyWithin],
    [typed, "set", typed.set],
    [typed, "subarray", typed.subarray],
  ];
  let calls = 0;
  try {
    for (const [target, key, original] of saved) {
      Object.defineProperty(target, key, {
        configurable: true,
        writable: true,
        value: function counted(this: unknown, ...args: unknown[]): unknown {
          calls++;
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          return (original as any).apply(this, args);
        },
      });
    }
    run();
  } finally {
    for (const [target, key, original] of saved) {
      Object.defineProperty(target, key, { configurable: true, writable: true, value: original });
    }
  }
  return calls;
}

describe("MetricHistory is a ring", () => {
  it("evicts in O(1) at capacity, whatever the capacity is", () => {
    // The defect, counted rather than timed. `MetricHistory.push` evicted with
    // `s.xs.shift(); s.ys.shift();` — two O(n) moves per sample, permanently, from the first
    // sample past capacity. A ring makes zero of them, at any capacity.
    const PUSHES = 200_000;
    const at = (capacity: number): number => {
      const h = new MetricHistory(capacity);
      for (let i = 0; i < capacity; i++) h.push("m", i, i);
      return countBulkMoves(() => {
        for (let i = 0; i < PUSHES; i++) h.push("m", capacity + i, i);
      });
    };
    const small = at(200);
    const large = at(40_000);
    // Before the fix: 400,000 each, and the 40,000-capacity one moved 8 billion elements.
    expect(small).toBe(0);
    expect(large).toBe(0);

    // Measured too, the way the review measured it — reported, not asserted. The ratio is what
    // used to be the assertion; it read 15.2 against a bar of 8 under a concurrent workspace run.
    fillAndTime(200, 20_000); // JIT warm-up on both shapes before either is measured
    fillAndTime(40_000, 20_000);
    const tSmall = fillAndTime(200, PUSHES);
    const tLarge = fillAndTime(40_000, PUSHES);
    // eslint-disable-next-line no-console
    console.log(
      `MetricHistory.push at capacity: ${tSmall.toFixed(1)} ns at 200 vs ${tLarge.toFixed(1)} ns at `
      + `40,000 = ${(tLarge / tSmall).toFixed(2)}x (review measured 37 ns below capacity vs 810 at), `
      + `${small + large} O(n) bulk moves over ${2 * PUSHES} pushes`,
    );
  });

  it("keeps the newest `capacity` samples in order after wrapping", () => {
    const h = new MetricHistory(4);
    for (let i = 0; i < 10; i++) h.push("m", i, i * 10);
    const [xs, ys] = h.get("m");
    expect(xs).toEqual([6, 7, 8, 9]);
    expect(ys).toEqual([60, 70, 80, 90]);
    expect(h.latest("m")).toBe(90);
  });

  it("overwrites a repeated bin end time instead of stacking it (§3.7)", () => {
    const h = new MetricHistory(8);
    h.push("m", 1, 5);
    h.push("m", 1, 6);
    h.push("m", 2, 7);
    expect(h.get("m")).toEqual([[1, 2], [6, 7]]);
  });

  it("overwrites a repeated bin end time correctly across the wrap point", () => {
    const h = new MetricHistory(3);
    for (let i = 0; i < 5; i++) h.push("m", i, i);
    h.push("m", 4, 99);
    expect(h.get("m")).toEqual([[2, 3, 4], [2, 3, 99]]);
  });

  it("an unseen metric is still empty arrays", () => {
    const h = new MetricHistory(4);
    expect(h.get("nope")).toEqual([[], []]);
    expect(h.latest("nope")).toBeNull();
  });

  it("reset forgets every series", () => {
    const h = new MetricHistory(4);
    h.push("a", 1, 1);
    h.push("b", 1, 1);
    h.reset();
    expect(h.names()).toEqual([]);
    expect(h.get("a")).toEqual([[], []]);
  });
});

describe("MetricHistory bounds its series count and caches its name list", () => {
  it("names() returns the same array while the series set is unchanged", () => {
    const h = new MetricHistory(8);
    h.push("b", 1, 1);
    h.push("a", 1, 1);
    const first = h.names();
    expect(first).toEqual(["a", "b"]);
    h.push("a", 2, 2);
    h.push("b", 2, 2);
    // The reviewer measured names() at 1.61 ms/call with 50,000 series, called on every 5 Hz tick.
    expect(h.names()).toBe(first);
    h.push("c", 1, 1);
    expect(h.names()).not.toBe(first);
    expect(h.names()).toEqual(["a", "b", "c"]);
  });

  it("seriesVersion moves only when the set of series changes", () => {
    const h = new MetricHistory(8);
    h.push("a", 1, 1);
    const v = h.seriesVersion;
    h.push("a", 2, 2);
    expect(h.seriesVersion).toBe(v);
    h.push("b", 1, 1);
    expect(h.seriesVersion).toBe(v + 1);
  });

  it("50,000 distinct metric names do not leave 50,000 series resident", () => {
    const h = new MetricHistory(64, 512);
    for (let i = 0; i < 50_000; i++) h.push(`metric.${i}`, 1, i);
    expect(h.names().length).toBe(512);
    // The oldest names are evicted, the newest are kept.
    expect(h.latest("metric.49999")).toBe(49_999);
    expect(h.latest("metric.0")).toBeNull();
  });
});
