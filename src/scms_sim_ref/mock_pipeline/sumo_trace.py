"""SUMO-backed mobility for the PYTHON engine: freeze a trajectory, then replay it.

WHY THIS EXISTS. There are two engines in this repo and only one of them was ever validated
against reality. The MOSAIC/Java path drives real SUMO traffic and is what every GEH number in
`docs/realism/GEH-RESULT.md` measured. The Python path -- the one that actually WRITES the
datasets a researcher consumes -- moves its vehicles with a hand-rolled IDM integrator over a
synthetic or OSM-derived graph (`roads.Trip` + `run.car_follow`), and that mobility has never been
scored against a calibrated traffic model at all. This module closes the gap the only way that
keeps the rest of the stack intact: SUMO produces the movement, the Python engine REPLAYS it and
keeps its entire SCMS / attack / detector / misbehaviour-authority stack on top.

FREEZE-AND-REPLAY, NOT LIVE TraCI -- and the reason is determinism, not speed. `libsumo` here is
bit-deterministic and fast enough to drive in-loop (measured: identical trajectory hash over
141,500 vehicle-steps on a fixed seed, 1,335 sim-steps/s at 338 concurrent vehicles, 0 teleports --
`docs/realism/PHASE3-PROBE.md`). The problem is on the OTHER side of the seam: `run.run_pipeline`
draws packet loss, `report_prob`, collusion and `net_delay` from ONE global `random.Random`, and
the COUNT and ORDER of those draws is load-bearing for the pinned goldens. Interleaving a live
external engine perturbs that sequence. Freezing the trajectories to a canonical artifact keeps
SUMO's nondeterminism OUTSIDE the digest boundary: its sha256 is recorded in
`manifest["config"]["sumo_trace_sha256"]`, so a different SUMO seed -- or a different SUMO build --
becomes a DETECTABLE INPUT CHANGE (a refused run with a diagnostic) instead of a silent digest
break that looks like an engine regression.

THE ARTIFACT is line-oriented text, canonical by construction:

    #scms-sumo-trace/1
    #meta {...}                      one line of JSON, sort_keys=True, no insignificant space
    #vehicles <n>
    V <idx> <sumo_id> <first_step> <last_step> <depart_s> <arrive_s> <arrived> <route_len_m>
    ...                              sorted by idx; idx is the STABLE id mapping, in the file
    #rows <n>
    <step> <idx> <x> <y> <speed> <angle>
    ...                              sorted by (step, idx); every value fixed to 3 decimals

`x`/`y`/`angle` are RAW SUMO NETWORK COORDINATES and SUMO's own angle convention (degrees
CLOCKWISE from North). They are deliberately NOT converted at freeze time: the engine network is
imported from the same `.net.xml` through `netimport.net_to_network`, and BOTH consumers go through
one `netimport._transformer` call, so the replayed vehicles cannot land in a different frame from
the roads they are supposed to be driving on. That is the projection trap `osm.py` documents --
anything projected into the local equirectangular frame must reuse the exact
`(lat0, lon0, kx, ky)` tuple or it misaligns silently and the geometric channel's building blockage
is computed against a city translated by hundreds of metres.

`meta["invocation"]` records the exact SUMO command with FILE PATHS REDUCED TO BASENAMES, and
`meta["net_sha256"]` / `meta["routes_sha256"]` pin the actual input bytes. That is deliberate: the
artifact's own sha256 is the input hash, and it must not change merely because the scenario was
built under a different directory. The inputs are pinned by content, which is strictly stronger
than pinning them by path.

TOOLCHAIN TRAPS (both measured here, both cost a debugging cycle):

1. SUMO's XML tooling embeds its own command line in an XML comment, and `--` is illegal inside an
   XML comment -- so no SUMO scenario file may be written into a path containing a double dash.
   `freeze()` refuses such a path rather than emitting a silently empty routes file.

2. **`libsumo` and `pyarrow` fight over native DLLs, and whoever imports first wins.** Measured on
   this toolchain (Windows, SUMO 1.25.0, pandas 3.0.5): `import pandas` then `import libsumo`
   raises `ImportError: DLL load failed while importing _libsumo`; `import libsumo` then
   `import pandas` works, but pandas' parquet engine then fails with "Unable to find a usable
   engine". This is why `freeze()` imports `libsumo` LAZILY, inside the function, after argument
   validation: only the FREEZE step needs it. Everything else in this module -- `load()`, the replay
   provider, `engine_network()` -- runs on `sumolib`, which is pure Python and conflict-free. If you
   need to freeze a trajectory from a process that also uses pandas, run the freeze in its own
   interpreter (`python -m scms_sim_ref.mock_pipeline.sumo_trace ...`); the test suite does exactly
   that.
"""
from __future__ import annotations

import hashlib
import json
import math
import os

#: The SUMO build these traces are pinned to. `freeze()` refuses a different one unless
#: `strict_version=False`, because a SUMO minor release can change car-following defaults and would
#: therefore change the trajectories without changing the seed.
SUMO_VERSION_PINNED = "1.25.0"

#: Artifact format tag -- the first line of every trace file.
TRACE_FORMAT = "scms-sumo-trace/1"

#: Value quantisation. 1 mm on positions, 1 mm/s on speeds, 1 milli-degree on angles: far below
#: anything the engine's sensor model can resolve, and it makes the artifact byte-canonical.
_ROUND = 3

#: Trip-polyline decimation for the engine-side `roads.Trip` (metres). The replayed kinematics come
#: from the per-step arrays, never from the polyline, so this only affects the reported route length
#: and the geometry a downstream consumer could read off `Vehicle.trip`.
TRIP_DECIMATE_M = 2.0


# --------------------------------------------------------------------------- #
# small helpers
# --------------------------------------------------------------------------- #
def _q(v: float) -> str:
    """Canonical fixed-point rendering. `-0.000` is normalised to `0.000` so a value that differs
    only in the sign of zero can never change the artifact's bytes (and therefore its hash)."""
    f = round(float(v), _ROUND)
    if f == 0.0:
        f = 0.0
    return f"{f:.{_ROUND}f}"


def file_sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def derive_sumo_seed(run_seed: int, tag: str = "sumo") -> int:
    """SUMO's seed, DERIVED from the run seed rather than a second free parameter.

    A run is reproducible from one number. Deriving through sha256 (rather than reusing `run_seed`
    verbatim) keeps SUMO's stream independent of the engine's `random.Random(seed)` stream, so two
    runs that differ only in `seed` differ in BOTH the traffic and the network stochastics rather
    than in a correlated way."""
    d = hashlib.sha256(f"{int(run_seed)}|{tag}".encode()).digest()
    return int.from_bytes(d[:4], "big") % (2 ** 31 - 1)


def sumo_home() -> str:
    home = os.environ.get("SUMO_HOME", "")
    if not home or not os.path.isdir(home):
        raise RuntimeError("SUMO_HOME is not set to a SUMO installation; freezing a trajectory "
                           "needs the SUMO binaries (this repo's toolchain sets it in env.ps1)")
    return home


def sumo_binary(name: str = "sumo") -> str:
    home = sumo_home()
    for cand in (os.path.join(home, "bin", name + ".exe"), os.path.join(home, "bin", name)):
        if os.path.exists(cand):
            return cand
    raise RuntimeError(f"{name} not found under {os.path.join(home, 'bin')}")


def _no_double_dash(path: str, what: str) -> str:
    """SUMO embeds its command line in an XML comment; `--` is illegal there. A scenario written
    into such a path produces a SILENTLY EMPTY routes file and a run with zero vehicles."""
    if "--" in os.path.abspath(path):
        raise ValueError(
            f"{what} path contains a double dash ({path!r}). SUMO's XML tooling writes its own "
            f"command line into an XML comment and '--' is illegal inside one, so the generated "
            f"file would be silently empty. Use a path without '--'.")
    return path


# --------------------------------------------------------------------------- #
# the frozen artifact
# --------------------------------------------------------------------------- #
class TraceVehicle:
    """One vehicle's canonical record. `idx` is the STABLE id: it is assigned in
    `(first_step, sumo_id)` order at freeze time and written into the file, so nothing downstream
    ever has to re-derive it (and a re-derivation from a different SUMO id ordering cannot drift)."""
    __slots__ = ("idx", "sumo_id", "first_step", "last_step", "depart_s", "arrive_s", "arrived",
                 "route_length_m")

    def __init__(self, idx, sumo_id, first_step, last_step, depart_s, arrive_s, arrived,
                 route_length_m):
        self.idx = int(idx)
        self.sumo_id = str(sumo_id)
        self.first_step = int(first_step)
        self.last_step = int(last_step)
        self.depart_s = float(depart_s)
        self.arrive_s = float(arrive_s)
        self.arrived = bool(arrived)
        self.route_length_m = float(route_length_m)

    @property
    def n_steps(self) -> int:
        return self.last_step - self.first_step + 1

    def __repr__(self) -> str:                       # pragma: no cover - diagnostics only
        return (f"TraceVehicle(idx={self.idx}, sumo_id={self.sumo_id!r}, "
                f"steps={self.first_step}..{self.last_step}, len={self.route_length_m:.1f} m)")


class SumoTrace:
    """A loaded frozen trajectory: canonical metadata + per-vehicle step-indexed arrays.

    Raw SUMO frame throughout (network coordinates, SUMO angle). The transform into the engine's
    frame belongs to :class:`SumoReplayMobility`, which shares it with the network importer."""
    __slots__ = ("meta", "vehicles", "_x", "_y", "_v", "_a", "sha256", "path")

    def __init__(self, meta: dict, vehicles: list, arrays: dict, sha256: str = "", path: str = ""):
        self.meta = meta
        self.vehicles = tuple(vehicles)
        self._x, self._y, self._v, self._a = arrays["x"], arrays["y"], arrays["v"], arrays["a"]
        self.sha256 = sha256
        self.path = path

    # -- accessors -------------------------------------------------------- #
    @property
    def dt(self) -> float:
        return float(self.meta["dt"])

    @property
    def n_steps(self) -> int:
        return int(self.meta["steps"])

    @property
    def n_rows(self) -> int:
        return sum(len(a) for a in self._x)

    def series(self, idx: int):
        """(xs, ys, speeds, angles) for vehicle `idx`, indexed from its `first_step`."""
        return self._x[idx], self._y[idx], self._v[idx], self._a[idx]

    def state(self, idx: int, step: int):
        v = self.vehicles[idx]
        k = step - v.first_step
        if k < 0 or k >= len(self._x[idx]):
            return None
        return self._x[idx][k], self._y[idx][k], self._v[idx][k], self._a[idx][k]

    def summary(self) -> dict:
        pres = [v.n_steps for v in self.vehicles]
        lens = sorted(v.route_length_m for v in self.vehicles)
        return {"n_vehicles": len(self.vehicles), "n_rows": self.n_rows,
                "steps": self.n_steps, "dt": self.dt,
                "mean_presence_steps": round(sum(pres) / max(1, len(pres)), 3),
                "median_route_length_m": round(lens[len(lens) // 2], 3) if lens else 0.0,
                "arrived": sum(1 for v in self.vehicles if v.arrived),
                "teleports": self.meta.get("teleports", 0),
                "collisions": self.meta.get("collisions", 0),
                "sumo_version": self.meta.get("sumo_version", ""),
                "sumo_seed": self.meta.get("sumo_seed"),
                "sha256": self.sha256}


# --------------------------------------------------------------------------- #
# Phase A -- FREEZE
# --------------------------------------------------------------------------- #
def freeze(*, net: str, out: str, routes: str = "", sumocfg: str = "", seed: int | None = None,
           steps: int, dt: float = 1.0, begin: float = 0.0, sumo_bin: str = "",
           extra_args=(), strict_version: bool = True, run_seed: int | None = None) -> SumoTrace:
    """Run SUMO ONCE and write the canonical trajectory artifact to `out`.

    `seed` is SUMO's seed. Pass `run_seed` instead to derive it from the engine's run seed
    (`derive_sumo_seed`); the derivation and the resulting value are both recorded in the file.

    The invocation is single-threaded (`--threads 1`), teleport-free (`--time-to-teleport -1`, so a
    jammed vehicle waits instead of being ported across the map -- a teleport is a discontinuity the
    replay would faithfully reproduce as a physically impossible jump), and step-logging is off.
    Every one of those flags is recorded in `meta["invocation"]`."""
    # Argument validation FIRST, before the heavy optional dependency is loaded: a refusal that
    # depends on `libsumo` being importable is not a refusal.
    _no_double_dash(net, "net")
    if routes:
        _no_double_dash(routes, "routes")
    if sumocfg:
        _no_double_dash(sumocfg, "sumocfg")
    # NOT `out`: the trajectory artifact is written by Python, never by SUMO's XML tooling, so a
    # double dash in its path is harmless and refusing it would be a false alarm.
    if not routes and not sumocfg:
        raise ValueError("freeze() needs either routes= (a .rou.xml/.trips.xml) or sumocfg=")
    if steps <= 0:
        raise ValueError(f"steps must be > 0 (got {steps})")
    if dt <= 0:
        raise ValueError(f"dt must be > 0 (got {dt})")
    if run_seed is not None:
        seed = derive_sumo_seed(run_seed)
    if seed is None:
        raise ValueError("freeze() needs seed= (SUMO's own seed) or run_seed= (derive it)")

    import libsumo                                    # noqa: PLC0415  (heavy optional dependency)

    version = _sumo_version(libsumo)
    if strict_version and SUMO_VERSION_PINNED not in version:
        raise RuntimeError(
            f"this trace is pinned to SUMO {SUMO_VERSION_PINNED} but the toolchain reports "
            f"{version!r}. A SUMO minor release can change car-following defaults, so the same seed "
            f"would produce different trajectories. Install the pinned build, or pass "
            f"strict_version=False and accept that the artifact is not comparable.")

    binary = sumo_bin or sumo_binary("sumo")
    end = begin + steps * dt
    argv = [binary]
    if sumocfg:
        argv += ["-c", sumocfg]
    else:
        argv += ["-n", net, "-r", routes]
    argv += ["--seed", str(int(seed)),
             "--step-length", repr(float(dt)),
             "--begin", repr(float(begin)),
             "--end", repr(float(end)),
             "--threads", "1",
             "--no-step-log", "true",
             "--no-warnings", "true",
             "--time-to-teleport", "-1",
             "--ignore-route-errors", "true",
             *[str(a) for a in extra_args]]

    # --- drive SUMO ------------------------------------------------------ #
    #: raw[sumo_id] = [first_step, [x], [y], [speed], [angle], depart_s, last_dist, last_step]
    raw: dict = {}
    teleports = 0
    collisions = 0
    libsumo.start(argv)
    try:
        for step in range(steps):
            libsumo.simulationStep()
            teleports += libsumo.simulation.getStartingTeleportNumber()
            collisions += libsumo.simulation.getCollidingVehiclesNumber()
            now = libsumo.simulation.getTime()
            for vid in libsumo.vehicle.getIDList():
                rec = raw.get(vid)
                if rec is None:
                    rec = raw[vid] = [step, [], [], [], [], now, 0.0, step - 1]
                elif rec[7] != step - 1:
                    # The replay indexes a vehicle's state by `step - first_step`, and `load()`
                    # refuses a gap on read -- so a gap must be caught HERE rather than written out
                    # as a contiguous run that silently mislabels every later step.
                    raise RuntimeError(f"SUMO vehicle {vid!r} disappeared and came back "
                                       f"(steps {rec[7]} -> {step}); the trace format assumes one "
                                       f"contiguous presence per vehicle id")
                x, y = libsumo.vehicle.getPosition(vid)
                rec[1].append(x)
                rec[2].append(y)
                rec[3].append(libsumo.vehicle.getSpeed(vid))
                rec[4].append(libsumo.vehicle.getAngle(vid))
                rec[6] = libsumo.vehicle.getDistance(vid)
                rec[7] = step
        final_time = libsumo.simulation.getTime()
    finally:
        libsumo.close()

    if not raw:
        raise RuntimeError(
            "SUMO produced ZERO vehicle-steps. The usual cause is an empty routes file: SUMO's "
            "tools embed their own command line in an XML comment and a '--' anywhere in the output "
            "path makes that comment illegal XML, so randomTrips.py writes nothing. Check the "
            "routes file has <trip>/<vehicle> elements.")

    # --- canonical id mapping: (first_step, sumo_id), assigned once and WRITTEN DOWN ---------- #
    order = sorted(raw, key=lambda k: (raw[k][0], k))
    vehicles: list[TraceVehicle] = []
    xs, ys, vs, angs = [], [], [], []
    for idx, vid in enumerate(order):
        first, X, Y, V, A, depart, dist, _last_seen = raw[vid]
        if any(c.isspace() for c in vid):
            raise ValueError(f"SUMO vehicle id {vid!r} contains whitespace, which the line-oriented "
                             f"trace format cannot represent unambiguously")
        last = first + len(X) - 1
        vehicles.append(TraceVehicle(idx, vid, first, last, depart,
                                     begin + (last + 1) * dt, last < steps - 1, dist))
        xs.append(X)
        ys.append(Y)
        vs.append(V)
        angs.append(A)

    meta = {
        "format": TRACE_FORMAT,
        "sumo_version": version,
        "sumo_version_pinned": SUMO_VERSION_PINNED,
        "sumo_seed": int(seed),
        "seed_derivation": ("sha256(f'{run_seed}|sumo')[:4] mod 2**31-1"
                            if run_seed is not None else "explicit"),
        "run_seed": (int(run_seed) if run_seed is not None else None),
        "dt": float(dt),
        "begin": float(begin),
        "end": float(end),
        "steps": int(steps),
        "final_sim_time": float(final_time),
        # step k of this trace is SUMO time `begin + (k+1)*dt` -- the state AFTER the (k+1)-th
        # simulationStep(). The engine maps its own step k onto trace step k; the absolute offset
        # is recorded so a consumer can align against SUMO output files if it ever needs to.
        "step0_sim_time": float(begin + dt),
        "net_basename": os.path.basename(net),
        "net_sha256": file_sha256(net),
        "routes_basename": (os.path.basename(routes) if routes else ""),
        "routes_sha256": (file_sha256(routes) if routes else ""),
        "sumocfg_basename": (os.path.basename(sumocfg) if sumocfg else ""),
        "sumocfg_sha256": (file_sha256(sumocfg) if sumocfg else ""),
        # EXACT invocation, with file paths reduced to basenames so the artifact's hash does not
        # move merely because the scenario lives in a different directory. The inputs themselves are
        # pinned by content above, which is strictly stronger than pinning them by path.
        "invocation": [os.path.basename(a) if os.path.sep in a else a for a in argv],
        "teleports": int(teleports),
        "collisions": int(collisions),
        "coord_frame": "raw SUMO network coordinates (metres); angle = deg CLOCKWISE from North",
        "round_decimals": _ROUND,
    }
    trace = SumoTrace(meta, vehicles, {"x": xs, "y": ys, "v": vs, "a": angs})
    _write(out, trace)
    trace.sha256 = file_sha256(out)
    trace.path = os.path.abspath(out)
    return trace


def _sumo_version(libsumo) -> str:
    try:
        v = libsumo.getVersion()
    except Exception:                                 # pragma: no cover - libsumo always has it
        return "unknown"
    return str(v[1] if isinstance(v, (tuple, list)) and len(v) > 1 else v)


def _write(path: str, trace: SumoTrace) -> None:
    """Write the canonical artifact. Sorted by (step, idx); LF newlines; fixed rounding."""
    rows: list[tuple] = []
    for veh in trace.vehicles:
        X, Y, V, A = trace.series(veh.idx)
        base = veh.first_step
        for k in range(len(X)):
            rows.append((base + k, veh.idx, X[k], Y[k], V[k], A[k]))
    rows.sort(key=lambda r: (r[0], r[1]))
    d = os.path.dirname(os.path.abspath(path))
    if d:
        os.makedirs(d, exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(f"#{TRACE_FORMAT}\n")
        fh.write("#meta " + json.dumps(trace.meta, sort_keys=True, separators=(",", ":")) + "\n")
        fh.write(f"#vehicles {len(trace.vehicles)}\n")
        for veh in trace.vehicles:
            fh.write(f"V {veh.idx} {veh.sumo_id} {veh.first_step} {veh.last_step} "
                     f"{_q(veh.depart_s)} {_q(veh.arrive_s)} {int(veh.arrived)} "
                     f"{_q(veh.route_length_m)}\n")
        fh.write(f"#rows {len(rows)}\n")
        for step, idx, x, y, sp, ang in rows:
            fh.write(f"{step} {idx} {_q(x)} {_q(y)} {_q(sp)} {_q(ang)}\n")


def load(path: str) -> SumoTrace:
    """Read a frozen artifact back, verifying its self-declared row/vehicle counts."""
    with open(path, "r", encoding="utf-8") as fh:
        head = fh.readline().rstrip("\n")
        if head != "#" + TRACE_FORMAT:
            raise ValueError(f"{path}: not a {TRACE_FORMAT} artifact (first line {head!r})")
        line = fh.readline()
        if not line.startswith("#meta "):
            raise ValueError(f"{path}: missing #meta line")
        meta = json.loads(line[6:])
        line = fh.readline()
        if not line.startswith("#vehicles "):
            raise ValueError(f"{path}: missing #vehicles line")
        n_veh = int(line.split()[1])
        vehicles = []
        for _ in range(n_veh):
            p = fh.readline().split()
            if not p or p[0] != "V":
                raise ValueError(f"{path}: malformed vehicle record {p!r}")
            vehicles.append(TraceVehicle(int(p[1]), p[2], int(p[3]), int(p[4]),
                                         float(p[5]), float(p[6]), int(p[7]), float(p[8])))
        line = fh.readline()
        if not line.startswith("#rows "):
            raise ValueError(f"{path}: missing #rows line")
        n_rows = int(line.split()[1])
        xs = [[] for _ in vehicles]
        ys = [[] for _ in vehicles]
        vs = [[] for _ in vehicles]
        angs = [[] for _ in vehicles]
        seen = 0
        prev = (-1, -1)
        for raw_line in fh:
            if not raw_line.strip():
                continue
            step_s, idx_s, x_s, y_s, v_s, a_s = raw_line.split()
            step, idx = int(step_s), int(idx_s)
            if (step, idx) <= prev:
                raise ValueError(f"{path}: rows are not sorted by (step, vehicle) at {(step, idx)}")
            prev = (step, idx)
            veh = vehicles[idx]
            if step != veh.first_step + len(xs[idx]):
                raise ValueError(f"{path}: vehicle {idx} has a gap at step {step}; a replayed "
                                 f"vehicle must be present on a contiguous step range")
            xs[idx].append(float(x_s))
            ys[idx].append(float(y_s))
            vs[idx].append(float(v_s))
            angs[idx].append(float(a_s))
            seen += 1
        if seen != n_rows:
            raise ValueError(f"{path}: declares {n_rows} rows, found {seen}")
    for veh in vehicles:
        if len(xs[veh.idx]) != veh.n_steps:
            raise ValueError(f"{path}: vehicle {veh.idx} declares steps "
                             f"{veh.first_step}..{veh.last_step} but carries {len(xs[veh.idx])} rows")
    return SumoTrace(meta, vehicles, {"x": xs, "y": ys, "v": vs, "a": angs},
                     sha256=file_sha256(path), path=os.path.abspath(path))


# --------------------------------------------------------------------------- #
# Phase B -- REPLAY (the `mobility` built-in)
# --------------------------------------------------------------------------- #
class ReplaySpan:
    """What the engine needs to CREATE a vehicle for one frozen trajectory: when it appears, when
    it leaves, and the polyline it drives. `finish_time` is EXACT (SUMO already told us), which is
    what makes the certificate budget below exact rather than an estimate."""
    __slots__ = ("idx", "sumo_id", "spawn_time", "finish_time", "polyline", "mean_speed",
                 "route_length_m", "arrived")

    def __init__(self, idx, sumo_id, spawn_time, finish_time, polyline, mean_speed,
                 route_length_m, arrived):
        self.idx = idx
        self.sumo_id = sumo_id
        self.spawn_time = spawn_time
        self.finish_time = finish_time
        self.polyline = polyline
        self.mean_speed = mean_speed
        self.route_length_m = route_length_m
        self.arrived = arrived


class SumoReplayMobility:
    """Built-in `mobility` provider: feed a frozen SUMO trajectory into the engine's existing seam.

    It replaces exactly one thing -- where `Vehicle.cur_x/cur_y/cur_v/cur_h` come from, and when a
    vehicle spawns and despawns. Everything downstream (pseudonym rotation, the attack layer, the
    radio channel, the detectors, the misbehaviour authority, the report writer) sees the same
    `Vehicle` objects it always did and is untouched. The MA-visible / ORACLE firewall is unaffected:
    this provider only ever writes ground truth, and reads nothing.

    ZERO RNG. Not one draw comes from any generator here, so the mode cannot perturb the engine's
    global stream even in principle. The arrival process, the routes and the car-following are all
    SUMO's, decided before the run started and frozen to disk.

    HEADING CONVENTION. SUMO reports degrees CLOCKWISE FROM NORTH; the Python engine's heading is the
    math convention, degrees COUNTER-CLOCKWISE FROM EAST (`manifest["conventions"]["heading"]`).
    The conversion is `(90 - a) mod 360`, applied ONCE here at construction.
    """

    #: registry identity
    NAME = "sumo_replay"
    INTERFACE_NAME = "scms.mobility"
    INTERFACE_VERSION = "1.0"

    def __init__(self, trace: SumoTrace, *, dt: float, transform=None,
                 decimate_m: float = TRIP_DECIMATE_M):
        if abs(float(trace.dt) - float(dt)) > 1e-9:
            raise ValueError(
                f"the frozen trace was produced at dt={trace.dt} s but the run uses dt={dt} s. "
                f"Replay maps engine step k onto trace step k, so the two step grids must agree; "
                f"re-freeze the trajectory at the run's dt.")
        self.trace = trace
        self.dt = float(dt)
        self.decimate_m = float(decimate_m)
        tf = transform or (lambda x, y: (x, y))
        # Pre-transform ONCE, at construction: the run loop then does array indexing and nothing
        # else, and the coherence sample below sees byte-for-byte what the run will see.
        self._x, self._y, self._h, self._v, self._s = [], [], [], [], []
        for veh in trace.vehicles:
            X, Y, V, A = trace.series(veh.idx)
            tx, ty, th, ts = [], [], [], []
            run = 0.0
            px = py = None
            for k in range(len(X)):
                ax, ay = tf(X[k], Y[k])
                ax, ay = round(ax, _ROUND), round(ay, _ROUND)
                if px is not None:
                    run += math.hypot(ax - px, ay - py)
                px, py = ax, ay
                tx.append(ax)
                ty.append(ay)
                th.append(round((90.0 - A[k]) % 360.0, _ROUND))
                ts.append(run)
            self._x.append(tx)
            self._y.append(ty)
            self._h.append(th)
            self._v.append(list(V))
            self._s.append(ts)
        #: engine vid -> trace idx, filled by `bind()` as the engine creates its vehicles
        self._bind: dict = {}

    # -- interface ---------------------------------------------------------- #
    def capabilities(self) -> frozenset:
        return frozenset({"position", "speed", "heading", "spawn", "despawn", "route_length"})

    def describe(self) -> dict:
        return {"provider": self.NAME, "interface": self.INTERFACE_NAME,
                "interface_version": self.INTERFACE_VERSION, **self.trace.summary()}

    # -- spawn schedule ----------------------------------------------------- #
    def plan(self, *, total_time: float, max_vehicles: int = 0) -> list:
        """The spawn schedule, in canonical `idx` order. A vehicle whose first step is at or past
        the run's horizon is dropped (it would spawn and never be simulated)."""
        out = []
        for veh in self.trace.vehicles:
            spawn = veh.first_step * self.dt
            if spawn >= total_time:
                continue
            if max_vehicles and len(out) >= max_vehicles:
                break
            X, Y = self._x[veh.idx], self._y[veh.idx]
            poly = self._decimate(X, Y)
            span = veh.last_step * self.dt
            dur = max(self.dt, span - spawn)
            out.append(ReplaySpan(veh.idx, veh.sumo_id, spawn, span, poly,
                                  max(0.1, self._s[veh.idx][-1] / dur),
                                  veh.route_length_m, veh.arrived))
        return out

    def _decimate(self, X, Y) -> list:
        if len(X) < 2:
            return [(X[0], Y[0]), (X[0] + 1.0, Y[0])] if X else [(0.0, 0.0), (1.0, 0.0)]
        poly = [(X[0], Y[0])]
        for k in range(1, len(X) - 1):
            lx, ly = poly[-1]
            if math.hypot(X[k] - lx, Y[k] - ly) >= self.decimate_m:
                poly.append((X[k], Y[k]))
        poly.append((X[-1], Y[-1]))
        if len(poly) < 2:                             # a vehicle that never moved
            poly.append((poly[0][0] + 1.0, poly[0][1]))
        return poly

    def bind(self, vid: int, idx: int) -> None:
        """Associate an engine vehicle id with a frozen trajectory."""
        self._bind[int(vid)] = int(idx)

    # -- per-step drive ----------------------------------------------------- #
    def advance(self, active_list, step: int, t: float) -> None:
        """Write the frozen state into every active replayed vehicle. Called from the engine loop
        exactly where `car_follow` would be, and mutating exactly the same fields."""
        bind = self._bind
        for v in active_list:
            idx = bind.get(v.vid)
            if idx is None:                           # a VRU or any non-replayed actor
                continue
            k = step - self.trace.vehicles[idx].first_step
            X = self._x[idx]
            if k < 0:
                continue
            if k >= len(X):
                k = len(X) - 1                        # clamp: the despawn happens next step anyway
            v.cur_x = X[k]
            v.cur_y = self._y[idx][k]
            v.cur_v = self._v[idx][k]
            v.cur_h = self._h[idx][k]
            v.s_pos = self._s[idx][k]

    # -- network coherence -------------------------------------------------- #
    def offroad_stats(self, dist_fn, *, max_samples: int = 4000) -> dict:
        """Distance-to-road distribution over a deterministic sample of the replayed positions.

        THE COHERENCE GATE. A replayed vehicle must move on the SAME network the engine reasons
        about, or `dist_to_road`, `mapOffRoad` and the geometric channel's building blockage are all
        computed against a different city. A road-following vehicle sits within about half a
        carriageway of the centreline (SUMO puts it on a LANE centre; the engine's graph edge is the
        junction-to-junction line), so the distribution should be tight and bounded by lane
        geometry. Anything scattered means the projection frames disagree."""
        n = sum(len(a) for a in self._x)
        # Deterministic, allocation-free stride over the canonical (idx, step) order -- every
        # vehicle contributes in proportion to how long it was present.
        stride = max(1, n // max(1, max_samples))
        d = []
        seen = 0
        for idx in range(len(self.trace.vehicles)):
            X, Y = self._x[idx], self._y[idx]
            for k in range(len(X)):
                if seen % stride == 0:
                    d.append(float(dist_fn(X[k], Y[k])))
                seen += 1
        if not d:
            return {"n": 0, "p50": 0.0, "p95": 0.0, "p99": 0.0, "max": 0.0, "mean": 0.0,
                    "sampled_of": n}
        d.sort()
        def _p(q):
            return d[min(len(d) - 1, int(q * (len(d) - 1) + 0.5))]
        return {"n": len(d), "sampled_of": n,
                "p50": round(_p(0.50), 3), "p95": round(_p(0.95), 3), "p99": round(_p(0.99), 3),
                "max": round(d[-1], 3), "mean": round(sum(d) / len(d), 3)}


# --------------------------------------------------------------------------- #
# network coherence: ONE transform, two consumers
# --------------------------------------------------------------------------- #
def engine_network(net_path: str, *, frame_city: str = "", cache_dir: str = "datasets/_osmcache",
                   directed: bool = False, strong: bool | None = None, max_nodes: int = 0,
                   shapes: bool = True):
    """Import a SUMO `.net.xml` as the ENGINE's network and return the transform used to do it.

    Returns `(nodes, edges, info, tf)`. `tf` is the very function `netimport` applied to the
    junction coordinates, so handing it to :class:`SumoReplayMobility` puts the replayed vehicles
    and the roads they drive on in ONE frame by construction rather than by agreement.

    `frame_city` re-projects a GEO-REFERENCED net into `osm.py`'s local equirectangular frame for
    that city -- the exact `(lat0, lon0, kx, ky)` tuple derived from the ROAD ways of the same
    extract. Anything else landing in that frame with an independently derived origin misaligns
    silently by whole city blocks while every individual street still looks plausible, which is
    precisely what would make the geometric channel's building blockage nonsense. A procedural net
    (netgenerate) has no geo-projection and keeps its own metric coordinates."""
    from . import netimport                            # noqa: PLC0415  (needs sumolib)

    frame = None
    if frame_city:
        from .osm import CITY_BBOXES, fetch_osm, road_projection   # noqa: PLC0415
        if frame_city not in CITY_BBOXES:
            raise ValueError(f"sumo_frame_city must be one of {sorted(CITY_BBOXES)} "
                             f"(got {frame_city!r})")
        frame = road_projection(fetch_osm(CITY_BBOXES[frame_city], cache_dir))
    net = netimport.read_net(net_path)
    if strong is None:
        strong = directed
    nodes, edges, info = netimport.net_to_network(net, projection=frame, strong=strong,
                                                  max_nodes=max_nodes, shapes=shapes)
    tf, _param = netimport._transformer(net, frame)
    info = dict(info)
    info["projection"] = frame
    return nodes, edges, info, tf


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def main(argv=None) -> int:
    import argparse                                  # noqa: PLC0415

    p = argparse.ArgumentParser(
        prog="python -m scms_sim_ref.mock_pipeline.sumo_trace",
        description="Freeze a SUMO run to a canonical trajectory artifact the Python engine "
                    "replays (mobility_source=sumo_replay).")
    p.add_argument("--net", required=True, help="the SUMO .net.xml")
    g = p.add_mutually_exclusive_group(required=True)
    g.add_argument("--routes", help="a .rou.xml / .trips.xml demand file")
    g.add_argument("--sumocfg", help="a .sumocfg instead of --routes")
    p.add_argument("--out", required=True, help="trajectory artifact to write")
    s = p.add_mutually_exclusive_group(required=True)
    s.add_argument("--run-seed", type=int, help="the ENGINE's run seed; SUMO's seed is derived "
                                                "from it and recorded in the artifact")
    s.add_argument("--seed", type=int, help="SUMO's seed, given explicitly")
    p.add_argument("--steps", type=int, required=True, help="number of simulation steps to freeze")
    p.add_argument("--dt", type=float, default=1.0, help="step length (s); MUST match the run's dt")
    p.add_argument("--begin", type=float, default=0.0, help="SUMO start time (s)")
    p.add_argument("--allow-version-drift", action="store_true",
                   help=f"do not require SUMO {SUMO_VERSION_PINNED} (the trajectories are then not "
                        f"comparable with ones frozen on the pinned build)")
    p.add_argument("--inspect", help="print the summary of an EXISTING artifact and exit")
    a = p.parse_args(argv)

    if a.inspect:
        print(json.dumps(load(a.inspect).summary(), indent=1, sort_keys=True))
        return 0
    trace = freeze(net=a.net, routes=a.routes or "", sumocfg=a.sumocfg or "", out=a.out,
                   seed=a.seed, run_seed=a.run_seed, steps=a.steps, dt=a.dt, begin=a.begin,
                   strict_version=not a.allow_version_drift)
    print(json.dumps({**trace.summary(), "out": a.out,
                      "invocation": trace.meta["invocation"]}, indent=1, sort_keys=True))
    print(f"\nsumo_trace_sha256 = {trace.sha256}\n"
          f"  --road sumo --sumo-net {a.net} --mobility-source sumo_replay "
          f"--sumo-trace {a.out} --sumo-trace-sha256 {trace.sha256}")
    return 0


if __name__ == "__main__":                            # pragma: no cover
    import sys

    sys.exit(main())
