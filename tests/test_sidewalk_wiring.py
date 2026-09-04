"""`sidewalks`: the pedestrian network, wired into the engine and graded on where VRUs actually are.

`vru.py` builds sidewalks, crossings and corners and hands out `PedestrianWalk` itineraries, and
`tests/test_vru_sidewalks.py` grades all of that in isolation. None of it was reachable from a
config: `run.make_vru` placed a VRU `offroad_tol_m * (1.3..2.0)` metres off a random junction in
BOTH axes and left `trip=None`, so `Vehicle.true_state` walked it in a dead straight line at a
constant speed for its whole life. Measured on InTAS that put 5.59% of its sampled positions on a
pedestrian-legal area and 54.47% of them beyond `offroad_tol_m` of any road.

This file grades the WIRING -- the flag, the network build, the substitution of the walk for the
straight line, and the RNG isolation that keeps a sidewalk run from disturbing anything else. It
measures on a 5x5 grid so it always runs; the InTAS numbers are in the task report.
"""
from __future__ import annotations

import json

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.mock_pipeline import vru as V
from scms_sim_ref.mock_pipeline.roads import GridNetwork
from scms_sim_ref.mock_pipeline.run import validate_config

BASE = dict(seed=9, traffic_flow=True, road_network="grid", grid_w=5, grid_h=5,
            grid_block_m=120.0, duration_s=90.0, dt=0.5, arrival_rate=1.5, attacker_pct=0.2,
            vru_pct=0.25, vru_speed_mps=1.4, emit_mobility_oracle=True, verbose=False)


def _run(tmp_path, tag, **kw):
    return run_pipeline(PipelineConfig(out_dir=str(tmp_path / tag), **{**BASE, **kw}))


def _vru_positions(res):
    out = []
    with open(f"{res.out_dir}/ground_truth/gt_mobility_oracle.jsonl", encoding="utf-8") as fh:
        for ln in fh:
            r = json.loads(ln)
            if r.get("is_vru"):
                out.append((float(r["true_x"]), float(r["true_y"])))
    return out


@pytest.fixture(scope="module")
def yardstick():
    """The pedestrian layer BOTH arms are measured against -- one definition, two columns."""
    net = GridNetwork(BASE["grid_w"], BASE["grid_h"], BASE["grid_block_m"])
    return net, V.build_sidewalks(net, lane_width_m=3.5, lanes_per_dir=1, drive_side="right",
                                 sidewalk_width_m=2.0, kerb_clearance_m=0.5)


# --------------------------------------------------------------------------- the knob ----------- #
def test_the_knob_exists_end_to_end():
    cfg = PipelineConfig()
    assert (cfg.sidewalks, cfg.sidewalk_width_m, cfg.kerb_clearance_m,
            cfg.crossing_wait_max_s) == (False, 2.0, 0.5, 8.0)
    sch = config_schema()
    assert sch["sidewalks"]["group"] == "Network" and sch["sidewalks"]["default"] is False
    assert sch["sidewalk_width_m"]["unit"] == "m"
    assert sch["crossing_wait_max_s"]["group"] == "Mobility"
    import contextlib
    import io
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), pytest.raises(SystemExit):
        runmod.main(["--help"])
    txt = buf.getvalue()
    for flag in ("--sidewalks", "--sidewalk-width", "--kerb-clearance", "--crossing-wait-max"):
        assert flag in txt


@pytest.mark.parametrize("kw, msg", [
    (dict(vru_pct=0.0), "vru_pct > 0"),
    (dict(road_network="linear", traffic_flow=False, arrival_rate=0.0, duration_s=0.0),
     "routed road network"),
])
def test_a_dead_knob_is_refused_rather_than_ignored(kw, msg):
    with pytest.raises(ValueError, match=msg):
        validate_config(PipelineConfig(**{**BASE, **kw, "sidewalks": True}))


@pytest.mark.parametrize("field, value", [("sidewalk_width_m", 0.0), ("kerb_clearance_m", -1.0),
                                          ("crossing_wait_max_s", -1.0)])
def test_geometry_knobs_are_range_checked(field, value):
    with pytest.raises(ValueError, match=field):
        validate_config(PipelineConfig(**{**BASE, field: value}))


# --------------------------------------------------------------------------- the measurement ---- #
def test_vrus_move_onto_the_footways(tmp_path, yardstick):
    """The number this whole module exists to move: what fraction of a VRU's positions in a REAL
    RUN are somewhere a pedestrian may legally be. Both columns are the same measurement --
    `vru.measure_positions` against one derived pedestrian layer -- so the comparison is of the
    engine's behaviour, not of two definitions."""
    net, sw = yardstick
    before = V.measure_positions(_vru_positions(_run(tmp_path, "off", sidewalks=False)), sw, net)
    after = V.measure_positions(_vru_positions(_run(tmp_path, "on", sidewalks=True)), sw, net)
    assert before["n"] > 2000 and after["n"] > 2000
    assert before["on_legal_frac"] < 0.20, before
    assert after["on_legal_frac"] > 0.99, after
    assert after["legal_dist_p95"] <= before["legal_dist_p95"]
    assert after["stray_max_m"] < 1.0 <= before["stray_max_m"]
    # ... and they stop being 15 m off the road, which is what made the mapOffRoad exemption a
    # free prize for anything declaring station_type=vru
    assert after["road_dist_p95"] < before["road_dist_p95"]


def test_a_vru_gets_a_pedestrian_itinerary_not_a_straight_line(tmp_path):
    """`Vehicle.trip` is the substitution: a `PedestrianWalk` drops into `true_state`'s existing
    `trip is not None` branch, so the VRU walks legs, waits at kerbs and traverses crossings."""
    seen: list = []
    orig = V.SidewalkNetwork.walk

    def spy(self, *a, **kw):
        w = orig(self, *a, **kw)
        seen.append(w)
        return w

    V.SidewalkNetwork.walk = spy
    try:
        _run(tmp_path, "walks", sidewalks=True)
    finally:
        V.SidewalkNetwork.walk = orig
    assert len(seen) > 5
    assert all(isinstance(w, V.PedestrianWalk) for w in seen)
    assert sum(w.legs for w in seen) > 2 * len(seen)        # more than one leg each, on average
    assert sum(w.crossings for w in seen) > 0               # crossings are traversed
    assert max(w.length for w in seen) > 20.0


def test_a_sidewalk_run_is_deterministic(tmp_path):
    a = _run(tmp_path, "det_a", sidewalks=True)
    b = _run(tmp_path, "det_b", sidewalks=True)
    assert a.data_digest == b.data_digest


def test_the_walk_stream_is_the_only_thing_that_moves(tmp_path):
    """`SidewalkNetwork.walk` draws exclusively from `f"{seed}:vruwalk:{vid}"`, a stream that
    exists nowhere else. The VRU population -- how many arrive and when -- comes from
    `f"{seed}:vruflow"` and must be untouched, or the two arms are not comparable runs."""
    off = _run(tmp_path, "s_off", sidewalks=False)
    on = _run(tmp_path, "s_on", sidewalks=True)

    def vrus(res):
        with open(f"{res.out_dir}/ground_truth/gt_vehicle.jsonl", encoding="utf-8") as fh:
            return [(r["true_vehicle_id"], r["spawn_time"])
                    for r in map(json.loads, fh) if r.get("is_vru")]
    assert vrus(off) == vrus(on) and len(vrus(on)) > 5
    assert off.n_vehicles == on.n_vehicles


def test_the_manifest_records_what_was_built(tmp_path):
    res = _run(tmp_path, "man", sidewalks=True)
    man = json.load(open(f"{res.out_dir}/manifest.json", encoding="utf-8"))
    st = man["counts"]["sidewalks"]
    assert st["ped_links"] > 100 and st["crossings"] > 0
    assert st["sidewalk_width_m"] == 2.0 and st["kerb_clearance_m"] == 0.5
    # with traffic_lights off and no real programs, NOTHING is signalised -- the pedestrians are on
    # the same clock as the vehicles, which is the whole reason the set is chosen and not defaulted
    assert st["signalised_crossings"] == 0
    lit = _run(tmp_path, "man_lit", sidewalks=True, traffic_lights=True)
    st2 = json.load(open(f"{lit.out_dir}/manifest.json",
                         encoding="utf-8"))["counts"]["sidewalks"]
    assert st2["signalised_crossings"] == st2["crossings"] > 0


def test_a_signalised_crossing_asks_the_signal_about_the_ARM_it_crosses(tmp_path):
    """A derived crossing joins one arm's two kerbs, so its own heading is PERPENDICULAR to the
    traffic it conflicts with. Asking the signal about the crossing's heading answers about the
    cross street -- 90 degrees wrong, holding the pedestrian through the conflicting movement's red
    and releasing it into the green. The arm bearing is recorded on the link at build time."""
    net = GridNetwork(4, 4, 120.0)
    sw = V.build_sidewalks(net)
    xs = [lk for lk in sw.links if lk["kind"] == V.CROSSING]
    assert xs and all("arm" in lk for lk in xs)
    for lk in xs:
        (ax, ay), (bx, by) = lk["pts"][0], lk["pts"][-1]
        import math
        own = math.degrees(math.atan2(by - ay, bx - ax)) % 360.0
        d = abs(own - lk["arm"]) % 360.0
        d = d if d <= 180.0 else 360.0 - d
        assert 80.0 <= d <= 100.0, (own, lk["arm"])
    # and the wait solver really uses it: a pedestrian waits when the conflicting ARM is green
    asked: list = []

    def probe(node_xy, arm_deg, t):
        asked.append(arm_deg)
        return True
    sw.walk(0, 7, 0.0, 200.0, 1.4, signal_fn=probe)
    assert asked, "no signalised crossing was solved"
    arms = {round(a) % 180 for a in asked}
    assert arms <= {0, 90}, arms                 # grid arms run E-W or N-S, never diagonally
