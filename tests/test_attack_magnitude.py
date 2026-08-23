"""Per-attack-type falsification-magnitude control (attack_magnitude_scale).

Covers the new lever added in run.py: a "Type:scale,Type2:scale2" string that multiplies each named
type's falsification magnitude ON TOP of the global attack_intensity dial. The contract is:

  * the DEFAULT config (empty scale) stays BYTE-IDENTICAL, and an explicit scale of 1.0 for a type is
    also byte-identical (the multiplier folds into k as k*1.0==k, and the non-k speed branches keep
    their exact historic expression when scale==1.0);
  * a larger scale produces a larger falsification (bigger true-vs-claimed offset) and, for a normally
    easy-to-detect attack, HIGHER detector recall; a small scale shrinks the lie and makes a normally
    easy attack HARDER to detect (lower recall) -- that is the whole point of the control;
  * validate_config rejects an unknown type name or a negative scale;
  * a scaled config is itself deterministic (byte-identical run-to-run).
"""

import json

import pytest

from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import _parse_magnitude_scale, validate_config

# Goldens recorded on the base commit (09d690c) BEFORE the feature was wired; they MUST NOT move.
DEFAULT_GOLDEN = "04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38"
MULTI_ATTACK_GOLDEN = "8894cb268af3ab4f0833a371f34808539fb0521a565727aea8037e427693f63c"
# A multi-attack default run spanning several position/speed/heading types (all magnitudes default).
MULTI_TYPES = ("ConstPosOffset", "RandomPos", "ConstSpeedOffset", "RandomSpeed",
               "StopAndGo", "HeadingOffset", "SlowDrift", "AlongRoadOffset")


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _default_digest(out_dir):
    return run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=out_dir)).data_digest


def _multi_digest(out_dir, scale=""):
    return run_pipeline(PipelineConfig(
        seed=13, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.3, attack_types=MULTI_TYPES,
        attack_magnitude_scale=scale, out_dir=out_dir)).data_digest


def _single_type_run(tmp_path, typ, scale, tag):
    """Run one isolated single-attack-type flow and return (mean_falsified_offset_m, recall, out_dir).

    recall = fraction of true attacker vehicles that draw at least one CORRECT report. emit_sample_prob
    is forced to 1.0 so the true-vs-claimed offset sample is complete; faulty vehicles are turned off so
    the only positives are this one attack type (the effect is fully isolated)."""
    out = str(tmp_path / f"{tag}")
    cfg = PipelineConfig(
        seed=11, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=2.0,
        grid_w=5, grid_h=5, attacker_pct=0.3, attack_type=typ, attack_types=(typ,),
        faulty_pct=0.0, emit_sample_prob=1.0,
        attack_magnitude_scale=(f"{typ}:{scale}" if scale is not None else ""), out_dir=out)
    run_pipeline(cfg)
    em = _jsonl(tmp_path / tag / "ground_truth" / "gt_emissions_sample.jsonl")
    offs = [((e["claimed_x"] - e["true_x"]) ** 2 + (e["claimed_y"] - e["true_y"]) ** 2) ** 0.5
            for e in em if e["falsified"]]
    mean_off = sum(offs) / len(offs) if offs else 0.0
    lbl = _jsonl(tmp_path / tag / "ground_truth" / "gt_report_labels.jsonl")
    correct = {r["subject_true_id"] for r in lbl if r["report_correctness"] == "correct"}
    attackers = {e["true_vehicle_id"] for e in em if e["is_attacker"]}
    recall = len(correct & attackers) / len(attackers) if attackers else 0.0
    return mean_off, recall, out


# --------------------------------------------------------------------------- determinism / goldens

def test_default_golden_unchanged(tmp_path):
    assert _default_digest(str(tmp_path / "d")) == DEFAULT_GOLDEN


def test_multi_attack_default_golden_unchanged(tmp_path):
    assert _multi_digest(str(tmp_path / "m")) == MULTI_ATTACK_GOLDEN


def test_empty_scale_equals_unset(tmp_path):
    """attack_magnitude_scale='' is exactly the historic path (byte-identical to the golden)."""
    assert _multi_digest(str(tmp_path / "m"), scale="") == MULTI_ATTACK_GOLDEN


def test_explicit_scale_one_equals_default(tmp_path):
    """An explicit scale of 1.0 for several types (k-scaled AND non-k speed branches) is byte-identical
    to leaving the magnitudes at their default -- k*1.0==k, and the speed branches keep their exact
    historic expression at scale==1.0."""
    ones = "ConstPosOffset:1.0,RandomPos:1.0,ConstSpeedOffset:1.0,HeadingOffset:1.0," \
           "SlowDrift:1.0,AlongRoadOffset:1.0,RandomSpeed:1.0,StopAndGo:1.0"
    assert _multi_digest(str(tmp_path / "ones"), scale=ones) == MULTI_ATTACK_GOLDEN


def test_scaled_config_is_deterministic(tmp_path):
    """A NON-default (scaled) config is itself byte-identical run-to-run."""
    d1 = _multi_digest(str(tmp_path / "a"), scale="RandomPos:2.5,ConstPosOffset:0.4,RandomSpeed:1.7")
    d2 = _multi_digest(str(tmp_path / "b"), scale="RandomPos:2.5,ConstPosOffset:0.4,RandomSpeed:1.7")
    assert d1 == d2
    assert d1 != MULTI_ATTACK_GOLDEN            # ...and the scaling actually changed the dataset


# --------------------------------------------------------------------------- the control lever works

def test_position_scale_controls_magnitude_and_recall(tmp_path):
    """For a position attack, a larger per-type scale => larger true-vs-claimed offset AND (because the
    lie grows past the plausibility threshold) higher detector recall; a smaller scale shrinks both."""
    off_lo, rec_lo, _ = _single_type_run(tmp_path, "ConstPosOffset", 0.3, "lo")
    off_hi, rec_hi, _ = _single_type_run(tmp_path, "ConstPosOffset", 3.0, "hi")
    # magnitude: the 3.0 run's mean falsification is far larger than the 0.3 run's (10x the multiplier)
    assert off_hi > off_lo * 3.0, (off_lo, off_hi)
    # detection lever: the blatant run is caught far more often than the subtle one
    assert rec_hi > rec_lo, (rec_lo, rec_hi)


def test_small_scale_makes_easy_attack_harder_to_detect(tmp_path):
    """A normally EASY-to-detect attack (default scale => high recall) is driven UNDER the detector by a
    small per-type scale -- the lever can hide an attack, not only amplify it."""
    _, rec_default, _ = _single_type_run(tmp_path, "ConstPosOffset", None, "def")
    _, rec_tiny, _ = _single_type_run(tmp_path, "ConstPosOffset", 0.1, "tiny")
    assert rec_default >= 0.9, rec_default            # blatant-by-default -> caught nearly always
    assert rec_tiny < rec_default, (rec_tiny, rec_default)


def test_nonk_speed_branches_respond_to_scale(tmp_path):
    """The two historically non-k speed branches (RandomSpeed, StopAndGo) are byte-identical at scale
    1.0/unset yet widen (scale>1) or narrow (scale<1) their claimed-speed deviation."""
    for typ in ("RandomSpeed", "StopAndGo"):
        def _speed_span(scale, tag):
            out = str(tmp_path / f"{typ}_{tag}")
            run_pipeline(PipelineConfig(
                seed=11, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=2.0,
                grid_w=5, grid_h=5, attacker_pct=0.3, attack_type=typ, attack_types=(typ,),
                faulty_pct=0.0, emit_sample_prob=1.0,
                attack_magnitude_scale=(f"{typ}:{scale}" if scale is not None else ""), out_dir=out))
            cs = [e["claimed_speed"]
                  for e in _jsonl(tmp_path / f"{typ}_{tag}" / "ground_truth"
                                  / "gt_emissions_sample.jsonl") if e["falsified"]]
            return (max(cs) - min(cs)) if cs else 0.0
        span_unset = _speed_span(None, "unset")
        span_one = _speed_span(1.0, "one")
        span_lo = _speed_span(0.3, "lo")
        span_hi = _speed_span(3.0, "hi")
        assert span_one == pytest.approx(span_unset), (typ, span_one, span_unset)  # 1.0 == unset
        assert span_lo < span_unset < span_hi, (typ, span_lo, span_unset, span_hi)


# --------------------------------------------------------------------------- parser + validation

def test_parse_magnitude_scale_basics():
    assert _parse_magnitude_scale("") == {}
    assert _parse_magnitude_scale("   ") == {}
    # canonical KNOWN_ATTACK_TYPES order, regardless of input order
    parsed = _parse_magnitude_scale("RandomPos:2.0,ConstPosOffset:0.5")
    assert parsed == {"ConstPosOffset": 0.5, "RandomPos": 2.0}
    assert list(parsed) == ["ConstPosOffset", "RandomPos"]
    # a scale of exactly 0 is allowed (silences that type's falsification)
    assert _parse_magnitude_scale("HeadingOffset:0") == {"HeadingOffset": 0.0}


def test_validate_rejects_unknown_type():
    cfg = PipelineConfig(attack_magnitude_scale="NotAnAttack:2.0")
    with pytest.raises(ValueError):
        validate_config(cfg)


def test_validate_rejects_negative_scale():
    cfg = PipelineConfig(attack_magnitude_scale="RandomPos:-1.0")
    with pytest.raises(ValueError):
        validate_config(cfg)


def test_scaled_run_stays_leakage_free(tmp_path):
    """The lever is generation-side only: a scaled run's MA-visible reports carry no oracle/leakage
    keys (mirrors the leakage guard the other attack tests apply)."""
    out = str(tmp_path / "leak")
    run_pipeline(PipelineConfig(
        seed=11, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.3, attack_type="ConstPosOffset",
        attack_types=("ConstPosOffset",), attack_magnitude_scale="ConstPosOffset:2.0", out_dir=out))
    rows = _jsonl(tmp_path / "leak" / "ma" / "ma_reports.jsonl")
    assert rows
    for row in rows:
        assert not find_forbidden_keys(row), row
