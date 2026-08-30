"""SUMO-side realism gate: GEH over induction-loop detectors + an acceleration-plausibility check.

The MOSAIC/SUMO path is this repo's traffic flagship and every generated InTAS scenario already
carries the real Ingolstadt loop layout (``InTAS_E1.add.xml``, 196 ``<e1Detector>`` elements grouped
into stations by their ``name`` attribute). This script reads SUMO's E1 detector output and
aggregates it into per-station flows.

THERE ARE TWO REFERENCE MODES AND THEY ARE NOT INTERCHANGEABLE.

``--ref-counts`` -- VALIDATION against REAL-WORLD MEASURED COUNTS.
    ``comparison_kind = "fhwa_validation"``. Only this mode may use the FHWA calibration/validation
    vocabulary and the acceptance criteria pinned (with citations) in
    ``src/scms_sim_ref/datagen/refdata/geh_criteria.json`` (GEH < 5 on >= 85% of links, etc.).
    No such measured data is vendored in this repository -- see
    ``src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md`` for the file schema and for
    the candidate open data sources.

``--ref-det-out`` -- REPRODUCIBILITY against ANOTHER SUMO RUN of the same scenario.
    ``comparison_kind = "seed_stability"`` (or ``"regression_same_seed"`` when ``--seed`` and
    ``--ref-seed`` are supplied and equal). This is a run-to-run / seed-to-seed stability check of
    the simulator against itself. It is NOT calibration and NOT validation: nothing in it says the
    model resembles reality. Its gates carry ``seed_stability.*`` / ``regression.*`` ids, their own
    thresholds (derived in ``SEED_STABILITY_BASIS`` below, NOT from FHWA), and the report carries a
    top-level ``warning``.

    # VALIDATION -- real measured counts (JSON {station: veh/h} or CSV station,count[,duration_s])
    python tools/sumo_realism.py --det-out InTAS_Detectors_Output.xml \
        --det-add InTAS_E1.add.xml --ref-counts ref_counts.json --json geh.json

    # REPRODUCIBILITY -- another SUMO run of the same scenario (NOT a validation result)
    python tools/sumo_realism.py --det-out after.xml --ref-det-out before.xml \
        --det-add InTAS_E1.add.xml --seed 23423 --ref-seed 987654

    # acceleration plausibility from an fcd / emission trace (optional, independent of GEH)
    python tools/sumo_realism.py --fcd fcd.xml

Both modes EXCLUDE stations whose modelled and reference flows are both zero: such a station carries
no information and counting it as a pass inflates the pass fraction. The excluded ids are reported in
``both_zero_stations``.

Standalone: only the stdlib + numpy are required. ``sumolib`` (from ``$SUMO_HOME/tools``) is used
when importable, and only to resolve a detector's lane to its edge for ``--group-by edge``; the tool
degrades to the detector ``name``/``id`` grouping when it is absent. Reading is streamed
(``iterparse``) so multi-GB fcd traces do not have to fit in memory.

Exit code: 0 when every evaluated gate passes, 1 when a gate fails, 2 on a usage error.
"""
from __future__ import annotations

import argparse
import csv
import json
import math
import os
import sys
import xml.etree.ElementTree as ET
from pathlib import Path
from statistics import NormalDist

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "src"))
from scms_sim_ref.datagen.realism_bench import (  # noqa: E402
    geh_summary, load_refdata, _ref, _ref_text,
)

MAX_FD_DT_S = 2.0          # finite-difference gap ceiling for fcd-derived accelerations
MIN_ACCEL_SAMPLES = 30

# ------------------------------------------------------------------------------------------------
# reference-mode identity
# ------------------------------------------------------------------------------------------------
KIND_VALIDATION = "fhwa_validation"
KIND_SEED_STABILITY = "seed_stability"
KIND_REGRESSION = "regression_same_seed"

SIM_VS_SIM_WARNING = (
    "NOT A VALIDATION RESULT AND NOT A CALIBRATION RESULT. This report compares one SUMO run "
    "against ANOTHER SUMO RUN of the same scenario (--ref-det-out). It measures only the "
    "run-to-run / seed-to-seed reproducibility of the simulator against itself; nothing in it "
    "states that the model resembles real traffic. No real-world measured count data for the "
    "Ingolstadt (InTAS) induction loops is vendored in this repository -- the InTAS route files "
    "are DEMAND (model input, so grading against them would be circular) and every vendored "
    "InTAS_Detectors_Output.xml under third_party/veremi-nextgen is a config-echo stub with zero "
    "<interval> rows. Do NOT cite these gates as FHWA calibration/validation evidence, and do not "
    "quote the FHWA 'GEH < 5 on >= 85% of links' criterion from them. To obtain a real validation "
    "result, supply measured counts via --ref-counts; see "
    "src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md."
)

# --- seed-stability thresholds -------------------------------------------------------------------
# GEH is NOT scale-free: GEH(k*m, k*c) = sqrt(k) * GEH(m, c). The FHWA GEH < 5 criterion is defined
# on HOURLY volumes, so grading a T-second window that has been extrapolated to veh/h inflates GEH
# by sqrt(3600/T) (x3.464 for T = 300 s). Seed stability is therefore graded on the WINDOW-NATIVE
# counts -- the vehicles the loops actually recorded over the common window -- and the thresholds
# below are derived from the exact sampling null, not borrowed from FHWA.
SEED_STABILITY_BASIS = (
    "Exact conditional null for two runs of the SAME scenario with different RNG seeds: at a "
    "station the two runs record m and c vehicles over the same window; conditional on the total "
    "n = m + c, an unbiased split gives m ~ Binomial(n, 1/2), so (m - c)/sqrt(n) ~ N(0,1) and the "
    "window-native GEH = sqrt(2*(m-c)^2/(m+c)) = sqrt(2) * |Z| exactly. A per-station GEH bound is "
    "therefore a two-sided z-test with a known false-alarm rate, and the bounds below are quoted "
    "as such. These are derived thresholds local to tools/sumo_realism.py; they are NOT an "
    "external standard and are deliberately NOT the FHWA criteria."
)
SEED_STABILITY_ALPHA = 0.05            # per-station two-sided false-alarm rate for the link gate
SEED_STABILITY_FWER = 0.05             # family-wise false-alarm rate for the worst-station gate
SEED_STABILITY_MIN_PASS_FRACTION = 0.85
SEED_STABILITY_TOTAL_REL_TOL = 0.03    # network-total flow tolerance (FHWA validation allows 0.05)
SEED_STABILITY_COUNT_ABS_FLOOR = 3.0   # vehicles; band = max(floor, rel * max(m, c))
SEED_STABILITY_COUNT_REL_TOL = 0.25
# The count band is an ENGINEERING equivalence bound, not a significance test: at a median station
# count of ~10 vehicles per 300 s, Poisson counting noise alone is ~+-30%, so a +-25% / +-3-vehicle
# band cannot be met by every station even for a perfectly stable simulator. Its required pass rate
# is therefore lower than the z-calibrated GEH gate's, while the BAND itself is far tighter than the
# FHWA below_700 tolerance (+-100 veh/h = +-8.33 vehicles over a 300 s window). The gate's job is to
# catch a systematic drift that moves many stations together, not to certify per-station equality.
SEED_STABILITY_COUNT_MIN_PASS_FRACTION = 0.80
SEED_STABILITY_STRICT_GEH = math.sqrt(2.0)   # |Z| < 1: reported tightness indicator, not a gate


def _geh_bound(alpha: float) -> float:
    """Window-native GEH bound equivalent to a two-sided z-test at level ``alpha``."""
    return math.sqrt(2.0) * NormalDist().inv_cdf(1.0 - alpha / 2.0)


# ------------------------------------------------------------------------------------------------
# optional sumolib (lane -> edge resolution only)
# ------------------------------------------------------------------------------------------------
def _maybe_sumolib():
    home = os.environ.get("SUMO_HOME")
    if home:
        tools = os.path.join(home, "tools")
        if os.path.isdir(tools) and tools not in sys.path:
            sys.path.append(tools)
    try:
        import sumolib  # noqa: F401
        return sumolib
    except Exception:
        return None


# ------------------------------------------------------------------------------------------------
# parsing
# ------------------------------------------------------------------------------------------------
def parse_e1_additional(path: str) -> dict[str, dict]:
    """``InTAS_E1.add.xml`` -> ``{detector_id: {"station":…, "lane":…, "edge":…}}``.

    Real loop layouts place several single-lane E1 detectors at one physical counting station and tag
    them with a shared ``name`` (e.g. four ``1010_*`` detectors named ``1010``); ``name`` is therefore
    the station key, falling back to the detector id when the file carries no name.
    """
    out: dict[str, dict] = {}
    for _ev, el in ET.iterparse(path, events=("end",)):
        if el.tag not in ("e1Detector", "inductionLoop"):
            continue
        did = el.get("id")
        if did:
            lane = el.get("lane") or ""
            name = el.get("name")
            out[did] = {"station": name or did, "lane": lane, "named": bool(name),
                        "edge": lane.rsplit("_", 1)[0] if "_" in lane else lane}
        el.clear()
    return out


def parse_e1_output(path: str, begin: float | None = None, end: float | None = None) -> dict:
    """SUMO E1 output -> per-detector vehicle counts plus the covered time span.

    Counts come from ``nVehContrib`` (vehicles that completely passed the loop in the interval),
    which is the count a real loop reports; ``flow`` is a derived veh/h and is only used as a
    fallback when ``nVehContrib`` is absent.
    """
    counts: dict[str, float] = {}
    t0, t1, n_int = None, None, 0
    for _ev, el in ET.iterparse(path, events=("end",)):
        if el.tag != "interval":
            el.clear()
            continue
        did = el.get("id")
        try:
            b = float(el.get("begin", "nan")); e = float(el.get("end", "nan"))
        except (TypeError, ValueError):
            el.clear()
            continue
        if did is None or not (b == b and e == e):     # NaN check without math import
            el.clear()
            continue
        if (begin is not None and b < begin) or (end is not None and e > end):
            el.clear()
            continue
        nv = el.get("nVehContrib")
        if nv is not None:
            c = float(nv)
        else:
            c = float(el.get("flow", 0.0) or 0.0) * max(0.0, e - b) / 3600.0
        counts[did] = counts.get(did, 0.0) + c
        t0 = b if t0 is None else min(t0, b)
        t1 = e if t1 is None else max(t1, e)
        n_int += 1
        el.clear()
    return {"counts": counts, "begin": t0, "end": t1, "n_intervals": n_int,
            "duration_s": (None if t0 is None else max(0.0, t1 - t0))}


def to_station_flows(parsed: dict, mapping: dict[str, dict] | None, group_by: str = "station",
                     duration_s: float | None = None) -> dict[str, float]:
    """Aggregate per-detector counts into per-station (or per-edge) flows in veh/h."""
    dur = duration_s if duration_s else parsed.get("duration_s")
    if not dur or dur <= 0:
        dur = 3600.0
    agg: dict[str, float] = {}
    for did, c in parsed["counts"].items():
        meta = (mapping or {}).get(did) or {}
        key = str(meta.get(group_by) or meta.get("station") or did)
        agg[key] = agg.get(key, 0.0) + float(c)
    return {k: agg[k] * 3600.0 / dur for k in sorted(agg)}


def load_reference_counts(path: str) -> tuple[dict[str, float], str]:
    """Reference flows from JSON or CSV. Returns ``({station: veh/h}, description)``.

    JSON: either ``{station: veh_per_h}`` or ``{"unit": "count"|"veh_per_h", "duration_s": T,
    "stations": {station: value}}``. CSV: a header row with a station column plus one of
    ``flow``/``veh_per_h``/``count``; a ``duration_s`` column (or ``--ref-duration-s``) converts
    counts to veh/h.
    """
    if path.lower().endswith(".json"):
        with open(path, encoding="utf-8") as fh:
            doc = json.load(fh)
        stations = doc.get("stations") if isinstance(doc, dict) and "stations" in doc else doc
        unit = (doc.get("unit") if isinstance(doc, dict) else None) or "veh_per_h"
        dur = float((doc.get("duration_s") if isinstance(doc, dict) else None) or 0.0)
        out = {}
        for k, v in dict(stations).items():
            f = float(v)
            if unit.startswith("count") and dur > 0:
                f = f * 3600.0 / dur
            out[str(k)] = f
        return out, f"{os.path.basename(path)} (unit={unit})"
    with open(path, encoding="utf-8", newline="") as fh:
        rows = list(csv.DictReader(fh))
    if not rows:
        return {}, os.path.basename(path)
    cols = {c.lower(): c for c in rows[0]}
    scol = cols.get("station") or cols.get("id") or cols.get("name") or list(rows[0])[0]
    fcol = cols.get("flow") or cols.get("veh_per_h") or cols.get("veh_h")
    ccol = cols.get("count") or cols.get("nvehcontrib") or cols.get("vehicles")
    dcol = cols.get("duration_s")
    out: dict[str, float] = {}
    for r in rows:
        key = str(r[scol])
        if fcol:
            out[key] = out.get(key, 0.0) + float(r[fcol] or 0.0)
        elif ccol:
            dur = float(r[dcol]) if (dcol and r.get(dcol)) else 3600.0
            out[key] = out.get(key, 0.0) + float(r[ccol] or 0.0) * 3600.0 / max(dur, 1e-9)
    return out, f"{os.path.basename(path)} ({'flow' if fcol else 'count'} column)"


def parse_trace_accelerations(path: str, max_dt: float = MAX_FD_DT_S) -> dict:
    """Per-vehicle accelerations from an fcd-output or emission-output XML (streamed).

    Uses SUMO's own ``acceleration``/``a`` attribute when the trace carries one
    (``--fcd-output.acceleration``); otherwise finite-differences ``speed`` between consecutive
    timesteps of the same vehicle.
    """
    prev: dict[str, tuple[float, float]] = {}
    acc: list[float] = []
    speeds: list[float] = []
    native = 0
    t_now = None
    for ev, el in ET.iterparse(path, events=("start", "end")):
        if ev == "start" and el.tag == "timestep":
            try:
                t_now = float(el.get("time"))
            except (TypeError, ValueError):
                t_now = None
            continue
        if ev != "end":
            continue
        if el.tag == "vehicle" and t_now is not None:
            vid = el.get("id")
            sp = el.get("speed")
            if vid is not None and sp is not None:
                try:
                    v = float(sp)
                except ValueError:
                    el.clear()
                    continue
                speeds.append(v)
                a_attr = el.get("acceleration", el.get("a"))
                if a_attr is not None:
                    try:
                        acc.append(float(a_attr))
                        native += 1
                    except ValueError:
                        pass
                else:
                    p = prev.get(vid)
                    if p is not None:
                        dt = t_now - p[0]
                        if 0.0 < dt <= max_dt:
                            acc.append((v - p[1]) / dt)
                prev[vid] = (t_now, v)
            el.clear()
        elif el.tag == "timestep":
            el.clear()
    return {"accel": np.array(acc, dtype=float), "speed": np.array(speeds, dtype=float),
            "n_vehicles": len(prev), "native_accel_samples": native}


# ------------------------------------------------------------------------------------------------
# gates
# ------------------------------------------------------------------------------------------------
def _gate(gid: str, title: str, value, unit: str, ref: dict | None, n: int | None = None,
          reason: str | None = None, extra: dict | None = None) -> dict:
    if reason is not None or ref is None or value is None:
        status = "na"
        why = reason or ("no reference entry" if ref is None else "no data")
    else:
        v = float(value)
        lo, hi = ref.get("min"), ref.get("max")
        if ref.get("range") is not None:
            status = "pass" if float(ref["range"][0]) <= v <= float(ref["range"][1]) else "fail"
        elif lo is None and hi is None:
            status, why = "na", "reference entry carries no numeric threshold"
            return {"id": gid, "title": title, "value": v, "unit": unit, "n": n,
                    "status": status, "reason": why}
        else:
            status = "pass"
            if lo is not None and v < float(lo):
                status = "fail"
            if hi is not None and v > float(hi):
                status = "fail"
        why = None
    row = {"id": gid, "title": title,
           "value": (None if value is None else round(float(value), 4)),
           "unit": unit, "n": n, "status": status}
    if why:
        row["reason"] = why
    if ref:
        row["reference"] = {"ref_id": ref.get("ref_id"), "threshold": _ref_text(ref),
                            "cite": ref.get("cite"), "source": ref.get("source")}
    if extra:
        row["details"] = extra
    return row


def _flow_tolerance_pass(modelled: float, counted: float, bands: dict) -> bool:
    """FHWA per-link flow tolerance; the band is selected on the COUNTED (reference) flow."""
    if counted < 700.0:
        return abs(modelled - counted) <= float(bands["below_700"]["max_abs_veh_h"])
    if counted <= 2700.0:
        return abs(modelled - counted) <= float(bands["700_to_2700"]["max_rel"]) * counted
    return abs(modelled - counted) <= float(bands["above_2700"]["max_abs_veh_h"])


def _partition(modelled: dict[str, float], counted: dict[str, float]) -> dict:
    """Split the two flow dicts into only-in-X, shared, both-zero and comparable station sets.

    ``both_zero`` stations (no vehicle on either side) carry no information: they are EXCLUDED from
    every pass fraction and reported separately, because scoring them as passes inflates the rate.
    """
    shared = sorted(set(modelled) & set(counted))
    both_zero = [s for s in shared if float(modelled[s]) == 0.0 and float(counted[s]) == 0.0]
    bz = set(both_zero)
    return {
        "only_modelled": sorted(set(modelled) - set(counted)),
        "only_reference": sorted(set(counted) - set(modelled)),
        "shared": shared, "both_zero": both_zero,
        "comparable": [s for s in shared if s not in bz],
    }


def _scale_counts(flows: dict[str, float], window_s: float | None) -> dict[str, float]:
    """veh/h -> vehicles actually recorded over a ``window_s`` window (identity when unknown)."""
    if not window_s or window_s <= 0:
        return dict(flows)
    return {k: v * window_s / 3600.0 for k, v in flows.items()}


def geh_report(modelled: dict[str, float], counted: dict[str, float], refdata: dict,
               window_s: float | None = None) -> dict:
    """VALIDATION mode: per-station GEH + the FHWA acceptance gates, against MEASURED counts.

    Only reachable via ``--ref-counts``. Flows are veh/h on both sides, which is the unit the FHWA
    criteria are written in.
    """
    part = _partition(modelled, counted)
    cmpbl = part["comparable"]
    geh_max = float((_ref(refdata, "geh.geh_link_max") or {}).get("max", 5.0))
    summ = geh_summary(((s, modelled[s], counted[s]) for s in cmpbl), geh_max=geh_max)

    bands = (_ref(refdata, "geh.link_flow_tolerance_bands") or {}).get("value")
    tol_frac = None
    if bands and cmpbl:
        tol_frac = sum(1 for s in cmpbl if _flow_tolerance_pass(modelled[s], counted[s], bands))
        tol_frac /= len(cmpbl)
    tgt = float((_ref(refdata, "geh.geh_calibration_target") or {}).get("max", 3.0))
    no_data = None if cmpbl else "no station id with a non-zero flow is present in both inputs"

    gates = [
        _gate("geh.link_pass_fraction", f"FHWA VALIDATION: stations with GEH < {geh_max:g}",
              summ["pass_fraction"], "fraction",
              _ref(refdata, "geh.geh_link_min_pass_fraction"), summ["n_stations"], reason=no_data,
              extra={"geh_median": summ["geh_median"], "geh_p85": summ["geh_p85"],
                     "n_both_zero_excluded": len(part["both_zero"])}),
        _gate("geh.total_flow_geh", "FHWA VALIDATION: GEH on the summed flow of all stations",
              summ["total_geh"], "GEH", _ref(refdata, "geh.geh_total_max"), summ["n_stations"],
              reason=no_data),
        _gate("geh.total_flow_rel_error", "FHWA VALIDATION: relative error on total flow",
              (abs(summ["total_rel_error"]) if summ["total_rel_error"] is not None else None),
              "fraction", _ref(refdata, "geh.total_flow_tolerance_fraction"), summ["n_stations"],
              reason=no_data),
        _gate("geh.link_flow_tolerance_pass_fraction",
              "FHWA VALIDATION: stations inside the FHWA flow tolerance", tol_frac, "fraction",
              _ref(refdata, "geh.link_flow_min_pass_fraction"), len(cmpbl),
              reason=(no_data or (None if bands else "no link_flow_tolerance_bands reference")),
              extra={"bands": bands}),
    ]
    n_strict = sum(1 for r in summ["stations"] if r["geh"] < tgt)
    notes = []
    if window_s and window_s < 1800.0:
        notes.append(
            f"SHORT WINDOW: these flows are veh/h extrapolated from a {window_s:g} s aggregation. "
            f"GEH is not scale-free (GEH(k*m, k*c) = sqrt(k)*GEH(m, c)) and the FHWA GEH < 5 "
            f"criterion is written for ~1 h volumes, so this comparison inflates GEH by "
            f"sqrt(3600/{window_s:g}) = {math.sqrt(3600.0 / window_s):.4f} relative to the counts "
            f"the loops actually recorded. Prefer a >= 1 h window before quoting the FHWA gates.")
    return {
        "comparison_kind": KIND_VALIDATION,
        "comparison_summary": "modelled SUMO flows vs REAL-WORLD MEASURED counts (--ref-counts)",
        "criteria_source": "src/scms_sim_ref/datagen/refdata/geh_criteria.json "
                           "(FHWA Traffic Analysis Toolbox Vol III, FHWA-HRT-04-040, 2004, sect. 5)",
        "units": "veh_per_h", "window_s": window_s,
        "n_stations_modelled": len(modelled), "n_stations_counted": len(counted),
        "n_stations_shared": len(part["shared"]), "n_stations_compared": len(cmpbl),
        "stations_only_in_modelled": part["only_modelled"][:50],
        "stations_only_in_reference": part["only_reference"][:50],
        "both_zero_stations": part["both_zero"],
        "n_both_zero_excluded": len(part["both_zero"]),
        "geh": summ,
        "calibration_target_pass_fraction": (round(n_strict / len(cmpbl), 4) if cmpbl else None),
        "calibration_target_geh": tgt,
        "notes": notes,
        "gates": gates,
    }


def seed_stability_report(modelled: dict[str, float], counted: dict[str, float],
                          window_s: float | None = None, same_seed: bool = False) -> dict:
    """SIM-vs-SIM mode: run-to-run reproducibility of the simulator against itself.

    NOT calibration and NOT validation -- see ``SIM_VS_SIM_WARNING``. Grading happens on the
    WINDOW-NATIVE counts (the vehicles the loops actually recorded over the common window) because
    GEH is not scale-free and the veh/h extrapolation of a short window inflates it by
    ``sqrt(3600/window_s)``. Thresholds come from ``SEED_STABILITY_BASIS``, never from FHWA.
    """
    part = _partition(modelled, counted)
    cmpbl = part["comparable"]
    n = len(cmpbl)
    scale = math.sqrt(3600.0 / window_s) if (window_s and window_s > 0) else 1.0
    m_cnt, c_cnt = _scale_counts(modelled, window_s), _scale_counts(counted, window_s)

    link_bound = round(_geh_bound(SEED_STABILITY_ALPHA), 4)
    # Bonferroni over the N compared stations: the WHOLE gate keeps a family-wise alpha
    fwer_bound = (round(_geh_bound(SEED_STABILITY_FWER / n), 4) if n else None)
    summ = geh_summary(((s, m_cnt[s], c_cnt[s]) for s in cmpbl), geh_max=link_bound)
    for row in summ["stations"]:
        row["geh_veh_h"] = round(row["geh"] * scale, 4)                 # continuity with FHWA mode

    geh_max_obs = max((r["geh"] for r in summ["stations"]), default=None)
    worst = max(summ["stations"], key=lambda r: r["geh"], default=None) if n else None
    exact = sum(1 for s in cmpbl if m_cnt[s] == c_cnt[s])
    strict = sum(1 for r in summ["stations"] if r["geh"] < SEED_STABILITY_STRICT_GEH)
    tol_ok = [s for s in cmpbl
              if abs(m_cnt[s] - c_cnt[s]) <= max(SEED_STABILITY_COUNT_ABS_FLOOR,
                                                 SEED_STABILITY_COUNT_REL_TOL
                                                 * max(m_cnt[s], c_cnt[s]))]
    tot_m, tot_c = sum(m_cnt[s] for s in cmpbl), sum(c_cnt[s] for s in cmpbl)
    rel_err = (abs(tot_m - tot_c) / tot_c) if tot_c else None
    split_z = ((tot_m - tot_c) / math.sqrt(tot_m + tot_c)) if (tot_m + tot_c) > 0 else None
    no_data = None if n else "no station id with a non-zero count is present in both runs"
    # Every bound below is stated in VEHICLES over the common window. Without a common window the
    # values are veh/h and neither the absolute floor nor the sqrt(2)|Z| identity holds, so the
    # count band is reported n/a rather than silently graded on the wrong scale.
    no_window = (None if (window_s and window_s > 0) else
                 "the two runs do not share an aggregation window, so per-station VEHICLE counts "
                 "are not recoverable and an absolute vehicle band is not meaningful")

    def _r(ref_id, *, mn=None, mx=None, note=""):
        return {"ref_id": ref_id, "min": mn, "max": mx,
                "cite": "tools/sumo_realism.py SEED_STABILITY_BASIS (derived; NOT an external "
                        "standard and NOT the FHWA criteria)",
                "source": SEED_STABILITY_BASIS + (" " + note if note else "")}

    if same_seed:
        gates = [
            _gate("regression.identical_station_counts",
                  "REGRESSION (same seed): stations whose counts are bit-identical to the baseline",
                  (exact / n if n else None), "fraction",
                  _r("regression.identical_station_counts", mn=1.0,
                     note="Two runs of the same scenario with the SAME seed must reproduce "
                          "exactly; any non-zero delta is a regression, not sampling noise."),
                  n, reason=no_data, extra={"n_stations_differing": (n - exact) if n else None}),
            _gate("regression.max_abs_count_delta",
                  "REGRESSION (same seed): largest per-station count delta vs the baseline",
                  (max((abs(m_cnt[s] - c_cnt[s]) for s in cmpbl), default=0.0) if n else None),
                  "vehicles",
                  _r("regression.max_abs_count_delta", mx=0.0,
                     note="Same-seed runs must agree exactly, so the tolerance is zero."),
                  n, reason=no_data),
        ]
    else:
        gates = [
            _gate("seed_stability.link_geh_pass_fraction",
                  f"SEED-STABILITY: stations whose two-seed count split is inside the "
                  f"alpha={SEED_STABILITY_ALPHA:g} band (window-native GEH < {link_bound:.4f})",
                  summ["pass_fraction"], "fraction",
                  _r("seed_stability.link_geh_alpha", mn=SEED_STABILITY_MIN_PASS_FRACTION,
                     note=f"Bound {link_bound:.4f} = sqrt(2)*z_(1-alpha/2) at alpha="
                          f"{SEED_STABILITY_ALPHA:g}, so under the null each station fails with "
                          f"probability {SEED_STABILITY_ALPHA:g}; requiring "
                          f"{SEED_STABILITY_MIN_PASS_FRACTION:g} of N stations leaves a small "
                          f"documented false-alarm budget (~1.5% at N=22). This is 1.80x tighter "
                          f"than the FHWA GEH < 5 link bound ON THE SAME WINDOW-NATIVE SCALE."),
                  n, reason=no_data,
                  extra={"geh_median": summ["geh_median"], "geh_p85": summ["geh_p85"],
                         "geh_max": (round(geh_max_obs, 4) if geh_max_obs is not None else None),
                         "n_both_zero_excluded": len(part["both_zero"])}),
            _gate("seed_stability.max_link_geh",
                  "SEED-STABILITY: worst station's window-native GEH (Bonferroni family-wise "
                  f"alpha={SEED_STABILITY_FWER:g} over N stations)",
                  geh_max_obs, "GEH",
                  (_r("seed_stability.max_link_geh_fwer", mx=fwer_bound,
                      note=f"Bound {fwer_bound:.4f} = sqrt(2)*z_(1-alpha/(2N)) with alpha="
                           f"{SEED_STABILITY_FWER:g} and N={n}: under the null the WHOLE gate "
                           f"raises a false alarm with probability <= {SEED_STABILITY_FWER:g}. "
                           f"This is the gate that catches a change localised to one loop.")
                   if fwer_bound is not None else None),
                  n, reason=no_data,
                  extra=({"worst_station": worst["station"], "modelled": worst["modelled"],
                          "reference": worst["counted"]} if worst else None)),
            _gate("seed_stability.total_flow_rel_error",
                  "SEED-STABILITY: relative difference of the network-total loop count",
                  rel_err, "fraction",
                  _r("seed_stability.total_flow_rel_tol", mx=SEED_STABILITY_TOTAL_REL_TOL,
                     note=f"{SEED_STABILITY_TOTAL_REL_TOL:g} vs the 0.05 FHWA VALIDATION allows: "
                          f"demand is a fixed route file, so the network-wide loop total is not "
                          f"free to drift with the seed -- only crossing times shift. The 0.6 "
                          f"ratio mirrors FHWA's own calibration-to-validation tightening "
                          f"(GEH 3 vs 5)."),
                  n, reason=no_data,
                  extra={"total_modelled_vehicles": round(tot_m, 3),
                         "total_reference_vehicles": round(tot_c, 3),
                         "total_split_z": (round(split_z, 4) if split_z is not None else None)}),
            _gate("seed_stability.link_count_tolerance_pass_fraction",
                  "SEED-STABILITY: stations inside the per-station count band "
                  f"|delta| <= max({SEED_STABILITY_COUNT_ABS_FLOOR:g} veh, "
                  f"{SEED_STABILITY_COUNT_REL_TOL:g}*max(m,c))",
                  (len(tol_ok) / n if n else None), "fraction",
                  _r("seed_stability.link_count_tolerance",
                     mn=SEED_STABILITY_COUNT_MIN_PASS_FRACTION,
                     note=f"An ENGINEERING equivalence bound, complementary to the z-calibrated "
                          f"GEH gate: a relative-only tolerance is meaningless at 1-3 vehicles, "
                          f"hence the absolute floor. Every station in this layout sits in FHWA's "
                          f"below_700 band, whose tolerance is +-100 veh/h "
                          f"(= +-{100.0 * (window_s or 3600.0) / 3600.0:.2f} vehicles over this "
                          f"window), so the band is far tighter than FHWA's. The required pass "
                          f"rate is {SEED_STABILITY_COUNT_MIN_PASS_FRACTION:g} rather than "
                          f"{SEED_STABILITY_MIN_PASS_FRACTION:g} because pure counting noise at "
                          f"these station counts (~+-30% at 10 vehicles) already pushes some "
                          f"stations outside the band; the gate targets a systematic drift that "
                          f"moves many stations together, not per-station equality."),
                  n, reason=(no_data or no_window),
                  extra={"abs_floor_vehicles": SEED_STABILITY_COUNT_ABS_FLOOR,
                         "rel_tolerance": SEED_STABILITY_COUNT_REL_TOL,
                         "stations_outside_band": [s for s in cmpbl if s not in set(tol_ok)]}),
        ]

    return {
        "comparison_kind": (KIND_REGRESSION if same_seed else KIND_SEED_STABILITY),
        "comparison_summary": ("modelled SUMO run vs ANOTHER SUMO RUN of the same scenario "
                               "(--ref-det-out) -- reproducibility only, NOT validation"),
        "warning": SIM_VS_SIM_WARNING,
        "criteria_source": "tools/sumo_realism.py SEED_STABILITY_BASIS (derived in-tool; the FHWA "
                           "criteria in refdata/geh_criteria.json are deliberately NOT applied)",
        "units": ("vehicles_per_window" if (window_s and window_s > 0) else "veh_per_h"),
        "window_s": window_s,
        "veh_h_inflation_factor": round(scale, 4),
        "veh_h_scaling_note": (
            "Per-station 'modelled'/'counted' are the vehicles recorded over the common "
            f"{window_s:g} s window. Multiply a GEH by {scale:.4f} to get the value the veh/h "
            "extrapolation would have produced; that inflated form is what an FHWA-style GEH < 5 "
            "gate would have been scoring." if (window_s and window_s > 0) else
            "Window duration unknown; flows were compared in veh/h as supplied."),
        "n_stations_modelled": len(modelled), "n_stations_counted": len(counted),
        "n_stations_shared": len(part["shared"]), "n_stations_compared": n,
        "stations_only_in_modelled": part["only_modelled"][:50],
        "stations_only_in_reference": part["only_reference"][:50],
        "both_zero_stations": part["both_zero"],
        "n_both_zero_excluded": len(part["both_zero"]),
        "geh": summ,
        "exact_match_fraction": (round(exact / n, 4) if n else None),
        "strict_agreement_fraction": (round(strict / n, 4) if n else None),
        "strict_agreement_geh": round(SEED_STABILITY_STRICT_GEH, 4),
        "gates": gates,
    }


def accel_report(trace: dict, refdata: dict) -> dict:
    """Acceleration-plausibility gates over an fcd/emission trace."""
    a = trace["accel"]
    n = int(a.size)
    hard = _ref(refdata, "kinematics.accel_hard_bound_mps2")
    comf = _ref(refdata, "kinematics.accel_comfort_band_mps2")
    few = None if n >= MIN_ACCEL_SAMPLES else f"only {n} acceleration samples (need {MIN_ACCEL_SAMPLES})"
    lo, hi = (hard or {}).get("range", (-8.0, 4.0))
    clo, chi = (comf or {}).get("range", (-3.0, 3.0))
    f_hard = float(np.mean((a >= float(lo)) & (a <= float(hi)))) if n else None
    f_comf = float(np.mean((a >= float(clo)) & (a <= float(chi)))) if n else None
    hard_gate = ({"ref_id": "kinematics.accel_hard_bound_mps2", "min": 1.0,
                  "cite": (hard or {}).get("cite"), "source": (hard or {}).get("source")}
                 if hard else None)
    return {
        "n_accel_samples": n, "n_vehicles": trace["n_vehicles"],
        "native_accel_samples": trace["native_accel_samples"],
        "accel_min": (round(float(a.min()), 4) if n else None),
        "accel_max": (round(float(a.max()), 4) if n else None),
        "accel_p01": (round(float(np.percentile(a, 1)), 4) if n else None),
        "accel_p99": (round(float(np.percentile(a, 99)), 4) if n else None),
        "speed_max_mps": (round(float(trace["speed"].max()), 4) if trace["speed"].size else None),
        "gates": [
            _gate("accel.within_hard_bound_frac", "Accelerations inside the human plausibility bound",
                  f_hard, "fraction", hard_gate, n, reason=few,
                  extra={"band_mps2": [float(lo), float(hi)]}),
            _gate("accel.within_comfort_frac", "Accelerations inside the comfort band",
                  f_comf, "fraction", _ref(refdata, "kinematics.accel_comfort_min_fraction"), n,
                  reason=few, extra={"band_mps2": [clo, chi]}),
        ],
    }


# ------------------------------------------------------------------------------------------------
# CLI
# ------------------------------------------------------------------------------------------------
def build(args) -> dict:
    # provenance labels are optional and callers may pass a bare Namespace without them
    seed = getattr(args, "seed", None)
    ref_seed = getattr(args, "ref_seed", None)
    refdata = load_refdata(args.refdata)
    out: dict = {"refdata_dir": refdata.get("dir"),
                 "refdata_sets": sorted(refdata.get("sets", {})), "sections": {}}

    mapping = None
    if args.det_add:
        mapping = parse_e1_additional(args.det_add)
        # `n_stations` counts DISTINCT GROUPING KEYS, which is not the same as the number of real
        # counting stations: a detector with no @name falls back to its own id and becomes a
        # pseudo-station. InTAS_E1.add.xml carries 196 e1Detectors = 194 loops in 25 named station
        # groups + 2 unnamed scenario-gate detectors ("income"/"outgoing", writing to gate.xml).
        # Only the named groups are keys a --ref-counts file may use.
        unnamed = sorted(d for d, m in mapping.items() if m["station"] == d and not m.get("named"))
        out["detector_layout"] = {
            "file": args.det_add, "n_detectors": len(mapping),
            "n_stations": len({m["station"] for m in mapping.values()}),
            "n_named_stations": len({m["station"] for m in mapping.values() if m.get("named")}),
            "named_stations": sorted({m["station"] for m in mapping.values() if m.get("named")}),
            "unnamed_detectors": unnamed[:50],
            "n_edges": len({m["edge"] for m in mapping.values() if m["edge"]}),
            "note": "n_stations counts distinct grouping keys; detectors without an @name fall "
                    "back to their own id. Only named_stations are valid --ref-counts keys.",
        }
        if args.group_by == "edge" and _maybe_sumolib() is None:
            out.setdefault("notes", []).append(
                "sumolib is not importable; --group-by edge falls back to the lane-id prefix, which "
                "is the edge id for standard SUMO lane naming (<edge>_<index>).")

    if args.det_out:
        parsed = parse_e1_output(args.det_out, args.begin, args.end)
        modelled = to_station_flows(parsed, mapping, args.group_by, args.duration_s)
        m_dur = args.duration_s or parsed["duration_s"]
        out["modelled"] = {"file": args.det_out, "n_intervals": parsed["n_intervals"],
                           "begin_s": parsed["begin"], "end_s": parsed["end"],
                           "duration_s": m_dur, "n_keys": len(modelled),
                           "group_by": args.group_by, "seed": seed}
        counted, desc, ref_kind, r_dur = {}, None, None, None
        if args.ref_counts:
            counted, desc = load_reference_counts(args.ref_counts)
            ref_kind = KIND_VALIDATION
        elif args.ref_det_out:
            rp = parse_e1_output(args.ref_det_out, args.begin, args.end)
            counted = to_station_flows(rp, mapping, args.group_by, args.ref_duration_s)
            r_dur = args.ref_duration_s or rp["duration_s"]
            desc = (f"{os.path.basename(args.ref_det_out)} -- ANOTHER SUMO RUN of the same "
                    f"scenario (simulated, NOT measured real-world counts)")
            ref_kind = KIND_SEED_STABILITY

        if counted and ref_kind == KIND_VALIDATION:
            out["comparison_kind"] = KIND_VALIDATION
            out["reference"] = {
                "source": desc, "file": args.ref_counts, "n_keys": len(counted),
                "kind": "real_world_measured_counts", "is_real_world_counts": True,
                "is_simulation_output": False,
                "schema_doc": "src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md"}
            out["sections"]["geh"] = geh_report(modelled, counted, refdata, window_s=m_dur)
        elif counted:
            same_seed = bool(seed and ref_seed and str(seed) == str(ref_seed))
            win = m_dur if (m_dur and r_dur and abs(m_dur - r_dur) <= 0.01 * m_dur) else None
            out["comparison_kind"] = KIND_REGRESSION if same_seed else KIND_SEED_STABILITY
            out["warning"] = SIM_VS_SIM_WARNING
            out["reference"] = {
                "source": desc, "file": args.ref_det_out, "n_keys": len(counted),
                "kind": "sumo_detector_output_of_another_run", "is_real_world_counts": False,
                "is_simulation_output": True, "duration_s": r_dur, "seed": ref_seed,
                "schema_doc": "src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md"}
            if win is None and m_dur and r_dur:
                out.setdefault("notes", []).append(
                    f"modelled window {m_dur:g} s and reference window {r_dur:g} s differ; the "
                    f"window-native count comparison was skipped and veh/h flows were graded "
                    f"instead, which inflates GEH by sqrt(3600/T) on each side.")
            out["sections"]["geh"] = seed_stability_report(modelled, counted, window_s=win,
                                                           same_seed=same_seed)
        else:
            out["comparison_kind"] = "none"
            out["sections"]["geh"] = {
                "status": "na",
                "reason": "no reference counts supplied (pass --ref-counts or --ref-det-out)",
                "modelled_flows_veh_h": {k: round(v, 3) for k, v in modelled.items()},
            }

    if args.fcd:
        out["sections"]["accel"] = accel_report(parse_trace_accelerations(args.fcd), refdata)
        out["sections"]["accel"]["file"] = args.fcd

    gates = []
    for sect in out["sections"].values():
        gates.extend(sect.get("gates", []) if isinstance(sect, dict) else [])
    out["summary"] = {
        "n_gates": len(gates),
        "pass": sum(1 for g in gates if g["status"] == "pass"),
        "fail": sum(1 for g in gates if g["status"] == "fail"),
        "na": sum(1 for g in gates if g["status"] == "na"),
        "failures": [g["id"] for g in gates if g["status"] == "fail"],
    }
    # comparison_kind / warning must be the FIRST thing a reader (or a diff) sees, not buried
    # after the sections -- mislabelling this report is the exact failure mode being guarded here.
    head = [k for k in ("comparison_kind", "warning", "notes", "modelled", "reference",
                        "detector_layout") if k in out]
    return {**{k: out[k] for k in head}, **{k: v for k, v in out.items() if k not in head}}


_KIND_HEADLINE = {
    KIND_VALIDATION: "VALIDATION vs REAL-WORLD MEASURED COUNTS (FHWA criteria apply)",
    KIND_SEED_STABILITY: "SEED-STABILITY vs ANOTHER SUMO RUN (reproducibility only -- "
                         "NOT validation, NOT calibration)",
    KIND_REGRESSION: "REGRESSION vs ANOTHER SUMO RUN AT THE SAME SEED (must reproduce exactly -- "
                     "NOT validation, NOT calibration)",
}


def _wrap(text: str, width: int = 96, indent: str = "  ") -> list[str]:
    out, line = [], indent
    for word in text.split():
        if len(line) + len(word) + 1 > width and line.strip():
            out.append(line.rstrip())
            line = indent
        line += word + " "
    if line.strip():
        out.append(line.rstrip())
    return out


def render(report: dict) -> str:
    L = ["# SUMO realism gate", ""]
    kind = report.get("comparison_kind")
    if kind in _KIND_HEADLINE:
        L.append(f"COMPARISON KIND: {kind} -- {_KIND_HEADLINE[kind]}")
        L.append("")
    if report.get("warning"):
        L.append("!! WARNING !!")
        L.extend(_wrap(report["warning"]))
        L.append("")
    if "detector_layout" in report:
        dl = report["detector_layout"]
        L.append(f"- Detector layout `{dl['file']}`: {dl['n_detectors']} loops -> "
                 f"{dl['n_stations']} grouping keys ({dl.get('n_named_stations')} named stations + "
                 f"{len(dl.get('unnamed_detectors') or [])} unnamed) across {dl['n_edges']} edges")
    if "modelled" in report:
        m = report["modelled"]
        L.append(f"- Modelled `{m['file']}`: {m['n_intervals']} intervals, "
                 f"{m['begin_s']}-{m['end_s']} s, {m['n_keys']} {m['group_by']}(s)")
    if "reference" in report:
        L.append(f"- Reference: {report['reference']['source']} "
                 f"({report['reference']['n_keys']} keys)")
    g = report["sections"].get("geh")
    if g and "geh" in g:
        s = g["geh"]
        unit = "veh/h" if g.get("units") == "veh_per_h" else "vehicles/window"
        L.append("")
        L.append(f"## GEH [{g.get('comparison_kind')}] "
                 f"({g['n_stations_compared']} comparable stations, unit {unit}"
                 + (f", window {g['window_s']:g} s" if g.get("window_s") else "") + ")")
        L.append(f"- median GEH {s['geh_median']}, p85 {s['geh_p85']}, "
                 f"pass fraction {s['pass_fraction']} at GEH < {s['geh_max_threshold']}")
        L.append(f"- total modelled {s['total_modelled']} vs reference {s['total_counted']} "
                 f"-> GEH {s['total_geh']}, relative error {s['total_rel_error']}")
        if g.get("n_both_zero_excluded"):
            L.append(f"- EXCLUDED {g['n_both_zero_excluded']} both-zero station(s) "
                     f"{g['both_zero_stations']} from every pass fraction "
                     f"(no traffic on either side -> no information)")
        if g.get("veh_h_inflation_factor", 1.0) != 1.0:
            L.append(f"- GEH here is window-native; the veh/h extrapolation of this window would "
                     f"multiply every GEH by {g['veh_h_inflation_factor']}")
        if g.get("exact_match_fraction") is not None:
            L.append(f"- stations identical between the two runs: {g['exact_match_fraction']}; "
                     f"within 1 sigma of a fair split (GEH < {g['strict_agreement_geh']}): "
                     f"{g['strict_agreement_fraction']}")
        for note in g.get("notes") or []:
            L.extend(_wrap("NOTE: " + note, indent="  "))
    elif g:
        L.append("")
        L.append(f"## GEH — unavailable: {g.get('reason')}")
    a = report["sections"].get("accel")
    if a:
        L.append("")
        L.append(f"## Acceleration plausibility ({a['n_accel_samples']} samples over "
                 f"{a['n_vehicles']} vehicles)")
        L.append(f"- range {a['accel_min']}..{a['accel_max']} m/s^2 "
                 f"(p01 {a['accel_p01']}, p99 {a['accel_p99']}); max speed {a['speed_max_mps']} m/s")
    L.append("")
    L.append("## Gates")
    for sect in report["sections"].values():
        for gate in (sect.get("gates", []) if isinstance(sect, dict) else []):
            icon = {"pass": "PASS", "fail": "FAIL", "na": "n/a "}[gate["status"]]
            ref = gate.get("reference") or {}
            tail = (f" (ref {ref.get('threshold')}; {ref.get('cite')})" if ref else
                    f" — {gate.get('reason', '')}")
            L.append(f"- [{icon}] {gate['title']}: {gate['value']} {gate['unit']}{tail}")
    if not any(sect.get("gates") for sect in report["sections"].values() if isinstance(sect, dict)):
        L.append("- (no gate was evaluated)")
    L.append("")
    return "\n".join(L)


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0],
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--det-out", help="SUMO E1 detector output XML (the modelled flows)")
    p.add_argument("--det-add", help="E1 additional file (e.g. InTAS_E1.add.xml) for detector->station mapping")
    p.add_argument("--ref-counts",
                   help="VALIDATION mode: REAL-WORLD MEASURED counts, JSON {station: veh/h} or CSV "
                        "station,flow|count (schema: refdata/geh_reference_counts.README.md). "
                        "Only this mode grades against the FHWA criteria.")
    p.add_argument("--ref-det-out",
                   help="REPRODUCIBILITY mode: E1 detector output of ANOTHER SUMO RUN of the same "
                        "scenario. Produces comparison_kind=seed_stability (or regression_same_seed "
                        "when --seed == --ref-seed) -- NOT a validation or calibration result.")
    p.add_argument("--seed", default=None,
                   help="provenance label for the modelled run's RNG seed (free-form, recorded "
                        "verbatim in the report)")
    p.add_argument("--ref-seed", default=None,
                   help="provenance label for the --ref-det-out run's seed; when it equals --seed "
                        "the comparison becomes a strict same-seed regression check")
    p.add_argument("--fcd", help="fcd-output or emission-output XML for the acceleration gate")
    p.add_argument("--group-by", choices=("station", "edge", "lane"), default="station",
                   help="aggregation key for detector flows (default: station = e1Detector @name)")
    p.add_argument("--begin", type=float, default=None, help="only use intervals starting at/after this time")
    p.add_argument("--end", type=float, default=None, help="only use intervals ending at/before this time")
    p.add_argument("--duration-s", type=float, default=None,
                   help="override the modelled aggregation duration used to convert counts to veh/h")
    p.add_argument("--ref-duration-s", type=float, default=None, help="same override for --ref-det-out")
    p.add_argument("--refdata", default=None,
                   help="reference-summary directory (default: src/scms_sim_ref/datagen/refdata)")
    p.add_argument("--json", dest="json_out", default=None, help="write the report JSON here")
    p.add_argument("--no-fail", action="store_true", help="always exit 0 (measure-only)")
    a = p.parse_args(argv)

    if not a.det_out and not a.fcd:
        p.error("nothing to do: pass --det-out (GEH) and/or --fcd (acceleration gate)")
    if a.ref_counts and a.ref_det_out:
        p.error("--ref-counts (validation against measured counts) and --ref-det-out "
                "(reproducibility against another simulation) are different comparisons and "
                "cannot be combined; pass exactly one")
    for flag, path in (("--det-out", a.det_out), ("--det-add", a.det_add),
                       ("--ref-counts", a.ref_counts), ("--ref-det-out", a.ref_det_out),
                       ("--fcd", a.fcd)):
        if path and not os.path.exists(path):
            p.error(f"{flag}: file not found: {path}")

    report = build(a)
    print(render(report))
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(report, fh, indent=2, default=str)
            fh.write("\n")
        print(f"[wrote {a.json_out}]")
    return 0 if (a.no_fail or not report["summary"]["failures"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
