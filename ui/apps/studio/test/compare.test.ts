/**
 * The comparison arithmetic — the part of the side-by-side that can be wrong quietly.
 *
 * A plot hides all three of the cases below. A metric only one run reports must not read as a
 * difference of zero; a metric neither run has sampled yet at the scrubbed instant must not read as
 * zero either; and a baseline of exactly zero has no relative difference, which is different from
 * having one of 0 %.
 *
 * `MetricHistory.at` is tested here rather than beside the rest of `lib/history.ts` because this is
 * what it exists for: "the same simulated time" in two runs has to mean the same instant, and a
 * metric binned at 1 s is almost never sampled at the instant a scrub lands on.
 */

import { describe, expect, it } from "vitest";

import { MetricHistory } from "../src/lib/history.js";
import { DIFF_EPSILON, alignedDiffSeries, metricDifferences } from "../src/state/compare.js";

/** A history with `name` sampled at each `[t, value]`. */
function history(series: Record<string, readonly (readonly [number, number])[]>, capacity = 900): MetricHistory {
  const h = new MetricHistory(capacity);
  for (const [name, samples] of Object.entries(series)) {
    for (const [t, value] of samples) h.push(name, t, value);
  }
  return h;
}

describe("MetricHistory.at", () => {
  const h = history({ pdr: [[1, 0.9], [2, 0.8], [3, 0.7], [4, 0.6]] });

  it("returns the sample at an exact time", () => {
    expect(h.at("pdr", 2)).toBe(0.8);
  });

  it("steps back to the last sample at or before the asked time", () => {
    // Step, not linear: §3.7 samples carry the bin's end time, and interpolating between two bins
    // would invent a value the run never produced.
    expect(h.at("pdr", 2.9)).toBe(0.8);
    expect(h.at("pdr", 3.0)).toBe(0.7);
  });

  it("holds the last value past the end of the series", () => {
    expect(h.at("pdr", 99)).toBe(0.6);
  });

  it("returns null before the first sample, rather than the first value", () => {
    expect(h.at("pdr", 0.5)).toBeNull();
    expect(h.at("pdr", 0)).toBeNull();
  });

  it("returns null for a metric it has never seen, and for a non-finite time", () => {
    expect(h.at("nope", 2)).toBeNull();
    expect(h.at("pdr", Number.NaN)).toBeNull();
  });

  it("reads correctly after the ring has wrapped", () => {
    // The binary search walks a logical index over a ring whose `start` has moved; getting the
    // modulo wrong here would return a value from before the eviction.
    const small = new MetricHistory(4);
    for (let t = 1; t <= 10; t++) small.push("cbr", t, t / 100);
    // Capacity 4, so only t = 7…10 are retained.
    expect(small.at("cbr", 6)).toBeNull();
    expect(small.at("cbr", 7)).toBeCloseTo(0.07, 10);
    expect(small.at("cbr", 9.5)).toBeCloseTo(0.09, 10);
    expect(small.at("cbr", 50)).toBeCloseTo(0.1, 10);
  });

  it("has() distinguishes an unsampled metric from a sampled one", () => {
    expect(h.has("pdr")).toBe(true);
    expect(h.has("cbr")).toBe(false);
  });
});

describe("metricDifferences", () => {
  it("computes B − A and the relative difference at one instant per side", () => {
    const a = history({ pdr: [[1, 0.8], [2, 0.8]] });
    const b = history({ pdr: [[1, 0.6], [2, 0.6]] });
    const [row] = metricDifferences(["pdr"], a, b, 2, 2, { pdr: "ratio" });
    expect(row.a).toBeCloseTo(0.8, 10);
    expect(row.b).toBeCloseTo(0.6, 10);
    expect(row.delta).toBeCloseTo(-0.2, 10);
    expect(row.relative).toBeCloseTo(-0.25, 10);
    expect(row.unit).toBe("ratio");
    expect(row.oneSided).toBe(false);
  });

  it("reads each side at its own time, which is what the offset is for", () => {
    const a = history({ cbr: [[10, 0.30]] });
    const b = history({ cbr: [[10, 0.99], [40, 0.31]] });
    // A at t = 10 against B at t = 40: the offset is the researcher's alignment assertion, and a
    // difference read at the wrong instant is the failure it exists to prevent.
    const [row] = metricDifferences(["cbr"], a, b, 10, 40, {});
    expect(row.b).toBeCloseTo(0.31, 10);
    expect(row.delta).toBeCloseTo(0.01, 10);
  });

  it("marks a metric only one side reports, and does not call the difference zero", () => {
    const a = history({ pdr: [[1, 0.8]] });
    const b = history({ pdr: [[1, 0.7]], "privacy.tracking_s": [[1, 42]] });
    const rows = metricDifferences(["pdr", "privacy.tracking_s"], a, b, 1, 1, {});
    const oneSided = rows.find((r) => r.metric === "privacy.tracking_s");
    expect(oneSided?.oneSided).toBe(true);
    expect(oneSided?.a).toBeNull();
    expect(oneSided?.delta).toBeNull();
    expect(oneSided?.relative).toBeNull();
    expect(rows.find((r) => r.metric === "pdr")?.oneSided).toBe(false);
  });

  it("returns a null difference when one side has no sample yet at this time", () => {
    const a = history({ pdr: [[1, 0.8]] });
    const b = history({ pdr: [[5, 0.8]] });
    const [row] = metricDifferences(["pdr"], a, b, 1, 1, {});
    // B reports the metric, but not yet at t = 1. Both sides have the metric, so it is not
    // one-sided — it is simply not comparable at this instant.
    expect(row.oneSided).toBe(false);
    expect(row.b).toBeNull();
    expect(row.delta).toBeNull();
  });

  it("has no relative difference against a zero baseline", () => {
    const a = history({ drops: [[1, 0]] });
    const b = history({ drops: [[1, 12]] });
    const [row] = metricDifferences(["drops"], a, b, 1, 1, {});
    expect(row.delta).toBe(12);
    // Not Infinity, and not 0: "12 more than none" has an absolute difference and no ratio.
    expect(row.relative).toBeNull();
  });

  it("treats a baseline below the epsilon as no baseline at all", () => {
    // Otherwise a metric that decayed to 1e-300 would report a relative difference of 1e300 %.
    const a = history({ decay: [[1, DIFF_EPSILON / 2]] });
    const b = history({ decay: [[1, 1]] });
    const [row] = metricDifferences(["decay"], a, b, 1, 1, {});
    expect(row.relative).toBeNull();
    expect(row.delta).not.toBeNull();
  });

  it("uses the magnitude of the baseline, so a negative baseline keeps the sign of the change", () => {
    // `delta / a` would flip the sign of an improvement when the baseline is negative (a clock
    // offset, a relative error), which reads as a regression.
    const a = history({ offset: [[1, -4]] });
    const b = history({ offset: [[1, -2]] });
    const [row] = metricDifferences(["offset"], a, b, 1, 1, {});
    expect(row.delta).toBe(2);
    expect(row.relative).toBeCloseTo(0.5, 10);
  });

  it("sorts rows by metric name, so the table does not reorder between ticks", () => {
    const a = history({ zeta: [[1, 1]], alpha: [[1, 1]], mu: [[1, 1]] });
    const rows = metricDifferences(["zeta", "mu", "alpha"], a, a, 1, 1, {});
    expect(rows.map((r) => r.metric)).toEqual(["alpha", "mu", "zeta"]);
  });

  it("leaves the caller's name list alone", () => {
    const names = ["zeta", "alpha"];
    const a = history({ zeta: [[1, 1]], alpha: [[1, 1]] });
    metricDifferences(names, a, a, 1, 1, {});
    expect(names).toEqual(["zeta", "alpha"]);
  });

  it("returns an empty table for an empty name list", () => {
    expect(metricDifferences([], new MetricHistory(), new MetricHistory(), 0, 0, {})).toEqual([]);
  });
});

describe("alignedDiffSeries", () => {
  it("plots on side A's sample grid and reads B at the offset", () => {
    const a = history({ pdr: [[1, 0.9], [2, 0.8]] });
    const b = history({ pdr: [[3, 0.5], [4, 0.4]] });
    const [xs, av, bv, dv] = alignedDiffSeries("pdr", a, b, 2);
    expect(xs).toEqual([1, 2]);
    expect(av).toEqual([0.9, 0.8]);
    expect(bv).toEqual([0.5, 0.4]);
    expect(dv[0]).toBeCloseTo(-0.4, 10);
    expect(dv[1]).toBeCloseTo(-0.4, 10);
  });

  it("leaves a gap where B has nothing yet, rather than a difference of zero", () => {
    const a = history({ pdr: [[1, 0.9], [2, 0.8], [3, 0.7]] });
    const b = history({ pdr: [[3, 0.5]] });
    const [, , bv, dv] = alignedDiffSeries("pdr", a, b, 0);
    expect(bv).toEqual([null, null, 0.5]);
    // A zero here would claim the two runs agreed at instants one of them had not reached.
    expect(dv[0]).toBeNull();
    expect(dv[1]).toBeNull();
    expect(dv[2]).toBeCloseTo(-0.2, 10);
  });

  it("is empty when side A has never sampled the metric", () => {
    const [xs, av, bv, dv] = alignedDiffSeries("pdr", new MetricHistory(), history({ pdr: [[1, 1]] }), 0);
    expect(xs).toEqual([]);
    expect(av).toEqual([]);
    expect(bv).toEqual([]);
    expect(dv).toEqual([]);
  });
});
