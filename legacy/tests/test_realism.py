"""Tests for the enriched reference generator: attack diversity, sensor realism, faulty class."""

import json

from scms_sim_ref.datagen import calibration as C
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _rich(tmp_path, **over):
    cfg = PipelineConfig(out_dir=str(tmp_path / "run"), seed=7, n_vehicles=48, n_steps=70,
                         attacker_pct=0.25, faulty_pct=0.1, weather="rain", **over)
    return run_pipeline(cfg), cfg


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def test_attacks_are_diverse_with_onset(tmp_path):
    res, _ = _rich(tmp_path)
    atk = _jsonl(tmp_path / "run" / "ground_truth" / "gt_attacks.jsonl")
    types = {a["attack_type"] for a in atk}
    assert len(types) >= 5, f"expected diverse attacks, got {types}"
    # every attacker that actually falsified has an onset timestamp
    assert all(a["attack_onset_time"] is not None for a in atk)


def test_faulty_class_present_and_not_falsely_revoked(tmp_path):
    res, _ = _rich(tmp_path)
    veh = _jsonl(tmp_path / "run" / "ground_truth" / "gt_vehicle.jsonl")
    assert any(v.get("is_faulty") for v in veh), "no faulty vehicles generated"
    gt_rev = _jsonl(tmp_path / "run" / "ground_truth" / "gt_linkage_revocation.jsonl")
    # sustained-bias faults are smooth/self-consistent -> must not be revoked
    faulty_ids = {v["true_vehicle_id"] for v in veh if v.get("is_faulty")}
    revoked_ids = {r["true_vehicle_id"] for r in gt_rev}
    assert not (faulty_ids & revoked_ids), "a faulty (non-attacker) vehicle was revoked"


def test_sensor_noise_yields_realistic_false_positives(tmp_path):
    """Benign GNSS error must produce some honest false-positive reports (precision < 1.0),
    the whole point of the sensor model."""
    res, _ = _rich(tmp_path)
    labels = [r["report_correctness"]
              for r in _jsonl(tmp_path / "run" / "ground_truth" / "gt_report_labels.jsonl")]
    assert "false_positive" in labels, "no benign false positives — data is unrealistically clean"


def test_emissions_and_calibration(tmp_path):
    _rich(tmp_path)
    emis = _jsonl(tmp_path / "run" / "ground_truth" / "gt_emissions_sample.jsonl")
    assert emis and all("pos_conf" in e for e in emis)
    cal = C.calibrate(str(tmp_path / "run"))
    assert cal["gps_error"]["cep_in_real_range"], cal["gps_error"]


def test_rich_run_is_leakage_free_and_deterministic(tmp_path):
    r1, _ = _rich(tmp_path / "a")
    r2, _ = _rich(tmp_path / "b")
    assert r1.data_digest == r2.data_digest
    for name in ("ma_reports.jsonl", "ma_cert_status.jsonl", "ma_investigations.jsonl"):
        for row in _jsonl(tmp_path / "a" / "run" / "ma" / name):
            assert find_forbidden_keys(row) == [], f"{name} leaks: {find_forbidden_keys(row)}"
