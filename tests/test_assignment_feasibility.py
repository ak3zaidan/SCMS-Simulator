"""Unit tests for tools/assignment_feasibility.py.

The tool's whole value is that its answer is trusted without a simulation to check it
against, so the parts that could be silently wrong are pinned here against cases whose
answer is known by hand:

  * the capacity model reading green splits out of ``<tlLogic>``,
  * the time-dependent edge-cost lookup that reproduces duarouter's own pricing,
  * the LP itself, on a network small enough to solve in your head.

No fixture here contains measured Ingolstadt counts; every number is invented.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "tools"))

af = pytest.importorskip("assignment_feasibility")
pytest.importorskip("scipy")


# --------------------------------------------------------------------------- fixtures
NET = """<?xml version="1.0" encoding="UTF-8"?>
<net>
    <edge id="in" from="A" to="B" priority="1">
        <lane id="in_0" index="0" speed="10.0" length="100.00"/>
        <lane id="in_1" index="1" speed="10.0" length="100.00"/>
    </edge>
    <edge id="up" from="B" to="C" priority="1">
        <lane id="up_0" index="0" speed="10.0" length="200.00"/>
    </edge>
    <edge id="down" from="B" to="C" priority="1">
        <lane id="down_0" index="0" speed="10.0" length="400.00"/>
    </edge>
    <edge id="out" from="C" to="D" priority="1">
        <lane id="out_0" index="0" speed="10.0" length="100.00"/>
    </edge>
    <edge id=":B_0" function="internal">
        <lane id=":B_0_0" index="0" speed="10.0" length="5.00"/>
    </edge>
    <tlLogic id="TL" type="static" programID="0" offset="0">
        <phase duration="30" state="Gr"/>
        <phase duration="10" state="rG"/>
    </tlLogic>
    <connection from="in" to="up" fromLane="0" toLane="0" tl="TL" linkIndex="0"/>
    <connection from="in" to="down" fromLane="1" toLane="0" tl="TL" linkIndex="1"/>
</net>
"""

# Two parallel routes between the same origin and destination.  Only "up" is a counting
# edge.  "up" has ONE lane whose movement gets 30 s of a 40 s cycle, so g/C = 0.75.
POOL = """<?xml version="1.0" encoding="UTF-8"?>
<routes>
    <vehicle id="a" depart="0.00"><route edges="in up out"/></vehicle>
    <vehicle id="b" depart="0.00"><route edges="in down out"/></vehicle>
    <vehicle id="c" depart="0.00"><route edges="in up out"/></vehicle>
</routes>
"""

EDGEDATA = """<?xml version="1.0" encoding="UTF-8"?>
<meandata>
    <interval begin="0.00" end="900.00">
        <edge id="in" traveltime="10.00"/>
        <edge id="up" traveltime="20.00"/>
        <edge id="down" traveltime="40.00"/>
        <edge id="out" traveltime="10.00"/>
    </interval>
    <interval begin="900.00" end="1800.00">
        <edge id="in" traveltime="10.00"/>
        <edge id="up" traveltime="200.00"/>
        <edge id="down" traveltime="40.00"/>
        <edge id="out" traveltime="10.00"/>
    </interval>
</meandata>
"""


def write_targets(path, target_up):
    path.write_text(json.dumps({
        "counting_edges": ["up"],
        "intervals": [{"begin": 0.0, "end": 3600.0,
                       "total_target_cars": target_up,
                       "per_edge": {"up": {"target_cars": target_up}}}],
    }), encoding="utf-8")


@pytest.fixture()
def scen(tmp_path):
    (tmp_path / "net.xml").write_text(NET, encoding="utf-8")
    (tmp_path / "pool.rou.xml").write_text(POOL, encoding="utf-8")
    (tmp_path / "edgedata.xml").write_text(EDGEDATA, encoding="utf-8")
    return tmp_path


# --------------------------------------------------------------------------- capacity
def test_capacity_uses_the_real_green_split(scen):
    lanes, tls, conns = af.parse_net(str(scen / "net.xml"))
    cap = af.capacity(lanes, tls, conns, sat_flow=1800.0, minor_cap=0.0)
    # "in" lane 0 -> linkIndex 0, green in the 30 s phase of a 40 s cycle => 0.75
    # "in" lane 1 -> linkIndex 1, green in the 10 s phase                 => 0.25
    assert cap["in"] == pytest.approx(1800 * 0.75 + 1800 * 0.25)
    # "up"/"down"/"out" have no signal-controlled connections at all
    assert cap["up"] == pytest.approx(1800.0)


def test_minor_cap_penalises_only_uncontrolled_lanes(scen):
    lanes, tls, conns = af.parse_net(str(scen / "net.xml"))
    cap = af.capacity(lanes, tls, conns, sat_flow=1800.0, minor_cap=500.0)
    assert cap["up"] == pytest.approx(500.0)          # uncontrolled -> capped
    assert cap["in"] == pytest.approx(1800.0)         # signalised -> untouched


def test_internal_edges_are_excluded(scen):
    lanes, _, _ = af.parse_net(str(scen / "net.xml"))
    assert ":B_0" not in lanes
    assert set(lanes) == {"in", "up", "down", "out"}


# --------------------------------------------------------------------------- costs
def test_edge_costs_are_looked_up_per_time_bin(scen):
    bins = af.edge_traveltimes(scen / "edgedata.xml")
    assert [b[0] for b in bins] == [0.0, 900.0]
    assert bins[0][2]["up"] == pytest.approx(20.0)
    assert bins[1][2]["up"] == pytest.approx(200.0)


def test_freeflow_times_use_the_fastest_lane(scen):
    ff = af.freeflow_times(str(scen / "net.xml"))
    assert ff["down"] == pytest.approx(40.0)          # 400 m / 10 m/s
    assert ":B_0" not in ff


# --------------------------------------------------------------------------- the LP
def _run_lp(scen, **kw):
    write_targets(scen / "targets.json", kw.pop("target", 1000.0))
    argv = ["lp", "--net", str(scen / "net.xml"), "--pool", str(scen / "pool.rou.xml"),
            "--targets-meta", str(scen / "targets.json"),
            "--begin", "0", "--end", "3600",
            "--json", str(scen / ("out%s.json" % kw.pop("tag", "")))]
    for k, v in kw.items():
        argv += ["--" + k.replace("_", "-"), str(v)]
    af.main(argv)
    return json.load(open(argv[argv.index("--json") + 1], encoding="utf-8"))


def test_lp_serves_a_target_inside_capacity(scen):
    """1000 veh/h asked of a lane that can pass 1800 -- must be served exactly."""
    rep = _run_lp(scen, target=1000.0)
    assert rep["lp_total_abs_mismatch"] == pytest.approx(0.0, abs=1e-6)
    assert rep["served_fraction_of_target"] == pytest.approx(1.0)


def test_lp_reports_the_shortfall_when_capacity_binds(scen):
    """Ask 1000 through a lane capped at 400: exactly 600 must come back unservable."""
    rep = _run_lp(scen, target=1000.0, minor_cap=400.0, tag="cap")
    assert rep["lp_served_total"] == pytest.approx(400.0)
    assert rep["lp_underflow_total"] == pytest.approx(600.0)
    assert rep["lp_overflow_total"] == pytest.approx(0.0)
    assert rep["n_binding_capacity_edges"] >= 1


def test_lp_ignores_pool_routes_that_miss_every_counting_edge(scen):
    """The 'down' route touches no counting edge and must be dropped before solving."""
    rep = _run_lp(scen, target=100.0, tag="drop")
    # only the two identical "in up out" instances survive, and they are one distinct route
    assert rep["n_candidate_routes"] == 1


@pytest.mark.parametrize("tol, kept", [
    # Walking the clock from t=0 in the first bin: "in up out" = 10+20+10 = 40 s,
    # "in down out" = 10+40+10 = 60 s.  The reference shortest path is the 40 s one, so
    # the 60 s route survives a cap of exactly 1.5x and nothing tighter.
    (0.25, 1),
    (0.49, 1),
    (0.50, 2),      # boundary is inclusive
    (1.00, 2),
])
def test_detour_filter_keeps_only_near_shortest_routes(scen, tmp_path, tol, kept):
    shortest = tmp_path / "shortest.rou.xml"
    shortest.write_text(
        '<routes><vehicle id="s" depart="0.00">'
        '<route edges="in up out"/></vehicle></routes>', encoding="utf-8")
    # widen the counting set to both parallel edges so both routes are candidates
    (scen / "targets.json").write_text(json.dumps({
        "counting_edges": ["up", "down"],
        "intervals": [{"begin": 0.0, "end": 3600.0, "total_target_cars": 100.0,
                       "per_edge": {"up": {"target_cars": 100.0},
                                    "down": {"target_cars": 0.0}}}]}), encoding="utf-8")
    out = scen / f"detour{tol}.json"
    af.main(["lp", "--net", str(scen / "net.xml"), "--pool", str(scen / "pool.rou.xml"),
             "--targets-meta", str(scen / "targets.json"), "--begin", "0", "--end", "3600",
             "--max-detour", str(tol), "--edgedata", str(scen / "edgedata.xml"),
             "--shortest", str(shortest), "--json", str(out)])
    rep = json.load(open(out, encoding="utf-8"))
    assert rep["detour_filter"]["pool_routes"] == 2
    assert rep["detour_filter"]["kept"] == kept
    assert rep["detour_filter"]["dropped"] == 2 - kept


# --------------------------------------------------------------------------- detour
def _detour(scen, tmp_path, tag, edgedata):
    routes = tmp_path / "assigned.rou.xml"
    routes.write_text('<routes><vehicle id="v" depart="0.00">'
                      '<route edges="in down out"/></vehicle></routes>', encoding="utf-8")
    shortest = tmp_path / "shortest.rou.xml"
    shortest.write_text('<routes><vehicle id="v" depart="0.00">'
                        '<route edges="in up out"/></vehicle></routes>', encoding="utf-8")
    out = scen / f"detour_{tag}.json"
    argv = ["detour", "--net", str(scen / "net.xml"), "--routes", str(routes),
            "--shortest", str(shortest), "--begin", "0", "--end", "3600",
            "--json", str(out)]
    if edgedata:
        argv += ["--edgedata", str(scen / "edgedata.xml")]
    af.main(argv)
    return json.load(open(out, encoding="utf-8"))


def test_detour_prices_on_measured_weights_when_given_them(scen, tmp_path):
    """down = 10+40+10 = 60 s against up = 10+20+10 = 40 s, so the ratio is 1.5."""
    rep = _detour(scen, tmp_path, "cong", edgedata=True)
    assert rep["median_cost_ratio"] == pytest.approx(1.5)
    assert rep["weights"].endswith("edgedata.xml")


def test_detour_falls_back_to_free_flow_without_edgedata(scen, tmp_path):
    """Free-flow: down = 400 m / 10 = 40 s, up = 200/10 = 20 s, in+out cancel.

    Both routes share 'in' (100 m => 10 s) and 'out' (100 m => 10 s), so the ratio is
    (10+40+10) / (10+20+10) = 1.5 as well -- the point of the test is that the fallback
    is used and reported, not that the number differs on this toy net.
    """
    rep = _detour(scen, tmp_path, "ff", edgedata=False)
    assert rep["weights"].startswith("free-flow")
    assert rep["median_cost_ratio"] == pytest.approx(1.5)
    assert rep["edges_with_no_cost"] == 0


# --------------------------------------------------------------------------- licence
def test_output_is_refused_inside_the_tracked_repo(scen):
    """The counts have no stated licence; nothing derived from them may leave .cache/."""
    write_targets(scen / "targets.json", 100.0)
    with pytest.raises(SystemExit) as e:
        af.main(["lp", "--net", str(scen / "net.xml"),
                 "--pool", str(scen / "pool.rou.xml"),
                 "--targets-meta", str(scen / "targets.json"),
                 "--begin", "0", "--end", "3600",
                 "--json", str(REPO / "docs" / "realism" / "must_not_appear.json")])
    assert "refused" in str(e.value)
    assert not (REPO / "docs" / "realism" / "must_not_appear.json").exists()
