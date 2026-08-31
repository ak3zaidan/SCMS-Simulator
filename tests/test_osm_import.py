"""OSM real-city import: XML -> custom-network conversion (offline, synthetic fixture)."""
import json
import math

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline.osm import (_parse_maxspeed, _rdp, _ring_area, extract_buildings,
                                            osm_to_network)
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


# --------------------------------------------------------------------------- #
# Building footprints (Phase-2 geometric radio model)
#
# The trap this section guards: `osm_to_network` projects the ROAD ways with a
# local equirectangular frame whose origin is min(lat)/min(lon) OVER THE ROADS
# and whose north-south scale is ky = 110540.0 (NOT the 111320 a generic
# snippet uses). Buildings projected with any independently derived origin or
# scale land off the road graph and silently corrupt every LOS/NLOS decision
# while still producing plausible-looking polygons.
# --------------------------------------------------------------------------- #
def _osm_with_buildings(ways, buildings, unclosed=(), incomplete=()):
    """CROSS-style XML plus `building=yes` ways. buildings = [[(lat,lon), ... closed], ...]."""
    doc = _osm(ways)
    body = doc[:-len("</osm>")]
    nid, wid, extra = 90000, 8000, []
    for kind, rings in (("closed", buildings), ("unclosed", unclosed)):
        for ring in rings:
            refs = []
            for lat, lon in ring:
                nid += 1
                extra.append(f'<node id="{nid}" lat="{lat}" lon="{lon}"/>')
                refs.append(str(nid))
            if kind == "closed":
                refs.append(refs[0])
            wid += 1
            nds = "".join(f'<nd ref="{r}"/>' for r in refs)
            extra.append(f'<way id="{wid}">{nds}<tag k="building" v="yes"/></way>')
    for ring in incomplete:                              # a way clipped at the bbox edge
        refs = []
        for lat, lon in ring:
            nid += 1
            extra.append(f'<node id="{nid}" lat="{lat}" lon="{lon}"/>')
            refs.append(str(nid))
        refs.append("999999999")                         # unresolvable node ref
        wid += 1
        nds = "".join(f'<nd ref="{r}"/>' for r in refs)
        extra.append(f'<way id="{wid}">{nds}<tag k="building" v="house"/></way>')
    return body + "".join(extra) + "</osm>"


# a ~55 m x 55 m block sitting inside the CROSS extent, plus a tiny 2 m shed
_BLOCK = [(48.0995, 11.5005), (48.0995, 11.5012), (48.1002, 11.5012), (48.1002, 11.5005)]
_SHED = [(48.0996, 11.5030), (48.0996, 11.50302), (48.09962, 11.50302), (48.09962, 11.5030)]


def test_buildings_reuse_the_road_projection_and_land_on_the_road_graph():
    xml = _osm_with_buildings(CROSS, [_BLOCK])
    nodes, _edges, info = osm_to_network(xml)
    proj, bbox = info["projection"], info["road_bbox"]
    assert proj["ky"] == 110540.0                        # the code's constant, not 111320
    assert proj["lat0"] == pytest.approx(min(48.099, 48.100, 48.101))
    polys, binfo = extract_buildings(xml, proj, bbox)
    assert binfo["building_ways"] == 1 and binfo["polygons"] == 1
    assert binfo["centroid_inside_road_bbox_frac"] == 1.0
    assert binfo["road_bbox_covered_by_buildings"] > 0.0
    assert binfo["alignment_tolerance_m"] > 0.0
    # the footprint must land inside the road extent, in metres, in the SAME frame
    ring = polys[0]
    cx = sum(p[0] for p in ring) / len(ring)
    cy = sum(p[1] for p in ring) / len(ring)
    assert bbox[0] <= cx <= bbox[2] and bbox[1] <= cy <= bbox[3]
    assert 2000.0 < _ring_area([tuple(p) for p in ring]) < 8000.0    # ~55 m x 55 m block


def test_buildings_reject_a_wrong_projection_loudly():
    """The whole point of the alignment assertion: a shifted origin must RAISE, not produce
    plausible-looking polygons a hundred metres off the streets."""
    xml = _osm_with_buildings(CROSS, [_BLOCK])
    _n, _e, info = osm_to_network(xml)
    good = info["projection"]
    bad = dict(good, lat0=good["lat0"] + 0.05)           # ~5.5 km north
    with pytest.raises(ValueError) as ei:
        extract_buildings(xml, bad, info["road_bbox"])
    assert "projection trap" in str(ei.value)
    with pytest.raises(ValueError):                      # ... and a shifted longitude origin too
        extract_buildings(xml, dict(good, lon0=good["lon0"] - 0.05), info["road_bbox"])
    # the ky slip (111320 instead of 110540) is a 0.7% scale error -- it does NOT trip the bbox
    # assertion at this extract size, which is exactly why the tuple is threaded through rather
    # than re-derived; record the sensitivity limit rather than pretend it is covered
    skewed = dict(good, ky=111320.0)
    polys_ok, _ = extract_buildings(xml, good, info["road_bbox"])
    polys_skew, _ = extract_buildings(xml, skewed, info["road_bbox"])
    assert polys_ok[0][0][1] != polys_skew[0][0][1]


def test_building_extraction_drops_incomplete_unclosed_and_tiny_footprints():
    xml = _osm_with_buildings(CROSS, [_BLOCK, _SHED],
                              unclosed=[[(48.0997, 11.5020), (48.0998, 11.5021)]],
                              incomplete=[[(48.0999, 11.5024), (48.0999, 11.5026)]])
    _n, _e, info = osm_to_network(xml)
    polys, binfo = extract_buildings(xml, info["projection"], info["road_bbox"])
    assert binfo["building_ways"] == 4
    assert binfo["incomplete"] == 1                      # clipped at the bbox edge -> dropped
    assert binfo["unclosed"] == 1                        # not a ring -> dropped
    assert binfo["dropped_below_min_area"] == 1          # the 2 m shed
    assert binfo["polygons"] == len(polys) == 1


def test_building_layer_round_trips_into_the_geometric_radio_model(tmp_path):
    """osm.py --buildings writes `buildings` beside nodes/edges; run.py reads it back and the
    geometric model classifies links against it."""
    from scms_sim_ref.mock_pipeline.run import _parse_buildings
    xml = _osm_with_buildings(CROSS, [_BLOCK])
    nodes, edges, info = osm_to_network(xml)
    polys, _ = extract_buildings(xml, info["projection"], info["road_bbox"])
    doc = json.dumps({"nodes": nodes, "edges": edges, "buildings": polys})
    assert _parse_buildings(doc) and len(_parse_buildings(doc)) == len(polys)
    assert _parse_buildings(json.dumps({"nodes": nodes, "edges": edges})) == []
    res = run_pipeline(PipelineConfig(
        seed=3, traffic_flow=True, road_network="custom", custom_network=doc,
        duration_s=40.0, arrival_rate=1.5, attacker_pct=0.2, radio_model="geometric",
        radio_cap_max_mult=2.0, out_dir=str(tmp_path / "geo")))
    assert res.n_vehicles > 0
