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
def _ttt_arg(v: float) -> str:
    """`--time-to-teleport` as SUMO's own CLI writes it: an integer when it is one."""
    f = float(v)
    return str(int(f)) if f == int(f) else repr(f)


def freeze(*, net: str, out: str, routes: str = "", sumocfg: str = "", seed: int | None = None,
           steps: int, dt: float = 1.0, begin: float = 0.0, sumo_bin: str = "",
           extra_args=(), strict_version: bool = True, run_seed: int | None = None,
           warmup_steps: int = 0, substeps: int = 1,
           time_to_teleport: float = -1.0, split_on_gap: bool = False) -> SumoTrace:
    """Run SUMO ONCE and write the canonical trajectory artifact to `out`.

    `seed` is SUMO's seed. Pass `run_seed` instead to derive it from the engine's run seed
    (`derive_sumo_seed`); the derivation and the resulting value are both recorded in the file.

    `warmup_steps` (default 0) runs that many steps from `begin` WITHOUT recording, so the recorded
    window starts on an already-loaded network instead of an empty one. A demand file whose vehicles
    depart before `begin` is discarded by SUMO, so `--begin 25200` alone starts an empty city and the
    first minutes of the trace are a fill transient, not the peak. Step 0 of the artifact is then
    SUMO time `begin + (warmup_steps + 1) * dt`, which is what `step0_sim_time` records either way.
    A vehicle already on the network when recording starts is written with `first_step = 0`; its
    `depart_s` is the time it was FIRST SEEN, i.e. the start of the recorded window, not its true
    SUMO departure (which is outside the artifact). Default 0 keeps `meta` byte-identical: the
    `warmup_steps` key is emitted only when it is non-zero.

    `substeps` (default 1) DECOUPLES SUMO's integration step from the artifact's sample interval:
    SUMO is stepped at `dt / substeps` and every `substeps`-th state is recorded, so the artifact's
    dt -- and the engine's -- is still `dt`. This is not an optimisation. A scenario is calibrated
    at a particular step length (InTAS: 0.1 s, EIDM, `lateral-resolution 0.8`) and re-integrating it
    at 1 s is a DIFFERENT model, not a coarser view of the same one: on InTAS it produces collisions
    the calibrated configuration does not have, and SUMO 1.25.0 aborts outright with "Request
    lateral offset of vehicle ... for invalid lane" when the sublane model is asked to place a
    vehicle after a 1 s jump onto an internal lane. Sub-stepping keeps the mobility exactly the one
    the scenario was calibrated for and samples it at the rate the engine (and a CAM stream) works
    at. Teleport and collision counters are polled on EVERY SUMO step, not only on recorded ones, so
    an event between two samples is still reported. Default 1 keeps `meta` byte-identical: the
    `substeps` key is emitted only when it is greater than 1.

    `time_to_teleport` (default `-1`, i.e. never) is SUMO's `--time-to-teleport`. A teleport is a
    discontinuity the replay would faithfully reproduce as a physically impossible jump, so the
    default disables it -- but `-1` is a change to the SCENARIO, not just to the artifact, and on a
    congested real network it is a large one: InTAS's AM peak under its own `300` s policy teleports
    **365 times (49 jam, 289 yield, 23 wrongLane) in 25,271 vehicles**, and suppressing all of them
    leaves those vehicles stuck instead, which depresses exactly the flows a validation run
    measures. Passing the scenario's own value keeps the mobility the one the scenario was
    calibrated for, and makes the artifact comparable with an ordinary run of it.

    THIS IS ALSO WHERE SUMO ITSELF WILL FAIL, and the failure is SEED-dependent, not flag-dependent.
    On InTAS from 21600 s, SUMO 1.25.0 dies with a Windows ACCESS_VIOLATION (0xC0000005 / SIGSEGV)
    at sim time **23544.1** under `-1` and at **23904.7** under `300`, at the same seed. The same
    23904.7 crash reproduces in the plain `sumo` binary with neither `libsumo` nor
    `--ignore-route-errors` involved, and the same window at a different SUMO seed runs to 28800
    cleanly. So the teleport policy moves WHEN it happens, not WHETHER: if a long freeze aborts with
    no Python traceback, re-seed before changing anything else.

    `meta["time_to_teleport"]` records the value whenever it is not the default, and
    `meta["teleports"]` is measured either way -- so a trace with teleports in it says so.

    `split_on_gap` (default False) is what makes such a run WRITABLE. A vehicle SUMO teleports --
    or parks, with `parking.maneuver` on -- leaves `vehicle.getIDList()` and comes back later
    somewhere else (measured on InTAS: `carIn5871:1` absent at step 25, present again at 26). The
    format assumes one contiguous presence per id, so the default refuses such a trace outright,
    which is right: writing it as a contiguous run would silently mislabel every later step. With
    `split_on_gap` the return is recorded as a SEPARATE trajectory `<id>#<n>` instead. That is not a
    workaround, it is the better model: a 500 m jump in one step is a discontinuity every position-
    plausibility detector in this repository would correctly flag, whereas a second trajectory is
    simply another vehicle appearing -- which is what a teleported vehicle physically resembles. Its
    `route_length_m` is its OWN driven distance (`getDistance` is cumulative over the whole SUMO
    trip, so the segment start is subtracted); the first segment keeps the raw value, so nothing
    about a gap-free freeze moves. `meta["gap_splits"]` counts them, and is emitted only when the
    option is on.

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
    warmup_steps = int(warmup_steps)
    if warmup_steps < 0:
        raise ValueError(f"warmup_steps must be >= 0 (got {warmup_steps})")
    substeps = int(substeps)
    if substeps < 1:
        raise ValueError(f"substeps must be >= 1 (got {substeps})")
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
    end = begin + (warmup_steps + steps) * dt
    argv = [binary]
    if sumocfg:
        argv += ["-c", sumocfg]
    else:
        argv += ["-n", net, "-r", routes]
    argv += ["--seed", str(int(seed)),
             "--step-length", repr(float(dt) / substeps),
             "--begin", repr(float(begin)),
             "--end", repr(float(end)),
             "--threads", "1",
             "--no-step-log", "true",
             "--no-warnings", "true",
             # formatted as an integer when it is one, so the DEFAULT renders "-1" exactly as it did
             # before this became a parameter -- `meta["invocation"]` is inside the hashed artifact.
             "--time-to-teleport", _ttt_arg(time_to_teleport),
             "--ignore-route-errors", "true",
             *[str(a) for a in extra_args]]

    # --- drive SUMO ------------------------------------------------------ #
    #: raw[key] = [first_step, [x], [y], [speed], [angle], depart_s, last_dist, last_step, dist0]
    #: `key` is the SUMO id, or `<id>#<n>` for the n-th segment when `split_on_gap` is on.
    raw: dict = {}
    live: dict = {}                 # sumo id -> the key it is currently being recorded under
    seg_no: dict = {}               # sumo id -> how many times it has left and come back
    split_keys: set = set()         # the keys that are post-gap segments (NOT `"#" in key`: a SUMO
    splits = 0                      # id may legitimately contain a '#')
    teleports = 0
    collisions = 0
    libsumo.start(argv)
    try:
        for _ in range(warmup_steps * substeps):
            # WARM-UP: stepped, never recorded. Teleports/collisions in here are not counted either
            # -- they did not happen inside the artifact, and claiming them would misattribute a
            # transient of the fill phase to the window the engine actually replays.
            libsumo.simulationStep()
        for step in range(steps):
            for _ in range(substeps):
                libsumo.simulationStep()
                # polled on EVERY SUMO step: these are per-step counters, so sampling them only on
                # recorded steps would silently drop every event that happened in between.
                teleports += libsumo.simulation.getStartingTeleportNumber()
                collisions += libsumo.simulation.getCollidingVehiclesNumber()
            now = libsumo.simulation.getTime()
            for vid in libsumo.vehicle.getIDList():
                rec = raw.get(live.get(vid))
                if rec is not None and rec[7] != step - 1:
                    # The replay indexes a vehicle's state by `step - first_step`, and `load()`
                    # refuses a gap on read -- so a gap must be handled HERE rather than written out
                    # as a contiguous run that silently mislabels every later step.
                    if not split_on_gap:
                        raise RuntimeError(
                            f"SUMO vehicle {vid!r} disappeared and came back "
                            f"(steps {rec[7]} -> {step}); the trace format assumes one contiguous "
                            f"presence per vehicle id. Pass split_on_gap=True "
                            f"(--split-on-gap) to record the return as a separate trajectory")
                    splits += 1
                    seg_no[vid] = seg_no.get(vid, 0) + 1
                    live[vid] = f"{vid}#{seg_no[vid]}"
                    split_keys.add(live[vid])
                    rec = None
                if rec is None:
                    key = live.setdefault(vid, vid)
                    rec = raw[key] = [step, [], [], [], [], now, 0.0, step - 1,
                                      libsumo.vehicle.getDistance(vid)]
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
    rec_begin = begin + warmup_steps * dt          # SUMO time the RECORDED window starts from
    order = sorted(raw, key=lambda k: (raw[k][0], k))
    vehicles: list[TraceVehicle] = []
    xs, ys, vs, angs = [], [], [], []
    for idx, vid in enumerate(order):
        first, X, Y, V, A, depart, dist, _last_seen, dist0 = raw[vid]
        if vid in split_keys:
            # a post-gap segment: `getDistance` is cumulative over the whole SUMO trip, so the
            # segment's own driven length is the difference. The first segment keeps the raw value,
            # which is what every artifact frozen before splitting existed already recorded.
            dist -= dist0
        if any(c.isspace() for c in vid):
            raise ValueError(f"SUMO vehicle id {vid!r} contains whitespace, which the line-oriented "
                             f"trace format cannot represent unambiguously")
        last = first + len(X) - 1
        vehicles.append(TraceVehicle(idx, vid, first, last, depart,
                                     rec_begin + (last + 1) * dt, last < steps - 1, dist))
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
        # step k of this trace is SUMO time `begin + (warmup_steps+k+1)*dt` -- the state AFTER that
        # many simulationStep()s. The engine maps its own step k onto trace step k; the absolute
        # offset is recorded so a consumer can align against SUMO output files if it ever needs to.
        "step0_sim_time": float(rec_begin + dt),
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
    if warmup_steps:
        # Emitted ONLY when non-zero, so a warm-up-free freeze keeps `meta` -- and therefore the
        # artifact's sha256 -- byte-identical to one taken before this option existed.
        meta["warmup_steps"] = int(warmup_steps)
        meta["recorded_begin"] = float(rec_begin)
    if substeps > 1:                                   # same rule: emitted only when it is not 1
        meta["substeps"] = int(substeps)
        meta["sumo_step_length"] = float(dt) / substeps
    if float(time_to_teleport) != -1.0:                # ... and only when it is not "never"
        meta["time_to_teleport"] = float(time_to_teleport)
    if split_on_gap:                                   # ... and only when splitting is on
        meta["split_on_gap"] = True
        meta["gap_splits"] = int(splits)
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
def frame_for_city(frame_city: str, cache_dir: str = "datasets/_osmcache") -> dict | None:
    """`sumo_frame_city` -> the projection tuple every layer of that map must share, or None.

    ONE definition, used by `engine_network` for the roads and by the caller for anything else that
    has to land in the same frame (building footprints, above all). A second derivation of the same
    tuple is the projection trap with extra steps."""
    if not frame_city:
        return None
    from .osm import CITY_BBOXES, fetch_osm, road_projection   # noqa: PLC0415

    if frame_city not in CITY_BBOXES:
        raise ValueError(f"sumo_frame_city must be one of {sorted(CITY_BBOXES)} "
                         f"(got {frame_city!r})")
    return road_projection(fetch_osm(CITY_BBOXES[frame_city], cache_dir))


def engine_network(net_path: str, *, frame_city: str = "", cache_dir: str = "datasets/_osmcache",
                   directed: bool = False, strong: bool | None = None, max_nodes: int = 0,
                   shapes: bool = True, surface: bool = True, signals: bool = False):
    """Import a SUMO `.net.xml` as the ENGINE's network and return the transform used to do it.

    Returns `(nodes, edges, info, tf)`. `tf` is the very function `netimport` applied to the
    junction coordinates, so handing it to :class:`SumoReplayMobility` puts the replayed vehicles
    and the roads they drive on in ONE frame by construction rather than by agreement.

    `frame_city` re-projects a GEO-REFERENCED net into `osm.py`'s local equirectangular frame for
    that city -- the exact `(lat0, lon0, kx, ky)` tuple derived from the ROAD ways of the same
    extract. Anything else landing in that frame with an independently derived origin misaligns
    silently by whole city blocks while every individual street still looks plausible, which is
    precisely what would make the geometric channel's building blockage nonsense. A procedural net
    (netgenerate) has no geo-projection and keeps its own metric coordinates.

    THREE THINGS THIS FIXES ON EVERY SUMO NET, each measured on InTAS (1,188 vehicles, 4,071 sampled
    replayed positions; distance from the position to the engine's roads):

    1. `undirected_shapes=True` -- the engine builds from the document's UNDIRECTED array, and as
       `[a, b, speed]` triples that array describes every curved road as its straight chord. That
       alone was p50 2.203 m / p95 17.434 m / max 95.227 m, 12.7% of positions more than 8 m off
       road, and it is what made the coherence gate fire on an honest trace.
    2. `surface=True` -- `dist_to_road` measures against the map's real tarmac (every carriageway's
       own polyline, plus the junction polygons a vehicle crosses on an internal lane) rather than
       against the routing graph. Final: p50 0.351 m / p95 3.195 m / max 4.804 m, 0.00% over 8 m,
       which is the raw-SUMO-network reference (p95 3.201 m / max 6.400 m) reached from inside the
       engine.
    3. `strong=True` unconditionally -- see below.

    STRONG CONNECTIVITY IS NOT OPTIONAL HERE, and this is a deliberate change of behaviour. InTAS
    imports 3,328 junctions of which 3,289 are in the largest strongly connected component: 39 can be
    driven into and never out of (a bbox-clipped one-way pair, or a ramp whose only exit leaves the
    extract). Keeping them meant the engine's map DEPENDED on `custom_network_directed`: off, the
    router ignored one-ways and the traps were invisible; on, `run._parse_custom_network` silently
    trimmed them. Two configurations, two different cities, one manifest schema describing both.
    They are dropped in both cases now -- 39 nodes and 49 undirected edges, 1.2% of the graph -- so
    the network the manifest describes is the network the run used. The dropped roads stay in the
    `road_surface` layer, because a vehicle SUMO drove down a road the engine's router declined to
    use is still on a road, and flagging it `mapOffRoad` would be exactly the false positive this
    whole change removes.

    `directed` is therefore no longer what selects the trim -- it is kept because callers pass it and
    because it still records which LAYER of the document the caller intends to build from. Pass
    `strong=False` explicitly to opt out, which only a diagnostic should do.

    `signals=True` additionally reads the net's REAL ``<tlLogic>`` programs and emits
    ``info["signal_programs"]`` -- the per-junction phase strings plus the movement -> link-index
    mapping, in THESE node indices (see `signals.py`). It also forces the sumolib read to
    ``withPrograms=True``, because sumolib silently drops every ``<tlLogic>`` otherwise and the
    caller would get a city whose signals do not exist rather than an error. Off by default: the
    read is ~15% slower with programs on a 16.9 MB net, and nothing consumes the layer unless
    `PipelineConfig.real_signals` is set."""
    from . import netimport                            # noqa: PLC0415  (needs sumolib)

    frame = frame_for_city(frame_city, cache_dir)
    net = netimport.read_net(net_path, programs=bool(signals))
    if strong is None:
        strong = True
    nodes, edges, info = netimport.net_to_network(net, projection=frame, strong=strong,
                                                  max_nodes=max_nodes, shapes=shapes,
                                                  undirected_shapes=shapes, surface=surface,
                                                  signals=bool(signals))
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
    p.add_argument("--warmup", type=int, default=0, metavar="STEPS",
                   help="steps to run from --begin WITHOUT recording, so the artifact starts on a "
                        "loaded network instead of an empty one (default 0)")
    p.add_argument("--time-to-teleport", type=float, default=-1.0, metavar="S",
                   help="SUMO --time-to-teleport (default -1 = never). Pass the SCENARIO's own "
                        "value to keep its calibrated mobility: InTAS's AM peak teleports 365 "
                        "times under its 300 s policy, and -1 leaves those vehicles stuck instead")
    p.add_argument("--split-on-gap", action="store_true",
                   help="record a vehicle that leaves the network and returns (teleport, parking) "
                        "as a SEPARATE trajectory '<id>#<n>' instead of refusing the trace")
    p.add_argument("--substeps", type=int, default=1, metavar="N",
                   help="step SUMO at --dt/N and record every Nth state: keeps the scenario's own "
                        "calibrated integration step while the artifact stays at --dt (default 1)")
    p.add_argument("--sumo-arg", action="append", default=[], metavar="ARG",
                   help="extra argument passed straight to SUMO, repeatable. USE THE = FORM for a "
                        "value that starts with a dash, or argparse eats it: "
                        "`--sumo-arg=--output-prefix --sumo-arg=mine_`. That is the one you will "
                        "actually need -- a scenario's own .sumocfg names its summary/tripinfo/log "
                        "outputs, and freezing it OVERWRITES those committed files in place unless "
                        "they are prefixed. Recorded in meta['invocation'], i.e. inside the "
                        "artifact's hash")
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
                   warmup_steps=a.warmup, substeps=a.substeps, extra_args=tuple(a.sumo_arg),
                   time_to_teleport=a.time_to_teleport, split_on_gap=a.split_on_gap,
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
