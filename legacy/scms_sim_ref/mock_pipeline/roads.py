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
"""
from __future__ import annotations

import heapq
import math
from collections import deque


class Trip:
    """A routed journey along a polyline of waypoints at a constant desired speed.

    `caps` (optional) holds a per-SEGMENT speed limit (m/s) aligned with the waypoint pairs --
    None entries mean unlimited. Networks with per-edge speed limits (highway vs residential in a
    custom map) pass it; the car-following loop then caps the vehicle's target speed per segment."""
    __slots__ = ("wp", "cum", "speed", "t0", "length", "t1", "caps")

    def __init__(self, waypoints: list[tuple[float, float]], speed: float, spawn_time: float,
                 caps: list | None = None):
        if len(waypoints) < 2:
            waypoints = [waypoints[0], (waypoints[0][0] + 1.0, waypoints[0][1])]
            caps = None
        self.wp = waypoints
        self.speed = max(1.0, speed)
        self.t0 = spawn_time
        self.caps = caps if (caps and any(c is not None for c in caps)) else None
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
        """The next route vertex (intersection) ahead of arc-length s: ((x, y), distance) or (None, inf)."""
        for k in range(1, len(self.cum)):
            if self.cum[k] > s + 1e-6:
                return self.wp[k], self.cum[k] - s
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


class GridNetwork:
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
        path = self._bfs(o, d)
        wp = [self._coord(n) for n in path]
        caps = None
        if self.arterial_every > 0 and (self.arterial_speed > 0 or self.local_speed > 0):
            caps = [self._edge_cap(a, b) for a, b in zip(path, path[1:])]
        return Trip(wp, speed, spawn_time, caps=caps)


def _pt_seg_dist(px, py, ax, ay, bx, by):
    """Distance from point (px,py) to segment (a,b)."""
    dx, dy = bx - ax, by - ay
    dd = dx * dx + dy * dy
    t = 0.0 if dd == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / dd))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


class RingNetwork:
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

    def _arc(self, o: int, d: int) -> list[int]:
        cw, ccw = (d - o) % self.n, (o - d) % self.n
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
        return Trip(wp, speed, spawn_time, caps=caps)

    def geometry(self) -> dict:
        return {"nodes": [[*self._coord(i)] for i in self.nodes],
                "edges": [[i, (i + 1) % self.n] for i in range(self.n)]}


class CustomNetwork:
    """An arbitrary road graph: nodes at metre coordinates + undirected edges. This is the
    'AI designs the map' primitive -- any topology (radial city, highway with on-ramps, river town
    with two bridges, ...) expressed as {nodes: [[x,y]...], edges: [[a,b]...]}.

    Same interface as GridNetwork; routing = Dijkstra on edge length (deterministic tie-break).
    dist_to_road uses an exact cell index over edge segments plus a memo cache (the same claimed
    position is checked by every receiver in range, so caching is nearly free coverage)."""

    MAX_NODES = 400
    MAX_EDGES = 1600

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
        for e in edges:
            try:
                a, b = int(e[0]), int(e[1])
            except (TypeError, ValueError, IndexError):
                raise ValueError(f"edge {e!r} must be [a, b] node indices "
                                 f"(optionally [a, b, speed_mps])") from None
            if not (0 <= a < n and 0 <= b < n):
                raise ValueError(f"edge [{a},{b}] references a missing node (have {n} nodes)")
            if a == b:
                raise ValueError(f"edge [{a},{b}] is a self-loop")
            key = (min(a, b), max(a, b))
            seen.add(key)
            if isinstance(e, (list, tuple)) and len(e) > 2 and e[2] is not None:
                sp = float(e[2])
                if not (1.0 <= sp <= 70.0):
                    raise ValueError(f"edge [{a},{b}] speed limit {sp} out of range 1-70 m/s "
                                     f"(33 ~ 120 km/h highway, 8.3 ~ 30 km/h zone)")
                self.edge_speed[key] = sp
        self.edges: list[tuple[int, int]] = sorted(seen)
        self.nodes = list(range(n))
        self.adj: dict[int, list[tuple[int, float]]] = {i: [] for i in self.nodes}
        for a, b in self.edges:
            d = math.dist(self.coords[a], self.coords[b])
            if d < 1.0:
                raise ValueError(f"edge [{a},{b}] is shorter than 1 m -- merge those nodes")
            self.adj[a].append((b, d))
            self.adj[b].append((a, d))
        # must be CONNECTED (unreachable islands would strand trips)
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
        dead_ends = [i for i in self.nodes if len(self.adj[i]) == 1]
        self.boundary = sorted(set(dead_ends) | set(rim)) or list(self.nodes)
        self.w = self.h = n                    # legacy attr compat (RSU spread/perimeter used instead)
        self.block = (sum(math.dist(self.coords[a], self.coords[b]) for a, b in self.edges)
                      / len(self.edges))       # mean road-segment length (informational)
        self.total_road_m = sum(math.dist(self.coords[a], self.coords[b]) for a, b in self.edges)
        self._closed: set = set()              # timed closures: {(a,b) sorted} routing-only
        self._hops_cache: dict[int, dict] = {}
        # exact spatial index over edge segments for dist_to_road
        self._cell = max(100.0, max(math.dist(self.coords[a], self.coords[b]) for a, b in self.edges))
        self._index: dict[tuple[int, int], list[int]] = {}
        for k, (a, b) in enumerate(self.edges):
            (ax, ay), (bx, by) = self.coords[a], self.coords[b]
            for ci in range(int(min(ax, bx) // self._cell), int(max(ax, bx) // self._cell) + 1):
                for cj in range(int(min(ay, by) // self._cell), int(max(ay, by) // self._cell) + 1):
                    self._index.setdefault((ci, cj), []).append(k)
        self._max_ring = int(max(self.bbox[2] - self.bbox[0], self.bbox[3] - self.bbox[1])
                             // self._cell) + 2
        self._d2r_cache: dict = {}
        self._phase: dict | None = None        # lazily-built deterministic node 2-colouring
        self._coord_idx: dict | None = None    # lazily-built coord -> node index (for node_phase)

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
                for v, _w in self.adj[u]:
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
            reach = {0}
            q = deque([0])
            while q:
                for m, _ in self._open_adj(q.popleft()):
                    if m not in reach:
                        reach.add(m)
                        q.append(m)
            if len(reach) == len(self.nodes):
                applied.append([a, b])
            else:
                self._closed.discard(key)
        return applied

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

    def _route(self, o: int, d: int) -> list[int]:
        """Shortest path by road length (Dijkstra; ties broken by node index -> deterministic)."""
        dist = {o: 0.0}
        prev: dict[int, int | None] = {o: None}
        pq = [(0.0, o)]
        while pq:
            dd, cur = heapq.heappop(pq)
            if cur == d:
                break
            if dd > dist.get(cur, math.inf) + 1e-9:
                continue
            for m, w in self._open_adj(cur):
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
        ci, cj = int(x // self._cell), int(y // self._cell)
        for r in range(self._max_ring + 1):
            for i in range(ci - r, ci + r + 1):
                for j in range(cj - r, cj + r + 1):
                    if max(abs(i - ci), abs(j - cj)) != r:
                        continue
                    for k in self._index.get((i, j), ()):
                        a, b = self.edges[k]
                        (ax, ay), (bx, by) = self.coords[a], self.coords[b]
                        d = _pt_seg_dist(x, y, ax, ay, bx, by)
                        if d < best:
                            best = d
            if best <= r * self._cell:         # nothing in farther rings can be closer
                break
        if best is math.inf:                   # far outside the indexed extent: exact full scan
            for a, b in self.edges:
                (ax, ay), (bx, by) = self.coords[a], self.coords[b]
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
        wp = [self.coords[i] for i in route]
        caps = None
        if self.edge_speed:
            caps = [self.edge_speed.get((min(n1, n2), max(n1, n2)))
                    for n1, n2 in zip(route, route[1:])]
        return Trip(wp, speed, spawn_time, caps=caps)

    def geometry(self) -> dict:
        return {"nodes": [[x, y] for x, y in self.coords],
                "edges": [([a, b, self.edge_speed[(a, b)]] if (a, b) in self.edge_speed
                           else [a, b]) for a, b in self.edges]}

    def stats(self) -> dict:
        """Design feedback for the AI/user: size, extent, road length, connectivity facts."""
        out = {"n_nodes": len(self.nodes), "n_edges": len(self.edges),
               "total_road_m": round(self.total_road_m, 1),
               "mean_segment_m": round(self.block, 1),
               "bbox_m": [round(v, 1) for v in self.bbox],
               "n_dead_ends": sum(1 for i in self.nodes if len(self.adj[i]) == 1),
               "boundary_nodes": len(self.boundary), "center_node": self.center}
        if self.edge_speed:
            out["speed_limited_edges"] = len(self.edge_speed)
            out["speed_range_mps"] = [min(self.edge_speed.values()), max(self.edge_speed.values())]
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
