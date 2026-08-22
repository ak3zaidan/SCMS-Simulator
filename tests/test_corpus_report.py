"""Tests for scms_sim_ref.datagen.corpus_report.

Uses small SYNTHETIC corpora written to disk (a few vehicle_labels/report_labels rows + a
domain_catalog.json) so the tests are fast and deterministic. The attack family/type SPACE is
imported from the module under test (which imports it from the engine), so these tests exercise the
same single source of truth the tool uses -- never a hardcoded list.
"""
from __future__ import annotations

import json
import os

import pandas as pd
import pytest

from scms_sim_ref.datagen.corpus_report import (
    ALL_FAMILIES, ALL_TYPES, OPT_IN_FAMILIES, TYPE_TO_FAMILY,
    build_report, render_markdown, main,
)


# --------------------------------------------------------------------------------------------------
# synthetic-corpus builders
# --------------------------------------------------------------------------------------------------
def _write_corpus(root, vehicle_rows, report_rows, catalog=None, manifest=None):
    ml = os.path.join(root, "ml")
    os.makedirs(ml, exist_ok=True)
    pd.DataFrame(vehicle_rows).to_csv(os.path.join(ml, "vehicle_labels.csv"), index=False)
    if report_rows is not None:
        pd.DataFrame(report_rows).to_csv(os.path.join(ml, "report_labels.csv"), index=False)
    if catalog is not None:
        with open(os.path.join(root, "domain_catalog.json"), "w", encoding="utf-8") as fh:
            json.dump(catalog, fh)
    if manifest is not None:
        with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8") as fh:
            json.dump(manifest, fh)
    return root


def _balanced_corpus(root):
    """A well-balanced, fully-covered corpus: every family + every type present, 3 topologies,
    events, wide difficulty spread, even per-domain rows -> should produce ZERO warnings."""
    veh = []
    i = 0
    # 60 benign, 6 faulty, 3 attackers per family (all 8 families) -> 90 vehicles
    for _ in range(60):
        veh.append({"domain_id": i % 3, "entity_id": f"ent_{i}", "label_is_attacker": 0,
                    "label_is_faulty": 0, "attack_family": "none", "true_vehicle_id": f"v{i}"})
        i += 1
    for _ in range(6):
        veh.append({"domain_id": i % 3, "entity_id": f"ent_{i}", "label_is_attacker": 0,
                    "label_is_faulty": 1, "attack_family": "none", "true_vehicle_id": f"v{i}"})
        i += 1
    for fam in ALL_FAMILIES:
        for _ in range(3):
            veh.append({"domain_id": i % 3, "entity_id": f"ent_{i}", "label_is_attacker": 1,
                        "label_is_faulty": 0, "attack_family": fam, "true_vehicle_id": f"v{i}"})
            i += 1

    # even per-domain report rows (30 each) to avoid a dominance warning; ~30% target an attacker
    reports = []
    for d in range(3):
        for k in range(30):
            reports.append({"domain_id": d, "report_id": f"d{d}_r{k}",
                            "label_subject_is_attacker": int(k < 9)})

    # recall_by_type across the 3 domains unions to ALL_TYPES; each family scored every domain
    chunks = [ALL_TYPES[j::3] for j in range(3)]
    recalls = [0.35, 0.65, 0.95]            # wide spread (0.6) -> spans easy->hard
    topos = ["grid", "ring", "spider"]      # 3 distinct topologies
    events = [[{"t": 10.0, "type": "attack_wave"}], [], [{"t": 5.0, "type": "demand", "mult": 2.0}]]
    catalog = []
    for d in range(3):
        catalog.append({
            "domain_id": d, "scenario": "ALL", "road_network": topos[d], "events": events[d],
            "precision": 0.8 + 0.05 * d, "recall": recalls[d],
            "recall_by_family": {fam: recalls[d] for fam in ALL_FAMILIES},
            "recall_by_type": {t: {"recall": recalls[d], "n": 4} for t in chunks[d]},
        })
    manifest = {"grid": "synthetic", "seed": 1, "n_domains_ok": 3, "n_domains_failed": 0,
                "row_counts": {"vehicle_labels": len(veh), "report_labels": len(reports)}}
    return _write_corpus(root, veh, reports, catalog, manifest)


def _imbalanced_corpus(root):
    """Deliberately broken: 0 faulty, a single topology, only the 'position' family / 'ConstPos'
    type present -> must emit several warnings (incl. the opt-in 'combined' family gap)."""
    veh = []
    i = 0
    for _ in range(40):
        veh.append({"domain_id": i % 2, "entity_id": f"ent_{i}", "label_is_attacker": 0,
                    "label_is_faulty": 0, "attack_family": "none", "true_vehicle_id": f"v{i}"})
        i += 1
    for _ in range(10):     # attackers, all position family; NO faulty vehicles at all
        veh.append({"domain_id": i % 2, "entity_id": f"ent_{i}", "label_is_attacker": 1,
                    "label_is_faulty": 0, "attack_family": "position", "true_vehicle_id": f"v{i}"})
        i += 1
    reports = [{"domain_id": d, "report_id": f"d{d}_r{k}", "label_subject_is_attacker": int(k < 5)}
               for d in range(2) for k in range(20)]
    catalog = [
        {"domain_id": 0, "scenario": "ConstPos", "road_network": "grid",
         "events": [{"t": 5.0, "type": "demand", "mult": 2.0}], "precision": 0.9, "recall": 0.4,
         "recall_by_family": {"position": 0.4}, "recall_by_type": {"ConstPos": {"recall": 0.4, "n": 5}}},
        {"domain_id": 1, "scenario": "ConstPos", "road_network": "grid",  # SAME topology -> single topology
         "events": [], "precision": 0.9, "recall": 0.85,
         "recall_by_family": {"position": 0.85}, "recall_by_type": {"ConstPos": {"recall": 0.85, "n": 5}}},
    ]
    return _write_corpus(root, veh, reports, catalog)


# --------------------------------------------------------------------------------------------------
# family/type space is sourced from code (sanity on the imported single source of truth)
# --------------------------------------------------------------------------------------------------
def test_family_type_space_is_from_code():
    # ~25 renderable types, 8 families (7 base + the opt-in "combined")
    assert len(ALL_TYPES) >= 24
    assert "combined" in ALL_FAMILIES
    assert OPT_IN_FAMILIES == ("combined",)
    assert len(ALL_FAMILIES) == 8
    assert TYPE_TO_FAMILY["ConstPos"] == "position"
    assert TYPE_TO_FAMILY["Sybil"] == "identity"
    # every combined attack maps to the opt-in family
    assert all(TYPE_TO_FAMILY[t] == "combined" for t in ALL_TYPES if t in
               ("Disruptive", "PosSpeedInconsistent", "PosHeadingInconsistent", "EventualStop"))


# --------------------------------------------------------------------------------------------------
# class balance ratios
# --------------------------------------------------------------------------------------------------
def test_class_balance_ratios(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    vb = rep["class_balance"]["vehicle"]
    assert vb["total"] == 90
    assert (vb["benign"], vb["attacker"], vb["faulty"]) == (60, 24, 6)
    assert vb["attacker_frac"] == round(24 / 90, 4)
    assert vb["faulty_frac"] == round(6 / 90, 4)
    assert vb["attacker_benign_ratio"] == round(24 / 60, 4)
    # report-level: 27 attacker reports of 90
    rl = rep["class_balance"]["report"]
    assert rl["total"] == 90 and rl["attacker"] == 27


# --------------------------------------------------------------------------------------------------
# attack coverage: present vs absent families/types
# --------------------------------------------------------------------------------------------------
def test_coverage_full_when_balanced(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    fams = rep["attack_coverage"]["families"]
    assert set(fams["present"]) == set(ALL_FAMILIES)
    assert fams["absent"] == []
    types = rep["attack_coverage"]["types"]
    assert types["available"] is True
    assert set(types["present"]) == set(ALL_TYPES)
    assert types["absent"] == []


def test_coverage_gaps_when_imbalanced(tmp_path):
    root = _imbalanced_corpus(str(tmp_path / "imbalanced"))
    rep = build_report(root)
    fams = rep["attack_coverage"]["families"]
    assert fams["present"] == ["position"]
    assert "combined" in fams["absent"]
    assert "identity" in fams["absent"]
    types = rep["attack_coverage"]["types"]
    assert types["present"] == ["ConstPos"]
    assert "Sybil" in types["absent"]
    assert types["n_present"] == 1 and types["n_total"] == len(ALL_TYPES)


# --------------------------------------------------------------------------------------------------
# the opt-in "combined" family: warned when absent, silent when present
# --------------------------------------------------------------------------------------------------
def test_combined_family_absent_is_warned(tmp_path):
    root = _imbalanced_corpus(str(tmp_path / "imbalanced"))
    rep = build_report(root)
    assert any("combined" in w for w in rep["warnings"])


def test_combined_family_present_is_not_warned(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    assert not any("combined" in w for w in rep["warnings"])


# --------------------------------------------------------------------------------------------------
# domain diversity + difficulty spread
# --------------------------------------------------------------------------------------------------
def test_domain_diversity_and_difficulty(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    dd = rep["domain_diversity"]
    assert dd["available"] is True
    assert dd["n_domains"] == 3
    assert dd["topologies"] == {"grid": 1, "ring": 1, "spider": 1}
    assert dd["n_topologies"] == 3
    assert dd["event_domains"] == 2
    ds = rep["difficulty_spread"]
    assert ds["available"] is True
    assert ds["recall"]["min"] == 0.35 and ds["recall"]["max"] == 0.95
    assert ds["recall"]["spread"] == 0.6


def test_single_topology_warned(tmp_path):
    root = _imbalanced_corpus(str(tmp_path / "imbalanced"))
    rep = build_report(root)
    assert rep["domain_diversity"]["n_topologies"] == 1
    assert any("one road-network topology" in w for w in rep["warnings"])


# --------------------------------------------------------------------------------------------------
# warnings: none for balanced, several for imbalanced
# --------------------------------------------------------------------------------------------------
def test_balanced_has_no_warnings(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    assert rep["warnings"] == [], f"unexpected warnings: {rep['warnings']}"


def test_imbalanced_emits_warnings(tmp_path):
    root = _imbalanced_corpus(str(tmp_path / "imbalanced"))
    rep = build_report(root)
    joined = " || ".join(rep["warnings"])
    assert rep["warnings"], "expected warnings for a deliberately imbalanced corpus"
    assert "faulty" in joined                       # 0 faulty flagged
    assert "one road-network topology" in joined    # single topology flagged
    assert any("types ABSENT" in w for w in rep["warnings"])


# --------------------------------------------------------------------------------------------------
# size section
# --------------------------------------------------------------------------------------------------
def test_size_section(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    rep = build_report(root)
    sz = rep["size"]
    assert sz["n_domains"] == 3
    assert sz["row_counts"]["vehicle_labels"] == 90
    # even per-domain rows -> min == median == max, no dominance warning
    assert sz["per_domain_rows"]["max"] == sz["per_domain_rows"]["min"]


# --------------------------------------------------------------------------------------------------
# graceful degradation: no domain_catalog / no manifest
# --------------------------------------------------------------------------------------------------
def test_degrades_without_catalog(tmp_path):
    root = str(tmp_path / "nocatalog")
    veh = [{"domain_id": 0, "entity_id": "e0", "label_is_attacker": 0, "label_is_faulty": 0,
            "attack_family": "none", "true_vehicle_id": "v0"},
           {"domain_id": 0, "entity_id": "e1", "label_is_attacker": 1, "label_is_faulty": 0,
            "attack_family": "position", "true_vehicle_id": "v1"}]
    reports = [{"domain_id": 0, "report_id": "r0", "label_subject_is_attacker": 1}]
    _write_corpus(root, veh, reports, catalog=None, manifest=None)
    rep = build_report(root)                          # must not raise
    assert rep["has_domain_catalog"] is False
    assert rep["has_manifest"] is False
    assert rep["attack_coverage"]["types"]["available"] is False
    assert rep["domain_diversity"]["available"] is False
    assert rep["difficulty_spread"]["available"] is False
    # family coverage still works from the ml table
    assert "position" in rep["attack_coverage"]["families"]["present"]
    assert any("type coverage" in w.lower() for w in rep["warnings"])


# --------------------------------------------------------------------------------------------------
# markdown rendering
# --------------------------------------------------------------------------------------------------
def test_markdown_has_section_headers(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    md = render_markdown(build_report(root))
    for header in ("# Corpus balance & coverage report", "## Class balance", "## Attack coverage",
                   "## Domain diversity", "## Difficulty spread", "## Size", "## Warnings"):
        assert header in md


def test_markdown_renders_for_degraded_corpus(tmp_path):
    root = str(tmp_path / "nocatalog")
    veh = [{"domain_id": 0, "entity_id": "e0", "label_is_attacker": 0, "label_is_faulty": 0,
            "attack_family": "none", "true_vehicle_id": "v0"}]
    _write_corpus(root, veh, report_rows=None, catalog=None, manifest=None)
    md = render_markdown(build_report(root))          # must not raise on missing sections
    assert "## Domain diversity" in md and "Unavailable" in md


# --------------------------------------------------------------------------------------------------
# determinism + read-only guarantee
# --------------------------------------------------------------------------------------------------
def test_deterministic(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    assert build_report(root) == build_report(root)


def _snapshot(root):
    snap = {}
    for dirpath, _dirs, files in os.walk(root):
        for f in files:
            p = os.path.join(dirpath, f)
            st = os.stat(p)
            snap[os.path.relpath(p, root)] = (st.st_size, st.st_mtime_ns)
    return snap


def test_read_only_does_not_modify_corpus(tmp_path):
    root = _balanced_corpus(str(tmp_path / "balanced"))
    before = _snapshot(root)
    build_report(root)
    render_markdown(build_report(root))
    after = _snapshot(root)
    assert before == after                           # no files added/removed/rewritten


# --------------------------------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------------------------------
def test_cli_writes_out_and_exit_codes(tmp_path, capsys):
    balanced = _balanced_corpus(str(tmp_path / "balanced"))
    imbalanced = _imbalanced_corpus(str(tmp_path / "imbalanced"))
    out_path = str(tmp_path / "report.md")           # OUTSIDE the corpus dir

    rc = main(["--corpus", balanced, "--out", out_path])
    assert rc == 0                                   # balanced -> no warnings -> exit 0
    assert os.path.exists(out_path)
    with open(out_path, encoding="utf-8") as fh:
        assert "# Corpus balance & coverage report" in fh.read()
    # writing --out must not have touched the corpus itself
    assert not os.path.exists(os.path.join(balanced, "report.md"))

    rc2 = main(["--corpus", imbalanced])
    assert rc2 == 1                                  # warnings -> exit 1
