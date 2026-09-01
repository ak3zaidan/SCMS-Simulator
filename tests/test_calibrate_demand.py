"""`tools/calibrate_demand.py`: demand calibration against measured loop counts.

Hermetic. Every fixture is hand-written and tiny, so each assertion is checkable by hand:

  * a 3-edge `.net.xml` whose first edge carries a 2 m ``allow="pedestrian"`` sidewalk at lane
    index 0 -- the defect this tool exists to detect in the real InTAS network;
  * a 4-detector E1 additional file, one of them on that sidewalk;
  * a reference-count JSON in the schema `tools/fetch_ingolstadt_counts.py` writes;
  * a hand-written E1 detector output.

No SUMO run, no network fetch, no measured data.  The one test that needs `routeSampler.py`
skips when `SUMO_HOME` is unset.
"""
from __future__ import annotations

import importlib.util
import json
import math
import os
import subprocess
import sys
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("calibrate_demand",
                                               ROOT / "tools" / "calibrate_demand.py")
cd = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cd)


NET = """<?xml version="1.0" encoding="UTF-8"?>
<net>
    <edge id="A" function="normal">
        <lane id="A_0" index="0" allow="pedestrian" length="100.00" width="2.00"/>
        <lane id="A_1" index="1" disallow="pedestrian tram rail" length="100.00"/>
        <lane id="A_2" index="2" disallow="pedestrian tram rail" length="100.00"/>
    </edge>
    <edge id="B" function="normal">
        <lane id="B_0" index="0" disallow="pedestrian" length="80.00"/>
        <lane id="B_1" index="1" disallow="pedestrian" length="80.00"/>
    </edge>
    <edge id=":J0_0" function="internal">
        <lane id=":J0_0_0" index="0" length="5.00"/>
    </edge>
</net>
"""

# S1 has two loops on edge A -- one of them on the SIDEWALK (A_0).
# S2 has one loop on edge B, whose other car lane is uninstrumented.
ADD = """<?xml version="1.0" encoding="UTF-8"?>
<additional>
    <e1Detector id="S1_1" lane="A_0" pos="10.00" freq="900.00" name="S1" file="o.xml"/>
    <e1Detector id="S1_2" lane="A_1" pos="12.00" freq="900.00" name="S1" file="o.xml"/>
    <e1Detector id="S2_1" lane="B_0" pos="7.00" freq="900.00" name="S2" file="o.xml"/>
    <e1Detector id="gate" lane="B_1" pos="9.00" freq="900.00" file="o.xml"/>
</additional>
"""


@pytest.fixture()
def fixture_dir(tmp_path):
    (tmp_path / "net.xml").write_text(NET, encoding="utf-8")
    (tmp_path / "e1.add.xml").write_text(ADD, encoding="utf-8")
    return tmp_path


def _ref(tmp_path, name, s1=(100.0, 200.0), s2=300.0):
    """A reference-count file in the fetch tool's schema."""
    doc = {
        "stations": {"S1": s1[0] + s1[1], "S2": s2},
        "station_details": {
            "S1": {"comparability": "exact", "count": s1[0] + s1[1],
                   "matched_detector_ids": ["S1_1", "S1_2"],
                   "per_detector": {"S1_1": {"count": s1[0]}, "S1_2": {"count": s1[1]}}},
            "S2": {"comparability": "subset", "count": s2,
                   "matched_detector_ids": ["S2_1"],
                   "per_detector": {"S2_1": {"count": s2}}},
            "S3": {"comparability": "unusable", "count": None,
                   "matched_detector_ids": [], "per_detector": {}},
        },
    }
    p = tmp_path / name
    p.write_text(json.dumps(doc), encoding="utf-8")
    return p


# --------------------------------------------------------------------------- #
# 1. the sidewalk defect: detection and repair
# --------------------------------------------------------------------------- #
def test_layout_detects_detector_on_a_pedestrian_lane(fixture_dir):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    assert lay["n_detectors_on_non_car_lane_before"] == 1
    assert lay["detectors_on_non_car_lane_before"] == ["S1_1"]


def test_layout_repair_moves_every_loop_onto_a_car_lane_without_collision(fixture_dir):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    assert lay["n_detectors_on_non_car_lane_after"] == 0
    assert lay["problems"] == []
    # both loops on edge A shift by one, so they land on the two car lanes and stay distinct
    assert lay["detectors"]["S1_1"]["fixed_lane"] == "A_1"
    assert lay["detectors"]["S1_2"]["fixed_lane"] == "A_2"
    # edge B has no sidewalk, so nothing there moves
    assert lay["detectors"]["S2_1"]["fixed_lane"] == "B_0"


def test_layout_classifies_full_vs_partial_instrumentation(fixture_dir):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    # A: 2 car lanes, 2 named loops after the shift -> exact.  B: 2 car lanes, 1 named loop
    # (the unnamed gate counter does not belong to a station) -> lower bound only.
    assert lay["edges"]["A"]["fully_instrumented"] is True
    assert lay["edges"]["B"]["fully_instrumented"] is False
    assert lay["n_edges_fully_instrumented"] == 1


def test_layout_never_shifts_a_detector_off_the_end_of_its_edge(tmp_path):
    """The repair must refuse rather than silently produce an out-of-range lane."""
    (tmp_path / "net.xml").write_text(
        '<net><edge id="A" function="normal">'
        '<lane id="A_0" index="0" allow="pedestrian" length="10"/>'
        '<lane id="A_1" index="1" disallow="pedestrian" length="10"/>'
        "</edge></net>", encoding="utf-8")
    (tmp_path / "e1.add.xml").write_text(
        '<additional>'
        '<e1Detector id="X_1" lane="A_0" pos="1" freq="900" name="X" file="o.xml"/>'
        '<e1Detector id="X_2" lane="A_1" pos="1" freq="900" name="X" file="o.xml"/>'
        "</additional>", encoding="utf-8")
    lay = cd.build_layout(tmp_path / "e1.add.xml", tmp_path / "net.xml")
    assert [p["problem"] for p in lay["problems"]] == ["overflow"]
    # and it leaves the layout alone rather than corrupting it
    assert lay["detectors"]["X_1"]["fixed_lane"] == "A_0"


def test_layout_files_keep_original_ids_and_omit_empty_names(fixture_dir, tmp_path):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    out = tmp_path / "lay"
    paths = cd.write_layout_files(lay, out)
    merged = paths["merged"].read_text(encoding="utf-8")
    assert 'id="S1_1" lane="A_0"' in merged            # 1: shipped layout, verbatim
    assert 'id="fx_S1_1" lane="A_1"' in merged         # 2: repaired copy, new id
    assert 'id="cov_A_1"' in merged and 'id="cov_A_2"' in merged   # 3: every car lane
    assert 'id="cov_A_0"' not in merged                # ... but never the sidewalk
    # SUMO rejects name="": the unnamed gate counter must stay unnamed
    assert 'name=""' not in merged
    assert 'id="gate"' in merged


# --------------------------------------------------------------------------- #
# 2. count targets
# --------------------------------------------------------------------------- #
def test_targets_only_uses_fully_instrumented_edges(fixture_dir, tmp_path):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    lm = tmp_path / "map.json"
    lm.write_text(json.dumps(lay), encoding="utf-8")
    ref = _ref(tmp_path, "r1.json")
    out = tmp_path / "t.edg.xml"
    cd.main(["targets", "--layout-map", str(lm), "--interval", f"0:3600={ref}",
             "--out", str(out)])
    xml = out.read_text(encoding="utf-8")
    assert 'id="A" entered="300"' in xml               # 100 + 200 on the two loops of edge A
    assert 'id="B"' not in xml                         # partial -> a lower bound, never a target
    meta = json.loads((tmp_path / "t.edg.meta.json").read_text(encoding="utf-8"))
    assert meta["counting_edges"] == ["A"]
    # the counting edges cover 300 of the 600 measured vehicles, and that is stated
    assert meta["coverage"][0]["fraction_of_reference"] == pytest.approx(0.5)
    assert meta["coverage"][0]["stations_with_zero_coverage"] == ["S2"]


def test_targets_average_several_days_and_record_the_spread(fixture_dir, tmp_path):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    lm = tmp_path / "map.json"
    lm.write_text(json.dumps(lay), encoding="utf-8")
    r1 = _ref(tmp_path, "r1.json", s1=(100.0, 200.0))
    r2 = _ref(tmp_path, "r2.json", s1=(120.0, 180.0))
    out = tmp_path / "t.edg.xml"
    cd.main(["targets", "--layout-map", str(lm), "--interval", f"0:3600={r1},{r2}",
             "--out", str(out)])
    meta = json.loads((tmp_path / "t.edg.meta.json").read_text(encoding="utf-8"))
    e = meta["intervals"][0]["per_edge"]["A"]
    assert e["measured_per_day"] == [300.0, 300.0]
    assert e["measured_mean"] == 300.0
    assert e["target_cars"] == 300


def test_targets_subtract_scheduled_bus_passages(fixture_dir, tmp_path):
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    lm = tmp_path / "map.json"
    lm.write_text(json.dumps(lay), encoding="utf-8")
    bus = tmp_path / "bus.flow.xml"
    bus.write_text('<routes><flow id="L1" begin="0" end="3600" period="900">'
                   '<route edges="A B"/></flow></routes>', encoding="utf-8")
    ref = _ref(tmp_path, "r1.json")
    out = tmp_path / "t.edg.xml"
    cd.main(["targets", "--layout-map", str(lm), "--interval", f"0:3600={ref}",
             "--bus-flows", str(bus), "--out", str(out)])
    # 4 scheduled departures in the hour, each crossing edge A once
    assert 'id="A" entered="296"' in out.read_text(encoding="utf-8")


def test_bus_passages_counts_every_flow_style(tmp_path):
    p = tmp_path / "_bus.xml"
    p.write_text('<routes>'
                 '<flow id="p" begin="0" end="3600" period="1200"><route edges="A"/></flow>'
                 '<flow id="h" begin="0" end="3600" vehsPerHour="2"><route edges="A B"/></flow>'
                 '<flow id="n" begin="0" end="3600" number="5"><route edges="B"/></flow>'
                 "</routes>", encoding="utf-8")
    per_edge, total = cd.bus_passages(p, 0, 3600, {"A", "B"})
    assert per_edge["A"] == 3 + 2          # period 1200 -> 0/1200/2400; 2 veh/h -> 0/1800
    assert per_edge["B"] == 2 + 5
    assert total == 3 + 2 + 5
    # a window that contains no scheduled departure yields nothing rather than a stale count
    assert cd.bus_passages(p, 3600, 7200, {"A", "B"}) == ({}, 0)


# --------------------------------------------------------------------------- #
# 3. grading: GEH, the FHWA gates, and the loop-subset rule
# --------------------------------------------------------------------------- #
def test_geh_formula_and_scale_dependence():
    assert cd.geh(100.0, 100.0) == 0.0
    assert cd.geh(0.0, 0.0) == 0.0
    assert cd.geh(110.0, 100.0) == pytest.approx(math.sqrt(2 * 100 / 210))
    # GEH(k*m, k*c) = sqrt(k) * GEH(m, c): the reason a 300 s window may not be extrapolated
    for k in (2.0, 12.0):
        assert cd.geh(k * 110, k * 100) == pytest.approx(math.sqrt(k) * cd.geh(110, 100))


def test_criteria_come_from_the_repo_refdata_not_from_literals():
    """The gates must be the same numbers tools/sumo_realism.py grades with."""
    ref = json.loads(cd.REFDATA.read_text(encoding="utf-8"))["entries"]
    f = cd.load_fhwa()
    assert f["source"].endswith("geh_criteria.json")
    assert f["geh_link_max"] == ref["geh_link_max"]["max"] == 5.0
    assert f["geh_link_min_pass_fraction"] == ref["geh_link_min_pass_fraction"]["min"] == 0.85
    assert f["geh_total_max"] == ref["geh_total_max"]["max"] == 4.0
    assert f["total_flow_tolerance_fraction"] == ref["total_flow_tolerance_fraction"]["max"] == 0.05
    assert f["bands"] == ref["link_flow_tolerance_bands"]["value"]


def test_flow_tolerance_bands_are_selected_on_the_counted_side():
    assert cd.flow_tolerance_pass(600.0, 650.0)          # < 700 -> +-100 veh/h
    assert not cd.flow_tolerance_pass(500.0, 650.0)
    assert cd.flow_tolerance_pass(1150.0, 1000.0)        # 700..2700 -> +-15 %
    assert not cd.flow_tolerance_pass(1160.0, 1000.0)
    assert cd.flow_tolerance_pass(3400.0, 3000.0)        # > 2700 -> +-400 veh/h
    assert not cd.flow_tolerance_pass(3401.0, 3000.0)


def test_grade_stations_gates_and_ratio_distribution():
    rep = cd.grade_stations({"a": 100, "b": 50, "c": 900},
                            {"a": 100, "b": 200, "c": 1000})
    assert rep["n_stations"] == 3
    assert rep["total_modelled"] == 1050 and rep["total_measured"] == 1300
    assert rep["total_rel_error"] == pytest.approx((1050 - 1300) / 1300, abs=1e-4)
    # a: GEH 0.  c: sqrt(2*100^2/1900) = 3.24, inside the gate even at a 10 % shortfall --
    # which is exactly why the pass FRACTION and not the ratio is the FHWA link criterion.
    # b: sqrt(2*150^2/250) = 13.4.
    assert rep["n_geh_lt_5"] == 2
    assert rep["n_under_075"] == 1 and rep["n_over_125"] == 0
    assert rep["ratio_median"] == pytest.approx(0.9)
    st = {g["id"]: g["status"] for g in rep["gates"]}
    assert st["geh.link_pass_fraction"] == "fail"      # 1/3 < 0.85
    assert st["geh.total_flow_rel_error"] == "fail"


def test_grade_stations_excludes_both_zero_pairs():
    rep = cd.grade_stations({"a": 100, "z": 0}, {"a": 100, "z": 0})
    assert rep["n_stations"] == 1                      # a zero/zero pair carries no information
    assert rep["gates"][0]["value"] == 1.0


def test_modelled_side_is_restricted_to_the_loops_the_reference_measured(fixture_dir, tmp_path):
    """A station where the reference measured fewer loops than InTAS models must not be
    graded by summing all the modelled loops -- that flatters the model."""
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    ref = cd.load_ref(_ref(tmp_path, "r.json"))
    ref["stations"]["S1"]["matched"] = {"S1_1"}        # pretend only one loop was measured
    counts = {"fx_S1_1": 11.0, "fx_S1_2": 999.0, "fx_S2_1": 7.0}
    modelled, used = cd.modelled_by_station(counts, lay, ref, cd.FIXED_PREFIX)
    assert modelled["S1"] == 11.0 and used["S1"] == 1
    assert modelled["S2"] == 7.0


def test_read_e1_only_uses_intervals_fully_inside_the_window(tmp_path):
    p = tmp_path / "det.xml"
    p.write_text('<detector>'
                 '<interval begin="0.00" end="900.00" id="d" nVehContrib="10"/>'
                 '<interval begin="900.00" end="1800.00" id="d" nVehContrib="20"/>'
                 '<interval begin="1800.00" end="2700.00" id="d" nVehContrib="40"/>'
                 "</detector>", encoding="utf-8")
    tot, n = cd.read_e1(p, 900, 1800)
    assert tot["d"] == 20 and n == 1
    tot, n = cd.read_e1(p, 0, 2700)
    assert tot["d"] == 70 and n == 3


def test_grade_end_to_end_repaired_layout_beats_the_shipped_one(fixture_dir, tmp_path):
    """The whole point of the layout repair, in one assertion: with the SAME simulated
    traffic, the shipped layout under-counts because one of its loops is on the sidewalk."""
    lay = cd.build_layout(fixture_dir / "e1.add.xml", fixture_dir / "net.xml")
    lm = tmp_path / "map.json"
    lm.write_text(json.dumps(lay), encoding="utf-8")
    ref = _ref(tmp_path, "r.json", s1=(100.0, 200.0), s2=300.0)
    det = tmp_path / "det.xml"
    det.write_text(
        '<detector>'
        # shipped layout: S1_1 sits on the sidewalk and counts nothing
        '<interval begin="0" end="3600" id="S1_1" nVehContrib="0"/>'
        '<interval begin="0" end="3600" id="S1_2" nVehContrib="200"/>'
        '<interval begin="0" end="3600" id="S2_1" nVehContrib="300"/>'
        # repaired layout: the same traffic, counted on car lanes
        '<interval begin="0" end="3600" id="fx_S1_1" nVehContrib="100"/>'
        '<interval begin="0" end="3600" id="fx_S1_2" nVehContrib="200"/>'
        '<interval begin="0" end="3600" id="fx_S2_1" nVehContrib="300"/>'
        "</detector>", encoding="utf-8")

    shipped = tmp_path / "shipped.json"
    cd.main(["grade", "--det-out", str(det), "--layout-map", str(lm), "--ref", f"{ref}=held-out",
             "--begin", "0", "--end", "3600", "--json", str(shipped)])
    a = json.loads(shipped.read_text(encoding="utf-8"))["reports"][0]
    assert a["total_modelled"] == 500 and a["total_measured"] == 600
    assert a["layout"] == "as-shipped" and a["set"] == "held-out"

    repaired = tmp_path / "repaired.json"
    cd.main(["grade", "--det-out", str(det), "--layout-map", str(lm), "--ref", str(ref),
             "--id-prefix", cd.FIXED_PREFIX, "--begin", "0", "--end", "3600",
             "--json", str(repaired)])
    b = json.loads(repaired.read_text(encoding="utf-8"))["reports"][0]
    assert b["total_modelled"] == 600 and b["total_rel_error"] == 0.0
    assert b["layout"] == "repaired"
    assert all(g["status"] == "pass" for g in b["gates"])


# --------------------------------------------------------------------------- #
# 4. candidate extraction
# --------------------------------------------------------------------------- #
def test_candidates_take_the_modal_route_inside_the_departure_band(tmp_path):
    src = tmp_path / "in.rou.xml"
    src.write_text(
        '<routes><vType id="t"/>'
        '<vehicle id="early" type="t" depart="10.00">'
        '<routeDistribution><route probability="1.0" edges="X Y"/></routeDistribution></vehicle>'
        '<vehicle id="in" type="t" depart="150.00"><routeDistribution>'
        '<route probability="0.1" edges="A B"/>'
        '<route probability="0.9" edges="A C"/></routeDistribution></vehicle>'
        '<vehicle id="late" type="t" depart="900.00">'
        '<routeDistribution><route probability="1.0" edges="Z"/></routeDistribution></vehicle>'
        "</routes>", encoding="utf-8")
    out = tmp_path / "cand.rou.xml"
    cd.main(["candidates", "--route-files", str(src), "--begin", "100", "--end", "200",
             "--out", str(out)])
    txt = out.read_text(encoding="utf-8")
    assert 'edges="A C"' in txt                 # the modal alternative
    assert 'edges="A B"' not in txt
    assert "X Y" not in txt and ">Z<" not in txt
    meta = json.loads((tmp_path / "cand.rou.meta.json").read_text(encoding="utf-8"))
    assert meta["n_vehicles_scanned"] == 3 and meta["n_routes_kept"] == 1


# --------------------------------------------------------------------------- #
# 5. data hygiene: measured counts must never land in the tracked tree
# --------------------------------------------------------------------------- #
def test_guard_refuses_to_write_count_derived_output_into_the_repo():
    with pytest.raises(SystemExit) as e:
        cd.guard_out(ROOT / "docs" / "leaked_counts.json")
    assert "refused" in str(e.value)


def test_guard_allows_the_gitignored_cache_roots_and_paths_outside_the_repo(tmp_path):
    assert cd.guard_out(ROOT / ".cache" / "calib" / "x.json").name == "x.json"
    assert cd.guard_out(ROOT / ".realism_cache" / "x.json").name == "x.json"
    assert cd.guard_out(tmp_path / "x.json").name == "x.json"


def test_the_gitignore_really_excludes_those_roots():
    """The guard is only as good as the .gitignore it trusts."""
    ig = (ROOT / ".gitignore").read_text(encoding="utf-8")
    assert "/.cache/" in ig and "/.realism_cache/" in ig


# --------------------------------------------------------------------------- #
# 6. scenario wiring
# --------------------------------------------------------------------------- #
def test_scenario_writes_an_opt_in_cfg_and_leaves_the_original_alone(tmp_path):
    src = tmp_path / "orig.sumocfg"
    src.write_text('<configuration><input>'
                   '<net-file value="n.net.xml"/>'
                   '<route-files value="routes/ped.rou.xml,routes/BusRoutes.flow.xml,'
                   'routes/InTAS_001.rou.xml"/>'
                   '<additional-files value="a.add.xml"/>'
                   "</input></configuration>", encoding="utf-8")
    before = src.read_bytes()
    routes = tmp_path / "cal.rou.xml"
    routes.write_text("<routes/>", encoding="utf-8")
    cd.main(["scenario", "--sumocfg", str(src), "--routes", str(routes),
             "--name", "cal.sumocfg"])
    out = (tmp_path / "cal.sumocfg").read_text(encoding="utf-8")
    assert 'value="routes/ped.rou.xml,routes/BusRoutes.flow.xml,cal.rou.xml"' in out
    assert "InTAS_001.rou.xml" not in out
    assert src.read_bytes() == before          # the InTAS default stays byte-identical


def test_cfg_set_creates_a_missing_option_and_section(tmp_path):
    txt = "<configuration><input><net-file value=\"n\"/></input></configuration>"
    got = cd._cfg_set(txt, "route-files", "r.xml", "input")
    assert '<route-files value="r.xml"/>' in got
    got2 = cd._cfg_set(got, "log", "l.txt", "report")
    assert "<report>" in got2 and '<log value="l.txt"/>' in got2


# --------------------------------------------------------------------------- #
# 5b. calibration vs held-out aggregation
# --------------------------------------------------------------------------- #
def _grade_doc(tmp_path, name, rows):
    """rows: (set, measured_total, modelled_total, geh_median)."""
    reps = []
    for i, (s, c, m, g) in enumerate(rows):
        reps.append({
            "run": "r", "layout": "repaired", "set": s, "window_sumo": [25200, 28800],
            "reference": {"path": str(tmp_path / f"ref{i}.json")},
            "total_measured": c, "total_modelled": m, "total_rel_error": (m - c) / c,
            "geh_median": g, "n_geh_lt_5": 20, "n_stations": 23,
            "gates": [{"id": "geh.link_flow_tolerance_pass_fraction", "value": 0.9}],
            "criteria_source": "test",
        })
    p = tmp_path / name
    p.write_text(json.dumps({"reports": reps}), encoding="utf-8")
    return p


def test_report_flags_a_calibration_to_heldout_gap_as_overfitting(tmp_path):
    p = _grade_doc(tmp_path, "g.json", [
        ("calibration", 45000, 45100, 3.0), ("calibration", 44500, 44600, 3.1),
        ("held-out", 45000, 36000, 14.0), ("held-out", 44800, 35900, 14.2)])
    out = tmp_path / "sum.json"
    cd.main(["report", "--grade", str(p), "--out", str(out)])
    g = json.loads(out.read_text(encoding="utf-8"))["groups"][0]
    assert g["sets"]["calibration"]["n_windows"] == 2
    assert g["sets"]["held-out"]["n_windows"] == 2
    assert g["overfitting"]["rel_error_gap"] > 0.19
    assert "GAP EXCEEDS" in g["overfitting"]["verdict"]


def test_report_does_not_cry_overfitting_when_the_gap_is_inside_the_noise_floor(tmp_path):
    p = _grade_doc(tmp_path, "g.json", [
        ("calibration", 45000, 45100, 3.0), ("calibration", 44500, 44600, 3.1),
        ("held-out", 45000, 45000, 3.2), ("held-out", 44800, 44850, 3.0)])
    out = tmp_path / "sum.json"
    cd.main(["report", "--grade", str(p), "--out", str(out)])
    g = json.loads(out.read_text(encoding="utf-8"))["groups"][0]
    assert "no evidence of overfitting" in g["overfitting"]["verdict"]
    # the noise floor is measured from the reference itself, never assumed
    assert g["sets"]["held-out"]["reference_day_to_day_cv"] is not None


def _gen_scenario():
    """Import scms-sim/scenarios/gen_scenario.py (its directory is not a package)."""
    d = ROOT / "scms-sim" / "scenarios"
    if str(d) not in sys.path:
        sys.path.insert(0, str(d))
    spec = importlib.util.spec_from_file_location("gen_scenario", d / "gen_scenario.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


def _fake_scenario(tmp_path):
    (tmp_path / "sumo").mkdir()
    (tmp_path / "sumo" / "s.sumocfg").write_text(
        '<configuration><input><net-file value="n.net.xml"/>'
        '<route-files value="routes/ped.rou.xml,routes/BusRoutes.flow.xml,'
        'routes/InTAS_001.rou.xml,routes/InTAS_002.rou.xml"/>'
        '<additional-files value="BusStations.add.xml,InTAS_E1.add.xml,buildings.poly.xml"/>'
        "</input></configuration>", encoding="utf-8")
    return tmp_path


def test_gen_scenario_demand_default_is_intas_and_changes_nothing(tmp_path, monkeypatch):
    gs = _gen_scenario()
    dst = _fake_scenario(tmp_path)
    before = (dst / "sumo" / "s.sumocfg").read_bytes()
    monkeypatch.delenv("SCMS_DEMAND", raising=False)
    info = gs._apply_demand(dst, "route")
    assert info["demand_source"] == "intas"
    assert (dst / "sumo" / "s.sumocfg").read_bytes() == before


def test_gen_scenario_calibrated_demand_swaps_routes_and_replaces_the_layout(tmp_path, monkeypatch):
    gs = _gen_scenario()
    dst = _fake_scenario(tmp_path)
    routes = tmp_path / "cal.rou.xml"
    routes.write_text("<routes/>", encoding="utf-8")
    add = tmp_path / "fixed.add.xml"
    add.write_text("<additional/>", encoding="utf-8")
    monkeypatch.setenv("SCMS_DEMAND", "calibrated")
    monkeypatch.setenv("SCMS_DEMAND_ROUTES", str(routes))
    monkeypatch.setenv("SCMS_DEMAND_DETECTORS", str(add))
    info = gs._apply_demand(dst, "route")
    txt = (dst / "sumo" / "s.sumocfg").read_text(encoding="utf-8")
    assert "InTAS_001.rou.xml" not in txt and "InTAS_002.rou.xml" not in txt
    assert "routes/ped.rou.xml" in txt and "routes/BusRoutes.flow.xml" in txt
    assert "cal.rou.xml" in txt
    # loading the shipped and the repaired layout together defines every id twice
    assert "InTAS_E1.add.xml" not in txt
    assert "fixed.add.xml" in txt and "BusStations.add.xml" in txt
    assert info["demand_source"] == "calibrated"
    assert info["demand_kept_routes"] == ["routes/ped.rou.xml", "routes/BusRoutes.flow.xml"]


def test_gen_scenario_calibrated_demand_refuses_to_run_underspecified(tmp_path, monkeypatch):
    gs = _gen_scenario()
    dst = _fake_scenario(tmp_path)
    monkeypatch.setenv("SCMS_DEMAND", "calibrated")
    monkeypatch.delenv("SCMS_DEMAND_ROUTES", raising=False)
    with pytest.raises(SystemExit):
        gs._apply_demand(dst, "route")
    monkeypatch.setenv("SCMS_DEMAND_ROUTES", str(tmp_path / "missing.rou.xml"))
    with pytest.raises(SystemExit):
        gs._apply_demand(dst, "route")
    # and it is a route-map concept only: flow maps get their demand from MOSAIC
    (tmp_path / "cal.rou.xml").write_text("<routes/>", encoding="utf-8")
    monkeypatch.setenv("SCMS_DEMAND_ROUTES", str(tmp_path / "cal.rou.xml"))
    with pytest.raises(SystemExit):
        gs._apply_demand(dst, "flow")


# --------------------------------------------------------------------------- #
# 7. routeSampler really is driven correctly (needs SUMO_HOME)
# --------------------------------------------------------------------------- #
@pytest.mark.skipif(not os.environ.get("SUMO_HOME"), reason="SUMO_HOME not set")
def test_sample_hits_the_counts_and_records_a_reproducible_invocation(tmp_path):
    cands = tmp_path / "c.rou.xml"
    cands.write_text('<routes>'
                     + "".join(f'<route id="r{i}" edges="A B"/>' for i in range(50))
                     + "".join(f'<route id="s{i}" edges="A"/>' for i in range(50))
                     + "</routes>", encoding="utf-8")
    tgt = tmp_path / "t.edg.xml"
    tgt.write_text('<data><interval id="c" begin="0" end="3600">'
                   '<edge id="A" entered="100"/><edge id="B" entered="40"/>'
                   "</interval></data>", encoding="utf-8")
    out = tmp_path / "cal.rou.xml"
    cd.main(["sample", "--candidates", str(cands), "--targets", str(tgt), "--out", str(out),
             "--begin", "0", "--end", "3600", "--interval", "3600", "--seed", "7"])
    txt = out.read_text(encoding="utf-8")
    assert txt.count("<vehicle ") == 100          # 60 A-only + 40 A->B reproduces both counts
    assert '<vType id="calib_car" vClass="passenger"/>' in txt
    meta = json.loads((tmp_path / "cal.rou.meta.json").read_text(encoding="utf-8"))
    assert meta["n_vehicles"] == 100
    assert "routeSampler.py" in meta["command"]
    assert "--seed 7" in meta["command"]
    assert meta["inputs"]["targets"]["sha256"] == cd.sha256_file(tgt)
    # the recorded argv is the reproduction recipe: it must actually run
    r = subprocess.run([sys.executable] + meta["argv"], capture_output=True, text=True)
    assert r.returncode == 0, r.stderr[-2000:]
