"""Tests for opt-in MOBIL-style discretionary lane changes (audit gap #7).

The feature adds realistic benign lateral maneuvers (a smooth lane change with a brief heading swing)
that are the dominant source of the HARDEST misbehaviour false positives. It is DEFAULT OFF, so every
existing dataset must stay byte-identical; when ON it must produce observable, deterministic lane
transitions with plausible heading transients that do NOT cause mass false revocations.
"""

import json
import pathlib

import pytest

import scms_sim_ref.mock_pipeline.run as run_mod
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.datagen import validate as V

# Frozen goldens (recorded on the base commit BEFORE this feature). Default OFF => byte-identical.
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 04ae9736f519... (default-config run; behaviour unchanged).
DEFAULT_DIGEST = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 38845a32f35e... (multilane, lane_changes off run; behaviour unchanged).
MULTILANE_DIGEST = "0a9e82ec549f876843ba39cce241ab28fdb39ea94900151d511e64e8c93f2277"


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _default_cfg(out_dir):
    return PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                          arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=out_dir)


def _multilane_cfg(out_dir, **over):
    kw = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=3.0,
              grid_w=6, grid_h=6, n_lanes=3, attacker_pct=0.25, out_dir=out_dir)
    kw.update(over)
    return PipelineConfig(**kw)


def _dense_cfg(out_dir, **over):
    """Dense multi-lane flow so plenty of vehicles get blocked and want to overtake."""
    kw = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=120, arrival_rate=4.0,
              grid_w=6, grid_h=6, n_lanes=3, attacker_pct=0.1, out_dir=out_dir)
    kw.update(over)
    return PipelineConfig(**kw)


def _collect_changes(cfg):
    """Run with the telemetry hook installed; return the list of initiated lane-change events."""
    events = []
    run_mod.LANE_CHANGE_HOOK = events.append
    try:
        res = run_pipeline(cfg)
    finally:
        run_mod.LANE_CHANGE_HOOK = None
    return res, events


# --------------------------------------------------------------------------- #
# 1. Determinism firewall: default OFF => existing digests byte-identical
# --------------------------------------------------------------------------- #
def test_default_golden_digest_unchanged(tmp_path):
    assert run_pipeline(_default_cfg(str(tmp_path / "d"))).data_digest == DEFAULT_DIGEST


def test_multilane_without_lane_changes_matches_golden(tmp_path):
    # n_lanes=3 but lane_changes defaults OFF -> the multi-lane path stays byte-identical.
    res = run_pipeline(_multilane_cfg(str(tmp_path / "m")))
    assert res.data_digest == MULTILANE_DIGEST
    # the new knob is genuinely off by default (guards against an accidental default flip)
    assert PipelineConfig.lane_changes is False


def test_multilane_lane_changes_off_explicit_matches_golden(tmp_path):
    res = run_pipeline(_multilane_cfg(str(tmp_path / "m"), lane_changes=False))
    assert res.data_digest == MULTILANE_DIGEST


# --------------------------------------------------------------------------- #
# 2. When ON: observable, adjacent-lane transitions actually occur
# --------------------------------------------------------------------------- #
def test_lane_changes_produce_observable_transitions(tmp_path):
    _, events = _collect_changes(_dense_cfg(str(tmp_path / "on"), lane_changes=True))
    assert len(events) >= 30, f"expected many lane changes in dense flow, got {len(events)}"
    vids = {e["vid"] for e in events}
    assert len(vids) >= 15, f"lane changes should span many vehicles, got {len(vids)}"
    # every change moves to an ADJACENT lane (one lane width of lateral offset, either direction)
    for e in events:
        assert abs(abs(e["to_off"] - e["from_off"]) - 3.5) < 1e-6, e


def test_lane_changes_off_yields_no_transitions(tmp_path):
    # With the flag OFF the maneuver code (and its telemetry) is never reached.
    _, events = _collect_changes(_dense_cfg(str(tmp_path / "off"), lane_changes=False))
    assert events == []


# --------------------------------------------------------------------------- #
# 3. Realism pressure: benign vehicles get brief, PLAUSIBLE heading transients
# --------------------------------------------------------------------------- #
def test_lane_changes_create_benign_heading_transients(tmp_path):
    _, events = _collect_changes(_dense_cfg(str(tmp_path / "on"), lane_changes=True))
    benign = [e for e in events if not e["is_attacker"] and not e["is_faulty"]]
    assert len(benign) >= 20, f"need benign lane changes for FP pressure, got {len(benign)}"
    devs = [e["peak_heading_dev_deg"] for e in benign]
    # the maneuver produces a real heading swing (several benign vehicles well above sensor jitter) ...
    assert sum(d > 3.0 for d in devs) >= 10, f"benign heading transients too small: {sorted(devs)[-5:]}"
    # ... but it stays PLAUSIBLE: capped well under the heading-inconsistency detector threshold (35 deg),
    # so a single benign lane change is not itself an attack-looking heading lie.
    assert max(devs) <= 13.0, f"heading swing implausibly large: max={max(devs)}"
    assert max(devs) < 35.0


# --------------------------------------------------------------------------- #
# 4. No precision collapse: benign lane changes must not cause mass false revocations
# --------------------------------------------------------------------------- #
def _precision_and_benign_fp(out_dir, lane_changes):
    cfg = PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=200,
                         arrival_rate=2.0, grid_w=6, grid_h=6, grid_block_m=140.0, n_lanes=3,
                         attacker_pct=0.1, faulty_pct=0.0, radio_range_m=250.0,
                         lane_changes=lane_changes, out_dir=out_dir)
    res = run_pipeline(cfg)
    stats, _ = V.validate(res.out_dir)
    veh = _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_vehicle.jsonl")
    attackers = {v["true_vehicle_id"] for v in veh if v.get("is_attacker")}
    revoked = {r["true_vehicle_id"] for r in
               _jsonl(pathlib.Path(res.out_dir) / "ground_truth" / "gt_linkage_revocation.jsonl")}
    return stats["precision"], stats["recall"], len(revoked - attackers)


def test_lane_changes_do_not_collapse_revocation_precision(tmp_path):
    p_off, r_off, fp_off = _precision_and_benign_fp(str(tmp_path / "off"), False)
    p_on, r_on, fp_on = _precision_and_benign_fp(str(tmp_path / "on"), True)
    # benign-heavy operating point: precision stays high with lane changes ON ...
    assert p_on >= 0.9, f"precision collapsed with lane changes: {p_on} (off={p_off})"
    # ... and no WORSE than with the maneuver off (the sustained-evidence gate absorbs the transient).
    assert p_on >= p_off - 0.03, f"lane changes degraded precision: on={p_on} off={p_off}"
    assert fp_on <= fp_off + 1, f"lane changes added benign false revocations: on={fp_on} off={fp_off}"


# --------------------------------------------------------------------------- #
# 5. Determinism with the feature ON (same seed+config -> byte-identical)
# --------------------------------------------------------------------------- #
def test_lane_changes_on_is_deterministic(tmp_path):
    a = run_pipeline(_dense_cfg(str(tmp_path / "a"), lane_changes=True)).data_digest
    b = run_pipeline(_dense_cfg(str(tmp_path / "b"), lane_changes=True)).data_digest
    assert a == b


def test_telemetry_hook_does_not_change_the_digest(tmp_path):
    # The telemetry seam must be side-effect free on the dataset (like PER_STEP_HOOK).
    with_hook, _ = _collect_changes(_dense_cfg(str(tmp_path / "h"), lane_changes=True))
    without = run_pipeline(_dense_cfg(str(tmp_path / "n"), lane_changes=True))
    assert with_hook.data_digest == without.data_digest


# --------------------------------------------------------------------------- #
# 6. Validation: lane_changes is only meaningful with n_lanes>1 AND traffic_flow
# --------------------------------------------------------------------------- #
def test_lane_changes_requires_multi_lane(tmp_path):
    with pytest.raises(ValueError, match="n_lanes"):
        run_pipeline(_multilane_cfg(str(tmp_path / "x"), n_lanes=1, lane_changes=True))


def test_lane_changes_requires_traffic_flow(tmp_path):
    with pytest.raises(ValueError, match="traffic_flow"):
        run_pipeline(PipelineConfig(seed=7, traffic_flow=False, n_lanes=3, lane_changes=True,
                                    out_dir=str(tmp_path / "y")))
