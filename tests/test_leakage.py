"""Tests for the leakage linter and the trust-visibility firewall."""

import pytest

from scms_sim_ref.datagen.leakage_linter import (
    LeakageViolation,
    assert_ma_visible,
    find_forbidden_keys,
    lint_feature_frame,
)
from scms_sim_ref.schemas.records import (
    GtVehicle,
    MaEvidenceMessage,
    MaReport,
    is_forbidden_feature_key,
)


def _clean_report() -> MaReport:
    return MaReport(
        report_id="r1", ingest_time=10.0, detection_time=9.5, generation_time=9.4,
        reporter_cert_digest="aabbccddeeff0011",
        subject_cert_digest="1100ffeeddccbbaa",
        reason_codes=["positionPlausibility"],
        detector_outputs=[{"check_id": "rangePlausibility", "score": 0.9, "verdict": "fail"}],
        cert_validity={"sig_valid": True, "not_expired": True, "not_revoked": True, "chain_ok": True},
        evidence_msg_refs=["m1"], st_bbox=[0.0, 0.0, 10.0, 10.0], st_tstart=9.0, st_tend=9.5,
    )


def test_forbidden_key_predicate():
    for k in ("true_vehicle_id", "senderRealId", "reportedRealId", "true_x",
              "is_attacker", "attack_type", "reporter_true_id", "real_id"):
        assert is_forbidden_feature_key(k), k
    for k in ("reporter_cert_digest", "subject_cert_digest", "reason_codes", "score"):
        assert not is_forbidden_feature_key(k), k


def test_clean_ma_report_passes():
    assert_ma_visible(_clean_report(), context="clean report")
    assert find_forbidden_keys(_clean_report().to_dict()) == []


def test_evidence_message_is_clean():
    ev = MaEvidenceMessage(
        msg_ref="m1", subject_cert_digest="1100ffeeddccbbaa", gen_time=9.4,
        claimed_x=1.0, claimed_y=2.0, claimed_speed=3.0, claimed_heading=90.0,
        sig_valid=True, cert_status_at_eval="valid",
    )
    assert_ma_visible(ev)


def test_injected_top_level_leak_is_caught():
    d = _clean_report().to_dict()
    d["true_vehicle_id"] = "veh_42"          # the classic leak
    with pytest.raises(LeakageViolation):
        assert_ma_visible(d)


def test_injected_nested_leak_is_caught():
    d = _clean_report().to_dict()
    d["detector_outputs"][0]["senderRealId"] = "veh_42"   # F2MD-style nested leak
    with pytest.raises(LeakageViolation):
        assert_ma_visible(d)


def test_ground_truth_record_rejected_from_ma():
    gt = GtVehicle(true_vehicle_id="veh_1", spawn_time=0.0, is_attacker=True,
                   attacker_role="position_falsifier")
    with pytest.raises(LeakageViolation):
        assert_ma_visible(gt, context="attempted GT leak")


def test_feature_frame_linting():
    rows = [{"reason_code_posP": 1, "detector_score": 0.8, "n_reporters": 3}]
    assert lint_feature_frame(rows) == 1
    rows_bad = [{"detector_score": 0.8, "true_speed": 12.3}]
    with pytest.raises(LeakageViolation):
        lint_feature_frame(rows_bad)
