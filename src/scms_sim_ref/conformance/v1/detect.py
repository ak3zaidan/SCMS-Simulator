"""The v1 DETECTOR contract (PLUGIN-ARCHITECTURE.md section 5, `D1`-`D7`).

Subclass it in YOUR test suite and pytest collects seven real tests; or run it from the CLI with
``scms-poc conformance --slot check --ref myorg.det:Threshold``; or let the engine refuse an
unattested plugin. Same seven checks either way -- one implementation, two drivers, because a suite
whose CLI and whose pytest surface can disagree about what "passed" means is worse than no suite.

    class TestMyThreshold(CheckContract):
        REF = "myorg.det:ProximityThreshold"
        PARAMS = {"max_plausible_speed_mps": 40.0}

**What this suite is FOR, stated plainly.** The channel contract's headline result was that its two
detection layers are independent: an oracle-reading channel model failed `C6b` while being perfectly
byte-reproducible, so the pinned-golden layer was provably blind to it. The detector seam has the
same property and a sharper stake, because a detector that can see the answers does not merely
mismodel physics -- it makes the whole dataset worthless while every digest still matches. `D2` and
`D3` are the two checks that speak to that, and they are the reason this file exists.

**Three deviations from the memo's sketch, all forced by the ABI and all stated here.**

1. **`D2` is stronger than the memo's name-based reading and weaker than its wording.**
   :class:`~scms_sim_ref.api.detect.Observation` is `slots=True`, so `dataclasses.fields` and the
   recorded attribute set are the same closed vocabulary by construction -- there is no `__dict__`
   to rummage through, and the check therefore cannot fail for an honest plugin. What it CAN catch,
   and does, is a plugin reaching for an attribute that is not on the DTO at all (a subclassed or
   monkey-patched Observation, an engine-internal name a plugin learned from reading `run.py`), and
   a plugin that MUTATES the observation it was handed. Both are reported by name.
2. **`D4` is graded on an HONEST observation, not on a threshold sweep.** The memo's "assert
   `>= 1.0` == violating" is not a statement about an arbitrary check in isolation: `certValidity`
   and `signatureVerification` are step functions with no dial to sweep. What IS decidable for every
   check is the polarity: a self-consistent, plausible, correctly-signed claim MUST NOT score as a
   violation. An F2MD check ported without inverting its `[0,1]` LOW-means-implausible convention
   scores ~1.0 on exactly that input, and fails here.
3. **`D6` needs a value from BEFORE the plugin existed.** "Registering is not enabling" can only be
   graded against a digest obtained without the plugin on the path, so the contract subclass supplies
   it as `GOLDEN`; with none supplied the check SKIPS and says so rather than inventing a
   comparison that would pass by construction. `D6` runs a full pipeline, so -- like `C12` -- it is
   excluded from the engine's in-run attestation.

**`D8` (added 2026-08-31) is the vocabulary check.** `D1`-`D7` grade one check's *behaviour*; `D8`
grades its *name*, because a detector whose column silently lands on top of another detector's is
not a detection failure, it is a data-corruption failure, and no digest, no lint and no monotonicity
test can see it. It runs the plugin through the ENGINE'S OWN LOADER alongside the full built-in
suite (and any sibling checks the contract declares via :attr:`CheckContract.PEERS`) and asserts the
resulting column vector has no duplicate. Cheap -- it builds a check suite, never a pipeline -- so
unlike `D6` it is included in the in-run attestation.
"""
from __future__ import annotations

import math
import random
import types

from ...api import detect as _detect
from ...api import registry as _registry
from ...api.detect import NamespacedState, Observation
from ...api.rng import RngNamespace
from ...schemas.records import is_forbidden_feature_key
from .channel import CheckSkipped          # ONE skip type, so `runner._is_skip` recognises both

SUITE_VERSION = "v1"

#: The check ids, in the order they are run and reported.
DETECT_CHECKS = (
    "D1_pure",
    "D2_reads_only_ma_visible",
    "D3_label_invariance",
    "D4_firing_convention",
    "D5_monotone_in_attack_magnitude",
    "D6_off_by_default_is_byte_identical",
    "D7_state_namespacing",
    "D8_reason_code_does_not_collide",
)


def _skip(reason: str):
    try:
        import pytest
    except ImportError:                                     # pragma: no cover - pytest optional
        raise CheckSkipped(reason) from None
    pytest.skip(reason)


def _engine():
    """Importing the engine is what populates the built-in registry."""
    from ...mock_pipeline import run as _run
    return _run


# --------------------------------------------------------------------------- #
# Observations, built to order
# --------------------------------------------------------------------------- #
def observation(*, offset_m: float = 0.0, t: float = 10.0, dt: float = 1.0, speed: float = 12.0,
                heading: float = 0.0, conf: float = 1.5, sig_ok: bool = True,
                station_type: str = "vehicle", msg_type: str = "cam", event_type=None,
                msg_count: int = 1, cell_cert_count: float = 1.0, map_offroad_m: float = 0.0,
                rx_reach_m: float = 500.0, rssi_dbm=None, first_sight: bool = False,
                cert_valid_from: float = 0.0, cert_valid_to: float = 3600.0,
                gen_time=None) -> Observation:
    """One HONEST observation, optionally displaced by `offset_m` metres of claimed position.

    "Honest" is doing real work here: with `offset_m = 0` the claim is exactly where a vehicle
    travelling at `speed` for one interval would be, its heading matches its bearing, its
    certificate is valid, its signature verifies, and it is alone in its Sybil cell. Every built-in
    scores it below 1.0, and so must any correctly-polarised third-party check (`D4`).
    """
    ref_t = t - dt
    ref_x, ref_y = 0.0, 0.0
    cx = ref_x + speed * dt + offset_m
    cy = ref_y
    return Observation(
        cert_digest="0123456789abcdef", station_type=station_type,
        claimed_x=cx, claimed_y=cy, claimed_speed=speed, claimed_heading=heading,
        pos_conf=conf, gen_time=t if gen_time is None else gen_time, msg_count=msg_count,
        msg_type=msg_type, event_type=event_type, sig_ok=sig_ok,
        cert_valid_from=cert_valid_from, cert_valid_to=cert_valid_to,
        rx_x=0.0, rx_y=0.0, rx_reach_m=rx_reach_m, rssi_dbm=rssi_dbm, link_state=None,
        t=t, dt=dt,
        first_sight=first_sight,
        ref_x=ref_x, ref_y=ref_y, ref_speed=speed, ref_heading=heading, ref_t=ref_t,
        prev_x=ref_x, prev_y=ref_y, prev_speed=speed, prev_heading=heading, prev_t=ref_t,
        map_offroad_m=map_offroad_m,
        neighbourhood=types.MappingProxyType({"cell_cert_count": cell_cert_count, "cbr": 0.2}))


class RecordingProxy:
    """Records every attribute name a check touches, and refuses every write.

    A CAPABILITY check, not a name check -- which is precisely what the existing name-based linter
    (`datagen/leakage_linter.py`, post-hoc and never called from `run.py`) provably cannot do.
    """

    __slots__ = ("_obs", "touched", "written")

    def __init__(self, obs):
        object.__setattr__(self, "_obs", obs)
        object.__setattr__(self, "touched", set())
        object.__setattr__(self, "written", set())

    def __getattr__(self, name):
        self.touched.add(name)
        return getattr(self._obs, name)

    def __setattr__(self, name, value):
        self.written.add(name)
        raise AttributeError(f"Observation is frozen; a check may not write {name!r}")


class OracleObservation(Observation):
    """An observation carrying GROUND TRUTH alongside identical declared fields.

    The detector-side equivalent of the channel suite's `OracleStation`. It is what makes `D3` a real
    anti-laundering test rather than a tautology: the two streams are field-for-field identical in
    everything the interface declares, and differ ONLY in oracle attributes no honest check can
    legitimately have heard of.
    """

    def __init__(self, obs, **oracle):
        super().__init__(**{f: getattr(obs, f) for f in _FIELDS})
        for k, v in oracle.items():
            object.__setattr__(self, k, v)

    __slots__ = ("is_attacker", "attack_type", "falsified", "true_x", "true_y", "veh")


_FIELDS = tuple(sorted(_detect.OBSERVATION_FIELDS))


class CheckContract:
    """The v1 check contract. Subclass, set :attr:`REF` or override :meth:`make`."""

    SLOT = "check"
    INTERFACE_VERSION = _detect.INTERFACE_VERSION
    REF: str = None                       #: dotted path, entry-point name or built-in registry key
    PARAMS: dict = {}                     #: params handed to the plugin, as a config would
    SEED = 20260831                       #: the ONLY source of variation in this whole suite
    PRECISION = 6                         #: decimals used when comparing two scores

    #: A digest obtained WITHOUT this plugin on the path, for D6. None -> D6 skips and says so.
    GOLDEN: str = None
    #: The config D6 runs, as a dict handed to `PipelineConfig`. Small on purpose.
    GOLDEN_CONFIG: dict = {}

    #: SIBLING checks shipped in the same distribution, as refs (or `{"ref":..., "params":{...}}`
    #: entries), graded together with this one by `D8`. A distribution that ships several checks can
    #: only be sure they do not claim each other's column by loading them into ONE vector, and one
    #: contract instance grades one ref -- so the family is declared here. Empty is normal and
    #: `D8` still grades this check against the full built-in suite.
    PEERS: tuple = ()

    #: check_id -> WRITTEN justification (Django's `django_test_skips`). Empty text is REFUSED.
    waivers: dict = {}

    # -- construction ------------------------------------------------------------------------- #
    def declared_waivers(self) -> dict:
        if self.REF is None:
            return {}
        try:
            _engine()
            cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        except Exception:                                   # unresolvable -> the checks will say so
            return {}
        return dict(getattr(cls, "conformance_waivers", None) or {})

    def waiver_for(self, check_id: str):
        return dict(self.declared_waivers(), **(self.waivers or {})).get(check_id)

    def env(self) -> dict:
        return {"dt": 1.0, "seed": self.SEED, "t0": 0.0}

    def params(self) -> dict:
        """The params the plugin is constructed with.

        A BUILT-IN's knobs are the engine's own config fields, so they are resolved from a default
        `PipelineConfig` exactly as `run.build_checks` resolves them -- which is what lets the
        shipped checks be graded by their own suite instead of being exempt from it.
        """
        run = _engine()
        cls, how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        if how == "builtin" and _registry.is_builtin(self.SLOT, cls):
            cfg = run.PipelineConfig()
            return {n: getattr(cfg, f) for n, f in (getattr(cls, "cfg_fields", {}) or {}).items()}
        p = {n: fs.default for n, fs in (run._plugin_config_fields(cls) or {}).items()}
        p.update(self.PARAMS)
        return p

    def make(self, **params):
        """Build one fresh instance. Every check that needs one builds its own -- a contract check
        may never observe state another check left behind."""
        if self.REF is None:
            raise NotImplementedError(
                f"{type(self).__name__}: set REF = 'package.module:Class' (or a built-in registry "
                f"key), or override make()")
        _engine()
        cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        p = dict(self.params())
        p.update(params)
        pid = _registry.plugin_id_of(cls, self.REF.rsplit(":", 1)[-1].lower())
        self._ns = RngNamespace(self.SEED, pid)
        self._ns.begin_step(0)
        self._pid = pid
        self._params = p
        return _registry.instantiate(cls, params=p, rng=self._ns, env=self.env())

    def capabilities(self) -> frozenset:
        return frozenset(self.make().capabilities())

    def msg_type(self) -> str:
        """The message type this check scores. A check declaring only "denm" is graded on DENMs."""
        return tuple(getattr(self.make(), "msg_types", ("cam",)))[0]

    def state(self) -> dict:
        """A fresh per-(receiver, sender) state dict, shaped exactly as the engine's is."""
        return {"h": [(0.0, 0.0, 12.0, 0.0, 9.0)], "streak": {}, "touch": 0}

    def obs(self, **kw) -> Observation:
        kw.setdefault("msg_type", self.msg_type())
        if kw["msg_type"] == "denm":
            kw.setdefault("event_type", "stationaryVehicle")
            kw.setdefault("speed", 0.2)          # a genuinely stopped hazard reporter
        return observation(**kw)

    def score(self, check, obs, state=None, params=None):
        st = self.state() if state is None else state
        return float(check.evaluate(obs, st, self._params if params is None else params, self._ns))

    # ===================================================================================== #
    # D1-D2: purity and the firewall
    # ===================================================================================== #
    def check_D1_pure(self):
        """Same `(obs, state)` twice -> same score, to the declared precision.

        Catches a check that reads a clock, `os.urandom`, `id()` or an unordered set. It is the
        cheapest possible statement of the property the whole project rests on, and it is graded
        before any dataset exists -- the pinned goldens cannot say WHY two runs differed.
        """
        o = self.obs()
        a = self.score(self.make(), o)
        b = self.score(self.make(), o)
        assert round(a, self.PRECISION) == round(b, self.PRECISION), (
            f"two identical evaluations returned {a!r} and {b!r}")

    def check_D2_reads_only_ma_visible(self):
        """Every attribute the check touched is a DECLARED `Observation` field, and it wrote none.

        The DTO is frozen and slotted, so this cannot fail for an honest plugin -- which is the
        point: the firewall is STRUCTURAL, and this check is the assertion that it is. What it
        catches is a plugin reaching for an engine-internal name (`veh`, `falsified`, `true_x`), and
        the report names the offender rather than merely saying "leak".
        """
        proxy = RecordingProxy(self.obs())
        self.score(self.make(), proxy)
        undeclared = sorted(n for n in proxy.touched
                            if n not in _detect.OBSERVATION_FIELDS and not n.startswith("__"))
        assert not undeclared, (
            f"read attribute(s) {undeclared} that the Observation does not declare; the declared "
            f"vocabulary is {sorted(_detect.OBSERVATION_FIELDS)}")
        forbidden = sorted(n for n in proxy.touched if is_forbidden_feature_key(n))
        assert not forbidden, f"read ORACLE-side name(s) {forbidden}"
        assert not proxy.written, f"wrote {sorted(proxy.written)} on the observation it was handed"
        # ... and the DTO really is frozen, not merely documented as such.
        o = self.obs()
        try:
            o.claimed_x = 0.0
        except (AttributeError, TypeError):
            return f"{len(proxy.touched)} declared fields read, none written"
        raise AssertionError("Observation accepted an attribute write -- it is not frozen")

    def check_D3_label_invariance(self):
        """THE ANTI-LAUNDERING TEST.

        Two observation streams identical in every DECLARED field, one of them additionally carrying
        ground truth (`is_attacker`, `falsified`, `true_x`, a `veh` handle). A check keying on the
        oracle changes its output; an honest one CANNOT. This is the detector-side `C6b`, and it is
        the one check the pinned-golden layer is structurally blind to: a leaky detector is perfectly
        byte-reproducible.
        """
        clean = [self.obs(offset_m=float(i) * 3.0) for i in range(6)]
        dirty = [OracleObservation(o, is_attacker=(i % 2 == 0), attack_type="ConstPos",
                                   falsified=(i % 2 == 0), true_x=-999.0, true_y=-999.0, veh=object())
                 for i, o in enumerate(clean)]
        a = [self.score(self.make(), o) for o in clean]
        b = [self.score(self.make(), o) for o in dirty]
        assert [round(x, self.PRECISION) for x in a] == [round(x, self.PRECISION) for x in b], (
            f"scores changed when GROUND TRUTH was attached to an otherwise identical observation: "
            f"{a} vs {b} -- this check reads the oracle")

    # ===================================================================================== #
    # D4-D5: the scoring convention
    # ===================================================================================== #
    def check_D4_firing_convention(self):
        """`>= 1.0` == VIOLATING, and an HONEST claim is therefore NOT a violation.

        Also: every score is a finite, non-negative float. `detnorm` is a normalised residual, and a
        negative one has no meaning under a `>= 1.0` cliff -- `tools/verify_data.py` already refuses
        negative `detnorm_*` in shipped data (`V2_detnorm_nonneg`).
        """
        check = self.make()
        honest = self.score(check, self.obs())
        assert honest == honest and -math.inf < honest < math.inf, (
            f"honest observation scored a non-finite {honest!r}")
        assert honest >= 0.0, f"honest observation scored a NEGATIVE {honest!r}"
        assert round(honest, int(getattr(check, "precision", 3))) < _detect.VIOLATION_THRESHOLD, (
            f"a self-consistent, correctly-signed, in-range, currently-valid claim scored "
            f"{honest!r} >= {_detect.VIOLATION_THRESHOLD} -- either the check is inverted (F2MD's "
            f"[0,1] LOW-means-implausible convention, which must be inverted for this engine) or it "
            f"fires on every honest message")
        for o in (self.obs(offset_m=250.0), self.obs(offset_m=-250.0)):
            v = self.score(check, o)
            assert v == v and -math.inf < v < math.inf and v >= 0.0, (
                f"score {v!r} is not a finite non-negative float")
        return f"honest score {round(honest, 4)} < 1.0"

    def check_D5_monotone_in_attack_magnitude(self):
        """Score NON-DECREASING in the magnitude of the falsification.

        The dial is the claimed-position offset -- the same quantity
        `tests/test_attack_magnitude.py` turns for the engine. A check that is flat in it passes
        (not every check keys on position); a check that goes DOWN as the lie gets bigger is broken
        in the direction that matters, and that is what this refuses.
        """
        ladder = [0.0, 5.0, 20.0, 60.0, 150.0, 400.0]
        scores = [self.score(self.make(), self.obs(offset_m=d)) for d in ladder]
        for (d0, s0), (d1, s1) in zip(zip(ladder, scores), zip(ladder[1:], scores[1:])):
            assert round(s1, self.PRECISION) >= round(s0, self.PRECISION), (
                f"score FELL as the falsification grew: {d0} m -> {s0}, {d1} m -> {s1}")
        return ("flat in claimed-position offset" if len(set(scores)) == 1
                else f"{scores[0]:.3f} -> {scores[-1]:.3f} over 0 -> 400 m")

    # ===================================================================================== #
    # D6-D7: the engine-level properties
    # ===================================================================================== #
    def check_D6_off_by_default_is_byte_identical(self):
        """REGISTERING IS NOT ENABLING: importing/installing the plugin, with no config entry,
        reproduces the golden.

        Runs a full pipeline, so the engine's in-run attestation excludes it exactly as it excludes
        `C12` -- running it from inside a pipeline would nest a pipeline in a pipeline.
        """
        if not self.GOLDEN:
            _skip("no GOLDEN pinned: 'registering is not enabling' can only be graded against a "
                  "digest obtained BEFORE this plugin was importable; set GOLDEN on the contract")
        run = _engine()
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            cfg = run.PipelineConfig(out_dir=td, **dict(self.GOLDEN_CONFIG))
            res = run.run_pipeline(cfg)
        assert res.data_digest == self.GOLDEN, (
            f"the default run's digest moved to {res.data_digest} merely because this plugin is "
            f"importable; expected {self.GOLDEN}")
        return res.data_digest[:16] + "..."

    def check_D7_state_namespacing(self):
        """Writes only `state["plugin:<id>"]`; never `h` / `streak` / `touch` / `kf`.

        Graded through the very wrapper the engine hands a third party, so this is not a code
        inspection -- it is the boundary itself, exercised. A BUILT-IN legitimately owns those keys
        and declares a waiver for this check (Django's `django_test_skips`, and the justification
        lands in the report next to the artifact).
        """
        st = self.state()
        before = {"h": list(st["h"]), "streak": dict(st["streak"]), "touch": st["touch"]}
        check = self.make()
        wrapped = NamespacedState(st, self._pid)
        try:
            for i in range(3):
                check.evaluate(self.obs(offset_m=float(i)), wrapped, self._params, self._ns)
        except Exception as e:                                # noqa: BLE001 - reported as a failure
            raise AssertionError(f"writing outside its own namespace: {type(e).__name__}: {e}") \
                from None
        assert list(st["h"]) == before["h"], "mutated the engine's claim history"
        assert dict(st["streak"]) == before["streak"], "mutated the fusion's streak counters"
        assert st["touch"] == before["touch"], "mutated the engine's prune bookkeeping"
        assert "kf" not in st, "wrote the built-in tracker's reserved 'kf' key"
        own = st.get(f"plugin:{self._pid}")
        return (f"own namespace holds {sorted(own)}" if own else "kept no state")

    # ===================================================================================== #
    # D8: the vocabulary
    # ===================================================================================== #
    def check_D8_reason_code_does_not_collide(self):
        """This check's column is well-formed, namespaced, and UNIQUE in the vector it joins.

        `D1`-`D7` grade behaviour. This one grades the NAME, and it is not a lesser concern: a check
        whose column lands on top of another check's is a silent data-corruption bug that every other
        layer of this project is structurally blind to. The suite still runs, every score is finite,
        non-negative, pure, monotone and oracle-free, the run is byte-reproducible, and the dataset
        is wrong -- one of the two checks' scores is simply gone, overwritten in the dict, with no
        error anywhere and a `detnorm_*` column that means different things on different rows.

        Three things are asserted, in increasing strength:

        1. **Well-formed.** `namespaced_key(plugin_id, reason_code)` accepts the pair, so the
           emitted column is `detnorm_x_<plugin_id>_<reason_code>` and the `x_` prefix makes a
           collision with a built-in's column, or with a future standardised one, structurally
           impossible. (`x_` is to this vocabulary what `X-` was to HTTP headers.)
        2. **Not a built-in's name.** The prefix protects the COLUMN; it does not protect the CODE.
           A third party that names its check `positionSpeedInconsistency` puts that string into
           `reason_codes` as `x_<id>_positionSpeedInconsistency` and into the ML tables as
           `reason_x_<id>_positionSpeedInconsistency`, next to the built-in's own -- and any analysis
           grouping by the human-readable tail conflates two different detectors. Refused.
        3. **Unique in the real vector.** The plugin is loaded through the ENGINE'S OWN LOADER,
           alongside the full built-in suite with both feature gates open and any siblings the
           contract declares in :attr:`PEERS`, and the resulting column tuple must have no
           duplicate. This is the assertion that actually bites, because it grades what the engine
           built rather than what the class declared.

        Builds a check suite, never a pipeline, so this check is INCLUDED in the engine's in-run
        attestation (unlike `D6`).
        """
        run = _engine()
        cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        builtin = _registry.is_builtin(self.SLOT, cls)
        code = str(getattr(cls, "reason_code", "") or "")
        assert code, f"{self.REF} declares no reason_code; it has no column to emit"

        builtin_codes = {}
        for name in _registry.builtin_names(self.SLOT):
            builtin_codes.setdefault(str(getattr(_registry.builtin(self.SLOT, name),
                                                 "reason_code", name)), name)

        entries = ["@builtins"]
        if not builtin:
            pid = _registry.plugin_id_of(cls, _fallback_pid(self.REF))
            # 1. well-formed: raises ConfigError on a malformed id or code, before step 0.
            column = _detect.namespaced_key(pid, code)
            assert column.startswith(_detect.THIRD_PARTY_PREFIX), (
                f"third-party column {column!r} is not namespaced with "
                f"{_detect.THIRD_PARTY_PREFIX!r}")
            # 2. not a built-in's name.
            assert code not in builtin_codes, (
                f"reason_code {code!r} is the BUILT-IN check {builtin_codes[code]!r}'s name. The "
                f"`x_` prefix keeps the COLUMNS apart, but the code itself lands in `reason_codes` "
                f"and in the ML tables' `reason_*` one-hots beside the built-in's, so any analysis "
                f"keyed on the readable tail conflates two different detectors. Pick another name.")
            entries.append({"ref": self.REF, "params": dict(self.PARAMS)})
        entries.extend({"ref": p} if isinstance(p, str) else dict(p) for p in (self.PEERS or ()))

        # 3. unique in the vector the ENGINE actually builds. Both feature gates open, so the two
        # conditionally registered built-ins are in it too.
        cfg = run.PipelineConfig(plugins={"check": entries})
        suite = run.build_checks(cfg, station_types=True, denm=True)
        cols = list(suite.columns)
        dupes = sorted({c for c in cols if cols.count(c) > 1})
        assert not dupes, (
            f"{len(suite.checks)} checks resolved to {len(set(cols))} columns: {dupes} is claimed "
            f"more than once, and the later check silently OVERWRITES the earlier one's score in "
            f"every report row. Two checks may not share a (plugin_id, reason_code) pair.")
        where = ("built-in" if builtin
                 else f"column {_detect.namespaced_key(pid, code)!r} at index {cols.index(column)}")
        return f"{where}, {len(cols)} unique columns"

    # ===================================================================================== #
    # pytest surface -- one thin method per check, all sharing _run()
    #
    # The module docstring promises "subclass it in YOUR test suite and pytest collects real
    # tests", and a promise like that is worthless without these wrappers: pytest collects
    # `test_*`, so a contract carrying only `check_*` methods collects ZERO tests while looking
    # exactly like a suite. That is the "defined, documented, called by nothing" failure mode this
    # design document's own adversarial review names three times. Mirrors
    # `ChannelModelContract`'s surface method-for-method, including the waiver route.
    # ===================================================================================== #
    def _run(self, check_id: str):
        fn = getattr(self, "check_" + check_id)
        try:
            return fn()
        except AssertionError as e:
            why = self.waiver_for(check_id)
            if why:
                _skip(f"WAIVED ({check_id}): {why} -- underlying failure: {e}")
            raise

    def test_D1_pure(self):
        self._run("D1_pure")

    def test_D2_reads_only_ma_visible(self):
        self._run("D2_reads_only_ma_visible")

    def test_D3_label_invariance(self):
        self._run("D3_label_invariance")

    def test_D4_firing_convention(self):
        self._run("D4_firing_convention")

    def test_D5_monotone_in_attack_magnitude(self):
        self._run("D5_monotone_in_attack_magnitude")

    def test_D6_off_by_default_is_byte_identical(self):
        self._run("D6_off_by_default_is_byte_identical")

    def test_D7_state_namespacing(self):
        self._run("D7_state_namespacing")

    def test_D8_reason_code_does_not_collide(self):
        self._run("D8_reason_code_does_not_collide")


def _fallback_pid(ref: str) -> str:
    """The plugin id the ENGINE would fall back to for a ref that declares none.

    Restated rather than imported from `run.py` so this module stays importable without the engine
    at module scope; `check_D8` resolves through `_engine()` first, so the two can never be reached
    with different answers in the same process. Mirrors `run._fallback_pid`.
    """
    tail = ref.rsplit(":", 1)[-1] if ":" in ref else ref
    out = "".join(ch if ch.isalnum() else "_" for ch in tail).lower().strip("_")
    return out[:32] or "plugin"
