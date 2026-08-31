#!/usr/bin/env python3
"""Per-interval network loop total from a SUMO E1 output -- the warm-up / shape evidence.

A validation run that starts from an EMPTY network needs a warm-up before the graded window, and
"the warm-up was long enough" is a claim that has to be measured, not asserted. This prints the
summed ``nVehContrib`` per E1 interval over the named station groups, so the fill-up transient is
visible and the graded window can be shown to sit on the plateau.

Optionally restricts the sum to a station subset (``--stations``), which is what makes the printed
profile comparable to a reference file covering only those stations.

Reads only this repository's own SUMO output. No measured data, no licence constraint.
"""
from __future__ import annotations

import argparse
import xml.etree.ElementTree as ET
from collections import defaultdict
from pathlib import Path


def station_of(add_path: Path) -> dict[str, str]:
    out = {}
    for _ev, el in ET.iterparse(add_path, events=("end",)):
        if el.tag in ("e1Detector", "inductionLoop") and el.get("id"):
            out[el.get("id")] = el.get("name") or el.get("id")
        el.clear()
    return out


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description="Per-interval E1 totals (warm-up evidence).")
    p.add_argument("--det-out", required=True)
    p.add_argument("--det-add", required=True)
    p.add_argument("--stations", help="comma-separated station subset (default: all named)")
    p.add_argument("--per-station", action="store_true", help="also print a station x interval grid")
    a = p.parse_args(argv)

    smap = station_of(Path(a.det_add))
    named = {d: s for d, s in smap.items() if s != d}
    want = set(a.stations.split(",")) if a.stations else set(named.values())

    per_int: dict[tuple[float, float], float] = defaultdict(float)
    per_st: dict[str, dict[tuple[float, float], float]] = defaultdict(lambda: defaultdict(float))
    for _ev, el in ET.iterparse(a.det_out, events=("end",)):
        if el.tag == "interval":
            st = named.get(el.get("id"))
            if st in want:
                key = (float(el.get("begin")), float(el.get("end")))
                c = float(el.get("nVehContrib", 0.0))
                per_int[key] += c
                per_st[st][key] += c
        el.clear()

    keys = sorted(per_int)
    print(f"detectors grouped into {len(want)} station(s); {len(keys)} interval(s)")
    print(f"{'begin':>8} {'end':>8} {'hh:mm':>7} {'veh':>8}   {'vs prev':>8}")
    prev = None
    for b, e in keys:
        d = "" if prev is None else f"{(per_int[(b, e)] - prev) / prev * 100:+7.1f}%" if prev else ""
        print(f"{b:8.0f} {e:8.0f} {int(b) // 3600:02d}:{int(b) % 3600 // 60:02d} "
              f"{per_int[(b, e)]:8.0f}   {d:>8}")
        prev = per_int[(b, e)]
    tot = sum(per_int.values())
    print(f"total over all intervals: {tot:.0f} veh")

    if a.per_station:
        print("\nstation " + " ".join(f"{int(b):>7}" for b, _ in keys))
        for st in sorted(per_st):
            print(f"{st:<7} " + " ".join(f"{per_st[st][k]:7.0f}" for k in keys))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
