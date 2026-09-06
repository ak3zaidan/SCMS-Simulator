"""`ChannelModel` -- the "pick any network system" seam (PLUGIN-ARCHITECTURE.md section 2.1).

Derived from the ACTUAL de-facto interface `GeometricChannel` (run.py:539-711) already implements,
not invented: `begin_step`, `cbr`, `collision_loss`, `evaluate`, plus the run-scalar `cap_m`. That
class already proves the two properties this contract needs -- it draws ZERO from the engine's
global `rng`, and its persistent per-link state advances exactly once per step (the
`st["step"] == self.step` guard). It was selected by an `if`, not a registry. **That `if` was the
whole problem.**

Two levels:

* :class:`BatchChannelModel` -- THE canonical ABI. One exchange per step (**D1**). This is the only
  shape in which an out-of-process ns-3 / OMNeT++ backend can exist: a mid-size run is ~6 000 link
  decisions per step over ~3 600 steps (~2.2e7 link decisions); at 2 ms per local-socket round trip
  that is ~12 hours per link versus ~7 seconds per step -- about 1e4x. The ABI must be per-step, and
  per-step FROM THE START, because retrofitting batching later means re-deriving every pinned digest.
* :class:`LinkChannelModel` -- the convenience shape for a pure in-process analytic model (literally
  `GeometricChannel`'s existing surface), wrapped into the canonical ABI by :class:`PerLinkAdapter`.

The message vocabulary below -- not a class signature -- is the ABI. It is modelled on Eclipse
MOSAIC, where `sns` / `ns3` / `omnetpp` are interchangeable because they subscribe to an IDENTICAL
interaction set; that contract is in this repo's own tree at
`third_party/veremi-nextgen/Generator/simulation/mosaic/etc/runtime.json:94-154`.

DEVIATIONS from the memo's literal listing, and why (all forced by the live tree):

1. ``StationSnapshot.rx_range_m`` and :meth:`LinkChannelModel.reach_m_for` -- the memo models
   ``reach_m`` as a RUN SCALAR. The engine has a PER-RECEIVER range (`rx.rx_range or
   cfg.radio_range_m`; an RSU's `rsu_range_m` legitimately exceeds a vehicle's), and
   `acceptanceRangeThreshold` reads it per receiver (run.py:3469). ``reach_m`` stays the run-scalar
   upper bound; ``reach_m_for(rx)`` is the per-receiver refinement and defaults to it.
2. :meth:`LinkChannelModel.window_m` is SEPARATE from ``reach_m_for``. The memo conflates the
   candidate-search cutoff with the declared delivery reach. The live tree does not, and the
   difference is digest-bearing: under `logdistance`, `cap` (a shadow-widened SEARCH window) and
   `art_reach` (= `rr`) differ (run.py:3305-3310 vs 3469). Collapsing them moves `939b4faa...`.
3. ``Transmission.cert_digest`` -- `logdistance`'s shadowing stream is keyed on the sender's CERT
   DIGEST (run.py:3360). That is a documented correctness bug (a pseudonym rotation resamples the
   channel) deliberately preserved here, because fixing it moves `939b4faa...`; it is scheduled for
   a re-pin, not smuggled into a refactor. The field is honest regardless: the pseudonym a PDU was
   sent under is a wire-level property of that transmission.
4. ``LinkOutcome.tx_index`` / ``rx_vid`` default to :data:`UNBOUND` (-1). On the per-link path the
   engine already knows both, so a stateless model can return a shared frozen constant instead of
   allocating per delivered link. :meth:`PerLinkAdapter.deliver` binds them before yielding, so the
   canonical ABI never sees an unbound outcome.
5. :meth:`LinkChannelModel.delivery_coin` -- the engine composes loss (it owns the congestion term,
   the weather term and, for the grandfathered built-ins, the global-`rng` coin), so a model that
   declares ``independent_survival`` must supply the delivery coin from ITS OWN stream. That is what
   keeps `geometric` drawing zero from the global `rng`.
"""
from __future__ import annotations

import math
from dataclasses import dataclass, field
from types import MappingProxyType as _MappingProxyType
from typing import Iterable, Mapping, Optional, Protocol, Sequence, runtime_checkable

from . import guard


def _zero2(_a, _b) -> float:
    """The `0.0` a batch model that models neither congestion nor collisions has always returned."""
    return 0.0


def _bind_guarded(model, name: str, guard_label, default=None):
    """`model.<name>`, guarded, bound ONCE at adapter construction.

    A model that does not define `name` keeps the behaviour it had when the adapter forwarded
    through `self.model.<name>` on every call: `default` if one is given, otherwise a shim that
    raises the same `AttributeError` at CALL time. Binding must not turn "this optional method was
    never called" into "this adapter cannot be built".
    """
    fn = getattr(model, name, None)
    if fn is None:
        if default is not None:
            return guard.guarded(default, guard_label)

        def missing(*args, **kwargs):
            return getattr(model, name)(*args, **kwargs)      # raises AttributeError, as before
        return missing
    return guard.guarded(fn, guard_label)

INTERFACE_NAME = "ChannelModel"
INTERFACE_VERSION = "ChannelModel/1.0"
#: highest MINOR of ChannelModel/1.x this engine can speak (major must match exactly)
MAX_MINOR = 0

#: sentinel for LinkOutcome.tx_index / rx_vid on the per-link path (see deviation 4 above)
UNBOUND = -1

# --------------------------------------------------------------------------- #
# Capabilities. Declared, negotiated at load, recorded in the manifest, and used to gate optional
# output columns exactly as `_emit_rssi` does today (run.py:1984).
# --------------------------------------------------------------------------- #
CAP_RSSI = "rssi"
CAP_LINK_STATE = "link_state"
CAP_REACH = "reach"
CAP_CBR = "cbr"
CAP_DELAY = "delay"
CAP_STATEFUL = "stateful"
CAP_BATCH = "batch"
CAP_OUT_OF_PROCESS = "out_of_process"
CAP_TX_POWER = "tx_power"
CAP_PER_FRAME = "per_frame"
CAP_FROZEN = "frozen"

LOSS_ADDITIVE_LEGACY = "loss_composition:additive_legacy"
LOSS_INDEPENDENT_SURVIVAL = "loss_composition:independent_survival"
CAP_LEGACY_GLOBAL_RNG = "legacy_global_rng"

KNOWN_CAPABILITIES = frozenset({
    CAP_RSSI, CAP_LINK_STATE, CAP_REACH, CAP_CBR, CAP_DELAY, CAP_STATEFUL, CAP_BATCH,
    CAP_OUT_OF_PROCESS, CAP_TX_POWER, CAP_PER_FRAME, CAP_FROZEN,
    LOSS_ADDITIVE_LEGACY, LOSS_INDEPENDENT_SURVIVAL, CAP_LEGACY_GLOBAL_RNG,
})

#: Refused from anything that did not come out of the BUILT-IN registry. `disc` / `logdistance` draw
#: the packet-loss coin from the global `rng` (run.py:3393) and compose loss ADDITIVELY (a sum that
#: can exceed 1.0) -- exactly what the contract forbids. Rather than change it (which would move
#: 0bd93655...), they declare the grandfathering, the resolver refuses it from third parties, and the
#: manifest records it. A deliberate, documented wart with a scheduled close (roadmap phase 6).
RESERVED_CAPABILITIES = frozenset({CAP_LEGACY_GLOBAL_RNG, LOSS_ADDITIVE_LEGACY})

#: The closed link-state vocabulary (conformance check C7).
LINK_STATES = frozenset({"LOS", "NLOSv", "NLOSb"})

#: THE published `rssi_dbm` bounds. ONE definition, imported by `conformance.v1.channel` rather than
#: restated there: `check_outcome` is C7's runtime form, and a runtime gate whose bounds are wider
#: than the check's is not that check -- it is a second, laxer contract with the same name. (They
#: were [-200, 50] here and [-140, 0] in C7 until 2026-08-31.) Wider than physically usual on
#: purpose -- a 33 dBm ETSI-cap transmitter at a few metres is legitimately close to 0 dBm -- but
#: closed, so a model returning a linear-scale watt figure through a field documented as dBm is
#: caught immediately.
RSSI_MIN_DBM = -140.0
RSSI_MAX_DBM = 0.0


# --------------------------------------------------------------------------- #
# The message vocabulary. This -- not a class signature -- is the ABI.
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class StationSnapshot:
    """One station's TRUE physical state for this step.

    ORACLE-side BY NECESSITY: channel physics must not be steerable by a position-falsifying
    attacker (run.py:3281-3282, `RxChannel.java:11-17`). A ChannelModel is therefore explicitly
    INSIDE the oracle boundary and MUST NOT be handed to, or be able to write into, the detection
    layer except through the declared :class:`LinkOutcome` fields.
    """
    vid: int                      # rotation-stable TRUE id; the persistent-state key
    x: float                      # true position, local metres
    y: float
    ant_h_m: float                # V2X_ANTENNA_HEIGHT_M or RSU_ANTENNA_HEIGHT_M
    blocker_h_m: float            # TR37885_BLOCKER_HEIGHT_M[veh_type]; 0.0 if not a blocker
    is_rsu: bool = False
    is_vru: bool = False
    tx_power_dbm: Optional[float] = None   # None => model default (the seam TPC/DCC needs)
    rx_range_m: float = 0.0       # this receiver's own configured range; 0.0 => model default


@dataclass(frozen=True, slots=True)
class Transmission:
    """One PDU offered to the channel this step."""
    tx_index: int                 # index into the step's broadcast list; the canonical sort key
    tx_vid: int
    msg_type: str                 # "cam" | "denm" | "vam" | ...
    msg_count: int                # burst multiplier
    wire_size_bytes: int          # feeds airtime -> CBR
    cert_digest: str = ""         # the pseudonym this PDU was sent under (HashedId8 hex)
    priority: int = 3             # EN 302 663 access category / user priority
    channel_id: str = "CCH"


@dataclass(frozen=True, slots=True)
class StepFrame:
    """Everything the channel is told about this step.

    Deterministically ordered: `transmissions` is in tx_index order, `receivers` in the engine's
    receiver order. `stations` may be a LAZY mapping -- the engine does not materialise station
    snapshots for a model that never asks for them, so the default `disc` path pays one object
    allocation per step and nothing else.
    """
    step: int
    t: float
    dt: float
    stations: Mapping[int, StationSnapshot]
    transmissions: Sequence[Transmission]
    receivers: Sequence[int]
    weather_loss: float           # WEATHER_RADIO_LOSS drop probability (run.py:138)
    env: Mapping[str, object] = field(default_factory=dict)   # buildings, scenario events; read-only


@dataclass(frozen=True, slots=True)
class LinkOutcome:
    """Result for one (transmission, receiver) pair the model says was DELIVERED.

    Undelivered links are simply ABSENT -- never emitted with `heard=False`.
    """
    tx_index: int = UNBOUND
    rx_vid: int = UNBOUND
    rssi_dbm: Optional[float] = None    # faded received power; MA-visible, gated column
    link_state: Optional[str] = None    # "LOS" | "NLOSv" | "NLOSb" | None
    delay_s: float = 0.0               # propagation + queueing; 0.0 == today's same-step delivery
    extras: Mapping[str, float] = ()   # namespaced -> `x_<plugin_id>_<key>` columns only

    def bind(self, tx_index: int, rx_vid: int) -> "LinkOutcome":
        """Return this outcome with its (tx_index, rx_vid) filled in (per-link path -> batch ABI)."""
        if self.tx_index == tx_index and self.rx_vid == rx_vid:
            return self
        return LinkOutcome(tx_index, rx_vid, self.rssi_dbm, self.link_state,
                           self.delay_s, self.extras)


#: A model with no per-link metadata (disc / logdistance) returns this shared, frozen constant
#: instead of allocating one object per delivered link.
DELIVERED = LinkOutcome()


# --------------------------------------------------------------------------- #
# Protocols. Structural on purpose: the implementer needs no import of ours, which is the point.
# Note honestly that NEITHER Protocol NOR ABC enforces the ABI -- `@runtime_checkable isinstance`
# checks member PRESENCE only, never signatures. `api.registry._check_signature` does the real work.
# --------------------------------------------------------------------------- #
@runtime_checkable
class BatchChannelModel(Protocol):
    """THE canonical channel ABI. One exchange per step."""

    interface_version: str        # must satisfy "ChannelModel/1.x"
    plugin_id: str                # reserved RNG/config/column namespace, [a-z0-9_]{2,32}
    reach_m: float                # run-scalar upper bound on delivery distance. MANDATORY.

    def capabilities(self) -> frozenset: ...

    def begin_step(self, frame: StepFrame) -> None:
        """Advance any per-step state EXACTLY ONCE."""

    def deliver(self, frame: StepFrame,
                candidates: Sequence[tuple]) -> Iterable[LinkOutcome]:
        """`candidates` = (tx_index, rx_vid, distance_m), pre-filtered by the window and supplied in
        canonical order (tx_index ascending within rx_vid ascending). Return outcomes in ANY order;
        the engine re-sorts by (rx_vid, tx_index) before use, so an out-of-process backend's
        internal ordering can never affect the digest."""

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float: ...

    def close(self) -> None:
        """Release subprocess/socket resources. Always called, including on the SIGINT path."""


@runtime_checkable
class LinkChannelModel(Protocol):
    """Convenience shape for a pure in-process analytic model -- literally `GeometricChannel`'s
    existing surface. Wrapped by :class:`PerLinkAdapter` into a :class:`BatchChannelModel`."""

    interface_version: str
    plugin_id: str
    reach_m: float

    def capabilities(self) -> frozenset: ...
    def begin_step(self, frame: StepFrame) -> None: ...
    def evaluate(self, tx: StationSnapshot, rx: StationSnapshot,
                 d_m: float, txn: Transmission) -> Optional[LinkOutcome]:
        """None == not delivered."""
    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float: ...
    def collision_loss(self, dist_m: float, cbr: float) -> float: ...


# --------------------------------------------------------------------------- #
# Optional convenience base classes carrying default implementations, for authors who prefer
# inheritance. Purely additive: a Protocol implementer never needs these.
# --------------------------------------------------------------------------- #
class LinkChannelModelBase:
    """Defaults for `reach_m_for`, `window_m`, `capabilities`, a no-op `begin_step`, zero CBR and
    zero collision loss. Subclasses need only `plugin_id`, `reach_m` and `evaluate`."""

    interface_version: str = INTERFACE_VERSION
    plugin_id: str = "unnamed"
    reach_m: float = 0.0

    def capabilities(self) -> frozenset:
        return frozenset({CAP_REACH, LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame: StepFrame) -> None:
        return None

    def reach_m_for(self, rx: StationSnapshot) -> float:
        """Declared DELIVERY reach for this receiver -- what `acceptanceRangeThreshold` bounds on."""
        return self.reach_m

    def window_m(self, rx: StationSnapshot) -> float:
        """Candidate-SEARCH cutoff for this receiver. Must be >= reach_m_for(rx)."""
        return self.reach_m_for(rx)

    def evaluate(self, tx: StationSnapshot, rx: StationSnapshot,
                 d_m: float, txn: Transmission) -> Optional[LinkOutcome]:
        raise NotImplementedError

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        return 0.0

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        return 0.0

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        """Uniform [0,1) from the MODEL's OWN stream, drawn once per offered PDU when the engine's
        composed survival probability is < 1. Only consulted under `independent_survival`."""
        raise NotImplementedError(f"{type(self).__name__} declares independent_survival but does "
                                  f"not implement delivery_coin()")

    def prune(self, live_vids) -> None:
        """Drop persistent per-link state for stations that are gone (optional)."""
        return None

    def close(self) -> None:
        return None


def reach_of(model, rx: StationSnapshot) -> float:
    """Per-receiver DELIVERY reach, falling back to the run scalar for a model that has no opinion."""
    fn = getattr(model, "reach_m_for", None)
    return float(fn(rx)) if fn is not None else float(model.reach_m)


def window_of(model, rx: StationSnapshot) -> float:
    """Per-receiver candidate-SEARCH cutoff, falling back to the delivery reach."""
    fn = getattr(model, "window_m", None)
    return float(fn(rx)) if fn is not None else reach_of(model, rx)


def loss_composition(model) -> str:
    """"additive_legacy" | "independent_survival", read off the declared capabilities."""
    caps = model.capabilities()
    if LOSS_ADDITIVE_LEGACY in caps:
        return "additive_legacy"
    return "independent_survival"


class PerLinkAdapter:
    """Wraps a :class:`LinkChannelModel` into the canonical :class:`BatchChannelModel`.

    The in-process engine loop calls :meth:`evaluate_link` at the existing point in the existing
    loop, so `cand.sort()` (run.py:3325), the index-parallel outcome list and the `in_range` order
    are all untouched -- which is what makes the migration CODE MOTION ONLY (section 8.1).
    :meth:`deliver` is the same computation expressed as the per-step exchange, and
    `tests/test_plugin_api.py` asserts the two agree link-for-link, so the phase-4 switch to a
    batch-consuming loop is provable rather than hopeful.
    """

    #: `evaluate_link` is BOUND AT CONSTRUCTION rather than defined as a forwarding method: the
    #: engine calls it once per candidate link (~10^7 times on a mid-size run), and one fewer
    #: Python frame per link is the difference between a refactor that is free and one that is not.
    __slots__ = ("model", "evaluate_link", "rng_ns", "validate", "_evaluate", "_begin_step",
                 "_busy", "_collision", "_coin")

    def __init__(self, model, rng_ns=None, validate: bool = False, guard_label=None):
        self.model = model
        #: `validate` is what turns `check_outcome` from a function nobody calls into the engine's
        #: outcome gate. On for third-party models, off for built-ins (the goldens grade those, and
        #: this is a per-delivered-link call on a ~10^7-link loop).
        self.validate = bool(validate)
        #: EVERY model-facing entry point the engine calls goes through
        #: :func:`~scms_sim_ref.api.guard.guarded` when `guard_label` is set, and `guarded(fn, None)`
        #: returns `fn` itself -- so a built-in pays not even a wrapper frame, and a third party pays
        #: ~100 ns per call for a runtime refusal of `sys._getframe` and the other reflective routes
        #: into this loop's frame. A gap in this bracket IS a hole: a model whose `delivery_coin`
        #: could walk the stack while its `evaluate` could not would be no protection at all.
        self._evaluate = _bind_guarded(model, "evaluate", guard_label)
        self._begin_step = _bind_guarded(model, "begin_step", guard_label)
        self._busy = _bind_guarded(model, "channel_busy_ratio", guard_label)
        self._collision = _bind_guarded(model, "collision_loss", guard_label)
        self._coin = _bind_guarded(model, "delivery_coin", guard_label)
        self.evaluate_link = (_checked_evaluate(self._evaluate) if validate else self._evaluate)
        #: The model's `RngNamespace`, so :meth:`begin_step` can advance it. See
        #: :meth:`BatchAdapter.begin_step` for why the ADAPTER owns that responsibility.
        self.rng_ns = rng_ns

    # -- BatchChannelModel surface ------------------------------------------------------------ #
    @property
    def interface_version(self) -> str:
        return self.model.interface_version

    @property
    def plugin_id(self) -> str:
        return self.model.plugin_id

    @property
    def reach_m(self) -> float:
        return float(self.model.reach_m)

    def capabilities(self) -> frozenset:
        return frozenset(self.model.capabilities()) | {CAP_BATCH}

    def begin_step(self, frame: StepFrame) -> None:
        if self.rng_ns is not None:
            self.rng_ns.begin_step(frame.step)
        self._begin_step(frame)

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        return self._busy(rx_vid, offered)

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        return self._collision(dist_m, cbr)

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        return self._coin(tx_vid, rx_vid)

    def close(self) -> None:
        close = getattr(self.model, "close", None)
        if close is not None:
            close()

    def deliver(self, frame: StepFrame, candidates: Sequence[tuple]) -> Iterable[LinkOutcome]:
        stations = frame.stations
        txns = frame.transmissions
        out = []
        for tx_index, rx_vid, d_m in candidates:
            txn = txns[tx_index]
            tx = stations[txn.tx_vid]
            rx = stations[rx_vid]
            if d_m > window_of(self.model, rx):
                continue
            res = self._evaluate(tx, rx, d_m, txn)
            if res is not None:
                # VALIDATE FIRST, THEN BIND. `check_outcome` returns a fresh `LinkOutcome` built from
                # values read exactly once, so `bind` below is OUR method on OUR object -- a model
                # cannot supply its own `bind` (or its own stateful `rssi_dbm`) and have the engine
                # use the result. Binding first and validating the return of the model's `bind` is
                # what left the time-of-check/time-of-use gap open.
                out.append(check_outcome(res).bind(tx_index, rx_vid) if self.validate
                           else res.bind(tx_index, rx_vid))
        return out

    # -- in-process fast path (`evaluate_link` is bound in __init__) ---------------------------- #
    def reach_m_for(self, rx: StationSnapshot) -> float:
        return reach_of(self.model, rx)

    def window_m(self, rx: StationSnapshot) -> float:
        return window_of(self.model, rx)

    def prune(self, live_vids) -> None:
        fn = getattr(self.model, "prune", None)
        if fn is not None:
            fn(live_vids)


class BatchAdapter:
    """Thin facade over a model that already implements the canonical per-step ABI.

    It supplies only the OPTIONAL parts of the engine-facing surface a `BatchChannelModel` need not
    define (`reach_m_for`, `window_m`, `collision_loss`, `prune`), so the engine has one shape to
    talk to whichever side of the ABI a model implements. It deliberately has NO `evaluate_link`:
    that absence is how the engine knows to drive `deliver`.
    """

    __slots__ = ("model", "rng_ns", "validate", "_deliver", "_begin_step", "_busy", "_collision",
                 "_coin")

    def __init__(self, model, rng_ns=None, validate: bool = False, guard_label=None):
        self.model = model
        self.rng_ns = rng_ns
        self.validate = bool(validate)
        # See `PerLinkAdapter.__init__`: every model-facing entry point, or none of them.
        self._deliver = _bind_guarded(model, "deliver", guard_label)
        self._begin_step = _bind_guarded(model, "begin_step", guard_label)
        # `channel_busy_ratio` and `collision_loss` are OPTIONAL on the batch ABI (a model that
        # models neither simply has none), so the zero default is kept here rather than in the
        # caller -- same values as before, one fewer `getattr` per link.
        self._busy = _bind_guarded(model, "channel_busy_ratio", guard_label, _zero2)
        self._collision = _bind_guarded(model, "collision_loss", guard_label, _zero2)
        self._coin = _bind_guarded(model, "delivery_coin", guard_label)

    @property
    def interface_version(self) -> str:
        return self.model.interface_version

    @property
    def plugin_id(self) -> str:
        return self.model.plugin_id

    @property
    def reach_m(self) -> float:
        return float(self.model.reach_m)

    def capabilities(self) -> frozenset:
        return frozenset(self.model.capabilities()) | {CAP_BATCH}

    def begin_step(self, frame: StepFrame) -> None:
        # THE ADAPTER advances the RngNamespace, not the engine and not the model. `stream()` keys on
        # `RngNamespace._step`, and that field is only ever moved by `RngNamespace.begin_step`. Wiring
        # it here -- one place, on the object every driver of a model already calls `begin_step` on
        # (the engine loop, `conformance.v1.harness.trace`, C4 and C8) -- is what makes the documented
        # "pure function of (seed, replicate, plugin, label, ids, step)" true. It was previously
        # called by NOBODY: `_step` stayed -1 for a whole run, every `stream()` key ended `:s-1`, and
        # the advertised per-packet fade of a stateless model was a fixed per-link constant for the
        # entire run. The plugin must never call it (that would let a model rewrite its own key
        # space mid-step), which is why it is not on the model-facing surface.
        if self.rng_ns is not None:
            self.rng_ns.begin_step(frame.step)
        self._begin_step(frame)

    def deliver(self, frame: StepFrame, candidates: Sequence[tuple]) -> Iterable[LinkOutcome]:
        outs = self._deliver(frame, candidates)
        return [check_outcome(o) for o in outs] if self.validate else outs

    def reach_m_for(self, rx: StationSnapshot) -> float:
        return reach_of(self.model, rx)

    def window_m(self, rx: StationSnapshot) -> float:
        return window_of(self.model, rx)

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        return float(self._busy(rx_vid, offered))

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        return float(self._collision(dist_m, cbr))

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        return self._coin(tx_vid, rx_vid)

    def prune(self, live_vids) -> None:
        fn = getattr(self.model, "prune", None)
        if fn is not None:
            fn(live_vids)

    def close(self) -> None:
        fn = getattr(self.model, "close", None)
        if fn is not None:
            fn()


def sort_outcomes(outcomes: Iterable[LinkOutcome]) -> list:
    """Canonical order: (rx_vid, tx_index). Applied to whatever a backend returns, so its internal
    ordering is structurally irrelevant to the digest."""
    return sorted(outcomes, key=lambda o: (o.rx_vid, o.tx_index))


def check_outcome(o) -> LinkOutcome:
    """Conformance check C7's runtime form: ranges, closed vocabulary, no NaN/inf.

    Called by the engine on EVERY delivered link of a THIRD-PARTY model (see
    :class:`PerLinkAdapter`'s `validate` flag). It used to be called by nobody, which meant the only
    outcome validation in the tree lived in a conformance suite that is off by default: a plugin
    returning `LinkOutcome(rssi_dbm=9999.0, link_state="TELEPATHY")` on every link completed at exit
    0 with a valid manifest, and every row of `ma/ma_reports.jsonl` carried `"rssi_dbm": 9999.0`
    (+9999 dBm is about 10^997 W) into the MA-visible dataset.

    **IT RETURNS A COPY, AND THAT IS THE POINT.** This function used to validate by READING the
    caller's object and then hand the SAME OBJECT back, which is a textbook time-of-check /
    time-of-use gap: nothing obliges a plugin to return a `LinkOutcome` at all, and an object whose
    `rssi_dbm` is a stateful ``property`` returns one value to the checker and a different one to the
    engine a moment later. Measured before this change, on the per-link path: a model returning an
    object whose first read of `rssi_dbm` is -70.0 and every subsequent read is 9999.0 passed the
    range check and put **9999.0** into `ma/ma_reports.jsonl`. The same shape defeats the link-state
    vocabulary and the delay bound.

    So every field is read EXACTLY ONCE into a local, the LOCALS are what get validated, and a fresh
    `LinkOutcome` -- the base class, built here, with primitives coerced to `int` / `float` /
    plain `str` -- is what the engine goes on to use. A plugin cannot influence the values after the
    check because it no longer holds the object the engine reads. `extras` is copied into a
    `MappingProxyType` over a plain dict of coerced floats for the same reason.

    Built-ins skip it: they are graded by the pinned goldens, and this is a per-delivered-link call
    on a ~10^7-link loop.
    """
    from .errors import ConfigError
    # ONE read per field. Everything below validates and returns these LOCALS, never `o` again.
    tx_index = getattr(o, "tx_index", UNBOUND)
    rx_vid = getattr(o, "rx_vid", UNBOUND)
    rssi, state, delay, extras = o.rssi_dbm, o.link_state, o.delay_s, o.extras
    if rssi is not None:
        rssi = float(rssi)
        if not math.isfinite(rssi) or not (RSSI_MIN_DBM <= rssi <= RSSI_MAX_DBM):
            raise ConfigError(
                f"LinkOutcome.rssi_dbm out of range: {rssi!r} (tx {tx_index} -> rx "
                f"{rx_vid}); the declared bound is [{RSSI_MIN_DBM}, {RSSI_MAX_DBM}] dBm. A value "
                f"outside it is normally a UNITS bug -- linear milliwatts through a field documented "
                f"as dBm -- and it reaches the MA-visible dataset as evidence.")
    if state is not None:
        state = str(state)                      # a `str` SUBCLASS with a custom __eq__ is not a state
        if state not in LINK_STATES:
            raise ConfigError(f"LinkOutcome.link_state {state!r} not in {sorted(LINK_STATES)} "
                              f"(tx {tx_index} -> rx {rx_vid}); the vocabulary is CLOSED")
    delay = float(delay)
    if not math.isfinite(delay) or delay < 0.0:
        raise ConfigError(f"LinkOutcome.delay_s must be finite and >= 0 (got {delay!r})")
    if extras:
        items = extras.items() if hasattr(extras, "items") else extras
        copied = {}
        for k, v in items:
            fv = float(v)
            if not math.isfinite(fv):
                raise ConfigError(f"LinkOutcome.extras[{k!r}] = {v!r} is not finite")
            copied[str(k)] = fv
        extras = _MappingProxyType(copied)
    else:
        extras = ()
    return LinkOutcome(int(tx_index), int(rx_vid), rssi, state, delay, extras)


def _checked_evaluate(evaluate):
    """`model.evaluate` with :func:`check_outcome` on every delivered link.

    The per-link path hands the model no `tx_index`/`rx_vid` (the adapter binds them afterwards), so
    the outcome's own identity fields are the `UNBOUND` sentinel and would make the message read
    "tx -1 -> rx -1". The link is re-identified here from the arguments the model actually got.
    """
    from .errors import ConfigError

    def evaluate_link(tx, rx, d_m, txn):
        out = evaluate(tx, rx, d_m, txn)
        if out is None:
            return None
        try:
            return check_outcome(out)
        except ConfigError as e:
            raise ConfigError(f"{e} [link: tx vid {txn.tx_vid} -> rx vid {rx.vid}, "
                              f"d = {float(d_m):.1f} m]") from None
    return evaluate_link
