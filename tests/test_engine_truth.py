"""Engine-truth regression tests for a product audit's data-quality bugs.

Each fix affects only NON-default configs (collusion, RSU-with-longer-range, user-set attacker_ids /
attack_type / victim_pct); the default-config golden digest MUST stay byte-identical. See the
per-test docstrings for the exact bug each guards against.
"""

import json

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import config_from_dict, validate_config


# Default-path reference digest, measured on this branch's starting commit BEFORE any fix. Every
# fix here is gated so the default config produces zero new draws / zero changed output.
# ADR 0002 re-pin (true_speed/true_heading added to gt_emissions_sample):
# superseded 04ae9736f519... (default-config run; behaviour unchanged).
_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


def _jsonl(p):
    return [json.loads(ln) for ln in open(p, encoding="utf-8") if ln.strip()]


def _labelled_reports(out):
    """Return ma_reports joined with their ground-truth correctness label."""
    reports = _jsonl(out + "/ma/ma_reports.jsonl")
    labels = {x["report_id"]: x for x in _jsonl(out + "/ground_truth/gt_report_labels.jsonl")}
    for r in reports:
        r["_correctness"] = labels[r["report_id"]]["report_correctness"]
    return reports


# --------------------------------------------------------------------------- #
# Determinism contract: the default golden must never move.
# --------------------------------------------------------------------------- #
def test_default_golden_digest_unchanged(tmp_path):
    """The canonical reference config still hashes to the golden digest: none of the audit fixes
    perturb the default path (collude_pct=0, n_rsus=0, default attack_type/attacker_pct)."""
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == _GOLDEN, r.data_digest


# --------------------------------------------------------------------------- #
# BUG 1: colluder fabricated reports no longer carry a constant fingerprint.
# --------------------------------------------------------------------------- #
def test_bug1_colluder_false_reports_are_plausible_not_a_constant_fingerprint(tmp_path):
    """Colluder-fabricated false reports used to file identical detector_score=1.3 / conf=5.0 on every
    row -- a free 100%-separable oracle for collusion. After the fix they draw from the colluder's own
    keyed RNG, so the values VARY per report and land inside the range genuine reports occupy."""
    out = str(tmp_path / "collude")
    run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=150.0,
                                arrival_rate=1.5, grid_w=6, grid_h=6, attacker_pct=0.3,
                                collude_pct=0.5, victim_pct=0.10, out_dir=out))
    reps = _labelled_reports(out)
    mal = [r for r in reps if r["_correctness"] == "malicious_false_report"]
    genuine = [r for r in reps if r["_correctness"] != "malicious_false_report"]
    assert len(mal) > 100, f"need a body of fabricated reports to test (got {len(mal)})"
    assert genuine, "need genuine reports to define the plausible range"

    mal_scores = [r["detector_score"] for r in mal]
    mal_confs = [r["subject_pos_confidence"] for r in mal]

    # (a) values VARY -- the constant 1.3 / 5.0 fingerprint is gone.
    assert len(set(mal_scores)) > 20, f"detector_score barely varies: {len(set(mal_scores))} distinct"
    assert len(set(mal_confs)) > 20, f"pos_confidence barely varies: {len(set(mal_confs))} distinct"
    assert not all(s == 1.3 for s in mal_scores), "still the old constant detector_score fingerprint"
    assert not all(c == 5.0 for c in mal_confs), "still the old constant pos_confidence fingerprint"

    # (b) fabricated values fall INSIDE the range genuine reports occupy (plausible, not out-of-band).
    g_scores = [r["detector_score"] for r in genuine]
    g_confs = [r["subject_pos_confidence"] for r in genuine]
    assert min(g_scores) <= min(mal_scores) and max(mal_scores) <= max(g_scores), \
        (min(mal_scores), max(mal_scores), min(g_scores), max(g_scores))
    assert min(g_confs) <= min(mal_confs) and max(mal_confs) <= max(g_confs), \
        (min(mal_confs), max(mal_confs), min(g_confs), max(g_confs))

    # (c) no longer trivially separable: genuine reports EXIST inside the fabricated value band, so a
    # single detector_score/pos_confidence threshold cannot cleanly split malicious from genuine.
    lo_s, hi_s = min(mal_scores), max(mal_scores)
    lo_c, hi_c = min(mal_confs), max(mal_confs)
    overlap = [r for r in genuine
               if lo_s <= r["detector_score"] <= hi_s and lo_c <= r["subject_pos_confidence"] <= hi_c]
    assert len(overlap) > 20, f"fabricated band still separable from genuine (overlap={len(overlap)})"

    # (d) the ALWAYS-ON radio detectors are no longer a structural giveaway. Every genuine in-range
    # report carries sybilCoLocation>0 (self-count) and beaconFrequency>0 (beacon rate); fabricated
    # reports that left these exactly 0.0 stayed 100%-separable on the detnorm_* fusion features even
    # after score/conf were varied (regression-audit finding F1). After the fix they carry plausible
    # non-zero values overlapping the genuine band.
    for key in ("detnorm_beaconFrequency", "detnorm_sybilCoLocation"):
        mvals = [r.get(key, 0.0) for r in mal]
        gvals = [r.get(key, 0.0) for r in genuine]
        frac_zero = sum(v == 0.0 for v in mvals) / len(mvals)
        assert frac_zero < 0.05, f"{key}: {frac_zero:.0%} of fabricated reports are exactly 0.0 (separable)"
        lo, hi = min(mvals), max(mvals)
        assert any(lo <= g <= hi for g in gvals), f"{key}: fabricated band shares no genuine reports"

    # subject/label integrity preserved: still frames the victim, still labelled malicious.
    assert all(r["reason_codes"] == ["positionSpeedInconsistency"] for r in mal)


# --------------------------------------------------------------------------- #
# BUG 3b: acceptanceRangeThreshold uses the RECEIVER's actual range for RSUs.
# --------------------------------------------------------------------------- #
def test_bug3b_long_range_rsu_does_not_flag_honest_distant_vehicles(tmp_path):
    """An RSU with rsu_range_m > radio_range_m legitimately hears distant honest vehicles. The
    acceptanceRangeThreshold detector used to measure excess distance against the global radio_range_m,
    so it flagged those honest vehicles as out-of-range. Now it measures against the receiver's actual
    range, so honest vehicles within the RSU's true range are NOT flagged.

    Counterfactual on this exact config: the pre-fix formula produced 2806 spurious benign
    acceptanceRangeThreshold firings (all by RSUs); the fix drops that to 0."""
    out = str(tmp_path / "rsu")
    run_pipeline(PipelineConfig(seed=11, traffic_flow=True, road_network="grid", duration_s=200.0,
                                arrival_rate=0.8, grid_w=6, grid_h=6, grid_block_m=120.0,
                                radio_range_m=70.0, attacker_pct=0.2, attack_type="RandomPos",
                                attack_types=("RandomPos",), faulty_pct=0.0,
                                n_rsus=8, rsu_range_m=500.0, out_dir=out))
    reps = _labelled_reports(out)
    idm = {m["pseudonym_cert_digest"] for m in _jsonl(out + "/ground_truth/gt_identity_map.jsonl")}
    rsu_reps = [r for r in reps if r["reporter_cert_digest"] not in idm]
    assert rsu_reps, "RSUs must be active receivers for this test to be meaningful"

    # honest vehicles (benign subjects) must not be flagged with acceptanceRangeThreshold at all.
    spurious = [r for r in reps if "acceptanceRangeThreshold" in r.get("reason_codes", [])
                and r["_correctness"] in ("false_positive", "faulty_detection")]
    assert spurious == [], f"honest vehicles flagged as out-of-range by a long-range RSU: {len(spurious)}"


# --------------------------------------------------------------------------- #
# BUG 4: attacker_ids element coercion + validation.
# --------------------------------------------------------------------------- #
def test_bug4_attacker_ids_coerced_from_json_strings():
    """A saved attacker_ids=(7,) round-trips through JSON as ["7"]; config_from_dict must coerce the
    elements back to int so they match integer vehicle ids (else silent zero-attacker datasets)."""
    c = config_from_dict({"attacker_ids": ["7"], "attack_types": ["ConstPos", "RandomPos"]})
    assert c.attacker_ids == (7,)
    assert 7 in set(c.attacker_ids)
    assert all(isinstance(a, int) and not isinstance(a, bool) for a in c.attacker_ids)
    assert all(isinstance(a, str) for a in c.attack_types)


def test_bug4_validate_config_rejects_bad_attacker_ids():
    """validate_config fails fast on non-int and out-of-range attacker ids (fixed-fleet path)."""
    import pytest

    with pytest.raises(ValueError, match="attacker_ids"):
        validate_config(PipelineConfig(attacker_ids=("7",), n_vehicles=12))    # non-int
    with pytest.raises(ValueError, match="out of range"):
        validate_config(PipelineConfig(attacker_ids=(99,), n_vehicles=12))     # out of range
    # a valid fixed-fleet id passes untouched
    ok = validate_config(PipelineConfig(attacker_ids=(7,), n_vehicles=12))
    assert ok.attacker_ids == (7,)


# --------------------------------------------------------------------------- #
# BUG 6: attack_type and flow-mode victim_pct are no longer dead knobs.
# --------------------------------------------------------------------------- #
def _attack_types_present(out):
    return {a["attack_type"] for a in _jsonl(out + "/ground_truth/gt_attacks.jsonl")}


def test_bug6_attack_type_narrows_catalog_when_attack_types_default(tmp_path):
    """Setting attack_type to a non-default single value (with attack_types left at its default full
    catalog) makes the whole run use only that type. The default attack_type keeps the full catalog."""
    common = dict(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0, arrival_rate=1.5,
                  grid_w=6, grid_h=6, attacker_pct=0.3, faulty_pct=0.0)
    narrowed = str(tmp_path / "narrow")
    run_pipeline(PipelineConfig(attack_type="RandomPos", out_dir=narrowed, **common))
    assert _attack_types_present(narrowed) == {"RandomPos"}

    # control: default attack_type -> full catalog round-robin -> many types present.
    full = str(tmp_path / "full")
    run_pipeline(PipelineConfig(out_dir=full, **common))
    assert len(_attack_types_present(full)) > 1, "default attack_type must keep the full catalog"


def test_bug6_victim_pct_scales_flow_framing(tmp_path):
    """In flow mode victim_pct now scales how many benign vehicles colluders frame; victim_pct=0
    keeps the legacy 2-nearest behaviour (still frames some victims)."""
    def n_framed(vpct):
        out = str(tmp_path / f"v{vpct}")
        run_pipeline(PipelineConfig(seed=13, traffic_flow=True, road_network="grid", duration_s=120.0,
                                    arrival_rate=1.5, grid_w=6, grid_h=6, attacker_pct=0.3,
                                    collude_pct=1.0, victim_pct=vpct, faulty_pct=0.0, out_dir=out))
        labs = _jsonl(out + "/ground_truth/gt_report_labels.jsonl")
        return sum(1 for x in labs if x["report_correctness"] == "malicious_false_report")

    low, high = n_framed(0.15), n_framed(0.40)
    assert high > low * 1.5, f"victim_pct must scale flow framing: {low} -> {high}"
    assert n_framed(0.0) > 0, "victim_pct=0 must keep the legacy 2-nearest framing"


# --------------------------------------------------------------------------- #
# Determinism: a collusion + RSU config is byte-reproducible.
# --------------------------------------------------------------------------- #
def test_collusion_and_rsu_config_is_deterministic(tmp_path):
    """The new per-colluder / per-run keyed RNGs preserve the determinism contract: same seed+config
    -> byte-identical digest, exercising BUG1 (fabrication), BUG3b (RSU range) and BUG6 (victim_pct)."""
    def run(o):
        return run_pipeline(PipelineConfig(seed=21, traffic_flow=True, road_network="grid",
                                           duration_s=120.0, arrival_rate=1.0, grid_w=6, grid_h=6,
                                           radio_range_m=100.0, attacker_pct=0.3, collude_pct=0.6,
                                           victim_pct=0.15, n_rsus=6, rsu_range_m=300.0,
                                           out_dir=o)).data_digest
    assert run(str(tmp_path / "a")) == run(str(tmp_path / "b"))


# --------------------------------------------------------------------------- #
# Consolidation-audit follow-ups (LOW findings #2/#4/#5): config-surface hygiene.
# --------------------------------------------------------------------------- #
import pytest  # noqa: E402


def test_empty_attack_types_falls_back_to_constpos_not_sentinel(tmp_path):
    """#2: an explicit attack_types=() must not inject the "" sentinel as a literal no-op 'attacker'
    (which would poison ground truth with undetectable positives). It falls back to ConstPos."""
    run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=40,
                                arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.3,
                                attack_types=(), out_dir=str(tmp_path / "e")))
    types = {a["attack_type"] for a in _jsonl(tmp_path / "e" / "ground_truth" / "gt_attacks.jsonl")}
    assert types == {"ConstPos"} and "" not in types


def test_speed_caps_rejected_on_topologies_that_ignore_them():
    """#4: arterial_*/local_speed_mps are dead knobs off grid/ring -> validate_config rejects them."""
    for rn in ("linear", "spider", "custom"):
        with pytest.raises(ValueError, match="grid/ring|apply only"):
            validate_config(PipelineConfig(
                traffic_flow=True, road_network=rn, arterial_speed_mps=20.0,
                custom_network='{"nodes":[[0,0],[100,0],[100,100]],"edges":[[0,1],[1,2],[2,0]]}'))
    with pytest.raises(ValueError, match="ring uses only"):
        validate_config(PipelineConfig(traffic_flow=True, road_network="ring", grid_w=10,
                                       local_speed_mps=8.0))
    # ring + a single whole-ring cap is allowed; grid + the full trio is allowed
    validate_config(PipelineConfig(traffic_flow=True, road_network="ring", grid_w=10,
                                   arterial_speed_mps=15.0))
    validate_config(PipelineConfig(traffic_flow=True, road_network="grid", grid_w=6, grid_h=6,
                                   arterial_every=3, arterial_speed_mps=25.0, local_speed_mps=9.0))


def test_new_opt_in_fields_have_validate_bounds():
    """#5: rx_sensitivity_margin_db and lane_change_threshold now have runtime guards."""
    with pytest.raises(ValueError, match="rx_sensitivity_margin_db"):
        validate_config(PipelineConfig(radio_model="logdistance", rx_sensitivity_margin_db=99.0))
    with pytest.raises(ValueError, match="lane_change_threshold"):
        validate_config(PipelineConfig(traffic_flow=True, n_lanes=3, lane_changes=True,
                                       lane_change_threshold=-1.0))
