"""Full-chain regression test: generate -> featurize -> benchmark -> calibration -> datasheet -> audit.

Exercises a realistic flow dataset (routed car-following, all attack families, signals, mixed fleet)
through every downstream stage and asserts the cross-cutting invariants hold end to end.
"""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path

from scms_sim_ref.datagen import benchmark, calibration, datasheet, featurize, validate
from scms_sim_ref.datagen.leakage_linter import is_forbidden_feature_key
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

REPO = Path(__file__).resolve().parents[1]
_vd_spec = importlib.util.spec_from_file_location("verify_data", REPO / "tools" / "verify_data.py")
verify_data = importlib.util.module_from_spec(_vd_spec)
_vd_spec.loader.exec_module(verify_data)


def test_full_flow_pipeline_end_to_end(tmp_path):
    out = str(tmp_path / "run")
    cfg = PipelineConfig(out_dir=out, traffic_flow=True, road_network="grid", car_following=True,
                         duration_s=220.0, arrival_rate=2.5, grid_w=6, grid_h=6, attacker_pct=0.18,
                         faulty_pct=0.06, collude_pct=0.3, rotate_period_s=60.0, traffic_lights=True,
                         fleet="mixed", weather="rain", radio_range_m=250.0, seed=17)
    res = run_pipeline(cfg)
    assert res.n_vehicles > 100 and res.n_reports > 0 and res.n_revoked > 0

    # 1) featurize + column-level leakage check across every FEATURE table
    featurize.build(out)
    for tbl in ("report_features", "subject_features", "vehicle_features", "vehicle_features_ma"):
        cols = _cols(Path(out) / "ml" / f"{tbl}.csv")
        bad = [c for c in cols if c not in ("split", "time_split", "domain_id") and is_forbidden_feature_key(c)]
        assert not bad, f"{tbl} leaks: {bad}"

    # 2) validate: realistic precision/recall + latency present
    vsum, _ = validate.validate(out)
    assert 0.6 <= vsum["precision"] <= 1.0 and vsum["recall"] > 0.3

    # 3) benchmark: both baselines run and score non-trivially
    bench = benchmark.run(out)
    subj = bench["tasks"].get("subject_is_attacker", {})
    assert subj.get("roc_auc") is not None and subj["roc_auc"] > 0.5
    assert subj.get("gbdt", {}).get("roc_auc") is not None

    # 4) calibration: GNSS error in the real range
    assert calibration.calibrate(out)["gps_error"]["cep_in_real_range"]

    # 5) datasheet renders and includes the traffic-flow section
    md = datasheet.build(out)
    assert "Traffic flow & congestion" in md and "Fleet composition" in md

    # 6) data-integrity audit: no FAILs on this dataset
    fails = [r for r in verify_data.run_audit(Path(out).parent) if r[2] == "FAIL"]
    assert not fails, fails

    # 7) determinism: a second identical run is byte-identical
    assert run_pipeline(PipelineConfig(**{**cfg.__dict__, "out_dir": str(tmp_path / "run2")})).data_digest \
        == res.data_digest


def _cols(p: Path):
    if not p.exists():
        return []
    return p.read_text(encoding="utf-8").splitlines()[0].split(",")
