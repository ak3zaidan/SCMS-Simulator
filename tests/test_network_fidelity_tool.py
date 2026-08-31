"""`tools/network_fidelity.py`: scoring an engine road network against a SUMO `.net.xml`, and
attributing `traffic.overlap_events` to the network layer.

Hermetic. The ground-truth side uses a hand-written 3-junction `.net.xml` (the same fixture shape
`test_netimport.py` uses) so no netconvert run and no OSM download is needed; the overlap side uses
a synthetic dataset directory whose emission trace places vehicle pairs in known geometric
relationships, so the classifier's answer is checkable by hand rather than by re-deriving it.

The one non-hermetic test scores the real cached Ingolstadt city and skips when `datasets/`
(gitignored) has no cache.
"""
from __future__ import annotations

import importlib.util
import json
import math
import os
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("network_fidelity",
                                               ROOT / "tools" / "network_fidelity.py")
nf = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(nf)


# --------------------------------------------------------------------------- #
# 1. KS statistic
# --------------------------------------------------------------------------- #
def test_ks_of_identical_samples_is_zero_with_p_one():
    """D == 0 must not fall into the alternating-series trap (it sums to 0, not 1, at lambda 0)."""
    a = [1, 2, 2, 3, 3, 3, 4]
    d, p = nf.ks_two_sample(a, list(a))
    assert d == 0.0
    assert p == 1.0


def test_ks_of_disjoint_samples_is_one():
    d, p = nf.ks_two_sample([1, 1, 1], [9, 9, 9])
    assert d == 1.0
    assert p < 0.5


def test_ks_matches_a_hand_computed_ecdf_gap():
    """[1,2,3,4] vs [3,4,5,6]: the ECDFs are furthest apart just after 2 (0.50 vs 0.00)."""
    d, _p = nf.ks_two_sample([1, 2, 3, 4], [3, 4, 5, 6])
    assert d == pytest.approx(0.5, abs=1e-12)


def test_ks_is_symmetric_and_handles_empty_input():
    a, b = [1, 2, 3, 3, 8], [2, 2, 4, 9]
    assert nf.ks_two_sample(a, b)[0] == nf.ks_two_sample(b, a)[0]
    assert math.isnan(nf.ks_two_sample([], [1, 2])[0])


def test_rel_err_treats_a_matched_absence_as_no_error():
    assert nf._rel_err(0, 0) == 0.0
    assert nf._rel_err(5, 0) == float("inf")
    assert nf._rel_err(9, 10) == pytest.approx(-0.1)


# --------------------------------------------------------------------------- #
# 2. Document summaries: the legacy undirected model vs the directed one
# --------------------------------------------------------------------------- #
SQUARE = [[0, 0], [100, 0], [100, 100], [0, 100]]
SQUARE_EDGES = [[0, 1], [1, 2], [2, 3], [3, 0]]


def test_legacy_document_is_summarised_the_way_the_engine_drives_it():
    """A `{nodes, edges}` map has no directions and no lanes, so every road yields BOTH directions
    at one lane each -- one-way share 0 by construction. That is the pre-fix baseline, and the
    whole point of scoring it."""
    s = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES}, "legacy")
    assert s["n_nodes"] == 4 and s["n_roads_undirected"] == 4
    assert s["n_directed_edges"] == 8
    assert s["oneway_share"] == 0.0 and s["oneway_road_share"] == 0.0
    assert s["lane_histogram"] == {1: 8}
    assert s["degree_histogram"] == {2: 4}
    assert s["intersections_deg_ge3"] == 0 and s["dead_ends"] == 0
    assert s["directed_edge_km"] == pytest.approx(0.8)      # 8 x 100 m
    assert s["lane_km"] == pytest.approx(0.8)
    assert s["engine_default_signalised_nodes"] == 4        # traffic_lights=True signalises all


def test_directed_document_carries_real_oneway_share_and_lanes():
    doc = {"nodes": SQUARE,
           "edges": SQUARE_EDGES,
           "directed_edges": [{"a": 0, "b": 1, "lanes": 2, "length_m": 100.0},
                              {"a": 1, "b": 2, "lanes": 3, "length_m": 100.0},
                              {"a": 2, "b": 1, "lanes": 1, "length_m": 100.0},
                              {"a": 2, "b": 3, "lanes": 1, "length_m": 100.0},
                              {"a": 3, "b": 0, "lanes": 1, "length_m": 100.0}],
           "signal_nodes": [1, 2]}
    s = nf.document_summary(doc, "directed")
    assert s["n_directed_edges"] == 5
    # 0->1, 2->3 and 3->0 have no reverse; 1<->2 does
    assert s["oneway_directed_edges"] == 3
    assert s["oneway_share"] == pytest.approx(3 / 5)
    assert s["oneway_road_share"] == pytest.approx(3 / 4)   # 3 of 4 physical roads are one-way
    assert s["lane_histogram"] == {1: 3, 2: 1, 3: 1}
    assert s["lane_km"] == pytest.approx((2 + 3 + 1 + 1 + 1) * 100.0 / 1000.0)
    assert s["n_signal_nodes"] == 2
    assert "engine_default_signalised_nodes" not in s       # a real signal layer is present


def test_shape_polylines_lengthen_an_edge():
    """A curved edge is longer than the straight line between its junctions; the summary has to use
    the polyline or lane-km is understated on every real import."""
    straight = nf.document_summary({"nodes": [[0, 0], [100, 0]], "edges": [[0, 1]]}, "s")
    curved = nf.document_summary(
        {"nodes": [[0, 0], [100, 0]], "edges": [{"a": 0, "b": 1, "shape": [[50, 50]]}]}, "c")
    assert straight["directed_edge_km"] == pytest.approx(0.2)
    # km are reported to 3 dp (metre resolution), so compare at that resolution
    assert curved["directed_edge_km"] == pytest.approx(4 * math.hypot(50, 50) / 1000.0, abs=1e-3)
    assert curved["length_source"].startswith("edge shape")


def test_extended_positional_edges_declare_lanes_and_oneway():
    """[a, b, speed, lanes, oneway] -- roads.parse_edge_spec's own schema, read straight through."""
    s = nf.document_summary({"nodes": SQUARE,
                             "edges": [[0, 1, 13.9, 4, False], [1, 2, 13.9, 2, True],
                                       [2, 3], [3, 0]]}, "positional")
    assert s["n_directed_edges"] == 7                       # 2 + 1 + 2 + 2
    assert s["oneway_directed_edges"] == 1
    assert s["lane_histogram"] == {1: 4, 2: 3}              # 4 total lanes split 2+2 on road 0-1


def test_assume_lanes_is_reported_and_applied():
    s = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES}, "legacy", assume_lanes=2)
    assert s["lane_histogram"] == {2: 8}
    assert s["assumed_lanes_per_direction"] == 2


# --------------------------------------------------------------------------- #
# 3. compare(): the gate metrics
# --------------------------------------------------------------------------- #
def test_a_network_scored_against_itself_is_perfect():
    s = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES,
                             "directed_edges": [{"a": 0, "b": 1, "lanes": 1, "length_m": 100.0},
                                                {"a": 1, "b": 2, "lanes": 1, "length_m": 100.0},
                                                {"a": 2, "b": 3, "lanes": 1, "length_m": 100.0},
                                                {"a": 3, "b": 0, "lanes": 1, "length_m": 100.0}],
                             "signal_nodes": [0]}, "self")
    res = nf.compare(s, s)
    assert res["summary"]["fail"] == 0 and res["summary"]["hard_failures"] == []
    assert all(m["value"] == 0.0 for m in res["metrics"])


def test_the_undirected_model_fails_the_oneway_gate_against_a_oneway_reference():
    """The G7 headline in one assertion: a laneless undirected map cannot represent one-ways, so its
    one-way share is 0 and the gate reports the full pp deficit."""
    cand = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES}, "legacy")
    gt = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES,
                              "directed_edges": [{"a": 0, "b": 1, "lanes": 1, "length_m": 100.0},
                                                 {"a": 1, "b": 2, "lanes": 1, "length_m": 100.0},
                                                 {"a": 2, "b": 3, "lanes": 1, "length_m": 100.0},
                                                 {"a": 3, "b": 0, "lanes": 1, "length_m": 100.0}]},
                             "gyratory")
    res = nf.compare(cand, gt)
    ow = next(m for m in res["metrics"] if m["id"] == "oneway_share_pp")
    assert ow["value"] == pytest.approx(-100.0)             # 0% vs 100%
    assert ow["status"] == "fail" and ow["severity"] == "hard"
    assert "oneway_share_pp" in res["summary"]["hard_failures"]


def test_gate_limits_are_the_roadmap_numbers():
    assert nf.GATES["oneway_share_pp"]["limit"] == 5.0
    assert nf.GATES["degree_ks"]["limit"] == 0.10
    assert nf.GATES["intersection_rel_err"]["limit"] == 0.10
    assert [k for k, v in nf.GATES.items() if v["severity"] == "hard"] == [
        "oneway_share_pp", "degree_ks", "intersection_rel_err"]


def test_informational_metrics_never_become_hard_failures():
    cand = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES}, "a")
    gt = nf.document_summary({"nodes": SQUARE, "edges": SQUARE_EDGES,
                              "signal_nodes": [0, 1, 2, 3]}, "b")
    res = nf.compare(cand, gt)
    sig = next(m for m in res["metrics"] if m["id"] == "signal_rel_err")
    assert sig["status"] == "fail" and sig["severity"] == "informational"
    assert res["summary"]["hard_failures"] == []


# --------------------------------------------------------------------------- #
# 4. Ground truth from a .net.xml
# --------------------------------------------------------------------------- #
pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")

# A -> B one-way (2 lanes, curved via 100,20), B <-> C two-way (1 lane each way). B is signalised.
NET_XML = """<?xml version="1.0" encoding="UTF-8"?>
<net version="1.20" junctionCornerDetail="5" limitTurnSpeed="5.50">
    <location netOffset="0.00,0.00" convBoundary="0.00,0.00,200.00,150.00"
 origBoundary="11.41,48.75,11.44,48.77" projParameter="!"/>
    <type id="highway.residential" priority="3" numLanes="1" speed="13.89" oneway="0"/>
    <type id="highway.primary" priority="9" numLanes="2" speed="22.22" oneway="1"/>

    <edge id="AB" from="A" to="B" priority="9" type="highway.primary">
        <lane id="AB_0" index="0" speed="22.22" length="200.00" width="3.20"
              shape="0.00,0.00 100.00,20.00 200.00,0.00"/>
        <lane id="AB_1" index="1" speed="22.22" length="200.00" width="3.20"
              shape="0.00,3.20 100.00,23.20 200.00,3.20"/>
    </edge>
    <edge id="BC" from="B" to="C" priority="3" type="highway.residential">
        <lane id="BC_0" index="0" speed="13.89" length="140.00" width="3.20"
              shape="200.00,0.00 200.00,150.00"/>
    </edge>
    <edge id="CB" from="C" to="B" priority="3" type="highway.residential">
        <lane id="CB_0" index="0" speed="13.89" length="140.00" width="3.20"
              shape="200.00,150.00 200.00,0.00"/>
    </edge>

    <junction id="A" type="priority" x="0.00" y="0.00" incLanes="" intLanes="" shape="0.00,0.00"/>
    <junction id="B" type="traffic_light" x="200.00" y="0.00" incLanes="AB_0 AB_1 CB_0"
              intLanes="" shape="200.00,0.00"/>
    <junction id="C" type="priority" x="200.00" y="150.00" incLanes="BC_0" intLanes=""
              shape="200.00,150.00"/>
</net>
"""


@pytest.fixture()
def net_path(tmp_path):
    p = tmp_path / "fixture.net.xml"
    p.write_text(NET_XML, encoding="utf-8")
    return str(p)


def test_net_ground_truth_reads_directions_lanes_and_signals(net_path):
    gt = nf.net_ground_truth(net_path)
    assert gt["n_nodes"] == 3 and gt["n_roads_undirected"] == 2
    assert gt["n_directed_edges"] == 3
    assert gt["oneway_directed_edges"] == 1                 # A->B has no reverse
    assert gt["oneway_share"] == pytest.approx(1 / 3, abs=1e-4)     # reported to 4 dp
    assert gt["oneway_road_share"] == pytest.approx(0.5)
    assert gt["lane_histogram"] == {1: 2, 2: 1}
    assert gt["degree_histogram"] == {1: 2, 2: 1}
    assert gt["intersections_deg_ge3"] == 0 and gt["dead_ends"] == 2
    assert gt["n_signal_nodes"] == 1


def test_gt_length_convention_is_selectable_and_both_are_reported(net_path):
    """SUMO's `edge.getLength()` stops at the junction boundary; the engine's junctions are points,
    so its edges run centre to centre and are longer. Charging the candidate for that difference is
    a measurement bug, so the default is centre-to-centre and the other number is still reported."""
    centre = nf.net_ground_truth(net_path, length="centre")
    lane = nf.net_ground_truth(net_path, length="lane")
    # sumolib derives an edge's shape as the MEAN of its lane shapes, so A->B bends through
    # y = (20 + 23.2) / 2 = 21.6, and the centre-to-centre polyline runs A -> that vertex -> B
    ab = 2 * math.hypot(100, 21.6)
    assert centre["directed_edge_km"] == pytest.approx((ab + 150 + 150) / 1000.0, abs=1e-3)
    assert lane["directed_edge_km"] == pytest.approx((200 + 140 + 140) / 1000.0, abs=1e-3)
    assert centre["lane_km_sumo_lane_length"] == lane["lane_km"]
    assert centre["lane_km_centre_to_centre"] == centre["lane_km"]
    assert centre["gt_length_convention"] == "centre"


def test_gt_drops_non_passenger_edges_and_says_so(net_path, tmp_path):
    blocked = NET_XML.replace('<lane id="BC_0" index="0"',
                              '<lane id="BC_0" index="0" allow="rail"')
    p = tmp_path / "blocked.net.xml"
    p.write_text(blocked, encoding="utf-8")
    gt = nf.net_ground_truth(str(p))
    assert gt["n_directed_edges"] == 2                      # B->C is no longer car-drivable
    assert gt["gt_vclass"] == "passenger"
    assert nf.net_ground_truth(str(p), vclass=None)["n_directed_edges"] == 3


def test_an_import_of_the_reference_scores_perfectly_against_it(net_path):
    """The importer's own output vs the net it came from: `ground truth by construction` has to
    actually hold, or the metric is measuring the tool rather than the network."""
    from scms_sim_ref.mock_pipeline.netimport import import_net
    from scms_sim_ref.mock_pipeline.osm import network_document
    nodes, edges, info = import_net(net_path, projection=None, shapes=True)
    cand = nf.document_summary(network_document(nodes, edges, info), "netimport")
    res = nf.compare(cand, nf.net_ground_truth(net_path))
    assert res["summary"]["hard_failures"] == []
    for mid in ("oneway_share_pp", "degree_ks", "intersection_rel_err", "lane_ks"):
        assert next(m for m in res["metrics"] if m["id"] == mid)["value"] == 0.0


# --------------------------------------------------------------------------- #
# 5. Overlap attribution
# --------------------------------------------------------------------------- #
def _dataset(tmp_path, rows, **cfg):
    """A minimal dataset directory: just the manifest config and the emission trace the tool reads."""
    d = tmp_path / "ds"
    (d / "ground_truth").mkdir(parents=True, exist_ok=True)
    conf = {"road_network": "grid", "grid_w": 3, "grid_h": 3, "grid_block_m": 100.0,
            "grid_dropout": 0.0, "seed": 1, "n_lanes": 1, "lane_width_m": 3.5}
    conf.update(cfg)
    (d / "manifest.json").write_text(json.dumps({"config": conf}), encoding="utf-8")
    with open(d / "ground_truth" / "gt_emissions_sample.jsonl", "w", encoding="utf-8") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    return str(d)


def _row(t, vid, x, y):
    return {"t": t, "true_vehicle_id": vid, "true_x": x, "true_y": y}


def test_head_on_pair_on_one_edge_is_attributed_to_the_shared_centreline(tmp_path):
    """Two vehicles on the y=0 road, driving at each other, 0.4 m apart at t=2. That pair exists
    ONLY because both directions share one centreline."""
    rows = []
    for t, a, b in ((1.0, 39.0, 61.0), (2.0, 49.8, 50.2), (3.0, 61.0, 39.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows))
    assert res["overlap_events"] == 1
    assert res["buckets"] == {"same_edge/opposing": 1}
    assert res["attribution"]["shared_centreline_opposing_same_carriageway"] == 1
    assert res["attribution"]["shared_centreline_share"] == 1.0
    assert res["detail"]["max_dist_to_nearest_edge_m"] == 0.0


def test_same_direction_pair_is_not_the_networks_fault(tmp_path):
    """Both driving +x on the same road, 0.5 m apart: an IDM minimum-gap defect. Separating the
    carriageways cannot help, and the attribution must not claim it does."""
    rows = []
    for t, a, b in ((1.0, 20.0, 20.5), (2.0, 30.0, 30.5), (3.0, 40.0, 40.5)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows))
    assert res["overlap_events"] == 3
    assert res["buckets"] == {"same_edge/same_direction": 3}
    assert res["attribution"]["shared_centreline_opposing_same_carriageway"] == 0
    assert res["attribution"]["car_following_same_direction"] == 3


def test_crossing_pair_on_two_streets_is_a_junction_conflict(tmp_path):
    """One vehicle running +x along y=0, one running +y along x=100, meeting at the junction."""
    rows = []
    for t, ax, by in ((1.0, 80.0, 80.0), (2.0, 100.0, 100.0), (3.0, 120.0, 120.0)):
        rows += [_row(t, "veh_a", ax, 0.0), _row(t, "veh_b", 100.0, by - 100.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows))
    assert res["overlap_events"] == 1
    assert res["attribution"]["junction_conflict_different_streets"] == 1
    assert res["attribution"]["shared_centreline_opposing_same_carriageway"] == 0
    assert res["detail"]["at_junction_both"] == 1


def test_a_head_on_pair_straddling_a_junction_is_still_the_same_street(tmp_path):
    """The pair sits either side of the junction at x=100, so the two vehicles snap to DIFFERENT
    grid edges -- but they are on one straight street, so the shared centreline is still the cause.
    Attributing this to 'junction conflict' would understate what directed edges fix."""
    rows = []
    for t, a, b in ((1.0, 89.0, 111.0), (2.0, 99.8, 100.2), (3.0, 111.0, 89.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows))
    assert res["overlap_events"] == 1
    assert res["buckets"] == {"same_street_across_junction/opposing": 1}
    assert res["attribution"]["shared_centreline_opposing_same_carriageway"] == 1
    assert res["attribution"]["of_which_across_a_junction"] == 1
    assert res["attribution"]["junction_conflict_different_streets"] == 0


def test_a_stopped_vehicle_keeps_the_direction_it_was_driving(tmp_path):
    """At a red light a vehicle's instantaneous displacement is zero. Falling back to 'unknown'
    would silently drop queue overlaps out of the attribution, so the search walks outward in time
    to the nearest step in which it actually moved."""
    rows = []
    for t in (1.0, 2.0, 3.0):                               # veh_a stopped at x=50 for 3 s
        rows.append(_row(t, "veh_a", 50.0, 0.0))
    rows += [_row(4.0, "veh_a", 55.0, 0.0)]                 # ... then moves +x
    for t, b in ((1.0, 70.0), (2.0, 60.0), (3.0, 50.4), (4.0, 40.0)):
        rows.append(_row(t, "veh_b", b, 0.0))               # veh_b drives -x into it
    res = nf.attribute_overlaps(_dataset(tmp_path, rows))
    assert res["overlap_events"] == 1
    assert res["detail"]["unknown_heading"] == 0
    assert res["buckets"] == {"same_edge/opposing": 1}


def test_projection_into_carriageways_removes_head_on_pairs_but_not_queue_pairs(tmp_path):
    """The predictive half: re-project the same trace into 1+1 directed lane frames. The head-on
    pair is separated by a full 3.5 m; the same-direction pair moves as a rigid body and stays."""
    rows = []
    for t, a, b in ((1.0, 39.0, 61.0), (2.0, 49.8, 50.2), (3.0, 61.0, 39.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    for t, c in ((1.0, 20.0), (2.0, 30.0), (3.0, 40.0)):
        rows += [_row(t, "veh_c", c, 100.0), _row(t, "veh_d", c + 0.5, 100.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows), project=True)
    assert res["overlap_events"] == 4                       # 1 head-on + 3 queue instants
    assert res["projected"]["carriageway_offset_m"] == pytest.approx(-1.75)
    assert res["projected"]["overlap_events"] == 3          # only the queue pairs survive
    assert res["projected"]["removed"] == 1
    assert res["projected"]["buckets"] == {"same_edge/same_direction": 3}


def test_left_hand_traffic_separates_the_same_pairs(tmp_path):
    rows = []
    for t, a, b in ((1.0, 39.0, 61.0), (2.0, 49.8, 50.2), (3.0, 61.0, 39.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows), project=True, drive_side="left")
    assert res["projected"]["carriageway_offset_m"] == pytest.approx(1.75)
    assert res["projected"]["overlap_events"] == 0


def test_wider_carriageways_separate_further(tmp_path):
    rows = []
    for t, a, b in ((1.0, 39.0, 61.0), (2.0, 49.8, 50.2), (3.0, 61.0, 39.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows), project=True, lanes_per_dir=3)
    assert res["projected"]["carriageway_offset_m"] == pytest.approx(-5.25)


def test_instant_selection_reproduces_the_harness_rule(tmp_path):
    """The harness sub-samples instants with deterministic even spacing. The attribution has to use
    the SAME rule or its total is not comparable to the scorecard's overlap_events."""
    rows = []
    for k in range(50):                                     # 50 instants, all overlapping
        t = float(k + 1)
        rows += [_row(t, "veh_a", 20.0, 0.0), _row(t, "veh_b", 20.4, 0.0)]
    full = nf.attribute_overlaps(_dataset(tmp_path, rows), all_instants=True)
    assert full["trace"]["instants_with_2plus"] == 50
    assert full["overlap_events"] == 50 and not full["trace"]["sub_sampled"]
    capped = nf.attribute_overlaps(_dataset(tmp_path, rows), max_instants=10, all_instants=True)
    assert capped["trace"]["instants_examined"] == 10 and capped["overlap_events"] == 10
    assert capped["all_instants"]["overlap_events"] == 50


def test_pairs_further_apart_than_the_threshold_are_not_counted(tmp_path):
    rows = []
    for t in (1.0, 2.0):
        rows += [_row(t, "veh_a", 20.0, 0.0), _row(t, "veh_b", 21.5, 0.0)]
    assert nf.attribute_overlaps(_dataset(tmp_path, rows))["overlap_events"] == 0
    assert nf.attribute_overlaps(_dataset(tmp_path, rows), overlap_m=2.0)["overlap_events"] == 2


def test_a_networkless_run_is_refused_rather_than_silently_scored(tmp_path):
    rows = [_row(1.0, "veh_a", 0.0, 0.0), _row(1.0, "veh_b", 0.4, 0.0)]
    with pytest.raises(ValueError, match="linear"):
        nf.attribute_overlaps(_dataset(tmp_path, rows, road_network="linear"))


def test_a_custom_network_run_is_attributed_against_its_own_map(tmp_path):
    doc = {"nodes": [[0, 0], [200, 0]], "edges": [[0, 1]]}
    rows = []
    for t, a, b in ((1.0, 39.0, 61.0), (2.0, 49.8, 50.2), (3.0, 61.0, 39.0)):
        rows += [_row(t, "veh_a", a, 0.0), _row(t, "veh_b", b, 0.0)]
    res = nf.attribute_overlaps(_dataset(tmp_path, rows, road_network="custom",
                                         custom_network=json.dumps(doc)))
    assert res["network"]["kind"] == "custom" and res["network"]["nodes"] == 2
    assert res["buckets"] == {"same_edge/opposing": 1}


# --------------------------------------------------------------------------- #
# 6. Real city (skipped without the gitignored OSM cache)
# --------------------------------------------------------------------------- #
_CACHE = ROOT / "datasets" / "_osmcache"


def _cached_city():
    from scms_sim_ref.mock_pipeline.netimport import net_cache_path
    from scms_sim_ref.mock_pipeline.osm import CITY_BBOXES, osm_cache_path
    for city, bbox in sorted(CITY_BBOXES.items()):
        xml = osm_cache_path(bbox, str(_CACHE))
        net = net_cache_path(bbox, str(_CACHE))
        if os.path.exists(xml) and os.path.exists(net):
            return city, xml, net
    return None


@pytest.mark.skipif(_cached_city() is None,
                    reason="no cached OSM extract + netconvert net (datasets/ is gitignored)")
def test_real_city_directed_import_beats_the_undirected_one_on_every_gate():
    """The measurement the roadmap's G7 asks for, on a real city: a directed import reproduces the
    reference one-way share, and the legacy undirected document cannot (its share is 0 by
    construction, so the pp deficit is the reference share itself)."""
    from scms_sim_ref.mock_pipeline.netimport import import_net
    from scms_sim_ref.mock_pipeline.osm import network_document
    city, _xml, net = _cached_city()
    gt = nf.net_ground_truth(net)
    assert gt["oneway_share"] > 0.1, f"{city} has too few one-ways to be a useful reference"

    nodes, edges, info = import_net(net, projection=None, shapes=True)
    directed = nf.compare(nf.document_summary(network_document(nodes, edges, info), "netimport"),
                          gt)
    legacy = nf.compare(nf.document_summary({"nodes": nodes, "edges": edges}, "legacy"), gt)

    assert directed["summary"]["hard_failures"] == []
    assert "oneway_share_pp" in legacy["summary"]["hard_failures"]
    d_ow = next(m for m in directed["metrics"] if m["id"] == "oneway_share_pp")["value"]
    l_ow = next(m for m in legacy["metrics"] if m["id"] == "oneway_share_pp")["value"]
    assert abs(d_ow) <= 5.0 and abs(l_ow) == pytest.approx(gt["oneway_share"] * 100.0, abs=1e-6)
