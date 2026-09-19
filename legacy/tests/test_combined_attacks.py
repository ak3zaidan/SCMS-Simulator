"""Tests for the opt-in "combined" attack family (audit gap #9) and the attack_type narrowing fix (F2).

The "combined" family (Disruptive / PosSpeedInconsistent / PosHeadingInconsistent / EventualStop) is
a single attacker falsifying MULTIPLE fields mutually inconsistently, so it trips several detector
families at once. It is OPT-IN: excluded from the default round-robin catalog so the golden data_digest
stays byte-identical, and produced only when explicitly requested via attack_types / attack_mix /
attack_type. These tests cover: the frozen golden digest, per-type assignment + falsification +
multi-detector firing, opt-in safety, the featurize family fold, and the F2 attack_type sentinel.
"""

import collections
import json
import tempfile
import pathlib

import pandas as pd
import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import ATTACK_CATALOG, COMBINED_ATTACKS
from scms_sim_ref.datagen import featurize

# The golden default-selection digest (multiple attackers, round-robin over the DEFAULT catalog). Any
# change to the attack catalog / assignment order would move it; the combined family must NOT.
GOLDEN_DIGEST = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"

# A config large enough that a single opt-in type is assigned to many attackers over many steps.
_COMMON = dict(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0, arrival_rate=1.5,
               grid_w=6, grid_h=6, attacker_pct=0.3, faulty_pct=0.0)


def _jsonl(p):
    p = pathlib.Path(p)
    return [json.loads(ln) for ln in p.read_text(encoding="utf-8").splitlines() if ln.strip()] \
        if p.exists() else []


def _attack_types_present(out):
    return {a["attack_type"] for a in _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")}


def _analyze(out, wanted_type):
    """Return per-type stats for a run in which every attacker uses `wanted_type`."""
    gt = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_attacks.jsonl")
    att_ids = {a["true_vehicle_id"] for a in gt}
    n_onset = sum(1 for a in gt if a.get("attack_onset_time") is not None)
    labels = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_report_labels.jsonl")
    subj = {l["report_id"]: l["subject_true_id"] for l in labels}
    corr = {l["report_id"]: l["report_correctness"] for l in labels}
    reasons = collections.Counter()
    detected = set()
    for r in _jsonl(pathlib.Path(out) / "ma" / "ma_reports.jsonl"):
        rid = r["report_id"]
        if subj.get(rid) in att_ids and corr.get(rid) == "correct":
            detected.add(subj[rid])
            reasons.update(r.get("reason_codes") or [])
    emis = _jsonl(pathlib.Path(out) / "ground_truth" / "gt_emissions_sample.jsonl")
    return {
        "types": {a["attack_type"] for a in gt},
        "n_att": len(att_ids),
        "n_onset": n_onset,
        "distinct_reasons": set(reasons),
        "recall": len(detected) / max(1, len(att_ids)),
        "falsified_emissions": sum(1 for e in emis if e.get("is_attacker") and e.get("falsified")),
    }


def test_default_golden_digest_unchanged():
    """Adding the opt-in combined family must not perturb the DEFAULT-selection path: same seed+config
    -> byte-identical data_digest as before this change."""
    tmp = pathlib.Path(tempfile.mkdtemp())
    res = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                      arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                      out_dir=str(tmp / "g")))
    assert res.data_digest == GOLDEN_DIGEST


def test_combined_family_excluded_from_default_catalog():
    """Opt-in invariant: none of the combined types may be a member of the default round-robin set."""
    assert not (set(COMBINED_ATTACKS) & set(ATTACK_CATALOG))
    assert PipelineConfig().attack_types == ATTACK_CATALOG        # default selection is the frozen catalog


@pytest.mark.parametrize("atype", list(COMBINED_ATTACKS))
def test_combined_type_assigned_falsifies_and_trips_multiple_detectors(tmp_path, atype):
    """Each combined type, when explicitly selected, is (a) assigned to attackers, (b) actually
    falsifies (onset set + detected with correct reports), and (c) trips MORE THAN ONE distinct
    detector reason across the run -- the defining property of the 'combined' family."""
    out = str(tmp_path / atype)
    run_pipeline(PipelineConfig(attack_types=(atype,), out_dir=out, **_COMMON))
    st = _analyze(out, atype)
    # (a) assigned: every attacker carries exactly this type
    assert st["types"] == {atype}
    assert st["n_att"] >= 5, "config should yield several attackers to exercise the type"
    # (b) actually falsifies: most attackers emitted a falsified broadcast (onset stamped) and a
    #     non-trivial fraction were caught with CORRECT reports.
    assert st["n_onset"] >= 0.5 * st["n_att"], f"{atype}: too few attackers ever falsified"
    assert st["recall"] >= 0.5, f"{atype}: recall too low ({st['recall']:.2f})"
    # (c) the 'combined' point: multiple distinct detector families fire across the run.
    assert len(st["distinct_reasons"]) >= 2, \
        f"{atype} should trip >1 detector, got {st['distinct_reasons']}"


def test_default_run_has_no_combined_types(tmp_path):
    """Opt-in safety: a DEFAULT run's attacker type multiset never contains a combined type, and the
    full default catalog is still exercised (round-robin unchanged)."""
    out = str(tmp_path / "default")
    run_pipeline(PipelineConfig(out_dir=out, **_COMMON))
    present = _attack_types_present(out)
    assert not (present & set(COMBINED_ATTACKS)), f"default run leaked combined types: {present}"
    assert present <= set(ATTACK_CATALOG)
    assert len(present) > 1, "default run must round-robin the full catalog"


def test_featurize_maps_combined_family_and_fold_is_available(tmp_path):
    """featurize._ATTACK_FAMILY maps all four combined bases to family 'combined', and a run using
    them yields a 'combined' label in vehicle_labels -> a leave-one-family-out fold now exists."""
    for t in COMBINED_ATTACKS:
        assert featurize._ATTACK_FAMILY.get(t) == "combined", t
    out = str(tmp_path / "combined")
    run_pipeline(PipelineConfig(attack_types=COMBINED_ATTACKS, out_dir=out, **_COMMON))
    featurize.build(out)
    fams = set(pd.read_parquet(pathlib.Path(out) / "ml" / "vehicle_labels.parquet")["attack_family"])
    assert "combined" in fams, f"combined fold missing from vehicle_labels ({fams})"


def test_combined_type_reachable_via_attack_mix(tmp_path):
    """The opt-in family is also reachable through the attack_mix weighted selector (not just
    attack_types)."""
    out = str(tmp_path / "mix")
    run_pipeline(PipelineConfig(attack_mix="Disruptive:1.0", out_dir=out, **_COMMON))
    assert _attack_types_present(out) == {"Disruptive"}


def test_combined_run_is_deterministic(tmp_path):
    """Same seed+config with a combined type -> byte-identical data_digest (per-vehicle keyed rng)."""
    def dig(tag):
        return run_pipeline(PipelineConfig(attack_types=("Disruptive",),
                                           out_dir=str(tmp_path / tag), **_COMMON)).data_digest
    assert dig("a") == dig("b")


# --------------------------------------------------------------------------- #
# F2: attack_type narrowing now distinguishes "explicitly set" from "left at default".
# --------------------------------------------------------------------------- #
def test_f2_attack_type_constpos_narrows_and_sentinel_does_not(tmp_path):
    """F2 fix: attack_type is a sentinel ("") by default, so ANY explicit value -- including the real
    catalog member "ConstPos" -- narrows the run to just that type; the sentinel leaves the full
    catalog. Both paths are deterministic."""
    assert PipelineConfig().attack_type == ""                    # default is the unset sentinel

    # a real, default-named value now narrows (the pre-fix bug: "ConstPos" silently did nothing)
    cp = str(tmp_path / "constpos")
    run_pipeline(PipelineConfig(attack_type="ConstPos", out_dir=cp, **_COMMON))
    assert _attack_types_present(cp) == {"ConstPos"}

    # the sentinel leaves the catalog alone -> full round-robin
    sent = str(tmp_path / "sentinel")
    run_pipeline(PipelineConfig(attack_type="", out_dir=sent, **_COMMON))
    assert len(_attack_types_present(sent)) > 1

    # a combined type is a valid narrowing target too
    dis = str(tmp_path / "dis")
    run_pipeline(PipelineConfig(attack_type="Disruptive", out_dir=dis, **_COMMON))
    assert _attack_types_present(dis) == {"Disruptive"}


def test_f2_explicit_attack_types_wins_over_attack_type(tmp_path):
    """An explicitly-set (non-default) attack_types still overrides a single attack_type."""
    out = str(tmp_path / "wins")
    run_pipeline(PipelineConfig(attack_type="ConstPos", attack_types=("RandomPos", "Teleport"),
                                out_dir=out, **_COMMON))
    assert _attack_types_present(out) == {"RandomPos", "Teleport"}
