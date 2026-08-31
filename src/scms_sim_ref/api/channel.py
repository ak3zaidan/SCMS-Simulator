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
from typing import Iterable, Mapping, Optional, Protocol, Sequence, runtime_checkable

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
    __slots__ = ("model", "evaluate_link")

    def __init__(self, model):
        self.model = model
        self.evaluate_link = model.evaluate

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
        self.model.begin_step(frame)

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        return self.model.channel_busy_ratio(rx_vid, offered)

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        return self.model.collision_loss(dist_m, cbr)

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        return self.model.delivery_coin(tx_vid, rx_vid)

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
            res = self.model.evaluate(tx, rx, d_m, txn)
            if res is not None:
                out.append(res.bind(tx_index, rx_vid))
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

    __slots__ = ("model",)

    def __init__(self, model):
        self.model = model

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
        self.model.begin_step(frame)

    def deliver(self, frame: StepFrame, candidates: Sequence[tuple]) -> Iterable[LinkOutcome]:
        return self.model.deliver(frame, candidates)

    def reach_m_for(self, rx: StationSnapshot) -> float:
        return reach_of(self.model, rx)

    def window_m(self, rx: StationSnapshot) -> float:
        return window_of(self.model, rx)

    def channel_busy_ratio(self, rx_vid: int, offered: float) -> float:
        fn = getattr(self.model, "channel_busy_ratio", None)
        return float(fn(rx_vid, offered)) if fn is not None else 0.0

    def collision_loss(self, dist_m: float, cbr: float) -> float:
        fn = getattr(self.model, "collision_loss", None)
        return float(fn(dist_m, cbr)) if fn is not None else 0.0

    def delivery_coin(self, tx_vid: int, rx_vid: int) -> float:
        return self.model.delivery_coin(tx_vid, rx_vid)

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


def check_outcome(o: LinkOutcome) -> LinkOutcome:
    """Conformance check C7's runtime form: ranges, closed vocabulary, no NaN/inf."""
    from .errors import ConfigError
    if o.rssi_dbm is not None:
        r = float(o.rssi_dbm)
        if not math.isfinite(r) or not (-200.0 <= r <= 50.0):
            raise ConfigError(f"LinkOutcome.rssi_dbm out of range: {o.rssi_dbm!r}")
    if o.link_state is not None and o.link_state not in LINK_STATES:
        raise ConfigError(f"LinkOutcome.link_state {o.link_state!r} not in {sorted(LINK_STATES)}")
    if not math.isfinite(o.delay_s) or o.delay_s < 0.0:
        raise ConfigError(f"LinkOutcome.delay_s must be finite and >= 0 (got {o.delay_s!r})")
    return o
