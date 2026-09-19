"""CRL-aware evasive attackers: adversaries that watch the PUBLIC CRL and go dormant (broadcast
honestly) after an accomplice is revoked -- a feedback-aware adversary that starves the detector of
sustained evidence. Off by default (crl_aware_pct=0) -> byte-identical to the pre-feature pipeline."""

import glob
import json

from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _count_reports(out):
    return len(_jsonl(out + "/ma/ma_reports.jsonl"))


# The reference default-path digest, measured on the starting commit BEFORE this feature existed.
_GOLDEN = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"


def test_default_path_is_byte_identical(tmp_path):
    """The canonical reference config (crl_aware_pct defaults to 0) still hashes to the golden digest:
    the feature adds zero draws and zero output on the default path."""
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == _GOLDEN, r.data_digest


def test_crl_aware_zero_explicit_matches_default(tmp_path):
    """crl_aware_pct=0 stated explicitly is indistinguishable from leaving it at its default."""
    a = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "a")))
    b = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    crl_aware_pct=0.0, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest


def test_crl_aware_is_deterministic(tmp_path):
    """Same seed + config with the feature ON -> byte-identical digest (dormancy uses no RNG)."""
    cfg = dict(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0, arrival_rate=1.5,
               grid_w=6, grid_h=6, attacker_pct=0.3, attack_type="RandomPos",
               attack_types=("RandomPos",), faulty_pct=0.0, crl_aware_pct=0.6)
    a = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "d1")))
    b = run_pipeline(PipelineConfig(**cfg, out_dir=str(tmp_path / "d2")))
    assert a.data_digest == b.data_digest


def test_crl_aware_attackers_evade_detection(tmp_path):
    """An easily-detected attack (RandomPos): making every attacker CRL-aware makes them lie low each
    time an accomplice is busted, so far FEWER misbehaviour reports are filed than when the identical
    fleet attacks obliviously. Long enough that several revocations occur mid-run."""
    common = dict(seed=13, traffic_flow=True, road_network="grid", duration_s=150.0, arrival_rate=1.5,
                  grid_w=6, grid_h=6, attacker_pct=0.3, attack_type="RandomPos",
                  attack_types=("RandomPos",), faulty_pct=0.0)
    off = str(tmp_path / "off")
    on = str(tmp_path / "on")
    r_off = run_pipeline(PipelineConfig(**common, crl_aware_pct=0.0, out_dir=off))
    run_pipeline(PipelineConfig(**common, crl_aware_pct=1.0, out_dir=on))
    assert r_off.n_revoked > 3, "need several revocations for the CRL feedback to matter"
    assert _count_reports(on) < _count_reports(off), (_count_reports(on), _count_reports(off))


def test_gt_vehicle_labels_crl_aware_without_leaking(tmp_path):
    """gt_vehicle (ORACLE) carries is_crl_aware for aware attackers; the label appears in NO MA-visible
    file (the leakage linter finds nothing on ma/*.jsonl)."""
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0,
                                arrival_rate=1.5, grid_w=6, grid_h=6, attacker_pct=0.3,
                                attack_type="RandomPos", attack_types=("RandomPos",), faulty_pct=0.0,
                                crl_aware_pct=1.0, out_dir=out))
    veh = _jsonl(out + "/ground_truth/gt_vehicle.jsonl")
    aware = [v for v in veh if v.get("is_crl_aware")]
    assert aware, "expected some CRL-aware attackers labelled in ground truth"
    assert all(v["is_attacker"] for v in aware), "only attackers can be CRL-aware"
    # every MA-visible record is free of ground-truth keys (incl. is_crl_aware)
    for f in glob.glob(out + "/ma/*.jsonl"):
        for row in _jsonl(f):
            assert find_forbidden_keys(row) == [], (f, find_forbidden_keys(row))
