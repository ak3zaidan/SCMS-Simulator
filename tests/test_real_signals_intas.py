"""`real_signals` driven by the REAL Ingolstadt programs, through the engine's own config path.

`tests/test_signals_intas.py` grades the IMPORT against the .net.xml. This file grades what the
ENGINE does with it: that `PipelineConfig(road_network="sumo", real_signals=True)` places all 98
programs on the graph, that a vehicle is told the colour of its own movement out of the state string
the file ships, and that the four characters stay four characters.

The scenario directory is generated and gitignored, so everything here SKIPS when it is absent;
`tests/test_real_signals.py` covers the same wiring hermetically and always runs.
"""
from __future__ import annotations

import json
import os

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.mock_pipeline import signals as S

pytest.importorskip("sumolib", reason="sumolib (SUMO tools) not installed")

NET = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "scms-sim", "scenarios", "gen_intas_urban_low", "sumo", "ingolstadt.net.xml")
pytestmark = pytest.mark.skipif(not os.path.exists(NET),
                                reason=f"InTAS scenario not generated ({NET})")

BASE = dict(seed=42, traffic_flow=True, road_network="sumo", sumo_net=NET,
            duration_s=90.0, dt=0.5, arrival_rate=2.0, attacker_pct=0.2,
            car_following=True, emit_sample_prob=0.0, verbose=False)


@pytest.fixture(scope="module")
def run_with_signals(tmp_path_factory):
    """One 90 s InTAS run with the real programs on, plus every colour it served."""
    rows: list[dict] = []
    runmod.SIGNAL_HOOK = rows.append
    try:
        res = run_pipeline(PipelineConfig(
            out_dir=str(tmp_path_factory.mktemp("intas_real")), real_signals=True, **BASE))
    finally:
        runmod.SIGNAL_HOOK = None
    man = json.load(open(os.path.join(res.out_dir, "manifest.json"), encoding="utf-8"))
    return res, rows, man


def test_all_98_programs_land_on_the_engines_graph(run_with_signals):
    """The end-to-end number `signals.py` documents, reached from a CONFIG rather than from a
    notebook: 98 programs, 330 approaches, 778 movements, 0 coordinate collisions."""
    _res, _rows, man = run_with_signals
    plan = man["counts"]["signal_service"]["plan"]
    assert plan == {"programs": 98, "records": 98, "skipped_records": 0,
                    "movements": 778, "approaches": 330, "collisions": 0}


def test_the_four_characters_stay_four_characters(run_with_signals):
    """G / g / y / r are all served, and the two distinctions that matter are visible in the run:
    permissive green is not protected green, and yellow is not red."""
    _res, rows, man = run_with_signals
    served = man["counts"]["signal_service"]
    for ch in ("G", "g", "y", "r"):
        assert served[ch] > 0, f"{ch!r} was never served ({served})"
    assert {r["char"] for r in rows} >= {"G", "g", "y", "r"}
    # a permissive green is a green: it is never a stop of itself. (Whether it must GIVE WAY
    # depends on there being an opposing protected stream within the critical gap, which needs more
    # traffic than 90 s of arrivals puts on a 3,289-junction city -- `tests/test_real_signals.py`
    # forces that case on a single junction, and the 300 s InTAS measurement records 31 of them.)
    assert any(r["char"] == "g" and not r["stopped"] for r in rows)
    # yellow is a stop for a driver who can stop and a go for one who cannot
    assert any(r["char"] == "y" and r["stopped"] for r in rows)
    assert served["dilemma_go"] > 0
    # red is unconditional
    assert all(r["stopped"] for r in rows if r["char"] == "r")
    assert all(not r["stopped"] for r in rows if r["char"] == "G")


def test_every_colour_served_is_the_one_that_movements_own_link_index_names(run_with_signals):
    """The failure this whole layer exists to prevent, checked against the FILE.

    For every observed (junction, from, to, t) the character is re-derived from sumolib: find the
    controlled connection whose from-edge starts at `from` and whose to-edge ends at `to`, read its
    OWN `linkIndex`, and index the phase string in force at `t`. Nothing here goes through
    `SignalPlan`, so a mapping error inside it cannot confirm itself."""
    _res, rows, _man = run_with_signals
    assert rows, "no vehicle reached a signalised junction in 90 s"
    from scms_sim_ref.mock_pipeline import netimport
    net = netimport.read_net(NET, programs=True)
    tf, _p = netimport._transformer(net, None)
    conns = S.tls_connections(net)
    # phase durations + state strings straight off the file, not through `SignalProgram`
    raw = {}
    for tls in net.getTrafficLights():
        progs = tls.getPrograms()
        if progs:
            p = progs[sorted(progs)[0]]
            raw[tls.getID()] = (float(p.getOffset()),
                                [(float(ph.duration), ph.state) for ph in p.getPhases()])

    # junction coordinate (in the engine's frame) -> tls id
    node_tls: dict = {}
    # (tls, from-node coord, to-node coord) -> the link indices that movement is served by
    move_links: dict = {}
    for tls_id, cs in conns.items():
        for c in cs:
            node_tls[_r2(tf(*c.getFrom().getToNode().getCoord()))] = tls_id
            a = _r2(tf(*c.getFrom().getFromNode().getCoord()))
            b = _r2(tf(*c.getTo().getToNode().getCoord()))
            move_links.setdefault((tls_id, a, b), set()).add(c.getTLLinkIndex())

    def state_at(tls_id, t):
        """SUMO's own semantics, restated here: walk the cumulative durations of
        (t - offset) mod cycle on half-open intervals."""
        offset, phases = raw[tls_id]
        e = (t - offset) % sum(d for d, _s in phases)
        acc = 0.0
        for d, s in phases:
            acc += d
            if e < acc:
                return s
        return phases[-1][1]

    #: most-permissive-first over the raw characters -- a driver picks a lane, so a movement served
    #: by several links gets the best signal any of its lanes offers. Written out here rather than
    #: imported so the rule is stated twice, independently.
    rank = "GgsyuoOr"
    checked = 0
    for r in rows:
        if r["to"] is None:
            continue                       # the primary-movement degradation, not a mapping claim
        tls_id = node_tls.get(_r2(r["node"]))
        if tls_id is None:
            continue
        links = move_links.get((tls_id, _r2(r["frm"]), _r2(r["to"])))
        if not links:
            # the engine's graph merges some SUMO edges (curve chords, node dedupe), so a movement
            # it names need not correspond to one connection pair in the file. Only exact matches
            # are graded; `checked` below is what keeps that from silently emptying the test.
            continue
        state = state_at(tls_id, r["t"])
        best = min((rank.index(state[li]), state[li]) for li in sorted(links))[1]
        assert r["char"] == best, (
            f"{tls_id} {r['frm']}->{r['to']} at t={r['t']}: engine says {r['char']!r}, the "
            f"<tlLogic> state {state!r} at link(s) {sorted(links)} says {best!r}")
        checked += 1
    assert checked >= 200, f"only {checked} movements could be matched against the file"


def _r2(p):
    # 1 dp: `netimport.net_to_network` rounds junction coordinates to a decimetre on the way into
    # the document, so a 2-dp SUMO coordinate never compares equal to the graph node it became.
    return (round(float(p[0]), 1), round(float(p[1]), 1))


def test_the_engine_reproduces_the_documented_colour_budget(run_with_signals):
    """What the programs actually SERVE a driver, as against `signals.py`'s uniform sampling over
    movements (G 34.0 / g 8.45 / y 4.30 / r 53.24 %). A vehicle-step budget is not a movement
    budget and must not be: a driver held at a red is counted once per step, so red dominates."""
    _res, _rows, man = run_with_signals
    served = man["counts"]["signal_service"]
    tot = sum(served[c] for c in ("G", "g", "y", "r"))
    assert tot > 500, tot
    red = served["r"] / tot
    green = (served["G"] + served["g"]) / tot
    assert 0.5 < red < 0.95, red             # queueing makes red over-represented, as it must
    assert green > 0.05, green
    assert 0.0 < served["y"] / tot < 0.10, served["y"] / tot


def test_junctions_with_no_program_keep_the_unsignalised_behaviour(run_with_signals):
    """98 of InTAS's 3,289 graph junctions carry a program. With `traffic_lights` off the other
    3,191 must stay exactly what they were -- uncontrolled -- and the counter says so."""
    _res, _rows, man = run_with_signals
    served = man["counts"]["signal_service"]
    assert served["toy_red"] == 0 and served["toy_green"] == 0
    assert served["none"] > served["r"], (served["none"], served["r"])
