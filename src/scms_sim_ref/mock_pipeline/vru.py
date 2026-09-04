"""Road-following pedestrians: sidewalks, crossings, and crossing behaviour for VRUs.

WHY THIS EXISTS. Until now a VRU was placed `offroad_tol_m * (1.3..2.0)` metres off a random
network node in BOTH axes and then walked in a dead-straight line at a constant `vru_speed_mps`
for its whole life (`run.make_vru` + `Vehicle.true_state`'s no-trip branch). It never used a
footway, never crossed at a crossing, and walked through buildings, carriageways and junctions
alike. That is not a cosmetic defect in THIS project: it ships a VRU-impersonation attack family
and a VAM message path, and an attacker that claims `station_type="vru"` is only distinguishable
from a genuine pedestrian if the genuine pedestrian behaves like one. With random-walk VRUs the
detector is separating two kinds of implausible, and nothing map-shaped can be used as evidence.

WHAT THIS BUILDS, from geometry that already exists (no new download, no new dependency):

  * SIDEWALKS -- one polyline per side of every road edge, offset OUTSIDE the carriageway. The
    offset is derived, not guessed: `roads._LaneFrameMixin` puts the a->b carriageway at
    `side * 0.5 * L_ab * W` off the centreline in its own left normal and the b->a carriageway at
    the mirror of that, so a two-way road occupies `[-L_ab*W, +L_ba*W]` in the a->b left-normal
    frame (right-hand traffic; the roles swap for `drive_side="left"`). A one-way road, and any
    road on a network without `enable_directed_lanes()`, instead has its lanes spread symmetrically
    about the centreline by `run.py` (`lane_off = (i - (L-1)/2) * lane_width_m`), so its half-width
    is `L*W/2` on BOTH sides. `_halfwidths` reproduces exactly those two cases; the sidewalk centre
    then sits at `halfwidth + kerb_clearance_m + sidewalk_width_m/2`, i.e. strictly outside the
    tarmac with a documented kerb gap. Nothing here mutates the road network.

    Own-road arithmetic is necessary and not sufficient -- a footway correctly offset from road A
    lands on road B at an acute-angle junction -- so `_clear_of_carriageways` then PUSHES (bounded,
    per-vertex) and TRIMS (unbounded) against every carriageway on the map. The invariant it buys is
    asserted at build time and measured on InTAS below: no kept sidewalk position is on a lane.

MEASURED, InTAS `ingolstadt.net.xml` (4,344 directed edges, 3,289 junctions, 109 real signalised
junctions), 400 VRUs x 90 s at 1.0 s = 36,000 sampled positions, seed 7, same protocol both columns:

                                          BEFORE (today)      AFTER (this module)
    on a pedestrian-legal area              5.59%               100.00%
    distance to nearest legal area  p50     14.247 m              0.000 m
                                    p95     80.621 m              0.000 m
                                    max    197.190 m              0.000 m
    stray beyond the legal strip    max    196.190 m              0.000 m
    distance to nearest ROAD        p50     17.411 m              3.831 m
                                    p95     79.120 m              8.500 m
                                    max    187.250 m             20.743 m
                                    mean    24.481 m              4.145 m
    beyond `offroad_tol_m` (15 m)           54.47%                0.12%
    INSIDE a traffic lane                   17.64% (max 13.94 m)  0.00% of 27,750 sidewalk samples
                                                                  (worst clearance -0.024 m)

`road_dist` is measured against the map's REAL drivable surface (`CustomNetwork.set_road_surface`:
23,705 SUMO lane segments + 3,332 junction discs), which is what `mapOffRoad` sees. `INSIDE a
traffic lane` is measured against the engine's own carriageway frame -- the lanes vehicles are
actually placed in -- which is the stricter and more load-bearing of the two.

  * CROSSINGS -- at every junction, one per arm, perpendicular to that arm and set back from the
    junction centre by the corner trim, spanning kerb to kerb. They fall out of the geometry for
    free: a crossing is exactly the segment joining an arm's two trimmed sidewalk ends.

  * CORNERS -- at each junction the arms are ordered by bearing and consecutive arms are joined
    `arm_i.LEFT <-> arm_{i+1}.RIGHT` (the standard corner relation: walking anticlockwise round a
    junction you leave one arm on its left kerb and arrive at the next on its right kerb). This is
    what makes the pedestrian graph CONNECTED without ever entering the junction box.

  * BEHAVIOUR -- `PedestrianWalk` is a `Trip`-compatible mobility object (`.state(t) ->
    (x, y, speed, heading)`), so it drops straight into `Vehicle.true_state`'s existing
    `self.trip is not None` branch with no change to the kinematics. A pedestrian walks along
    sidewalks, WAITS at the kerb before a crossing (claimed speed 0, heading facing the crossing)
    and then traverses it. Waits are signal-aware where a signal exists and a gap-acceptance
    surrogate where one does not.

VEHICLES DO NOT YIELD TO PEDESTRIANS. Stated plainly rather than implied, because it bounds what
this module can honestly claim. `run.car_follow` opens with `active_list = [v for v in active_list
if v.cf]`; `make_vru` never sets `cf`, so a VRU is absent from the `snap`/`buckets` the IDM leader
search reads, absent from the `claims` dict gap-acceptance ranks, and absent from the signal logic.
No pedestrian is ever an IDM leader, a gap-acceptance claimant, or a reason for a red. A pedestrian
on a crossing is geometrically overrun by traffic rather than braked for, and the crossing wait
modelled here is a pedestrian waiting for a signal or a gap, never a vehicle waiting for a
pedestrian. MEASURED consequence: with the crossing behaviour on, a VRU spends 12.10% of its samples
on a crossing and a further 11.64% on a corner link -- roughly a quarter of its life on tarmac that
no driver in this model can see. That is a position claim the detectors observe, NOT a collision
risk the engine resolves, and no safety number may be read off it. Wiring a real yield is a change
to `run.car_follow` (a virtual stopped leader at the pedestrian's arc position, the same mechanism
`_lights` and `_gap` already use), which this module does not own; see the WIRING report.

THREAT MODEL, because a mobility model that ships beside a VRU-impersonation attack has one. The
`vruImpersonation` check reads ONLY kinematics -- a speed arm (`claimed_speed / vru_max_plausible_
speed_mps`) and a jump arm (`|claimed - lagged_ref| / (vmax*dt + z*conf + outlier)`) -- and never
the map. Realistic walking therefore does NOT make that check discriminate better: a genuine VRU's
step displacement is 1.80 m either way (jump arm 0.061 vs 0.063 of its firing point) and its speed
arm only relaxes, 0.180 flat -> 0.168 mean, because kerb waits put 6.6% of its samples at exactly
0 m/s. What realistic walking buys is a discriminator that CANNOT EXIST today. Applying a map test
(`legal_distance <= 1 m`) to any beacon declaring `station_type="vru"`, measured on InTAS:

    genuine VRU, this module        passes 100.00%   (false-revocation rate 0.00%)
    genuine VRU, today's random walk passes  5.59%   (false-revocation rate 94.41%) <- unusable
    VruImpersonation (honest position, so on a lane)   passes  9.21% -> 90.79% caught
    VruPositionSpoof (claims a 30-60 m teleport)       passes  6.23% -> 93.77% caught

and restricted to SIDEWALK surface only (crossings and corners are ON tarmac by construction):
genuine 80.86%, impersonator 0.41% (99.59% caught), position-spoofer 4.62% (95.38% caught). That is
the point of this module: today the VRU detector exemptions (`mapOffRoad` + the vehicle-kinematic
checks are suppressed for a vru-declared beacon) are load-bearing because genuine VRUs are 54.47%
beyond `offroad_tol_m`; afterwards they are 0.12% beyond it, and the exemption stops being the
attacker's free prize. Building that detector is NOT this module's job and is not done here.

DETERMINISM. Every draw comes from `random.Random(f"{seed}:vruwalk:{vid}")`, a dedicated
string-keyed stream that exists nowhere else, so enabling sidewalks perturbs no other stream --
not the vehicle fleet, not the existing `f"{seed}:vru:{vid}"` placement stream, not the channel.
With sidewalks off, nothing in this module is imported into the run at all.

WIRED INTO THE ENGINE by ``PipelineConfig.sidewalks`` / ``--sidewalks`` (with `sidewalk_width_m`,
`kerb_clearance_m`, `crossing_wait_max_s`), default OFF and gated on ``vru_pct > 0``, so no pinned
digest can see it. `run.py` builds the network once after the map, and `make_vru` sets
``trip=walk`` -- that substitution IS the behavioural change, because `Vehicle.true_state` already
had a ``trip is not None`` branch. WHICH CROSSINGS ARE SIGNALISED comes from the same source the
VEHICLES use -- every junction under `traffic_lights`, the imported `<tlLogic>` junctions under
`real_signals`, none otherwise -- so the two populations at a junction are on one clock; under
`real_signals` the walk signal is the complement of that ARM's own program links
(`run._make_ped_signal_fn`) rather than of a second fixed cycle.

MEASURED end to end on InTAS (`real_signals`, 300 s, dt 0.5, seed 42, 6,065 sampled VRU positions),
both columns scored by `measure_positions` against ONE derived pedestrian layer:

                                       sidewalks OFF        sidewalks ON
    on a pedestrian-legal area            2.65 %              100.00 %
    distance to a legal area  p95        66.827 m               0.000 m
    distance to the nearest road  p95    58.220 m               8.500 m

STILL NOT WIRED, and stated rather than implied: a pedestrian-aware YIELD. `run.car_follow` opens
with `active_list = [v for v in active_list if v.cf]` and `make_vru` never sets `cf`, so a VRU is
still absent from the IDM leader search, from the gap-acceptance claims and from the signal logic.
A pedestrian on a crossing is overrun rather than braked for; see VEHICLES DO NOT YIELD above.

A DEFECT FOUND AND FIXED WHILE WIRING THIS: `_crossing_wait` used to hand `signal_fn` the
CROSSING's own heading, and a derived crossing joins one arm's two kerbs -- it runs PERPENDICULAR
to the traffic it conflicts with. The signal was therefore being asked about the CROSS street: the
pedestrian was held through the conflicting movement's red and released into its green, exactly
inverted. The arm bearing is now recorded on the link at build time (`add_link(..., arm=...)`) and
is what gets asked.
"""
from __future__ import annotations

import bisect
import math
import random

# Offsetting a polyline sideways with mitred joins is `roads`'s own primitive and is the exact
# operation the carriageway frame uses; reusing it keeps sidewalk joins consistent with the
# carriageway joins they parallel (a bevel where the carriageway bevels).
from .roads import _offset_polyline

__all__ = ["SidewalkNetwork", "PedestrianWalk", "build_sidewalks", "engine_signal_fn",
           "measure_positions", "legacy_offroad_positions", "SIDEWALK_WIDTH_M", "KERB_CLEARANCE_M",
           "UNSIGNALISED_MAX_WAIT_S", "PED_WALK_FRACTION"]

# --------------------------------------------------------------------------- #
# Documented defaults. These are the numbers the offset story above is told in.
# --------------------------------------------------------------------------- #
#: Footway width (m). German urban standard (RASt 06) puts a two-way footway at 2.5 m and an
#: absolute minimum at 1.5 m; 2.0 m is a defensible middle that also keeps the legal-area test
#: (+/- half this) from being generous enough to hide a metre of error.
SIDEWALK_WIDTH_M = 2.0
#: Gap between the outer edge of the carriageway (the kerb line) and the inner edge of the
#: footway (m). Stands for kerb + gutter + any verge. Small and explicit so the sidewalk centre
#: is provably outside the tarmac by `KERB_CLEARANCE_M` at minimum.
KERB_CLEARANCE_M = 0.5
#: Longest a pedestrian waits at an UNSIGNALISED crossing before accepting a gap (s). A surrogate
#: for gap acceptance, which cannot be modelled honestly here because vehicles do not yield and
#: pedestrians are not in the car-following state -- there is no gap to accept or reject.
UNSIGNALISED_MAX_WAIT_S = 8.0
#: Fraction of a signal half-cycle during which a pedestrian may START crossing. A real pedestrian
#: phase gives green-man for part of the conflicting movement's red, then a clearance interval.
PED_WALK_FRACTION = 0.7
#: Probe step when solving "when does the signal next let me cross?" (s). Deterministic, no RNG.
_SIGNAL_PROBE_S = 0.5
#: Arc-length step at which a candidate sidewalk polyline is tested against the carriageway layer
#: (m). Vertex-only testing is NOT enough: a straight sidewalk segment between two clear vertices
#: can still cut the corner of a neighbouring carriageway, and at 8,688 sidewalk sides on InTAS that
#: is not a hypothetical -- see `_clear_of_carriageways`.
_TARMAC_PROBE_M = 1.0
#: A sidewalk fragment shorter than this after trimming is not a footway, it is a shard; drop it.
MIN_SIDEWALK_M = 4.0
#: How far a footway may be pushed BEYOND its own kerb line to dodge a neighbouring carriageway (m).
#: One lane's worth: enough to clear a service road or a slip lane that the graph placed within a
#: lane of this one, and small enough that a pushed footway is still recognisably this road's
#: footway. Anything that cannot be cleared inside this budget is TRIMMED AWAY, not pushed further.
MAX_EXTRA_OFFSET_M = 3.5
#: Slack allowed on a crossing's or corner's derived length bound (m). Absorbs road curvature at the
#: junction and a modest asymmetric trim; anything past it is not a crossing or a corner.
LINK_SLACK_M = 6.0
#: How far a mapped OSM footway END may be joined to the derived kerb by a connector (m). A footway
#: mapped as its own way stops at the property line or a few metres short of the kerb; this closes
#: that gap. Larger than a carriageway half-width would start inventing links across roads.
FOOTWAY_SNAP_M = 12.0
#: Link kinds.
SIDEWALK, CROSSING, CORNER = 0, 1, 2
_KIND_NAME = {SIDEWALK: "sidewalk", CROSSING: "crossing", CORNER: "corner"}
#: Sanity bound on the derived pedestrian graph. A 4000-node / 12000-edge road map (the
#: `CustomNetwork` ceiling) yields 4 ped nodes and ~4 links per road edge.
MAX_PED_LINKS = 120_000
#: Sanity bound on ONE pedestrian's itinerary. The walk loop terminates on travel TIME, and every
#: leg consumes some, so it always ends -- but a graph full of very short corner links could end it
#: after tens of thousands of legs and a `PedestrianWalk` with a point array to match. Measured on
#: InTAS a 90 s walk uses 5.78 legs on average and 26 at worst, so this bound is ~40x the observed
#: maximum: it catches a degenerate map, never a normal one.
MAX_WALK_LEGS = 1000


# --------------------------------------------------------------------------- #
# geometry helpers
# --------------------------------------------------------------------------- #
def _polyline_cum(pts: list) -> list:
    cum = [0.0]
    for (ax, ay), (bx, by) in zip(pts, pts[1:]):
        cum.append(cum[-1] + math.hypot(bx - ax, by - ay))
    return cum


def _sub_polyline(pts: list, cum: list, s0: float, s1: float) -> list:
    """The piece of `pts` between arc-lengths s0 < s1, with the two cut points inserted."""
    total = cum[-1]
    s0 = min(max(s0, 0.0), total)
    s1 = min(max(s1, s0), total)

    def at(s):
        k = bisect.bisect_left(cum, s)
        if k <= 0:
            return pts[0]
        if k >= len(pts):
            return pts[-1]
        seg = cum[k] - cum[k - 1]
        f = (s - cum[k - 1]) / seg if seg > 0 else 0.0
        (ax, ay), (bx, by) = pts[k - 1], pts[k]
        return (ax + (bx - ax) * f, ay + (by - ay) * f)

    out = [at(s0)]
    for k in range(len(pts)):
        if s0 < cum[k] < s1:
            out.append(pts[k])
    end = at(s1)
    if end != out[-1]:
        out.append(end)
    if len(out) < 2:
        out.append(out[0])
    return out


def _pt_seg_dist(px, py, ax, ay, bx, by) -> float:
    dx, dy = bx - ax, by - ay
    dd = dx * dx + dy * dy
    if dd <= 0.0:
        return math.hypot(px - ax, py - ay)
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / dd))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def _halfwidths(lanes_ab: int, lanes_ba: int, oneway: bool, lane_w: float, side: float,
                directed_lanes: bool, lanes_global: int) -> tuple[float, float]:
    """Carriageway half-width (m) along the +left-normal and the -left-normal of a->b.

    Reproduces exactly what the engine does with the tarmac, which is what makes "outside the
    carriageway" a fact rather than an assertion:

    * `directed_lanes` ON and the road two-way -- each direction owns its own strip; with
      right-hand traffic (`side < 0`) a->b sits on the -normal side with `L_ab` lanes and b->a on
      the +normal side with `L_ba`, so the road spans `[-L_ab*W, +L_ba*W]`. Left-hand traffic
      swaps the two.
    * otherwise (one-way road, or a network that never called `enable_directed_lanes`) -- `run.py`
      spreads lanes symmetrically about the centreline, `lane_off = (i-(L-1)/2)*W`, so the road is
      `L*W/2` wide on each side. `L` takes the global `n_lanes` into account because that is the
      count `run.py` actually uses when the network carries no per-direction frame.
    """
    if directed_lanes and not oneway:
        if side < 0.0:                       # right-hand traffic
            return max(1, lanes_ba) * lane_w, max(1, lanes_ab) * lane_w
        return max(1, lanes_ab) * lane_w, max(1, lanes_ba) * lane_w
    if directed_lanes:                       # one-way road keeps the centreline (offset 0)
        total = max(1, lanes_ab, lanes_ba)
    else:
        total = max(1, lanes_global, lanes_ab, lanes_ba)
    h = 0.5 * total * lane_w
    return h, h


class _CarriagewayIndex:
    """Where the tarmac is: every road segment with the half-width it occupies on each side.

    Exists because "offset the sidewalk outside ITS OWN carriageway" is necessary and NOT
    sufficient. On a real imported map two physical roads run within metres of each other -- a
    service road beside an arterial, two arms meeting at a sharp angle, a slip road -- and a
    sidewalk correctly offset from road A lands on road B's tarmac. Measured on InTAS
    (`ingolstadt.net.xml`, 4,344 directed graph edges, 8,688 candidate footway sides): with a single
    global push bounded at 6 m, 9.34% of the 33,390 sidewalk VERTICES were still inside some
    carriageway, up to 12.92 m deep, while every one of them was correctly outside its own -- and
    the p50 vertex sat at exactly -1.500 m, i.e. its own kerb clearance, confirming that the own-road
    arithmetic was never the problem. That is exactly the class of defect that still looks plausible
    in a plot. `_clear_of_carriageways` replaces the global push with a per-vertex push plus a trim
    and drives the residual to 0 of 27,451 sampled sidewalk positions (worst clearance -0.023 m).
    """

    __slots__ = ("segs", "cell", "index", "reach")

    def __init__(self, e_pts: list, e_half: list):
        self.segs: list[tuple] = []                # (ax, ay, bx, by, half_plus, half_minus)
        for pts, (wp, wm) in zip(e_pts, e_half):
            for p, q in zip(pts, pts[1:]):
                if p != q:
                    self.segs.append((p[0], p[1], q[0], q[1], wp, wm))
        self.reach = max((max(s[4], s[5]) for s in self.segs), default=0.0)
        lens = sorted(math.hypot(s[2] - s[0], s[3] - s[1]) for s in self.segs) or [50.0]
        self.cell = min(200.0, max(2.0 * self.reach + 1.0, 2.0 * lens[len(lens) // 2], 20.0))
        ix: dict[tuple[int, int], set] = {}
        for k, (ax, ay, bx, by, _p, _m) in enumerate(self.segs):
            steps = max(1, int(math.hypot(bx - ax, by - ay) // self.cell) + 1)
            for c in range(steps):
                t0, t1 = c / steps, (c + 1) / steps
                x0, y0 = ax + (bx - ax) * t0, ay + (by - ay) * t0
                x1, y1 = ax + (bx - ax) * t1, ay + (by - ay) * t1
                for ci in range(int(min(x0, x1) // self.cell), int(max(x0, x1) // self.cell) + 1):
                    for cj in range(int(min(y0, y1) // self.cell),
                                    int(max(y0, y1) // self.cell) + 1):
                        ix.setdefault((ci, cj), set()).add(k)
        self.index = {c: tuple(sorted(v)) for c, v in ix.items()}

    def candidates(self, x0: float, y0: float, x1: float, y1: float) -> tuple:
        """Every segment whose carriageway could reach the box [x0,x1] x [y0,y1], de-duplicated.

        The cell is at least `2 * max_half_width` wide, so widening the box by one cell in each
        direction and taking those cells is EXACT, not a heuristic. Gathering candidates ONCE for a
        whole sidewalk polyline instead of per sample is the difference between one index query and
        several hundred, which is where the build time went.
        """
        cell = self.cell
        out: set = set()
        for i in range(int(x0 // cell) - 1, int(x1 // cell) + 2):
            for j in range(int(y0 // cell) - 1, int(y1 // cell) + 2):
                out.update(self.index.get((i, j), ()))
        segs = self.segs
        return tuple(segs[k] for k in sorted(out))

    @staticmethod
    def penetration_in(x: float, y: float, cand) -> float:
        """How far INSIDE the deepest carriageway of `cand` (m). <= 0 means clear of all of them."""
        worst = -math.inf
        for ax, ay, bx, by, hp, hm in cand:
            dx, dy = bx - ax, by - ay
            dd = dx * dx + dy * dy
            if dd <= 0.0:
                continue
            t = ((x - ax) * dx + (y - ay) * dy) / dd
            t = 0.0 if t < 0.0 else (1.0 if t > 1.0 else t)
            ex, ey = x - (ax + t * dx), y - (ay + t * dy)
            d = math.sqrt(ex * ex + ey * ey)
            lat = (-dy) * (x - ax) + dx * (y - ay)          # +ve = left of a->b (sign only)
            half = hp if lat >= 0.0 else hm
            if half - d > worst:
                worst = half - d
        return worst

    def penetration(self, x: float, y: float) -> float:
        """How far INSIDE the deepest carriageway (m). <= 0 means outside every carriageway."""
        return self.penetration_in(x, y, self.candidates(x, y, x, y))


def _sample_polyline(pts: list, probe_m: float):
    """(arc-lengths, points) sampling `pts` at every vertex AND every `probe_m` along it.

    The sample set always contains both endpoints and every vertex, so a polyline shorter than one
    probe step is still tested, and the returned arc-lengths are strictly increasing.
    """
    cum = _polyline_cum(pts)
    total = cum[-1]
    marks = set(cum)
    s = probe_m
    while s < total:
        marks.add(s)
        s += probe_m
    ss = sorted(marks)
    out = []
    for s in ss:
        k = bisect.bisect_left(cum, s)
        if k <= 0:
            out.append(pts[0])
        elif k >= len(pts):
            out.append(pts[-1])
        else:
            seg = cum[k] - cum[k - 1]
            f = (s - cum[k - 1]) / seg if seg > 0 else 0.0
            (ax, ay), (bx, by) = pts[k - 1], pts[k]
            out.append((ax + (bx - ax) * f, ay + (by - ay) * f))
    return ss, out


def _dense_penetration(pts: list, carr: "_CarriagewayIndex", probe_m: float, cand=None):
    """(arc-lengths, penetration at each) along `pts`. `cand` = a candidate set from
    `_CarriagewayIndex.candidates` covering the whole polyline (gathered once by the caller)."""
    ss, sp = _sample_polyline(pts, probe_m)
    if cand is None:
        xs = [p[0] for p in sp]
        ys = [p[1] for p in sp]
        cand = carr.candidates(min(xs), min(ys), max(xs), max(ys))
    pen_in = carr.penetration_in
    return ss, [pen_in(x, y, cand) for x, y in sp]


def _clear_of_carriageways(core: list, sign: float, base: float, carr: "_CarriagewayIndex", *,
                           kerb: float, max_extra: float, probe_m: float, min_keep_m: float):
    """Offset `core` by `sign * base` and make the result provably clear of ALL tarmac.

    Two operators, in this order, because they answer two different failure modes:

    1. PUSH (bounded, per-vertex). Offsetting a footway outside its OWN carriageway is necessary and
       not sufficient: on an imported city a second physical road runs within metres of the first --
       a service road beside an arterial, a slip lane, two arms meeting at 20 degrees -- and a
       correctly-offset footway lands on THAT road's tarmac. Where the conflict is a metre or two,
       pushing the footway a lane's width further out is the right physical answer and is what a
       real street does. The push is per-VERTEX (segment offset = max of its two ends) rather than
       one number for the whole polyline, because the conflict is local: a uniform push large enough
       to clear a junction throat drives the rest of the footway through the buildings behind it.

    2. TRIM (unbounded, and the reason the invariant HOLDS). Where the push cannot clear the tarmac
       inside `max_extra`, the honest answer is that this stretch has no derivable footway, so the
       stretch is CUT OUT and only the longest surviving run is kept. Pushing harder instead would
       buy the invariant with a footway 10 m inside a building, and dropping the check would put
       "legal pedestrian area" on a traffic lane -- which is worse than the random walk this
       replaces, because a detector would then have map evidence saying the lane is a footway.

    Returns `(pts | None, trimmed_m, worst_penetration)`. `pts is None` means nothing survived. The
    caller asserts `worst_penetration <= 0` over what IS returned; that is the whole point.
    Deterministic, draws no RNG.
    """
    n = len(core)
    ex = [0.0] * n
    spts = None
    # ONE candidate gather for this footway, over the widest box any of the four rounds can reach:
    # the core polyline grown by the full offset budget in every direction. Every round then scores
    # against the same exact set (the box is a superset of each round's own, so nothing is missed).
    reach = base + max_extra
    cxs = [p[0] for p in core]
    cys = [p[1] for p in core]
    cand = carr.candidates(min(cxs) - reach, min(cys) - reach, max(cxs) + reach, max(cys) + reach)
    for _round in range(4):
        offs = [sign * (base + max(ex[j], ex[j + 1])) for j in range(n - 1)]
        spts = _offset_polyline(core, offs)
        _ss, pen = _dense_penetration(spts, carr, probe_m, cand)
        if max(pen) <= 0.0:
            return spts, 0.0, max(pen)
        # raise the offset at every VERTEX whose own position is inside tarmac; a dense sample that
        # is inside between two clear vertices is left to the trim below (pushing for it would move
        # geometry that is already correct).
        bumped = False
        for k, (px, py) in enumerate(spts):
            p = carr.penetration_in(px, py, cand)
            if p > 0.0 and ex[k] < max_extra:
                ex[k] = min(max_extra, ex[k] + p + kerb)
                bumped = True
        if not bumped:
            break
    ss, pen = _dense_penetration(spts, carr, probe_m, cand)
    total = ss[-1]
    # longest contiguous run of CLEAR samples, in arc-length
    best = (0.0, 0.0, -1.0)                      # (s0, s1, length)
    i = 0
    while i < len(ss):
        if pen[i] > 0.0:
            i += 1
            continue
        j = i
        while j + 1 < len(ss) and pen[j + 1] <= 0.0:
            j += 1
        if ss[j] - ss[i] > best[2]:
            best = (ss[i], ss[j], ss[j] - ss[i])
        i = j + 1
    if best[2] < 0.0:
        return None, total, max(pen)
    # pull the cut ends in by one probe step: the run boundary sits BETWEEN a clear sample and a
    # dirty one, so the true boundary is somewhere in that gap and the conservative end is ours.
    s0 = best[0] + (probe_m if best[0] > 0.0 else 0.0)
    s1 = best[1] - (probe_m if best[1] < total else 0.0)
    if s1 - s0 < min_keep_m:
        return None, total, max(pen)
    cum = _polyline_cum(spts)
    kept = _sub_polyline(spts, cum, s0, s1)
    _ss2, pen2 = _dense_penetration(kept, carr, probe_m, cand)
    if max(pen2) > 0.0:                          # the shrink was not enough: refuse rather than lie
        return None, total, max(pen2)
    return kept, total - (s1 - s0), max(pen2)


def engine_signal_fn(net, half_cycle_s: float):
    """A `signal_fn(junction_xy, arm_heading_deg, t) -> bool` that agrees with `run._light_green`.

    A pedestrian crossing an arm conflicts with the traffic travelling ALONG that arm, so the walk
    signal is the complement of that arm's vehicle green, minus a clearance tail: a pedestrian may
    START crossing during the first `PED_WALK_FRACTION` of the conflicting movement's red. The
    phase itself comes from `net.node_phase` -- the same stable 2-colouring the vehicle signals
    use -- so pedestrians and vehicles at a junction are on ONE clock rather than two.

    This is the fixed-time approximation the engine's own signals are; it is deliberately a
    parameter, not a hard-coded rule, so a real TLS program (per-junction phase state strings and
    durations imported from a SUMO net) can be substituted by passing a different `signal_fn`
    without this module changing at all.
    """
    half = max(1.0, float(half_cycle_s))

    def signal_fn(node_xy, arm_heading_deg: float, t: float) -> bool:
        phase = net.node_phase(node_xy)
        h = math.radians(arm_heading_deg)
        axis_x = abs(math.cos(h)) >= abs(math.sin(h))
        offset = (phase % 2) * half
        x_phase = int((t + offset) // half) % 2 == 0
        veh_green = (x_phase == axis_x)
        if veh_green:
            return False
        into_red = (t + offset) % half       # how far into the conflicting movement's red we are
        return into_red <= PED_WALK_FRACTION * half

    return signal_fn


# --------------------------------------------------------------------------- #
# the pedestrian network
# --------------------------------------------------------------------------- #
class SidewalkNetwork:
    """Sidewalks + crossings + corners derived from a road network, as a walkable graph.

    Node key `(edge_index, side, end)`: `side` 0 = left of a->b, 1 = right; `end` 0 = the `a` end,
    1 = the `b` end. Every road edge contributes 4 nodes, one sidewalk link, two crossings (one per
    junction) and its share of the corner links.

    All coordinates are in the SAME metric frame as the road network -- these polylines are offsets
    of the road polylines and nothing is re-projected, so a map imported through `osm.py` keeps the
    `(lat0, lon0, kx, ky)` registration the roads, the buildings and the SUMO import already share.
    """

    __slots__ = ("pts", "links", "adj", "node_xy", "junction_of", "signalised",
                 "_segs", "_seg_link", "_cell", "_index", "_cbox", "_max_ring",
                 "_walk_start_links", "_prov", "_road_bbox")

    def __init__(self):
        self.pts: list[tuple[float, float]] = []          # ped-graph node coordinates
        self.links: list[dict] = []                       # {"kind","a","b","pts","cum","len","node"}
        self.adj: list[list[tuple[int, int]]] = []        # node -> [(other node, link index)]
        self.node_xy: dict = {}                           # node key -> index
        self.junction_of: list = []                       # link index -> junction coord | None
        self.signalised: list[bool] = []                  # link index -> crossing is signalised
        self._walk_start_links: list[int] = []
        self._prov: dict = {}
        self._road_bbox = (0.0, 0.0, 0.0, 0.0)

    # ---------------------------------------------------------------- build #
    @classmethod
    def from_network(cls, net, *, lane_width_m: float = 3.5, lanes_per_dir: int = 1,
                     drive_side: str = "right", sidewalk_width_m: float = SIDEWALK_WIDTH_M,
                     kerb_clearance_m: float = KERB_CLEARANCE_M,
                     sidewalk_sides: dict | None = None, signal_nodes=None,
                     osm_footways=None, osm_crossings=None,
                     crossing_match_m: float = 25.0,
                     max_extra_offset_m: float = MAX_EXTRA_OFFSET_M,
                     probe_m: float = _TARMAC_PROBE_M,
                     min_sidewalk_m: float = MIN_SIDEWALK_M,
                     link_slack_m: float = LINK_SLACK_M,
                     footway_snap_m: float = FOOTWAY_SNAP_M) -> "SidewalkNetwork":
        """Derive the pedestrian network from `net` (any `roads` topology).

        `net` is read through its public surface only -- `geometry()` for nodes/edges, and, where
        the topology provides them, `edge_points` / `lanes_for` / `is_oneway` for curve geometry
        and per-direction lane counts. A `GridNetwork` or `RingNetwork` (which have none of those)
        gets straight edges and the global lane count, which is exactly what its vehicles use.

        `sidewalk_sides` maps a road-edge index to a set of sides to BUILD ({0}, {1}, {0,1} or
        set()); the OSM `sidewalk=*` tag is the source (see `osm.pedestrian_tag_stats`). Absent ->
        both sides, and on the measured evidence that is the right default rather than a shrug. On
        the cached Ingolstadt extract, of 347 drivable ways: 92 (26.51%) say nothing about a
        footway at all, 158 (45.53%) say `sidewalk=separate` -- the footway EXISTS but is mapped as
        its own way, so it arrives through `osm_footways` and the road should not also imply one --
        87 (25.07%) name sides (79 `both`, 5 `right`, 3 `left`), and only 10 (2.88%) say `no`.
        Suppressing a side is therefore a 2.88% correction to a 26.51% blind spot; deriving both
        sides and letting `osm_footways` add the separately-mapped reality is the honest default.

        `signal_nodes=None` signalises every junction -- what `cfg.traffic_lights` does today.
        `()` signalises none. Otherwise it is an iterable of either node INDICES into this network,
        or `(x, y)` COORDINATES. Prefer the coordinate form on an imported map, and this is not a
        style preference: `osm.py` reports `signal_nodes` as indices into the graph it built, and
        `roads.largest_strong_component` -- which a directed OSM import must run through -- REMAPS
        node indices and does not return the remap, so an index carried across that boundary
        silently signalises the wrong junction. Coordinates survive the remap unchanged.

        `osm_footways` / `osm_crossings` come from `osm.extract_footways` in the same projected
        frame: footways are ADDED as extra legal walking surface, and a crossing whose junction
        lies within `crossing_match_m` of an OSM `highway=crossing` node inherits that node's
        signalisation (`crossing=traffic_signals`) instead of the network-wide default.
        """
        self = cls()
        geo = net.geometry()
        nodes = [(float(p[0]), float(p[1])) for p in geo["nodes"]]
        raw_edges = [(int(e[0]), int(e[1])) for e in geo["edges"]]
        if not nodes or not raw_edges:
            raise ValueError("cannot derive sidewalks from a network with no nodes/edges")
        xs = [p[0] for p in nodes]
        ys = [p[1] for p in nodes]
        self._road_bbox = (min(xs), min(ys), max(xs), max(ys))

        directed_lanes = bool(getattr(net, "directed_lanes", False))
        lane_w = float(getattr(net, "_lane_w", lane_width_m) if directed_lanes else lane_width_m)
        side = float(getattr(net, "_side", -1.0)) if directed_lanes else (
            -1.0 if drive_side == "right" else 1.0)
        edge_points = getattr(net, "edge_points", None)
        lanes_for = getattr(net, "lanes_for", None)
        is_oneway = getattr(net, "is_oneway", None)
        # A GridNetwork / RingNetwork has no per-edge lane counts, but `enable_directed_lanes` DID
        # record the per-direction count the engine offsets its carriageways by. Reading it here is
        # what keeps the footway outside the tarmac on those topologies too; taking the caller's
        # default instead would put a 2-lanes-each-way grid's footway 3.5 m inside the far lane.
        if directed_lanes and lanes_for is None:
            lanes_per_dir = int(getattr(net, "_lanes_per_dir", lanes_per_dir))

        # ---- per-edge geometry, half-widths and sidewalk offsets -------------------- #
        e_pts: list[list] = []
        e_half: list[tuple[float, float]] = []            # carriageway half-widths (+n, -n)
        e_off: list[tuple[float, float]] = []             # (+left offset, -right offset magnitude)
        half = 0.5 * float(sidewalk_width_m)
        for a, b in raw_edges:
            pts = [tuple(p) for p in edge_points(a, b)] if edge_points else [nodes[a], nodes[b]]
            l_ab = int(lanes_for(a, b)) if lanes_for else lanes_per_dir
            l_ba = int(lanes_for(b, a)) if lanes_for else lanes_per_dir
            ow = bool(is_oneway(a, b)) if is_oneway else False
            wp, wm = _halfwidths(l_ab, l_ba, ow, lane_w, side, directed_lanes, lanes_per_dir)
            e_pts.append(pts)
            e_half.append((wp, wm))
            e_off.append((wp + kerb_clearance_m + half, wm + kerb_clearance_m + half))
        carr = _CarriagewayIndex(e_pts, e_half)

        # ---- corner trim: consistent per JUNCTION so both kerbs of a crossing line up  #
        arms: dict[int, list[tuple[int, int]]] = {}       # node -> [(edge index, end)]
        for k, (a, b) in enumerate(raw_edges):
            arms.setdefault(a, []).append((k, 0))
            arms.setdefault(b, []).append((k, 1))
        trim_at: dict[int, float] = {}
        for n, ar in arms.items():
            trim_at[n] = max(max(e_off[k]) for k, _end in ar) + kerb_clearance_m

        # ---- sidewalk polylines, trimmed at both ends ------------------------------- #
        def node_index(key, xy) -> int:
            i = self.node_xy.get(key)
            if i is None:
                i = len(self.pts)
                self.node_xy[key] = i
                self.pts.append(xy)
                self.adj.append([])
            return i

        def add_link(kind, na, nb, pts, junction=None, signalised=False, arm=None) -> int:
            cum = _polyline_cum(pts)
            if cum[-1] <= 1e-9 or na == nb:
                return -1
            if len(self.links) >= MAX_PED_LINKS:
                raise ValueError(f"pedestrian network exceeds {MAX_PED_LINKS} links -- the road "
                                 f"map is too large to derive sidewalks for")
            k = len(self.links)
            rec = {"kind": kind, "a": na, "b": nb, "pts": pts, "cum": cum, "len": cum[-1]}
            if arm is not None:
                # Bearing (deg) of the ROAD ARM this crossing crosses, which is NOT the crossing's
                # own heading: a derived crossing joins one arm's two kerbs, so it runs
                # PERPENDICULAR to the traffic it conflicts with. Recorded at build time because
                # only the builder knows which arm a crossing belongs to, and because a signal
                # answer taken off the crossing's own heading is exactly 90 degrees wrong -- it
                # holds the pedestrian through the conflicting movement's red and releases it into
                # the green. See `_crossing_wait`.
                rec["arm"] = float(arm) % 360.0
            self.links.append(rec)
            self.junction_of.append(junction)
            self.signalised.append(bool(signalised))
            self.adj[na].append((nb, k))
            self.adj[nb].append((na, k))
            return k

        sig_all = signal_nodes is None
        sig_req = [] if sig_all else list(signal_nodes)
        sig_set: set[int] = set()
        n_sig_unmatched = 0
        if not sig_all:
            coord_idx = {}
            for i, c in enumerate(nodes):
                coord_idx.setdefault((round(c[0], 1), round(c[1], 1)), i)
            for item in sig_req:
                if isinstance(item, (int, float)) and not isinstance(item, bool):
                    k = int(item)
                    if 0 <= k < len(nodes):
                        sig_set.add(k)
                    else:
                        n_sig_unmatched += 1
                    continue
                key = (round(float(item[0]), 1), round(float(item[1]), 1))
                k = coord_idx.get(key)
                if k is None:
                    n_sig_unmatched += 1
                else:
                    sig_set.add(k)
        # OSM crossing nodes -> junction coordinates they annotate (nearest junction wins)
        osm_sig_nodes: set[int] = set()
        osm_cross_nodes: set[int] = set()
        n_osm_cross = 0
        if osm_crossings:
            # bucketed at the match radius so this is O(crossings) rather than
            # O(crossings x junctions) -- a 4,000-junction map with a few thousand crossing nodes
            # would otherwise spend longer here than on the whole geometry pass
            cellm = max(1.0, float(crossing_match_m))
            buck: dict[tuple[int, int], list[int]] = {}
            for i, (nx, ny) in enumerate(nodes):
                buck.setdefault((int(nx // cellm), int(ny // cellm)), []).append(i)
            for rec in osm_crossings:
                cx, cy = float(rec[0]), float(rec[1])
                kind = str(rec[2]) if len(rec) > 2 else ""
                best, bd = -1, crossing_match_m
                bi, bj = int(cx // cellm), int(cy // cellm)
                for di in (-1, 0, 1):
                    for dj in (-1, 0, 1):
                        for i in buck.get((bi + di, bj + dj), ()):
                            nx, ny = nodes[i]
                            d = math.hypot(cx - nx, cy - ny)
                            if d < bd or (d == bd and i < best):
                                best, bd = i, d
                if best >= 0:
                    n_osm_cross += 1
                    osm_cross_nodes.add(best)
                    if kind == "traffic_signals":
                        osm_sig_nodes.add(best)

        sidewalk_m = 0.0
        n_sides = n_trimmed = n_dropped = 0
        trimmed_m = dropped_m = 0.0
        worst_pen = -math.inf
        min_clear = math.inf
        for k, (a, b) in enumerate(raw_edges):
            pts = e_pts[k]
            cum = _polyline_cum(pts)
            L = cum[-1]
            ta, tb = trim_at.get(a, 0.0), trim_at.get(b, 0.0)
            if ta + tb > 0.8 * L:                        # short edge: keep a walkable stub
                room = max(0.25, 0.4 * L)
                scale = (2.0 * room) / max(1e-9, ta + tb)
                ta, tb = ta * scale, tb * scale
            core = _sub_polyline(pts, cum, ta, L - tb)
            want = sidewalk_sides.get(k, {0, 1}) if sidewalk_sides is not None else {0, 1}
            for s in (0, 1):
                if s not in want:
                    continue
                n_sides += 1
                base = e_off[k][0] if s == 0 else e_off[k][1]
                sign = 1.0 if s == 0 else -1.0
                spts, cut_m, pen = _clear_of_carriageways(
                    core, sign, base, carr, kerb=kerb_clearance_m, max_extra=max_extra_offset_m,
                    probe_m=probe_m, min_keep_m=min_sidewalk_m)
                if spts is None:                         # nothing here is derivable footway
                    n_dropped += 1
                    dropped_m += cut_m
                    continue
                if cut_m > 1e-6:
                    n_trimmed += 1
                    trimmed_m += cut_m
                worst_pen = max(worst_pen, pen)
                min_clear = min(min_clear, -pen)
                na = node_index((k, s, 0), spts[0])
                nb = node_index((k, s, 1), spts[-1])
                li = add_link(SIDEWALK, na, nb, spts)
                if li >= 0:
                    sidewalk_m += self.links[li]["len"]
                    self._walk_start_links.append(li)
        if not self._walk_start_links:
            raise ValueError("no sidewalk survived the carriageway-clearance test -- every derived "
                             "footway lay on tarmac, which means the lane geometry and the road "
                             "geometry disagree (check lane_width_m / directed_lanes)")
        # THE INVARIANT, asserted rather than hoped for: nothing kept as sidewalk is on a traffic
        # lane. `_clear_of_carriageways` returns only clear geometry, so this can fire only if the
        # carriageway index and the trim disagree -- a bug, not a map defect.
        if worst_pen > 0.0:
            raise AssertionError(f"a kept sidewalk is {worst_pen:.3f} m inside a carriageway")

        # ---- crossings: one per (edge, junction end), kerb to kerb ------------------ #
        # BOUNDED, and the bound is derived rather than picked. A crossing of edge k is the straight
        # chord between that edge's two footway ends, so its honest length is `off_plus + off_minus`
        # -- the full width of road plus kerbs. When one side has been trimmed back (see
        # `_clear_of_carriageways`) the two surviving ends can be hundreds of metres apart along the
        # street, and the chord between them is not a crossing at all: measured on InTAS before this
        # bound, crossing lengths ran p50 17.0 m / p95 24.8 m / p99 65.0 m / max 445.1 m. A 445 m
        # "crossing" would be a legal pedestrian area straight down the middle of a street.
        crossing_m = 0.0
        n_signalised = 0
        n_cross_oversized = 0
        for k, (a, b) in enumerate(raw_edges):
            bound = 1.5 * (e_off[k][0] + e_off[k][1]) + link_slack_m
            for end, jn in ((0, a), (1, b)):
                i0 = self.node_xy.get((k, 0, end))
                i1 = self.node_xy.get((k, 1, end))
                if i0 is None or i1 is None:
                    continue                             # a side was suppressed -> no crossing
                if math.dist(self.pts[i0], self.pts[i1]) > bound:
                    n_cross_oversized += 1               # not a crossing: this street has none here
                    continue
                # OSM crossing evidence WINS where the map has any for this junction -- a
                # `crossing=unmarked` node at a signalised junction is the map saying that arm has
                # no pedestrian phase. Where the map says nothing, fall back to `signal_nodes`;
                # ignoring it there would silently unsignalise every junction the crossing survey
                # happens not to cover.
                if jn in osm_cross_nodes:
                    sig = jn in osm_sig_nodes
                else:
                    sig = sig_all or jn in sig_set
                # the arm this crossing crosses: the edge's own direction at that junction. The
                # crossing polyline joins the arm's two kerbs and is perpendicular to it, so this
                # is the heading the signal must be asked about (see `add_link`).
                _ap = e_pts[k]
                (_ax, _ay), (_bx, _by) = ((_ap[0], _ap[1]) if end == 0 else (_ap[-1], _ap[-2]))
                li = add_link(CROSSING, i0, i1, [self.pts[i0], self.pts[i1]],
                              junction=nodes[jn], signalised=sig,
                              arm=math.degrees(math.atan2(_by - _ay, _bx - _ax)))
                if li >= 0:
                    crossing_m += self.links[li]["len"]
                    n_signalised += 1 if sig else 0

        # ---- corners: arm_i.LEFT <-> arm_{i+1}.RIGHT, anticlockwise round a junction  #
        def arm_bearing(k: int, end: int) -> float:
            pts = e_pts[k]
            (ax, ay), (bx, by) = (pts[0], pts[1]) if end == 0 else (pts[-1], pts[-2])
            return math.atan2(by - ay, bx - ax)

        def arm_left(k, end):                            # ped node on the arm's LEFT looking away
            return self.node_xy.get((k, 0 if end == 0 else 1, end))

        def arm_right(k, end):
            return self.node_xy.get((k, 1 if end == 0 else 0, end))

        corner_m = 0.0
        n_corner_oversized = 0
        for n, ar in arms.items():
            if len(ar) < 2:
                continue                                 # dead end: the crossing already turns it
            # same bound, same reason: both ends of a corner are footway ends AT this junction, so
            # they cannot honestly be further apart than twice the junction's own trim radius.
            bound = 2.0 * trim_at.get(n, 0.0) + link_slack_m
            order = sorted(ar, key=lambda ke: (arm_bearing(*ke), ke))
            for i in range(len(order)):
                k0, e0 = order[i]
                k1, e1 = order[(i + 1) % len(order)]
                i0, i1 = arm_left(k0, e0), arm_right(k1, e1)
                if i0 is None or i1 is None or i0 == i1:
                    continue
                if math.dist(self.pts[i0], self.pts[i1]) > bound:
                    n_corner_oversized += 1
                    continue
                li = add_link(CORNER, i0, i1, [self.pts[i0], self.pts[i1]], junction=nodes[n])
                if li >= 0:
                    corner_m += self.links[li]["len"]

        # ---- OSM footways: extra legal surface, walkable end to end ----------------- #
        # THREE things have to happen or the mapped footway is decoration. (a) Endpoints are keyed
        # by rounded COORDINATE, not by way index: two OSM ways that meet at a shared node must
        # share a pedestrian node, or 763 Ingolstadt footways arrive as 763 disconnected islands.
        # (b) A footway whose geometry crosses tarmac is filed as a CROSSING, not a SIDEWALK -- it
        # usually IS one (`footway=crossing`), and it keeps "every sidewalk link is clear of every
        # traffic lane" absolute rather than approximately true. (c) Endpoints are SNAPPED to the
        # derived kerb within `footway_snap_m` by a short connector, which is what joins the mapped
        # network to the derived one; a connector that would itself run on tarmac is refused.
        n_footway_links, footway_m = 0, 0.0
        n_footway_cross = n_snap = n_snap_refused = 0
        if osm_footways:
            n_derived_links = len(self.links)        # snap targets: the DERIVED kerb, nothing added
            fnodes: list[int] = []
            for poly in osm_footways:
                pts = [(float(p[0]), float(p[1])) for p in poly]
                if len(pts) < 2:
                    continue
                na = node_index(("f", round(pts[0][0], 1), round(pts[0][1], 1)), pts[0])
                nb = node_index(("f", round(pts[-1][0], 1), round(pts[-1][1], 1)), pts[-1])
                _ss, pen = _dense_penetration(pts, carr, probe_m)
                on_tarmac = max(pen) > 0.0
                li = add_link(CROSSING if on_tarmac else SIDEWALK, na, nb, pts)
                if li >= 0:
                    n_footway_links += 1
                    n_footway_cross += 1 if on_tarmac else 0
                    footway_m += self.links[li]["len"]
                    fnodes.extend((na, nb))
            # Snap to the nearest point on the nearest DERIVED link, not to its nearest END: a
            # footway meets a pavement MID-BLOCK, and a grid whose sidewalk links are a whole block
            # long has its endpoints 50 m away from where the join belongs. Where the nearest point
            # is not already a node, the target link is SPLIT there. Candidates are collected
            # against one index snapshot and then applied per link in descending arc order, so an
            # earlier split never invalidates a later one's arc-length.
            cands: dict[int, list[tuple[float, int, float]]] = {}
            direct: list[tuple[int, int, float]] = []
            if fnodes:
                self._build_index()
                for fi in sorted(set(fnodes)):
                    fx, fy = self.pts[fi]
                    hit = self._nearest_on_links(fx, fy, float(footway_snap_m),
                                                 max_link=n_derived_links)
                    if hit is None:
                        continue
                    tl, ts, td = hit
                    if td <= 1e-6:
                        continue
                    lk = self.links[tl]
                    if ts <= min_sidewalk_m:
                        direct.append((fi, lk["a"], td))
                    elif ts >= lk["len"] - min_sidewalk_m:
                        direct.append((fi, lk["b"], td))
                    else:
                        cands.setdefault(tl, []).append((ts, fi, td))
            for tl in sorted(cands):
                for ts, fi, _td in sorted(cands[tl], reverse=True):
                    mi = self._split_link(tl, ts)
                    direct.append((fi, mi, 0.0))
            for fi, ti, _d in direct:
                if fi == ti:
                    continue
                seg = [self.pts[fi], self.pts[ti]]
                _ss, pen = _dense_penetration(seg, carr, probe_m)
                if max(pen) > 0.0:
                    n_snap_refused += 1              # the join would run down a traffic lane
                    continue
                if add_link(SIDEWALK, fi, ti, seg) >= 0:
                    n_snap += 1
            # every SIDEWALK link is clear of every carriageway (asserted above for the derived
            # ones, tested here for the mapped ones), so all of them are valid spawn points
            self._walk_start_links = [i for i, lk in enumerate(self.links)
                                      if lk["kind"] == SIDEWALK]

        self._build_index()
        n_cross = sum(1 for lk in self.links if lk["kind"] == CROSSING)
        offs = sorted(v for pair in e_off for v in pair)
        comp_nodes, comp_links, n_comp, comp = self._largest_component()
        # SPAWN ON THE MAIN NETWORK. Trimming and the crossing/corner bounds cut links, so the
        # walkable graph fragments (36 components on InTAS, the largest holding 93.11% of the
        # nodes). A pedestrian that started on a 3-link island would spend its whole life pacing it
        # -- a different implausibility from the one this module removes, and one that would be
        # invisible in the legal-area numbers because an island IS legal area. Starting only on the
        # largest component costs the same single RNG draw.
        main = [li for li in self._walk_start_links if self.links[li]["a"] in comp]
        if main:
            self._walk_start_links = main
        self._prov = {
            "road_edges": len(raw_edges), "road_junctions": len(arms),
            "ped_nodes": len(self.pts), "ped_links": len(self.links),
            "sidewalk_links": sum(1 for lk in self.links if lk["kind"] == SIDEWALK),
            "crossings": n_cross,
            "corner_links": sum(1 for lk in self.links if lk["kind"] == CORNER),
            "signalised_crossings": n_signalised,
            "signalised_share": round(n_signalised / max(1, n_cross), 4),
            # links whose two ends were too far apart to be the thing they claim to be
            "crossings_oversized_dropped": n_cross_oversized,
            "corners_oversized_dropped": n_corner_oversized,
            # DERIVED crossings only (`junction_of` is set): a mapped OSM footway filed as a
            # crossing is a pedestrianised street or a marked crossing way, and its length says
            # nothing about whether the derived kerb-to-kerb geometry is sane.
            "crossing_max_m": round(max((lk["len"] for lk, j in zip(self.links, self.junction_of)
                                         if lk["kind"] == CROSSING and j is not None),
                                        default=0.0), 1),
            "corner_max_m": round(max((lk["len"] for lk in self.links
                                       if lk["kind"] == CORNER), default=0.0), 1),
            # CONNECTIVITY. Trimming and bounding cut links, so the walkable graph can fragment;
            # a walker confined to a 3-link island is a different defect from one on a traffic lane,
            # and it is invisible unless it is counted.
            "components": n_comp,
            "largest_component_nodes": comp_nodes,
            "largest_component_node_share": round(comp_nodes / max(1, len(self.pts)), 4),
            "largest_component_links": comp_links,
            "walk_start_links": len(self._walk_start_links),
            "signal_nodes_requested": (None if sig_all else len(sig_req)),
            "signal_nodes_matched": (None if sig_all else len(sig_set)),
            "signal_nodes_unmatched": (None if sig_all else n_sig_unmatched),
            "sidewalk_total_m": round(sidewalk_m, 1),
            "crossing_total_m": round(crossing_m, 1),
            "corner_total_m": round(corner_m, 1),
            "sidewalk_offset_m": [round(offs[0], 2), round(offs[len(offs) // 2], 2),
                                  round(offs[-1], 2)],
            "sidewalk_width_m": float(sidewalk_width_m),
            "kerb_clearance_m": float(kerb_clearance_m),
            # conflict resolution against NEIGHBOURING carriageways (see `_clear_of_carriageways`)
            "sidewalk_sides_considered": n_sides,
            "sidewalk_sides_trimmed": n_trimmed,
            "sidewalk_trimmed_m": round(trimmed_m, 1),
            "sidewalk_sides_dropped": n_dropped,
            "sidewalk_dropped_m": round(dropped_m, 1),
            # <= 0 by construction and asserted above: how deep the WORST kept sidewalk sample sits
            # inside a carriageway (negative = clear of tarmac by that many metres).
            "max_residual_penetration_m": round(max(worst_pen, -999.0), 3),
            "min_tarmac_clearance_m": round(min(min_clear, 999.0), 3),
            "max_extra_offset_m": float(max_extra_offset_m),
            "tarmac_probe_m": float(probe_m),
            "min_sidewalk_m": float(min_sidewalk_m),
            "lane_width_m": lane_w, "directed_lanes": directed_lanes,
            "drive_side": "left" if side > 0 else "right",
            "osm_footway_links": n_footway_links,
            "osm_footway_m": round(footway_m, 1),
            # a mapped footway whose own geometry runs on tarmac is filed as a CROSSING, so the
            # sidewalk invariant stays absolute; usually these are `footway=crossing` ways
            "osm_footways_on_tarmac": n_footway_cross,
            "osm_footway_snaps": n_snap,
            "osm_footway_snaps_refused": n_snap_refused,
            "footway_snap_m": float(footway_snap_m),
            "osm_crossing_nodes_matched": n_osm_cross,
            "osm_crossing_junctions": len(osm_cross_nodes),
            "cell_m": round(self._cell, 1),
        }
        return self

    # -------------------------------------------------- snapping / splitting #
    def _nearest_on_links(self, x: float, y: float, radius: float, *, max_link: int):
        """(link index, arc-length along it, distance) of the nearest point on any link with index
        < `max_link`, within `radius`. Needs a current `_build_index`. Deterministic on ties."""
        best = (-1, 0.0, radius)
        cell = self._cell
        ci, cj = int(x // cell), int(y // cell)
        rr = int(radius // cell) + 1
        for i in range(ci - rr, ci + rr + 1):
            for j in range(cj - rr, cj + rr + 1):
                for k in self._index.get((i, j), ()):
                    li = self._seg_link[k]
                    if li >= max_link:
                        continue
                    ax, ay, bx, by = self._segs[k]
                    dx, dy = bx - ax, by - ay
                    dd = dx * dx + dy * dy
                    if dd <= 0.0:
                        continue
                    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / dd))
                    px, py = ax + t * dx, ay + t * dy
                    d = math.hypot(x - px, y - py)
                    if d < best[2] or (d == best[2] and li < best[0]):
                        lk = self.links[li]
                        # arc-length of (px, py) along the LINK, not along the segment
                        s = 0.0
                        for p, q in zip(lk["pts"], lk["pts"][1:]):
                            if abs(p[0] - ax) < 1e-9 and abs(p[1] - ay) < 1e-9 and \
                                    abs(q[0] - bx) < 1e-9 and abs(q[1] - by) < 1e-9:
                                break
                            s += math.hypot(q[0] - p[0], q[1] - p[1])
                        best = (li, s + t * math.sqrt(dd), d)
        return None if best[0] < 0 else best

    def _split_link(self, li: int, s: float) -> int:
        """Split link `li` at arc-length `s`, returning the new middle node.

        `li` keeps the first half (so every existing link index stays valid) and the second half is
        appended. Adjacency is rewritten rather than rebuilt, which keeps the operation local."""
        lk = self.links[li]
        pts, cum = lk["pts"], lk["cum"]
        left = _sub_polyline(pts, cum, 0.0, s)
        right = _sub_polyline(pts, cum, s, cum[-1])
        mi = len(self.pts)
        self.pts.append(left[-1])
        self.adj.append([])
        a, b = lk["a"], lk["b"]
        lk["pts"], lk["cum"] = left, _polyline_cum(left)
        lk["len"] = lk["cum"][-1]
        lk["b"] = mi
        self.adj[a] = [((mi, l) if l == li else (nb, l)) for nb, l in self.adj[a]]
        self.adj[b] = [(nb, l) for nb, l in self.adj[b] if l != li]
        self.adj[mi].append((a, li))
        k = len(self.links)
        rcum = _polyline_cum(right)
        self.links.append({"kind": lk["kind"], "a": mi, "b": b, "pts": right, "cum": rcum,
                           "len": rcum[-1]})
        self.junction_of.append(self.junction_of[li])
        self.signalised.append(self.signalised[li])
        self.adj[mi].append((b, k))
        self.adj[b].append((mi, k))
        return mi

    # -------------------------------------------------------- connectivity #
    def _largest_component(self) -> tuple[int, int, int, set]:
        """(nodes, links, component count, node set) of the largest connected piece of the graph."""
        seen = [False] * len(self.pts)
        best_nodes: list[int] = []
        n_comp = 0
        for s in range(len(self.pts)):
            if seen[s]:
                continue
            n_comp += 1
            comp = [s]
            seen[s] = True
            stack = [s]
            while stack:
                for nb, _lk in self.adj[stack.pop()]:
                    if not seen[nb]:
                        seen[nb] = True
                        comp.append(nb)
                        stack.append(nb)
            if len(comp) > len(best_nodes):
                best_nodes = comp
        inc = set(best_nodes)
        links = sum(1 for lk in self.links if lk["a"] in inc)
        return len(best_nodes), links, n_comp, inc

    # ------------------------------------------------------- spatial index #
    def _build_index(self):
        self._segs = []
        self._seg_link = []
        for k, lk in enumerate(self.links):
            pts = lk["pts"]
            for p, q in zip(pts, pts[1:]):
                if p != q:
                    self._segs.append((p[0], p[1], q[0], q[1]))
                    self._seg_link.append(k)
        if not self._segs:
            raise ValueError("pedestrian network came out empty (no walkable geometry derived)")
        lens = sorted(math.hypot(s[2] - s[0], s[3] - s[1]) for s in self._segs)
        self._cell = min(200.0, max(20.0, 2.0 * lens[len(lens) // 2]))
        cell = self._cell
        ix: dict[tuple[int, int], set] = {}
        for k, (ax, ay, bx, by) in enumerate(self._segs):
            steps = max(1, int(math.hypot(bx - ax, by - ay) // cell) + 1)
            for c in range(steps):
                t0, t1 = c / steps, (c + 1) / steps
                x0, y0 = ax + (bx - ax) * t0, ay + (by - ay) * t0
                x1, y1 = ax + (bx - ax) * t1, ay + (by - ay) * t1
                for ci in range(int(min(x0, x1) // cell), int(max(x0, x1) // cell) + 1):
                    for cj in range(int(min(y0, y1) // cell), int(max(y0, y1) // cell) + 1):
                        ix.setdefault((ci, cj), set()).add(k)
        self._index = {c: tuple(sorted(v)) for c, v in ix.items()}
        cis = [c[0] for c in self._index]
        cjs = [c[1] for c in self._index]
        self._cbox = (min(cis), min(cjs), max(cis), max(cjs))
        self._max_ring = (self._cbox[2] - self._cbox[0]) + (self._cbox[3] - self._cbox[1]) + 2

    def legal_distance(self, x: float, y: float) -> float:
        """Distance (m) from (x, y) to the nearest pedestrian facility CENTRELINE.

        A position is on a pedestrian-legal area when this is <= `sidewalk_width_m / 2` (a footway
        is a strip, not a line). Exact ring walk over the cell index -- the same construction
        `roads.dist_to_road` uses, so the two distances are comparable measurement for measurement.
        """
        best = math.inf
        cell = self._cell
        ci, cj = int(x // cell), int(y // cell)
        i0, j0, i1, j1 = self._cbox
        r_lo = max(0, i0 - ci, ci - i1, j0 - cj, cj - j1)
        r_hi = max(abs(i0 - ci), abs(i1 - ci), abs(j0 - cj), abs(j1 - cj)) + 1
        for r in range(r_lo, r_hi + 1):
            if r == 0:
                cells = ((ci, cj),)
            else:
                cells = [(i, cj - r) for i in range(ci - r, ci + r + 1)]
                cells += [(i, cj + r) for i in range(ci - r, ci + r + 1)]
                cells += [(ci - r, j) for j in range(cj - r + 1, cj + r)]
                cells += [(ci + r, j) for j in range(cj - r + 1, cj + r)]
            for c in cells:
                for k in self._index.get(c, ()):
                    ax, ay, bx, by = self._segs[k]
                    d = _pt_seg_dist(x, y, ax, ay, bx, by)
                    if d < best:
                        best = d
            if best <= r * cell:
                break
        return best

    def is_legal(self, x: float, y: float, tol_m: float | None = None) -> bool:
        tol = 0.5 * self._prov["sidewalk_width_m"] if tol_m is None else float(tol_m)
        return self.legal_distance(x, y) <= tol

    # ------------------------------------------------------------ products #
    def stats(self) -> dict:
        return dict(self._prov)

    def geometry(self) -> dict:
        """The pedestrian layer as plain data for a UI/network.json (`{kind, pts}` per link).

        Map geometry, not oracle truth: it says where a pedestrian MAY walk, exactly as the road
        arrays say where a vehicle may drive. Nothing here identifies an actor.
        """
        return {"ped_links": [{"kind": _KIND_NAME[lk["kind"]],
                               "signalised": self.signalised[i] if lk["kind"] == CROSSING else None,
                               "pts": [[round(p[0], 2), round(p[1], 2)] for p in lk["pts"]]}
                              for i, lk in enumerate(self.links)],
                "ped_stats": self.stats()}

    # ---------------------------------------------------------------- walk #
    def walk(self, vid: int, seed, spawn_time: float, life: float, speed: float, *,
             signal_fn=None, unsignalised_max_wait_s: float = UNSIGNALISED_MAX_WAIT_S,
             straight_bias: float = 3.0) -> "PedestrianWalk":
        """One pedestrian's itinerary: sidewalk legs, kerb waits, crossing traversals.

        Route choice is a self-avoiding random walk on the pedestrian graph with a straightness
        preference (`straight_bias`), which is what a real pedestrian trajectory looks like at this
        scale -- long stretches of one footway, occasional turns, a crossing when the route needs
        the other side of the street. It is deliberately NOT a shortest path: a shortest path needs
        an origin-destination model this engine does not have for pedestrians, and inventing one
        would put a second unvalidated assumption behind the first.

        EVERY draw comes from `random.Random(f"{seed}:vruwalk:{vid}")` and from nowhere else.
        """
        rng = random.Random(f"{seed}:vruwalk:{vid}")
        v = max(0.1, float(speed))
        horizon = max(1.0, float(life))

        # start: a random point along a random sidewalk link, walking towards a random end
        li = self._walk_start_links[rng.randrange(len(self._walk_start_links))]
        lk = self.links[li]
        f = 0.05 + 0.9 * rng.random()             # never start exactly on a graph node
        forward = rng.random() < 0.5
        s0 = f * lk["len"]
        first = (_sub_polyline(lk["pts"], lk["cum"], s0, lk["len"]) if forward
                 else _sub_polyline(lk["pts"], lk["cum"], 0.0, s0)[::-1])
        cur_node = lk["b"] if forward else lk["a"]
        # leg = (link index, points in travel order, wait BEFORE traversing, link kind)
        legs: list[tuple[int, list, float, int]] = [(li, first, 0.0, lk["kind"])]
        t_used = _polyline_cum(first)[-1] / v
        prev_link = li
        prev_h = _heading_of(first)

        while t_used < horizon and len(legs) < MAX_WALK_LEGS:
            cands = [(nb, lnk) for nb, lnk in self.adj[cur_node] if lnk != prev_link]
            if not cands:
                cands = list(self.adj[cur_node])
            if not cands:
                break
            weights = []
            for nb, lnk in cands:
                pts = self._oriented(lnk, cur_node)
                dh = _ang_diff(prev_h, _heading_of(pts))
                weights.append(math.exp(-straight_bias * (dh / 180.0)))
            nb, lnk = _weighted_choice(rng, cands, weights)
            pts = self._oriented(lnk, cur_node)
            kind = self.links[lnk]["kind"]
            wait = 0.0
            if kind == CROSSING:
                wait = self._crossing_wait(lnk, spawn_time + t_used, rng, signal_fn,
                                           unsignalised_max_wait_s, _heading_of(pts))
            legs.append((lnk, pts, wait, kind))
            t_used += wait + self.links[lnk]["len"] / v
            prev_h = _heading_of(pts)
            prev_link = lnk
            cur_node = nb

        return PedestrianWalk(legs, v, spawn_time)

    def _oriented(self, li: int, from_node: int) -> list:
        lk = self.links[li]
        return list(lk["pts"]) if lk["a"] == from_node else list(lk["pts"])[::-1]

    def _crossing_wait(self, li: int, t: float, rng, signal_fn, max_unsig: float,
                       heading_deg: float) -> float:
        """Kerb wait (s) before stepping onto crossing `li` at time `t`.

        Signalised + a `signal_fn` -- solved deterministically by probing the signal forward in
        `_SIGNAL_PROBE_S` steps (no RNG at all, so a signalised pedestrian's timing is a property
        of the junction, not of the seed). Unsignalised -- a uniform draw bounded by
        `max_unsig`, standing in for gap acceptance, which cannot be modelled honestly while
        vehicles do not yield and pedestrians are outside the car-following state.

        THE SIGNAL IS ASKED ABOUT THE ARM, NOT ABOUT THE CROSSING. A derived crossing joins one
        arm's two kerbs, so its own heading is PERPENDICULAR to the traffic it conflicts with, and
        a `signal_fn` given the crossing's heading answers about the cross street -- exactly 90
        degrees wrong, which holds the pedestrian through the conflicting movement's red and
        releases it into the green. `links[li]["arm"]` carries the real arm bearing, recorded at
        build time; a crossing derived from a MAPPED footway has no single arm, so it falls back to
        the perpendicular of its own heading, which is the same answer for a footway that crosses
        the road it is filed against squarely.
        """
        if self.signalised[li] and signal_fn is not None and self.junction_of[li] is not None:
            node = self.junction_of[li]
            arm = self.links[li].get("arm")
            if arm is None:
                arm = heading_deg + 90.0
            tt = 0.0
            limit = 300.0
            while tt < limit:
                if signal_fn(node, arm, t + tt):
                    return tt
                tt += _SIGNAL_PROBE_S
            return 0.0
        return rng.random() * max(0.0, float(max_unsig))


def _heading_of(pts: list) -> float:
    (ax, ay), (bx, by) = pts[0], pts[-1]
    return math.degrees(math.atan2(by - ay, bx - ax)) % 360.0


def _ang_diff(a: float, b: float) -> float:
    d = abs(a - b) % 360.0
    return d if d <= 180.0 else 360.0 - d


def _weighted_choice(rng, items, weights):
    total = sum(weights)
    if total <= 0.0:
        return items[rng.randrange(len(items))]
    r = rng.random() * total
    acc = 0.0
    for it, w in zip(items, weights):
        acc += w
        if r <= acc:
            return it
    return items[-1]


# --------------------------------------------------------------------------- #
# the mobility object
# --------------------------------------------------------------------------- #
class PedestrianWalk:
    """A pedestrian's realised trajectory, `Trip`-compatible.

    Flattened into two aligned arrays -- `pts[k]` and the time `tarr[k]` at which the pedestrian is
    AT `pts[k]` -- so a kerb wait is simply the same point appearing twice with different times and
    `state()` is one bisect plus a lerp, with no per-step branch on "am I waiting". Speed during a
    wait comes out as exactly 0.0 and the heading holds the direction of the crossing about to be
    used, which is what a waiting pedestrian faces.

    Exposes `.speed`, `.length`, `.t0`, `.t1`, `.state(t)` and `.at_distance(d)`, the subset of
    `roads.Trip` that `Vehicle.true_state` and the GUI touch. It deliberately does NOT expose
    `caps` / `nodes` / `lanes` / `next_node` / `next_turn`: those are car-following inputs and a
    pedestrian is not a car-following actor, so anything that reached for them would be a bug
    worth failing on rather than silently answering.
    """

    __slots__ = ("pts", "tarr", "hdg", "cum", "speed", "t0", "t1", "length", "legs", "waits",
                 "wait_s", "crossings", "kinds", "kind_at_pt")

    def __init__(self, legs: list, speed: float, spawn_time: float):
        self.speed = float(speed)
        self.t0 = float(spawn_time)
        pts: list[tuple[float, float]] = []
        tarr: list[float] = []
        hdg: list[float] = []
        kind_at: list[int] = []          # kind of the link being traversed to REACH pts[k]
        t = float(spawn_time)
        n_wait = 0
        wait_s = 0.0
        n_cross = 0
        for _li, lpts, wait, kind in legs:
            h = _heading_of(lpts)
            n_cross += 1 if kind == CROSSING else 0
            if not pts:
                pts.append(lpts[0])
                tarr.append(t)
                hdg.append(h)
                kind_at.append(kind)
            if wait > 0.0:                          # kerb wait: same point, later time
                t += wait
                pts.append(pts[-1])
                tarr.append(t)
                hdg.append(h)                       # facing the crossing
                kind_at.append(SIDEWALK)            # WAITING is at the kerb, not on the crossing
                n_wait += 1
                wait_s += wait
            for p, q in zip(lpts, lpts[1:]):
                d = math.hypot(q[0] - p[0], q[1] - p[1])
                if d <= 0.0:
                    continue
                t += d / self.speed
                pts.append(q)
                tarr.append(t)
                hdg.append(math.degrees(math.atan2(q[1] - p[1], q[0] - p[0])) % 360.0)
                kind_at.append(kind)
        if len(pts) < 2:                            # degenerate: stand still (never happens on a
            pts.append(pts[0] if pts else (0.0, 0.0))   # derived network, but keep state() total)
            tarr.append(t + 1.0)
            hdg.append(hdg[0] if hdg else 0.0)
            kind_at.append(kind_at[0] if kind_at else SIDEWALK)
        self.pts = pts
        self.tarr = tarr
        self.hdg = hdg
        self.kind_at_pt = kind_at
        self.cum = _polyline_cum(pts)
        self.length = self.cum[-1]
        self.t1 = tarr[-1]
        self.legs = len(legs)
        self.waits = n_wait                       # kerb waits actually served (a zero-length wait
        self.wait_s = wait_s                      # at a green signal is not one)
        self.crossings = n_cross                  # crossings TRAVERSED
        self.kinds = tuple(k for _li, _p, _w, k in legs)

    def state(self, t: float) -> tuple[float, float, float, float]:
        """True (x, y, speed, heading[deg]) at time t; clamped at both ends (a pedestrian that has
        finished its itinerary stands where it stopped rather than teleporting)."""
        ta = self.tarr
        if t <= ta[0]:
            return self.pts[0][0], self.pts[0][1], 0.0, self.hdg[0]
        if t >= ta[-1]:
            return self.pts[-1][0], self.pts[-1][1], 0.0, self.hdg[-1]
        k = bisect.bisect_left(ta, t)
        k = max(1, min(k, len(ta) - 1))
        (ax, ay), (bx, by) = self.pts[k - 1], self.pts[k]
        dt = ta[k] - ta[k - 1]
        f = (t - ta[k - 1]) / dt if dt > 0 else 0.0
        d = math.hypot(bx - ax, by - ay)
        sp = (d / dt) if dt > 0 else 0.0
        return ax + (bx - ax) * f, ay + (by - ay) * f, sp, self.hdg[k]

    def kind_at(self, t: float) -> int:
        """Which facility the pedestrian is ON at time t (SIDEWALK / CROSSING / CORNER).

        The EXPOSURE metric: `CROSSING` is the only kind that puts a pedestrian on a traffic lane,
        and since vehicles do not yield (see the module docstring) the time spent there is time a
        real pedestrian would be at risk and this engine's pedestrian simply is not.
        """
        ta = self.tarr
        if t <= ta[0]:
            return self.kind_at_pt[0]
        if t >= ta[-1]:
            return self.kind_at_pt[-1]
        k = max(1, min(bisect.bisect_left(ta, t), len(ta) - 1))
        return self.kind_at_pt[k]

    def at_distance(self, d: float) -> tuple[float, float, float]:
        """(x, y, heading[deg]) at arc-length d along the walk (clamped)."""
        if d <= 0.0:
            return self.pts[0][0], self.pts[0][1], self.hdg[0]
        if d >= self.length:
            return self.pts[-1][0], self.pts[-1][1], self.hdg[-1]
        k = max(1, bisect.bisect_left(self.cum, d))
        (ax, ay), (bx, by) = self.pts[k - 1], self.pts[k]
        seg = self.cum[k] - self.cum[k - 1]
        f = (d - self.cum[k - 1]) / seg if seg > 0 else 0.0
        return ax + (bx - ax) * f, ay + (by - ay) * f, self.hdg[k]

    def start(self) -> tuple[float, float]:
        return self.pts[0]


# --------------------------------------------------------------------------- #
# front door + measurement
# --------------------------------------------------------------------------- #
def build_sidewalks(net, **kw) -> SidewalkNetwork | None:
    """`SidewalkNetwork.from_network(net, **kw)`, or None when there is no routed network.

    `road_network="linear"` has no graph at all (`net is None`), so it keeps today's behaviour --
    which is the correct answer, not a fallback: there is no geometry to derive a footway from.
    """
    if net is None:
        return None
    return SidewalkNetwork.from_network(net, **kw)


def legacy_offroad_positions(net, *, seed=7, n: int = 400, vid0: int = 0, life_s: float = 90.0,
                             dt: float = 1.0, speed: float = 1.8,
                             offroad_tol_m: float = 15.0) -> list:
    """The CURRENT (pre-sidewalk) VRU trajectory, replayed for MEASUREMENT only.

    This is a faithful copy of `run.make_vru`'s placement and of `Vehicle.true_state`'s no-trip
    branch, including the RNG DRAW ORDER (node, |dx|, sign(dx), |dy|, sign(dy), direction,
    wander_amp, wander_w, phase), so the "before" column of the realism table is the behaviour the
    engine actually ships rather than a description of it. It exists so the comparison is a property
    of this module that a test can re-derive, not a number in a report.

    It is NEVER called from the run: nothing in `run.py` imports it, and it draws from
    `f"{seed}:vru:{vid}"` -- the existing placement stream -- so calling it here consumes nothing.
    If `run.make_vru` changes, this must change with it; the test that compares the two is the
    tripwire.
    """
    nodes = [(float(p[0]), float(p[1])) for p in net.geometry()["nodes"]]
    out = []
    steps = max(1, int(life_s / max(1e-9, dt)))
    for i in range(n):
        vid = vid0 + i
        vr = random.Random(f"{seed}:vru:{vid}")
        nx, ny = nodes[vr.randrange(len(nodes))]
        dx = offroad_tol_m * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
        dy = offroad_tol_m * (1.3 + 0.7 * vr.random()) * (1.0 if vr.random() < 0.5 else -1.0)
        sx, sy = nx + dx, ny + dy
        direction = vr.random() * 6.283
        wander_amp = 0.5 + vr.random()
        wander_w = 0.1 + vr.random() * 0.2
        phase = vr.random() * 6.283
        ux, uy = math.cos(direction), math.sin(direction)
        nxx, nyy = -uy, ux
        for k in range(steps):
            t = k * dt
            along = speed * t
            lat = wander_amp * math.sin(wander_w * t + phase)
            out.append((sx + along * ux + lat * nxx, sy + along * uy + lat * nyy))
    return out


def sidewalk_positions(sidewalks: SidewalkNetwork, *, seed=7, n: int = 400, vid0: int = 0,
                       life_s: float = 90.0, dt: float = 1.0, speed: float = 1.8,
                       signal_fn=None) -> list:
    """The SIDEWALK-following trajectory on exactly the protocol `legacy_offroad_positions` uses
    (same seed, same vids, same life, same sampling cadence), so the before/after columns differ in
    the mobility model and in nothing else."""
    out = []
    steps = max(1, int(life_s / max(1e-9, dt)))
    for i in range(n):
        vid = vid0 + i
        w = sidewalks.walk(vid, seed, 0.0, life_s, speed, signal_fn=signal_fn)
        for k in range(steps):
            x, y, _sp, _h = w.state(k * dt)
            out.append((x, y))
    return out


def measure_positions(positions, sidewalks: SidewalkNetwork, net=None, *,
                      tol_m: float | None = None) -> dict:
    """Score a bag of (x, y) VRU positions against the pedestrian layer and the roads.

    Returns the three numbers the realism question actually asks: what fraction of the time a VRU
    is somewhere a pedestrian may legally be, how far it is from the nearest ROAD (the quantity
    `mapOffRoad` measures and the quantity the old placement rule deliberately maximised), and how
    far the worst position strays from any legal area. Same function for before and after, so the
    two columns are the same measurement rather than two definitions.
    """
    tol = (0.5 * sidewalks.stats()["sidewalk_width_m"]) if tol_m is None else float(tol_m)
    legal = [sidewalks.legal_distance(x, y) for x, y in positions]
    road = [net.dist_to_road(x, y) for x, y in positions] if net is not None else []
    n = max(1, len(legal))

    def pct(vals, q):
        if not vals:
            return None
        s = sorted(vals)
        return round(s[min(len(s) - 1, int(q * len(s)))], 3)

    strays = sorted(max(0.0, d - tol) for d in legal)
    out = {"n": len(legal),
           "legal_tol_m": round(tol, 3),
           "on_legal_frac": round(sum(1 for d in legal if d <= tol) / n, 4),
           "legal_dist_p50": pct(legal, 0.50), "legal_dist_p90": pct(legal, 0.90),
           "legal_dist_p95": pct(legal, 0.95), "legal_dist_p99": pct(legal, 0.99),
           "legal_dist_max": round(max(legal), 3) if legal else None,
           # how far a VRU STRAYS beyond any legal walking area (0 while it is on one)
           "stray_p50": pct(strays, 0.50), "stray_p95": pct(strays, 0.95),
           "stray_p99": pct(strays, 0.99),
           "stray_max_m": round(max(0.0, max(legal, default=0.0) - tol), 3)}
    if road:
        # `GridNetwork.dist_to_road` answers 1e9 for a position off the lattice extent entirely --
        # a real state for today's VRUs, which walk in a straight line for their whole life and
        # leave the map. Those are counted, not averaged, or one of them swamps the mean.
        on = [d for d in road if d < 1e8]
        out.update({"road_dist_offmap_frac": round(1.0 - len(on) / max(1, len(road)), 4),
                    "road_dist_p50": pct(on, 0.50), "road_dist_p95": pct(on, 0.95),
                    "road_dist_max": round(max(on), 3) if on else None,
                    "road_dist_mean": round(sum(on) / len(on), 3) if on else None})
    return out
