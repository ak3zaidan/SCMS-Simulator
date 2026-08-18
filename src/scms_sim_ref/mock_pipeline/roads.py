"""Grid road network + route-following trips for realistic, long-running mobility.

A vehicle in flow mode gets a `Trip`: a shortest-path route between two intersections of a grid
road network, driven at a desired speed. This gives finite journeys (the vehicle despawns at its
destination), real intersections and turns, and heading that follows the road -- everything the
straight-line model lacked for long simulations with continuous vehicle turnover.
"""
from __future__ import annotations

import math
from collections import deque


class Trip:
    """A routed journey along a polyline of waypoints at a constant desired speed."""
    __slots__ = ("wp", "cum", "speed", "t0", "length", "t1")

    def __init__(self, waypoints: list[tuple[float, float]], speed: float, spawn_time: float):
        if len(waypoints) < 2:
            waypoints = [waypoints[0], (waypoints[0][0] + 1.0, waypoints[0][1])]
        self.wp = waypoints
        self.speed = max(1.0, speed)
        self.t0 = spawn_time
        cum = [0.0]
        for (ax, ay), (bx, by) in zip(waypoints, waypoints[1:]):
            cum.append(cum[-1] + math.hypot(bx - ax, by - ay))
        self.cum = cum
        self.length = cum[-1]
        self.t1 = spawn_time + self.length / self.speed   # arrival (despawn) time

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

    def __init__(self, w: int, h: int, block: float):
        self.w, self.h, self.block = int(w), int(h), float(block)
        self.nodes = [(i, j) for i in range(self.w) for j in range(self.h)]

    def _coord(self, n: tuple[int, int]) -> tuple[float, float]:
        return (n[0] * self.block, n[1] * self.block)

    def _neighbors(self, n: tuple[int, int]) -> list[tuple[int, int]]:
        i, j = n
        out = []
        if i > 0: out.append((i - 1, j))
        if i < self.w - 1: out.append((i + 1, j))
        if j > 0: out.append((i, j - 1))
        if j < self.h - 1: out.append((i, j + 1))
        return out

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
                    dest_hint=None, od_model: str = "uniform", gravity_scale: float = 2.0) -> Trip:
        o = rng.choice(self.nodes)
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
        wp = [self._coord(n) for n in self._bfs(o, d)]
        return Trip(wp, speed, spawn_time)
