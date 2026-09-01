"""netconvert/sumolib road import: a SUMO `.net.xml` -> the engine's custom-network document.

This is the high-fidelity import path. netconvert has already resolved OSM into a validated
topology -- directed edges, real lane counts, junction geometry, turn connections, traffic-light
programs -- and folds curve vertices into edge SHAPE instead of spending graph nodes on them, so
the importer inherits all of that instead of re-parsing OSM.

The tests are hermetic: a hand-written 3-junction `.net.xml` (geo-referenced in UTM zone 32, the
same projection netconvert emits for a German OSM extract) exercises directed edges, per-edge lane
counts, shape polylines, signalised junctions, the node-cap escalation and -- above all -- the
PROJECTION contract, which is the one thing here that fails silently. Tests that need the real
cached city extract skip when it is absent (`datasets/` is gitignored).
"""
from __future__ import annotations

import json
import math
import os

import pytest

from scms_sim_ref.mock_pipeline import netimport
from scms_sim_ref.mock_pipeline.osm import _frame, network_document, osm_cache_path, osm_to_network
from scms_sim_ref.mock_pipeline.roads import CustomNetwork
from scms_sim_ref.mock_pipeline.run import _parse_custom_network

sumolib = pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")

# --------------------------------------------------------------------------- #
# A minimal geo-referenced net: A --(one-way, 2 lanes, curved)--> B <--> C.
# netOffset is a real Ingolstadt-area UTM origin, so the junctions land on the real city.
# --------------------------------------------------------------------------- #
NET_XML = """<?xml version="1.0" encoding="UTF-8"?>
<net version="1.20" junctionCornerDetail="5" limitTurnSpeed="5.50">
    <location netOffset="-677584.01,-5403359.85" convBoundary="0.00,0.00,200.00,150.00"
 origBoundary="11.41,48.75,11.44,48.77"
 projParameter="+proj=utm +zone=32 +ellps=WGS84 +datum=WGS84 +units=m +no_defs"/>
    <type id="highway.residential" priority="3" numLanes="1" speed="13.89" oneway="0"/>
    <type id="highway.primary" priority="9" numLanes="2" speed="22.22" oneway="1"/>

    <edge id="AB" from="A" to="B" priority="9" type="highway.primary">
        <lane id="AB_0" index="0" speed="22.22" length="204.60" width="3.20"
              shape="0.00,0.00 100.00,20.00 200.00,0.00"/>
        <lane id="AB_1" index="1" speed="22.22" length="204.60" width="3.20"
              shape="0.00,3.20 100.00,23.20 200.00,3.20"/>
    </edge>
    <edge id="BC" from="B" to="C" priority="3" type="highway.residential">
        <lane id="BC_0" index="0" speed="13.89" length="150.00" width="3.20"
              shape="200.00,0.00 200.00,150.00"/>
    </edge>
    <edge id="CB" from="C" to="B" priority="3" type="highway.residential">
        <lane id="CB_0" index="0" speed="13.89" length="150.00" width="3.20"
              shape="200.00,150.00 200.00,0.00"/>
    </edge>

    <junction id="A" type="priority" x="0.00" y="0.00" incLanes="" intLanes="" shape="0.00,0.00"/>
    <junction id="B" type="traffic_light" x="200.00" y="0.00" incLanes="AB_0 AB_1 CB_0"
              intLanes="" shape="200.00,0.00"/>
    <junction id="C" type="priority" x="200.00" y="150.00" incLanes="BC_0" intLanes=""
              shape="200.00,150.00"/>

    <connection from="AB" to="BC" fromLane="0" toLane="0" dir="l" state="M"/>
    <connection from="CB" to="BC" fromLane="0" toLane="0" dir="t" state="M"/>
</net>
"""
_OFFSET = (-677584.01, -5403359.85)

# the same net plus a one-way return leg C -> A, which makes it STRONGLY connected -- what a
# directed consumer requires (`CustomNetwork` rejects a one-way map you can drive into and not out)
_RETURN_LEG = """    <edge id="CA" from="C" to="A" priority="3" type="highway.residential">
        <lane id="CA_0" index="0" speed="13.89" length="250.00" width="3.20"
              shape="200.00,150.00 0.00,0.00"/>
    </edge>
"""
NET_XML_LOOP = NET_XML.replace('    <junction id="A"', _RETURN_LEG + '    <junction id="A"')


@pytest.fixture()
def net_path(tmp_path):
    p = tmp_path / "fixture.net.xml"
    p.write_text(NET_XML, encoding="utf-8")
    return str(p)


# --------------------------------------------------------------------------- #
# inverse UTM (pyproj is NOT installed here, so sumolib's own converter raises)
# --------------------------------------------------------------------------- #
def _meridian_arc(lat_deg: float) -> float:
    """Distance from the equator along the meridian, by NUMERIC integration of the meridional
    radius -- deliberately a different method from the series `inverse_utm` inverts, so agreement
    is evidence rather than a restatement."""
    a, f = 6378137.0, 1 / 298.257223563
    e2 = f * (2 - f)
    n = 20000
    lat = math.radians(lat_deg)
    h = lat / n
    total = 0.0
    for i in range(n + 1):                       # Simpson's rule
        phi = i * h
        m = a * (1 - e2) / (1 - e2 * math.sin(phi) ** 2) ** 1.5
        w = 1 if i in (0, n) else (4 if i % 2 else 2)
        total += w * m
    return total * h / 3.0


def test_inverse_utm_agrees_with_an_independently_computed_meridian_arc():
    for lat in (48.7654, 52.5, 40.75):
        northing = _meridian_arc(lat) * 0.9996   # k0, on the central meridian
        lon, got = netimport.inverse_utm(500000.0, northing, 32)
        assert got == pytest.approx(lat, abs=1e-7)
        assert lon == pytest.approx(9.0, abs=1e-9)   # zone 32 central meridian, exactly


def test_inverse_utm_scale_and_zone_handling():
    lon_a, lat_a = netimport.inverse_utm(500000.0, 5403359.85, 32)
    lon_b, lat_b = netimport.inverse_utm(501000.0, 5403359.85, 32)
    # 1 km of easting at 48.8N is ~1 km on the ground: 1 deg lon ~ 111320*cos(lat)
    dx = (lon_b - lon_a) * 111320.0 * math.cos(math.radians(lat_a))
    assert dx == pytest.approx(1000.0, rel=0.002)
    assert netimport.inverse_utm(500000.0, 5403359.85, 33)[0] == pytest.approx(15.0, abs=1e-9)


def test_utm_zone_parsing():
    assert netimport._utm_zone("+proj=utm +zone=32 +ellps=WGS84") == (32, True)
    assert netimport._utm_zone("+proj=utm +zone=19 +south +ellps=WGS84") == (19, False)
    assert netimport._utm_zone("+proj=tmerc +lat_0=0") is None
    assert netimport._utm_zone("!") is None


def test_double_dash_paths_are_refused_before_sumo_silently_corrupts_them():
    """SUMO writes its own command line into an XML comment of the file it produces, and `--` is
    illegal inside an XML comment: the output silently comes out broken. Scratch directories on
    this machine really do contain `--`, so this guard is not theoretical."""
    with pytest.raises(ValueError, match="--"):
        netimport._check_sumo_path("C:/tmp/c--Users-x/out.net.xml", "netconvert output")
    assert netimport._check_sumo_path("C:/tmp/clean/out.net.xml", "x")


# --------------------------------------------------------------------------- #
# .net.xml -> network
# --------------------------------------------------------------------------- #
def test_import_keeps_direction_lanes_speed_and_shape(net_path):
    nodes, edges, info = netimport.import_net(net_path, projection=None)
    assert nodes == [[0.0, 0.0], [200.0, 0.0], [200.0, 150.0]]
    assert edges == [[0, 1, 22.2], [1, 2, 13.9]]       # undirected view, max speed per pair
    directed = info["directed_edges"]
    assert len(directed) == 3                          # AB one-way, BC/CB a two-way pair
    ab = next(d for d in directed if d["id"] == "AB")
    assert (ab["a"], ab["b"]) == (0, 1) and ab["lanes"] == 2 and ab["class"] == "primary"
    assert not any(d["a"] == 1 and d["b"] == 0 for d in directed)   # no reverse of the one-way
    assert {(d["a"], d["b"]) for d in directed if d["id"] in ("BC", "CB")} == {(1, 2), (2, 1)}
    # the curve survives as SHAPE, not as extra graph nodes. roads.py's schema is INTERMEDIATE
    # vertices only (the junction coordinates are implied), so the endpoints are not repeated.
    assert ab["shape"] == [[100.0, 21.6]]
    assert nodes[0] not in ab["shape"] and nodes[1] not in ab["shape"]
    assert ab["length_m"] == pytest.approx(204.6, abs=0.2)             # measured along the polyline
    assert info["signal_nodes"] == [1]                 # only junction B is a traffic light
    assert info["n_signal_nodes"] == 1
    CustomNetwork(nodes, edges)                        # a valid engine road graph


def test_topology_stats_are_the_fidelity_numbers(net_path):
    _n, _e, info = netimport.import_net(net_path, projection=None)
    assert info["oneway_directed_edges"] == 1 and info["oneway_share"] == pytest.approx(1 / 3, 1e-3)
    assert info["degree_histogram"] == {1: 2, 2: 1}
    assert info["dead_ends"] == 2 and info["intersections_deg_ge3"] == 0
    assert info["lane_histogram"] == {1: 2, 2: 1}
    assert info["mean_lanes"] == pytest.approx(4 / 3, abs=1e-3)
    # lane-km = sum(length * lanes) over DIRECTED edges: 204.6*2 + 150 + 150 m
    assert info["lane_km"] == pytest.approx((204.6 * 2 + 300.0) / 1000.0, abs=1e-3)
    assert info["directed_edge_km"] == pytest.approx((204.6 + 300.0) / 1000.0, abs=1e-3)


def test_turn_restrictions_are_exported_on_request(net_path):
    _n, _e, info = netimport.import_net(net_path, projection=None, turns=True)
    by_id = {d["id"]: d for d in info["directed_edges"]}
    # the only connection out of AB is into BC -- a left turn netconvert already resolved
    assert by_id["AB"]["turns"] == [info["directed_edges"].index(by_id["BC"])]
    assert by_id["BC"]["turns"] == []                  # C is a dead end: nothing permitted onward
    _n2, _e2, plain = netimport.import_net(net_path, projection=None)
    assert all("turns" not in d for d in plain["directed_edges"])   # opt-in


def test_node_cap_is_off_by_default_and_escalates_when_set(net_path):
    """3b: netconvert output carries no fake curve nodes, so the ~380-node budget the raw-OSM path
    needs does not apply -- but a cap can still be asked for, and then minor classes go first."""
    _n, _e, info = netimport.import_net(net_path, projection=None)
    assert info["dropped_classes"] == []
    nodes, _e2, capped = netimport.import_net(net_path, projection=None, max_nodes=2)
    assert "residential" in capped["dropped_classes"] and len(nodes) == 2


def test_shapes_can_be_dropped(net_path):
    _n, _e, info = netimport.import_net(net_path, projection=None, shapes=False)
    assert all("shape" not in d for d in info["directed_edges"])


# --------------------------------------------------------------------------- #
# the projection contract -- the failure that is invisible in the output
# --------------------------------------------------------------------------- #
def _fixture_frame():
    """The osm.py-style frame for the fixture's own three junctions."""
    pts = []
    for x, y in ((0.0, 0.0), (200.0, 0.0), (200.0, 150.0)):
        lon, lat = netimport.inverse_utm(x - _OFFSET[0], y - _OFFSET[1], 32)
        pts.append((lat, lon))
    return _frame(pts)


def test_net_is_reprojected_into_the_osm_frame(net_path):
    """A geo net must land in the SAME local metric frame osm.py derives from the road ways --
    that is what registers an imported network against the building footprints already in it.

    The contract is "identical to what the frame formula gives", NOT "true ground distance": the
    frame's ky = 110540 under-scales north-south by ~0.6% against the WGS84 meridian, and the
    raw-OSM graph carries exactly the same distortion. Matching the frame is what keeps the layers
    on top of each other; matching true metres would pull them apart."""
    frame = _fixture_frame()
    nodes, _e, info = netimport.import_net(net_path, projection=frame)
    # the frame's origin is the min corner of the three junctions -> that junction is at (0, 0)
    assert min(p[0] for p in nodes) == pytest.approx(0.0, abs=0.2)
    assert min(p[1] for p in nodes) == pytest.approx(0.0, abs=0.2)
    # every node is exactly the frame projection of its own lon/lat
    for x, y in ((0.0, 0.0), (200.0, 0.0), (200.0, 150.0)):
        lon, lat = netimport.inverse_utm(x - _OFFSET[0], y - _OFFSET[1], 32)
        want = [(lon - frame["lon0"]) * frame["kx"], (lat - frame["lat0"]) * frame["ky"]]
        assert min(math.dist(want, n) for n in nodes) < 0.11        # 0.1 m node rounding
    assert math.dist(nodes[0], nodes[1]) == pytest.approx(200.0, abs=0.5)     # east-west: ~exact
    ns = math.dist(nodes[1], nodes[2])                                        # north-south: -0.6%
    assert 0.99 < ns / 150.0 < 0.995, ns
    assert info["projection"] == frame
    assert "+proj=utm" in info["proj_parameter"]


def test_alignment_gate_rejects_a_shifted_frame(net_path):
    """The projection trap: a wrong origin translates the whole network while every street still
    looks perfectly plausible. The gate is the only place that is observable.

    A pure LONGITUDE shift is the clean translation case -- kx depends on latitude only, so the
    frame stays internally consistent and the extent arm is what has to fire.
    """
    frame = _fixture_frame()
    expect = [0.0, 0.0, 250.0, 200.0]
    netimport.import_net(net_path, projection=frame, expect_bbox_xy=expect)   # passes
    with pytest.raises(ValueError, match="does not land on the expected extent"):
        netimport.import_net(net_path, projection=dict(frame, lon0=frame["lon0"] - 0.05),
                             expect_bbox_xy=expect)                          # ~3.7 km east
    with pytest.raises(ValueError, match="does not land on the expected extent"):
        netimport.import_net(net_path, projection=dict(frame, lat0=frame["lat0"] + 0.05),
                             expect_bbox_xy=expect)                          # ~5.5 km south


def test_the_frame_gate_catches_the_scale_constant_its_message_names():
    """Finding: `_assert_alignment`'s message warned about "ky = 110540, not 111320" while being
    structurally unable to fire on it -- `expect_bbox_xy` is derived from the SAME tuple under test,
    so both sides move together. Measured on the cached extracts, the substitution displaces nodes by
    only 10.6-18.7 m and every city passed. That is far too little for a translation gate to see and
    far too much for the geometric channel's building-blockage test, which would then compute NLOS
    against footprints offset from the road graph.

    `_assert_frame` tests the CONSTANTS instead, so it fires regardless of geometry.
    """
    frame = _fixture_frame()
    netimport._assert_frame(frame)                                   # the real frame passes
    with pytest.raises(ValueError, match="THIS IS THE TRAP"):
        netimport._assert_frame(dict(frame, ky=111320.0))
    with pytest.raises(ValueError, match=r"outside \(0,"):
        netimport._assert_frame(dict(frame, kx=200000.0))
    with pytest.raises(ValueError, match="missing"):
        netimport._assert_frame({"lat0": 48.0, "lon0": 11.0, "kx": 74000.0})
    with pytest.raises(ValueError, match="not a WGS84 coordinate"):
        netimport._assert_frame(dict(frame, lat0=480.0))
    # kx and ky swapped -- the classic units typo, and the reason the band exists at all
    with pytest.raises(ValueError, match="inconsistent with lat0"):
        netimport._assert_frame(dict(frame, kx=netimport.FRAME_KY, ky=netimport.FRAME_KY))
    # the band is symmetric in |lat|: in the southern hemisphere lat0 is the MOST NEGATIVE latitude,
    # so the extract's mean is CLOSER to the equator and kx is LARGER than cos(lat0) gives. A
    # one-sided band derived from the northern case would reject every southern import.
    for lat0 in (-33.87, -22.91, 33.87):
        mean = lat0 + 0.006                        # a ~700 m tall extract, as the cached ones are
        netimport._assert_frame({"lat0": lat0, "lon0": 151.2, "ky": netimport.FRAME_KY,
                                 "kx": netimport.FRAME_KX * math.cos(math.radians(mean))})


def test_the_extent_gate_is_tighter_than_the_city_it_guards():
    """Finding: `tol = max(250, 0.5*diagonal)` around the whole bbox accepted an east-west
    translation of 1821 m and a diagonal one of 2281 m on the real 217-junction Ingolstadt cloud --
    both LARGER than the 1468 x 1216 m modelled area, so a city could be projected entirely off
    itself and still pass.

    The rule now bounds |median - bbox centre| per axis. Asserted on a synthetic cloud so it needs no
    network access, and calibrated (see `ALIGN_CENTRE_FRAC`) against the seven cached cities, whose
    worst real offset fraction is 0.104.
    """
    w, h = 1468.0, 1216.0
    expect = [0.0, 0.0, w, h]
    grid = [[x, y] for x in range(0, int(w), 40) for y in range(0, int(h), 40)]
    netimport._assert_alignment(grid, expect)                        # centred -> passes
    tol_x = max(netimport.ALIGN_MARGIN_M, netimport.ALIGN_CENTRE_FRAC * w)
    assert tol_x < w, "the tolerance must be smaller than the city it guards"
    # the OLD rule's tolerance, for the record: it exceeded the extent in both axes
    assert max(250.0, 0.5 * math.hypot(w, h)) > 0.5 * w
    ok = [[x + tol_x * 0.9, y] for x, y in grid]
    netimport._assert_alignment(ok, expect)
    bad = [[x + tol_x * 1.2, y] for x, y in grid]
    with pytest.raises(ValueError, match="does not land on the expected extent"):
        netimport._assert_alignment(bad, expect)
    # the second arm: a cloud whose CENTRE is right but whose scale is wrong
    spread = [[(x - w / 2) * 3.0 + w / 2, (y - h / 2) * 3.0 + h / 2] for x, y in grid]
    with pytest.raises(ValueError, match="land inside the requested extent"):
        netimport._assert_alignment(spread, expect)


def test_alignment_reports_coverage_without_a_bbox(net_path):
    _n, _e, info = netimport.import_net(net_path, projection=None)
    assert info["alignment"]["node_bbox"] == [0.0, 0.0, 200.0, 150.0]
    assert "nodes_inside_expected_bbox_frac" not in info["alignment"]


def test_non_geo_net_cannot_claim_a_frame(tmp_path):
    p = tmp_path / "plain.net.xml"
    p.write_text(NET_XML.replace(
        '+proj=utm +zone=32 +ellps=WGS84 +datum=WGS84 +units=m +no_defs', "!"), encoding="utf-8")
    with pytest.raises(ValueError, match="no geo-projection"):
        netimport.import_net(str(p), projection=_fixture_frame())
    nodes, _e, _i = netimport.import_net(str(p), projection=None)      # ... but plain import works
    assert nodes[0] == [0.0, 0.0]


# --------------------------------------------------------------------------- #
# document / engine round trip
# --------------------------------------------------------------------------- #
def test_two_way_pairs_share_one_geometry(tmp_path):
    """netconvert gives each carriageway of a two-way street its OWN offset polyline; the engine
    stores ONE geometry per physical road and rejects a second, different one. So the importer has
    to pick a canonical centreline -- the low->high direction -- and mirror it for the other."""
    xml = NET_XML.replace('<lane id="BC_0" index="0" speed="13.89" length="150.00" width="3.20"\n'
                          '              shape="200.00,0.00 200.00,150.00"/>',
                          '<lane id="BC_0" index="0" speed="13.89" length="150.00" width="3.20"\n'
                          '              shape="200.00,0.00 205.00,75.00 200.00,150.00"/>')
    xml = xml.replace('shape="200.00,150.00 200.00,0.00"',
                      'shape="197.00,150.00 190.00,75.00 197.00,0.00"')   # a DIFFERENT carriageway
    p = tmp_path / "twoway.net.xml"
    p.write_text(xml, encoding="utf-8")
    _n, _e, info = netimport.import_net(str(p), projection=None)
    bc = next(d for d in info["directed_edges"] if d["id"] == "BC")
    cb = next(d for d in info["directed_edges"] if d["id"] == "CB")
    assert bc["shape"] and cb["shape"] == list(reversed(bc["shape"]))
    assert bc["length_m"] == cb["length_m"]             # re-measured on the canonical polyline


def test_directed_records_load_into_the_engine_road_graph(tmp_path):
    """Cross-module contract with roads.py: `directed_edges` -> `edges_from_directed` ->
    `CustomNetwork`, with one-ways, per-direction lanes and edge shapes preserved."""
    from scms_sim_ref.mock_pipeline import roads
    adapter = getattr(roads, "edges_from_directed", None)
    if adapter is None:                                  # engine-side directed support not landed
        pytest.skip("roads.edges_from_directed not present in this working tree")
    p = tmp_path / "loop.net.xml"
    p.write_text(NET_XML_LOOP, encoding="utf-8")
    nodes, _edges, info = netimport.import_net(str(p), projection=None)
    assert info["weak_only_nodes"] == 0                  # A->B->C->A: strongly connected already
    net = CustomNetwork(nodes, adapter(info["directed_edges"]))
    assert getattr(net, "directed", False) is True
    assert net.edge_oneway.get((0, 1)) == 1              # AB is one-way, low -> high
    assert net.edge_oneway.get((0, 2)) == -1             # CA is one-way, high -> low
    assert (1, 2) not in net.edge_oneway                 # BC/CB merged back into one two-way road
    assert net.edge_lanes[(0, 1)] == (2, 0)              # 2 lanes forward, none against
    assert net.edge_shape[(0, 1)] == ((100.0, 21.6),)    # the curve, without the junctions
    assert net.edge_len[(0, 1)] == pytest.approx(204.6, abs=0.2)


def test_strong_component_pruning_is_opt_in(net_path):
    """A one-way street out of a clipped extract leaves junctions you can enter and never leave.
    They are always MEASURED; `strong=True` removes them."""
    _n, _e, plain = netimport.import_net(net_path, projection=None)
    # A --one-way--> B <--> C: A can never be re-entered, so the strong component is {B, C}
    assert plain["weak_only_nodes"] == 1 and plain["strong_component_nodes"] == 2
    assert plain["strongly_connected"] is False and plain["n_nodes"] == 3
    nodes, edges, info = netimport.import_net(net_path, projection=None, strong=True)
    assert info["n_nodes"] == 2 and info["weak_only_nodes"] == 0
    assert [d["id"] for d in info["directed_edges"]] == ["BC", "CB"]
    assert edges == [[0, 1, 13.9]] and len(nodes) == 2


def test_document_round_trips_through_the_engine_parser(net_path):
    nodes, edges, info = netimport.import_net(net_path, projection=None)
    doc = network_document(nodes, edges, info)
    blob = json.dumps(doc)
    got_nodes, got_edges = _parse_custom_network(blob)
    assert (got_nodes, got_edges) == (nodes, edges)
    assert json.loads(blob)["signal_nodes"] == [1]
    CustomNetwork(got_nodes, got_edges)


# --------------------------------------------------------------------------- #
# real city: netconvert net vs raw-OSM import (skipped without the gitignored cache)
# --------------------------------------------------------------------------- #
_CACHE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "datasets", "_osmcache")
_INGOLSTADT = (11.4180, 48.7590, 11.4380, 48.7700)
_XML = osm_cache_path(_INGOLSTADT, _CACHE)
_NET = netimport.net_cache_path(_INGOLSTADT, _CACHE)


@pytest.mark.skipif(not (os.path.exists(_XML) and os.path.exists(_NET)),
                    reason="no cached Ingolstadt extract + .net.xml (build with "
                           "`python -m scms_sim_ref.mock_pipeline.netimport --city ingolstadt "
                           "--out ing.json`)")
def test_real_city_netconvert_beats_the_raw_osm_topology():
    """The measured fidelity gate. Same 1.5 km^2 extract, both importers, netconvert as ground
    truth: 217 real junctions vs 341 mostly-fake ones for 123 vs 126 actual intersections, and a
    one-way share the raw path can only reach because it now reads the `oneway` tag at all."""
    with open(_XML, encoding="utf-8") as fh:
        xml = fh.read()
    raw_nodes, raw_edges, raw_info = osm_to_network(xml, attrs=True, signals=True)
    raw = netimport.topology_stats(raw_nodes, raw_edges, raw_info["directed_edges"],
                                   raw_info["signal_nodes"])
    nodes, edges, info = netimport.import_net(_NET, projection=None)

    # 1. the netconvert graph is a TRUE junction graph: far fewer nodes, the same intersections
    assert info["n_nodes"] < 0.75 * raw["n_nodes"]
    assert abs(info["intersections_deg_ge3"] - raw["intersections_deg_ge3"]) < 0.25 * \
        raw["intersections_deg_ge3"]
    # degree-2 nodes are the tell: they are curve vertices, not junctions
    assert raw["degree_histogram"].get(2, 0) > 2 * info["degree_histogram"].get(2, 0)
    # 2. one-way streets exist on both paths now, and agree to a few points
    assert info["oneway_share"] > 0.2
    assert abs(info["oneway_share"] - raw["oneway_share"]) < 0.06
    # 3. real per-edge lane counts (the engine's single global n_lanes replaced by a distribution)
    assert set(info["lane_histogram"]) >= {1, 2}
    assert info["mean_lanes"] > 1.1
    # 4. road length agrees within a few percent -- the two importers see the same city
    assert abs(info["lane_km"] - raw["lane_km"]) / info["lane_km"] < 0.10


@pytest.mark.skipif(not (os.path.exists(_XML) and os.path.exists(_NET)),
                    reason="no cached Ingolstadt extract + .net.xml")
def test_real_city_import_lands_on_the_raw_osm_graph():
    """Registration, measured: re-projected into osm.py's frame, the netconvert junctions sit on
    top of the raw-OSM graph (median offset ~0 m) and share its bounding box. A silent projection
    error would show up here as tens or hundreds of metres."""
    with open(_XML, encoding="utf-8") as fh:
        xml = fh.read()
    raw_nodes, _raw_edges, raw_info = osm_to_network(xml)
    proj = raw_info["projection"]
    expect = [(_INGOLSTADT[0] - proj["lon0"]) * proj["kx"],
              (_INGOLSTADT[1] - proj["lat0"]) * proj["ky"],
              (_INGOLSTADT[2] - proj["lon0"]) * proj["kx"],
              (_INGOLSTADT[3] - proj["lat0"]) * proj["ky"]]
    nodes, _edges, info = netimport.import_net(_NET, projection=proj, expect_bbox_xy=expect)
    cell = 60.0
    grid: dict = {}
    for p in raw_nodes:
        grid.setdefault((int(p[0] // cell), int(p[1] // cell)), []).append(p)
    offsets = []
    for q in nodes:
        ci, cj = int(q[0] // cell), int(q[1] // cell)
        best = math.inf
        for i in range(ci - 1, ci + 2):
            for j in range(cj - 1, cj + 2):
                for p in grid.get((i, j), ()):
                    best = min(best, math.dist(p, q))
        if math.isfinite(best):
            offsets.append(best)
    offsets.sort()
    assert len(offsets) > 0.9 * len(nodes)
    assert offsets[len(offsets) // 2] < 2.0            # median offset: sub-2 m
    assert info["alignment"]["nodes_inside_expected_bbox_frac"] > 0.8
    for k in range(4):                                 # same extent, to a few metres
        assert abs(info["road_bbox"][k] - raw_info["road_bbox"][k]) < 5.0
