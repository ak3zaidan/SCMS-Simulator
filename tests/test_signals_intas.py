"""`signals.py` MEASURED against the real Ingolstadt net.

This is the file that makes every number in `signals.py`'s module docstring falsifiable. It reads
``scms-sim/scenarios/gen_intas_urban_low/sumo/ingolstadt.net.xml`` -- 16.9 MB, 3332 junctions, the
98 real InTAS traffic-light programs -- and re-derives them. The scenario directory is generated and
gitignored, so every test here SKIPS when it is absent; `tests/test_signals.py` covers the same code
hermetically and always runs.

It also re-derives the two hand-verified junctions whose constants `tests/test_signals.py` quotes,
so those constants cannot drift away from the file they were read out of.

Reading the net costs ~0.4 s and is done once for the module.
"""
from __future__ import annotations

import os

import pytest

from scms_sim_ref.mock_pipeline import netimport, signals as S
from tests.test_signals import (INTAS_GAP_LINKS, INTAS_GAP_STATES, INTAS_T_FOES, INTAS_T_PHASES)

sumolib = pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")

NET = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "scms-sim", "scenarios", "gen_intas_urban_low", "sumo", "ingolstadt.net.xml")
pytestmark = pytest.mark.skipif(not os.path.exists(NET),
                                reason=f"InTAS scenario not generated ({NET})")

T_JUNCTION = "1863241632"                     # T-junction, protected + permissive left
GAP_JUNCTION = "cluster_13876325_281116670"   # 4-arm, a state column with no connection


@pytest.fixture(scope="module")
def intas():
    net = sumolib.net.readNet(NET, withPrograms=True)
    return net, S.programs_from_net(net), S.tls_connections(net)


# --------------------------------------------------------------------------- #
# what the city actually contains
# --------------------------------------------------------------------------- #
def test_program_census_matches_the_documented_measurement(intas):
    net, progs, conns = intas
    tl_nodes = {n.getID() for n in net.getNodes() if n.getType().startswith("traffic_light")}
    controlled = {c.getFrom().getToNode().getID() for cs in conns.values() for c in cs}
    assert len(net.getNodes()) == 3332 and len(net.getEdges()) == 7942
    assert len(tl_nodes) == 109                      # junctions netconvert typed traffic_light
    assert len(progs) == 98                          # <tlLogic> programs
    assert len(controlled) == 98 and controlled <= tl_nodes
    assert len(tl_nodes - controlled) == 11          # pedestrian-only clusters, no <tlLogic>
    assert sum(len(v) for v in conns.values()) == 1042
    # every one of the 11 really does have connections, and every one of those is untl'd
    for nid in tl_nodes - controlled:
        n = net.getNode(nid)
        tlids = {c.getTLSID() for e in n.getIncoming()
                 for lst in e.getOutgoing().values() for c in lst}
        assert n.getIncoming() and tlids == {""} and nid not in progs
    # no joined tls: every program governs exactly one junction
    assert {len({c.getFrom().getToNode().getID() for c in cs}) for cs in conns.values()} == {1}


def test_program_stats_are_the_numbers_in_the_module_docstring(intas):
    _net, progs, _conns = intas
    st = S.program_stats(progs)
    assert st["programs"] == 98
    assert st["cycle_s"] == {"min": 77.0, "p25": 90.0, "median": 90.0, "p75": 90.0,
                             "max": 116.0, "mean": 90.59}
    assert st["cycle_hist"] == {77: 2, 86: 2, 90: 86, 91: 2, 94: 1, 95: 1,
                                105: 1, 107: 1, 113: 1, 116: 1}
    assert st["phases_hist"] == {3: 2, 4: 17, 5: 1, 6: 48, 7: 4, 8: 24, 10: 1, 11: 1}
    assert st["green_stages_hist"] == {1: 2, 2: 18, 3: 50, 4: 26, 5: 1, 6: 1}
    assert st["links"] == {"min": 3, "p25": 7.25, "median": 10.0, "p75": 13.0,
                           "max": 19, "mean": 10.53}
    assert st["type_hist"] == {"actuated": 76, "static": 22}
    assert st["state_char_hist"] == {"G": 1186, "g": 303, "r": 4084, "y": 1162}
    assert sum(st["state_char_hist"].values()) == 6735
    assert st["offsets_nonzero"] == 0 and st["programs_without_yellow"] == 0


def test_actuation_is_declared_far_more_often_than_it_is_real(intas):
    """`type="actuated"` alone does NOT mean SUMO would vary the timing: 56 of the 76 declared
    programs carry no minDur/maxDur at all and run fixed-time in SUMO too. Reporting only the
    declared count would overstate what the fixed-time approximation hides by more than 3x."""
    _net, progs, _conns = intas
    st = S.program_stats(progs)
    assert st["actuated_declared"] == 76
    assert st["actuated_effective"] == 20 and st["fixed_time_effective"] == 78
    band = st["actuated_cycle_band_s"]
    assert band["min_sum"] == {"min": 24.0, "p25": 24.0, "median": 32.0, "p75": 41.25,
                               "max": 50.0, "mean": 34.2}
    assert band["max_sum"] == {"min": 110.0, "p25": 159.0, "median": 165.0, "p75": 212.0,
                               "max": 220.0, "mean": 175.55}
    # what we actually run is the nominal duration, and for the 78 fixed programs it is exact
    for p in progs.values():
        lo, hi = p.cycle_bounds_s
        assert lo <= p.cycle_s <= hi
        if not p.actuated:
            assert (lo, hi) == (p.cycle_s, p.cycle_s)


def test_how_much_of_the_state_string_addresses_a_connection(intas):
    _net, progs, conns = intas
    cols = mapped_cols = chars = mapped_chars = 0
    unmapped = {}
    for tid, p in progs.items():
        idxs = {c.getTLLinkIndex() for c in conns.get(tid, ())}
        assert all(0 <= i < p.n_links for i in idxs), tid     # never outside the state string
        cols += p.n_links
        mapped_cols += len(idxs)
        chars += p.n_links * len(p.durations)
        mapped_chars += len(idxs) * len(p.durations)
        miss = sorted(set(range(p.n_links)) - idxs)
        if miss:
            unmapped[tid] = miss
    assert (cols, mapped_cols) == (1032, 1027)
    assert (chars, mapped_chars) == (6735, 6696)
    assert round(100.0 * mapped_cols / cols, 2) == 99.52
    assert round(100.0 * mapped_chars / chars, 2) == 99.42
    assert unmapped == {"249179919": [4], GAP_JUNCTION: [6],
                        "cluster_1427494838_273472399": [0, 1],
                        "cluster_308989441_476075007_476075018": [14]}


# --------------------------------------------------------------------------- #
# THE INDEX MAPPING
# --------------------------------------------------------------------------- #
def test_the_junction_request_index_is_a_second_ordering_not_a_restatement(intas):
    """The audit's independence rests on this. If the junction request index were always equal to
    the tlLogic's linkIndex, the foe matrix would still be an independent SOURCE but not an
    independent ORDERING. On InTAS they differ for 41 of 1042 connections, in 6 junctions."""
    _net, _progs, conns = intas
    same = diff = 0
    junctions = set()
    for cs in conns.values():
        for c in cs:
            ri = c.getFrom().getToNode().getLinkIndex(c)
            assert ri >= 0
            if ri == c.getTLLinkIndex():
                same += 1
            else:
                diff += 1
                junctions.add(c.getTLSID())
    assert (same, diff) == (1001, 41) and len(junctions) == 6
    assert GAP_JUNCTION in junctions


def test_audit_shift_zero_is_thirty_times_cleaner_than_any_shift(intas):
    net, progs, _conns = intas
    rows = {r["shift"]: r for r in
            S.audit_link_indices(net, shifts=(-2, -1, 0, 1, 2), programs=progs)}
    assert rows[0] == {"mapping": "linkIndex", "shift": 0, "conflicts": 11, "pairs": 1951,
                       "junctions": 4, "out_of_range": 0}
    assert rows[1]["conflicts"] == 319 and rows[1]["junctions"] == 78
    assert rows[-1]["conflicts"] == 456 and rows[-1]["junctions"] == 95
    assert rows[2]["conflicts"] == 259 and rows[-2]["conflicts"] == 366
    for s in (-2, -1, 1, 2):
        assert rows[s]["conflicts"] >= 23 * rows[0]["conflicts"], s
        assert rows[s]["out_of_range"] > 0, s          # a shift also runs off the string


def test_dense_renumbering_moves_seventeen_movements_and_creates_seven_conflicts(intas):
    """The realistic bug -- sizing the state string by the connection count -- rather than an
    abstract +-1. It is invisible at 95 of the 98 junctions, which is exactly why it is dangerous."""
    net, progs, conns = intas
    remap = S.dense_link_indices(conns)
    moved = [(t, c.getTLLinkIndex()) for t, cs in conns.items() for c in cs
             if remap[t][c.getTLLinkIndex()] != c.getTLLinkIndex()]
    assert len(moved) == 17
    assert {t: sum(1 for x in moved if x[0] == t) for t in {m[0] for m in moved}} == {
        "249179919": 2, "cluster_1427494838_273472399": 8, GAP_JUNCTION: 7}
    true_row = S.audit_link_indices(net, programs=progs, detail=True)[0]
    dense_row = S.audit_link_indices(net, programs=progs, dense=True, detail=True)[0]
    assert (true_row["conflicts"], true_row["junctions"]) == (11, 4)
    assert (dense_row["conflicts"], dense_row["junctions"]) == (18, 6)
    new = set(map(tuple, dense_row["detail"])) - set(map(tuple, true_row["detail"]))
    assert {(r[0], r[1], r[3], r[4]) for r in new} == {
        (GAP_JUNCTION, 0, 3, 13), (GAP_JUNCTION, 0, 4, 13), (GAP_JUNCTION, 0, 5, 13),
        (GAP_JUNCTION, 4, 1, 9),
        ("cluster_1427494838_273472399", 0, 4, 9), ("cluster_1427494838_273472399", 0, 5, 9),
        ("cluster_1427494838_273472399", 2, 4, 6)}


def test_the_eleven_residual_conflicts_are_properties_of_the_source_net(intas):
    """A residual at shift 0 is expected and must be explained, not waved through. 3 of the 11 are
    two lanes of ONE approach merging into one exit lane -- netconvert calls any merge a foe. The
    other 8 are two InTAS programs that genuinely serve crossing movements protected-green."""
    net, progs, conns = intas
    row = S.audit_link_indices(net, programs=progs, detail=True)[0]
    assert row["conflicts"] == 11
    per_junction = {}
    merges = 0
    for tls_id, _k, _state, la, lb in row["detail"]:
        per_junction[tls_id] = per_junction.get(tls_id, 0) + 1
        froms = {c.getFrom().getID() for c in conns[tls_id] if c.getTLLinkIndex() in (la, lb)}
        tos = {c.getTo().getID() for c in conns[tls_id] if c.getTLLinkIndex() in (la, lb)}
        if len(froms) == 1 and len(tos) == 1:
            merges += 1
    assert per_junction == {"279299817": 7, "cluster_1443568599_365519573": 1,
                            "gneJ144": 1, "gneJ210": 2}
    assert merges == 3                                 # the two lane-merge junctions
    # 94 of the 98 junctions are completely clean
    clean = sum(1 for tid, p in progs.items()
                if not any(st[a] == "G" and st[b] == "G"
                           for a, b in S.foe_pairs(net, tid, conns=conns) for st in p.states))
    assert clean == 94


# --------------------------------------------------------------------------- #
# YELLOW, over the whole city
# --------------------------------------------------------------------------- #
def test_no_link_in_the_whole_city_steps_from_green_to_red_without_a_yellow(intas):
    _net, progs, _conns = intas
    transitions = to_yellow = to_red = 0
    after_yellow = {}
    for p in progs.values():
        n = len(p.states)
        for li in range(p.n_links):
            seq = [p.states[k][li] for k in range(n)]
            for k in range(n):
                a, b = seq[k], seq[(k + 1) % n]
                if a in "Gg":
                    transitions += 1
                    to_yellow += (b == "y")
                    to_red += (b == "r")
                if a == "y":
                    after_yellow[b] = after_yellow.get(b, 0) + 1
    assert (transitions, to_yellow, to_red) == (1489, 1162, 0)
    # a yellow is nearly always followed by red; 134 of them return to green (a movement served by
    # two consecutive stages), which is real and is why "yellow -> red" is NOT asserted
    assert after_yellow == {"r": 1028, "G": 133, "g": 1}


# --------------------------------------------------------------------------- #
# the two HAND-VERIFIED junctions, re-derived from the file
# --------------------------------------------------------------------------- #
def test_hand_verified_t_junction_matches_the_constants_in_test_signals(intas):
    """`1863241632`. Read against the raw XML: 3 arms, 7 controlled connections, linkIndex 0..6
    contiguous, and the geometry says link 1 is the east arm's LEFT turn while link 3 is the
    southern approach's THROUGH movement -- which the foe matrix independently agrees cross."""
    net, progs, conns = intas
    p = progs[T_JUNCTION]
    assert [(d, s, lo, hi) for d, s, lo, hi
            in zip(p.durations, p.states, p.min_dur, p.max_dur)] == INTAS_T_PHASES
    assert (p.type, p.offset, p.n_links, p.cycle_s) == ("actuated", 0.0, 7, 90.0)
    assert p.actuated and p.cycle_bounds_s == (39.0, 165.0)
    assert S.foe_pairs(net, T_JUNCTION, conns=conns) == INTAS_T_FOES
    cs = {c.getTLLinkIndex(): c for c in conns[T_JUNCTION]}
    assert sorted(cs) == list(range(7))
    assert [cs[i].getDirection() for i in range(7)] == ["r", "l", "r", "s", "s", "s", "l"]
    # link 1: comes in on the east arm, leaves on the southbound carriageway -> a LEFT turn
    assert cs[1].getFrom().getID() == "224892361#7" and cs[1].getTo().getID() == "-170018165#0"
    # link 3: comes in from the south, continues north -> the opposing THROUGH movement
    assert cs[3].getFrom().getID() == "170018165#0" and cs[3].getTo().getID() == "170018165#1"
    assert (1, 3) in INTAS_T_FOES
    # the geometry, independently: the two really do cross (bearings ~90 deg apart at the junction)
    import math

    def bearing(e):
        a, b = e.getFromNode().getCoord(), e.getToNode().getCoord()
        return math.degrees(math.atan2(b[0] - a[0], b[1] - a[1])) % 360.0
    turn1 = (bearing(cs[1].getTo()) - bearing(cs[1].getFrom()) + 180) % 360 - 180
    turn3 = (bearing(cs[3].getTo()) - bearing(cs[3].getFrom()) + 180) % 360 - 180
    assert turn1 < -60.0 and abs(turn3) < 10.0          # a left turn across a straight-ahead


def test_hand_verified_gap_junction_matches_the_constants_in_test_signals(intas):
    """`cluster_13876325_281116670`. 14-character state string, 13 controlled connections, and no
    connection anywhere in the file carries linkIndex 6."""
    net, progs, conns = intas
    p = progs[GAP_JUNCTION]
    assert list(p.states) == INTAS_GAP_STATES
    assert (p.type, p.offset, p.n_links, p.cycle_s) == ("static", 0.0, 14, 90.0)
    idx = sorted(c.getTLLinkIndex() for c in conns[GAP_JUNCTION])
    assert idx == INTAS_GAP_LINKS and len(idx) == 13 and 6 not in idx
    # column 6 is a real signal group -- it shows green and yellow -- with no vehicular connection
    assert {st[6] for st in p.states} == {"g", "G", "y", "r"}
    # and the movements that the dense bug would collide are genuine foes
    pairs = set(S.foe_pairs(net, GAP_JUNCTION, conns=conns))
    assert {(3, 13), (4, 13), (5, 13)} <= pairs
    assert p.states[0][13] == "r" and p.states[0][4] == "G" and p.states[0][5] == "G"


# --------------------------------------------------------------------------- #
# the whole importer, end to end
# --------------------------------------------------------------------------- #
@pytest.fixture(scope="module")
def imported():
    return netimport.import_net(NET, signals=True, strong=True, undirected_shapes=True)


def test_the_import_places_every_program_on_a_graph_junction(imported):
    nodes, _edges, info = imported
    st = info["signal_program_stats"]
    recs = info["signal_programs"]
    assert len(nodes) == 3289                          # largest strongly connected component
    assert len(recs) == 98 and st["junctions"] == 98 and st["tls_no_node"] == 0
    assert st["signal_nodes"] == 109 and st["signal_nodes_with_program"] == 98
    assert st["coverage_of_signal_nodes"] == 0.8991
    assert st["program_nodes_not_typed_traffic_light"] == 0
    assert st["links_total"] == 1042 and st["links_mapped"] == 1030
    assert st["links_unmapped"] == 10 and st["links_self_loop"] == 2
    assert st["state_columns"] == 1032 and st["state_columns_mapped"] == 1015
    assert st["movements"] == 778 and st["joined_tls"] == 0
    # the per-record count is real, not a stub
    assert sum(r["unmapped_links"] for r in recs) == 12
    assert sum(r["mapped_columns"] for r in recs) == 1015
    assert {r["node"] for r in recs} <= set(range(len(nodes)))
    assert len({r["node"] for r in recs}) == 98        # one graph junction each, no collision


def test_every_imported_movement_answers_with_a_colour(imported):
    nodes, _edges, info = imported
    recs = info["signal_programs"]
    plan = S.SignalPlan.from_records(recs, nodes)
    assert plan.stats == {"programs": 98, "records": 98, "skipped_records": 0,
                          "movements": 778, "approaches": 330, "collisions": 0}
    assert plan.collisions == []
    hist = {}
    for r in recs:
        xy = tuple(nodes[r["node"]])
        for a, b, _links in r["movements"]:
            frm, to = tuple(nodes[a]), tuple(nodes[b])
            assert plan.colour(xy, frm, to, 0.0) is not None
            for t in range(90):
                hist[plan.char(xy, frm, to, float(t))] = \
                    hist.get(plan.char(xy, frm, to, float(t)), 0) + 1
    n = sum(hist.values())
    assert n == 778 * 90 and set(hist) == {"G", "g", "y", "r"}
    # THE REALISM DELTA. `roads.node_phase` + `run._light_green` give every movement a flat
    # 50 % green / 50 % red with no yellow at all; the real programs give this:
    share = {k: round(v / n, 4) for k, v in hist.items()}
    assert share == {"G": 0.34, "g": 0.0845, "y": 0.043, "r": 0.5324}
    assert 0.4 < share["G"] + share["g"] < 0.44        # green share, not the toy model's 0.50


def test_a_signalised_import_is_still_byte_identical_with_signals_off():
    on_nodes, on_edges, on_info = netimport.import_net(NET, signals=True, strong=True,
                                                       undirected_shapes=True)
    off_nodes, off_edges, off_info = netimport.import_net(NET, strong=True,
                                                          undirected_shapes=True)
    assert on_nodes == off_nodes and on_edges == off_edges
    assert "signal_programs" not in off_info and "signal_program_stats" not in off_info
    for k in off_info:
        assert on_info[k] == off_info[k], k
