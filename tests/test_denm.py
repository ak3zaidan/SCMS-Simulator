"""DECENTRALIZED EVENT MESSAGE (DENM) layer: benign event messages + the FakeHazard attack.

BACKGROUND. Every broadcast so far is a periodic CAM/beacon (position + kinematics). Real V2X also
has DENMs -- event-triggered messages announcing a road hazard (emergency electronic brake light,
stationary vehicle, ...). This module adds an OPT-IN, DEFAULT-OFF DENM layer:

  * BENIGN DENMs: a vehicle that experiences a REAL trigger (a hard deceleration to a near-stop, or
    being stationary) occasionally broadcasts a signed DENM announcing that real event, at rate
    ``denm_rate`` (expected DENMs / vehicle / 100 s). The announced event corroborates the sender's
    own low claimed speed.
  * MALICIOUS DENMs: the OPT-IN "FakeHazard" attack -- a vehicle that drives HONESTLY on its CAMs but
    emits PHANTOM DENMs announcing a hazard (emergency brake) its own kinematics do not back (it is
    cruising). Excluded from the default catalog, so the default dataset is byte-identical.
  * DETECTOR "denmPlausibility": a receiver checks whether a received DENM corroborates the sender's
    own observed kinematics. A brake/stationary hazard whose sender still CLAIMS a high speed is
    implausible -> the detector fires. Benign (slow/stopped) DENMs stay below the threshold, so it
    NEVER fires on them -> no false revocations, while a persistent fake-DENM attacker is revocable.

Guarantees exercised here:
  * ``denm_rate=0`` (the default) is BYTE-IDENTICAL: no DENM is built, no DENM RNG is drawn.
  * FakeHazard is OPT-IN only (not in the default round-robin) -> the default golden is byte-identical.
  * benign DENMs are emitted + corroborate; denmPlausibility never false-flags them (precision stays
    high, zero benign-DENM false flags).
  * fake DENMs trip denmPlausibility, the MA revokes a meaningful fraction (recall > 0), and the
    attack folds to the new "event" family.
  * leakage firewall: the real/fake flag is ORACLE-only (gt_denm); no forbidden key reaches MA files
    or feature tables; ``n_denms_sent`` + ``detnorm_denmPlausibility`` are leakage-safe features.
  * determinism: a DENM run (benign + fake) is byte-identical across two runs.
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
                                            IDENTITY_SPOOF_ATTACKS, KNOWN_ATTACK_TYPES)

# The published default golden digest (see the determinism contract). The DENM layer is OPT-IN and
# DEFAULT-OFF (denm_rate defaults to 0.0, FakeHazard is not in the default catalog), so the default
# run MUST stay byte-identical to this value.
GOLDEN = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"

ATTACK = "FakeHazard"
DETECTOR = "denmPlausibility"

# Shared geometry (matches the other suites' conventions). Benign: default-catalog attackers + benign
# DENMs, with traffic lights so vehicles genuinely stop -> real triggers. Fake: FakeHazard-only.
_BENIGN = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
               grid_w=6, grid_h=6, attacker_pct=0.12, faulty_pct=0.02, traffic_lights=True,
               chan_capacity=400, radio_range_m=250.0, denm_rate=60.0)
_FAKE = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
             grid_w=6, grid_h=6, attacker_pct=0.25, chan_capacity=400, radio_range_m=250.0,
             denm_rate=60.0)


def _jsonl(p):
    p = pathlib.Path(p)
    return [json.loads(ln) for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()] \
        if p.exists() else []


# --------------------------------------------------------------------------- #
# 1. opt-in invariant + default golden byte-identical
# --------------------------------------------------------------------------- #
def test_fakehazard_is_opt_in_only():
    """FakeHazard is a KNOWN (renderable) type but NOT part of the default round-robin catalog,
    exactly like the combined / identity-spoof families -- so it cannot perturb the default dataset."""
    assert ATTACK in DENM_ATTACKS
    assert ATTACK in KNOWN_ATTACK_TYPES
    assert ATTACK not in ATTACK_CATALOG
    assert ATTACK not in COMBINED_ATTACKS
    assert ATTACK not in IDENTITY_SPOOF_ATTACKS
    assert PipelineConfig().attack_types == ATTACK_CATALOG      # default selection is the frozen catalog
    assert PipelineConfig().denm_rate == 0.0                    # DENM layer defaults OFF


def test_default_golden_unchanged(tmp_path):
    """Adding the opt-in DENM layer + FakeHazard + denmPlausibility must not perturb the DEFAULT path:
    byte-identical to the published golden digest."""
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == GOLDEN


def test_denm_rate_zero_is_byte_identical_to_default(tmp_path):
    """Explicit denm_rate=0.0 == the default: no DENMs, no RNG change, no DENM files, byte-identical."""
    a = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "a")))
    b = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    denm_rate=0.0, out_dir=str(tmp_path / "b")))
    assert a.data_digest == b.data_digest == GOLDEN
    # no DENM files are written on the default (layer-off) path
    assert not (pathlib.Path(tmp_path / "b") / "ma" / "ma_denm_log.jsonl").exists()
    assert not (pathlib.Path(tmp_path / "b") / "ground_truth" / "gt_denm_emissions.jsonl").exists()


def test_default_run_never_selects_fakehazard(tmp_path):
    """A DEFAULT-selection run's attacker types never include FakeHazard, and (no DENM layer) writes
    no DENM files."""
    out = str(tmp_path / "d")
    run_pipeline(PipelineConfig(out_dir=out, **{k: v for k, v in _FAKE.items() if k != "denm_rate"}))
    present = {a["attack_type"] for a in _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")}
    assert ATTACK not in present
    assert present <= set(ATTACK_CATALOG)
    assert not (pathlib.Path(out) / "ground_truth" / "gt_denm_emissions.jsonl").exists()


def test_denm_rate_must_be_non_negative():
    with pytest.raises(ValueError):
        run_pipeline(PipelineConfig(denm_rate=-1.0, out_dir="unused"))


# --------------------------------------------------------------------------- #
# 2. benign DENMs: emitted, corroborate, and NEVER false-flagged
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def benign_run(tmp_path_factory):
    out = str(tmp_path_factory.mktemp("denm_benign"))
    run_pipeline(PipelineConfig(out_dir=out, **_BENIGN))
    return out


def test_benign_denms_are_emitted_and_corroborate(benign_run):
    """With denm_rate>0 and NO fake-hazard attackers, real DENMs are emitted (visible in the ORACLE
    gt_denm stream and the MA-visible ma_denm_log), and they all CORROBORATE -- every observed DENM is
    plausible (claimed sender speed below the firing threshold)."""
    gd = _jsonl(pathlib.Path(benign_run) / "ground_truth" / "gt_denm_emissions.jsonl")
    log = _jsonl(pathlib.Path(benign_run) / "ma" / "ma_denm_log.jsonl")
    assert gd and log, "benign DENMs should be emitted and observed"
    real = [x for x in gd if not x["is_fake"]]
    assert len(real) >= 20, f"expected many benign DENMs, got {len(real)}"
    assert all(not x["is_fake"] for x in gd), "no fake DENMs without a FakeHazard attacker"
    # corroboration: no observed DENM is implausible (would need a cruising sender announcing a brake)
    assert all(d["denm_plausibility"] < 1.0 for d in log), \
        "benign DENMs must corroborate (plausibility below the firing threshold)"
    # the events announced are the benign triggers
    assert {x["event_type"] for x in gd} <= {"emergencyElectronicBrakeLight", "stationaryVehicle"}


def test_benign_denms_cause_no_false_flags_or_revocations(benign_run, capsys):
    """denmPlausibility must NOT fire on benign DENMs -> zero benign-DENM false flags and no benign
    vehicle revoked through it; revocation precision stays high."""
    reports = _jsonl(pathlib.Path(benign_run) / "ma" / "ma_reports.jsonl")
    denm_flags = [r for r in reports if DETECTOR in (r.get("reason_codes") or [])]
    s = V.validate(benign_run)[0]
    with capsys.disabled():
        print(f"\n[DENM benign] real_DENMs="
              f"{sum(1 for x in _jsonl(pathlib.Path(benign_run) / 'ground_truth' / 'gt_denm_emissions.jsonl') if not x['is_fake'])} "
              f"denmPlausibility_false_flags={len(denm_flags)} precision={s['precision']} "
              f"recall={s['recall']} revoked={s['revoked']} leakage={s.get('leakage_violations')}")
    assert denm_flags == [], f"denmPlausibility must not fire on benign DENMs, got {len(denm_flags)}"
    assert s["precision"] >= 0.9, s["precision"]
    assert s.get("leakage_violations", 0) == 0


# --------------------------------------------------------------------------- #
# 3. fake DENMs: FakeHazard attackers are caught + revoked
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def fake_run(tmp_path_factory):
    out = str(tmp_path_factory.mktemp("denm_fake"))
    run_pipeline(PipelineConfig(attack_types=(ATTACK,), out_dir=out, **_FAKE))
    return out


def _analyze(out):
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
    return {"types": {a["attack_type"] for a in gt}, "att_ids": att_ids, "idmap": idmap,
            "reports": reports, "recall": len(detected) / max(1, len(att_ids)),
            "revoked_atts": {x["true_vehicle_id"] for x in rev} & att_ids}


def test_fakehazard_emits_phantom_denms_and_is_caught(fake_run, capsys):
    """Attackers use the type; they emit PHANTOM DENMs (fake in the oracle) while their CAMs stay
    honest; denmPlausibility is what fires on them; and a MEANINGFUL fraction is revoked (recall>0)."""
    a = _analyze(fake_run)
    assert a["types"] == {ATTACK}
    assert len(a["att_ids"]) >= 5, "attacker population should be non-trivial"

    gd = _jsonl(pathlib.Path(fake_run) / "ground_truth" / "gt_denm_emissions.jsonl")
    fake = [x for x in gd if x["is_fake"]]
    assert fake, "FakeHazard attackers must emit phantom DENMs"
    assert all(x["true_vehicle_id"] in a["att_ids"] for x in fake), "fake DENMs come from attackers"

    # denmPlausibility is the detector that catches them
    att_reports = [r for r in a["reports"] if a["idmap"].get(r["subject_cert_digest"]) in a["att_ids"]]
    reasons = collections.Counter()
    for r in att_reports:
        reasons.update(r.get("reason_codes") or [])
    with capsys.disabled():
        print(f"\n[DENM fake] attackers={len(a['att_ids'])} fake_DENMs={len(fake)} "
              f"denmPlausibility_reports={reasons.get(DETECTOR, 0)} recall={a['recall']:.3f} "
              f"revoked_attackers={len(a['revoked_atts'])}")
    assert reasons.get(DETECTOR, 0) > 0, f"denmPlausibility must fire on attackers: {dict(reasons)}"
    assert a["recall"] > 0.0, f"recall on FakeHazard attackers must be > 0 (got {a['recall']:.3f})"


def test_fakehazard_honest_cams_only_denm_falsified(fake_run):
    """FakeHazard drives HONESTLY -- the falsification is the DENM, not the CAM. So the attackers'
    caught reports are denmPlausibility (the motion/heading detectors don't do the catching), and the
    attackers still get an onset stamped (the phantom DENM is their onset)."""
    a = _analyze(fake_run)
    att_reports = [r for r in a["reports"] if a["idmap"].get(r["subject_cert_digest"]) in a["att_ids"]]
    reasons = collections.Counter(c for r in att_reports for c in (r.get("reason_codes") or []))
    # denmPlausibility dominates the correct evidence against these attackers
    assert reasons.get(DETECTOR, 0) >= max((v for k, v in reasons.items() if k != DETECTOR), default=0)
    gt = _jsonl(pathlib.Path(fake_run) / "ground_truth" / "gt_attacks.jsonl")
    assert sum(1 for x in gt if x.get("attack_onset_time") is not None) >= 0.5 * len(gt)


def test_fakehazard_precision_stays_high(fake_run):
    """The denmPlausibility detector is specific (benign DENMs never trip it) -> adding the attack +
    detector does not wreck revocation precision."""
    s = V.validate(fake_run)[0]
    assert s["precision"] >= 0.9, s["precision"]
    assert s.get("leakage_violations", 0) == 0
    assert s["recall_by_family"].get("event", 0.0) > 0.0, s["recall_by_family"]


def test_attack_family_maps_to_event(fake_run):
    """featurize._ATTACK_FAMILY folds FakeHazard into the new 'event' family, and a run using it yields
    an 'event' label in vehicle_labels -> the leave-one-family-out fold exists."""
    assert featurize._ATTACK_FAMILY.get(ATTACK) == "event"
    featurize.build(fake_run)
    fams = set(pd.read_parquet(pathlib.Path(fake_run) / "ml" / "vehicle_labels.parquet")["attack_family"])
    assert "event" in fams, f"event fold missing from vehicle_labels ({fams})"


# --------------------------------------------------------------------------- #
# 4. leakage firewall: the real/fake flag is ORACLE-only
# --------------------------------------------------------------------------- #
def test_real_fake_flag_is_oracle_only(fake_run):
    """is_fake lives ONLY in the ORACLE gt_denm stream; NO forbidden key reaches MA files; and the
    MA-visible ma_denm_log carries counts/plausibility but never the real/fake flag."""
    # ORACLE side: is_fake present in gt_denm.
    assert any(x.get("is_fake") for x in
               _jsonl(pathlib.Path(fake_run) / "ground_truth" / "gt_denm_emissions.jsonl"))
    # MA side: no is_fake anywhere under ma/, and the leakage linter is clean.
    for f in (pathlib.Path(fake_run) / "ma").glob("*.jsonl"):
        for row in _jsonl(f):
            assert "is_fake" not in row, f
            assert find_forbidden_keys(row) == [], (f, row)
    # ma_denm_log carries MA-visible fields only
    log = _jsonl(pathlib.Path(fake_run) / "ma" / "ma_denm_log.jsonl")
    assert log and all("is_fake" not in d and "true_vehicle_id" not in d for d in log)


def test_denm_features_are_leakage_safe(fake_run):
    """detnorm_denmPlausibility (per-report fusion) and n_denms_sent / n_denms_implausible (per-subject
    counts) are leakage-safe features; the real/fake flag never appears; and the whole MA-visible
    dataset passes validate()'s leakage scan."""
    featurize.build(fake_run)
    ml = pathlib.Path(fake_run) / "ml"
    rf = pd.read_csv(ml / "report_features.csv")
    assert "detnorm_denmPlausibility" in rf.columns
    assert find_forbidden_keys({c: 0 for c in rf.columns if c not in ("split", "time_split")}) == []
    for name in ("subject_features", "vehicle_features", "vehicle_features_ma"):
        df = pd.read_csv(ml / f"{name}.csv")
        assert "n_denms_sent" in df.columns and "n_denms_implausible" in df.columns, name
        assert "is_fake" not in df.columns, name
        assert find_forbidden_keys({c: 0 for c in df.columns if c not in ("split", "time_split")}) == [], name
    # schema.json advertises the fusion feature (with a description) and the count features
    schema = json.loads((ml / "schema.json").read_text())
    rfs = {c["name"]: c for c in schema["report_features"]}
    assert rfs["detnorm_denmPlausibility"]["kind"] == "fusion_feature"
    assert rfs["detnorm_denmPlausibility"].get("desc")
    sfs = {c["name"]: c["kind"] for c in schema["subject_features"]}
    assert sfs.get("n_denms_sent") == "feature"
    # the whole MA-visible dataset is leakage-free
    assert V.validate(fake_run)[0].get("leakage_violations", 0) == 0


# --------------------------------------------------------------------------- #
# 5. reachability via the other opt-in selectors + determinism
# --------------------------------------------------------------------------- #
def test_reachable_via_attack_mix_and_attack_type(tmp_path):
    """The opt-in attack is reachable through attack_mix and the single attack_type narrowing selector,
    not only attack_types."""
    common = {k: v for k, v in _FAKE.items()}
    mix = str(tmp_path / "mix")
    run_pipeline(PipelineConfig(attack_mix=f"{ATTACK}:1.0", out_dir=mix, **common))
    assert {a["attack_type"] for a in _jsonl(pathlib.Path(mix) / "ground_truth" / "gt_attacks.jsonl")} \
        == {ATTACK}
    single = str(tmp_path / "single")
    run_pipeline(PipelineConfig(attack_type=ATTACK, out_dir=single, **common))
    assert {a["attack_type"] for a in _jsonl(pathlib.Path(single) / "ground_truth" / "gt_attacks.jsonl")} \
        == {ATTACK}


def test_denm_run_benign_and_fake_is_deterministic(tmp_path):
    """A run carrying BOTH benign DENMs (denm_rate>0) AND FakeHazard attackers -> byte-identical
    data_digest across two runs (per-vehicle string-keyed DENM rng only)."""
    def dig(tag):
        return run_pipeline(PipelineConfig(attack_types=(ATTACK,), traffic_lights=True,
                                           out_dir=str(tmp_path / tag), **_FAKE)).data_digest
    assert dig("a") == dig("b")
