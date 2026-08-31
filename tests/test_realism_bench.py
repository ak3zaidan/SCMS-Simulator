"""Tests for the Phase-0 realism benchmark harness.

Every metric is exercised on a TINY SYNTHETIC input with a hand-computable answer -- constant-
acceleration tracks, a rigid platoon, a single space-time cell with a known Edie flow, a reception
curve with a known shape -- so the suite is deterministic and fast (no pipeline run, no SUMO).
Three contracts get their own tests because other code depends on them:

  * ``ks_statistic`` must reproduce ``calibration._ks_vs_rayleigh`` to 1e-9 (the harness reuses the
    project's existing KS implementation rather than forking a second one);
  * ``geh`` must match published GEH values;
  * the scorecard must be read-only, deterministic, and free of any forbidden (ORACLE) feature key.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import re
import sys

import numpy as np
import pytest

from scms_sim_ref.datagen import calibration as calib
from scms_sim_ref.datagen import corpus_report as cr
from scms_sim_ref.datagen import datasheet as ds
from scms_sim_ref.datagen import realism_bench as rb
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys

REFDATA = rb.load_refdata()


# ==================================================================================================
# synthetic dataset builders
# ==================================================================================================
def _write_jsonl(path, rows):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        for r in rows:
            fh.write(json.dumps(r, sort_keys=True) + "\n")


def _straight_line_emissions(n_veh=6, n_steps=40, dt=1.0, v=12.0, spacing=60.0, accel=0.0):
    """A single-file platoon on the x axis: vehicle j starts at -j*spacing and drives east."""
    rows = []
    k = 0
    for step in range(n_steps):
        t = step * dt
        for j in range(n_veh):
            x = -j * spacing + v * t + 0.5 * accel * t * t
            # a deterministic (seed-free) GNSS wobble on the CLAIMED position: it never touches a
            # mobility metric (those read true_x/true_y) but keeps calibration.py's Rayleigh fit
            # from dividing by a zero scale when a datasheet is built over this fixture.
            ox = 1.0 + 0.25 * ((step * 7 + j * 3) % 9)
            oy = 0.5 + 0.25 * ((step * 5 + j * 2) % 7)
            rows.append({"_visibility": "ORACLE", "emit_id": f"emt_{k:08d}", "t": round(t, 3),
                         "true_vehicle_id": f"veh_{j:03d}", "true_x": round(x, 3), "true_y": 0.0,
                         "claimed_x": round(x + ox, 3), "claimed_y": round(oy, 3),
                         "claimed_speed": round(v + accel * t, 3), "pos_conf": 5.0,
                         "is_attacker": False, "is_faulty": False, "falsified": False})
            k += 1
    return rows


def _make_dataset(root, *, emissions, reports=None, labels=None, config=None,
                  generator="scms_sim_ref.mock_pipeline (pre-MOSAIC reference, realistic v2)"):
    cfg = {"emit_sample_prob": 1.0, "road_network": "linear", "n_lanes": 1, "dt": 1.0,
           "radio_range_m": 250.0, "art_max_m": 150.0}
    cfg.update(config or {})
    os.makedirs(os.path.join(root, "ground_truth"), exist_ok=True)
    os.makedirs(os.path.join(root, "ma"), exist_ok=True)
    _write_jsonl(os.path.join(root, "ground_truth", "gt_emissions_sample.jsonl"), emissions)
    _write_jsonl(os.path.join(root, "ground_truth", "gt_report_labels.jsonl"), labels or [])
    _write_jsonl(os.path.join(root, "ma", "ma_reports.jsonl"), reports or [])
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump({"dataset_version": "0.3.0", "generator": generator, "seed": 1, "config": cfg},
                  fh, indent=2)
    return root


# ==================================================================================================
# KS self-check: identical to calibration.py's implementation
# ==================================================================================================
@pytest.mark.parametrize("sigma", [0.5, 2.5 / 1.1774, 7.0])
def test_ks_statistic_matches_calibration_within_1e_9(sigma):
    """The harness's generic KS reproduces calibration._ks_vs_rayleigh exactly (same estimator)."""
    rng = np.random.default_rng(20260830)
    err = rng.rayleigh(scale=2.0, size=500)
    ours = rb.ks_statistic(err, lambda x: 1.0 - np.exp(-(x ** 2) / (2.0 * sigma ** 2)))
    theirs = calib._ks_vs_rayleigh(err, sigma)
    assert abs(ours - theirs) < 1e-9, (ours, theirs)


def test_ks_statistic_reproduces_calibration_on_a_real_calibrate_call(tmp_path):
    """End-to-end: the number calibration.calibrate() publishes is recomputable with ks_statistic."""
    rng = np.random.default_rng(7)
    n = 400
    ex, ey = rng.normal(0, 2.0, n), rng.normal(0, 2.0, n)
    emis = [{"true_x": 0.0, "true_y": 0.0, "claimed_x": float(ex[i]), "claimed_y": float(ey[i]),
             "pos_conf": 6.0, "is_attacker": False, "is_faulty": False, "falsified": False,
             "t": float(i), "true_vehicle_id": f"veh_{i:03d}"} for i in range(n)]
    root = _make_dataset(str(tmp_path / "cal"), emissions=emis)
    out = calib.calibrate(root)["gps_error"]
    err = np.hypot(ex, ey)
    ours = rb.ks_statistic(err, lambda x: 1.0 - np.exp(-(x ** 2) / (2.0 * calib.REF_SIGMA ** 2)))
    assert abs(round(ours, 3) - out["ks_vs_reference_rayleigh"]) < 1e-9


def test_ks_statistic_empty_sample_is_nan():
    assert math.isnan(rb.ks_statistic([], lambda x: x))


# ==================================================================================================
# GEH
# ==================================================================================================
def test_geh_known_values():
    assert rb.geh(100, 100) == 0.0
    assert rb.geh(0, 0) == 0.0                                  # zero-guard, not a division error
    assert rb.geh(0, 100) == pytest.approx(math.sqrt(200.0))    # 14.142135...
    assert rb.geh(200, 100) == pytest.approx(math.sqrt(200.0 / 3.0))   # 8.164965...
    assert rb.geh(1000, 1100) == pytest.approx(math.sqrt(2 * 100.0 ** 2 / 2100.0))  # 3.0860...
    # the published worked example: 5% off a 1000 veh/h link is comfortably inside GEH < 5
    assert rb.geh(1050, 1000) < 5.0
    # symmetry in its two arguments
    assert rb.geh(731, 909) == pytest.approx(rb.geh(909, 731))


def test_geh_summary_aggregates_sorts_and_flags_empty_stations():
    s = rb.geh_summary([("b", 1000, 1000), ("a", 0, 100), ("c", 0, 0)], geh_max=5.0)
    assert [r["station"] for r in s["stations"]] == ["a", "b", "c"]      # deterministic order
    assert s["n_stations"] == 3 and s["n_both_zero"] == 1
    assert s["stations"][0]["geh"] == pytest.approx(math.sqrt(200.0), abs=1e-4)
    assert s["pass_fraction"] == pytest.approx(2 / 3, abs=1e-4)   # only the 0-vs-100 station fails
    assert s["total_modelled"] == 1000.0 and s["total_counted"] == 1100.0
    assert s["total_geh"] == pytest.approx(rb.geh(1000, 1100), abs=1e-4)
    assert s["total_rel_error"] == pytest.approx((1000 - 1100) / 1100, abs=1e-5)


# ==================================================================================================
# trajectory reconstruction: speed / acceleration / teleports / overlap
# ==================================================================================================
def test_finite_difference_speed_and_acceleration_are_exact():
    emis = _straight_line_emissions(n_veh=1, n_steps=30, v=0.0, accel=2.0)
    tracks = rb.build_tracks(emis)
    segs = rb.segment_table(tracks)
    # x = 0.5*a*t^2 -> mean speed over [t, t+1] = a*(t+0.5); accel between consecutive segments = a
    assert segs["n"] == 29
    assert segs["speed"][0] == pytest.approx(1.0)      # a*(0+0.5)
    assert segs["speed"][5] == pytest.approx(11.0)     # a*(5+0.5)
    assert np.allclose(segs["speed_long"], segs["speed"])       # straight line: no lateral component
    acc = rb.accelerations(tracks)
    # 28 finite-difference pairs, less the two that touch the track's first/last step (a vehicle is
    # inserted part-way through a step and its last sample is clamped at arrival, so neither of those
    # displacements is a step's worth of driving -- see test_arrival_clamp_... below)
    assert acc.size == 26
    assert np.allclose(acc, 2.0)
    assert rb.kinematics(tracks)["accel_all"].size == 28        # unscreened series is still there


def test_long_sampling_gaps_are_dropped_not_finite_differenced():
    emis = [{"t": 0.0, "true_vehicle_id": "v", "true_x": 0.0, "true_y": 0.0},
            {"t": 1.0, "true_vehicle_id": "v", "true_x": 10.0, "true_y": 0.0},
            {"t": 30.0, "true_vehicle_id": "v", "true_x": 300.0, "true_y": 0.0}]
    segs = rb.segment_table(rb.build_tracks(emis))
    assert segs["n"] == 1                       # the 29 s gap is not a trustworthy difference
    assert segs["n_dropped_gap"] == 1


def test_teleport_is_detected_as_a_hard_failure(tmp_path):
    emis = _straight_line_emissions(n_veh=2, n_steps=40, v=12.0)
    emis.append({"t": 41.0, "true_vehicle_id": "veh_000", "true_x": 99999.0, "true_y": 0.0,
                 "claimed_x": 99999.0, "claimed_y": 0.0, "claimed_speed": 12.0, "pos_conf": 5.0,
                 "is_attacker": False, "is_faulty": False, "falsified": False})
    card = rb.scorecard(_make_dataset(str(tmp_path / "tele"), emissions=emis))
    m = _metric(card, "traffic.teleport_events")
    assert m["value"] == 1 and m["status"] == "fail" and m["severity"] == rb.HARD
    assert "traffic.teleport_events" in card["summary"]["hard_failures"]


def test_overlapping_vehicles_are_detected(tmp_path):
    emis = _straight_line_emissions(n_veh=3, n_steps=30, v=10.0, spacing=50.0)
    # park veh_002 exactly on top of veh_001 for 12 instants
    lookup = {(r["true_vehicle_id"], r["t"]): r for r in emis}
    for step in range(12):
        t = float(step)
        src, dst = lookup[("veh_001", t)], lookup[("veh_002", t)]
        dst["true_x"], dst["true_y"] = src["true_x"], src["true_y"]
    card = rb.scorecard(_make_dataset(str(tmp_path / "ovl"), emissions=emis))
    m = _metric(card, "traffic.overlap_events")
    assert m["value"] == 12 and m["status"] == "fail" and m["severity"] == rb.HARD


def test_clean_platoon_passes_the_hard_gates(tmp_path):
    card = rb.scorecard(_make_dataset(str(tmp_path / "clean"),
                                      emissions=_straight_line_emissions()))
    for mid in ("traffic.teleport_events", "traffic.overlap_events",
                "traffic.accel_within_hard_bound_frac"):
        assert _metric(card, mid)["status"] == "pass", mid
    assert card["summary"]["hard_failures"] == []


def test_teleports_across_dropped_sampling_gaps_are_counted(tmp_path):
    """One vehicle sampled every 50 s that jumps 500 km per gap.

    Every jump lands across a gap longer than MAX_FD_DT_S, so a teleport scan restricted to
    finite-difference SEGMENTS sees none of them and the hard gate passes a dataset that teleports
    on every step. Mean speed over the gap is the honest test and does not care how long the gap is.
    """
    emis = [{"t": 50.0 * k, "true_vehicle_id": "veh_000", "true_x": 500_000.0 * k, "true_y": 0.0,
             "claimed_x": 0.0, "claimed_y": 0.0, "claimed_speed": 1.0, "pos_conf": 5.0,
             "is_attacker": False, "is_faulty": False, "falsified": False} for k in range(60)]
    tracks = rb.build_tracks(emis)
    assert rb.segment_table(tracks)["n"] == 0            # the finite-difference guard drops them all
    n_tele, n_pairs = rb.teleport_events(tracks, 60.0)
    assert n_tele == 59 and n_pairs == 59
    card = rb.scorecard(_make_dataset(str(tmp_path / "jump"), emissions=emis,
                                      config={"emit_sample_prob": 0.02}))
    m = _metric(card, "traffic.teleport_events")
    assert m["value"] == 59 and m["status"] == "fail" and m["severity"] == rb.HARD
    assert "traffic.teleport_events" in card["summary"]["hard_failures"]


def test_teleport_metric_has_a_sample_floor(tmp_path):
    """Below MIN_SAMPLES consecutive-sample pairs the counter reads 'na', not 'pass': one clean pair
    is not evidence that a dataset does not teleport (accel needs 30, overlap needs 10 instants)."""
    emis = [{"t": float(t), "true_vehicle_id": "veh_000", "true_x": 10.0 * t, "true_y": 0.0}
            for t in range(4)]
    card = rb.scorecard(_make_dataset(str(tmp_path / "tiny"), emissions=emis))
    m = _metric(card, "traffic.teleport_events")
    assert m["status"] == "na" and str(rb.MIN_SAMPLES) in m["reason"]
    assert card["summary"]["hard_failures"] == []


def test_a_frozen_fleet_fails_the_liveness_hard_gate(tmp_path):
    """20 permanently parked vehicles satisfy every IMPOSSIBILITY gate (no teleport, no overlap, no
    acceleration outside the band). Only a liveness gate notices that nothing ever moved."""
    emis = []
    for step in range(120):
        for j in range(20):
            emis.append({"t": float(step), "true_vehicle_id": f"veh_{j:03d}",
                         "true_x": 100.0 * j, "true_y": 0.0, "claimed_x": 100.0 * j,
                         "claimed_y": 0.0, "claimed_speed": 0.0, "pos_conf": 5.0,
                         "is_attacker": False, "is_faulty": False, "falsified": False})
    card = rb.scorecard(_make_dataset(str(tmp_path / "parked"), emissions=emis))
    for mid in ("traffic.teleport_events", "traffic.overlap_events",
                "traffic.accel_within_hard_bound_frac"):
        assert _metric(card, mid)["status"] == "pass", mid
    mv = _metric(card, "traffic.moving_vehicle_frac")
    assert mv["value"] == 0.0 and mv["status"] == "fail" and mv["severity"] == rb.HARD
    assert card["summary"]["hard_failures"] == ["traffic.moving_vehicle_frac"]
    assert any("moving_vehicle_frac" in w for w in rb.hard_failures(card))


def test_a_moving_fleet_passes_the_liveness_gate(tmp_path):
    card = rb.scorecard(_make_dataset(str(tmp_path / "alive"),
                                      emissions=_straight_line_emissions()))
    mv = _metric(card, "traffic.moving_vehicle_frac")
    assert mv["value"] == 1.0 and mv["status"] == "pass" and mv["n"] == 6


def test_implausible_acceleration_fails_the_hard_bound(tmp_path):
    # stop-go every other second: speeds alternate 0 and 40 m/s -> +-40 m/s^2, far outside [-8, +4]
    emis = []
    for step in range(60):
        x = 40.0 * (step // 2)
        emis.append({"t": float(step), "true_vehicle_id": "veh_000", "true_x": x, "true_y": 0.0,
                     "claimed_x": x, "claimed_y": 0.0, "claimed_speed": 20.0, "pos_conf": 5.0,
                     "is_attacker": False, "is_faulty": False, "falsified": False})
    card = rb.scorecard(_make_dataset(str(tmp_path / "jerky"), emissions=emis))
    m = _metric(card, "traffic.accel_within_hard_bound_frac")
    assert m["value"] == 0.0 and m["status"] == "fail" and m["severity"] == rb.HARD
    assert m["reference"]["cite"]                      # the failure carries its citation
    assert "Punzo" in m["reference"]["source"]


# ==================================================================================================
# longitudinal / lateral decomposition -- the lane-change artefact and what must survive it
#
# Every trajectory below is written out by hand so the expected answer is arithmetic, not a golden
# number: a lane change moves a vehicle one lane sideways with its forward speed unchanged, a hard
# brake changes its forward speed with no sideways motion at all, and neither an irregular sample
# interval nor a corner is allowed to invent either one.
# ==================================================================================================
def _track_rows(vid, t, x, y):
    return [{"t": round(float(ti), 6), "true_vehicle_id": vid,
             "true_x": round(float(xi), 6), "true_y": round(float(yi), 6),
             "claimed_x": round(float(xi), 6), "claimed_y": round(float(yi), 6),
             "claimed_speed": 0.0, "pos_conf": 5.0, "is_attacker": False, "is_faulty": False,
             "falsified": False}
            for ti, xi, yi in zip(t, x, y)]


def _lane_change_track(vid="veh_000", n=60, dt=0.1, v=12.0, jump_at=30, lat=rb.LANE_WIDTH_M):
    """Constant 12 m/s east; at ONE sample the vehicle is a whole lane further north.

    This is SUMO with `--lanechange.duration 0`: the vehicle is re-assigned from one lane centreline
    to the next between two consecutive CAM samples, its longitudinal motion completely unchanged.
    """
    t = [i * dt for i in range(n)]
    x = [v * ti for ti in t]
    y = [0.0 if i < jump_at else lat for i in range(n)]
    return t, x, y


def test_a_lane_change_is_not_an_acceleration():
    """The reported failure: 3.2 m sideways in 0.1 s read as 32 m/s forward and ~200 m/s^2."""
    t, x, y = _lane_change_track()
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))

    # what the pre-fix estimator saw: |displacement|/dt differenced again
    raw_v = np.hypot(np.diff(x), np.diff(y)) / np.diff(t)
    raw_a = (raw_v[1:] - raw_v[:-1]) / ((np.diff(t)[:-1] + np.diff(t)[1:]) * 0.5)
    assert abs(raw_a).max() > 200.0, abs(raw_a).max()

    # what the decomposition sees: one lateral step, and a forward speed that never changed
    assert int(k["lateral"].sum()) == 1
    assert k["lateral"][29]                                     # the step that crosses the jump
    assert float(np.abs(k["d_lat"]).max()) == pytest.approx(rb.LANE_WIDTH_M, abs=1e-6)
    assert np.allclose(k["speed_long"], 12.0, atol=1e-6)        # including the lane-change step
    assert k["accel"].size == 54                                # 58 pairs - 2 boundary - 2 screened
    assert float(np.abs(k["accel"]).max()) < 1e-6
    assert k["n_excl"]["lateral"] == 2 and k["n_excl"]["boundary"] == 2


def test_lane_change_scorecard_reports_the_event_and_passes_the_accel_gate(tmp_path):
    emis = []
    for j in range(6):
        t, x, y = _lane_change_track(jump_at=20 + 4 * j)
        emis.extend(_track_rows(f"veh_{j:03d}", t, [xi + 500.0 * j for xi in x], y))
    card = rb.scorecard(_make_dataset(str(tmp_path / "lc"), emissions=emis,
                                      config={"dt": 0.1, "road_network": "grid"}))
    lat = _metric(card, "traffic.lateral_discontinuity_events")
    assert lat["details"]["events"] == 6                        # exactly one per vehicle
    assert lat["details"]["full_lane_steps"] == 6
    assert lat["severity"] == rb.SOFT                           # known-open defect, not a CI blocker
    assert lat["status"] == "fail" and lat["reference"]["cite"]
    # 6 vehicles x 59 steps x 12 m/s x 0.1 s = 424.8 m of path; the rate is per vehicle-km
    assert lat["value"] == pytest.approx(6.0 / (6 * 59 * 1.2 / 1000.0), rel=1e-3)
    acc = _metric(card, "traffic.accel_within_hard_bound_frac")
    assert acc["value"] == 1.0 and acc["status"] == "pass"
    assert acc["details"]["screened_out"]["lateral"] == 12
    assert card["summary"]["hard_failures"] == []


def test_a_genuine_hard_brake_is_still_caught(tmp_path):
    """-12 m/s^2 with no lateral motion at all must still break the hard gate."""
    dt, n_cruise, n_brake = 0.1, 25, 10
    speeds = ([25.0] * n_cruise + [25.0 - 1.2 * (i + 1) for i in range(n_brake)]
              + [13.0] * n_cruise)
    t, x = [0.0], [0.0]
    for v in speeds:
        t.append(round(t[-1] + dt, 6))
        x.append(x[-1] + v * dt)
    y = [0.0] * len(t)
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["lateral"].sum()) == 0                         # nothing sideways happened
    assert float(k["accel"].min()) == pytest.approx(-12.0, abs=1e-6)
    assert int((k["accel"] < -8.0).sum()) == n_brake            # every braking step is reported

    emis = []
    for j in range(6):
        emis.extend(_track_rows(f"veh_{j:03d}", t, [xi + 500.0 * j for xi in x], y))
    card = rb.scorecard(_make_dataset(str(tmp_path / "brake"), emissions=emis,
                                      config={"dt": 0.1}))
    m = _metric(card, "traffic.accel_within_hard_bound_frac")
    assert m["status"] == "fail" and m["severity"] == rb.HARD
    assert m["details"]["accel_min"] == pytest.approx(-12.0, abs=1e-3)
    assert "traffic.accel_within_hard_bound_frac" in card["summary"]["hard_failures"]


def test_a_hard_brake_during_a_lane_change_is_not_hidden_by_the_screen():
    """The screen removes the lane-change STEP, not the braking around it."""
    dt = 0.1
    speeds = [20.0] * 20 + [20.0 - 1.0 * (i + 1) for i in range(12)] + [8.0] * 20
    t, x = [0.0], [0.0]
    for v in speeds:
        t.append(round(t[-1] + dt, 6))
        x.append(x[-1] + v * dt)
    y = [0.0 if i < 26 else rb.LANE_WIDTH_M for i in range(len(t))]   # lane change mid-brake
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["lateral"].sum()) == 1
    assert float(k["accel"].min()) == pytest.approx(-10.0, abs=1e-6)
    assert int((k["accel"] < -8.0).sum()) == 10       # 12 braking pairs, less the 2 the jump touches
    assert k["n_excl"]["lateral"] == 2


def test_irregular_sampling_does_not_fabricate_acceleration():
    """dt cycling 0.1/0.2/0.3/0.4/1.0 s (ETSI CAM triggering) at a rigidly constant speed."""
    gaps = [0.1, 0.2, 0.3, 0.4, 1.0]
    t, v = [0.0], 15.0
    for i in range(60):
        t.append(round(t[-1] + gaps[i % len(gaps)], 6))
    x = [v * ti for ti in t]
    y = [0.0] * len(t)
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert k["accel"].size >= rb.MIN_SAMPLES
    assert np.allclose(k["speed_long"], v, atol=1e-6)
    assert float(np.abs(k["accel"]).max()) < 1e-6
    assert len(set(np.round(k["accel_pair_dt"], 3))) > 1        # the gaps really did vary


def test_irregular_sampling_on_a_curve_does_not_fabricate_acceleration():
    """Constant 10 m/s around a 50 m arc, sampled at 0.1-1.0 s: only the chord/arc deficit remains.

    A chord is shorter than the arc it subtends by a factor that depends on how much of the turn one
    sample covers, so mixing sample intervals on a curve necessarily jitters the measured speed. The
    point of this test is that the jitter stays a fraction of a m/s^2 -- unlike the ~10 m/s^2 a
    heading projection applied unconditionally would produce at the same corner.
    """
    r, v, gaps = 50.0, 10.0, [0.1, 0.2, 0.3, 0.4, 1.0]
    t = [0.0]
    for i in range(80):
        t.append(round(t[-1] + gaps[i % len(gaps)], 6))
    ang = [v * ti / r for ti in t]
    x = [r * math.cos(a) for a in ang]
    y = [r * math.sin(a) for a in ang]
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["lateral"].sum()) == 0                         # a curve is not a lane change
    assert float(np.abs(k["accel"]).max()) < 0.5, float(np.abs(k["accel"]).max())
    assert np.allclose(np.abs(k["speed_long"]), v, atol=0.05)


def test_a_single_step_corner_is_not_a_lane_change_and_does_not_fabricate_acceleration():
    """A 90-degree turn taken inside ONE 1 s sample -- the Python engine's grid junctions.

    The step's own heading is 45 degrees off the road either side of it, which is exactly what a
    lane-change teleport looks like locally; only the headings BEFORE and AFTER separate the two, and
    they differ by 90 degrees here. Projecting such a step onto a median heading that cannot turn
    that fast would shrink a 12 m step to ~8.5 m and read back as a 3.5 m/s^2 phantom brake plus an
    equal phantom acceleration; taking the along-path chord instead keeps that bounded.
    """
    dt, v = 1.0, 12.0
    t = [float(i) for i in range(41)]
    x, y = [], []
    for i in range(41):
        if i <= 20:
            x.append(-v * i); y.append(0.0)
        else:
            x.append(-v * 20 + 6.0); y.append(v * (i - 20) - 6.0)
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["lateral"].sum()) == 0, float(np.abs(k["d_lat"]).max())
    lo, hi = REFDATA["entries"]["kinematics.accel_hard_bound_mps2"]["range"]
    assert float(k["accel"].min()) >= lo and float(k["accel"].max()) <= hi
    assert float(np.abs(k["accel"]).max()) == pytest.approx(12.0 - math.hypot(6.0, 6.0), abs=1e-6)


def test_arrival_clamp_at_the_end_of_a_track_is_not_an_acceleration():
    """The Python engine's last sample is the vehicle's destination, not a second of driving.

    x runs 18.0 -> 1.2 -> 0.0 (clamped to the link end): the final 1 s step covers 1.2 m, so a naive
    second difference reads -15.6 m/s^2 and the hard gate fails on a vehicle that simply arrived.
    """
    t = [float(i) for i in range(12)]
    x = [16.8 * i for i in range(11)] + [16.8 * 10 + 1.2]
    y = [0.0] * 12
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert float(k["accel_all"].min()) == pytest.approx(-15.6, abs=1e-6)   # unscreened: the artefact
    assert float(np.abs(k["accel"]).max()) < 1e-6                          # screened: nothing at all
    assert k["n_excl"]["boundary"] == 2


def test_smoothed_heading_rejects_one_rogue_step_but_follows_a_turn():
    ang = np.zeros(11)
    ang[5] = math.radians(80.0)                       # one lane-change step among ten straight ones
    hs = rb.smoothed_heading(ang, np.ones(11, dtype=bool))
    assert abs(float(hs[5])) < 1e-9                   # the median does not move at all
    turn = np.radians(np.arange(11) * 9.0)            # a monotone 90-degree turn
    hs2 = rb.smoothed_heading(turn, np.ones(11, dtype=bool))
    assert np.allclose(hs2[3:-3], turn[3:-3], atol=1e-9)     # median of a monotone window = itself
    # wrap-safety: headings straddling +-pi must not average to zero
    wrap = np.array([math.pi - 0.05, -math.pi + 0.05, math.pi - 0.02, -math.pi + 0.01,
                     math.pi - 0.03])
    hs3 = rb.smoothed_heading(wrap, np.ones(5, dtype=bool))
    assert np.all(np.abs(np.abs(hs3) - math.pi) < 0.1)


def test_stationary_steps_carry_no_heading_and_no_lateral_event():
    """A parked vehicle's atan2 is undefined noise; it must not become a lateral discontinuity."""
    t = [float(i) for i in range(40)]
    x = [0.0] * 40
    y = [0.0] * 40
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["lateral"].sum()) == 0 and int(k["reversal"].sum()) == 0
    assert np.allclose(k["speed_long"], 0.0) and np.allclose(k["accel"], 0.0)


def test_teleport_steps_are_not_double_reported_as_accelerations():
    """A step the teleport gate already fails must not fail the acceleration gate as well."""
    t = [float(i) for i in range(40)]
    x = [10.0 * i for i in range(20)] + [10.0 * 19 + 5000.0 + 10.0 * i for i in range(20)]
    y = [0.0] * 40
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y), speed_bound_mps=60.0)
    assert int(k["teleport"].sum()) == 1 and k["n_excl"]["teleport"] == 2
    assert float(np.abs(k["accel"]).max()) < 1e-6
    # and with no bound supplied the teleport is NOT screened (the caller decides)
    k2 = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k2["teleport"].sum()) == 0 and float(np.abs(k2["accel"]).max()) > 1000.0


def test_a_backward_position_remap_is_counted_and_screened():
    t = [float(i) for i in range(40)]
    x = [10.0 * i for i in range(40)]
    x[20] = x[20] - 2.0                     # one sample lands 2 m behind where the vehicle was
    y = [0.0] * 40
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert int(k["reversal"].sum()) == 0    # the step INTO the remap is still forward (8 m)
    assert k["n_excl"]["lateral"] == 0
    x2 = list(x)
    x2[20] = x2[19] - 2.0                   # ... now it really does go backwards
    k2 = rb.track_kinematics(np.array(t), np.array(x2), np.array(y))
    assert int(k2["reversal"].sum()) == 1 and k2["n_excl"]["reversal"] == 2


# ==================================================================================================
# the lateral metric under SUB-SAMPLED emissions
#
# `traffic.lateral_discontinuity_events` is a claim about ONE sampling interval. Across a gap wider
# than the harness's own finite-difference ceiling there is no such claim to make: whatever sideways
# component the chord has is where the vehicle drove while nobody was looking. Before this was
# masked, the metric read 0.6799 ev/veh-km on datasets/mosaic_intas_urban_low_gate (emit_p 0.02) --
# 535 "events", 473 of them (88.4%) on pairs wider than 2 s, median gap 10.2 s, maximum "lateral
# offset" 891.7 m -- against 0.5872 on a full trace of the SAME scenario.
# ==================================================================================================
def _sparse_jog_track(n=40, gap=10.0, v=12.0, jog_at=20, jog_m=40.0):
    """A vehicle sampled every `gap` s that drives east and jogs one block north and back again.

    The sub-sampled artefact in miniature. Nothing moves sideways inside a sampling interval -- the
    vehicle merely drives round a block between two samples 10 s apart -- yet every clause of the
    lane-change test is satisfied: the chord has a 40 m component across the neighbouring chords'
    direction, its lateral 'speed' is 4 m/s, and the RAW headings either side of the jog agree
    exactly (both due east), so the cornering guard sees a perfectly stable heading.
    """
    t = [i * gap for i in range(n)]
    x = [v * ti for ti in t]
    y = [jog_m if i == jog_at else 0.0 for i in range(n)]
    return t, x, y


def test_a_gap_wider_than_the_ceiling_cannot_produce_a_lane_change_event():
    """Same trajectory, only the finite-difference ceiling differs -- 2 events become 0."""
    t, x, y = _sparse_jog_track()
    wide = rb.track_kinematics(np.array(t), np.array(x), np.array(y), max_dt=1e9)
    assert int(wide["lateral"].sum()) == 2                  # into the jog and back out of it
    assert float(np.abs(wide["d_lat"]).max()) == pytest.approx(40.0, abs=1e-6)
    assert float(wide["lateral_speed"].max()) == pytest.approx(4.0, abs=1e-6)

    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))   # the real MAX_FD_DT_S
    assert int(k["lateral"].sum()) == 0
    assert int(k["lateral_scan"].sum()) == 0                # nothing is even scannable at a 10 s gap
    assert int(k["reversal"].sum()) == 0


def test_lateral_metric_is_na_on_a_sub_sampled_trace(tmp_path):
    """The defect: at emit_sample_prob 0.02 the metric used to publish a rate from 10 s gaps."""
    emis = []
    for j in range(8):
        t, x, y = _sparse_jog_track(jog_at=15 + j)
        emis.extend(_track_rows(f"veh_{j:03d}", t, [xi + 5000.0 * j for xi in x], y))
    card = rb.scorecard(_make_dataset(str(tmp_path / "sparse"), emissions=emis,
                                      config={"emit_sample_prob": 0.02, "road_network": "grid"}))
    m = _metric(card, "traffic.lateral_discontinuity_events")
    assert m["status"] == "na" and m["value"] is None
    assert "finite-difference ceiling" in m["reason"] and str(rb.MIN_SAMPLES) in m["reason"]
    assert m["details"]["events"] == 0 and m["details"]["sample_pairs"] == 0
    assert m["details"]["sample_pairs_total"] == 8 * 39
    assert m["details"]["max_lateral_step_m"] is None      # the 40 m diagnostics are gone too
    assert m["details"]["full_lane_steps"] == 0
    # an "na" is not a failure: the metric must drop out of the soft-failure list, not fail loudly
    assert "traffic.lateral_discontinuity_events" not in card["summary"]["soft_failures"]

    # ... and the SAME jog sampled densely enough to resolve it is scored again
    dense = []
    for j in range(8):
        t, x, y = _sparse_jog_track(n=120, gap=0.5, v=12.0, jog_at=60, jog_m=4.0)
        dense.extend(_track_rows(f"veh_{j:03d}", t, [xi + 5000.0 * j for xi in x], y))
    card2 = rb.scorecard(_make_dataset(str(tmp_path / "dense"), emissions=dense,
                                       config={"dt": 0.5, "road_network": "grid"}))
    m2 = _metric(card2, "traffic.lateral_discontinuity_events")
    assert m2["status"] == "fail" and m2["details"]["events"] == 16


def test_lateral_rate_is_normalised_over_the_scannable_subset_only(tmp_path):
    """One real lane change per vehicle, plus a 30 s hole that must not enter EITHER side of the rate.

    Each track is 59 steps at 0.1 s with a lane change at step 29; sample 50 onward is displaced
    30 s / 360 m east / 100 m north, so step 49 is a 30.1 s gap whose 100 m sideways component would
    otherwise be counted as a second lane change AND whose 361 m of travel would otherwise pad the
    vehicle-km the rate is divided by (making the rate look ~7x better than it is).
    """
    emis = []
    for j in range(6):
        t, x, y = _lane_change_track(n=60, dt=0.1, v=12.0, jump_at=30)
        t = [ti + (30.0 if i >= 50 else 0.0) for i, ti in enumerate(t)]
        x = [xi + 500.0 * j + (360.0 if i >= 50 else 0.0) for i, xi in enumerate(x)]
        y = [yi + (100.0 if i >= 50 else 0.0) for i, yi in enumerate(y)]
        emis.extend(_track_rows(f"veh_{j:03d}", t, x, y))
    # the gap step really would have been counted without the mask
    t0, x0, y0 = ([r["t"] for r in emis[:60]], [r["true_x"] for r in emis[:60]],
                  [r["true_y"] for r in emis[:60]])
    wide = rb.track_kinematics(np.array(t0), np.array(x0), np.array(y0), max_dt=1e9)
    assert int(wide["lateral"].sum()) == 2                 # the lane change AND the 30 s hole

    card = rb.scorecard(_make_dataset(str(tmp_path / "hole"), emissions=emis,
                                      config={"dt": 0.1, "road_network": "grid"}))
    d = _metric(card, "traffic.lateral_discontinuity_events")["details"]
    assert d["events"] == 6                                 # only the six real lane changes
    assert d["sample_pairs_total"] == 6 * 59
    assert d["sample_pairs_inside_gap_ceiling"] == 6 * 58    # one 30.1 s step per vehicle
    # ... and the +-3-step window the smoothed heading is read from must be inside the ceiling too
    assert d["sample_pairs"] == 6 * (59 - (2 * rb.HEADING_WINDOW_PAIRS + 1))
    assert d["sample_pairs_dropped_sampling_gap"] == 6 * 7
    assert d["vehicle_km"] == pytest.approx(6 * 52 * 1.2 / 1000.0, abs=1e-3)      # published to 3 dp
    assert d["vehicle_km_all_pairs"] == pytest.approx(6 * (58 * 1.2 + 361.2) / 1000.0, abs=1e-3)
    assert _metric(card, "traffic.lateral_discontinuity_events")["value"] == pytest.approx(
        6.0 / (6 * 52 * 1.2 / 1000.0), abs=1e-4)
    assert d["max_lateral_step_m"] == pytest.approx(rb.LANE_WIDTH_M, abs=1e-6)   # not 100.0


def test_the_full_trace_lateral_rate_is_unchanged_by_the_gap_mask():
    """A full trace has no dropped gaps, so the mask must be a no-op on every scored baseline."""
    t, x, y = _lane_change_track()
    k = rb.track_kinematics(np.array(t), np.array(x), np.array(y))
    assert bool(k["lateral_scan"].all()) and int(k["lateral"].sum()) == 1
    kin = rb.kinematics(rb.build_tracks(_track_rows("veh_000", t, x, y)))
    assert kin["path_m"] == pytest.approx(kin["path_m_all"])


def test_lateral_metric_reference_entry_is_soft_and_cites_the_sumo_default():
    ref = REFDATA["entries"]["kinematics.lateral_discontinuity_per_vehicle_km_max"]
    assert ref["max"] == 0.0 and ref["unit"] == "events/vehicle-km"
    assert "lanechange.duration" in ref["cite"]
    assert "2-4" in REFDATA["entries"]["kinematics.lane_change_duration_s"]["source"]
    lat_ref = REFDATA["entries"]["kinematics.lateral_speed_max_mps"]
    assert lat_ref["max"] == rb.LATERAL_SPEED_MAX_MPS       # the code and the reference agree
    assert rb.LATERAL_JUMP_M == rb.LANE_WIDTH_M / 2.0


# ==================================================================================================
# ADR 0002 -- true_speed / true_heading in the ground-truth record
#
# The harness does not own the emission schema, so it must (a) use the fields when they are there,
# (b) keep every pre-ADR dataset working unchanged, and (c) never trust a field it cannot corroborate
# against the geometry it can already measure. A silently mis-read heading convention would rotate
# the whole lateral decomposition by 90 degrees and still produce plausible-looking output.
# ==================================================================================================
def _gt_rows(vid, t, x, y, speed, heading_rad, convention="deg_ccw_from_east"):
    """`_track_rows` plus true_speed / true_heading, the heading written in `convention`."""
    if convention == "deg_ccw_from_east":
        h = [math.degrees(a) % 360.0 for a in heading_rad]
    elif convention == "deg_cw_from_north":
        h = [(90.0 - math.degrees(a)) % 360.0 for a in heading_rad]
    elif convention == "rad_ccw_from_east":
        h = list(heading_rad)
    elif convention == "rad_cw_from_north":
        h = [(math.pi / 2.0) - a for a in heading_rad]
    else:                                                        # pragma: no cover - test guard
        raise AssertionError(convention)
    rows = _track_rows(vid, t, x, y)
    for r, s, hh in zip(rows, speed, h):
        r["true_speed"] = round(float(s), 6)
        r["true_heading"] = round(float(hh), 6)
    return rows


def _diagonal_track(n=60, dt=0.5, v=14.0, ang_deg=30.0):
    """A straight track at 30 degrees: a bearing no two heading conventions agree on."""
    a = math.radians(ang_deg)
    t = [round(i * dt, 6) for i in range(n)]
    return t, [v * ti * math.cos(a) for ti in t], [v * ti * math.sin(a) for ti in t], a


@pytest.mark.parametrize("conv", sorted(rb.HEADING_CONVENTIONS))
def test_ground_truth_heading_convention_is_detected_and_reported(conv):
    """A 30-degree bearing separates all four candidates; the fit is validated, not assumed."""
    t, x, y, a = _diagonal_track()
    rows = _gt_rows("veh_000", t, x, y, [14.0] * len(t), [a] * len(t), conv)
    gt = rb.ground_truth_kinematics(rb.build_tracks(rows))
    assert gt["heading"]["used"] is True and gt["heading"]["convention"] == conv
    assert gt["heading"]["residual_deg"] == pytest.approx(0.0, abs=1e-6)
    assert gt["heading"]["source"].endswith(f"({conv})")
    # every other candidate is measurably worse -- the pick is not a coin toss
    others = [v for k, v in gt["heading"]["convention_residuals_deg"].items() if k != conv]
    assert min(others) > rb.GT_HEADING_MAX_RESIDUAL_DEG
    assert gt["speed"]["used"] is True and gt["speed"]["residual_vs_chord_speed_mps"] < 1e-6


def test_a_heading_field_that_fits_no_convention_is_rejected():
    """Garbage in the field must fall back to the chord bearing, not rotate the decomposition."""
    t, x, y, _a = _diagonal_track()
    rows = _track_rows("veh_000", t, x, y)
    for i, r in enumerate(rows):
        r["true_heading"] = float((i * 37) % 360)          # unrelated to where the vehicle points
        r["true_speed"] = 14.0
    gt = rb.ground_truth_kinematics(rb.build_tracks(rows))
    assert gt["heading"]["used"] is False
    assert "no heading convention fits" in gt["heading"]["reason"]
    assert gt["speed"]["used"] is True                     # the speed field is judged on its own


def test_a_ground_truth_speed_in_the_wrong_unit_is_rejected():
    t, x, y, a = _diagonal_track()
    rows = _gt_rows("veh_000", t, x, y, [14.0 * 3.6] * len(t), [a] * len(t))   # km/h, not m/s
    gt = rb.ground_truth_kinematics(rb.build_tracks(rows))
    assert gt["speed"]["used"] is False
    assert "wrong unit or frame" in gt["speed"]["reason"]
    assert gt["speed"]["residual_vs_chord_speed_mps"] == pytest.approx(14.0 * 2.6, abs=1e-3)
    assert gt["heading"]["used"] is True                   # ... and the heading still is


def test_a_partially_populated_ground_truth_field_is_not_interpolated():
    t, x, y, a = _diagonal_track()
    rows = _gt_rows("veh_000", t, x, y, [14.0] * len(t), [a] * len(t))
    rows[7].pop("true_speed")                              # one sample short of a complete track
    tracks = rb.build_tracks(rows)
    assert "v" not in tracks["veh_000"] and "h_raw" in tracks["veh_000"]
    gt = rb.ground_truth_kinematics(tracks)
    assert gt["speed"]["used"] is False and gt["speed"]["present_tracks"] == 0
    assert gt["heading"]["used"] is True and gt["heading"]["present_tracks"] == 1


def test_a_half_migrated_dataset_does_not_mix_two_estimators():
    """One vehicle out of four still lacks the field: the panel must not average two estimators."""
    t, x, y, a = _diagonal_track()
    rows = []
    for j in range(4):
        yy = [yi + 400.0 * j for yi in y]
        if j == 3:
            rows.extend(_track_rows(f"veh_{j:03d}", t, x, yy))          # the un-migrated vehicle
        else:
            rows.extend(_gt_rows(f"veh_{j:03d}", t, x, yy, [14.0] * len(t), [a] * len(t)))
    gt = rb.ground_truth_kinematics(rb.build_tracks(rows))
    assert gt["speed"]["used"] is False and gt["heading"]["used"] is False
    assert gt["speed"]["present_tracks"] == 3 and gt["speed"]["total_tracks"] == 4
    assert "mix two estimators" in gt["speed"]["reason"]
    assert "mix two estimators" in gt["heading"]["reason"]


def test_ground_truth_speed_removes_the_arrival_clamp_artefact_entirely():
    """The ADR's own worked example. Same trace as test_arrival_clamp_...: the final 1 s step covers
    1.2 m because the vehicle's last sample is its destination, which a position double-difference
    reads as -15.6 m/s^2 and only a boundary screen can suppress. The simulator knew all along that
    the speed never changed, so with true_speed there is no artefact to screen."""
    n = 40                                                 # long enough to validate the field on
    t = [float(i) for i in range(n)]
    x = [16.8 * i for i in range(n - 1)] + [16.8 * (n - 2) + 1.2]
    y = [0.0] * n
    rows = _gt_rows("veh_000", t, x, y, [16.8] * n, [0.0] * n)
    tracks = rb.build_tracks(rows)
    gt = rb.ground_truth_kinematics(tracks)
    assert gt["speed"]["used"] is True and gt["speed"]["validated_steps"] == n - 1

    fallback = rb.kinematics(tracks)                       # what a pre-ADR dataset gets
    assert float(fallback["accel_all"].min()) == pytest.approx(-15.6, abs=1e-6)
    assert fallback["n_excl"]["boundary"] == 2

    k = rb.kinematics(tracks, gt=gt)
    assert float(np.abs(k["accel_all"]).max()) == 0.0      # not screened -- never generated
    assert k["accel"].size == n - 1 and k["n_excl"] == {"boundary": 0, "lateral": 0, "reversal": 0,
                                                        "teleport": 0}
    assert np.allclose(k["accel_pair_dt"], 1.0)            # one STEP, not a midpoint separation


def test_ground_truth_speed_still_reports_a_genuine_hard_brake():
    """The gate must not become vacuous: a real -12 m/s^2 in the speed field is still a failure."""
    dt = 0.1
    speeds = [25.0] * 25 + [25.0 - 1.2 * (i + 1) for i in range(10)] + [13.0] * 25
    t, x = [0.0], [0.0]
    for v in speeds:
        t.append(round(t[-1] + dt, 6))
        x.append(x[-1] + v * dt)
    y = [0.0] * len(t)
    rows = _gt_rows("veh_000", t, x, y, [speeds[0]] + speeds, [0.0] * len(t))
    k = rb.kinematics(rb.build_tracks(rows), gt=rb.ground_truth_kinematics(rb.build_tracks(rows)))
    assert float(k["accel"].min()) == pytest.approx(-12.0, abs=1e-6)
    assert int((k["accel"] < -8.0).sum()) == 10


def test_ground_truth_heading_finds_the_lane_change_with_no_smoothing_window():
    """With a measured heading the lane-change test reads the vehicle's own turn, and the +-3-step
    scan window disappears -- so a step next to a dropped gap is still scannable."""
    t, x, y = _lane_change_track()
    rows = _gt_rows("veh_000", t, x, y, [12.0] * len(t), [0.0] * len(t))
    tracks = rb.build_tracks(rows)
    gt = rb.ground_truth_kinematics(tracks)
    k = rb.kinematics(tracks, gt=gt)
    assert int(k["lateral"].sum()) == 1 and k["lateral"][29]
    assert bool(k["lateral_scan"].all())
    assert float(np.abs(k["d_lat"]).max()) == pytest.approx(rb.LANE_WIDTH_M, abs=1e-6)
    # a corner is separated by the heading itself turning, not by neighbouring chords
    ang = [0.0] * 30 + [math.pi / 2.0] * 30
    xx = [12.0 * 0.1 * i for i in range(30)] + [12.0 * 0.1 * 29] * 30
    yy = [0.0] * 30 + [12.0 * 0.1 * (i + 1) for i in range(30)]
    corner = _gt_rows("veh_001", t, xx, yy, [12.0] * 60, ang)
    tr2 = rb.build_tracks(corner)
    k2 = rb.kinematics(tr2, gt=rb.ground_truth_kinematics(tr2))
    assert int(k2["lateral"].sum()) == 0


def test_scorecard_records_which_kinematic_path_produced_each_metric(tmp_path):
    t, x, y, a = _diagonal_track(n=120, dt=0.5)
    emis = []
    for j in range(6):
        emis.extend(_gt_rows(f"veh_{j:03d}", t, [xi + 900.0 * j for xi in x], y,
                             [14.0] * len(t), [a] * len(t), "deg_cw_from_north"))
    card = rb.scorecard(_make_dataset(str(tmp_path / "gt"), emissions=emis,
                                      config={"dt": 0.5, "road_network": "grid"}))
    ks = card["kinematics_source"]
    assert ks["speed"]["used"] is True and ks["heading"]["convention"] == "deg_cw_from_north"
    for mid in ("traffic.speed_p50_mps", "traffic.speed_max_mps",
                "traffic.accel_within_hard_bound_frac", "traffic.accel_within_comfort_frac"):
        assert _metric(card, mid)["details"]["kinematics_source"] == "ground truth true_speed", mid
    lat = _metric(card, "traffic.lateral_discontinuity_events")
    assert lat["details"]["kinematics_source"].startswith("ground truth true_heading")
    assert "true_heading (ground truth" in lat["details"]["method"]
    acc = _metric(card, "traffic.accel_within_hard_bound_frac")
    assert "FIRST difference of the ground-truth true_speed" in acc["details"]["method"]
    assert acc["value"] == 1.0 and acc["status"] == "pass"
    # the speed metric now samples EVERY emission, not every usable step
    assert _metric(card, "traffic.speed_p50_mps")["n"] == 6 * len(t)
    assert _metric(card, "traffic.speed_max_mps")["value"] == pytest.approx(14.0, abs=1e-6)
    assert card["kinematics_source"]["heading"]["residual_deg"] == pytest.approx(0.0, abs=1e-6)


def test_a_pre_adr_dataset_reports_the_fallback_path_and_is_unchanged(tmp_path):
    """Older datasets carry neither field: the harness must say so and behave exactly as before."""
    card = rb.scorecard(_make_dataset(str(tmp_path / "old"),
                                      emissions=_straight_line_emissions()))
    ks = card["kinematics_source"]
    assert ks["speed"]["used"] is False and ks["heading"]["used"] is False
    assert ks["speed"]["source"] == "position finite difference"
    assert "no track carries true_speed" in ks["speed"]["reason"]
    assert "no track carries true_heading" in ks["heading"]["reason"]
    assert _metric(card, "traffic.speed_p50_mps")["details"]["source_field"].startswith(
        "longitudinal component")
    assert _metric(card, "traffic.speed_p50_mps")["value"] == pytest.approx(12.0, abs=1e-6)


def test_ground_truth_kinematics_never_leaks_a_per_entity_value(tmp_path):
    """The fields are ORACLE: the scorecard may name them, never carry one vehicle's value."""
    t, x, y, a = _diagonal_track(n=120, dt=0.5)
    emis = []
    for j in range(6):
        emis.extend(_gt_rows(f"veh_{j:03d}", t, [xi + 900.0 * j for xi in x], y,
                             [14.0] * len(t), [a] * len(t)))
    card = rb.scorecard(_make_dataset(str(tmp_path / "gtleak"), emissions=emis))
    assert find_forbidden_keys(card) == []                 # a VALUE named "true_speed" is not a key
    blob = json.dumps(card, default=str)
    assert "veh_000" not in blob and "veh_005" not in blob


# ==================================================================================================
# time headway
# ==================================================================================================
def test_time_headway_of_a_rigid_platoon_is_spacing_over_speed():
    """20 m spacing at 10 m/s must read back as exactly 2.0 s, with no octant-centre distortion."""
    emis = _straight_line_emissions(n_veh=4, n_steps=20, v=10.0, spacing=20.0)
    hw = rb._time_headways(rb.segment_table(rb.build_tracks(emis)))
    assert hw.size > 0
    assert np.allclose(hw, 2.0, atol=1e-6), (hw.min(), hw.max())


def test_headways_are_not_fabricated_between_adjacent_lanes():
    """Two lanes 3.2 m apart, 40 m in-lane spacing at 20 m/s -> a true 2.0 s headway everywhere.

    Projecting onto the along-road axis without a lateral test pairs the interleaved neighbours in
    the OTHER lane, halving every spacing (and, when they are abreast, reporting ~0 s).
    """
    emis = []
    for step in range(25):
        t = float(step)
        for lane in (0, 1):
            for j in range(4):
                x = -j * 40.0 + 20.0 * t + lane * 20.0      # lane 1 sits between lane 0's vehicles
                emis.append({"t": t, "true_vehicle_id": f"veh_{lane}{j}",
                             "true_x": round(x, 3), "true_y": lane * 3.2})
    hw = rb._time_headways(rb.segment_table(rb.build_tracks(emis)))
    assert hw.size == 6 * 24                               # 3 followers per lane per instant
    assert np.allclose(hw, 2.0, atol=1e-6), (hw.min(), hw.max())


def test_long_headways_are_not_censored_by_a_grouping_cell():
    """4 vehicles 90 m apart at 30 m/s -> exactly 3.0 s. Every leader/follower pair straddles a 50 m
    cell boundary, so a cell-grouped implementation observes ZERO headways here and its distribution
    is truncated on one side only (short headways survive, long ones cannot)."""
    emis = []
    for step in range(25):
        for j in range(4):
            emis.append({"t": float(step), "true_vehicle_id": f"veh_{j}",
                         "true_x": -j * 90.0 + 30.0 * float(step), "true_y": 0.0})
    hw = rb._time_headways(rb.segment_table(rb.build_tracks(emis)))
    assert hw.size == 3 * 24
    assert np.allclose(hw, 3.0, atol=1e-6)
    # and the full HEADWAY_MAX_S range is reachable: 1 km apart at 20 m/s is a 50 s headway
    far = [{"t": float(s), "true_vehicle_id": f"veh_{j}",
            "true_x": -j * 1000.0 + 20.0 * float(s), "true_y": 0.0}
           for s in range(5) for j in range(2)]
    hw_far = rb._time_headways(rb.segment_table(rb.build_tracks(far)))
    assert hw_far.size > 0 and np.allclose(hw_far, 50.0, atol=1e-6)


def test_headway_floor_fraction_flags_impossible_following(tmp_path):
    # 2 m spacing at 10 m/s -> 0.2 s headway, below the 0.5 s physical floor
    emis = _straight_line_emissions(n_veh=4, n_steps=25, v=10.0, spacing=2.0)
    card = rb.scorecard(_make_dataset(str(tmp_path / "tight"), emissions=emis))
    m = _metric(card, "traffic.headway_below_floor_frac")
    assert m["value"] == 1.0 and m["status"] == "fail"
    assert m["details"]["floor_s"] == 0.5


def test_headway_metrics_are_na_when_emissions_are_sampled(tmp_path):
    card = rb.scorecard(_make_dataset(str(tmp_path / "thin"),
                                      emissions=_straight_line_emissions(),
                                      config={"emit_sample_prob": 0.02}))
    m = _metric(card, "traffic.headway_p50_s")
    assert m["status"] == "na" and "emit_sample_prob" in m["reason"]


# ==================================================================================================
# fundamental diagram
# ==================================================================================================
def test_fd_binning_recovers_a_hand_computed_edie_flow():
    """5 vehicles each crossing one 100 m cell in 10 s within a 60 s window.

    Edie over a section of length L observed for T seconds: q = sum(d)/(L*T), k = sum(t)/(L*T).
    sum(d) = 5*100 m and sum(t) = 5*10 s, so q = 500/(100*60) veh/s = 300 veh/h and
    k = 50/(100*60) veh/m = 8.333 veh/km, with space-mean speed sum(d)/sum(t) = 10 m/s.
    """
    emis = []
    for j in range(5):
        for step in range(11):                    # x = 0,10,...,100 -> mid-points all inside cell 0
            emis.append({"t": float(step), "true_vehicle_id": f"veh_{j:03d}",
                         "true_x": 10.0 * step, "true_y": 0.5 * j})
    segs = rb.segment_table(rb.build_tracks(emis))
    fd = rb._fundamental_diagram(segs, n_lanes=1, cell_m=100.0, window_s=60.0)
    assert fd["cells"] == 1
    assert float(fd["q"][0]) == pytest.approx(300.0)
    assert float(fd["k"][0]) == pytest.approx(1000.0 * 50.0 / (100.0 * 60.0))
    assert float(fd["v"][0]) == pytest.approx(10.0)


def test_fd_flow_is_per_lane():
    emis = [{"t": float(s), "true_vehicle_id": f"veh_{j}", "true_x": 10.0 * s, "true_y": 0.0}
            for j in range(5) for s in range(11)]
    segs = rb.segment_table(rb.build_tracks(emis))
    one = rb._fundamental_diagram(segs, 1, 100.0, 60.0)
    two = rb._fundamental_diagram(segs, 2, 100.0, 60.0)
    assert float(two["q"][0]) == pytest.approx(float(one["q"][0]) / 2.0)


def test_fd_cells_below_the_sample_floor_are_dropped():
    emis = [{"t": float(s), "true_vehicle_id": "veh_0", "true_x": 10.0 * s, "true_y": 0.0}
            for s in range(3)]                            # 2 segments < FD_MIN_CELL_SAMPLES
    fd = rb._fundamental_diagram(rb.segment_table(rb.build_tracks(emis)), 1, 100.0, 60.0)
    assert fd["cells"] == 0


def test_fd_separates_parallel_roads_and_opposing_directions():
    """A square cell is not a road section: two carriageways inside one cell used to be summed into
    one per-lane flow, which is why every MOSAIC dataset (no `n_lanes` in the manifest -> divisor 1)
    failed the capacity gate for reasons unrelated to its traffic."""
    emis = []
    for road, y in enumerate((10.0, 50.0)):                # two one-way roads, 40 m apart, same cell
        for j in range(5):
            for step in range(11):
                emis.append({"t": float(step), "true_vehicle_id": f"veh_{road}{j}",
                             "true_x": 10.0 * step, "true_y": y})
    fd = rb._fundamental_diagram(rb.segment_table(rb.build_tracks(emis)), 1, 100.0, 60.0)
    assert fd["cells"] == 1 and float(fd["lanes_per_cell"][0]) == 2.0
    assert float(fd["q"][0]) == pytest.approx(300.0)       # not 600.0 summed onto a single lane

    emis2 = []
    for j in range(5):
        for step in range(11):
            emis2.append({"t": float(step), "true_vehicle_id": f"e{j}",
                          "true_x": 10.0 * step, "true_y": 0.0})
            emis2.append({"t": float(step), "true_vehicle_id": f"w{j}",
                          "true_x": 100.0 - 10.0 * step, "true_y": 5.0})
    fd2 = rb._fundamental_diagram(rb.segment_table(rb.build_tracks(emis2)), 1, 100.0, 60.0)
    assert fd2["cells"] == 2                               # opposing headings -> separate sections
    assert np.allclose(fd2["q"], 300.0)


def test_fd_lane_estimate_is_floored_by_the_manifest_lane_count():
    emis = [{"t": float(s), "true_vehicle_id": f"veh_{j}", "true_x": 10.0 * s, "true_y": 0.0}
            for j in range(5) for s in range(11)]
    segs = rb.segment_table(rb.build_tracks(emis))
    assert float(rb._fundamental_diagram(segs, 1, 100.0, 60.0)["lanes_per_cell"][0]) == 1.0
    assert float(rb._fundamental_diagram(segs, 3, 100.0, 60.0)["lanes_per_cell"][0]) == 3.0


def test_backward_wave_speed_recovers_a_planted_congested_branch():
    """A congested branch with slope -18 km/h must read back as a +18 km/h backward wave."""
    k = np.array([10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 110.0, 120.0])
    q = np.where(k <= 60.0, 40.0 * k, 2400.0 - 18.0 * (k - 60.0))
    wave, n_cong = rb._wave_speed_kmh({"q": q, "k": k})
    assert n_cong == 6
    assert wave == pytest.approx(18.0, abs=1e-6)


# ==================================================================================================
# PDR-vs-distance reconstruction
# ==================================================================================================
def _two_vehicle_snapshot(bucket, dist):
    return {"bucket": bucket, "vids": ["a", "b"],
            "x": np.array([0.0, dist]), "y": np.array([0.0, 0.0]),
            "t": np.array([float(bucket), float(bucket)])}


def test_pdr_curve_reconstructs_a_planted_reception_profile():
    """Opportunities are uniform per 50 m bin; links follow a known 1.0/0.5/0.2/0.0 profile."""
    snaps = []
    b = 0
    for d in (25.0, 75.0, 125.0, 175.0):
        for _ in range(100):
            snaps.append(_two_vehicle_snapshot(b, d))
            b += 1
    heard = np.array([25.0] * 100 + [75.0] * 50 + [125.0] * 20)
    curve = rb._pdr_curve(heard, snaps, bin_m=50.0, max_m=200.0)
    assert list(curve["den"]) == [100, 100, 100, 100]
    assert list(curve["num"]) == [100, 50, 20, 0]
    assert np.allclose(curve["normalized"], [1.0, 0.5, 0.2, 0.0])
    # 100 m sits on a bin edge: the anchor averages its two neighbouring bins (70 links / 200 pairs)
    val, n_at = rb._curve_at(curve, 100.0)
    assert val == pytest.approx(0.35) and n_at == 70
    assert rb._curve_at(curve, 25.0)[0] == pytest.approx(1.0)


def test_gray_zone_width_from_the_planted_curve():
    snaps = []
    b = 0
    for d in (25.0, 75.0, 125.0, 175.0):
        for _ in range(100):
            snaps.append(_two_vehicle_snapshot(b, d))
            b += 1
    curve = rb._pdr_curve(np.array([25.0] * 100 + [75.0] * 50 + [125.0] * 20), snaps, 50.0, 200.0)
    # bin centres 25/75/125/175 with y = 1.0/0.5/0.2/0.0
    assert rb._crossing(curve, 0.90) == pytest.approx(35.0)    # 25 + 50*(1.0-0.9)/(1.0-0.5)
    assert rb._crossing(curve, 0.20) == pytest.approx(125.0)   # first bin at/below 0.2
    assert rb._crossing(curve, 0.99) is not None


def test_step_function_radio_has_no_gray_zone():
    """A hard unit-disc cutoff never crosses 0.90 -> 0.20 gradually; the metric must say so."""
    snaps = []
    b = 0
    for d in (25.0, 75.0, 125.0, 175.0):
        for _ in range(100):
            snaps.append(_two_vehicle_snapshot(b, d))
            b += 1
    curve = rb._pdr_curve(np.array([25.0] * 100 + [75.0] * 100), snaps, 50.0, 200.0)
    assert np.allclose(curve["normalized"], [1.0, 1.0, 0.0, 0.0])
    d90, d20 = rb._crossing(curve, 0.90), rb._crossing(curve, 0.20)
    # both crossings land inside the SAME bin step, so the gray zone is far below the 100 m gate
    assert d90 == pytest.approx(80.0) and d20 == pytest.approx(115.0)
    assert (d20 - d90) < 100.0
    ref = REFDATA["entries"]["v2x_awareness.pdr_gray_zone_width_min_m"]
    assert (d20 - d90) < ref["min"]           # a unit-disc radio fails this gate by construction


def test_a_single_noisy_bin_cannot_manufacture_a_gray_zone():
    """A hard cutoff at 350 m with ONE Poisson dip at 175 m.

    Taking the first downward crossing of the raw curve puts d90 at the dip and d20 only at the real
    cutoff, reporting a ~200 m 'stochastic gray zone' for a step-function radio that has none --
    exactly the failure refdata/v2x_awareness.json's pdr_gray_zone_width_min_m note says must not
    happen. The crossing is read off the non-increasing majorant instead.
    """
    snaps, b = [], 0
    for d in (25.0, 75.0, 125.0, 175.0, 225.0, 275.0, 325.0, 375.0):
        for _ in range(100):
            snaps.append(_two_vehicle_snapshot(b, d))
            b += 1
    heard = np.array([25.0] * 100 + [75.0] * 100 + [125.0] * 100 + [175.0] * 67
                     + [225.0] * 100 + [275.0] * 100 + [325.0] * 100)
    curve = rb._pdr_curve(heard, snaps, 50.0, 400.0)
    assert curve["normalized"][3] == pytest.approx(0.67)        # the dip is still REPORTED
    d90, d20 = rb._crossing(curve, 0.90), rb._crossing(curve, 0.20)
    assert d90 == pytest.approx(330.0) and d20 == pytest.approx(365.0)
    ref = REFDATA["entries"]["v2x_awareness.pdr_gray_zone_width_min_m"]
    assert (d20 - d90) < ref["min"]        # a hard cutoff still fails the gate, as it must


def test_thin_distance_bins_are_masked_out_of_the_curve():
    """A bin estimated from a handful of co-presence pairs is not evidence about PDR."""
    snaps, b = [], 0
    for d, n in ((25.0, 100), (75.0, 5), (125.0, 100)):        # the middle bin is under-sampled
        for _ in range(n):
            snaps.append(_two_vehicle_snapshot(b, d))
            b += 1
    curve = rb._pdr_curve(np.array([25.0] * 100 + [125.0] * 100), snaps, 50.0, 200.0)
    assert list(curve["usable"]) == [True, False, True, False]
    assert math.isnan(curve["normalized"][1])
    assert rb._curve_at(curve, 75.0) == (None, 0)              # no usable bin overlaps the anchor


def test_honest_links_exclude_distance_triggered_reports():
    reports = [{"report_id": "r1", "reason_codes": ["positionJump"]},
               {"report_id": "r2", "reason_codes": ["acceptanceRangeThreshold"]},
               {"report_id": "r3", "reason_codes": ["positionJump"]}]
    labels = {"r1": {"report_correctness": "false_positive"},
              "r2": {"report_correctness": "false_positive"},
              "r3": {"report_correctness": "correct"}}     # subject is a real attacker -> excluded
    got = rb._honest_links(reports, labels)
    assert [r["report_id"] for r in got] == ["r1"]


def test_art_reconstruction_matches_each_engine_convention():
    links = [{"detnorm_acceptanceRangeThreshold": 0.0},
             {"detnorm_acceptanceRangeThreshold": 0.5},
             {"detnorm_acceptanceRangeThreshold": 1.0}]
    py = {"art_max_m": 150.0, "radio_range_m": 250.0, "art_censored": True}
    d, censored = rb._art_link_distances(links, py)
    assert censored == 1 and np.allclose(d, [0.5 * 150 + 250, 1.0 * 150 + 250])
    mos = {"art_max_m": 1000.0, "radio_range_m": 0.0, "art_censored": False}
    d2, cens2 = rb._art_link_distances(links, mos)
    assert cens2 == 1 and np.allclose(d2, [500.0, 1000.0])


def test_comm_panel_reconstructs_links_from_ground_truth_positions(tmp_path):
    """A synthetic run where the reception radius is 150 m: awareness must collapse past it."""
    emis, reports, labels = [], [], []
    k = 0
    for step in range(60):
        t = float(step)
        for j in range(8):
            emis.append({"t": t, "true_vehicle_id": f"veh_{j:03d}", "true_x": 100.0 * j,
                         "true_y": 0.0, "claimed_x": 100.0 * j, "claimed_y": 0.0,
                         "claimed_speed": 0.0, "pos_conf": 5.0, "is_attacker": False,
                         "is_faulty": False, "falsified": False})
        for a in range(8):
            for b_ in range(8):
                if a == b_ or abs(a - b_) * 100.0 > 150.0:
                    continue                     # only neighbours within 150 m ever "hear"
                rid = f"rpt_{k:05d}"
                k += 1
                reports.append({"report_id": rid, "detection_time": t,
                                "reason_codes": ["positionJump"],
                                "detnorm_acceptanceRangeThreshold": 0.0})
                labels.append({"report_id": rid, "reporter_true_id": f"veh_{a:03d}",
                               "subject_true_id": f"veh_{b_:03d}",
                               "report_correctness": "false_positive"})
    card = rb.scorecard(_make_dataset(str(tmp_path / "comm"), emissions=emis, reports=reports,
                                      labels=labels))
    links = _metric(card, "comm.honest_links")
    assert links["details"]["reconstruction_method"] == "gt_link_distance"
    assert _metric(card, "comm.awareness_ratio_100m")["value"] == pytest.approx(1.0)
    assert _metric(card, "comm.awareness_ratio_300m")["value"] == pytest.approx(0.0)
    eff = _metric(card, "comm.effective_range_m")["value"]
    assert 100.0 <= eff <= 200.0, eff


def test_cam_gap_metric_and_its_sampling_guard(tmp_path):
    card = rb.scorecard(_make_dataset(str(tmp_path / "gap"), emissions=_straight_line_emissions()))
    m = _metric(card, "comm.cam_inter_packet_gap_p50_s")
    assert m["value"] == pytest.approx(1.0) and m["status"] == "pass"
    assert m["reference"]["ref_id"] == "etsi_cam_dcc.cam_interval_s"
    thin = rb.scorecard(_make_dataset(str(tmp_path / "gap2"), emissions=_straight_line_emissions(),
                                      config={"emit_sample_prob": 0.05}))
    assert _metric(thin, "comm.cam_inter_packet_gap_p50_s")["status"] == "na"


# ==================================================================================================
# engine probing + graceful degradation
# ==================================================================================================
def test_probe_identifies_both_producers(tmp_path):
    py = _make_dataset(str(tmp_path / "py"), emissions=_straight_line_emissions())
    p = rb.probe_dataset(py)
    assert p["engine"] == "python_mock" and p["art_censored"] is True
    assert p["art_max_m"] == 150.0 and p["radio_range_m"] == 250.0
    assert p["regime"] == "highway" and p["full_trace"] is True

    mo = _make_dataset(str(tmp_path / "mo"), emissions=_straight_line_emissions(),
                       generator="scms_sim_ref (MOSAIC layer, full-entity back-end v4)",
                       config={"reception": "MOSAIC AdHoc ITS-G5 CCH via SNS (range/delay)"})
    with open(os.path.join(mo, "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    man["config"].pop("emit_sample_prob")
    man["config"].pop("road_network")
    man["config"].pop("art_max_m")                   # a MOSAIC manifest that pre-dates the field
    with open(os.path.join(mo, "manifest.json"), "w", encoding="utf-8", newline="\n") as fh:
        json.dump(man, fh)
    q = rb.probe_dataset(mo)
    assert q["engine"] == "mosaic" and q["art_censored"] is False
    assert q["art_max_m"] == rb.MOSAIC_ART_MAX_M      # ScmsBeaconApp SCMS_ART_MAX_M default
    assert q["emit_sample_prob"] == pytest.approx(0.02)   # SCMS_EMIT_SAMPLE default
    assert q["regime"] is None and q["full_trace"] is False


def test_probe_honours_the_mosaic_manifest_art_max_m(tmp_path):
    """ScmsBackend.java:869 writes the RESOLVED SCMS_ART_MAX_M into config; ignoring it and using
    the 1000 m default rescales every reconstructed link distance by art_max/1000."""
    mo = _make_dataset(str(tmp_path / "mo_art"), emissions=_straight_line_emissions(),
                       generator="scms_sim_ref (MOSAIC layer, full-entity back-end v4)",
                       config={"art_max_m": 250.0, "radio_range_m": 709.4})
    p = rb.probe_dataset(mo)
    assert p["engine"] == "mosaic" and p["art_max_m"] == 250.0 and p["art_censored"] is False
    d, _cens = rb._art_link_distances([{"detnorm_acceptanceRangeThreshold": 0.5}], p)
    assert d[0] == pytest.approx(125.0)      # not 500.0, which the hardcoded default would give


def test_urban_regime_selects_the_urban_reference_band(tmp_path):
    root = _make_dataset(str(tmp_path / "urban"), emissions=_straight_line_emissions(v=9.0),
                         config={"road_network": "grid"})
    card = rb.scorecard(root)
    m = _metric(card, "traffic.speed_p50_mps")
    assert m["reference"]["ref_id"] == "traffic_regimes.urban.speed_p50_mps"
    assert m["status"] == "pass"
    # the same 9 m/s corridor is BELOW the highway p50 band -> the regime really is load-bearing
    forced = rb.scorecard(root, regime="highway")
    assert _metric(forced, "traffic.speed_p50_mps")["status"] == "fail"


def test_missing_signals_degrade_to_na_with_a_reason(tmp_path):
    root = str(tmp_path / "bare")
    os.makedirs(os.path.join(root, "ground_truth"))
    os.makedirs(os.path.join(root, "ma"))
    with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump({"generator": "something else"}, fh)
    card = rb.scorecard(root)
    assert card["probe"]["engine"] == "unknown"
    assert card["summary"]["fail"] == 0                       # nothing is graded as a failure
    for m in card["panels"]["traffic"] + card["panels"]["comm"]:
        assert m["status"] == "na" and m.get("reason")
    # the panel keeps its full shape so a consumer can index it by metric id unconditionally
    full = rb.scorecard(_make_dataset(str(tmp_path / "full"), emissions=_straight_line_emissions()))
    assert ([m["id"] for m in card["panels"]["traffic"]]
            == [m["id"] for m in full["panels"]["traffic"]])
    assert ([m["id"] for m in card["panels"]["comm"]]
            == [m["id"] for m in full["panels"]["comm"]])
    assert _metric(card, "traffic.trace_segments")["reason"].endswith("missing or empty")


def test_scorecard_on_a_missing_directory_raises(tmp_path):
    with pytest.raises(FileNotFoundError):
        rb.scorecard(str(tmp_path / "nope"))


def test_cli_errors_on_a_missing_directory(tmp_path):
    with pytest.raises(SystemExit) as e:
        rb.main([str(tmp_path / "nope")])
    assert e.value.code == 2                                  # argparse usage error


def test_cli_writes_json_and_gates_on_hard_failures(tmp_path, capsys):
    emis = _straight_line_emissions(n_veh=2, n_steps=30, v=10.0)
    emis.append({"t": 31.0, "true_vehicle_id": "veh_000", "true_x": 99999.0, "true_y": 0.0,
                 "claimed_x": 0.0, "claimed_y": 0.0, "claimed_speed": 1.0, "pos_conf": 5.0,
                 "is_attacker": False, "is_faulty": False, "falsified": False})
    root = _make_dataset(str(tmp_path / "cli"), emissions=emis)
    out = str(tmp_path / "card.json")                          # OUTSIDE the dataset dir
    assert rb.main([root, "--json", out]) == 0                 # measure-only by default
    assert rb.main([root, "--json", out, "--fail-on-hard"]) == 1
    with open(out, encoding="utf-8") as fh:
        card = json.load(fh)
    assert "traffic.teleport_events" in card["summary"]["hard_failures"]
    assert not os.path.exists(os.path.join(root, "card.json"))
    assert rb.main([root, "--markdown"]) == 0
    assert "Traffic panel" in capsys.readouterr().out


# ==================================================================================================
# reference data: every number carries a citation
# ==================================================================================================
def test_refdata_loads_and_every_entry_carries_a_source():
    rd = rb.load_refdata()
    assert rd["sets"], "no reference sets found"
    assert {"kinematics", "traffic_regimes", "fundamental_diagram", "v2x_awareness",
            "etsi_cam_dcc", "pathloss_37885", "geh"} <= set(rd["sets"])
    for ref_id, entry in rd["entries"].items():
        assert entry.get("source"), f"{ref_id} has no source citation"
        assert len(entry["source"]) > 40, f"{ref_id} source is too thin to audit: {entry['source']}"
        assert entry.get("cite"), f"{ref_id} has no short citation label"
        assert entry.get("confidence") in {"anchored", "coarse", "unavailable"}, ref_id
        assert any(k in entry for k in ("range", "min", "max", "value", "points", "available")), ref_id


_RESEARCH_REPORT = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "docs", "realism", "investigation", "research-realism-benchmarks.json")

# Every ``benchmarks[N]`` pointer a refdata file is allowed to make, and a distinctive phrase that
# MUST appear in the benchmark it names. An in-range but WRONG index is a silently broken audit
# trail, which a "source is longer than 40 characters" check cannot catch.
_BENCHMARK_POINTERS = {
    ("etsi_cam_dcc.json", 13): "CBR",
    ("fundamental_diagram.json", 6): "lane capacity",
    ("geh_criteria.json", 0): "GEH statistic",
    ("geh_criteria.json", 1): "Link flow tolerances",
    ("geh_criteria.json", 2): "Travel/journey times",
    ("geh_criteria.json", 3): "FHWA 2019 update",
    ("kinematics_plausibility.json", 5): "acceleration magnitudes",
    ("traffic_regimes.json", 6): "lane capacity",
    ("traffic_regimes.json", 7): "Kolmogorov-Smirnov",
    ("v2x_awareness.json", 10): "PDR-vs-distance",
    ("v2x_awareness.json", 11): "C-V2X Mode 4",
    ("v2x_awareness.json", 12): "Latency CDF",
    ("v2x_awareness.json", 14): "awareness ratio",
}


def test_refdata_benchmark_pointers_resolve_to_the_right_benchmark():
    if not os.path.exists(_RESEARCH_REPORT):
        pytest.skip("docs/realism/investigation/research-realism-benchmarks.json not present")
    with open(_RESEARCH_REPORT, encoding="utf-8") as fh:
        benchmarks = json.load(fh)["benchmarks"]
    found = set()
    for name in sorted(os.listdir(rb.REFDATA_DIR)):
        if not name.endswith(".json"):
            continue
        with open(os.path.join(rb.REFDATA_DIR, name), encoding="utf-8") as fh:
            txt = fh.read()
        for m in re.finditer(r"benchmarks\[([0-9,\s]+)\]", txt):
            for tok in m.group(1).split(","):
                i = int(tok.strip())
                assert 0 <= i < len(benchmarks), f"{name}: benchmarks[{i}] out of range"
                key = (name, i)
                assert key in _BENCHMARK_POINTERS, f"{name} cites an unvetted benchmarks[{i}]"
                assert _BENCHMARK_POINTERS[key].lower() in benchmarks[i].lower(), \
                    f"{name}: benchmarks[{i}] is the wrong entry -> {benchmarks[i][:90]}"
                found.add(key)
    assert found == set(_BENCHMARK_POINTERS), f"missing pointers: {set(_BENCHMARK_POINTERS) - found}"


def test_refdata_coarse_entries_explain_their_derivation():
    """A number we derived rather than transcribed must say so, so nobody mistakes it for measured."""
    for ref_id, entry in rb.load_refdata()["entries"].items():
        if entry.get("confidence") == "coarse":
            assert entry.get("derivation") or entry.get("note"), ref_id


def test_refdata_dir_can_be_overridden_and_missing_dir_is_harmless(tmp_path):
    empty = rb.load_refdata(str(tmp_path / "does_not_exist"))
    assert empty["entries"] == {} and empty["sets"] == {}
    card = rb.scorecard(_make_dataset(str(tmp_path / "d"), emissions=_straight_line_emissions()),
                        refdata=str(tmp_path / "does_not_exist"))
    assert card["summary"]["fail"] == 0                        # no references -> nothing is graded
    assert all(m["status"] == "na" for m in card["panels"]["traffic"])


# ==================================================================================================
# harness contracts: deterministic, read-only, leakage-free
# ==================================================================================================
def test_scorecard_is_deterministic(tmp_path):
    root = _make_dataset(str(tmp_path / "det"), emissions=_straight_line_emissions())
    a = json.dumps(rb.scorecard(root), sort_keys=True, default=str)
    b = json.dumps(rb.scorecard(root), sort_keys=True, default=str)
    assert a == b


def test_scorecard_is_read_only(tmp_path):
    root = _make_dataset(str(tmp_path / "ro"), emissions=_straight_line_emissions())
    def snap():
        out = {}
        for dp, _dirs, files in os.walk(root):
            for f in files:
                p = os.path.join(dp, f)
                st = os.stat(p)
                out[os.path.relpath(p, root)] = (st.st_size, st.st_mtime_ns)
        return out
    before = snap()
    rb.scorecard(root)
    rb.render_lines(rb.scorecard(root))
    assert snap() == before


def test_scorecard_carries_no_forbidden_feature_key(tmp_path):
    """Schema firewall: the harness reads ORACLE data but must never re-export an ORACLE key."""
    root = _make_dataset(str(tmp_path / "leak"), emissions=_straight_line_emissions(),
                         reports=[{"report_id": "r1", "detection_time": 1.0,
                                   "reason_codes": ["positionJump"],
                                   "detnorm_acceptanceRangeThreshold": 0.1}],
                         labels=[{"report_id": "r1", "reporter_true_id": "veh_000",
                                  "subject_true_id": "veh_001",
                                  "report_correctness": "false_positive"}])
    card = rb.scorecard(root)
    assert find_forbidden_keys(card) == []
    assert find_forbidden_keys(rb.load_refdata()) == []
    # and no per-entity value survives into the scorecard (aggregates only)
    blob = json.dumps(card, default=str)
    assert "veh_000" not in blob and "veh_001" not in blob


def test_render_lines_and_warning_lines_quote_the_citation(tmp_path):
    emis = _straight_line_emissions(n_veh=4, n_steps=25, v=10.0, spacing=2.0)   # 0.2 s headways
    card = rb.scorecard(_make_dataset(str(tmp_path / "cite"), emissions=emis))
    lines = rb.render_lines(card, include_na=True)
    assert any("Traffic panel" in ln for ln in lines)
    assert any("Cassidy" in ln or "FHWA" in ln or "Punzo" in ln for ln in lines)
    warns = rb.warning_lines(card)
    assert any("headway_below_floor_frac" in w for w in warns)
    assert all(("REALISM HARD FAIL" in w) or ("out of reference range" in w) for w in warns)


# ==================================================================================================
# wiring: datasheet + corpus_report
# ==================================================================================================
def test_datasheet_embeds_the_measured_realism_panel(tmp_path):
    root = _make_dataset(str(tmp_path / "sheet"), emissions=_straight_line_emissions(v=30.0))
    md = ds.build(root)
    assert "### Realism benchmark (measured, vs pinned reference summaries)" in md
    assert "Traffic panel" in md
    # a measured speed metric REPLACES the wide sanity band (no two competing speed verdicts)
    assert "Benign speed p95 (m/s)" not in md
    assert "Benign true speed p95" in md


def test_datasheet_keeps_the_wide_band_when_realism_cannot_score_speed(tmp_path):
    root = _make_dataset(str(tmp_path / "sheet2"), emissions=_straight_line_emissions(n_steps=3))
    md = ds.build(root)
    assert "Benign speed p95 (m/s)" in md                    # too few segments -> fall back


def test_corpus_report_default_behaviour_is_unchanged(tmp_path):
    corpus = str(tmp_path / "corpus")
    os.makedirs(os.path.join(corpus, "ml"))
    rep = cr.build_report(corpus)
    assert rep["realism"] is None and rep["realism_warnings"] == []
    assert "## Realism" not in cr.render_markdown(rep)
    assert cr.realism_hard_failures(rep) == []


def test_corpus_report_realism_flag_gates_on_hard_failures(tmp_path, capsys):
    corpus = str(tmp_path / "corpus2")
    os.makedirs(os.path.join(corpus, "ml"))
    emis = _straight_line_emissions(n_veh=2, n_steps=30, v=10.0)
    emis.append({"t": 31.0, "true_vehicle_id": "veh_000", "true_x": 99999.0, "true_y": 0.0,
                 "claimed_x": 0.0, "claimed_y": 0.0, "claimed_speed": 1.0, "pos_conf": 5.0,
                 "is_attacker": False, "is_faulty": False, "falsified": False})
    _make_dataset(os.path.join(corpus, "dom_000"), emissions=emis)

    rep = cr.build_report(corpus, realism=True)
    assert rep["realism"]["available"] and rep["realism"]["n_datasets"] == 1
    assert "traffic.teleport_events" in rep["realism"]["hard_failures"]
    assert any("REALISM HARD FAIL" in w for w in rep["realism_warnings"])
    assert not any("REALISM" in w for w in rep["warnings"])   # kept out of the balance warnings
    md = cr.render_markdown(rep)
    assert "## Realism" in md and "## Realism warnings" in md

    # the flag is what turns a realism failure into a non-zero exit; without it, nothing changes
    corpus_ok = str(tmp_path / "corpus3")
    os.makedirs(os.path.join(corpus_ok, "ml"))
    _make_dataset(os.path.join(corpus_ok, "dom_000"), emissions=_straight_line_emissions())
    assert cr.main(["--corpus", corpus_ok]) == 1              # balance warnings (empty ml/) as before
    capsys.readouterr()


def test_corpus_report_realism_without_any_dataset_is_reported_not_crashed(tmp_path):
    corpus = str(tmp_path / "corpus4")
    os.makedirs(os.path.join(corpus, "ml"))
    rep = cr.build_report(corpus, realism=True)
    assert rep["realism"]["available"] is False
    assert "no dataset directory" in rep["realism"]["reason"]
    assert "## Realism" in cr.render_markdown(rep)


def test_corpus_report_finds_dataset_dirs(tmp_path):
    corpus = str(tmp_path / "corpus5")
    os.makedirs(corpus)
    _make_dataset(os.path.join(corpus, "b_dom"), emissions=_straight_line_emissions(n_steps=3))
    _make_dataset(os.path.join(corpus, "a_dom"), emissions=_straight_line_emissions(n_steps=3))
    os.makedirs(os.path.join(corpus, "ml"))
    found = [os.path.basename(d) for d in cr.find_dataset_dirs(corpus)]
    assert found == ["a_dom", "b_dom"]                        # sorted, ml/ ignored


# ==================================================================================================
# tools/sumo_realism.py -- GEH over induction loops + the acceleration gate
# ==================================================================================================
_TOOLS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "tools")
if _TOOLS not in sys.path:
    sys.path.insert(0, _TOOLS)
import sumo_realism as sumo_rb  # noqa: E402

_INTAS_E1 = os.path.join(
    os.path.dirname(_TOOLS), "third_party", "veremi-nextgen", "Generator", "simulation", "mosaic",
    "scenarios", "InTAS_urban_2_4_test", "sumo", "InTAS_E1.add.xml")


def _write_e1_add(path, dets):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<additional>\n')
        for did, lane, name in dets:
            fh.write(f'  <e1Detector id="{did}" lane="{lane}" pos="1.0" freq="900.00" '
                     f'name="{name}" file="out.xml" friendlyPos="1"/>\n')
        fh.write("</additional>\n")
    return path


def _write_e1_out(path, rows, begin=0.0, end=3600.0):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<detector>\n')
        for did, n in rows:
            fh.write(f'  <interval begin="{begin:.2f}" end="{end:.2f}" id="{did}" '
                     f'nVehContrib="{n}" flow="{n * 3600.0 / max(end - begin, 1e-9):.2f}" '
                     f'occupancy="1.0" speed="12.0" harmonicMeanSpeed="11.5" length="4.5" '
                     f'nVehEntered="{n}"/>\n')
        fh.write("</detector>\n")
    return path


def _write_fcd(path, frames):
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<fcd-export>\n')
        for t, vehs in frames:
            fh.write(f'  <timestep time="{t:.2f}">\n')
            for vid, speed in vehs:
                fh.write(f'    <vehicle id="{vid}" x="0.0" y="0.0" angle="90.0" '
                         f'type="car" speed="{speed:.3f}" pos="0.0" lane="e_0"/>\n')
            fh.write("  </timestep>\n")
        fh.write("</fcd-export>\n")
    return path


def test_sumo_realism_parses_the_real_intas_loop_layout():
    if not os.path.exists(_INTAS_E1):
        pytest.skip("third_party/veremi-nextgen submodule not checked out")
    m = sumo_rb.parse_e1_additional(_INTAS_E1)
    assert len(m) == 196                                   # the real InTAS E1 loop count
    assert len({d["station"] for d in m.values()}) == 27    # grouped by the shared @name
    assert m["1010_1"]["station"] == "1010" and m["1010_1"]["lane"] == "172519650_0"
    assert m["1010_1"]["edge"] == "172519650"              # lane id minus its index suffix


def test_sumo_realism_empty_detector_output_degrades(tmp_path):
    p = str(tmp_path / "empty.xml")
    with open(p, "w", encoding="utf-8") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<detector>\n</detector>\n')
    parsed = sumo_rb.parse_e1_output(p)
    assert parsed["n_intervals"] == 0 and parsed["counts"] == {} and parsed["duration_s"] is None


def test_sumo_realism_geh_end_to_end(tmp_path, capsys):
    add = _write_e1_add(str(tmp_path / "E1.add.xml"),
                        [("d1", "eA_0", "S1"), ("d2", "eA_1", "S1"), ("d3", "eB_0", "S2")])
    # station S1 = 1000 veh/h (two lanes), S2 = 500 veh/h over one hour
    out = _write_e1_out(str(tmp_path / "out.xml"), [("d1", 600), ("d2", 400), ("d3", 500)])
    ref = str(tmp_path / "ref.json")
    with open(ref, "w", encoding="utf-8") as fh:
        json.dump({"unit": "veh_per_h", "stations": {"S1": 1000.0, "S2": 500.0}}, fh)

    rep = sumo_rb.build(argparse.Namespace(
        det_out=out, det_add=add, ref_counts=ref, ref_det_out=None, fcd=None,
        group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
        refdata=None, json_out=None, no_fail=False))
    g = rep["sections"]["geh"]
    assert g["n_stations_compared"] == 2
    assert g["geh"]["pass_fraction"] == 1.0 and g["geh"]["total_geh"] == 0.0
    assert all(gate["status"] == "pass" for gate in g["gates"])
    assert rep["summary"]["fail"] == 0
    assert rep["detector_layout"]["n_detectors"] == 3 and rep["detector_layout"]["n_stations"] == 2

    # a 40% shortfall on S1 must break the GEH gate
    bad = _write_e1_out(str(tmp_path / "bad.xml"), [("d1", 300), ("d2", 300), ("d3", 500)])
    rep2 = sumo_rb.build(argparse.Namespace(
        det_out=bad, det_add=add, ref_counts=ref, ref_det_out=None, fcd=None,
        group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
        refdata=None, json_out=None, no_fail=False))
    g2 = rep2["sections"]["geh"]
    assert g2["geh"]["stations"][0]["geh"] == pytest.approx(rb.geh(600, 1000), abs=1e-3)
    assert g2["geh"]["pass_fraction"] == 0.5
    assert "geh.link_pass_fraction" in rep2["summary"]["failures"]
    assert sumo_rb.main(["--det-out", bad, "--det-add", add, "--ref-counts", ref]) == 1
    assert sumo_rb.main(["--det-out", out, "--det-add", add, "--ref-counts", ref]) == 0
    assert "GEH" in capsys.readouterr().out


def test_sumo_realism_baseline_mode_and_grouping(tmp_path):
    add = _write_e1_add(str(tmp_path / "E1.add.xml"),
                        [("d1", "eA_0", "S1"), ("d2", "eA_1", "S1"), ("d3", "eB_0", "S2")])
    a = _write_e1_out(str(tmp_path / "a.xml"), [("d1", 600), ("d2", 400), ("d3", 500)])
    b = _write_e1_out(str(tmp_path / "b.xml"), [("d1", 600), ("d2", 400), ("d3", 500)])
    rep = sumo_rb.build(argparse.Namespace(
        det_out=a, det_add=add, ref_counts=None, ref_det_out=b, fcd=None, group_by="edge",
        begin=None, end=None, duration_s=None, ref_duration_s=None, refdata=None,
        json_out=None, no_fail=False))
    g = rep["sections"]["geh"]
    assert sorted(s["station"] for s in g["geh"]["stations"]) == ["eA", "eB"]   # grouped by edge
    assert g["geh"]["total_geh"] == 0.0 and rep["summary"]["fail"] == 0


def test_sumo_realism_without_reference_counts_reports_na(tmp_path):
    out = _write_e1_out(str(tmp_path / "out.xml"), [("d1", 600)])
    rep = sumo_rb.build(argparse.Namespace(
        det_out=out, det_add=None, ref_counts=None, ref_det_out=None, fcd=None,
        group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
        refdata=None, json_out=None, no_fail=False))
    g = rep["sections"]["geh"]
    assert g["status"] == "na" and "no reference counts" in g["reason"]
    assert g["modelled_flows_veh_h"]["d1"] == pytest.approx(600.0)
    assert rep["summary"]["n_gates"] == 0


def test_sumo_realism_reference_csv_counts(tmp_path):
    p = str(tmp_path / "ref.csv")
    with open(p, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("station,count,duration_s\nS1,500,1800\nS2,250,1800\n")
    counts, desc = sumo_rb.load_reference_counts(p)
    assert counts == {"S1": 1000.0, "S2": 500.0} and "count" in desc


def test_sumo_realism_accel_gate(tmp_path):
    good = _write_fcd(str(tmp_path / "good.xml"),
                      [(float(t), [("v0", 10.0 + t), ("v1", 20.0)]) for t in range(40)])
    rep = sumo_rb.build(argparse.Namespace(
        det_out=None, det_add=None, ref_counts=None, ref_det_out=None, fcd=good,
        group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
        refdata=None, json_out=None, no_fail=False))
    a = rep["sections"]["accel"]
    assert a["n_vehicles"] == 2 and a["accel_max"] == pytest.approx(1.0)
    assert all(g["status"] == "pass" for g in a["gates"])

    bad = _write_fcd(str(tmp_path / "bad.xml"),
                     [(float(t), [("v0", 0.0 if t % 2 else 30.0)]) for t in range(60)])
    rep2 = sumo_rb.build(argparse.Namespace(
        det_out=None, det_add=None, ref_counts=None, ref_det_out=None, fcd=bad,
        group_by="station", begin=None, end=None, duration_s=None, ref_duration_s=None,
        refdata=None, json_out=None, no_fail=False))
    a2 = rep2["sections"]["accel"]
    assert a2["accel_max"] == pytest.approx(30.0)          # +-30 m/s^2, outside [-8, +4]
    assert "accel.within_hard_bound_frac" in rep2["summary"]["failures"]
    assert rep2["sections"]["accel"]["gates"][0]["reference"]["cite"]


def test_sumo_realism_uses_native_acceleration_attribute_when_present(tmp_path):
    p = str(tmp_path / "acc.xml")
    with open(p, "w", encoding="utf-8", newline="\n") as fh:
        fh.write('<?xml version="1.0" encoding="UTF-8"?>\n<fcd-export>\n')
        for t in range(40):
            fh.write(f'  <timestep time="{t:.2f}">\n'
                     f'    <vehicle id="v0" x="0" y="0" speed="10.0" acceleration="-2.5"/>\n'
                     f'  </timestep>\n')
        fh.write("</fcd-export>\n")
    tr = sumo_rb.parse_trace_accelerations(p)
    assert tr["native_accel_samples"] == 40 and np.allclose(tr["accel"], -2.5)


def test_sumo_realism_cli_requires_an_input(tmp_path):
    with pytest.raises(SystemExit) as e:
        sumo_rb.main([])
    assert e.value.code == 2
    with pytest.raises(SystemExit) as e2:
        sumo_rb.main(["--det-out", str(tmp_path / "nope.xml")])
    assert e2.value.code == 2


def test_sumo_realism_shares_the_refdata_citations():
    rd = sumo_rb.load_refdata()
    assert rd["entries"]["geh.geh_link_max"]["max"] == 5.0
    assert "FHWA" in rd["entries"]["geh.geh_link_max"]["source"]
    assert rd["entries"]["geh.geh_link_min_pass_fraction"]["min"] == 0.85


# ==================================================================================================
# helper
# ==================================================================================================
def _metric(card: dict, mid: str) -> dict:
    for m in card["panels"]["traffic"] + card["panels"]["comm"]:
        if m["id"] == mid:
            return m
    raise AssertionError(f"metric {mid} not in the scorecard: "
                         f"{[m['id'] for m in card['panels']['traffic'] + card['panels']['comm']]}")
