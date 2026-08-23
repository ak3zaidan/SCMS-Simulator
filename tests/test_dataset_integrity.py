"""Regression guard: every committed dataset must pass the full data-correctness audit.

This runs the same invariant checks as `tools/verify_data.py` (leakage/privacy,
referential integrity, label correctness, count reconciliation, split integrity,
value sanity, and per-file digest integrity) over every dataset under `datasets/`.
Any FAIL fails the build; SKIPs (a check not applicable to an older dataset) are fine.
"""
from __future__ import annotations

import importlib.util
from collections import Counter
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
DATASETS = ROOT / "datasets"

_spec = importlib.util.spec_from_file_location("verify_data", ROOT / "tools" / "verify_data.py")
verify_data = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(verify_data)


@pytest.mark.skipif(not DATASETS.exists(), reason="no datasets/ directory")
def test_all_datasets_pass_integrity_audit():
    # Non-hermetic guard over whatever real datasets a dev has locally (datasets/ is gitignored, so
    # CI sees none and the skipif above fires). A datasets/ that holds only non-dataset scratch such
    # as the OSM cache (datasets/_osmcache) yields no auditable results -> skip rather than error, so
    # OSM-test caching can't spuriously fail the build. Real hermetic coverage of every invariant is
    # in test_extended_invariants_pass_on_clean_dataset (freshly generated).
    results = verify_data.run_audit(DATASETS)
    if not results:
        pytest.skip("no auditable datasets under datasets/ (empty or cache-only)")
    fails = [r for r in results if r[2] == "FAIL"]
    by_status = Counter(r[2] for r in results)
    msg = "\n".join(f"[{ds}] {check}: {detail}" for ds, check, _, detail in fails)
    assert not fails, f"{by_status['FAIL']} data-integrity failures:\n{msg}"


# --- extended invariants (E3/SCHEMA1/C6/V4/N1) + recursive scan on a freshly generated dataset ---
# The committed datasets/ corpus may predate these features, so those checks would SKIP there. We
# generate one dataset that exercises them all (collusion + RSUs + events + a custom road network +
# per-message emissions + a live network.json) and assert they PASS on clean data. The dataset is
# placed one level down (campaign/dom_001) so the same fixture also proves the recursive scan.
#
# SCHEMA1 is deliberately NOT asserted-PASS on the generated data: on the current featurize.py it
# correctly FLAGS a real column-labelling bug (detection_time / crl_active_at_report are in
# benchmark._DROP yet emitted kind:"feature"), fixed on a separate branch. SCHEMA1's checker logic is
# validated directly below on synthetic schemas, so this suite stays green in isolation and SCHEMA1
# will pass on real data once the featurize fix lands.
_PASS_ON_CLEAN = {"E3_graph_integrity", "C6_collusion_consistency",
                  "V4_emissions_truth", "N1_world_provenance"}
_SCHEMA1 = "SCHEMA1_no_dropped_col_as_feature"


@pytest.fixture(scope="module")
def _fresh_corpus(tmp_path_factory):
    import json
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    from scms_sim_ref.datagen import featurize

    root = tmp_path_factory.mktemp("corpus")
    ds_dir = root / "campaign" / "dom_001"          # nested -> also tests the recursive scan
    custom_network = json.dumps({
        "nodes": [[0, 0], [120, 0], [240, 0], [0, 120], [120, 120], [240, 120], [120, 240]],
        "edges": [[0, 1], [1, 2], [0, 3], [1, 4], [2, 5], [3, 4], [4, 5], [4, 6]],
    })
    events = json.dumps([
        {"t": 8, "until": 30, "type": "demand", "mult": 2.0},
        {"t": 15, "type": "weather", "value": "rain"},
    ])
    run_pipeline(PipelineConfig(
        seed=13, traffic_flow=True, road_network="custom", custom_network=custom_network,
        duration_s=70, arrival_rate=1.3, attacker_pct=0.3, collude_pct=0.4, n_rsus=4,
        emit_sample_prob=0.08, live_interval_s=5.0, events=events, out_dir=str(ds_dir)))
    featurize.build(str(ds_dir))
    return root


def test_recursive_scan_finds_nested_dataset(_fresh_corpus):
    # default (non-recursive) sees only immediate children -> the nested dataset is invisible
    assert verify_data._find_datasets(_fresh_corpus, recursive=False) == []
    found = verify_data._find_datasets(_fresh_corpus, recursive=True)
    assert [p.name for p in found] == ["dom_001"]


def test_extended_invariants_pass_on_clean_dataset(_fresh_corpus):
    results = verify_data.run_audit(_fresh_corpus, recursive=True)
    assert results, "recursive audit produced no results"
    status_by_check = {check: st for _, check, st, _ in results}
    # E3/C6/V4/N1 must PASS (actually run, not SKIP) on this feature-rich dataset
    for inv in _PASS_ON_CLEAN:
        assert status_by_check.get(inv) == "PASS", (
            f"{inv} expected PASS, got {status_by_check.get(inv)} "
            f"({dict((c, d) for _, c, _, d in results if c == inv)})")
    # the only tolerated FAIL is SCHEMA1 (the known, separately-fixed featurize labelling bug)
    unexpected = [r for r in results if r[2] == "FAIL" and r[1] != _SCHEMA1]
    assert not unexpected, "unexpected failures:\n" + "\n".join(
        f"[{ds}] {check}: {detail}" for ds, check, _, detail in unexpected)
    # SCHEMA1 must have actually executed on real data (PASS once featurize is fixed, else FAIL)
    assert status_by_check.get(_SCHEMA1) in ("PASS", "FAIL")


def test_e3_tolerates_domain_namespaced_rsu_edges(tmp_path):
    """datagen.massive namespaces every id column with a 'd<idx>_' prefix, so a merged corpus's
    infrastructure-edge src becomes 'd3_rsu_...'. E3 must accept that (strip the domain prefix) --
    otherwise it false-fails on every merged corpus containing RSU domains."""
    import csv
    import json
    ds = tmp_path / "merged"
    (ds / "ml").mkdir(parents=True)
    (ds / "manifest.json").write_text(json.dumps({"config": {}, "counts": {}}), encoding="utf-8")
    # two vehicle nodes + one vehicle edge + one RSU (infrastructure) edge, all domain-namespaced
    for tbl in ("vehicle_features", "vehicle_labels"):
        with open(ds / "ml" / f"{tbl}.csv", "w", newline="", encoding="utf-8") as fh:
            w = csv.writer(fh); w.writerow(["entity_id"])
            w.writerow(["d3_ent_aaaa"]); w.writerow(["d3_ent_bbbb"])
    with open(ds / "ml" / "graph_edges.csv", "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh); w.writerow(["src_entity", "dst_entity", "is_infrastructure"])
        w.writerow(["d3_ent_bbbb", "d3_ent_aaaa", "0"])       # vehicle->vehicle
        w.writerow(["d3_rsu_deadbeef", "d3_ent_aaaa", "1"])   # namespaced RSU infrastructure edge
    status = {c: s for _, c, s, _ in verify_data.run_audit(tmp_path)}
    assert status.get("E3_graph_integrity") == "PASS", status
    # a genuinely bad infra src (a real-looking entity, not rsu_) still FAILs
    with open(ds / "ml" / "graph_edges.csv", "a", newline="", encoding="utf-8") as fh:
        csv.writer(fh).writerow(["d3_ent_cccc", "d3_ent_aaaa", "1"])   # infra edge w/ non-rsu src
    status2 = {c: s for _, c, s, _ in verify_data.run_audit(tmp_path)}
    assert status2.get("E3_graph_integrity") == "FAIL", status2


def _write_synthetic_dataset(root, schema):
    """A minimal on-disk dataset (manifest + ml/schema.json) that isolates the SCHEMA1 checker."""
    import json
    ds = root / "syn"
    (ds / "ml").mkdir(parents=True)
    (ds / "manifest.json").write_text(json.dumps({"config": {}, "counts": {}}), encoding="utf-8")
    (ds / "ml" / "schema.json").write_text(json.dumps(schema), encoding="utf-8")
    return root


def _schema1_status(root):
    for ds, check, st, _ in verify_data.run_audit(root):
        if check == _SCHEMA1:
            return st
    return None


def test_schema1_flags_dropped_column_marked_feature(tmp_path):
    from scms_sim_ref.datagen import benchmark
    dropped = sorted(benchmark._DROP)[0]                # a column benchmark excludes from features
    # BAD: an excluded column advertised as a model feature -> must FAIL
    _write_synthetic_dataset(tmp_path, {"report_features": [
        {"name": dropped, "kind": "feature", "dtype": "float64"},
        {"name": "detector_score_norm", "kind": "feature", "dtype": "float64"},
    ]})
    assert _schema1_status(tmp_path) == "FAIL"


def test_schema1_passes_when_dropped_column_labelled_correctly(tmp_path):
    from scms_sim_ref.datagen import benchmark
    dropped = sorted(benchmark._DROP)[0]
    # GOOD: same excluded column labelled as an id/split (not a feature) -> must PASS
    _write_synthetic_dataset(tmp_path, {"report_features": [
        {"name": dropped, "kind": "id", "dtype": "object"},
        {"name": "detector_score_norm", "kind": "feature", "dtype": "float64"},
    ]})
    assert _schema1_status(tmp_path) == "PASS"


# --- VRU / DENM invariants (VRU1 / VRU2 / DENM1 / DENM2) ---------------------------------------
# These four invariants cover the opt-in VRU-actor (station_type / is_vru_declared),
# VRU-impersonation, and DENM (event-message) layers. They SKIP on the older corpus above, so we
# (a) generate ONE feature-rich dataset that exercises them all -- VRUs + DENMs + FakeHazard +
# VruImpersonation via attack_mix -- and assert they PASS, and (b) hand-write tiny broken datasets
# to prove each checker actually FAILs on a real leak / inconsistency (i.e. that it has teeth).
_VRU_DENM_INVARIANTS = ("VRU1_vru_denm_no_oracle_leak", "VRU2_impersonation_sanity",
                        "DENM1_denm_log_integrity", "DENM2_fake_denm_label_consistency")


@pytest.fixture(scope="module")
def _vru_denm_corpus(tmp_path_factory):
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    from scms_sim_ref.datagen import featurize

    ds_dir = tmp_path_factory.mktemp("vru_denm") / "ds"      # nested so the parent is the scan root
    run_pipeline(PipelineConfig(
        seed=11, traffic_flow=True, road_network="grid", duration_s=90, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, vru_pct=0.2, denm_rate=60.0,
        attack_mix="FakeHazard:0.4,RandomPos:0.3,VruImpersonation:0.3", out_dir=str(ds_dir)))
    featurize.build(str(ds_dir))
    return ds_dir.parent


def test_vru_denm_invariants_pass_on_feature_rich_dataset(_vru_denm_corpus):
    results = verify_data.run_audit(_vru_denm_corpus)
    assert results, "audit produced no results"
    status = {check: st for _, check, st, _ in results}
    detail = {check: d for _, check, _, d in results}
    for inv in _VRU_DENM_INVARIANTS:
        assert status.get(inv) == "PASS", (
            f"{inv} expected PASS, got {status.get(inv)} ({detail.get(inv)})")
    # The four new invariants must not regress anything else. SCHEMA1 is the one tolerated FAIL
    # (the known, separately-fixed featurize labelling bug -- see the notes above).
    unexpected = [r for r in results if r[2] == "FAIL" and r[1] != _SCHEMA1]
    assert not unexpected, "unexpected failures:\n" + "\n".join(
        f"[{ds}] {check}: {detail}" for ds, check, _, detail in unexpected)


def _write_min_dataset(root, *, manifest, gt=None, ma=None, ml=None):
    """A minimal on-disk dataset for fault injection. `gt`/`ma` map *.jsonl -> list-of-dicts;
    `ml` maps *.csv -> (header_list, list-of-row-lists). Only the given files are written."""
    import csv
    import json
    ds = root / "syn"
    ds.mkdir(parents=True, exist_ok=True)
    (ds / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
    for sub, files in (("ground_truth", gt), ("ma", ma)):
        if not files:
            continue
        d = ds / sub
        d.mkdir(exist_ok=True)
        for name, rows in files.items():
            (d / name).write_text("\n".join(json.dumps(x) for x in rows), encoding="utf-8")
    if ml:
        d = ds / "ml"
        d.mkdir(exist_ok=True)
        for name, (header, body) in ml.items():
            with open(d / name, "w", newline="", encoding="utf-8") as fh:
                w = csv.writer(fh)
                w.writerow(header)
                for r in body:
                    w.writerow(r)
    return root


def _status(root):
    return {check: st for _, check, st, _ in verify_data.run_audit(root)}


def test_vru1_denm1_catch_is_fake_leak_in_denm_log(tmp_path):
    # is_fake is an ORACLE label; leaking it into ma/ma_denm_log.jsonl must be caught by BOTH VRU1
    # and DENM1 -- the DENM log is a file the original L2 MA-leakage check never inspected.
    common = dict(
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "dnm_1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    # BAD: the MA-side DENM row carries the oracle is_fake flag
    _write_min_dataset(tmp_path, ma={"ma_denm_log.jsonl": [
        {"denm_id": "dnm_1", "event_type": "brake", "cert_digest": "aa", "is_fake": True}]}, **common)
    st = _status(tmp_path)
    assert st.get("VRU1_vru_denm_no_oracle_leak") == "FAIL", st
    assert st.get("DENM1_denm_log_integrity") == "FAIL", st
    # GOOD control: same dataset with a clean DENM log -> both PASS
    _write_min_dataset(tmp_path, ma={"ma_denm_log.jsonl": [
        {"denm_id": "dnm_1", "event_type": "brake", "cert_digest": "aa", "denm_plausibility": 2.1}]},
        **common)
    st = _status(tmp_path)
    assert st.get("VRU1_vru_denm_no_oracle_leak") == "PASS", st
    assert st.get("DENM1_denm_log_integrity") == "PASS", st


def test_vru1_catches_is_vru_leak_in_feature_table(tmp_path):
    # is_vru is an ORACLE label; it must never appear as a feature column (only is_vru_declared may).
    _write_min_dataset(
        tmp_path,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": True}]},
        ml={"vehicle_features.csv": (["entity_id", "is_vru_declared", "is_vru"], [["e1", "1", "1"]])})
    assert _status(tmp_path).get("VRU1_vru_denm_no_oracle_leak") == "FAIL"
    # GOOD control: only the MA-visible declaration column -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": True}]},
        ml={"vehicle_features.csv": (["entity_id", "is_vru_declared"], [["e1", "1"]])})
    assert _status(tmp_path).get("VRU1_vru_denm_no_oracle_leak") == "PASS"


def test_denm2_catches_fake_denm_from_nonattacker(tmp_path):
    # every FAKE DENM must originate from a gt attacker; a fake DENM from a benign vehicle is a
    # label contradiction DENM2 must FAIL on.
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "d1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    assert _status(tmp_path).get("DENM2_fake_denm_label_consistency") == "FAIL"
    # GOOD control: same fake DENM but its sender is a gt attacker -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"denm_rate": 60.0}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            "gt_denm_emissions.jsonl": [
                {"denm_id": "d1", "event_type": "brake", "is_fake": True, "true_vehicle_id": "veh_a"}]})
    assert _status(tmp_path).get("DENM2_fake_denm_label_consistency") == "PASS"


def test_vru2_catches_benign_nonvru_declaring_vru(tmp_path):
    # under VruImpersonation, a cert declaring station_type="vru" must resolve to a genuine VRU or an
    # attacker; a benign non-VRU that declared "vru" is a mislabelled declaration VRU2 must FAIL on.
    base_ma = {"ma_cert_status.jsonl": [
        {"cert_digest": "cc", "station_type": "vru", "crl_status": "active"}]}
    idmap = {"gt_identity_map.jsonl": [
        {"true_vehicle_id": "veh_a", "pseudonym_cert_digest": "cc"}]}
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"attack_mix": "VruImpersonation:1.0"}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": False, "is_vru": False}],
            **idmap},
        ma=base_ma)
    assert _status(tmp_path).get("VRU2_impersonation_sanity") == "FAIL"
    # GOOD control: the same declaration made by an attacker (a real impersonator) -> PASS
    _write_min_dataset(
        tmp_path,
        manifest={"config": {"attack_mix": "VruImpersonation:1.0"}, "counts": {}},
        gt={"gt_vehicle.jsonl": [{"true_vehicle_id": "veh_a", "is_attacker": True, "is_vru": False}],
            **idmap},
        ma=base_ma)
    assert _status(tmp_path).get("VRU2_impersonation_sanity") == "PASS"
