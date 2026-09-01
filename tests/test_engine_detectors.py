"""`tools/engine_detectors.py` -- counting induction-loop crossings from trajectories.

The tool exists so the PYTHON engine can be graded against real measured loop counts by the same
`tools/sumo_realism.py --ref-counts` path the MOSAIC/SUMO engine is. Its whole value rests on the
count meaning the same thing SUMO's `nVehContrib` means, so the tests here are about the gate's
DISCRIMINATION -- what it must count, and what it must refuse to count -- plus one end-to-end
agreement check against SUMO's own detectors on a run both sides can see.
"""
from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import xml.etree.ElementTree as ET

import pytest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_spec = importlib.util.spec_from_file_location(
    "engine_detectors", os.path.join(REPO, "tools", "engine_detectors.py"))
ed = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ed)


def _have_sumo():
    if not os.environ.get("SUMO_HOME"):
        return False
    try:
        import sumolib  # noqa: F401,PLC0415
    except ImportError:
        return False
    return os.path.exists(os.path.join(os.environ["SUMO_HOME"], "bin"))


needs_sumo = pytest.mark.skipif(not _have_sumo(), reason="SUMO_HOME + sumolib required")

GRID_N, GRID_LEN, STEPS = 4, 200.0, 120


@pytest.fixture(scope="module")
def net_with_loops(tmp_path_factory):
    """A netgenerate grid plus one E1 loop per lane of one edge, and a frozen SUMO run of it.

    NOTE the directory: SUMO's tools embed their own command line in an XML comment, so a path
    containing `--` yields a silently empty routes file (documented in docs/realism/SUMO-MOBILITY.md).
    """
    import re
    d = tmp_path_factory.mktemp("edet")
    assert "--" not in str(d)
    home = os.environ["SUMO_HOME"]
    net = str(d / "grid.net.xml")
    rou = str(d / "grid.trips.xml")
    subprocess.run([os.path.join(home, "bin", "netgenerate.exe"), "--grid",
                    "--grid.number", str(GRID_N), "--grid.length", str(GRID_LEN),
                    "-j", "traffic_light", "-o", net], check=True, capture_output=True)
    subprocess.run([sys.executable, os.path.join(home, "tools", "randomTrips.py"),
                    "-n", net, "-o", rou, "-e", str(STEPS), "-p", "0.4", "--seed", "7"],
                   check=True, capture_output=True)
    body = open(rou, encoding="utf-8").read()
    open(rou, "w", encoding="utf-8").write(re.sub(r"<!--.*?-->", "", body, flags=re.S))

    import sumolib
    sn = sumolib.net.readNet(net)
    # every lane of every internal-to-internal edge gets a loop halfway along it, all under ONE
    # station name so the station sum is the network total
    lanes = [ln.getID() for e in sn.getEdges() for ln in e.getLanes()
             if not e.getID().startswith(":")]
    lanes.sort()
    add = str(d / "e1.add.xml")
    with open(add, "w", encoding="utf-8", newline="\n") as fh:
        fh.write("<additional>\n")
        for i, lid in enumerate(lanes):
            half = sn.getLane(lid).getLength() / 2.0
            fh.write(f'  <e1Detector id="det{i}" lane="{lid}" pos="{half:.2f}" period="100000" '
                     f'name="ST" file="e1out.xml"/>\n')
        fh.write("</additional>\n")

    trace = str(d / "a.trace")
    r = subprocess.run([sys.executable, "-m", "scms_sim_ref.mock_pipeline.sumo_trace",
                        "--net", net, "--routes", rou, "--out", trace,
                        "--seed", "23423", "--steps", str(STEPS), "--dt", "1.0"],
                       capture_output=True, text=True, cwd=REPO,
                       env={**os.environ, "PYTHONPATH": os.path.join(REPO, "src")})
    assert r.returncode == 0, r.stderr
    # SUMO's own loops over the SAME run: identical seed, step length and teleport policy
    e1out = str(d / "e1out.xml")
    subprocess.run([os.path.join(home, "bin", "sumo.exe"), "-n", net, "-r", rou, "-a", add,
                    "--seed", "23423", "--step-length", "1.0", "--begin", "0.0",
                    "--end", str(float(STEPS)), "--threads", "1", "--no-step-log", "true",
                    "--no-warnings", "true", "--time-to-teleport", "-1",
                    "--ignore-route-errors", "true"],
                   check=True, capture_output=True, cwd=str(d))
    return {"dir": str(d), "net": net, "add": add, "trace": trace, "e1out": e1out,
            "lanes": lanes}


# --------------------------------------------------------------------------- #
# the gate: what it counts and what it refuses
# --------------------------------------------------------------------------- #
@needs_sumo
def test_gate_sits_on_the_lane_where_the_detector_says_it_does(net_with_loops):
    import sumolib
    sn = sumolib.net.readNet(net_with_loops["net"])
    gates = ed.build_gates(net_with_loops["net"], net_with_loops["add"])
    assert len(gates) == len(net_with_loops["lanes"])
    for did, g in gates.items():
        lane = sn.getLane(g["lane"])
        shape = [(float(x), float(y)) for x, y in lane.getShape()]
        # the gate point is ON the lane's own polyline, and the tangent is a unit vector
        d = min(_pt_seg(g["p"], a, b) for a, b in zip(shape, shape[1:]))
        assert d < 1e-6, f"{did}: gate point is {d:.4f} m off its own lane"
        assert abs((g["t"][0] ** 2 + g["t"][1] ** 2) ** 0.5 - 1.0) < 1e-9
        assert 0 < g["w"] <= lane.getWidth() / 2.0 + 1e-9


def _pt_seg(p, a, b):
    px, py = p
    ax, ay = a
    bx, by = b
    dx, dy = bx - ax, by - ay
    L2 = dx * dx + dy * dy
    u = 0.0 if L2 == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / L2))
    return ((ax + u * dx - px) ** 2 + (ay + u * dy - py) ** 2) ** 0.5


def _one_gate(p=(0.0, 0.0), t=(1.0, 0.0), w=1.6):
    return {"d": {"p": p, "t": t, "w": w, "station": "S", "lane": "l_0", "edge": "l"}}


@pytest.mark.parametrize("track,expect,why", [
    ([(-5.0, 0.0), (5.0, 0.0)], 1, "straight through the loop counts once"),
    ([(-5.0, 0.0), (-1.0, 0.0), (5.0, 0.0)], 1, "two samples either side still count ONCE"),
    ([(5.0, 0.0), (-5.0, 0.0)], 0, "the wrong way down the lane is not a crossing"),
    ([(0.0, -5.0), (0.0, 5.0)], 0, "crossing the loop sideways is the conflicting arm, not a pass"),
    ([(-5.0, 6.0), (5.0, 6.0)], 0, "a parallel road 6 m away is a different road"),
    ([(-5.0, 0.0), (-0.5, 0.0)], 0, "stopping short of the loop is not a pass"),
    ([(0.0, 0.0), (5.0, 0.0)], 0, "starting ON the loop is already past it (half-open gate)"),
    ([(-5.0, 0.0), (0.0, 0.0), (5.0, 0.0)], 1, "landing exactly on the loop counts once, not twice"),
    ([(-5.0, 0.0), (5.0, 0.0), (-5.0, 0.0), (5.0, 0.0)], 2, "two genuine passes count twice"),
])
def test_the_gate_discriminates(track, expect, why):
    samples = [("v", float(i), x, y) for i, (x, y) in enumerate(track)]
    res = ed.count_crossings(iter(samples), _one_gate())
    assert res["counts"]["d"] == expect, why


def test_a_step_longer_than_the_index_cell_is_refused_not_guessed():
    """A jump the index cannot bound is reported, never silently interpolated through."""
    samples = [("v", 0.0, -300.0, 0.0), ("v", 1.0, 300.0, 0.0)]
    res = ed.count_crossings(iter(samples), _one_gate())
    assert res["counts"]["d"] == 0
    assert res["skipped_long_steps"] == 1
    with pytest.raises(ValueError, match="superset"):
        ed.count_crossings(iter(samples), _one_gate(), cell_m=50.0, max_step_m=500.0)


def test_interleaved_vehicles_do_not_bleed_into_each_others_tracks():
    """The frozen trace is sorted by STEP, not by vehicle, so the counter must key on the vehicle."""
    samples = [("a", 0.0, -5.0, 0.0), ("b", 0.0, 5.0, 0.0),
               ("a", 1.0, 5.0, 0.0), ("b", 1.0, 15.0, 0.0)]
    res = ed.count_crossings(iter(samples), _one_gate())
    assert res["counts"]["d"] == 1 and res["n_vehicles"] == 2


# --------------------------------------------------------------------------- #
# end to end against SUMO's own loops
# --------------------------------------------------------------------------- #
@needs_sumo
def test_counts_from_the_trace_agree_with_sumos_own_detectors(net_with_loops):
    """The counter, run on the positions SUMO itself reported, must reproduce SUMO's loop counts.

    It cannot reproduce them exactly and must not claim to: the artifact samples at 1 Hz while the
    loop is continuous, and `nVehContrib` counts a vehicle that has COMPLETELY passed while this
    counts the front crossing. The tolerance is the one measured on InTAS (+4.3% network-total on a
    300 s window, every station's GEH under 1.0); anything outside it is a geometry bug, not
    sampling.
    """
    gates = ed.build_gates(net_with_loops["net"], net_with_loops["add"])
    res = ed.count_crossings(ed.iter_trace(net_with_loops["trace"]), gates)
    mine = sum(res["counts"].values())

    sumo = {}
    for _ev, el in ET.iterparse(net_with_loops["e1out"], events=("end",)):
        if el.tag == "interval":
            sumo[el.get("id")] = sumo.get(el.get("id"), 0.0) + float(el.get("nVehContrib", 0))
        el.clear()
    theirs = sum(sumo.values())

    assert theirs > 50, f"the control run must actually cross some loops (got {theirs})"
    assert abs(mine - theirs) / theirs < 0.10, (
        f"trajectory count {mine} vs SUMO's own loops {theirs} "
        f"({(mine - theirs) / theirs:+.1%}) -- outside the sampling tolerance")
    # and per detector, not only in aggregate: a compensating pair of errors must not pass
    worst = max(((d, res["counts"].get(d, 0), c) for d, c in sumo.items() if c >= 10),
                key=lambda r: abs(r[1] - r[2]), default=None)
    if worst is not None:
        assert abs(worst[1] - worst[2]) <= max(3, 0.25 * worst[2]), f"worst detector: {worst}"


@needs_sumo
def test_the_written_xml_is_read_back_by_sumo_realism_unchanged(net_with_loops, tmp_path):
    """The output must be consumable by the EXISTING grading path, with no new reader."""
    sys.path.insert(0, os.path.join(REPO, "tools"))
    try:
        import sumo_realism as sr
    finally:
        sys.path.pop(0)
    gates = ed.build_gates(net_with_loops["net"], net_with_loops["add"])
    res = ed.count_crossings(ed.iter_trace(net_with_loops["trace"]), gates)
    out = str(tmp_path / "counts.xml")
    ed.write_e1_xml(out, res["counts"], 0.0, float(STEPS), gates)

    parsed = sr.parse_e1_output(out)
    assert parsed["counts"] == {d: float(c) for d, c in res["counts"].items()}
    assert parsed["duration_s"] == pytest.approx(float(STEPS))
    mapping = sr.parse_e1_additional(net_with_loops["add"])
    flows = sr.to_station_flows(parsed, mapping, "station")
    assert "ST" in (flows["flows"] if isinstance(flows, dict) and "flows" in flows else flows)
