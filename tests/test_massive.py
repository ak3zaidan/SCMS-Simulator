"""Test the factorial grid campaign: enumeration, merge, domain_id, leakage-safety."""

import json

import pandas as pd

from scms_sim_ref.datagen import massive
from scms_sim_ref.datagen.leakage_linter import find_forbidden_keys


def test_enumerate_cells_is_full_cartesian_product():
    grid = {"scenario": ["ConstPos", "ALL"], "weather": ["clear", "rain"],
            "rotate_period_s": [0.0], "collude_pct": [0.0], "faulty_pct": [0.0],
            "attacker_pct": [0.2], "n_vehicles": [40]}
    cells = massive.enumerate_cells(grid)
    assert len(cells) == 2 * 2, "product of the axis sizes"
    assert {c["scenario"] for c in cells} == {"ConstPos", "ALL"}


def test_massive_grid_merges_with_domain_id_and_is_leakage_safe(tmp_path, monkeypatch):
    # a tiny 4-cell grid so the test is fast but exercises the full merge path
    tiny = {"scenario": ["ConstPos", "ALL"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.0], "attacker_pct": [0.25], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)
    out = tmp_path / "massive"
    rc = massive.main(["--grid", "tiny", "--seed", "3", "--steps", "50", "--out", str(out)])
    assert rc == 0

    man = json.loads((out / "manifest.json").read_text())
    assert man["n_domains_run"] == 2 and man["n_cells_in_product"] == 2

    vf = pd.read_csv(out / "ml" / "vehicle_features.csv")
    assert "domain_id" in vf.columns and vf["domain_id"].nunique() == 2
    # entity ids are namespaced per domain so they never collide across the merge
    assert vf["entity_id"].str.startswith("d").all()
    # merged FEATURE tables carry no ground-truth column
    for tbl in ("report_features", "subject_features", "vehicle_features", "vehicle_features_ma"):
        cols = [c for c in pd.read_csv(out / "ml" / f"{tbl}.csv").columns
                if c not in ("split", "time_split", "domain_id")]
        assert find_forbidden_keys({c: 0 for c in cols}) == [], tbl

    # per-domain dirs are cleaned up by default; catalog records every cell
    assert not (out / "domains").exists()
    cat = json.loads((out / "domain_catalog.json").read_text())
    assert len(cat) == 2 and {c["scenario"] for c in cat} == {"ConstPos", "ALL"}
    # each domain carries difficulty labels (precision/recall/family recall) for curriculum/stratified use
    for c in cat:
        assert "recall" in c and "precision" in c and "recall_by_family" in c
        assert "recall_by_type" in c, "per-domain labels include per-attack-type difficulty"
        assert c["recall"] is None or 0.0 <= c["recall"] <= 1.0


def test_medium_grid_is_valid_and_between_quick_and_full(tmp_path):
    from scms_sim_ref.mock_pipeline.run import ATTACK_CATALOG
    for g in ("quick", "medium", "full"):
        for s in massive.GRIDS[g]["scenario"]:
            assert s == "ALL" or s in ATTACK_CATALOG, f"{g} has invalid scenario {s}"
    n = {g: len(massive.enumerate_cells(massive.GRIDS[g])) for g in ("quick", "medium", "full")}
    assert n["quick"] < n["medium"] < n["full"]
    # the medium grid runs end-to-end (capped to keep the test fast)
    out = tmp_path / "med"
    rc = massive.main(["--grid", "medium", "--max-domains", "2", "--steps", "40",
                       "--seed", "5", "--out", str(out)])
    assert rc == 0 and (out / "manifest.json").exists()


def test_massive_dry_run_writes_nothing(tmp_path):
    """--dry-run previews the plan and exits without creating the output directory."""
    out = tmp_path / "dry"
    rc = massive.main(["--grid", "medium", "--dry-run", "--out", str(out)])
    assert rc == 0
    assert not out.exists(), "dry-run must not write any output"


def test_massive_isolates_a_failing_domain(tmp_path, monkeypatch):
    """One bad cell must not abort the campaign: it is recorded in failed_domains and the rest merge."""
    tiny = {"scenario": ["ConstPos", "RandomSpeed", "ALL"], "weather": ["clear"],
            "rotate_period_s": [0.0], "collude_pct": [0.0], "faulty_pct": [0.0],
            "attacker_pct": [0.25], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)

    real = massive.run_pipeline
    def boom(cfg):                                   # make domain 1 explode, others run normally
        if "d0001" in str(cfg.out_dir):
            raise RuntimeError("synthetic domain failure")
        return real(cfg)
    monkeypatch.setattr(massive, "run_pipeline", boom)

    out = tmp_path / "massive"
    rc = massive.main(["--grid", "tiny", "--seed", "3", "--steps", "50", "--out", str(out)])
    assert rc == 0                                   # campaign still succeeds (some domains merged)

    man = json.loads((out / "manifest.json").read_text())
    assert man["n_domains_failed"] == 1 and man["n_domains_ok"] == 2
    assert man["failed_domains"][0]["domain_id"] == 1
    assert "synthetic domain failure" in man["failed_domains"][0]["error"]
    vf = pd.read_csv(out / "ml" / "vehicle_features.csv")
    assert set(vf["domain_id"].unique()) == {0, 2}, "only the successful domains are merged"


def test_massive_flow_domains_are_routed_simulations(tmp_path, monkeypatch):
    """--flow makes each domain a long routed car-following sim; demand becomes an axis."""
    tiny = {"scenario": ["ConstPos", "ALL"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.05], "attacker_pct": [0.2], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)
    out = tmp_path / "mflow"
    rc = massive.main(["--grid", "tiny", "--flow", "--flow-duration", "120", "--max-domains", "3",
                       "--sample", "--seed", "5", "--out", str(out)])
    assert rc == 0
    cat = json.loads((out / "domain_catalog.json").read_text())
    assert all("demand" in c for c in cat), "flow domains carry a demand profile"
    assert all("duty" in c for c in cat), "flow domains span the pulsed-attack difficulty axis"
    vf = pd.read_csv(out / "ml" / "vehicle_features.csv")
    assert "domain_id" in vf.columns and len(vf) > 0
