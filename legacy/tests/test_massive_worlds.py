"""Randomized-worlds corpus axes: per-domain sampled topology + scenario-event timelines.

Covers (a) a tiny --flow corpus whose domains span multiple topologies and carry
road_network/dims/events provenance in the catalog, (b) byte-reproducible world sampling,
and (c) the DATASHEET.md "Scenario provenance" section for event-driven datasets.
"""

import json

import pandas as pd

from scms_sim_ref.datagen import datasheet, massive
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.run import _parse_events


# ---------------------------------------------------------------------------- #
# (b) sampling: determinism + constraints (pure, no simulation -> fast)
# ---------------------------------------------------------------------------- #

def test_sample_world_is_deterministic_and_within_constraints():
    dur = 300.0
    worlds = [massive.sample_world(5, i, dur) for i in range(40)]
    again = [massive.sample_world(5, i, dur) for i in range(40)]
    # byte-reproducible: an identical (seed, idx) re-enumeration yields identical params
    assert json.dumps(worlds, sort_keys=True) == json.dumps(again, sort_keys=True)

    assert len({w["road"] for w in worlds}) == 3, "grid, ring and spider all appear"
    eventful = 0
    for w in worlds:
        assert w["road"] in massive.WORLD_TOPOLOGIES
        if w["road"] == "grid":
            assert 4 <= w["grid_w"] <= 8 and 4 <= w["grid_h"] <= 8
        elif w["road"] == "ring":
            assert w["grid_w"] >= 8, "ring wants at least 8 loop intersections"
        else:  # spider
            assert 4 <= w["grid_w"] <= 8 and 2 <= w["grid_h"] <= 4, "4-8 arms x 2-4 rings"
        assert w["grid_block_m"] > 0
        # every generated timeline passes the pipeline's own validator
        parsed = _parse_events(json.dumps(w["events"]))
        assert len(parsed) <= 2
        eventful += bool(w["events"])
        for e in w["events"]:
            assert 0 <= e["t"] < dur, "event start inside the domain duration"
            if e.get("until") is not None:
                assert e["t"] < e["until"] <= dur, "event end inside the domain duration"
            if e["type"] == "close_edge":
                assert w["road"] == "grid", "road closures only on grid domains"
                (i1, j1), (i2, j2) = e["edge"]
                assert abs(i1 - i2) + abs(j1 - j2) == 1, "adjacent intersections"
                assert 0 <= i1 < w["grid_w"] and 0 <= i2 < w["grid_w"]
                assert 0 <= j1 < w["grid_h"] and 0 <= j2 < w["grid_h"]
    # roughly half the domains draw a timeline; timelines of 0 events thin that out further,
    # so just require a healthy deterministic mix of eventful and quiet worlds
    assert 0 < eventful < len(worlds)

    # a different corpus seed samples a different world sequence (the axis actually varies)
    other = [massive.sample_world(6, i, dur) for i in range(40)]
    assert json.dumps(worlds, sort_keys=True) != json.dumps(other, sort_keys=True)


def test_sample_world_spans_multiple_topologies_across_a_handful_of_indices():
    roads = {massive.sample_world(5, i, 80.0)["road"] for i in range(8)}
    assert len(roads) > 1, "a handful of consecutive domains must not share one topology"


# ---------------------------------------------------------------------------- #
# (a) a tiny flow corpus end-to-end: catalog provenance + valid merged output
# ---------------------------------------------------------------------------- #

def test_flow_corpus_samples_worlds_with_catalog_provenance(tmp_path, monkeypatch):
    tiny = {"scenario": ["ConstPos", "ALL"], "weather": ["clear"], "rotate_period_s": [0.0],
            "collude_pct": [0.0], "faulty_pct": [0.05], "attacker_pct": [0.2], "n_vehicles": [40]}
    monkeypatch.setitem(massive.GRIDS, "tiny", tiny)
    out = tmp_path / "mworlds"
    # seed 5 -> domains 0..2 sample ring/spider/spider with events on domains 0 and 2
    rc = massive.main(["--grid", "tiny", "--flow", "--flow-duration", "80", "--max-domains", "3",
                       "--seed", "5", "--out", str(out)])
    assert rc == 0

    man = json.loads((out / "manifest.json").read_text())
    assert man["n_domains_failed"] == 0 and man["n_domains_ok"] == 3
    assert "road" not in man["axes"], "topology is a sampled world axis, not a grid axis"
    assert man["world_sampling"]["topologies"] == list(massive.WORLD_TOPOLOGIES)

    cat = json.loads((out / "domain_catalog.json").read_text())
    assert len(cat) == 3
    for c in cat:  # per-domain provenance: topology, dims, and the events list
        assert c["road_network"] in massive.WORLD_TOPOLOGIES
        assert c["grid_w"] > 0 and c["grid_h"] > 0 and c["grid_block_m"] > 0
        assert isinstance(c["events"], list)
        _parse_events(json.dumps(c["events"]))          # recorded events are valid timelines
    assert len({c["road_network"] for c in cat}) > 1, "the tiny corpus spans >1 topology"
    assert any(c["events"] for c in cat), "the EVENTS axis fires for some domains"
    # world params match a re-enumeration of the sampler (catalog is honest provenance)
    for c in cat:
        w = massive.sample_world(5, c["domain_id"], 80.0)
        assert (c["road_network"], c["grid_w"], c["grid_h"], c["grid_block_m"], c["events"]) \
            == (w["road"], w["grid_w"], w["grid_h"], w["grid_block_m"], w["events"])

    # the merged output is valid: every domain contributed namespaced rows
    vf = pd.read_csv(out / "ml" / "vehicle_features.csv")
    assert len(vf) > 0 and set(vf["domain_id"].unique()) == {0, 1, 2}
    assert vf["entity_id"].str.startswith("d").all()
    rf = pd.read_csv(out / "ml" / "report_features.csv")
    assert len(rf) == man["row_counts"]["report_features"] > 0


# ---------------------------------------------------------------------------- #
# (c) DATASHEET.md gains a Scenario provenance section for event-driven datasets
# ---------------------------------------------------------------------------- #

def test_datasheet_scenario_provenance_section(tmp_path):
    out = str(tmp_path / "ds_events")
    events = [
        {"t": 8, "until": 20, "type": "demand", "mult": 2.5},
        {"t": 12, "type": "weather", "value": "fog"},
        {"t": 10, "until": 25, "type": "close_edge", "edge": [[1, 1], [2, 1]]},
        {"t": 5, "until": 30, "type": "attack_wave"},
    ]
    run_pipeline(PipelineConfig(out_dir=out, traffic_flow=True, road_network="grid",
                                car_following=True, duration_s=40.0, arrival_rate=1.0,
                                grid_w=4, grid_h=4, attacker_pct=0.25, seed=23,
                                events=json.dumps(events)))
    assert datasheet.main([out]) == 0                    # the writer produces DATASHEET.md
    md = (tmp_path / "ds_events" / "DATASHEET.md").read_text(encoding="utf-8")
    assert "## Scenario provenance" in md
    assert "routed street grid, 4 x 4 intersections" in md          # topology + dims
    assert "demand surge" in md and "x2.5" in md                    # each event, plain language
    assert "weather front" in md and "fog" in md
    assert "road closure" in md and "[[1, 1], [2, 1]]" in md
    assert "attack wave" in md and "t=5s until 30s" in md


def test_datasheet_omits_scenario_provenance_for_default_datasets(tmp_path):
    out = str(tmp_path / "ds_plain")
    run_pipeline(PipelineConfig(out_dir=out, n_vehicles=10, n_steps=30, seed=9))
    md = datasheet.build(out)
    assert "## Scenario provenance" not in md, \
        "default road_network + no events -> no provenance section"
