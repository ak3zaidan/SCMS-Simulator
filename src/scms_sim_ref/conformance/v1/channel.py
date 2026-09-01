"""`ChannelModelContract` -- checks C1-C13 for `ChannelModel/1.x`.

Subclass it in YOUR test suite and pytest collects fourteen real tests (the thirteen numbered checks
plus C6's second arm)::

    from scms_sim_ref.conformance import ChannelModelContract

    class TestRayleigh(ChannelModelContract):
        REF = "myorg.radio:Rayleigh"
        PARAMS = {"range_m": 420.0}

...or override :meth:`make` and build the object however you like. Or skip pytest entirely::

    scms-poc conformance --slot channel_model --ref myorg.radio:Rayleigh --report conf.json

All checks are seeded, offline and stdlib-only. Nothing here is randomised run-to-run: the whole
suite is a pure function of :attr:`ChannelModelContract.SEED`.

WAIVERS ARE DATA, NOT EDITS (the Django doctrine). Set :attr:`waivers` to
`{check_id: "written justification"}`; a waived check that fails is reported as WAIVED with its
justification carried into `conformance_report.json`, and the suite still returns a pass. A waiver
without a justification string is refused -- the point of the mechanism is that the excuse is
recorded next to the artifact, not that the check disappears.

DEVIATION from the design memo, stated up front. The memo names check C6b
`rssi_tracks_true_geometry_not_claimed` and describes it as a verbatim port of
`tests/test_geometric_channel.py::test_rssi_tracks_true_geometry_not_the_claimed_position`. That
port cannot be written against this ABI, because :class:`StationSnapshot` has no claimed position in
it -- the DTO is oracle-side by necessity and carries TRUE geometry only, so "did the model use the
claimed position" is not a question the interface can even pose. The check implemented here,
:meth:`check_C6b_oracle_invariance`, is the strictly stronger statement the ABI CAN pose: two frame
sequences whose declared fields are identical field-for-field, one of which additionally carries
ground truth (attacker flags, falsification flags, true coordinates) on the station objects and in
`frame.env`, must produce IDENTICAL traces. A model that keys on any of it is caught, whether it
laundered a claimed position, an attacker flag or anything else. The memo's original, dataset-level
correlation test is untouched and still runs in `tests/test_geometric_channel.py`.
"""
from __future__ import annotations

import math
import random

from ...api import channel as _channel
from ...api import registry as _registry
from ...api.errors import ConfigError
from ...api.rng import RngNamespace
from ...schemas.records import is_forbidden_feature_key
from . import harness as H
from .harness import DrawCounter, audit_guard, build_frames, trace

SUITE_VERSION = "v1"

#: The check ids, in the order they are run and reported. Thirteen numbered checks; C6 has two arms
#: (the structural one and the anti-laundering one), so there are fourteen rows.
CHANNEL_CHECKS = (
    "C1_repeatable",
    "C2_call_order_independent",
    "C3_global_rng_untouched",
    "C4_state_advances_once_per_step",
    "C5_no_io",
    "C6_no_oracle_leak",
    "C6b_oracle_invariance",
    "C7_ranges",
    "C8_reach_honesty",
    "C9_monotone_in_distance",
    "C10_fail_fast",
    "C11_float_hygiene",
    "C12_pipeline_two_run_digest",
    "C13_config_not_mutated",
)

#: C7's published bounds, IMPORTED rather than restated. `api.channel.check_outcome` is C7's runtime
#: form and the engine now runs it on every delivered link of a third-party model; two independent
#: literals for one published bound is how a runtime gate ends up laxer than the check it claims to
#: be (they read [-200, 50] and [-140, 0] respectively until 2026-08-31).
RSSI_MIN_DBM = _channel.RSSI_MIN_DBM
RSSI_MAX_DBM = _channel.RSSI_MAX_DBM

#: C9's tolerances. `PDR_SLACK` / `RSSI_SLACK_DB` bound BOTH the adjacent-rung comparison and the
#: first-rung-versus-last one, so the ladder's total tolerance is one slack rather than six.
PDR_SLACK = 0.10
RSSI_SLACK_DB = 1.5

#: C9's non-degeneracy floor. The ladder spans a 19x distance ratio (0.05 -> 0.95 of the declared
#: reach); the shallowest path-loss exponent any FieldSpec in this tree admits (n = 1.6) already
#: gives 10 * 1.6 * log10(19) = 20.5 dB there, so 3 dB is not a physics claim, it is the line below
#: which a model is not modelling distance at all.
MIN_ATTENUATION_DB = 3.0
#: The same statement for a model that reports no rssi: PDR must fall by at least this much.
MIN_PDR_DROP = 0.05


class CheckSkipped(Exception):
    """A check that could not be RUN (not one that passed). Always carries a reason."""


def _skip(reason: str):
    try:
        import pytest
    except ImportError:                                     # pragma: no cover - pytest optional
        raise CheckSkipped(reason) from None
    pytest.skip(reason)


class ChannelModelContract:
    """The v1 channel contract. Subclass, set :attr:`REF` or override :meth:`make`."""

    # -- what to test ------------------------------------------------------------------------- #
    SLOT = "channel_model"
    INTERFACE_VERSION = _channel.INTERFACE_VERSION
    REF: str = None                       #: dotted path, entry-point name or built-in registry key
    PARAMS: dict = {}                     #: params handed to the plugin, as a config would
    SEED = 20260831                       #: the ONLY source of variation in this whole suite
    PRECISION = 6                         #: decimals C11 rounds to before comparing

    #: Config an engine-side built-in reads at construction. A third-party model should read
    #: `params`; these exist so the built-ins can be graded by their own suite.
    RADIO_RANGE_M = 500.0

    #: check_id -> WRITTEN justification (Django's `django_test_skips` doctrine). A waiver with an
    #: empty justification is REFUSED.
    #:
    #: The memo also lists an `expected_failures: frozenset` beside this. It is deliberately NOT
    #: implemented: it is a second way to excuse a failing check that carries no justification, which
    #: is precisely the mute button the waiver mechanism exists to avoid being. One mechanism, and it
    #: costs a sentence of explanation that lands in the report next to the artifact.
    waivers: dict = {}

    def declared_waivers(self) -> dict:
        """Waivers the IMPLEMENTATION itself declares, as `SomeModel.conformance_waivers`.

        This is Django's `DatabaseFeatures.django_test_skips` verbatim: a backend states what it
        legitimately cannot pass as DATA it ships, not by editing the suite, and the written
        justification travels into `conformance_report.json` and from there into the manifest. A
        contract subclass's own :attr:`waivers` override the implementation's for the same check id.
        """
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

    # -- construction ------------------------------------------------------------------------- #
    def env(self) -> dict:
        """The construction environment, shaped exactly like `run._channel_env`."""
        cfg = self._config()
        env = {k: getattr(cfg, k) for k in _CHANNEL_ENV_KEYS if hasattr(cfg, k)}
        env["dt"] = 1.0
        env["buildings"] = []
        # Read-only, exactly as the engine hands it over -- a contract that gave the model a WRITABLE
        # config would be grading a different construction environment from the one it will get.
        env["config"] = _engine().ReadOnlyConfig(cfg)
        # C13 watches THIS object. The snapshot is taken here, before the model exists, so a write
        # performed during __init__ -- the cheapest place to do it and the one the engine could not
        # see until `_write_manifest` ran an hour later -- is inside the watched window.
        self._env_cfg = cfg
        self._env_cfg_before = _config_snapshot(cfg)
        return env

    def _config(self):
        return _engine().PipelineConfig(seed=self.SEED, radio_range_m=self.RADIO_RANGE_M,
                                        radio_nlosb_density_per_km=0.0)

    def make(self, **params):
        """Build one fresh instance. Every check that needs an instance builds its own -- a contract
        check may never observe state another check left behind."""
        if self.REF is None:
            raise NotImplementedError(
                f"{type(self).__name__}: set REF = 'package.module:Class' (or a built-in registry "
                f"key), or override make()")
        p = dict(self.PARAMS)
        p.update(params)
        _engine()                       # the built-in registry is populated by importing the engine
        cls, _how, _iv, _shape = _registry.resolve(self.SLOT, self.REF)
        _validate_params(cls, p)
        pid = _registry.plugin_id_of(cls, self.REF.rsplit(":", 1)[-1].lower())
        self._ns = RngNamespace(self.SEED, pid)
        return _registry.instantiate(cls, params=p, rng=self._ns, env=self.env())

    def capabilities(self) -> frozenset:
        return frozenset(self.make().capabilities())

    # -- driving a model, always with its namespace attached ---------------------------------- #
    # Every drive site goes through these two so the suite can never grade a model with a FROZEN
    # RngNamespace step. `make()` sets `self._ns` immediately before returning, and Python evaluates
    # a call's arguments before the call, so `self._trace(self.make(), ...)` reads the namespace of
    # the model it is about to drive.
    def _trace(self, model, frames, **kw):
        kw.setdefault("rng_ns", getattr(self, "_ns", None))
        return trace(model, frames, **kw)

    def _adapt(self, model):
        return H.adapt(model, getattr(self, "_ns", None))

    # -- frames ------------------------------------------------------------------------------- #
    def frames(self, **kw):
        kw.setdefault("n_steps", 6)
        kw.setdefault("n_stations", 12)
        kw.setdefault("spacing_m", 55.0)
        kw.setdefault("move_m", 7.0)
        return build_frames(**kw)

    # ===================================================================================== #
    # determinism
    # ===================================================================================== #
    def check_C1_repeatable(self):
        """Two fresh instances, the same frames, byte-equal traces.

        This is the FIRST of the design's two independent detection layers. It catches a plugin that
        reads a clock, `os.urandom`, `id()` or an unordered set BEFORE any dataset exists -- which is
        the whole point of having it: the second layer (the pinned goldens and the two-run digest
        gate) can only speak after a full run, and cannot say WHY the digests differ.
        """
        fr = self.frames()
        a = self._trace(self.make(), fr)
        b = self._trace(self.make(), fr)
        assert a, "the model delivered nothing at all -- the scenario cannot grade it"
        assert a == b, _first_diff(a, b)

    def check_C2_call_order_independent(self):
        """The ns-3 `AssignStreams` property, restated for string-keyed streams: a link's outcome
        must not depend on the ORDER in which links were evaluated.

        A model whose draws come off one shared sequential stream passes C1 and fails here, and the
        failure is the useful one: it means the model's results are a function of the engine's loop
        order, so any future re-ordering of the receive loop silently re-derives every digest.
        """
        fr = self.frames()
        a = self._trace(self.make(), fr, order="sorted")
        b = self._trace(self.make(), fr, order="reversed")
        ka = {(r[0], r[1], r[2]): r[3:] for r in a}
        kb = {(r[0], r[1], r[2]): r[3:] for r in b}
        assert ka == kb, ("per-link results depend on call ORDER -> draws are not identity-keyed. "
                          + _first_diff(sorted(ka.items()), sorted(kb.items())))

    def check_C3_global_rng_untouched(self):
        """D3, asserted: the plugin never reaches the engine's shared stream.

        FOUR traps, and the last two exist because the first two catch only the shapes that are
        provably HARMLESS.

        Measured 2026-08-31 on five models that subclass the reference plugin with `plugin_id`
        forced to `rayleigh`, so physics and RNG namespace are bit-identical and any digest movement
        is attributable to the ENGINE's stream and nothing else. Control: `be3b5dea…`, 3012
        `ma_reports`, 25 investigations, 25 CRL events, 67 sampled emissions.

        =========================== ============= ========================================
        vector                      old C3        engine `data_digest`
        =========================== ============= ========================================
        module `random.random()`    **FAIL**      unchanged
        `random.seed(time_ns())`    **FAIL**      unchanged
        `sys._getframe` -> `rng`    pass (13/13)  **moved** `433dd921…`, reports 3012 -> 3000,
                                                  emissions 67 -> 64
        `Random.random` passthrough pass (13/13)  unchanged (it consumes the same draws)
        `Random.random` biased x0.5 pass (13/13)  **moved** `9cdbed88…`, reports 3012 -> 46356,
                                                  investigations and CRL events 25 -> 100,
                                                  emissions 67 -> 359
        =========================== ============= ========================================

        Read that table twice. **The check was detecting exactly the two vectors that cannot reach
        the engine** -- its `rng` is a private `random.Random` instance, so a module-level draw
        provably cannot touch it -- while the two that can scored 13/13 and also passed
        `conformance="required"` attestation. The passthrough row is the honest caveat: rebinding
        alone moves nothing, because a wrapper that just forwards consumes the same draws. It is the
        CAPABILITY that matters, and the last row is what that capability is worth once exercised:
        fifteen times the reports and every attacker revoked, from one line at construction.

        1. **module state** -- a bare `random.random()` / `random.seed()` anywhere in the model.
        2. **`frame.env["rng"]`** -- `env` is the one mapping a plugin is handed that could
           plausibly be made to carry a stream.
        3. **the `random.Random` CLASS SURFACE** -- every generator method's identity is compared
           before and after. A model that rebinds `random.Random.random` (or `gauss`, or `seed`)
           reaches every stream in the process at once, including the engine's private instance,
           without ever touching a state this check could otherwise snapshot. Restored in `finally`,
           the same way the module state is: a check that detects a tamper and leaves it installed
           has poisoned the interpreter for everything that runs after it.
        4. **a frame-local decoy** -- the trace is driven from a frame whose local is named `rng`
           and holds a live `random.Random`, exactly as `run_pipeline`'s is. A plugin that walks
           `sys._getframe(k).f_locals` looking for the engine's stream finds this one.

        Stated honestly, because the design insists on it: capability-by-omission is a real
        STRUCTURAL defence against the ACCIDENTAL case and is not enforceable against a determined
        one -- a plugin can reach any object in the process through `gc.get_objects()`. What these
        four traps do is make the realistic deliberate vectors visible instead of invisible, and the
        engine-side statement that actually holds remains the artifact-level one: the pinned goldens
        and the two-run digest gate.
        """
        saved = random.getstate()
        # The baseline is the one the RUNNER took before ANY check ran, when no plugin code had yet
        # executed. Taking it here instead would compare a model against a class it had already
        # patched during C1 -- and an evasive patch is written idempotently precisely so that the
        # second construction is a no-op. `run_contract` stashes it; a bare pytest invocation of this
        # one method falls back to a fresh snapshot.
        surface = getattr(self, "_pristine_random", None) or H.random_class_surface()
        try:
            random.seed(999)
            before = random.getstate()
            probe = random.Random(1)
            probe_state = probe.getstate()
            fr = build_frames(n_steps=4, n_stations=10,
                              env={"buildings": [], "weather": "clear", "rng": probe})
            decoy, decoy_state = self._trace_behind_a_decoy_rng(fr)
            assert random.getstate() == before, (
                "the model drew from the module-level `random` stream. A plugin's only source of "
                "randomness is its RngNamespace: the engine's global stream's draw COUNT AND ORDER "
                "are load-bearing across packet loss, report_prob, collusion and net_delay.")
            assert probe.getstate() == probe_state, (
                "the model found a Random in frame.env and drew from it -- capability by omission "
                "means not touching a stream you were not handed")
            tampered = H.tampered_random_names(surface)
            assert not tampered, (
                f"the model REBOUND {tampered} on the `random` module or the `random.Random` class. "
                f"That reaches every stream in the process at once -- including the engine's private "
                f"instance, whose draw count and order are load-bearing -- while leaving every state "
                f"snapshot a check could take perfectly intact. It is the vector capability by "
                f"omission cannot close by omitting anything.")
            assert decoy.getstate() == decoy_state, (
                "the model walked the call stack to a caller's local named `rng` and drew from it. "
                "`run_pipeline` holds the engine's shared stream in a local of exactly that name "
                "(`rng = random.Random(cfg.seed)`), so this is not a hypothetical shape: it moves "
                "report_prob, collusion, net_delay and emit sampling, and no state this plugin was "
                "handed changes.")
        finally:
            H.restore_random_class_surface(surface)       # undo any tamper the model installed
            random.setstate(saved)

    def _trace_behind_a_decoy_rng(self, fr):
        """Drive the trace from a frame carrying a local named `rng` that IS a `random.Random`.

        Deliberately shaped like `run_pipeline`'s own frame (`rng = random.Random(cfg.seed)` beside
        a `cfg`), so a plugin walking `sys._getframe(k).f_locals` for the engine's shared stream --
        by name or by type -- finds this object and moves its state where the check can see it.
        """
        rng = random.Random(4242)                          # noqa: F841 - the decoy IS the point
        cfg = self._config()                               # noqa: F841 - so is its neighbour
        state = rng.getstate()
        self._trace(self.make(), fr)
        return rng, state

    def check_C4_state_advances_once_per_step(self):
        """Only for `capabilities() & {"stateful"}`: with a FROZEN scene and an identical candidate
        set every step, the number of draws the model makes must be CONSTANT across steps.

        That is `GeometricChannel`'s `if st["step"] == self.step: return cached` guard, promoted from
        a private convention to an enforceable one. Step 0 is excluded and the exclusion is not a
        fudge: step 0 is where every per-link stream is CREATED and every AR(1) process is seeded
        from its stationary distribution, so its draw count is structurally different from every
        later step's. What C4 asserts is that steps 1..N-1 agree with each other.
        """
        if _channel.CAP_STATEFUL not in self.capabilities():
            _skip("model does not declare the `stateful` capability")
        model = self.make()
        fr = build_frames(n_steps=6, n_stations=10, move_m=0.0)   # frozen scene
        per_step = []
        ns_counts = []
        for frame in fr:
            with DrawCounter() as dc:
                ad = self._adapt(model)
                ad.begin_step(frame)
                cands = H.candidates_for(ad, frame)
                list(ad.deliver(frame, cands))
            per_step.append(dc.count)
            ns = getattr(self, "_ns", None)
            ns_counts.append(sum(ns.advance_counts().values()) if ns is not None else 0)
        tail = per_step[1:]
        assert len(set(tail)) == 1, (
            f"per-step draw count is not constant across steps 1..{len(fr) - 1}: {per_step} "
            f"(step 0 excluded -- that is where streams are created). A model whose state advances "
            f"more than once per step re-randomises a link mid-step; one that advances less has a "
            f"stale step guard.")
        deltas = [ns_counts[i] - ns_counts[i - 1] for i in range(2, len(ns_counts))]
        assert len(set(deltas)) <= 1, (
            f"RngNamespace.persistent() hand-outs per step are not constant: {ns_counts}")

    # ===================================================================================== #
    # safety
    # ===================================================================================== #
    def check_C5_no_io(self):
        """No filesystem writes, no sockets, no subprocesses while evaluating links.

        Stated honestly, because the design insists on it: this is DETECTION, not sandboxing. PEP 578
        says so itself -- *"is not sandboxing... does not attempt to prevent malicious behavior"* --
        and audit hooks fire only in the current interpreter, so a subprocess escapes entirely. What
        it does catch is the realistic failure: a model that memoises to a temp file, phones a
        licence server, or shells out, any of which makes the run non-reproducible on another host.
        A model that legitimately does I/O declares `out_of_process` or `frozen` and is skipped here.
        """
        caps = self.capabilities()
        if caps & {_channel.CAP_OUT_OF_PROCESS, _channel.CAP_FROZEN}:
            _skip("model declares out_of_process/frozen -- I/O is its declared mechanism")
        if self.REF is not None:
            _registry.resolve(self.SLOT, self.REF)          # warm the import OUTSIDE the guard
        fr = self.frames(n_steps=3)
        with audit_guard() as g:
            self._trace(self.make(), fr)
        assert not g.hits, f"denied I/O during evaluation: {g.hits}"

    def check_C6_no_oracle_leak(self):
        """The structural arm: the DTOs are actually frozen, the model did not mutate the frame, and
        every `extras` key is namespaced and is not a forbidden ground-truth name.

        `extras` is the only channel through which a channel model can add a COLUMN to the dataset,
        so `x_<plugin_id>_` namespacing is not cosmetic: it is what keeps a third-party key from
        colliding with the standardised vocabulary or with a future built-in.
        """
        import dataclasses
        s = _channel.StationSnapshot(1, 0.0, 0.0, 1.6, 1.6)
        for obj, attr in ((s, "x"), (_channel.Transmission(0, 0, "cam", 1, 300), "tx_vid"),
                          (_channel.LinkOutcome(), "rssi_dbm")):
            try:
                setattr(obj, attr, 0.0)
            except (dataclasses.FrozenInstanceError, AttributeError):
                pass
            else:                                            # pragma: no cover - defensive
                raise AssertionError(f"{type(obj).__name__}.{attr} is writable; the DTO must be "
                                     f"frozen, or the boundary is advisory")
        model = self.make()
        pid = str(getattr(model, "plugin_id", "plugin"))
        fr = self.frames(n_steps=4)
        before = [{v: (st.vid, st.x, st.y, st.ant_h_m, st.blocker_h_m, st.is_rsu, st.is_vru,
                       st.tx_power_dbm, st.rx_range_m) for v, st in f.stations.items()} for f in fr]
        rows = self._trace(model, fr)
        after = [{v: (st.vid, st.x, st.y, st.ant_h_m, st.blocker_h_m, st.is_rsu, st.is_vru,
                      st.tx_power_dbm, st.rx_range_m) for v, st in f.stations.items()} for f in fr]
        assert before == after, "the model mutated the StepFrame it was handed"
        bad = sorted({k for r in rows for k, _v in r[6]
                      if is_forbidden_feature_key(k) or not k.startswith(f"x_{pid}_")})
        assert not bad, (f"LinkOutcome.extras keys {bad} are either ground-truth names or not "
                         f"namespaced as x_{pid}_<key>")

    def check_C6b_oracle_invariance(self):
        """THE ANTI-LAUNDERING CHECK, and the only one here that catches a plausible-LOOKING wrong
        implementation.

        Two frame sequences, identical in every DECLARED field of every DTO. One additionally carries
        ground truth the ABI never promised: `is_attacker` / `falsified` / `attack_type` / `true_x`
        on the station objects, and the same flags in `frame.env`. A model that reads any of it --
        `if getattr(tx, "is_attacker", False): return None` is one line and looks like nothing --
        produces a different trace. An honest one CANNOT.

        This is a CAPABILITY check, not a name check, which is precisely what the existing name-based
        linter (`datagen/leakage_linter.py`, and it is never called from `run.py`) provably cannot
        do. It is the channel-side form of the detector suite's D3.
        """
        plain = build_frames(n_steps=5, n_stations=12, move_m=7.0, oracle=False)
        oracle_env = {"buildings": [], "weather": "clear",
                      "is_attacker": {i: (i % 3 == 0) for i in range(12)},
                      "falsified": {i: (i % 3 == 0) for i in range(12)},
                      "attack_types": ["ConstPos"]}
        spiked = build_frames(n_steps=5, n_stations=12, move_m=7.0, oracle=True, env=oracle_env)
        for fa, fb in zip(plain, spiked):                    # the declared fields must be identical
            assert [_declared(s) for s in fa.stations.values()] == \
                   [_declared(s) for s in fb.stations.values()], "harness bug: frames differ"
        a = self._trace(self.make(), plain)
        b = self._trace(self.make(), spiked)
        assert a == b, (
            "the model's output changed when GROUND TRUTH it was never promised appeared beside the "
            "declared fields. It is reading the oracle: every physics number it produces is "
            "laundering a label, and no range check, plausibility eyeball or name-based lint can "
            "see it. " + _first_diff(a, b))

    # ===================================================================================== #
    # output sanity
    # ===================================================================================== #
    def check_C7_ranges(self):
        """rssi in [-140, 0] dBm or None; link_state in the CLOSED vocabulary {LOS, NLOSv, NLOSb};
        delay_s finite and >= 0; reach_m finite and > 0; no NaN or inf anywhere."""
        model = self.make()
        assert H.finite(model.reach_m) and float(model.reach_m) > 0.0, (
            f"reach_m must be finite and > 0 (got {model.reach_m!r}). It is MANDATORY: the engine "
            f"reads it for the candidate window AND acceptanceRangeThreshold reads it as art_reach, "
            f"so a model that omits or understates it makes that detector wrong for every honest "
            f"long link.")
        rows = self._trace(model, self.frames())
        for step, ti, rv, rssi, state, delay, extras in rows:
            where = f"step {step} link (tx {ti} -> rx {rv})"
            if rssi is not None:
                assert H.finite(rssi), f"{where}: rssi_dbm is not finite ({rssi!r})"
                assert RSSI_MIN_DBM <= rssi <= RSSI_MAX_DBM, (
                    f"{where}: rssi_dbm {rssi} outside [{RSSI_MIN_DBM}, {RSSI_MAX_DBM}] dBm")
            assert state is None or state in _channel.LINK_STATES, (
                f"{where}: link_state {state!r} not in the closed vocabulary "
                f"{sorted(_channel.LINK_STATES)}")
            assert H.finite(delay) and float(delay) >= 0.0, f"{where}: delay_s {delay!r}"
            for k, v in extras:
                assert H.finite(v), f"{where}: extras[{k!r}] = {v!r} is not finite"

    def check_C8_reach_honesty(self):
        """No link is delivered beyond the model's own DECLARED reach.

        This is what keeps `acceptanceRangeThreshold` correct: the detector bounds a claimed
        distance on `art_reach`, which IS the channel's declared reach, so every metre of undeclared
        delivery is a false positive against an honest long link. The check deliberately offers the
        model candidates 60 % beyond its declared reach rather than trusting the engine's own
        pre-filter, because the question is what the MODEL does, not what the loop does for it.
        """
        model = self.make()
        ad = self._adapt(model)
        fr = build_frames(n_steps=4, n_stations=14, spacing_m=90.0, move_m=5.0)
        for frame in fr:
            for rx_vid in frame.receivers:
                rx = frame.stations[rx_vid]
                assert _channel.window_of(ad, rx) >= _channel.reach_of(ad, rx) - 1e-9, (
                    f"window_m({rx_vid}) < reach_m_for({rx_vid}): a model may search wider than it "
                    f"delivers, never narrower")
        over = []
        for frame in fr:
            ad.begin_step(frame)
            cands = H.candidates_for(ad, frame, beyond=0.6 * float(model.reach_m))
            dist = {(t, r): d for t, r, d in cands}
            for o in _channel.sort_outcomes(ad.deliver(frame, cands)):
                reach = _channel.reach_of(ad, frame.stations[o.rx_vid])
                d = dist[(o.tx_index, o.rx_vid)]
                if d > reach + 1e-6:
                    over.append((frame.step, o.tx_index, o.rx_vid, round(d, 1), round(reach, 1)))
        assert not over, (
            f"{len(over)} link(s) delivered beyond the declared reach, e.g. "
            f"{over[:5]} as (step, tx, rx, distance_m, declared_reach_m)")

    # ===================================================================================== #
    # physics
    # ===================================================================================== #
    def check_C9_monotone_in_distance(self):
        """PDR(d) and mean rssi(d) NON-INCREASING **and actually distance-dependent** over a fixed
        ladder, all else equal.

        Waivable WITH A WRITTEN JUSTIFICATION: a model with a deliberate near-field, a two-ray
        ground-reflection null or a beam pattern legitimately fails the monotone arms, and an
        idealised unit-disc model (`disc`) legitimately fails the attenuation arm. The waiver
        mechanism exists so that stays a declared property of the implementation, recorded in
        `conformance_report.json` next to the artifact, rather than a silent hole.

        Sampling budget and tolerances are fixed, not tuned: 12 transmitters x 8 steps per rung, a
        0.10 absolute PDR tolerance and a 1.5 dB rssi tolerance. The ladder ROTATES the transmitters
        around the receiver, holding the distance exactly constant while moving both endpoints far
        enough to decorrelate an AR(1) shadowing process -- otherwise the rungs would be one
        correlated trajectory rather than independent samples.

        THREE arms, and the second and third exist because the first cannot fail the two shapes that
        matter most:

        * **adjacent** -- `pdr[i] <= pdr[i-1] + PDR_SLACK` and the same for mean rssi. Catches a
          steeply anti-monotone model.
        * **cumulative** -- the FIRST rung against the LAST, bounded by ONE slack. The adjacent arm
          alone tolerates `PDR_SLACK` at every one of the six steps, i.e. a monotone RISE of 0.60
          PDR and 9 dB across the whole ladder, which it is structurally incapable of seeing.
        * **attenuation** -- across a 19x distance ratio (0.05 to 0.95 of the declared reach) the
          model must show SOME distance dependence. A model returning a flat delivery probability
          and a constant rssi at every distance is not a channel, and the first two arms pass it by
          construction: `<=` is satisfied by equality. Graded on rssi when the model produces any
          (`MIN_ATTENUATION_DB`, far below the ~21 dB a 1.6-exponent path loss gives over that
          ratio, so no honest model is near the line), otherwise on PDR.
        """
        model = self.make()
        reach = float(model.reach_m)
        rungs = [round(f * reach, 1) for f in (0.05, 0.12, 0.25, 0.40, 0.60, 0.80, 0.95)]
        pdrs, rssis = [], []
        for d in rungs:
            m = self.make()
            pdr, mean, offered = H.pdr_and_rssi(m, d, rng_ns=getattr(self, "_ns", None))
            assert offered > 0, f"ladder rung {d} m offered no candidate links"
            pdrs.append(pdr)
            rssis.append(mean)
        ladder = list(zip(rungs, [round(p, 3) for p in pdrs],
                          [None if r is None else round(r, 2) for r in rssis]))
        # -- arm 1: adjacent rungs ---------------------------------------------------------- #
        for i in range(1, len(rungs)):
            assert pdrs[i] <= pdrs[i - 1] + PDR_SLACK, (
                f"PDR rises with distance: {rungs[i - 1]} m -> {pdrs[i - 1]:.3f}, "
                f"{rungs[i]} m -> {pdrs[i]:.3f} (ladder {ladder})")
            if rssis[i] is not None and rssis[i - 1] is not None:
                assert rssis[i] <= rssis[i - 1] + RSSI_SLACK_DB, (
                    f"mean rssi rises with distance: {rungs[i - 1]} m -> {rssis[i - 1]:.2f} dBm, "
                    f"{rungs[i]} m -> {rssis[i]:.2f} dBm (ladder {ladder})")
        # -- arm 2: cumulative, first rung against last ------------------------------------- #
        assert pdrs[-1] <= pdrs[0] + PDR_SLACK, (
            f"PDR is higher at the FAR end of the ladder than the near end: {rungs[0]} m -> "
            f"{pdrs[0]:.3f}, {rungs[-1]} m -> {pdrs[-1]:.3f}. Each adjacent step stayed inside the "
            f"{PDR_SLACK} tolerance, but they accumulate (ladder {ladder})")
        if rssis[0] is not None and rssis[-1] is not None:
            assert rssis[-1] <= rssis[0] + RSSI_SLACK_DB, (
                f"mean rssi is higher at the FAR end of the ladder than the near end: {rungs[0]} m "
                f"-> {rssis[0]:.2f} dBm, {rungs[-1]} m -> {rssis[-1]:.2f} dBm (ladder {ladder})")
        # -- arm 3: the model must depend on distance at all -------------------------------- #
        measured = [r for r in rssis if r is not None]
        if len(measured) >= 2 and rssis[0] is not None and rssis[-1] is not None:
            drop, unit, floor = rssis[0] - rssis[-1], "dB", MIN_ATTENUATION_DB
        else:
            drop, unit, floor = pdrs[0] - pdrs[-1], "PDR", MIN_PDR_DROP
        assert drop >= floor, (
            f"the model is CONSTANT in distance: over a {rungs[-1] / max(rungs[0], 1e-9):.0f}x "
            f"distance ratio ({rungs[0]} m -> {rungs[-1]} m) it fell by only {drop:.3f} {unit}, "
            f"below the {floor} {unit} floor. Arms 1 and 2 are '<=' comparisons and are satisfied "
            f"by exact equality, so a flat delivery probability with a constant rssi passes both "
            f"while being physically impossible. Declare a waiver if this model is a deliberate "
            f"idealisation (`disc` does). Ladder: {ladder}")
        return f"ladder {ladder}"

    def check_C10_fail_fast(self):
        """Invalid params raise at CONSTRUCTION, never at step k > 0.

        Resolution happens once, before step 0, and every failure is fatal there -- because a plugin
        that raises mid-loop produces a partial dataset whose digest matches nothing, and it must not
        take the SIGINT path that finalises a VALID manifest for a partial run.

        Two arms. The first (an undeclared param name) is enforced by the framework and passes for
        any plugin, which is stated rather than hidden. The second (a value outside a bound the
        plugin ITSELF declared through `FieldSpec`) is the plugin's own declaration doing the work,
        and it only runs when the plugin declares a bounded field.
        """
        try:
            self.make(**{"__definitely_not_a_declared_param__": 1})
        except (ConfigError, ValueError, TypeError):
            pass
        else:
            raise AssertionError("an undeclared parameter was accepted at construction")
        _engine()
        spec = _config_fields(_registry.resolve(self.SLOT, self.REF)[0]) if self.REF else {}
        bounded = [(n, fs) for n, fs in sorted(spec.items())
                   if fs.hi is not None or fs.lo is not None or fs.options is not None]
        if not bounded:
            return "only the undeclared-parameter arm ran: the plugin declares no bounded FieldSpec"
        name, fs = bounded[0]
        bad = (fs.hi + abs(fs.hi) + 1.0) if fs.hi is not None else (
            (fs.lo - abs(fs.lo) - 1.0) if fs.lo is not None else "__not_an_option__")
        try:
            self.make(**{name: bad})
        except (ConfigError, ValueError, TypeError):
            return f"bounded field {name!r} rejected {bad!r} at construction"
        raise AssertionError(
            f"the plugin declares {name!r} with bounds lo={fs.lo} hi={fs.hi} but accepted {bad!r} "
            f"at construction; an out-of-range value must be fatal before step 0")

    def check_C11_float_hygiene(self):
        """Outputs finite, and EQUAL after `round(x, PRECISION)`.

        Not a restatement of C1. C1 asks whether the model repeats itself; C11 asks whether its
        output survives being written to a file and read back at the precision it declares. A model
        whose distinguishing information lives below its own declared precision has a determinism
        contract that the dataset cannot keep, and `det[k] >= 1.0` downstream is a CLIFF -- a
        last-ulp difference flips a whole report.
        """
        fr = self.frames()
        a = self._trace(self.make(), fr)
        b = self._trace(self.make(), fr)
        ra = [_round_row(r, self.PRECISION) for r in a]
        rb = [_round_row(r, self.PRECISION) for r in b]
        assert ra == rb, _first_diff(ra, rb)
        for r in a:
            assert H.finite(r[3]) and H.finite(r[5]), f"non-finite output in {r!r}"

    # ===================================================================================== #
    # the acceptance check
    # ===================================================================================== #
    def check_C12_pipeline_two_run_digest(self):        # noqa: D401 - see the docstring below
        """The whole engine, twice, byte-identical.

        C1 is the unit-level statement; this is the artifact-level one, and it is the layer the
        design pairs with the content-hash lock: no identity drift plus digest drift means the plugin
        is nondeterministic, identity drift plus no digest drift means a harmless refactor. Neither
        layer can say that alone.
        """
        if self.REF is None:
            _skip("C12 needs a config-declarable REF -- an anonymous make() cannot be replayed")
        import tempfile
        run_pipeline, PipelineConfig = _engine().run_pipeline, _engine().PipelineConfig
        plugins = {"channel_model": {"ref": self.REF, "params": dict(self.PARAMS)}}
        with tempfile.TemporaryDirectory(prefix="scms_conf_") as td:
            digests = []
            for name in ("a", "b"):
                res = run_pipeline(PipelineConfig(
                    seed=self.SEED % 100000, traffic_flow=True, road_network="grid",
                    duration_s=30, arrival_rate=1.2, grid_w=4, grid_h=4, attacker_pct=0.25,
                    packet_loss_base=0.05, radio_range_m=self.RADIO_RANGE_M,
                    plugins=plugins, out_dir=f"{td}/{name}"))
                digests.append(res.data_digest)
        assert digests[0] == digests[1], (
            f"two identical runs produced different datasets: {digests[0]} != {digests[1]}")
        return f"data_digest={digests[0]}"

    def check_C13_config_not_mutated(self):
        """The MANIFEST REPLAY contract, asserted against plugin code: the model does not write to
        the run's configuration.

        `env["config"]` is the run's `PipelineConfig`, and `_write_manifest` serialises
        `cfg.__dict__` at the END of the run. So a plugin that writes one attribute through it --
        `env["config"].report_prob = 1.0`, at construction or at step 30 -- makes the artifact
        describe a config that did not produce it: the steps before the write ran under the user's
        value and the steps after under the plugin's, and the manifest records only the second.
        Measured before the fix, on the 60-step acceptance config with the reference plugin: the run
        completed at exit 0, its `data_digest` was byte-identical to the honest control (so the
        pinned-golden layer sees nothing at all), `provenance_digest` was unchanged, `verify-plugins`
        reported OK, and the manifest replayed to a DIFFERENT dataset.

        THREE arms, and the third is the one that actually holds:

        1. **construction** -- a write during `__init__`, the cheapest place to do it.
        2. **the run** -- a write at step k > 0, which no construction-time gate can see.
        3. **the snapshot**, which is arm 1 and 2's backstop. Arms 1 and 2 recognise the
           `ReadOnlyConfig` refusal, i.e. they see a plugin that went through the reference the
           engine handed it. Python cannot make that reference unforgeable -- `env` is a plain dict,
           and the real object is reachable through this view's own slot, through `gc.get_objects()`
           or through a frame walk -- so the enforceable statement is not "you cannot write" but
           "if the config moved, this is not a conformant model". The dict comparison is that
           statement, and it does not care how the write was performed.

        Stated the same way C5 states PEP 578 and C3 states capability-by-omission: this is the
        detection layer. The engine's `_assert_config_unmoved` is the one that refuses to write an
        artifact, and the two are deliberately independent.
        """
        _engine()
        try:
            model = self.make()
        except ConfigError as e:
            if "READ-ONLY" in str(e):
                raise AssertionError(
                    f"the model wrote to the run's configuration through env['config'] AT "
                    f"CONSTRUCTION. The manifest records `cfg.__dict__` at the end of the run, so "
                    f"the artifact would describe a config that did not produce it. Declare the "
                    f"knob through your own config_fields() and read it from `params`. "
                    f"[{' '.join(str(e).split())[:200]}]") from None
            raise
        cfg = getattr(self, "_env_cfg", None)
        if cfg is None:                                     # a contract whose env() is fully custom
            _skip("this contract's env() does not build a PipelineConfig, so there is nothing "
                  "for C13 to watch")
        before = self._env_cfg_before
        _assert_unmoved(before, cfg, "during CONSTRUCTION")
        # 12 contiguous steps -- past the horizon of every other scenario in this suite (the longest
        # is C9's 8-step ladder) -- and then a four-frame tail at step 10_000. The tail is the
        # important half: the realistic shape of this bug is `if frame.step >= 30`, guarded so the
        # write lands once the run has settled, and NO contiguous window shorter than 31 steps can
        # see that. Jumping the step label catches every threshold below 10_000 for the price of
        # four frames. It is still a bounded probe, and that is stated rather than papered over:
        # the unbounded statement is the engine's `_assert_config_unmoved`, which compares the real
        # config at the real end of the real run and refuses to write a manifest if it moved.
        frames = self.frames(n_steps=12) + build_frames(n_steps=4, n_stations=12, spacing_m=55.0,
                                                        move_m=7.0, step0=10_000)
        try:
            self._trace(model, frames)
        except ConfigError as e:
            if "READ-ONLY" in str(e):
                raise AssertionError(
                    f"the model wrote to the run's configuration through env['config'] DURING THE "
                    f"RUN. The steps before the write ran under one config and the steps after "
                    f"under another, so no config describes the dataset -- and the manifest records "
                    f"the one that produced only its tail. "
                    f"[{' '.join(str(e).split())[:200]}]") from None
            raise
        _assert_unmoved(before, cfg, "during the run")
        return "config unmoved across construction, 12 steps and a tail at step 10000"

    # ===================================================================================== #
    # pytest surface -- one thin method per check, all sharing _run()
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

    def test_C1_repeatable(self):
        self._run("C1_repeatable")

    def test_C2_call_order_independent(self):
        self._run("C2_call_order_independent")

    def test_C3_global_rng_untouched(self):
        self._run("C3_global_rng_untouched")

    def test_C4_state_advances_once_per_step(self):
        self._run("C4_state_advances_once_per_step")

    def test_C5_no_io(self):
        self._run("C5_no_io")

    def test_C6_no_oracle_leak(self):
        self._run("C6_no_oracle_leak")

    def test_C6b_oracle_invariance(self):
        self._run("C6b_oracle_invariance")

    def test_C7_ranges(self):
        self._run("C7_ranges")

    def test_C8_reach_honesty(self):
        self._run("C8_reach_honesty")

    def test_C9_monotone_in_distance(self):
        self._run("C9_monotone_in_distance")

    def test_C10_fail_fast(self):
        self._run("C10_fail_fast")

    def test_C11_float_hygiene(self):
        self._run("C11_float_hygiene")

    def test_C12_pipeline_two_run_digest(self):
        self._run("C12_pipeline_two_run_digest")

    def test_C13_config_not_mutated(self):
        self._run("C13_config_not_mutated")


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
#: Mirrors `run._CHANNEL_ENV_KEYS`. Restated rather than imported so the contract can be constructed
#: without pulling the engine in at module scope; `_config()` imports it lazily when it is needed.
_CHANNEL_ENV_KEYS = ("radio_range_m", "pathloss_exponent", "shadowing_sigma_db",
                     "rx_sensitivity_margin_db", "radio_cap_sigma", "radio_cap_max_mult",
                     "radio_env", "radio_tx_power_dbm", "radio_rx_sensitivity_dbm",
                     "radio_nlosb_density_per_km", "chan_capacity", "packet_loss_base",
                     "nlos_loss", "seed", "dt")

_DECLARED_FIELDS = ("vid", "x", "y", "ant_h_m", "blocker_h_m", "is_rsu", "is_vru", "tx_power_dbm",
                    "rx_range_m")


def _engine():
    """The engine module, imported LAZILY and once.

    Two reasons it is not a module-level import. The built-in registry is populated as a SIDE EFFECT
    of importing `run.py` (the `register_builtin` loop at its `DiscChannel`/`LogDistanceChannel`/
    `GeometricChannel` definitions), so grading a built-in requires the engine to have been imported
    -- and grading a THIRD-PARTY plugin should not pay for that import until C12 needs a pipeline.
    And the contract must stay importable by a plugin author whose install closure is the api
    package, not the whole engine.
    """
    from ...mock_pipeline import run as _run
    return _run


def _declared(st):
    return tuple(getattr(st, f) for f in _DECLARED_FIELDS)


def _config_snapshot(cfg) -> dict:
    """`cfg.__dict__` in the shape `manifest["config"]` carries it.

    Delegates to the engine's own `_config_dict` when it is importable, so C13 and the engine's
    `_assert_config_unmoved` can never disagree about what "the config" is -- the same reason C7
    imports its dBm bounds from `api.channel` instead of restating them. The fallback is that
    function's body, for a contract used without the engine on the path.
    """
    fn = getattr(_engine(), "_config_dict", None)
    if fn is not None:
        return dict(fn(cfg))
    return {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()}


def _assert_unmoved(before: dict, cfg, when: str) -> None:
    """C13's snapshot arm: the config dict is field-for-field what it was."""
    now = _config_snapshot(cfg)
    moved = sorted(k for k in set(before) | set(now) if before.get(k) != now.get(k))
    if not moved:
        return
    detail = "; ".join(f"{k}: {before.get(k)!r} -> {now.get(k)!r}" for k in moved[:5])
    raise AssertionError(
        f"the run configuration MOVED {when}: {detail}"
        + (f" (and {len(moved) - 5} more)" if len(moved) > 5 else "")
        + ". Only the model can have done this -- nothing else touched the object. It did not go "
          "through env['config'] (that view refuses writes), so it reached the real config another "
          "way: this view's own slot, a frame walk, or gc. The dataset is unreplayable either way, "
          "because the manifest records cfg.__dict__ as it stands at the END of the run.")


def _config_fields(cls) -> dict:
    fn = getattr(cls, "config_fields", None)
    if not callable(fn):
        return {}
    try:
        return dict(fn() or {})
    except TypeError:                                       # a plain instance method -> unreachable
        return {}


def _validate_params(cls, params) -> None:
    """The same gate `run._validate_plugins` applies, so `make()` and the engine agree on what is
    a legal parameter set -- a contract that accepted params the engine refuses would be lying."""
    spec = _config_fields(cls)
    for k, v in (params or {}).items():
        fs = spec.get(k)
        if fs is None:
            raise ConfigError(f"{cls.__name__} declares no field {k!r} "
                              f"(declared: {sorted(spec)})")
        fs.validate(k, v)
    own = getattr(cls, "validate_params", None)
    if callable(own):
        try:
            own(dict(params or {}))
        except TypeError:                                   # pragma: no cover - instance method
            pass


def _round_row(row, precision):
    step, ti, rv, rssi, state, delay, extras = row
    return (step, ti, rv,
            None if rssi is None else round(float(rssi), precision),
            state, round(float(delay), precision),
            tuple((k, round(float(v), precision)) for k, v in extras))


def _first_diff(a, b) -> str:
    if len(a) != len(b):
        return f"trace lengths differ: {len(a)} vs {len(b)}"
    for i, (x, y) in enumerate(zip(a, b)):
        if x != y:
            return f"first difference at row {i}: {x!r} != {y!r}"
    return "traces are equal"


def dbm_ok(x) -> bool:                                      # pragma: no cover - convenience export
    return x is None or (math.isfinite(x) and RSSI_MIN_DBM <= x <= RSSI_MAX_DBM)
