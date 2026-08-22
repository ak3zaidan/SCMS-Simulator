"""ML-contract regression tests (product-audit fixes).

BUG 2: ml/schema.json must never advertise a benchmark-excluded column as a trainable feature.
BUG 3a: RSU (infrastructure) evidence must be visible to the ML tables -- as opaque graph
        nodes/edges and as per-subject/per-vehicle features -- without leaking any identity.
"""

import json
import os

import pandas as pd

from scms_sim_ref.datagen import benchmark, featurize
from scms_sim_ref.datagen.benchmark import EXCLUDED_FEATURE_COLUMNS
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys, lint_feature_frame
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

_FEATURE_KINDS = {"feature", "fusion_feature", "fusion_feature_agg", "reason_flag_feature"}


def _build_collusion_rsus(out_dir: str, split_seed: int = 5) -> str:
    """A dataset that exercises BOTH the collusion path and RSU infrastructure reporters."""
    run_pipeline(PipelineConfig(seed=21, traffic_flow=True, road_network="grid", duration_s=300.0,
                                arrival_rate=0.25, grid_w=6, grid_h=6, grid_block_m=120.0,
                                radio_range_m=90.0, attacker_pct=0.25, attack_type="RandomPos",
                                attack_types=("RandomPos",), faulty_pct=0.0, collude_pct=0.3,
                                n_rsus=20, out_dir=out_dir))
    featurize.build(out_dir, split_seed=split_seed)
    return out_dir


# ---------------------------------------------------------------------------------------------- #
# BUG 2 -- SCHEMA1
# ---------------------------------------------------------------------------------------------- #
def test_schema_never_marks_a_benchmark_excluded_column_as_feature(tmp_path):
    """No column that benchmark.py drops from the model feature matrix may be a (fusion) feature in
    schema.json -- schema.json is THE machine-readable contract, so it must agree with the model."""
    out = _build_collusion_rsus(str(tmp_path / "run"))
    schema = json.loads((tmp_path / "run" / "ml" / "schema.json").read_text())

    # (a) exact source-of-truth agreement: intersection of {kind in feature_kinds} with the shared
    # exclusion constant is empty in EVERY table.
    offenders = []
    for tbl, cols in schema.items():
        if tbl == "_legend":
            continue
        for c in cols:
            if c["kind"] in _FEATURE_KINDS and c["name"] in EXCLUDED_FEATURE_COLUMNS:
                offenders.append((tbl, c["name"], c["kind"]))
    assert not offenders, f"benchmark-excluded columns marked as features: {offenders}"

    # (b) the two specific audit leaks are present in report_features but NOT kind=feature.
    rf = {c["name"]: c for c in schema["report_features"]}
    for leaky in ("detection_time", "crl_active_at_report"):
        assert leaky in rf, f"{leaky} should still be kept in report_features"
        assert rf[leaky]["kind"] != "feature", f"{leaky} must not be advertised as a feature"
        assert rf[leaky]["kind"] == "metadata", rf[leaky]

    # sanity: there are still real feature columns to train on
    feats = [c for c in schema["report_features"] if c["kind"] in _FEATURE_KINDS]
    assert feats, "report_features should still expose trainable feature columns"


# ---------------------------------------------------------------------------------------------- #
# BUG 3a -- RSU evidence is ML-consumable
# ---------------------------------------------------------------------------------------------- #
def test_rsu_evidence_surfaces_in_graph_and_features_without_leakage(tmp_path):
    out = _build_collusion_rsus(str(tmp_path / "run"))
    ml = os.path.join(out, "ml")
    ge = pd.read_parquet(os.path.join(ml, "graph_edges.parquet"))
    sf = pd.read_parquet(os.path.join(ml, "subject_features.parquet"))
    vf = pd.read_parquet(os.path.join(ml, "vehicle_features.parquet"))

    # graph_edges carries the flag and RSU-sourced edges with opaque rsu_* endpoints
    assert "is_infrastructure" in ge.columns
    rsu_edges = ge[ge["is_infrastructure"] == 1]
    assert len(rsu_edges) > 0, "RSU-sourced edges should now appear in graph_edges"
    assert rsu_edges["src_entity"].str.startswith("rsu_").all(), "RSU edge src must be an opaque rsu_ id"
    assert rsu_edges["dst_entity"].str.startswith("ent_").all(), "RSU edge dst is an opaque vehicle id"
    # raw reporter cert must never appear as a node id (opaque only: rsu_ = 4 + 12 hex chars)
    assert (rsu_edges["src_entity"].str.len() == len("rsu_") + 12).all()
    # vehicle edges keep the old opaque scheme and the flag is 0 for them
    veh_edges = ge[ge["is_infrastructure"] == 0]
    if len(veh_edges):
        assert veh_edges["src_entity"].str.startswith("ent_").all()

    # per-subject and per-vehicle RSU features exist and fire for at least some subjects
    for df, key in ((sf, "subject_features"), (vf, "vehicle_features")):
        assert "n_rsu_reporters" in df.columns, key
        assert "frac_rsu_reports" in df.columns, key
        assert df["n_rsu_reporters"].max() > 0, f"{key}: some subject should have an RSU reporter"
        assert df["frac_rsu_reports"].max() > 0.0, key
        assert (df["frac_rsu_reports"] >= 0.0).all() and (df["frac_rsu_reports"] <= 1.0).all()

    # leakage linter: no forbidden key in the new columns OR on the actual rows
    for df in (ge, sf, vf):
        cols = {c: 0 for c in df.columns if c not in ("split", "time_split")}
        assert find_forbidden_keys(cols) == []
        rows = df.drop(columns=[c for c in ("split", "time_split") if c in df.columns]).to_dict("records")
        lint_feature_frame(rows, context="rsu_rows")   # raises LeakageViolation on any forbidden key

    # the new feature names themselves are not leaky under the project's own predicate
    from scms_sim_ref.schemas.records import is_forbidden_feature_key
    for name in ("n_rsu_reporters", "frac_rsu_reports", "is_infrastructure"):
        assert not is_forbidden_feature_key(name), name


# ---------------------------------------------------------------------------------------------- #
# Determinism -- RSU handling must stay byte-stable
# ---------------------------------------------------------------------------------------------- #
def test_rsu_featurize_is_byte_identical_across_runs(tmp_path):
    """Featurizing the SAME dataset twice yields byte-identical ml/graph_edges + *_features, even
    with RSU nodes/edges and set-based intermediate structures in play."""
    out = _build_collusion_rsus(str(tmp_path / "run"))
    ml = os.path.join(out, "ml")
    tables = ("graph_edges", "subject_features", "vehicle_features", "report_features",
              "vehicle_features_ma")

    def _snapshot():
        return {t: open(os.path.join(ml, f"{t}.csv"), "rb").read() for t in tables}

    first = _snapshot()
    featurize.build(out, split_seed=5)   # rebuild on the same dataset
    second = _snapshot()
    for t in tables:
        assert first[t] == second[t], f"{t}.csv is not byte-identical across featurize runs"

    # graph_edges are deterministically ordered (independent of dict/set iteration)
    a = pd.read_parquet(os.path.join(ml, "graph_edges.parquet"))
    featurize.build(out, split_seed=5)
    b = pd.read_parquet(os.path.join(ml, "graph_edges.parquet"))
    assert a.equals(b)


def test_excluded_feature_columns_is_the_shared_source_of_truth():
    """featurize imports the very constant benchmark uses for its feature matrix."""
    assert EXCLUDED_FEATURE_COLUMNS is benchmark._DROP
    assert {"detection_time", "crl_active_at_report"} <= set(EXCLUDED_FEATURE_COLUMNS)
