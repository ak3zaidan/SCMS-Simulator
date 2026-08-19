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


def test_validate_reports_rsu_contribution(tmp_path):
    """validate() quantifies RSU-sourced evidence; absent (empty) when no RSUs are configured."""
    out = _sparse(tmp_path, 20)
    s = V.validate(out)[0]
    rc = s["rsu_contribution"]
    assert rc and rc["reports"] > 0 and rc["rsus_reporting"] >= 1
    assert 0.0 <= rc["precision"] <= 1.0
    # no RSUs -> empty contribution block
    out0 = _sparse(tmp_path, 0)
    assert V.validate(out0)[0]["rsu_contribution"] == {}


def test_rsu_placement_strategies_all_work(tmp_path):
    """Every RSU placement strategy produces a valid run; 'all' puts one RSU at every intersection."""
    for pl in ("spread", "perimeter", "center", "corners"):
        out = str(tmp_path / pl)
        run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0,
                                    arrival_rate=0.8, grid_w=5, grid_h=5, radio_range_m=120.0,
                                    attacker_pct=0.2, n_rsus=6, rsu_placement=pl, out_dir=out))
        rc = V.validate(out)[0]["rsu_contribution"]
        assert rc.get("rsus_reporting", 0) >= 1
    # 'all' ignores n_rsus and covers every one of the 5x5 = 25 intersections
    out = str(tmp_path / "all")
    run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0,
                                arrival_rate=0.8, grid_w=5, grid_h=5, radio_range_m=120.0,
                                attacker_pct=0.2, n_rsus=6, rsu_placement="all", out_dir=out))
    assert V.validate(out)[0]["rsu_contribution"]["rsus_reporting"] <= 25


def test_rsu_range_extends_coverage(tmp_path):
    """A longer RSU radio range makes each RSU hear more transmitters -> more RSU-sourced reports."""
    def rsu_reports(rng_m):
        out = str(tmp_path / f"r{rng_m}")
        run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=150.0,
                                    arrival_rate=0.5, grid_w=6, grid_h=6, radio_range_m=100.0,
                                    attacker_pct=0.25, attack_type="RandomPos",
                                    attack_types=("RandomPos",), n_rsus=6, rsu_range_m=rng_m,
                                    out_dir=out))
        return V.validate(out)[0]["rsu_contribution"].get("reports", 0)
    assert rsu_reports(400.0) > rsu_reports(100.0) * 2, "longer RSU range should hear many more CAMs"


def test_rsus_improve_detection_in_reporter_starved_traffic(tmp_path):
    """In sparse traffic (few mobile reporters), always-present RSUs raise revocation recall of an
    easily-detected attack without hurting precision -- infrastructure-assisted detection."""
    s0, _ = V.validate(_sparse(tmp_path, 0))
    s1, _ = V.validate(_sparse(tmp_path, 20))
    assert s1["recall"] > s0["recall"] + 0.15, (s0["recall"], s1["recall"])
    assert s1["precision"] >= 0.9, s1["precision"]
