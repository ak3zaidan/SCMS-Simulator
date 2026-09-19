"""Vulnerable Road Users (VRUs: pedestrians/cyclists). OPT-IN benign, self-declaring transmitters.

Guarantees exercised here:
  * vru_pct=0 (default) is BYTE-IDENTICAL -- VRUs draw no RNG and touch no code path.
  * vru_pct>0 adds benign VRU actors that broadcast a beacon SELF-DECLARING station_type=vru
    (an MA-visible field), are labelled is_vru in the ORACLE ground truth (never attackers), and
    travel at ~vru_speed_mps.
  * A receiver seeing a VRU-declared beacon SUPPRESSES the off-road + vehicle-kinematic detectors,
    so VRUs generate NO benign false revocations and revocation precision stays high.
  * Leakage firewall: MA-visible files carry station_type; the oracle is_vru appears ONLY in
    gt_vehicle; no forbidden key reaches MA reports or feature tables.
  * Determinism: a VRU run is reproducible byte-for-byte.
"""

import json

import pytest

from scms_sim_ref.datagen import validate as V
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

# The published default golden digest (see the determinism contract). vru_pct defaults to 0.0, so the
# default run MUST stay byte-identical to this value after the VRU feature is added.
GOLDEN = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _default_cfg(tmp, **kw):
    base = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
    base.update(kw)
    return PipelineConfig(**base)


# --------------------------------------------------------------------------- #
# 1. determinism / byte-identical default
# --------------------------------------------------------------------------- #
def test_default_golden_unchanged(tmp_path):
    """The documented default digest is unchanged by the VRU feature (vru_pct defaults to 0)."""
    r = run_pipeline(_default_cfg(tmp_path, out_dir=str(tmp_path / "g")))
    assert r.data_digest == GOLDEN


def test_vru_pct_zero_is_byte_identical_to_default(tmp_path):
    """Explicit vru_pct=0.0 == the default: no VRUs, no RNG change, byte-identical dataset."""
    a = run_pipeline(_default_cfg(tmp_path, out_dir=str(tmp_path / "a")))
    b = run_pipeline(_default_cfg(tmp_path, vru_pct=0.0, vru_speed_mps=1.8, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest == GOLDEN


def test_vru_run_is_deterministic(tmp_path):
    """Same seed + config with vru_pct=0.3 -> byte-identical data digest across two runs."""
    a = run_pipeline(_default_cfg(tmp_path, vru_pct=0.3, out_dir=str(tmp_path / "a")))
    b = run_pipeline(_default_cfg(tmp_path, vru_pct=0.3, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest


# --------------------------------------------------------------------------- #
# 2. VRUs are benign, self-declaring transmitters that move at ~vru_speed
# --------------------------------------------------------------------------- #
def test_vrus_are_benign_self_declaring_transmitters(tmp_path):
    out = str(tmp_path / "v")
    run_pipeline(_default_cfg(tmp_path, duration_s=150, grid_w=6, grid_h=6, arrival_rate=1.5,
                              attacker_pct=0.1, faulty_pct=0.02, vru_pct=0.25, vru_speed_mps=1.8,
                              chan_capacity=400, radio_range_m=250.0, out_dir=out))

    # ORACLE ground truth: VRUs present, labelled is_vru, NEVER attackers or faulty.
    gv = _jsonl(out + "/ground_truth/gt_vehicle.jsonl")
    vru = [v for v in gv if v.get("is_vru")]
    assert vru, "vru_pct>0 should produce VRU actors in the ground truth"
    assert all(not v["is_attacker"] and not v.get("is_faulty") for v in vru)
    assert all(v["veh_type"] == "vru" for v in vru)
    vru_ids = {v["true_vehicle_id"] for v in vru}

    # MA-VISIBLE self-declaration: some observed certs declare station_type=vru (a transmitted field).
    cert_status = _jsonl(out + "/ma/ma_cert_status.jsonl")
    assert all("station_type" in c for c in cert_status), "cert_status must carry the declared station type"
    assert any(c["station_type"] == "vru" for c in cert_status), "some transmitters declare station=vru"

    # VRUs travel at ~vru_speed_mps: check the sampled (oracle) emissions for VRU actors.
    em = _jsonl(out + "/ground_truth/gt_emissions_sample.jsonl")
    vspeeds = [e["claimed_speed"] for e in em if e["true_vehicle_id"] in vru_ids]
    assert vspeeds, "expected some sampled VRU beacons"
    assert 1.0 <= (sum(vspeeds) / len(vspeeds)) <= 2.8, sum(vspeeds) / len(vspeeds)


def test_vru_count_tracks_vru_pct(tmp_path):
    """More VRUs at a higher vru_pct; VRU fraction of all actors is ~vru_pct."""
    def frac(pct):
        out = str(tmp_path / f"p{pct}")
        run_pipeline(_default_cfg(tmp_path, duration_s=180, grid_w=6, grid_h=6, vru_pct=pct,
                                  out_dir=out))
        gv = _jsonl(out + "/ground_truth/gt_vehicle.jsonl")
        n_vru = sum(1 for v in gv if v.get("is_vru"))
        return n_vru, n_vru / len(gv)

    (n0, _), (n2, f2), (n4, f4) = frac(0.0), frac(0.2), frac(0.4)
    assert n0 == 0 and n2 > 0 and n4 > n2
    assert abs(f2 - 0.2) < 0.1 and abs(f4 - 0.4) < 0.1     # ~fraction of spawned actors


# --------------------------------------------------------------------------- #
# 3. detector suppression -> NO VRU false revocations, precision stays high
# --------------------------------------------------------------------------- #
def test_no_vru_false_revocations_and_precision_stays_high(tmp_path, capsys):
    """A benign-heavy run with VRUs must revoke NO VRU (off-road + kinematic detectors are correctly
    suppressed for self-declared VRUs) and keep revocation precision high."""
    out = str(tmp_path / "v")
    run_pipeline(_default_cfg(tmp_path, duration_s=180, grid_w=6, grid_h=6, arrival_rate=1.5,
                              attacker_pct=0.12, faulty_pct=0.02, vru_pct=0.25,
                              chan_capacity=400, radio_range_m=250.0, out_dir=out))
    gv = _jsonl(out + "/ground_truth/gt_vehicle.jsonl")
    vru_ids = {v["true_vehicle_id"] for v in gv if v.get("is_vru")}
    assert vru_ids

    rev = _jsonl(out + "/ground_truth/gt_linkage_revocation.jsonl")
    revoked_vrus = [x["true_vehicle_id"] for x in rev if x["true_vehicle_id"] in vru_ids]
    assert revoked_vrus == [], f"VRUs must never be falsely revoked, got {revoked_vrus}"

    # No VRU is ever the SUBJECT of a report for a suppressed reason (off-road / vehicle-kinematics).
    idm = {m["pseudonym_cert_digest"]: m["true_vehicle_id"]
           for m in _jsonl(out + "/ground_truth/gt_identity_map.jsonl")}
    suppressed = {"mapOffRoad", "positionSpeedInconsistency", "positionJump",
                  "headingInconsistency", "constantPositionFrozen", "implausibleAcceleration"}
    for r in _jsonl(out + "/ma/ma_reports.jsonl"):
        if idm.get(r["subject_cert_digest"]) in vru_ids:
            assert not (set(r.get("reason_codes", [])) & suppressed), r["reason_codes"]

    s = V.validate(out)[0]
    with capsys.disabled():
        print(f"\n[VRU benign run] precision={s['precision']} recall={s['recall']} "
              f"revoked={s['revoked']} VRUs={len(vru_ids)} revoked_VRUs={len(revoked_vrus)}")
    assert s["precision"] >= 0.9, s["precision"]


def test_vrus_do_not_degrade_precision_vs_no_vrus(tmp_path):
    """With adequate channel capacity (so VRU VAMs do not induce extra congestion loss), adding VRUs
    does not meaningfully change revocation precision -- the suppression, not luck, keeps it clean."""
    def prec(pct):
        out = str(tmp_path / f"p{pct}")
        run_pipeline(_default_cfg(tmp_path, seed=11, duration_s=180, grid_w=6, grid_h=6,
                                  attacker_pct=0.15, faulty_pct=0.03, chan_capacity=400,
                                  radio_range_m=250.0, vru_pct=pct, out_dir=out))
        return V.validate(out)[0]["precision"]

    assert prec(0.25) >= prec(0.0) - 0.05


# --------------------------------------------------------------------------- #
# 4. leakage: oracle is_vru vs MA-visible station_type
# --------------------------------------------------------------------------- #
def test_leakage_oracle_is_vru_vs_ma_visible_station_type(tmp_path):
    """is_vru is ORACLE-only (gt_vehicle); station_type is MA-visible; no forbidden key reaches the
    MA reports/cert-status or the feature tables (mirrors tests/test_rsu.py)."""
    from scms_sim_ref.datagen import featurize
    import pandas as pd
    out = str(tmp_path / "v")
    run_pipeline(_default_cfg(tmp_path, duration_s=150, grid_w=6, grid_h=6, attacker_pct=0.15,
                              chan_capacity=400, radio_range_m=250.0, vru_pct=0.25, out_dir=out))

    # ORACLE side: is_vru lives ONLY in gt_vehicle.
    assert any(v.get("is_vru") for v in _jsonl(out + "/ground_truth/gt_vehicle.jsonl"))

    # MA-visible side: NO is_vru anywhere under ma/; reports & cert_status pass the leakage linter and
    # cert_status carries the legitimately-transmitted station_type instead.
    import pathlib
    for f in (pathlib.Path(out) / "ma").glob("*.jsonl"):
        for row in _jsonl(str(f)):
            assert "is_vru" not in row, f
            assert find_forbidden_keys(row) == [], (f, row)
    assert any(c.get("station_type") == "vru" for c in _jsonl(out + "/ma/ma_cert_status.jsonl"))

    # Feature tables: is_vru_declared IS a (leakage-safe) feature; the oracle is_vru never appears.
    featurize.build(out)
    ml = pathlib.Path(out) / "ml"
    for name in ("report_features", "subject_features", "vehicle_features", "vehicle_features_ma"):
        df = pd.read_csv(ml / f"{name}.csv")
        assert "is_vru_declared" in df.columns, name
        assert "is_vru" not in df.columns, name
        assert find_forbidden_keys({c: 0 for c in df.columns if c not in ("split", "time_split")}) == []
    # schema.json advertises it as a real feature, and the leakage linter accepts the name.
    schema = json.loads((ml / "schema.json").read_text())
    kinds = {c["name"]: c["kind"] for c in schema["subject_features"]}
    assert kinds.get("is_vru_declared") == "feature"


def test_vru_dataset_passes_validation_leakage_scan(tmp_path):
    """The whole MA-visible dataset of a VRU run passes validate()'s leakage scan (0 violations)."""
    out = str(tmp_path / "v")
    run_pipeline(_default_cfg(tmp_path, duration_s=120, grid_w=6, grid_h=6, attacker_pct=0.2,
                              vru_pct=0.2, out_dir=out))
    s = V.validate(out)[0]
    assert s.get("leakage_violations", 0) == 0


# --------------------------------------------------------------------------- #
# 5. config plumbing
# --------------------------------------------------------------------------- #
def test_vru_speed_must_be_positive_when_enabled():
    with pytest.raises(ValueError):
        run_pipeline(PipelineConfig(vru_pct=0.2, vru_speed_mps=0.0, out_dir="unused"))
