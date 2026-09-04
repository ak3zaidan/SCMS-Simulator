"""Road-following pedestrians: the sidewalk layer, the OSM pedestrian tags, and the realism delta.

WHAT IS UNDER TEST. `mock_pipeline/vru.py` derives a walkable graph (sidewalks + crossings +
corners) from a road network and gives a VRU a `Trip`-compatible itinerary on it, replacing the
straight-line off-road random walk `run.make_vru` + `Vehicle.true_state` produce today.
`mock_pipeline/osm.py` gains the pedestrian half of the tag reader (`pedestrian_tag_stats`,
`extract_footways`).

THE FOUR THINGS THAT MUST HOLD, and why each is a test rather than a comment:

  1. BYTE-IDENTICAL DEFAULT. The module is not on any default path; importing it, building a
     sidewalk network and walking it must draw nothing from the global RNG and must leave the
     published default digest untouched.
  2. OUTSIDE THE CARRIAGEWAY. A sidewalk that overlaps a traffic lane is WORSE than the random walk
     it replaces -- it would give a map-based detector evidence that a lane is a footway. The offset
     arithmetic is checked against `roads`' own lane frame, exactly, and the resulting geometry is
     checked against every carriageway on the map including the ones it does not belong to.
  3. THE REALISM DELTA IS REAL. Same seeds, same VRU ids, same sampling cadence, one difference:
     the mobility model. Both columns come from the same `measure_positions`.
  4. NO CAR-FOLLOWING SURFACE. A pedestrian is not a car-following actor and must not accidentally
     look like one, because vehicles do not yield to it (see the module docstring).
"""

import math
import random

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import osm as O
from scms_sim_ref.mock_pipeline import vru as V
from scms_sim_ref.mock_pipeline.roads import CustomNetwork, GridNetwork

# The published default golden (mirrors tests/test_vru.py). Nothing in this feature is on the
# default path, so it must be unchanged by the module's existence.
GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


def _grid(w=5, h=5, block=120.0, lanes=1, lane_w=3.5, side="right"):
    net = GridNetwork(w, h, block)
    net.enable_directed_lanes(lane_width_m=lane_w, drive_side=side, lanes_per_dir=lanes)
    return net


@pytest.fixture(scope="module")
def gridwalk():
    net = _grid()
    return net, V.build_sidewalks(net)


# --------------------------------------------------------------------------- #
# 1. byte-identical default / zero new RNG
# --------------------------------------------------------------------------- #
def test_default_golden_unchanged_by_the_sidewalk_module(tmp_path):
    """Importing vru.py and using it changes no default output: the published digest still holds."""
    net = _grid()
    sw = V.build_sidewalks(net)
    sw.walk(0, 7, 0.0, 90.0, 1.8)                    # exercise the module, then run the default
    r = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network="grid", duration_s=60,
                                    arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,
                                    out_dir=str(tmp_path / "g")))
    assert r.data_digest == GOLDEN


def test_building_and_walking_draws_nothing_from_the_global_rng(gridwalk):
    """Every draw comes from `random.Random(f"{seed}:vruwalk:{vid}")`. If any code path fell back to
    the module-level `random`, the global stream would advance and every other stream in the run
    would shift -- which is precisely how a "harmless" feature breaks a pinned digest."""
    net, sw = gridwalk
    random.seed(12345)
    expect = [random.random() for _ in range(5)]
    random.seed(12345)
    sw2 = V.build_sidewalks(net)
    for vid in range(20):
        sw2.walk(vid, 7, 0.0, 90.0, 1.8, signal_fn=V.engine_signal_fn(net, 12.0))
    V.measure_positions([(0.0, 0.0), (10.0, 10.0)], sw2, net)
    assert [random.random() for _ in range(5)] == expect


def test_walks_are_deterministic_and_per_vehicle(gridwalk):
    _net, sw = gridwalk
    a = sw.walk(3, 7, 0.0, 90.0, 1.8)
    b = sw.walk(3, 7, 0.0, 90.0, 1.8)
    c = sw.walk(4, 7, 0.0, 90.0, 1.8)
    d = sw.walk(3, 8, 0.0, 90.0, 1.8)
    assert a.pts == b.pts and a.tarr == b.tarr
    assert (a.pts, a.tarr) != (c.pts, c.tarr)        # a different VRU walks somewhere else
    assert (a.pts, a.tarr) != (d.pts, d.tarr)        # ... and so does a different seed


def test_build_is_deterministic_and_a_linear_world_has_no_sidewalks():
    """`road_network="linear"` has no graph (`net is None`): keeping today's behaviour is the
    correct answer there, not a fallback -- there is no geometry to derive a footway from."""
    assert V.build_sidewalks(None) is None
    s1 = V.build_sidewalks(_grid()).stats()
    s2 = V.build_sidewalks(_grid()).stats()
    assert s1 == s2


# --------------------------------------------------------------------------- #
# 2. the sidewalk lands OUTSIDE the carriageway -- the arithmetic, exactly
# --------------------------------------------------------------------------- #
@pytest.mark.parametrize("lanes,lane_w", [(1, 3.5), (2, 3.5), (3, 3.0), (1, 2.75)])
def test_sidewalk_offset_equals_halfwidth_plus_kerb_plus_half_footway(lanes, lane_w):
    """The offset is DERIVED from `roads`' lane frame, not chosen.

    `_LaneFrameMixin.carriageway_offset` puts the a->b carriageway centre at
    `side * 0.5 * L * W` in a->b's own left normal, and `run.py` spreads that direction's lanes by
    `(i - (L-1)/2) * W` about that centre, so the outer edge of the outermost lane is at
    `0.5*L*W + 0.5*L*W = L*W` from the road centreline. The footway CENTRE therefore belongs at
    `L*W + kerb_clearance + sidewalk_width/2`, and its INNER edge at `L*W + kerb_clearance`.
    """
    net = _grid(lanes=lanes, lane_w=lane_w)
    sw = V.build_sidewalks(net)
    st = sw.stats()
    expect_centre = lanes * lane_w + V.KERB_CLEARANCE_M + 0.5 * V.SIDEWALK_WIDTH_M
    lo, _med, hi = st["sidewalk_offset_m"]
    assert lo == hi == pytest.approx(expect_centre, abs=1e-6), st["sidewalk_offset_m"]
    # ... and the arithmetic agrees with the engine's own carriageway offset for that direction
    outer_lane_edge = abs(net.carriageway_offset(lanes, False)) + 0.5 * lanes * lane_w
    assert outer_lane_edge == pytest.approx(lanes * lane_w, abs=1e-9)
    assert expect_centre - 0.5 * V.SIDEWALK_WIDTH_M == pytest.approx(
        outer_lane_edge + V.KERB_CLEARANCE_M, abs=1e-9)


def test_no_kept_sidewalk_position_is_inside_any_carriageway_grid(gridwalk):
    """The invariant on a grid: densely sampled, every sidewalk position is clear of every lane."""
    net, sw = gridwalk
    st = sw.stats()
    assert st["max_residual_penetration_m"] <= 0.0, st
    assert st["min_tarmac_clearance_m"] >= 0.0, st
    # independently re-derived, not read back off the same provenance dict
    carr = _carriageway_index(net)
    worst = -math.inf
    for lk in sw.links:
        if lk["kind"] != V.SIDEWALK:
            continue
        _ss, sp = V._sample_polyline(lk["pts"], 0.5)
        for x, y in sp:
            worst = max(worst, carr.penetration(x, y))
    assert worst <= 0.0, f"a sidewalk is {worst:.3f} m inside a traffic lane"


def _carriageway_index(net):
    geo = net.geometry()
    nodes = [(float(p[0]), float(p[1])) for p in geo["nodes"]]
    e_pts, e_half = [], []
    ep = getattr(net, "edge_points", None)
    lf = getattr(net, "lanes_for", None)
    ow = getattr(net, "is_oneway", None)
    lpd = int(getattr(net, "_lanes_per_dir", 1))
    for e in geo["edges"]:
        a, b = int(e[0]), int(e[1])
        e_pts.append([tuple(p) for p in ep(a, b)] if ep else [nodes[a], nodes[b]])
        e_half.append(V._halfwidths(int(lf(a, b)) if lf else lpd, int(lf(b, a)) if lf else lpd,
                                    bool(ow(a, b)) if ow else False, net._lane_w, net._side,
                                    True, lpd))
    return V._CarriagewayIndex(e_pts, e_half)


def test_a_footway_that_lands_on_a_NEIGHBOURING_road_is_pushed_or_trimmed_away():
    """Own-road arithmetic is not sufficient. Two roads 6 m apart: a footway offset correctly from
    the first lands inside the second, and must be pushed out or cut away rather than kept."""
    # two parallel two-way roads, 6 m apart: each carriageway is +/-3.5 m, so the 5 m footway
    # offset of the lower road falls squarely inside the upper road's near lane.
    nodes = [[0.0, 0.0], [400.0, 0.0], [0.0, 6.0], [400.0, 6.0],
             [0.0, 300.0], [400.0, 300.0]]
    edges = [[0, 1, 13.9], [2, 3, 13.9], [4, 5, 13.9], [0, 2, 13.9], [1, 3, 13.9],
             [2, 4, 13.9], [3, 5, 13.9]]
    net = CustomNetwork(nodes, edges)
    net.enable_directed_lanes(lane_width_m=3.5, lanes_per_dir=1)
    sw = V.build_sidewalks(net)
    st = sw.stats()
    assert st["max_residual_penetration_m"] <= 0.0, st
    assert st["sidewalk_sides_trimmed"] + st["sidewalk_sides_dropped"] > 0, \
        "the 6 m-apart pair must have forced a push/trim somewhere"
    carr = _carriageway_index(net)
    for lk in sw.links:
        if lk["kind"] != V.SIDEWALK:
            continue
        _ss, sp = V._sample_polyline(lk["pts"], 0.5)
        assert max(carr.penetration(x, y) for x, y in sp) <= 0.0


def test_a_crossing_is_bounded_by_the_road_it_crosses(gridwalk):
    """A crossing is the chord between one edge's two footway ends, so it is at most that road's
    width plus kerbs. When one side has been trimmed back the surviving ends can be hundreds of
    metres apart along the street, and the chord between them is a legal pedestrian area running
    down the middle of a carriageway. Measured on InTAS before this bound: crossing lengths reached
    445.1 m."""
    net, sw = gridwalk
    st = sw.stats()
    # a 1+1 lane grid road is 7 m of tarmac; footway centres sit at 5.0 m either side
    assert st["crossing_max_m"] <= 1.5 * (2 * 5.0) + V.LINK_SLACK_M + 1e-6, st
    assert st["corner_max_m"] <= 2.0 * (5.0 + V.KERB_CLEARANCE_M) + V.LINK_SLACK_M + 1e-6, st
    for i, lk in enumerate(sw.links):
        if lk["kind"] == V.CROSSING:
            assert lk["len"] <= st["crossing_max_m"] + 1e-9


def test_the_walkable_graph_reports_its_own_fragmentation_and_spawns_on_the_main_piece(gridwalk):
    """Trimming and the length bounds cut links, so the graph can fragment. A pedestrian confined to
    a three-link island is legal-by-construction and still implausible, which is exactly the kind of
    defect the on-legal-area number cannot see -- so it is counted, and starts avoid it."""
    _net, sw = gridwalk
    st = sw.stats()
    assert st["components"] >= 1
    assert st["largest_component_node_share"] > 0.5, st
    assert st["walk_start_links"] > 0
    comp_nodes, _cl, _nc, comp = sw._largest_component()
    assert comp_nodes == st["largest_component_nodes"]
    assert all(sw.links[li]["a"] in comp for li in sw._walk_start_links)


def test_left_hand_traffic_mirrors_the_offset_and_still_clears_the_tarmac():
    net = _grid(lanes=2, side="left")
    sw = V.build_sidewalks(net)
    st = sw.stats()
    assert st["drive_side"] == "left"
    assert st["max_residual_penetration_m"] <= 0.0
    assert st["sidewalk_offset_m"][0] == pytest.approx(2 * 3.5 + 0.5 + 1.0)


def test_undirected_network_uses_the_symmetric_lane_spread():
    """Without `enable_directed_lanes`, `run.py` spreads `n_lanes` symmetrically about the
    centreline (`lane_off = (i-(L-1)/2)*W`), so the half-width is `L*W/2` on BOTH sides -- half of
    what a directed 1+1 road occupies. `_halfwidths` must reproduce that, or the footway on an
    undirected map sits a lane too far out."""
    net = GridNetwork(4, 4, 120.0)                  # NOT directed
    sw = V.build_sidewalks(net, lane_width_m=3.5, lanes_per_dir=2)
    assert sw.stats()["directed_lanes"] is False
    assert sw.stats()["sidewalk_offset_m"][0] == pytest.approx(0.5 * 2 * 3.5 + 0.5 + 1.0)


# --------------------------------------------------------------------------- #
# 3. the realism delta
# --------------------------------------------------------------------------- #
def test_before_and_after_measured_on_the_same_protocol(gridwalk):
    """The whole point, in one assertion pair. Same seed, same vids, same cadence, same metric."""
    net, sw = gridwalk
    kw = dict(seed=7, n=60, life_s=90.0, dt=1.0, speed=1.8)
    before = V.legacy_offroad_positions(net, **kw)
    after = V.sidewalk_positions(sw, signal_fn=V.engine_signal_fn(net, 12.0), **kw)
    assert len(before) == len(after)
    mb = V.measure_positions(before, sw, net)
    ma = V.measure_positions(after, sw, net)
    # BEFORE: the off-road random walk is almost never on a pedestrian-legal area, and strays far
    assert mb["on_legal_frac"] < 0.20, mb
    assert mb["legal_dist_p50"] > 5.0 and mb["stray_max_m"] > 50.0, mb
    # AFTER: a pedestrian is ON the pedestrian layer, always, by construction
    assert ma["on_legal_frac"] == 1.0, ma
    assert ma["legal_dist_max"] == 0.0 and ma["stray_max_m"] == 0.0, ma
    # ... and it is no longer implausibly far from the road network either
    assert ma["road_dist_p95"] < mb["road_dist_p95"], (ma, mb)


def test_the_before_baseline_reproduces_run_make_vru_exactly(gridwalk):
    """`legacy_offroad_positions` is the measurement baseline; if it drifts from `run.make_vru` the
    before-column stops being the shipped behaviour. Re-derive the first sample independently."""
    net, _sw = gridwalk
    nodes = [(float(p[0]), float(p[1])) for p in net.geometry()["nodes"]]
    for vid in (0, 1, 7):
        vr = random.Random(f"7:vru:{vid}")
        nx, ny = nodes[vr.randrange(len(nodes))]
        dx = 15.0 * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
        dy = 15.0 * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
        direction = vr.random() * 6.283
        wander_amp = 0.5 + vr.random()
        _wander_w = 0.1 + vr.random() * 0.2
        phase = vr.random() * 6.283
        # `Vehicle.true_state` applies the lateral wander at t == spawn_time too (along = 0 but
        # lat = amp*sin(phase)), so the spawn SAMPLE is not the spawn POINT.
        lat = wander_amp * math.sin(phase)
        nxx, nyy = -math.sin(direction), math.cos(direction)
        got = V.legacy_offroad_positions(net, seed=7, n=1, vid0=vid, life_s=1.0, dt=1.0)[0]
        assert got == pytest.approx((nx + dx + lat * nxx, ny + dy + lat * nyy))
        # and it is placed off-road by MORE than the mapOffRoad tolerance, in both axes, as
        # `run.make_vru` documents ("a naive mapOffRoad check WOULD flag them")
        assert abs(dx) > 15.0 and abs(dy) > 15.0


def test_pedestrians_wait_at_kerbs_and_walk_at_walking_speed(gridwalk):
    net, sw = gridwalk
    sig = V.engine_signal_fn(net, 12.0)
    walks = [sw.walk(v, 7, 0.0, 120.0, 1.4, signal_fn=sig) for v in range(40)]
    assert any(w.crossings for w in walks), "some pedestrian must cross a road"
    assert any(w.waits for w in walks), "some pedestrian must wait at a kerb"
    speeds = [w.state(t)[2] for w in walks for t in range(0, 120)]
    assert min(speeds) == 0.0                          # a kerb wait is genuinely stationary
    assert max(speeds) <= 1.4 + 1e-9                   # and nothing exceeds the walking speed
    assert sum(1 for s in speeds if s == 0.0) > 0


def test_crossing_wait_is_signal_solved_not_random(gridwalk):
    """At a SIGNALISED crossing the wait is solved from the signal, so it is a property of the
    junction and the clock -- not of the seed. Two different RNGs must give the same answer."""
    net, sw = gridwalk
    sig = V.engine_signal_fn(net, 12.0)
    li = next(i for i, lk in enumerate(sw.links)
              if lk["kind"] == V.CROSSING and sw.signalised[i] and sw.junction_of[i] is not None)
    a = sw._crossing_wait(li, 3.0, random.Random("a"), sig, 8.0, 0.0)
    b = sw._crossing_wait(li, 3.0, random.Random("b"), sig, 8.0, 0.0)
    assert a == b
    assert a % V._SIGNAL_PROBE_S == pytest.approx(0.0)
    # an UNSIGNALISED crossing falls back to the bounded gap-acceptance surrogate, which IS seeded
    u = sw._crossing_wait(li, 3.0, random.Random("a"), None, 8.0, 0.0)
    assert 0.0 <= u <= 8.0


def test_signalising_only_the_real_junctions_changes_the_share(gridwalk):
    """`signal_nodes=None` signalises every junction (what `cfg.traffic_lights` does today); a
    coordinate list signalises only those. Coordinates, not indices: `largest_strong_component`
    remaps node indices and does not return the remap."""
    net, sw_all = gridwalk
    assert sw_all.stats()["signalised_share"] == 1.0
    coords = [tuple(p) for p in net.geometry()["nodes"][:4]]
    sw_some = V.build_sidewalks(net, signal_nodes=coords)
    st = sw_some.stats()
    assert st["signal_nodes_requested"] == 4 and st["signal_nodes_matched"] == 4
    assert st["signal_nodes_unmatched"] == 0
    assert 0.0 < st["signalised_share"] < 1.0, st
    none_at_all = V.build_sidewalks(net, signal_nodes=())
    assert none_at_all.stats()["signalised_share"] == 0.0
    # a coordinate that is not a junction is COUNTED as unmatched, never silently signalises another
    bogus = V.build_sidewalks(net, signal_nodes=[(-99999.0, -99999.0)])
    assert bogus.stats()["signal_nodes_unmatched"] == 1
    assert bogus.stats()["signalised_share"] == 0.0


# --------------------------------------------------------------------------- #
# 4. a pedestrian is NOT a car-following actor (and nothing yields to it)
# --------------------------------------------------------------------------- #
def test_pedestrianwalk_exposes_no_car_following_surface(gridwalk):
    """`run.car_follow` filters `[v for v in active_list if v.cf]` and `make_vru` never sets `cf`,
    so a pedestrian is never an IDM leader nor a gap-acceptance claimant -- vehicles do not yield.
    The walk object must not accidentally answer the car-following questions either: something that
    reached for them would be a bug worth failing on, not one worth silently answering."""
    _net, sw = gridwalk
    w = sw.walk(1, 7, 0.0, 90.0, 1.8)
    for attr in ("caps", "nodes", "lanes", "next_node", "next_turn", "cf"):
        assert not hasattr(w, attr), attr
    for attr in ("state", "at_distance", "speed", "length", "t0", "t1"):
        assert hasattr(w, attr), attr


def test_state_is_total_and_clamped_at_both_ends(gridwalk):
    _net, sw = gridwalk
    w = sw.walk(2, 7, 10.0, 60.0, 1.8)
    assert w.state(-5.0)[:2] == w.pts[0] and w.state(-5.0)[2] == 0.0
    assert w.state(1e6)[:2] == w.pts[-1] and w.state(1e6)[2] == 0.0
    prev = w.state(w.t0)[:2]
    for k in range(1, 400):                             # continuity: no teleports
        cur = w.state(w.t0 + k * 0.25)[:2]
        assert math.dist(prev, cur) <= 1.8 * 0.25 + 1e-6
        prev = cur


def test_exposure_on_a_crossing_is_reported_not_hidden(gridwalk):
    """`kind_at` is the exposure metric: it says how much of a pedestrian's life is spent on tarmac
    that no driver in this model brakes for. It must be non-zero (pedestrians do cross) and it must
    be reported rather than implied."""
    net, sw = gridwalk
    sig = V.engine_signal_fn(net, 12.0)
    walks = [sw.walk(v, 7, 0.0, 90.0, 1.8, signal_fn=sig) for v in range(60)]
    kinds = [w.kind_at(t) for w in walks for t in range(90)]
    on_crossing = sum(1 for k in kinds if k == V.CROSSING)
    assert 0 < on_crossing < len(kinds)
    # a pedestrian WAITING at a kerb is not on the crossing yet
    waiter = next((w for w in walks if w.waits), None)
    assert waiter is not None
    k = next(i for i in range(1, len(waiter.tarr)) if waiter.pts[i] == waiter.pts[i - 1])
    assert waiter.kind_at_pt[k] == V.SIDEWALK


def test_geometry_export_is_map_data_and_names_no_actor(gridwalk):
    """The pedestrian layer is MAP geometry (like the road arrays), never oracle truth."""
    _net, sw = gridwalk
    g = sw.geometry()
    assert set(g) == {"ped_links", "ped_stats"}
    kinds = {lk["kind"] for lk in g["ped_links"]}
    assert kinds <= {"sidewalk", "crossing", "corner"} and "sidewalk" in kinds
    blob = repr(g)
    for forbidden in ("vid", "is_vru", "true_vehicle_id", "attacker", "cert"):
        assert forbidden not in blob


# --------------------------------------------------------------------------- #
# 5. the OSM pedestrian layer
# --------------------------------------------------------------------------- #
def _osm(ways, nodes=()):
    """Minimal OSM XML. `ways` = [(tags dict, [(lat, lon), ...])]; `nodes` = [(lat, lon, tags)]."""
    out = ['<?xml version="1.0"?><osm version="0.6">']
    nid = 1
    body = []
    for tags, pts in ways:
        refs = []
        for la, lo in pts:
            out.append(f'<node id="{nid}" lat="{la}" lon="{lo}"/>')
            refs.append(nid)
            nid += 1
        t = "".join(f'<tag k="{k}" v="{v}"/>' for k, v in tags.items())
        body.append("<way id=\"%d\">%s%s</way>"
                    % (nid, "".join(f'<nd ref="{r}"/>' for r in refs), t))
        nid += 1
    for la, lo, tags in nodes:
        t = "".join(f'<tag k="{k}" v="{v}"/>' for k, v in tags.items())
        out.append(f'<node id="{nid}" lat="{la}" lon="{lo}">{t}</node>')
        nid += 1
    return "".join(out) + "".join(body) + "</osm>"


ROADS = [({"highway": "residential", "sidewalk": "both"}, [(48.10, 11.50), (48.10, 11.505)]),
         ({"highway": "residential", "sidewalk": "separate"}, [(48.10, 11.505), (48.101, 11.505)]),
         ({"highway": "residential", "sidewalk": "no"}, [(48.101, 11.505), (48.101, 11.50)]),
         ({"highway": "residential"}, [(48.101, 11.50), (48.10, 11.50)]),
         ({"highway": "residential", "sidewalk:left": "yes"},
          [(48.10, 11.50), (48.101, 11.505)])]
FOOT = [({"highway": "footway"}, [(48.1002, 11.5002), (48.1002, 11.5045)]),
        ({"highway": "steps"}, [(48.1005, 11.5002), (48.1006, 11.5004)]),
        ({"highway": "cycleway"}, [(48.1008, 11.5002), (48.1008, 11.5040)]),
        ({"highway": "cycleway", "foot": "designated"},
         [(48.1009, 11.5002), (48.1009, 11.5040)]),
        ({"highway": "footway", "foot": "no"}, [(48.1007, 11.5002), (48.1007, 11.5040)])]
CROSS = [(48.1000, 11.5025, {"highway": "crossing", "crossing": "traffic_signals"}),
         (48.1010, 11.5050, {"highway": "crossing", "crossing": "unmarked"}),
         (48.1005, 11.5000, {"highway": "crossing"})]


def test_pedestrian_tag_stats_separates_the_four_sidewalk_verdicts():
    """`separate` (the footway exists as its own way), `no`, a named side, and silence are four
    different facts, and a reader that conflates them either double-counts a footway or invents one
    where the map says there is none."""
    st = O.pedestrian_tag_stats(_osm(ROADS + FOOT, CROSS))
    assert st["drivable_ways"] == 5
    assert st["sidewalk_verdict"] == {"both": 1, "separate": 1, "none": 1, "left": 1, "untagged": 1}
    assert st["sidewalk_tagged_ways"] == 4
    assert st["sidewalk_tagged_share"] == pytest.approx(0.8)
    # walkable ways: footway + steps + the foot=designated cycleway; NOT the bare cycleway, NOT
    # the foot=no footway
    assert st["ped_way_classes"] == {"footway": 1, "steps": 1, "cycleway": 1}
    assert st["ped_ways_rejected"] == {"cycleway": 1, "footway": 1}
    assert st["crossing_nodes"] == 3 and st["crossing_nodes_signalised"] == 1


def test_extract_footways_projects_into_the_road_frame_and_keeps_the_crossings():
    xml = _osm(ROADS + FOOT, CROSS)
    _n, _e, info = O.osm_to_network(xml, tol_m=1.0)
    fw, cross, finfo = O.extract_footways(xml, info["projection"], info["road_bbox"])
    assert finfo["footways"] == len(fw) >= 2
    assert finfo["crossings"] == 3 and finfo["crossings_signalised"] == 1
    assert all(c[2] in ("traffic_signals", "unmarked", "unknown") for c in cross)
    # the footways land ON the road extent, which is the whole reason the projection is threaded
    rx0, ry0, rx1, ry1 = info["road_bbox"]
    for w in fw:
        for x, y in w:
            assert rx0 - 200 <= x <= rx1 + 200 and ry0 - 200 <= y <= ry1 + 200
    assert finfo["tag_coverage"]["drivable_ways"] == 5


def test_extract_footways_refuses_a_wrong_projection_loudly():
    """The projection trap: a pedestrian layer with a re-derived origin lands city blocks off the
    roads while every individual footway still looks like a footway. It must be a GATE."""
    xml = _osm(ROADS + FOOT, CROSS)
    _n, _e, info = O.osm_to_network(xml, tol_m=1.0)
    good = info["projection"]
    with pytest.raises(ValueError, match="projection trap"):
        O.extract_footways(xml, dict(good, lat0=good["lat0"] - 0.05), info["road_bbox"])
    with pytest.raises(ValueError, match="projection trap"):
        O.extract_footways(xml, dict(good, lon0=good["lon0"] - 0.05), info["road_bbox"])


def test_extract_tag_stats_gained_the_pedestrian_block_additively():
    st = O.extract_tag_stats(_osm(ROADS + FOOT, CROSS))
    assert st["drivable_ways"] == 5                    # everything it reported before is unchanged
    assert "pedestrian" in st
    assert st["pedestrian"]["crossing_nodes"] == 3


def test_osm_footways_are_added_as_extra_legal_surface(gridwalk):
    """A separately-mapped footway is real walkable surface the derived layer cannot invent, so it
    is ADDED. `sidewalk=separate` on 45.5% of the tagged Ingolstadt roads is exactly this case."""
    net, _sw = gridwalk
    plain = V.build_sidewalks(net)
    # mid-block and 30 m off the nearest road: nothing the derived layer can reach (its sidewalks
    # sit at |y| = 5.0 and its crossings at the junctions), so this is genuinely NEW surface.
    extra = [[[40.0, 30.0], [80.0, 30.0]], [[40.0, 90.0], [80.0, 90.0]]]
    with_fw = V.build_sidewalks(net, osm_footways=extra)
    st = with_fw.stats()
    assert st["osm_footway_links"] == 2
    assert st["osm_footway_m"] == pytest.approx(80.0, abs=0.5)
    assert st["ped_links"] == plain.stats()["ped_links"] + 2
    assert not plain.is_legal(60.0, 30.0), plain.legal_distance(60.0, 30.0)
    assert with_fw.is_legal(60.0, 30.0)


def test_mapped_footways_join_each_other_and_the_kerb_instead_of_forming_islands(gridwalk):
    """Footway endpoints are keyed by COORDINATE, so two ways meeting at a shared OSM node share a
    pedestrian node; and an endpoint within `footway_snap_m` of the derived kerb is joined to it.
    Without both, the 763 Ingolstadt footways arrive as 763 disconnected islands and 45 km of real
    mapped walking surface is decoration."""
    net, _sw = gridwalk
    # an L of two ways meeting at (60, 30), whose free end (60, 8) is 3 m from the y=+5 sidewalk
    extra = [[[20.0, 30.0], [60.0, 30.0]], [[60.0, 30.0], [60.0, 8.0]]]
    sw = V.build_sidewalks(net, osm_footways=extra)
    st = sw.stats()
    assert st["osm_footway_links"] == 2
    assert st["osm_footway_snaps"] >= 1, st            # the (60, 8) end reached the kerb
    # shared endpoint -> ONE node, so the two ways are mutually reachable
    shared = sw.node_xy[("f", 60.0, 30.0)]
    assert sum(1 for _nb, _lk in sw.adj[shared]) >= 2
    # and the footway is now part of the main walkable component, not an island
    _n, _l, _c, comp = sw._largest_component()
    assert shared in comp


def test_a_mapped_footway_that_runs_on_tarmac_is_filed_as_a_crossing(gridwalk):
    """`footway=crossing` ways genuinely cross a carriageway. Filing them as SIDEWALK would break
    the one invariant that matters -- no sidewalk on a traffic lane -- so they become crossings."""
    net, _sw = gridwalk
    across = [[[60.0, -12.0], [60.0, 12.0]]]          # straight over the y = 0 road
    sw = V.build_sidewalks(net, osm_footways=across)
    st = sw.stats()
    assert st["osm_footway_links"] == 1 and st["osm_footways_on_tarmac"] == 1
    carr = _carriageway_index(net)
    for lk in sw.links:
        if lk["kind"] == V.SIDEWALK:
            _ss, sp = V._sample_polyline(lk["pts"], 0.5)
            assert max(carr.penetration(x, y) for x, y in sp) <= 0.0


def test_osm_crossing_nodes_drive_the_signalisation(gridwalk):
    """A crossing at a junction annotated `crossing=traffic_signals` inherits a pedestrian phase;
    everything else does not -- instead of the network-wide "signalise everything" default."""
    net, _sw = gridwalk
    coords = [tuple(p) for p in net.geometry()["nodes"]]
    cross = [[coords[0][0], coords[0][1], "traffic_signals"],
             [coords[1][0], coords[1][1], "unmarked"]]
    sw = V.build_sidewalks(net, osm_crossings=cross)
    st = sw.stats()
    assert st["osm_crossing_nodes_matched"] == 2
    assert st["osm_crossing_junctions"] == 2
    assert 0 < st["signalised_crossings"] < st["crossings"]


# --------------------------------------------------------------------------- #
# 6. the real map: the invariant must survive InTAS, not just a lattice
# --------------------------------------------------------------------------- #
def test_intas_sidewalks_clear_every_carriageway_and_beat_the_random_walk():
    """The load-bearing case. A grid has no acute-angle junctions, no parallel service roads and no
    multi-lane arterials; InTAS has all three, and it is where a footway offset correctly from its
    own road lands on somebody else's."""
    import os
    from scms_sim_ref.mock_pipeline.run import _parse_custom_network
    from scms_sim_ref.mock_pipeline.sumo_trace import engine_network
    path = os.path.join("scms-sim", "scenarios", "gen_intas_urban_low", "sumo",
                        "ingolstadt.net.xml")
    if not os.path.exists(path):
        pytest.skip("InTAS net not present")
    nodes, edges, info, _tf = engine_network(path, directed=True)
    doc = O.network_document(nodes, edges, info)
    net = CustomNetwork(*_parse_custom_network(doc, directed=True))
    net.enable_directed_lanes()
    net.set_road_surface(**info["road_surface"])
    sig_xy = [tuple(doc["nodes"][i]) for i in info.get("signal_nodes", ())]
    sw = V.build_sidewalks(net, signal_nodes=sig_xy)
    st = sw.stats()
    assert st["max_residual_penetration_m"] <= 0.0, st
    assert st["signal_nodes_unmatched"] == 0, st
    assert st["signalised_share"] < 0.2, st        # real signals, not "every junction"
    assert st["crossing_max_m"] < 60.0, st         # a crossing is a road width, not a street
    assert st["corner_max_m"] < 60.0, st
    assert st["largest_component_node_share"] > 0.85, st
    kw = dict(seed=7, n=40, life_s=90.0, dt=1.0, speed=1.8)
    mb = V.measure_positions(V.legacy_offroad_positions(net, **kw), sw, net)
    ma = V.measure_positions(
        V.sidewalk_positions(sw, signal_fn=V.engine_signal_fn(net, 12.0), **kw), sw, net)
    assert mb["on_legal_frac"] < 0.20 and ma["on_legal_frac"] == 1.0, (mb, ma)
    assert mb["stray_max_m"] > 50.0 and ma["stray_max_m"] == 0.0, (mb, ma)
    assert ma["road_dist_p95"] < 0.3 * mb["road_dist_p95"], (mb, ma)
