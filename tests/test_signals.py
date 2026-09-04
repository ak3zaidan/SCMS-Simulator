"""REAL traffic-signal programs: `mock_pipeline/signals.py`.

Everything here is hermetic. Two kinds of ground truth are used and neither is invented:

  * a 4-arm fixture net built once by **netconvert 1.25.0** and embedded verbatim -- so its
    ``<request ... foes=...>`` matrix is netconvert's own geometry, not something written to make a
    test pass. `NET_XML_GAP` is the same net with the InTAS defect injected: the controller reserves
    a state column for a pedestrian group, so ``linkIndex`` skips 6 and the string is one wider than
    the connection count;
  * two REAL InTAS programs, quoted character-for-character out of
    ``gen_intas_urban_low/sumo/ingolstadt.net.xml`` together with the link indices and foe pairs of
    their junctions. `tests/test_signals_intas.py` re-derives those same constants from the file
    itself, so they cannot rot silently -- it skips when the (gitignored) scenario is absent, and
    this file still runs.

The three questions this file exists to answer: does a movement get ITS OWN light (the index
mapping), is yellow a state of its own, and can two movements that physically cross ever both be
told green.
"""
from __future__ import annotations

import math

import pytest

from scms_sim_ref.mock_pipeline import signals as S
from scms_sim_ref.mock_pipeline.roads import CustomNetwork, Trip

sumolib = pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")


# --------------------------------------------------------------------------- #
# Fixture net: J is a 4-arm signalised junction, arms N/E/S/W at 200 m.
# Built by `netconvert --node-files --edge-files --no-turnarounds --no-internal-links`
# (SUMO 1.25.0) and pasted unedited apart from shortened junction shapes: the tlLogic,
# the 12 <connection linkIndex=...> and the 12 <request foes=...> rows are netconvert's.
# --------------------------------------------------------------------------- #
_EDGES = """
    <edge id="EJ" from="E" to="J" priority="-1">
        <lane id="EJ_0" index="0" speed="13.89" length="200.00" shape="400.00,201.60 207.20,201.60"/>
    </edge>
    <edge id="JE" from="J" to="E" priority="-1">
        <lane id="JE_0" index="0" speed="13.89" length="200.00" shape="207.20,198.40 400.00,198.40"/>
    </edge>
    <edge id="JN" from="J" to="N" priority="-1">
        <lane id="JN_0" index="0" speed="13.89" length="200.00" shape="201.60,207.20 201.60,400.00"/>
    </edge>
    <edge id="JS" from="J" to="S" priority="-1">
        <lane id="JS_0" index="0" speed="13.89" length="200.00" shape="198.40,192.80 198.40,0.00"/>
    </edge>
    <edge id="JW" from="J" to="W" priority="-1">
        <lane id="JW_0" index="0" speed="13.89" length="200.00" shape="192.80,201.60 0.00,201.60"/>
    </edge>
    <edge id="NJ" from="N" to="J" priority="-1">
        <lane id="NJ_0" index="0" speed="13.89" length="200.00" shape="198.40,400.00 198.40,207.20"/>
    </edge>
    <edge id="SJ" from="S" to="J" priority="-1">
        <lane id="SJ_0" index="0" speed="13.89" length="200.00" shape="201.60,0.00 201.60,192.80"/>
    </edge>
    <edge id="WJ" from="W" to="J" priority="-1">
        <lane id="WJ_0" index="0" speed="13.89" length="200.00" shape="0.00,198.40 192.80,198.40"/>
    </edge>
"""
# netconvert's own foe matrix for J. Row i is read MSB-first: areFoes(i, k) is bit [len - k - 1].
_REQUESTS = """        <request index="0"  response="000000000000" foes="000100010000"/>
        <request index="1"  response="000000000000" foes="111100110000"/>
        <request index="2"  response="000011000000" foes="110011110000"/>
        <request index="3"  response="000010000000" foes="100010000000"/>
        <request index="4"  response="000110000111" foes="100110000111"/>
        <request index="5"  response="011110000110" foes="011110000110"/>
        <request index="6"  response="000000000000" foes="010000000100"/>
        <request index="7"  response="000000000000" foes="110000111100"/>
        <request index="8"  response="000000000011" foes="110000110011"/>
        <request index="9"  response="000000000010" foes="000000100010"/>
        <request index="10" response="000111000110" foes="000111100110"/>
        <request index="11" response="000110011110" foes="000110011110"/>
"""
_JUNCTIONS = """    <junction id="E" type="dead_end" x="400.00" y="200.00" incLanes="JE_0" intLanes="" shape="400.00,200.00 400.00,196.80"/>
    <junction id="J" type="traffic_light" x="200.00" y="200.00" incLanes="NJ_0 EJ_0 SJ_0 WJ_0" intLanes="" shape="196.80,207.20 203.20,207.20 207.20,203.20 207.20,196.80 203.20,192.80 196.80,192.80 192.80,196.80 192.80,203.20">
%s    </junction>
    <junction id="N" type="dead_end" x="200.00" y="400.00" incLanes="JN_0" intLanes="" shape="200.00,400.00 203.20,400.00"/>
    <junction id="S" type="dead_end" x="200.00" y="0.00" incLanes="JS_0" intLanes="" shape="200.00,0.00 196.80,0.00"/>
    <junction id="W" type="dead_end" x="0.00" y="200.00" incLanes="JW_0" intLanes="" shape="0.00,200.00 0.00,203.20"/>
""" % _REQUESTS


def _net(tl_body: str, link_index) -> str:
    """The fixture net with a given <tlLogic> body and a given linkIndex per connection."""
    conns = [("EJ", "JN", "r"), ("EJ", "JW", "s"), ("EJ", "JS", "l"),
             ("NJ", "JW", "r"), ("NJ", "JS", "s"), ("NJ", "JE", "l"),
             ("SJ", "JE", "r"), ("SJ", "JN", "s"), ("SJ", "JW", "l"),
             ("WJ", "JS", "r"), ("WJ", "JE", "s"), ("WJ", "JN", "l")]
    # netconvert emits them sorted by from-edge id, with linkIndex following the incLanes order
    # N(0,1,2) E(3,4,5) S(6,7,8) W(9,10,11)
    order = [3, 4, 5, 0, 1, 2, 6, 7, 8, 9, 10, 11]
    rows = "".join(
        '    <connection from="%s" to="%s" fromLane="0" toLane="0" tl="J" linkIndex="%d" '
        'dir="%s" state="o"/>\n' % (f, t, link_index[order[k]], d)
        for k, (f, t, d) in enumerate(conns))
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<net version="1.20" junctionCornerDetail="5" limitTurnSpeed="5.50">\n'
            '    <location netOffset="200.00,200.00" convBoundary="0.00,0.00,400.00,400.00"'
            ' origBoundary="-200.00,-200.00,200.00,200.00" projParameter="!"/>\n'
            + _EDGES
            + '    <tlLogic id="J" type="static" programID="0" offset="0">\n' + tl_body
            + '    </tlLogic>\n\n' + _JUNCTIONS + "\n" + rows + "</net>\n")


#: netconvert's own program for J: 12 links, 4 phases, cycle 90 s.
_TL_PLAIN = """        <phase duration="42" state="GGgrrrGGgrrr"/>
        <phase duration="3"  state="yyyrrryyyrrr"/>
        <phase duration="42" state="rrrGGgrrrGGg"/>
        <phase duration="3"  state="rrryyyrrryyy"/>
"""
#: the same program with a PEDESTRIAN COLUMN inserted at index 6 -- the InTAS
#: `cluster_13876325_281116670` shape. 13 columns, 12 vehicular connections, no linkIndex 6.
_TL_GAP = """        <phase duration="42" state="GGgrrrgGGgrrr"/>
        <phase duration="3"  state="yyyrrrgyyyrrr"/>
        <phase duration="42" state="rrrGGgGrrrGGg"/>
        <phase duration="3"  state="rrryyyyrrryyy"/>
"""
NET_XML = _net(_TL_PLAIN, list(range(12)))
NET_XML_GAP = _net(_TL_GAP, list(range(6)) + list(range(7, 13)))


@pytest.fixture()
def net_plain(tmp_path):
    p = tmp_path / "plain.net.xml"
    p.write_text(NET_XML, encoding="utf-8")
    return sumolib.net.readNet(str(p), withPrograms=True), str(p)


@pytest.fixture()
def net_gap(tmp_path):
    p = tmp_path / "gap.net.xml"
    p.write_text(NET_XML_GAP, encoding="utf-8")
    return sumolib.net.readNet(str(p), withPrograms=True), str(p)


# --------------------------------------------------------------------------- #
# REAL InTAS constants (see the module docstring; re-derived in test_signals_intas.py)
# --------------------------------------------------------------------------- #
#: ingolstadt.net.xml, <tlLogic id="1863241632" type="actuated" programID="0" offset="0">.
#: A T-junction: N-S main street (170018165) plus one arm east (224892361). 7 links:
#:   0 E->N right   1 E->S left   2 S->E right   3 S->N through
#:   4,5 N->S through (two lanes)  6 N->E left
INTAS_T_PHASES = [(35.0, "rrGGGGg", 7.0, 50.0), (5.0, "rryyyyg", -1.0, -1.0),
                  (6.0, "rrrrGGG", 7.0, 50.0), (5.0, "rrrryyy", -1.0, -1.0),
                  (34.0, "GGGrrrr", 10.0, 50.0), (5.0, "yyyrrrr", -1.0, -1.0)]
#: its junction's <request foes=...> matrix, translated to TL link indices
INTAS_T_FOES = [(0, 3), (1, 3), (1, 4), (1, 5), (1, 6), (2, 6), (3, 6)]

#: ingolstadt.net.xml, <tlLogic id="cluster_13876325_281116670" type="static" ...>. A 4-arm
#: junction whose state string is 14 wide but which has only 13 controlled connections: the file
#: carries linkIndex 0,1,2,3,4,5,7,8,9,10,11,12,13 and NO linkIndex 6.
INTAS_GAP_STATES = ["rrrGGGgrrrGGGr", "rrryyygrrryyyr", "rrrrrrGrrrrrrG", "rrrrrryrrrrrry",
                    "GGgrrrrGGgrrrr", "yygrrrryygrrrr", "rrGrrrrrrGrrrr", "rryrrrrrryrrrr"]
INTAS_GAP_LINKS = [0, 1, 2, 3, 4, 5, 7, 8, 9, 10, 11, 12, 13]


# --------------------------------------------------------------------------- #
# the character vocabulary
# --------------------------------------------------------------------------- #
def test_char_colour_covers_the_whole_sumo_vocabulary_and_defaults_to_red():
    assert [S.char_colour(c) for c in "Ggs"] == [S.GREEN] * 3
    assert [S.char_colour(c) for c in "yu"] == [S.YELLOW] * 2
    assert S.char_colour("r") == S.RED
    assert [S.char_colour(c) for c in "oO"] == [S.OFF] * 2
    # an unknown character must be the CONSERVATIVE reading, never an invented green
    for junk in ("", "x", "1", "\x00", "GG"):
        assert S.char_colour(junk) == S.RED


def test_permissive_and_protected_split_the_greens():
    greens = [c for c in "GgsyuroO" if S.char_colour(c) == S.GREEN]
    assert greens == ["G", "g", "s"]
    assert [S.is_protected(c) for c in greens] == [True, False, False]
    assert [S.is_permissive(c) for c in greens] == [False, True, True]
    assert not S.is_protected("g")                 # the whole point: 'g' must yield
    assert not any(S.is_protected(c) for c in "yur")


# --------------------------------------------------------------------------- #
# timing
# --------------------------------------------------------------------------- #
def test_phase_at_walks_half_open_intervals():
    p = S.SignalProgram("t", [(10.0, "G"), (5.0, "y"), (15.0, "r")])
    assert p.cycle_s == 30.0 and p.n_links == 1
    assert [p.phase_at(t) for t in (0.0, 9.999, 10.0, 14.999, 15.0, 29.999)] == [0, 0, 1, 1, 2, 2]
    assert p.phase_at(30.0) == 0 and p.phase_at(-1.0) == 2       # wraps both ways
    assert [p.state_at(t) for t in (0.0, 12.0, 20.0)] == ["G", "y", "r"]


def test_phase_at_subtracts_the_offset_as_sumo_1_25_0_does():
    """The sign was measured against SUMO itself: a 2/3/42/43 s program (cycle 90) with offset 50,
    driven through TraCI at 0.5 s steps, switched at 7.5 / 50.5 / 52.5 / 55.5 / 97.5 s -- the
    model's 7 / 50 / 52 / 55 / 97 plus one end-of-step read lag. `(t - offset)` matched 195/200
    samples, `(t + offset)` 134/200."""
    ph = [(2.0, "G"), (3.0, "y"), (42.0, "r"), (43.0, "G")]
    minus = S.SignalProgram("t", ph, offset=50.0)
    plus = S.SignalProgram("t", ph, offset=-50.0)                # what "+ offset" would mean
    boundaries = sorted({round((c + 50.0) % 90.0, 3) for c in (0.0, 2.0, 5.0, 47.0)})
    assert boundaries == [7.0, 50.0, 52.0, 55.0]
    switches = sorted({round(t, 3) for t in (k * 0.5 for k in range(180))
                       if minus.phase_at(t) != minus.phase_at(t - 0.5)})
    assert switches == boundaries
    assert minus.phase_at(50.0) == 0 and minus.phase_at(49.5) == 3
    assert plus.phase_at(50.0) != 0                              # the wrong sign disagrees
    assert sum(1 for k in range(200)
               if minus.phase_at(k * 0.5) == plus.phase_at(k * 0.5)) < 200


def test_program_rejects_input_it_cannot_represent():
    with pytest.raises(ValueError, match="no phases"):
        S.SignalProgram("t", [])
    with pytest.raises(ValueError, match="disagree on link count"):
        S.SignalProgram("t", [(10.0, "GG"), (5.0, "y")])
    with pytest.raises(ValueError, match="empty state"):
        S.SignalProgram("t", [(10.0, "")])
    with pytest.raises(ValueError, match="finite and >= 0"):
        S.SignalProgram("t", [(-1.0, "G")])
    with pytest.raises(ValueError, match="finite and >= 0"):
        S.SignalProgram("t", [(math.inf, "G")])
    with pytest.raises(ValueError, match="zero-length cycle"):
        S.SignalProgram("t", [(0.0, "G"), (0.0, "r")])


def test_char_at_outside_the_state_string_is_red_not_a_wrapped_neighbour():
    p = S.SignalProgram("t", [(10.0, "Gr")])
    assert (p.char_at(0.0, 0), p.char_at(0.0, 1)) == ("G", "r")
    assert p.char_at(0.0, 2) == "r" and p.char_at(0.0, -1) == "r" and p.char_at(0.0, 99) == "r"
    assert p.colour_at(0.0, 2) == S.RED


def test_char_of_takes_the_best_lane_and_keeps_protected_distinct_from_permissive():
    p = S.SignalProgram("t", [(10.0, "Ggyr")])
    assert p.char_of(0.0, (3,)) == "r" and p.char_of(0.0, (2, 3)) == "y"
    assert p.char_of(0.0, (1, 3)) == "g"          # a permissive lane and a red lane -> permissive
    assert p.char_of(0.0, (0, 1)) == "G"          # a protected lane wins over a permissive one
    assert p.char_of(0.0, ()) == "r"
    # colour_of is exactly char_colour(char_of), so the two can never disagree about the winner
    for links in ((3,), (2, 3), (1, 3), (0, 1), (0, 1, 2, 3)):
        assert p.colour_of(0.0, links) == S.char_colour(p.char_of(0.0, links))
    assert p.colour_of(0.0, (1, 3)) == S.GREEN and p.char_of(0.0, (1, 3)) == "g"


def test_program_facts_separate_declared_from_effective_actuation():
    fixed = S.SignalProgram("a", [(30.0, "G"), (5.0, "y"), (30.0, "r")], type="actuated")
    assert fixed.type == "actuated" and fixed.actuated is False   # typed, but no minDur/maxDur band
    assert fixed.cycle_bounds_s == (65.0, 65.0)
    act = S.SignalProgram("b", [(30.0, "G", 10.0, 50.0), (5.0, "y"), (30.0, "r", 10.0, 50.0)],
                          type="actuated")
    assert act.actuated is True and act.cycle_bounds_s == (25.0, 105.0)
    assert act.cycle_s == 65.0                    # the FIXED-TIME approximation we actually run
    st = S.program_stats([fixed, act])
    assert st["actuated_declared"] == 2 and st["actuated_effective"] == 1
    assert st["fixed_time_effective"] == 1 and st["programs_without_yellow"] == 0


def test_green_stages_counts_serving_phases_not_transitions():
    p = S.SignalProgram("t", INTAS_T_PHASES)
    assert len(p.durations) == 6 and p.green_stages() == 3      # 3 yellow phases are not stages
    assert p.cycle_s == 90.0 and p.n_links == 7


# --------------------------------------------------------------------------- #
# YELLOW IS A DISTINCT STATE (question 2)
# --------------------------------------------------------------------------- #
def test_yellow_is_neither_green_nor_red_and_a_real_program_never_skips_it():
    """On the real InTAS T-junction program no link ever steps from a green character straight to
    'r': every one of its green->non-green transitions is green->yellow. (Measured over the whole
    net in test_signals_intas.py: 0 of 1489.)"""
    p = S.SignalProgram("1863241632", INTAS_T_PHASES, type="actuated")
    assert S.char_colour("y") == S.YELLOW
    assert S.YELLOW not in (S.GREEN, S.RED, S.OFF)
    assert not S.is_protected("y") and not S.is_permissive("y")
    n = len(p.states)
    seen_yellow = 0
    for li in range(p.n_links):
        seq = [p.states[k][li] for k in range(n)]
        assert not (set(seq) <= {"r"}), f"link {li} is never anything but red"
        for k in range(n):
            a, b = seq[k], seq[(k + 1) % n]
            if a in "Gg":
                assert b != "r", f"link {li} phase {k}: green -> red with no yellow"
                seen_yellow += (b == "y")
    assert seen_yellow >= p.n_links                 # every link really does show a yellow
    # and yellow occupies real time: 15 s of the 90 s cycle is a yellow phase
    assert sum(d for d, s in zip(p.durations, p.states) if "y" in s) == 15.0


# --------------------------------------------------------------------------- #
# CONFLICTING MOVEMENTS ARE NEVER BOTH GREEN (question 3)
# --------------------------------------------------------------------------- #
def test_real_junction_never_gives_two_conflicting_movements_a_protected_green():
    """The pair is constructed from the REAL program, not invented: `INTAS_T_FOES` is junction
    1863241632's own `<request foes=...>` matrix in TL link indices. Link 1 is the east arm's LEFT
    turn across the main street; link 3 is the southern approach's THROUGH movement. They cross."""
    p = S.SignalProgram("1863241632", INTAS_T_PHASES, type="actuated")
    assert (1, 3) in INTAS_T_FOES
    for a, b in INTAS_T_FOES:
        for k, st in enumerate(p.states):
            assert not (st[a] == "G" and st[b] == "G"), \
                f"phase {k} {st!r} gives conflicting links {a},{b} both a protected green"
    # NOT vacuous: both members of the pair really are served, in different phases
    green1 = {k for k, st in enumerate(p.states) if st[1] == "G"}
    green3 = {k for k, st in enumerate(p.states) if st[3] == "G"}
    assert green1 == {4} and green3 == {0} and not (green1 & green3)
    # and at no instant of the cycle are both green -- checked as COLOURS, through the plan API
    plan = S.SignalPlan()
    E, J, Nn, Sth = (100.0, 0.0), (0.0, 0.0), (0.0, 100.0), (0.0, -100.0)
    plan.add(J, p, movements=[(E, Sth, [1]), (Sth, Nn, [3])])
    both = [t for t in range(90)
            if plan.colour(J, E, Sth, float(t)) == S.GREEN
            and plan.colour(J, Sth, Nn, float(t)) == S.GREEN]
    assert both == []
    assert sum(1 for t in range(90) if plan.colour(J, E, Sth, float(t)) == S.GREEN) == 34
    assert sum(1 for t in range(90) if plan.colour(J, Sth, Nn, float(t)) == S.GREEN) == 35


def test_permissive_green_is_allowed_to_share_a_phase_with_a_conflicting_protected_green():
    """Phase 0 of the same junction is `rrGGGGg`: link 6 (the north arm's LEFT) is 'g' while links
    2 and 3 -- both of which it crosses -- are 'G'. That is correct and is why 'g' must not be
    flattened into 'G': the audit only ever counts G-G, and a caller that reads the character knows
    it has to yield."""
    p = S.SignalProgram("1863241632", INTAS_T_PHASES)
    assert p.states[0][6] == "g" and p.states[0][2] == "G" and p.states[0][3] == "G"
    assert (2, 6) in INTAS_T_FOES and (3, 6) in INTAS_T_FOES
    assert S.char_colour("g") == S.GREEN and S.is_permissive("g") and not S.is_protected("g")
    # ... and when it finally gets its own protected stage, both its foes are red
    assert p.states[2] == "rrrrGGG"
    for a, b in INTAS_T_FOES:
        if 6 in (a, b):
            other = a if b == 6 else b
            assert p.states[2][other] == "r"


# --------------------------------------------------------------------------- #
# THE INDEX MAPPING (question 1)
# --------------------------------------------------------------------------- #
def test_intas_gap_junction_would_be_silently_wrong_under_dense_renumbering():
    """`cluster_13876325_281116670` verbatim. Reading each connection's own linkIndex, the west
    arm's LEFT turn (link 13) is red during the east-west through stage. Renumber the 13 connections
    densely 0..12 -- what you get if you size the state by the connection count -- and it reads
    column 12 instead, a PROTECTED green, straight into links 4 and 5."""
    p = S.SignalProgram("cluster_13876325_281116670", [(1.0, s) for s in INTAS_GAP_STATES])
    assert p.n_links == 14 and len(INTAS_GAP_LINKS) == 13 and 6 not in INTAS_GAP_LINKS
    dense = {li: k for k, li in enumerate(INTAS_GAP_LINKS)}
    assert dense[13] == 12 and dense[5] == 5      # only links above the gap move
    phase0 = INTAS_GAP_STATES[0]
    assert p.char_at(0.0, 13) == "r"              # truth: the left turn is held
    assert phase0[dense[13]] == "G"               # the bug: it is given right of way
    assert phase0[4] == "G" and phase0[5] == "G"  # ... while the movements it crosses run
    # 7 of the 13 connections read the wrong column, and only those above the gap
    assert sorted(li for li in INTAS_GAP_LINKS if dense[li] != li) == [7, 8, 9, 10, 11, 12, 13]


def test_dense_link_indices_is_the_identity_without_a_gap(net_plain, net_gap):
    plain, _ = net_plain
    gap, _ = net_gap
    cp, cg = S.tls_connections(plain), S.tls_connections(gap)
    assert S.dense_link_indices(cp)["J"] == {i: i for i in range(12)}
    assert S.dense_link_indices(cg)["J"] == dict(zip(list(range(6)) + list(range(7, 13)),
                                                     range(12)))


def test_tls_connections_reads_link_index_from_the_connection(net_gap):
    net, _ = net_gap
    conns = S.tls_connections(net)
    assert set(conns) == {"J"} and len(conns["J"]) == 12
    got = {(c.getFrom().getID(), c.getTo().getID()): c.getTLLinkIndex() for c in conns["J"]}
    assert got == {("NJ", "JW"): 0, ("NJ", "JS"): 1, ("NJ", "JE"): 2,
                   ("EJ", "JN"): 3, ("EJ", "JW"): 4, ("EJ", "JS"): 5,
                   ("SJ", "JE"): 7, ("SJ", "JN"): 8, ("SJ", "JW"): 9,
                   ("WJ", "JS"): 10, ("WJ", "JE"): 11, ("WJ", "JN"): 12}
    prog = S.programs_from_net(net)["J"]
    assert prog.n_links == 13 and len(conns["J"]) == 12      # one column has no connection
    assert max(got.values()) == 12 and 6 not in set(got.values())


def test_audit_is_clean_at_shift_zero_and_lights_up_when_shifted(net_plain):
    """One 12-link symmetric cross is a WEAK version of this alarm and the numbers say so: the
    junction has a 3-link rotational symmetry, so +1 and +3 happen to land conflict-free while -1
    gives 4 and +2 gives 2. What is unambiguous even here is shift 0 -- 0 conflicts over all 12
    pairs with nothing out of range. The alarm's real magnitude is measured on the 98 InTAS
    programs (11 -> 319 / 456), in test_signals_intas.py."""
    net, _ = net_plain
    rows = {r["shift"]: r for r in S.audit_link_indices(net, shifts=(-2, -1, 0, 1, 2))}
    assert rows[0] == {"mapping": "linkIndex", "shift": 0, "conflicts": 0, "pairs": 12,
                       "junctions": 0, "out_of_range": 0}
    assert rows[-1]["conflicts"] == 4 and rows[-1]["junctions"] == 1
    assert rows[2]["conflicts"] == 2 and rows[2]["junctions"] == 1
    # a shift also pushes links off the end of the string, which is itself evidence of a bad mapping
    assert rows[-1]["out_of_range"] == 4 and rows[2]["out_of_range"] == 8


def test_audit_dense_mode_catches_the_pedestrian_column(net_plain, net_gap):
    plain, _ = net_plain
    gap, _ = net_gap
    # no gap -> dense is the identity -> the audit cannot tell the difference
    assert (S.audit_link_indices(plain, dense=True)[0]["conflicts"]
            == S.audit_link_indices(plain)[0]["conflicts"] == 0)
    true_row = S.audit_link_indices(gap)[0]
    dense_row = S.audit_link_indices(gap, dense=True, detail=True)[0]
    assert true_row["conflicts"] == 0 and true_row["out_of_range"] == 0, \
        "the real mapping must be clean"
    assert dense_row["conflicts"] == 5 and dense_row["junctions"] == 1
    assert dense_row["mapping"] == "dense" and dense_row["out_of_range"] == 0
    assert {(r[3], r[4]) for r in dense_row["detail"]} == {(0, 9), (1, 9), (3, 12), (4, 12),
                                                           (7, 11)}
    # every one of those pairs really is a physical conflict at this junction
    pairs = set(S.foe_pairs(gap, "J"))
    assert {(r[3], r[4]) for r in dense_row["detail"]} <= pairs
    # and every shift of the gap net is caught too
    shifted = {r["shift"]: r["conflicts"] for r in S.audit_link_indices(gap, shifts=(-2, -1, 1, 2))}
    assert shifted == {-2: 2, -1: 5, 1: 3, 2: 6}


def test_foe_pairs_are_symmetric_ordered_and_match_the_audit(net_plain):
    net, _ = net_plain
    pairs = S.foe_pairs(net, "J")
    assert pairs and all(a < b for a, b in pairs) and len(set(pairs)) == len(pairs)
    prog = S.programs_from_net(net)["J"]
    both = [(a, b, k) for a, b in pairs for k, st in enumerate(prog.states)
            if st[a] == "G" and st[b] == "G"]
    assert both == []                                    # agrees with audit shift 0 -> 0 conflicts
    # the pair set is non-trivial: opposing throughs are NOT foes, crossing ones are
    assert (1, 7) not in pairs                           # N through vs S through
    assert (1, 4) in pairs                               # N through vs E through
    assert S.foe_pairs(net, "nosuchtls") == []


# --------------------------------------------------------------------------- #
# extract / SignalPlan
# --------------------------------------------------------------------------- #
def _plan_from_fixture(net):
    """Run `extract` with the identity index closures (the fixture graph is the SUMO graph)."""
    order = ["J", "N", "E", "S", "W"]
    idx = {n: i for i, n in enumerate(order)}
    pts = [(200.0, 200.0), (200.0, 400.0), (400.0, 200.0), (200.0, 0.0), (0.0, 200.0)]
    ends = {"NJ": (1, 0), "JN": (0, 1), "EJ": (2, 0), "JE": (0, 2),
            "SJ": (3, 0), "JS": (0, 3), "WJ": (4, 0), "JW": (0, 4)}
    recs, st = S.extract(net, idx.get, ends.get)
    return recs, st, S.SignalPlan.from_records(recs, pts), pts


def test_extract_builds_movements_and_a_primary_per_approach(net_plain):
    net, _ = net_plain
    recs, st, plan, pts = _plan_from_fixture(net)
    assert len(recs) == 1 and recs[0]["node"] == 0 and recs[0]["tls"] == "J"
    assert st["junctions"] == 1 and st["joined_tls"] == 0 and st["links_total"] == 12
    assert st["links_mapped"] == 12 and st["links_unmapped"] == 0 and st["links_self_loop"] == 0
    assert st["state_columns"] == 12 and st["state_columns_mapped"] == 12
    assert st["approaches_merged"] == 0
    assert recs[0]["unmapped_links"] == 0 and recs[0]["mapped_columns"] == 12
    assert len(recs[0]["movements"]) == 12 and len(recs[0]["primary"]) == 4
    # the primary of each approach is its STRAIGHT movement (dir="s"), never its lowest index
    prim = dict(recs[0]["primary"])
    assert prim == {1: 1, 2: 4, 3: 7, 4: 10}
    assert plan.stats == {"programs": 1, "records": 1, "skipped_records": 0,
                          "movements": 12, "approaches": 4, "collisions": 0}


def test_plan_answers_the_movement_and_falls_back_to_the_primary(net_plain):
    net, _ = net_plain
    _recs, _st, plan, pts = _plan_from_fixture(net)
    J, N, E, Sn, W = pts
    # phase 0 is 'GGgrrrGGgrrr': the N and S arms run, E and W are red
    assert plan.char(J, N, Sn, 0.0) == "G"          # N through
    assert plan.char(J, N, E, 0.0) == "g"           # N left  -- permissive, must yield
    assert plan.colour(J, N, E, 0.0) == S.GREEN and S.is_permissive(plan.char(J, N, E, 0.0))
    assert plan.char(J, E, W, 0.0) == "r"           # E through
    assert plan.colour(J, E, W, 45.0) == S.GREEN    # phase 2, the other stage
    # approach only -> the approach's PRIMARY (straight) movement, not a union of every exit
    assert plan.char(J, E, None, 0.0) == "r"
    assert plan.links(J, E, None) == (4,)
    # an exit the program does not control degrades to the primary rather than inventing a colour
    assert plan.links(J, E, (999.0, 999.0)) == (4,)
    # an unknown junction / approach is None, which is what keeps every other map unchanged
    assert plan.colour((1.0, 1.0), N, Sn, 0.0) is None
    assert plan.char(J, (1.0, 1.0), Sn, 0.0) is None
    assert plan.has(J) and not plan.has((1.0, 1.0))
    assert plan.program(J).tls_id == "J" and plan.program((1.0, 1.0)) is None


def test_plan_records_round_trip_through_the_on_disk_schema(net_plain):
    net, _ = net_plain
    recs, _st, plan, pts = _plan_from_fixture(net)
    import json
    again = S.SignalPlan.from_records(json.loads(json.dumps(recs)), pts)
    for t in (0.0, 21.5, 44.9, 45.0, 89.9):
        for a, b, _links in recs[0]["movements"]:
            assert plan.char(pts[0], pts[a], pts[b], t) == again.char(pts[0], pts[a], pts[b], t)
    assert again.stats["skipped_records"] == 0
    # a record the document cannot resolve is SKIPPED, never guessed at
    broken = [dict(recs[0], node=999), dict(recs[0], phases=[]), {"node": 0}]
    assert S.SignalPlan.from_records(broken, pts).stats["skipped_records"] == 3


def test_two_programs_on_one_coordinate_are_recorded_not_silently_overwritten():
    a = S.SignalProgram("a", [(10.0, "G"), (10.0, "r")])
    b = S.SignalProgram("b", [(10.0, "r"), (10.0, "G")])
    plan = S.SignalPlan()
    J, F, T = (0.0, 0.0), (100.0, 0.0), (0.0, 100.0)
    plan.add(J, a, movements=[(F, T, [0])])
    plan.add(J, b, movements=[(F, T, [0])])
    assert plan.collisions == [(J, "b", "a")]
    assert plan.program(J) is a                     # first wins, deterministically
    assert plan.char(J, F, T, 0.0) == "G"


def test_program_stats_reports_what_the_fixed_time_approximation_hides(net_plain):
    net, _ = net_plain
    st = S.program_stats(S.programs_from_net(net))
    assert st["programs"] == 1 and st["cycle_s"]["median"] == 90.0
    assert st["phases_hist"] == {4: 1} and st["green_stages_hist"] == {2: 1}
    assert st["state_char_hist"] == {"G": 8, "g": 4, "r": 24, "y": 12}
    assert sum(st["state_char_hist"].values()) == 4 * 12          # phases x links
    assert st["programs_without_yellow"] == 0 and st["offsets_nonzero"] == 0
    assert st["actuated_declared"] == 0 and st["actuated_effective"] == 0
    assert st["actuated_cycle_band_s"] == {}
    assert S.program_stats([]) == {"programs": 0}


# --------------------------------------------------------------------------- #
# netimport: opt-in, and OFF is byte-identical
# --------------------------------------------------------------------------- #
def test_netimport_signals_layer_is_opt_in_and_off_changes_nothing(net_plain):
    from scms_sim_ref.mock_pipeline import netimport
    from scms_sim_ref.mock_pipeline.osm import network_document
    _net, path = net_plain
    off_nodes, off_edges, off_info = netimport.import_net(path)
    on_nodes, on_edges, on_info = netimport.import_net(path, signals=True)
    assert off_nodes == on_nodes and off_edges == on_edges
    assert "signal_programs" not in off_info and "signal_program_stats" not in off_info
    assert off_info["signal_nodes"] == on_info["signal_nodes"]
    for k in off_info:
        if k not in ("signal_programs", "signal_program_stats"):
            assert off_info[k] == on_info[k], k
    # the document a consumer reads is byte-identical without the layer
    import json
    doc_off = netimport.signal_document(off_nodes, off_edges, off_info)
    assert "signal_programs" not in doc_off
    doc_on = netimport.signal_document(on_nodes, on_edges, on_info)
    assert doc_on["signal_programs"] == on_info["signal_programs"]
    assert json.dumps({k: v for k, v in doc_on.items() if k != "signal_programs"},
                      sort_keys=True) == json.dumps(doc_off, sort_keys=True)


def test_netimport_refuses_a_net_read_without_programs(net_plain, tmp_path):
    from scms_sim_ref.mock_pipeline import netimport
    _net, path = net_plain
    plain = netimport.read_net(path)                     # programs=False -> sumolib drops them
    assert S.programs_from_net(plain) == {}
    with pytest.raises(ValueError, match="withPrograms"):
        netimport.net_to_network(plain, signals=True)


def test_netimport_signal_layer_reaches_a_working_plan(net_plain):
    from scms_sim_ref.mock_pipeline import netimport
    _net, path = net_plain
    nodes, _edges, info = netimport.import_net(path, signals=True)
    recs = info["signal_programs"]
    assert len(recs) == 1 and info["signal_program_stats"]["signal_nodes_with_program"] == 1
    assert info["signal_program_stats"]["coverage_of_signal_nodes"] == 1.0
    plan = S.SignalPlan.from_records(recs, nodes)
    J = tuple(nodes[recs[0]["node"]])
    assert plan.has(J)
    seen = {plan.char(J, tuple(nodes[a]), tuple(nodes[b]), float(t))
            for a, b, _ in recs[0]["movements"] for t in range(0, 90, 3)}
    assert seen == {"G", "g", "y", "r"}


# --------------------------------------------------------------------------- #
# the roads.py seam
# --------------------------------------------------------------------------- #
def test_next_movement_names_the_junction_and_both_of_its_neighbours():
    wp = [(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]
    trip = Trip(wp, 10.0, 0.0)
    assert trip.next_movement(0.0) == ((100.0, 0.0), 100.0, (0.0, 0.0), (100.0, 100.0))
    node, d, frm, to = trip.next_movement(150.0)
    assert node == (100.0, 100.0) and to is None and frm == (100.0, 0.0)
    assert d == pytest.approx(50.0)
    assert trip.next_movement(250.0) == (None, math.inf, None, None)
    # with `nodes` (directed carriageways / shape polylines) the CURVE VERTICES are skipped and the
    # true junction coordinates are returned -- the same ones SignalPlan is keyed on
    driven = [(0.0, 2.0), (50.0, 7.0), (100.0, 2.0), (102.0, 50.0), (102.0, 100.0)]
    nodes = [(0.0, 0.0), None, (100.0, 0.0), None, (100.0, 100.0)]
    t2 = Trip(driven, 10.0, 0.0, nodes=nodes)
    node, _d, frm, to = t2.next_movement(0.0)
    assert (node, frm, to) == ((100.0, 0.0), (0.0, 0.0), (100.0, 100.0))
    assert t2.next_node(0.0)[0] == node               # agrees with the existing junction lookup


def test_every_topology_answers_the_signal_query_and_defaults_to_none():
    """The surface lives on `_LaneFrameMixin`, so a caller never needs a getattr and a grid or ring
    map -- which has no real programs, ever -- answers None instead of raising."""
    from scms_sim_ref.mock_pipeline.roads import GridNetwork, RingNetwork
    for net in (GridNetwork(3, 3, 100.0), RingNetwork(6, 100.0),
                CustomNetwork([[0.0, 0.0], [100.0, 0.0]], [[0, 1]])):
        assert net.signal_plan is None
        assert net.signal_char((0.0, 0.0), (1.0, 1.0), (2.0, 2.0), 0.0) is None
        assert net.signal_colour((0.0, 0.0)) is None
        assert "signal_plan" not in vars(net)          # a class default: nothing allocated


def test_signal_plan_on_a_network_is_opt_in_and_answers_by_movement():
    nodes = [[0.0, 0.0], [100.0, 0.0], [0.0, 100.0], [-100.0, 0.0]]
    net = CustomNetwork(nodes, [[0, 1], [0, 2], [0, 3]])
    J, E, N, W = (0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (-100.0, 0.0)
    assert net.signal_plan is None
    assert net.signal_colour(J, E, N, 0.0) is None and net.signal_char(J, E, N, 0.0) is None
    assert net.signal_char(None, E, N, 0.0) is None
    prog = S.SignalProgram("J", [(30.0, "Gr"), (5.0, "yr"), (30.0, "rG"), (5.0, "ry")])
    plan = S.SignalPlan()
    plan.add(J, prog, movements=[(E, N, [0]), (E, W, [1])], primary={E: 0})
    stats = net.set_signal_plan(plan)
    assert stats == {} or isinstance(stats, dict)
    # the SAME approach, two exits, two different colours -- which the 2-colouring cannot express
    assert net.signal_char(J, E, N, 0.0) == "G" and net.signal_char(J, E, W, 0.0) == "r"
    assert net.signal_char(J, E, N, 40.0) == "r" and net.signal_char(J, E, W, 40.0) == "G"
    assert net.signal_colour(J, E, N, 32.0) == S.YELLOW
    assert net.signal_char(J, N, E, 0.0) is None       # an approach the program does not control
    assert net.signal_colour((7.0, 7.0), E, N, 0.0) is None
    # node_phase is untouched by any of this
    assert net.node_phase(J) in (0, 1)
    assert net.set_signal_plan(None) == {} and net.signal_char(J, E, N, 0.0) is None
