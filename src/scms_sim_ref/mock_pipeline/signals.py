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

MEASURED on ``scms-sim/scenarios/gen_intas_urban_low/sumo/ingolstadt.net.xml`` (SUMO/sumolib
1.25.0, 16.9 MB, 3332 junctions / 7942 edges). Every number below is reproduced by
`tests/test_signals_intas.py`, which reads that same file:

    junctions netconvert typed traffic_light            109
    <tlLogic> programs                                   98   all programID "0", all offset 0
    junctions carrying a usable program                  98   (of the 109; the other 11 below)
    junctions per tls                             {1: 98}     -- InTAS has NO joined tls
    TLS-controlled vehicular connections               1042
    type=static 22, type=actuated 76 -- but only **20** carry a phase with minDur != maxDur
    cycle time  min 77 s, p25 90, median 90, p75 90, max 116, mean 90.59
                77:2  86:2  90:86  91:2  94:1  95:1  105:1  107:1  113:1  116:1
    phases per program    3:2  4:17  5:1  6:48  7:4  8:24  10:1  11:1
    green stages/program  1:2  2:18  3:50  4:26  5:1  6:1
    links per program     min 3, p25 7.25, median 10, p75 13, max 19, mean 10.53
    state characters      r 4084, G 1186, g 303, y 1162  (6735 total; no 'u'/'o'/'O'/'s' anywhere)
    programs with no yellow phase                         0

The 11 traffic_light junctions with no program are pedestrian-only clusters: each has incoming
edges and connections, but every one of those connections carries ``tl=""`` and there is no
``<tlLogic>`` for it anywhere in the file (``cluster_2043477676_292226046``,
``cluster_1031988642_267289885_349081748_937878779``, ``cluster_497590130_656751589``,
``cluster_1197035492_314912397``, ``cluster_1497476927_cluster_1379976907_1501766404``,
``cluster_1500083093_1500083101_1507566574_267783912``, ``cluster_1840464356_32564064``,
``cluster_272514267_2937390079_2937390082``, ``cluster_1840209252_292578437``,
``cluster_26288888_310442452``, ``cluster_1993601936_2043477649``). They are not a coverage gap in
this code: there is nothing to import.

HOW MUCH OF THE STATE STRING IS ADDRESSABLE. The 98 programs span **1032 state columns**, of which
**1027 (99.52 %)** resolve to at least one vehicular connection, and **6735 state characters**, of
which **6696 (99.42 %)** sit on a resolvable column. The 5 unresolvable columns are signal groups
with no vehicular connection at all -- pedestrian/bicycle stages the InTAS controllers keep in the
string: ``249179919`` col 4, ``cluster_13876325_281116670`` col 6,
``cluster_1427494838_273472399`` cols 0-1, ``cluster_308989441_476075007_476075018`` col 14. No
connection anywhere in the net has a ``linkIndex`` outside its program's state string (0 of 1042),
and no two controlled junctions share an exact coordinate (0 collisions).

THE PART THAT SILENTLY BREAKS: the connection -> phase-index mapping. A ``<phase state="rrGGGGg">``
is a string indexed by ``<connection ... linkIndex="N">``; get N off by one and every movement gets a
neighbour's light while the trace still looks entirely plausible. Three things guard it here:

  1. the index is read from the connection's OWN ``linkIndex`` attribute, never from its position in
     any list. THAT IS NOT COSMETIC, and InTAS proves it. ``cluster_13876325_281116670`` has a
     14-character state string but only **13** controlled connections: the file contains
     ``linkIndex`` 0,1,2,3,4,5,7,8,9,10,11,12,13 and NO connection with ``linkIndex="6"`` -- column
     6 is a pedestrian group. Renumber the connections densely (0..12, the natural thing to do if
     you size the state by the connection count) and links 7..13 each read their left neighbour's
     column: in phase 0 ``rrrGGGgrrrGGGr`` the west-arm LEFT (link 13) reads column 12 = 'G' and is
     given a PROTECTED green straight into the east-arm through movements (links 4 and 5), which
     netconvert's foe matrix marks as its foes. `audit_link_indices(..., dense=True)` measures
     exactly this: **17 of 1042 connections move**, across 3 junctions (``249179919`` 2,
     ``cluster_1427494838_273472399`` 8, ``cluster_13876325_281116670`` 7), and the protected-green
     foe count goes **11 in 4 junctions -> 18 in 6**. The 7 new pairs are precisely the ones just
     described: ``cluster_13876325_281116670`` (3,13) (4,13) (5,13) in phase 0 and (1,9) in phase 4,
     ``cluster_1427494838_273472399`` (4,9) (5,9) in phase 0 and (4,6) in phase 2;
  2. `audit_link_indices()` re-derives the answer from a source the tlLogic never touched -- the
     junction's ``<request>`` FOE MATRIX, which netconvert computes from geometry. Two movements
     that physically cross must never both show 'G'. The junction request index is genuinely a
     second ordering, not a restatement of ``linkIndex``: they agree for 1001 of the 1042
     connections and **disagree for 41, across 6 junctions**. MEASURED on InTAS, the true mapping
     leaves **11 protected-green foe pairs out of 1951** (0.56 %, in 4 of 98 junctions); shifting
     the mapping by +1 gives **319/1548 (20.6 %) in 78 junctions**, by -1 **456/1714 (26.6 %) in 95
     junctions**, by +2 **259/1273 in 66**, by -2 **366/1415 in 67**. A 30x-47x jump: that is the
     alarm. The 11 residuals are properties of the source net and were read individually --
     3 are two lanes of ONE approach merging into one exit lane, which netconvert always calls a
     foe (``cluster_1443568599_365519573`` 1, ``gneJ210`` 2), and 8 are two InTAS programs that
     genuinely serve crossing movements protected-green (``279299817`` 7, ``gneJ144`` 1);
  3. two junctions were then read against the raw XML by hand -- ``1863241632`` (T-junction,
     7 links, protected + permissive left) and ``cluster_13876325_281116670`` (4-arm, the column-6
     gap above). Both are re-derived from geometry in `tests/test_signals_intas.py`.

TIMING, and it was measured rather than assumed. SUMO's phase at simulation time ``t`` is found by
walking the cumulative phase durations of ``(t - offset) mod cycle`` on half-open intervals. Nothing
in InTAS can check the SIGN of that subtraction -- all 98 of its programs have ``offset="0"`` -- so
it was checked against SUMO 1.25.0 itself: a 3x3 ``netgenerate`` grid, its junction's program
replaced by 4 phases of 2/3/42/43 s (cycle 90) with ``offset="50"``, driven through TraCI for 200
steps of 0.5 s. ``(t - offset)`` matched **195/200** samples, ``(t + offset)`` only **134/200**. The
5 misses are all switch boundaries and all one step wide: SUMO reported the new phase at
7.5 / 50.5 / 52.5 / 55.5 / 97.5 s against the model's 7 / 50 / 52 / 55 / 97, which is TraCI's usual
end-of-step read lag. And those boundaries are themselves the proof: with cumulative phase ends
2 / 5 / 47 / 90 and offset 50, ``t - offset`` puts switches at (0+50), (2+50), (5+50), (47+50) mod 90
= 50, 52, 55, 7 -- exactly the observed set.

ACTUATION IS NOT REPRODUCED, and that is a stated approximation rather than a silent one. A SUMO
``type="actuated"`` program gap-outs or maxes-out each phase from induction-loop occupancy at the
stop line. Reproducing it needs three things this engine does not have: E1/E2 detectors at a fixed
setback from each stop line, a per-lane occupancy/time-gap signal sampled every step, and the
controller's own gap/passing-time parameters -- none of which survive into `roads.CustomNetwork`,
which knows only junction coordinates and edge polylines. There is nothing to drive the controller
with, so inventing one would produce timing that is arbitrary rather than actuated.

THE APPROXIMATION, stated exactly: every program runs FIXED-TIME on each phase's ``duration``
attribute, cycling with period ``sum(duration)``, i.e. `phase_at` walks the cumulative durations of
``(t - offset) mod cycle_s``. ``duration`` is SUMO's own nominal value -- the length an actuated
phase starts with and departs from only when a detector says so -- so the approximation is exactly
"every detector reports the nominal demand". It is exact for the 78 InTAS programs with no
minDur/maxDur band, and for the 20 that have one it fixes the cycle at its nominal length instead of
letting it breathe. MEASURED width of what is NOT modelled, over those 20: shortest possible cycle
median 32 s (min 24, max 50), longest possible median 165 s (min 110, max 220), against a nominal
90 s. ``min_dur``/``max_dur`` are carried through unchanged so a real controller can be added later
without re-importing, and `program_stats["actuated_cycle_band_s"]` reports the band.

END TO END, and this is the number the realism harness cares about. `netimport.import_net(net,
signals=True, strong=True, undirected_shapes=True)` on InTAS places all 98 programs on 98 graph
junctions (3289 nodes after the strong-component trim), maps 1030 of the 1042 controlled
connections into **778 movements over 330 approaches** -- 57 three-arm junctions, 36 four-arm, 2
five-arm -- and leaves 1015 of 1032 state columns addressable. 12 links are lost, all to the
strong-component trim, in 4 records (``281967823`` 2, ``cluster_1270380467_1270380469_1270380471``
1, ``cluster_308989441_476075007_476075018`` 3, ``cluster_475944687_475944688_480162640`` 6 -- that
last one keeps only 1 of its 7 columns, because the trim took 3 of its 4 arms). Sampling every one
of those 778 movements at 1 s over a 90 s cycle:

    real programs   G 34.00 %   g 8.45 %   y 4.30 %   r 53.24 %
    node_phase      G 50 %      g --       y 0 %      r 50 %     (every junction, one 24 s cycle)

CONSUMING IT. `roads._LaneFrameMixin.set_signal_plan` attaches a `SignalPlan` -- the surface is on
the mixin, so grid, ring and custom maps all answer it and a caller needs no getattr. `signal_char`
/ `signal_colour` answer for a movement and return **None** when no real program governs it, which
is what lets a caller keep its existing behaviour untouched; `signal_plan` is a CLASS attribute
defaulting to None, so a map that never opts in allocates nothing. `roads.Trip.next_movement`
supplies the (from, junction, to) triple.

WIRED INTO THE ENGINE BY ``PipelineConfig.real_signals`` / ``--real-signals`` (default OFF, so
nothing on a default path reaches this module and both pinned digests hold byte for byte). `run.py`
attaches the plan after building the map, and `car_follow` resolves each vehicle's own movement once
per step and acts on the CHARACTER, not merely on the colour:

    'G'      proceed -- this movement owns the junction
    'g'/'s'  proceed, but GIVE WAY to a conflicting protected stream inside the critical gap
             (`run.PERMISSIVE_CRITICAL_GAP_S`, HCM 6th ed. 4.1 s for a permitted left); a permissive
             left crosses the oncoming through movement, which is why 'g' must never be read as 'G'
    'y'/'u'  stop, unless already inside the dilemma zone (v^2/2b > distance to the line)
    'r'      stop
    None     no real program governs this -- keep the caller's existing behaviour exactly

MEASURED end to end on InTAS (internal IDM, 300 s, dt 0.5, 334 vehicles, seed 42), against the toy
2-colouring the engine had before, on identical everything else:

                                   toy (all 3289 junctions)   real (98 programs)
    stops per vehicle, mean                     3.517                0.477
    vehicle-steps stopped                      11.32 %               7.44 %
    queue at a signalised junction, mean         1.089                1.275
    ... p95 / max                                2 / 3                3 / 5

The real programs take the phantom signals off the 3,191 junctions the city does not signalise and
put LONGER queues on the 98 it does, which is the shape the toy model could not produce at all.

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
#: right-on-arrow). Exposed because "green" and "protected" are different questions, and InTAS
#: leans on the difference: 303 of its 1489 green characters are 'g'. At
#: `cluster_13876325_281116670` phase 4 the north and south LEFT turns are 'g' while the opposing
#: through movements are 'G' -- read 'g' as protected and that phase has two head-on conflicts;
#: read it as red and the junction loses both left turns.
_PERMISSIVE = frozenset("gso")

#: Most-permissive-first ordering over the RAW characters, used when one movement is served by
#: several links (one per lane): a vehicle picks a lane, so it gets the best signal any of its lanes
#: offers. 'G' beats 'g' so a protected lane is reported as protected. Ranking the characters rather
#: than the colours is what lets `SignalPlan.char` answer "protected or permissive?" at all.
_CHAR_RANK = {"G": 0, "g": 1, "s": 2, "y": 3, "u": 4, "o": 5, "O": 6, "r": 7}
_WORST_CHAR = "r"


def char_colour(ch: str) -> str:
    """One SUMO link-state character -> `GREEN` / `YELLOW` / `RED` / `OFF`.

    An unknown character is RED: the conservative reading, and loud in the stats rather than
    silently permissive."""
    return _CHAR_COLOUR.get(ch, RED)


def is_permissive(ch: str) -> bool:
    """True for a green that must give way ('g', 's') or a blinking-off signal ('o')."""
    return ch in _PERMISSIVE


def is_protected(ch: str) -> bool:
    """True only for 'G' -- green WITH right of way.

    The complement of `is_permissive` within the greens. A caller that runs gap acceptance at
    unsignalised nodes wants this: on 'G' the movement owns the junction, on 'g' it must still
    yield to conflicting traffic exactly as it would with no signal at all."""
    return ch == "G"


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

    def char_of(self, t: float, links) -> str:
        """The best RAW character for a movement served by SEVERAL links (one per lane).

        A driver chooses a lane, so a movement with one 'G' lane and one 'r' lane is 'G' for a
        vehicle that wants it. 'G' beats 'g' beats 'y' beats 'r' (`_CHAR_RANK`), so a movement whose
        lanes disagree about right of way is reported as PROTECTED only if some lane really is."""
        best = _WORST_CHAR
        rank = _CHAR_RANK[_WORST_CHAR]
        for li in links:
            ch = self.char_at(t, li)
            r = _CHAR_RANK.get(ch, rank + 1)
            if r < rank:
                best, rank = ch, r
                if r == 0:
                    break
        return best

    def colour_of(self, t: float, links) -> str:
        """Colour for a movement served by SEVERAL links (one per lane): the most permissive.

        Exactly `char_colour(char_of(...))` -- one ordering, so the colour and the character can
        never disagree about which lane won."""
        return char_colour(self.char_of(t, links))

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
    `char(J, A, B, t)` and is told 'G' / 'g' / 'y' / 'r' -- because that MOVEMENT's link index is
    what the phase string is indexed by. `colour()` is the same answer as GREEN / YELLOW / RED /
    OFF; prefer `char` wherever the answer feeds a yield decision, since 'G' and 'g' are both green
    and only 'G' grants right of way.

    `char(J, A, None, t)` (approach only, no intended exit) answers with the approach's PRIMARY
    movement: its straight-ahead link where it has one, otherwise its lowest link index. That is a
    documented degradation, not an equivalence: an approach's left turn and its through movement
    genuinely have different colours for part of every cycle at a junction with a turn phase. On
    InTAS's `cluster_13876325_281116670` the west approach is through-green for 27 s of its 90 s
    cycle and left-green for a different 6 s.

    Junctions and approaches are addressed by COORDINATE, never by node index. Node indices do not
    survive `roads.largest_strong_component`, which every directed import goes through and which
    returns no remap -- a plan keyed on indices would signalise the wrong junctions and look fine.
    Coordinates survive it exactly.

    `None` from `colour()` means "no program governs this" and the caller must fall back to its
    existing behaviour. That is what keeps every unsignalised map byte-identical."""

    __slots__ = ("programs", "_move", "_appr", "_primary", "stats", "collisions")

    def __init__(self):
        #: junction coordinate -> SignalProgram
        self.programs: dict[tuple[float, float], SignalProgram] = {}
        #: (junction, from) -> {to: (link, ...)}
        self._move: dict[tuple, dict[tuple, tuple]] = {}
        #: (junction, from) -> (link, ...) over every exit
        self._appr: dict[tuple, tuple] = {}
        #: (junction, from) -> the approach's primary link (straight where there is one)
        self._primary: dict[tuple, int] = {}
        #: [(coordinate, losing tls, winning tls), ...] -- two DIFFERENT programs landed on one
        #: junction coordinate, so one of them was discarded. Never silent: an importer whose node
        #: dedupe merged two signalised junctions would otherwise light half the movements from a
        #: program that does not govern them. 0 on InTAS.
        self.collisions: list[tuple] = []
        self.stats: dict = {}

    # -------------------------------------------------------------- build
    def add(self, node_xy, program: SignalProgram, movements=(), primary=None) -> None:
        """Attach `program` to the junction at `node_xy`.

        `movements` is ``[(from_xy, to_xy, [link, ...]), ...]``; `primary` is
        ``{from_xy: link}``. A junction may legitimately appear twice (a joined tls controls two
        junctions with one program) -- each gets its own movement set out of the shared state
        string. Two DIFFERENT programs on one coordinate is not legitimate: the first one wins and
        the clash is recorded in `collisions`."""
        key = _key(node_xy)
        prev = self.programs.get(key)
        if prev is not None and prev is not program:
            self.collisions.append((key, program.tls_id, prev.tls_id))
            return
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
             "unmapped_links": 0,          links at this junction with no movement (real count)
             "mapped_columns": 7}          state columns this junction can address

        Node indices are resolved to coordinates HERE and never used again. `nodes` must be the
        array the RECORDS were written against, not a later trim of it: the plan then keys on the
        right coordinates and a junction the trim removed is simply never queried."""
        plan = cls()
        pts = [(float(p[0]), float(p[1])) for p in nodes]
        n = len(pts)
        skipped = 0
        for rec in records or ():
            i = rec.get("node")
            if not isinstance(i, int) or not (0 <= i < n) or not rec.get("phases"):
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
                      "approaches": len(plan._appr),
                      "collisions": len(plan.collisions)}
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

    def approaches(self, node_xy):
        """``[(from_xy, (link, ...)), ...]`` -- every APPROACH this plan controls at `node_xy`.

        The movement queries above answer for one driver; this answers for a consumer that has to
        reason about the junction as a whole and has no `from` coordinate to offer -- a PEDESTRIAN
        deciding whether the arm it is about to cross is being served. Given an arm's bearing, the
        approaches (anti)parallel to it are the ones a crossing of that arm conflicts with, and
        `SignalProgram.char_of` over their pooled links answers "is any of that traffic green?".

        Sorted by coordinate so a consumer that iterates is deterministic. Empty for a junction no
        real program governs -- the same None-shaped answer `char` gives, and the same fallback."""
        key = _key(node_xy)
        return [(fk, ls) for (nk, fk), ls in sorted(self._appr.items()) if nk == key]

    def char(self, node_xy, from_xy=None, to_xy=None, t: float = 0.0):
        """The RAW SUMO character a vehicle at `from_xy -> node_xy -> to_xy` sees at `t`, or None.

        None means "not governed by a real program here" -- an unsignalised junction, a junction
        whose program was not imported, or an approach the program does not control (a slip road
        that bypasses the signal). The caller keeps its existing behaviour for None; that is the
        whole compatibility story.

        Prefer this over `colour` wherever the answer feeds a yield decision: 'G' and 'g' are both
        GREEN but only 'G' grants right of way, and 303 of InTAS's 1489 green characters are 'g'."""
        prog = self.programs.get(_key(node_xy))
        if prog is None:
            return None
        ls = self.links(node_xy, from_xy, to_xy)
        if not ls:
            return None
        return prog.char_of(t, ls)

    def colour(self, node_xy, from_xy=None, to_xy=None, t: float = 0.0):
        """`GREEN` / `YELLOW` / `RED` / `OFF` for that movement at `t`, or None. See `char`."""
        ch = self.char(node_xy, from_xy, to_xy, t)
        return None if ch is None else char_colour(ch)


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
          "junctions": 0, "links_total": 0, "links_mapped": 0, "links_unmapped": 0,
          "links_self_loop": 0, "movements": 0, "joined_tls": 0,
          # state columns that no vehicular connection addresses (pedestrian groups, and any
          # column lost to the graph trim). The engine can never turn these green; they are the
          # honest measure of how much of the imported program is actually reachable. Counted PER
          # RECORD, so a joined tls counts its shared string once for each junction it governs.
          "state_columns": 0, "state_columns_mapped": 0,
          # two SUMO approach edges that the importer collapsed onto ONE graph node: their
          # movements share a `from` key and their colours merge most-permissive-first.
          "approaches_merged": 0}
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
            from_edges: dict[int, set] = {}
            mapped_cols: set = set()
            unmapped = 0
            for c in by_node[sumo_node]:
                st["links_total"] += 1
                li = c.getTLLinkIndex()
                fe = edge_endpoints(c.getFrom().getID())
                te = edge_endpoints(c.getTo().getID())
                if fe is None or te is None:
                    st["links_unmapped"] += 1
                    unmapped += 1
                    continue
                frm, to = fe[0], te[1]
                if frm == gi or to == gi or frm == to:
                    # the graph collapsed one of the two arms onto the junction itself; there is no
                    # movement to attach the link to. Counted, never guessed at.
                    st["links_self_loop"] += 1
                    unmapped += 1
                    continue
                st["links_mapped"] += 1
                mapped_cols.add(li)
                moves.setdefault((frm, to), set()).add(li)
                from_edges.setdefault(frm, set()).add(c.getFrom().getID())
                rank = (_DIR_RANK.get(c.getDirection(), 9), li)
                if frm not in best_dir or rank < best_dir[frm]:
                    best_dir[frm] = rank
            if not moves:
                continue
            rec = prog.to_record()
            rec["node"] = gi
            rec["movements"] = [[a, b, sorted(v)] for (a, b), v in sorted(moves.items())]
            rec["primary"] = [[a, r[1]] for a, r in sorted(best_dir.items())]
            # links of THIS junction that could not be turned into a movement (never a stub: an
            # importer that silently dropped an approach would otherwise leave no trace at all)
            rec["unmapped_links"] = unmapped
            # how many of the program's state columns this junction can actually address
            rec["mapped_columns"] = len(mapped_cols)
            records.append(rec)
            st["state_columns"] += prog.n_links
            st["state_columns_mapped"] += len(mapped_cols)
            st["approaches_merged"] += sum(1 for v in from_edges.values() if len(v) > 1)
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


def dense_link_indices(conns) -> dict:
    """``tls id -> {true linkIndex: densely renumbered index}`` -- the plausible WRONG mapping.

    This is what you get if you size the state string by the connection count and hand out indices
    in order, instead of reading each connection's own ``linkIndex``. It is identical to the truth
    wherever the controlled indices are contiguous from 0, and slides everything above a gap down by
    one for each missing column. On InTAS it moves 17 of 1042 connections, at exactly the three
    junctions whose programs reserve a column for a pedestrian group."""
    out: dict[str, dict] = {}
    for tls_id, cs in conns.items():
        seen = sorted({c.getTLLinkIndex() for c in cs})
        out[tls_id] = {li: k for k, li in enumerate(seen)}
    return out


def foe_pairs(net, tls_id, *, conns=None) -> list[tuple[int, int]]:
    """The TL link-index pairs at `tls_id` that netconvert's geometry says PHYSICALLY CROSS.

    Read off the junction's ``<request ... foes=...>`` matrix -- a source the ``<tlLogic>`` never
    touches -- and translated back into the link indices the phase string is addressed by, so a
    caller can assert directly that no phase gives both members of a pair a protected green.

    Pairs are ``(a, b)`` with ``a < b`` and de-duplicated: several lane-to-lane connections can
    share one link index, and one conflicting lane pair is enough to make the movement pair
    conflicting."""
    conns = tls_connections(net) if conns is None else conns
    cs = conns.get(tls_id, ())
    out: set = set()
    for i, a in enumerate(cs):
        na = a.getFrom().getToNode()
        ia = na.getLinkIndex(a)
        if ia < 0:
            continue
        for b in cs[i + 1:]:
            if b.getFrom().getToNode() is not na:
                continue                        # a joined tls: different junction, cannot conflict
            ib = na.getLinkIndex(b)
            if ib < 0 or not na.areFoes(ia, ib):
                continue
            la, lb = a.getTLLinkIndex(), b.getTLLinkIndex()
            if la != lb:
                out.add((min(la, lb), max(la, lb)))
    return sorted(out)


def audit_link_indices(net, *, shifts=(0,), programs=None, dense=False,
                       detail=False) -> list[dict]:
    """Re-derive the connection -> phase-index mapping from the junction FOE MATRIX and report the
    disagreement, for the true mapping and for any deliberately corrupted one.

    THE POINT. ``<request index=... foes=...>`` is netconvert's geometric conflict matrix; the
    ``<tlLogic>`` never touches it. So "two movements that physically cross both show 'G'" is an
    independent test of the index mapping. The junction request index is a second ordering as well
    as a second source: on InTAS it equals ``linkIndex`` for 1001 of 1042 connections and differs
    for 41, in 6 junctions.

    MEASURED on InTAS. shift 0 -> 11 conflicting pairs of 1951 (4 junctions); +1 -> 319 of 1548
    (78); -1 -> 456 of 1714 (95); +2 -> 259 of 1273 (66); -2 -> 366 of 1415 (67). ``dense=True``
    (the realistic bug, see `dense_link_indices`) -> 18 of 1944 (6). Both alarms fire.

    A residual at shift 0 is expected and is a property of the source net: 3 of the 11 are two lanes
    of one approach merging into one exit lane, which netconvert always calls a foe, and 8 are two
    InTAS programs (``279299817``, ``gneJ144``) that genuinely serve crossing movements
    protected-green. What must never be true is that a corrupted mapping looks comparable.

    Returns one dict per shift: ``{"mapping", "shift", "conflicts", "pairs", "junctions",
    "out_of_range"}``, plus ``"detail"`` -- ``[(tls, phase, state, link_a, link_b), ...]`` -- when
    `detail` is set. Requires `net` read with ``withPrograms=True`` (and `withFoes`, sumolib's
    default)."""
    programs = programs_from_net(net) if programs is None else programs
    conns = tls_connections(net)
    remap = dense_link_indices(conns) if dense else None
    out = []
    for shift in shifts:
        conflicts = pairs = oob = 0
        bad: set = set()
        rows: list = []
        for tls_id, prog in sorted(programs.items()):
            cs = conns.get(tls_id, ())
            base = remap[tls_id] if remap is not None else None
            for k, state in enumerate(prog.states):
                prot = []
                for c in cs:
                    raw = c.getTLLinkIndex()
                    li = (base[raw] if base is not None else raw) + shift
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
                            if detail:
                                rows.append((tls_id, k, state,
                                             a.getTLLinkIndex(), b.getTLLinkIndex()))
        rec = {"mapping": "dense" if dense else "linkIndex", "shift": shift,
               "conflicts": conflicts, "pairs": pairs,
               "junctions": len(bad), "out_of_range": oob}
        if detail:
            rec["detail"] = rows
        out.append(rec)
    return out
