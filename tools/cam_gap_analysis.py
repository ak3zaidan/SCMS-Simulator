"""The CAM inter-packet gap distribution, in the shape the Java engine published its own.

`counts["protocol"]["cam_generation"]` gives a mean, a max and a trigger histogram. The cross-engine
comparison in `docs/realism/CROSS-ENGINE-RADIO.md` section 7 is stated as percentiles and as a
six-bucket histogram over the HONEST vehicles, so this reads the emission stream and produces
exactly those, per arm, with the honest/all split the Java table also carries.

The split is not cosmetic. This engine evaluates clause 6.1.3 on the station's TRUE ego state, not
on the position it claims (`run.py`, at the `_cs_st.evaluate` call site), so an attacker does not
trigger itself faster by lying -- but attackers are still a different subpopulation, a DoS station
emits bursts the CAM service never gated, and the Java figure this is compared against is quoted for
honest vehicles. Measured on bench A the two differ by 1.2 % on the mean gap (0.302321 s all,
0.306104 s honest), which is small and worth showing rather than assuming.

    python tools/cam_gap_analysis.py C:/Temp/pstack/intas_cam --json gaps.json
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

#: The Java engine's own bucket edges (CROSS-ENGINE-RADIO.md section 7), so the two histograms are
#: read off the same ruler rather than re-binned by eye.
MOSAIC_EDGES = ((0.0, 0.15), (0.15, 0.25), (0.25, 0.35), (0.35, 0.55), (0.55, 0.95), (0.95, 1.05),
                (1.05, 1e9))

#: What the Java engine measured on the same EN 302 637-2 rules, for the side-by-side.
JAVA = {"mean_gap_s_honest": 0.3399, "rate_hz_honest": 2.942,
        "mean_gap_s_all": 0.3338, "rate_hz_all": 2.996,
        "median_s": 0.300, "p05_s": 0.200, "p25_s": 0.200, "p75_s": 0.400, "p95_s": 1.000,
        "p99_s": 1.000, "max_s": 1.100,
        "share_at_t_gen_cam_min": 0.0491, "share_at_t_gen_cam_max": 0.0551,
        "dynamics_share": 0.8958,
        "hist": {"<0.15": 8950, "0.15-0.25": 45893, "0.25-0.35": 67129, "0.35-0.55": 45797,
                 "0.55-0.95": 4328, "0.95-1.05": 10009},
        "source": "docs/realism/CROSS-ENGINE-RADIO.md section 7 (MOSAIC/ScmsBeaconApp, "
                  "gen_intas_urban_low, seed 20260809)"}


def _rows(dataset_dir: str):
    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            if ln.strip():
                yield json.loads(ln)


def _stats(gaps: list[float]) -> dict:
    if not gaps:
        return {"gaps": 0}
    gaps.sort()
    n = len(gaps)

    def q(p):
        return gaps[min(n - 1, max(0, int(math.ceil(p * n)) - 1))]

    hist = {}
    for lo, hi in MOSAIC_EDGES:
        k = f"<{hi}" if lo == 0.0 else (f">={lo}" if hi > 1e8 else f"{lo}-{hi}")
        hist[k] = sum(1 for g in gaps if lo <= g < hi)
    total = sum(gaps)
    return {"gaps": n, "mean_gap_s": round(total / n, 6), "rate_hz": round(n / total, 6),
            "median_s": q(0.50), "p05_s": q(0.05), "p25_s": q(0.25), "p75_s": q(0.75),
            "p95_s": q(0.95), "p99_s": q(0.99), "max_s": gaps[-1], "min_s": gaps[0],
            "share_at_t_gen_cam_min": round(sum(1 for g in gaps if g <= 0.1000001) / n, 6),
            "share_at_t_gen_cam_max": round(sum(1 for g in gaps if g >= 0.9999) / n, 6),
            "hist": hist,
            "hist_share": {k: round(v / n, 6) for k, v in hist.items()}}


def analyse(dataset_dir: str) -> dict:
    per: dict[str, list[float]] = {}
    honest: dict[str, bool] = {}
    for r in _rows(dataset_dir):
        vid = str(r.get("true_vehicle_id"))
        per.setdefault(vid, []).append(float(r["t"]))
        if vid not in honest:
            honest[vid] = not (r.get("is_attacker") or r.get("is_faulty"))
    g_all: list[float] = []
    g_hon: list[float] = []
    for vid, ts in per.items():
        ts.sort()
        g = [round(b - a, 6) for a, b in zip(ts, ts[1:])]
        g_all.extend(g)
        if honest.get(vid):
            g_hon.extend(g)
    return {"dataset_dir": dataset_dir, "stations": len(per),
            "stations_honest": sum(1 for v in honest.values() if v),
            "all": _stats(g_all), "honest": _stats(g_hon), "java_reference": JAVA}


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("dataset_dirs", nargs="+")
    p.add_argument("--json", dest="json_out", default="")
    a = p.parse_args(argv)
    out = {os.path.basename(d.rstrip("/\\")): analyse(d) for d in a.dataset_dirs}
    txt = json.dumps(out, indent=1, sort_keys=True)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
