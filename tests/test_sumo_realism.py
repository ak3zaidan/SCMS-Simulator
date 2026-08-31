"""Seed-stability gate tests for ``tools/sumo_realism.py`` -- above all, POWER tests.

`docs/realism/REVIEW-FINDINGS.md` C2 exists for one reason: **nothing in this repository ever
tested that the seed-stability gate could FAIL.** The gate was written, its thresholds were derived
from an asserted null, and every run of it passed -- including runs in which one station's flow had
doubled. A gate that cannot be shown to fire on a known regression is not evidence of anything.

So the load-bearing tests here inject a regression whose size is stated in vehicles and assert the
gate FAILS:

* ``test_power_*``           -- halve one station's count, shift the network total: the gate must fire.
* ``test_c2_*``              -- the exact case C2 names (reference c = 10, modelled m = 20, GEH
                                2.5820): the discarded Binomial(n, 1/2) bound 2.7718 PASSES it; the
                                calibrated bound must flag it.
* ``test_c1_*``              -- the discarded 0.03 network-total tolerance must be shown to
                                false-alarm on unchanged runs, and the calibrated one must not.
* ``test_no_false_alarm_*``  -- and the calibrated gate must NOT fire on genuine seed-vs-seed pairs,
                                because a gate that always fails is no better than one that always
                                passes.
* ``test_uncalibrated_*``    -- with no calibration on disk, a pass must be unquotable.

Tests that need real SUMO output read it from the per-seed counts embedded in the shipped
calibration (``tools/calibration/seed_stability_intas_urban_low.json``); they skip if it is absent.
Everything else is synthetic and runs offline in milliseconds.
"""
from __future__ import annotations

import argparse
import itertools
import json
import math
import os
import random
import statistics
import sys
from math import comb

import pytest

_TOOLS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools")
if _TOOLS not in sys.path:
    sys.path.insert(0, _TOOLS)
import sumo_realism as sr  # noqa: E402

REPO = os.path.dirname(_TOOLS)
SHIPPED_CALIBRATION = os.path.join(_TOOLS, "calibration", "seed_stability_intas_urban_low.json")
LEGACY_LINK_BOUND = 2.7718          # sqrt(2)*z_0.975, the discarded Binomial(n, 1/2) bound
LEGACY_TOTAL_REL_TOL = 0.03         # the discarded network-total tolerance (C1)


# ==================================================================================================
# fixtures / helpers
# ==================================================================================================
def _synthetic_runs(k: int = 12, phi: float = 0.22, seed: int = 4242) -> dict[str, dict[str, float]]:
    """K runs of a fake scenario with a KNOWN index of dispersion ``phi``.

    Deliberately under-dispersed (phi < 1) the way real route choice is, so the fallback
    Binomial(n, 1/2) null is wrong here for the same reason it is wrong on InTAS.
    """
    rng = random.Random(seed)
    means = {"A": 28.0, "B": 22.0, "C": 17.0, "D": 11.0, "E": 9.0, "F": 6.0,
             "G": 5.0, "H": 3.0, "I": 2.0, "J": 24.0, "K": 13.0, "L": 20.0}
    runs: dict[str, dict[str, float]] = {}
    for i in range(k):
        row = {}
        for s, mu in means.items():
            row[s] = float(max(0, round(mu + rng.gauss(0.0, math.sqrt(phi * mu)))))
        runs[str(i + 1)] = row
    return runs


def _calibrate(runs, **kw):
    kw.setdefault("bootstrap_b", 120)          # tests do not need 1200 resamples
    kw.setdefault("window_s", 300.0)
    kw.setdefault("scenario_id", "synthetic")
    return sr.calibrate_seed_stability(runs, **kw)


def _report(m, c, cal=None, window_s=300.0):
    """Window-native counts in -> the seed-stability report (counts are already per-window)."""
    per_h_m = {k: v * 3600.0 / window_s for k, v in m.items()}
    per_h_c = {k: v * 3600.0 / window_s for k, v in c.items()}
    return sr.seed_stability_report(per_h_m, per_h_c, window_s=window_s, calibration=cal)


def _failures(rep):
    return [g["id"] for g in rep["gates"] if g["status"] == "fail"]


@pytest.fixture(scope="module")
def shipped():
    if not os.path.exists(SHIPPED_CALIBRATION):
        pytest.skip("tools/calibration/seed_stability_intas_urban_low.json not present "
                    "(run tools/sumo_realism.py --calibrate)")
    return sr.load_calibration(SHIPPED_CALIBRATION)


@pytest.fixture(scope="module")
def real_runs(shipped):
    return {k: dict(v) for k, v in shipped["runs"].items()}


def _legacy_pair_fails(st):
    """Exactly what tools/sumo_realism.py graded before 2026-08-30, for BEFORE/AFTER contrast."""
    n = st["n"]
    fwer = sr._geh_bound(0.05 / n)
    pf = sum(1 for v in st["geh"].values() if v < LEGACY_LINK_BOUND) / n
    return (pf < 0.85,
            st["max_geh"] > fwer,
            (st["total_rel_error"] or 0.0) > LEGACY_TOTAL_REL_TOL,
            (st["count_band_pass_fraction"] or 1.0) < 0.80)


def _sweep(runs, thresholds, mutate=None):
    """(legacy_failures, calibrated_failures) counts over every run pair."""
    legacy = [0, 0, 0, 0, 0]
    new = [0, 0, 0, 0, 0]
    pairs = list(itertools.combinations(sorted(runs), 2))
    for a, b in pairs:
        ra = mutate(dict(runs[a])) if mutate else runs[a]
        st = sr.pair_statistics(ra, runs[b])
        lf, nf = _legacy_pair_fails(st), sr._pair_fails(thresholds, st)
        for i in range(4):
            legacy[i] += bool(lf[i]); new[i] += bool(nf[i])
        legacy[4] += any(lf); new[4] += any(nf)
    return legacy, new, len(pairs)


# ==================================================================================================
# THE POWER TESTS -- a gate that has never been shown to fail is not a gate
# ==================================================================================================
def test_power_halving_one_station_fails_the_gate_synthetic():
    """Halve ONE station in an otherwise identical pair of runs -> the gate must FAIL."""
    runs = _synthetic_runs()
    cal = _calibrate(runs)
    a, b = runs["1"], runs["2"]

    clean = _report(a, b, cal)
    assert _failures(clean) == [], "the two genuine runs must not trip anything first"

    hurt = dict(a)
    hurt["A"] = hurt["A"] / 2.0                       # ~28 -> ~14 vehicles at one station
    rep = _report(hurt, b, cal)
    assert "seed_stability.max_link_geh" in _failures(rep), (
        f"halving station A ({a['A']} -> {hurt['A']} vs {b['A']}) must fail the worst-station "
        f"gate; report said {json.dumps([(g['id'], g['status'], g['value']) for g in rep['gates']])}")
    assert "A" in rep["stations_outside_geh_band"]
    assert rep["calibration"]["status"] == "calibrated"


def test_power_total_flow_shift_fails_the_gate_synthetic():
    """Move the whole network total and the total-flow gate must FAIL (C1's gate, calibrated)."""
    runs = _synthetic_runs()
    cal = _calibrate(runs)
    a, b = runs["1"], runs["2"]
    tol = cal["thresholds"]["total_flow_rel_tol"]

    shifted = {k: v * 0.80 for k, v in a.items()}     # -20%, far outside the measured null
    rep = _report(shifted, b, cal)
    assert "seed_stability.total_flow_rel_error" in _failures(rep)
    gate = next(g for g in rep["gates"] if g["id"] == "seed_stability.total_flow_rel_error")
    assert gate["value"] > tol
    assert gate["reference"]["threshold"]


def test_power_halving_one_station_on_real_intas_runs(real_runs, shipped):
    """Same injection on the real 20-seed InTAS material, measured over all 190 run pairs.

    BEFORE: the two GEH gates caught essentially nothing -- every "detection" the old gate set
    produced came from the 0.03 total-flow gate that also fires on 45% of UNCHANGED pairs, so it was
    not power at all. AFTER: the worst-station gate alone catches most of them.
    """
    th = shipped["thresholds"]
    means = {s: statistics.mean(r.get(s, 0.0) for r in real_runs.values())
             for s in sorted({k for r in real_runs.values() for k in r})}
    biggest = max(means, key=lambda s: means[s])
    legacy, new, n = _sweep(real_runs, th, mutate=lambda r: {**r, biggest: r[biggest] * 0.5})

    assert legacy[1] <= 0.05 * n, (
        f"BEFORE: the Binomial(n,1/2) worst-station gate should be near-powerless, "
        f"caught {legacy[1]}/{n}")
    assert new[1] >= 0.70 * n, (
        f"AFTER: the calibrated worst-station gate must catch a halved {biggest} "
        f"(mean {means[biggest]:.1f} veh) in most pairs, caught {new[1]}/{n}")
    assert new[4] >= 0.75 * n, f"AFTER: whole-report detection only {new[4]}/{n}"


def test_power_total_flow_shift_on_real_intas_runs(real_runs, shipped):
    th = shipped["thresholds"]
    for factor, floor in ((0.85, 0.90), (0.50, 1.00)):
        _legacy, new, n = _sweep(real_runs, th,
                                 mutate=lambda r, f=factor: {k: v * f for k, v in r.items()})
        assert new[2] >= floor * n * 0.95, (
            f"AFTER: a x{factor} network-total shift must fail the total-flow gate, "
            f"caught {new[2]}/{n}")
        assert new[4] >= floor * n, f"AFTER: whole-report detection only {new[4]}/{n} at x{factor}"


def test_power_the_gate_can_fail_even_uncalibrated(real_runs):
    """A large enough regression must fail even under the loose fallback -- and stay a FAIL."""
    a = dict(next(iter(real_runs.values())))
    b = dict(list(real_runs.values())[1])
    wrecked = {k: v * 3.0 for k, v in a.items()}
    rep = _report(wrecked, b, cal=None)
    assert "seed_stability.link_geh_pass_fraction" in _failures(rep)
    gate = next(g for g in rep["gates"] if g["id"] == "seed_stability.link_geh_pass_fraction")
    assert gate["status"] == "fail" and gate["note"] == sr.UNCALIBRATED_FAIL_NOTE
    assert "provisional_status" not in gate          # a failure is never demoted


# ==================================================================================================
# and the calibrated gate must NOT fire on genuine seed-vs-seed pairs
# ==================================================================================================
def test_no_false_alarm_on_a_genuine_seed_pair(real_runs, shipped):
    """The published baseline pair (seed 23423 vs 987654) must pass every calibrated gate."""
    if "23423" not in real_runs or "987654" not in real_runs:
        pytest.skip("shipped calibration does not carry the baseline seed pair")
    rep = _report(real_runs["23423"], real_runs["987654"], shipped)
    assert _failures(rep) == []
    assert all(g["status"] == "pass" for g in rep["gates"])
    assert rep["uncalibrated"] is False


def test_measured_false_alarm_rate_over_every_seed_pair(real_runs, shipped):
    """BEFORE 45%+ of unchanged pairs failed; AFTER the whole report must sit near alpha."""
    legacy, new, n = _sweep(real_runs, shipped["thresholds"])
    assert legacy[2] >= 0.40 * n, (
        f"C1 regression guard: the 0.03 tolerance is supposed to false-alarm; it fired on "
        f"{legacy[2]}/{n}")
    assert new[4] <= 0.10 * n, (
        f"AFTER: the calibrated report must not false-alarm on unchanged runs, "
        f"fired on {new[4]}/{n}")
    assert new[2] <= 0.05 * n, f"AFTER: total-flow gate fired on {new[2]}/{n} unchanged pairs"


def test_synthetic_calibration_holds_its_own_alpha():
    runs = _synthetic_runs(k=14, seed=99)
    cal = _calibrate(runs)
    assert cal["validation"]["in_sample_any_gate_rate"] <= cal["alpha_report"] + 1e-9
    # the out-of-sample estimate is the honest one and must at least be recorded
    assert cal["validation"]["leave_one_seed_out_any_gate_rate"] is not None


# ==================================================================================================
# C1 -- the network-total tolerance
# ==================================================================================================
def test_c1_the_003_tolerance_is_gone_and_the_calibrated_one_is_wider(shipped):
    assert sr.SEED_STABILITY_TOTAL_REL_TOL is None, (
        "the invented 0.03 network-total tolerance must not survive as a usable constant")
    tol = shipped["thresholds"]["total_flow_rel_tol"]
    spread = shipped["network_total"]["spread_max_min_over_mean"]
    assert tol > LEGACY_TOTAL_REL_TOL, "a calibrated tolerance must not be tighter than the truth"
    assert tol >= spread * 0.9, (
        f"the calibrated tolerance {tol:.4f} must cover the measured seed-to-seed spread {spread:.4f}")
    assert shipped["legacy_thresholds_on_these_pairs"]["total_rel_tol_0p03_false_alarm_rate"] > 0.4


def test_c1_uncalibrated_total_flow_gate_reports_no_threshold():
    """With no calibration there is no defensible tolerance, so the gate must say so, not guess."""
    runs = _synthetic_runs()
    rep = _report(runs["1"], runs["2"], cal=None)
    gate = next(g for g in rep["gates"] if g["id"] == "seed_stability.total_flow_rel_error")
    assert gate["status"] == "na"
    assert gate["reason"] == sr.NO_TOTAL_TOLERANCE_REASON
    assert "reference" not in gate and gate["value"] is not None


# ==================================================================================================
# C2 -- the per-station null and the power it destroyed
# ==================================================================================================
def test_c2_a_2x_flow_change_at_c10_passes_the_old_bound_and_is_flagged_by_the_new_one(shipped):
    """The exact case REVIEW-FINDINGS C2 names: c = 10, m = 20, GEH = 2.5820."""
    g = math.sqrt(2.0 * (20 - 10) ** 2 / 30.0)
    assert round(g, 4) == 2.5820
    assert g < LEGACY_LINK_BOUND, "the premise of C2: the old bound let a 2x change through"
    assert g >= shipped["thresholds"]["link_geh_bound"], (
        "the calibrated bound must flag a 2x change at a 10-vehicle station")

    rep = _report({"S": 20.0, "T": 10.0, "U": 10.0}, {"S": 10.0, "T": 10.0, "U": 10.0}, shipped)
    assert "S" in rep["stations_outside_geh_band"]
    assert rep["geh"]["stations"][0]["geh"] == pytest.approx(2.5820, abs=1e-4)


def test_c2_the_baseline_station_4140_flips_from_pass_to_flagged(real_runs, shipped):
    """C2's own exhibit: baseline station 4140, 9 vs 17 vehicles, GEH 2.2188, reported 'pass'."""
    if "23423" not in real_runs or "987654" not in real_runs:
        pytest.skip("shipped calibration does not carry the baseline seed pair")
    m, c = real_runs["23423"]["4140"], real_runs["987654"]["4140"]
    geh = sr._geh(m, c)
    assert {m, c} == {9.0, 17.0} and round(geh, 4) == 2.2188
    assert geh < LEGACY_LINK_BOUND                                   # old verdict: pass
    assert geh >= shipped["thresholds"]["link_geh_bound"]            # new verdict: flagged
    rep = _report(real_runs["23423"], real_runs["987654"], shipped)
    assert "4140" in rep["stations_outside_geh_band"]


def test_c2_measured_dispersion_is_far_below_the_binomial_null(shipped):
    phi = shipped["dispersion"]["phi_pooled"]
    assert 0.05 < phi < 0.5, f"measured index of dispersion {phi} should be strongly under-dispersed"
    assert shipped["dispersion"]["binomial_null_value"] == 1.0
    # and the bound moves with it: sqrt(2*phi)*z, not sqrt(2)*z
    assert shipped["thresholds"]["link_geh_bound"] < LEGACY_LINK_BOUND
    assert sr._geh_bound(0.05, phi=1.0) == pytest.approx(LEGACY_LINK_BOUND, abs=1e-4)
    assert sr._geh_bound(0.05, phi=phi) < sr._geh_bound(0.05, phi=1.0)


def test_c2_measured_exceedance_of_the_old_bound_was_nowhere_near_the_documented_alpha(shipped):
    lg = shipped["legacy_thresholds_on_these_pairs"]
    assert lg["link_geh_bound_binomial"] == pytest.approx(LEGACY_LINK_BOUND, abs=1e-3)
    assert lg["documented_exceedance_rate"] == 0.05
    assert lg["link_geh_binomial_exceedance_rate"] < 0.01, (
        "the whole of C2: the documented 0.05 per-station false-alarm rate was measured at "
        f"{lg['link_geh_binomial_exceedance_rate']}")


def test_detectability_is_reported_so_the_gates_power_is_never_implicit(shipped, real_runs):
    rep = _report(real_runs["23423"], real_runs["987654"], shipped)
    d = rep["detectability"]
    assert d["link_geh_bound"] == pytest.approx(shipped["thresholds"]["link_geh_bound"], abs=1e-4)
    assert 1.0 < d["min_detectable_ratio_median"] < 2.0
    # small stations genuinely cannot see a 2x change: say so rather than implying otherwise
    assert isinstance(d["stations_where_a_2x_change_is_invisible"], list)
    for st in rep["geh"]["stations"]:
        if st["counted"] > 0:
            assert st["min_detectable_ratio"] > 1.0


def test_min_detectable_ratio_is_the_inverse_of_the_geh_bound():
    for c in (2.0, 5.0, 10.0, 30.0, 100.0):
        for bound in (1.0, 1.5370, 2.7718):
            r = sr._min_detectable_ratio(c, bound)
            assert sr._geh(r * c, c) == pytest.approx(bound, rel=1e-6)


# ==================================================================================================
# the documented-rate errors flagged in REVIEW-FINDINGS "Minor"
# ==================================================================================================
def test_binomial_gate_trip_probability_is_00222_not_15_percent():
    """`tools/sumo_realism.py:526` said "~1.5% at N=22"; P(Bin(22, 0.05) >= 4) = 0.0222."""
    p = sum(comb(22, k) * 0.05 ** k * 0.95 ** (22 - k) for k in range(4, 23))
    assert round(p, 4) == 0.0222
    assert math.ceil(0.85 * 22) == 19 and 22 - 19 + 1 == 4     # 4 failures trip the 0.85 gate

    runs = _synthetic_runs()
    note = next(g for g in _report(runs["1"], runs["2"], cal=None)["gates"]
                if g["id"] == "seed_stability.link_geh_pass_fraction")["reference"]["source"]
    assert "0.0222" in note
    # the old figure may only appear as the thing being corrected, never as the claim
    assert "NOT the '~1.5%' documented" in note
    assert "documented false-alarm budget (~1.5% at N=22)" not in note


@pytest.mark.parametrize("n,expected", [
    (1, 0.0), (2, 0.0), (3, 0.0), (4, 0.125), (5, 0.0625), (6, 0.03125), (7, 0.015625),
    (8, 0.0703125), (10, 0.021484375), (13, 0.0224609375), (17, 0.049041748046875),
])
def test_binomial_per_station_failure_probability_is_not_a_flat_005(n, expected):
    """`:523-524` claimed each station fails with probability 0.05. Discreteness says otherwise."""
    z = LEGACY_LINK_BOUND / math.sqrt(2.0)
    p = sum(comb(n, k) for k in range(n + 1) if abs(2 * k - n) >= z * math.sqrt(n)) / 2 ** n
    assert p == pytest.approx(expected, abs=1e-9)
    if n <= 3:
        assert p == 0.0, "a station with 3 or fewer vehicles can never fail the fallback bound"


def test_calibrated_bound_gives_the_small_stations_power_back(shipped):
    """Under the fallback, stations 3021 (n=2) and 5060 (n=3) could never fail. Now they can."""
    bound = shipped["thresholds"]["link_geh_bound"]
    for n in (2, 3):
        p_fallback = sum(comb(n, k) for k in range(n + 1)
                         if 2 * (2 * k - n) ** 2 / n >= LEGACY_LINK_BOUND ** 2) / 2 ** n
        p_cal = sum(comb(n, k) for k in range(n + 1)
                    if 2 * (2 * k - n) ** 2 / n >= bound ** 2) / 2 ** n
        assert p_fallback == 0.0
        assert p_cal > 0.0, f"n={n} still has zero power under the calibrated bound {bound}"


def test_fallback_note_states_the_discreteness_instead_of_a_flat_alpha():
    runs = _synthetic_runs()
    note = next(g for g in _report(runs["1"], runs["2"], cal=None)["gates"]
                if g["id"] == "seed_stability.link_geh_pass_fraction")["reference"]["source"]
    assert "discrete" in note.lower()
    assert "CANNOT fail" in note and "n <= 3" in note
    assert "each station fails with probability 0.05" not in note


# ==================================================================================================
# uncalibrated results must never be quotable
# ==================================================================================================
def test_uncalibrated_report_demotes_every_pass_to_na():
    runs = _synthetic_runs()
    rep = _report(runs["1"], runs["1"], cal=None)            # identical: everything would pass
    assert rep["uncalibrated"] is True
    assert rep["calibration"]["status"] == "uncalibrated"
    assert rep["calibration"]["warning"] == sr.UNCALIBRATED_WARNING
    for g in rep["gates"]:
        assert g["status"] != "pass", f"{g['id']} passed without a calibration"
        assert g["calibrated"] is False
    demoted = [g for g in rep["gates"] if g.get("provisional_status") == "pass"]
    assert len(demoted) == 3                                  # total-flow has no threshold at all
    for g in demoted:
        assert g["reason"] == sr.UNCALIBRATED_PASS_REASON


def test_uncalibrated_flag_reaches_the_top_level_summary(tmp_path):
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 40), ("d2", 30)])
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 41), ("d2", 29)])
    rep = sr.build(_ns(det_out=a, det_add=add, ref_det_out=b, no_calibration=True))
    assert rep["uncalibrated"] is True
    assert rep["warning_uncalibrated"] == sr.UNCALIBRATED_WARNING
    assert rep["summary"]["quotable_as_a_gate_result"] is False
    assert rep["summary"]["n_gates_demoted_uncalibrated"] == 3
    assert rep["summary"]["fail"] == 0                        # demoted, not failed
    assert "UNCALIBRATED" in sr.render(rep)


def test_require_calibration_turns_a_missing_calibration_into_a_failure(tmp_path):
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 40), ("d2", 30)])
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 41), ("d2", 29)])
    rc = sr.main(["--det-out", a, "--det-add", add, "--ref-det-out", b,
                  "--no-calibration", "--require-calibration"])
    assert rc == 1
    rc_ok = sr.main(["--det-out", a, "--det-add", add, "--ref-det-out", b, "--no-calibration"])
    assert rc_ok == 0


def test_same_seed_regression_gates_need_no_calibration(tmp_path):
    """A zero-tolerance identity check has no null model, so it stays quotable uncalibrated."""
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 40), ("d2", 30)])
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 40), ("d2", 30)])
    rep = sr.build(_ns(det_out=a, det_add=add, ref_det_out=b, seed="7", ref_seed="7",
                       no_calibration=True))
    g = rep["sections"]["geh"]
    assert g["comparison_kind"] == sr.KIND_REGRESSION
    assert [gate["status"] for gate in g["gates"]] == ["pass", "pass"]
    assert not rep.get("uncalibrated")

    drift = _write_e1_out(tmp_path / "c.xml", [("d1", 41), ("d2", 30)])
    rep2 = sr.build(_ns(det_out=drift, det_add=add, ref_det_out=b, seed="7", ref_seed="7",
                        no_calibration=True))
    assert "regression.identical_station_counts" in rep2["summary"]["failures"]


# ==================================================================================================
# calibration mechanics
# ==================================================================================================
def test_calibration_is_deterministic():
    runs = _synthetic_runs()
    a, b = _calibrate(runs), _calibrate(runs)
    a.pop("generated_utc"); b.pop("generated_utc")
    assert json.dumps(a, sort_keys=True, default=str) == json.dumps(b, sort_keys=True, default=str)


def test_calibration_recovers_the_injected_dispersion():
    for phi in (0.10, 0.25, 0.60):
        cal = _calibrate(_synthetic_runs(k=16, phi=phi, seed=int(phi * 1000)))
        got = cal["dispersion"]["phi_pooled"]
        assert got == pytest.approx(phi, rel=0.45), f"phi {phi} recovered as {got}"
        assert cal["dispersion"]["phi_upper"] >= got
        assert cal["thresholds"]["link_geh_bound"] < LEGACY_LINK_BOUND


def test_calibration_records_its_provenance():
    cal = _calibrate(_synthetic_runs(), sumo_version="Eclipse SUMO sumo 1.25.0",
                     scenario_hash={"sha256": "deadbeef"})
    assert cal["schema"] == sr.CALIBRATION_SCHEMA
    assert cal["sumo_version"] == "Eclipse SUMO sumo 1.25.0"
    assert cal["scenario_hash"]["sha256"] == "deadbeef"
    assert cal["n_seeds"] == len(cal["seeds"]) == 12 and cal["n_pairs"] == 66
    assert cal["window_s"] == 300.0 and cal["group_by"] == "station"
    assert cal["bootstrap"]["rng_seed"] == sr.CALIB_BOOTSTRAP_SEED
    assert cal["runs"], "the per-seed counts must be persisted so the calibration is auditable"
    assert cal["caveats"] and cal["licence_note"] == sr.LICENCE_NOTICE


def test_calibration_flags_low_confidence_below_the_recommended_seed_count():
    assert _calibrate(_synthetic_runs(k=10))["low_confidence"] is True
    assert _calibrate(_synthetic_runs(k=sr.CALIB_RECOMMENDED_SEEDS))["low_confidence"] is False


def test_calibration_is_refused_when_the_window_or_grouping_does_not_match(tmp_path):
    cal = _calibrate(_synthetic_runs(), window_s=300.0)
    path = tmp_path / "cal.json"
    path.write_text(json.dumps(cal, default=str), encoding="utf-8")

    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 40), ("d2", 30)], end=3600.0)
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 41), ("d2", 29)], end=3600.0)
    rep = sr.build(_ns(det_out=a, det_add=add, ref_det_out=b, calibration=str(path)))
    assert rep["uncalibrated"] is True
    assert "REFUSED" in rep["calibration_lookup"]["reason"]
    assert "window" in rep["calibration_lookup"]["reason"]


def test_load_calibration_rejects_a_foreign_document(tmp_path):
    p = tmp_path / "x.json"
    p.write_text(json.dumps({"schema": "something/else", "thresholds": {}}), encoding="utf-8")
    with pytest.raises(ValueError, match="not a"):
        sr.load_calibration(p)


def test_calibrate_cli_offline_writes_a_usable_calibration(tmp_path, capsys):
    """--calibrate straight from existing per-seed detector outputs (no SUMO in the loop)."""
    add = _write_e1_add(tmp_path / "E1.add.xml",
                        [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2"), ("d3", "eC_0", "S3")])
    runs = _synthetic_runs(k=12, seed=5)
    args = ["--calibrate", "--det-add", add, "--scenario-id", "cli_synth",
            "--calib-out", str(tmp_path / "cal.json")]
    for i, (s, row) in enumerate(runs.items()):
        vals = list(row.values())
        path = _write_e1_out(tmp_path / f"s{s}.xml",
                             [("d1", vals[0]), ("d2", vals[1]), ("d3", vals[2])], end=300.0)
        args += ["--calib-det-out", f"{s}={path}"]
    assert sr.main(args) == 0
    out = capsys.readouterr().out
    assert "Seed-stability calibration for `cli_synth`" in out
    assert "MEASURED FALSE-ALARM RATE" in out
    assert "License Not Specified" in out or "REFERENCE-DATA LICENCE" in out

    cal = sr.load_calibration(tmp_path / "cal.json")
    assert cal["scenario_id"] == "cli_synth" and cal["n_seeds"] == 12
    assert cal["window_s"] == 300.0
    assert set(cal["runs"]["1"]) == {"S1", "S2", "S3"}


def test_calibrate_cli_refuses_too_few_seeds(tmp_path):
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "S1")])
    args = ["--calibrate", "--det-add", add, "--calib-out", str(tmp_path / "c.json")]
    for s in range(3):
        p = _write_e1_out(tmp_path / f"s{s}.xml", [("d1", 10 + s)], end=300.0)
        args += ["--calib-det-out", f"{s}={p}"]
    with pytest.raises(SystemExit) as e:
        sr.main(args)
    assert e.value.code == 2


def test_parse_seed_spec():
    assert sr.parse_seed_spec("1-5") == [1, 2, 3, 4, 5]
    assert sr.parse_seed_spec("1,2,23423") == [1, 2, 23423]
    assert sr.parse_seed_spec("1-3,3,7") == [1, 2, 3, 7]


def test_pair_statistics_matches_the_gate_it_feeds():
    m = {"A": 20.0, "B": 10.0, "C": 0.0}
    c = {"A": 10.0, "B": 10.0, "C": 0.0}
    st = sr.pair_statistics(m, c)
    assert st["stations"] == ["A", "B"] and st["n"] == 2      # C is both-zero -> excluded
    assert st["geh"]["A"] == pytest.approx(2.5820, abs=1e-4)
    assert st["max_geh"] == pytest.approx(2.5820, abs=1e-4)
    assert st["total_rel_error"] == pytest.approx(10.0 / 20.0)
    assert st["dispersion_terms"][0] == pytest.approx(100.0 / 30.0)


def test_scenario_hash_changes_with_the_detector_layout(tmp_path):
    a = _write_e1_add(tmp_path / "a.add.xml", [("d1", "eA_0", "S1")])
    b = _write_e1_add(tmp_path / "b.add.xml", [("d1", "eA_0", "S1"), ("d2", "eB_0", "S2")])
    assert sr.scenario_hash(None, a)["sha256"] != sr.scenario_hash(None, b)["sha256"]
    assert sr.scenario_hash(None, a)["sha256"] == sr.scenario_hash(None, a)["sha256"]


def test_e1_output_file_picks_the_majority_target(tmp_path):
    p = tmp_path / "E1.add.xml"
    p.write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n<additional>\n'
        '  <e1Detector id="a" lane="x_0" pos="1" freq="900" name="S1" file="Det.xml"/>\n'
        '  <e1Detector id="b" lane="y_0" pos="1" freq="900" name="S1" file="Det.xml"/>\n'
        '  <e1Detector id="income" lane="z_0" pos="1" freq="900" file="gate.xml"/>\n'
        "</additional>\n", encoding="utf-8")
    assert sr.e1_output_file(str(p)) == "Det.xml"


# ==================================================================================================
# the shipped InTAS calibration itself
# ==================================================================================================
def test_shipped_calibration_is_self_consistent(shipped):
    assert shipped["schema"] == sr.CALIBRATION_SCHEMA
    assert shipped["n_seeds"] >= sr.CALIB_MIN_SEEDS
    assert shipped["n_pairs"] == shipped["n_seeds"] * (shipped["n_seeds"] - 1) // 2
    assert shipped["window_s"] == 300.0 and shipped["group_by"] == "station"
    assert shipped["sumo_version"].startswith("Eclipse SUMO")
    assert shipped["scenario_hash"]["sha256"]
    th = shipped["thresholds"]
    for key in ("link_geh_bound", "max_link_geh_bound", "link_geh_min_pass_fraction",
                "total_flow_rel_tol", "link_count_min_pass_fraction"):
        assert th[key] is not None
    assert th["link_geh_bound"] < th["max_link_geh_bound"]
    v = shipped["validation"]
    assert v["in_sample_any_gate_rate"] <= shipped["alpha_report"]
    assert v["leave_one_seed_out_any_gate_rate"] <= 0.12       # honest out-of-sample figure
    assert shipped["heavy_tailed_stations"]                    # the null IS heavy-tailed here
    assert len(shipped["runs"]) == shipped["n_seeds"]


def test_shipped_calibration_reproduces_from_its_own_recorded_runs(shipped):
    """The persisted runs must regenerate the persisted thresholds -- no hand-edited numbers."""
    again = sr.calibrate_seed_stability(
        {k: dict(v) for k, v in shipped["runs"].items()},
        alpha=shipped["alpha_report"], window_s=shipped["window_s"],
        group_by=shipped["group_by"], scenario_id=shipped["scenario_id"])
    for key in ("link_geh_bound", "max_link_geh_bound", "link_geh_min_pass_fraction",
                "total_flow_rel_tol", "link_count_min_pass_fraction"):
        assert again["thresholds"][key] == pytest.approx(shipped["thresholds"][key], rel=1e-9)
    assert again["dispersion"]["phi_pooled"] == pytest.approx(
        shipped["dispersion"]["phi_pooled"], rel=1e-9)


def test_shipped_calibration_is_auto_discovered_by_scenario_id(shipped, tmp_path):
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "1010")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 11)], end=300.0)
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 12)], end=300.0)
    rep = sr.build(_ns(det_out=a, det_add=add, ref_det_out=b, scenario_id="intas_urban_low"))
    assert rep["calibration_lookup"]["how"] == "auto"
    assert rep["sections"]["geh"]["calibration"]["status"] == "calibrated"
    assert not rep.get("uncalibrated")


def test_render_always_prints_provenance_and_the_licence_caveat(shipped, tmp_path):
    add = _write_e1_add(tmp_path / "E1.add.xml", [("d1", "eA_0", "1010")])
    a = _write_e1_out(tmp_path / "a.xml", [("d1", 11)], end=300.0)
    b = _write_e1_out(tmp_path / "b.xml", [("d1", 12)], end=300.0)
    text = sr.render(sr.build(_ns(det_out=a, det_add=add, ref_det_out=b,
                                  scenario_id="intas_urban_low")))
    assert "## Provenance and licence" in text
    assert "License Not Specified" in text
    assert ".realism_cache/" in text and "MUST NOT be committed" in text
    assert "CALIBRATED from" in text and "leave-one-seed-out" in text
    assert shipped["sumo_version"] in text


def test_calibration_data_is_our_own_output_not_third_party_counts(shipped):
    """The licence constraint: nothing measured may be vendored. This file is SUMO output only."""
    assert shipped["licence_note"] == sr.LICENCE_NOTICE
    assert "License Not Specified" in shipped["licence_note"]
    cache = str(shipped.get("inputs", {}).get("cache_dir", ""))
    assert ".realism_cache" in cache or cache == ""
    gitignore = open(os.path.join(REPO, ".gitignore"), encoding="utf-8").read()
    assert "/.realism_cache/" in gitignore


# ==================================================================================================
# local file writers (kept here so this module does not depend on test_realism_bench)
# ==================================================================================================
def _write_e1_add(path, dets):
    path = str(path)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<additional>\n')
        for did, lane, name in dets:
            fh.write(f'  <e1Detector id="{did}" lane="{lane}" pos="1.0" freq="900.00" '
                     f'name="{name}" file="out.xml" friendlyPos="1"/>\n')
        fh.write("</additional>\n")
    return path


def _write_e1_out(path, rows, begin=0.0, end=300.0):
    path = str(path)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<detector>\n')
        for did, n in rows:
            fh.write(f'  <interval begin="{begin:.2f}" end="{end:.2f}" id="{did}" '
                     f'nVehContrib="{n}" flow="{n * 3600.0 / max(end - begin, 1e-9):.2f}" '
                     f'occupancy="1.0" speed="12.0" length="4.5" nVehEntered="{n}"/>\n')
        fh.write("</detector>\n")
    return path


def _ns(**kw):
    """A build() Namespace with every flag defaulted, so tests only state what they mean."""
    base = dict(det_out=None, det_add=None, ref_counts=None, ref_det_out=None, fcd=None,
                group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
                refdata=None, json_out=None, no_fail=False, seed=None, ref_seed=None,
                calibration=None, calibration_dir=None, no_calibration=False,
                require_calibration=False, scenario_id=None)
    base.update(kw)
    return argparse.Namespace(**base)
