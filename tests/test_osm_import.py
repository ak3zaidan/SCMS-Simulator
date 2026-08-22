"""OSM real-city import: XML -> custom-network conversion (offline, synthetic fixture)."""
import json
import math

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.osm import _parse_maxspeed, _rdp, osm_to_network
from scms_sim_ref.mock_pipeline.roads import CustomNetwork


def _osm(ways, extra_nodes=()):
    """Build a minimal OSM XML document. ways = [(highway, maxspeed|None, [(lat,lon),...]), ...]."""
    nodes, refs_per_way, nid = {}, [], [0]

    def ref_of(pt):
        for r, q in nodes.items():
            if q == pt:
                return r
        nid[0] += 1
        r = str(nid[0])
        nodes[r] = pt
        return r

    for _hw, _ms, pts in ways:
        refs_per_way.append([ref_of(p) for p in pts])
    for pt in extra_nodes:
        ref_of(pt)
    out = ['<?xml version="1.0"?><osm version="0.6">']
    for r, (lat, lon) in nodes.items():
        out.append(f'<node id="{r}" lat="{lat}" lon="{lon}"/>')
    for k, (hw, ms, _pts) in enumerate(ways):
        nds = "".join(f'<nd ref="{r}"/>' for r in refs_per_way[k])
        tags = f'<tag k="highway" v="{hw}"/>' + (f'<tag k="maxspeed" v="{ms}"/>' if ms else "")
        out.append(f'<way id="{1000 + k}">{nds}{tags}</way>')
    out.append("</osm>")
    return "".join(out)


# ~0.001 deg lat ~ 110 m; a cross of two arterials + a residential loop off one arm
CROSS = [
    ("primary", "50", [(48.100, 11.500), (48.100, 11.502), (48.100, 11.504)]),
    ("secondary", None, [(48.099, 11.502), (48.100, 11.502), (48.101, 11.502)]),
    ("residential", "30", [(48.099, 11.500), (48.099, 11.502)]),
]


def test_osm_conversion_builds_a_drivable_graph():
    nodes, edges, info = osm_to_network(_osm(CROSS))
    net = CustomNetwork(nodes, edges)                    # connected, valid
    assert info["kept_nodes"] == len(nodes) >= 5
    # maxspeed 50 km/h -> 13.9 m/s on the primary; residential default/tag 30 km/h -> 8.3
    speeds = {round(s, 1) for _a, _b, s in edges}
    assert 13.9 in speeds and 8.3 in speeds
    # the shared crossing node is an intersection (degree >= 3)
    assert max(len(net.adj[i]) for i in net.nodes) >= 3


def test_osm_footpaths_and_fragments_are_dropped():
    ways = CROSS + [
        ("footway", None, [(48.105, 11.505), (48.106, 11.506)]),        # not drivable
        ("residential", None, [(48.200, 11.700), (48.201, 11.701)]),    # disconnected fragment
    ]
    nodes, edges, _info = osm_to_network(_osm(ways))
    net = CustomNetwork(nodes, edges)
    # fragment + footpath gone: everything reachable, extent stays local to the cross
    assert net.bbox[2] - net.bbox[0] < 1000.0


def test_osm_maxspeed_parsing():
    assert _parse_maxspeed("50") == pytest.approx(13.9, abs=0.1)
    assert _parse_maxspeed("30 mph") == pytest.approx(13.4, abs=0.1)
    assert _parse_maxspeed("50; 30") == pytest.approx(13.9, abs=0.1)
    assert _parse_maxspeed("walk") is None and _parse_maxspeed(None) is None


def test_rdp_bounds_deviation():
    pts = [(0.0, 0.0), (50.0, 1.0), (100.0, 0.0), (150.0, 30.0), (200.0, 0.0)]
    out = _rdp(pts, 5.0)
    assert out[0] == pts[0] and out[-1] == pts[-1]
    assert (150.0, 30.0) in out                          # a 30 m bump survives a 5 m tolerance
    assert (50.0, 1.0) not in out                        # a 1 m wiggle is simplified away


def test_node_budget_escalation_drops_minor_roads():
    # residential teeth hanging off a primary spine: connected, and each tooth end is a node
    ways = [("primary", None, [(48.1, 11.5 + i * 0.001) for i in range(12)])]
    for i in range(12):
        lon = 11.5 + i * 0.001
        ways.append(("residential", None, [(48.1, lon), (48.1005 + 0.0002 * (i % 3), lon)]))
    full_nodes, _e, info_full = osm_to_network(_osm(ways), max_nodes=10_000)
    assert info_full["dropped_classes"] == []            # generous budget keeps everything
    # a budget below the full graph but above the arterial skeleton forces dropping
    nodes, edges, info = osm_to_network(_osm(ways), max_nodes=len(full_nodes) - 2)
    assert "residential" in info["dropped_classes"]
    assert len(nodes) <= len(full_nodes) - 2
    CustomNetwork(nodes, edges)


def test_osm_graph_runs_in_the_pipeline(tmp_path):
    nodes, edges, _ = osm_to_network(_osm(CROSS))
    res = run_pipeline(PipelineConfig(
        seed=3, traffic_flow=True, road_network="custom",
        custom_network=json.dumps({"nodes": nodes, "edges": edges}),
        duration_s=40.0, arrival_rate=1.0, attacker_pct=0.2, out_dir=str(tmp_path / "o")))
    assert res.n_vehicles > 0
