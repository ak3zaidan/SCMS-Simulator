"""End-to-end tests for the closed-loop reference pipeline.

Validates the P1 acceptance criteria: the causal chain attack -> detection ->
report -> correlation -> linkage resolution -> revocation -> CRL -> enforcement
is present in MA-visible data; ground truth is isolated; the leakage linter is
clean on MA outputs; and the same seed yields byte-identical data.
"""

import json
import os

from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _read_jsonl(path):
    with open(path, encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


def _run(tmp_path, **over):
    cfg = PipelineConfig(out_dir=str(tmp_path / "run"), **over)
    return run_pipeline(cfg), cfg


def test_closed_loop_revokes_the_attacker(tmp_path):
    res, cfg = _run(tmp_path)
    assert res.n_reports > 0, "no misbehaviour reports were generated"
    assert res.n_revoked == len(cfg.attacker_ids) == 1
    assert res.n_investigations == 1

    # Every revocation targeted a true attacker (precision == 1.0).
    gt_rev = _read_jsonl(tmp_path / "run" / "ground_truth" / "gt_linkage_revocation.jsonl")
    assert gt_rev and all(r["should_have_been_revoked"] for r in gt_rev)

    # The attacker was actually revoked (recall for the single attacker).
    gt_veh = {r["true_vehicle_id"]: r for r in
              _read_jsonl(tmp_path / "run" / "ground_truth" / "gt_vehicle.jsonl")}
    revoked_ids = {r["true_vehicle_id"] for r in gt_rev}
    attackers = {vid for vid, r in gt_veh.items() if r["is_attacker"]}
    assert revoked_ids == attackers


def test_ma_outputs_have_zero_leakage(tmp_path):
    _run(tmp_path)
    ma_dir = tmp_path / "run" / "ma"
    for name in os.listdir(ma_dir):
        for row in _read_jsonl(ma_dir / name):
            hits = find_forbidden_keys(row)
            assert hits == [], f"{name} leaks ground truth: {hits}"


def test_ground_truth_is_quarantined_and_would_be_caught(tmp_path):
    _run(tmp_path)
    # The GT plane DOES carry identity/labels -> proves the linter would flag it
    # if it were ever placed in MA-visible data.
    gt_veh = _read_jsonl(tmp_path / "run" / "ground_truth" / "gt_vehicle.jsonl")
    assert any(find_forbidden_keys(r) for r in gt_veh)


def test_enforcement_stops_reports_after_revocation(tmp_path):
    res, cfg = _run(tmp_path)
    attacker_digest = res.revoked_cert_digests[0]
    inv = _read_jsonl(tmp_path / "run" / "ma" / "ma_investigations.jsonl")[0]
    revoke_time = inv["opened_time"]
    reports = _read_jsonl(tmp_path / "run" / "ma" / "ma_reports.jsonl")
    attacker_reports = [r for r in reports if r["subject_cert_digest"] == attacker_digest]
    assert attacker_reports, "attacker should have been reported"
    last_detection = max(r["detection_time"] for r in attacker_reports)
    # No new reports about the attacker once the CRL has propagated.
    assert last_detection <= revoke_time + cfg.crl_propagation_delay


def test_determinism_same_seed_byte_identical(tmp_path):
    r1, _ = _run(tmp_path / "a", seed=2024)
    r2, _ = _run(tmp_path / "b", seed=2024)
    assert r1.data_digest == r2.data_digest


def test_different_seed_changes_data(tmp_path):
    r1, _ = _run(tmp_path / "a", seed=1)
    r2, _ = _run(tmp_path / "b", seed=2)
    assert r1.data_digest != r2.data_digest
