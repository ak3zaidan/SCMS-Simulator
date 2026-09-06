"""Awareness with the shot multiplicity MEASURED instead of derived from `dt`.

`datagen/awareness.py` computes the engine's shot multiplicity as `z_for_engine(dt_s, 1.0)` --
`floor(1 / dt)` capped by the reference's fitted Z. That is exact for an engine that emits one CAM
per step, which is what the engine did when the module was written. Under EN 302 637-2 generation
rules it is not: at `dt = 0.1` the config says ten shots per second and the CAM service actually
delivers about three, so `nar_at_engine_rate` is read at a Z the run never had.

This measures N from the emission stream -- CAMs per station per 1 s window, the quantity eq. (4)
bounds Z by -- and re-evaluates the awareness block at that Z beside the config-derived one, on the
identical PDR curve. It changes nothing in `src/`; it reports what the difference is worth.

`nar90_equivalent_range_m` is deliberately reported unchanged: it is a crossing of OUR PER-PACKET
curve at the per-packet level the REFERENCE's NAR 0.90 implies at the REFERENCE's Z, so no engine
rate enters it. Saying so is half the answer to "do the earlier conclusions change".

    python tools/awareness_shots.py C:/Temp/pstack/flag_dt01_cam --json aw.json
"""
from __future__ import annotations

import argparse
import json
import math
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)

from scms_sim_ref.datagen import awareness as AW                      # noqa: E402


def measured_shots(dataset_dir: str, window_s: float = 1.0) -> dict:
    """CAMs per station per whole `window_s`, over stations actually present in that window."""
    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    counts: dict[tuple, int] = {}
    tmax = 0.0
    with open(path, encoding="utf-8") as fh:
        for ln in fh:
            if not ln.strip():
                continue
            r = json.loads(ln)
            t = float(r["t"])
            tmax = max(tmax, t)
            k = (str(r.get("true_vehicle_id")), int(math.floor(t / window_s)))
            counts[k] = counts.get(k, 0) + 1
    last = int(math.floor(tmax / window_s))
    # The first and last windows are partial for any station that spawns or leaves inside them, so
    # a station is counted only where it was present for the whole window: it emitted in the window
    # before AND the window after. That is the population whose awareness the metric describes.
    present = set(counts)
    vals = [v for (vid, w), v in counts.items()
            if 0 < w < last and (vid, w - 1) in present and (vid, w + 1) in present]
    if not vals:
        return {"windows": 0}
    vals.sort()
    n = len(vals)
    return {"windows": n, "window_s": window_s,
            "mean": round(sum(vals) / n, 4), "median": vals[n // 2],
            "p10": vals[max(0, int(0.10 * n) - 1)], "p90": vals[min(n - 1, int(0.90 * n))],
            "min": vals[0], "max": vals[-1],
            "hist": {str(k): vals.count(k) for k in sorted(set(vals))}}


def report(dataset_dir: str) -> dict:
    rep = AW.awareness_report(dataset_dir)
    sh = measured_shots(dataset_dir)
    z_ref = rep["shot_multiplicity"]["z_reference_urban"]
    z_lo, z_hi = rep["shot_multiplicity"]["z_reference_range"]
    z_cfg = rep["shot_multiplicity"]["z_engine_effective"]
    n_meas = sh.get("mean", 1.0)
    z_meas = min(max(1.0, n_meas), z_ref)

    anchors = {}
    for a, row in rep["anchors"].items():
        p = row["pdr_per_packet"]
        anchors[str(a)] = {
            "pdr_per_packet": p,
            "nar_at_config_z": row["nar_at_engine_rate"],
            "nar_at_measured_z": (round(AW.nar_from_pdr(p, z_meas), 4) if p is not None else None),
            "nar_at_z1_single_shot": p,
            "nar_at_reference_10hz": row["nar_at_reference_10hz_rate"],
            "link_state_mix": row["link_state_mix"], "n_pairs": row["n_pairs"]}
    return {
        "dataset_dir": dataset_dir,
        "config": rep["config"],
        "geometry": rep["geometry"],
        "link_state_mix_overall": rep["link_state_mix_overall"],
        "shots": {
            "measured_cams_per_1s_window": sh,
            "z_from_config_dt": z_cfg,
            "z_from_measured_rate": round(z_meas, 4),
            "z_reference_urban": z_ref, "z_reference_range": [z_lo, z_hi],
            "overstatement_factor": (round(z_cfg / z_meas, 4) if z_meas else None),
        },
        "crossings_m": rep["crossings_m"],
        "reference": rep["reference"],
        "verdict": rep["verdict"],
        "anchors": anchors,
        "note": ("nar90_equivalent_range_m is a crossing of the PER-PACKET curve at the level the "
                 "reference's NAR 0.90 implies at the REFERENCE's own Z; no engine rate enters it, "
                 "so the generation rules cannot move it. What they move is nar_at_engine_rate."),
    }


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("dataset_dirs", nargs="+")
    p.add_argument("--json", dest="json_out", default="")
    a = p.parse_args(argv)
    out = {os.path.basename(d.rstrip("/\\")): report(d) for d in a.dataset_dirs}
    txt = json.dumps(out, indent=1, sort_keys=True, default=str)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
