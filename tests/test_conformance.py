"""Phase 2: the conformance suite (C1-C13) and the provenance lock.

The point of a conformance suite is that it can FAIL, so most of what is pinned here is that each
check catches the specific thing it was built to catch. Three properties, in the order they matter:

* **Non-tautological.** Every check is graded against a model that violates it, written inline, and
  each violator must fail its own check WHILE PASSING the others. A suite where a bad model fails
  everything proves only that something is wrong; the diagnostic value is in which one fired.
* **The two detection layers are INDEPENDENT.** A nondeterministic model is caught by C1 and by the
  two-run digest gate; a leaky model is caught by C6b and is INVISIBLE to the digest gate (it is
  perfectly reproducible). Neither layer subsumes the other, and the design's whole diagnosis table
  is built out of the difference.
* **The built-ins are graded by their own suite.** All three pass, and `logdistance` declares a
  waiver for C8 -- data supplied by the implementation, with a written justification, in Django's
  `django_test_skips` shape. That waiver documents a real acceptanceRangeThreshold false-positive
  channel; discovering it is what the suite is for.

The out-of-repo half of the acceptance gate (a plugin in its own installed distribution) lives at
`C:/Temp/scms_plugin_demo` and is documented, with its measured evidence, in
`docs/realism/PLUGIN-CONFORMANCE-EVIDENCE.md`. It is deliberately NOT vendored here: a plugin
demonstration that lives inside the repository proves nothing about forking.
"""
from __future__ import annotations

import json
import random
import sys

import pytest

from scms_sim_ref import api
from scms_sim_ref.api import channel as apichan
from scms_sim_ref.conformance import ChannelModelContract, run_contract, run_ref
from scms_sim_ref.conformance.v1 import harness as H
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import run as RM

DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


# --------------------------------------------------------------------------- #
# A minimal, honest reference model, and one violator per check.
# Defined in-process (not installed) because these grade the SUITE; the installed-distribution
# acceptance gate is the separate out-of-repo demo.
# --------------------------------------------------------------------------- #
class Good:
    """Hard-range + Rayleigh-ish fade. Deterministic, identity-keyed, oracle-blind."""

    interface_version = apichan.INTERFACE_VERSION
    plugin_id = "goodchan"

    @classmethod
    def config_fields(cls):
        from scms_sim_ref.api.fields import FieldSpec
        return {"range_m": FieldSpec("float", 500.0, "hard reach", lo=10.0, hi=2000.0, unit="m")}

    def __init__(self, *, params, rng, env):
        self.reach_m = float((params or {}).get("range_m", env.get("radio_range_m", 500.0)))
        self._rng = rng
        self.step = -1

    def capabilities(self):
        return frozenset({"rssi", "reach", apichan.LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame):
        self.step = frame.step

    def reach_m_for(self, rx):
        return rx.rx_range_m or self.reach_m

    def _rx_dbm(self, tx, rx, d_m):
        import math
        u = self._rng.stream("fade", tx.vid, rx.vid).random()
        fade = 10.0 * math.log10(max(-math.log(max(1.0 - u, 1e-300)), 1e-12))
        return 23.0 - (47.87 + 24.0 * math.log10(max(d_m, 1.0))) + fade

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        p = self._rx_dbm(tx, rx, d_m)
        return apichan.LinkOutcome(rssi_dbm=p) if p >= -95.0 else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return self._rng.persistent("coin", tx_vid, rx_vid).random()


class UsesGlobalRng(Good):
    """Violates C3: draws from the module-level `random`, the exact thing D3 forbids."""
    plugin_id = "globalrng"

    def _rx_dbm(self, tx, rx, d_m):
        return -60.0 - 20.0 * random.random()


class MonkeypatchesRandom(Good):
    """Violates C3 by REBINDING `random.Random.random` at construction.

    The vector capability-by-omission cannot close by omitting anything: it reaches every stream in
    the process at once -- including the engine's private `random.Random(cfg.seed)` -- while every
    state snapshot a check could take stays perfectly intact. Measured against the real engine, this
    shape scored 13/13 on the old suite and MOVED the data digest. The wrapper is a pure passthrough
    here, so this fixture violates C3 and nothing else.
    """
    plugin_id = "patcher"

    def __init__(self, *, params, rng, env):
        super().__init__(params=params, rng=rng, env=env)
        original = random.Random.random
        if getattr(original, "_scms_test_wrapper", False):
            return                                   # idempotent: every check builds a fresh model

        def wrapper(inner_self, *a, **kw):
            return original(inner_self, *a, **kw)
        wrapper._scms_test_wrapper = True
        random.Random.random = wrapper


class WalksTheStack(Good):
    """Violates C3 by frame-walking to a caller's local named `rng` and drawing from it.

    `run_pipeline` holds the engine's shared stream in a local of exactly that name. The draw is
    discarded, so this model's OWN output is unchanged -- which is the point: it perturbs the
    engine's stream (report_prob, collusion, net_delay, emit sampling) and passes every other check.
    """
    plugin_id = "stackwalk"

    def _rx_dbm(self, tx, rx, d_m):
        f = sys._getframe(1)
        while f is not None:
            candidate = f.f_locals.get("rng")
            if isinstance(candidate, random.Random):
                candidate.random()
                break
            f = f.f_back
        return super()._rx_dbm(tx, rx, d_m)


class OrderDependent(Good):
    """Violates C2: one shared sequential stream, so a link's value depends on when it was asked
    for. Passes C1 -- it repeats perfectly, as long as nothing reorders the loop."""
    plugin_id = "orderdep"

    def __init__(self, *, params, rng, env):
        super().__init__(params=params, rng=rng, env=env)
        self._shared = rng.persistent("shared")

    def _rx_dbm(self, tx, rx, d_m):
        return -60.0 - 20.0 * self._shared.random()


class TwicePerStep(Good):
    """Violates C4: declares `stateful` but advances its per-link state on EVERY call instead of
    once per step, so two PDUs on one link in one step see two different channels."""
    plugin_id = "twicestep"

    def capabilities(self):
        return frozenset({"rssi", "reach", "stateful", apichan.LOSS_INDEPENDENT_SURVIVAL})

    def _rx_dbm(self, tx, rx, d_m):
        r = self._rng.persistent("shadow", tx.vid, rx.vid)
        return -60.0 + r.gauss(0.0, 4.0) + (r.gauss(0.0, 1.0) if self.step % 2 else 0.0)


class WritesFiles(Good):
    """Violates C5: memoises to a temp file, which is how a model quietly becomes host-dependent."""
    plugin_id = "writesfile"

    def _rx_dbm(self, tx, rx, d_m):
        import tempfile
        with tempfile.NamedTemporaryFile("w", delete=False, suffix=".cache") as fh:
            fh.write("%d\n" % tx.vid)
        return super()._rx_dbm(tx, rx, d_m)


class LeakyKeys(Good):
    """Violates C6: emits an `extras` column named after ground truth."""
    plugin_id = "leakykeys"

    def evaluate(self, tx, rx, d_m, txn):
        out = super().evaluate(tx, rx, d_m, txn)
        return out if out is None else apichan.LinkOutcome(
            rssi_dbm=out.rssi_dbm, extras={"is_attacker": 1.0})


class OracleReader(Good):
    """Violates C6b, and NOTHING else. Deterministic, plausible, in range, monotone -- and every
    number it emits is a laundered attacker label."""
    plugin_id = "oracleread"

    def evaluate(self, tx, rx, d_m, txn):
        out = super().evaluate(tx, rx, d_m, txn)
        if out is None or not getattr(tx, "is_attacker", False):
            return out
        return apichan.LinkOutcome(rssi_dbm=out.rssi_dbm - 12.0)


class BadUnits(Good):
    """Violates C7: returns received power in linear milliwatts through a field documented as dBm --
    a units bug no name-based check can see, because the column name is right."""
    plugin_id = "badunits"

    def evaluate(self, tx, rx, d_m, txn):
        out = super().evaluate(tx, rx, d_m, txn)
        return out if out is None else apichan.LinkOutcome(rssi_dbm=10.0 ** (out.rssi_dbm / 10.0))


class OverReaches(Good):
    """Violates C8 AND ONLY C8: delivers past its own declared reach, which is exactly what makes
    acceptanceRangeThreshold wrong for every honest long link.

    It keeps `Good`'s distance-dependent physics and merely drops the decode floor, so the only
    contract statement it breaks is the reach one. (It used to return a CONSTANT -70.0 dBm at every
    distance; C9 could not see that, because C9 compared adjacent rungs with `<=` and equality
    satisfies `<=`. C9's attenuation arm sees it now, which would make this fixture a two-check
    violator and defeat the "and only that" half of the suite's own grading test.)"""
    plugin_id = "overreach"

    def window_m(self, rx):
        return 3.0 * self.reach_m

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > 3.0 * self.reach_m:
            return None
        p = self._rx_dbm(tx, rx, d_m)
        return apichan.LinkOutcome(rssi_dbm=p) if p >= -125.0 else None


class Antimonotone(Good):
    """Violates C9's ADJACENT arm: PDR RISES steeply with distance."""
    plugin_id = "antimono"

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        frac = d_m / max(self.reach_m, 1.0)
        u = self._rng.stream("keep", tx.vid, rx.vid).random()
        return apichan.LinkOutcome(rssi_dbm=-90.0 + 30.0 * frac) if u < frac else None


class _LadderQuantised(Good):
    """Base for the two C9 fixtures whose whole point is an EXACT delivery ratio.

    C9's ladder offers exactly `n_tx * n_steps` = 96 candidate links per rung, one per
    `(step, tx_index)` pair. Selecting on that pair instead of tossing a coin makes the measured PDR
    exact rather than binomial, so a fixture built to sit a stated distance either side of a stated
    tolerance sits there deterministically instead of on a sampling distribution that straddles it.
    """
    _SLOTS = 96
    _N_TX = 12

    def _slot(self, txn) -> int:
        return (max(self.step, 0) * self._N_TX + txn.tx_index) % self._SLOTS

    def _deliver(self, txn, p: float) -> bool:
        return self._slot(txn) < round(self._SLOTS * p)


class RisesWithinAdjacentTolerance(_LadderQuantised):
    """Violates C9's CUMULATIVE arm and nothing else.

    PDR rises by exactly 0.08 per ladder rung: under the 0.10 adjacent-rung tolerance at EVERY one
    of the six steps, and +0.48 end to end. The adjacent arm is structurally incapable of seeing it,
    which is the whole reason the cumulative arm exists. RSSI falls honestly, so the rssi arms and
    the attenuation arm all pass and the failure is attributable to one arm.
    """
    plugin_id = "creeper"
    #: the ladder fractions C9 uses, so the model can tell which rung it is standing on
    _FRACS = (0.05, 0.12, 0.25, 0.40, 0.60, 0.80, 0.95)

    def evaluate(self, tx, rx, d_m, txn):
        import math
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        frac = d_m / max(self.reach_m, 1.0)
        rung = max(0, sum(1 for f in self._FRACS if frac >= f - 1e-9) - 1)
        if not self._deliver(txn, 0.30 + 0.08 * rung):
            return None
        return apichan.LinkOutcome(rssi_dbm=-70.0 - 20.0 * math.log10(max(d_m, 1.0)))


class ConstantInDistance(_LadderQuantised):
    """Violates C9's ATTENUATION arm and nothing else.

    A flat 50 % delivery probability and a constant -70.0 dBm whether the link is 25 m or 475 m --
    physically impossible, and invisible to any arm built out of `<=` comparisons, because equality
    satisfies every one of them. This is the shape the check could not fail before.
    """
    plugin_id = "constdist"

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        return apichan.LinkOutcome(rssi_dbm=-70.0) if self._deliver(txn, 0.5) else None


class AcceptsAnything(Good):
    """Violates C10: swallows a parameter its own FieldSpec declares out of range, so the failure
    surfaces at step k > 0 as a physically absurd run rather than before step 0 as an error."""
    plugin_id = "acceptsany"

    @classmethod
    def config_fields(cls):
        return {}                      # declares nothing -> the framework has nothing to enforce

    def __init__(self, *, params, rng, env):
        super().__init__(params={}, rng=rng, env=env)


class MutatesTheConfig(Good):
    """The replay-contract vector: writes to the run's config through `env["config"]`.

    Physics identical to `Good`. `_write_manifest` serialised the config at the END of the run, so
    before the fix this ran to completion at exit 0 with a manifest recording `report_prob = 1.0` --
    not the `0.9` the user's config asked for. Measured against the reference plugin on the 60-step
    acceptance config: the run's own `data_digest` was byte-identical to the honest control (so the
    pinned-golden layer sees nothing), and replaying the manifest at the `1.0` it records produced a
    DIFFERENT dataset. The manifest did not replay to the dataset it described.
    """
    plugin_id = "cfgwriter"
    WRITE_AT_STEP = 3

    def __init__(self, *, params, rng, env):
        super().__init__(params=params, rng=rng, env=env)
        self._cfg = env["config"]

    def begin_step(self, frame):
        super().begin_step(frame)
        if frame.step == self.WRITE_AT_STEP:
            self._cfg.report_prob = 1.0


class MutatesTheConfigLate(MutatesTheConfig):
    """C13's violator: the same write, at step 8.

    Step 8 is past the horizon of every other scenario in the suite (the longest is C9's 8-step
    ladder, steps 0..7) and inside C13's own 12-step window, so this model fails C13 and NOTHING
    ELSE. That is not a convenience: a writer at step 0 makes every check that constructs a model
    error out -- correct behaviour, but it proves nothing about WHICH check saw it, and
    attributability is the whole diagnostic value of the suite.
    """
    plugin_id = "cfgwriterlate"
    WRITE_AT_STEP = 8


class MutatesTheConfigOnceSettled(MutatesTheConfig):
    """C13's high-step-tail violator: `if frame.step >= 30`, which is the REALISTIC shape.

    A plugin author who writes to the config does it once the run has settled, not at step 0. No
    contiguous conformance window shorter than 31 steps can see that, so C13 drives a four-frame
    tail at step 10 000 after its twelve contiguous ones: one cheap jump in the step label catches
    every threshold below it. Verbatim the shape of the out-of-repo `LateConfigMutator` fixture,
    which writes `report_prob = 1.0` at step 30 of a 60-step run and, before the fix, produced a
    manifest at exit 0 that replayed to a different dataset.
    """
    plugin_id = "cfgsettled"
    WRITE_AT_STEP = -1                      # never equal to a step: the guard below is >=, not ==

    def begin_step(self, frame):
        Good.begin_step(self, frame)
        if frame.step >= 30:
            self._cfg.report_prob = 1.0


class MutatesTheConfigBehindTheView(Good):
    """C13's arm-3 violator: reaches the REAL config around the read-only view and writes silently.

    `ReadOnlyConfig` keeps the live object in a slot, so `env["config"]._ReadOnlyConfig__cfg` is the
    dataclass itself -- and `gc.get_objects()` and a frame walk are two more routes to it. Python
    cannot make a reference unforgeable. This model therefore raises NOTHING: no view refuses it, no
    exception is logged, and the only thing that can see the write is the before/after dict
    comparison. That is why C13 has a snapshot arm rather than merely catching the view's own error.
    """
    plugin_id = "cfgsneak"

    def __init__(self, *, params, rng, env):
        super().__init__(params=params, rng=rng, env=env)
        view = env["config"]
        self._real = getattr(view, "_ReadOnlyConfig__cfg", view)

    def begin_step(self, frame):
        super().begin_step(frame)
        if frame.step == 8:
            object.__setattr__(self._real, "report_prob", 1.0)


_VIOLATORS = {
    "C3_global_rng_untouched": UsesGlobalRng,
    "C2_call_order_independent": OrderDependent,
    "C4_state_advances_once_per_step": TwicePerStep,
    "C5_no_io": WritesFiles,
    "C6_no_oracle_leak": LeakyKeys,
    "C6b_oracle_invariance": OracleReader,
    "C7_ranges": BadUnits,
    "C8_reach_honesty": OverReaches,
    "C9_monotone_in_distance": Antimonotone,
    "C13_config_not_mutated": MutatesTheConfigLate,
}


def _contract(model_cls, **attrs):
    """A contract bound to an in-process class -- `make()` is overridden, so no REF is needed and
    the pipeline-level C12 skips (an anonymous class cannot be named in a replayable config)."""
    class _C(ChannelModelContract):
        def make(self, **params):
            p = dict(self.PARAMS)
            p.update(params)
            from scms_sim_ref.conformance.v1.channel import _validate_params
            _validate_params(model_cls, p)
            self._ns = api.RngNamespace(self.SEED, model_cls.plugin_id)
            return model_cls(params=p, rng=self._ns, env=self.env())
    for k, v in attrs.items():
        setattr(_C, k, v)
    return _C()


# --------------------------------------------------------------------------- the suite grades ---- #
def test_the_reference_model_passes_every_check_it_can_run():
    rep = run_contract(_contract(Good))
    assert rep.ok, rep.to_text()
    statuses = {r["check"]: r["status"] for r in rep.rows}
    assert statuses["C12_pipeline_two_run_digest"] == "SKIP"     # anonymous class -> not replayable
    assert rep.count("FAIL") == 0 and rep.count("ERROR") == 0
    assert rep.count("PASS") >= 11


@pytest.mark.parametrize("check_id,cls", sorted(_VIOLATORS.items()))
def test_each_check_catches_its_own_violator_and_only_that(check_id, cls):
    """Non-tautology, one row at a time. A violator must fail the check it violates -- and, for the
    checks whose failure is genuinely local, nothing else."""
    rep = run_contract(_contract(cls))
    statuses = {r["check"]: r["status"] for r in rep.rows}
    assert statuses[check_id] == "FAIL", rep.to_text()
    assert not rep.ok
    # C6b's violator is the important one: it must fail ONLY C6b, because every other signal a
    # reviewer or a linter could look at is clean.
    if check_id == "C6b_oracle_invariance":
        failed = {c for c, s in statuses.items() if s == "FAIL"}
        assert failed == {"C6b_oracle_invariance"}, rep.to_text()


@pytest.mark.parametrize("cls,marker", [
    (MonkeypatchesRandom, "REBOUND"),
    (WalksTheStack, "walked the call stack"),
])
def test_C3_catches_the_two_vectors_that_actually_move_the_engines_stream(cls, marker):
    """The two RNG attacks that used to score 13/13.

    Measured against the real engine before the traps were added: both of these MOVED the pipeline's
    `data_digest` (ma_reports 3203 -> 3147 rows, ma_investigations and ma_crl_events 25 -> 24, i.e.
    they perturbed report_prob, revocation and emit sampling), and both passed conformance, including
    `conformance="required"` attestation. Meanwhile the two vectors C3 DID catch -- a module-level
    `random.random()` and a `random.seed()` -- left the digest unchanged, because the engine's stream
    is a private `random.Random` instance a module-level draw cannot reach. The check was detecting
    exactly the harmless half.

    Each must fail C3 alone: a blanket tightening that fails everything proves nothing about which
    property broke.
    """
    surface = H.random_class_surface()
    try:
        rep = run_contract(_contract(cls))
        statuses = {r["check"]: r["status"] for r in rep.rows}
        failed = {c for c, s in statuses.items() if s in ("FAIL", "ERROR")}
        assert failed == {"C3_global_rng_untouched"}, rep.to_text()
        row, = [r for r in rep.rows if r["check"] == "C3_global_rng_untouched"]
        assert marker in row["detail"], row["detail"]
    finally:                                       # belt and braces; run_contract restores it too
        H.restore_random_class_surface(surface)


def test_the_runner_leaves_the_interpreter_a_hostile_plugin_patched_repaired():
    """A suite that detects a tamper and leaves it installed has poisoned everything that runs
    after it -- a worse outcome than not checking. `run_contract` restores in `finally`."""
    before = random.Random.__dict__.get("random")
    surface = H.random_class_surface()
    try:
        rep = run_contract(_contract(MonkeypatchesRandom))
        assert not rep.ok                                  # it WAS caught ...
        assert random.Random.__dict__.get("random") is before      # ... and it was put back
        assert not getattr(random.Random.random, "_scms_test_wrapper", False)
    finally:
        H.restore_random_class_surface(surface)


@pytest.mark.parametrize("cls,arm", [(RisesWithinAdjacentTolerance, "cumulative"),
                                     (ConstantInDistance, "attenuation")])
def test_C9_catches_what_adjacent_rung_comparison_structurally_cannot(cls, arm):
    """The two C9 shapes that passed 12/1 before the arms were added.

    A model whose PDR creeps up by less than the per-rung tolerance at every rung, and a model that
    is simply CONSTANT in distance, both satisfy `pdr[i] <= pdr[i-1] + 0.10` at every step -- the
    second by exact equality. Neither is a channel. Both must FAIL, and each must fail C9 alone, so
    the new arms are attributable rather than a blanket tightening.
    """
    rep = run_contract(_contract(cls))
    statuses = {r["check"]: r["status"] for r in rep.rows}
    failed = {c for c, s in statuses.items() if s in ("FAIL", "ERROR")}
    assert failed == {"C9_monotone_in_distance"}, rep.to_text()
    row, = [r for r in rep.rows if r["check"] == "C9_monotone_in_distance"]
    assert arm in ("cumulative", "attenuation")
    marker = "FAR end of the ladder" if arm == "cumulative" else "CONSTANT in distance"
    assert marker in row["detail"], row["detail"]


def test_waivers_are_data_with_a_justification_not_a_mute_button():
    """Django's doctrine, enforced: a waiver without a written justification is refused outright,
    and a waived failure is reported as WAIVED WITH its justification, never as a pass."""
    rep = run_contract(_contract(OverReaches, waivers={
        "C8_reach_honesty": "this model is a soft-edge propagation model by design"}))
    row, = [r for r in rep.rows if r["check"] == "C8_reach_honesty"]
    assert row["status"] == "WAIVED"
    assert "soft-edge propagation model" in row["detail"]
    assert "underlying:" in row["detail"]          # the real failure travels with the excuse
    assert rep.ok and rep.waived == ["C8_reach_honesty"]
    assert rep.summary()["waivers"]["C8_reach_honesty"].startswith("this model is a soft-edge")
    with pytest.raises(ValueError, match="no written justification"):
        run_contract(_contract(OverReaches, waivers={"C8_reach_honesty": ""}))


# --------------------------------------------------------------------------- the built-ins ------- #
@pytest.mark.parametrize("ref", ["disc", "logdistance", "geometric"])
def test_every_builtin_passes_its_own_conformance_suite(ref):
    rep = run_ref("channel_model", ref)
    assert rep.ok, rep.to_text()
    assert rep.count("ERROR") == 0


def test_logdistance_declares_its_C8_waiver_with_a_quantified_justification():
    """The suite found a real defect and the built-in declares it rather than the suite hiding it:
    `logdistance`'s reach_m is a MEDIAN-range calibration, so a favourable shadow legitimately
    closes links past it -- and acceptanceRangeThreshold bounds on that same number."""
    rep = run_ref("channel_model", "logdistance")
    row, = [r for r in rep.rows if r["check"] == "C8_reach_honesty"]
    assert row["status"] == "WAIVED"
    assert "MEDIAN-range calibration" in row["detail"]
    assert "acceptanceRangeThreshold" in row["detail"]
    assert "14.13" in row["detail"]                # the justification carries measured numbers
    assert rep.ok
    # ...and it is the IMPLEMENTATION that ships the waiver, not the suite
    assert "C8_reach_honesty" in RM.LogDistanceChannel.conformance_waivers


def test_geometric_passes_the_stateful_step_guard_check():
    """C4 is only meaningful for a model that declares `stateful`; `geometric` is the in-tree one,
    and its `st["step"] == self.step` guard is the reference implementation the check encodes."""
    rep = run_ref("channel_model", "geometric")
    statuses = {r["check"]: r["status"] for r in rep.rows}
    assert statuses["C4_state_advances_once_per_step"] == "PASS", rep.to_text()
    assert statuses["C8_reach_honesty"] == "PASS"
    # disc and logdistance are stateless, so the check SKIPs rather than silently passing
    assert {r["check"]: r["status"] for r in run_ref("channel_model", "disc").rows
            }["C4_state_advances_once_per_step"] == "SKIP"


# --------------------------------------------------------------------------- the harness --------- #
def test_draw_counter_counts_top_level_calls_not_rejection_samples():
    """The re-entrancy guard is what makes C4 measurable at all: `gammavariate` is rejection
    sampling and calls `random()` a VARIABLE number of times, so counting primitives would report a
    correct step guard as a violation."""
    counts = []
    for _ in range(6):
        with H.DrawCounter() as dc:
            r = random.Random(11)
            for _k in range(20):
                r.gammavariate(2.5, 0.4)
        counts.append(dc.count)
    assert counts == [20] * 6
    # and the class is left exactly as it was found
    assert random.Random(1).random() == random.Random(1).random()


def test_audit_guard_detects_writes_sockets_and_subprocesses_but_allows_reads():
    from scms_sim_ref.conformance.v1.harness import IoViolation, audit_guard
    import tempfile
    with audit_guard() as g:
        open(__file__, encoding="utf-8").close()       # a READ is allowed
    assert not g.hits
    with pytest.raises(IoViolation):
        with audit_guard():
            with tempfile.NamedTemporaryFile("w", delete=False):
                pass
    # ...and the guard is disarmed on exit, so the rest of the suite is unaffected
    with tempfile.NamedTemporaryFile("w", delete=False):
        pass


def test_the_ladder_holds_distance_exactly_while_moving_the_endpoints():
    """C9 measures distance and nothing else only because the ladder ROTATES: a ladder that did not
    move would report one correlated shadowing trajectory as n_tx * n_steps independent samples."""
    import math
    frames = H.build_ladder(250.0, n_tx=8, n_steps=5)
    moved = 0.0
    prev = None
    for f in frames:
        rx = f.stations[0]
        for vid, st in f.stations.items():
            if vid == 0:
                continue
            assert abs(math.hypot(st.x - rx.x, st.y - rx.y) - 250.0) < 1e-9
        here = f.stations[1]
        if prev is not None:
            moved += math.dist((here.x, here.y), prev)
        prev = (here.x, here.y)
    assert moved > 4 * 25.0            # tens of metres per step -> shadowing decorrelates


def test_oracle_station_is_field_identical_to_a_plain_snapshot():
    """C6b is only a valid experiment if the two frame sequences really are indistinguishable
    through the DECLARED interface. Asserted, not assumed."""
    from scms_sim_ref.conformance.v1.channel import _DECLARED_FIELDS, _declared
    a = H.build_frames(n_steps=3, n_stations=6, oracle=False)
    b = H.build_frames(n_steps=3, n_stations=6, oracle=True)
    for fa, fb in zip(a, b):
        assert [_declared(s) for s in fa.stations.values()] == \
               [_declared(s) for s in fb.stations.values()]
        assert fa.transmissions == fb.transmissions
    spiked = b[0].stations[0]
    assert spiked.is_attacker is True and hasattr(spiked, "falsified")
    assert not set(_DECLARED_FIELDS) & {"is_attacker", "falsified", "true_x", "attack_type"}


# --------------------------------------------------------------------------- the two layers ------ #
def test_the_two_detection_layers_are_independent(tmp_path):
    """THE point of the design's diagnosis table, in one test.

    A leaky model is byte-reproducible, so the digest gate is BLIND to it and only C6b sees it. The
    converse (a nondeterministic model that C1 catches and the digest gate also catches, by a
    different mechanism) is demonstrated end-to-end by the out-of-repo acceptance gate. Neither
    layer subsumes the other; a project with only pinned goldens cannot tell an oracle leak from
    good physics, and a project with only a unit-level suite cannot prove an ARTIFACT reproduces.
    """
    cfg = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=30, arrival_rate=1.2,
               grid_w=4, grid_h=4, attacker_pct=0.25)
    rep = run_contract(_contract(OracleReader))
    assert {r["check"] for r in rep.rows if r["status"] == "FAIL"} == {"C6b_oracle_invariance"}
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **cfg))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), **cfg))
    assert a.data_digest == b.data_digest        # the digest layer says "clean" about determinism
    assert a.data_digest


# ------------------------------------------------------- the engine-side gates a plugin meets ---- #
class GarbageOut(Good):
    """The outcome-validation vector: +9999 dBm (about 10^997 W) and an invented link state, on
    every delivered link, straight into the MA-visible `rssi_dbm` evidence column."""
    plugin_id = "garbageout"

    def capabilities(self):
        return frozenset({"rssi", "link_state", "reach", apichan.LOSS_INDEPENDENT_SURVIVAL})

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        return apichan.LinkOutcome(rssi_dbm=9999.0, link_state="TELEPATHY")


def _plugin_cfg(cls, tmp_path, name, **over):
    """A run configured to load `cls` by dotted path out of THIS test module."""
    ref = f"{cls.__module__}:{cls.__name__}"
    return PipelineConfig(seed=17, traffic_flow=True, road_network="grid", duration_s=20,
                          arrival_rate=1.5, grid_w=4, grid_h=4, attacker_pct=0.25,
                          out_dir=str(tmp_path / name),
                          plugins={"channel_model": {"ref": ref}}, **over)


def test_a_plugin_cannot_rewrite_the_config_the_manifest_records(tmp_path):
    """Constraint 2 (the manifest replay contract), enforced against plugin code.

    Two arms, because the two failure modes are different: the read-only view makes the write itself
    an error, and the snapshot comparison is what holds even if a plugin reaches the real object some
    other way. The run must NOT produce a manifest describing a config that did not produce it.
    """
    with pytest.raises(api.ConfigError, match="READ-ONLY"):
        run_pipeline(_plugin_cfg(MutatesTheConfig, tmp_path, "mut", report_prob=0.9))
    # ...and the snapshot arm, exercised directly: the gate does not depend on the view holding
    cfg = _plugin_cfg(Good, tmp_path, "snap", report_prob=0.9)
    before = RM._config_dict(cfg)
    cfg.report_prob = 1.0
    with pytest.raises(api.ConfigError, match="MUTATED"):
        RM._assert_config_unmoved(before, cfg, "during the run")


def test_C13_catches_the_config_write_the_engine_layer_only_sees_at_the_end():
    """C13, the conformance-side half of the same statement, and it must be ATTRIBUTABLE.

    The engine gate (`_assert_config_unmoved`) refuses to write an artifact; C13 tells a plugin
    author before they ever produce one. The two are independent on purpose -- conformance is off by
    default, and the engine gate cannot say WHICH property broke.
    """
    rep = run_contract(_contract(MutatesTheConfigLate))
    statuses = {r["check"]: r["status"] for r in rep.rows}
    failed = {c for c, s in statuses.items() if s in ("FAIL", "ERROR")}
    assert failed == {"C13_config_not_mutated"}, rep.to_text()
    row, = [r for r in rep.rows if r["check"] == "C13_config_not_mutated"]
    assert "DURING THE RUN" in row["detail"], row["detail"]


def test_C13_catches_the_realistic_write_at_step_30_that_no_short_window_can_see():
    """The high-step tail, and the reason it exists.

    `if frame.step >= 30` is what this bug actually looks like in the wild -- the write lands once
    the run has settled. Twelve contiguous steps cannot see it and neither can thirty; the fix is
    not a longer trace but a JUMP in the step label, four frames at step 10 000, which catches every
    threshold below that for the price of four frames. Measured against the out-of-repo
    `hostile.replay_break:LateConfigMutator` (physics identical to the reference plugin, writes
    `report_prob = 1.0` at step 30): 13 passed, 1 failed, the failure being C13 alone.
    """
    rep = run_contract(_contract(MutatesTheConfigOnceSettled))
    statuses = {r["check"]: r["status"] for r in rep.rows}
    failed = {c for c, s in statuses.items() if s in ("FAIL", "ERROR")}
    assert failed == {"C13_config_not_mutated"}, rep.to_text()
    # ...and the tail is genuinely what catches it: the contiguous window alone does not
    plain = H.build_frames(n_steps=12, n_stations=12, move_m=7.0)
    model = _contract(MutatesTheConfigOnceSettled).make()
    H.trace(model, plain)                                   # 12 steps -> no write, no error
    with pytest.raises(api.ConfigError, match="READ-ONLY"):
        H.trace(model, H.build_frames(n_steps=4, n_stations=12, move_m=7.0, step0=10_000))


def test_build_frames_step0_labels_the_frames_without_moving_the_geometry():
    """`step0` shifts the step LABEL only. If it also shifted the geometry, C13's tail would be a
    different scenario rather than the same one seen from a later step number."""
    a = H.build_frames(n_steps=4, n_stations=6, move_m=7.0)
    b = H.build_frames(n_steps=4, n_stations=6, move_m=7.0, step0=10_000)
    assert [f.step for f in a] == [0, 1, 2, 3]
    assert [f.step for f in b] == [10_000, 10_001, 10_002, 10_003]
    for fa, fb in zip(a, b):
        assert {v: (s.x, s.y) for v, s in fa.stations.items()} == \
               {v: (s.x, s.y) for v, s in fb.stations.items()}


def test_C13_catches_a_write_that_goes_AROUND_the_read_only_view():
    """Arm 3, the one that actually holds.

    `ReadOnlyConfig` makes the ACCIDENTAL and one-line-deliberate write a loud error. It cannot make
    the reference unforgeable -- the live object sits in the view's own slot, and `gc.get_objects()`
    and a frame walk reach it too. This model writes through the slot: nothing raises, nothing is
    logged, the physics is `Good`'s, and only the before/after dict comparison sees it.
    """
    rep = run_contract(_contract(MutatesTheConfigBehindTheView))
    statuses = {r["check"]: r["status"] for r in rep.rows}
    failed = {c for c, s in statuses.items() if s in ("FAIL", "ERROR")}
    assert failed == {"C13_config_not_mutated"}, rep.to_text()
    row, = [r for r in rep.rows if r["check"] == "C13_config_not_mutated"]
    assert "MOVED during the run" in row["detail"] and "report_prob" in row["detail"], row["detail"]


def test_C13_passes_the_honest_model_and_every_builtin():
    """A check that no honest model can pass is not a check. All three built-ins read `env['config']`
    at construction (`GeometricChannel.from_plugin`) and none of them writes to it."""
    assert run_contract(_contract(Good)).rows                      # smoke: the contract is runnable
    for ref in ("disc", "logdistance", "geometric"):
        rep = run_ref("channel_model", ref, {}, exclude=("C12_pipeline_two_run_digest",))
        row, = [r for r in rep.rows if r["check"] == "C13_config_not_mutated"]
        assert row["status"] == "PASS", f"{ref}: {row}"


def test_an_out_of_range_outcome_never_reaches_the_dataset(tmp_path):
    """C7's runtime form, on the DEFAULT third-party path.

    Conformance is off by default, so before the fix this completed at exit 0 with a valid manifest
    and every `ma_reports` row carrying `"rssi_dbm": 9999.0`. `check_outcome` existed and was called
    by nobody.
    """
    with pytest.raises(api.ConfigError, match="rssi_dbm out of range"):
        run_pipeline(_plugin_cfg(GarbageOut, tmp_path, "garbage"))
    # the same model still fails C7, so the two layers agree rather than one covering for the other
    rep = run_contract(_contract(GarbageOut))
    assert {r["check"] for r in rep.rows if r["status"] == "FAIL"} >= {"C7_ranges"}


def test_check_outcome_and_C7_publish_the_SAME_bounds():
    """One definition. A runtime gate whose band is wider than the check it is the runtime form of
    is a second, laxer contract wearing the same name."""
    from scms_sim_ref.conformance.v1 import channel as C9M
    assert (apichan.RSSI_MIN_DBM, apichan.RSSI_MAX_DBM) == (C9M.RSSI_MIN_DBM, C9M.RSSI_MAX_DBM)
    with pytest.raises(api.ConfigError):
        apichan.check_outcome(apichan.LinkOutcome(rssi_dbm=apichan.RSSI_MAX_DBM + 1.0))
    with pytest.raises(api.ConfigError):
        apichan.check_outcome(apichan.LinkOutcome(link_state="TELEPATHY"))


# --------------------------------------------------------------------------- CLI + lock ---------- #
def test_verify_plugins_cli_round_trips_a_real_manifest(tmp_path, capsys):
    run_pipeline(PipelineConfig(seed=5, n_steps=12, n_vehicles=8, out_dir=str(tmp_path / "r")))
    man = str(tmp_path / "r" / "manifest.json")
    assert RM.main(["verify-plugins", man]) == 0
    out = capsys.readouterr().out
    assert "no drift" in out and "provenance_digest" in out and "OK" in out
    assert RM.main(["verify-plugins", man, "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["provenance_digest_ok"] is True and payload["drifts"] == []
    assert payload["third_party"] == 0 and payload["runtime"]["python"] == sys.version
    # an EDITED lock is caught by the recomputed provenance digest, with no plugin resolution at all
    doc = json.loads(open(man, encoding="utf-8").read())
    doc["plugins"]["loaded"][0]["params"] = {"tampered": True}
    tampered = tmp_path / "tampered.json"
    tampered.write_text(json.dumps(doc), encoding="utf-8")
    assert RM.main(["verify-plugins", str(tampered)]) == 2
    assert "provenance_digest does not match" in capsys.readouterr().err


_SIBLING_PKG_PHYSICS = """\
GAIN_DB = 0.0


class Base:
    def rx_dbm(self, d_m):
        return -60.0 - 20.0 * d_m + GAIN_DB
"""

_SIBLING_PKG_MODEL = """\
from scms_sim_ref.api.channel import INTERFACE_VERSION, LinkOutcome
from .physics import Base


class Model(Base):
    interface_version = INTERFACE_VERSION
    plugin_id = "sibtest"
    reach_m = 500.0

    def __init__(self, *, params, rng, env):
        self._rng = rng

    def capabilities(self):
        return frozenset({"rssi", "reach"})

    def begin_step(self, frame):
        pass

    def evaluate(self, tx, rx, d_m, txn):
        return LinkOutcome(rssi_dbm=self.rx_dbm(d_m))
"""


def _install_sibling_pkg(root, physics=_SIBLING_PKG_PHYSICS):
    pkg = root / "sibpkg"
    pkg.mkdir(exist_ok=True)
    (pkg / "__init__.py").write_text("", encoding="utf-8")
    (pkg / "physics.py").write_text(physics, encoding="utf-8")
    (pkg / "model.py").write_text(_SIBLING_PKG_MODEL, encoding="utf-8")
    return pkg


def test_the_lock_sees_an_edit_to_a_SIBLING_module_not_just_the_defining_file(tmp_path):
    """D4's failure mode, closed.

    `module_sha256` hashes ONLY the file the class is defined in, and `dist_sha256` is copied out of
    the wheel RECORD -- a record of what the installer wrote, which does not move when a file is
    edited in place. A plugin class that inherits its physics from a sibling module therefore had a
    lock that did not move when that physics was rewritten: `verify_lock` returned clean and the
    replay produced a different dataset at exit 0. `package_sha256` is the field that closes it.

    Measured against the real out-of-repo demo before the fix: editing `rayleigh.py` to add 6 dB of
    transmit power left `scms_demo_channel.leaky:LeakyChannel`'s `module_sha256` (leaky.py) and
    `dist_sha256` unmoved, `verify-plugins` printed "no drift" and exited 0, and the replay produced
    a different `data_digest`.
    """
    import importlib
    import shutil

    _install_sibling_pkg(tmp_path)
    sys.path.insert(0, str(tmp_path))
    try:
        for name in [m for m in list(sys.modules) if m == "sibpkg" or m.startswith("sibpkg.")]:
            del sys.modules[name]
        importlib.invalidate_caches()
        RM._api_registry._MODULE_HASH_CACHE.clear()
        obj, how, iv, _shape = RM._api_registry.resolve("channel_model", "sibpkg.model:Model")
        prov = RM._api_registry.make_provenance("channel_model", 0, "sibpkg.model:Model", obj,
                                                how, iv, {"rssi", "reach"}, (), {})
        entry = prov.to_dict()
        assert entry["package_sha256"], "a package plugin must carry a package-level hash"
        lock = {"loaded": [entry]}
        assert RM._api_registry.verify_lock(lock) == []          # clean before the edit

        # a real physics change, in a file the class is NOT defined in
        (tmp_path / "sibpkg" / "physics.py").write_text(
            _SIBLING_PKG_PHYSICS.replace("GAIN_DB = 0.0", "GAIN_DB = 6.0"), encoding="utf-8")
        RM._api_registry._MODULE_HASH_CACHE.clear()
        assert RM._api_registry.module_sha256(obj) == entry["module_sha256"], (
            "the defining file is untouched -- which is precisely why module_sha256 cannot see this")
        with pytest.raises(api.PluginDriftError) as e:
            RM._api_registry.verify_lock(lock)
        assert e.value.field == "package_sha256"
        # --allow-plugin-drift still downgrades a hard stop to a loud one, never to silence
        assert len(RM._api_registry.verify_lock(lock, allow_drift=True)) == 1
    finally:
        sys.path.remove(str(tmp_path))
        for name in [m for m in list(sys.modules) if m == "sibpkg" or m.startswith("sibpkg.")]:
            del sys.modules[name]
        shutil.rmtree(tmp_path / "sibpkg", ignore_errors=True)
        RM._api_registry._MODULE_HASH_CACHE.clear()


def test_verify_plugins_fails_when_the_plugin_is_simply_not_installed(tmp_path, capsys):
    """The realistic CI case: the dataset was produced elsewhere and the plugin is not on this
    machine. An unresolvable lock entry is drift, not a warning -- the alternative is a replay that
    silently falls back to a built-in and exits 0."""
    run_pipeline(PipelineConfig(seed=5, n_steps=10, n_vehicles=9, out_dir=str(tmp_path / "r")))
    doc = json.loads((tmp_path / "r" / "manifest.json").read_text(encoding="utf-8"))
    entry = dict(doc["plugins"]["loaded"][0])
    entry.update(resolved_via="dotted_path", ref="not_installed_anywhere.radio:Model",
                 module_sha256="0" * 64)
    doc["plugins"]["loaded"] = [entry]
    doc["plugins"]["provenance_digest"] = RM._api_registry.provenance_digest([entry])
    p = tmp_path / "foreign.json"
    p.write_text(json.dumps(doc), encoding="utf-8")
    assert RM.main(["verify-plugins", str(p)]) == 2
    assert "cannot import module" in capsys.readouterr().err
    # ...and --allow-drift downgrades the hard stop to a loud one rather than a silent one
    assert RM.main(["verify-plugins", str(p), "--allow-drift"]) == 0
    assert "DRIFT ALLOWED" in capsys.readouterr().err


def test_strict_plugins_false_is_the_documented_escape_hatch(capsys):
    """`strict_plugins=False` exists for a tool reading a FOREIGN manifest it only wants the config
    out of. It must not be reachable by accident: the default is True, and the escape hatch neither
    verifies the lock nor refuses an unintelligible plugin key."""
    doc = {"seed": 3, "plugin_backends": {"x": 1}}
    with pytest.raises(api.ConfigError, match="plugin"):
        RM.config_from_dict(doc)                                  # default: strict
    cfg = RM.config_from_dict(doc, strict_plugins=False)
    assert cfg.seed == 3 and cfg.plugins == {}
    assert "ignoring 1 unknown key" in capsys.readouterr().err    # dropped LOUDLY, still


def test_conformance_cli_exit_codes(capsys):
    assert RM.main(["conformance", "--ref", "geometric"]) == 0
    assert "14 passed" in capsys.readouterr().out       # 13 numbered checks; C6 has two arms
    assert RM.main(["conformance", "--ref", "logdistance"]) == 0     # waived, not failed
    assert "WAIVED" in capsys.readouterr().out
    assert RM.main(["conformance", "--ref", "nope-not-a-model"]) == 1
    assert RM.main(["conformance", "--ref", "disc", "--params", "{oops"]) == 2
    assert "not valid JSON" in capsys.readouterr().err


def test_conformance_report_is_the_shape_the_manifest_embeds(tmp_path):
    rep = run_ref("channel_model", "geometric")
    path = rep.write(str(tmp_path / "conformance_report.json"))
    doc = json.loads(open(path, encoding="utf-8").read())
    assert doc["suite"] == "v1" and doc["interface_version"] == "ChannelModel/1.0"
    s = doc["summary"]
    assert set(s) >= {"suite", "interface_version", "passed", "failed", "skipped", "waived", "ok"}
    assert s["ok"] is True and s["failed"] == 0
    assert [r["check"] for r in doc["checks"]] == list(
        __import__("scms_sim_ref.conformance", fromlist=["x"]).CHANNEL_CHECKS)


def test_dist_sha256_is_recorded_for_a_normally_installed_wheel():
    """Regression: `_distribution_uncached` bailed to None on the FIRST unhashed RECORD row, and a
    wheel's RECORD always has three kinds of unhashed row (RECORD itself, `__pycache__/*.pyc`,
    `direct_url.json`). The effect was that `dist_sha256` -- the design's STRONGEST identity -- was
    null for every normally-installed distribution, not just for the editable installs the design
    calls out. Graded against pytest, which is installed here as an ordinary wheel."""
    import pytest as _pytest
    from scms_sim_ref.api.registry import _distribution_for
    name, version, dsha = _distribution_for(_pytest.approx)
    assert name == "pytest" and version
    assert dsha is not None and len(dsha) == 64
    assert _distribution_for(_pytest.approx)[2] == dsha          # stable across calls


def test_allowed_drift_is_recorded_in_the_new_manifest_and_is_bound_to_one_config(tmp_path):
    """Section 4.3: `--allow-plugin-drift` WRITES the drift into the new manifest, it does not
    silence it. And the record belongs to ONE CONFIG OBJECT, not to the process.

    The weaker "consume at the start of the next run" design is wrong, and this test is what proves
    it: a caller that builds a drifted config and never runs it -- a `--check-config`, a GUI
    validation, a test -- would otherwise leave a record that the next unrelated `run_pipeline`
    stamps into its manifest, and the in-process multi-run drivers make that routine.
    """
    drifted = PipelineConfig(seed=5, n_steps=10, n_vehicles=9, out_dir=str(tmp_path / "a"))
    other = PipelineConfig(seed=5, n_steps=10, n_vehicles=9, out_dir=str(tmp_path / "b"))
    RM._PLUGIN_DRIFT_ALLOWED.update(cfg=drifted, drifts=["synthetic drift record for the test"])
    b = run_pipeline(other)                          # an UNRELATED run must not claim it
    a = run_pipeline(drifted)
    ma = json.loads((tmp_path / "a" / "manifest.json").read_text(encoding="utf-8"))
    mb = json.loads((tmp_path / "b" / "manifest.json").read_text(encoding="utf-8"))
    assert ma["plugins"]["drift_allowed"] == ["synthetic drift record for the test"]
    assert "drift_allowed" not in mb["plugins"]
    assert a.data_digest == b.data_digest            # manifest-only: zero effect on the data digest
    assert RM._PLUGIN_DRIFT_ALLOWED["cfg"] is None   # claimed exactly once
    assert RM._claim_drift_record(drifted) == []     # and never twice


# --------------------------------------------------------------------------- attestation -------- #
def test_conformance_required_attests_the_plugin_into_the_manifest(tmp_path):
    """The design's third delivery route -- *let the engine refuse an unattested plugin* -- as a
    CONFIG declaration rather than a flag, so it lands in `manifest["config"]` and replays."""
    cfg = PipelineConfig(
        seed=5, n_steps=12, n_vehicles=9, out_dir=str(tmp_path / "att"),
        plugins={"channel_model": {"ref": "geometric", "conformance": "required"}})
    res = run_pipeline(cfg)
    man = json.loads((tmp_path / "att" / "manifest.json").read_text(encoding="utf-8"))
    conf = man["plugins"]["loaded"][0]["conformance"]
    assert conf["ok"] is True and conf["suite"] == "v1" and conf["failed"] == 0
    assert conf["interface_version"] == "ChannelModel/1.0"
    # C12 runs two pipelines, so running it from inside one would recurse without bound. It is
    # EXCLUDED and the exclusion is recorded -- nobody should read `passed` here and believe the
    # artifact-level check ran.
    assert conf["excluded"] == ["C12_pipeline_two_run_digest"]
    assert "pipeline inside a pipeline" in conf["excluded_reason"]
    assert conf["passed"] == 13 and conf["skipped"] == 0        # the other thirteen all ran
    # ...and attestation is manifest-only: it changes no data
    plain = run_pipeline(PipelineConfig(seed=5, n_steps=12, n_vehicles=9,
                                        out_dir=str(tmp_path / "plain"),
                                        plugins={"channel_model": {"ref": "geometric"}}))
    assert plain.data_digest == res.data_digest
    assert "conformance" not in json.loads(
        (tmp_path / "plain" / "manifest.json").read_text(encoding="utf-8"))["plugins"]["loaded"][0]


def test_excluded_checks_are_not_run_at_all_not_merely_dropped_from_the_report():
    """`exclude` has to prevent EXECUTION. Filtering C12's row out after the fact would leave it
    running two pipelines from inside the pipeline it is attesting -- the report would look right
    and the cost and the nesting would both still be there."""
    from scms_sim_ref.conformance.runner import run_ref as _run_ref
    full = _run_ref("channel_model", "disc")
    trimmed = _run_ref("channel_model", "disc", exclude=RM._ATTEST_EXCLUDES)
    assert "C12_pipeline_two_run_digest" in {r["check"] for r in full.rows}
    assert "C12_pipeline_two_run_digest" not in {r["check"] for r in trimmed.rows}
    assert len(trimmed.rows) == len(full.rows) - 1
    assert trimmed.seconds < full.seconds        # it was not run, so the time is not spent


def test_conformance_required_refuses_a_nonconformant_plugin(tmp_path):
    """A model that fails a check never reaches step 0, and the error names the checks that failed."""
    import sys as _sys
    mod = tmp_path / "badmod.py"
    mod.write_text(
        "from scms_sim_ref.api.channel import INTERFACE_VERSION, LinkOutcome\n"
        "class OverReach:\n"
        "    interface_version = INTERFACE_VERSION\n"
        "    plugin_id = 'overreach2'\n"
        "    def __init__(self, *, params, rng, env):\n"
        "        self.reach_m = float(env.get('radio_range_m', 500.0)); self._rng = rng\n"
        "    def capabilities(self):\n"
        "        return frozenset({'reach', 'loss_composition:independent_survival'})\n"
        "    def begin_step(self, frame): pass\n"
        "    def window_m(self, rx): return 3.0 * self.reach_m\n"
        "    def evaluate(self, tx, rx, d_m, txn):\n"
        "        return LinkOutcome() if d_m <= 3.0 * self.reach_m else None\n"
        "    def channel_busy_ratio(self, rx_vid, offered): return 0.0\n"
        "    def collision_loss(self, dist_m, cbr): return 0.0\n"
        "    def delivery_coin(self, a, b): return self._rng.stream('c', a, b).random()\n",
        encoding="utf-8")
    _sys.path.insert(0, str(tmp_path))
    try:
        with pytest.raises(api.ConfigError) as e:
            run_pipeline(PipelineConfig(
                seed=5, n_steps=12, n_vehicles=9, out_dir=str(tmp_path / "no"),
                plugins={"channel_model": {"ref": "badmod:OverReach",
                                           "conformance": "required"}}))
        assert "C8_reach_honesty" in str(e.value)
        assert not (tmp_path / "no").exists()          # refused BEFORE step 0
    finally:
        _sys.path.remove(str(tmp_path))
        _sys.modules.pop("badmod", None)


def test_a_refused_plugin_is_a_clean_nonzero_exit_not_a_traceback(tmp_path, capsys):
    """Every designed refusal -- unknown ref, signature mismatch, reserved capability, a failed
    attestation -- must reach the operator as an explained exit 2. A traceback out of `main` is not
    a contract; it is an unhandled error that happens to have the right effect."""
    rc = RM.main(["--seed", "5", "--steps", "8", "--vehicles", "9",
                  "--out", str(tmp_path / "nope"),
                  "--plugins", json.dumps({"channel_model": {"ref": "no.such.module:Model"}})])
    assert rc == 2
    err = capsys.readouterr().err
    assert err.startswith("PLUGIN REFUSED:") and "cannot import module" in err
    assert not (tmp_path / "nope").exists()


def test_a_mistyped_plugin_section_key_is_an_error_not_a_no_op():
    with pytest.raises(ValueError, match="unknown key"):
        RM.validate_config(PipelineConfig(
            plugins={"channel_model": {"ref": "disc", "conformanc": "required"}}))
    with pytest.raises(ValueError, match="conformance must be one of"):
        RM.validate_config(PipelineConfig(
            plugins={"channel_model": {"ref": "disc", "conformance": "yes please"}}))


def test_the_conformance_machinery_moves_no_golden(tmp_path):
    """V1/D6 restated for phase 2: the suite, the runner and the lock all live outside the engine
    path. Importing them, and running one, must not perturb the determinism contract."""
    run_ref("channel_model", "disc")
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "g")))
    assert res.data_digest == DEFAULT_GOLDEN
