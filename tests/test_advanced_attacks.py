"""Tests for pseudonym rotation, collusion / false accusation, and Sybil-ghost attacks."""

import collections
import json

from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def test_rotation_gives_multiple_pseudonyms_but_vehicle_level_revocation(tmp_path):
    out = str(tmp_path / "run")
    cfg = PipelineConfig(seed=5, n_vehicles=40, n_steps=100, attacker_pct=0.15,
                         rotate_period_s=30.0, faulty_pct=0.0, out_dir=out)
    res = run_pipeline(cfg)
    idm = _jsonl(tmp_path / "run" / "ground_truth" / "gt_identity_map.jsonl")
    per_vehicle = collections.Counter(m["true_vehicle_id"] for m in idm)
    assert max(per_vehicle.values()) >= 3, "rotation should give several pseudonyms per vehicle"
    # a revoked vehicle's pseudonyms are ALL revoked (vehicle-level via one CRL entry)
    status = _jsonl(tmp_path / "run" / "ma" / "ma_cert_status.jsonl")
    d2v = {m["pseudonym_cert_digest"]: m["true_vehicle_id"] for m in idm}
    revoked_vehicles = {d2v[s["cert_digest"]] for s in status
                        if s.get("crl_status") == "revoked" and s["cert_digest"] in d2v}
    assert len(revoked_vehicles) == res.n_revoked


def test_collusion_frames_victims_and_defense_reduces_it(tmp_path):
    def framings(defense):
        out = str(tmp_path / f"run_{defense}")
        cfg = PipelineConfig(seed=5, n_vehicles=60, n_steps=80, attacker_pct=0.15,
                             collude_pct=0.6, victim_pct=0.15, ma_defense=defense,
                             faulty_pct=0.0, out_dir=out)
        run_pipeline(cfg)
        labels = [r["report_correctness"]
                  for r in _jsonl(tmp_path / f"run_{defense}" / "ground_truth" / "gt_report_labels.jsonl")]
        rev = _jsonl(tmp_path / f"run_{defense}" / "ground_truth" / "gt_linkage_revocation.jsonl")
        assert "malicious_false_report" in labels, "colluders must file false reports"
        return sum(1 for r in rev if not r["should_have_been_revoked"])   # framed benign victims

    framed_off = framings(False)
    framed_on = framings(True)
    assert framed_off >= 1, "without defense, collusion should frame at least one victim"
    assert framed_on < framed_off, "the trusted-reporter defense must reduce framings"


def test_sybil_ghosts_are_co_located_and_flagged(tmp_path):
    out = str(tmp_path / "run")
    cfg = PipelineConfig(seed=3, n_vehicles=40, n_steps=60, attacker_ids=(5,),
                         attack_type="Sybil", attack_types=("Sybil",), sybil_ghosts=6,
                         faulty_pct=0.0, out_dir=out)
    run_pipeline(cfg)
    idm = _jsonl(tmp_path / "run" / "ground_truth" / "gt_identity_map.jsonl")
    per = collections.Counter(m["true_vehicle_id"] for m in idm)
    assert per["veh_005"] == 7, "Sybil attacker = 1 real + 6 ghost identities"
    reasons = collections.Counter()
    for r in _jsonl(tmp_path / "run" / "ma" / "ma_reports.jsonl"):
        reasons.update(r.get("reason_codes", []))
    assert reasons["sybilCoLocation"] > 0, "co-located ghosts must trigger sybilCoLocation"


def test_invalid_signature_attack_is_caught_by_signature_verification(tmp_path):
    """Forged/tampered messages (sig_valid=False) must trip the signatureVerification detector and
    be reported with sig_valid=False; content plausibility is moot for an unverifiable message."""
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, n_vehicles=40, n_steps=60, attacker_ids=(5, 9, 14),
                                attack_type="InvalidSignature", attack_types=("InvalidSignature",),
                                faulty_pct=0.0, seed=4))
    reasons = collections.Counter()
    sig_false = 0
    for ln in open(out + "/ma/ma_reports.jsonl", encoding="utf-8"):
        r = json.loads(ln)
        reasons.update(r.get("reason_codes", []))
        if r.get("sig_valid") is False:
            sig_false += 1
    assert reasons["signatureVerification"] > 0
    assert sig_false > 0, "invalid-signature reports must carry sig_valid=False"
    rev = _jsonl(tmp_path / "run" / "ground_truth" / "gt_linkage_revocation.jsonl")
    assert all(r["should_have_been_revoked"] for r in rev), "no benign should be revoked here"


def test_expired_and_not_yet_valid_certs_are_caught(tmp_path):
    """Using a cert outside its validity window must trip the certValidity detector."""
    for atk in ("ExpiredCert", "NotYetValid"):
        out = str(tmp_path / atk)
        run_pipeline(PipelineConfig(out_dir=out, n_vehicles=36, n_steps=60, attacker_ids=(5, 11),
                                    attack_type=atk, attack_types=(atk,), faulty_pct=0.0, seed=4))
        reasons = collections.Counter()
        for ln in open(out + "/ma/ma_reports.jsonl", encoding="utf-8"):
            reasons.update(json.loads(ln).get("reason_codes", []))
        assert reasons["certValidity"] > 0, f"{atk} should trigger certValidity"
        rev = _jsonl(tmp_path / atk / "ground_truth" / "gt_linkage_revocation.jsonl")
        assert rev and all(r["should_have_been_revoked"] for r in rev), atk


def test_attack_intensity_controls_difficulty(tmp_path):
    """Lower attack_intensity -> subtler falsification -> harder to catch (lower recall)."""
    from scms_sim_ref.datagen import validate as V

    def recall(k):
        out = str(tmp_path / f"i{k}")
        run_pipeline(PipelineConfig(out_dir=out, traffic_flow=True, road_network="grid",
                                    car_following=True, duration_s=160.0, arrival_rate=2.5, grid_w=6,
                                    grid_h=6, attacker_pct=0.2, attack_type="ConstPosOffset",
                                    attack_types=("ConstPosOffset",), attack_intensity=k,
                                    faulty_pct=0.0, radio_range_m=250.0, seed=6))
        return V.validate(out)[0]["recall"]

    assert recall(0.3) < recall(1.5), "subtle attacks must be harder to detect than blatant ones"


def test_advanced_run_is_deterministic_and_leakage_free(tmp_path):
    kw = dict(seed=9, n_vehicles=50, n_steps=90, attacker_pct=0.2, rotate_period_s=40.0,
              collude_pct=0.5, victim_pct=0.1, faulty_pct=0.05)
    r1 = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **kw))
    r2 = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), **kw))
    assert r1.data_digest == r2.data_digest
    for name in ("ma_reports.jsonl", "ma_cert_status.jsonl", "ma_investigations.jsonl"):
        for row in _jsonl(tmp_path / "a" / "ma" / name):
            assert find_forbidden_keys(row) == [], f"{name} leaks: {find_forbidden_keys(row)}"


def _falsified_fraction(tmp_path, duty):
    out = str(tmp_path / f"run{duty}")
    cfg = PipelineConfig(seed=9, n_vehicles=60, n_steps=160, attacker_pct=0.3, faulty_pct=0.0,
                         attack_start=5.0, attack_end=160.0, attack_duty_cycle=duty,
                         attack_pulse_period_s=20.0, emit_sample_prob=1.0, out_dir=out)
    res = run_pipeline(cfg)
    em = _jsonl(tmp_path / f"run{duty}" / "ground_truth" / "gt_emissions_sample.jsonl")
    atk = [e for e in em if e["is_attacker"]]
    frac = sum(1 for e in atk if e["falsified"]) / max(1, len(atk))
    return frac, res.n_revoked


def test_pulsed_attack_falsifies_less_and_evades_more(tmp_path):
    """An intermittent (25% duty) attacker falsifies far fewer messages than a continuous one, and
    the sustained-evidence MA revokes fewer of them -> a genuine evasion / difficulty lever."""
    cont_frac, cont_rev = _falsified_fraction(tmp_path, 1.0)
    puls_frac, puls_rev = _falsified_fraction(tmp_path, 0.25)
    assert cont_frac > 0.8, cont_frac
    assert puls_frac < 0.5, f"25%% duty should falsify a minority of messages: {puls_frac}"
    assert puls_rev <= cont_rev, (puls_rev, cont_rev)


def test_duty_cycle_one_is_byte_identical_to_default(tmp_path):
    """duty_cycle=1.0 (default) must not perturb generation at all."""
    base = run_pipeline(PipelineConfig(seed=3, n_vehicles=30, n_steps=80, attacker_pct=0.2,
                                       out_dir=str(tmp_path / "a")))
    same = run_pipeline(PipelineConfig(seed=3, n_vehicles=30, n_steps=80, attacker_pct=0.2,
                                       attack_duty_cycle=1.0, attack_pulse_period_s=20.0,
                                       out_dir=str(tmp_path / "b")))
    assert base.data_digest == same.data_digest


def test_attack_delay_jitter_spreads_onset_and_is_byte_identical_at_zero(tmp_path):
    """Onset jitter diversifies attacker start times; jitter=0 is byte-identical to the default."""
    import statistics
    # byte-identical no-op at 0
    a = run_pipeline(PipelineConfig(seed=4, n_vehicles=30, n_steps=80, attacker_pct=0.3,
                                    out_dir=str(tmp_path / "a")))
    b = run_pipeline(PipelineConfig(seed=4, n_vehicles=30, n_steps=80, attacker_pct=0.3,
                                    attack_delay_jitter_s=0.0, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest

    # with jitter, attacker start_times are spread out (not all identical)
    run_pipeline(PipelineConfig(seed=4, n_vehicles=40, n_steps=80, attacker_pct=0.4,
                                attack_start=5.0, attack_delay_jitter_s=20.0,
                                out_dir=str(tmp_path / "j")))
    starts = [atk["start_time"] for atk in _jsonl(tmp_path / "j" / "ground_truth" / "gt_attacks.jsonl")]
    assert len(set(starts)) > 3 and statistics.pstdev(starts) > 1.0, starts
    assert min(starts) >= 5.0 and max(starts) <= 25.0 + 1e-6


def test_mixed_run_catches_credential_attacks(tmp_path):
    """In a realistic MIXED run (all attack types, no heavy collusion) the credential family
    (ExpiredCert/NotYetValid/InvalidSignature) is reliably revoked -- the practical case works even
    though a pathological pure-credential high-density run can evade the reporter-budget gate."""
    from scms_sim_ref.datagen import validate as V
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(seed=7, n_vehicles=120, n_steps=120, attacker_pct=0.3,
                                collude_pct=0.0, faulty_pct=0.05, out_dir=out))
    s, _ = V.validate(out)
    cred = s["recall_by_family"].get("credential")
    assert cred is not None and cred >= 0.6, f"mixed run should catch credential attacks: {cred}"
