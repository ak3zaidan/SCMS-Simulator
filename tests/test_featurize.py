"""End-to-end test for the ML featurizer/splitter."""

import os

import pandas as pd

from scms_sim_ref.datagen import featurize
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline


def test_featurize_end_to_end(tmp_path):
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=7))
    summary = featurize.build(out, split_seed=99)

    assert summary["n_reports"] > 0
    assert summary["n_subjects"] > 0
    assert summary["leakage_violations"] == 0
    assert summary["split_leakage"] == 0

    ml = os.path.join(out, "ml")
    rf = pd.read_parquet(os.path.join(ml, "report_features.parquet"))
    sf = pd.read_parquet(os.path.join(ml, "subject_features.parquet"))
    sl = pd.read_parquet(os.path.join(ml, "subject_labels.parquet"))

    # feature tables carry no ground-truth column names
    assert find_forbidden_keys({c: 0 for c in rf.columns if c != "split"}) == []
    assert find_forbidden_keys({c: 0 for c in sf.columns if c != "split"}) == []

    # labels present and at least one attacker labelled
    assert "label_is_attacker" in sl.columns
    assert sl["label_is_attacker"].sum() >= 1

    # splits are vehicle-disjoint
    seen = {}
    for _, row in sl.iterrows():
        v, s = row["true_vehicle_id"], row["split"]
        assert seen.get(v, s) == s
        seen[v] = s

    # every split value is valid
    assert set(sl["split"]).issubset({"train", "val", "test"})


def test_graph_and_vehicle_exports_are_leakage_safe(tmp_path):
    """The GNN/sequence exports and the vehicle-level table must carry no ground-truth columns."""
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=11))
    featurize.build(out, split_seed=5)
    ml = os.path.join(out, "ml")

    for name in ("vehicle_features", "graph_edges", "subject_windows"):
        df = pd.read_parquet(os.path.join(ml, f"{name}.parquet"))
        assert find_forbidden_keys({c: 0 for c in df.columns if c != "split"}) == [], name

    # vehicle features keyed by an OPAQUE entity id (never the true vehicle id)
    vf = pd.read_parquet(os.path.join(ml, "vehicle_features.parquet"))
    assert "entity_id" in vf.columns and "true_vehicle_id" not in vf.columns
    assert vf["entity_id"].str.startswith("ent_").all()

    # the report graph references only opaque entities
    ge = pd.read_parquet(os.path.join(ml, "graph_edges.parquet"))
    if len(ge):
        assert ge["src_entity"].str.startswith("ent_").all()
        assert ge["dst_entity"].str.startswith("ent_").all()


def test_ma_linked_vehicle_table_is_leakage_safe_and_realistic(tmp_path):
    """The MA-linked vehicle table groups pseudonyms by MA-visible co-revocation only (no oracle
    map), is keyed by an opaque id, and its benchmark AUC is reported with a linkage-quality metric."""
    from scms_sim_ref.datagen import benchmark

    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=13))
    featurize.build(out, split_seed=5)
    ml = os.path.join(out, "ml")

    vfm = pd.read_parquet(os.path.join(ml, "vehicle_features_ma.parquet"))
    vlm = pd.read_parquet(os.path.join(ml, "vehicle_labels_ma.parquet"))
    # leakage-safe features, opaque id (never the true id)
    assert find_forbidden_keys({c: 0 for c in vfm.columns if c != "split"}) == []
    assert "entity_id" in vfm.columns and "true_vehicle_id" not in vfm.columns
    assert vfm["entity_id"].str.startswith("ment_").all()
    # each MA entity links >=1 true vehicle; purity column present for honest reporting
    assert "link_true_id_count" in vlm.columns
    assert (vlm["link_true_id_count"] >= 1).all()

    r = benchmark.run(out)
    lq = r.get("linkage_quality")
    assert lq is not None and 0.0 <= lq["entity_purity"] <= 1.0


def test_featurize_is_deterministic(tmp_path):
    """Same inputs + split seed -> identical feature tables."""
    out = str(tmp_path / "run")
    run_pipeline(PipelineConfig(out_dir=out, seed=3))
    featurize.build(out, split_seed=7)
    a = pd.read_parquet(os.path.join(out, "ml", "vehicle_features.parquet"))
    featurize.build(out, split_seed=7)
    b = pd.read_parquet(os.path.join(out, "ml", "vehicle_features.parquet"))
    assert a.equals(b)
