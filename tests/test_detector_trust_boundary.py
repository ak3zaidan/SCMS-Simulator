"""The detector seam's TRUST BOUNDARY: what it actually guarantees, asserted rather than claimed.

`tests/test_detector_plugins.py` pins the FUNCTIONAL claim -- a third-party thresholding detector
runs with zero engine edits, its knob moves the metrics, the lock and the drift gate work. That
claim held and still holds. This file pins the SAFETY claim, which did not.

The defect, measured on this engine before any of the fixes below, from inside a `Check.evaluate()`::

    b = sys._getframe(1).f_locals["b"]
    b["veh"].is_attacker      -> True                  # the label the detector is predicting
    b["veh"].attack_type      -> 'ConstPosOffset'
    b["x"], b["y"]            -> the sender's TRUE position
    b["falsified"], b["ghost"]
    sys._getframe(1).f_locals["rng"] -> the engine's single global random.Random

A detector scoring that fired on 1878 of 2297 reports, perfectly deterministically, and every other
layer of this project -- the pinned goldens, the content-hash lock, the two-run equality gate,
`Observation`'s field list -- was blind to it, because reading the oracle is reproducible.

**The architectural truth this file is written around: an in-process Python plugin cannot be
sandboxed.** `sys._getframe`, `gc.get_referrers`, `inspect` and module globals are reachable from any
callable the engine invokes. So every test here asserts one of three things, and never "it is
impossible":

1. the boundary refuses what an HONEST plugin would do by accident (a write to the shared
   observation, a reserved state key, a duplicate column, a self-registration);
2. the source gate turns the deliberate reach into a NAMED refusal at load time;
3. the residual escapes are exactly where the documentation says they are -- pinned, so the docs
   cannot quietly become false.
"""

import dataclasses
import importlib
import json
import sys

import pytest

from scms_sim_ref.api import detect as apidet
from scms_sim_ref.api import registry as apireg
from scms_sim_ref.api import srcgate as SG
from scms_sim_ref.api.errors import ConfigError
from scms_sim_ref.conformance.v1.detect import observation
from scms_sim_ref.datagen import featurize as FZ
from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
from scms_sim_ref.mock_pipeline import detectors as DET
from scms_sim_ref.mock_pipeline import run as RM

#: The determinism-contract default digest, pinned identically in 7 other test files.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.25)


# --------------------------------------------------------------------------- #
# Third-party modules, written to real files so the SOURCE GATE has source to read
# --------------------------------------------------------------------------- #
_CLEAN = '''\
"""An honest third-party detector distribution. Imports only the published api."""
import math

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase, FusionBase, ReportDecision
from scms_sim_ref.api.fields import FieldSpec
from scms_sim_ref.api import fields              # `from scms_sim_ref.api import X` must pass
from scms_sim_ref import api                     # ...and so must this


class RangeCheck(CheckBase):
    """One scalar threshold on one MA-visible residual."""

    interface_version = INTERFACE_VERSION
    plugin_id = "tpclean"
    reason_code = "claimedRange"
    precision = 3

    @classmethod
    def config_fields(cls):
        return {"max_range_m": FieldSpec("float", 250.0, "plausible claimed distance",
                                         lo=1.0, hi=5000.0, unit="m"),
                "tolerance_m": FieldSpec("float", 100.0, "excess distance scoring 1.0",
                                         lo=1.0, hi=100000.0, unit="m")}

    def evaluate(self, obs, state, params, rng):
        d = math.hypot(obs.claimed_x - obs.rx_x, obs.claimed_y - obs.rx_y)
        state["seen"] = state.get("seen", 0) + 1
        return max(0.0, d - params["max_range_m"]) / params["tolerance_m"]


class AliasCheck(RangeCheck):
    """A DIFFERENT ref that resolves to the SAME (plugin_id, reason_code) -- and therefore the same
    column. The de-duplication must catch this; de-duplicating by `ref` cannot."""


class StateFusion(FusionBase):
    """A third-party fusion that keeps state. It must get the same NamespacedState a check gets."""

    interface_version = INTERFACE_VERSION
    plugin_id = "tpfuse"

    def decide(self, scores, state, obs, params, rng):
        state["seen"] = state.get("seen", 0) + 1
        fired = [k for k in self.keys if scores[k] >= 1.0]
        if not fired:
            return None
        fired.sort(key=lambda k: -scores[k])
        return ReportDecision(True, fired, scores[fired[0]], max(scores.values()))


class HistoryClobberFusion(FusionBase):
    """A third-party fusion that writes the engine's RESERVED claim-history key.

    Unwrapped -- which is what the fusion slot did -- this silently empties the per-link history
    every history-bearing check compares against, for the rest of that link's life.
    """

    interface_version = INTERFACE_VERSION
    plugin_id = "tpclob"

    def decide(self, scores, state, obs, params, rng):
        state["h"] = []
        return None
'''

_WALKER = '''\
"""The measured attack: read the labels out of the caller's frame."""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class FrameWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "walker"
    reason_code = "walk"

    def evaluate(self, obs, state, params, rng):
        b = sys._getframe(1).f_locals["b"]
        return 5.0 if b["veh"].is_attacker else 0.0
'''

_GC_WALKER = '''\
"""The same reach, through the garbage collector instead of the frame stack."""
import gc

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class GcWalker(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "gcwalk"
    reason_code = "gcwalk"

    def evaluate(self, obs, state, params, rng):
        for holder in gc.get_referrers(obs):
            if isinstance(holder, dict) and "veh" in holder:
                return 5.0
        return 0.0
'''

_ENGINE_IMPORT = '''\
"""No frame walk needed: import the engine and read the oracle out of its own module."""
from scms_sim_ref.mock_pipeline import run as engine

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase


class Importer(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "importer"
    reason_code = "importer"

    def evaluate(self, obs, state, params, rng):
        return 0.0 if engine is None else 0.0
'''

_OBFUSCATED = '''\
"""The SAME frame walk, spelled so a name-matching gate cannot see it.

Pinned as a test on purpose: the gate is a guard rail, not a sandbox, and a document that says so
is only credible if the repository proves it.
"""
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase

_F = getattr(sys, "_get" + "frame")


class Obfuscated(CheckBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "obfusc"
    reason_code = "obfusc"

    def evaluate(self, obs, state, params, rng):
        return 0.0
'''

_MODULES = {"tb_clean": _CLEAN, "tb_walker": _WALKER, "tb_gc": _GC_WALKER,
            "tb_engine_import": _ENGINE_IMPORT, "tb_obfuscated": _OBFUSCATED}


@pytest.fixture(scope="module")
def tp(tmp_path_factory):
    """Real .py files on sys.path -- installed, declared nowhere. The gate reads source from disk,
    so an `exec`-ed module would not exercise it."""
    root = tmp_path_factory.mktemp("trust_boundary")
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


@pytest.fixture
def registry_sandbox():
    """Snapshot/restore the process-global built-in registry.

    Necessary because the whole point of two tests below is that `register_builtin` is reachable
    from any imported module, which means a test exercising it would otherwise poison every test
    that runs after it in the same process.
    """
    before = dict(apireg._BUILTINS["check"])
    try:
        yield apireg
    finally:
        apireg._BUILTINS["check"].clear()
        apireg._BUILTINS["check"].update(before)


def _reports(res):
    with open(f"{res.out_dir}/ma/ma_reports.jsonl", encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


# ================================================================= 1. Observation immutability === #
def test_observation_refuses_a_write_through_every_path_reachable_by_name():
    """`frozen=True` overrides `__setattr__` ONLY. `object.__setattr__` skips that override and
    finds the slot's member_descriptor, which writes -- so the documented "you cannot mutate it"
    was false for one line of code, on an object EVERY check in the vector shares."""
    o = observation()
    baseline = {f: getattr(o, f) for f in apidet.OBSERVATION_FIELDS}
    with pytest.raises(dataclasses.FrozenInstanceError):
        o.claimed_x = 1.0
    for field in sorted(apidet.OBSERVATION_FIELDS):
        with pytest.raises(AttributeError):
            object.__setattr__(o, field, "tampered")       # <- this SUCCEEDED before the seal
        with pytest.raises(AttributeError):
            setattr(o, field, "tampered")
        with pytest.raises(AttributeError):
            object.__delattr__(o, field)
    assert {f: getattr(o, f) for f in apidet.OBSERVATION_FIELDS} == baseline


def test_the_observation_still_behaves_as_a_dataclass_after_the_seal():
    """The seal replaces `__init__` and every field descriptor, so the things that read the DTO --
    the conformance suite's `dataclasses.fields`, D2's declared vocabulary, keyword construction,
    equality, repr -- have to be asserted, not assumed."""
    o = observation()
    names = tuple(f.name for f in dataclasses.fields(apidet.Observation))
    assert names == apidet.OBSERVATION_FIELD_ORDER
    assert set(names) == set(apidet.OBSERVATION_FIELDS)
    kw = {f: getattr(o, f) for f in names}
    assert apidet.Observation(**kw) == o                   # keyword form (conformance builds these)
    assert apidet.Observation(*[kw[f] for f in names]) == o    # positional form (the engine's)
    assert "claimed_x" in repr(o)
    assert not hasattr(o, "__dict__")


def test_a_new_attribute_still_cannot_be_attached():
    o = observation()
    with pytest.raises((AttributeError, TypeError)):
        object.__setattr__(o, "is_attacker", True)


def test_the_seal_is_honest_about_the_one_escape_it_leaves():
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE.

    `api/detect.Observation` claims "no write BY NAME", not "immutable", because the saved slot
    descriptor is still reachable at `type(obs).<field>.fget.__self__`. Nothing in an in-process
    interpreter closes that. If a future change does close it, this test fails and the docstring
    and `docs/realism/DETECTOR-PLUGIN.md` section 2 must be upgraded to match.
    """
    o = observation()
    member = type(o).claimed_x.fget.__self__
    member.__set__(o, 4321.0)
    assert o.claimed_x == 4321.0


# ================================================================= 2. @builtins cannot be injected #
def test_builtins_expands_from_the_shipped_tuple_not_the_live_registry(registry_sandbox):
    """`@builtins` used to expand from `registry.builtin_names("check")` -- a view of a global dict
    that `register_builtin()` writes into and that importing a distribution is enough to reach. An
    installed-but-undeclared plugin could therefore add a column to a run that never named it."""

    class Injected(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "injected"
        reason_code = "injectedCheck"

        def evaluate(self, obs, state, params, rng):
            return 0.0

    registry_sandbox.register_builtin("check", "injectedCheck", Injected)
    # It IS in the live registry -- the injection itself is not prevented, and cannot be.
    assert "injectedCheck" in registry_sandbox.builtin_names("check")
    # ...and it reaches NEITHER the default suite nor the `@builtins` expansion.
    assert "injectedCheck" not in RM.default_check_refs()
    assert "injectedCheck" not in RM.default_check_refs(station_types=True, denm=True)
    assert "injectedCheck" not in RM.build_checks(PipelineConfig()).columns
    assert "injectedCheck" not in RM.build_checks(
        PipelineConfig(plugins={"check": ["@builtins"]})).columns
    assert RM.default_check_refs() == tuple(c.reason_code for c in DET.BUILTIN_CHECKS
                                            if getattr(c, "gate", None) is None)


def test_an_injected_registration_does_not_move_the_default_digest(tmp_path, registry_sandbox):
    """The digest form of the same property, because it is the one that matters: registering is not
    enabling, even for something that registered itself into the BUILT-IN table."""

    class Injected(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "injected2"
        reason_code = "injectedCheck2"

        def evaluate(self, obs, state, params, rng):
            return 7.0                                     # would fire on every single message

    registry_sandbox.register_builtin("check", "injectedCheck2", Injected)
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "inj"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


def test_a_hijacked_builtin_name_is_refused_by_identity(registry_sandbox):
    """The other half: `register_builtin` OVERWRITES per (slot, name), so a third party could bind
    itself to `positionJump` and be resolved as a BUILT-IN -- engine config fields for params, the
    raw (unrounded) cliff compare, no `x_` column namespacing, no source gate."""

    class Impostor(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "impostor"
        reason_code = "positionJump"

        def evaluate(self, obs, state, params, rng):
            return 0.0

    registry_sandbox.register_builtin("check", "positionJump", Impostor)
    with pytest.raises(ConfigError, match="register_builtin"):
        RM.build_checks(PipelineConfig())
    with pytest.raises(ValueError, match="register_builtin"):
        RM.validate_config(PipelineConfig(plugins={"check": ["positionJump"]}))


def test_a_hijacked_fusion_name_is_refused_too(registry_sandbox):
    class Impostor(apidet.FusionBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "impostorfusion"

        def decide(self, scores, state, obs, params, rng):
            return None

    before = dict(apireg._BUILTINS["fusion"])
    try:
        apireg.register_builtin("fusion", "streak_v1", Impostor)
        with pytest.raises(ConfigError, match="register_builtin"):
            RM.build_checks(PipelineConfig())
    finally:
        apireg._BUILTINS["fusion"].clear()
        apireg._BUILTINS["fusion"].update(before)


# ================================================================= 3. duplicate-column guard ===== #
def test_two_different_refs_resolving_to_one_column_are_refused(tp):
    """De-duplicating by `ref` cannot see this. Measured before the fix: `@builtins` + a check + a
    trivial subclass of it loaded N checks into N-1 distinct column slots, the per-message call plan
    ran that column twice, the second score silently overwrote the first in every report row, and
    the zero-template the engine copies per message was one entry short -- no warning anywhere."""
    plugins = {"check": ["@builtins",
                         {"ref": "tb_clean:RangeCheck"},
                         {"ref": "tb_clean:AliasCheck"}]}
    with pytest.raises(ConfigError, match="resolve to column"):
        RM.build_checks(PipelineConfig(plugins=plugins))
    with pytest.raises(ValueError, match="resolve to column"):
        RM.validate_config(PipelineConfig(plugins=plugins))
    # ...and the message names BOTH offending entries and the pair they share, so it is actionable.
    try:
        RM.build_checks(PipelineConfig(plugins=plugins))
    except ConfigError as e:
        msg = str(e)
    assert "tb_clean:RangeCheck" in msg and "tb_clean:AliasCheck" in msg
    assert "x_tpclean_claimedRange" in msg and "'tpclean'" in msg


def test_the_by_ref_guard_still_fires_first(tp):
    """The cheap half is kept: the same ref twice is caught without resolving anything."""
    with pytest.raises(ConfigError, match="more than once"):
        RM.build_checks(PipelineConfig(
            plugins={"check": [{"ref": "tb_clean:RangeCheck"}, {"ref": "tb_clean:RangeCheck"}]}))


def test_one_third_party_check_alongside_the_builtins_still_loads(tp, tmp_path):
    """The guard must refuse collisions and NOTHING else -- the shipped suite plus one plugin is the
    ordinary case and it has to stay ordinary."""
    suite = RM.build_checks(PipelineConfig(
        plugins={"check": ["@builtins", {"ref": "tb_clean:RangeCheck"}]}))
    assert suite.columns[-1] == "kalmanConsistency"
    assert "x_tpclean_claimedRange" in suite.columns
    assert len(set(suite.columns)) == len(suite.columns)


# ================================================================= 4. fusion namespacing ========= #
def test_a_third_party_fusion_gets_the_same_namespacing_a_check_gets(tp):
    """The fusion slot handed the RAW per-(rx, sender) dict to everyone. It is called once per
    message on the same `st` the check vector was just wrapped away from, so a third-party fusion
    could read and rewrite `h`, `streak` and `kf` with none of the discipline enforced one call
    earlier on the same object."""
    assert RM.build_checks(PipelineConfig()).fusion_wrap is None      # built-in keeps the raw dict
    suite = RM.build_checks(PipelineConfig(plugins={"fusion": {"ref": "tb_clean:StateFusion"}}))
    assert suite.fusion_wrap == "tpfuse"

    st = {"h": [(0.0, 0.0, 1.0, 0.0, 0.0)], "streak": {"positionJump": 2}, "touch": 3}
    ns = apidet.NamespacedState(st, suite.fusion_wrap)
    scores = dict.fromkeys(suite.keys, 0.0)
    assert suite.fusion.decide(scores, ns, observation(), suite.fusion_params,
                               suite.fusion_rng) is None
    assert st["plugin:tpfuse"] == {"seen": 1}               # its own namespace
    assert st["streak"] == {"positionJump": 2}              # the engine's, untouched
    assert st["h"] == [(0.0, 0.0, 1.0, 0.0, 0.0)]


def test_a_third_party_fusion_writing_a_reserved_key_is_refused_by_the_engine(tp, tmp_path):
    """End to end, through the real reception loop: the write RAISES instead of silently emptying
    the claim history every history-bearing check compares against."""
    with pytest.raises(ConfigError, match="reserved"):
        run_pipeline(PipelineConfig(
            plugins={"fusion": {"ref": "tb_clean:HistoryClobberFusion"}},
            out_dir=str(tmp_path / "clob"), **_CFG))


def test_the_builtin_fusion_is_deliberately_not_wrapped(tmp_path):
    """`streak_v1` reads and MUTATES `state["streak"]` on the raw dict, which is what every pinned
    golden was recorded on. Wrapping it would be a silent re-pin, so it is exempt by declaration."""
    suite = RM.build_checks(PipelineConfig())
    assert suite.fusion_wrap is None
    assert run_pipeline(PipelineConfig(out_dir=str(tmp_path / "g"),
                                       **_DEFAULT)).data_digest == DEFAULT_GOLDEN


# ================================================================= 5. NamespacedState honesty ==== #
def test_namespaced_state_says_what_it_does_and_does_not_guarantee():
    """The docstring used to say reads and writes land in one namespace "and nowhere else". Through
    the MAPPING INTERFACE that is true; as a capability claim it is false, and this pins both halves
    so neither can drift."""
    st = {"h": [(0.0, 0.0, 1.0, 0.0, 0.0)], "streak": {"positionJump": 2}, "touch": 3}
    ns = apidet.NamespacedState(st, "tpclean")
    ns["seen"] = 1
    assert st["plugin:tpclean"] == {"seen": 1} and dict(ns) == {"seen": 1}
    for reserved in sorted(apidet.RESERVED_STATE_KEYS):
        with pytest.raises(ConfigError):
            ns[reserved] = "mine"
    assert isinstance(ns["h"], tuple)                      # a copy, not the engine's list
    with pytest.raises(TypeError):
        ns["streak"]["positionJump"] = 99                  # a read-only proxy over a COPY
    assert st["streak"] == {"positionJump": 2}

    # THE ESCAPE, pinned. The wrapper holds the engine's dict; `object.__getattribute__` returns it.
    # Documented in `NamespacedState.__doc__` and in DETECTOR-PLUGIN.md section 2. If a future change
    # closes it, this test fails and both texts must be upgraded rather than left overstating.
    raw = object.__getattribute__(ns, "_st")
    assert raw is st
    doc = apidet.NamespacedState.__doc__
    assert "not a capability boundary" in doc and "process boundary" in doc


# ================================================================= 6. featurize column stability = #
def test_reason_columns_do_not_depend_on_which_codes_happened_to_fire():
    """The ML table's schema was a function of the RUN'S OUTCOME: a third-party check that fired at
    least once contributed a `reason_x_<id>_<code>` one-hot and the same detector at a stricter
    threshold contributed none, so two operating points of one sweep produced tables that cannot be
    concatenated, and a model fitted on train could not score val."""
    col = "detnorm_x_tpclean_claimedRange"
    fired = [{col: 2.0, "reason_codes": ["x_tpclean_claimedRange"]}, {col: 0.0, "reason_codes": []}]
    quiet = [{col: 0.0, "reason_codes": []}, {col: 0.0, "reason_codes": []}]
    assert FZ._observed_reasons(fired) == FZ._observed_reasons(quiet)
    assert "x_tpclean_claimedRange" in FZ._observed_reasons(quiet)
    assert FZ._observed_detectors(fired) == FZ._observed_detectors(quiet)
    # a code with no column at all (a dataset from another engine version) is still carried
    assert "fromElsewhere" in FZ._observed_reasons(quiet + [{"reason_codes": ["fromElsewhere"]}])


def test_the_ml_table_schema_is_identical_across_a_threshold_sweep(tp, tmp_path):
    """The same property end to end, on two real datasets from the same suite at two operating
    points -- one where the plugin fires and one where it cannot."""
    import pandas as pd
    cols = []
    for name, tol in (("loose", 100000.0), ("tight", 1.0)):
        out = str(tmp_path / name)
        res = run_pipeline(PipelineConfig(
            plugins={"check": ["@builtins",
                               {"ref": "tb_clean:RangeCheck",
                                "params": {"max_range_m": 1.0, "tolerance_m": tol}}]},
            out_dir=out, **_CFG))
        rows = _reports(res)
        fired = sum(1 for r in rows if r["detnorm_x_tpclean_claimedRange"] >= 1.0)
        FZ.build(out, split_seed=3)
        cols.append((fired, list(pd.read_parquet(f"{out}/ml/report_features.parquet").columns)))
    (n_loose, c_loose), (n_tight, c_tight) = cols
    assert n_loose == 0 and n_tight > 0                    # the two runs really do differ
    assert c_loose == c_tight                              # ...and their schemas do not
    assert "reason_x_tpclean_claimedRange" in c_loose
    assert "detnorm_x_tpclean_claimedRange" in c_loose


def test_the_featurize_vocabulary_is_not_a_view_of_the_live_registry(registry_sandbox):
    """Same injection vector, same fix: `featurize`'s built-in vocabulary is read from the shipped
    `detectors.BUILTIN_CHECKS` tuple, so an imported distribution cannot add a column to the ML
    tables of a dataset it had nothing to do with."""

    class Injected(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "injected3"
        reason_code = "injectedCheck3"

        def evaluate(self, obs, state, params, rng):
            return 0.0

    registry_sandbox.register_builtin("check", "injectedCheck3", Injected)
    assert FZ.DETECTORS == [c.reason_code for c in DET.BUILTIN_CHECKS]
    assert "injectedCheck3" not in FZ.DETECTORS
    assert "injectedCheck3" not in FZ.REASON_VOCAB
    assert "injectedCheck3" not in FZ._observed_detectors([{"detnorm_positionJump": 0.0}])


# ================================================================= 7. the source gate ============ #
def test_the_frame_walk_is_refused_at_resolution_naming_the_construct_and_line(tp, tmp_path):
    """THE defect, closed at the point it can be closed in-process: not silently, not at step k, but
    at plugin resolution, with the construct and the line number in the message."""
    plugins = {"check": ["@builtins", {"ref": "tb_walker:FrameWalker"}]}
    with pytest.raises(SG.SourceGateError) as ei:
        RM.build_checks(PipelineConfig(plugins=plugins))
    msg = str(ei.value)
    walk_line = next(i for i, ln in enumerate(_WALKER.splitlines(), 1) if "_getframe" in ln)
    assert "sys._getframe" in msg and "f_locals" in msg
    assert f"line {walk_line}:" in msg
    assert "guard rail, not a sandbox" in msg
    assert 'source_gate": "off"' in msg or "source_gate" in msg
    # and it never gets as far as producing a dataset
    with pytest.raises(SG.SourceGateError):
        run_pipeline(PipelineConfig(plugins=plugins, out_dir=str(tmp_path / "walk"), **_CFG))
    assert not (tmp_path / "walk").exists()


def test_the_gate_refuses_a_ConfigError_so_existing_handling_applies(tp):
    """`SourceGateError` is a `ConfigError`, which is what makes the refusal land on the engine's
    existing "fatal before step 0, no output directory" path rather than needing new plumbing."""
    assert issubclass(SG.SourceGateError, ConfigError)
    with pytest.raises(ConfigError):
        RM.build_checks(PipelineConfig(plugins={"check": [{"ref": "tb_walker:FrameWalker"}]}))
    with pytest.raises(ValueError):                        # ConfigError is a ValueError
        RM.validate_config(PipelineConfig(plugins={"check": [{"ref": "tb_walker:FrameWalker"}]}))


def test_the_gate_refuses_gc_reflection_and_engine_internal_imports(tp):
    """Two routes to the same oracle that never touch a frame: `gc.get_referrers(obs)` finds the
    broadcast dict from the observation itself, and importing the engine reaches it with no
    reflection at all."""
    with pytest.raises(SG.SourceGateError, match="get_referrers"):
        RM.build_checks(PipelineConfig(plugins={"check": [{"ref": "tb_gc:GcWalker"}]}))
    with pytest.raises(SG.SourceGateError, match="ENGINE INTERNALS"):
        RM.build_checks(PipelineConfig(plugins={"check": [{"ref": "tb_engine_import:Importer"}]}))


def test_the_gate_also_covers_the_fusion_slot(tp):
    """A fusion sees the same frame and the same `st`. Gating only the check slot would leave the
    identical vector open one call later."""
    walker = (tp / "tb_fusion_walker.py")
    walker.write_text('''\
import sys

from scms_sim_ref.api.detect import INTERFACE_VERSION, FusionBase


class WalkingFusion(FusionBase):
    interface_version = INTERFACE_VERSION
    plugin_id = "fwalk"

    def decide(self, scores, state, obs, params, rng):
        sys._getframe(1).f_locals
        return None
''', encoding="utf-8")
    importlib.invalidate_caches()
    try:
        with pytest.raises(SG.SourceGateError, match="_getframe"):
            RM.build_checks(PipelineConfig(
                plugins={"fusion": {"ref": "tb_fusion_walker:WalkingFusion"}}))
    finally:
        sys.modules.pop("tb_fusion_walker", None)


def test_an_honest_plugin_passes_the_gate_and_runs(tp, tmp_path):
    """The gate must not be a tax on the case it exists to protect. The clean distribution imports
    `scms_sim_ref.api`, `from scms_sim_ref.api import fields` and `from scms_sim_ref import api`, and
    all three spellings pass."""
    assert SG.scan_source(_CLEAN, "clean.py") == []
    res = run_pipeline(PipelineConfig(
        plugins={"check": ["@builtins", {"ref": "tb_clean:RangeCheck",
                                         "params": {"max_range_m": 120.0, "tolerance_m": 40.0}}]},
        out_dir=str(tmp_path / "ok"), **_CFG))
    rows = _reports(res)
    assert rows and all("detnorm_x_tpclean_claimedRange" in r for r in rows)


def test_the_opt_out_is_explicit_per_plugin_and_lands_in_the_config(tp, tmp_path):
    """`source_gate: "off"` is for code the user themselves wrote or audited. It is a CONFIG key, so
    it replays and it is serialised verbatim into `manifest["config"]` -- a dataset built with the
    gate disabled says so in its own manifest."""
    plugins = {"check": ["@builtins",
                         {"ref": "tb_walker:FrameWalker", "source_gate": "off"}]}
    suite = RM.build_checks(PipelineConfig(plugins=plugins))
    assert "x_walker_walk" in suite.columns
    res = run_pipeline(PipelineConfig(plugins=plugins, out_dir=str(tmp_path / "off"), **_CFG))
    man = json.loads((tmp_path / "off" / "manifest.json").read_text(encoding="utf-8"))
    entry, = [e for e in man["config"]["plugins"]["check"]
              if isinstance(e, dict) and e.get("ref") == "tb_walker:FrameWalker"]
    assert entry["source_gate"] == "off"
    assert res.n_reports > 0


def test_an_unknown_gate_mode_is_an_error_not_a_silent_default(tp):
    with pytest.raises(ConfigError, match="source_gate"):
        RM.build_checks(PipelineConfig(
            plugins={"check": [{"ref": "tb_clean:RangeCheck", "source_gate": "maybe"}]}))
    with pytest.raises(ConfigError, match="source_gate"):
        RM.build_checks(PipelineConfig(
            plugins={"fusion": {"ref": "tb_clean:StateFusion", "source_gate": "yes"}}))


def test_the_gate_refuses_source_it_cannot_read():
    """A module with no readable source is refused rather than waved through: "the gate saw nothing"
    and "there is nothing to see" are different statements and only one of them is a pass."""
    class NoSource(apidet.CheckBase):
        plugin_id = "nosource"
        reason_code = "nosource"
    NoSource.__module__ = "definitely_not_a_real_module"
    with pytest.raises(SG.SourceGateError, match="not in sys.modules"):
        SG.gate("check", "x:NoSource", NoSource)


def test_the_gate_is_a_guard_rail_and_the_repository_proves_it(tp):
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE.

    `getattr(sys, "_get" + "frame")` is the same frame walk with the name assembled at run time, and
    a static name-matching gate cannot see it. `api/srcgate.py`'s module docstring, its refusal
    message and DETECTOR-PLUGIN.md section 2 all say the gate is defeatable; this is the assertion
    that keeps that admission true rather than decorative.
    """
    assert SG.scan_source(_OBFUSCATED, "obfuscated.py") == []
    suite = RM.build_checks(PipelineConfig(
        plugins={"check": [{"ref": "tb_obfuscated:Obfuscated"}]}))
    assert suite.columns == ("x_obfusc_obfusc",)
    # ...and the honesty is written down where a plugin author reads it, not only here
    assert "not a sandbox" in SG.__doc__ and "out-of-process" in SG.__doc__


def test_the_conformance_suite_is_blind_to_a_frame_walk_and_the_docs_say_so():
    """PINNED SO THE DOCUMENTATION CANNOT SILENTLY BECOME FALSE.

    D2 (a recording proxy over the observation) and D3 (paired streams, one carrying ground truth)
    are the strongest run-time instruments this project has, and DETECTOR-PLUGIN.md section 4.1 now
    says plainly that neither sees a frame walk: the conformance harness's own frame carries no
    broadcast dict, so the plugin reads nothing there, scores identically on both streams, and
    passes everything. Measured here, so the sentence is an assertion rather than a claim.
    """
    from scms_sim_ref.api.rng import RngNamespace
    from scms_sim_ref.conformance.runner import run_contract
    from scms_sim_ref.conformance.v1.detect import CheckContract

    class QuietFrameWalker(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "quietwalk"
        reason_code = "quietwalk"

        def evaluate(self, obs, state, params, rng):
            f = sys._getframe(1)
            while f is not None:
                b = f.f_locals.get("b")
                if isinstance(b, dict) and "veh" in b:     # inside the ENGINE: the oracle
                    return 5.0 if getattr(b["veh"], "is_attacker", False) else 0.0
                f = f.f_back
            # inside the CONFORMANCE HARNESS: a plausible, honest-looking residual
            return max(0.0, abs(obs.claimed_x - obs.ref_x) - 200.0) / 100.0

    c = CheckContract()
    c.make = lambda **p: QuietFrameWalker(params={}, rng=RngNamespace(1, "quietwalk"), env={})
    c._params, c._ns, c._pid = {}, RngNamespace(1, "quietwalk"), "quietwalk"
    c._ns.begin_step(0)
    rep = run_contract(c, ["D1_pure", "D2_reads_only_ma_visible", "D3_label_invariance",
                           "D4_firing_convention", "D5_monotone_in_attack_magnitude",
                           "D7_state_namespacing"])
    assert rep.ok and rep.count("FAIL") == 0, rep.to_text()
    # The static gate is the instrument that does see it, which is why both exist. (Findings are
    # ordered by (line, col, construct); the two nodes of `sys._getframe(1).f_locals` share a line
    # AND a column -- an Attribute node's col_offset is the start of the whole chain -- so the
    # construct name is the tie-break and `f_locals` sorts first.)
    assert sorted(f.construct for f in SG.scan_source(
        "import sys\ndef e(o):\n    return sys._getframe(1).f_locals\n", "q.py")) == [
        "f_locals", "sys._getframe"]


def test_the_gate_reports_every_finding_at_once_with_line_numbers():
    """A gate that stops at the first hit makes a plugin author iterate; the message lists all of
    them, in line order, each with what it is and why it is refused."""
    src = ("import gc\n"
           "import sys\n"
           "def f(o):\n"
           "    sys._getframe(1)\n"
           "    gc.get_objects()\n"
           "    eval('1')\n")
    found = SG.scan_source(src, "multi.py")
    assert [(f.line, f.construct) for f in found] == [
        (4, "sys._getframe"), (5, "gc.get_objects"), (6, "eval()")]
    assert all(f.why for f in found)


def test_builtins_are_never_gated():
    """The built-ins ARE the engine. Gating them would be both meaningless and, since `run.py`
    imports `mock_pipeline` by definition, permanently failing."""
    suite = RM.build_checks(PipelineConfig())
    assert all(c.builtin for c in suite.checks)
    assert suite.fusion_wrap is None
