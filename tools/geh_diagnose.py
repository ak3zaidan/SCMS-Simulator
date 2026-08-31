#!/usr/bin/env python3
"""Diagnose the STRUCTURE of a GEH validation error. Diagnostic only -- never a result.

A failing GEH says "the model disagrees with reality"; it does not say *how*. Two very different
failures look identical in the headline number:

* a **level** error -- every station is off by roughly the same factor, i.e. the demand total is
  wrong but the assignment is right;
* an **assignment** error -- the total is close but flow is distributed over the wrong approaches.

This script separates them by looking at the per-station ratio ``modelled / measured``: a tight
ratio distribution means level, a wide one means assignment.

WHAT THIS IS NOT
----------------
The "after a uniform rescale" line is a COUNTERFACTUAL used to attribute error. It is **not** a
calibration, it is not applied to anything, and the rescaled GEH must never be quoted as the
model's GEH. Rescaling demand so a validation passes would be exactly the fiddling that makes a
validation worthless. The headline stays the untouched number in the report.

Reads a report written by ``tools/sumo_realism.py --ref-counts``.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
from pathlib import Path

BANNER = ("DIAGNOSTIC ONLY -- the rescaled figures below are a counterfactual for attributing the\n"
          "error. They are NOT a result and NOT a calibration. Never quote them as the model's GEH.")


def geh(m: float, c: float) -> float:
    s = m + c
    return 0.0 if s == 0 else math.sqrt(2.0 * (m - c) ** 2 / s)


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Level-vs-assignment attribution for a GEH failure.")
    p.add_argument("--report", required=True)
    p.add_argument("--top", type=int, default=8, help="how many worst stations to list")
    a = p.parse_args(argv)

    rep = json.loads(Path(a.report).read_text(encoding="utf-8"))
    if rep.get("comparison_kind") != "fhwa_validation":
        raise SystemExit(f"{a.report}: not a --ref-counts validation report.")
    rows = rep["sections"]["geh"]["geh"]["stations"]
    rows = [r for r in rows if float(r["counted"]) > 0]
    ratios = [float(r["modelled"]) / float(r["counted"]) for r in rows]
    tot_m = sum(float(r["modelled"]) for r in rows)
    tot_c = sum(float(r["counted"]) for r in rows)

    print(BANNER)
    print(f"\nstations {len(rows)}   total modelled {tot_m:,.0f}   total measured {tot_c:,.0f}   "
          f"level ratio {tot_m / tot_c:.4f}")
    print(f"per-station modelled/measured ratio: min {min(ratios):.3f}  p25 "
          f"{statistics.quantiles(ratios, n=4)[0]:.3f}  median {statistics.median(ratios):.3f}  "
          f"p75 {statistics.quantiles(ratios, n=4)[2]:.3f}  max {max(ratios):.3f}")
    print(f"  mean {statistics.mean(ratios):.4f}   sd {statistics.stdev(ratios):.4f}   "
          f"coefficient of variation {statistics.stdev(ratios) / statistics.mean(ratios):.4f}")

    k = tot_m / tot_c
    resc = [(r["station"], geh(float(r["modelled"]) / k, float(r["counted"]))) for r in rows]
    n_pass = sum(1 for _, g in resc if g < 5.0)
    print(f"\nCOUNTERFACTUAL, level error removed (every modelled flow divided by {k:.4f}):")
    print(f"  GEH < 5 on {n_pass}/{len(resc)} = {n_pass / len(resc) * 100:.1f}% of stations "
          f"(actual: {rep['sections']['geh']['geh']['pass_fraction'] * 100:.1f}%)")
    print(f"  median GEH {statistics.median(g for _, g in resc):.3f}   "
          f"max {max(g for _, g in resc):.3f}")
    print("  -> residual after removing the level error is the ASSIGNMENT/shape component.")

    print(f"\nworst {a.top} stations by absolute vehicle difference:")
    print(f"{'station':>8} {'modelled':>9} {'measured':>9} {'diff':>8} {'ratio':>7} {'GEH':>7} "
          f"{'share of total diff':>20}")
    tot_absdiff = sum(abs(float(r['modelled']) - float(r['counted'])) for r in rows)
    for r in sorted(rows, key=lambda r: -abs(float(r["modelled"]) - float(r["counted"])))[:a.top]:
        m, c = float(r["modelled"]), float(r["counted"])
        print(f"{r['station']:>8} {m:9,.0f} {c:9,.0f} {m - c:+8,.0f} {m / c:7.3f} "
              f"{float(r['geh']):7.2f} {abs(m - c) / tot_absdiff * 100:19.1f}%")
    print(f"\nsum of |per-station difference| = {tot_absdiff:,.0f} veh; "
          f"net difference = {tot_m - tot_c:+,.0f} veh; "
          f"cancellation = {(1 - abs(tot_m - tot_c) / tot_absdiff) * 100:.1f}% "
          f"(high cancellation means the total hides offsetting station errors)")
    print(f"\n{BANNER}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
