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
    vf = pd.read_csv(out / "ml" / "vehicle_features.csv")
    assert "domain_id" in vf.columns and len(vf) > 0
