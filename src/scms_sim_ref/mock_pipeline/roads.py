"""Road networks + route-following trips for realistic, long-running mobility.

A vehicle in flow mode gets a `Trip`: a shortest-path route between two intersections of a road
network, driven at a desired speed. This gives finite journeys (the vehicle despawns at its
destination), real intersections and turns, and heading that follows the road -- everything the
straight-line model lacked for long simulations with continuous vehicle turnover.

Topologies (all expose the same interface: nodes / boundary / center / _coord / dist_to_road /
random_trip / geometry, so everything downstream is topology-agnostic):
  - GridNetwork    w x h Manhattan grid (optional dropout for irregularity)
  - RingNetwork    circular ring road
  - CustomNetwork  an ARBITRARY node/edge graph -- lets a user (or the AI copilot) design any map:
                   radial cities, highways with on-ramps, river towns with bridges, ...
  - spider_graph() generator for the classic radial+ring "spider" city, fed to CustomNetwork

DIRECTION, LANES AND GEOMETRY (all OPT-IN; nothing below changes a default run or draws any RNG).
The network was a set of UNDIRECTED straight centrelines with one global lane count, so the two
directions of a street shared the identical polyline: opposing vehicles physically passed through
each other and only a heading filter kept them out of car-following. The realism harness counts
those as `traffic.overlap_events` -- a HARD gate the SUMO path scores 0 on. Three pieces close it:

  * `parse_edge_spec` -- the custom-network edge schema. `[a,b]` and `[a,b,speed]` still mean what
    they always meant; `[a,b,speed,lanes,oneway]` and the object form add `oneway` (OSM's yes/-1),
    per-direction `lanes`, and a `shape` polyline that carries an edge's CURVE geometry without
    spending graph nodes on it. One-way edges make `CustomNetwork` a directed graph -- routing and
    the strong-connectivity check follow.
  * `enable_directed_lanes()` (every topology) -- each direction of travel gets its OWN carriageway,
    offset to its side of the centreline by half its own lane count, with mitre joins at the
    junctions. `Trip.nodes` keeps the true junction coordinate so signal phase and gap-acceptance
    conflicts still key on one shared point per intersection; `Trip.lanes` reports the per-edge lane
    count so a consumer can size its within-carriageway offset per road instead of globally.
  * `set_route_metric()` -- an opt-in length- or time-weighted Dijkstra. The grid's default router is
    still hop-count BFS, byte-for-byte.

`largest_strong_component()` trims a one-way document to what is actually drivable, which a
bbox-clipped OSM/netconvert import needs, and `edges_from_directed()` converts the importers'
directed-edge records into this module's schema.

`CustomNetwork.set_road_surface()` (also opt-in) separates two questions the routing graph was
answering with one object: WHERE CAN A TRIP GO (the graph) and WHERE IS THERE TARMAC (the map).
`dist_to_road` only ever asked the second, and on an imported city the graph is a bad proxy for it
-- one centreline per physical road, nothing at all inside a junction. See that method for the
measured cost of conflating them.
"""
from __future__ import annotations

import heapq
import math
from collections import deque


class Trip:
    """A routed journey along a polyline of waypoints at a constant desired speed.

    `caps` (optional) holds a per-SEGMENT speed limit (m/s) aligned with the waypoint pairs --
    None entries mean unlimited. Networks with per-edge speed limits (highway vs residential in a
    custom map) pass it; the car-following loop then caps the vehicle's target speed per segment.

    `nodes` (optional, OPT-IN) holds the TRUE intersection coordinate for each waypoint, or None for
    a waypoint that is not an intersection. It exists because two features move `wp` off the graph
    nodes: (a) directed lane frames offset the driven polyline sideways off the road centreline, and
    (b) edge shape polylines insert curve vertices that are NOT junctions. Downstream code keys
    traffic-signal phase and gap-acceptance conflicts on the intersection identity returned by
    `next_node`, so that must stay the true node. `nodes=None` -> `next_node` returns `wp[k]`
    exactly as before (byte-identical default).

    `lanes` (optional, OPT-IN) holds the per-SEGMENT lane count IN THE DIRECTION OF TRAVEL, so a
    caller can size a vehicle's within-carriageway lane offset per edge instead of from one global
    n_lanes. `None` -> no per-edge lane information (unchanged)."""
    __slots__ = ("wp", "cum", "speed", "t0", "length", "t1", "caps", "nodes", "lanes")

    def __init__(self, waypoints: list[tuple[float, float]], speed: float, spawn_time: float,
                 caps: list | None = None, nodes: list | None = None, lanes: list | None = None):
        if len(waypoints) < 2:
            waypoints = [waypoints[0], (waypoints[0][0] + 1.0, waypoints[0][1])]
            caps = nodes = lanes = None
        self.wp = waypoints
        self.speed = max(1.0, speed)
        self.t0 = spawn_time
        self.caps = caps if (caps and any(c is not None for c in caps)) else None
        # `nodes` must be waypoint-aligned to be meaningful; a mismatched list is ignored rather than
        # silently mis-labelling junctions.
        self.nodes = nodes if (nodes is not None and len(nodes) == len(waypoints)) else None
        self.lanes = lanes if (lanes and any(n is not None for n in lanes)) else None
        cum = [0.0]
        for (ax, ay), (bx, by) in zip(waypoints, waypoints[1:]):
            cum.append(cum[-1] + math.hypot(bx - ax, by - ay))
        self.cum = cum
        self.length = cum[-1]
        self.t1 = spawn_time + self.length / self.speed   # arrival (despawn) time

    def cap_at(self, s: float):
        """Speed limit (m/s) of the segment containing arc-length s, or None (no caps/unlimited)."""
        if self.caps is None:
            return None
        for k in range(1, len(self.cum)):
            if s <= self.cum[k]:
                return self.caps[k - 1] if k - 1 < len(self.caps) else None
        return self.caps[-1] if self.caps else None

    def lanes_at(self, s: float):
        """Lane count of the segment containing arc-length s IN THE DIRECTION OF TRAVEL, or None
        (no per-edge lane information). Mirrors `cap_at` so the two are consumed the same way."""
        if self.lanes is None:
            return None
        for k in range(1, len(self.cum)):
            if s <= self.cum[k]:
                return self.lanes[k - 1] if k - 1 < len(self.lanes) else None
        return self.lanes[-1]

    def at_distance(self, d: float) -> tuple[float, float, float]:
        """(x, y, heading[deg]) at arc-length d along the route (clamped to the endpoints)."""
        if d <= 0.0:
            (ax, ay), (bx, by) = self.wp[0], self.wp[1]
            return ax, ay, math.degrees(math.atan2(by - ay, bx - ax)) % 360.0
        if d >= self.length:
            (ax, ay), (bx, by) = self.wp[-2], self.wp[-1]
            return bx, by, math.degrees(math.atan2(by - ay, bx - ax)) % 360.0
        for k in range(1, len(self.cum)):
            if d <= self.cum[k]:
                (ax, ay), (bx, by) = self.wp[k - 1], self.wp[k]
                seg = self.cum[k] - self.cum[k - 1]
                f = (d - self.cum[k - 1]) / seg if seg > 0 else 0.0
                hd = math.degrees(math.atan2(by - ay, bx - ax)) % 360.0
                return ax + (bx - ax) * f, ay + (by - ay) * f, hd
        bx, by = self.wp[-1]
        return bx, by, 0.0

    def state(self, t: float) -> tuple[float, float, float, float]:
        """True (x, y, speed, heading[deg]) at time t at the FREE (constant) speed along the route."""
        x, y, hd = self.at_distance(self.speed * (t - self.t0))
        return x, y, self.speed, hd

    def next_node(self, s: float):
        """The next route INTERSECTION ahead of arc-length s: ((x, y), distance) or (None, inf).

        With `nodes` set (directed lane frames / edge shape polylines) the returned point is the TRUE
        junction coordinate -- shared by every approach -- not the laterally-offset driven vertex,
        and curve vertices are skipped. Traffic-signal phase lookup and gap-acceptance conflict
        grouping both key on this point, so it must agree across approaches or opposing streams stop
        seeing each other. Without `nodes` (the default) the driven vertex IS the junction and the
        behaviour is unchanged."""
        if self.nodes is None:
            for k in range(1, len(self.cum)):
                if self.cum[k] > s + 1e-6:
                    return self.wp[k], self.cum[k] - s
            return None, math.inf
        for k in range(1, len(self.cum)):
            if self.cum[k] > s + 1e-6 and self.nodes[k] is not None:
                return self.nodes[k], self.cum[k] - s
        return None, math.inf

    def next_turn(self, s: float) -> tuple[float, float]:
        """The next interior vertex ahead where the route BENDS: (distance, bend_angle[deg]).

        The bend angle is the change of heading at that vertex (0 = straight through, ~90 for a
        grid corner). Used to slow a vehicle realistically into turns. (inf, 0) if none ahead."""
        for k in range(1, len(self.wp) - 1):        # interior vertices have an outgoing segment
            if self.cum[k] > s + 1e-6:
                (ax, ay), (bx, by), (cx, cy) = self.wp[k - 1], self.wp[k], self.wp[k + 1]
                h_in = math.atan2(by - ay, bx - ax)
                h_out = math.atan2(cy - by, cx - bx)
                ang = abs(math.degrees(h_out - h_in))
                ang = ang if ang <= 180.0 else 360.0 - ang
                return self.cum[k] - s, ang
        return math.inf, 0.0


# --------------------------------------------------------------------------------------------
# Directed lane frames: give each direction of travel its OWN geometry instead of one shared
# centreline. Without this, opposing vehicles drive the identical polyline in opposite directions
# and physically pass through each other (only a heading filter keeps them out of car-following) --
# the realism harness counts those as traffic.overlap_events. All of it is OPT-IN: a network only
# builds offset geometry after `enable_directed_lanes()` is called, and draws no RNG either way.
# --------------------------------------------------------------------------------------------

#: drive_side -> sign of the carriageway offset in the LEFT-normal frame of the direction of travel.
#: The consumer's convention (run.py) is `x += off * -sin(h); y += off * cos(h)`, i.e. +off is to the
#: vehicle's LEFT. Right-hand traffic therefore sits at a NEGATIVE offset from the road centreline.
DRIVE_SIDES = {"right": -1.0, "left": 1.0}

MAX_LANES_PER_EDGE = 8               # sanity bound on a per-edge lane count (motorways top out ~6)


def _offset_polyline(pts: list, offs: list, miter_limit: float = 8.0) -> list:
    """Offset a polyline sideways: segment k is shifted by offs[k] along its LEFT normal, and the
    shifted segments are rejoined at each interior vertex by MITRE (intersecting the two offset
    lines). Returns exactly len(pts) points, so caps/lanes/nodes arrays stay index-aligned.

    Near-parallel joins (and mitres longer than `miter_limit` x the offset, i.e. hairpins) fall back
    to the midpoint of the two offset endpoints -- a bevel -- which keeps the result finite and the
    vertex count fixed. Zero offsets return the input points unchanged (identity), so an edge with no
    lateral offset never perturbs geometry."""
    n = len(pts)
    if n < 2 or not any(offs):
        return list(pts)
    seg = []                                       # (a', b') for each offset segment
    for k in range(n - 1):
        (ax, ay), (bx, by) = pts[k], pts[k + 1]
        dx, dy = bx - ax, by - ay
        L = math.hypot(dx, dy)
        if L <= 0.0:
            seg.append(((ax, ay), (bx, by)))
            continue
        o = offs[k] if k < len(offs) else 0.0
        nx, ny = -dy / L * o, dx / L * o           # LEFT normal, scaled by the offset
        seg.append(((ax + nx, ay + ny), (bx + nx, by + ny)))
    out = [seg[0][0]]
    for k in range(1, n - 1):
        (p1x, p1y), (p2x, p2y) = seg[k - 1]        # incoming offset segment
        (q1x, q1y), (q2x, q2y) = seg[k]            # outgoing offset segment
        rx, ry = p2x - p1x, p2y - p1y
        sx, sy = q2x - q1x, q2y - q1y
        den = rx * sy - ry * sx
        mid = ((p2x + q1x) * 0.5, (p2y + q1y) * 0.5)
        if abs(den) < 1e-12:                       # collinear / anti-parallel -> bevel
            out.append(mid)
            continue
        t = ((q1x - p1x) * sy - (q1y - p1y) * sx) / den
        ix, iy = p1x + t * rx, p1y + t * ry
        lim = miter_limit * max(1.0, abs(offs[k - 1] if k - 1 < len(offs) else 0.0),
                               abs(offs[k] if k < len(offs) else 0.0))
        if math.hypot(ix - pts[k][0], iy - pts[k][1]) > lim:   # hairpin -> bevel instead
            out.append(mid)
        else:
            out.append((ix, iy))
    out.append(seg[-1][1])
    return out


class _LaneFrameMixin:
    """Opt-in per-direction lateral geometry, shared by every topology.

    Model: a two-way road of L_fwd + L_bwd lanes is centred on the graph edge. Travelling a->b the
    carriageway centre sits `sign * L_fwd * W / 2` off the centreline in that direction's own left
    normal (sign = -1 for right-hand traffic), so the two directions occupy DISJOINT strips either
    side of the centreline and the opposing streams are `(L_fwd + L_bwd) * W / 2` apart -- 3.5 m for
    the 1+1 default, comfortably beyond any overlap threshold. A ONE-WAY edge owns the whole road, so
    its carriageway offset is 0 and its geometry is exactly the old centreline.

    Within a carriageway the individual lane offset is still `(i - (L-1)/2) * W` about the
    carriageway centre -- exactly the formula the caller already uses with a global n_lanes -- so a
    consumer needs no new lateral maths, only the per-edge L from `Trip.lanes`."""

    directed_lanes = False               # class-level default: OFF -> no offsets, no new state
    _lane_w = 3.5
    _side = -1.0                         # right-hand traffic
    _lanes_per_dir = 1

    def enable_directed_lanes(self, lane_width_m: float = 3.5, drive_side: str = "right",
                              lanes_per_dir: int = 1):
        """Turn on directed lane frames. Idempotent, draws no RNG, and returns self for chaining.
        `lanes_per_dir` is the fallback lane count per direction for edges that carry none."""
        w = float(lane_width_m)
        if not (w > 0.0) or not math.isfinite(w):
            raise ValueError(f"lane_width_m must be a finite positive number (got {lane_width_m!r})")
        if drive_side not in DRIVE_SIDES:
            raise ValueError(f"drive_side must be one of {sorted(DRIVE_SIDES)} (got {drive_side!r})")
        lpd = int(lanes_per_dir)
        if not (1 <= lpd <= MAX_LANES_PER_EDGE):
            raise ValueError(f"lanes_per_dir must be 1..{MAX_LANES_PER_EDGE} (got {lanes_per_dir!r})")
        self.directed_lanes = True
        self._lane_w = w
        self._side = DRIVE_SIDES[drive_side]
        self._lanes_per_dir = lpd
        return self

    def carriageway_offset(self, lanes_dir: int, oneway: bool) -> float:
        """Lateral offset (m, +ve = LEFT of travel) of the carriageway centre for a direction with
        `lanes_dir` lanes. One-way roads keep the centreline (0.0)."""
        if oneway:
            return 0.0
        return self._side * 0.5 * max(1, int(lanes_dir)) * self._lane_w

    def _lane_frame(self, coords: list, lanes_seq: list, oneway_seq: list):
        """(driven_waypoints, true_nodes) for a route: `coords` offset per segment into its own
        carriageway, plus the untouched junction coordinates for `Trip.nodes`."""
        offs = [self.carriageway_offset(ln, ow) for ln, ow in zip(lanes_seq, oneway_seq)]
        return _offset_polyline(coords, offs), list(coords)


ROUTE_METRICS = ("hops", "length", "time")


def _check_route_metric(metric: str, free_speed_mps: float) -> tuple[str, float]:
    if metric not in ROUTE_METRICS:
        raise ValueError(f"route_metric must be one of {list(ROUTE_METRICS)} (got {metric!r})")
    fs = float(free_speed_mps)
    if not (fs > 0.0) or not math.isfinite(fs):
        raise ValueError(f"route_free_speed_mps must be finite and > 0 (got {free_speed_mps!r})")
    return metric, fs


class GridNetwork(_LaneFrameMixin):
    """A w x h grid of intersections spaced `block` metres apart, 4-neighbour roads."""

    def __init__(self, w: int, h: int, block: float, dropout: float = 0.0, seed: int = 0,
                 arterial_every: int = 0, arterial_speed: float = 0.0, local_speed: float = 0.0):
        self.w, self.h, self.block = int(w), int(h), float(block)
        # OPT-IN per-edge speed hierarchy (highway vs residential). arterial_every == 0 disables it
        # entirely -> random_trip attaches NO caps -> byte-identical to a plain grid. When enabled,
        # every arterial_every-th grid row and column is an arterial (through-road) posted at
        # arterial_speed; all other (local) roads are posted at local_speed. A 0 speed means that
        # tier is uncapped (vehicles drive their desired speed there).
        self.arterial_every = max(0, int(arterial_every))
        self.arterial_speed = float(arterial_speed)
        self.local_speed = float(local_speed)
        self.nodes = [(i, j) for i in range(self.w) for j in range(self.h)]
        self.center = (self.w // 2, self.h // 2)
        # perimeter intersections: realistic traffic sources/sinks (edges of the modelled area)
        self.boundary = [(i, j) for (i, j) in self.nodes
                         if i in (0, self.w - 1) or j in (0, self.h - 1)]
        # optional irregularity: remove a fraction of roads while keeping the grid CONNECTED (only
        # redundant, non-spanning-tree edges are droppable). dropout=0 -> no edges removed -> unchanged.
        self._dropped: set = set()
        self._closed: set = set()        # timed road closures (events); routing-only, road still exists
        if dropout > 0:
            self._dropped = self._pick_dropped(dropout, seed)

    def _full_neighbors(self, n):
        i, j = n
        out = []
        if i > 0: out.append((i - 1, j))
        if i < self.w - 1: out.append((i + 1, j))
        if j > 0: out.append((i, j - 1))
        if j < self.h - 1: out.append((i, j + 1))
        return out

    def _pick_dropped(self, dropout: float, seed: int) -> set:
        import random as _r
        edges, tree = [], set()
        for n in self.nodes:                         # every undirected edge once
            for m in self._full_neighbors(n):
                if m > n:
                    edges.append((n, m))
        # BFS spanning tree (protected so the graph stays connected)
        prev = {self.nodes[0]: None}
        q = deque([self.nodes[0]])
        while q:
            n = q.popleft()
            for m in self._full_neighbors(n):
                if m not in prev:
                    prev[m] = n
                    tree.add(frozenset((n, m)))
                    q.append(m)
        droppable = [e for e in edges if frozenset(e) not in tree]
        _r.Random(f"{seed}:dropout").shuffle(droppable)
        n_drop = min(len(droppable), int(dropout * len(edges)))
        return {frozenset(e) for e in droppable[:n_drop]}

    def _coord(self, n: tuple[int, int]) -> tuple[float, float]:
        return (n[0] * self.block, n[1] * self.block)

    def node_phase(self, node) -> int:
        """Deterministic 2-colouring of an intersection for signal timing: the grid checkerboard
        (i + j) % 2, recovered from the node's metre coordinate. `node` is an (x, y) waypoint (what
        Trip.next_node returns), so a signal's phase is a STABLE property of the intersection rather
        than something re-derived from arbitrary coordinates. Matches the historical grid phase
        exactly (coords are integer multiples of `block`), so grid signal timing is byte-identical."""
        ni = int(round(node[0] / self.block))
        nj = int(round(node[1] / self.block))
        return (ni + nj) % 2

    def _edge_cap(self, n1: tuple[int, int], n2: tuple[int, int]):
        """Posted speed limit (m/s) for the grid edge n1-n2, or None (uncapped). Arterials are the
        every-Nth rows/columns; everything else is local. Only called when the feature is enabled."""
        (i1, j1), (i2, j2) = n1, n2
        if j1 == j2:                                     # horizontal edge -> runs along row j1
            arterial = (j1 % self.arterial_every == 0)
        else:                                            # vertical edge -> runs along column i1
            arterial = (i1 % self.arterial_every == 0)
        sp = self.arterial_speed if arterial else self.local_speed
        return sp if sp > 0 else None

    # ---- OPT-IN directed lanes / weighted routing (all default-inert) ----
    route_metric = "hops"                 # class defaults: nothing allocated, nothing changed
    route_free_speed = 13.9
    _arterial_lanes = 0                   # 0 -> arterials use the same lanes_per_dir as local roads

    def enable_directed_lanes(self, lane_width_m: float = 3.5, drive_side: str = "right",
                              lanes_per_dir: int = 1, arterial_lanes: int = 0):
        """As `_LaneFrameMixin.enable_directed_lanes`, plus a grid-specific `arterial_lanes`: the
        per-direction lane count on the every-Nth arterial rows/columns (0 = same as local roads).
        Needs `arterial_every > 0` to mean anything -- the arterial grid lines are defined by it."""
        super().enable_directed_lanes(lane_width_m, drive_side, lanes_per_dir)
        al = int(arterial_lanes)
        if al and not (1 <= al <= MAX_LANES_PER_EDGE):
            raise ValueError(f"arterial_lanes must be 0 or 1..{MAX_LANES_PER_EDGE} (got {arterial_lanes!r})")
        if al and self.arterial_every <= 0:
            raise ValueError("arterial_lanes needs arterial_every > 0 (that is what defines which "
                             "grid rows/columns are arterials)")
        self._arterial_lanes = al
        return self

    def _is_arterial(self, n1, n2) -> bool:
        if self.arterial_every <= 0:
            return False
        (i1, j1), (i2, j2) = n1, n2
        return (j1 % self.arterial_every == 0) if j1 == j2 else (i1 % self.arterial_every == 0)

    def edge_lanes(self, n1, n2) -> int:
        """Lane count for travelling n1 -> n2 (grid roads are all two-way; arterials may be wider)."""
        if self._arterial_lanes and self._is_arterial(n1, n2):
            return self._arterial_lanes
        return self._lanes_per_dir

    def set_route_metric(self, metric: str = "hops", free_speed_mps: float = 13.9):
        """Choose the routing objective. "hops" (default) keeps the historical BFS hop-count path
        BYTE-FOR-BYTE. "length" and "time" switch to Dijkstra: on a uniform grid every edge is one
        `block` long so "length" only changes tie-breaking, but "time" weights each edge by
        block / posted_speed, so traffic prefers the arterials over the local grid -- the actual
        behaviour a navigation system produces. `free_speed_mps` is the speed assumed on edges with
        no posted limit."""
        self.route_metric, self.route_free_speed = _check_route_metric(metric, free_speed_mps)
        return self

    def _edge_weight(self, n1, n2) -> float:
        if self.route_metric == "hops":
            return 1.0
        if self.route_metric == "length":
            return self.block
        cap = self._edge_cap(n1, n2) if self.arterial_every > 0 else None
        return self.block / (cap if cap else self.route_free_speed)

    def _dijkstra(self, o, d) -> list:
        """Weighted shortest path (ties broken by node tuple order -> deterministic)."""
        dist = {o: 0.0}
        prev: dict = {o: None}
        pq = [(0.0, o)]
        while pq:
            dd, cur = heapq.heappop(pq)
            if cur == d:
                break
            if dd > dist.get(cur, math.inf) + 1e-9:
                continue
            for m in self._neighbors(cur):
                nd = dd + self._edge_weight(cur, m)
                if nd < dist.get(m, math.inf) - 1e-9:
                    dist[m] = nd
                    prev[m] = cur
                    heapq.heappush(pq, (nd, m))
        if d not in prev:
            return [o]
        path, cur = [], d
        while cur is not None:
            path.append(cur)
            cur = prev[cur]
        return list(reversed(path))

    def _path(self, o, d) -> list:
        return self._bfs(o, d) if self.route_metric == "hops" else self._dijkstra(o, d)

    def dist_to_road(self, x: float, y: float) -> float:
        """HD-map check: distance from (x,y) to the nearest road. On a grid the roads are the lines
        x=k*block and y=k*block within the network extent."""
        gw, gh, blk = (self.w - 1) * self.block, (self.h - 1) * self.block, self.block
        vx = round(x / blk) * blk
        dv = abs(x - vx) if (0 <= vx <= gw and -blk <= y <= gh + blk) else 1e9
        hy = round(y / blk) * blk
        dh = abs(y - hy) if (0 <= hy <= gh and -blk <= x <= gw + blk) else 1e9
        return min(dv, dh)

    def _neighbors(self, n: tuple[int, int]) -> list[tuple[int, int]]:
        nb = self._full_neighbors(n)
        if self._dropped:
            nb = [m for m in nb if frozenset((n, m)) not in self._dropped]
        if self._closed:
            nb = [m for m in nb if frozenset((n, m)) not in self._closed]
        return nb

    def set_closures(self, pairs) -> list:
        """Replace the timed-closure set. Each pair ((i,j),(i2,j2)) is applied only if the road
        exists and closing it keeps the network CONNECTED (a bridge closure is skipped). Returns
        the list of pairs actually applied. Deterministic (pairs processed in the given order)."""
        self._closed = set()
        applied = []
        for pair in pairs:
            try:
                a, b = tuple(pair[0]), tuple(pair[1])
            except (TypeError, IndexError):
                continue                        # wrong shape for a grid edge (e.g. [a,b] indices)
            if a not in self.nodes or b not in self._full_neighbors(a):
                continue
            e = frozenset((a, b))
            if e in self._dropped or e in self._closed:
                continue
            self._closed.add(e)
            seen = {self.nodes[0]}
            q = deque([self.nodes[0]])
            while q:
                for m in self._neighbors(q.popleft()):
                    if m not in seen:
                        seen.add(m)
                        q.append(m)
            if len(seen) == len(self.nodes):
                applied.append((a, b))
            else:                                   # closing this road would strand intersections
                self._closed.discard(e)
        return applied

    def geometry(self) -> dict:
        """Static road geometry for UIs: {'nodes': [[x,y]...], 'edges': [[i0,i1]...]} (dropout
        respected; timed closures NOT removed -- a closed road still exists physically)."""
        idx = {n: k for k, n in enumerate(self.nodes)}
        edges = []
        for n in self.nodes:
            for m in self._full_neighbors(n):
                if m > n and frozenset((n, m)) not in self._dropped:
                    edges.append([idx[n], idx[m]])
        return {"nodes": [[*self._coord(n)] for n in self.nodes], "edges": edges}

    def _bfs(self, o: tuple[int, int], d: tuple[int, int]) -> list[tuple[int, int]]:
        prev = {o: None}
        q = deque([o])
        while q:
            n = q.popleft()
            if n == d:
                break
            for m in self._neighbors(n):
                if m not in prev:
                    prev[m] = n
                    q.append(m)
        path, cur = [], d
        while cur is not None:
            path.append(cur)
            cur = prev.get(cur)
        return list(reversed(path))

    def _gravity_dest(self, rng, o: tuple[int, int], min_hops: int, scale: float):
        """Pick a destination with a distance-decay (gravity) law: weight ~ exp(-(hops-min)/scale),
        so most trips are short and a few are long -- the realistic urban trip-length distribution.
        Deterministic (fixed node order + a single rng draw)."""
        cands, weights = [], []
        s = max(0.5, scale)
        for n in self.nodes:
            hd = abs(n[0] - o[0]) + abs(n[1] - o[1])
            if hd >= min_hops:
                cands.append(n)
                weights.append(math.exp(-(hd - min_hops) / s))
        if not cands:
            return o
        r = rng.random() * sum(weights)
        acc = 0.0
        for n, w in zip(cands, weights):
            acc += w
            if r <= acc:
                return n
        return cands[-1]

    def random_trip(self, rng, speed: float, spawn_time: float, min_hops: int = 3,
                    dest_hint=None, od_model: str = "uniform", gravity_scale: float = 2.0,
                    boundary_origin: bool = False) -> Trip:
        o = rng.choice(self.boundary if (boundary_origin and self.boundary) else self.nodes)
        if dest_hint is not None and dest_hint != o:
            d = dest_hint                         # OD bias (e.g. commute toward the centre)
        elif od_model == "gravity":
            d = self._gravity_dest(rng, o, min_hops, gravity_scale)
        else:
            d = o
            for _ in range(8):
                cand = rng.choice(self.nodes)
                if abs(cand[0] - o[0]) + abs(cand[1] - o[1]) >= min_hops:
                    d = cand
                    break
        path = self._path(o, d)
        wp = [self._coord(n) for n in path]
        caps = None
        if self.arterial_every > 0 and (self.arterial_speed > 0 or self.local_speed > 0):
            caps = [self._edge_cap(a, b) for a, b in zip(path, path[1:])]
        if not self.directed_lanes or len(wp) < 2:
            return Trip(wp, speed, spawn_time, caps=caps)
        lanes = [self.edge_lanes(a, b) for a, b in zip(path, path[1:])]
        driven, nodes = self._lane_frame(wp, lanes, [False] * len(lanes))   # grid roads are two-way
        return Trip(driven, speed, spawn_time, caps=caps, nodes=nodes, lanes=lanes)


def _pt_seg_dist(px, py, ax, ay, bx, by):
    """Distance from point (px,py) to segment (a,b)."""
    dx, dy = bx - ax, by - ay
    dd = dx * dx + dy * dy
    t = 0.0 if dd == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / dd))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


class RingNetwork(_LaneFrameMixin):
    """A ring road: `n` intersections evenly spaced on a circle, connected in a cycle. Vehicles route
    the shorter way round. Same Trip/coord interface as GridNetwork (topology-agnostic downstream)."""

    def __init__(self, n: int, block: float, ring_speed: float = 0.0):
        self.n = max(3, int(n))
        self.block = float(block)
        # OPT-IN single speed limit for the whole ring (m/s); 0 = uncapped -> no caps -> byte-identical.
        # A ring has no row/column structure, so a per-edge arterial rule does not apply; a uniform
        # posted limit is the natural analogue (documented deviation from the grid's tiered rule).
        self.ring_speed = float(ring_speed)
        self.R = self.n * self.block / (2.0 * math.pi)     # circumference ~ n*block
        self.cx = self.cy = self.R                          # centre (keeps coords >= 0)
        self.nodes = list(range(self.n))
        self.center = self.n // 2                          # dest-hint compat (ignored by ring trips)
        self.boundary = list(self.nodes)                   # every node is on the ring
        self.w = self.h = self.n                            # for RSU-placement compatibility

    def _coord(self, i: int) -> tuple[float, float]:
        th = 2.0 * math.pi * (i % self.n) / self.n
        return (self.cx + self.R * math.cos(th), self.cy + self.R * math.sin(th))

    def node_phase(self, node) -> int:
        """Deterministic 2-colouring for signal timing: alternate around the ring by node index
        parity. `node` is an (x, y) waypoint; recover the ring index from its polar angle. Adjacent
        ring intersections alternate phase, giving coherent (not coordinate-noise) signal timing."""
        th = math.atan2(node[1] - self.cy, node[0] - self.cx)
        i = int(round(th / (2.0 * math.pi) * self.n)) % self.n
        return i % 2

    # ---- OPT-IN directed lanes / routing (default-inert) ----
    ring_oneway = False                   # a gyratory: traffic may only run one way round
    route_metric = "hops"
    route_free_speed = 13.9

    def enable_directed_lanes(self, lane_width_m: float = 3.5, drive_side: str = "right",
                              lanes_per_dir: int = 1, oneway: bool = False):
        """As `_LaneFrameMixin.enable_directed_lanes`. `oneway=True` makes the ring a GYRATORY:
        every trip runs the same way round (increasing node index) and, having no opposing stream,
        the carriageway stays on the centreline."""
        super().enable_directed_lanes(lane_width_m, drive_side, lanes_per_dir)
        self.ring_oneway = bool(oneway)
        return self

    def set_route_metric(self, metric: str = "hops", free_speed_mps: float = 13.9):
        """Accepted for interface parity, but a ring has ONE route between any two nodes in each
        direction and every arc is the same length at the same posted speed, so hop-, length- and
        time-optimal all select the identical arc. Stored for reporting; the route is unchanged."""
        self.route_metric, self.route_free_speed = _check_route_metric(metric, free_speed_mps)
        return self

    def _arc(self, o: int, d: int) -> list[int]:
        cw, ccw = (d - o) % self.n, (o - d) % self.n
        if self.ring_oneway:                       # gyratory: only the forward direction is legal
            return [(o + k) % self.n for k in range(cw + 1)]
        return [(o + k) % self.n for k in range(cw + 1)] if cw <= ccw \
            else [(o - k) % self.n for k in range(ccw + 1)]

    def dist_to_road(self, x: float, y: float) -> float:
        best = 1e18
        for i in range(self.n):
            ax, ay = self._coord(i)
            bx, by = self._coord(i + 1)
            best = min(best, _pt_seg_dist(x, y, ax, ay, bx, by))
        return best

    def random_trip(self, rng, speed: float, spawn_time: float, min_hops: int = 3,
                    dest_hint=None, od_model: str = "uniform", gravity_scale: float = 2.0,
                    boundary_origin: bool = False) -> Trip:
        o = rng.choice(self.nodes)
        d = o
        for _ in range(8):
            cand = rng.choice(self.nodes)
            if min((cand - o) % self.n, (o - cand) % self.n) >= min_hops:
                d = cand
                break
        wp = [self._coord(i) for i in self._arc(o, d)]
        caps = [self.ring_speed] * (len(wp) - 1) if self.ring_speed > 0 else None
        if not self.directed_lanes or len(wp) < 2:
            return Trip(wp, speed, spawn_time, caps=caps)
        lanes = [self._lanes_per_dir] * (len(wp) - 1)
        oneway = [self.ring_oneway] * len(lanes)
        driven, nodes = self._lane_frame(wp, lanes, oneway)
        return Trip(driven, speed, spawn_time, caps=caps, nodes=nodes, lanes=lanes)

    def geometry(self) -> dict:
        return {"nodes": [[*self._coord(i)] for i in self.nodes],
                "edges": [[i, (i + 1) % self.n] for i in range(self.n)]}


def _as_bool_oneway(v, a, b) -> int:
    """OSM-style oneway value -> 0 (two-way), +1 (a->b only) or -1 (b->a only)."""
    if v is None or v is False or v == 0 or v in ("no", "false", "0", ""):
        return 0
    if v is True or v == 1 or v in ("yes", "true", "1", "forward"):
        return 1
    if v == -1 or v in ("-1", "reverse", "backward"):
        return -1
    raise ValueError(f"edge [{a},{b}] oneway must be true/false or -1 (OSM reverse-direction "
                     f"convention); got {v!r}")


def _as_lane_count(v, a, b, what) -> int:
    try:
        n = int(v)
    except (TypeError, ValueError):
        raise ValueError(f"edge [{a},{b}] {what} must be a whole number of lanes "
                         f"(got {v!r})") from None
    if not (1 <= n <= MAX_LANES_PER_EDGE):
        raise ValueError(f"edge [{a},{b}] {what}={n} out of range 1-{MAX_LANES_PER_EDGE}")
    return n


def parse_edge_spec(e, n_nodes: int) -> dict:
    """One custom-network edge entry -> a normalised spec dict. THE EDGE SCHEMA lives here.

    Positional (legacy-compatible, extended):
        [a, b]                             two-way, no posted limit, default lanes
        [a, b, speed]                      + posted limit in m/s (1..70), null = none
        [a, b, speed, lanes]               + TOTAL lanes on the road (OSM `lanes` convention)
        [a, b, speed, lanes, oneway]       + true / false / -1 (OSM `oneway`, -1 = b->a only)

    Object form (everything the positional form has, plus the per-direction and geometry keys):
        {"a": 0, "b": 1,                   node indices (required)
         "speed": 13.9,                    m/s, optional
         "lanes": 4,                       TOTAL lanes; split evenly unless overridden
         "lanes_forward": 3,               lanes a->b   (overrides the split)
         "lanes_backward": 1,              lanes b->a   (overrides the split)
         "oneway": true,                   true | false | -1
         "shape": [[x,y], ...]}            intermediate CURVE vertices (metres), a->b order

    Notes that matter:
      * `lanes` follows OSM: it is the TOTAL across both directions of a two-way road, so
        `{"lanes": 2}` is one lane each way, NOT two each way. A one-way edge gives all of them to
        its single legal direction.
      * `shape` carries curve geometry ON the edge, so a bend no longer costs graph nodes. That is
        what lets the node cap buy far more map.
      * Two one-way entries for the same node pair (a->b and b->a) describe one physical road with
        independent per-direction properties; they merge into a single two-way edge.
    Returns keys: a, b, speed, lanes_f, lanes_b, oneway, shape (all optional values may be None)."""
    if isinstance(e, dict):
        if "a" not in e or "b" not in e:
            raise ValueError(f'edge object {e!r} needs "a" and "b" node indices')
        raw = [e.get("a"), e.get("b"), e.get("speed"), e.get("lanes"), e.get("oneway")]
        lf_in, lb_in, shape_in = e.get("lanes_forward"), e.get("lanes_backward"), e.get("shape")
    elif isinstance(e, (list, tuple)):
        raw = list(e) + [None] * (5 - len(e))
        lf_in = lb_in = shape_in = None
    else:
        raise ValueError(f"edge {e!r} must be [a, b, ...] node indices or an object with a/b")
    try:
        a, b = int(raw[0]), int(raw[1])
    except (TypeError, ValueError, IndexError):
        raise ValueError(f"edge {e!r} must be [a, b] node indices "
                         f"(optionally [a, b, speed_mps, lanes, oneway])") from None
    if not (0 <= a < n_nodes and 0 <= b < n_nodes):
        raise ValueError(f"edge [{a},{b}] references a missing node (have {n_nodes} nodes)")
    if a == b:
        raise ValueError(f"edge [{a},{b}] is a self-loop")
    speed = None
    if raw[2] is not None:
        speed = float(raw[2])
        if not (1.0 <= speed <= 70.0):
            raise ValueError(f"edge [{a},{b}] speed limit {speed} out of range 1-70 m/s "
                             f"(33 ~ 120 km/h highway, 8.3 ~ 30 km/h zone)")
    oneway = _as_bool_oneway(raw[4], a, b)
    total = _as_lane_count(raw[3], a, b, "lanes") if raw[3] is not None else None
    lf = _as_lane_count(lf_in, a, b, "lanes_forward") if lf_in is not None else None
    lb = _as_lane_count(lb_in, a, b, "lanes_backward") if lb_in is not None else None
    if oneway > 0:
        lf = lf if lf is not None else total          # all lanes belong to the legal direction
        lb = None
    elif oneway < 0:
        lb = lb if lb is not None else total
        lf = None
    elif total is not None:                            # two-way: OSM `lanes` is the TOTAL -> split
        if lf is None and lb is None:
            lf = max(1, total // 2)
            lb = max(1, total - lf)
        elif lf is None:
            lf = max(1, total - lb)
        elif lb is None:
            lb = max(1, total - lf)
    shape = None
    if shape_in is not None:
        if not isinstance(shape_in, (list, tuple)):
            raise ValueError(f"edge [{a},{b}] shape must be a list of [x, y] points")
        pts = []
        for k, p in enumerate(shape_in):
            try:
                px, py = float(p[0]), float(p[1])
            except (TypeError, ValueError, IndexError):
                raise ValueError(f"edge [{a},{b}] shape[{k}] must be [x, y] in metres "
                                 f"(got {p!r})") from None
            if not (math.isfinite(px) and math.isfinite(py)):
                raise ValueError(f"edge [{a},{b}] shape[{k}] has a non-finite coordinate")
            pts.append((px, py))
        shape = tuple(pts) or None
    return {"a": a, "b": b, "speed": speed, "lanes_f": lf, "lanes_b": lb,
            "oneway": oneway, "shape": shape}


def largest_strong_component(nodes, edges) -> tuple[list, list, dict]:
    """Trim a (possibly one-way) node/edge document to its largest STRONGLY connected component.

    Real one-way imports need this. An OSM extract is clipped to a bbox, and the importer keeps the
    largest WEAKLY connected component -- fine while every road was bidirectional, but once `oneway`
    is honoured a clipped one-way pair (in on one street, out on another that leaves the box) becomes
    a node you can enter and never leave. MEASURED on the cached Ingolstadt extract: the directed
    graph has 6 such nodes, which is exactly the CustomNetwork strong-connectivity error.

    Returns (nodes, edges, info) with node indices REMAPPED to the kept set; `info` reports what was
    dropped. Deterministic: Tarjan in index order, ties broken by lowest node index. Draws no RNG."""
    n = len(nodes)
    specs = [parse_edge_spec(e, n) for e in edges]
    out_adj: dict[int, list[int]] = {i: [] for i in range(n)}
    for s in specs:
        a, b, ow = s["a"], s["b"], s["oneway"]
        if ow >= 0:
            out_adj[a].append(b)
        if ow <= 0:
            out_adj[b].append(a)
    index: dict[int, int] = {}
    low: dict[int, int] = {}
    on_stack: set = set()
    stack: list[int] = []
    comps: list[list[int]] = []
    counter = [0]
    for root in range(n):                       # iterative Tarjan (deep graphs blow the C stack)
        if root in index:
            continue
        work = [(root, 0)]
        while work:
            v, pi = work.pop()
            if pi == 0:
                index[v] = low[v] = counter[0]
                counter[0] += 1
                stack.append(v)
                on_stack.add(v)
            recurse = False
            for i in range(pi, len(out_adj[v])):
                w = out_adj[v][i]
                if w not in index:
                    work.append((v, i + 1))
                    work.append((w, 0))
                    recurse = True
                    break
                if w in on_stack:
                    low[v] = min(low[v], index[w])
            if recurse:
                continue
            if low[v] == index[v]:
                comp = []
                while True:
                    w = stack.pop()
                    on_stack.discard(w)
                    comp.append(w)
                    if w == v:
                        break
                comps.append(sorted(comp))
            if work:
                u = work[-1][0]
                low[u] = min(low[u], low[v])
    keep = set(max(comps, key=lambda c: (len(c), -c[0])))
    remap = {old: new for new, old in enumerate(sorted(keep))}
    out_nodes = [nodes[i] for i in sorted(keep)]
    out_edges = []
    for e, s in zip(edges, specs):
        if s["a"] not in keep or s["b"] not in keep:
            continue
        if isinstance(e, dict):
            e2 = dict(e)
            e2["a"], e2["b"] = remap[s["a"]], remap[s["b"]]
        else:
            e2 = list(e)
            e2[0], e2[1] = remap[s["a"]], remap[s["b"]]
        out_edges.append(e2)
    return out_nodes, out_edges, {"kept_nodes": len(out_nodes), "kept_edges": len(out_edges),
                                  "dropped_nodes": n - len(out_nodes),
                                  "dropped_edges": len(edges) - len(out_edges),
                                  "n_components": len(comps)}


def edges_from_directed(records) -> list[dict]:
    """`osm.py`'s `directed_edges` -> custom-network edge specs this module can load.

    The importer emits ONE record per LEGAL direction of travel
    (`{"a", "b", "speed_mps", "lanes", "class"?, "roundabout"?}`), so a two-way street appears twice
    with its own per-direction lane count and a one-way street appears once. Each record therefore
    maps to a ONE-WAY spec, and `CustomNetwork` merges the two records of a two-way street back into
    a single physical road carrying (lanes_forward, lanes_backward) -- which is exactly the
    information the directed lane frame needs to place the two carriageways.

    Deterministic and order-preserving; draws no RNG. Unknown record keys are ignored."""
    out: list[dict] = []
    for rec in records:
        if not isinstance(rec, dict) or "a" not in rec or "b" not in rec:
            raise ValueError(f"directed edge record {rec!r} needs 'a' and 'b' node indices")
        sp = rec.get("speed_mps", rec.get("speed"))
        e: dict = {"a": rec["a"], "b": rec["b"], "oneway": True}
        if sp is not None:
            e["speed"] = sp
        if rec.get("lanes") is not None:
            e["lanes"] = rec["lanes"]
        if rec.get("shape") is not None:
            e["shape"] = rec["shape"]
        out.append(e)
    return out


class CustomNetwork(_LaneFrameMixin):
    """An arbitrary road graph: nodes at metre coordinates + edges that may be one-way, may carry
    their own lane counts, and may carry their own curve geometry. This is the 'AI designs the map'
    primitive -- any topology (radial city, highway with on-ramps, river town with two bridges, ...)
    expressed as {nodes: [[x,y]...], edges: [...]}. See `parse_edge_spec` for the full edge schema;
    the historical `[a,b]` / `[a,b,speed]` forms load unchanged and behave exactly as before.

    Same interface as GridNetwork; routing = Dijkstra on edge length by default (deterministic
    tie-break), switchable with `set_route_metric`. dist_to_road uses an exact cell index over edge
    segments plus a memo cache (the same claimed position is checked by every receiver in range, so
    caching is nearly free coverage)."""

    # Caps sized by MEASUREMENT (Python 3.12, Windows Server 2022; a 63x63 lattice, 3969 nodes):
    #   400 nodes /  1121 edges (the old cap):  3.1 ms build, 0.34 MB, 36 us / cold dist_to_road,
    #                                           0.10 ms / route
    #   3969 nodes / 11656 edges (this cap):   36.6 ms build, 5.81 MB, 50 us / cold dist_to_road,
    #                                           1.16 ms / route
    # Nothing is super-linear. The PER-STEP term (dist_to_road) is essentially flat in map size --
    # 22.5 -> 29.3 us over a 10x node increase -- because the cell index is sized from the median
    # segment; routing is the only term that grows, and it is paid once per SPAWN (a pre-pass), not
    # once per step. `edges` counts the INPUT list, so a document from `edges_from_directed` (one
    # record per direction) fits ~6000 physical two-way roads.
    MAX_NODES = 4000
    MAX_EDGES = 12000

    def __init__(self, nodes, edges):
        if not isinstance(nodes, (list, tuple)) or len(nodes) < 2:
            raise ValueError("custom network needs at least 2 nodes ([[x,y], ...] in metres)")
        if len(nodes) > self.MAX_NODES:
            raise ValueError(f"custom network too large: {len(nodes)} nodes (max {self.MAX_NODES})")
        self.coords: list[tuple[float, float]] = []
        for k, p in enumerate(nodes):
            try:
                x, y = float(p[0]), float(p[1])
            except (TypeError, ValueError, IndexError):
                raise ValueError(f"node {k} must be [x, y] in metres (got {p!r})") from None
            if not (math.isfinite(x) and math.isfinite(y)):
                raise ValueError(f"node {k} has a non-finite coordinate")
            self.coords.append((x, y))
        if not isinstance(edges, (list, tuple)) or not edges:
            raise ValueError("custom network needs at least 1 edge ([[a,b], ...] node indices)")
        if len(edges) > self.MAX_EDGES:
            raise ValueError(f"custom network too large: {len(edges)} edges (max {self.MAX_EDGES})")
        n = len(self.coords)
        seen: set = set()
        self.edge_speed: dict[tuple[int, int], float] = {}   # optional per-edge limit (m/s)
        # OPT-IN per-edge structure; each dict stays EMPTY for a legacy [a,b] / [a,b,speed] map, and
        # every code path below checks emptiness first, so an old map takes the old branch exactly.
        self.edge_oneway: dict[tuple[int, int], int] = {}    # +1 = low->high only, -1 = high->low
        self.edge_lanes: dict[tuple[int, int], tuple[int, int]] = {}   # (lanes low->high, high->low)
        self.edge_shape: dict[tuple[int, int], tuple] = {}   # intermediate curve vertices, low->high
        allow: dict[tuple[int, int], list[bool]] = {}        # [low->high legal, high->low legal]
        for e in edges:
            spec = parse_edge_spec(e, n)
            a, b, key = spec["a"], spec["b"], (min(spec["a"], spec["b"]), max(spec["a"], spec["b"]))
            fwd_is_low = (a == key[0])                       # is a->b the low->high direction?
            seen.add(key)
            if spec["speed"] is not None:
                # two records for one physical road (the per-direction form) keep the HIGHER posted
                # limit -- the same rule osm.py:171 uses when two OSM ways share a segment
                self.edge_speed[key] = max(self.edge_speed.get(key, 0.0), spec["speed"])
            ow = spec["oneway"]
            # a->b legal unless oneway says b->a only; b->a legal unless oneway says a->b only
            ab, ba = (ow >= 0), (ow <= 0)
            cur = allow.setdefault(key, [False, False])
            cur[0] |= (ab if fwd_is_low else ba)             # low->high
            cur[1] |= (ba if fwd_is_low else ab)             # high->low
            if spec["lanes_f"] is not None or spec["lanes_b"] is not None:
                lo, hi = self.edge_lanes.get(key, (0, 0))
                l_ab, l_ba = spec["lanes_f"] or 0, spec["lanes_b"] or 0
                self.edge_lanes[key] = ((max(lo, l_ab), max(hi, l_ba)) if fwd_is_low
                                        else (max(lo, l_ba), max(hi, l_ab)))
            if spec["shape"] is not None:
                sh = spec["shape"] if fwd_is_low else tuple(reversed(spec["shape"]))
                if key in self.edge_shape and self.edge_shape[key] != sh:
                    raise ValueError(f"edge [{a},{b}] is given two different shapes -- one physical "
                                     f"road has one geometry (reverse it for the other direction)")
                self.edge_shape[key] = sh
        for key, (lo_hi, hi_lo) in allow.items():
            if not (lo_hi and hi_lo):
                self.edge_oneway[key] = 1 if lo_hi else -1
        # a two-way edge whose lanes were only given for one direction still needs the other side
        for key, (lo, hi) in list(self.edge_lanes.items()):
            ow = self.edge_oneway.get(key, 0)
            self.edge_lanes[key] = (max(1, lo) if ow >= 0 else 0, max(1, hi) if ow <= 0 else 0)
        self.directed = bool(self.edge_oneway)
        self.edges: list[tuple[int, int]] = sorted(seen)
        self.nodes = list(range(n))
        # edge length = POLYLINE length (identical to the straight distance when there is no shape)
        self.edge_len: dict[tuple[int, int], float] = {}
        for key in self.edges:
            a, b = key
            sh = self.edge_shape.get(key)
            if sh is None:
                d = math.dist(self.coords[a], self.coords[b])
            else:
                pts = [self.coords[a], *sh, self.coords[b]]
                d = sum(math.dist(p, q) for p, q in zip(pts, pts[1:]))
            if d < 1.0:
                raise ValueError(f"edge [{a},{b}] is shorter than 1 m -- merge those nodes")
            self.edge_len[key] = d
        # `uadj` ignores one-ways (graph colouring, dead-end detection, underlying connectivity);
        # `adj` is what ROUTING walks and honours them. Undirected map -> the same object, so an
        # unchanged map allocates and iterates exactly what it did before.
        self.uadj: dict[int, list[tuple[int, float]]] = {i: [] for i in self.nodes}
        for a, b in self.edges:
            d = self.edge_len[(a, b)]
            self.uadj[a].append((b, d))
            self.uadj[b].append((a, d))
        if not self.directed:
            self.adj = self.uadj
        else:
            self.adj = {i: [] for i in self.nodes}
            for key in self.edges:
                a, b = key
                d, ow = self.edge_len[key], self.edge_oneway.get(key, 0)
                if ow >= 0:
                    self.adj[a].append((b, d))
                if ow <= 0:
                    self.adj[b].append((a, d))
        # must be CONNECTED (unreachable islands would strand trips). A directed map must be STRONGLY
        # connected: reachable one way is not enough -- a trip that can arrive but never leave, or a
        # destination nothing can reach, is a stranded vehicle.
        reach = {0}
        q = deque([0])
        while q:
            for m, _ in self.adj[q.popleft()]:
                if m not in reach:
                    reach.add(m)
                    q.append(m)
        if len(reach) != n:
            missing = sorted(set(self.nodes) - reach)
            raise ValueError(f"custom network must be connected: node(s) "
                             f"{missing[:12]}{'...' if len(missing) > 12 else ''} are unreachable "
                             f"from node 0 -- add edges linking them to the rest (or remove them)")
        if self.directed:
            rev: dict[int, list[int]] = {i: [] for i in self.nodes}
            for u in self.nodes:
                for v, _w in self.adj[u]:
                    rev[v].append(u)
            back = {0}
            q = deque([0])
            while q:
                for m in rev[q.popleft()]:
                    if m not in back:
                        back.add(m)
                        q.append(m)
            if len(back) != n:
                missing = sorted(set(self.nodes) - back)
                raise ValueError(f"one-way network must be STRONGLY connected: node(s) "
                                 f"{missing[:12]}{'...' if len(missing) > 12 else ''} cannot reach "
                                 f"node 0 -- a vehicle routed there could never leave. Add a return "
                                 f"path, relax the oneway flags on that side, or trim the document "
                                 f"with roads.largest_strong_component(nodes, edges) first (which "
                                 f"is what a bbox-clipped OSM import needs).")
        xs = [c[0] for c in self.coords]
        ys = [c[1] for c in self.coords]
        self.bbox = (min(xs), min(ys), max(xs), max(ys))
        cx, cy = sum(xs) / n, sum(ys) / n
        self.center = min(self.nodes, key=lambda i: (math.dist(self.coords[i], (cx, cy)), i))
        # boundary (sources/sinks): dead ends + nodes near the bounding-box rim; fallback all
        margin = max(20.0, 0.12 * max(self.bbox[2] - self.bbox[0], self.bbox[3] - self.bbox[1]))
        rim = [i for i in self.nodes if
               self.coords[i][0] <= self.bbox[0] + margin or self.coords[i][0] >= self.bbox[2] - margin or
               self.coords[i][1] <= self.bbox[1] + margin or self.coords[i][1] >= self.bbox[3] - margin]
        dead_ends = [i for i in self.nodes if len(self.uadj[i]) == 1]
        self.boundary = sorted(set(dead_ends) | set(rim)) or list(self.nodes)
        self.w = self.h = n                    # legacy attr compat (RSU spread/perimeter used instead)
        self.block = sum(self.edge_len.values()) / len(self.edges)   # mean road length (informational)
        self.total_road_m = sum(self.edge_len.values())
        self._closed: set = set()              # timed closures: {(a,b) sorted} routing-only
        self._hops_cache: dict[int, dict] = {}
        # ---- exact spatial index over the PHYSICAL road segments (shape vertices included) ----
        # `_segs` is the geometry distances are measured against; `_index` maps a cell -> the segments
        # passing through it. Long segments are CHUNKED for REGISTRATION ONLY -- the distance is
        # still taken against the whole parent segment -- so the result is bit-for-bit independent of
        # the cell size and of the chunking. That matters: the old rule sized the cell from the
        # LONGEST edge, so a single motorway link in an OSM import inflated the cell to kilometres
        # and collapsed every dist_to_road into a full scan (measured: 26 us -> 2.3 ms at 4k nodes).
        self._segs: list[tuple[float, float, float, float]] = []
        self._seg_edge: list[int] = []         # parent index into self.edges (shape sub-segments)
        for k, key in enumerate(self.edges):
            a, b = key
            sh = self.edge_shape.get(key)
            pts = ([self.coords[a], self.coords[b]] if sh is None
                   else [self.coords[a], *sh, self.coords[b]])
            for p, q in zip(pts, pts[1:]):
                self._segs.append((p[0], p[1], q[0], q[1]))
                self._seg_edge.append(k)
        _lens = sorted(math.hypot(s[2] - s[0], s[3] - s[1]) for s in self._segs)
        self._cell = min(600.0, max(100.0, 2.0 * _lens[len(_lens) // 2]))   # robust: median, not max
        cell = self._cell
        _ix: dict[tuple[int, int], set] = {}
        for k, (ax, ay, bx, by) in enumerate(self._segs):
            steps = max(1, int(math.hypot(bx - ax, by - ay) // cell) + 1)   # chunks <= one cell long
            for c in range(steps):
                t0, t1 = c / steps, (c + 1) / steps
                px0, py0 = ax + (bx - ax) * t0, ay + (by - ay) * t0
                px1, py1 = ax + (bx - ax) * t1, ay + (by - ay) * t1
                for ci in range(int(min(px0, px1) // cell), int(max(px0, px1) // cell) + 1):
                    for cj in range(int(min(py0, py1) // cell), int(max(py0, py1) // cell) + 1):
                        _ix.setdefault((ci, cj), set()).add(k)
        self._index: dict[tuple[int, int], tuple] = {c: tuple(sorted(v)) for c, v in _ix.items()}
        _cis = [c[0] for c in self._index]
        _cjs = [c[1] for c in self._index]
        self._cbox = (min(_cis), min(_cjs), max(_cis), max(_cjs))   # occupied cell-index extent
        self._max_ring = int(max(self.bbox[2] - self.bbox[0], self.bbox[3] - self.bbox[1])
                             // self._cell) + 2
        self._d2r_cache: dict = {}
        # ---- OPT-IN drivable-SURFACE layer (see `set_road_surface`) ----------------------------
        # EMPTY unless a caller opts in, and every `dist_to_road` fast path tests emptiness first,
        # so an ordinary map computes exactly what it always did.
        self._junc: tuple = ()                 # (cx, cy, r) junction discs; distance 0 inside
        self._jindex: dict = {}                # cell -> junction indices (shares self._cell)
        self._surface: dict = {}               # provenance counts, reported in the manifest
        self._phase: dict | None = None        # lazily-built deterministic node 2-colouring
        self._coord_idx: dict | None = None    # lazily-built coord -> node index (for node_phase)

    # ------------------------------------------------------------------ #
    # OPT-IN: the drivable SURFACE `dist_to_road` measures against
    # ------------------------------------------------------------------ #
    def set_road_surface(self, polylines=(), junctions=()) -> dict:
        """Give `dist_to_road` the map's REAL drivable surface instead of the routing graph.

        WHY THIS EXISTS, and it is not a convenience. `dist_to_road` answers a MAP question -- "is
        this vehicle on a road" -- and it feeds the `mapOffRoad` detector and the geometric channel's
        building blockage. The routing graph is a poor answer to that question on an imported city:

          * a graph edge is one centreline per physical road, but a real two-way street has two
            carriageways with their OWN polylines, and two distinct roads may join the same pair of
            junctions (35 such node-pairs in the InTAS import) -- the graph can hold only one of them;
          * a graph edge stops at the junction CENTRE, but a vehicle crossing a large signalised
            junction drives an internal connection tens of metres long that no graph edge covers.

        Measured on InTAS (1,188 SUMO vehicles, 4,071 sampled positions), distance from the replayed
        position to the engine's roads:

            routing graph, straight chords          p50 2.203  p95 17.434  max 95.227  12.70% > 8 m
            + curve geometry on the graph edges     p50 1.671  p95  7.219  max 57.481   3.22% > 8 m
            + this surface layer                    p50 0.363  p95  3.197  max  4.803   0.00% > 8 m

        and `dist_to_road` goes 15.9 -> 29.6 us cold, 0.66 -> 0.73 us warm. Warm is the case that
        matters: the memo is keyed on the claimed position, so one vehicle-step costs one computation
        however many receivers heard it.

        `polylines` are extra road centrelines ([[x, y], ...] each, >= 2 points) that participate in
        `dist_to_road` and in NOTHING else -- not routing, not `edge_len`, not `random_trip`. They are
        map geometry, not topology, and keeping them out of the graph is what makes this affordable:
        the InTAS surface is 23,705 segments against a `MAX_EDGES` of 12,000.

        `junctions` are `(x, y, radius)` discs standing for the paved junction AREA; a position inside
        one is on the road (distance 0). This is the alternative to importing SUMO's internal junction
        lanes, which is not arithmetically available: InTAS has 15,706 of them against `MAX_EDGES`
        12,000 and `MAX_NODES` 4,000, and they are a routing artefact anyway -- the junction polygon
        is the map's own statement of where the tarmac is.

        Idempotent-ish and explicit: calling it twice re-adds. Draws no RNG; deterministic in input
        order. Returns the provenance dict it also stores on `self._surface`."""
        base_segs = len(self._segs)
        added_pts = 0
        polylines = list(polylines)
        for poly in polylines:
            pts = [(float(p[0]), float(p[1])) for p in poly]
            if len(pts) < 2:
                raise ValueError(f"road-surface polyline needs >= 2 points (got {len(pts)})")
            for p, q in zip(pts, pts[1:]):
                if not all(math.isfinite(v) for v in (*p, *q)):
                    raise ValueError("road-surface polyline has a non-finite coordinate")
                if p != q:
                    self._segs.append((p[0], p[1], q[0], q[1]))
                    self._seg_edge.append(-1)          # -1 = surface geometry, no parent graph edge
            added_pts += len(pts)
        discs = []
        for j in junctions:
            jx, jy, jr = float(j[0]), float(j[1]), float(j[2])
            if not (math.isfinite(jx) and math.isfinite(jy) and math.isfinite(jr) and jr >= 0.0):
                raise ValueError(f"junction disc {j!r} must be (x, y, radius >= 0) and finite")
            discs.append((jx, jy, jr))
        self._junc = tuple(discs)
        # ---- rebuild the cell index over the enlarged segment set --------------------------- #
        _lens = sorted(math.hypot(s[2] - s[0], s[3] - s[1]) for s in self._segs)
        self._cell = min(600.0, max(100.0, 2.0 * _lens[len(_lens) // 2]))
        cell = self._cell
        _ix: dict[tuple[int, int], set] = {}
        for k, (ax, ay, bx, by) in enumerate(self._segs):
            steps = max(1, int(math.hypot(bx - ax, by - ay) // cell) + 1)
            for c in range(steps):
                t0, t1 = c / steps, (c + 1) / steps
                px0, py0 = ax + (bx - ax) * t0, ay + (by - ay) * t0
                px1, py1 = ax + (bx - ax) * t1, ay + (by - ay) * t1
                for ci in range(int(min(px0, px1) // cell), int(max(px0, px1) // cell) + 1):
                    for cj in range(int(min(py0, py1) // cell), int(max(py0, py1) // cell) + 1):
                        _ix.setdefault((ci, cj), set()).add(k)
        self._index = {c: tuple(sorted(v)) for c, v in _ix.items()}
        # A disc is registered in every cell its BOUNDING BOX touches, so every point of the disc --
        # its boundary included -- lies in a registered cell. That is the same invariant the segment
        # chunking above maintains, and it is what makes the ring walk's early break exact.
        _jx: dict[tuple[int, int], list] = {}
        for k, (jx, jy, jr) in enumerate(self._junc):
            for ci in range(int((jx - jr) // cell), int((jx + jr) // cell) + 1):
                for cj in range(int((jy - jr) // cell), int((jy + jr) // cell) + 1):
                    _jx.setdefault((ci, cj), []).append(k)
        self._jindex = {c: tuple(v) for c, v in _jx.items()}
        cis = [c[0] for c in self._index] + [c[0] for c in self._jindex]
        cjs = [c[1] for c in self._index] + [c[1] for c in self._jindex]
        self._cbox = (min(cis), min(cjs), max(cis), max(cjs))
        self._max_ring = (self._cbox[2] - self._cbox[0]) + (self._cbox[3] - self._cbox[1]) + 2
        self._d2r_cache = {}                   # the answer changed; the memo must not survive it
        self._surface = {"graph_segments": base_segs,
                         "surface_polylines": len(polylines),
                         "surface_segments": len(self._segs) - base_segs,
                         "surface_vertices": added_pts,
                         "junction_discs": len(self._junc),
                         "junction_radius_max_m": round(max((d[2] for d in self._junc), default=0.0), 2),
                         "cell_m": round(self._cell, 1)}
        return dict(self._surface)

    def _junction_dist(self, x: float, y: float) -> float:
        """Distance to the nearest junction DISC (0 inside one). Same ring walk as `dist_to_road`,
        over `_jindex`; only reached when a surface has been installed."""
        best = math.inf
        cell = self._cell
        ci, cj = int(x // cell), int(y // cell)
        i0, j0, i1, j1 = self._cbox
        r_lo = max(0, i0 - ci, ci - i1, j0 - cj, cj - j1)
        r_hi = max(abs(i0 - ci), abs(i1 - ci), abs(j0 - cj), abs(j1 - cj))
        jindex, junc = self._jindex, self._junc
        for r in range(r_lo, r_hi + 1):
            if r == 0:
                cells = ((ci, cj),)
            else:
                cells = [(i, cj - r) for i in range(ci - r, ci + r + 1)]
                cells += [(i, cj + r) for i in range(ci - r, ci + r + 1)]
                cells += [(ci - r, j) for j in range(cj - r + 1, cj + r)]
                cells += [(ci + r, j) for j in range(cj - r + 1, cj + r)]
            for c in cells:
                for k in jindex.get(c, ()):
                    jx, jy, jr = junc[k]
                    d = math.hypot(x - jx, y - jy) - jr
                    if d < best:
                        best = d if d > 0.0 else 0.0
            if best <= r * cell:
                break
        return best

    def _coord(self, i: int) -> tuple[float, float]:
        return self.coords[i]

    def _colouring(self) -> dict[int, int]:
        """Deterministic graph 2-colouring by BFS: the root of each component is colour 0 and every
        step flips colour, so tree edges always join opposite colours (a proper 2-colouring on any
        bipartite map; a stable, deterministic near-2-colouring otherwise). Nodes visited in index
        order -> byte-stable. Used to phase traffic signals on arbitrary (spider/custom/OSM) maps."""
        colour: dict[int, int] = {}
        for start in self.nodes:
            if start in colour:
                continue
            colour[start] = 0
            q = deque([start])
            while q:
                u = q.popleft()
                for v, _w in self.uadj[u]:     # one-ways do not change which junctions are adjacent
                    if v not in colour:
                        colour[v] = colour[u] ^ 1
                        q.append(v)
        return colour

    def node_phase(self, node) -> int:
        """Deterministic 0/1 signal phase for the intersection at (x, y) = `node` (a Trip waypoint),
        from the graph 2-colouring. Adjacent intersections alternate, so signals have a coherent
        phase on any topology instead of the meaningless grid-coordinate arithmetic used before."""
        if self._phase is None:
            self._phase = self._colouring()
        if self._coord_idx is None:
            self._coord_idx = {}
            for i, c in enumerate(self.coords):
                self._coord_idx.setdefault(c, i)
        i = self._coord_idx.get(tuple(node))
        return 0 if i is None else self._phase.get(i, 0)

    def _open_adj(self, i: int):
        if not self._closed:
            return self.adj[i]
        return [(m, w) for m, w in self.adj[i] if (min(i, m), max(i, m)) not in self._closed]

    def set_closures(self, pairs) -> list:
        """Replace the timed-closure set (routing only -- the road still physically exists).
        A closure that would disconnect the network is skipped. Returns applied [a,b] pairs."""
        self._closed = set()
        self._hops_cache = {}
        applied = []
        edge_set = set(self.edges)
        for e in pairs:
            try:
                a, b = int(e[0]), int(e[1])
            except (TypeError, ValueError, IndexError):
                continue                        # wrong shape (e.g. grid-style [[i,j],[i,j]] pair)
            key = (min(a, b), max(a, b))
            if key not in edge_set or key in self._closed:
                continue
            self._closed.add(key)
            if self._still_connected():
                applied.append([a, b])
            else:
                self._closed.discard(key)
        return applied

    def _still_connected(self) -> bool:
        """Every node reachable from node 0 under the current closures -- and, on a one-way map, able
        to reach node 0 again (a closure that leaves a node enterable but not leavable strands
        whoever routes there)."""
        reach = {0}
        q = deque([0])
        while q:
            for m, _ in self._open_adj(q.popleft()):
                if m not in reach:
                    reach.add(m)
                    q.append(m)
        if len(reach) != len(self.nodes):
            return False
        if not self.directed:
            return True
        rev: dict[int, list[int]] = {i: [] for i in self.nodes}
        for u in self.nodes:
            for v, _w in self._open_adj(u):
                rev[v].append(u)
        back = {0}
        q = deque([0])
        while q:
            for m in rev[q.popleft()]:
                if m not in back:
                    back.add(m)
                    q.append(m)
        return len(back) == len(self.nodes)

    def _hops_from(self, o: int) -> dict[int, int]:
        h = self._hops_cache.get(o)
        if h is None:
            h = {o: 0}
            q = deque([o])
            while q:
                cur = q.popleft()
                for m, _ in self._open_adj(cur):
                    if m not in h:
                        h[m] = h[cur] + 1
                        q.append(m)
            self._hops_cache[o] = h
        return h

    # ---- OPT-IN routing objective (default "length" == the historical behaviour) ----
    route_metric = "length"
    route_free_speed = 13.9
    _adjw: dict | None = None              # metric-weighted adjacency; None -> use lengths

    def set_route_metric(self, metric: str = "length", free_speed_mps: float = 13.9):
        """Choose the routing objective. "length" (default) is the historical road-length Dijkstra
        and leaves every existing map's routes untouched. "time" divides each edge length by its
        posted limit (or `free_speed_mps` where none is posted), so a motorway beats a shorter crawl
        through a 30-zone -- what a navigation system actually does. "hops" reproduces the old grid
        router's junction-count objective on an arbitrary graph. Rebuilding the weighted adjacency is
        O(E) and happens here, once, not per route."""
        self.route_metric, self.route_free_speed = _check_route_metric(metric, free_speed_mps)
        if metric == "length":
            self._adjw = None
            return self
        w: dict[int, list[tuple[int, float]]] = {i: [] for i in self.nodes}
        for u in self.nodes:
            for v, ln in self.adj[u]:
                if metric == "hops":
                    w[u].append((v, 1.0))
                else:
                    sp = self.edge_speed.get((min(u, v), max(u, v))) or self.route_free_speed
                    w[u].append((v, ln / sp))
        self._adjw = w
        return self

    def _open_adjw(self, i: int):
        """Metric-weighted, closure-filtered adjacency of node i."""
        if self._adjw is None:
            return self._open_adj(i)
        adj = self._adjw[i]
        if not self._closed:
            return adj
        return [(m, w) for m, w in adj if (min(i, m), max(i, m)) not in self._closed]

    def _route(self, o: int, d: int) -> list[int]:
        """Shortest path under `route_metric` (Dijkstra; ties broken by node index -> deterministic).
        One-way edges are absent from `adj` in that direction, so they are honoured for free."""
        dist = {o: 0.0}
        prev: dict[int, int | None] = {o: None}
        pq = [(0.0, o)]
        while pq:
            dd, cur = heapq.heappop(pq)
            if cur == d:
                break
            if dd > dist.get(cur, math.inf) + 1e-9:
                continue
            for m, w in self._open_adjw(cur):
                nd = dd + w
                if nd < dist.get(m, math.inf) - 1e-9:
                    dist[m] = nd
                    prev[m] = cur
                    heapq.heappush(pq, (nd, m))
        if d not in prev:                      # unreachable under closures -> stay at origin
            return [o]
        path: list[int] = []
        cur: int | None = d
        while cur is not None:
            path.append(cur)
            cur = prev[cur]
        return list(reversed(path))

    def dist_to_road(self, x: float, y: float) -> float:
        """Exact distance to the nearest road segment (spatial rings + memo; a claim heard by many
        receivers in one step is computed once)."""
        key = (round(x, 2), round(y, 2))
        hit = self._d2r_cache.get(key)
        if hit is not None:
            return hit
        best = math.inf
        # OPT-IN surface layer. `_junc` is EMPTY on every map that has not called
        # `set_road_surface`, so this is one tuple truth-test per call and the loops below are
        # untouched -- the default path computes byte-identically what it always did. When a surface
        # IS installed, seeding `best` with the junction distance only makes the ring walk break
        # EARLIER, and it breaks on a real distance to real road geometry, so the result stays exact.
        if self._junc:
            best = self._junction_dist(x, y)
            if best <= 0.0:                    # inside the paved junction area: on the road
                if len(self._d2r_cache) > 200_000:
                    self._d2r_cache.clear()
                self._d2r_cache[key] = 0.0
                return 0.0
        cell = self._cell
        ci, cj = int(x // cell), int(y // cell)
        i0, j0, i1, j1 = self._cbox
        # Only rings that can intersect the occupied cell box are worth walking, and only the ring
        # PERIMETER is new at each radius. The old loop rescanned the whole r x r square per ring, so
        # a claim outside the modelled area walked O(R^3) cells (measured: 5.8 ms at 4k nodes).
        r_lo = max(0, i0 - ci, ci - i1, j0 - cj, cj - j1)
        r_hi = max(abs(i0 - ci), abs(i1 - ci), abs(j0 - cj), abs(j1 - cj))
        segs, index = self._segs, self._index
        for r in range(r_lo, r_hi + 1):
            if r == 0:
                cells = ((ci, cj),)
            else:
                cells = [(i, cj - r) for i in range(ci - r, ci + r + 1)]
                cells += [(i, cj + r) for i in range(ci - r, ci + r + 1)]
                cells += [(ci - r, j) for j in range(cj - r + 1, cj + r)]
                cells += [(ci + r, j) for j in range(cj - r + 1, cj + r)]
            for c in cells:
                for k in index.get(c, ()):
                    ax, ay, bx, by = segs[k]
                    d = _pt_seg_dist(x, y, ax, ay, bx, by)
                    if d < best:
                        best = d
            if best <= r * cell:               # nothing in farther rings can be closer
                break
        if best is math.inf:                   # empty index (unreachable): exact full scan
            for ax, ay, bx, by in segs:
                d = _pt_seg_dist(x, y, ax, ay, bx, by)
                if d < best:
                    best = d
        if len(self._d2r_cache) > 200_000:     # deterministic, bounded memo
            self._d2r_cache.clear()
        self._d2r_cache[key] = best
        return best

    def _gravity_dest(self, rng, o: int, hops: dict, min_hops: int, scale: float) -> int:
        cands, weights = [], []
        s = max(0.5, scale)
        for m in self.nodes:
            hd = hops.get(m)
            if hd is not None and hd >= min_hops:
                cands.append(m)
                weights.append(math.exp(-(hd - min_hops) / s))
        if not cands:
            return o
        r = rng.random() * sum(weights)
        acc = 0.0
        for m, w in zip(cands, weights):
            acc += w
            if r <= acc:
                return m
        return cands[-1]

    def random_trip(self, rng, speed: float, spawn_time: float, min_hops: int = 3,
                    dest_hint=None, od_model: str = "uniform", gravity_scale: float = 2.0,
                    boundary_origin: bool = False) -> Trip:
        o = rng.choice(self.boundary if (boundary_origin and self.boundary) else self.nodes)
        hops = self._hops_from(o)
        mh = min(min_hops, max(hops.values()) if hops else 0)   # small nets may lack 3-hop pairs
        if dest_hint is not None and dest_hint != o and dest_hint in self.adj \
                and dest_hint in hops:
            d = dest_hint
        elif od_model == "gravity":
            d = self._gravity_dest(rng, o, hops, mh, gravity_scale)
        else:
            d = o
            for _ in range(8):
                cand = rng.choice(self.nodes)
                if hops.get(cand, -1) >= mh and cand != o:
                    d = cand
                    break
        route = self._route(o, d)
        if not self._rich:                     # legacy map: no lanes, no one-ways, no shapes
            wp = [self.coords[i] for i in route]
            caps = None
            if self.edge_speed:
                caps = [self.edge_speed.get((min(n1, n2), max(n1, n2)))
                        for n1, n2 in zip(route, route[1:])]
            return Trip(wp, speed, spawn_time, caps=caps)
        wp, nodes, caps, lanes, oneway = self.route_geometry(route)
        if self.directed_lanes and len(wp) > 1:
            wp, _ = self._lane_frame(wp, lanes, oneway)
        return Trip(wp, speed, spawn_time, caps=caps if any(c is not None for c in caps) else None,
                    nodes=nodes, lanes=lanes)

    # ---- per-edge geometry / lane accessors (public: the importer and tests read these) ----
    @property
    def _rich(self) -> bool:
        """True once anything beyond the legacy [a,b]/[a,b,speed] model is in play."""
        return bool(self.edge_shape or self.edge_lanes or self.directed or self.directed_lanes)

    def edge_points(self, u: int, v: int) -> list:
        """The full geometry of edge u-v IN TRAVEL ORDER: [u, *shape, v] (just [u, v] with no shape)."""
        key = (min(u, v), max(u, v))
        sh = self.edge_shape.get(key)
        if sh is None:
            return [self.coords[u], self.coords[v]]
        pts = [self.coords[key[0]], *sh, self.coords[key[1]]]
        return pts if u == key[0] else pts[::-1]

    def lanes_for(self, u: int, v: int) -> int:
        """Lane count for travelling u -> v (the per-direction fallback when the edge declares none)."""
        ln = self.edge_lanes.get((min(u, v), max(u, v)))
        if ln is None:
            return self._lanes_per_dir
        return max(1, ln[0] if u < v else ln[1])

    def is_oneway(self, u: int, v: int) -> bool:
        """True if the physical road carrying u-v is one-way (in either direction)."""
        return (min(u, v), max(u, v)) in self.edge_oneway

    def allows(self, u: int, v: int) -> bool:
        """True if travelling u -> v is legal (a two-way edge, or a one-way pointing that way)."""
        key = (min(u, v), max(u, v))
        if key not in self.edge_len:
            return False
        ow = self.edge_oneway.get(key, 0)
        return ow == 0 or (ow > 0) == (u < v)

    def route_geometry(self, route: list) -> tuple:
        """Expand a NODE route into driven geometry: (waypoints, node_marks, caps, lanes, oneway).

        `node_marks` is waypoint-aligned and holds the junction coordinate at real intersections and
        None at shape (curve) vertices -- what `Trip.nodes` wants. `caps`/`lanes`/`oneway` are
        segment-aligned, so a shaped edge contributes one entry per sub-segment."""
        wp: list = []
        marks: list = []
        caps: list = []
        lanes: list = []
        oneway: list = []
        for k, (u, v) in enumerate(zip(route, route[1:])):
            pts = self.edge_points(u, v)
            sp = self.edge_speed.get((min(u, v), max(u, v)))
            ln, ow = self.lanes_for(u, v), self.is_oneway(u, v)
            if k == 0:
                wp.append(pts[0])
                marks.append(pts[0])
            last = len(pts) - 1
            for i in range(1, len(pts)):
                wp.append(pts[i])
                marks.append(pts[i] if i == last else None)
                caps.append(sp)
                lanes.append(ln)
                oneway.append(ow)
        if not wp:                              # degenerate one-node route (unreachable destination)
            wp, marks = [self.coords[route[0]]], [None]
        return wp, marks, caps, lanes, oneway

    def _edge_doc(self, key: tuple[int, int]) -> dict | None:
        """The non-default per-edge structure of `key`, or None when it has none."""
        ow, ln, sh = self.edge_oneway.get(key), self.edge_lanes.get(key), self.edge_shape.get(key)
        if ow is None and ln is None and sh is None:
            return None
        e: dict = {}
        if ow is not None:
            e["oneway"] = True if ow > 0 else -1
        if ln is not None:
            if ow is None:
                e["lanes_forward"], e["lanes_backward"] = ln[0], ln[1]
            else:
                e["lanes"] = ln[0] or ln[1]
        if sh is not None:
            e["shape"] = [[x, y] for x, y in sh]
        return e

    def geometry(self) -> dict:
        """Static road geometry for UIs. `edges` KEEPS the historical [a, b] / [a, b, speed] list
        form for every edge -- the GUI map and tools/verify_data.py's N1 provenance check both index
        e[0]/e[1] positionally, and an object there would break them. Per-edge structure that has no
        place in that form (lanes, one-way, shape) is reported alongside in the OPTIONAL `edge_attrs`
        key, aligned index-for-index with `edges` (None where an edge has none). A plain map emits no
        `edge_attrs` key at all, so its geometry document is byte-identical to before.
        Use `document()` when you want something that loads back in."""
        out: dict = {"nodes": [[x, y] for x, y in self.coords],
                     "edges": [([a, b, self.edge_speed[(a, b)]] if (a, b) in self.edge_speed
                                else [a, b]) for a, b in self.edges]}
        attrs = [self._edge_doc(key) for key in self.edges]
        if any(a is not None for a in attrs):
            out["edge_attrs"] = attrs
        return out

    def document(self) -> dict:
        """The map as a custom_network document that `CustomNetwork(**doc)` reloads exactly: edges
        keep the [a, b] / [a, b, speed] list form unless they carry lanes, a one-way flag or a shape,
        in which case they use the object form `parse_edge_spec` accepts."""
        edges: list = []
        for key in self.edges:
            a, b = key
            sp = self.edge_speed.get(key)
            extra = self._edge_doc(key)
            if extra is None:
                edges.append([a, b, sp] if sp is not None else [a, b])
                continue
            e: dict = {"a": a, "b": b}
            if sp is not None:
                e["speed"] = sp
            e.update(extra)
            edges.append(e)
        return {"nodes": [[x, y] for x, y in self.coords], "edges": edges}

    def stats(self) -> dict:
        """Design feedback for the AI/user: size, extent, road length, connectivity facts."""
        out = {"n_nodes": len(self.nodes), "n_edges": len(self.edges),
               "total_road_m": round(self.total_road_m, 1),
               "mean_segment_m": round(self.block, 1),
               "bbox_m": [round(v, 1) for v in self.bbox],
               "n_dead_ends": sum(1 for i in self.nodes if len(self.uadj[i]) == 1),
               "boundary_nodes": len(self.boundary), "center_node": self.center}
        if self.edge_speed:
            out["speed_limited_edges"] = len(self.edge_speed)
            out["speed_range_mps"] = [min(self.edge_speed.values()), max(self.edge_speed.values())]
        if self.edge_oneway:
            out["oneway_edges"] = len(self.edge_oneway)
            out["oneway_share"] = round(len(self.edge_oneway) / len(self.edges), 4)
        if self.edge_lanes:
            per_dir = [v for ln in self.edge_lanes.values() for v in ln if v]
            out["lane_specified_edges"] = len(self.edge_lanes)
            out["lanes_per_direction_range"] = [min(per_dir), max(per_dir)]
        if self.edge_shape:
            out["shaped_edges"] = len(self.edge_shape)
            out["shape_vertices"] = sum(len(s) for s in self.edge_shape.values())
        if self.directed_lanes:
            out["directed_lanes"] = True
            out["lane_width_m"] = self._lane_w
            out["drive_side"] = "left" if self._side > 0 else "right"
        if self.route_metric != "length":
            out["route_metric"] = self.route_metric
        if self._surface:
            out["road_surface"] = dict(self._surface)
        return out


def spider_graph(arms: int, rings: int, block: float) -> tuple[list, list]:
    """The classic radial city: `arms` spokes from a central plaza crossed by `rings` concentric
    ring roads spaced `block` m apart. Returns (nodes, edges) for CustomNetwork."""
    arms, rings = max(3, int(arms)), max(1, int(rings))
    span = rings * float(block)                # shift so all coordinates are >= 0
    nodes: list[list[float]] = [[span, span]]  # 0 = centre
    edges: list[list[int]] = []
    idx: dict[tuple[int, int], int] = {}
    for r in range(1, rings + 1):
        for a in range(arms):
            th = 2.0 * math.pi * a / arms
            nodes.append([span + r * block * math.cos(th), span + r * block * math.sin(th)])
            idx[(r, a)] = len(nodes) - 1
    for a in range(arms):
        edges.append([0, idx[(1, a)]])                          # centre -> innermost ring
        for r in range(1, rings):
            edges.append([idx[(r, a)], idx[(r + 1, a)]])        # radial spokes
    for r in range(1, rings + 1):
        for a in range(arms):
            edges.append([idx[(r, a)], idx[(r, (a + 1) % arms)]])   # ring roads
    return nodes, edges
