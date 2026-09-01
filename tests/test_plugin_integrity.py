"""Whole-run integrity: the three defects the plugin seam still had, each with the attack that
measured it.

`tests/test_detector_plugins.py` pins the FUNCTIONAL claim (a third-party detector runs with zero
engine edits). `tests/test_detector_trust_boundary.py` pins the DATA boundary (what a plugin can and
cannot read by name). This file pins the TEMPORAL one: *when* the engine checks a plugin, and whether
what it checked is still true afterwards.

Three defects, all reproduced before the fixes:

**A. Attestation ran AFTER construction.** `build_channel` instantiated the run's plugin at
`run.py:1169` and only called `_attest` at `:1188`; the detector slot repeated the ordering
(`:1493` / `:1499`). A hostile `__init__` therefore executed *before* anything gated it -- it could
rebind `random.Random`, replace `api.channel.check_outcome`, or monkeypatch
`conformance.runner.run_ref` itself, and then "pass" attestation performed by a process it had
already edited.

**B. A time-delayed attack evades every conformance trap.** A `random.Random` class rebind installed
at `frame.step >= 30` passes all four of C3's traps. That is not a hole in the traps: conformance
drives a bounded window of steps and the attack waits it out. **A fixed-window contract suite can
only certify the behaviour it observed.** `test_conformance_is_a_sampling_check_...` asserts that
directly -- the suite reports `ok` on the attack -- and the two tests after it assert that the
whole-run monitor catches what the suite structurally cannot.

**C. `check_outcome` was a read-time check that returned the caller's object.** An outcome whose
`rssi_dbm` is a stateful `property` returned one value to the checker and another to the engine.

Every test here fails if its fix is reverted; several assert the ORDER rather than the outcome,
because the order is what the defect was.

None of this is a sandbox, and nothing here should be read as one. A detector that reads the labels
through a frame walk moves nothing on any watch list in this file. See
`docs/realism/DETECTOR-PLUGIN.md` section 2.
"""

import importlib
import json
import os
import random
import sys

import pytest

from scms_sim_ref.api import channel as apichan
from scms_sim_ref.api import integrity as INTEG
from scms_sim_ref.api.errors import ConfigError
from scms_sim_ref.conformance.runner import run_ref
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

#: The determinism-contract default digest, pinned identically in 7 other test files.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
#: dt is 1.0, so duration_s=40 is forty steps -- past the `step >= 30` guard the delayed attacks use.
_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.25)


# --------------------------------------------------------------------------- #
# One third-party distribution, written to a real file so nothing about it is special-cased
# --------------------------------------------------------------------------- #
_SRC = '''\
"""A third-party plugin distribution whose classes MEASURE the integrity defects.

Every hostile line here is one line, at a point the engine hands control to the plugin, and each one
was verified to succeed before the corresponding fix.
"""
import random

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION, LOSS_INDEPENDENT_SURVIVAL
from scms_sim_ref.api.detect import INTERFACE_VERSION as DET_IV, CheckBase, FusionBase

#: appended to by every hostile `__init__`, so a test can assert the constructor NEVER RAN.
CONSTRUCTED = []


class Impostor(random.Random):
    """What a class rebind installs. A plain subclass: bit-identical output, which is the point --
    the rebind is invisible to every digest, golden and content hash in the project."""


class HardRange:
    """An honest per-link model. The base for every hostile variant below."""

    interface_version = INTERFACE_VERSION
    plugin_id = "tiplain"

    #: Declared, not edited around: this is a unit-disc idealisation, so C9's attenuation arm
    #: legitimately cannot pass. `disc` ships the same waiver for the same reason.
    conformance_waivers = {
        "C9_monotone_in_distance":
            "A deliberate unit-disc IDEALISATION -- heard iff d <= reach_m -- so delivery is "
            "constant in distance inside the disc by construction. Used here only as the honest "
            "baseline the hostile variants inherit from; nothing in these tests measures physics.",
    }

    def __init__(self, *, params, rng, env):
        self.reach_m = float(params.get("range_m", env["radio_range_m"]))
        self._rng = rng
        self.step = -1

    def capabilities(self):
        return frozenset({"reach", LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame):
        self.step = frame.step

    def reach_m_for(self, rx):
        return rx.rx_range_m or self.reach_m

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= (rx.rx_range_m or self.reach_m) else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return self._rng.persistent("deliver", tx_vid, rx_vid).random()


# ---------------------------------------------------------------- A: construction-time attacks -- #
class ConstructionRebindChannel(HardRange):
    """One line in `__init__` takes ownership of every keyed stream the engine builds afterwards."""

    plugin_id = "ticonstr"

    def __init__(self, *, params, rng, env):
        CONSTRUCTED.append("ConstructionRebindChannel")
        random.Random = Impostor
        super().__init__(params=params, rng=rng, env=env)


class ConstructionGateChannel(HardRange):
    """The same shape, aimed at the engine's OWN GATE rather than at the RNG: `check_outcome` is
    what validates every delivered link of a third-party model, so replacing it with the identity
    function at construction disables the only outcome validation on the default plugin path."""

    plugin_id = "ticongate"

    def __init__(self, *, params, rng, env):
        from scms_sim_ref.api import channel as _c
        CONSTRUCTED.append("ConstructionGateChannel")
        _c.check_outcome = lambda o: o
        super().__init__(params=params, rng=rng, env=env)


class AttestSubverterChannel(HardRange):
    """Fails conformance for real (it draws from the module-level `random`), AND patches the
    conformance runner at construction so that the suite about to judge it always says yes.

    Under the OLD ordering this was constructed first, so the patch was installed before `_attest`
    ran and the model was attested by machinery it had just rewritten. Under the new ordering it is
    never constructed in the engine's process at all."""

    plugin_id = "tisubv"

    def __init__(self, *, params, rng, env):
        from scms_sim_ref.conformance import runner as _r
        CONSTRUCTED.append("AttestSubverterChannel")
        _r.run_ref = lambda *a, **k: _ALWAYS_OK
        super().__init__(params=params, rng=rng, env=env)

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        return DELIVERED if random.random() > 0.02 else None      # not reproducible -> fails C1


class _AlwaysOk:
    ok = True
    rows = ()

    def summary(self):
        return {"suite": "v1", "passed": 99, "failed": 0, "skipped": 0, "errored": 0,
                "waived": [], "ok": True}

    def to_text(self):
        return "forged"


_ALWAYS_OK = _AlwaysOk()


class ConstructionRebindCheck(CheckBase):
    """The DETECTOR slot's copy of the same defect: instantiate, then attest."""

    interface_version = DET_IV
    plugin_id = "tichk"
    reason_code = "tamper"

    def __init__(self, *, params=None, rng=None, env=None):
        CONSTRUCTED.append("ConstructionRebindCheck")
        random.Random = Impostor
        super().__init__(params=params, rng=rng, env=env)

    def evaluate(self, obs, state, params, rng):
        return 0.0


class ConstructionRebindFusion(FusionBase):
    """And the fusion slot's."""

    interface_version = DET_IV
    plugin_id = "tifus"

    def __init__(self, *, params=None, rng=None, env=None):
        CONSTRUCTED.append("ConstructionRebindFusion")
        random.Random = Impostor
        super().__init__(params=params, rng=rng, env=env)

    def decide(self, scores, state, obs, params, rng):
        return None


# ------------------------------------------------------------------- B: the delayed attacks ----- #
FIRED = []


class DelayedClassRebind(HardRange):
    """**THE measured attack.** Identical to `HardRange` for the first thirty steps, which is longer
    than any window the conformance suite drives from step 0, and then it owns `random.Random`."""

    plugin_id = "tidelay"

    def begin_step(self, frame):
        super().begin_step(frame)
        if frame.step >= 30 and random.Random is not Impostor:
            FIRED.append(frame.step)
            random.Random = Impostor


def _biased(self):
    return 0.25


class DelayedMethodRebindThenRestore(HardRange):
    """The second form, and the reason the monitor needs TWO instruments.

    It rebinds `random.Random.random` at step 30 and PUTS IT BACK at step 34. At the end of the run
    every identity on the watch list is exactly what it was, so the snapshot comparison is clean --
    and the engine's own Mersenne-Twister stream is four steps' worth of draws behind where the draws
    it made would put it, which is what `WitnessedRandom.verify_stream` measures."""

    plugin_id = "tidelay2"
    _saved = []

    def begin_step(self, frame):
        super().begin_step(frame)
        if frame.step == 30:
            type(self)._saved.append(random.Random.__dict__.get("random", None))
            random.Random.random = _biased
        elif frame.step == 34 and type(self)._saved:
            was = type(self)._saved.pop()
            if was is None:
                try:
                    del random.Random.random
                except AttributeError:
                    pass
            else:
                random.Random.random = was


# --------------------------------------------------------------- C: the time-of-check/use gap --- #
class StatefulOutcome:
    """A LinkOutcome-SHAPED object -- nothing obliges a model to return the real class -- whose
    `rssi_dbm` and `link_state` change between the CHECK's read and the ENGINE's."""

    __slots__ = ("_rssi_reads", "_state_reads")

    tx_index = -1
    rx_vid = -1
    delay_s = 0.0
    extras = ()

    def __init__(self):
        self._rssi_reads = 0
        self._state_reads = 0

    @property
    def rssi_dbm(self):
        self._rssi_reads += 1
        return -70.0 if self._rssi_reads == 1 else 9999.0

    @property
    def link_state(self):
        self._state_reads += 1
        return "LOS" if self._state_reads == 1 else "TELEPATHY"

    def bind(self, tx_index, rx_vid):
        return self


class ToctouChannel(HardRange):
    """Declares `rssi`, so its per-link figure reaches the MA-visible dataset."""

    plugin_id = "titoctou"

    def capabilities(self):
        return frozenset({"reach", "rssi", "link_state", LOSS_INDEPENDENT_SURVIVAL})

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        return StatefulOutcome()
'''


#: An EARLIER hook than `__init__`: module-level code, which runs when the resolver imports the
#: module. Kept in its own file because module-level code executes exactly once per process, so the
#: test that exercises it has to evict the module first.
_IMPORT_TIME_SRC = '''\
"""The attack that runs at IMPORT, before any constructor exists to gate."""
import random

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION, LOSS_INDEPENDENT_SURVIVAL


class Impostor(random.Random):
    pass


random.Random = Impostor          # <- module level: runs inside `importlib.import_module`


class ImportTimeRebind:
    interface_version = INTERFACE_VERSION
    plugin_id = "tiimport"
    reach_m = 300.0

    def __init__(self, *, params, rng, env):
        self.reach_m = float(params.get("range_m", env["radio_range_m"]))
        self._rng = rng

    def capabilities(self):
        return frozenset({"reach", LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame):
        pass

    def reach_m_for(self, rx):
        return rx.rx_range_m or self.reach_m

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= (rx.rx_range_m or self.reach_m) else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return self._rng.persistent("deliver", tx_vid, rx_vid).random()
'''


@pytest.fixture(scope="module")
def tp(tmp_path_factory):
    """The distribution on `sys.path` -- installed, and declared only where a test declares it."""
    root = tmp_path_factory.mktemp("integrity_plugins")
    (root / "ti_plugins.py").write_text(_SRC, encoding="utf-8")
    (root / "ti_importtime.py").write_text(_IMPORT_TIME_SRC, encoding="utf-8")
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()
    try:
        yield importlib.import_module("ti_plugins")
    finally:
        sys.path.remove(str(root))
        sys.modules.pop("ti_plugins", None)
        sys.modules.pop("ti_importtime", None)


@pytest.fixture(autouse=True)
def _pristine():
    """Every test here deliberately runs code that rebinds interpreter-wide objects.

    The engine restores what it detects, but a test that asserts a DEFECT must not depend on the
    fix's own cleanup for the next test to be valid -- so the snapshot is taken and enforced here
    too, and a leak fails the test that caused it rather than the one after it.
    """
    keep = INTEG.Sentinel(armed=True)
    yield
    leaked = keep.drift()
    keep.restore()
    assert not leaked, f"this test leaked interpreter state: {[w for w, _ in leaked]}"


def _run(tmp_path, name, **kw):
    base = dict(_CFG)
    base.update(kw)
    return run_pipeline(PipelineConfig(out_dir=str(tmp_path / name), **base))


def _manifest_exists(tmp_path, name) -> bool:
    return os.path.exists(os.path.join(str(tmp_path / name), "manifest.json"))


# ======================================================= A. attestation runs after construction === #
def test_a_hostile_constructor_is_refused_before_step_0_channel_slot(tp, tmp_path):
    """A, channel slot. `__init__` rebinds `random.Random`; the run is refused at CONSTRUCTION.

    Before the fix this ran to exit 0 with a valid manifest: the class rebind owns every
    `random.Random(f"{seed}:...")` the engine builds afterwards (about twenty keyed sites), and
    because a plain subclass is bit-identical the digest did not move either.
    """
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a1", plugins={"channel_model": {"ref": "ti_plugins:ConstructionRebindChannel"}})
    msg = str(e.value)
    assert "INTEGRITY FAILURE while LOADING the channel plugin" in msg
    assert "random.Random" in msg
    assert "ti_plugins:ConstructionRebindChannel" in msg
    # constructed, detected, and PUT BACK -- a detector that leaves the tamper installed is worse
    # than no detector.
    assert random.Random is not tp.Impostor
    assert not _manifest_exists(tmp_path, "a1")


def test_a_hostile_constructor_that_disables_the_engines_own_gate_is_refused(tp, tmp_path):
    """A, and the sharpest form of it: the constructor replaces `api.channel.check_outcome`.

    That is the function that validates every delivered link of a third-party model. Replacing it
    with the identity function is one line, it is perfectly deterministic, and before this fix
    nothing in the project could see it.
    """
    original = apichan.check_outcome
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a2", plugins={"channel_model": {"ref": "ti_plugins:ConstructionGateChannel"}})
    assert "scms_sim_ref.api.channel.check_outcome" in str(e.value)
    assert "ENGINE object" in str(e.value)
    assert apichan.check_outcome is original


def test_attestation_now_happens_before_the_plugin_is_ever_constructed(tp, tmp_path):
    """A, asserted as the ORDER rather than as an outcome -- because the order IS the defect.

    `AttestSubverterChannel.__init__` patches `conformance.runner.run_ref` to a function that always
    reports success, and the model separately fails C1 for real. Under the old ordering the engine
    constructed it first, so the patch was in place before `_attest` ran and a genuinely
    non-conformant model was certified by machinery it had just rewritten.

    The assertion is not "it was refused" but **"its constructor never ran in this process"**: the
    verdict is reached before the candidate has a constructor call in the engine's interpreter at
    all, and the suite that reaches it runs in a child process that is thrown away.
    """
    from scms_sim_ref.conformance import runner as _runner
    before_run_ref = _runner.run_ref
    tp.CONSTRUCTED.clear()
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a3", plugins={"channel_model": {
            "ref": "ti_plugins:AttestSubverterChannel", "conformance": "required"}})
    assert "does not conform" in str(e.value)
    assert tp.CONSTRUCTED == [], (
        "the plugin was CONSTRUCTED in the engine's process before it was attested -- which is the "
        f"defect: {tp.CONSTRUCTED}")
    assert _runner.run_ref is before_run_ref, "the candidate patched the runner that judged it"
    assert not _manifest_exists(tmp_path, "a3")


def test_attestation_runs_in_a_child_process(tp, tmp_path):
    """A, the second half: conformance must construct the candidate to grade it, so it does that
    somewhere the run does not live."""
    from scms_sim_ref.conformance.attest import run_out_of_process
    tp.CONSTRUCTED.clear()
    report = run_out_of_process("channel_model", "ti_plugins:HardRange", {},
                                exclude=("C12_pipeline_two_run_digest",))
    assert report["summary"]["ok"] is True
    assert report["integrity"]["ok"] is True
    assert tp.CONSTRUCTED == [], "the suite constructed the candidate in THIS process"


def test_a_hostile_constructor_is_refused_before_step_0_check_slot(tp, tmp_path):
    """A, detector slot -- the identical ordering defect at `run.py:1493` / `:1499`."""
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a4", plugins={"check": ["@builtins",
                                                {"ref": "ti_plugins:ConstructionRebindCheck"}]})
    msg = str(e.value)
    assert "INTEGRITY FAILURE while LOADING a detector plugin" in msg
    assert "ti_plugins:ConstructionRebindCheck" in msg
    assert random.Random is not tp.Impostor
    assert not _manifest_exists(tmp_path, "a4")


def test_a_hostile_constructor_is_refused_before_step_0_fusion_slot(tp, tmp_path):
    """A, fusion slot."""
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a5", plugins={"fusion": {"ref": "ti_plugins:ConstructionRebindFusion"}})
    assert "INTEGRITY FAILURE while LOADING the fusion plugin" in str(e.value)
    assert random.Random is not tp.Impostor


def test_an_import_time_attack_is_caught_too(tp, tmp_path):
    """A, and the hook that is EARLIER than `__init__`.

    `resolve()` calls `importlib.import_module`, so module-level code runs before any constructor
    exists to bracket. The snapshot is therefore taken before `resolve`, not before `instantiate` --
    a distinction with no cost and a real difference, since `random.Random = Impostor` at module
    scope is one line and the source gate's name list does not contain it (and does not apply to the
    channel slot at all).
    """
    sys.modules.pop("ti_importtime", None)           # module-level code runs ONCE per process
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "a7", plugins={"channel_model": {"ref": "ti_importtime:ImportTimeRebind"}})
    # `validate_config` is what resolves (and therefore imports) the ref, so the checkpoint that
    # catches this is the one taken before validation -- the FIRST statement of `run_pipeline`.
    assert "INTEGRITY FAILURE while RESOLVING the declared plugins" in str(e.value)
    assert "random.Random" in str(e.value)
    assert not _manifest_exists(tmp_path, "a7")


def test_an_honest_plugin_constructs_and_runs_untouched(tp, tmp_path):
    """The other half of every gate: it must not fire on an honest plugin."""
    res = _run(tmp_path, "a6", plugins={"channel_model": {"ref": "ti_plugins:HardRange"}})
    assert res.n_reports >= 0 and _manifest_exists(tmp_path, "a6")


# ================================================================ B. the time-delayed attack ===== #
def test_conformance_is_a_sampling_check_and_reports_ok_on_the_delayed_attack(tp):
    """B, stated as a measurement rather than as a caveat.

    The v1 suite runs the delayed-rebind model and reports **ok**. That is not a bug in C3's four
    traps -- they are correct, and they catch this exact rebind when it is installed inside the
    window they drive. It is a property of ANY fixed-window contract test: the suite exercises a
    bounded number of steps, the attack is guarded on `frame.step >= 30`, and a suite can only
    certify the behaviour it observed. Lengthening the window moves the number an attacker waits
    out; it does not change the shape of the statement.
    """
    tp.FIRED.clear()
    rep = run_ref("channel_model", "ti_plugins:DelayedClassRebind", {},
                  exclude=("C12_pipeline_two_run_digest",))
    assert rep.ok, rep.to_text()
    assert rep.count("FAIL") == 0 and rep.count("ERROR") == 0
    # The suite restores what it detects, so the interpreter is clean afterwards even though the
    # attack DID fire during the run (C13 drives a tail at step 10 000).
    assert random.Random is not tp.Impostor


def test_the_whole_run_monitor_catches_the_delayed_class_rebind(tp, tmp_path):
    """B, the fix. What a bounded window cannot see, an end-of-run comparison can."""
    tp.FIRED.clear()
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "b1", plugins={"channel_model": {"ref": "ti_plugins:DelayedClassRebind"}})
    msg = str(e.value)
    assert "INTEGRITY FAILURE at the END of the run" in msg
    assert "random.Random" in msg
    assert "SAMPLING CHECK" in msg, "the refusal must say why conformance did not catch it"
    assert tp.FIRED and min(tp.FIRED) >= 30, f"the attack never fired: {tp.FIRED}"
    assert random.Random is not tp.Impostor
    assert not _manifest_exists(tmp_path, "b1")


def test_the_delayed_rebind_is_invisible_to_every_digest_layer(tp):
    """B, why the monitor is the only thing that could have caught it.

    `Impostor` is a plain `random.Random` subclass, so every stream the engine builds after the
    rebind produces bit-identical numbers. The pinned goldens, the two-run equality gate and the
    content-hash lock are therefore all structurally blind to the rebind -- exactly as they are to a
    frame walk. Owning `random.Random` is a CAPABILITY; what it is worth is whatever the next
    version of the plugin does with it.
    """
    for seed in ("a", "b:c", 17, 4242):
        a, b = random.Random(seed), tp.Impostor(seed)
        assert [a.random() for _ in range(8)] == [b.random() for _ in range(8)]
        assert a.getstate()[1] == b.getstate()[1]


def test_the_stream_witness_catches_a_rebind_that_was_put_back(tp, tmp_path):
    """B, the second instrument, and the case the identity snapshot CANNOT see.

    `DelayedMethodRebindThenRestore` rebinds `random.Random.random` at step 30 and restores it at
    step 34. At the end of the run every watched identity is exactly what it was. What is not what it
    was is the engine's own Mersenne-Twister state: MT19937 consumes two 32-bit words per `random()`
    and `ceil(k/32)` per `getrandbits(k)`, so the state after W counted words from a fixed start is
    fully determined -- and for four steps the engine's counted `random()` calls returned a constant
    without advancing the generator at all.
    """
    tp.DelayedMethodRebindThenRestore._saved.clear()
    with pytest.raises(ConfigError) as e:
        _run(tmp_path, "b2", plugins={"channel_model": {
            "ref": "ti_plugins:DelayedMethodRebindThenRestore"}})
    msg = str(e.value)
    assert "the engine's global random stream is NOT where" in msg
    assert "counted Mersenne-Twister words" in msg
    assert tp.DelayedMethodRebindThenRestore._saved == [], "the plugin did not restore the rebind"
    assert not _manifest_exists(tmp_path, "b2")


def test_the_witness_is_bit_identical_to_a_plain_random(tmp_path):
    """B's cost, pinned. The instrumented stream is only armed when the config declares plugins, and
    it must not move a single byte when it is: `random()` and `getrandbits()` add one integer
    increment each and delegate to the same C primitive, and every other generator in `random.py` is
    built on those two."""
    base = dict(_DEFAULT)
    plain = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "w0"), **base))
    assert plain.data_digest == DEFAULT_GOLDEN
    witnessed = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "w1"),
                                            plugins={"channel_model": {"ref": "disc"}}, **base))
    assert witnessed.data_digest == DEFAULT_GOLDEN
    assert isinstance(_engine_rng_kind(tmp_path / "w1"), str)     # armed, per the manifest


def _engine_rng_kind(out_dir):
    with open(os.path.join(str(out_dir), "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    return json.dumps(man["plugins"]["integrity"])


def test_the_manifest_records_what_the_monitor_covered(tmp_path):
    """A monitored run says so in its own artifact, with the word count its stream advanced."""
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "m1"),
                                plugins={"channel_model": {"ref": "disc"}}, **_CFG))
    with open(str(tmp_path / "m1" / "manifest.json"), encoding="utf-8") as fh:
        block = json.load(fh)["plugins"]["integrity"]
    assert block["armed"] is True and block["ok"] is True
    assert block["watched"] > 100
    assert block["engine_rng_words"] > 0
    assert "end of run" in block["verified_at"]


def test_a_run_with_no_plugins_is_not_monitored_and_says_nothing(tmp_path):
    """The default path pays nothing and its manifest is byte-identical to what it was: a run with
    no plugins runs no third-party code, so there is nothing to monitor."""
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "m0"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN
    with open(str(tmp_path / "m0" / "manifest.json"), encoding="utf-8") as fh:
        man = json.load(fh)
    assert "integrity" not in man["plugins"]


# ============================================== C. check_outcome's time-of-check/time-of-use gap = #
def test_check_outcome_returns_a_copy_not_the_callers_object():
    """C, the unit form. `check_outcome` read the caller's object and handed the SAME object back."""
    o = _StatefulProbe()
    checked = apichan.check_outcome(o)
    assert checked is not o
    assert isinstance(checked, apichan.LinkOutcome)
    # the checker read each field exactly once, and the value it validated is the value the engine
    # now holds -- re-reading it as many times as the engine will cannot change it
    assert [checked.rssi_dbm for _ in range(5)] == [-70.0] * 5
    assert [checked.link_state for _ in range(5)] == ["LOS"] * 5
    assert o.rssi_reads == 1 and o.state_reads == 1, (
        f"check_outcome read the plugin's fields {o.rssi_reads}/{o.state_reads} times; the whole "
        f"point is exactly once")


def test_the_second_read_would_have_been_out_of_band():
    """C, the proof that the copy is what is doing the work: the value the object serves on its
    SECOND read is one `check_outcome` would have refused outright."""
    o = _StatefulProbe()
    assert o.rssi_dbm == -70.0
    assert o.rssi_dbm == 9999.0                      # +9999 dBm is about 10^997 W
    assert o.link_state == "LOS"
    assert o.link_state == "TELEPATHY"               # and outside the CLOSED vocabulary
    with pytest.raises(ConfigError):
        apichan.check_outcome(_StatefulProbe(first_rssi=9999.0))


def test_the_adapter_hands_the_engine_the_validated_copy(tp):
    """C, at the seam the engine actually uses: `PerLinkAdapter.evaluate_link` (the per-link fast
    path) and `deliver` (the canonical batch ABI) both hand on the sanitised object."""
    from scms_sim_ref.api.rng import RngNamespace
    from scms_sim_ref.conformance.v1 import harness as H

    model = tp.ToctouChannel(params={}, rng=RngNamespace(1, "titoctou"),
                             env={"radio_range_m": 300.0})
    adapter = apichan.PerLinkAdapter(model, RngNamespace(1, "titoctou"), True)
    frames = H.build_frames(n_steps=2, n_stations=6, spacing_m=40.0)
    frame = frames[0]
    adapter.begin_step(frame)
    outs = adapter.deliver(frame, H.candidates_for(adapter, frame))
    assert outs, "the model delivered nothing, so the test measured nothing"
    for o in outs:
        assert isinstance(o, apichan.LinkOutcome)
        assert o.rssi_dbm == -70.0 and o.link_state == "LOS"
        assert o.tx_index >= 0 and o.rx_vid >= 0, "bind() must still have run, on OUR object"
    tx = frame.stations[frame.transmissions[1].tx_vid]
    rx = frame.stations[0]
    one = adapter.evaluate_link(tx, rx, 40.0, frame.transmissions[1])
    assert isinstance(one, apichan.LinkOutcome) and one.rssi_dbm == -70.0


def test_the_stateful_outcome_no_longer_reaches_the_dataset(tp, tmp_path):
    """C, end to end. Before the fix, `9999.0` -- the value the object served on its second read --
    was what `link_meta[li].rssi_dbm` handed the report writer, and it landed in
    `ma/ma_reports.jsonl` as MA-visible evidence."""
    res = _run(tmp_path, "c1", plugins={"channel_model": {"ref": "ti_plugins:ToctouChannel"}})
    with open(os.path.join(res.out_dir, "ma", "ma_reports.jsonl"), encoding="utf-8") as fh:
        rows = [json.loads(ln) for ln in fh if ln.strip()]
    seen = [r["rssi_dbm"] for r in rows if r.get("rssi_dbm") is not None]
    assert seen, "no report carried an rssi_dbm, so this measured nothing"
    assert set(seen) == {-70.0}, f"the engine used a value the checker never validated: {set(seen)}"


class _StatefulProbe:
    """A `LinkOutcome`-shaped object whose fields change between reads. Nothing obliges a plugin to
    return the real class, and a `property` is all it takes."""

    __slots__ = ("rssi_reads", "state_reads", "_first_rssi")

    tx_index = 3
    rx_vid = 7
    delay_s = 0.0
    extras = ()

    def __init__(self, first_rssi=-70.0):
        self.rssi_reads = 0
        self.state_reads = 0
        self._first_rssi = first_rssi

    @property
    def rssi_dbm(self):
        self.rssi_reads += 1
        return self._first_rssi if self.rssi_reads == 1 else 9999.0

    @property
    def link_state(self):
        self.state_reads += 1
        return "LOS" if self.state_reads == 1 else "TELEPATHY"

    def bind(self, tx_index, rx_vid):
        return self


# ================================================================== the honest negative claims === #
def test_the_monitor_is_not_a_sandbox_and_the_module_says_so():
    """Pinned so the documentation cannot quietly become an overclaim.

    A passing verification means "nothing on the watch list moved". It does not mean the plugin was
    honest: reading the oracle through a frame walk moves nothing at all, and stays perfectly
    reproducible while doing it.
    """
    doc = " ".join(INTEG.__doc__.split())
    for phrase in ("cannot be sandboxed", "detection after the fact",
                   "never \"this plugin is honest\"", "out-of-process boundary",
                   "sampling check, not a proof"):
        assert phrase in doc, phrase
    # and the residue is real: a plugin that restores a binding before the checkpoint is not seen
    s = INTEG.Sentinel(armed=True)
    saved = random.Random.__dict__.get("gauss")
    random.Random.gauss = lambda self, mu, sigma: 0.0
    random.Random.gauss = saved                     # put it back before the checkpoint
    assert s.drift() == [], "this assertion documents a LIMIT, not a capability"


def test_the_sentinel_is_free_when_it_is_not_armed():
    s = INTEG.Sentinel(armed=False)
    assert s.watched == 0 and s.drift() == []
    s.verify("nowhere")                              # never raises
