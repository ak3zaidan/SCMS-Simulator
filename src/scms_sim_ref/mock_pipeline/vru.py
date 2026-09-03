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

VEHICLES DO NOT YIELD, stated plainly rather than implied. `run.car_follow` filters its active
list to `v.cf` and VRUs are not car-following actors, so no pedestrian is ever an IDM leader, is
never a gap-acceptance claimant, and never appears in the signal logic. A pedestrian on a crossing
is therefore geometrically overrun by traffic rather than braked for. That is a MODELLING choice
with a consequence worth being explicit about: pedestrians in this engine are exposed to conflict
but never protected from it, so "time spent on a crossing" is not a collision risk the engine
resolves -- it is only a position claim the detectors see. Wiring a yield is a change to
`run.car_follow`, described in the module's NOT-WIRED note below.

DETERMINISM. Every draw comes from `random.Random(f"{seed}:vruwalk:{vid}")`, a dedicated
string-keyed stream that exists nowhere else, so enabling sidewalks perturbs no other stream --
not the vehicle fleet, not the existing `f"{seed}:vru:{vid}"` placement stream, not the channel.
With sidewalks off, nothing in this module is imported into the run at all.

NOT WIRED HERE (owned by `run.py`, see the task report): the config flags, the call that builds
the network, the substitution of `trip=walk` in `make_vru`, and a pedestrian-aware yield.
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
           "measure_positions", "SIDEWALK_WIDTH_M", "KERB_CLEARANCE_M",
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
#: Link kinds.
SIDEWALK, CROSSING, CORNER = 0, 1, 2
_KIND_NAME = {SIDEWALK: "sidewalk", CROSSING: "crossing", CORNER: "corner"}
#: Sanity bound on the derived pedestrian graph. A 4000-node / 12000-edge road map (the
#: `CustomNetwork` ceiling) yields 4 ped nodes and ~4 links per road edge.
MAX_PED_LINKS = 120_000


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
    sidewalk correctly offset from road A lands on road B's tarmac. Measured on the Ingolstadt
    import before this pass existed: 8.8% of sidewalk vertices were inside SOME carriageway, up to
    3.48 m deep, while every one of them was correctly outside its own. That is exactly the class
    of defect that still looks plausible in a plot.
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

    def penetration(self, x: float, y: float) -> float:
        """How far INSIDE the deepest carriageway (m). <= 0 means outside every carriageway.

        The cell is at least `2 * max_half_width` wide, so the 3x3 block around the query contains
        every segment whose carriageway could possibly reach it -- exact, not a heuristic.
        """
        worst = -math.inf
        cell = self.cell
        ci, cj = int(x // cell), int(y // cell)
        for i in (ci - 1, ci, ci + 1):
            for j in (cj - 1, cj, cj + 1):
                for k in self.index.get((i, j), ()):
                    ax, ay, bx, by, hp, hm = self.segs[k]
                    dx, dy = bx - ax, by - ay
                    dd = dx * dx + dy * dy
                    if dd <= 0.0:
                        continue
                    t = max(0.0, min(1.0, ((x - ax) * dx + (y - ay) * dy) / dd))
                    cx, cy = ax + t * dx, ay + t * dy
                    d = math.hypot(x - cx, y - cy)
                    L = math.sqrt(dd)
                    lat = ((-dy) * (x - ax) + dx * (y - ay)) / L     # +ve = left of a->b
                    half = hp if lat >= 0.0 else hm
                    if half - d > worst:
                        worst = half - d
        return worst if worst > -math.inf else -math.inf


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
                     max_extra_offset_m: float = 6.0) -> "SidewalkNetwork":
        """Derive the pedestrian network from `net` (any `roads` topology).

        `net` is read through its public surface only -- `geometry()` for nodes/edges, and, where
        the topology provides them, `edge_points` / `lanes_for` / `is_oneway` for curve geometry
        and per-direction lane counts. A `GridNetwork` or `RingNetwork` (which have none of those)
        gets straight edges and the global lane count, which is exactly what its vehicles use.

        `sidewalk_sides` maps a road-edge index to a set of sides to BUILD ({0}, {1}, {0,1} or
        set()); the OSM `sidewalk=*` tag is the source (see `osm.extract_footways`). Absent -> both
        sides, which is the right default: on the cached Ingolstadt extract only 4.0% of drivable
        ways say `sidewalk=no`, and 63% of the tagged ones say `separate` (the footway EXISTS, it
        is simply mapped as its own way rather than as an attribute of the road).

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

        def add_link(kind, na, nb, pts, junction=None, signalised=False) -> int:
            cum = _polyline_cum(pts)
            if cum[-1] <= 1e-9 or na == nb:
                return -1
            if len(self.links) >= MAX_PED_LINKS:
                raise ValueError(f"pedestrian network exceeds {MAX_PED_LINKS} links -- the road "
                                 f"map is too large to derive sidewalks for")
            k = len(self.links)
            self.links.append({"kind": kind, "a": na, "b": nb, "pts": pts, "cum": cum,
                               "len": cum[-1]})
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
            for rec in osm_crossings:
                cx, cy = float(rec[0]), float(rec[1])
                kind = str(rec[2]) if len(rec) > 2 else ""
                best, bd = -1, crossing_match_m
                for i, (nx, ny) in enumerate(nodes):
                    d = math.hypot(cx - nx, cy - ny)
                    if d < bd:
                        best, bd = i, d
                if best >= 0:
                    n_osm_cross += 1
                    osm_cross_nodes.add(best)
                    if kind == "traffic_signals":
                        osm_sig_nodes.add(best)

        sidewalk_m = 0.0
        n_pushed = n_conflict = 0
        push_m = conflict_m = 0.0
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
                base = e_off[k][0] if s == 0 else e_off[k][1]
                sign = 1.0 if s == 0 else -1.0
                # push the footway clear of any NEIGHBOURING carriageway it lands on, bounded, and
                # count the ones that cannot be resolved rather than pretending they were
                extra, pen = 0.0, 0.0
                spts = None
                for _ in range(4):
                    spts = _offset_polyline(core, [sign * (base + extra)] * max(1, len(core) - 1))
                    pen = max(carr.penetration(px, py) for px, py in spts)
                    if pen <= 0.0 or extra >= max_extra_offset_m:
                        break
                    step = min(pen + kerb_clearance_m, max_extra_offset_m - extra)
                    if step <= 1e-6:
                        break
                    extra += step
                if extra > 0.0:
                    n_pushed += 1
                    push_m = max(push_m, extra)
                if pen > 0.0:
                    n_conflict += 1
                    conflict_m = max(conflict_m, pen)
                na = node_index((k, s, 0), spts[0])
                nb = node_index((k, s, 1), spts[-1])
                li = add_link(SIDEWALK, na, nb, spts)
                if li >= 0:
                    sidewalk_m += self.links[li]["len"]
                    self._walk_start_links.append(li)

        # ---- crossings: one per (edge, junction end), kerb to kerb ------------------ #
        crossing_m = 0.0
        n_signalised = 0
        for k, (a, b) in enumerate(raw_edges):
            for end, jn in ((0, a), (1, b)):
                i0 = self.node_xy.get((k, 0, end))
                i1 = self.node_xy.get((k, 1, end))
                if i0 is None or i1 is None:
                    continue                             # a side was suppressed -> no crossing
                sig = (jn in osm_sig_nodes) if osm_crossings else (sig_all or jn in sig_set)
                li = add_link(CROSSING, i0, i1, [self.pts[i0], self.pts[i1]],
                              junction=nodes[jn], signalised=sig)
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
        for n, ar in arms.items():
            if len(ar) < 2:
                continue                                 # dead end: the crossing already turns it
            order = sorted(ar, key=lambda ke: (arm_bearing(*ke), ke))
            for i in range(len(order)):
                k0, e0 = order[i]
                k1, e1 = order[(i + 1) % len(order)]
                i0, i1 = arm_left(k0, e0), arm_right(k1, e1)
                if i0 is None or i1 is None or i0 == i1:
                    continue
                li = add_link(CORNER, i0, i1, [self.pts[i0], self.pts[i1]], junction=nodes[n])
                if li >= 0:
                    corner_m += self.links[li]["len"]

        # ---- OSM footways: extra legal surface, walkable end to end ----------------- #
        n_footway_links, footway_m = 0, 0.0
        if osm_footways:
            for w, poly in enumerate(osm_footways):
                pts = [(float(p[0]), float(p[1])) for p in poly]
                if len(pts) < 2:
                    continue
                na = node_index(("f", w, 0), pts[0])
                nb = node_index(("f", w, 1), pts[-1])
                li = add_link(SIDEWALK, na, nb, pts)
                if li >= 0:
                    n_footway_links += 1
                    footway_m += self.links[li]["len"]

        self._build_index()
        n_cross = sum(1 for lk in self.links if lk["kind"] == CROSSING)
        offs = sorted(v for pair in e_off for v in pair)
        self._prov = {
            "road_edges": len(raw_edges), "road_junctions": len(arms),
            "ped_nodes": len(self.pts), "ped_links": len(self.links),
            "sidewalk_links": sum(1 for lk in self.links if lk["kind"] == SIDEWALK),
            "crossings": n_cross,
            "corner_links": sum(1 for lk in self.links if lk["kind"] == CORNER),
            "signalised_crossings": n_signalised,
            "signalised_share": round(n_signalised / max(1, n_cross), 4),
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
            # conflict resolution against NEIGHBOURING carriageways (see `_CarriagewayIndex`)
            "sidewalks_pushed_out": n_pushed,
            "max_push_m": round(push_m, 2),
            "sidewalks_still_on_tarmac": n_conflict,
            "max_residual_penetration_m": round(conflict_m, 2),
            "max_extra_offset_m": float(max_extra_offset_m),
            "lane_width_m": lane_w, "directed_lanes": directed_lanes,
            "drive_side": "left" if side > 0 else "right",
            "osm_footway_links": n_footway_links,
            "osm_footway_m": round(footway_m, 1),
            "osm_crossing_nodes_matched": n_osm_cross,
            "osm_crossing_junctions": len(osm_cross_nodes),
            "cell_m": round(self._cell, 1),
        }
        return self

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

        while t_used < horizon:
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
        """
        if self.signalised[li] and signal_fn is not None and self.junction_of[li] is not None:
            node = self.junction_of[li]
            tt = 0.0
            limit = 300.0
            while tt < limit:
                if signal_fn(node, heading_deg, t + tt):
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
                 "wait_s", "crossings", "kinds")

    def __init__(self, legs: list, speed: float, spawn_time: float):
        self.speed = float(speed)
        self.t0 = float(spawn_time)
        pts: list[tuple[float, float]] = []
        tarr: list[float] = []
        hdg: list[float] = []
        t = float(spawn_time)
        dist = 0.0
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
            if wait > 0.0:                          # kerb wait: same point, later time
                t += wait
                pts.append(pts[-1])
                tarr.append(t)
                hdg.append(h)                       # facing the crossing
                n_wait += 1
                wait_s += wait
            for p, q in zip(lpts, lpts[1:]):
                d = math.hypot(q[0] - p[0], q[1] - p[1])
                if d <= 0.0:
                    continue
                dist += d
                t += d / self.speed
                pts.append(q)
                tarr.append(t)
                hdg.append(math.degrees(math.atan2(q[1] - p[1], q[0] - p[0])) % 360.0)
        if len(pts) < 2:                            # degenerate: stand still (never happens on a
            pts.append(pts[0] if pts else (0.0, 0.0))   # derived network, but keep state() total)
            tarr.append(t + 1.0)
            hdg.append(hdg[0] if hdg else 0.0)
        self.pts = pts
        self.tarr = tarr
        self.hdg = hdg
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

    out = {"n": len(legal),
           "legal_tol_m": round(tol, 3),
           "on_legal_frac": round(sum(1 for d in legal if d <= tol) / n, 4),
           "legal_dist_p50": pct(legal, 0.50), "legal_dist_p95": pct(legal, 0.95),
           "legal_dist_max": round(max(legal), 3) if legal else None,
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
