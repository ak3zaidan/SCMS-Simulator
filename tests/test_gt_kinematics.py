"""ADR 0002 -- true_speed / true_heading in the ground-truth emission record.

The simulator knows both quantities exactly at emission time; before this change the realism harness
reconstructed them by differencing `true_x`/`true_y`, which reads a 3.2 m lane change as a ~276 m/s^2
acceleration. These tests pin the four properties the ADR promises:

  1. COMPLETENESS -- every emitted sample of every vehicle carries both fields (the harness rejects a
     partially populated field wholesale, by design).
  2. TRUTH -- the value written is the engine's own state, not a reconstruction: on an all-honest run
     the claimed speed (which the engine copies verbatim from the true speed) equals `true_speed`
     exactly, and `true_heading` points along the direction the vehicle actually moved.
  3. CONVENTION -- the Python engine's heading is degrees COUNTER-CLOCKWISE FROM EAST, and the
     manifest says so. (The MOSAIC/Java engine uses degrees clockwise from North; the two datasets
     disagree, which is exactly why the manifest has to declare it.)
  4. FIREWALL -- both names are ORACLE-only. They must never appear in an MA-visible row, and the
     leakage linter must reject them as feature keys.
"""
import json
import math

import pytest

from scms_sim_ref.datagen.leakage_linter import (LeakageViolation, assert_ma_visible,
                                                 find_forbidden_keys)
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.schemas.records import is_forbidden_feature_key


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _run(out_dir, **over):
    kw = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
              grid_w=5, grid_h=5, attacker_pct=0.25, emit_sample_prob=1.0, out_dir=str(out_dir))
    kw.update(over)
    return run_pipeline(PipelineConfig(**kw))


@pytest.fixture(scope="module")
def mixed(tmp_path_factory):
    """Full-trace run with attackers and faulty vehicles present (the realistic mix)."""
    out = tmp_path_factory.mktemp("gtkin_mixed")
    res = _run(out)
    return res, _jsonl(f"{res.out_dir}/ground_truth/gt_emissions_sample.jsonl")


@pytest.fixture(scope="module")
def honest(tmp_path_factory):
    """Full-trace run with NO attackers and NO faulty vehicles: every claim is the honest value, so
    claimed_speed is the engine's true speed and can be compared to true_speed exactly."""
    out = tmp_path_factory.mktemp("gtkin_honest")
    res = _run(out, attacker_pct=0.0, faulty_pct=0.0)
    return res, _jsonl(f"{res.out_dir}/ground_truth/gt_emissions_sample.jsonl")


# --------------------------------------------------------------------------- #
# 1. completeness
# --------------------------------------------------------------------------- #
def test_every_emission_carries_true_speed_and_heading(mixed):
    _res, rows = mixed
    assert len(rows) > 500, f"need a real trace to test (got {len(rows)})"
    missing = [r["emit_id"] for r in rows if "true_speed" not in r or "true_heading" not in r]
    assert missing == [], f"{len(missing)} rows lack the ADR 0002 fields, e.g. {missing[:3]}"
    for r in rows:
        assert isinstance(r["true_speed"], float) and isinstance(r["true_heading"], float)
        assert r["true_speed"] >= 0.0, r
        assert 0.0 <= r["true_heading"] < 360.0, r


def test_fields_are_present_for_every_vehicle_not_just_some(mixed):
    """A partially populated field is rejected by the harness wholesale, so check per-vehicle."""
    _res, rows = mixed
    by_veh = {}
    for r in rows:
        has = "true_speed" in r and "true_heading" in r
        by_veh.setdefault(r["true_vehicle_id"], [0, 0])[0 if has else 1] += 1
    assert len(by_veh) > 20
    assert all(bad == 0 for _good, bad in by_veh.values())


# --------------------------------------------------------------------------- #
# 2. truth -- the written value is the engine's own state
# --------------------------------------------------------------------------- #
def test_true_speed_is_the_engine_speed_not_a_reconstruction(honest):
    """With nobody attacking or faulty the engine claims its true speed verbatim, so the ORACLE field
    and the MA-visible claim must agree to the last recorded digit. A differenced reconstruction
    could not do this: it would disagree on every turn and every lane offset."""
    _res, rows = honest
    assert rows and all(not r["falsified"] for r in rows)
    bad = [(r["emit_id"], r["true_speed"], r["claimed_speed"])
           for r in rows if r["true_speed"] != r["claimed_speed"]]
    assert bad == [], f"{len(bad)}/{len(rows)} rows disagree, e.g. {bad[:3]}"


def test_true_speed_beats_the_differenced_reconstruction(honest):
    """The chord speed |dx,dy|/dt is what the harness used to be forced to use. Over straight
    segments it agrees with true_speed; the point of the field is the tail where it does not."""
    _res, rows = honest
    tracks = {}
    for r in rows:
        tracks.setdefault(r["true_vehicle_id"], []).append(r)
    errs = []
    for track in tracks.values():
        track.sort(key=lambda r: r["t"])
        for a, b in zip(track, track[1:]):
            dt = b["t"] - a["t"]
            if not 0.0 < dt <= 1.001:
                continue
            chord = math.hypot(b["true_x"] - a["true_x"], b["true_y"] - a["true_y"]) / dt
            errs.append(abs(chord - b["true_speed"]))
    assert len(errs) > 300
    errs.sort()
    assert errs[len(errs) // 2] < 1.0, f"median |chord - true_speed| = {errs[len(errs) // 2]:.3f}"
    assert max(errs) > 1.0, "expected the reconstruction to be wrong somewhere (turns/lane offsets)"


# --------------------------------------------------------------------------- #
# 3. convention -- degrees CCW from East, and the manifest declares it
# --------------------------------------------------------------------------- #
def _median_bearing_error(rows, as_cw_from_north: bool) -> float:
    tracks = {}
    for r in rows:
        tracks.setdefault(r["true_vehicle_id"], []).append(r)
    errs = []
    for track in tracks.values():
        track.sort(key=lambda r: r["t"])
        for a, b in zip(track, track[1:]):
            dt = b["t"] - a["t"]
            dx, dy = b["true_x"] - a["true_x"], b["true_y"] - a["true_y"]
            if not 0.0 < dt <= 1.001 or math.hypot(dx, dy) < 2.0:
                continue                      # need real displacement for a meaningful bearing
            h = a["true_heading"]
            ang = math.radians(90.0 - h) if as_cw_from_north else math.radians(h)
            moved = math.atan2(dy, dx)
            errs.append(abs((math.degrees(ang - moved) + 180.0) % 360.0 - 180.0))
    errs.sort()
    return errs[len(errs) // 2] if errs else float("nan")


def test_true_heading_points_along_the_direction_of_travel(honest):
    _res, rows = honest
    assert _median_bearing_error(rows, as_cw_from_north=False) < 5.0


def test_heading_convention_is_ccw_from_east_not_cw_from_north(honest):
    """Guards the cross-engine divergence recorded in ADR 0002: reading the Python field with the
    MOSAIC/SUMO/ETSI convention must be visibly wrong, so nobody merges the two datasets blind."""
    _res, rows = honest
    ccw_east = _median_bearing_error(rows, as_cw_from_north=False)
    cw_north = _median_bearing_error(rows, as_cw_from_north=True)
    assert cw_north > 30.0 > ccw_east, (ccw_east, cw_north)


def test_manifest_declares_schema_version_and_conventions(mixed):
    res, _rows = mixed
    man = json.load(open(f"{res.out_dir}/manifest.json", encoding="utf-8"))
    assert man["schema_versions"] == {"ma_visible": 1, "ground_truth": 2}
    assert man["conventions"]["heading"] == "deg_ccw_from_east"
    assert man["conventions"]["speed"] == "m_s"


# --------------------------------------------------------------------------- #
# 4. firewall -- ORACLE only
# --------------------------------------------------------------------------- #
def test_new_fields_are_forbidden_feature_keys():
    assert is_forbidden_feature_key("true_speed") and is_forbidden_feature_key("true_heading")
    assert find_forbidden_keys({"detector_score": 1.0, "true_speed": 3.0}) == ["true_speed"]


def test_emission_rows_are_oracle_and_rejected_by_the_ma_linter(mixed):
    _res, rows = mixed
    assert all(r["_visibility"] == "ORACLE" for r in rows)
    with pytest.raises(LeakageViolation):
        assert_ma_visible(rows[0], context="gt_emissions_sample[0]")


def test_no_ma_visible_file_carries_the_new_fields(mixed):
    res, _rows = mixed
    import glob
    import os
    checked = 0
    for path in sorted(glob.glob(os.path.join(res.out_dir, "ma", "*.jsonl"))):
        for row in _jsonl(path):
            assert find_forbidden_keys(row) == [], (path, row)
            assert "true_speed" not in json.dumps(row) and "true_heading" not in json.dumps(row)
            checked += 1
    assert checked > 100, f"expected MA rows to lint (got {checked})"
