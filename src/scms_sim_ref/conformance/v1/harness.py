"""Deterministic scenarios and instruments for the v1 conformance suite.

Everything here is seeded, offline and stdlib-only. Three instruments do the work the design's
section 5 asks for and that no `Protocol`, `ABC` or name-based linter can do:

* :func:`build_frames` / :func:`build_ladder` -- fixed, replayable :class:`StepFrame` sequences. The
  ladder rotates its transmitters around the receiver each step, so the DISTANCE is held exactly
  constant while the link moves far enough through the environment to decorrelate any AR(1)
  shadowing. That is what makes C9's PDR-versus-distance sample independent rather than one
  correlated trajectory measured seven times.
* :class:`DrawCounter` -- counts TOP-LEVEL `random.Random` method calls, with a re-entrancy guard so
  `gammavariate`'s internal rejection loop (a VARIABLE number of `random()` calls) counts once, not
  once per rejection. Without the guard C4 would be unmeasurable for any model using a gamma or
  beta variate -- which includes this repo's own `geometric`.
* :func:`audit_guard` -- a PEP 578 audit hook for filesystem/network/subprocess DETECTION. Stated
  honestly: PEP 578 itself says it *"is not sandboxing... does not attempt to prevent malicious
  behavior"*, and hooks fire only in the current interpreter, so a subprocess escapes entirely. C5
  detects accidental I/O; it does not contain hostile I/O.
"""
from __future__ import annotations

import math
import random
import sys
import threading

from ...api import channel as _channel
from ...api.channel import BatchAdapter, PerLinkAdapter, StationSnapshot, StepFrame, Transmission

#: Antenna and blocker heights used by every scenario. The engine's own values
#: (`V2X_ANTENNA_HEIGHT_M`, `TR37885_BLOCKER_HEIGHT_M["car"]`), restated here so the harness has no
#: engine import at module scope.
ANT_H_M = 1.6
BLOCKER_H_M = 1.6
RSU_ANT_H_M = 5.0

#: Bytes on the wire for one native_v1 PDU (`NATIVE_WIRE_SIZE_BYTES`).
WIRE_SIZE_BYTES = 300


def cert_digest(vid: int, step: int) -> str:
    """A deterministic HashedId8-shaped pseudonym. Rotated per step on purpose: `logdistance` keys
    its shadowing stream on the cert digest (a documented correctness bug, deliberately preserved),
    so a harness that held the digest fixed would silently freeze that model's randomness."""
    return "%016x" % ((vid * 2654435761 + step * 40503) & 0xFFFFFFFFFFFFFFFF)


# --------------------------------------------------------------------------- #
# Scenarios
# --------------------------------------------------------------------------- #
def _station(vid, x, y, *, is_rsu=False, is_vru=False, rx_range_m=0.0, oracle=None):
    if oracle is None:
        return StationSnapshot(vid, x, y, RSU_ANT_H_M if is_rsu else ANT_H_M,
                               0.0 if (is_vru or is_rsu) else BLOCKER_H_M,
                               is_rsu, is_vru, None, rx_range_m)
    return OracleStation(vid, x, y, RSU_ANT_H_M if is_rsu else ANT_H_M,
                         0.0 if (is_vru or is_rsu) else BLOCKER_H_M,
                         is_rsu, is_vru, None, rx_range_m, **oracle)


class OracleStation(StationSnapshot):
    """A :class:`StationSnapshot` carrying GROUND TRUTH the ABI does not declare.

    Used by check C6b, and it is the single most important object in this file. The declared DTO
    fields are IDENTICAL between a plain snapshot and one of these; the only difference is oracle
    state a conformant model has no business reading. A model that keys on it -- `if
    getattr(tx, "is_attacker", False): return None` -- is a perfect oracle-laundering vector that
    passes every range check, every name-based lint and every plausibility eyeball, because its
    output looks like ordinary physics. Comparing the two traces is the only thing that catches it.

    This is the channel-side form of F2MD's `realDynamicMap` mistake (`LegacyChecks.h` hands the
    checks object a pointer to the ground-truth positions of every node), restated as a test.
    """

    __slots__ = ("is_attacker", "falsified", "true_x", "true_y", "attack_type")

    def __init__(self, vid, x, y, ant_h_m, blocker_h_m, is_rsu, is_vru, tx_power_dbm, rx_range_m,
                 *, is_attacker=False, falsified=False, attack_type=""):
        super().__init__(vid, x, y, ant_h_m, blocker_h_m, is_rsu, is_vru, tx_power_dbm, rx_range_m)
        # frozen dataclass -> the slots have to be written the way the dataclass itself writes them
        object.__setattr__(self, "is_attacker", bool(is_attacker))
        object.__setattr__(self, "falsified", bool(falsified))
        object.__setattr__(self, "attack_type", str(attack_type))
        object.__setattr__(self, "true_x", float(x))
        object.__setattr__(self, "true_y", float(y))


def build_frames(*, n_steps: int = 6, n_stations: int = 12, spacing_m: float = 55.0,
                 move_m: float = 0.0, weather_loss: float = 0.0, oracle: bool = False,
                 env: dict = None, dt: float = 1.0, step0: int = 0):
    """A street of `n_stations` stations, one PDU each per step.

    `move_m == 0.0` freezes the geometry, which is what C4 needs: with an identical candidate set
    every step, a model that advances its per-step state exactly once draws the same number of times
    every step, and any other count is a step-guard bug.

    `oracle=True` swaps in :class:`OracleStation`s whose DECLARED fields are unchanged (C6b).

    `step0` starts the numbering somewhere other than 0. C13 uses it to drive a short tail at a HIGH
    step index: a plugin whose side effect is guarded by `if frame.step >= 30` is invisible to any
    contiguous window shorter than 31 steps, and the realistic shape of a config write really is
    "after the run has settled". A cheap jump in the step number costs four frames and catches every
    threshold below it, which a longer contiguous trace would only do by being longer.
    """
    envmap = dict(env or {"buildings": [], "weather": "clear"})
    frames = []
    for k in range(n_steps):
        step = step0 + k
        shift = move_m * k                  # geometry advances with the FRAME, not the step label
        stations = {}
        for i in range(n_stations):
            x = i * spacing_m + shift
            y = float((i * 37) % 7) * 4.0
            ora = None
            if oracle:
                ora = {"is_attacker": (i % 3 == 0), "falsified": (i % 3 == 0),
                       "attack_type": "ConstPos" if i % 3 == 0 else ""}
            stations[i] = _station(i, x, y, oracle=ora)
        txns = [Transmission(i, i, "cam", 1, WIRE_SIZE_BYTES, cert_digest(i, step))
                for i in range(n_stations)]
        frames.append(StepFrame(step, float(step) * dt, dt, stations, txns,
                                sorted(stations), weather_loss, envmap))
    return frames


def build_ladder(distance_m: float, *, n_tx: int = 12, n_steps: int = 8, dt: float = 1.0):
    """One receiver at the origin and `n_tx` transmitters on a circle of radius `distance_m`.

    The circle is ROTATED by a fixed angle each step. Distance is preserved EXACTLY (so the ladder
    measures distance and nothing else) while both endpoints move tens of metres through the
    environment, which is what decorrelates a Gudmundson/AR(1) shadowing process between samples.
    A ladder that moved radially, or did not move at all, would report one correlated trajectory as
    if it were `n_tx * n_steps` independent samples.
    """
    frames = []
    for step in range(n_steps):
        theta0 = 0.37 * step
        stations = {0: _station(0, 0.0, 0.0)}
        for k in range(n_tx):
            th = theta0 + 2.0 * math.pi * k / n_tx
            stations[k + 1] = _station(k + 1, distance_m * math.cos(th),
                                       distance_m * math.sin(th))
        txns = [Transmission(k, k + 1, "cam", 1, WIRE_SIZE_BYTES, cert_digest(k + 1, step))
                for k in range(n_tx)]
        frames.append(StepFrame(step, float(step) * dt, dt, stations, txns, [0], 0.0,
                                {"buildings": [], "weather": "clear"}))
    return frames


# --------------------------------------------------------------------------- #
# Driving a model
# --------------------------------------------------------------------------- #
def adapt(model, rng_ns=None):
    """Wrap a raw model in whichever adapter its shape calls for -- exactly as `build_channel` does,
    so the suite drives a plugin through the same object the engine drives it through.

    `rng_ns` is the model's `RngNamespace`. Passing it is not optional book-keeping: the adapter's
    `begin_step` is what advances `RngNamespace._step`, so a harness that omits it grades the model
    with a FROZEN step -- every `stream()` key ending `:s-1` -- which is a DIFFERENT MODEL from the
    one the engine runs. Measured on the reference plugin's own C9 ladder, frozen versus advanced:
    PDR `1.000 1.000 1.000 1.000 0.969 0.885 0.792` versus `1.000 1.000 0.979 0.948 0.844 0.698
    0.615`. A frozen stateless draw is a fixed per-link offset, so the curve flattens and 0.18 of
    range loss disappears at the far rung; C1/C2/C11 likewise compare traces whose step dimension
    carries no information.
    """
    if hasattr(model, "evaluate"):
        return PerLinkAdapter(model, rng_ns)
    return BatchAdapter(model, rng_ns)


def candidates_for(adapter, frame, *, beyond: float = 0.0):
    """`(tx_index, rx_vid, distance_m)` in the engine's canonical order: tx_index ascending within
    rx_vid ascending, pre-filtered by the model's own candidate window.

    `beyond > 0` widens the filter by that many metres, which is how C8 offers the model links it
    has declared it cannot deliver.
    """
    out = []
    for rx_vid in frame.receivers:
        rx = frame.stations[rx_vid]
        cap = _channel.window_of(adapter, rx) + beyond
        row = []
        for txn in frame.transmissions:
            if txn.tx_vid == rx_vid:
                continue
            tx = frame.stations[txn.tx_vid]
            d = math.hypot(tx.x - rx.x, tx.y - rx.y)
            if d <= cap:
                row.append((txn.tx_index, rx_vid, d))
        row.sort()
        out.extend(row)
    return out


def trace(model, frames, *, order: str = "sorted", beyond: float = 0.0, step_hook=None,
          rng_ns=None):
    """Drive `model` over `frames` and return a canonical, comparable list of rows.

    One row per DELIVERED link: `(step, tx_index, rx_vid, rssi_dbm, link_state, delay_s,
    extras_tuple)`. Undelivered links are absent -- never `heard=False` -- which is the ABI's rule.
    Outcomes are re-sorted by `(rx_vid, tx_index)` before they are recorded, so a backend's internal
    ordering is structurally incapable of reaching the comparison.
    """
    ad = adapt(model, rng_ns)
    rows = []
    for frame in frames:
        ad.begin_step(frame)
        cands = candidates_for(ad, frame, beyond=beyond)
        if order == "reversed":
            cands = list(reversed(cands))
        elif order != "sorted":
            raise ValueError(f"unknown order {order!r}")
        dist = {(t, r): d for t, r, d in cands}
        outs = _channel.sort_outcomes(ad.deliver(frame, cands))
        for o in outs:
            rows.append((frame.step, o.tx_index, o.rx_vid, o.rssi_dbm, o.link_state, o.delay_s,
                         tuple(sorted(dict(o.extras or {}).items()))))
        if step_hook is not None:
            step_hook(frame, cands, outs, dist)
    return rows


# --------------------------------------------------------------------------- #
# DrawCounter -- the instrument C4 needs
# --------------------------------------------------------------------------- #
#: Every public generator method on `random.Random`. Patched on the CLASS, so it also covers the
#: module-level `random.random()` shims (bound methods of the hidden global instance) -- which is
#: precisely the leak C3 hunts for.
_RANDOM_METHODS = (
    "random", "getrandbits", "randbytes", "randrange", "randint", "choice", "choices", "shuffle",
    "sample", "uniform", "triangular", "normalvariate", "gauss", "lognormvariate", "expovariate",
    "vonmisesvariate", "gammavariate", "betavariate", "paretovariate", "weibullvariate", "binomialvariate",
)


#: sentinel: the attribute is inherited from the C base `_random.Random`, not owned by `random.Random`
_INHERITED = object()


#: Module-level `random` names the surface records alongside the class methods. `random.Random`
#: ITSELF is here because rebinding the CLASS -- `random.Random = MyImpostor` -- is a distinct attack
#: from rebinding one of its methods: it owns every `random.Random(f"{seed}:...")` the engine
#: constructs AFTERWARDS (about twenty keyed sites), and it is what a delayed attack installs.
#: Without it the restore path was incomplete in the worst possible way: the per-method loop wrote
#: the engine's original methods onto the IMPOSTOR and left the impostor bound, so a suite run
#: against such a model handed the rest of the process a rebound `random`.
_RANDOM_MODULE_NAMES = ("Random", "SystemRandom", "_inst", "random", "seed", "getstate", "setstate")


def random_class_surface() -> dict:
    """Identity of every generator method on `random.Random`, the class itself, and the module-level
    bindings.

    The instrument C3's third trap needs. Rebinding `random.Random.random` reaches EVERY stream in
    the process at once -- including the engine's private `random.Random(cfg.seed)`, whose draw
    count and order are load-bearing -- while leaving every generator STATE a check could snapshot
    perfectly intact. Comparing identities is the only way to see it.
    """
    surface = {name: random.Random.__dict__.get(name, _INHERITED) for name in _RANDOM_METHODS}
    for name in _RANDOM_MODULE_NAMES:
        surface[f"random.{name}"] = getattr(random, name, None)
    return surface


def tampered_random_names(surface: dict) -> list:
    """Sorted names in `surface` whose binding is no longer the one it recorded.

    When the CLASS itself moved, only that is reported: every per-method comparison is then against a
    different class and would list twenty consequences of one cause.
    """
    out = [n for n in (f"random.{m}" for m in _RANDOM_MODULE_NAMES)
           if getattr(random, n.split(".", 1)[1], None) is not surface[n]]
    if random.Random is surface["random.Random"]:
        out += [n for n in _RANDOM_METHODS
                if random.Random.__dict__.get(n, _INHERITED) is not surface[n]]
    return sorted(out)


def restore_random_class_surface(surface: dict) -> list:
    """Put back anything that moved. Returns the names that had been tampered with.

    A check that DETECTS a tamper and leaves it installed has poisoned the interpreter for
    everything that runs after it, which is a worse outcome than not checking. The MODULE names go
    back first: with `random.Random` rebound, the per-method loop would otherwise patch the impostor.
    """
    moved = tampered_random_names(surface)
    for name in _RANDOM_MODULE_NAMES:
        if getattr(random, name, None) is not surface[f"random.{name}"]:
            setattr(random, name, surface[f"random.{name}"])
    for name in _RANDOM_METHODS:
        original = surface[name]
        if random.Random.__dict__.get(name, _INHERITED) is original:
            continue
        if original is _INHERITED:
            try:
                delattr(random.Random, name)              # was inherited from _random.Random
            except AttributeError:                        # pragma: no cover
                pass
        else:
            setattr(random.Random, name, original)
    return moved


class DrawCounter:
    """Count TOP-LEVEL `random.Random` draws made while the context is open.

    The re-entrancy guard is the whole trick. `gammavariate` is rejection sampling: it calls
    `self.random()` a VARIABLE number of times per invocation, and `gauss`/`normalvariate` call it
    too. Counting primitives would therefore make the per-step draw count of any model using a gamma
    variate -- `geometric`'s Nakagami fade, for one -- legitimately non-constant, and C4 would be
    measuring the rejection sampler rather than the step guard. Counting only the OUTERMOST call
    measures what the contract actually says: how many times the model asked for a number.
    """

    __slots__ = ("count", "_orig", "_local")

    def __init__(self):
        self.count = 0
        self._orig = {}
        self._local = threading.local()

    def _wrap(self, name, fn):
        def wrapper(inner_self, *a, **kw):
            depth = getattr(self._local, "d", 0)
            if depth == 0:
                self.count += 1
            self._local.d = depth + 1
            try:
                return fn(inner_self, *a, **kw)
            finally:
                self._local.d = depth
        wrapper.__name__ = name
        return wrapper

    def __enter__(self):
        for name in _RANDOM_METHODS:
            fn = getattr(random.Random, name, None)
            if fn is None:
                continue
            # remember whether the attribute was OWNED by random.Random or inherited from the C
            # base, so it can be restored to the right place rather than shadowed forever
            self._orig[name] = random.Random.__dict__.get(name, None)
            setattr(random.Random, name, self._wrap(name, fn))
        return self

    def __exit__(self, *exc):
        for name, original in self._orig.items():
            if original is None:
                try:
                    delattr(random.Random, name)          # was inherited from _random.Random
                except AttributeError:                    # pragma: no cover
                    pass
            else:
                setattr(random.Random, name, original)
        self._orig.clear()
        return False


# --------------------------------------------------------------------------- #
# audit_guard -- the instrument C5 needs
# --------------------------------------------------------------------------- #
class IoViolation(AssertionError):
    """A plugin performed filesystem, network or subprocess I/O inside a guarded region."""


DENY_DEFAULT = frozenset({"open-write", "socket.connect", "subprocess.Popen", "os.system"})

#: PEP 578 event names, mapped onto the design's four deny labels. `open` is audited with
#: `(path, mode, flags)`; a READ is allowed (a plugin may legitimately read its own refdata), a
#: write/append/create is not, because a plugin-written file is either digest-bearing data that must
#: be declared or an undeclared side channel.
_EVENT_LABELS = {
    "socket.connect": "socket.connect", "socket.getaddrinfo": "socket.connect",
    "socket.bind": "socket.connect", "socket.sendto": "socket.connect",
    "subprocess.Popen": "subprocess.Popen", "os.system": "os.system",
    "os.exec": "os.system", "os.posix_spawn": "os.system",
    "os.remove": "open-write", "os.rename": "open-write", "os.mkdir": "open-write",
    "os.rmdir": "open-write", "shutil.copyfile": "open-write",
    "urllib.Request": "socket.connect", "ftplib.connect": "socket.connect",
}

_GUARD = threading.local()
_HOOK_INSTALLED = [False]


def _audit_hook(event, args):
    state = getattr(_GUARD, "state", None)
    if state is None:
        return
    label = _EVENT_LABELS.get(event)
    if label is None and event == "open":
        mode = args[1] if len(args) > 1 else ""
        if isinstance(mode, str) and any(c in mode for c in "wax+"):
            label = "open-write"
    if label is None or label not in state["deny"]:
        return
    detail = f"{event}{args[:1]!r}"
    state["hits"].append(detail)
    raise IoViolation(f"plugin performed denied I/O: {detail} (denied: {sorted(state['deny'])})")


class audit_guard:                                        # noqa: N801 - context-manager naming
    """Deny filesystem/network/subprocess events for the duration of the block.

    `sys.addaudithook` CANNOT be removed once added, so the hook is installed lazily -- the first
    time a conformance run actually needs it -- and is inert (one dict lookup on a thread-local)
    whenever no guard is armed. That keeps the cost off every other test in the suite, which is why
    this is not simply installed at import.
    """

    __slots__ = ("deny", "hits", "_prev")

    def __init__(self, deny=DENY_DEFAULT):
        self.deny = frozenset(deny)
        self.hits: list = []

    def __enter__(self):
        if not _HOOK_INSTALLED[0]:
            sys.addaudithook(_audit_hook)
            _HOOK_INSTALLED[0] = True
        self._prev = getattr(_GUARD, "state", None)
        _GUARD.state = {"deny": self.deny, "hits": self.hits}
        return self

    def __exit__(self, *exc):
        _GUARD.state = self._prev
        return False


# --------------------------------------------------------------------------- #
# small helpers
# --------------------------------------------------------------------------- #
def finite(x) -> bool:
    return x is None or (isinstance(x, (int, float)) and math.isfinite(float(x)))


def pdr_and_rssi(model, distance_m, *, n_tx=12, n_steps=8, rng_ns=None):
    """(PDR, mean rssi or None, n_offered) at one rung of the C9 distance ladder."""
    frames = build_ladder(distance_m, n_tx=n_tx, n_steps=n_steps)
    offered = [0]

    def _count(frame, cands, outs, dist):
        offered[0] += len(cands)

    rows = trace(model, frames, step_hook=_count, rng_ns=rng_ns)
    got = len(rows)
    rssis = [r[3] for r in rows if r[3] is not None]
    mean = (sum(rssis) / len(rssis)) if rssis else None
    return (got / offered[0] if offered[0] else 0.0), mean, offered[0]
