"""Directed edges, one-way streets, per-edge lane counts, edge shape polylines and weighted routing.

The defect these close (docs/realism/investigation/road-network.json, roadmap G7): the pure-Python
road network was a set of UNDIRECTED straight centrelines with one global lane count, so opposing
traffic shared the identical geometry and physically passed through each other -- counted by the
realism harness as `traffic.overlap_events`, a HARD gate failure the SUMO path scores 0 on.

Everything here is OPT-IN. The last section is the guard that says so: with nothing enabled, every
network produces the exact objects it produced before and consumes the exact same RNG.
"""
import json
import math
import random

import pytest

from scms_sim_ref.mock_pipeline.roads import (CustomNetwork, GridNetwork, RingNetwork, Trip,
                                              _offset_polyline, _pt_seg_dist, edges_from_directed,
                                              largest_strong_component, parse_edge_spec,
                                              spider_graph)

# a corridor of three collinear nodes: the simplest map with a well-defined "opposite direction"
LINE_NODES = [[0, 0], [500, 0], [1000, 0]]
LINE_EDGES = [[0, 1], [1, 2]]
# an H-shaped town (mirrors tests/test_custom_networks.py so the two files describe one map)
H_NODES = [[0, 0], [0, 300], [0, 600], [400, 300], [800, 0], [800, 300], [800, 600], [400, 0]]
H_EDGES = [[0, 1], [1, 2], [1, 3], [3, 5], [4, 5], [5, 6], [0, 7], [7, 4]]


# --------------------------------------------------------------------------- #
# 1. Edge schema: the old forms still mean what they meant
# --------------------------------------------------------------------------- #
def test_legacy_edge_forms_are_unchanged():
    """[a,b] and [a,b,speed] parse to a plain two-way edge with no lane/oneway/shape state."""
    assert parse_edge_spec([0, 1], 2) == {"a": 0, "b": 1, "speed": None, "lanes_f": None,
                                          "lanes_b": None, "oneway": 0, "shape": None}
    assert parse_edge_spec([0, 1, 13.9], 2)["speed"] == 13.9
    net = CustomNetwork(LINE_NODES, [[0, 1, 4.0], [1, 2]])
    assert net.edge_speed == {(0, 1): 4.0}          # exact historical attribute + key shape
    assert net.edge_oneway == {} and net.edge_lanes == {} and net.edge_shape == {}
    assert net.directed is False and net._rich is False
    assert net.geometry()["edges"] == [[0, 1, 4.0], [1, 2]]      # list form preserved for the GUI


def test_legacy_error_messages_survive():
    with pytest.raises(ValueError, match="self-loop"):
        CustomNetwork(LINE_NODES, [[0, 0]])
    with pytest.raises(ValueError, match="missing node"):
        CustomNetwork(LINE_NODES, [[0, 9]])
    with pytest.raises(ValueError, match="out of range"):
        CustomNetwork(LINE_NODES, [[0, 1, 500.0]])
    with pytest.raises(ValueError, match="shorter than 1 m"):
        CustomNetwork([[0, 0], [0.1, 0]], [[0, 1]])
    with pytest.raises(ValueError, match="connected"):
        CustomNetwork([[0, 0], [100, 0], [500, 500], [600, 500]], [[0, 1], [2, 3]])


# --------------------------------------------------------------------------- #
# 2. Edge schema: the new forms
# --------------------------------------------------------------------------- #
def test_positional_edge_form_extends_to_lanes_and_oneway():
    s = parse_edge_spec([0, 1, 13.9, 4, True], 2)
    assert (s["speed"], s["lanes_f"], s["lanes_b"], s["oneway"]) == (13.9, 4, None, 1)
    s = parse_edge_spec([0, 1, None, 4], 2)                 # two-way: OSM `lanes` is the TOTAL
    assert (s["lanes_f"], s["lanes_b"], s["oneway"]) == (2, 2, 0)
    s = parse_edge_spec([0, 1, None, 3], 2)                 # odd total splits 1/2, never 0
    assert (s["lanes_f"], s["lanes_b"]) == (1, 2)


def test_object_edge_form_and_per_direction_lanes():
    s = parse_edge_spec({"a": 0, "b": 1, "speed": 20.0, "lanes_forward": 3,
                         "lanes_backward": 1, "shape": [[250, 40]]}, 2)
    assert s["lanes_f"] == 3 and s["lanes_b"] == 1 and s["shape"] == ((250.0, 40.0),)
    net = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "lanes_forward": 3, "lanes_backward": 1},
                                     [1, 2]])
    assert net.edge_lanes == {(0, 1): (3, 1)}
    assert net.lanes_for(0, 1) == 3 and net.lanes_for(1, 0) == 1
    assert net.lanes_for(1, 2) == 1                          # unspecified -> the per-direction default


def test_oneway_reverse_convention():
    """OSM `oneway=-1` means the way is drivable only AGAINST its node order."""
    assert parse_edge_spec([0, 1, None, None, -1], 2)["oneway"] == -1
    assert parse_edge_spec({"a": 0, "b": 1, "oneway": "-1"}, 2)["oneway"] == -1
    with pytest.raises(ValueError, match="oneway must be"):
        parse_edge_spec({"a": 0, "b": 1, "oneway": "sometimes"}, 2)


def test_lane_count_bounds_are_enforced():
    with pytest.raises(ValueError, match="lanes=0 out of range"):
        parse_edge_spec({"a": 0, "b": 1, "lanes": 0}, 2)
    with pytest.raises(ValueError, match="out of range"):
        parse_edge_spec({"a": 0, "b": 1, "lanes": 99}, 2)


# --------------------------------------------------------------------------- #
# 3. One-way streets actually constrain travel
# --------------------------------------------------------------------------- #
def _one_way_square():
    """A 400 m square driven anticlockwise -- legal in one rotation only."""
    nodes = [[0, 0], [400, 0], [400, 400], [0, 400]]
    edges = [{"a": 0, "b": 1, "oneway": True}, {"a": 1, "b": 2, "oneway": True},
             {"a": 2, "b": 3, "oneway": True}, {"a": 3, "b": 0, "oneway": True}]
    return CustomNetwork(nodes, edges)


def test_oneway_edges_are_directed_in_the_graph_and_the_router():
    net = _one_way_square()
    assert net.directed is True and len(net.edge_oneway) == 4
    assert net.allows(0, 1) and not net.allows(1, 0)
    assert net.is_oneway(0, 1) and net.is_oneway(1, 0)      # the ROAD is one-way, either way you ask
    assert net._route(0, 3) == [0, 1, 2, 3]                 # the long way round; [0,3] is illegal
    assert net._route(3, 0) == [3, 0]
    # the undirected view still sees both neighbours (signal phase / dead-end detection use it)
    assert sorted(m for m, _w in net.uadj[0]) == [1, 3]
    assert sorted(m for m, _w in net.adj[0]) == [1]


def test_oneway_trips_only_ever_run_the_legal_way():
    net = _one_way_square()
    rng = random.Random(4)
    checked = 0
    for _ in range(60):
        trip = net.random_trip(rng, 12.0, 0.0)
        if trip.nodes is None:
            continue        # origin == destination: Trip pads a stub, there is no route to check
        checked += 1
        wp = trip.wp
        for (ax, ay), (bx, by) in zip(wp, wp[1:]):
            # anticlockwise square: every legal move is +x on y=0, +y on x=400, -x on y=400, -y on x=0
            legal = ((ay == 0 and bx > ax) or (ax == 400 and by > ay)
                     or (ay == 400 and bx < ax) or (ax == 0 and by < ay))
            assert legal, f"illegal move {(ax, ay)} -> {(bx, by)}"
    assert checked > 30, checked


def test_a_oneway_map_must_be_strongly_connected():
    """A node you can enter but never leave strands whatever routes there -- reject it at build."""
    nodes = [[0, 0], [400, 0], [400, 400], [0, 400]]
    edges = [{"a": 0, "b": 1, "oneway": True}, {"a": 1, "b": 2, "oneway": True},
             {"a": 2, "b": 3, "oneway": True}, {"a": 0, "b": 3, "oneway": True}]  # 3 is a sink
    with pytest.raises(ValueError, match="STRONGLY connected"):
        CustomNetwork(nodes, edges)


def test_two_oneway_records_for_one_pair_merge_into_a_two_way_road():
    """This is how an OSM import describes a two-way street: one record per direction."""
    net = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "oneway": True, "lanes": 3},
                                     {"a": 1, "b": 0, "oneway": True, "lanes": 1},
                                     [1, 2]])
    assert net.directed is False                       # both directions legal -> one two-way road
    assert net.edges == [(0, 1), (1, 2)]                # ...and ONE physical edge, not two
    assert net.lanes_for(0, 1) == 3 and net.lanes_for(1, 0) == 1


def test_edges_from_directed_bridges_the_osm_importer():
    # a triangle: 0<->1 two-way (asymmetric lanes, two records), then a one-way loop 1->2->0
    tri = [[0, 0], [500, 0], [250, 400]]
    recs = [{"a": 0, "b": 1, "speed_mps": 13.9, "lanes": 2, "class": "primary"},
            {"a": 1, "b": 0, "speed_mps": 13.9, "lanes": 1, "class": "primary"},
            {"a": 1, "b": 2, "speed_mps": 8.3, "lanes": 1},        # one-way: only one record
            {"a": 2, "b": 0, "speed_mps": 8.3, "lanes": 1}]
    net = CustomNetwork(tri, edges_from_directed(recs))
    assert net.lanes_for(0, 1) == 2 and net.lanes_for(1, 0) == 1
    assert net.edge_speed == {(0, 1): 13.9, (1, 2): 8.3, (0, 2): 8.3}
    assert net.is_oneway(1, 2) and not net.is_oneway(0, 1)
    assert net.allows(1, 2) and not net.allows(2, 1)


def _osm_xml(ways):
    """Minimal OSM XML. ways = [({tag: value}, [(lat, lon), ...]), ...]."""
    nodes: dict = {}
    out = ['<?xml version="1.0"?><osm version="0.6">']
    refs = []
    for _tags, pts in ways:
        r = []
        for p in pts:
            if p not in nodes:
                nodes[p] = str(len(nodes) + 1)
            r.append(nodes[p])
        refs.append(r)
    for (lat, lon), rid in nodes.items():
        out.append(f'<node id="{rid}" lat="{lat}" lon="{lon}"/>')
    for k, (tags, _pts) in enumerate(ways):
        nds = "".join(f'<nd ref="{r}"/>' for r in refs[k])
        tg = "".join(f'<tag k="{a}" v="{b}"/>' for a, b in tags.items())
        out.append(f'<way id="{2000 + k}">{nds}{tg}</way>')
    out.append("</osm>")
    return "".join(out)


def test_osm_oneway_tags_reach_the_engine_end_to_end():
    """osm.py already keeps `oneway`/`lanes` (attrs=True); this is the piece that lets the ENGINE
    act on them -- directed_edges -> edge specs -> a CustomNetwork whose router refuses the illegal
    direction and whose carriageways are separated."""
    from scms_sim_ref.mock_pipeline.osm import osm_to_network
    ways = [                                      # a one-way loop plus a two-way spur
        ({"highway": "primary", "oneway": "yes", "lanes": "2"},
         [(48.100, 11.500), (48.100, 11.503)]),
        ({"highway": "primary", "oneway": "yes", "lanes": "2"},
         [(48.100, 11.503), (48.102, 11.503)]),
        ({"highway": "primary", "oneway": "yes", "lanes": "2"},
         [(48.102, 11.503), (48.102, 11.500)]),
        ({"highway": "primary", "oneway": "yes", "lanes": "2"},
         [(48.102, 11.500), (48.100, 11.500)]),
        ({"highway": "residential", "lanes": "2"},
         [(48.100, 11.503), (48.100, 11.506)]),
    ]
    nodes, _edges, info = osm_to_network(_osm_xml(ways), attrs=True)
    directed = info["directed_edges"]
    assert directed, "attrs=True must expose directed edges"
    spec = edges_from_directed(directed)
    kept_n, kept_e, _scc = largest_strong_component(nodes, spec)
    net = CustomNetwork(kept_n, kept_e).enable_directed_lanes(lane_width_m=3.5)
    assert net.directed, "a one-way loop must survive as a DIRECTED graph"
    assert net.stats()["oneway_share"] > 0.0
    rng = random.Random(3)
    for _ in range(30):                            # every driven segment is a legal move
        t = net.random_trip(rng, 12.0, 0.0)
        if t.nodes is None:
            continue
        assert t.lanes is not None and min(t.lanes) >= 1


def test_largest_strong_component_trims_a_clipped_oneway_import():
    """The failure mode a real bbox-clipped one-way import hits: node 4 can be entered from the loop
    but its only exit is a one-way pointing back in, so nothing routed there can ever leave."""
    nodes = [[0, 0], [400, 0], [400, 400], [0, 400], [800, 200]]
    edges = [{"a": 0, "b": 1, "oneway": True}, {"a": 1, "b": 2, "oneway": True},
             {"a": 2, "b": 3, "oneway": True}, {"a": 3, "b": 0, "oneway": True},
             {"a": 1, "b": 4, "oneway": True}]            # a spur with no way back
    with pytest.raises(ValueError, match="STRONGLY connected"):
        CustomNetwork(nodes, edges)
    kept_n, kept_e, info = largest_strong_component(nodes, edges)
    assert info["kept_nodes"] == 4 and info["dropped_nodes"] == 1 and info["dropped_edges"] == 1
    net = CustomNetwork(kept_n, kept_e)                    # ...and now it builds
    assert net.directed and len(net.nodes) == 4


def test_largest_strong_component_is_a_no_op_on_a_two_way_map():
    kept_n, kept_e, info = largest_strong_component(H_NODES, H_EDGES)
    assert kept_n == H_NODES and kept_e == H_EDGES
    assert info["dropped_nodes"] == 0 and info["n_components"] == 1


def test_largest_strong_component_remaps_indices_and_keeps_edge_attributes():
    nodes = [[800, 200], [0, 0], [400, 0], [400, 400], [0, 400]]      # node 0 is the doomed spur
    edges = [{"a": 1, "b": 2, "oneway": True, "lanes": 3}, {"a": 2, "b": 3, "oneway": True},
             {"a": 3, "b": 4, "oneway": True}, {"a": 4, "b": 1, "oneway": True},
             {"a": 2, "b": 0, "oneway": True}]
    kept_n, kept_e, _info = largest_strong_component(nodes, edges)
    assert kept_n == nodes[1:]                                   # the spur node is gone
    assert kept_e[0] == {"a": 0, "b": 1, "oneway": True, "lanes": 3}   # ...and indices shifted by 1
    assert CustomNetwork(kept_n, kept_e).lanes_for(0, 1) == 3


# --------------------------------------------------------------------------- #
# 4. Directed lane frames: opposing directions occupy DIFFERENT geometry
# --------------------------------------------------------------------------- #
def test_offset_polyline_is_identity_for_zero_offsets():
    pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]
    assert _offset_polyline(pts, [0.0, 0.0]) == pts


def test_offset_polyline_mitres_a_corner_and_keeps_the_vertex_count():
    pts = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]
    out = _offset_polyline(pts, [-3.5, -3.5])          # 3.5 m to the RIGHT of travel throughout
    assert len(out) == len(pts)
    assert out[0] == pytest.approx((0.0, -3.5))
    assert out[1] == pytest.approx((103.5, -3.5))      # mitre: the corner moves out on both axes
    assert out[2] == pytest.approx((103.5, 100.0))


def test_opposing_grid_carriageways_never_share_a_point():
    """The core fix. Two vehicles driving the same road in opposite directions are now on polylines
    that are (lanes_fwd + lanes_bwd) * width / 2 apart -- so no position they can hold overlaps."""
    for lpd, width in ((1, 3.5), (2, 3.5), (3, 3.0)):
        net = GridNetwork(5, 5, 100.0).enable_directed_lanes(lane_width_m=width, lanes_per_dir=lpd)
        route = [(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)]
        east, _ = net._lane_frame(route, [lpd] * 2, [False] * 2)
        west, _ = net._lane_frame(route[::-1], [lpd] * 2, [False] * 2)
        sep = min(_pt_seg_dist(px, py, *a, *b)
                  for px, py in east for a, b in zip(west, west[1:]))
        assert sep == pytest.approx(lpd * width), (lpd, width, sep)
        assert sep > 1.0                          # realism_bench.OVERLAP_DIST_M


def test_right_hand_traffic_puts_each_direction_on_its_own_side():
    net = GridNetwork(3, 3, 100.0).enable_directed_lanes(lane_width_m=3.5, lanes_per_dir=1)
    east, _ = net._lane_frame([(0.0, 0.0), (100.0, 0.0)], [1], [False])
    assert east[0][1] == pytest.approx(-1.75)     # eastbound sits SOUTH of the centreline
    left = GridNetwork(3, 3, 100.0).enable_directed_lanes(drive_side="left", lanes_per_dir=1)
    east_l, _ = left._lane_frame([(0.0, 0.0), (100.0, 0.0)], [1], [False])
    assert east_l[0][1] == pytest.approx(+1.75)   # ...and NORTH where they drive on the left


def test_a_oneway_road_keeps_the_centreline():
    """With no opposing stream there is nothing to make room for, so a one-way edge is unmoved --
    which is also why enabling the feature on a fully one-way map changes no geometry at all."""
    net = _one_way_square().enable_directed_lanes(lanes_per_dir=2)
    wp, _marks, _caps, lanes, ow = net.route_geometry([0, 1, 2])
    driven, _ = net._lane_frame(wp, lanes, ow)
    assert ow == [True, True] and driven == wp


def test_per_edge_lane_counts_size_the_offset():
    """A 1-lane street and a 4-lane arterial put their carriageway centres at different offsets."""
    net = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "lanes": 2}, {"a": 1, "b": 2, "lanes": 8}])
    net.enable_directed_lanes(lane_width_m=3.5)
    assert net.lanes_for(0, 1) == 1 and net.lanes_for(1, 2) == 4
    assert net.carriageway_offset(1, False) == pytest.approx(-1.75)
    assert net.carriageway_offset(4, False) == pytest.approx(-7.0)
    wp, _m, _c, lanes, ow = net.route_geometry([0, 1, 2])
    driven, _ = net._lane_frame(wp, lanes, ow)
    assert driven[0][1] == pytest.approx(-1.75) and driven[-1][1] == pytest.approx(-7.0)


def test_grid_arterials_can_be_wider_than_local_roads():
    net = GridNetwork(9, 9, 100.0, arterial_every=4, arterial_speed=20.0, local_speed=8.0)
    net.enable_directed_lanes(lanes_per_dir=1, arterial_lanes=3)
    assert net.edge_lanes((0, 0), (1, 0)) == 3         # row j=0 is an arterial
    assert net.edge_lanes((1, 1), (2, 1)) == 1         # row j=1 is local
    with pytest.raises(ValueError, match="arterial_every"):
        GridNetwork(5, 5, 100.0).enable_directed_lanes(arterial_lanes=2)


def test_enable_directed_lanes_validates_its_arguments():
    for kw in ({"lane_width_m": 0.0}, {"lane_width_m": float("nan")},
               {"drive_side": "middle"}, {"lanes_per_dir": 0}, {"lanes_per_dir": 99}):
        with pytest.raises(ValueError):
            GridNetwork(4, 4, 100.0).enable_directed_lanes(**kw)


# --------------------------------------------------------------------------- #
# 5. Trip.nodes: junction identity survives the offset (signals + gap acceptance depend on it)
# --------------------------------------------------------------------------- #
def test_next_node_returns_the_shared_junction_from_every_approach():
    """run.py groups gap-acceptance conflicts by the rounded coordinate `next_node` returns, and
    looks the signal phase up with it. If each approach reported its own offset vertex, opposing and
    crossing streams would stop seeing each other at the very junctions they conflict at."""
    net = GridNetwork(5, 5, 100.0).enable_directed_lanes(lanes_per_dir=2)
    east = Trip(*_frame_trip(net, [(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)]))
    north = Trip(*_frame_trip(net, [(100.0, -100.0), (100.0, 0.0), (100.0, 100.0)]))
    assert east.next_node(1.0)[0] == (100.0, 0.0)
    assert north.next_node(1.0)[0] == (100.0, 0.0)
    # ...and the DRIVEN geometry at that moment is genuinely off the junction
    assert east.wp[1] != (100.0, 0.0)


def _frame_trip(net, route):
    lanes = [net._lanes_per_dir] * (len(route) - 1)
    driven, nodes = net._lane_frame(route, lanes, [False] * len(lanes))
    return driven, 12.0, 0.0, None, nodes, lanes


def test_trip_nodes_defaults_to_the_old_behaviour():
    t = Trip([(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)], 10.0, 0.0)
    assert t.nodes is None and t.lanes is None
    assert t.next_node(1.0) == ((100.0, 0.0), 99.0)
    assert t.lanes_at(50.0) is None


def test_trip_nodes_is_ignored_when_it_does_not_align():
    t = Trip([(0.0, 0.0), (100.0, 0.0)], 10.0, 0.0, nodes=[(0.0, 0.0)])
    assert t.nodes is None                      # a mismatched list would mislabel junctions


def test_trip_lanes_at_tracks_the_segment():
    t = Trip([(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)], 10.0, 0.0, lanes=[1, 3])
    assert t.lanes_at(10.0) == 1 and t.lanes_at(150.0) == 3
    assert t.lanes_at(1e6) == 3


# --------------------------------------------------------------------------- #
# 6. Edge shape polylines: curve geometry without spending graph nodes
# --------------------------------------------------------------------------- #
def test_shape_carries_curve_geometry_off_the_node_budget():
    bend = [[250.0, 60.0]]
    net = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "shape": bend}, [1, 2]])
    assert len(net.nodes) == 3                                     # no extra graph nodes
    straight = math.dist((0, 0), (500, 0))
    assert net.edge_len[(0, 1)] > straight                         # length follows the polyline
    assert net.edge_len[(0, 1)] == pytest.approx(math.dist((0, 0), (250, 60))
                                                 + math.dist((250, 60), (500, 0)))
    assert net.dist_to_road(250.0, 60.0) == pytest.approx(0.0)     # the curve IS road
    assert net.dist_to_road(250.0, 0.0) > 5.0                      # the chord is not
    assert net.edge_points(0, 1) == [(0.0, 0.0), (250.0, 60.0), (500.0, 0.0)]
    assert net.edge_points(1, 0) == [(500.0, 0.0), (250.0, 60.0), (0.0, 0.0)]


def test_shape_vertices_are_driven_but_are_not_junctions():
    net = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "shape": [[250.0, 60.0]]}, [1, 2]])
    wp, marks, caps, lanes, _ow = net.route_geometry([0, 1, 2])
    assert wp == [(0.0, 0.0), (250.0, 60.0), (500.0, 0.0), (1000.0, 0.0)]
    assert marks == [(0.0, 0.0), None, (500.0, 0.0), (1000.0, 0.0)]
    assert len(caps) == len(lanes) == len(wp) - 1
    t = Trip(wp, 10.0, 0.0, nodes=marks)
    assert t.next_node(1.0)[0] == (500.0, 0.0)          # the bend is skipped: it is not an junction
    d, ang = t.next_turn(1.0)                           # ...but it IS a bend, so curve-speed sees it
    assert ang > 10.0 and d < 300.0


def test_conflicting_shapes_for_one_road_are_rejected():
    with pytest.raises(ValueError, match="two different shapes"):
        CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "shape": [[250.0, 60.0]]},
                                   {"a": 0, "b": 1, "shape": [[250.0, -60.0]]}, [1, 2]])


def test_shape_must_be_finite_points():
    with pytest.raises(ValueError, match="shape\\[0\\] must be"):
        CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "shape": [["x", 1]]}, [1, 2]])
    with pytest.raises(ValueError, match="non-finite"):
        CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "shape": [[float("inf"), 1]]}, [1, 2]])


# --------------------------------------------------------------------------- #
# 7. Routing: an opt-in metric-weighted path
# --------------------------------------------------------------------------- #
def test_grid_default_router_is_still_hop_count_bfs():
    net = GridNetwork(8, 8, 100.0)
    assert net.route_metric == "hops"
    for o, d in (((0, 0), (7, 7)), ((3, 1), (0, 6)), ((5, 5), (5, 0))):
        assert net._path(o, d) == net._bfs(o, d)


def test_grid_time_metric_routes_onto_the_arterials():
    """The whole point of a weighted router: a longer run on a 20 m/s arterial beats a shorter crawl
    down an 8 m/s local street. Hop count cannot express that -- every grid edge is one hop."""
    net = GridNetwork(9, 9, 100.0, arterial_every=4, arterial_speed=20.0, local_speed=8.0)
    direct = net._path((0, 1), (8, 1))
    assert len(direct) == 9 and all(n[1] == 1 for n in direct)      # BFS: straight down row 1
    net.set_route_metric("time", free_speed_mps=13.9)
    fast = net._path((0, 1), (8, 1))
    assert len(fast) > len(direct)                                  # more hops...
    assert sum(1 for n in fast if n[1] == 0) >= 8                   # ...spent on the j=0 arterial

    def travel_time(path):
        return sum(net.block / (net._edge_cap(a, b) or 13.9) for a, b in zip(path, path[1:]))
    assert travel_time(fast) < travel_time(direct)


def test_grid_length_metric_is_length_optimal():
    net = GridNetwork(7, 7, 100.0).set_route_metric("length")
    path = net._path((0, 0), (6, 3))
    assert len(path) == 10                                          # Manhattan distance + 1
    assert path[0] == (0, 0) and path[-1] == (6, 3)


def test_custom_time_metric_prefers_the_faster_road():
    """Two routes between the same pair: a short 30-zone and a long fast road."""
    nodes = [[0, 0], [1000, 0], [500, 900]]
    edges = [[0, 1, 4.0], [0, 2, 30.0], [1, 2, 30.0]]               # direct road is a 4 m/s zone
    net = CustomNetwork(nodes, edges)
    assert net._route(0, 1) == [0, 1]                               # length-optimal: straight
    net.set_route_metric("time")
    assert net._route(0, 1) == [0, 2, 1]                            # time-optimal: round the fast way
    net.set_route_metric("hops")
    assert net._route(0, 1) == [0, 1]


def test_route_metric_validation():
    for net in (GridNetwork(4, 4, 100.0), RingNetwork(8, 100.0), CustomNetwork(H_NODES, H_EDGES)):
        with pytest.raises(ValueError, match="route_metric must be"):
            net.set_route_metric("cheapest")
        with pytest.raises(ValueError, match="route_free_speed_mps"):
            net.set_route_metric("time", free_speed_mps=0.0)


def test_weighted_routing_honours_one_ways_and_closures():
    net = _one_way_square().set_route_metric("time")
    assert net._route(0, 3) == [0, 1, 2, 3]
    net = CustomNetwork(H_NODES, H_EDGES).set_route_metric("time")
    applied = net.set_closures([[1, 3]])                            # close the central bridge
    assert applied == [[1, 3]]
    assert 3 not in net._route(0, 5)


def test_a_closure_that_would_strand_a_oneway_node_is_skipped():
    net = _one_way_square()
    assert net.set_closures([[0, 1]]) == []                         # would break strong connectivity


# --------------------------------------------------------------------------- #
# 8. Ring gyratory
# --------------------------------------------------------------------------- #
def test_ring_gyratory_runs_one_way_only():
    net = RingNetwork(12, 100.0).enable_directed_lanes(oneway=True)
    assert net._arc(2, 0) == [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 0]    # never the short way back
    plain = RingNetwork(12, 100.0)
    assert plain._arc(2, 0) == [2, 1, 0]
    rng = random.Random(9)
    for _ in range(40):
        t = net.random_trip(rng, 12.0, 0.0)
        assert t.nodes is not None and t.lanes is not None
        assert t.wp == t.nodes                                      # one-way -> no lateral offset


def test_ring_two_way_separates_the_carriageways():
    net = RingNetwork(16, 100.0).enable_directed_lanes(lanes_per_dir=1, lane_width_m=3.5)
    t = net.random_trip(random.Random(2), 12.0, 0.0)
    R = net.R
    for (x, y) in t.wp:                       # right-hand traffic on a ring runs INSIDE the centreline
        assert math.hypot(x - net.cx, y - net.cy) < R


# --------------------------------------------------------------------------- #
# 9. Size caps and the cost of the map at them
# --------------------------------------------------------------------------- #
def test_caps_were_raised_and_are_still_enforced():
    assert CustomNetwork.MAX_NODES == 4000 and CustomNetwork.MAX_EDGES == 12000
    with pytest.raises(ValueError, match="too large.*nodes"):
        CustomNetwork([[float(i), 0.0] for i in range(0, CustomNetwork.MAX_NODES + 1)], [[0, 1]])
    nodes = [[i * 100.0, 0.0] for i in range(3)]
    with pytest.raises(ValueError, match="too large.*edges"):
        CustomNetwork(nodes, [[0, 1]] * (CustomNetwork.MAX_EDGES + 1))


def _lattice(k, block=60.0):
    nodes = [[i * block, j * block] for i in range(k) for j in range(k)]
    edges = []
    for i in range(k):
        for j in range(k):
            if i + 1 < k:
                edges.append([i * k + j, (i + 1) * k + j])
            if j + 1 < k:
                edges.append([i * k + j, i * k + j + 1])
    return nodes, edges


def test_a_map_at_the_new_cap_builds_and_routes():
    nodes, edges = _lattice(63)                       # 3969 nodes / 7812 edges
    net = CustomNetwork(nodes, edges)
    assert len(net.nodes) == 3969 and len(net.edges) == 7812
    path = net._route(0, len(nodes) - 1)
    assert path[0] == 0 and path[-1] == len(nodes) - 1
    t = net.random_trip(random.Random(1), 12.0, 0.0)
    assert t.length > 0.0


def test_dist_to_road_is_exact_and_size_independent():
    """The index is a search accelerator only: it must agree bit-for-bit with a full scan, including
    for a network holding one very long edge (which used to blow the cell size up to kilometres)."""
    nodes, edges = _lattice(20)
    edges = list(edges) + [[0, len(nodes) - 1]]       # one 1.6 km diagonal across the whole lattice
    net = CustomNetwork(nodes, edges)
    rng = random.Random(11)
    x0, y0, x1, y1 = net.bbox
    for _ in range(300):
        x = rng.uniform(x0 - 900, x1 + 900)           # inside AND far outside the modelled area
        y = rng.uniform(y0 - 900, y1 + 900)
        ref = min(_pt_seg_dist(x, y, *s) for s in net._segs)
        assert net.dist_to_road(x, y) == ref, (x, y)


# --------------------------------------------------------------------------- #
# 10. OPT-IN guard: nothing above changes anything until it is switched on
# --------------------------------------------------------------------------- #
def test_networks_are_inert_until_enabled():
    for net in (GridNetwork(6, 6, 100.0), RingNetwork(10, 100.0),
                CustomNetwork(H_NODES, H_EDGES), CustomNetwork(*spider_graph(6, 3, 120.0))):
        assert net.directed_lanes is False
        t = net.random_trip(random.Random(5), 12.0, 0.0)
        assert t.nodes is None and t.lanes is None


def test_enabling_directed_lanes_draws_no_extra_rng():
    """A new feature that consumed RNG would move every downstream draw and break the goldens."""
    def draws(net):
        rng = random.Random(1234)
        for _ in range(40):
            net.random_trip(rng, 12.0, 0.0)
        return rng.random()
    assert draws(GridNetwork(6, 6, 100.0)) == draws(
        GridNetwork(6, 6, 100.0).enable_directed_lanes(lanes_per_dir=2))
    assert draws(RingNetwork(10, 100.0)) == draws(
        RingNetwork(10, 100.0).enable_directed_lanes())
    assert draws(CustomNetwork(H_NODES, H_EDGES)) == draws(
        CustomNetwork(H_NODES, H_EDGES).enable_directed_lanes())
    assert draws(GridNetwork(6, 6, 100.0)) == draws(
        GridNetwork(6, 6, 100.0).set_route_metric("length"))


def test_default_trips_are_geometrically_unchanged():
    """The waypoints a default network hands out are still exactly the graph node coordinates."""
    grid = GridNetwork(6, 6, 100.0)
    t = grid.random_trip(random.Random(5), 12.0, 0.0)
    for x, y in t.wp:
        assert x % 100.0 == 0.0 and y % 100.0 == 0.0
    net = CustomNetwork(H_NODES, H_EDGES)
    t = net.random_trip(random.Random(5), 12.0, 0.0)
    coords = {tuple(float(v) for v in p) for p in H_NODES}
    assert all(p in coords for p in t.wp)


def test_custom_network_stats_stay_quiet_on_a_plain_map():
    plain = CustomNetwork(H_NODES, H_EDGES).stats()
    assert "oneway_edges" not in plain and "lane_specified_edges" not in plain
    assert "shaped_edges" not in plain and "directed_lanes" not in plain
    rich = CustomNetwork(LINE_NODES, [{"a": 0, "b": 1, "lanes": 4, "shape": [[250, 40]]},
                                      {"a": 1, "b": 2, "oneway": True},
                                      {"a": 2, "b": 1, "oneway": True}]).stats()
    assert rich["lane_specified_edges"] == 1 and rich["shaped_edges"] == 1
    assert rich["shape_vertices"] == 1 and "oneway_edges" not in rich


# --------------------------------------------------------------------------- #
# 11. End to end: the defect this closes, measured the way the realism harness measures it
# --------------------------------------------------------------------------- #
def _opposing_overlaps(dataset_dir):
    """Pairs of distinct vehicles under OVERLAP_DIST_M apart at one instant, split by heading.

    Same rule as realism_bench._overlap_events, plus the heading of each vehicle so the pairs can be
    attributed: >135 deg apart is head-on traffic sharing one centreline, which is the defect
    directed lane frames exist to remove. (Same-direction pairs are a car-following gap issue and are
    NOT claimed to be fixed here.)"""
    import json
    import os
    from scms_sim_ref.datagen import realism_bench as rb
    path = os.path.join(dataset_dir, "ground_truth", "gt_emissions_sample.jsonl")
    with open(path, encoding="utf-8") as fh:
        ems = [json.loads(ln) for ln in fh]
    inst: dict = {}
    for e in ems:
        inst.setdefault(round(float(e["t"]), 3), {})[str(e["true_vehicle_id"])] = (
            float(e["true_x"]), float(e["true_y"]), float(e.get("true_heading") or 0.0))
    keys = [t for t in sorted(inst) if len(inst[t]) >= 2]
    # The harness's OWN sampler, called rather than copied. This used to be a two-line copy of the
    # fixed-stride rule; that rule was withdrawn for phase-locking onto the signal cycle, and a copy
    # of a seeded rule diverges the moment either side changes. `stream="overlap"` is the same
    # stream `_overlap_events` uses, so this selects the same instants it does.
    keys = rb._subsample(keys, rb.MAX_TIME_BUCKETS, stream="overlap")
    opposing = same = 0
    for t in keys:
        pts = [inst[t][v] for v in sorted(inst[t])]
        for i in range(len(pts)):
            for j in range(i + 1, len(pts)):
                if math.hypot(pts[i][0] - pts[j][0], pts[i][1] - pts[j][1]) >= rb.OVERLAP_DIST_M:
                    continue
                dh = abs(pts[i][2] - pts[j][2]) % 360.0
                dh = dh if dh <= 180.0 else 360.0 - dh
                if dh > 135.0:
                    opposing += 1
                else:
                    same += 1
    return opposing, same


def test_directed_lane_frames_eliminate_head_on_overlaps(tmp_path):
    """The G7 gate, end to end, THROUGH THE SHIPPED CONFIG SURFACE.

    Two identical routed grid runs; the second sets `directed_lanes=True` and nothing else. Head-on
    overlaps -- distinct vehicles occupying the same point while driving opposite ways -- go to zero.

    This used to install a `class _Directed(GridNetwork)` subclass over `roads.GridNetwork`, because
    run.py was owned by a parallel workflow when the measurement was made. That made the number real
    and the PRODUCT incapable of producing it: `grep` over run.py found zero occurrences of
    `enable_directed_lanes`, and `PipelineConfig` had no field matching direct*/carriage*/oneway/
    drive_side/lanes_per. A user who read the commit message and set every knob the product exposed
    could not reproduce it. The monkeypatch is gone; if `directed_lanes` ever stops being wired into
    `run_pipeline`, this test fails instead of quietly measuring its own subclass.
    """
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

    cfg = dict(traffic_flow=True, road_network="grid", grid_w=6, grid_h=6, n_lanes=2,
               arrival_rate=3.0, duration_s=300.0, demand_profile="rush", traffic_lights=True,
               od_model="gravity", fleet="mixed", attacker_pct=0.15, faulty_pct=0.06,
               weather="clear", radio_range_m=250.0, seed=1,
               emit_sample_prob=1.0)      # the harness needs a full trace to see every instant

    base_dir = str(tmp_path / "base")
    run_pipeline(PipelineConfig(out_dir=base_dir, **cfg))
    base_opp, base_same = _opposing_overlaps(base_dir)

    dir_dir = str(tmp_path / "dir")
    run_pipeline(PipelineConfig(out_dir=dir_dir, directed_lanes=True, **cfg))
    dir_opp, dir_same = _opposing_overlaps(dir_dir)

    assert base_opp > 0, "the baseline must actually exhibit the defect for this to mean anything"
    assert dir_opp == 0, f"head-on overlaps survived: {dir_opp} (baseline {base_opp})"
    assert dir_opp + dir_same < base_opp + base_same


def test_directed_lanes_is_reachable_from_the_config_and_inert_by_default(tmp_path, capsys):
    """The activation surface itself, asserted end to end: dataclass field, `config_schema()` entry,
    CLI flag, and a real `main()` invocation that reaches `run_pipeline`.

    The defect was not that the geometry was wrong; it was that the geometry was UNREACHABLE from
    every surface the product exposes. So what is pinned here is reachability and default-inertness,
    not physics.
    """
    import dataclasses

    from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema
    from scms_sim_ref.mock_pipeline.run import main

    names = {f.name for f in dataclasses.fields(PipelineConfig)}
    assert {"directed_lanes", "drive_side", "custom_network_directed"} <= names
    d = PipelineConfig()
    assert d.directed_lanes is False and d.custom_network_directed is False
    assert d.drive_side == "right"
    sch = config_schema()
    assert sch["drive_side"]["options"] == ["right", "left"]
    assert sch["directed_lanes"]["type"] == "bool"

    # the CLI flag exists, is accepted, and lands in the run's own manifest
    rc = main(["--flow", "--road", "grid", "--grid", "4", "--duration", "20", "--arrival-rate", "1",
               "--lanes", "2", "--directed-lanes", "--drive-side", "left", "--seed", "3",
               "--out", str(tmp_path / "cli")])
    capsys.readouterr()
    assert rc == 0
    man = json.loads((tmp_path / "cli" / "manifest.json").read_text(encoding="utf-8"))
    assert man["config"]["directed_lanes"] is True and man["config"]["drive_side"] == "left"


def test_the_custom_network_loader_consumes_the_directed_layer_when_asked(tmp_path):
    """Finding: `_parse_custom_network` read only the UNDIRECTED `edges` array, so every one-way
    flag, per-direction lane count and shape polyline `netimport.py` / `osm.py` produce was silently
    discarded on the only path a user can take.

    Backward compatibility is the reason the default stays undirected, so both halves are pinned:
    off -> the pre-schema graph, byte-for-byte; on -> the directed one.
    """
    import json

    from scms_sim_ref.mock_pipeline.run import _parse_custom_network

    doc = {
        "nodes": [[0, 0], [400, 0], [400, 400], [0, 400]],
        "edges": [[0, 1], [1, 2], [2, 3], [3, 0]],                 # undirected, as before
        "directed_edges": [                                        # a one-way gyratory
            {"a": 0, "b": 1, "speed_mps": 14.0, "lanes": 2},
            {"a": 1, "b": 2, "speed_mps": 14.0, "lanes": 2},
            {"a": 2, "b": 3, "speed_mps": 14.0, "lanes": 1},
            {"a": 3, "b": 0, "speed_mps": 14.0, "lanes": 1},
        ],
    }
    blob = json.dumps(doc)
    plain = CustomNetwork(*_parse_custom_network(blob))
    assert plain.directed is False
    assert plain.stats().get("oneway_edges", 0) == 0
    assert plain.stats().get("lane_specified_edges", 0) == 0

    rich = CustomNetwork(*_parse_custom_network(blob, directed=True))
    assert rich.directed is True
    assert rich.stats()["oneway_edges"] == 4
    assert rich.stats()["lane_specified_edges"] == 4

    # an opt-in that finds nothing to opt into must SAY so, not fall back to the old graph
    with pytest.raises(ValueError, match="no \"directed_edges\" layer"):
        _parse_custom_network(json.dumps({"nodes": doc["nodes"], "edges": doc["edges"]}),
                              directed=True)


def test_a_clipped_one_way_import_is_trimmed_to_its_drivable_core():
    """A bbox clip leaves nodes you can enter and never leave; `CustomNetwork` refuses a directed map
    that is not strongly connected. The loader runs `largest_strong_component` so the documented
    `netimport.py --city ... --out map.json` workflow ends in a runnable map rather than an error."""
    import json

    from scms_sim_ref.mock_pipeline.run import _parse_custom_network

    doc = {
        "nodes": [[0, 0], [400, 0], [400, 400], [0, 400], [900, 400]],
        "edges": [[0, 1], [1, 2], [2, 3], [3, 0], [2, 4]],
        "directed_edges": [{"a": 0, "b": 1}, {"a": 1, "b": 2}, {"a": 2, "b": 3}, {"a": 3, "b": 0},
                           {"a": 2, "b": 4}],          # node 4: enter, never leave (clipped exit)
    }
    nodes, edges = _parse_custom_network(json.dumps(doc), directed=True)
    assert len(nodes) == 4                              # the dead-end sink is trimmed
    net = CustomNetwork(nodes, edges)                   # ... and the result actually builds
    assert net.directed is True


def _rich_square():
    square = [[0, 0], [400, 0], [400, 400], [0, 400]]
    return CustomNetwork(square, [{"a": 0, "b": 1, "speed": 20.0, "lanes_forward": 3,
                                   "lanes_backward": 1, "shape": [[200, 40]]},
                                  {"a": 1, "b": 2, "oneway": True, "lanes": 2},
                                  {"a": 2, "b": 3, "oneway": True, "lanes": 2},
                                  {"a": 3, "b": 0, "oneway": True, "lanes": 2}])


def test_document_round_trips_back_into_the_parser():
    net = _rich_square()
    doc = net.document()
    again = CustomNetwork(doc["nodes"], doc["edges"])
    assert again.edge_lanes == net.edge_lanes
    assert again.edge_oneway == net.edge_oneway
    assert again.edge_shape == net.edge_shape
    assert again.edge_speed == net.edge_speed


def test_geometry_keeps_the_positional_edge_form_every_consumer_indexes():
    """gui/server.py and tools/verify_data.py's N1 check both do int(e[0]), int(e[1]) on every edge.
    An object there would break them, so the structure goes in a parallel `edge_attrs` list."""
    plain = CustomNetwork(H_NODES, H_EDGES).geometry()
    assert "edge_attrs" not in plain                       # a plain map is unchanged
    geo = _rich_square().geometry()
    assert all(isinstance(e, list) and isinstance(int(e[0]), int) and isinstance(int(e[1]), int)
               for e in geo["edges"])
    assert len(geo["edge_attrs"]) == len(geo["edges"])
    a0 = geo["edge_attrs"][geo["edges"].index([0, 1, 20.0])]
    assert a0["lanes_forward"] == 3 and a0["lanes_backward"] == 1 and a0["shape"] == [[200.0, 40.0]]


# --------------------------------------------------------------------------- #
# the drivable SURFACE layer -- `dist_to_road` asks a MAP question, not a routing one
# --------------------------------------------------------------------------- #
def test_road_surface_is_absent_until_asked_for_and_changes_nothing():
    """The whole layer is opt-in. An untouched network exposes an empty surface and computes the
    identical distances, which is what keeps every pinned golden byte-identical."""
    net = CustomNetwork(H_NODES, H_EDGES)
    assert net._junc == () and net._jindex == {} and net._surface == {}
    assert "road_surface" not in net.stats()
    before = [net.dist_to_road(x, y) for x in range(0, 900, 97) for y in range(0, 700, 89)]
    other = CustomNetwork(H_NODES, H_EDGES)
    assert [other.dist_to_road(x, y) for x in range(0, 900, 97)
            for y in range(0, 700, 89)] == before


def test_a_junction_disc_makes_the_junction_INTERIOR_road():
    """The reason this exists: a vehicle crossing a signalised junction is on an internal lane that
    no graph edge covers, so it measures as far off-road as the junction is big. The junction
    polygon is the map's own statement that the whole area is tarmac."""
    net = CustomNetwork(H_NODES, H_EDGES)
    # (400, 150) is inside the H's crossbar area but 150 m from the nearest centreline
    off = net.dist_to_road(400.0, 150.0)
    assert off > 100.0
    net.set_road_surface(junctions=[(400.0, 150.0, 20.0)])
    assert net.dist_to_road(400.0, 150.0) == 0.0          # inside the disc
    assert net.dist_to_road(400.0, 165.0) == 0.0          # still inside
    assert net.dist_to_road(400.0, 200.0) == pytest.approx(30.0, abs=1e-6)   # 50 m out, r = 20
    # ... and a position genuinely off the map is still far off it: the gate keeps its teeth
    assert net.dist_to_road(400.0, 900.0) > 250.0


def test_surface_polylines_are_geometry_and_NOT_topology():
    """A surface polyline must not become a road you can drive: no node, no edge, no route, no
    length. It exists so `dist_to_road` stops calling an honest vehicle off-road."""
    net = CustomNetwork(H_NODES, H_EDGES)
    edges_before, len_before, adj_before = list(net.edges), dict(net.edge_len), dict(net.uadj)
    assert net.dist_to_road(200.0, 500.0) > 150.0
    info = net.set_road_surface(polylines=[[[0.0, 500.0], [400.0, 500.0], [800.0, 500.0]]],
                                junctions=[])
    assert net.dist_to_road(200.0, 500.0) == pytest.approx(0.0, abs=1e-9)
    assert list(net.edges) == edges_before and dict(net.edge_len) == len_before
    assert dict(net.uadj) == adj_before
    assert info["surface_segments"] == 2 and info["surface_polylines"] == 1
    assert net.stats()["road_surface"]["surface_segments"] == 2


def test_the_surface_ring_walk_agrees_with_a_brute_force_scan():
    """The cell index prunes; it must not approximate. Junction discs are registered by BOUNDING
    BOX so every point of a disc lies in a registered cell -- the same invariant the segment
    chunking maintains, and what makes the early break exact."""
    net = CustomNetwork(H_NODES, H_EDGES)
    polys = [[[0.0, 500.0], [400.0, 520.0], [800.0, 500.0]], [[100.0, -200.0], [700.0, -180.0]]]
    juncs = [(0.0, 0.0, 12.0), (400.0, 300.0, 35.0), (800.0, 600.0, 5.0)]
    net.set_road_surface(polylines=polys, junctions=juncs)
    segs = list(net._segs)
    for x in range(-300, 1200, 71):
        for y in range(-400, 900, 67):
            brute = min([_pt_seg_dist(x, y, *s) for s in segs]
                        + [max(0.0, math.hypot(x - jx, y - jy) - jr) for jx, jy, jr in juncs])
            assert net.dist_to_road(float(x), float(y)) == pytest.approx(brute, abs=1e-9), (x, y)


def test_the_surface_invalidates_the_distance_memo():
    """`dist_to_road` memoises, and the surface changes the answer. A stale memo would report the
    pre-surface distance for every position already queried during network construction."""
    net = CustomNetwork(H_NODES, H_EDGES)
    assert net.dist_to_road(400.0, 150.0) > 100.0         # populate the memo
    net.set_road_surface(junctions=[(400.0, 150.0, 20.0)])
    assert net.dist_to_road(400.0, 150.0) == 0.0


def test_a_malformed_surface_is_refused():
    net = CustomNetwork(H_NODES, H_EDGES)
    with pytest.raises(ValueError, match=">= 2 points"):
        net.set_road_surface(polylines=[[[0.0, 0.0]]])
    with pytest.raises(ValueError, match="radius"):
        net.set_road_surface(junctions=[(0.0, 0.0, -1.0)])
    with pytest.raises(ValueError, match="non-finite"):
        net.set_road_surface(polylines=[[[0.0, 0.0], [float("inf"), 1.0]]])
