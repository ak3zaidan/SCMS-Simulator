"""The detector contract (PLUGIN-ARCHITECTURE.md section 2.2) -- the user's thresholding case.

**Two layers, copied from F2MD's decomposition, with two deliberate deviations.**

F2MD splits *checks* (`mdChecks/{Legacy,CaTCh,Experi}Checks`, producing a `BsmCheck` -- a flat
struct of named doubles) from *applications* (`mdApplications/MDApplication.h`, one pure virtual
`bool CheckNodeForReport(pseudonym, bsm, bsmCheck, nodeTable)`, with `ThresholdApp`,
`BehavioralApp` and `MachineLearningApp` as peer subclasses). That split is exactly right and this
repo welded both together at the reception site. We copy the SPLIT; we do not copy the SELECTION
(an integer-indexed enum plus a `switch` -- the anti-pattern the registry replaced).

The two deviations, both load-bearing:

* **POLARITY IS INVERTED, and the contract states it unambiguously.** F2MD factors are ``[0, 1]``
  where **LOW means implausible** (`ThresholdApp` fires on ``getX() <= Threshold``). **This repo's
  ``detnorm`` is the other way round: ``>= 1.0`` means VIOLATING**, ``0.0`` means perfectly
  consistent, and the value is a *confidence-normalised residual* -- roughly ``1.0`` at the firing
  threshold and unbounded above. Any check ported from F2MD must be inverted. Conformance check
  ``D4`` asserts the convention; :func:`fires` is the single definition of "violating".
* **We do NOT copy F2MD's detector context.** `LegacyChecks.h` holds
  ``std::unordered_map<LAddress::L2Type, veins::Coord>* realDynamicMap`` -- a pointer to the
  GROUND-TRUTH positions of every node, used by its `PositionPlausibilityCheck`. A detector *handed*
  that reads the labels without deciding to, which makes every dataset it grades worthless.
  :class:`Observation` is modelled on `SignedCam.java:22-31` instead: no oracle field exists on it,
  so the leakage cannot be accidental.

**Read :class:`Observation`'s docstring and `api/srcgate.py` before calling any of this a firewall.**
An in-process plugin runs with the interpreter's full reflective powers, `sys._getframe` included,
and cannot be sandboxed. What this module provides is a boundary that makes leakage DELIBERATE and
detectable, not impossible; `docs/realism/DETECTOR-PLUGIN.md` section 2 states the vector, the
residues and the out-of-process answer in full.

**Report FORMAT is a third, separate seam** (`report_format`), exactly as F2MD's `mdReport/`
selects `BasicCheckReport` / `EvidenceReport` / `OneMessageReport` independently of the detector.
"""
from __future__ import annotations

from dataclasses import dataclass, fields as _dc_fields
from typing import Mapping, MutableMapping, Optional, Protocol, Sequence, runtime_checkable

from .errors import ConfigError
from .fields import FieldSpec
from .rng import RESERVED_PREFIX, check_plugin_id

INTERFACE_NAME = "Detector"
INTERFACE_VERSION = "Detector/1.0"

#: Highest interface MINOR this engine understands. A plugin declaring `Detector/1.1` against a 1.0
#: engine is refused at load, not at step k > 0.
MAX_MINOR = 0

# --------------------------------------------------------------------------- #
# Polarity. One definition, imported everywhere, so the cliff can never be spelled two ways.
# --------------------------------------------------------------------------- #
#: A score at or above this is a VIOLATION. See the module docstring: this is the OPPOSITE of
#: F2MD's `[0,1]` LOW-means-implausible convention.
VIOLATION_THRESHOLD = 1.0


def fires(score: float) -> bool:
    """``True`` when `score` is a violation under this repo's polarity (``>= 1.0``)."""
    return score >= VIOLATION_THRESHOLD


# --------------------------------------------------------------------------- #
# Capabilities
# --------------------------------------------------------------------------- #
CAP_STATEFUL = "stateful"           # keeps per-(receiver, sender) state across messages
CAP_SOFT = "soft"                   # scored and emitted, never fires on its own (cf. SOFT_KEYS)
CAP_HISTORY = "history"             # reads obs.ref_* / obs.prev_* (the engine's per-sender history)
CAP_RSSI = "rssi"                   # reads obs.rssi_dbm / obs.link_state (gated on the channel)
CAP_MAP = "map"                     # reads obs.map_offroad_m (the receiver's own HD map)
CAP_NEIGHBOURHOOD = "neighbourhood"  # reads obs.neighbourhood (aggregate MA-visible context)
CAP_EVENT = "event"                 # scores DENM / event messages
CAP_LEGACY_GLOBAL_RNG = "legacy_global_rng"
CAP_LEGACY_RAW_COMPARE = "legacy_raw_compare"

KNOWN_CAPABILITIES = frozenset({
    CAP_STATEFUL, CAP_SOFT, CAP_HISTORY, CAP_RSSI, CAP_MAP, CAP_NEIGHBOURHOOD, CAP_EVENT,
    CAP_LEGACY_GLOBAL_RNG, CAP_LEGACY_RAW_COMPARE,
})

#: Refused from anything that did not come out of the BUILT-IN registry.
#:
#: * ``legacy_global_rng`` -- the built-in fusion draws the `report_prob` Bernoulli from the
#:   engine's single global stream, whose draw COUNT AND ORDER are load-bearing for every pinned
#:   golden. Exactly what D3 forbids; grandfathered, declared, and recorded in the manifest rather
#:   than silently changed.
#: * ``legacy_raw_compare`` -- section 4.1 requires the engine to round a plugin's returned score to
#:   the plugin's declared `precision` BEFORE the ``>= 1.0`` compare, because that comparison is a
#:   CLIFF and a last-ulp difference flips a whole report. The built-ins compare the RAW float
#:   (that is what the goldens were pinned on); rounding them first would move `0bd93655...`. Third
#:   parties get the rounded compare, which is the behaviour the design specifies.
RESERVED_CAPABILITIES = frozenset({CAP_LEGACY_GLOBAL_RNG, CAP_LEGACY_RAW_COMPARE})


# --------------------------------------------------------------------------- #
# THE FIREWALL -- and exactly how far it reaches
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class Observation:
    """One received message as the RECEIVER can measure it. Sealed, slotted, MA-visible ONLY.

    **This is the data boundary.** It is built at the reception site from the engine's broadcast dict
    but WITHOUT `veh` (a whole `Vehicle`, carrying `.is_attacker` / `.attack_type` / `.victims`),
    WITHOUT the sender's TRUE `x` / `y`, and without `falsified`, `ghost`, `tspd` or `thdg`.

    **What that is, stated precisely, because the unqualified version of the claim is false.**
    An in-process Python plugin runs with the interpreter's full reflective powers and CANNOT be
    sandboxed; `sys._getframe(1).f_locals` inside `evaluate()` reaches the reception loop's own
    locals, this DTO or no. What this class guarantees is narrower and still worth having:

    * **No ground truth is reachable BY NAME through this object.** There is no oracle attribute to
      read, no ``__dict__`` to enumerate (``slots=True``), and no additional attribute can be
      attached to it at runtime. Accidental leakage -- the F2MD `realDynamicMap` shape, where the
      truth is simply *there* on the context object and gets used without anyone deciding to -- is
      impossible.
    * **It is genuinely read-only under every write path CPython exposes by name.**
      ``obs.claimed_x = v`` raises `FrozenInstanceError`; ``setattr(obs, "claimed_x", v)`` raises;
      and -- the case `frozen=True` alone does NOT cover, because `frozen` only overrides
      ``__setattr__`` -- ``object.__setattr__(obs, "claimed_x", v)`` now raises `AttributeError`
      too. The public field names are read-only *data descriptors* (see :func:`_seal`), so the
      generic C-level setattr refuses them before it ever looks at ``__setattr__``. That matters
      because ONE Observation is shared by every check in the vector: without it, the first plugin
      in the array could silently rewrite the claim the built-ins and every later plugin then score.
    * **What it does NOT guarantee.** The underlying slot descriptor is still reachable at
      ``type(obs).claimed_x.fget.__self__``, and calling its ``__set__`` writes. That is three
      deliberate steps, it appears in no honest detector, and it is exactly what the source gate in
      `api/srcgate.py` and the trust model in `docs/realism/DETECTOR-PLUGIN.md` are about: the
      boundary makes leakage and tampering DELIBERATE and DETECTABLE, it does not make them
      impossible. Genuinely untrusted detector code needs the out-of-process mode, not this class.

    Every field below is justified by what a real ITS-station receiver physically holds:

    * the **claimed** fields are the contents of the signed CAM/DENM it just decoded;
    * the **receiver-side** fields are its own GNSS fix, its own radio's declared reach, and its own
      PHY's measurement of the frame (`rssi_dbm`, `link_state`);
    * the **history** fields are earlier claims *from this same certificate to this same receiver* --
      the per-sender history the engine already maintains in `last_claimed[(rx, digest)]["h"]`;
    * `map_offroad_m` is the receiver's own HD map evaluated at the CLAIMED position -- a public
      static artifact applied to a transmitted value, revealing nothing about the truth;
    * `neighbourhood` is aggregate MA-visible context, never per-vehicle truth.

    Nothing here is derived from the sender's true state. The one field that is *computed* from true
    geometry, `rssi_dbm`, is a REVIEWED EXCEPTION documented in `datagen/leakage_linter.py`: a real
    PHY measures received power for every frame it decodes, so the MA legitimately holds it -- and
    it is precisely *because* it tracks true geometry rather than the claim that it is a detector
    input at all.
    """

    # -- the sender, as the MA sees it ------------------------------------------------------- #
    cert_digest: str              # HashedId8 hex; NOT rotation-stable, never a state key
    station_type: Optional[str]   # "vehicle" | "vru", SELF-DECLARED on the beacon (not the oracle)
    claimed_x: float
    claimed_y: float
    claimed_speed: float
    claimed_heading: float        # deg CCW from East (engine convention)
    pos_conf: float               # broadcast 95% position-uncertainty radius, in metres
    gen_time: float               # the claim's own generationTime
    msg_count: int                # beacons this sender offered this step (burst multiplier)
    msg_type: str                 # "cam" | "denm"
    event_type: Optional[str]     # DENM cause-code name; None for a CAM
    sig_ok: bool                  # did the signature verify
    cert_valid_from: float
    cert_valid_to: float

    # -- receiver-side, measured ------------------------------------------------------------- #
    rx_x: float                   # the receiver's own GNSS fix
    rx_y: float
    rx_reach_m: float             # the channel model's DECLARED delivery reach for this receiver
    rssi_dbm: Optional[float]     # this frame's received power, or None if the model has no PHY
    link_state: Optional[str]     # "LOS" | "NLOSv" | "NLOSb" | None
    t: float
    dt: float

    # -- per-sender history this receiver already holds -------------------------------------- #
    first_sight: bool             # True when this is the first message from this cert at this rx
    ref_x: float                  # the LAGGED reference fix (newest at least detector_lag_s old);
    ref_y: float                  # equal to the current claim when first_sight
    ref_speed: float
    ref_heading: float
    ref_t: float
    prev_x: float                 # the most recent prior fix (a ONE-STEP baseline)
    prev_y: float
    prev_speed: float
    prev_heading: float
    prev_t: float

    # -- receiver-side derived / aggregate context ------------------------------------------- #
    map_offroad_m: float          # distance from the CLAIMED position to the nearest road (HD map)
    neighbourhood: Mapping[str, float]   # e.g. {"cell_cert_count": 4.0, "cbr": 0.31}


#: Declared field names of :class:`Observation`, in declaration order. The constructor's positional
#: order, and the order :func:`_seal` rebuilds `__init__` in.
OBSERVATION_FIELD_ORDER = tuple(f.name for f in _dc_fields(Observation))

#: Field names of :class:`Observation`, as a frozen set -- what conformance check D2 grades a
#: recording proxy's touched-attribute set against.
OBSERVATION_FIELDS = frozenset(OBSERVATION_FIELD_ORDER)


def _seal(cls, names: tuple) -> None:
    """Make every declared field of a frozen, slotted dataclass genuinely unwritable BY NAME.

    ``@dataclass(frozen=True)`` only overrides ``__setattr__``. `object.__setattr__` skips that
    override entirely and goes to the generic C-level setattr, which finds the slot's
    `member_descriptor` -- a *data* descriptor that happily writes. So the one line

        object.__setattr__(obs, "claimed_x", 0.0)

    silently mutated the observation every other check in the vector was about to score, and the
    class docstring said it could not. Measured, before this: it succeeded.

    The fix is to replace each public name's `member_descriptor` with a ``property`` carrying only a
    getter. Generic setattr finds the property first (it is also a data descriptor), sees no setter,
    and raises `AttributeError` -- so `setattr`, `object.__setattr__` and `object.__delattr__` all
    refuse, and the dataclass's own `FrozenInstanceError` still covers plain assignment.

    Two details that make this affordable rather than merely correct:

    * the getter is the *original* descriptor's own bound ``__get__``, a C method-wrapper, not a
      Python function. Measured on this host (CPython 3.12.10): 25.0 ns per read against 7.1 ns for
      a bare slot, where a Python-level descriptor costs 53.8 ns. The reception loop reads this
      object a few dozen times per delivered link, and that is the whole cost of the property.
    * the generated ``__init__`` writes through the *saved* descriptors, so construction never goes
      near the now-setter-less name. It is measurably faster than the dataclass's own frozen
      ``__init__`` (0.122 s vs 0.152 s per million two-field constructions), because
      ``md.__set__(self, v)`` skips the name lookup that `object.__setattr__(self, "x", v)` does.

    Honest about the residue: the saved descriptor is reachable from Python as
    ``type(obs).<field>.fget.__self__``. Nothing in an in-process interpreter can close that. The
    claim this function supports is "a write by name is refused", not "this object is immutable".
    """
    members = tuple(cls.__dict__[n] for n in names)     # capture BEFORE the names are replaced
    env = {f"_w{i}": md.__set__ for i, md in enumerate(members)}
    body = [f"def __init__(self, {', '.join(names)}):"]
    body += [f"    _w{i}(self, {n})" for i, n in enumerate(names)]
    # `@dataclass(frozen=True, slots=True)` also generates __getstate__/__setstate__ so the instance
    # pickles; the generated __setstate__ uses object.__setattr__ and would now raise. Route it
    # through the same constructor rather than leaving a pickle round-trip broken by the seal.
    body += ["def __setstate__(self, state):",
             "    __init__(self, *state)"]
    exec(compile("\n".join(body), f"<{cls.__name__} seal>", "exec"), env)   # noqa: S102
    cls.__init__ = env["__init__"]
    cls.__setstate__ = env["__setstate__"]
    for name, md in zip(names, members):
        setattr(cls, name, property(md.__get__, None, None, f"{name} (read-only)"))


_seal(Observation, OBSERVATION_FIELD_ORDER)


# --------------------------------------------------------------------------- #
# Per-(receiver, sender) state
# --------------------------------------------------------------------------- #
#: Keys of the engine's own per-link state dict. They belong to the BUILT-INS and are exposed to a
#: third-party check as a read-only view: `h` is the claim history, `streak` the consecutive-
#: violation counters the fusion owns, `touch` the prune bookkeeping, `kf` the built-in tracker.
RESERVED_STATE_KEYS = frozenset({"h", "streak", "touch", "kf"})


class NamespacedState(MutableMapping):
    """The per-(receiver, sender) state a THIRD-PARTY check or fusion sees (section 4.4).

    **Through the mapping interface** -- ``ns[k]``, ``ns[k] = v``, ``del ns[k]``, iteration,
    ``len``, ``in``, and everything `MutableMapping` derives from those -- reads and writes land in
    ``st["plugin:<id>"]``. The reserved keys are READABLE (a detector legitimately wants the claim
    history) but never writable, and never writable *through* the objects handed back either: `h`
    comes back as a tuple, `streak` as a read-only proxy over a COPY. Writing a reserved key raises
    rather than being silently dropped, because a plugin that believes it wrote to `streak` and did
    not is worse than one that crashed.

    **It is not a capability boundary, and the earlier wording here ("and nowhere else") claimed it
    was.** This object holds a direct reference to the engine's own per-link dict in ``self._st``,
    and ``object.__getattribute__(ns, "_st")`` hands it over unwrapped -- after which every reserved
    key is writable. That is not a bug to be patched with another wrapper: any in-process wrapper is
    one `__getattribute__` / `gc.get_referrers` / `__reduce__` away from the thing it wraps, and
    stacking more of them only makes the false claim harder to disprove.

    What this class actually buys, and what it is for:

    * a plugin that *follows the interface* cannot collide with the engine's keys or with another
      plugin's, so state namespacing is automatic rather than a convention nobody enforces;
    * a plugin that reaches for `streak` or `kf` by accident gets a loud `ConfigError` naming the
      key, at the first message, instead of corrupting the fusion's counters silently;
    * a plugin that reaches around it has to write a line that means nothing else, which is what
      makes the reach reviewable, gate-able (`api/srcgate.py`) and non-accidental.

    The honest boundary for state that a plugin must not be able to touch is a process boundary.
    See `docs/realism/DETECTOR-PLUGIN.md` section 2.
    """

    __slots__ = ("_st", "_ns", "_own")

    def __init__(self, st: MutableMapping, plugin_id: str):
        self._st = st
        self._ns = f"{RESERVED_PREFIX}:{plugin_id}"
        own = st.get(self._ns)
        if own is None:
            own = st[self._ns] = {}
        self._own = own

    def __getitem__(self, key):
        if key in RESERVED_STATE_KEYS:
            return _read_only(self._st.get(key))
        return self._own[key]

    def __setitem__(self, key, value):
        if key in RESERVED_STATE_KEYS:
            raise ConfigError(f"state key {key!r} is reserved for the built-in detectors; a plugin "
                              f"writes only inside its own {self._ns!r} namespace")
        self._own[key] = value

    def __delitem__(self, key):
        if key in RESERVED_STATE_KEYS:
            raise ConfigError(f"state key {key!r} is reserved for the built-in detectors")
        del self._own[key]

    def __iter__(self):
        return iter(self._own)

    def __len__(self):
        return len(self._own)

    def __contains__(self, key):
        return key in self._own or (key in RESERVED_STATE_KEYS and key in self._st)

    def __repr__(self):                                    # pragma: no cover - debugging aid
        return f"NamespacedState({self._ns!r}, keys={sorted(self._own)})"


def _read_only(value):
    """A reserved value, in a form the plugin cannot write through."""
    if isinstance(value, list):
        return tuple(value)
    if isinstance(value, dict):
        import types
        return types.MappingProxyType(dict(value))
    return value


# --------------------------------------------------------------------------- #
# LAYER 1 -- the checks
# --------------------------------------------------------------------------- #
@runtime_checkable
class Check(Protocol):
    """Scores ONE observation. **The user's threshold detector is one of these.**

    Contract, in full:

    * ``evaluate`` returns a **detnorm**: ``>= 1.0`` is VIOLATING (the opposite of F2MD). ``0.0`` is
      perfectly consistent. The value should be normalised so that ``1.0`` is the firing point,
      which is what makes a vector of them comparable and fusible.
    * It MUST be a pure function of ``(obs, state, params)`` plus draws from ``rng``.
    * It MUST NOT import or touch the engine's global `random.Random(cfg.seed)`. It is not handed
      it -- but "not handed it" is not "cannot reach it": the reception loop's own `rng` local is
      one ``sys._getframe(1).f_locals["rng"]`` away, which is measured and stated in
      `docs/realism/DETECTOR-PLUGIN.md` rather than left for a reader to discover. This is a RULE
      the source gate raises the bar on, not an impossibility.
    * It MUST NOT write outside ``state["plugin:<id>"]``. :class:`NamespacedState` routes the
      mapping interface there and refuses the reserved keys; read its docstring for what that does
      and does not guarantee.
    * It MUST return a finite float. A NaN or an infinity is a load-bearing arithmetic bug in a
      cliff comparison, and the engine refuses it.
    """

    interface_version: str        # "Detector/1.x"
    plugin_id: str                # reserved RNG / config / column namespace, [a-z0-9_]{2,32}
    reason_code: str              # emitted as detnorm_x_<plugin_id>_<reason_code>
    soft: bool                    # True => scored and emitted, never fires (cf. SOFT_KEYS)
    precision: int                # decimals the engine rounds to BEFORE the >= 1.0 compare
    msg_types: tuple              # ("cam",) | ("denm",) | ("cam", "denm")

    def config_fields(self) -> Mapping[str, FieldSpec]: ...

    def capabilities(self) -> frozenset: ...

    def evaluate(self, obs: Observation, state: MutableMapping, params: Mapping,
                 rng) -> float: ...


class CheckBase:
    """Optional convenience base carrying the defaults, for authors who prefer inheritance.

    A `Protocol` is the published contract precisely so an implementer needs no import of ours --
    but neither `Protocol` nor `ABC` checks signatures at runtime, which is why the registry ships a
    load-time `inspect.signature` validator. This class exists only to spare an author the
    boilerplate; subclassing it is never required.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "check"
    reason_code = "check"
    soft = False
    precision = 3
    msg_types = ("cam",)

    #: Suppressed for a beacon that SELF-DECLARES station_type="vru": pedestrians and cyclists
    #: legitimately travel off the road centreline and move erratically, so vehicle-kinematic and
    #: HD-map checks raise benign false positives on them. Declared per check rather than kept in a
    #: hard-coded MOTION_KEYS list in the engine.
    vru_suppressed = False

    #: Optional run-feature gate: None (always available), "station_type" or "denm". A gated check
    #: is registered but only ENTERS THE SUITE when the run enables its feature -- the in-tree
    #: precedent (`_emit_station_type` / `_denm_enabled`) that keeps a new column from perturbing
    #: the default digest.
    gate = None

    def __init__(self, *, params=None, rng=None, env=None):
        self.params = dict(params or {})
        self.rng = rng
        self.env = dict(env or {})

    def config_fields(self) -> Mapping[str, FieldSpec]:
        return {}

    def capabilities(self) -> frozenset:
        return frozenset()

    def evaluate(self, obs, state, params, rng) -> float:  # pragma: no cover - abstract
        raise NotImplementedError


# --------------------------------------------------------------------------- #
# LAYER 2 -- fusion
# --------------------------------------------------------------------------- #
@dataclass(frozen=True, slots=True)
class ReportDecision:
    """What the fusion layer decided about one observation. ``None`` means "do not report"."""
    fire: bool
    reason_codes: Sequence[str]   # ordered, most-severe first
    top_score: float              # the score of reason_codes[0]
    score_norm: float             # the vector's summary score


@runtime_checkable
class Fusion(Protocol):
    """Turns the score VECTOR into a report decision.

    This is F2MD's `MDApplication` and its single pure virtual `CheckNodeForReport`, with
    `ThresholdApp` / `BehavioralApp` / `MachineLearningApp` as the peer implementations the seam
    exists for. The built-in default (`streak_v1`) reproduces the engine's historical
    streak-then-Bernoulli rule exactly.

    `scores` is ORDERED: hard checks in declared order, then soft ones. **That order is
    digest-bearing**, and it is worth being precise about how: the engine's report rows are
    canonicalised with SORTED keys, so key insertion order never reaches the bytes -- what reaches
    them is the STABLE SORT this layer performs over equal scores. Two checks that both fire at the
    same value are ranked by their position in this mapping, and the winner becomes
    `reason_codes[0]`, `detector_outputs[0].check_id` and `detector_score`. So the order is an
    explicit, replayable input, never a discovered one.
    """

    interface_version: str
    plugin_id: str

    def config_fields(self) -> Mapping[str, FieldSpec]: ...

    def capabilities(self) -> frozenset: ...

    def decide(self, scores: Mapping[str, float], state: MutableMapping, obs: Observation,
               params: Mapping, rng) -> Optional[ReportDecision]: ...


class FusionBase:
    """Optional convenience base for a fusion implementation."""

    interface_version = INTERFACE_VERSION
    plugin_id = "fusion"

    def __init__(self, *, params=None, rng=None, env=None):
        self.params = dict(params or {})
        self.rng = rng
        self.env = dict(env or {})
        #: Ordered hard reason codes and the soft ones, handed over at construction. A fusion that
        #: needs to know WHICH keys may fire gets them from the engine rather than re-deriving them.
        self.keys = tuple(self.env.get("keys", ()))
        self.soft_keys = tuple(self.env.get("soft_keys", ()))

    def config_fields(self) -> Mapping[str, FieldSpec]:
        return {}

    def capabilities(self) -> frozenset:
        return frozenset()

    def decide(self, scores, state, obs, params, rng):     # pragma: no cover - abstract
        raise NotImplementedError


# --------------------------------------------------------------------------- #
# Column namespacing (section 4.4)
# --------------------------------------------------------------------------- #
#: Reserved prefix for a THIRD-PARTY key, so a plugin's column can never collide with a built-in's
#: or with a future standardised one. `x_` is to this vocabulary what `X-` was to HTTP headers.
THIRD_PARTY_PREFIX = "x_"


def namespaced_key(plugin_id: str, reason_code: str) -> str:
    """``x_<plugin_id>_<reason_code>`` -- the emitted key for a third-party check.

    The report column is then `detnorm_x_<plugin_id>_<reason_code>`, and the reason code that lands
    in `reason_codes` is the same namespaced string, so neither can collide with a built-in.
    """
    check_plugin_id(plugin_id)
    code = str(reason_code)
    if not code or not code.replace("_", "").isalnum():
        raise ConfigError(f"reason_code {reason_code!r} must be alphanumeric (underscores allowed)")
    return f"{THIRD_PARTY_PREFIX}{plugin_id}_{code}"


def is_third_party_key(key: str) -> bool:
    return str(key).startswith(THIRD_PARTY_PREFIX)


#: The load-time signature specs the registry validates against. Neither `Protocol` nor `ABC` does
#: this at runtime; a mismatch must fail BEFORE step 0, naming the offending parameter.
CHECK_SPEC = {"capabilities": (), "config_fields": (),
              "evaluate": ("obs", "state", "params", "rng")}
FUSION_SPEC = {"capabilities": (), "config_fields": (),
               "decide": ("scores", "state", "obs", "params", "rng")}
