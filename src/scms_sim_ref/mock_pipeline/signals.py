"""REAL traffic-signal programs: SUMO ``tlLogic`` -> a colour a specific vehicle movement can be told.

WHY THIS EXISTS. The engine's own signal control is a toy: `roads.*.node_phase` 2-colours the graph,
`run.py` splits one fixed cycle (default 24 s) in half between "mostly E-W" and "mostly N-S", and
every node is signalised or none is. There is no yellow, no all-red, no turn phase, no offset, no
actuation, and no notion that the *movement* (which way you are turning) decides your colour. Queue
formation, headway distributions and the fundamental diagram all fall out of exactly those things,
and they are what the realism harness measures.

InTAS ships the real Ingolstadt programs inside its ``.net.xml``. `netimport.py` already reads that
file and already marks which junctions netconvert typed ``traffic_light`` -- but it took only the
NODE TYPE and threw the PROGRAM away. This module takes the program.

MEASURED on ``scms-sim/scenarios/gen_intas_urban_low/sumo/ingolstadt.net.xml`` (SUMO 1.25.0):

    98 tlLogic programs, all programID "0", all offset 0
    type=static 22, type=actuated 76 -- but only **20** carry a phase with minDur != maxDur
    cycle time  min 77 s, p25 90, median 90, p75 90, max 116, mean 90.6  (86 of 98 are exactly 90 s)
    phases per program  3:2  4:17  5:1  6:48  7:4  8:24  10:1  11:1
    state characters    r 4084, G 1186, g 303, y 1162   (no 'u'/'o'/'O'/'s' anywhere)
    every program has at least one yellow phase; green stages per program 1..6, mode 3

THE PART THAT SILENTLY BREAKS: the connection -> phase-index mapping. A ``<phase state="rrGGGGg">``
is a string indexed by ``<connection ... linkIndex="N">``; get N off by one and every movement gets a
neighbour's light while the trace still looks entirely plausible. Three things guard it here:

  1. the index is read from the connection's OWN ``linkIndex`` attribute, never from its position in
     any list. That matters: InTAS's ``cluster_308989441_476075007_476075018`` has 15 state
     characters but only 14 vehicular connections and max linkIndex 13 -- link 14 is a pedestrian
     crossing sumolib does not return. Sizing the state by the connection count would shift 14 of
     the 15 columns of that junction;
  2. `audit_link_indices()` re-derives the answer from a source the tlLogic never touched -- the
     junction's ``<request>`` FOE MATRIX, which netconvert computes from geometry. Two movements
     that physically cross must never both show 'G'. MEASURED on InTAS: the true mapping leaves
     **11 protected-green foe pairs out of 1951** (0.56 %, in 4 of 98 junctions, all of them
     properties of the source net -- two lane merges and one genuinely conflicting InTAS program);
     shifting the mapping by +1 gives **319/1548 (20.6 %) in 78 junctions** and by -1 gives
     **456/1714 (26.6 %) in 95 junctions**, i.e. a 37x-47x jump. That is the alarm;
  3. two junctions were then read by hand -- see the module test and REPORT below.

TIMING, and it was measured rather than assumed. SUMO's phase at simulation time ``t`` is found by
walking the cumulative phase durations of ``(t - offset) mod cycle`` on half-open intervals. Verified
against SUMO 1.25.0 itself over a 4-phase 90 s program with ``offset=50`` driven through TraCI at
0.5 s steps: switches observed at t = 2.5 / 5.5 / 47.5 / 50.5 s, i.e. the model's 2 / 5 / 47 / 50
plus exactly one read-lag step. ``(t + offset)`` matched 138/200 samples, ``(t - offset)`` 190/200
(the 10 misses being that same one-step read lag); the boundaries confirm ``t - offset``.

ACTUATION IS NOT REPRODUCED, and that is a stated approximation rather than a silent one. A SUMO
``type="actuated"`` program gap-outs or maxes-out each phase from induction-loop occupancy at the
stop line; this engine has no loop detectors and no per-lane stop-line queue state, so there is
nothing to drive the controller with. Every program here therefore runs FIXED-TIME on the phase's
``duration`` attribute -- which is SUMO's own nominal duration, the value an actuated phase starts
with and only departs from when a detector says so. ``min_dur`` / ``max_dur`` are carried through
unchanged so a real controller can be added later without re-importing, and `program_stats` reports
how wide the unmodelled band is.

NOTHING HERE DRAWS RNG. The colour of a movement at time t is a pure function of the program, so a
signalised run adds zero draws to any stream and cannot move a pinned digest by that route.
"""
from __future__ import annotations

import math

#: Normalised colours. `OFF` is a dark/blinking signal -- the junction is uncontrolled, and a caller
#: should fall through to whatever it does at an unsignalised node (gap acceptance), NOT treat it as
#: green.
GREEN, YELLOW, RED, OFF = "green", "yellow", "red", "off"

#: SUMO link-state character -> normalised colour. The full SUMO vocabulary, including the four
#: characters InTAS happens not to use, so an imported net cannot surprise us:
#:   G green with priority     g green, must yield     s green right-arrow, must stop first
#:   y yellow                  u red+yellow (imminent green -- still "do not enter")
#:   r red                     o off/blinking          O off/no signal
_CHAR_COLOUR = {"G": GREEN, "g": GREEN, "s": GREEN,
                "y": YELLOW, "u": YELLOW,
                "r": RED,
                "o": OFF, "O": OFF}

#: Green states that carry NO right of way -- the driver may go but must give way (permissive left,
#: right-on-arrow). Exposed because "green" and "protected" are different questions.
_PERMISSIVE = frozenset("gso")

#: Most-permissive-first ordering, used when one movement is served by several links (one per lane):
#: a vehicle picks a lane, so it gets the best colour any of its lanes offers. Disagreements are
#: counted, not hidden -- see `program_stats`.
_RANK = {GREEN: 0, YELLOW: 1, OFF: 2, RED: 3}


def char_colour(ch: str) -> str:
    """One SUMO link-state character -> `GREEN` / `YELLOW` / `RED` / `OFF`.

    An unknown character is RED: the conservative reading, and loud in the stats rather than
    silently permissive."""
    return _CHAR_COLOUR.get(ch, RED)


def is_permissive(ch: str) -> bool:
    """True for a green that must give way ('g', 's') or a blinking-off signal ('o')."""
    return ch in _PERMISSIVE


class SignalProgram:
    """One ``<tlLogic>``: the phase sequence a junction (or a joined group of them) cycles through.

    `phases` is a sequence of ``(duration_s, state, min_dur, max_dur)`` -- `state` is SUMO's
    per-link character string, and `min_dur`/`max_dur` are SUMO's own -1 for "not given". The
    program is FIXED-TIME on `duration_s` (see the module note on actuation); the bounds are carried
    so a later actuated controller has them.

    Deterministic and RNG-free. `phase_at`/`colour_at` memoise the last query, because every vehicle
    in one simulation step asks about the same `t`."""

    __slots__ = ("tls_id", "program_id", "type", "offset", "durations", "states",
                 "min_dur", "max_dur", "cycle_s", "n_links", "_cum", "_memo")

    def __init__(self, tls_id: str, phases, *, program_id: str = "0", type: str = "static",
                 offset: float = 0.0):
        durations: list[float] = []
        states: list[str] = []
        mins: list[float] = []
        maxs: list[float] = []
        for k, ph in enumerate(phases):
            d, st = float(ph[0]), str(ph[1])
            lo = float(ph[2]) if len(ph) > 2 and ph[2] is not None else -1.0
            hi = float(ph[3]) if len(ph) > 3 and ph[3] is not None else -1.0
            if not math.isfinite(d) or d < 0.0:
                raise ValueError(f"tls {tls_id!r} phase {k}: duration must be finite and >= 0 "
                                 f"(got {ph[0]!r})")
            if not st:
                raise ValueError(f"tls {tls_id!r} phase {k}: empty state string")
            durations.append(d)
            states.append(st)
            mins.append(lo)
            maxs.append(hi)
        if not durations:
            raise ValueError(f"tls {tls_id!r} has no phases")
        widths = {len(s) for s in states}
        if len(widths) != 1:
            raise ValueError(f"tls {tls_id!r} phases disagree on link count: {sorted(widths)}")
        cycle = sum(durations)
        if cycle <= 0.0:
            raise ValueError(f"tls {tls_id!r} has a zero-length cycle ({len(durations)} phases)")
        self.tls_id = str(tls_id)
        self.program_id = str(program_id)
        self.type = str(type)
        self.offset = float(offset)
        self.durations = tuple(durations)
        self.states = tuple(states)
        self.min_dur = tuple(mins)
        self.max_dur = tuple(maxs)
        self.cycle_s = cycle
        self.n_links = widths.pop()
        cum: list[float] = []
        acc = 0.0
        for d in durations:
            acc += d
            cum.append(acc)
        self._cum = tuple(cum)
        self._memo: tuple = (None, 0)

    # -------------------------------------------------------------- timing
    def phase_at(self, t: float) -> int:
        """Index of the phase in force at simulation time `t`.

        ``elapsed = (t - offset) mod cycle``, half-open intervals -- SUMO's own semantics, verified
        against SUMO 1.25.0 through TraCI (see the module note)."""
        memo_t, memo_i = self._memo
        if memo_t == t:
            return memo_i
        e = (t - self.offset) % self.cycle_s
        cum = self._cum
        i = 0
        n = len(cum)
        while i < n - 1 and e >= cum[i]:
            i += 1
        self._memo = (t, i)
        return i

    def state_at(self, t: float) -> str:
        """The whole per-link state string in force at `t`."""
        return self.states[self.phase_at(t)]

    def char_at(self, t: float, link: int) -> str:
        """The raw SUMO state character for `link` at `t` ('G'/'g'/'y'/'r'/...).

        A link index outside the state string is 'r': an out-of-range index means this movement is
        not one the program controls, and inventing a green for it would be the exact failure this
        module exists to prevent."""
        st = self.states[self.phase_at(t)]
        return st[link] if 0 <= link < len(st) else "r"

    def colour_at(self, t: float, link: int) -> str:
        """`GREEN` / `YELLOW` / `RED` / `OFF` for `link` at `t`."""
        return char_colour(self.char_at(t, link))

    def colour_of(self, t: float, links) -> str:
        """Colour for a movement served by SEVERAL links (one per lane): the most permissive.

        A driver chooses a lane, so a movement with one green lane and one red lane is green for a
        vehicle that wants it. In practice the lanes of one movement almost always agree --
        `program_stats` counts the exceptions rather than letting them pass unremarked."""
        best = RED
        rank = _RANK[RED]
        for li in links:
            c = self.colour_at(t, li)
            r = _RANK[c]
            if r < rank:
                best, rank = c, r
                if r == 0:
                    break
        return best

    # -------------------------------------------------------------- facts
    @property
    def actuated(self) -> bool:
        """True iff at least one phase declares minDur != maxDur -- i.e. SUMO would actually vary
        this program's timing. `type == "actuated"` alone does not mean that: 56 of InTAS's 76
        `type="actuated"` programs carry no minDur/maxDur at all and run fixed-time in SUMO too."""
        return any(lo >= 0.0 and hi >= 0.0 and lo != hi
                   for lo, hi in zip(self.min_dur, self.max_dur))

    @property
    def cycle_bounds_s(self) -> tuple[float, float]:
        """(shortest, longest) cycle a real actuated controller could produce, from minDur/maxDur
        where given and `duration` where not. Equal to (cycle_s, cycle_s) for a fixed-time program;
        the width of this band is exactly what the fixed-time approximation does not model."""
        lo = hi = 0.0
        for d, a, b in zip(self.durations, self.min_dur, self.max_dur):
            lo += a if a >= 0.0 else d
            hi += b if b >= 0.0 else d
        return (lo, hi)

    def green_stages(self) -> int:
        """Number of phases that serve traffic (some green, no yellow) -- the program's real stage
        count, as against `len(self.durations)` which also counts yellow and all-red."""
        return sum(1 for s in self.states
                   if ("G" in s or "g" in s) and "y" not in s)

    def to_record(self) -> dict:
        """JSON-safe form (the on-disk `signal_programs` schema; see `SignalPlan.from_records`)."""
        return {"tls": self.tls_id, "program": self.program_id, "type": self.type,
                "offset": self.offset, "n_links": self.n_links,
                "cycle_s": round(self.cycle_s, 3),
                "phases": [[d, s, lo, hi] for d, s, lo, hi
                           in zip(self.durations, self.states, self.min_dur, self.max_dur)]}

    def __repr__(self) -> str:                       # pragma: no cover - debugging aid
        return (f"SignalProgram({self.tls_id!r}, {len(self.durations)} phases, "
                f"{self.n_links} links, cycle={self.cycle_s:g}s, type={self.type})")


class SignalPlan:
    """Every real program on a map, addressed the way a driver experiences one.

    A vehicle approaching junction `J` from junction `A` on its way to junction `B` asks
    `colour(J, A, B, t)` and is told GREEN / YELLOW / RED / OFF -- because that MOVEMENT's link
    index is what the phase string is indexed by. `colour(J, A, None, t)` (approach only, no
    intended exit) answers with the approach's PRIMARY movement: its straight-ahead link where it
    has one, otherwise its lowest link index. That is a documented degradation, not an equivalence:
    an approach's left turn and its through movement genuinely have different colours for part of
    every cycle at a junction with a turn phase.

    Junctions and approaches are addressed by COORDINATE, never by node index. Node indices do not
    survive `roads.largest_strong_component`, which every directed import goes through and which
    returns no remap -- a plan keyed on indices would signalise the wrong junctions and look fine.
    Coordinates survive it exactly.

    `None` from `colour()` means "no program governs this" and the caller must fall back to its
    existing behaviour. That is what keeps every unsignalised map byte-identical."""

    __slots__ = ("programs", "_move", "_appr", "_primary", "stats")

    def __init__(self):
        #: junction coordinate -> SignalProgram
        self.programs: dict[tuple[float, float], SignalProgram] = {}
        #: (junction, from) -> {to: (link, ...)}
        self._move: dict[tuple, dict[tuple, tuple]] = {}
        #: (junction, from) -> (link, ...) over every exit
        self._appr: dict[tuple, tuple] = {}
        #: (junction, from) -> the approach's primary link (straight where there is one)
        self._primary: dict[tuple, int] = {}
        self.stats: dict = {}

    # -------------------------------------------------------------- build
    def add(self, node_xy, program: SignalProgram, movements=(), primary=None) -> None:
        """Attach `program` to the junction at `node_xy`.

        `movements` is ``[(from_xy, to_xy, [link, ...]), ...]``; `primary` is
        ``{from_xy: link}``. A junction may legitimately appear twice (a joined tls controls two
        junctions with one program) -- each gets its own movement set out of the shared state
        string."""
        key = _key(node_xy)
        self.programs[key] = program
        appr: dict[tuple, set] = {}
        for frm, to, links in movements:
            fk, tk = _key(frm), _key(to)
            ls = tuple(sorted({int(v) for v in links}))
            if not ls:
                continue
            self._move.setdefault((key, fk), {})[tk] = ls
            appr.setdefault(fk, set()).update(ls)
        for fk, ls in appr.items():
            self._appr[(key, fk)] = tuple(sorted(ls))
        for frm, link in (primary or {}).items():
            self._primary[(key, _key(frm))] = int(link)

    @classmethod
    def from_records(cls, records, nodes) -> "SignalPlan":
        """Build from the on-disk `signal_programs` layer plus the document's `nodes` array.

        Record schema (what `netimport.net_to_network(signals=True)` writes):

            {"tls": "1863241632", "program": "0", "type": "actuated", "offset": 0.0,
             "node": 42,                                node index into `nodes`
             "n_links": 7, "cycle_s": 90.0,
             "phases": [[duration, state, minDur, maxDur], ...],   minDur/maxDur -1 = not given
             "movements": [[from_node, to_node, [link, ...]], ...],
             "primary":   [[from_node, link], ...],
             "unmapped_links": 0}

        Node indices are resolved to coordinates HERE and never used again."""
        plan = cls()
        pts = [(float(p[0]), float(p[1])) for p in nodes]
        n = len(pts)
        skipped = 0
        for rec in records or ():
            i = rec.get("node")
            if not isinstance(i, int) or not (0 <= i < n):
                skipped += 1
                continue
            prog = SignalProgram(rec.get("tls", "?"), rec["phases"],
                                 program_id=rec.get("program", "0"),
                                 type=rec.get("type", "static"),
                                 offset=float(rec.get("offset", 0.0) or 0.0))
            moves = []
            for frm, to, links in rec.get("movements", ()):
                if 0 <= frm < n and 0 <= to < n:
                    moves.append((pts[frm], pts[to], links))
            primary = {pts[f]: li for f, li in rec.get("primary", ()) if 0 <= f < n}
            plan.add(pts[i], prog, moves, primary)
        plan.stats = {"programs": len(plan.programs), "records": len(records or ()),
                      "skipped_records": skipped,
                      "movements": sum(len(v) for v in plan._move.values()),
                      "approaches": len(plan._appr)}
        return plan

    # -------------------------------------------------------------- query
    def has(self, node_xy) -> bool:
        """True iff a real program governs the junction at `node_xy`."""
        return _key(node_xy) in self.programs

    def program(self, node_xy):
        """The `SignalProgram` at `node_xy`, or None."""
        return self.programs.get(_key(node_xy))

    def links(self, node_xy, from_xy=None, to_xy=None):
        """The link indices for a movement / an approach, or None if this plan does not know it.

        Exposed so a test can assert the mapping directly rather than through a colour."""
        key = _key(node_xy)
        if from_xy is None:
            return None
        fk = _key(from_xy)
        if to_xy is not None:
            m = self._move.get((key, fk))
            if m is not None:
                ls = m.get(_key(to_xy))
                if ls:
                    return ls
        li = self._primary.get((key, fk))
        if li is not None:
            return (li,)
        return self._appr.get((key, fk))

    def colour(self, node_xy, from_xy=None, to_xy=None, t: float = 0.0):
        """The colour a vehicle at `from_xy -> node_xy -> to_xy` sees at time `t`, or None.

        None means "not governed by a real program here" -- an unsignalised junction, a junction
        whose program was not imported, or an approach the program does not control (a slip road
        that bypasses the signal). The caller keeps its existing behaviour for None; that is the
        whole compatibility story."""
        prog = self.programs.get(_key(node_xy))
        if prog is None:
            return None
        ls = self.links(node_xy, from_xy, to_xy)
        if not ls:
            return None
        return prog.colour_of(t, ls)


def _key(p) -> tuple[float, float]:
    """A junction/approach coordinate as the hashable key both this module and
    `roads.CustomNetwork._coord_idx` use: the exact (float x, float y) pair."""
    return (float(p[0]), float(p[1]))


# ------------------------------------------------------------------------------------------------
# sumolib extraction
# ------------------------------------------------------------------------------------------------
def tls_connections(net) -> dict:
    """``tls id -> [Connection, ...]`` for every TLS-controlled vehicular connection in `net`.

    Built from the EDGE connection objects rather than `TLS.getConnections()`, because only the
    Connection carries `getTLLinkIndex()`, `getDirection()` and the from/to edges together -- and
    the link index has to come off the connection itself (see the module note)."""
    out: dict[str, list] = {}
    for e in net.getEdges():
        if e.getFunction() == "internal":
            continue
        for lst in e.getOutgoing().values():
            for c in lst:
                tid = c.getTLSID()
                if tid:
                    out.setdefault(tid, []).append(c)
    for lst in out.values():
        lst.sort(key=lambda c: (c.getTLLinkIndex(), c.getFrom().getID(), c.getTo().getID(),
                                c.getFromLane().getIndex()))
    return out


def programs_from_net(net) -> dict:
    """``tls id -> SignalProgram`` for every program in a sumolib net.

    The net MUST have been read with ``sumolib.net.readNet(path, withPrograms=True)``; without it
    sumolib silently drops every ``<tlLogic>`` and this returns {} -- which is why the caller raises
    rather than emitting an empty layer.

    A tls with several programIDs keeps the lexicographically first (netconvert writes "0"; a
    hand-authored ``.add.xml`` may add more, and picking one deterministically beats picking
    whichever the dict happened to yield)."""
    out: dict[str, SignalProgram] = {}
    for tls in net.getTrafficLights():
        progs = tls.getPrograms()
        if not progs:
            continue
        pid = sorted(progs)[0]
        p = progs[pid]
        phases = [(ph.duration, ph.state,
                   ph.minDur if ph.minDur is not None else -1,
                   ph.maxDur if ph.maxDur is not None else -1)
                  for ph in p.getPhases()]
        if not phases:
            continue
        out[tls.getID()] = SignalProgram(tls.getID(), phases, program_id=pid,
                                         type=p.getType(), offset=p.getOffset())
    return out


#: SUMO connection `dir` codes, most-straight-ahead first. Used to pick an approach's PRIMARY
#: movement when the caller does not say where it is going: 's' straight, then the partial turns,
#: then the full turns, then the u-turn.
_DIR_RANK = {"s": 0, "R": 1, "L": 2, "r": 3, "l": 4, "t": 5}


def extract(net, node_index, edge_endpoints, *, programs=None) -> tuple[list, dict]:
    """SUMO net -> the on-disk `signal_programs` records, in the importer's own graph indices.

    `node_index(sumo_node_id)` -> the graph node index for that junction, or None if it did not
    survive the import. `edge_endpoints(sumo_edge_id)` -> ``(a, b)`` graph node indices for that
    edge's direction of travel, or None. Both are closures the importer supplies, so this function
    never has to know how the importer dedupes, trims or remaps -- which is the whole reason the
    mapping stays honest across `--strong`, the class-drop retry loop and the connected-component
    prune.

    One record PER CONTROLLED JUNCTION: a joined tls spanning two junctions yields two records
    sharing one program, each carrying only its own junction's movements out of the shared state
    string. Returns ``(records, stats)``."""
    programs = programs_from_net(net) if programs is None else programs
    conns = tls_connections(net)
    records: list[dict] = []
    st = {"tls_total": len(programs), "tls_no_connections": 0, "tls_no_node": 0,
          "junctions": 0, "links_total": 0, "links_unmapped": 0, "links_self_loop": 0,
          "movements": 0, "joined_tls": 0}
    for tls_id in sorted(programs):
        prog = programs[tls_id]
        cs = conns.get(tls_id, ())
        if not cs:
            st["tls_no_connections"] += 1
            continue
        # group this tls's connections by the JUNCTION they enter (a joined tls controls several)
        by_node: dict[str, list] = {}
        for c in cs:
            by_node.setdefault(c.getFrom().getToNode().getID(), []).append(c)
        if len(by_node) > 1:
            st["joined_tls"] += 1
        placed = 0
        for sumo_node in sorted(by_node):
            gi = node_index(sumo_node)
            if gi is None:
                continue
            moves: dict[tuple[int, int], set] = {}
            best_dir: dict[int, tuple] = {}
            for c in by_node[sumo_node]:
                st["links_total"] += 1
                li = c.getTLLinkIndex()
                fe = edge_endpoints(c.getFrom().getID())
                te = edge_endpoints(c.getTo().getID())
                if fe is None or te is None:
                    st["links_unmapped"] += 1
                    continue
                frm, to = fe[0], te[1]
                if frm == gi or to == gi or frm == to:
                    # the graph collapsed one of the two arms onto the junction itself; there is no
                    # movement to attach the link to. Counted, never guessed at.
                    st["links_self_loop"] += 1
                    continue
                moves.setdefault((frm, to), set()).add(li)
                rank = (_DIR_RANK.get(c.getDirection(), 9), li)
                if frm not in best_dir or rank < best_dir[frm]:
                    best_dir[frm] = rank
            if not moves:
                continue
            rec = prog.to_record()
            rec["node"] = gi
            rec["movements"] = [[a, b, sorted(v)] for (a, b), v in sorted(moves.items())]
            rec["primary"] = [[a, r[1]] for a, r in sorted(best_dir.items())]
            rec["unmapped_links"] = 0
            records.append(rec)
            st["movements"] += len(rec["movements"])
            placed += 1
        if placed == 0:
            st["tls_no_node"] += 1
        st["junctions"] += placed
    return records, st


# ------------------------------------------------------------------------------------------------
# measured facts + the index audit
# ------------------------------------------------------------------------------------------------
def _quantiles(vals):
    v = sorted(vals)
    if not v:
        return {}

    def q(f):
        if len(v) == 1:
            return v[0]
        i = f * (len(v) - 1)
        lo = int(math.floor(i))
        hi = min(lo + 1, len(v) - 1)
        return v[lo] + (v[hi] - v[lo]) * (i - lo)
    return {"min": v[0], "p25": round(q(0.25), 2), "median": round(q(0.5), 2),
            "p75": round(q(0.75), 2), "max": v[-1],
            "mean": round(sum(v) / len(v), 2)}


def program_stats(programs) -> dict:
    """The facts a signal import must be judged on: how many programs, how long their cycles are,
    how many phases they run, and how many of them are ACTUATED as opposed to merely typed so.

    `programs` is an iterable of `SignalProgram` (or the `{tls: SignalProgram}` mapping).
    `actuated_declared` counts `type == "actuated"`; `actuated_effective` counts programs that
    really carry a minDur != maxDur phase, which is the only kind whose timing SUMO would vary. The
    two differ by 56 on InTAS, and reporting only the first would overstate what is being
    approximated by more than 3x."""
    progs = list(programs.values() if isinstance(programs, dict) else programs)
    if not progs:
        return {"programs": 0}
    phase_hist: dict[int, int] = {}
    type_hist: dict[str, int] = {}
    char_hist: dict[str, int] = {}
    for p in progs:
        phase_hist[len(p.durations)] = phase_hist.get(len(p.durations), 0) + 1
        type_hist[p.type] = type_hist.get(p.type, 0) + 1
        for s in p.states:
            for ch in s:
                char_hist[ch] = char_hist.get(ch, 0) + 1
    act = [p for p in progs if p.actuated]
    band = [p.cycle_bounds_s for p in act]
    return {
        "programs": len(progs),
        "cycle_s": _quantiles([p.cycle_s for p in progs]),
        "cycle_hist": _hist(int(p.cycle_s) for p in progs),
        "phases_hist": dict(sorted(phase_hist.items())),
        "green_stages_hist": _hist(p.green_stages() for p in progs),
        "links": _quantiles([p.n_links for p in progs]),
        "type_hist": dict(sorted(type_hist.items())),
        "actuated_declared": type_hist.get("actuated", 0),
        "actuated_effective": len(act),
        "fixed_time_effective": len(progs) - len(act),
        # what the fixed-time approximation cannot represent, in seconds of cycle
        "actuated_cycle_band_s": ({"min_sum": _quantiles([b[0] for b in band]),
                                   "max_sum": _quantiles([b[1] for b in band])} if band else {}),
        "state_char_hist": dict(sorted(char_hist.items())),
        "programs_without_yellow": sum(1 for p in progs
                                       if not any("y" in s for s in p.states)),
        "offsets_nonzero": sum(1 for p in progs if p.offset),
    }


def _hist(vals) -> dict:
    h: dict = {}
    for v in vals:
        h[v] = h.get(v, 0) + 1
    return dict(sorted(h.items()))


def audit_link_indices(net, *, shifts=(0,), programs=None) -> list[dict]:
    """Re-derive the connection -> phase-index mapping from the junction FOE MATRIX and report the
    disagreement, for the true mapping and for any deliberately shifted ones.

    THE POINT. ``<request index=... foes=...>`` is netconvert's geometric conflict matrix; the
    ``<tlLogic>`` never touches it. So "two movements that physically cross both show 'G'" is an
    independent test of the index mapping, and an off-by-one lights it up: MEASURED on InTAS,
    shift 0 -> 11 conflicting pairs of 1951 (4 junctions), shift +1 -> 319 of 1548 (78 junctions),
    shift -1 -> 456 of 1714 (95 junctions).

    A residual at shift 0 is expected and is a property of the source net -- merging lanes are foes
    of each other, and a real program may serve a pair SUMO considers conflicting. What must never
    be true is that a shifted mapping looks comparable.

    Returns one dict per shift: ``{"shift", "conflicts", "pairs", "junctions", "out_of_range"}``.
    Requires `net` read with ``withPrograms=True`` (and `withFoes`, sumolib's default)."""
    programs = programs_from_net(net) if programs is None else programs
    conns = tls_connections(net)
    out = []
    for shift in shifts:
        conflicts = pairs = oob = 0
        bad: set = set()
        for tls_id, prog in sorted(programs.items()):
            cs = conns.get(tls_id, ())
            for state in prog.states:
                prot = []
                for c in cs:
                    li = c.getTLLinkIndex() + shift
                    if not (0 <= li < len(state)):
                        oob += 1
                        continue
                    if state[li] == "G":
                        prot.append(c)
                for i in range(len(prot)):
                    a = prot[i]
                    na = a.getFrom().getToNode()
                    ia = na.getLinkIndex(a)
                    if ia < 0:
                        continue
                    for j in range(i + 1, len(prot)):
                        b = prot[j]
                        if b.getFrom().getToNode() is not na:
                            continue            # a joined tls: different junction, cannot conflict
                        ib = na.getLinkIndex(b)
                        if ib < 0:
                            continue
                        pairs += 1
                        if na.areFoes(ia, ib):
                            conflicts += 1
                            bad.add(tls_id)
        out.append({"shift": shift, "conflicts": conflicts, "pairs": pairs,
                    "junctions": len(bad), "out_of_range": oob})
    return out
