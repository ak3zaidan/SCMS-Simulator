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
    thresholds (MEASURED per scenario -- see ``--calibrate`` and ``SEED_STABILITY_BASIS`` below, NOT
    taken from FHWA), and the report carries a top-level ``warning``.

    # VALIDATION -- real measured counts (JSON {station: veh/h} or CSV station,count[,duration_s])
    python tools/sumo_realism.py --det-out InTAS_Detectors_Output.xml \
        --det-add InTAS_E1.add.xml --ref-counts ref_counts.json --json geh.json

    # REPRODUCIBILITY -- another SUMO run of the same scenario (NOT a validation result)
    python tools/sumo_realism.py --det-out after.xml --ref-det-out before.xml \
        --det-add InTAS_E1.add.xml --seed 23423 --ref-seed 987654

    # acceleration plausibility from an fcd / emission trace (optional, independent of GEH)
    python tools/sumo_realism.py --fcd fcd.xml

    # CALIBRATION -- derive the seed-stability null EMPIRICALLY from K runs of the scenario
    python tools/sumo_realism.py --calibrate --sumocfg .../InTAS_buildings.sumocfg \
        --det-add .../InTAS_E1.add.xml --end 300 --seeds 1-20 --scenario-id intas_urban_low

The seed-stability thresholds are NOT invented. They are measured: ``--calibrate`` runs K seeds of
the scenario, forms all K(K-1)/2 same-scenario run pairs, and reads the null distribution of every
gate statistic straight off those pairs (see ``SEED_STABILITY_BASIS``). The result is written to
``tools/calibration/seed_stability_<scenario_id>.json`` -- deliberately NOT into
``src/scms_sim_ref/datagen/refdata/``, which holds externally cited literature values only, while a
calibration is a property of one scenario on one SUMO build. Raw per-seed SUMO output stays in the
gitignored ``.realism_cache/`` and is never committed.

WITHOUT a calibration on disk the tool falls back to a parametric ``Binomial(n, 1/2)`` null that is
KNOWN TO BE WRONG for this scenario (measured index of dispersion ~0.22, not 1.0), so every
would-be-passing gate is demoted to ``na`` and the whole report is stamped ``uncalibrated``: a
fallback run can raise an alarm but can never be quoted as a passing gate.

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
import datetime as _dt
import hashlib
import itertools
import json
import math
import os
import random
import statistics
import subprocess
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

# --- seed-stability thresholds: MEASURED, not assumed ---------------------------------------------
# GEH is NOT scale-free: GEH(k*m, k*c) = sqrt(k) * GEH(m, c). The FHWA GEH < 5 criterion is defined
# on HOURLY volumes, so grading a T-second window that has been extrapolated to veh/h inflates GEH
# by sqrt(3600/T) (x3.464 for T = 300 s). Seed stability is therefore graded on the WINDOW-NATIVE
# counts -- the vehicles the loops actually recorded over the common window.
#
# HISTORY (why this section was rewritten). Until 2026-08-30 both seed-stability nulls were INVENTED
# rather than measured, and an adversarial review (docs/realism/REVIEW-FINDINGS.md C1/C2) measured
# how badly:
#
#   C1  A network-total tolerance of 0.03 was justified by "demand is a fixed route file, so the
#       network-wide loop total is not free to drift with the seed -- only crossing times shift."
#       FALSE: every InTAS vehicle carries a <routeDistribution> and InTAS_buildings.sumocfg sets
#       device.rerouting.probability = 0.82, so the seed changes ROUTE CHOICE, not just crossing
#       times. Measured network loop totals over 20 seeds of intas_urban_low @300 s span 226..247
#       ((max-min)/mean = 0.088); the 0.03 gate false-alarmed on 86 of 190 seed pairs (45.3%).
#   C2  A per-station bound of sqrt(2)*z_(1-alpha/2) = 2.7718 assumed m | n ~ Binomial(n, 1/2), whose
#       index of dispersion is 1.0. MEASURED index of dispersion over the same 20 seeds: 0.218
#       (per-station median 0.175) -- route choice is a Poisson-binomial over shared vehicles, so
#       counts are strongly UNDER-dispersed. Per-station exceedance of 2.7718 was 9/4269 = 0.0021
#       against a documented 0.05, i.e. ~24x conservative, so the gate had essentially no power:
#       against a reference of c = 10 vehicles every m in [3, 20] passed and a 2x flow change went
#       undetected (GEH(20, 10) = 2.5820 < 2.7718).
#
# Both defects have the same root cause -- a parametric null asserted for a quantity that can simply
# be MEASURED -- and the same fix: --calibrate runs K seeds of the scenario and reads every gate's
# null distribution off the K(K-1)/2 same-scenario run pairs.
SEED_STABILITY_BASIS = (
    "EMPIRICALLY CALIBRATED null. The scenario is run at K different RNG seeds and every one of the "
    "K(K-1)/2 same-scenario run pairs is scored with the same statistics the gate uses, so the null "
    "distribution of each statistic is measured rather than assumed. Two summaries drive the "
    "thresholds: (1) the index of dispersion phi = E[(m-c)^2/(m+c)] pooled over (pair, station), "
    "which is exactly the quantity the discarded Binomial(n, 1/2) null fixed at 1.0, giving a "
    "window-native GEH bound sqrt(2*phi)*z_(1-a/2) that degenerates to the old sqrt(2)*z_(1-a/2) "
    "when phi = 1; and (2) the coefficient of variation of the network-total loop count across "
    "seeds, giving a total-flow tolerance sqrt(2)*cv*z_(1-a/2). Each threshold is the LOOSER of that "
    "moment form (evaluated at a seed-level resampling upper confidence limit on phi / cv) and the "
    "direct empirical (1-a) quantile of the same statistic over the pairs, so a heavier-than-normal "
    "tail cannot be hidden by the moment form and a short calibration cannot be over-fitted to its "
    "own extremes. The common per-gate level a is then chosen as the LARGEST value in "
    "CALIB_GATE_LEVEL_DIVISORS for which the whole report's measured false-alarm rate over the "
    "calibration pairs is <= the report-level alpha, which maximises power subject to the "
    "report-level false-alarm target. These are thresholds LOCAL to one scenario on one SUMO build; "
    "they are NOT an external standard and are deliberately NOT the FHWA criteria."
)

CALIBRATION_SCHEMA = "sumo_realism/seed_stability_calibration/1"
DEFAULT_CALIBRATION_DIR = ROOT / "tools" / "calibration"
# Raw per-seed SUMO output is bulky and, for any future measured-count fetch, licence-encumbered:
# it lives here and this path is gitignored. Nothing under it is ever committed.
DEFAULT_CALIB_CACHE = ROOT / ".realism_cache" / "seed_runs"

SEED_STABILITY_REPORT_ALPHA = 0.05     # REPORT-level false-alarm target (the whole gate set)
CALIB_GATE_LEVEL_DIVISORS = (4.0, 3.0, 2.5, 2.0, 1.75, 1.5, 1.25, 1.0)
CALIB_BOOTSTRAP_B = 1200               # seed-level resamples for the phi / cv upper limits
CALIB_BOOTSTRAP_GAMMA = 0.05           # upper-confidence level for those limits
CALIB_BOOTSTRAP_SEED = 20260830        # pinned so a calibration is reproducible bit-for-bit
CALIB_MIN_SEEDS = 10                   # below this --calibrate refuses to write a file
CALIB_RECOMMENDED_SEEDS = 20           # below this the calibration is flagged low_confidence

SEED_STABILITY_COUNT_ABS_FLOOR = 3.0   # vehicles; band = max(floor, rel * max(m, c))
SEED_STABILITY_COUNT_REL_TOL = 0.25
# The count band is an ENGINEERING equivalence bound, not a significance test: at a median station
# count of ~10 vehicles per 300 s, counting noise alone is tens of percent, so a +-25% / +-3-vehicle
# band cannot be met by every station even for a perfectly stable simulator. The BAND itself is far
# tighter than the FHWA below_700 tolerance (+-100 veh/h = +-8.33 vehicles over a 300 s window); what
# is calibrated is the required PASS RATE, not the band. The gate's job is to catch a systematic
# drift that moves many stations together, not to certify per-station equality.
SEED_STABILITY_STRICT_GEH = math.sqrt(2.0)   # |Z| < 1: reported tightness indicator, not a gate

# --- parametric FALLBACK (used only when no calibration is on disk) -------------------------------
# Retained so the tool still says something on an uncalibrated scenario, but every would-be PASS it
# produces is demoted to `na`: see UNCALIBRATED_* below for exactly why each direction is or is not
# trustworthy.
FALLBACK_BASIS = (
    "PARAMETRIC FALLBACK, NOT A CALIBRATION. Conditional null for two runs of the SAME scenario "
    "with different RNG seeds: at a station the two runs record m and c vehicles over the same "
    "window; conditional on n = m + c an unbiased split gives m ~ Binomial(n, 1/2), so "
    "(m - c)/sqrt(n) ~ N(0,1) and the window-native GEH = sqrt(2*(m-c)^2/(m+c)) = sqrt(2)*|Z|. The "
    "ALGEBRA is exact but the GENERATIVE MODEL is known to be wrong wherever vehicles carry route "
    "distributions and rerouting is enabled: Binomial(n, 1/2) implies an index of dispersion of "
    "1.0, whereas route choice is a Poisson-binomial over shared vehicles (Var = sum p(1-p) <= "
    "sum p) and the measured index of dispersion on intas_urban_low is 0.218. The bound is "
    "therefore roughly sqrt(1/0.218) = 2.1x too loose and the gate has correspondingly little "
    "power, which is why a pass under this fallback is reported as `na` and never as a pass."
)
FALLBACK_ALPHA = 0.05                  # per-station two-sided level for the fallback link bound
FALLBACK_FWER = 0.05                   # family-wise level for the fallback worst-station bound
FALLBACK_MIN_PASS_FRACTION = 0.85
FALLBACK_COUNT_MIN_PASS_FRACTION = 0.80
# There is NO defensible parametric null for the network-total tolerance: the premise that carried
# the old 0.03 ("demand is a fixed route file, only crossing times shift") is false, and nothing
# replaces it without measurement. The fallback therefore reports the observed relative error with
# no threshold at all rather than inventing a second one. This constant is intentionally absent;
# the name is kept only so that a stale import fails loudly instead of silently reading 0.03.
SEED_STABILITY_TOTAL_REL_TOL = None

UNCALIBRATED_WARNING = (
    "UNCALIBRATED RESULT -- NOT QUOTABLE AS A PASSING GATE. No seed-stability calibration was found "
    "for this scenario, so the thresholds below come from the parametric Binomial(n, 1/2) fallback "
    "whose generative model is known to be wrong for any scenario with route distributions and "
    "rerouting (see FALLBACK_BASIS). The fallback is CONSERVATIVE -- it is ~2x too loose at the "
    "measured dispersion -- so a FAILURE under it is real evidence and is reported as a failure, "
    "while a PASS is uninformative and is reported as `na` with the would-be verdict kept in "
    "`provisional_status`. Run `tools/sumo_realism.py --calibrate` for this scenario before quoting "
    "any seed-stability gate."
)
UNCALIBRATED_PASS_REASON = (
    "uncalibrated: passing under the parametric Binomial(n, 1/2) fallback is uninformative because "
    "that null is ~2x too loose at the measured dispersion; run --calibrate before quoting this gate"
)
UNCALIBRATED_FAIL_NOTE = (
    "failure raised under the CONSERVATIVE parametric fallback: the calibrated bound would be "
    "tighter still, so this failure survives calibration -- but the numeric threshold quoted here "
    "is not a calibrated one"
)
NO_TOTAL_TOLERANCE_REASON = (
    "uncalibrated: the 0.03 network-total tolerance shipped before 2026-08-30 rested on a premise "
    "that is false for this scenario family (rerouting makes the seed change route choice, measured "
    "seed-to-seed total spread 8.8% on intas_urban_low @300 s), and no parametric replacement is "
    "defensible; the observed relative error is reported without a threshold until --calibrate has "
    "measured one"
)

LICENCE_NOTICE = (
    "REFERENCE-DATA LICENCE: this tool never vendors measured counts. Any --ref-counts file is "
    "supplied by the caller. The Ingolstadt open loop counts (SAVeNoW / TU Muenchen FROST "
    "SensorThings, https://savenow.gis.lrg.tum.de/frost/v1.1/) carry NO formally stated licence -- "
    "the TUM catalogue reads \"License Not Specified\" -- so any counts fetched from it must stay "
    "in the gitignored .realism_cache/ and MUST NOT be committed to this repository. Seed-stability "
    "calibration data is this repository's OWN SUMO output and carries no such restriction. See "
    "docs/realism/GEH-VALIDATION.md."
)

# Gates whose verdict depends on a null distribution, i.e. the ones that are only quotable when a
# calibration is present. `regression.*` (same-seed, must reproduce exactly) is NOT in this set:
# a zero-tolerance identity check needs no null model.
CALIBRATION_DEPENDENT_GATES = frozenset({
    "seed_stability.link_geh_pass_fraction",
    "seed_stability.max_link_geh",
    "seed_stability.total_flow_rel_error",
    "seed_stability.link_count_tolerance_pass_fraction",
})


def _geh_bound(alpha: float, phi: float = 1.0) -> float:
    """Window-native GEH bound at two-sided level ``alpha`` under index of dispersion ``phi``.

    ``phi = 1`` is the Binomial(n, 1/2) fallback and reproduces the historical
    ``sqrt(2) * z_(1-alpha/2)``; a calibration supplies the measured value instead.
    """
    return math.sqrt(2.0 * float(phi)) * NormalDist().inv_cdf(1.0 - alpha / 2.0)


# ------------------------------------------------------------------------------------------------
# empirical seed-stability calibration
#
# Everything below operates on ``runs``: ``{seed_label: {station: window_native_count}}``. That is
# deliberately a plain dict of numbers so the whole calibration is a PURE FUNCTION that unit tests
# can drive without SUMO, and so a calibration file can carry its own inputs and be re-derived.
# ------------------------------------------------------------------------------------------------
def _geh(m: float, c: float) -> float:
    s = float(m) + float(c)
    return math.sqrt(2.0 * (float(m) - float(c)) ** 2 / s) if s > 0 else 0.0


def _quantile(xs, p: float) -> float:
    """Order statistic at ``p``, rounded UP to an observed value (never interpolated downwards)."""
    a = np.asarray(list(xs), dtype=float)
    if a.size == 0:
        return float("nan")
    return float(np.quantile(a, min(max(p, 0.0), 1.0), method="higher"))


def pair_statistics(m_cnt: dict[str, float], c_cnt: dict[str, float]) -> dict:
    """Every gate statistic for ONE run pair, on window-native vehicle counts.

    The single place the gate statistics are defined, shared by the live report and by the
    calibration, so a calibrated threshold can never drift away from the statistic it grades.
    """
    shared = sorted(set(m_cnt) & set(c_cnt))
    cmpbl = [s for s in shared if not (float(m_cnt[s]) == 0.0 and float(c_cnt[s]) == 0.0)]
    # 4 dp mirrors geh_summary(), so a calibration grades the pairs with the SAME numbers the live
    # gate later sees; without this, thresholds land on ties at 1e-16 and shift the false-alarm rate.
    geh = {s: round(_geh(m_cnt[s], c_cnt[s]), 4) for s in cmpbl}
    tot_m = sum(float(m_cnt[s]) for s in cmpbl)
    tot_c = sum(float(c_cnt[s]) for s in cmpbl)
    band = [s for s in cmpbl
            if abs(float(m_cnt[s]) - float(c_cnt[s]))
            <= max(SEED_STABILITY_COUNT_ABS_FLOOR,
                   SEED_STABILITY_COUNT_REL_TOL * max(float(m_cnt[s]), float(c_cnt[s])))]
    n = len(cmpbl)
    return {
        "stations": cmpbl, "n": n, "geh": geh,
        "max_geh": max(geh.values(), default=0.0),
        "total_modelled": tot_m, "total_reference": tot_c,
        "total_rel_error": (abs(tot_m - tot_c) / tot_c) if tot_c else None,
        "count_band_pass_fraction": (len(band) / n) if n else None,
        "stations_outside_count_band": [s for s in cmpbl if s not in set(band)],
        # (m-c)^2/(m+c): the per-test index-of-dispersion contribution, == geh^2 / 2
        "dispersion_terms": [(float(m_cnt[s]) - float(c_cnt[s])) ** 2
                             / (float(m_cnt[s]) + float(c_cnt[s])) for s in cmpbl],
    }


def _pairs(seeds):
    return list(itertools.combinations(list(seeds), 2))


def _pooled_dispersion(runs: dict, seeds) -> float:
    """phi = mean over (pair, station) of (m-c)^2/(m+c). The Binomial(n,1/2) null asserts phi = 1."""
    terms: list[float] = []
    for a, b in _pairs(seeds):
        terms.extend(pair_statistics(runs[a], runs[b])["dispersion_terms"])
    return statistics.mean(terms) if terms else float("nan")


def _total_cv(runs: dict, seeds) -> tuple[float, float, float]:
    tot = [sum(runs[s].values()) for s in seeds]
    mu = statistics.mean(tot)
    sd = statistics.stdev(tot) if len(tot) > 1 else 0.0
    return mu, sd, (sd / mu if mu else float("nan"))


def _resample_upper(runs: dict, seeds, b: int, gamma: float, rng_seed: int) -> tuple[float, float]:
    """Seed-level resampling upper confidence limits on ``phi`` and on the network-total ``cv``.

    The resampling unit is the SEED, not the pair: the K(K-1)/2 pairs are strongly dependent (each
    run appears in K-1 of them), so resampling pairs would badly understate the spread. K seeds are
    drawn with replacement and the DISTINCT draws are used, which shrinks the effective K to ~0.63K
    and therefore widens the limit -- a deliberately conservative choice that keeps a short
    calibration from claiming a precision it does not have.
    """
    rng = random.Random(rng_seed)
    seeds = list(seeds)
    phis: list[float] = []
    cvs: list[float] = []
    for _ in range(int(b)):
        pick = [rng.choice(seeds) for _ in seeds]
        uniq = sorted(set(pick), key=seeds.index)
        if len(uniq) < 2:
            continue
        phis.append(_pooled_dispersion(runs, uniq))
        cvs.append(_total_cv(runs, uniq)[2])
    if not phis:
        return _pooled_dispersion(runs, seeds), _total_cv(runs, seeds)[2]
    return _quantile(phis, 1.0 - gamma), _quantile(cvs, 1.0 - gamma)


def _bounds_at_level(stats: list[dict], a: float, phi_up: float, cv_up: float,
                     n_typical: int) -> dict:
    """Thresholds at per-gate level ``a``: max(moment form at the upper limit, empirical quantile)."""
    z = NormalDist().inv_cdf(1.0 - a / 2.0)
    pooled = [v for st in stats for v in st["geh"].values()]
    maxes = [st["max_geh"] for st in stats]
    rels = [st["total_rel_error"] for st in stats if st["total_rel_error"] is not None]
    bands = [st["count_band_pass_fraction"] for st in stats
             if st["count_band_pass_fraction"] is not None]

    link_moment = math.sqrt(2.0 * phi_up) * z
    link_empirical = _quantile(pooled, 1.0 - a)
    link = max(link_moment, link_empirical)

    # worst-station gate: Bonferroni over the typical station count, on the CORRECTED null
    z_fwer = NormalDist().inv_cdf(1.0 - a / (2.0 * max(n_typical, 1)))
    max_moment = math.sqrt(2.0 * phi_up) * z_fwer
    max_empirical = _quantile(maxes, 1.0 - a)
    max_link = max(max_moment, max_empirical)

    rel_moment = z * math.sqrt(2.0) * cv_up
    rel_empirical = _quantile(rels, 1.0 - a) if rels else 0.0
    rel = max(rel_moment, rel_empirical)

    pass_fracs = [round(sum(1 for v in st["geh"].values() if v < link) / st["n"], 4) if st["n"]
                  else None for st in stats]
    pf = [v for v in pass_fracs if v is not None]
    return {
        "gate_alpha": a,
        "link_geh_bound": link,
        "link_geh_bound_moment": link_moment, "link_geh_bound_empirical": link_empirical,
        "max_link_geh_bound": max_link,
        "max_link_geh_bound_moment": max_moment, "max_link_geh_bound_empirical": max_empirical,
        "total_flow_rel_tol": rel,
        "total_flow_rel_tol_moment": rel_moment, "total_flow_rel_tol_empirical": rel_empirical,
        "link_geh_min_pass_fraction": _quantile(pf, a) if pf else None,
        "link_count_min_pass_fraction": _quantile(bands, a) if bands else None,
        "n_typical_stations": n_typical,
    }


def _pair_fails(th: dict, st: dict) -> tuple[bool, bool, bool, bool]:
    """(link pass-fraction, worst station, network total, count band) failure flags for one pair."""
    pf = (round(sum(1 for v in st["geh"].values() if v < th["link_geh_bound"]) / st["n"], 4)
          if st["n"] else None)
    return (
        pf is not None and th["link_geh_min_pass_fraction"] is not None
        and pf < th["link_geh_min_pass_fraction"],
        st["max_geh"] > th["max_link_geh_bound"],
        st["total_rel_error"] is not None and st["total_rel_error"] > th["total_flow_rel_tol"],
        st["count_band_pass_fraction"] is not None and th["link_count_min_pass_fraction"] is not None
        and st["count_band_pass_fraction"] < th["link_count_min_pass_fraction"],
    )


def _false_alarm(th: dict, stats: list[dict]) -> dict:
    c = [0, 0, 0, 0, 0]
    for st in stats:
        f = _pair_fails(th, st)
        for i, v in enumerate(f):
            c[i] += bool(v)
        c[4] += any(f)
    n = len(stats) or 1
    return {"n_pairs": len(stats),
            "link_geh_pass_fraction": c[0], "max_link_geh": c[1],
            "total_flow_rel_error": c[2], "link_count_tolerance_pass_fraction": c[3],
            "any_gate": c[4], "any_gate_rate": round(c[4] / n, 4)}


def _min_detectable_ratio(c: float, bound: float) -> float | None:
    """Smallest m/c > 1 whose window-native GEH exceeds ``bound`` at reference count ``c``.

    Solves ``2c(r-1)^2/(r+1) = bound^2`` for r > 1. This is the gate's own statement of what it can
    see: C2 existed because nobody ever asked it.
    """
    c = float(c)
    if c <= 0 or bound <= 0:
        return None
    k = bound * bound / (2.0 * c)
    disc = (2.0 + k) ** 2 - 4.0 * (1.0 - k)
    if disc < 0:
        return None
    return ((2.0 + k) + math.sqrt(disc)) / 2.0


def calibrate_seed_stability(runs: dict[str, dict[str, float]], *,
                             alpha: float = SEED_STABILITY_REPORT_ALPHA,
                             bootstrap_b: int = CALIB_BOOTSTRAP_B,
                             gamma: float = CALIB_BOOTSTRAP_GAMMA,
                             rng_seed: int = CALIB_BOOTSTRAP_SEED,
                             window_s: float | None = None,
                             group_by: str = "station",
                             scenario_id: str = "unknown",
                             scenario_hash: str | None = None,
                             sumo_version: str | None = None,
                             provenance: dict | None = None) -> dict:
    """Derive seed-stability thresholds from ``{seed: {station: window-native count}}``.

    Pure and deterministic: same ``runs`` + same knobs -> byte-identical output.
    """
    seeds = list(runs)
    if len(seeds) < 2:
        raise ValueError("calibration needs at least 2 seed runs")
    ps = _pairs(seeds)
    stats = [pair_statistics(runs[a], runs[b]) for a, b in ps]
    n_typ = int(statistics.median([st["n"] for st in stats])) if stats else 0
    phi_hat = _pooled_dispersion(runs, seeds)
    mu_t, sd_t, cv_t = _total_cv(runs, seeds)
    phi_up, cv_up = _resample_upper(runs, seeds, bootstrap_b, gamma, rng_seed)
    phi_up = max(phi_up, phi_hat)
    cv_up = max(cv_up, cv_t)

    # largest per-gate level whose REPORT-level false-alarm rate still meets the target: the
    # gates are positively correlated, so a plain Bonferroni split over-corrects and throws away
    # power. Fall back to the strictest divisor if even that misses the target.
    chosen = None
    ladder = []
    for div in CALIB_GATE_LEVEL_DIVISORS:
        th = _bounds_at_level(stats, alpha / div, phi_up, cv_up, n_typ)
        fa = _false_alarm(th, stats)
        ladder.append({"divisor": div, "gate_alpha": th["gate_alpha"],
                       "in_sample_any_gate_rate": fa["any_gate_rate"]})
        if fa["any_gate_rate"] <= alpha:
            chosen = th
    if chosen is None:
        chosen = _bounds_at_level(stats, alpha / max(CALIB_GATE_LEVEL_DIVISORS), phi_up, cv_up, n_typ)

    in_sample = _false_alarm(chosen, stats)

    # leave-one-seed-out: re-derive the thresholds without one seed and grade that seed's pairs
    # against them. The honest out-of-sample false-alarm estimate, and the only guard against a
    # short calibration fitting its own extremes.
    loo_any = loo_n = 0
    if len(seeds) >= 4:
        for held in seeds:
            rest = [s for s in seeds if s != held]
            sub = {s: runs[s] for s in rest}
            sub_stats = [pair_statistics(sub[a], sub[b]) for a, b in _pairs(rest)]
            sub_phi_up, sub_cv_up = _resample_upper(sub, rest, max(bootstrap_b // 4, 100), gamma,
                                                    rng_seed + 1)
            sub_phi_up = max(sub_phi_up, _pooled_dispersion(sub, rest))
            sub_cv_up = max(sub_cv_up, _total_cv(sub, rest)[2])
            sub_n = int(statistics.median([st["n"] for st in sub_stats])) if sub_stats else 0
            sub_th = None
            for div in CALIB_GATE_LEVEL_DIVISORS:
                t = _bounds_at_level(sub_stats, alpha / div, sub_phi_up, sub_cv_up, sub_n)
                if _false_alarm(t, sub_stats)["any_gate_rate"] <= alpha:
                    sub_th = t
            sub_th = sub_th or _bounds_at_level(sub_stats, alpha / max(CALIB_GATE_LEVEL_DIVISORS),
                                                sub_phi_up, sub_cv_up, sub_n)
            for other in rest:
                loo_any += any(_pair_fails(sub_th, pair_statistics(runs[held], runs[other])))
                loo_n += 1

    per_station = {}
    for s in sorted({k for r in runs.values() for k in r}):
        vals = [float(runs[k].get(s, 0.0)) for k in seeds]
        mu = statistics.mean(vals)
        var = statistics.variance(vals) if len(vals) > 1 else 0.0
        terms = [t for a, b in ps
                 for st, t in [(s, (runs[a].get(s, 0.0) - runs[b].get(s, 0.0)) ** 2
                                / max(runs[a].get(s, 0.0) + runs[b].get(s, 0.0), 1e-12))]
                 if (runs[a].get(s, 0.0) + runs[b].get(s, 0.0)) > 0]
        per_station[s] = {
            "mean_count": round(mu, 4), "variance": round(var, 4),
            "index_of_dispersion": (round(var / mu, 4) if mu else None),
            "phi_pairwise": (round(statistics.mean(terms), 4) if terms else None),
            "n_informative_pairs": len(terms),
            "min_detectable_ratio": (round(_min_detectable_ratio(mu, chosen["link_geh_bound"]), 4)
                                     if mu > 0 else None),
        }
    heavy = sorted(s for s, v in per_station.items()
                   if v["phi_pairwise"] is not None and v["phi_pairwise"] > 2.0 * phi_hat)

    caveats = [
        "Thresholds are LOCAL to this scenario, this window and this SUMO build. A different "
        "scenario, window, --group-by or SUMO version needs its own calibration.",
        "The K(K-1)/2 pairs are NOT independent (each run appears in K-1 of them); quoted "
        "quantiles are order statistics of a dependent sample and the honest out-of-sample figure "
        "is `validation.leave_one_seed_out_any_gate_rate`, not the in-sample one.",
    ]
    if len(seeds) < CALIB_RECOMMENDED_SEEDS:
        caveats.append(
            f"LOW CONFIDENCE: K={len(seeds)} seeds is below the recommended "
            f"{CALIB_RECOMMENDED_SEEDS}. Measured on intas_urban_low, two DISJOINT 10-seed halves "
            f"of the same 20 seeds gave phi 0.2713 and 0.1619 -- a 1.68x swing -- because the "
            f"per-station null is heavy-tailed (single runs occasionally double a station's count "
            f"through a route-choice flip). Thresholds derived from K=10 can be badly off.")
    if heavy:
        caveats.append(
            f"Heavy-tailed stations (pairwise phi > 2x the pooled {phi_hat:.4f}): "
            f"{', '.join(heavy)}. The worst-station bound is dominated by these; a change confined "
            f"to a quiet station is easier to see than one at these.")

    return {
        "schema": CALIBRATION_SCHEMA,
        "scenario_id": scenario_id,
        "scenario_hash": scenario_hash,
        "sumo_version": sumo_version,
        "generated_utc": _dt.datetime.now(_dt.timezone.utc).replace(microsecond=0).isoformat(),
        "basis": SEED_STABILITY_BASIS,
        "window_s": window_s,
        "group_by": group_by,
        "seeds": [str(s) for s in seeds],
        "n_seeds": len(seeds),
        "n_pairs": len(ps),
        "n_station_tests": sum(st["n"] for st in stats),
        "alpha_report": alpha,
        "gate_alpha": chosen["gate_alpha"],
        "gate_alpha_ladder": ladder,
        "bootstrap": {"resamples": bootstrap_b, "gamma": gamma, "rng_seed": rng_seed,
                      "unit": "seed (distinct draws)"},
        "dispersion": {
            "phi_pooled": round(phi_hat, 6), "phi_upper": round(phi_up, 6),
            "binomial_null_value": 1.0,
            "note": "phi = E[(m-c)^2/(m+c)] over (pair, station); the discarded Binomial(n, 1/2) "
                    "null asserts 1.0",
        },
        "network_total": {"mean_vehicles": round(mu_t, 4), "stdev_vehicles": round(sd_t, 4),
                          "cv": round(cv_t, 6), "cv_upper": round(cv_up, 6),
                          "totals_by_seed": {str(s): round(sum(runs[s].values()), 4)
                                             for s in seeds},
                          "spread_max_min_over_mean": (round((max(sum(runs[s].values())
                                                                  for s in seeds)
                                                              - min(sum(runs[s].values())
                                                                    for s in seeds)) / mu_t, 6)
                                                       if mu_t else None)},
        # NOT rounded: a threshold rounded UP past an exact order statistic (0.9090909... -> 0.909091)
        # silently converts every tie into a failure, which is how the in-sample rate and the live
        # rate diverged during development. Full precision, graded against the same 4 dp statistics.
        "thresholds": dict(chosen),
        "empirical_quantiles": {
            "station_geh": {str(p): round(_quantile([v for st in stats
                                                     for v in st["geh"].values()], p), 6)
                            for p in (0.5, 0.85, 0.95, 0.99, 1.0)},
            "pair_max_geh": {str(p): round(_quantile([st["max_geh"] for st in stats], p), 6)
                             for p in (0.5, 0.85, 0.95, 1.0)},
            "total_rel_error": {str(p): round(_quantile([st["total_rel_error"] for st in stats
                                                         if st["total_rel_error"] is not None], p), 6)
                                for p in (0.5, 0.85, 0.95, 1.0)},
        },
        "validation": {
            "in_sample": in_sample,
            "in_sample_any_gate_rate": in_sample["any_gate_rate"],
            "leave_one_seed_out_pairs": loo_n,
            "leave_one_seed_out_any_gate_failures": loo_any,
            "leave_one_seed_out_any_gate_rate": (round(loo_any / loo_n, 4) if loo_n else None),
        },
        "per_station": per_station,
        "heavy_tailed_stations": heavy,
        "low_confidence": len(seeds) < CALIB_RECOMMENDED_SEEDS,
        "caveats": caveats,
        # The per-seed window-native counts the thresholds were derived from. This is this
        # repository's OWN SUMO output (no third-party licence attaches) and it is what makes the
        # calibration auditable and the power tests runnable offline.
        "runs": {str(s): {k: round(float(v), 4) for k, v in sorted(runs[s].items())} for s in seeds},
        "licence_note": LICENCE_NOTICE,
    }


def load_calibration(path: str | os.PathLike) -> dict:
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    if doc.get("schema") != CALIBRATION_SCHEMA:
        raise ValueError(f"{path}: not a {CALIBRATION_SCHEMA} document "
                         f"(schema={doc.get('schema')!r})")
    for key in ("thresholds", "seeds", "alpha_report"):
        if key not in doc:
            raise ValueError(f"{path}: calibration is missing required key {key!r}")
    return doc


def calibration_path_for(scenario_id: str, directory: str | os.PathLike | None = None) -> Path:
    d = Path(directory) if directory else DEFAULT_CALIBRATION_DIR
    return d / f"seed_stability_{scenario_id}.json"


_GENERIC_DIRS = frozenset({"sumo", "scenarios", "seed_runs", ".realism_cache", "out", "output",
                           "scms-sim", "third_party", ""})


def _scenario_id_from_path(path: str | None) -> str | None:
    """Infer a scenario id from the DIRECTORY a scenario file sits in.

    ``.../scenarios/gen_intas_urban_low/sumo/x.xml`` -> ``intas_urban_low``. Only directories are
    considered (a detector-output FILENAME is per-run, not per-scenario), and a miss simply means no
    calibration is found -- which reports `uncalibrated` rather than grading against the wrong one.
    """
    if not path:
        return None
    p = Path(path).resolve()
    parts = list(p.parts[:-1]) if p.suffix else list(p.parts)
    for part in reversed(parts):
        if part.lower() in _GENERIC_DIRS:
            continue
        for prefix in ("gen_", "scms_"):
            if part.lower().startswith(prefix):
                return part[len(prefix):]
        return part
    return None


def scenario_hash(sumocfg: str | None, det_add: str | None) -> dict:
    """Content hash of the scenario inputs a calibration is only valid for.

    Full content hashes for the config, the E1 additional and the network (the three files whose
    change invalidates a calibration outright); name+size for the route/additional inputs, which are
    large and whose content change always moves their size in practice.
    """
    h = hashlib.sha256()
    inputs: list[str] = []
    cfg_dir = Path(sumocfg).parent if sumocfg else None

    def _feed_file(p: Path, label: str):
        h.update(label.encode()); h.update(b"\0")
        h.update(p.read_bytes())
        inputs.append(f"{label}=sha256(content)")

    if sumocfg and os.path.exists(sumocfg):
        _feed_file(Path(sumocfg), "sumocfg:" + Path(sumocfg).name)
    if det_add and os.path.exists(det_add):
        _feed_file(Path(det_add), "det_add:" + Path(det_add).name)
    if sumocfg and os.path.exists(sumocfg) and cfg_dir is not None:
        try:
            root = ET.parse(sumocfg).getroot()
        except ET.ParseError:
            root = None
        if root is not None:
            for tag in ("net-file", "route-files", "additional-files"):
                el = root.find(f".//{tag}")
                if el is None or not el.get("value"):
                    continue
                for name in el.get("value").split(","):
                    name = name.strip()
                    if not name:
                        continue
                    p = cfg_dir / name
                    if not p.exists():
                        h.update(f"{tag}:{name}:missing".encode())
                        inputs.append(f"{tag}:{name}=missing")
                    elif tag == "net-file":
                        _feed_file(p, f"{tag}:{name}")
                    else:
                        h.update(f"{tag}:{name}:{p.stat().st_size}".encode())
                        inputs.append(f"{tag}:{name}=size({p.stat().st_size})")
    return {"sha256": h.hexdigest(), "inputs": inputs}


def sumo_version(binary: str = "sumo") -> str:
    try:
        out = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=60)
        for line in (out.stdout or "").splitlines():
            if line.strip():
                return line.strip()
    except (OSError, subprocess.SubprocessError):
        pass
    return "unknown"


def e1_output_file(det_add: str) -> str | None:
    """The detector-output filename most of the loops in ``det_add`` write to."""
    tally: dict[str, int] = {}
    for _ev, el in ET.iterparse(det_add, events=("end",)):
        if el.tag in ("e1Detector", "inductionLoop"):
            f = el.get("file")
            if f:
                tally[f] = tally.get(f, 0) + 1
        el.clear()
    return max(tally, key=lambda k: (tally[k], k)) if tally else None


def parse_seed_spec(spec: str) -> list[int]:
    """``"1-20"`` / ``"1,2,5-8"`` -> a de-duplicated ordered seed list."""
    out: list[int] = []
    for chunk in str(spec).split(","):
        chunk = chunk.strip()
        if not chunk:
            continue
        if "-" in chunk[1:]:
            lo, hi = chunk.split("-", 1)
            out.extend(range(int(lo), int(hi) + 1))
        else:
            out.append(int(chunk))
    seen, uniq = set(), []
    for s in out:
        if s not in seen:
            seen.add(s); uniq.append(s)
    return uniq


def run_seed(sumocfg: str, seed: int, cache_dir: Path, det_out_name: str, *,
             begin: float | None = None, end: float | None = None,
             binary: str = "sumo", reuse: bool = True) -> Path:
    """Run SUMO once at ``seed``, writing every output into the gitignored cache directory.

    ``--output-prefix`` is resolved by SUMO RELATIVE TO THE CONFIG DIRECTORY, so an absolute prefix
    is rejected by SUMO ("Could not build output file"); a relative prefix is computed here.
    """
    cache_dir.mkdir(parents=True, exist_ok=True)
    target = cache_dir / f"s{seed}_{det_out_name}"
    if reuse and target.exists() and target.stat().st_size > 0:
        return target
    cfg_dir = Path(sumocfg).resolve().parent
    rel = os.path.relpath(cache_dir.resolve(), cfg_dir).replace("\\", "/")
    cmd = [binary, "-c", str(sumocfg), "--seed", str(seed),
           "--output-prefix", f"{rel}/s{seed}_", "--no-step-log", "--verbose", "false"]
    if begin is not None:
        cmd += ["--begin", str(begin)]
    if end is not None:
        cmd += ["--end", str(end)]
    res = subprocess.run(cmd, capture_output=True, text=True)
    if res.returncode != 0 or not target.exists():
        raise RuntimeError(f"SUMO seed {seed} failed (rc={res.returncode}): "
                           f"{(res.stderr or res.stdout or '')[-600:]}")
    return target


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


def _demote_uncalibrated(gates: list[dict]) -> list[dict]:
    """Strip every PASS produced by the parametric fallback down to ``na``.

    Direction matters and is the whole point. The fallback bound is ~2x LOOSER than the calibrated
    one at the measured dispersion, so a fallback FAILURE would also have failed a calibrated gate
    and is kept as a failure; a fallback PASS says nothing and must never be quotable, so it becomes
    ``na`` with the would-be verdict preserved in ``provisional_status``.
    """
    for g in gates:
        if g["id"] not in CALIBRATION_DEPENDENT_GATES:
            continue
        g["calibrated"] = False
        if g["status"] == "pass":
            g["provisional_status"] = "pass"
            g["status"] = "na"
            g["reason"] = UNCALIBRATED_PASS_REASON
        elif g["status"] == "fail":
            g["note"] = UNCALIBRATED_FAIL_NOTE
    return gates


def seed_stability_report(modelled: dict[str, float], counted: dict[str, float],
                          window_s: float | None = None, same_seed: bool = False,
                          calibration: dict | None = None) -> dict:
    """SIM-vs-SIM mode: run-to-run reproducibility of the simulator against itself.

    NOT calibration and NOT validation -- see ``SIM_VS_SIM_WARNING``. Grading happens on the
    WINDOW-NATIVE counts (the vehicles the loops actually recorded over the common window) because
    GEH is not scale-free and the veh/h extrapolation of a short window inflates it by
    ``sqrt(3600/window_s)``.

    With ``calibration`` the thresholds are the MEASURED ones (``SEED_STABILITY_BASIS``); without it
    they come from the parametric ``Binomial(n, 1/2)`` fallback and every passing gate is demoted to
    ``na`` (``FALLBACK_BASIS``, ``_demote_uncalibrated``). Never from FHWA either way.
    """
    part = _partition(modelled, counted)
    cmpbl = part["comparable"]
    n = len(cmpbl)
    scale = math.sqrt(3600.0 / window_s) if (window_s and window_s > 0) else 1.0
    m_cnt, c_cnt = _scale_counts(modelled, window_s), _scale_counts(counted, window_s)

    cal_th = (calibration or {}).get("thresholds") or {}
    calibrated = bool(cal_th)

    def _readable(x, *, upper: bool):
        """6 dp for a readable report, rounded in the LENIENT direction so no tie flips to a fail."""
        if x is None:
            return None
        f = math.ceil if upper else math.floor
        return f(float(x) * 1e6) / 1e6

    if calibrated:
        gate_alpha = float(calibration.get("gate_alpha") or calibration.get("alpha_report") or 0.05)
        link_bound = round(float(cal_th["link_geh_bound"]), 4)
        fwer_bound = (round(float(cal_th["max_link_geh_bound"]), 4) if n else None)
        min_pass = _readable(cal_th.get("link_geh_min_pass_fraction"), upper=False)
        min_band_pass = _readable(cal_th.get("link_count_min_pass_fraction"), upper=False)
        rel_tol = _readable(cal_th.get("total_flow_rel_tol"), upper=True)
        phi = float((calibration.get("dispersion") or {}).get("phi_upper", 1.0))
    else:
        gate_alpha = FALLBACK_ALPHA
        link_bound = round(_geh_bound(FALLBACK_ALPHA), 4)
        # Bonferroni over the N compared stations: the WHOLE gate keeps a family-wise alpha
        fwer_bound = (round(_geh_bound(FALLBACK_FWER / n), 4) if n else None)
        min_pass = FALLBACK_MIN_PASS_FRACTION
        min_band_pass = FALLBACK_COUNT_MIN_PASS_FRACTION
        rel_tol = None                                   # deliberately absent -- see C1
        phi = 1.0

    summ = geh_summary(((s, m_cnt[s], c_cnt[s]) for s in cmpbl), geh_max=link_bound)
    for row in summ["stations"]:
        row["geh_veh_h"] = round(row["geh"] * scale, 4)                 # continuity with FHWA mode
        row["outside_calibrated_band"] = bool(row["geh"] >= link_bound)
        row["min_detectable_ratio"] = (
            round(_min_detectable_ratio(row["counted"], link_bound), 4)
            if row["counted"] > 0 else None)

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
    outside = [r["station"] for r in summ["stations"] if r["outside_calibrated_band"]]
    mdr = [r["min_detectable_ratio"] for r in summ["stations"]
           if r["min_detectable_ratio"] is not None]
    # Every bound below is stated in VEHICLES over the common window. Without a common window the
    # values are veh/h and neither the absolute floor nor the sqrt(2)|Z| identity holds, so the
    # count band is reported n/a rather than silently graded on the wrong scale.
    no_window = (None if (window_s and window_s > 0) else
                 "the two runs do not share an aggregation window, so per-station VEHICLE counts "
                 "are not recoverable and an absolute vehicle band is not meaningful")

    basis = SEED_STABILITY_BASIS if calibrated else FALLBACK_BASIS
    cal_tag = (f"CALIBRATED from {calibration.get('n_seeds')} seeds of "
               f"{calibration.get('scenario_id')} ({calibration.get('sumo_version')}, "
               f"derived {calibration.get('generated_utc')})" if calibrated else
               "UNCALIBRATED parametric fallback")

    def _r(ref_id, *, mn=None, mx=None, note=""):
        return {"ref_id": ref_id, "min": mn, "max": mx,
                "cite": f"tools/sumo_realism.py {'SEED_STABILITY_BASIS' if calibrated else 'FALLBACK_BASIS'}"
                        f" -- {cal_tag}; NOT an external standard and NOT the FHWA criteria",
                "source": basis + (" " + note if note else "")}

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
        def _f4(x, dflt="n/a"):
            """Format a threshold that a malformed calibration could have left out."""
            return f"{float(x):.4f}" if x is not None else dflt

        if calibrated:
            link_note = (
                f"Bound {link_bound:.4f} = max(sqrt(2*phi_upper)*z_(1-a/2), empirical "
                f"(1-a) quantile of the per-station GEH) at a={gate_alpha:g}, with phi_upper="
                f"{phi:.4f} MEASURED over "
                f"{calibration.get('n_pairs')} same-scenario run pairs "
                f"({calibration.get('n_station_tests')} station tests). The discarded "
                f"Binomial(n, 1/2) null asserted phi = 1 and therefore a bound of "
                f"{_geh_bound(FALLBACK_ALPHA):.4f}, which was {_geh_bound(FALLBACK_ALPHA)/link_bound:.2f}x "
                f"too loose. The required pass fraction {_f4(min_pass)} is the a-quantile of "
                f"the per-pair pass fraction over the same pairs, not a round number.")
            max_note = (
                f"Bound {fwer_bound:.4f} = max(sqrt(2*phi_upper)*z_(1-a/(2N)), empirical (1-a) "
                f"quantile of the per-pair WORST station GEH) over the calibration pairs. This is "
                f"the gate that catches a change localised to one loop; per-station detectability "
                f"at the link bound is reported in `detectability`.")
            rel_note = (
                f"Tolerance {_f4(rel_tol)} = max(sqrt(2)*cv_upper*z_(1-a/2), empirical (1-a) "
                f"quantile of |dTotal|/Total) with the network-total coefficient of variation "
                f"MEASURED across the calibration seeds "
                f"(cv={float((calibration.get('network_total') or {}).get('cv', float('nan'))):.4f}, "
                f"seed-to-seed spread "
                f"{float((calibration.get('network_total') or {}).get('spread_max_min_over_mean', float('nan'))):.4f}). "
                f"It REPLACES the 0.03 tolerance shipped before 2026-08-30, which rested on the "
                f"false premise that a fixed route file pins the network total: rerouting makes the "
                f"seed change route choice, and 0.03 false-alarmed on "
                f"{(calibration.get('legacy_003_false_alarm_rate') or 'a measured 45%')} of pairs.")
            band_note = (
                f"An ENGINEERING equivalence bound whose PASS RATE is calibrated: a relative-only "
                f"tolerance is meaningless at 1-3 vehicles, hence the absolute floor, and the "
                f"required rate {_f4(min_band_pass)} is the a-quantile of the per-pair rate "
                f"over the calibration pairs. Every station in this layout sits in FHWA's "
                f"below_700 band (+-100 veh/h = "
                f"+-{100.0 * (window_s or 3600.0) / 3600.0:.2f} vehicles over this window), so the "
                f"band itself is far tighter than FHWA's.")
        else:
            # C2 minor: the exceedance is NOT a flat alpha per station -- it is zero for the small
            # stations and saw-toothed above them, purely from the discreteness of Binomial(n, 1/2).
            link_note = (
                f"FALLBACK bound {link_bound:.4f} = sqrt(2)*z_(1-alpha/2) at alpha="
                f"{FALLBACK_ALPHA:g}. The per-station false-alarm rate is NOT {FALLBACK_ALPHA:g}: "
                f"Binomial(n, 1/2) is discrete, so P(GEH >= {link_bound:.4f}) is exactly 0.000 for "
                f"n <= 3, then 0.1250 (n=4), 0.0625 (n=5), 0.0313 (n=6), 0.0156 (n=7), 0.0703 "
                f"(n=8), 0.0215 (n=10), 0.0225 (n=13), 0.0490 (n=17) -- a saw-tooth that never "
                f"settles at the nominal level. Stations recording 3 or fewer vehicles in total "
                f"CANNOT fail this bound at all. Requiring {float(min_pass):g} of N=22 stations "
                f"means the gate trips at 4 or more station failures, whose probability under this "
                f"(wrong) null is P(Bin(22, 0.05) >= 4) = 0.0222 -- NOT the '~1.5%' documented "
                f"before 2026-08-30. Every one of those numbers is moot in practice because the "
                f"null itself is wrong: measured exceedance on intas_urban_low is 9/4269 = 0.0021.")
            max_note = (
                f"FALLBACK bound {fwer_bound:.4f} = sqrt(2)*z_(1-alpha/(2N)) with alpha="
                f"{FALLBACK_FWER:g} and N={n}, inheriting the same wrong null; measured "
                f"exceedance over 190 intas_urban_low seed pairs was 0/190.")
            rel_note = NO_TOTAL_TOLERANCE_REASON
            band_note = (
                f"An ENGINEERING equivalence bound with an UNCALIBRATED required pass rate of "
                f"{float(min_band_pass):g}; the band itself is far tighter than FHWA's below_700 "
                f"tolerance (+-100 veh/h = "
                f"+-{100.0 * (window_s or 3600.0) / 3600.0:.2f} vehicles over this window).")

        gates = [
            _gate("seed_stability.link_geh_pass_fraction",
                  f"SEED-STABILITY: stations whose two-seed count split is inside the "
                  f"{'calibrated' if calibrated else 'FALLBACK'} band "
                  f"(window-native GEH < {link_bound:.4f})",
                  summ["pass_fraction"], "fraction",
                  _r("seed_stability.link_geh_alpha", mn=min_pass, note=link_note),
                  n, reason=no_data,
                  extra={"geh_median": summ["geh_median"], "geh_p85": summ["geh_p85"],
                         "geh_max": (round(geh_max_obs, 4) if geh_max_obs is not None else None),
                         "geh_bound": link_bound, "gate_alpha": gate_alpha,
                         "phi_used": round(phi, 6),
                         "stations_outside_band": outside,
                         "n_both_zero_excluded": len(part["both_zero"])}),
            _gate("seed_stability.max_link_geh",
                  f"SEED-STABILITY: worst station's window-native GEH "
                  f"({'calibrated family-wise bound' if calibrated else 'FALLBACK Bonferroni bound'}"
                  f" over N stations)",
                  geh_max_obs, "GEH",
                  (_r("seed_stability.max_link_geh_fwer", mx=fwer_bound, note=max_note)
                   if fwer_bound is not None else None),
                  n, reason=no_data,
                  extra=({"worst_station": worst["station"], "modelled": worst["modelled"],
                          "reference": worst["counted"]} if worst else None)),
            _gate("seed_stability.total_flow_rel_error",
                  "SEED-STABILITY: relative difference of the network-total loop count",
                  rel_err, "fraction",
                  (_r("seed_stability.total_flow_rel_tol", mx=rel_tol, note=rel_note)
                   if rel_tol is not None else None),
                  n, reason=(no_data or (None if rel_tol is not None
                                         else NO_TOTAL_TOLERANCE_REASON)),
                  extra={"total_modelled_vehicles": round(tot_m, 3),
                         "total_reference_vehicles": round(tot_c, 3),
                         "total_split_z": (round(split_z, 4) if split_z is not None else None)}),
            _gate("seed_stability.link_count_tolerance_pass_fraction",
                  "SEED-STABILITY: stations inside the per-station count band "
                  f"|delta| <= max({SEED_STABILITY_COUNT_ABS_FLOOR:g} veh, "
                  f"{SEED_STABILITY_COUNT_REL_TOL:g}*max(m,c))",
                  (len(tol_ok) / n if n else None), "fraction",
                  _r("seed_stability.link_count_tolerance", mn=min_band_pass, note=band_note),
                  n, reason=(no_data or no_window),
                  extra={"abs_floor_vehicles": SEED_STABILITY_COUNT_ABS_FLOOR,
                         "rel_tolerance": SEED_STABILITY_COUNT_REL_TOL,
                         "stations_outside_band": [s for s in cmpbl if s not in set(tol_ok)]}),
        ]
        if not calibrated:
            _demote_uncalibrated(gates)

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
        "calibration": ({
            "status": "calibrated",
            "scenario_id": calibration.get("scenario_id"),
            "scenario_hash": (calibration.get("scenario_hash") or {}).get("sha256")
            if isinstance(calibration.get("scenario_hash"), dict) else calibration.get("scenario_hash"),
            "sumo_version": calibration.get("sumo_version"),
            "generated_utc": calibration.get("generated_utc"),
            "seeds": calibration.get("seeds"), "n_seeds": calibration.get("n_seeds"),
            "n_pairs": calibration.get("n_pairs"),
            "window_s": calibration.get("window_s"), "group_by": calibration.get("group_by"),
            "alpha_report": calibration.get("alpha_report"),
            "gate_alpha": calibration.get("gate_alpha"),
            "phi_pooled": (calibration.get("dispersion") or {}).get("phi_pooled"),
            "phi_upper": (calibration.get("dispersion") or {}).get("phi_upper"),
            "measured_false_alarm_rate_in_sample":
                (calibration.get("validation") or {}).get("in_sample_any_gate_rate"),
            "measured_false_alarm_rate_leave_one_seed_out":
                (calibration.get("validation") or {}).get("leave_one_seed_out_any_gate_rate"),
            "low_confidence": calibration.get("low_confidence"),
            "source_file": calibration.get("_source_file"),
            "caveats": calibration.get("caveats"),
        } if calibrated else {
            "status": "uncalibrated",
            "warning": UNCALIBRATED_WARNING,
            "how_to_fix": "tools/sumo_realism.py --calibrate --sumocfg <scenario>.sumocfg "
                          "--det-add <E1>.add.xml --end <window_s> --seeds 1-20",
        }),
        "uncalibrated": (not calibrated) and not same_seed,
        # what the per-station gate can actually SEE at this bound -- the question C2 says nobody
        # ever asked. min_detectable_ratio[s] is the smallest m/counted[s] that leaves the band.
        "detectability": ({
            "link_geh_bound": link_bound,
            "min_detectable_ratio_median": round(float(np.median(mdr)), 4) if mdr else None,
            "min_detectable_ratio_best": round(min(mdr), 4) if mdr else None,
            "min_detectable_ratio_worst": round(max(mdr), 4) if mdr else None,
            "stations_where_a_2x_change_is_invisible":
                [r["station"] for r in summ["stations"]
                 if r["min_detectable_ratio"] is not None and r["min_detectable_ratio"] > 2.0],
            "note": "a station whose min_detectable_ratio exceeds 2.0 can double its flow without "
                    "leaving the per-station band; the worst-station gate is the only backstop "
                    "there",
        } if not same_seed else None),
        "stations_outside_geh_band": (outside if not same_seed else None),
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
def resolve_calibration(args) -> tuple[dict | None, dict]:
    """Find the seed-stability calibration for this run, or explain why there is none.

    Resolution order: ``--calibration PATH`` -> ``tools/calibration/seed_stability_<scenario_id>``
    for an explicit ``--scenario-id`` -> the same, for a scenario id inferred from ``--det-out`` /
    ``--det-add``. A calibration whose ``group_by`` or ``window_s`` does not match this comparison is
    REFUSED (its thresholds do not transfer), not silently applied.
    """
    prov: dict = {"searched": []}
    if getattr(args, "no_calibration", False):
        return None, {"reason": "--no-calibration was passed; the parametric fallback was forced",
                      "searched": []}
    explicit = getattr(args, "calibration", None)
    candidates: list[tuple[str, Path]] = []
    if explicit:
        candidates.append(("--calibration", Path(explicit)))
    else:
        # --det-add points at the scenario directory even when --det-out has been copied into a
        # cache, so try both; an explicit --scenario-id always wins.
        sids: list[str] = []
        for cand in (getattr(args, "scenario_id", None),
                     _scenario_id_from_path(getattr(args, "det_add", None)),
                     _scenario_id_from_path(getattr(args, "det_out", None))):
            if cand and cand not in sids:
                sids.append(cand)
        prov["scenario_id"] = sids[0] if sids else None
        prov["scenario_ids_tried"] = sids
        for sid in sids:
            candidates.append(("auto", calibration_path_for(
                sid, getattr(args, "calibration_dir", None))))
    for how, path in candidates:
        prov["searched"].append(str(path))
        if not path.exists():
            continue
        try:
            cal = load_calibration(path)
        except (OSError, ValueError, json.JSONDecodeError) as exc:
            prov["reason"] = f"{path}: unreadable calibration ({exc})"
            return None, prov
        cal["_source_file"] = str(path)
        prov.update({"file": str(path), "how": how})
        return cal, prov
    prov["reason"] = ("no calibration file found for this scenario; run --calibrate "
                      f"(looked in: {', '.join(prov['searched']) or 'nowhere -- no scenario id'})")
    return None, prov


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
            cal, cal_prov = resolve_calibration(args)
            if cal is not None:
                mismatch = []
                if cal.get("group_by") and cal["group_by"] != args.group_by:
                    mismatch.append(f"group_by {cal['group_by']!r} != {args.group_by!r}")
                cw = cal.get("window_s")
                if cw and win and abs(float(cw) - float(win)) > 0.01 * float(cw):
                    mismatch.append(f"window {cw:g} s != {win:g} s")
                if cw and not win:
                    mismatch.append(f"calibration is window-native ({cw:g} s) but this comparison "
                                    f"has no common window")
                if mismatch:
                    cal_prov["reason"] = ("calibration REFUSED, its thresholds do not transfer: "
                                          + "; ".join(mismatch))
                    cal_prov["refused_file"] = cal_prov.pop("file", None)
                    cal = None
            out["calibration_lookup"] = cal_prov
            out["sections"]["geh"] = seed_stability_report(modelled, counted, window_s=win,
                                                           same_seed=same_seed, calibration=cal)
            if cal is None and not same_seed:
                out["uncalibrated"] = True
                out["warning_uncalibrated"] = UNCALIBRATED_WARNING
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

    if getattr(args, "require_calibration", False) and out.get("comparison_kind") in (
            KIND_SEED_STABILITY,):
        ok = ((out["sections"].get("geh") or {}).get("calibration") or {}).get("status") \
            == "calibrated"
        out["sections"].setdefault("geh", {}).setdefault("gates", []).append(_gate(
            "seed_stability.calibration_present",
            "SEED-STABILITY: an empirical calibration exists for this scenario",
            1.0 if ok else 0.0, "boolean",
            {"ref_id": "seed_stability.calibration_present", "min": 1.0,
             "cite": "tools/sumo_realism.py --require-calibration",
             "source": "--require-calibration was passed: an uncalibrated seed-stability report "
                       "cannot be quoted as a gate result, so its absence is itself a failure. "
                       + ((out.get("calibration_lookup") or {}).get("reason") or "")},
            None))

    gates = []
    for sect in out["sections"].values():
        gates.extend(sect.get("gates", []) if isinstance(sect, dict) else [])
    out["summary"] = {
        "n_gates": len(gates),
        "pass": sum(1 for g in gates if g["status"] == "pass"),
        "fail": sum(1 for g in gates if g["status"] == "fail"),
        "na": sum(1 for g in gates if g["status"] == "na"),
        "failures": [g["id"] for g in gates if g["status"] == "fail"],
        "uncalibrated": bool(out.get("uncalibrated")),
        "quotable_as_a_gate_result": not out.get("uncalibrated"),
        "n_gates_demoted_uncalibrated": sum(1 for g in gates
                                            if g.get("provisional_status") == "pass"),
    }
    # comparison_kind / warning must be the FIRST thing a reader (or a diff) sees, not buried
    # after the sections -- mislabelling this report is the exact failure mode being guarded here.
    head = [k for k in ("comparison_kind", "warning", "warning_uncalibrated", "notes", "modelled",
                        "reference", "detector_layout") if k in out]
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
    if report.get("warning_uncalibrated"):
        L.append("!! UNCALIBRATED !!")
        L.extend(_wrap(report["warning_uncalibrated"]))
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
        if g.get("stations_outside_geh_band"):
            L.append(f"- OUTSIDE the per-station band: {g['stations_outside_geh_band']} "
                     f"(individually flagged even when the aggregate gate passes)")
        d = g.get("detectability")
        if d:
            L.append(f"- detectability at GEH < {d['link_geh_bound']}: a station must change by "
                     f"x{d['min_detectable_ratio_median']} (median; best "
                     f"x{d['min_detectable_ratio_best']}, worst x{d['min_detectable_ratio_worst']}) "
                     f"to leave the band")
            if d.get("stations_where_a_2x_change_is_invisible"):
                L.append(f"  - a 2x change is INVISIBLE to the per-station gate at "
                         f"{d['stations_where_a_2x_change_is_invisible']}")
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
            if gate.get("provisional_status") == "pass":
                icon = "UNC "                      # would have passed, but uncalibrated
            ref = gate.get("reference") or {}
            tail = (f" (ref {ref.get('threshold')}; {ref.get('cite')})" if ref else
                    f" — {gate.get('reason', '')}")
            L.append(f"- [{icon}] {gate['title']}: {gate['value']} {gate['unit']}{tail}")
    if not any(sect.get("gates") for sect in report["sections"].values() if isinstance(sect, dict)):
        L.append("- (no gate was evaluated)")
    L.extend(_render_provenance(report))
    L.append("")
    return "\n".join(L)


def _render_provenance(report: dict) -> list[str]:
    """Provenance + licence block. Printed on EVERY run, including the ones that pass."""
    L = ["", "## Provenance and licence"]
    g = report["sections"].get("geh") or {}
    cal = g.get("calibration") or {}
    lookup = report.get("calibration_lookup") or {}
    if cal.get("status") == "calibrated":
        L.append(f"- Seed-stability thresholds: CALIBRATED from {cal.get('n_seeds')} seeds "
                 f"({cal.get('n_pairs')} run pairs) of scenario `{cal.get('scenario_id')}`, "
                 f"window {cal.get('window_s')} s, group_by {cal.get('group_by')}")
        L.append(f"  - seeds {cal.get('seeds')}")
        L.append(f"  - {cal.get('sumo_version')}; scenario_hash {cal.get('scenario_hash')}; "
                 f"derived {cal.get('generated_utc')}")
        L.append(f"  - source file `{cal.get('source_file')}`")
        L.append(f"  - measured dispersion phi {cal.get('phi_pooled')} (upper limit "
                 f"{cal.get('phi_upper')}); the discarded Binomial(n,1/2) null asserted 1.0")
        L.append(f"  - measured report-level false-alarm rate: in-sample "
                 f"{cal.get('measured_false_alarm_rate_in_sample')}, leave-one-seed-out "
                 f"{cal.get('measured_false_alarm_rate_leave_one_seed_out')} "
                 f"(target alpha {cal.get('alpha_report')})")
        if cal.get("low_confidence"):
            L.append("  - LOW CONFIDENCE: fewer seeds than recommended, see caveats in the file")
        for c in (cal.get("caveats") or []):
            L.extend(_wrap("  - CAVEAT: " + c, indent="    "))
    elif cal.get("status") == "uncalibrated":
        L.append("- Seed-stability thresholds: NONE ON DISK -> parametric fallback, result is "
                 "UNCALIBRATED and NOT quotable as a passing gate")
        if lookup.get("reason"):
            L.extend(_wrap("  - " + lookup["reason"], indent="    "))
        L.extend(_wrap("  - " + cal.get("how_to_fix", ""), indent="    "))
    ref = report.get("reference") or {}
    if ref:
        L.append(f"- Reference data: `{ref.get('file')}` "
                 f"({'REAL-WORLD MEASURED counts' if ref.get('is_real_world_counts') else 'SUMO simulation output'})")
    L.extend(_wrap("- " + LICENCE_NOTICE, indent="  "))
    return L


def _main_calibrate(a, p) -> int:
    """``--calibrate``: measure the seed-stability null for one scenario and persist it."""
    if not a.det_add:
        p.error("--calibrate needs --det-add (the E1 additional file) to map detectors to stations")
    if not os.path.exists(a.det_add):
        p.error(f"--det-add: file not found: {a.det_add}")
    if not a.sumocfg and not a.calib_det_out:
        p.error("--calibrate needs either --sumocfg (to run the seeds) or repeated "
                "--calib-det-out SEED=PATH (to read existing per-seed outputs)")

    sid = a.scenario_id or _scenario_id_from_path(a.sumocfg or a.det_add) or "unknown"
    cache = Path(a.calib_cache) if a.calib_cache else (DEFAULT_CALIB_CACHE / sid)
    mapping = parse_e1_additional(a.det_add)

    per_seed: dict[str, str] = {}
    if a.calib_det_out:
        for item in a.calib_det_out:
            if "=" not in item:
                p.error(f"--calib-det-out expects SEED=PATH, got {item!r}")
            s, path = item.split("=", 1)
            if not os.path.exists(path):
                p.error(f"--calib-det-out {s}: file not found: {path}")
            per_seed[s.strip()] = path
    else:
        if not os.path.exists(a.sumocfg):
            p.error(f"--sumocfg: file not found: {a.sumocfg}")
        det_name = e1_output_file(a.det_add)
        if not det_name:
            p.error(f"--det-add {a.det_add}: no e1Detector carries a file= attribute")
        seeds = parse_seed_spec(a.seeds)
        if len(seeds) < CALIB_MIN_SEEDS:
            p.error(f"--seeds gives {len(seeds)} seed(s); a calibration needs at least "
                    f"{CALIB_MIN_SEEDS} ({CALIB_RECOMMENDED_SEEDS} recommended)")
        print(f"[calibrate] scenario {sid}: running {len(seeds)} seeds of {a.sumocfg}")
        print(f"[calibrate] raw SUMO output -> {cache} (gitignored, never committed)")
        for i, s in enumerate(seeds, 1):
            path = run_seed(a.sumocfg, s, cache, det_name, begin=a.begin, end=a.end,
                            binary=a.sumo_binary, reuse=not a.calib_no_reuse)
            per_seed[str(s)] = str(path)
            print(f"[calibrate] seed {s} ({i}/{len(seeds)}) -> {os.path.basename(str(path))}")

    runs: dict[str, dict[str, float]] = {}
    windows: set[float] = set()
    for s, path in per_seed.items():
        parsed = parse_e1_output(path, a.begin, a.end)
        if not parsed["counts"]:
            p.error(f"seed {s}: {path} has no <interval> rows")
        flows = to_station_flows(parsed, mapping, a.group_by, a.duration_s)
        win = a.duration_s or parsed["duration_s"]
        windows.add(round(float(win), 3))
        runs[s] = _scale_counts(flows, win)
    if len(windows) != 1:
        p.error(f"the seed runs do not share one aggregation window ({sorted(windows)}); a "
                f"calibration is window-native and cannot mix windows")
    window_s = windows.pop()
    if len(runs) < CALIB_MIN_SEEDS:
        p.error(f"only {len(runs)} usable seed run(s); a calibration needs at least "
                f"{CALIB_MIN_SEEDS}")

    cal = calibrate_seed_stability(
        runs, alpha=a.calib_alpha, window_s=window_s, group_by=a.group_by, scenario_id=sid,
        scenario_hash=scenario_hash(a.sumocfg, a.det_add),
        sumo_version=sumo_version(a.sumo_binary),
    )
    cal["inputs"] = {"det_add": a.det_add, "sumocfg": a.sumocfg,
                     "cache_dir": str(cache), "per_seed_det_out": per_seed,
                     "begin": a.begin, "end": a.end}
    # what the discarded thresholds would have done on exactly these pairs -- the C1/C2 receipt
    stats = [pair_statistics(runs[x], runs[y]) for x, y in _pairs(list(runs))]
    legacy_link = _geh_bound(FALLBACK_ALPHA)
    n_typ = cal["thresholds"]["n_typical_stations"]
    legacy_max = _geh_bound(FALLBACK_FWER / max(n_typ, 1))
    n_tests = sum(st["n"] for st in stats)
    cal["legacy_thresholds_on_these_pairs"] = {
        "note": "what tools/sumo_realism.py graded with before 2026-08-30, measured on exactly "
                "these pairs. C1 = the 0.03 total-flow tolerance, C2 = the Binomial(n,1/2) bounds.",
        "total_rel_tol_0p03_false_alarms": sum(
            1 for st in stats if (st["total_rel_error"] or 0.0) > 0.03),
        "total_rel_tol_0p03_false_alarm_rate": round(
            sum(1 for st in stats if (st["total_rel_error"] or 0.0) > 0.03) / max(len(stats), 1), 4),
        "link_geh_bound_binomial": round(legacy_link, 4),
        "link_geh_binomial_station_exceedances": sum(
            1 for st in stats for v in st["geh"].values() if v >= legacy_link),
        "link_geh_binomial_station_tests": n_tests,
        "link_geh_binomial_exceedance_rate": round(
            sum(1 for st in stats for v in st["geh"].values() if v >= legacy_link)
            / max(n_tests, 1), 6),
        "documented_exceedance_rate": FALLBACK_ALPHA,
        "max_link_geh_bound_binomial": round(legacy_max, 4),
        "max_link_geh_binomial_false_alarms": sum(1 for st in stats
                                                  if st["max_geh"] > legacy_max),
    }
    cal["legacy_003_false_alarm_rate"] = (
        f"{cal['legacy_thresholds_on_these_pairs']['total_rel_tol_0p03_false_alarm_rate']:.1%}")

    out_path = Path(a.calib_out) if a.calib_out else calibration_path_for(sid, a.calibration_dir)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(cal, fh, indent=2, sort_keys=False, default=str)
        fh.write("\n")

    th = cal["thresholds"]
    v = cal["validation"]
    lg = cal["legacy_thresholds_on_these_pairs"]
    print("")
    print(f"# Seed-stability calibration for `{sid}`  ({cal['n_seeds']} seeds, "
          f"{cal['n_pairs']} pairs, {cal['n_station_tests']} station tests, "
          f"window {cal['window_s']:g} s)")
    print(f"- {cal['sumo_version']}; scenario_hash "
          f"{(cal['scenario_hash'] or {}).get('sha256')}")
    print(f"- seeds: {', '.join(cal['seeds'])}")
    print(f"- measured index of dispersion phi = {cal['dispersion']['phi_pooled']} "
          f"(upper limit {cal['dispersion']['phi_upper']}); the Binomial(n,1/2) null asserted 1.0")
    print(f"- network total: mean {cal['network_total']['mean_vehicles']} veh, "
          f"cv {cal['network_total']['cv']}, seed spread "
          f"{cal['network_total']['spread_max_min_over_mean']}")
    print(f"- per-gate level a = {cal['gate_alpha']} (report-level alpha {cal['alpha_report']})")
    print("- THRESHOLDS")
    print(f"    link_geh_bound                {th['link_geh_bound']:.4f}   "
          f"(was {lg['link_geh_bound_binomial']:.4f} under Binomial(n,1/2))")
    print(f"    max_link_geh_bound            {th['max_link_geh_bound']:.4f}   "
          f"(was {lg['max_link_geh_bound_binomial']:.4f})")
    print(f"    link_geh_min_pass_fraction    {th['link_geh_min_pass_fraction']}")
    print(f"    total_flow_rel_tol            {th['total_flow_rel_tol']:.4f}   (was 0.0300)")
    print(f"    link_count_min_pass_fraction  {th['link_count_min_pass_fraction']}")
    print("- MEASURED FALSE-ALARM RATE over the calibration pairs")
    print(f"    BEFORE (0.03 total tolerance): "
          f"{lg['total_rel_tol_0p03_false_alarms']}/{cal['n_pairs']} = "
          f"{lg['total_rel_tol_0p03_false_alarm_rate']:.4f}")
    print(f"    BEFORE (GEH < {lg['link_geh_bound_binomial']:.4f} per station): "
          f"{lg['link_geh_binomial_station_exceedances']}/{lg['link_geh_binomial_station_tests']} "
          f"= {lg['link_geh_binomial_exceedance_rate']:.4f} vs a documented "
          f"{lg['documented_exceedance_rate']}")
    print(f"    AFTER  (all gates, in-sample):        {v['in_sample']['any_gate']}/"
          f"{cal['n_pairs']} = {v['in_sample_any_gate_rate']:.4f}")
    print(f"    AFTER  (all gates, leave-one-seed-out): "
          f"{v['leave_one_seed_out_any_gate_failures']}/{v['leave_one_seed_out_pairs']} = "
          f"{v['leave_one_seed_out_any_gate_rate']}")
    for c in cal["caveats"]:
        for line in _wrap("- CAVEAT: " + c, indent="  "):
            print(line)
    print("")
    for line in _wrap("- " + LICENCE_NOTICE, indent="  "):
        print(line)
    print(f"\n[wrote {out_path}]")
    return 0


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
    # --- empirical seed-stability calibration ----------------------------------------------------
    p.add_argument("--calibrate", action="store_true",
                   help="CALIBRATION mode: run K seeds of --sumocfg (or read --calib-det-out "
                        "pairs), measure the seed-stability null and write a calibration file. "
                        "Nothing is graded in this mode.")
    p.add_argument("--sumocfg", default=None,
                   help="scenario config for --calibrate; SUMO is run once per seed with every "
                        "output redirected into the gitignored cache")
    p.add_argument("--seeds", default="1-20",
                   help="seed list for --calibrate, e.g. '1-20' or '1,2,23423' (default 1-20; "
                        f"at least {CALIB_MIN_SEEDS}, {CALIB_RECOMMENDED_SEEDS} recommended)")
    p.add_argument("--calib-det-out", action="append", default=None, metavar="SEED=PATH",
                   help="offline --calibrate input: an existing E1 output for one seed; repeat "
                        "once per seed instead of passing --sumocfg")
    p.add_argument("--calib-cache", default=None,
                   help=f"directory for the raw per-seed SUMO output (default "
                        f"{DEFAULT_CALIB_CACHE}/<scenario_id>; gitignored, never committed)")
    p.add_argument("--calib-out", default=None,
                   help="where to write the calibration (default "
                        "tools/calibration/seed_stability_<scenario_id>.json)")
    p.add_argument("--calib-alpha", type=float, default=SEED_STABILITY_REPORT_ALPHA,
                   help="REPORT-level false-alarm target the thresholds are calibrated to "
                        f"(default {SEED_STABILITY_REPORT_ALPHA})")
    p.add_argument("--calib-no-reuse", action="store_true",
                   help="re-run every seed even when its cached detector output already exists")
    p.add_argument("--sumo-binary", default="sumo", help="SUMO executable for --calibrate")
    p.add_argument("--scenario-id", default=None,
                   help="scenario name used to name and look up the calibration file (default: "
                        "inferred from the scenario directory)")
    p.add_argument("--calibration", default=None,
                   help="explicit calibration file to grade against (default: auto-discovered in "
                        "tools/calibration/ by scenario id)")
    p.add_argument("--calibration-dir", default=None,
                   help=f"directory searched for calibrations (default {DEFAULT_CALIBRATION_DIR})")
    p.add_argument("--no-calibration", action="store_true",
                   help="ignore any calibration on disk and force the parametric fallback; every "
                        "would-be pass is then reported as `na`")
    p.add_argument("--require-calibration", action="store_true",
                   help="fail (exit 1) when a seed-stability comparison has no calibration, "
                        "instead of reporting uncalibrated `na` gates")
    a = p.parse_args(argv)

    if a.calibrate:
        return _main_calibrate(a, p)

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
