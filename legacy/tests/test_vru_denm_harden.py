"""Hardening two SILENT FALSE-NEGATIVE gaps a security audit found in the VRU/DENM features.

GAP #6 (VRU-impersonation, SLOW variant). A receiver GATES OFF mapOffRoad + every vehicle-kinematic
detector for ANY beacon that self-declares station_type="vru". The original vruImpersonation detector
only fired when the CLAIMED speed reached the cyclist bound (VRU_MAX_PLAUSIBLE_SPEED_MPS = 10), so a
SLOW impersonator (claimed speed < 10) that declared vru AND falsified its position dodged every
motion detector AND stayed under the speed bound -> never revoked. The fix broadens vruImpersonation
with a POSITION-plausibility arm: a genuine VRU moves smoothly at ~vru speed, so the displacement of
its claimed position since the lagged reference implies at most a VRU-grade speed; a declared-VRU
whose claimed position TELEPORTS implies a speed far above the VRU bound even while it claims a slow
speed. The opt-in "VruPositionSpoof" attack exercises the gap (it claims a VRU-plausible slow speed
while teleporting its position). The tolerance is the VRU bound over the interval PLUS the broadcast
confidence PLUS a full multipath-outlier magnitude, so GNSS jitter on a real VRU can NEVER trip it.

GAP #5 (FakeHazard recall). denmPlausibility only fired on a brake/stationary DENM whose CLAIMED
sender speed exceeded DENM_IMPLAUSIBLE_SPEED_MPS (6), ignoring event_type. A FakeHazard sender that
emits phantom brake DENMs while legitimately SLOW (claimed <= 6, e.g. crawling in congestion) was
never flagged. The fix makes the bound EVENT-TYPE aware: a genuine emergencyElectronicBrakeLight
sender has actually braked to a near stop (the benign trigger fires only at speed <=
DENM_BENIGN_MAX_SPEED_MPS, and the claimed speed is the noise-free true speed), so a brake DENM whose
sender is STILL MOVING NORMALLY (claimed speed above DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS, which sits just
above the benign post-brake bound) contradicts the very event it announces -- even below the generic
6 m/s line. Benign brake DENMs are <= the benign bound with margin, so they are NEVER flagged.

Guarantees exercised here:
  * the DEFAULT golden digest is byte-identical (both additions are OPT-IN; the default path is untouched).
  * GAP #6: a SLOW (< 10 m/s claimed) declared-VRU position-falsifying attacker IS now caught + revoked
    (recall > 0), while a benign-VRU run (vru_pct>0, no attackers) has 0 VRUs flagged by vruImpersonation
    and 0 VRUs revoked.
  * GAP #5: a FakeHazard sender that stays slow (claimed <= 6) is now flagged by denmPlausibility
    (recall > 0 in a congested/slow scenario), while benign DENMs are NEVER flagged (0 false flags) and
    a benign congested run keeps precision high.
  * determinism: the new attack/detector configs are byte-identical run-to-run.
  * leakage: no oracle key reaches the new detector/feature (find_forbidden_keys clean).
"""

import collections
import json
import pathlib

import pandas as pd
import pytest

from scms_sim_ref.datagen import featurize
from scms_sim_ref.datagen import validate as V
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import (ATTACK_CATALOG, COMBINED_ATTACKS, DENM_ATTACKS,
                                            DENM_BENIGN_MAX_SPEED_MPS,
                                            DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS,
                                            DENM_IMPLAUSIBLE_SPEED_MPS, IDENTITY_SPOOF_ATTACKS,
                                            KNOWN_ATTACK_TYPES, VRU_MAX_PLAUSIBLE_SPEED_MPS)

# The published default golden digest (see the determinism contract). Both hardening additions are
# OPT-IN, so the default run MUST stay byte-identical to this value.
GOLDEN = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"

PS_ATTACK = "VruPositionSpoof"          # GAP #6: slow, position-falsifying VRU impersonator
VRU_DETECTOR = "vruImpersonation"
FH_ATTACK = "FakeHazard"                # GAP #5: phantom-brake attacker (slow, in congestion)
DENM_DETECTOR = "denmPlausibility"

# GAP #6 shared geometry (mirrors tests/test_vru_spoofing.py): attackers + genuine VRUs present so the
# detector must DISCRIMINATE real vs fake VRUs.
_PS = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
           grid_w=6, grid_h=6, attacker_pct=0.25, vru_pct=0.2,
           chan_capacity=400, radio_range_m=250.0)
# GAP #6 benign-VRU run: VRUs present, NO attackers -> the position arm must never fire on a real VRU.
_BENIGN_VRU = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
                   grid_w=6, grid_h=6, attacker_pct=0.0, vru_pct=0.25,
                   chan_capacity=400, radio_range_m=250.0)

# GAP #5 congested/slow geometry: a speed cap (~6 m/s) keeps EVERY vehicle slow, so a FakeHazard
# attacker's phantom brake DENMs carry a low (<= 6) claimed speed -- exactly the gap the old bound
# missed -- without the extreme density that would swamp the CAM detectors with congestion FPs.
_FAKE_CONG = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
                  grid_w=6, grid_h=6, local_speed_mps=6.0, arterial_every=2, arterial_speed_mps=6.0,
                  traffic_lights=True, attacker_pct=0.25, chan_capacity=400, radio_range_m=250.0)
# GAP #5 benign congested run: same slow geometry, DEFAULT-catalog attackers + benign DENMs.
_BENIGN_CONG = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
                    grid_w=6, grid_h=6, local_speed_mps=6.0, arterial_every=2, arterial_speed_mps=6.0,
                    traffic_lights=True, attacker_pct=0.12, faulty_pct=0.02,
                    chan_capacity=400, radio_range_m=250.0, denm_rate=60.0)


def _jsonl(p):
    p = pathlib.Path(p)
    return [json.loads(ln) for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()] \
        if p.exists() else []


def _analyze(out):
    """Common recall/labelling read-out shared by the GAP #6 and GAP #5 analyses."""
    gv = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_vehicle.jsonl")
    vru_ids = {v["true_vehicle_id"] for v in gv if v.get("is_vru")}
    gt = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")
    att_ids = {a["true_vehicle_id"] for a in gt}
    idmap = {m["pseudonym_cert_digest"]: m["true_vehicle_id"]
             for m in _jsonl(pathlib.Path(out) / "ground_truth" / "gt_identity_map.jsonl")}
    reports = _jsonl(pathlib.Path(out) / "ma" / "ma_reports.jsonl")
    labels = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_report_labels.jsonl")
    corr = {l["report_id"]: l["report_correctness"] for l in labels}
    subj = {l["report_id"]: l["subject_true_id"] for l in labels}
    detected = {subj[r["report_id"]] for r in reports
                if subj.get(r["report_id"]) in att_ids and corr.get(r["report_id"]) == "correct"}
    rev = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_linkage_revocation.jsonl")
    revoked = {x["true_vehicle_id"] for x in rev}
    return {
        "types": {a["attack_type"] for a in gt},
        "vru_ids": vru_ids, "att_ids": att_ids, "idmap": idmap, "reports": reports,
        "recall": len(detected) / max(1, len(att_ids)),
        "revoked": revoked,
        "revoked_atts": revoked & att_ids,
        "revoked_vrus": [x for x in revoked if x in vru_ids],
    }


# --------------------------------------------------------------------------- #
# 0. opt-in invariants + default golden byte-identical
# --------------------------------------------------------------------------- #
def test_new_attack_is_opt_in_only():
    """VruPositionSpoof is a KNOWN (renderable) identity-spoof type but NOT part of the default
    round-robin catalog -- so it cannot perturb the default dataset."""
    assert PS_ATTACK in IDENTITY_SPOOF_ATTACKS
    assert PS_ATTACK in KNOWN_ATTACK_TYPES
    assert PS_ATTACK not in ATTACK_CATALOG
    assert PS_ATTACK not in COMBINED_ATTACKS
    assert PS_ATTACK not in DENM_ATTACKS
    assert PipelineConfig().attack_types == ATTACK_CATALOG      # default selection is the frozen catalog
    # sanity on the new event-type bound: it sits between the benign post-brake bound and the generic
    # bound, so real brakes (<= benign bound) never trip it while still-moving phantoms do.
    assert DENM_BENIGN_MAX_SPEED_MPS < DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS < DENM_IMPLAUSIBLE_SPEED_MPS


def test_default_golden_unchanged(tmp_path):
    """Both hardening additions are opt-in; the DEFAULT path must stay byte-identical to the golden."""
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == GOLDEN


def test_default_run_never_selects_new_attack(tmp_path):
    """A DEFAULT-selection run's attacker types never include the opt-in position-spoof type."""
    out = str(tmp_path / "d")
    run_pipeline(PipelineConfig(out_dir=out, **{k: v for k, v in _PS.items() if k != "vru_pct"}))
    present = {a["attack_type"] for a in _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")}
    assert PS_ATTACK not in present
    assert present <= set(ATTACK_CATALOG)


# --------------------------------------------------------------------------- #
# 1. GAP #6: a SLOW, position-falsifying VRU impersonator is now caught + revoked
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def ps_run(tmp_path_factory):
    out = str(tmp_path_factory.mktemp("vru_pos_spoof"))
    run_pipeline(PipelineConfig(attack_types=(PS_ATTACK,), out_dir=out, **_PS))
    return out


def test_gap6_slow_position_spoofer_is_caught(ps_run, capsys):
    """The position-spoof attackers declare vru, CLAIM a slow speed (< the cyclist bound, so the speed
    arm alone could not catch them), yet vruImpersonation fires on them via the POSITION arm and the MA
    revokes a meaningful fraction (recall > 0)."""
    a = _analyze(ps_run)
    assert a["types"] == {PS_ATTACK}
    assert len(a["att_ids"]) >= 5 and len(a["vru_ids"]) >= 5      # both populations are non-trivial

    idmap, att_ids = a["idmap"], a["att_ids"]
    imp_reports = [r for r in a["reports"] if idmap.get(r["subject_cert_digest"]) in att_ids]
    assert imp_reports, "position-spoof impersonators should be reported"
    assert all(r.get("station_type") == "vru" for r in imp_reports), \
        "impersonator reports must carry the fraudulently-declared station_type=vru"

    # the attackers stay UNDER the speed bound WHILE ATTACKING: every FALSIFIED (actively-spoofing)
    # beacon claims a slow speed < the cyclist bound, so the original speed-only vruImpersonation arm
    # would have missed every one of them -- the position arm is what closes the gap. (Non-attacking
    # beacons broadcast the honest vehicle speed and are excluded via the MA-invisible oracle `falsified`
    # flag, used here for test bookkeeping only.)
    em = _jsonl(pathlib.Path(ps_run) / "ground_truth" / "gt_emissions_sample.jsonl")
    att_speeds = [e["claimed_speed"] for e in em if e["true_vehicle_id"] in att_ids and e.get("falsified")]
    assert att_speeds, "expected sampled falsified attacker beacons"
    assert max(att_speeds) < VRU_MAX_PLAUSIBLE_SPEED_MPS, \
        f"the gap requires attackers UNDER the speed bound while spoofing; max claimed={max(att_speeds)}"

    # vruImpersonation (the broadened detector) is what catches them, and the MA can revoke them.
    reasons = collections.Counter()
    for r in imp_reports:
        reasons.update(r.get("reason_codes") or [])
    with capsys.disabled():
        print(f"\n[GAP#6 pos-spoof] attackers={len(att_ids)} max_claimed_speed={max(att_speeds):.2f} "
              f"vruImp_reports={reasons.get(VRU_DETECTOR, 0)} recall={a['recall']:.3f} "
              f"revoked_attackers={len(a['revoked_atts'])}")
    assert reasons.get(VRU_DETECTOR, 0) > 0, f"vruImpersonation must fire on pos-spoofers: {dict(reasons)}"
    assert a["recall"] > 0.0, f"recall on slow position-spoofers must be > 0 (got {a['recall']:.3f})"


def test_gap6_genuine_vrus_never_flagged_or_revoked(ps_run, capsys):
    """CRUCIAL: the broadened detector must still DISCRIMINATE -- no genuine VRU is ever the subject of
    a vruImpersonation report, and no genuine VRU is revoked. (Real VRUs move at ~vru speed with small
    steps + GNSS noise; the position arm's tolerance = the VRU bound over the interval + the broadcast
    confidence + a full outlier magnitude, so jitter/outliers can never push a real VRU to fire.)"""
    a = _analyze(ps_run)
    assert a["vru_ids"], "genuine VRUs must be present so the detector has to discriminate"
    flagged = [r for r in a["reports"]
               if a["idmap"].get(r["subject_cert_digest"]) in a["vru_ids"]
               and VRU_DETECTOR in (r.get("reason_codes") or [])]
    with capsys.disabled():
        print(f"\n[GAP#6 discrimination] genuine_VRUs={len(a['vru_ids'])} "
              f"vruImp_reports_on_VRUs={len(flagged)} revoked_VRUs={len(a['revoked_vrus'])}")
    assert flagged == [], f"vruImpersonation must NOT fire on genuine VRUs, got {len(flagged)}"
    assert a["revoked_vrus"] == [], f"genuine VRUs must never be revoked, got {a['revoked_vrus']}"


def test_gap6_benign_vru_run_is_clean(tmp_path, capsys):
    """A benign-VRU run (vru_pct>0, NO attackers) has 0 VRUs flagged by vruImpersonation and 0 VRUs
    revoked. (Baseline CAM detectors may still falsely revoke a few benign VEHICLES -- an effect that
    predates this change; the invariant this test guards is that the VRU detector never fires on a real
    VRU and no VRU is revoked.)"""
    out = str(tmp_path / "bvru")
    run_pipeline(PipelineConfig(out_dir=out, **_BENIGN_VRU))
    a = _analyze(out)
    assert a["vru_ids"] and not a["att_ids"], "expected genuine VRUs and no attackers"
    vru_flags = [r for r in a["reports"] if VRU_DETECTOR in (r.get("reason_codes") or [])]
    with capsys.disabled():
        print(f"\n[GAP#6 benign VRU] genuine_VRUs={len(a['vru_ids'])} vruImp_reports={len(vru_flags)} "
              f"revoked_VRUs={len(a['revoked_vrus'])} revoked_total={len(a['revoked'])}")
    assert vru_flags == [], f"vruImpersonation must never fire in a benign VRU run, got {len(vru_flags)}"
    assert a["revoked_vrus"] == [], f"no genuine VRU may be revoked, got {a['revoked_vrus']}"


# --------------------------------------------------------------------------- #
# 2. GAP #5: a slow-in-congestion FakeHazard is now caught; benign DENMs never flagged
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def fake_cong_run(tmp_path_factory):
    out = str(tmp_path_factory.mktemp("denm_fake_cong"))
    run_pipeline(PipelineConfig(attack_types=(FH_ATTACK,), out_dir=out, **_FAKE_CONG))
    return out


@pytest.fixture(scope="module")
def benign_cong_run(tmp_path_factory):
    out = str(tmp_path_factory.mktemp("denm_benign_cong"))
    run_pipeline(PipelineConfig(out_dir=out, **_BENIGN_CONG))
    return out


def test_gap5_slow_fakehazard_is_caught(fake_cong_run, capsys):
    """Every phantom brake DENM here carries a SLOW claimed speed (<= the generic 6 m/s bound), so the
    original speed-only arm would have caught ZERO of them -- yet the event-type-aware bound flags them
    and the MA revokes a meaningful fraction (recall > 0)."""
    a = _analyze(fake_cong_run)
    assert a["types"] == {FH_ATTACK}
    assert len(a["att_ids"]) >= 5

    gd = _jsonl(pathlib.Path(fake_cong_run) / "ground_truth" / "gt_denm_emissions.jsonl")
    fake = [x for x in gd if x["is_fake"]]
    assert fake, "FakeHazard attackers must emit phantom DENMs"
    slow = [x for x in fake if x["sender_speed"] <= DENM_IMPLAUSIBLE_SPEED_MPS]
    band = [x for x in fake if DENM_BENIGN_MAX_SPEED_MPS < x["sender_speed"] <= DENM_IMPLAUSIBLE_SPEED_MPS]
    # the gap population: almost all phantoms are slow (the old > 6 arm would have missed them)...
    assert len(slow) >= 0.8 * len(fake), \
        f"scenario must exercise the SLOW gap: {len(slow)}/{len(fake)} phantoms <= {DENM_IMPLAUSIBLE_SPEED_MPS}"
    # ...and a meaningful number sit in the (benign-bound, 6] band caught ONLY by the new event bound.
    assert len(band) >= 20, f"expected phantoms in the new-bound catch band, got {len(band)}"

    att_reports = [r for r in a["reports"] if a["idmap"].get(r["subject_cert_digest"]) in a["att_ids"]]
    reasons = collections.Counter(c for r in att_reports for c in (r.get("reason_codes") or []))
    with capsys.disabled():
        print(f"\n[GAP#5 slow fake] attackers={len(a['att_ids'])} phantoms={len(fake)} "
              f"slow(<=6)={len(slow)} band(4,6]={len(band)} denmPlausibility={reasons.get(DENM_DETECTOR, 0)} "
              f"recall={a['recall']:.3f} revoked_attackers={len(a['revoked_atts'])}")
    assert reasons.get(DENM_DETECTOR, 0) > 0, f"denmPlausibility must fire on slow phantoms: {dict(reasons)}"
    # denmPlausibility is the dominant evidence (FakeHazard CAMs are honest; the DENM is the lie).
    assert reasons.get(DENM_DETECTOR, 0) >= max((v for k, v in reasons.items() if k != DENM_DETECTOR),
                                                default=0)
    assert a["recall"] > 0.0, f"recall on slow FakeHazard must be > 0 (got {a['recall']:.3f})"
    assert a["revoked_atts"], "the MA must revoke at least one slow FakeHazard attacker"


def test_gap5_fake_precision_stays_high(fake_cong_run):
    """The event-type bound is specific (benign brakes stay below it), so catching the slow phantoms
    does not wreck revocation precision, and the 'event' family recall is > 0."""
    s = V.validate(fake_cong_run)[0]
    assert s["precision"] >= 0.9, s["precision"]
    assert s.get("leakage_violations", 0) == 0
    assert s["recall_by_family"].get("event", 0.0) > 0.0, s["recall_by_family"]


def test_gap5_benign_denms_never_flagged(benign_cong_run, capsys):
    """In a benign congested run (benign DENMs, NO FakeHazard), denmPlausibility must NOT fire on any
    benign subject -> zero benign-DENM false flags, and revocation precision stays high."""
    gd = _jsonl(pathlib.Path(benign_cong_run) / "ground_truth" / "gt_denm_emissions.jsonl")
    assert gd and all(not x["is_fake"] for x in gd), "benign run must emit real (never fake) DENMs"
    real = [x for x in gd if not x["is_fake"]]
    assert len(real) >= 20, f"expected many benign DENMs, got {len(real)}"

    a = _analyze(benign_cong_run)
    denm_flags = [r for r in a["reports"] if DENM_DETECTOR in (r.get("reason_codes") or [])]
    on_benign = [r for r in denm_flags if a["idmap"].get(r["subject_cert_digest"]) not in a["att_ids"]]
    s = V.validate(benign_cong_run)[0]
    with capsys.disabled():
        print(f"\n[GAP#5 benign cong] real_DENMs={len(real)} denmPlausibility_flags={len(denm_flags)} "
              f"on_benign={len(on_benign)} precision={s['precision']} leakage={s.get('leakage_violations')}")
    assert on_benign == [], f"denmPlausibility must never fire on a benign subject, got {len(on_benign)}"
    assert s["precision"] >= 0.9, s["precision"]
    assert s.get("leakage_violations", 0) == 0


# --------------------------------------------------------------------------- #
# 3. leakage firewall + attack-family fold
# --------------------------------------------------------------------------- #
def test_new_detectors_and_features_carry_no_oracle_identity(ps_run, fake_cong_run):
    """Neither hardening path leaks an oracle key: MA files pass the linter; is_vru / is_fake never
    appear; detnorm_vruImpersonation and detnorm_denmPlausibility are (leakage-safe) fusion features;
    and VruPositionSpoof folds into the 'identity' family."""
    for run in (ps_run, fake_cong_run):
        for f in (pathlib.Path(run) / "ma").glob("*.jsonl"):
            for row in _jsonl(f):
                assert "is_vru" not in row and "is_fake" not in row, f
                assert find_forbidden_keys(row) == [], (f, row)

    featurize.build(ps_run)
    rf = pd.read_csv(pathlib.Path(ps_run) / "ml" / "report_features.csv")
    assert "detnorm_vruImpersonation" in rf.columns
    assert find_forbidden_keys({c: 0 for c in rf.columns if c not in ("split", "time_split")}) == []

    featurize.build(fake_cong_run)
    rf2 = pd.read_csv(pathlib.Path(fake_cong_run) / "ml" / "report_features.csv")
    assert "detnorm_denmPlausibility" in rf2.columns
    assert find_forbidden_keys({c: 0 for c in rf2.columns if c not in ("split", "time_split")}) == []
    for name in ("subject_features", "vehicle_features", "vehicle_features_ma"):
        cols = pd.read_csv(pathlib.Path(fake_cong_run) / "ml" / f"{name}.csv").columns
        assert find_forbidden_keys({c: 0 for c in cols if c not in ("split", "time_split")}) == [], name

    assert featurize._ATTACK_FAMILY.get(PS_ATTACK) == "identity"
    fams = set(pd.read_parquet(pathlib.Path(ps_run) / "ml" / "vehicle_labels.parquet")["attack_family"])
    assert "identity" in fams, f"identity fold missing from vehicle_labels ({fams})"


# --------------------------------------------------------------------------- #
# 4. determinism of the new attack/detector configs
# --------------------------------------------------------------------------- #
def test_position_spoof_run_is_deterministic(tmp_path):
    """Same seed + config with the position-spoof attack -> byte-identical data_digest."""
    def dig(tag):
        return run_pipeline(PipelineConfig(attack_types=(PS_ATTACK,), out_dir=str(tmp_path / tag),
                                           **{**_PS, "duration_s": 80})).data_digest
    assert dig("a") == dig("b")


def test_fake_congested_run_is_deterministic(tmp_path):
    """Same seed + config with the congested FakeHazard scenario (event-type bound in play) ->
    byte-identical data_digest."""
    def dig(tag):
        return run_pipeline(PipelineConfig(attack_types=(FH_ATTACK,), out_dir=str(tmp_path / tag),
                                           **{**_FAKE_CONG, "duration_s": 80})).data_digest
    assert dig("a") == dig("b")
