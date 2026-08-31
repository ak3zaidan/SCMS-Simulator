"""OSM tag fidelity: the tags the importer used to throw away (oneway, lanes, roundabout,
turn:lanes, traffic_signals nodes) and the directed/signalised network document built from them.

Two properties are load-bearing here and are asserted independently of the feature tests:

* the graph the DEFAULT call produces is untouched -- `attrs=True` adds records, it does not move
  a single node, and only `signals=True` may add nodes (by pinning signalised junctions);
* the document stays readable by a consumer that predates the schema: `run.py`'s
  `_parse_custom_network` sees exactly the old `{nodes, edges}` graph, so nothing has to change in
  the engine before this import is useful.
"""
from __future__ import annotations

import json
import os

import pytest

from scms_sim_ref.mock_pipeline.osm import (_frame, _parse_lanes, _parse_oneway, _way_attrs,
                                            extract_tag_stats, network_document, osm_cache_path,
                                            osm_to_network, road_projection)
from scms_sim_ref.mock_pipeline.roads import CustomNetwork
from scms_sim_ref.mock_pipeline.run import _parse_custom_network

# --------------------------------------------------------------------------- #
# a tiny OSM document builder that can carry arbitrary way/node tags
# --------------------------------------------------------------------------- #
def _osm(ways, signal_pts=(), relations=()):
    """ways = [(highway, {tag: value}, [(lat, lon), ...]), ...]; signal_pts = [(lat, lon), ...]."""
    nodes: dict[str, tuple] = {}
    counter = [0]

    def ref_of(pt):
        for r, q in nodes.items():
            if q == pt:
                return r
        counter[0] += 1
        r = str(counter[0])
        nodes[r] = pt
        return r

    refs_per_way = [[ref_of(p) for p in pts] for _hw, _t, pts in ways]
    sig_refs = {ref_of(p) for p in signal_pts}
    out = ['<?xml version="1.0"?><osm version="0.6">']
    for r, (lat, lon) in nodes.items():
        tag = '<tag k="highway" v="traffic_signals"/>' if r in sig_refs else ""
        out.append(f'<node id="{r}" lat="{lat}" lon="{lon}">{tag}</node>')
    for k, (hw, tags, _pts) in enumerate(ways):
        nds = "".join(f'<nd ref="{r}"/>' for r in refs_per_way[k])
        tg = f'<tag k="highway" v="{hw}"/>' + "".join(
            f'<tag k="{a}" v="{b}"/>' for a, b in tags.items())
        out.append(f'<way id="{1000 + k}">{nds}{tg}</way>')
    for kind in relations:
        out.append(f'<relation id="1"><tag k="type" v="restriction"/>'
                   f'<tag k="restriction" v="{kind}"/></relation>')
    out.append("</osm>")
    return "".join(out)


# a cross of a one-way primary and a two-way secondary, plus a residential stub
WAYS = [
    ("primary", {"maxspeed": "50", "oneway": "yes", "lanes": "2"},
     [(48.100, 11.500), (48.100, 11.502), (48.100, 11.504)]),
    ("secondary", {"lanes": "4", "turn:lanes:forward": "left|through"},
     [(48.099, 11.502), (48.100, 11.502), (48.101, 11.502)]),
    ("residential", {"maxspeed": "30"}, [(48.099, 11.500), (48.099, 11.502)]),
]


# --------------------------------------------------------------------------- #
# tag parsing
# --------------------------------------------------------------------------- #
def test_oneway_parsing_covers_yes_reverse_reversible_and_the_implicit_cases():
    assert _parse_oneway({"oneway": "yes"}) == (1, False)
    assert _parse_oneway({"oneway": "true"}) == (1, False)
    assert _parse_oneway({"oneway": "-1"}) == (-1, False)          # digitised against the flow
    assert _parse_oneway({"oneway": "no"}) == (0, False)
    assert _parse_oneway({}) == (0, False)
    # reversible/alternating is a TIME-VARYING direction. The engine has no clock-dependent
    # topology, so it must stay bidirectional AND be flagged -- freezing it into one direction
    # would invent a restriction the city does not have.
    assert _parse_oneway({"oneway": "reversible"}) == (0, True)
    # implicit one-ways: a roundabout, and a motorway with no oneway tag at all
    assert _parse_oneway({"junction": "roundabout"}) == (1, False)
    assert _parse_oneway({"junction": "roundabout", "oneway": "no"}) == (0, False)
    assert _parse_oneway({}, "motorway") == (1, False)
    assert _parse_oneway({}, "residential") == (0, False)


def test_lane_splitting_follows_the_direction():
    assert _parse_lanes({"lanes": "4"}, 0) == (2, 2, True)          # even split, both directions
    assert _parse_lanes({"lanes": "3"}, 0) == (2, 1, True)          # odd -> extra lane forward
    assert _parse_lanes({"lanes": "2"}, 1) == (2, 0, True)          # one-way: all lanes forward
    assert _parse_lanes({"lanes": "3", "lanes:backward": "1"}, 0) == (2, 1, True)
    assert _parse_lanes({"lanes:forward": "2", "lanes:backward": "1"}, 0) == (2, 1, True)
    lanes, back, tagged = _parse_lanes({}, 0)
    assert (lanes, back) == (1, 1) and tagged is False              # default is FLAGGED as default
    assert _parse_lanes({"lanes": "junk"}, 0) == (1, 1, False)      # unparseable -> default


def test_way_attrs_records_roundabout_and_turn_lanes():
    a = _way_attrs({"junction": "roundabout", "turn:lanes": "left|right"}, "tertiary")
    assert a["dir"] == 1 and a["roundabout"] is True and a["turn_lanes"] == "left|right"
    assert "roundabout" not in _way_attrs({}, "residential")


def test_tag_stats_measures_the_extract():
    st = extract_tag_stats(_osm(WAYS, signal_pts=[(48.100, 11.502)], relations=["no_left_turn"]))
    assert st["drivable_ways"] == 3
    assert st["tag_counts"]["oneway"] == 1 and st["tag_counts"]["lanes"] == 2
    assert st["tag_fraction"]["lanes"] == pytest.approx(2 / 3, abs=1e-3)
    assert st["oneway_ways_explicit"] == 1 and st["oneway_share"] == pytest.approx(1 / 3, abs=1e-3)
    assert st["turn_lane_ways"] == 1
    assert st["traffic_signal_nodes"] == 1 and st["traffic_signal_nodes_on_drivable_way"] == 1
    assert st["restriction_relations"] == 1 and st["restriction_kinds"] == {"no_left_turn": 1}
    assert st["highway_classes"] == {"primary": 1, "residential": 1, "secondary": 1}


# --------------------------------------------------------------------------- #
# the graph itself must not move
# --------------------------------------------------------------------------- #
def test_attrs_do_not_change_the_graph():
    xml = _osm(WAYS)
    base_nodes, base_edges, base_info = osm_to_network(xml)
    nodes, edges, info = osm_to_network(xml, attrs=True)
    assert nodes == base_nodes and edges == base_edges
    assert info["projection"] == base_info["projection"]
    assert len(info["edge_attrs"]) == len(edges)                    # one record per edge
    assert "directed_edges" not in base_info                        # OFF by default


def test_road_projection_matches_the_frame_the_graph_was_built_with():
    """netimport re-projects a SUMO net into THIS frame, so the standalone helper must return
    exactly what osm_to_network derived -- not a re-derived origin."""
    xml = _osm(WAYS)
    _n, _e, info = osm_to_network(xml)
    assert road_projection(xml) == info["projection"]
    assert _frame([(48.1, 11.5), (48.2, 11.6)])["ky"] == 110540.0   # the code's constant


# --------------------------------------------------------------------------- #
# directed edges
# --------------------------------------------------------------------------- #
def test_one_way_streets_become_a_single_directed_edge():
    nodes, edges, info = osm_to_network(_osm(WAYS), attrs=True)
    directed = info["directed_edges"]
    pairs = {(d["a"], d["b"]) for d in directed}
    oneway = [d for d in directed if (d["b"], d["a"]) not in pairs]
    assert oneway, "the oneway=yes primary must survive as a single-direction edge"
    # every one-way edge belongs to the primary (2 lanes forward, per `lanes=2` + `oneway=yes`)
    assert all(d["class"] == "primary" and d["lanes"] == 2 for d in oneway)
    # ... while the two-way secondary appears in both directions with lanes=4 split 2/2
    sec = [d for d in directed if d.get("class") == "secondary"]
    assert sec and all(d["lanes"] == 2 for d in sec)
    assert {(d["a"], d["b"]) for d in sec} == {(d["b"], d["a"]) for d in sec}
    # the undirected array is unchanged in size: a one-way is still ONE undirected edge
    assert len(edges) == len(info["edge_attrs"])
    assert len(directed) == sum(1 if a.get("dir") else 2 for a in info["edge_attrs"])
    assert len(nodes) >= 5


def test_oneway_minus_one_is_stored_against_the_sorted_edge_key():
    """`oneway=-1` means the permitted direction is opposite to the way's node order. The edge key
    is sorted (min, max), so the sign has to be re-expressed against that key -- get this wrong and
    every `-1` street points the wrong way, which no aggregate count would reveal."""
    fwd = [("residential", {"oneway": "yes"}, [(48.100, 11.500), (48.100, 11.503)])]
    rev = [("residential", {"oneway": "-1"}, [(48.100, 11.500), (48.100, 11.503)])]
    _n1, _e1, i1 = osm_to_network(_osm(fwd), attrs=True)
    _n2, _e2, i2 = osm_to_network(_osm(rev), attrs=True)
    d1 = [(d["a"], d["b"]) for d in i1["directed_edges"]]
    d2 = [(d["a"], d["b"]) for d in i2["directed_edges"]]
    assert len(d1) == len(d2) == 1
    assert d1[0] == (d2[0][1], d2[0][0])                            # exactly reversed


# --------------------------------------------------------------------------- #
# signals
# --------------------------------------------------------------------------- #
def test_signal_nodes_are_reported_and_pinned_into_the_graph():
    """A signal sits mid-way at a point RDP would otherwise simplify away. With signals=True it
    must survive as a graph node -- a traffic light that is not a node cannot stop anybody."""
    ways = [("primary", {}, [(48.1000, 11.5000), (48.1000, 11.5010), (48.1000, 11.5020),
                             (48.1000, 11.5030)]),
            ("residential", {}, [(48.0995, 11.5030), (48.1000, 11.5030)])]
    mid = (48.1000, 11.5010)                     # collinear -> RDP removes it by default
    plain_nodes, _e, plain_info = osm_to_network(_osm(ways, signal_pts=[mid]))
    nodes, edges, info = osm_to_network(_osm(ways, signal_pts=[mid]), signals=True)
    assert "signal_nodes" not in plain_info
    assert info["signal_refs_found"] == 1
    assert len(info["signal_nodes"]) == 1
    assert len(nodes) == len(plain_nodes) + 1                       # the pinned junction
    sx, sy = nodes[info["signal_nodes"][0]]
    # ... and it really is the tagged point, not just any node
    assert any(abs(sx - x) < 0.2 and abs(sy - y) < 0.2 for x, y in nodes)
    CustomNetwork(nodes, edges)                                     # still a valid road graph


def test_signalisation_is_a_subset_not_all_or_nothing():
    """The point of the mode: FEWER nodes are signalised than there are intersections."""
    ways = [("primary", {}, [(48.100, 11.500 + 0.001 * i) for i in range(5)])]
    for i in range(1, 4):                                           # side streets -> intersections
        ways.append(("residential", {}, [(48.100, 11.500 + 0.001 * i),
                                         (48.101, 11.500 + 0.001 * i)]))
    nodes, _edges, info = osm_to_network(_osm(ways, signal_pts=[(48.100, 11.502)]), signals=True)
    assert len(info["signal_nodes"]) == 1 < len(nodes)


# --------------------------------------------------------------------------- #
# document schema / backward compatibility
# --------------------------------------------------------------------------- #
def test_network_document_is_readable_by_the_pre_schema_consumer():
    nodes, edges, info = osm_to_network(_osm(WAYS), attrs=True, signals=True)
    info["network_meta"] = {"source": "osm"}
    doc = network_document(nodes, edges, info)
    assert set(doc) >= {"nodes", "edges", "directed_edges", "network_meta"}
    blob = json.dumps(doc)
    # run.py's parser ignores everything it does not know: same graph, no new failure mode
    got_nodes, got_edges = _parse_custom_network(blob)
    assert got_nodes == nodes and got_edges == edges
    net = CustomNetwork(got_nodes, got_edges)
    assert net.stats()["n_nodes"] == len(nodes)
    assert json.loads(blob)["network_meta"]["schema"] == 2


def test_network_document_omits_absent_layers():
    nodes, edges, _info = osm_to_network(_osm(WAYS))
    assert set(network_document(nodes, edges, {})) == {"nodes", "edges"}


# --------------------------------------------------------------------------- #
# the real cached extract (skipped where the gitignored cache is absent)
# --------------------------------------------------------------------------- #
_CACHE = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                      "datasets", "_osmcache")
_INGOLSTADT = (11.4180, 48.7590, 11.4380, 48.7700)
_XML = osm_cache_path(_INGOLSTADT, _CACHE)


@pytest.mark.skipif(not os.path.exists(_XML), reason="no cached Ingolstadt OSM extract")
def test_real_extract_carries_the_tags_this_importer_now_reads():
    """Measured on the shipped cache: 347 drivable ways, 48.4% with an `oneway` tag (43.8%
    one-way), 54.5% with `lanes`, 17.9% with a `turn:lanes*`, 59 `traffic_signals` nodes (52 of
    them on a drivable way), 65 turn-restriction relations. Asserted as floors so a refreshed
    extract cannot silently become tag-free."""
    with open(_XML, encoding="utf-8") as fh:
        st = extract_tag_stats(fh.read())
    assert st["drivable_ways"] >= 300
    assert st["tag_fraction"]["oneway"] > 0.40
    assert st["tag_fraction"]["lanes"] > 0.50
    assert st["oneway_share"] > 0.35
    assert st["turn_lane_ways"] >= 50
    assert st["traffic_signal_nodes"] >= 50
    assert st["restriction_relations"] >= 50


@pytest.mark.skipif(not os.path.exists(_XML), reason="no cached Ingolstadt OSM extract")
def test_real_extract_default_import_is_unchanged_by_the_new_code_path():
    with open(_XML, encoding="utf-8") as fh:
        xml = fh.read()
    base_nodes, base_edges, _ = osm_to_network(xml)
    nodes, edges, info = osm_to_network(xml, attrs=True)
    assert (nodes, edges) == (base_nodes, base_edges)
    assert len(base_nodes) == 346 and len(base_edges) == 416       # the historical graph
    # ~44% of the imported edges are one-way -- all of which the old importer made bidirectional
    assert 0.35 < info["oneway_edges"] / len(edges) < 0.55
