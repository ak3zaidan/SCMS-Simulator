"""AI-designable environments: CustomNetwork (arbitrary road graphs), the spider topology,
and the timed scenario-event engine (demand surges, weather fronts, closures, attack waves).

Everything must be deterministic (same config -> byte-identical digest) and default-safe
(no custom net / no events -> the legacy paths, guarded by golden digests elsewhere).
"""
import json
import math
import random

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.roads import CustomNetwork, GridNetwork, spider_graph
from scms_sim_ref.mock_pipeline.run import _parse_events, validate_config
from scms_sim_ref.datagen import validate as V


# an H-shaped town: two vertical avenues joined by a central bridge, plus a southern bypass
H_NODES = [[0, 0], [0, 300], [0, 600], [400, 300], [800, 0], [800, 300], [800, 600], [400, 0]]
H_EDGES = [[0, 1], [1, 2], [1, 3], [3, 5], [4, 5], [5, 6], [0, 7], [7, 4]]
H_JSON = json.dumps({"nodes": H_NODES, "edges": H_EDGES})


# ---------------- CustomNetwork core ----------------
def test_custom_network_validation_errors():
    with pytest.raises(ValueError, match="at least 2 nodes"):
        CustomNetwork([[0, 0]], [[0, 0]])
    with pytest.raises(ValueError, match="self-loop"):
        CustomNetwork([[0, 0], [100, 0]], [[0, 0]])
    with pytest.raises(ValueError, match="missing node"):
        CustomNetwork([[0, 0], [100, 0]], [[0, 5]])
    with pytest.raises(ValueError, match="connected"):
        CustomNetwork([[0, 0], [100, 0], [500, 500], [600, 500]], [[0, 1], [2, 3]])
    with pytest.raises(ValueError, match="shorter than 1 m"):
        CustomNetwork([[0, 0], [0.1, 0]], [[0, 1]])


def test_custom_network_routes_take_the_shortcut():
    net = CustomNetwork(H_NODES, H_EDGES)
    # 0 -> 5: bridge (0-1-3-5 = 300+~412+~412) vs bypass (0-7-4-5 = 400+400+300 = 1100)
    path = net._route(0, 5)
    assert path == [0, 7, 4, 5] or path == [0, 1, 3, 5]
    total = sum(math.dist(net.coords[a], net.coords[b]) for a, b in zip(path, path[1:]))
    alt = [0, 1, 3, 5] if path == [0, 7, 4, 5] else [0, 7, 4, 5]
    alt_total = sum(math.dist(net.coords[a], net.coords[b]) for a, b in zip(alt, alt[1:]))
    assert total <= alt_total + 1e-6                     # Dijkstra picked the shorter road distance


def test_custom_network_trips_stay_on_roads():
    net = CustomNetwork(H_NODES, H_EDGES)
    rng = random.Random(42)
    for _ in range(20):
        trip = net.random_trip(rng, 12.0, 0.0)
        for f in (0.1, 0.35, 0.62, 0.9):
            x, y, _h = trip.at_distance(trip.length * f)
            assert net.dist_to_road(x, y) < 0.5          # route polyline lies on network edges


def test_dist_to_road_index_is_exact():
    net = CustomNetwork(H_NODES, H_EDGES)
    from scms_sim_ref.mock_pipeline.roads import _pt_seg_dist
    rng = random.Random(7)
    for _ in range(200):
        x, y = rng.uniform(-500, 1300), rng.uniform(-500, 1100)
        brute = min(_pt_seg_dist(x, y, *net.coords[a], *net.coords[b]) for a, b in net.edges)
        assert abs(net.dist_to_road(x, y) - brute) < 1e-9


def test_custom_network_closures_divert_and_protect_bridges():
    net = CustomNetwork(H_NODES, H_EDGES)
    assert net.set_closures([[1, 3]]) == [[1, 3]]        # central bridge closed (bypass remains)
    assert 3 not in [p for p in net._route(0, 5)[1:-1]] or net._route(0, 5) == [0, 7, 4, 5]
    assert net._route(0, 5) == [0, 7, 4, 5]              # forced onto the southern bypass
    # closing a true bridge edge (would strand nodes) is refused
    assert net.set_closures([[5, 6]]) == []              # 6 is a dead end -> 5-6 is a bridge
    net.set_closures([])
    assert net._route(0, 5) in ([0, 1, 3, 5], [0, 7, 4, 5])


def test_grid_set_closures_keeps_connectivity():
    g = GridNetwork(4, 4, 100.0)
    applied = g.set_closures([((0, 0), (0, 1)), ((1, 1), (1, 2))])
    assert len(applied) == 2
    assert (0, 1) not in g._neighbors((0, 0))
    # closing every road out of a corner is refused for the one that would strand it
    applied = g.set_closures([((0, 0), (0, 1)), ((0, 0), (1, 0))])
    assert len(applied) == 1                             # second closure would disconnect (0,0)
    g.set_closures([])
    assert (0, 1) in g._neighbors((0, 0))


def test_spider_graph_shape():
    nodes, edges = spider_graph(arms=6, rings=3, block=150.0)
    assert len(nodes) == 1 + 6 * 3
    assert len(edges) == 6 * 3 + 6 * 3                   # radials (incl centre links) + ring roads
    net = CustomNetwork(nodes, edges)                    # connected or CustomNetwork raises
    assert net.center == 0                                # the plaza is the centre node
    assert all(len(net.adj[i]) >= 3 for i in net.nodes if i != 0)


# ---------------- end-to-end pipeline on AI-designable maps ----------------
def _run(tmp, name, **kw):
    return run_pipeline(PipelineConfig(seed=11, traffic_flow=True, duration_s=50.0,
                                       arrival_rate=1.5, attacker_pct=0.25,
                                       out_dir=str(tmp / name), **kw))


def test_custom_map_pipeline_deterministic_and_clean(tmp_path):
    a = _run(tmp_path, "a", road_network="custom", custom_network=H_JSON)
    b = _run(tmp_path, "b", road_network="custom", custom_network=H_JSON)
    assert a.data_digest == b.data_digest and a.n_vehicles > 10 and a.n_reports > 0
    s = V.validate(str(tmp_path / "a"))[0]
    assert s["leakage_violations"] == 0
    assert s["precision"] is None or s["precision"] >= 0.7


def test_spider_map_pipeline_runs(tmp_path):
    a = _run(tmp_path, "sp", road_network="spider", grid_w=6, grid_h=3)
    assert a.n_vehicles > 10 and a.n_reports > 0
    assert V.validate(str(tmp_path / "sp"))[0]["leakage_violations"] == 0


def test_network_json_written_for_gui(tmp_path):
    _run(tmp_path, "g", road_network="custom", custom_network=H_JSON, live_interval_s=2.0)
    geo = json.loads((tmp_path / "g" / "network.json").read_text())
    assert geo["road_network"] == "custom"
    assert len(geo["nodes"]) == len(H_NODES) and len(geo["edges"]) == len(H_EDGES)


# ---------------- scenario events ----------------
def test_event_validation():
    assert _parse_events("") == []
    evs = _parse_events(json.dumps([{"t": 5, "type": "weather", "value": "fog"},
                                    {"t": 1, "until": 9, "type": "demand", "mult": 2}]))
    assert [e["type"] for e in evs] == ["demand", "weather"]   # sorted chronologically
    for bad in ([{"t": 5, "type": "nope"}],
                [{"t": 5, "type": "weather", "value": "hail"}],
                [{"t": 9, "until": 5, "type": "demand", "mult": 2}],
                [{"t": 5, "type": "demand", "mult": 2}],          # demand needs until
                [{"t": 5, "type": "attack_wave"}],                # wave needs until
                [{"t": 5, "type": "close_edge"}]):                # closure needs edge
        with pytest.raises(ValueError):
            _parse_events(json.dumps(bad))
    with pytest.raises(ValueError, match="close_edge"):
        validate_config(PipelineConfig(traffic_flow=True, road_network="ring", grid_w=8,
                                       events=json.dumps([{"t": 1, "type": "close_edge",
                                                           "edge": [0, 1]}])))


def test_custom_map_requires_traffic_flow():
    """Fixed-fleet vehicles drive straight lines -- off any designed road -- so custom/spider maps
    demand routed traffic flow (otherwise mapOffRoad mass-flags benign vehicles)."""
    with pytest.raises(ValueError, match="traffic_flow"):
        validate_config(PipelineConfig(road_network="custom", custom_network=H_JSON))
    with pytest.raises(ValueError, match="traffic_flow"):
        validate_config(PipelineConfig(road_network="spider", grid_w=6, grid_h=2))


def test_demand_surge_adds_traffic_inside_the_window(tmp_path):
    base = _run(tmp_path, "d0", road_network="custom", custom_network=H_JSON)
    surged = _run(tmp_path, "d1", road_network="custom", custom_network=H_JSON,
                  events=json.dumps([{"t": 10, "until": 40, "type": "demand", "mult": 3.0}]))
    assert surged.n_vehicles > base.n_vehicles * 1.5     # ~3x arrivals for 30 of 50 seconds
    again = _run(tmp_path, "d2", road_network="custom", custom_network=H_JSON,
                 events=json.dumps([{"t": 10, "until": 40, "type": "demand", "mult": 3.0}]))
    assert surged.data_digest == again.data_digest       # events stay deterministic


def test_attack_wave_gates_falsification(tmp_path):
    always = _run(tmp_path, "w0", road_network="custom", custom_network=H_JSON)
    # wave only active for the final second -> attackers barely ever falsify
    gated = _run(tmp_path, "w1", road_network="custom", custom_network=H_JSON,
                 events=json.dumps([{"t": 49, "until": 50, "type": "attack_wave"}]))
    s_always = V.validate(str(tmp_path / "w0"))[0]
    s_gated = V.validate(str(tmp_path / "w1"))[0]
    assert s_gated["revoked"] <= s_always["revoked"]
    assert gated.n_reports < always.n_reports            # far less misbehaviour on the air


def test_weather_front_changes_the_run(tmp_path):
    clear = _run(tmp_path, "wx0", road_network="custom", custom_network=H_JSON)
    fronted = _run(tmp_path, "wx1", road_network="custom", custom_network=H_JSON,
                   events=json.dumps([{"t": 20, "type": "weather", "value": "snow"}]))
    assert fronted.data_digest != clear.data_digest      # sensors/radio degrade after the front
    again = _run(tmp_path, "wx2", road_network="custom", custom_network=H_JSON,
                 events=json.dumps([{"t": 20, "type": "weather", "value": "snow"}]))
    assert fronted.data_digest == again.data_digest


def test_closure_event_reroutes_new_trips(tmp_path):
    ev = json.dumps([{"t": 0, "until": 50, "type": "close_edge", "edge": [1, 3]}])
    closed = _run(tmp_path, "c1", road_network="custom", custom_network=H_JSON, events=ev)
    open_ = _run(tmp_path, "c0", road_network="custom", custom_network=H_JSON)
    assert closed.data_digest != open_.data_digest       # trips diverted off the bridge
    assert closed.n_vehicles > 10                        # still a healthy simulation
