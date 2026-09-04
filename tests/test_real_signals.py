"""`real_signals`: the imported `<tlLogic>` programs, wired into the engine and graded on what they
actually serve a vehicle.

`signals.py` and `tests/test_signals*.py` prove the IMPORT is right -- the programs, the timing, and
above all the connection -> phase-index mapping (`audit_link_indices` gives 11 conflicting protected
pairs at shift 0 against 319 at +1 and 456 at -1, a 29-41x alarm). None of that was reachable from a
config: `run.py` called `net.node_phase` and nothing else, so every junction on every map ran one
24 s two-phase cycle with no yellow, no turn phase and no idea which way the vehicle was going.

This file grades the WIRING, and it does so on a hand-built map whose right answer is arithmetic
rather than on a city whose right answer is another copy of the same code:

  * `STAR` is a 4-arm junction whose program has TEN state columns for EIGHT signal groups -- each
    approach gets a through/right group and a LEFT group, so the MOVEMENT and not the approach
    decides the colour, and columns 2 and 7 are pedestrian groups no vehicular connection addresses.
    That is exactly the shape of InTAS's `cluster_13876325_281116670` (14 columns, 13 connections,
    no linkIndex 6). Its phase strings are chosen so that DENSE RENUMBERING -- the plausible bug,
    where you size the state by the connection count and hand out indices in order -- gives a
    DIFFERENT character for the movements above each gap. A test that only checked "some vehicle saw
    a red" would pass with the mapping wrong; these check the character against the column the
    movement's own `linkIndex` names, and assert that the dense mapping really would have differed.
  * the same map with an all-'G' program, an all-'r' program and a program with NO movements gives
    three exact digest identities: green-everywhere reproduces the unsignalised run byte for byte,
    no-movements reproduces the toy-signal run byte for byte, and red-everywhere does not reproduce
    either. That is what "a junction with no program keeps today's behaviour" and "no new rng on the
    default path" mean as measurements rather than as claims.

`tests/test_real_signals_intas.py` runs the same wiring against the real Ingolstadt programs.
"""
from __future__ import annotations

import json

import pytest

from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import run as runmod
from scms_sim_ref.mock_pipeline import signals as SIG
from scms_sim_ref.mock_pipeline.run import validate_config

# --------------------------------------------------------------------------- the map ------------ #
# node 0 = the junction; 1..4 = the N, E, S, W arms at 160 m.
STAR_NODES = [[0.0, 0.0], [0.0, 160.0], [160.0, 0.0], [0.0, -160.0], [-160.0, 0.0]]
STAR_EDGES = [[0, 1], [0, 2], [0, 3], [0, 4]]
NN, EE, SS, WW = 1, 2, 3, 4

#: movement -> the state column its OWN `linkIndex` names. Each approach gets TWO signal groups (a
#: through/right group and a LEFT group), which is what makes the MOVEMENT rather than the approach
#: decide the colour. Columns 2 and 7 are pedestrian groups no vehicular movement addresses -- the
#: InTAS shape -- so a dense renumbering slides columns 3-6 down by one and 8-9 down by two.
STAR_LINKS = {(WW, EE): 0, (WW, SS): 0, (WW, NN): 1,      # W approach: through/right 0, left 1
              (EE, WW): 3, (EE, NN): 3, (EE, SS): 4,      # E approach: through/right 3, left 4
              (SS, NN): 5, (SS, EE): 5, (SS, WW): 6,      # S approach: through/right 5, left 6
              (NN, SS): 8, (NN, WW): 8, (NN, EE): 9}      # N approach: through/right 8, left 9
PRIMARY = {WW: 0, EE: 3, SS: 5, NN: 8}                    # the approach's straight-ahead group
#: what you get if you size the state string by the CONNECTION COUNT and hand out indices in order
_USED = sorted(set(STAR_LINKS.values()))
DENSE_LINKS = {mv: _USED.index(li) for mv, li in STAR_LINKS.items()}

#: 4 phases, cycle 47 s: E-W served (with PERMISSIVE lefts), yellow, N-S served, yellow.
#:                      col:  0123456789
STAR_PHASES = [[20.0, "GgrGgrrrrr", -1, -1],
               [4.0,  "yyryyrrrrr", -1, -1],
               [18.0, "rrrrrGgrGg", -1, -1],
               [5.0,  "rrrrryyryy", -1, -1]]
CYCLE_S = 47.0
N_LINKS = 10


def _doc(phases=STAR_PHASES, movements=True, links=None):
    links = STAR_LINKS if links is None else links
    rec = {"tls": "J", "program": "0", "type": "static", "offset": 0.0,
           "n_links": N_LINKS, "cycle_s": CYCLE_S, "phases": phases, "node": 0,
           "movements": ([[a, b, [li]] for (a, b), li in sorted(links.items())]
                         if movements else []),
           "primary": ([[a, li] for a, li in sorted(PRIMARY.items())] if movements else []),
           "unmapped_links": 0, "mapped_columns": len(set(links.values()))}
    return json.dumps({"nodes": STAR_NODES, "edges": STAR_EDGES, "signal_programs": [rec]})


BASE = dict(seed=5, traffic_flow=True, road_network="custom", duration_s=60.0, dt=0.5,
            arrival_rate=1.2, attacker_pct=0.2, car_following=True, verbose=False)


def _run(tmp_path, tag, **kw):
    cfg = PipelineConfig(out_dir=str(tmp_path / tag), **{**BASE, **kw})
    return run_pipeline(cfg)


# --------------------------------------------------------------------------- the knob ----------- #
def test_the_knob_exists_end_to_end():
    """Dataclass field, schema entry, group, CLI flag -- the surface a user has to reach it by."""
    assert PipelineConfig().real_signals is False
    sch = config_schema()
    assert sch["real_signals"]["default"] is False
    assert sch["real_signals"]["group"] == "Network"
    assert sch["real_signals"]["type"] == "bool" and sch["real_signals"]["help"]
    import io
    import contextlib
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), pytest.raises(SystemExit):
        runmod.main(["--help"])
    assert "--real-signals" in buf.getvalue()


@pytest.mark.parametrize("kw, msg", [
    (dict(road_network="grid", grid_w=4, grid_h=4), "road_network='sumo'"),
    (dict(road_network="custom", custom_network=_doc(), traffic_flow=False,
          arrival_rate=0.0, duration_s=0.0, n_steps=10), "traffic_flow=true"),
    (dict(road_network="custom", custom_network=_doc(), car_following=False), "car_following=true"),
    (dict(road_network="custom", custom_network=json.dumps(
        {"nodes": STAR_NODES, "edges": STAR_EDGES})), "signal_programs"),
])
def test_a_dead_knob_is_refused_rather_than_ignored(kw, msg):
    cfg = PipelineConfig(**{**BASE, **kw, "real_signals": True})
    with pytest.raises(ValueError, match=msg):
        validate_config(cfg)


def test_replay_refuses_real_signals():
    """SUMO already ran the signal control that produced a frozen trajectory, and the engine's
    car-following integrator is off under replay -- nothing would ever read the programs."""
    cfg = PipelineConfig(**{**BASE, "road_network": "custom", "custom_network": _doc(),
                            "real_signals": True, "mobility_source": "sumo_replay"})
    with pytest.raises(ValueError, match="meaningless with mobility_source"):
        validate_config(cfg)


# --------------------------------------------------------------------------- the mapping -------- #
def _hooked(tmp_path, tag, **kw):
    rows: list[dict] = []
    runmod.SIGNAL_HOOK = rows.append
    try:
        res = _run(tmp_path, tag, **kw)
    finally:
        runmod.SIGNAL_HOOK = None
    return res, rows


#: the exit straight ahead of each approach (used only for the primary-movement degradation)
_STRAIGHT = {WW: EE, EE: WW, SS: NN, NN: SS}


def _straight(frm: int) -> int:
    return _STRAIGHT[frm]


def _expected(t: float, link: int) -> str:
    """The character the phase string gives `link` at `t`, computed here from the table above and
    not from `signals.py`, so this is an independent statement of the answer."""
    e = t % CYCLE_S
    acc = 0.0
    for dur, state, _lo, _hi in STAR_PHASES:
        acc += dur
        if e < acc:
            return state[link]
    return STAR_PHASES[-1][1][link]


def test_a_vehicle_is_told_the_colour_of_its_own_movement(tmp_path):
    """The whole point, and the thing a plausible-looking wrong mapping hides.

    Every hook row is checked against the column the movement's OWN `linkIndex` names -- and against
    the column DENSE RENUMBERING would have read, which must disagree somewhere or the map is not
    exercising the failure mode it was built for."""
    _res, rows = _hooked(tmp_path, "map", real_signals=True,
                         custom_network=_doc())
    assert rows, "no vehicle ever reached the signalised junction"
    xy = {tuple(p): i for i, p in enumerate(STAR_NODES)}
    disagreements = 0
    seen: set = set()
    for r in rows:
        assert tuple(r["node"]) == (0.0, 0.0)
        frm = xy[tuple(r["frm"])]
        if r["to"] is None:
            # the route ends at the junction: the plan degrades to the approach's PRIMARY movement,
            # which is exactly what `SignalPlan.links` documents
            link, dense = PRIMARY[frm], DENSE_LINKS[(frm, _straight(frm))]
            mv = (frm, None)
        else:
            mv = (frm, xy[tuple(r["to"])])
            assert mv in STAR_LINKS, f"unexpected movement {mv}"
            link, dense = STAR_LINKS[mv], DENSE_LINKS[mv]
        assert r["char"] == _expected(r["t"], link), (
            f"movement {mv} at t={r['t']} got {r['char']!r}, phase string says "
            f"{_expected(r['t'], link)!r} at column {link}")
        if _expected(r["t"], dense) != r["char"]:
            disagreements += 1
        seen.add(mv)
    assert len({m for m in seen if m[1] is not None}) >= 6, f"only {sorted(seen)} exercised"
    assert disagreements > 0, ("the dense-renumbering bug would have produced the same answer on "
                               "every observed row -- this map is no longer a regression test")


def test_yellow_and_permissive_green_are_distinct_characters(tmp_path):
    """'y' is not 'r' and 'g' is not 'G'. Both distinctions are load-bearing: InTAS has 1162 yellow
    characters and 303 permissive greens, and flattening either one changes who may move."""
    res, rows = _hooked(tmp_path, "chars", real_signals=True, custom_network=_doc())
    chars = {r["char"] for r in rows}
    assert {"G", "y", "r"} <= chars, chars
    assert "g" in chars, "the permissive green in phase 2 was never served"
    man = json.load(open(f"{res.out_dir}/manifest.json", encoding="utf-8"))
    served = man["counts"]["signal_service"]
    assert served["G"] > 0 and served["g"] > 0 and served["y"] > 0 and served["r"] > 0
    assert served["plan"] == {"programs": 1, "records": 1, "skipped_records": 0,
                              "movements": 12, "approaches": 4, "collisions": 0}
    # a yellow is a STOP for a vehicle that can still stop, and a GO for one that cannot
    stopped_y = {r["stopped"] for r in rows if r["char"] == "y"}
    assert True in stopped_y, "no vehicle ever stopped for a yellow"
    assert served["dilemma_go"] > 0, "no vehicle was ever caught inside the dilemma zone"


def test_a_permissive_green_gives_way_and_a_protected_one_does_not(tmp_path):
    """The behavioural half of the 'g' / 'G' distinction, which is the whole reason it may not be
    flattened. In phase 0 the west LEFT (column 1) is 'g' while the east through (column 3) is 'G':
    a west-bound left turn crosses the oncoming stream and must wait for a gap in it, and the phase
    string alone would have let it drive straight through. `PERMISSIVE_CRITICAL_GAP_S` (HCM 6th ed.
    4.1 s for a permitted left) is what it waits for, so it is never blocked for a whole green."""
    res, rows = _hooked(tmp_path, "perm", real_signals=True, custom_network=_doc())
    served = json.load(open(f"{res.out_dir}/manifest.json",
                            encoding="utf-8"))["counts"]["signal_service"]
    assert served["permissive_yield"] > 0
    # a protected green NEVER yields ...
    assert all(not r["stopped"] for r in rows if r["char"] == "G")
    # ... and a permissive one yields SOMETIMES, not always: it takes the gap when there is one
    perm = [r["stopped"] for r in rows if r["char"] == "g"]
    assert True in perm and False in perm, (sum(perm), len(perm))
    # every yielding 'g' is a movement that crosses something: on this map only the LEFT turns are
    # permissive, and their link is column 1 / 4 / 6 / 9
    xy = {tuple(p): i for i, p in enumerate(STAR_NODES)}
    for r in rows:
        if r["char"] == "g" and r["to"] is not None:
            mv = (xy[tuple(r["frm"])], xy[tuple(r["to"])])
            assert STAR_LINKS[mv] in (1, 4, 6, 9), mv


def test_a_red_stops_and_a_green_does_not(tmp_path):
    reds = greens = 0
    _res, rows = _hooked(tmp_path, "rg", real_signals=True, custom_network=_doc())
    for r in rows:
        if r["char"] == "r":
            assert r["stopped"] is True
            reds += 1
        elif r["char"] == "G":
            assert r["stopped"] is False
            greens += 1
    assert reds > 0 and greens > 0


# --------------------------------------------------------------------------- compatibility ------ #
ALL_GREEN = [[d, "G" * N_LINKS, -1, -1] for d, _s, _lo, _hi in STAR_PHASES]
ALL_RED = [[d, "r" * N_LINKS, -1, -1] for d, _s, _lo, _hi in STAR_PHASES]


def test_a_permanently_green_program_reproduces_the_unsignalised_run_byte_for_byte(tmp_path):
    """No new rng, and no other behaviour change, on the real-signal path -- as a digest identity
    rather than as an argument. A program that never says stop must leave the world exactly as an
    unsignalised map left it."""
    off = _run(tmp_path, "g_off", real_signals=False, custom_network=_doc())
    on = _run(tmp_path, "g_on", real_signals=True, custom_network=_doc(phases=ALL_GREEN))
    assert on.data_digest == off.data_digest


def test_a_program_with_no_movements_keeps_todays_behaviour_byte_for_byte(tmp_path):
    """"Where a junction has no program, keep today's behaviour exactly."

    A record whose movements are empty governs nothing: `signal_char` answers None for every
    approach and the caller must fall through to the toy cycle. The digest is the proof."""
    toy = _run(tmp_path, "n_toy", real_signals=False, traffic_lights=True,
               custom_network=_doc())
    both = _run(tmp_path, "n_both", real_signals=True, traffic_lights=True,
                custom_network=_doc(movements=False))
    assert both.data_digest == toy.data_digest
    man = json.load(open(f"{both.out_dir}/manifest.json", encoding="utf-8"))
    served = man["counts"]["signal_service"]
    assert served["G"] == served["g"] == served["y"] == served["r"] == 0
    assert served["toy_red"] > 0 and served["toy_green"] > 0     # the toy cycle really ran


def test_a_permanently_red_program_is_not_the_same_run(tmp_path):
    """The control for the two identities above: the mechanism can move the world."""
    off = _run(tmp_path, "r_off", real_signals=False, custom_network=_doc())
    on = _run(tmp_path, "r_on", real_signals=True, custom_network=_doc(phases=ALL_RED))
    assert on.data_digest != off.data_digest


def test_the_real_signal_run_is_deterministic(tmp_path):
    a = _run(tmp_path, "d_a", real_signals=True, custom_network=_doc())
    b = _run(tmp_path, "d_b", real_signals=True, custom_network=_doc())
    assert a.data_digest == b.data_digest


def test_a_junction_coordinate_carrying_two_programs_is_refused(tmp_path):
    """The importer's node dedupe merging two signalised junctions would light half the movements
    from a program that does not govern them. Never silent."""
    rec_a = json.loads(_doc())["signal_programs"][0]
    rec_b = dict(rec_a, tls="K", phases=ALL_RED)
    doc = json.dumps({"nodes": STAR_NODES, "edges": STAR_EDGES,
                      "signal_programs": [rec_a, rec_b]})
    cfg = PipelineConfig(out_dir=str(tmp_path / "clash"),
                         **{**BASE, "real_signals": True, "custom_network": doc})
    with pytest.raises(ValueError, match="two different <tlLogic> programs"):
        run_pipeline(cfg)


def test_records_that_address_no_node_are_refused(tmp_path):
    rec = json.loads(_doc())["signal_programs"][0]
    rec["node"] = 99
    doc = json.dumps({"nodes": STAR_NODES, "edges": STAR_EDGES, "signal_programs": [rec]})
    cfg = PipelineConfig(out_dir=str(tmp_path / "nonode"),
                         **{**BASE, "real_signals": True, "custom_network": doc})
    with pytest.raises(ValueError, match="no imported program landed on the graph"):
        run_pipeline(cfg)


# --------------------------------------------------------------------------- plan surface ------- #
def test_approaches_answers_what_the_pedestrian_signal_needs():
    """`SignalPlan.approaches` is what lets a crossing ask "is the traffic on this arm green?"
    without a `from` coordinate of its own."""
    prog = SIG.SignalProgram("J", STAR_PHASES)
    plan = SIG.SignalPlan()
    plan.add((0.0, 0.0), prog,
             [(STAR_NODES[a], STAR_NODES[b], [li]) for (a, b), li in STAR_LINKS.items()])
    appr = plan.approaches((0.0, 0.0))
    assert {frm for frm, _ls in appr} == {tuple(STAR_NODES[i]) for i in (NN, EE, SS, WW)}
    # the W approach pools BOTH its groups: a crossing of that arm conflicts with either
    assert dict(appr)[tuple(STAR_NODES[WW])] == (0, 1)
    assert plan.approaches((999.0, 999.0)) == []
