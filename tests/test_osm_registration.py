"""Road/building registration: simplification must not push a street into a wall.

The defect these tests pin is measured in `docs/realism/OSM-REGISTRATION.md`: on the Ingolstadt
extract the shipped import put **10.35% of its road length and 9.62% of its vehicle positions inside
its own building footprints**, of which 3.48 pp (95.2% of the true, halo-free overlap) was RDP at a
10 m tolerance chording a curved street straight through a block. What is left over after the fix is
`tunnel=building_passage` archways, which are real and are reported rather than deleted.

Every fixture here is synthetic and offline; the real-extract numbers live in the tool.
"""
import json
import math

import pytest

from scms_sim_ref.mock_pipeline.osm import (FootprintIndex, _point_in_ring, _rdp, _rdp_avoid,
                                            _seg_crosses, extract_buildings, network_document,
                                            osm_to_network, through_building_reason)

# --------------------------------------------------------------------------- #
# a synthetic extract: one street that bows AROUND a building, so its chord goes THROUGH it
# --------------------------------------------------------------------------- #
LAT0, LON0 = 48.100, 11.500
DLAT = 1.0 / 110540.0                    # 1 m in latitude, in osm.py's own frame
DLON = 1.0 / (111320.0 * math.cos(math.radians(LAT0)))


def _ll(x_m, y_m):
    return (LAT0 + y_m * DLAT, LON0 + x_m * DLON)


def _osm(ways, buildings=()):
    """ways = [(id, {tags}, [(x_m, y_m), ...])]; buildings = [(id, {tags}, [(x, y), ...])]."""
    nodes, out = {}, ['<?xml version="1.0"?><osm version="0.6">']
    nid = [0]

    def ref(pt):
        key = (round(pt[0], 6), round(pt[1], 6))
        if key not in nodes:
            nid[0] += 1
            nodes[key] = str(nid[0])
        return nodes[key]

    body = []
    for wid, tags, pts in ways:
        nds = "".join(f'<nd ref="{ref(_ll(*p))}"/>' for p in pts)
        tg = "".join(f'<tag k="{k}" v="{v}"/>' for k, v in tags.items())
        body.append(f'<way id="{wid}">{nds}{tg}</way>')
    for wid, tags, pts in buildings:
        ring = list(pts) + [pts[0]]
        nds = "".join(f'<nd ref="{ref(_ll(*p))}"/>' for p in ring)
        tg = "".join(f'<tag k="{k}" v="{v}"/>' for k, v in tags.items())
        body.append(f'<way id="{wid}">{nds}{tg}</way>')
    for (la, lo), r in nodes.items():
        out.append(f'<node id="{r}" lat="{la}" lon="{lo}"/>')
    out.extend(body)
    out.append("</osm>")
    return "".join(out)


#: a street that detours 9 m around a 20x14 m block sitting on its chord. 9 m < the 10 m RDP
#: tolerance, so plain RDP flattens the detour and drives the road straight through the building.
BOW = [(1, [(0.0, 0.0), (20.0, 0.0), (40.0, -9.0), (60.0, -9.0), (80.0, 0.0), (100.0, 0.0)])]
BLOCK = [(9001, {"building": "yes"}, [(35.0, -4.0), (65.0, -4.0), (65.0, 6.0), (35.0, 6.0)])]


def _extract(way_tags=None):
    tags = {"highway": "residential"}
    tags.update(way_tags or {})
    return _osm([(w, tags, pts) for w, pts in BOW], BLOCK)


def _polys():
    """The block in RAW (x, y) metres -- for the primitives, which take no projection."""
    return [[[35.0, -4.0], [65.0, -4.0], [65.0, 6.0], [35.0, 6.0]]]


def _projected(xml):
    """(nodes, edges, info, footprints) with the footprints in the graph's OWN frame.

    The frame's origin is min(lat)/min(lon) over the ROAD ways, so a fixture polygon written in raw
    metres is offset from the graph by whatever the road's southernmost point is -- exactly the trap
    `extract_buildings` exists to prevent. Going through `extract_buildings` is the only correct way
    to get them registered, here as much as in production."""
    nodes, edges, info = osm_to_network(xml, max_nodes=5000)
    polys, _bi = extract_buildings(xml, info["projection"], info["road_bbox"])
    return nodes, edges, info, polys


# --------------------------------------------------------------------------- #
# primitives
# --------------------------------------------------------------------------- #
def test_seg_crosses_is_proper_not_touching():
    # a clean X
    assert _seg_crosses(0, 0, 10, 10, 0, 10, 10, 0)
    # sharing an endpoint is NOT a crossing: a building_passage way ends ON the wall it pierces
    assert not _seg_crosses(0, 0, 10, 0, 10, 0, 10, 10)
    # disjoint
    assert not _seg_crosses(0, 0, 1, 0, 5, 5, 6, 6)


def test_point_in_ring_matches_an_obvious_square():
    ring = [(0, 0), (10, 0), (10, 10), (0, 10)]
    assert _point_in_ring(5, 5, ring)
    assert not _point_in_ring(15, 5, ring)
    assert not _point_in_ring(5, -1, ring)


def test_footprint_index_finds_crossing_and_containment():
    idx = FootprintIndex(_polys())
    assert idx.entered(30.0, 1.0, 70.0, 1.0) == frozenset({0})      # straight through
    assert idx.entered(40.0, 0.0, 60.0, 0.0) == frozenset({0})      # wholly inside, no crossing
    assert idx.entered(0.0, 50.0, 100.0, 50.0) == frozenset()       # well clear
    assert idx.polyline_entered([(0, 50), (50, 50), (100, 50)]) == frozenset()


# --------------------------------------------------------------------------- #
# the constrained simplification
# --------------------------------------------------------------------------- #
def test_plain_rdp_puts_the_road_in_the_building_and_rdp_avoid_does_not():
    pts = [(0.0, 0.0), (20.0, 0.0), (40.0, -9.0), (60.0, -9.0), (80.0, 0.0), (100.0, 0.0)]
    idx = FootprintIndex(_polys())
    plain = _rdp(pts, 10.0)
    assert plain == [pts[0], pts[-1]]                    # the whole bow is chorded away ...
    assert idx.polyline_entered(plain)                   # ... straight through the block
    stats = {}
    fixed = _rdp_avoid(pts, 10.0, idx, stats)
    assert not idx.polyline_entered(fixed)               # the fix keeps the road out of the wall
    assert len(fixed) > len(plain)
    assert stats["forced_splits"] >= 1


def test_rdp_avoid_does_not_explode_a_road_that_really_goes_through():
    """A chain already inside the footprint must not force a split for every source vertex."""
    pts = [(30.0, 1.0), (40.0, 1.0), (50.0, 1.0), (60.0, 1.0), (70.0, 1.0)]
    idx = FootprintIndex(_polys())
    stats = {}
    out = _rdp_avoid(pts, 10.0, idx, stats)
    assert out == [pts[0], pts[-1]]                      # unchanged from plain RDP
    assert stats.get("forced_splits", 0) == 0


def test_rdp_avoid_terminates_on_a_pathological_chain():
    """Worst case is the original polyline, never an infinite recursion."""
    idx = FootprintIndex(_polys())
    pts = [(x, 1.0 if i % 2 else -1.0) for i, x in enumerate(range(20, 80, 3))]
    out = _rdp_avoid(pts, 50.0, idx, {})
    assert 2 <= len(out) <= len(pts)


# --------------------------------------------------------------------------- #
# osm_to_network wiring
# --------------------------------------------------------------------------- #
def test_default_import_is_byte_identical_without_avoid_polygons():
    xml = _extract()
    a = osm_to_network(xml)
    b = osm_to_network(xml, avoid_polygons=None)
    assert a[0] == b[0] and a[1] == b[1]
    assert "road_building_overlap" not in a[2]


def test_avoid_polygons_removes_the_displacement_and_reports_none_left():
    xml = _extract()
    n0, e0, _i0, polys = _projected(xml)
    n1, e1, i1 = osm_to_network(xml, max_nodes=5000, avoid_polygons=polys)
    idx = FootprintIndex(polys)
    before = sum(1 for a, b, *_ in e0 if idx.entered(*n0[a], *n0[b]))
    after = sum(1 for a, b, *_ in e1 if idx.entered(*n1[a], *n1[b]))
    assert before >= 1 and after == 0
    assert len(n1) > len(n0)                             # the fix costs vertices, and says so
    rep = i1["road_building_overlap"]
    assert rep["n_edges_through_building"] == 0
    assert rep["rdp_forced_splits"] >= 1
    assert rep["rdp_vertices_kept"] > rep["rdp_vertices_plain"]


def test_a_real_building_passage_is_kept_and_flagged_not_deleted():
    """A road tagged `tunnel=building_passage` through the block stays, and is reported as real."""
    xml = _osm([(1, {"highway": "residential", "tunnel": "building_passage"},
                 [(20.0, 1.0), (50.0, 1.0), (80.0, 1.0)])], BLOCK)
    _n0, _e0, _i0, polys = _projected(xml)
    nodes, edges, info = osm_to_network(xml, max_nodes=5000, avoid_polygons=polys)
    rep = info["road_building_overlap"]
    assert rep["n_edges_through_building"] >= 1
    assert rep["n_tagged_passage"] == rep["n_edges_through_building"]
    assert rep["n_untagged"] == 0
    assert all(e["reason"] == "tunnel=building_passage" for e in rep["edges"] if e["inside_m"] > 0)
    assert rep["road_length_through_building_m"] > 0.0   # the street is still there
    doc = network_document(nodes, edges, info)
    assert doc["through_building_edges"]
    assert doc["through_building_edges"][0]["reason"] == "tunnel=building_passage"


def test_untagged_residue_is_reported_as_untagged():
    xml = _osm([(1, {"highway": "residential"},
                 [(20.0, 1.0), (50.0, 1.0), (80.0, 1.0)])], BLOCK)
    _n0, _e0, _i0, polys = _projected(xml)
    _n, _e, info = osm_to_network(xml, max_nodes=5000, avoid_polygons=polys)
    rep = info["road_building_overlap"]
    assert rep["n_untagged"] >= 1 and rep["n_tagged_passage"] == 0


def test_an_untagged_stub_of_a_tagged_passage_inherits_its_reason():
    """OSM splits a street where a tag changes, so the archway laps onto the way next door.

    Ingolstadt has exactly this twice (ways 11014139 and 24692328, 0.5 m each, sharing a junction
    node with 24991573 / 210464672). Calling those "untagged" would make the word useless."""
    xml = _osm([(1, {"highway": "residential", "tunnel": "building_passage"},
                 [(40.0, 1.0), (60.0, 1.0)]),                      # inside the block
                (2, {"highway": "residential"},
                 [(60.0, 1.0), (64.0, 1.0), (120.0, 1.0)])], BLOCK)  # shares the node, laps in
    _n0, _e0, _i0, polys = _projected(xml)
    _n, _e, info = osm_to_network(xml, max_nodes=5000, avoid_polygons=polys)
    rep = info["road_building_overlap"]
    assert rep["n_untagged"] == 0
    inherited = [e for e in rep["edges"] if e.get("via_way")]
    assert inherited and inherited[0]["reason"] == "tunnel=building_passage"
    assert inherited[0]["via_way"] == "1"


def test_inside_m_is_the_overlapping_length_not_the_whole_edge():
    """An edge that clips a corner must not be charged its full length."""
    xml = _osm([(1, {"highway": "residential"},
                 [(-200.0, 5.0), (200.0, 5.0)])], BLOCK)   # 30 m of the 400 m edge is inside
    _n0, _e0, _i0, polys = _projected(xml)
    _n, _e, info = osm_to_network(xml, max_nodes=5000, avoid_polygons=polys)
    rep = info["road_building_overlap"]
    inside = sum(e["inside_m"] for e in rep["edges"])
    whole = sum(e["length_m"] for e in rep["edges"])
    assert 0 < inside < whole
    assert inside == pytest.approx(30.0, abs=2.0)


# --------------------------------------------------------------------------- #
# the tag classifier
# --------------------------------------------------------------------------- #
@pytest.mark.parametrize("tags,expect", [
    ({"tunnel": "building_passage"}, "tunnel=building_passage"),
    ({"tunnel": "yes"}, "tunnel=yes"),
    ({"covered": "arcade"}, "covered=arcade"),
    ({"covered": "yes"}, "covered=yes"),
    ({"layer": "-1"}, "layer=-1"),
    ({"layer": "1"}, None),                              # ABOVE ground is not a passage
    ({"bridge": "yes"}, None),
    ({}, None),
    ({"layer": "junk"}, None),
])
def test_through_building_reason(tags, expect):
    assert through_building_reason(tags) == expect


def test_document_key_is_additive():
    """A document built without the constraint carries no new key at all."""
    xml = _extract()
    nodes, edges, info = osm_to_network(xml)
    doc = network_document(nodes, edges, info)
    assert set(doc) == {"nodes", "edges"}
    assert json.loads(json.dumps(doc)) == doc
