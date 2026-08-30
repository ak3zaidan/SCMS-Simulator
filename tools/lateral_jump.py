"""Measure single-step LATERAL (cross-heading) displacement in a ground-truth trace.

Why: without SUMO's sublane model a lane change is an instantaneous teleport between lane
centrelines -- the vehicle's true position moves a full lane width (3.2 m on a default lane) in a
single sample while its speed stays flat. That is physically impossible, it is precisely the
signature a V2X position-plausibility detector keys on, and it poisons any finite-difference
kinematics computed from the trace (differencing hypot(dx,dy)/dt across a 3.2 m jump at dt=0.1 s
reads as a 33 m/s speed and a ~275 m/s^2 acceleration).

This tool quantifies the artefact directly on the raw ``ground_truth/gt_emissions_sample.jsonl``:

    python tools/lateral_jump.py datasets/intas_nosublane_300s datasets/intas_sublane_300s

Method (identical for every dataset compared, so the comparison is apples-to-apples):

  * records are grouped by ``true_vehicle_id`` and sorted by ``t``;
  * for each consecutive pair the displacement d = (dx, dy) is decomposed against a REFERENCE
    HEADING taken from an EARLIER step of the same vehicle -- never from the step under test, whose
    own lateral kick is the thing being measured. The reference is the most recent preceding step
    that is long enough to define a direction (``--min-ref``, default 0.3 m) within ``--lookback``
    samples; pairs with no such reference are skipped and reported as ``pairs_no_reference``;
  * lateral = |d x u| (cross-product magnitude), longitudinal = d . u;
  * pairs whose ``dt`` exceeds ``--dt-max`` (default 1.0 s, the coarsest interval ETSI CAM
    triggering produces here) are skipped, which also drops re-insertions and SUMO teleports.

Reads ORACLE ground truth only -- an offline measurement tool, never part of the MA/ML path.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path


def _load(path: Path) -> dict[str, list[tuple[float, float, float, float]]]:
    """{vehicle_id: [(t, x, y, claimed_speed), ...]} from a gt_emissions_sample.jsonl."""
    tracks: dict[str, list[tuple[float, float, float, float]]] = {}
    with open(path, "r", encoding="utf-8", errors="replace") as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            try:
                r = json.loads(line)
            except ValueError:
                continue
            vid = r.get("true_vehicle_id")
            if vid is None or r.get("true_x") is None or r.get("true_y") is None:
                continue
            tracks.setdefault(vid, []).append(
                (float(r["t"]), float(r["true_x"]), float(r["true_y"]),
                 float(r.get("claimed_speed") or 0.0)))
    for v in tracks.values():
        v.sort(key=lambda s: s[0])
    return tracks


def _pct(sorted_vals: list[float], q: float) -> float:
    if not sorted_vals:
        return float("nan")
    i = min(len(sorted_vals) - 1, max(0, int(round(q * (len(sorted_vals) - 1)))))
    return sorted_vals[i]


def analyse(path: Path, threshold: float = 1.5, dt_max: float = 1.0,
            min_ref: float = 0.3, lookback: int = 30) -> dict:
    tracks = _load(path)
    lats: list[float] = []
    n_pairs = 0
    n_skipped_dt = 0
    n_no_ref = 0
    n_over = 0
    n_lane_width = 0     # cross-heading step inside the one-SUMO-lane-width band
    n_pure_lat = 0       # sideways with (almost) no forward motion -- physically impossible
    hist: dict[int, int] = {}
    worst = {"lateral_m": 0.0}
    accels: list[float] = []
    worst_accel = {"accel_ms2": 0.0}
    for vid, samples in tracks.items():
        for i in range(1, len(samples)):
            t1, x1, y1, _s1 = samples[i - 1]
            t2, x2, y2, s2 = samples[i]
            dt = t2 - t1
            if dt <= 0 or dt > dt_max:
                n_skipped_dt += 1
                continue
            dx, dy = x2 - x1, y2 - y1
            # reference heading from an EARLIER step of this vehicle (never the step under test)
            ux = uy = None
            for j in range(i - 1, max(0, i - 1 - lookback), -1):
                tp, xp, yp, _sp = samples[j - 1]
                tq, xq, yq, _sq = samples[j]
                if tq - tp <= 0 or tq - tp > dt_max:
                    continue
                rx, ry = xq - xp, yq - yp
                rn = math.hypot(rx, ry)
                if rn >= min_ref:
                    ux, uy = rx / rn, ry / rn
                    break
            if ux is None:
                n_no_ref += 1
                continue
            n_pairs += 1
            lat = abs(dx * uy - dy * ux)
            lats.append(lat)
            if lat > threshold:
                n_over += 1
                step = math.hypot(dx, dy)
                hist[int(lat / 0.5)] = hist.get(int(lat / 0.5), 0) + 1
                # 3.2 m is SUMO's default lane width: a cross-heading step in this band is the
                # centreline-to-centreline teleport signature.
                if 2.9 <= lat <= 3.5:
                    n_lane_width += 1
                # the step is (almost) entirely sideways -- a real vehicle cannot do that
                if step <= 1.2 * lat:
                    n_pure_lat += 1
            if lat > worst["lateral_m"]:
                worst = {"lateral_m": round(lat, 3), "vehicle": vid,
                         "t_from": t1, "t_to": t2, "dt_s": round(dt, 3),
                         "x_from": x1, "y_from": y1, "x_to": x2, "y_to": y2,
                         "longitudinal_m": round(dx * ux + dy * uy, 3),
                         "claimed_speed_ms": s2}
            # the knock-on effect on finite-difference kinematics (defect B)
            if i >= 2:
                t0, x0, y0, _s0 = samples[i - 2]
                dt0 = t1 - t0
                if 0 < dt0 <= dt_max:
                    v0 = math.hypot(x1 - x0, y1 - y0) / dt0
                    v1 = math.hypot(dx, dy) / dt
                    a = (v1 - v0) / dt
                    accels.append(abs(a))
                    if abs(a) > abs(worst_accel["accel_ms2"]):
                        worst_accel = {"accel_ms2": round(a, 2), "vehicle": vid, "t": t2}
    lats_sorted = sorted(lats)
    acc_sorted = sorted(accels)
    return {
        "dataset": str(path.parent.parent if path.name.endswith(".jsonl") else path),
        "trace": str(path),
        "vehicles": len(tracks),
        "samples": sum(len(v) for v in tracks.values()),
        "pairs_considered": n_pairs,
        "pairs_skipped_dt": n_skipped_dt,
        "pairs_no_reference": n_no_ref,
        "threshold_m": threshold,
        "lateral_jumps_over_threshold": n_over,
        "lateral_jumps_per_1000_pairs": round(1000.0 * n_over / n_pairs, 3) if n_pairs else None,
        # the two diagnostic sub-counts that separate "centreline teleport" from "road curvature"
        "lane_width_band_2.9_3.5m": n_lane_width,
        "near_pure_lateral_steps": n_pure_lat,
        "lateral_histogram_0.5m_bins": {f"{k * 0.5:.1f}-{k * 0.5 + 0.5:.1f}": v
                                        for k, v in sorted(hist.items())},
        "max_lateral_step_m": round(max(lats), 3) if lats else None,
        "p50_lateral_m": round(_pct(lats_sorted, 0.50), 4) if lats else None,
        "p99_lateral_m": round(_pct(lats_sorted, 0.99), 4) if lats else None,
        "p999_lateral_m": round(_pct(lats_sorted, 0.999), 4) if lats else None,
        "worst_pair": worst if lats else None,
        "derived_accel_max_abs_ms2": round(max(accels), 2) if accels else None,
        "derived_accel_p99_abs_ms2": round(_pct(acc_sorted, 0.99), 3) if accels else None,
        "derived_accel_worst": worst_accel if accels else None,
    }


def _trace_of(target: Path) -> Path:
    if target.is_file():
        return target
    p = target / "ground_truth" / "gt_emissions_sample.jsonl"
    if p.exists():
        return p
    raise SystemExit(f"no ground_truth/gt_emissions_sample.jsonl under {target}")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("targets", nargs="+", help="dataset dir(s) or gt_emissions_sample.jsonl path(s)")
    ap.add_argument("--threshold", type=float, default=1.5,
                    help="lateral displacement (m) above which a single step counts as a jump")
    ap.add_argument("--dt-max", type=float, default=1.0, help="ignore pairs with a larger gap (s)")
    ap.add_argument("--min-ref", type=float, default=0.3,
                    help="minimum length (m) of the earlier step used as the heading reference")
    ap.add_argument("--lookback", type=int, default=30,
                    help="how many earlier steps to search for a usable heading reference")
    ap.add_argument("--json", dest="json_out", help="also write the full result to this JSON file")
    a = ap.parse_args(argv)

    results = [analyse(_trace_of(Path(t)), a.threshold, a.dt_max, a.min_ref, a.lookback)
               for t in a.targets]
    for r in results:
        print(f"== {r['trace']}")
        print(f"   vehicles {r['vehicles']}  samples {r['samples']}  "
              f"pairs {r['pairs_considered']} (skipped dt {r['pairs_skipped_dt']}, "
              f"no-ref {r['pairs_no_reference']})")
        print(f"   lateral steps > {r['threshold_m']} m : {r['lateral_jumps_over_threshold']} "
              f"({r['lateral_jumps_per_1000_pairs']} per 1000 pairs)")
        print(f"     of which in the 2.9-3.5 m lane-width band : "
              f"{r['lane_width_band_2.9_3.5m']}")
        print(f"     of which (almost) pure sideways moves     : "
              f"{r['near_pure_lateral_steps']}")
        print(f"   max single-step lateral      : {r['max_lateral_step_m']} m")
        print(f"   lateral p50/p99/p99.9        : {r['p50_lateral_m']} / {r['p99_lateral_m']} / "
              f"{r['p999_lateral_m']} m")
        print(f"   derived |accel| max / p99    : {r['derived_accel_max_abs_ms2']} / "
              f"{r['derived_accel_p99_abs_ms2']} m/s^2")
        if r["worst_pair"]:
            w = r["worst_pair"]
            print(f"   worst pair: {w.get('vehicle')} t {w.get('t_from')}->{w.get('t_to')} "
                  f"lat {w.get('lateral_m')} m  lon {w.get('longitudinal_m')} m  "
                  f"claimed_speed {w.get('claimed_speed_ms')} m/s")
    if len(results) >= 2:
        base, new = results[0], results[-1]
        print()
        print("== reduction (first -> last)")
        for k in ("lateral_jumps_over_threshold", "lateral_jumps_per_1000_pairs",
                  "lane_width_band_2.9_3.5m", "near_pure_lateral_steps",
                  "p999_lateral_m", "max_lateral_step_m", "derived_accel_max_abs_ms2"):
            b, n = base.get(k), new.get(k)
            if isinstance(b, (int, float)) and isinstance(n, (int, float)):
                fac = f"{b / n:.1f}x lower" if n else "-> 0"
                print(f"   {k:36s} {b} -> {n}   ({fac})")
    if a.json_out:
        Path(a.json_out).write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
        print(f"\nwrote {a.json_out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
