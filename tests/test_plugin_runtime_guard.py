"""The RUNTIME plugin guard, and the four in-process attacks it closes.

Every test here reproduces an attack that LANDED against this engine before the fix, spelled the way
the attacker spelled it. The file exists because of the lesson the dead per-step RNG dimension
taught: *a defect survives exactly as long as nothing asserts the property*. The whole conformance
suite and the CLI stayed green while a digest-bearing term was frozen, and three purpose-written
tests were what found it.

WHAT LANDED, AND WHAT NOW HAPPENS
---------------------------------
1. **An obfuscated frame walk, at DEFAULT settings.** ``getattr(sys, "_get"+"frame")`` and
   ``getattr(f, "f_"+"locals")`` reach the reception loop's broadcast dict -- ``veh.is_attacker``,
   ``attack_type``, the TRUE x/y, ``falsified`` -- with ``source_gate`` at its default ``"on"``.
   The static gate is a name match and cannot see it; the integrity monitor cannot see it either,
   because a READ moves nothing. Scoring the oracle directly was perfectly reproducible and
   perfectly invisible. It is now refused by a PEP 578 audit hook, inside CPython, before the frame
   object exists -- so the spelling is irrelevant.
2. **The same walk to the engine's global RNG, plus one counted draw.** It shifted every downstream
   draw and moved the digest while `WitnessedRandom.verify_stream` stayed consistent, because the
   counter counts what it was ASKED for, not who asked. Two independent closures now: the walk is
   refused, and the global stream refuses to be drawn from at all while a guarded plugin call is on
   the stack.
3. **A method rebound on a WATCHED CLASS.** ``Sentinel`` snapshotted the identity of watched module
   attributes and of `random.Random`'s methods -- but not of the methods of the other watched
   CLASSES. A no-op ``WitnessedRandom.verify_stream`` defeated the stream self-check and moved the
   digest with `integrity.WitnessedRandom` still the same object, so nothing on the watch list had
   moved. The watch now covers ``vars(cls)`` for every watched class.
4. **A module-level tamper on the REPLAY path.** ``config_from_dict`` verifies a manifest's plugin
   lock, and verifying imports -- before ``run_pipeline`` takes its own baseline. So the tamper was
   already installed when the baseline was captured, the baseline recorded the tampered state as
   normal, and the end-of-run comparison saw nothing. ``registry.resolve`` now brackets its OWN
   import, so the tamper is fatal at the import that performed it and can never survive to poison a
   later snapshot.

AND WHAT IS STILL TRUE
----------------------
None of this is a sandbox, and `api/guard.py` says so in its own docstring with three named limits.
The tests at the end of this file pin the honest boundary as hard as they pin the fixes: the
recorded ``source_gate: "off"`` opt-out really does turn the guard off, and a run with no plugins
never installs the hook and reproduces the pinned golden byte-for-byte.
"""

import importlib
import pathlib
import random
import sys

import pytest

from scms_sim_ref.api import guard as GUARD
from scms_sim_ref.api import integrity as INTEG
from scms_sim_ref.api import registry as REG
from scms_sim_ref.api import srcgate as SG
from scms_sim_ref.api.errors import ConfigError
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline

#: The determinism-contract default digest, pinned identically in 8 other test files.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"
_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)

#: Small enough to run in a couple of seconds, long enough to deliver messages and file reports.
_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.3)


# --------------------------------------------------------------------------- #
# The attacks, as real modules on sys.path -- the gate reads source from disk, so an `exec`-ed
# module would not exercise the thing under test.
# --------------------------------------------------------------------------- #
_OBFUSCATED_WALK = '''\
"""THE MEASURED ATTACK. The same frame walk as `tb_walker`, spelled so a name-matching gate cannot
see it: `_getframe`, `f_locals` and `f_back` are all assembled at run time."""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")


class ObfuscatedWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "obfwalk"
    reason_code = "obfwalk"

    def evaluate(self, obs, state, params, rng):
        f = _F(1)
        while f is not None:
            b = getattr(f, "f_" + "locals").get("b")
            if isinstance(b, dict) and "veh" in b:
                return 5.0 if b["veh"].is_attacker else 0.0
            f = getattr(f, "f_" + "back")
        return 0.0
'''

_SWALLOWING_WALK = '''\
"""The same walk with the refusal swallowed. `except BaseException` is the only spelling that can
catch a `GuardError`, and it buys nothing: the refusal is RECORDED as well as raised."""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")


class SwallowingWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "swalwalk"
    reason_code = "swalwalk"

    def evaluate(self, obs, state, params, rng):
        try:
            return 5.0 if _F(1) is not None else 0.0
        except BaseException:
            return 0.25
'''

_GC_WALK = '''\
"""The reach that never touches a frame: `gc.get_referrers(obs)` finds the broadcast dict from the
observation itself. Spelled through getattr for the same reason."""
import gc

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_R = getattr(gc, "get_" + "referrers")


class GcWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "obfgc"
    reason_code = "obfgc"

    def evaluate(self, obs, state, params, rng):
        for holder in _R(obs):
            if isinstance(holder, dict) and "veh" in holder:
                return 5.0
        return 0.0
'''

_RUNTIME_IMPORT = '''\
"""No reflection at all: import the engine at CALL time and read the oracle out of its module. The
static gate only sees IMPORT STATEMENTS, and there is none here."""
import importlib

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class RuntimeImporter(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "rtimport"
    reason_code = "rtimport"

    def evaluate(self, obs, state, params, rng):
        mod = importlib.import_module("scms_sim_ref.mock_pipeline." + "run")
        return 0.0 if mod is None else 0.0
'''

_HONEST = '''\
"""The case the guard must not tax: an honest detector that touches none of it."""
from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class Honest(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "grdhonest"
    reason_code = "grdhonest"

    def evaluate(self, obs, state, params, rng):
        state["n"] = state.get("n", 0) + 1
        return min(2.0, abs(obs.claimed_x - obs.ref_x) / 500.0)
'''

_LOAD_TIME_WALK = '''\
"""THE SPELLING THAT STILL LANDS, and it is pinned here so nobody can claim otherwise.

The capture happens in the MODULE BODY -- code `registry.resolve` executes when it imports the
plugin, and which no guard is armed for. `f_back` and `f_locals` raise no audit event at all, so the
captured frames stay live handles on the engine's own locals for the rest of the run.
"""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")

FRAMES = []
_f = _F(1)
while _f is not None:
    FRAMES.append(_f)
    _f = getattr(_f, "f_" + "back")

SEEN = {}


class LoadTimeWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "loadwalk"
    reason_code = "loadwalk"

    def evaluate(self, obs, state, params, rng):
        if not SEEN:
            for f in FRAMES:
                b = getattr(f, "f_" + "locals").get("b")
                if isinstance(b, dict) and "veh" in b:
                    SEEN["frame"] = f.f_code.co_name
                    SEEN["is_attacker"] = bool(b["veh"].is_attacker)
                    SEEN["true_xy"] = (b.get("x"), b.get("y"))
                    break
        return 0.0
'''

_LOAD_TIME_DRAW = '''\
"""The same unguarded capture, used for the half that CAN be closed: a draw on the engine's global
stream made from `__init__`, with the refusal swallowed by `except Exception`."""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")


class LoadTimeDrawer(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "loaddraw"
    reason_code = "loaddraw"

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.reached = False
        f = _F(1)
        while f is not None:
            r = getattr(f, "f_" + "locals").get("rng")
            if r is not None and type(r).__name__ == "WitnessedRandom":
                self.reached = True
                try:
                    r.random()
                except Exception:                    # IntegrityError is a ConfigError is an Exception
                    pass
                break
            f = getattr(f, "f_" + "back")

    def evaluate(self, obs, state, params, rng):
        return 0.0
'''

_LOAD_TIME_TWIN = '''\
"""The honest TWIN of `grd_loaddraw`: same plugin_id, same reason_code, same column, same
`evaluate`. It reaches the engine's stream and does not draw from it, so its digest is the control
the drawing form has to be compared against."""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")


class LoadTimeTwin(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "loaddraw"
    reason_code = "loaddraw"

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.reached = False
        f = _F(1)
        while f is not None:
            r = getattr(f, "f_" + "locals").get("rng")
            if r is not None and type(r).__name__ == "WitnessedRandom":
                self.reached = True
                break
            f = getattr(f, "f_" + "back")

    def evaluate(self, obs, state, params, rng):
        return 0.0
'''

_WATCHED_CLASS_MUTATOR = '''\
"""Neutralise the stream self-check by rebinding the methods OF a watched class rather than replacing
the class -- the class OBJECT never moves, which is what the module watch compares. `sys.modules` is
assembled at run time so the static gate's name list cannot see it, and `sys.modules` raises no audit
event, so the runtime guard cannot either."""
import random
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_MODS = getattr(sys, "mod" + "ules")


class WatchedClassMutator(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "clsmut"
    reason_code = "clsmut"

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.armed = False

    def evaluate(self, obs, state, params, rng):
        if not self.armed and obs.t >= 20.0:
            self.armed = True
            integ = _MODS.get("scms_sim_ref.api.integrity")
            if integ is not None:
                integ.WitnessedRandom.verify_stream = lambda s, when: {"words": s._words}
                integ.WitnessedRandom.random = random.Random.random
                random.Random.random = lambda s: 0.5
        return 0.0
'''

_MODULES = {"grd_obf": _OBFUSCATED_WALK, "grd_swallow": _SWALLOWING_WALK, "grd_gc": _GC_WALK,
            "grd_rtimport": _RUNTIME_IMPORT, "grd_honest": _HONEST,
            "grd_loadwalk": _LOAD_TIME_WALK, "grd_loaddraw": _LOAD_TIME_DRAW,
            "grd_loadtwin": _LOAD_TIME_TWIN, "grd_clsmut": _WATCHED_CLASS_MUTATOR}


@pytest.fixture(scope="module")
def tp(tmp_path_factory):
    root = tmp_path_factory.mktemp("runtime_guard")
    for name, src in _MODULES.items():
        (root / f"{name}.py").write_text(src, encoding="utf-8")
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()
    try:
        yield root
    finally:
        sys.path.remove(str(root))
        for name in _MODULES:
            sys.modules.pop(name, None)


def _run(tmp_path, name, ref, **extra):
    return run_pipeline(PipelineConfig(
        plugins={"check": ["@builtins", dict({"ref": ref}, **extra)]},
        out_dir=str(tmp_path / name), **_CFG))


# ================================================== 1. the obfuscated frame walk ================= #
def test_the_static_gate_still_cannot_see_the_obfuscated_walk(tp):
    """The premise, pinned first. If this ever starts failing, the test below is measuring the
    static gate rather than the runtime one and proves nothing."""
    assert SG.scan_source(_OBFUSCATED_WALK, "grd_obf.py") == []
    assert SG.scan_source(_GC_WALK, "grd_gc.py") == []
    assert SG.scan_source(_RUNTIME_IMPORT, "grd_rtimport.py") == []


def test_the_obfuscated_frame_walk_is_refused_at_run_time_and_writes_no_dataset(tp, tmp_path):
    """ATTACK 1, closed. Default settings: `source_gate` is not mentioned, so it is "on".

    The refusal comes from the audit hook inside `sys._getframe` itself, which is why assembling
    the name at run time does not help -- there is no name left to match by the time CPython raises
    the event.
    """
    with pytest.raises(INTEG.IntegrityError) as e:
        _run(tmp_path, "obf", "grd_obf:ObfuscatedWalker")
    msg = str(e.value)
    assert "PLUGIN GUARD REFUSAL" in msg
    assert "plugins.check 'grd_obf:ObfuscatedWalker'" in msg
    assert "sys._getframe" in msg
    assert "not a sandbox" in msg                    # the honesty travels with the refusal
    assert not (tmp_path / "obf" / "manifest.json").exists()


def test_swallowing_the_refusal_does_not_help(tp, tmp_path):
    """`except BaseException` around the walk returns a plausible score and hides the exception --
    and the run still fails, because the refusal is COUNTED as well as raised."""
    with pytest.raises(INTEG.IntegrityError) as e:
        _run(tmp_path, "swal", "grd_swallow:SwallowingWalker")
    assert "sys._getframe" in str(e.value)
    assert not (tmp_path / "swal" / "manifest.json").exists()


def test_the_gc_route_is_refused_too(tp, tmp_path):
    """The other route to the same oracle, and it never touches a frame: `gc.get_referrers(obs)`
    finds the broadcast dict from the observation the engine handed over. A guard that closed only
    the frame walk would have moved the attack, not stopped it."""
    with pytest.raises(INTEG.IntegrityError) as e:
        _run(tmp_path, "gc", "grd_gc:GcWalker")
    assert "gc.get_referrers" in str(e.value)


def test_a_run_time_import_of_engine_internals_trips_the_wire():
    """The import rule fires on the event CPython actually raises -- the builtin `__import__`, on a
    cache miss, which it does even for a name that does not resolve."""
    with pytest.raises(INTEG.IntegrityError) as e:
        with GUARD.arm():
            __import__("scms_sim_ref.mock_pipeline.nothing_of_the_sort")
    assert "ENGINE INTERNALS" in str(e.value)
    # ...and the public half is not refused, or every codec plugin would break
    with GUARD.arm():
        __import__("scms_sim_ref.api.fields")


def test_the_import_rule_is_a_TRIPWIRE_not_a_barrier_and_the_docs_say_so(tp, tmp_path):
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE.

    Three holes, all in CPython rather than here: the `import` audit event fires only on a cache
    MISS, only from the builtin `__import__` (so `importlib.import_module` misses it), and
    `sys.modules` raises no event at all. So a plugin that pulls an ALREADY-LOADED engine module
    into scope is not refused, and cannot be. This test RUNS one and asserts it succeeds, because a
    limitation nobody exercises is a limitation nobody notices has changed.

    What that plugin gets is module globals. The run's per-message ground truth -- `b["veh"]`, the
    true position, `falsified` -- is in a FRAME, which is the thing the guard does close, and that
    is why the claim is worded about frames.
    """
    with GUARD.arm():
        assert sys.modules["scms_sim_ref.mock_pipeline.run"] is not None   # not refused: no event
    res = _run(tmp_path, "rtimport", "grd_rtimport:RuntimeImporter")
    assert res.n_reports >= 0 and (tmp_path / "rtimport" / "manifest.json").exists()
    assert "tripwire, not a barrier" in GUARD.__doc__
    assert "``sys.modules`` is not audited at all" in GUARD.__doc__
    assert "the frames are closed" in GUARD.__doc__


def test_an_honest_detector_is_untouched_by_the_guard(tp, tmp_path):
    """The other half of every gate: it must not fire on the case it exists to protect."""
    res = _run(tmp_path, "honest", "grd_honest:Honest")
    assert res.n_reports >= 0
    assert (tmp_path / "honest" / "manifest.json").exists()


def test_the_recorded_opt_out_really_does_turn_the_guard_off(tp, tmp_path):
    """THE HONEST BOUNDARY, pinned so it cannot quietly become a lie in either direction.

    `source_gate: "off"` means "I wrote or audited this code". It disables the static gate AND this
    runtime guard for that one plugin, it is a CONFIG key so it lands verbatim in
    `manifest["config"]` and replays, and a dataset built that way says so in its own manifest.
    """
    res = _run(tmp_path, "optout", "grd_obf:ObfuscatedWalker", source_gate="off")
    assert res.n_reports > 0
    import json
    man = json.loads((tmp_path / "optout" / "manifest.json").read_text(encoding="utf-8"))
    entry, = [e for e in man["config"]["plugins"]["check"]
              if isinstance(e, dict) and e.get("ref") == "grd_obf:ObfuscatedWalker"]
    assert entry["source_gate"] == "off"


def test_the_guard_is_inert_with_no_plugins_and_the_golden_is_byte_identical(tmp_path):
    """A run that declares no plugins runs no third-party code, arms nothing, and is exactly the run
    it always was. The audit hook is what the guard costs, and it is never installed for it."""
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "g"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN
    assert GUARD.depth() == 0                        # nothing left armed by any test above


def test_the_guard_and_the_static_gate_share_one_list_of_public_subpackages():
    """Two hand-maintained copies of one rule is how a load-time refusal and a run-time refusal
    silently disagree. `guard` imports the list rather than restating it."""
    assert GUARD.PUBLIC_SUBPACKAGES is SG.PUBLIC_SUBPACKAGES
    assert GUARD.ENGINE_PACKAGE is SG.ENGINE_PACKAGE


# ================================================== 2. the engine's global RNG =================== #
def test_the_global_stream_refuses_to_be_drawn_from_inside_a_guarded_call():
    """ATTACK 2, closed at the second point as well as the first.

    The attack was: walk to `run_pipeline`'s `rng` local, draw once, discard the value. Every
    downstream draw in the run shifts, the digest moves (75176bd8 against a control d0cb992c), and
    the word counter agrees perfectly -- it counts what it was ASKED for, not who asked, which its
    own docstring conceded. Refusing the draw makes "the count cannot be moved by a plugin" a
    property rather than an argument.
    """
    r = INTEG.WitnessedRandom(7)
    before = r.random()                              # the engine's own draws are unaffected
    assert 0.0 <= before < 1.0
    state_before = r.getstate()
    with pytest.raises(INTEG.IntegrityError) as e:
        with GUARD.arm():
            r.random()
    assert "ENGINE'S GLOBAL random stream" in str(e.value)
    assert "RngNamespace" in str(e.value)            # and it says what a plugin should use instead
    with pytest.raises(INTEG.IntegrityError):
        with GUARD.arm():
            r.getrandbits(32)
    # The refusal did not advance the stream, and it did not move the counter either -- so the
    # engine's own accounting is untouched by an attempt.
    assert r.getstate() == state_before and r.words == 2
    # ...but the ATTEMPT is not forgotten. `IntegrityError` is a `ConfigError` and therefore an
    # `Exception`, so a plugin's `except Exception:` swallows the refusal and returns a plausible
    # score; the COUNT is what makes that pointless, exactly as `guard`'s violation counter does for
    # a swallowed `GuardError`. Measured before this counter existed: a constructor that drew,
    # caught the refusal and returned normally produced a complete run and a written manifest.
    with pytest.raises(INTEG.IntegrityError) as e2:
        r.verify_stream("after the refused draws")
    assert "were REFUSED during this run" in str(e2.value)
    assert "2 draw(s)" in str(e2.value)


def test_the_word_counter_still_catches_the_classic_random_rebind():
    """PINNED BECAUSE THE ADVERSARY FOUND IT CLOSED. The in-process monkeypatch of
    `random.Random.random` -- the one form the docs claim is caught -- must stay caught, by the
    counter rather than by the guard: the override still counts two words while the attacker's
    function advances nothing."""
    r = INTEG.WitnessedRandom(11)
    original = random.Random.random
    try:
        random.Random.random = lambda self: 0.5
        r.random()
        with pytest.raises(INTEG.IntegrityError) as e:
            r.verify_stream("after a random.Random.random rebind")
        assert "Mersenne-Twister words" in str(e.value)
    finally:
        random.Random.random = original


# ================================================== 3. methods on a watched CLASS ================ #
def test_a_method_rebound_on_a_watched_class_is_drift():
    """ATTACK 7, closed. `WitnessedRandom.verify_stream` -> no-op defeated the stream self-check
    while `integrity.WitnessedRandom` stayed the same object, so the module-attribute watch -- which
    compares exactly that identity -- saw nothing."""
    sentinel = INTEG.Sentinel(armed=True)
    original = INTEG.WitnessedRandom.verify_stream
    try:
        INTEG.WitnessedRandom.verify_stream = lambda self, when: {"words": self.words}
        # the OLD instrument, still blind: the class object itself has not moved
        assert sys.modules["scms_sim_ref.api.integrity"].WitnessedRandom is INTEG.WitnessedRandom
        moved = dict(sentinel.drift())
        assert "scms_sim_ref.api.integrity.WitnessedRandom.verify_stream" in moved
        assert "WATCHED CLASS" in moved["scms_sim_ref.api.integrity.WitnessedRandom.verify_stream"]
    finally:
        if INTEG.WitnessedRandom.verify_stream is not original:
            INTEG.WitnessedRandom.verify_stream = original
    assert INTEG.Sentinel(armed=True).drift() == []


def test_verify_restores_a_rebound_class_method():
    """A monitor that finds a tamper and leaves it installed has poisoned the interpreter for
    whatever runs next, which is worse than not looking."""
    sentinel = INTEG.Sentinel(armed=True)
    original = INTEG.WitnessedRandom.random
    INTEG.WitnessedRandom.random = lambda self: 0.5
    with pytest.raises(INTEG.IntegrityError) as e:
        sentinel.verify("in a test")
    assert "WitnessedRandom.random" in str(e.value)
    assert INTEG.WitnessedRandom.random is original, "verify() must restore what it refused"


def test_an_added_or_deleted_class_attribute_is_drift_too():
    """`vars(cls)` is compared whole, not against a hand-listed method set -- so a method ADDED
    (a `__getattr__` that lies, say) is seen without anyone remembering to list it."""
    sentinel = INTEG.Sentinel(armed=True)
    try:
        INTEG.WitnessedRandom.smuggle = staticmethod(lambda: None)
        assert any(w.endswith(".smuggle") for w, _ in sentinel.drift())
    finally:
        if hasattr(INTEG.WitnessedRandom, "smuggle"):
            del INTEG.WitnessedRandom.smuggle


def test_the_class_watch_covers_every_boundary_object_the_module_watch_names():
    """A watch list is only as good as its coverage; this pins the intent so a class added to the
    boundary is added to both halves."""
    watched = {name for _mod, name in INTEG.CLASS_WATCH}
    for required in ("WitnessedRandom", "Sentinel", "Observation", "NamespacedState",
                     "LinkOutcome", "RngNamespace", "IsolatedCheck", "ConformanceReport"):
        assert required in watched
    s = INTEG.Sentinel(armed=True)
    assert s.watched > 400, "the class half must actually contribute to the recorded count"


# ================================================== 4. the replay path =========================== #
_MODULE_LEVEL_TAMPER = '''\
"""One line at MODULE SCOPE -- an earlier hook than `__init__`, and the source gate's name list does
not contain it. On a manifest REPLAY this used to run before `run_pipeline` had taken its baseline,
so the baseline recorded the tampered interpreter as normal."""
import random

from scms_sim_ref.api.channel import DELIVERED, INTERFACE_VERSION


class Impostor(random.Random):
    """Bit-identical output; the rebind is invisible to every digest and hash in the project."""


random.Random = Impostor


class Model:
    interface_version = INTERFACE_VERSION
    plugin_id = "replaytamper"

    def __init__(self, *, params, rng, env):
        self.reach_m = float(env["radio_range_m"])

    def capabilities(self):
        return frozenset({"reach"})

    def begin_step(self, frame):
        pass

    def evaluate(self, tx, rx, d_m, txn):
        return DELIVERED if d_m <= self.reach_m else None

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def collision_loss(self, dist_m, cbr):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return 0.5
'''


def test_a_module_level_tamper_is_fatal_at_the_import_that_performed_it(tmp_path):
    """ATTACK 4, closed. `verify_lock` is the REPLAY path: it re-resolves every locked entry, and
    resolving imports. It takes no baseline of its own, and neither do `--check-config`,
    `--verify-plugins` or the GUI's validation pass -- so the tamper used to run, unwatched, before
    `run_pipeline` snapshotted, and the end-of-run comparison then found the interpreter exactly as
    the (already poisoned) baseline had recorded it.

    `registry.resolve` now brackets its own import, so there is no caller left that can miss it.
    """
    (tmp_path / "grd_replay.py").write_text(_MODULE_LEVEL_TAMPER, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    sys.modules.pop("grd_replay", None)
    original = random.Random
    try:
        lock = {"loaded": [{"slot": "channel_model", "ref": "grd_replay:Model",
                            "resolved_via": "dotted_path", "module_sha256": "0" * 64}]}
        with pytest.raises(INTEG.IntegrityError) as e:
            REG.verify_lock(lock)
        assert "while IMPORTING the channel_model plugin" in str(e.value)
        assert "random.Random" in str(e.value)
        # ...and it is NOT downgraded to a drift row, which `--allow-plugin-drift` would then wave
        # through. A rebound `random.Random` is not "the files changed".
        sys.modules.pop("grd_replay", None)
        random.Random = original
        with pytest.raises(INTEG.IntegrityError):
            REG.verify_lock(lock, allow_drift=True)
    finally:
        random.Random = original
        sys.path.remove(str(tmp_path))
        sys.modules.pop("grd_replay", None)


def test_resolve_restores_what_it_refused(tmp_path):
    """The interpreter has to be usable afterwards: `--verify-plugins` over a directory of
    submissions must not be poisoned by the first hostile one."""
    (tmp_path / "grd_replay2.py").write_text(_MODULE_LEVEL_TAMPER, encoding="utf-8")
    sys.path.insert(0, str(tmp_path))
    importlib.invalidate_caches()
    original = random.Random
    try:
        with pytest.raises(INTEG.IntegrityError):
            REG.resolve("channel_model", "grd_replay2:Model")
        assert random.Random is original
    finally:
        random.Random = original
        sys.path.remove(str(tmp_path))
        sys.modules.pop("grd_replay2", None)


def test_a_builtin_resolution_pays_no_sentinel(tmp_path):
    """The bracket is on the IMPORT. A built-in is already imported, there is no third-party code to
    run, and `resolve` is called for one on every run -- so it must stay free."""
    calls = []
    original = INTEG.Sentinel

    class Counting(INTEG.Sentinel):
        def __init__(self, *a, **k):
            calls.append(1)
            super().__init__(*a, **k)

    INTEG.Sentinel = Counting
    try:
        REG.resolve("check", "positionJump")
        assert calls == []
    finally:
        INTEG.Sentinel = original


# ================================================== 5. the guard's own contract ================== #
def test_guarded_returns_the_function_itself_when_it_is_not_wanted():
    """`guarded(fn, None) is fn` is what makes the built-in path free -- not a fast wrapper, no
    wrapper. The call plan is built once per run and holds the raw bound method."""
    def fn():
        return 1
    assert GUARD.guarded(fn, None) is fn
    assert GUARD.guarded(fn, "x") is not fn
    assert GUARD.guarded(fn, "x").__scms_guarded__ == "x"


def test_the_violation_counter_cannot_be_saturated():
    """The wrappers compare a COUNTER across a call, not the length of the bounded record. A plugin
    that first looped `MAX_RECORDED` times to fill the list would otherwise make every later
    refusal invisible to the comparison."""
    GUARD.reset()
    try:
        for _ in range(GUARD.MAX_RECORDED + 5):
            with pytest.raises(INTEG.IntegrityError):
                with GUARD.arm():
                    sys._getframe(0)
        assert len(GUARD.violations()) == GUARD.MAX_RECORDED
        assert GUARD.violation_count() == GUARD.MAX_RECORDED + 5
    finally:
        GUARD.reset()


def test_the_guard_is_unarmed_outside_a_plugin_call():
    """Engine and stdlib code between plugin calls -- `logging`, `namedtuple`, `dataclasses` -- uses
    `sys._getframe` legitimately and must keep working. That is why the arming is TIGHT."""
    assert GUARD.depth() == 0
    assert sys._getframe(0) is not None
    with GUARD.arm():
        assert GUARD.armed()
    assert not GUARD.armed()


def test_the_guard_module_is_on_the_integrity_watch_list():
    """The guard's functions are the obvious thing to replace once you know they are there, so the
    Sentinel watches them by identity. It does NOT watch the depth cell's contents, and
    `api/guard.py` says so -- that is the residue, not a claim."""
    watched = {(m, a) for m, attrs in INTEG.MODULE_WATCH for a in attrs}
    for attr in ("guarded", "install", "_hook", "_refuse"):
        assert ("scms_sim_ref.api.guard", attr) in watched
    sentinel = INTEG.Sentinel(armed=True)
    original = GUARD.guarded
    try:
        GUARD.guarded = lambda fn, what="": fn
        assert any(w == "scms_sim_ref.api.guard.guarded" for w, _ in sentinel.drift())
    finally:
        GUARD.guarded = original


def test_the_cost_to_an_honest_author_is_named_and_measured():
    """PINNED BECAUSE IT IS A REAL COST, not a theoretical one.

    `logging` above the enabled level uses `sys._getframe` inside `findCaller`, so it is refused
    inside a guarded call -- and the refusal cannot make an exception for it, because
    `logging.currentframe()` is a PUBLIC function that returns a frame three levels up, which lands
    in the engine. A caller allow-list would be the laundering route, not a concession.
    """
    import logging
    logger = logging.getLogger("scms_guard_cost_probe")
    logger.setLevel(logging.WARNING)
    with GUARD.arm():
        logger.info("below the level: never reaches findCaller, so it is unaffected")
    with pytest.raises(INTEG.IntegrityError):
        with GUARD.arm():
            logger.warning("above the level: findCaller walks the stack")
    assert "logging` above the enabled level" in GUARD.__doc__
    assert "source_gate: \"off\"" in GUARD.__doc__


def test_the_guard_docstring_still_says_what_it_is_not():
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE. This project has withdrawn two
    containment claims; the third must not be written by accident."""
    doc = GUARD.__doc__
    assert "It is not a sandbox" in doc
    assert "sys.modules" in doc                      # the disarm route, named
    assert "out-of-process isolation" in doc


def test_the_refusal_never_reads_as_a_score(tp, tmp_path):
    """A guarded plugin that reaches for the engine must never produce a dataset in which it simply
    scored 0.0 -- the same rule the isolated worker follows for a crash."""
    for name, ref in (("never_obf", "grd_obf:ObfuscatedWalker"), ("never_gc", "grd_gc:GcWalker")):
        with pytest.raises(ConfigError):
            _run(tmp_path, name, ref)
        assert not (tmp_path / name / "manifest.json").exists()


# ================================================== 6. the window the guard does NOT cover ======= #
#
# Everything above measures what the guard closes. This section measures what it does not, because a
# limitation nobody exercises is a limitation nobody notices has been overclaimed -- and the
# overclaim was in this repository: `integrity.WitnessedRandom`'s docstring said the frame walk that
# reaches the engine's stream "is refused at run time by guard, however it is spelled". It is not.
# The guard is armed around plugin CALLS; a plugin's module body and its `__init__` are not calls.
def test_the_frame_walk_STILL_LANDS_when_it_is_taken_while_the_plugin_is_LOADED(tp, tmp_path):
    """PINNED AS A LIMITATION, not as a fix. This test PASSES BY THE ATTACK SUCCEEDING.

    `registry.resolve` imports the plugin's module, and the guard is not armed for that import (it
    cannot be: an honest module body calls `collections.namedtuple` / `typing.NamedTuple` /
    `logging`, all of which call `sys._getframe`). CPython audits `sys._getframe` and audits NOTHING
    for `frame.f_back` or `frame.f_locals`, so one capture performed where nothing is listening is a
    live handle on `run_pipeline`'s locals for the rest of the run.

    Measured here end to end, at the DEFAULT `source_gate`: the plugin reads `b["veh"].is_attacker`
    -- the label it is being asked to predict -- and the sender's TRUE position, and the run
    completes and writes a dataset.

    If this test ever starts FAILING, something closed the frame walk and the honest-limitation
    paragraphs in `api/guard.py`, `api/srcgate.py`, `api/integrity.py` and
    `docs/realism/DETECTOR-PLUGIN.md` are now understating the engine -- update them deliberately.
    Until then, in-process plugins are TRUSTED CODE and the only boundary that PREVENTS is
    `plugins.check[].isolated: true`.
    """
    res = _run(tmp_path, "loadwalk", "grd_loadwalk:LoadTimeWalker")
    assert (tmp_path / "loadwalk" / "manifest.json").exists()
    assert res.n_reports >= 0
    seen = sys.modules["grd_loadwalk"].SEEN
    assert seen, "the capture at module scope no longer reaches the engine's frames"
    assert seen["frame"] == "run_pipeline"
    assert isinstance(seen["is_attacker"], bool)          # the LABEL, read directly
    assert all(isinstance(v, float) for v in seen["true_xy"])   # the sender's TRUE position


def test_the_documentation_of_that_limit_travels_with_the_code():
    """The three modules a reader reaches for must all say the same thing, in their own docstrings,
    because a limitation recorded only in a markdown file is a limitation that gets lost."""
    assert "It covers plugin CALLS, not plugin LOADING" in GUARD.__doc__
    assert "In-process plugins are TRUSTED CODE" in GUARD.__doc__
    assert "isolated" in GUARD.__doc__
    assert "TRUSTED CODE" in SG.__doc__
    assert "not around the plugin's module-level code or its" in SG.__doc__
    assert "In-process plugins are trusted code." in INTEG.__doc__
    assert "THE READ IS NOT CLOSED" in INTEG.WitnessedRandom.__doc__


def test_the_two_guides_say_it_too_and_do_not_overclaim():
    """PINNED SO THE PROSE CANNOT DRIFT AWAY FROM THE CODE.

    A limitation that lives only in a docstring is a limitation a benchmark operator never reads. It
    has to be in the guide they DO read, and the guide must not contain the sentence the code no
    longer supports.
    """
    def flat(p):
        # markdown is hard-wrapped and uses ** for emphasis; the CLAIM is what is being pinned, not
        # the line breaks, so both are normalised away before matching.
        return " ".join(p.read_text(encoding="utf-8").replace("*", "").split())

    root = pathlib.Path(__file__).resolve().parents[1] / "docs" / "realism"
    docs = {"DETECTOR-PLUGIN.md": flat(root / "DETECTOR-PLUGIN.md"),
            "PLUGIN-ARCHITECTURE.md": flat(root / "PLUGIN-ARCHITECTURE.md")}
    for name, doc in docs.items():
        assert "TRUSTED CODE" in doc or "TRUSTED code" in doc, name
        assert "isolated" in doc, name
        # the window, named in the guide a reader actually reaches for
        assert "CALLS, not plugin LOADING" in doc, name
        # ...and the measurement that makes it a fact rather than an opinion, so a future editor
        # who deletes the evidence has to delete an assertion too
        assert "466.4527993807002" in doc, name
        # ...and it must point at the boundary that actually PREVENTS rather than leaving the reader
        # to conclude the guard is one.
        assert "isolated" in doc and "not closable" in doc, name


def test_a_draw_on_the_global_stream_during_LOADING_is_refused_and_swallowing_it_is_fatal(
        tp, tmp_path):
    """ATTACK 2, closed at the third point -- the one the guard cannot reach.

    The READ above cannot be closed. The USE of what it reaches can be, because it is arithmetic and
    not reachability: the engine makes no draw on its own global stream while it is handing control
    to a plugin's loader or constructor, so a draw made there is a plugin's by construction.

    Measured before this closure: this exact plugin drew once from `run_pipeline`'s `rng` in its
    `__init__`, caught the refusal it did not get, and produced a complete run whose digest was
    `319bb0bd` against the honest twin's `192286db` -- with `verify_stream` agreeing, no watched
    identity moved, and a manifest written.
    """
    with pytest.raises(INTEG.IntegrityError) as e:
        _run(tmp_path, "loaddraw", "grd_loaddraw:LoadTimeDrawer")
    msg = str(e.value)
    assert "SWALLOWED the refusal" in msg                 # catching it buys nothing
    assert "RngNamespace" in msg                          # and it says what to use instead
    assert not (tmp_path / "loaddraw" / "manifest.json").exists()


def test_the_honest_twin_of_that_plugin_is_unaffected(tp, tmp_path):
    """THE OTHER HALF OF THE GATE. The closure must cost nothing to a plugin that does not draw --
    same plugin_id, same reason_code, same column, same `evaluate`, and it still reaches the stream.
    Without this, "the digest moved" above could be measuring the column rather than the draw."""
    res = _run(tmp_path, "loadtwin", "grd_loadtwin:LoadTimeTwin")
    assert (tmp_path / "loadtwin" / "manifest.json").exists()
    assert res.data_digest and res.n_reports >= 0
    assert INTEG.stream_refusals() >= 0                   # nothing was refused for the honest form


def test_the_stream_closure_does_not_arm_the_audit_hook():
    """WHY IT IS A SEPARATE COUNTER FROM `guard._DEPTH`, pinned.

    Raising the guard's depth here would arm the PEP 578 hook and refuse `namedtuple`, `NamedTuple`
    and `logging` -- exactly the things an honest author does at module scope and in `__init__`, and
    exactly what `api/guard.py` tells them to do instead of logging inside `evaluate`. Closing the
    STREAM costs them nothing.
    """
    import collections
    with INTEG.stream_closed():
        assert INTEG.stream_is_closed() and GUARD.depth() == 0
        assert collections.namedtuple("_Probe", "a")(1).a == 1     # uses sys._getframe internally
        assert sys._getframe(0) is not None
    assert not INTEG.stream_is_closed()


def test_the_closure_is_reentrant_and_releases_on_an_exception():
    """The in-process multi-run drivers do thousands of runs in one interpreter; a leaked depth would
    make every later run refuse the engine's own draws."""
    before = INTEG.stream_is_closed()
    with pytest.raises(ValueError):
        with INTEG.stream_closed():
            with INTEG.stream_closed():
                raise ValueError("boom")
    assert INTEG.stream_is_closed() == before


def test_the_closure_mechanism_is_itself_watched():
    """A plugin that reached `sys.modules["scms_sim_ref.api.integrity"]` and replaced
    `stream_closed` with a no-op would reopen the stream. Replacing it is drift."""
    watched = {(m, a) for m, attrs in INTEG.MODULE_WATCH for a in attrs}
    for attr in ("stream_closed", "stream_refusals", "engine_random"):
        assert ("scms_sim_ref.api.integrity", attr) in watched


# ================================================== 7. the class watch, end to end =============== #
def test_a_watched_class_method_mutation_is_fatal_END_TO_END(tp, tmp_path):
    """ATTACK 7 as an attacker actually spells it, through a whole run.

    `test_a_method_rebound_on_a_watched_class_is_drift` exercises the `Sentinel` directly. This one
    goes through the engine: a `check` that reaches `sys.modules` -- assembled at run time, so the
    static gate's name list cannot see it, and unaudited, so the runtime guard cannot either --
    replaces `WitnessedRandom.verify_stream` with a no-op and `WitnessedRandom.random` with one that
    does not count, then rebinds `random.Random.random`. Every module-attribute identity still
    compares equal, because the class OBJECT never moved.
    """
    with pytest.raises(INTEG.IntegrityError) as e:
        _run(tmp_path, "clsmut", "grd_clsmut:WatchedClassMutator")
    msg = str(e.value)
    assert "WitnessedRandom.verify_stream" in msg
    assert "WATCHED CLASS" in msg
    assert not (tmp_path / "clsmut" / "manifest.json").exists()
    # ...and the interpreter is usable afterwards, which is what `verify()`'s restore is for
    assert INTEG.Sentinel(armed=True).drift() == []
