"""VRU-impersonation attack + the vruImpersonation detector that catches it.

BACKGROUND. main gained benign VRU actors that SELF-DECLARE station_type="vru"; a receiver GATES OFF
the off-road + vehicle-kinematic detectors (mapOffRoad + positionSpeedInconsistency / positionJump /
headingInconsistency / constantPositionFrozen / implausibleAcceleration) for a self-declared VRU so
benign VRUs are not false-flagged. That left a gap: a malicious VEHICLE can broadcast
station_type="vru" to DODGE exactly those detectors. This module covers the OPT-IN "VruImpersonation"
attack -- a moving vehicle that fraudulently declares vru while otherwise driving honestly -- and the
"vruImpersonation" detector (declared VRU + vehicle-grade CLAIMED speed) that closes the gap.

Guarantees exercised here:
  * the DEFAULT golden digest is byte-identical (the attack is OPT-IN; not in the default round-robin).
  * an impersonation run: impersonators broadcast station_type="vru" (seen in ma_reports), the
    vruImpersonation detector fires on them, and the MA revokes a meaningful fraction (recall > 0).
  * genuine VRUs are NEVER flagged by vruImpersonation and are NEVER revoked -- the detector reads the
    noise-free CLAIMED speed, so slow real VRUs stay far below the cyclist-bounded threshold.
  * leakage firewall: no oracle key reaches MA reports / feature tables; attack_family folds to
    "identity".
  * determinism: an impersonation run is byte-identical across two runs.
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
from scms_sim_ref.mock_pipeline.run import (ATTACK_CATALOG, COMBINED_ATTACKS,
                                            IDENTITY_SPOOF_ATTACKS, KNOWN_ATTACK_TYPES)

# The published default golden digest (see the determinism contract). VruImpersonation is opt-in, so
# the default run MUST stay byte-identical to this value.
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 04ae9736f519... (default-config run; behaviour unchanged).
GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

ATTACK = "VruImpersonation"
DETECTOR = "vruImpersonation"

# A run big enough that many attackers use the type over many steps, WITH genuine VRUs present so the
# detector must discriminate real vs fake VRUs. seed/geometry match the other suites' conventions.
_COMMON = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=150, arrival_rate=1.5,
               grid_w=6, grid_h=6, attacker_pct=0.25, vru_pct=0.2,
               chan_capacity=400, radio_range_m=250.0)


def _jsonl(p):
    p = pathlib.Path(p)
    return [json.loads(ln) for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()] \
        if p.exists() else []


@pytest.fixture(scope="module")
def imp_run(tmp_path_factory):
    """One shared impersonation run (attack_types=(VruImpersonation,) + genuine VRUs). Module-scoped so
    the heavy simulation runs once for the several read-only assertions below."""
    out = str(tmp_path_factory.mktemp("vru_imp"))
    run_pipeline(PipelineConfig(attack_types=(ATTACK,), out_dir=out, **_COMMON))
    return out


def _analyze(out):
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
    return {
        "types": {a["attack_type"] for a in gt},
        "vru_ids": vru_ids, "att_ids": att_ids, "idmap": idmap,
        "reports": reports,
        "recall": len(detected) / max(1, len(att_ids)),
        "revoked_vrus": [x["true_vehicle_id"] for x in rev if x["true_vehicle_id"] in vru_ids],
    }


# --------------------------------------------------------------------------- #
# 1. opt-in invariant + default golden byte-identical
# --------------------------------------------------------------------------- #
def test_impersonation_is_opt_in_only():
    """VruImpersonation is a KNOWN (renderable) type but NOT part of the default round-robin catalog,
    exactly like the combined family -- so it cannot perturb the default dataset."""
    assert ATTACK in IDENTITY_SPOOF_ATTACKS
    assert ATTACK in KNOWN_ATTACK_TYPES
    assert ATTACK not in ATTACK_CATALOG
    assert ATTACK not in COMBINED_ATTACKS
    assert PipelineConfig().attack_types == ATTACK_CATALOG      # default selection is the frozen catalog


def test_default_golden_unchanged(tmp_path):
    """Adding the opt-in attack + its detector must not perturb the DEFAULT path: byte-identical."""
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == GOLDEN


def test_default_run_never_selects_impersonation(tmp_path):
    """A DEFAULT-selection run's attacker types never include VruImpersonation."""
    out = str(tmp_path / "d")
    run_pipeline(PipelineConfig(out_dir=out, **{k: v for k, v in _COMMON.items() if k != "vru_pct"}))
    present = {a["attack_type"] for a in _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")}
    assert ATTACK not in present
    assert present <= set(ATTACK_CATALOG)


# --------------------------------------------------------------------------- #
# 2. the attack: impersonators declare vru, the detector fires, the MA revokes some
# --------------------------------------------------------------------------- #
def test_impersonators_declare_vru_and_are_caught(imp_run):
    """Every attacker uses the type; its reports carry the fraudulently-declared station_type=vru; the
    vruImpersonation detector is what fires on them; and a MEANINGFUL fraction is revoked (recall>0)."""
    a = _analyze(imp_run)
    assert a["types"] == {ATTACK}
    assert len(a["att_ids"]) >= 5 and len(a["vru_ids"]) >= 5     # both populations are non-trivial

    idmap, att_ids = a["idmap"], a["att_ids"]
    imp_reports = [r for r in a["reports"] if idmap.get(r["subject_cert_digest"]) in att_ids]
    assert imp_reports, "impersonators should be reported"
    # the fraudulent self-declaration is MA-visible on the beacon / report
    assert all(r.get("station_type") == "vru" for r in imp_reports), \
        "impersonator reports must carry the declared station_type=vru"
    # vruImpersonation is the detector that catches them
    reasons = collections.Counter()
    for r in imp_reports:
        reasons.update(r.get("reason_codes") or [])
    assert reasons.get(DETECTOR, 0) > 0, f"vruImpersonation must fire on impersonators: {dict(reasons)}"
    # the MA can actually revoke impersonators via this evidence
    assert a["recall"] > 0.0, f"recall on impersonators must be > 0 (got {a['recall']:.3f})"


def test_gt_attacks_carry_onset_for_impersonation(imp_run):
    """The station-type spoof is treated as a falsification -> attackers get an onset stamped."""
    gt = _jsonl(pathlib.Path(imp_run) / "ground_truth" / "gt_attacks.jsonl")
    assert gt and sum(1 for a in gt if a.get("attack_onset_time") is not None) >= 0.5 * len(gt)


# --------------------------------------------------------------------------- #
# 3. CRUCIAL: genuine VRUs are never flagged by vruImpersonation and never revoked
# --------------------------------------------------------------------------- #
def test_genuine_vrus_never_flagged_or_revoked(imp_run, capsys):
    """The detector must DISCRIMINATE real vs fake VRUs: no genuine VRU is ever the subject of a
    vruImpersonation report, and no genuine VRU is revoked. (Slow real VRUs claim a few m/s, far below
    the cyclist-bounded threshold; the detector reads the noise-free claimed speed, so GNSS jitter
    cannot inflate it.)"""
    a = _analyze(imp_run)
    idmap, vru_ids = a["idmap"], a["vru_ids"]
    assert vru_ids, "genuine VRUs must be present so the detector has to discriminate"

    flagged = [r for r in a["reports"]
               if idmap.get(r["subject_cert_digest"]) in vru_ids
               and DETECTOR in (r.get("reason_codes") or [])]
    with capsys.disabled():
        print(f"\n[VRU-impersonation] attackers={len(a['att_ids'])} recall={a['recall']:.3f} "
              f"genuine_VRUs={len(vru_ids)} vruImp_reports_on_VRUs={len(flagged)} "
              f"revoked_VRUs={len(a['revoked_vrus'])}")
    assert flagged == [], f"vruImpersonation must NOT fire on genuine VRUs, got {len(flagged)} reports"
    assert a["revoked_vrus"] == [], f"genuine VRUs must never be revoked, got {a['revoked_vrus']}"


def test_impersonation_precision_stays_high(imp_run):
    """Adding the attack + detector does not wreck revocation precision (the detector is specific)."""
    s = V.validate(imp_run)[0]
    assert s["precision"] >= 0.9, s["precision"]
    assert s.get("leakage_violations", 0) == 0


# --------------------------------------------------------------------------- #
# 4. leakage firewall + attack-family fold
# --------------------------------------------------------------------------- #
def test_detector_and_feature_carry_no_oracle_identity(imp_run):
    """The new detector/feature carries no oracle identity: MA files + every feature table pass the
    leakage linter; is_vru never appears; detnorm_vruImpersonation IS a (leakage-safe) fusion feature;
    and the attack folds to the 'identity' family."""
    for f in (pathlib.Path(imp_run) / "ma").glob("*.jsonl"):
        for row in _jsonl(f):
            assert "is_vru" not in row, f
            assert find_forbidden_keys(row) == [], (f, row)

    featurize.build(imp_run)
    ml = pathlib.Path(imp_run) / "ml"
    df = pd.read_csv(ml / "report_features.csv")
    assert "detnorm_vruImpersonation" in df.columns
    assert find_forbidden_keys({c: 0 for c in df.columns if c not in ("split", "time_split")}) == []
    for name in ("subject_features", "vehicle_features", "vehicle_features_ma"):
        cols = pd.read_csv(ml / f"{name}.csv").columns
        assert find_forbidden_keys({c: 0 for c in cols if c not in ("split", "time_split")}) == [], name

    # schema.json advertises the fusion feature (with a description) and never as a leaky key.
    schema = json.loads((ml / "schema.json").read_text())
    rf = {c["name"]: c for c in schema["report_features"]}
    assert rf["detnorm_vruImpersonation"]["kind"] == "fusion_feature"
    assert rf["detnorm_vruImpersonation"].get("desc")


def test_attack_family_maps_to_identity(imp_run):
    """featurize._ATTACK_FAMILY folds VruImpersonation into 'identity' (alongside Sybil), and a run
    using it yields an 'identity' label in vehicle_labels -> the leave-one-family-out fold exists."""
    assert featurize._ATTACK_FAMILY.get(ATTACK) == "identity"
    featurize.build(imp_run)
    fams = set(pd.read_parquet(pathlib.Path(imp_run) / "ml" / "vehicle_labels.parquet")["attack_family"])
    assert "identity" in fams, f"identity fold missing from vehicle_labels ({fams})"


# --------------------------------------------------------------------------- #
# 5. reachability via the other opt-in selectors + determinism
# --------------------------------------------------------------------------- #
def test_reachable_via_attack_mix_and_attack_type(tmp_path):
    """The opt-in attack is reachable through attack_mix and the single attack_type narrowing selector,
    not only attack_types."""
    mix = str(tmp_path / "mix")
    run_pipeline(PipelineConfig(attack_mix=f"{ATTACK}:1.0", out_dir=mix,
                                **{k: v for k, v in _COMMON.items() if k != "vru_pct"}))
    assert {a["attack_type"] for a in _jsonl(pathlib.Path(mix) / "ground_truth" / "gt_attacks.jsonl")} \
        == {ATTACK}

    single = str(tmp_path / "single")
    run_pipeline(PipelineConfig(attack_type=ATTACK, out_dir=single,
                                **{k: v for k, v in _COMMON.items() if k != "vru_pct"}))
    assert {a["attack_type"] for a in _jsonl(pathlib.Path(single) / "ground_truth" / "gt_attacks.jsonl")} \
        == {ATTACK}


def test_impersonation_run_is_deterministic(tmp_path):
    """Same seed + config with the impersonation attack -> byte-identical data_digest (per-vehicle
    string-keyed rng only)."""
    def dig(tag):
        return run_pipeline(PipelineConfig(attack_types=(ATTACK,),
                                           out_dir=str(tmp_path / tag), **_COMMON)).data_digest
    assert dig("a") == dig("b")
