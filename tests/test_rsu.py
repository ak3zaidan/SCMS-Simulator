"""Road-Side Units: fixed, always-trusted receivers (opt-in). Off by default -> byte-identical."""

import json

from scms_sim_ref.datagen import validate as V
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _sparse(tmp, n_rsus, seed=21):
    out = str(tmp / f"r{n_rsus}")
    run_pipeline(PipelineConfig(seed=seed, traffic_flow=True, road_network="grid", duration_s=300.0,
                                arrival_rate=0.25, grid_w=6, grid_h=6, grid_block_m=120.0,
                                radio_range_m=90.0, attacker_pct=0.25, attack_type="RandomPos",
                                attack_types=("RandomPos",), faulty_pct=0.0, n_rsus=n_rsus,
                                out_dir=out))
    return out


def test_rsus_off_is_a_no_op(tmp_path):
    """n_rsus=0 (default) must be byte-identical to not configuring RSUs at all."""
    a = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=120.0,
                                    arrival_rate=2.0, grid_w=5, grid_h=5, attacker_pct=0.2,
                                    out_dir=str(tmp_path / "a")))
    b = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=120.0,
                                    arrival_rate=2.0, grid_w=5, grid_h=5, attacker_pct=0.2, n_rsus=0,
                                    out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest


def test_rsus_file_trusted_reports_and_are_not_counted_as_vehicles(tmp_path):
    out = _sparse(tmp_path, 20)
    idm = {m["pseudonym_cert_digest"] for m in _jsonl(out + "/ground_truth/gt_identity_map.jsonl")}
    reports = _jsonl(out + "/ma/ma_reports.jsonl")
    rsu_reports = [r for r in reports if r["reporter_cert_digest"] not in idm]
    assert rsu_reports, "RSUs should file reports as receivers"
    # RSUs are infrastructure, not vehicles: none appear in the vehicle ground truth
    veh = _jsonl(out + "/ground_truth/gt_vehicle.jsonl")
    assert all(not v.get("is_rsu", False) for v in veh)
    # RSU reports carry no leaked identity (MA-visible only)
    for r in rsu_reports[:50]:
        assert find_forbidden_keys(r) == []


def test_rsus_improve_detection_in_reporter_starved_traffic(tmp_path):
    """In sparse traffic (few mobile reporters), always-present RSUs raise revocation recall of an
    easily-detected attack without hurting precision -- infrastructure-assisted detection."""
    s0, _ = V.validate(_sparse(tmp_path, 0))
    s1, _ = V.validate(_sparse(tmp_path, 20))
    assert s1["recall"] > s0["recall"] + 0.15, (s0["recall"], s1["recall"])
    assert s1["precision"] >= 0.9, s1["precision"]
