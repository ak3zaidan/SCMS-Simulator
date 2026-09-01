"""Phase-3 detector seam: the user's thresholding case, end to end.

THE REQUIREMENT, restated: *"integrate thresholding into the simulator"* -- plug in your own
misbehaviour detector and run simulations with it, **without forking the code**. Before this phase
that cost 14 edit sites across `run.py`, `featurize.py` and `CamDetector.java` plus a re-pin of 8
goldens. What is pinned here, in the order the design's phase-3 gate lists it:

* **All goldens hold with no plugins declared.** The extraction is code motion; the DTO, the
  registry and the fusion split move zero floats.
* **Declaring the built-ins explicitly is byte-identical to not declaring them.** The registry is a
  different SELECTION mechanism for the same components, never different behaviour.
* **A third-party threshold detector, living outside `src/`, reached by dotted path, adds
  `detnorm_x_<plugin_id>_<code>` to every report and to the ML tables with ZERO edits to `run.py`,
  `featurize.py` or any test file.** That is the whole requirement, as an assertion.
* **Installed but not declared is byte-identical (D6); declared, the digest moves.** Registering is
  not enabling.
* **The firewall is STRUCTURAL.** `Observation` is frozen, slotted, and carries no field whose name
  is in `FORBIDDEN_FEATURE_KEYS`; a detector cannot reach `veh` / `falsified` / true position by
  attribute, by `__dict__` (there is none) or by mutation. A leaky detector is byte-reproducible, so
  the pinned-golden layer is provably blind to it -- this is the layer that is not.
* **The vocabulary is derived, not transcribed.** `featurize.REASON_VOCAB` / `DETECTORS` /
  `_DETECTOR_DOCS` come from the registry, and the dead `positionSpeedConsistency` alias is gone by
  construction.
"""

import dataclasses
import importlib
import json
import random
import sys

import pytest

from scms_sim_ref.api import detect as apidet
from scms_sim_ref.api import registry as apireg
from scms_sim_ref.api.errors import CapabilityError, ConfigError
from scms_sim_ref.api.rng import RngNamespace
from scms_sim_ref.conformance.runner import run_ref
from scms_sim_ref.conformance.v1.detect import CheckContract, observation
from scms_sim_ref.datagen import featurize as FZ
from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import detectors as DET
from scms_sim_ref.mock_pipeline import run as RM
from scms_sim_ref.schemas.records import FORBIDDEN_FEATURE_KEYS, is_forbidden_feature_key

# The determinism-contract default digest, pinned identically in 6 other test files.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40, arrival_rate=1.5,
            grid_w=5, grid_h=5, attacker_pct=0.25)

#: The historical `DET_KEYS` tuple, transcribed from the pre-phase-3 `run.py` ONE last time. Its
#: ORDER is digest-bearing through the fusion's stable-sort tie-break: two checks firing at the same
#: score are ranked by their position in it, and the winner becomes `reason_codes[0]`. (The rows
#: themselves are canonicalised with sorted keys, so insertion order never reaches the bytes.)
#: Pinned here so a future registration reorder is caught by a named test rather than by eight
#: digest failures nobody can localise.
HISTORICAL_DET_KEYS = ("positionSpeedInconsistency", "positionJump", "headingInconsistency",
                       "staleOrReplay", "constantPositionFrozen", "implausibleAcceleration",
                       "sybilCoLocation", "acceptanceRangeThreshold", "beaconFrequency",
                       "signatureVerification", "certValidity", "mapOffRoad")
HISTORICAL_SOFT_KEYS = ("kalmanConsistency",)


# --------------------------------------------------------------------------- #
# A third-party detector distribution, written OUTSIDE src/, reached by dotted path.
# It is THE user's case: one scalar threshold applied to one MA-visible quantity.
# --------------------------------------------------------------------------- #
_PLUGIN_SRC = '''\
"""A third-party misbehaviour detector. Imports nothing from the engine -- only the published api.

`RssiRangeThreshold` is F2MD's ThresholdApp shape and this repo's polarity: one scalar threshold,
one MA-visible residual, `>= 1.0` == violating.
"""
import math

from scms_sim_ref.api.detect import INTERFACE_VERSION, CheckBase, FusionBase, ReportDecision
from scms_sim_ref.api.fields import FieldSpec

TOUCHED = []            # every attribute name the engine's Observation was asked for -- D2 evidence


class ClaimedRangeThreshold(CheckBase):
    """A pure thresholding scheme: how far beyond `max_range_m` the sender CLAIMS to be."""

    interface_version = INTERFACE_VERSION
    plugin_id = "myorg"
    reason_code = "claimedRange"
    precision = 3

    def __init__(self, *, params=None, rng=None, env=None):
        super().__init__(params=params, rng=rng, env=env)
        self.max_range_m = float(self.params["max_range_m"])
        self.tolerance_m = float(self.params["tolerance_m"])

    @classmethod
    def config_fields(cls):
        return {"max_range_m": FieldSpec("float", 250.0, "claimed distance treated as plausible",
                                         lo=10.0, hi=5000.0, unit="m"),
                "tolerance_m": FieldSpec("float", 100.0, "excess distance that scores 1.0",
                                         lo=1.0, hi=1000.0, unit="m")}

    def capabilities(self):
        return frozenset({"history"})

    def evaluate(self, obs, state, params, rng):
        d = math.hypot(obs.claimed_x - obs.rx_x, obs.claimed_y - obs.rx_y)
        seen = state.get("seen", 0)
        state["seen"] = seen + 1                      # its OWN namespace, and nothing else
        return max(0.0, d - self.max_range_m) / self.tolerance_m


class GreedyCheck(ClaimedRangeThreshold):
    """Declares a capability reserved for built-ins. Must be refused at load."""

    plugin_id = "greedy"
    reason_code = "greedy"

    def capabilities(self):
        return frozenset({"legacy_global_rng"})


class LeakyCheck(CheckBase):
    """Tries to read ground truth off the observation. There is nothing there to read."""

    interface_version = INTERFACE_VERSION
    plugin_id = "leaky"
    reason_code = "leak"

    def evaluate(self, obs, state, params, rng):
        return 3.0 if getattr(obs, "falsified", False) else 0.0


class AlwaysFusion(FusionBase):
    """A third-party fusion: reports on the FIRST violating score, no streak, no Bernoulli."""

    interface_version = INTERFACE_VERSION
    plugin_id = "myfusion"

    def capabilities(self):
        return frozenset()

    def decide(self, scores, state, obs, params, rng):
        fired = [k for k in self.keys if scores[k] >= 1.0]
        if not fired:
            return None
        fired.sort(key=lambda k: -scores[k])
        return ReportDecision(True, fired, scores[fired[0]], max(scores.values()))
'''


@pytest.fixture(scope="module")
def det_plugin(tmp_path_factory):
    """Put the third-party module on sys.path -- INSTALLED, but not DECLARED anywhere."""
    root = tmp_path_factory.mktemp("thirdparty_det")
    src = root / "myorg_det.py"
    src.write_text(_PLUGIN_SRC, encoding="utf-8")
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()
    try:
        yield src
    finally:
        sys.path.remove(str(root))
        sys.modules.pop("myorg_det", None)


def _reports(res):
    with open(f"{res.out_dir}/ma/ma_reports.jsonl", encoding="utf-8") as fh:
        return [json.loads(ln) for ln in fh if ln.strip()]


# ======================================================================= the registry ========== #
def test_the_check_and_fusion_slots_are_no_longer_empty():
    """The blocker, stated as a test: `builtin_names("check")` used to return an EMPTY tuple, so the
    registry reserved a slot nothing could be selected from."""
    checks = apireg.builtin_names("check")
    assert checks and apireg.builtin_names("fusion") == ("streak_v1",)
    assert checks == HISTORICAL_DET_KEYS + ("vruImpersonation", "denmPlausibility",
                                            "kalmanConsistency")
    assert "check" in apireg.INTERFACE and "fusion" in apireg.INTERFACE


def test_det_keys_order_is_the_historical_one():
    """Registration order IS evaluation order IS `DET_KEYS` order, and that order reaches the
    digest through the fusion's stable-sort tie-break."""
    assert RM.default_check_refs() == HISTORICAL_DET_KEYS + HISTORICAL_SOFT_KEYS
    assert (RM.default_check_refs(station_types=True, denm=True)
            == HISTORICAL_DET_KEYS + ("vruImpersonation", "denmPlausibility")
            + HISTORICAL_SOFT_KEYS)
    suite = RM.build_checks(PipelineConfig())
    assert suite.keys == HISTORICAL_DET_KEYS
    assert suite.soft_keys == HISTORICAL_SOFT_KEYS
    # the VRU suppression list is DECLARED per check, not a fourth hand-maintained tuple
    assert suite.vru_suppressed == ("positionSpeedInconsistency", "positionJump",
                                    "headingInconsistency", "constantPositionFrozen",
                                    "implausibleAcceleration", "mapOffRoad")


def test_gated_checks_are_registered_but_not_enabled():
    """The in-tree precedent this seam copies: a registered check that is not in the run's suite
    contributes no column, which is what keeps the default digest byte-identical."""
    assert "vruImpersonation" in apireg.builtin_names("check")
    assert "vruImpersonation" not in RM.build_checks(PipelineConfig()).columns
    assert "vruImpersonation" in RM.build_checks(PipelineConfig(), station_types=True).columns
    assert "denmPlausibility" in RM.build_checks(PipelineConfig(), denm=True).columns


# ======================================================================= the firewall ========== #
def test_observation_is_structurally_incapable_of_carrying_ground_truth():
    """THE most important property in this file. Not a lint, not a review rule -- a class shape."""
    fields = {f.name for f in dataclasses.fields(apidet.Observation)}
    assert not (fields & FORBIDDEN_FEATURE_KEYS)
    assert not [f for f in fields if is_forbidden_feature_key(f)]
    # the exact things the engine's broadcast dict carries and this DTO must not
    assert not (fields & {"veh", "x", "y", "falsified", "ghost", "tspd", "thdg"})
    o = observation()
    assert not hasattr(o, "__dict__")            # slots: nothing can be attached at runtime
    with pytest.raises((dataclasses.FrozenInstanceError, AttributeError)):
        o.claimed_x = 1.0
    # An UNDECLARED name is refused too, though CPython's slots+frozen `__setattr__` reaches its
    # zero-arg `super()` before the AttributeError and reports the miss as a TypeError. Both are
    # refusals; the property being pinned is that nothing can be attached, not the exception type.
    with pytest.raises((dataclasses.FrozenInstanceError, AttributeError, TypeError)):
        o.smuggled = 1.0
    # `rssi_dbm` IS present and IS legitimate: a real PHY measures received power for every frame it
    # decodes. It is the reviewed exception documented in datagen/leakage_linter.py.
    assert "rssi_dbm" in fields and not is_forbidden_feature_key("rssi_dbm")


def test_namespaced_state_refuses_the_reserved_keys():
    """A third party writes inside `state['plugin:<id>']` and nowhere else -- and reading a reserved
    key hands back something it cannot write THROUGH either."""
    st = {"h": [(0.0, 0.0, 1.0, 0.0, 0.0)], "streak": {"positionJump": 2}, "touch": 3}
    ns = apidet.NamespacedState(st, "myorg")
    ns["seen"] = 1
    assert st["plugin:myorg"] == {"seen": 1} and dict(ns) == {"seen": 1}
    assert isinstance(ns["h"], tuple)                       # a copy, not the engine's list
    with pytest.raises(TypeError):
        ns["streak"]["positionJump"] = 99                   # a read-only proxy
    for reserved in ("h", "streak", "touch", "kf"):
        with pytest.raises(ConfigError):
            ns[reserved] = "mine"
    assert st["streak"] == {"positionJump": 2} and st["touch"] == 3


def test_a_leaky_detector_finds_nothing_to_read(det_plugin):
    """The anti-laundering property, as the engine actually runs it: a check that asks the
    observation for `falsified` gets nothing, so its score is constant and it can never separate
    attackers from benign vehicles."""
    res = run_pipeline(PipelineConfig(
        plugins={"check": ["@builtins", {"ref": "myorg_det:LeakyCheck"}]},
        out_dir=str(det_plugin.parent / "leak"), **_CFG))
    col = "detnorm_x_leaky_leak"
    scores = {r[col] for r in _reports(res)}
    assert scores == {0.0}


# ======================================================================= the digests ============ #
def test_default_golden_holds_with_no_plugins_declared(tmp_path):
    """V1: the extraction is CODE MOTION. Nine inline detectors and the streak/report_prob block
    became registry components, and not one float moved."""
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


def test_declaring_the_builtins_explicitly_is_byte_identical(tmp_path):
    """THE phase-3 acceptance gate. The registry is a different SELECTION mechanism for the same
    components -- never different behaviour. Three spellings, one digest."""
    plain = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "plain"), **_DEFAULT))
    token = run_pipeline(PipelineConfig(plugins={"check": ["@builtins"],
                                                 "fusion": {"ref": "streak_v1"}},
                                        out_dir=str(tmp_path / "tok"), **_DEFAULT))
    listed = run_pipeline(PipelineConfig(
        plugins={"check": [{"ref": r} for r in RM.default_check_refs()], "fusion": "streak_v1"},
        out_dir=str(tmp_path / "list"), **_DEFAULT))
    assert plain.data_digest == DEFAULT_GOLDEN
    assert token.data_digest == DEFAULT_GOLDEN
    assert listed.data_digest == DEFAULT_GOLDEN


def test_installed_but_not_declared_is_byte_identical(tmp_path, det_plugin):
    """D6: REGISTERING IS NOT ENABLING. The third-party module is importable for the whole module's
    tests; a run that does not declare it reproduces the golden exactly."""
    import myorg_det                                        # noqa: F401 - importing is the point
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "nd"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


# ======================================================================= NO FORK =============== #
def test_third_party_threshold_detector_runs_with_zero_edits_to_the_engine(tmp_path, det_plugin):
    """**THE REQUIREMENT.** A thresholding scheme in its own file, outside `src/`, selected by a
    dotted path in config, contributing a namespaced column to every report and its own knobs to the
    schema -- with no edit to `run.py`, `featurize.py` or any test file."""
    plugins = {"check": ["@builtins",
                         {"ref": "myorg_det:ClaimedRangeThreshold",
                          "params": {"max_range_m": 120.0, "tolerance_m": 40.0}}]}
    res = run_pipeline(PipelineConfig(plugins=plugins, out_dir=str(tmp_path / "tp"), **_CFG))
    base = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "base"), **_CFG))
    rows = _reports(res)
    col = "detnorm_x_myorg_claimedRange"

    # 1. the column exists, is namespaced, and cannot collide with a built-in's
    assert rows and all(col in r for r in rows)
    assert any(r[col] > 0 for r in rows)
    assert {k for r in rows for k in r if k.startswith("detnorm_x_")} == {col}
    # 2. it is APPENDED after the built-ins, whose columns and ORDER are untouched. (The JSONL rows
    # are canonicalised with sorted keys, so the vector's order is asserted where it actually bites:
    # the suite the engine built, which is what the fusion's stable-sort tie-break reads.)
    suite = RM.build_checks(PipelineConfig(plugins=plugins))
    assert suite.columns == (HISTORICAL_DET_KEYS + (col[len("detnorm_"):],) + HISTORICAL_SOFT_KEYS)
    assert {k[len("detnorm_"):] for r in rows for k in r if k.startswith("detnorm_")} \
        == set(suite.columns)
    # 3. it CHANGES the dataset (it is a real detector, not a passenger)
    assert res.data_digest != base.data_digest
    assert any(r["reason_codes"] and r["reason_codes"][0] == "x_myorg_claimedRange" for r in rows)
    # 4. the manifest LOCKS it: content hash, declared params, its position in the vector
    man = json.loads((tmp_path / "tp" / "manifest.json").read_text(encoding="utf-8"))
    entry, = [e for e in man["plugins"]["loaded"]
              if e["ref"] == "myorg_det:ClaimedRangeThreshold"]
    assert entry["slot"] == "check" and entry["resolved_via"] == "dotted_path"
    assert entry["order"] == len(HISTORICAL_DET_KEYS) + 1          # after the 12, before the soft
    assert len(entry["module_sha256"]) == 64 and entry["params"]["max_range_m"] == 120.0
    assert entry["interface_version"] == "Detector/1.0"
    # 5. and it replays: the config round-trips through the manifest unchanged
    assert RM.config_from_dict(man).plugins["check"][1]["params"]["tolerance_m"] == 40.0


def test_third_party_column_reaches_the_ml_tables_with_zero_edits(tmp_path, det_plugin):
    """`featurize` derives its vocabulary from the registry, and carries a column the registry has
    never heard of straight through -- which is what makes the detector usable, not merely runnable."""
    plugins = {"check": ["@builtins",
                         {"ref": "myorg_det:ClaimedRangeThreshold",
                          "params": {"max_range_m": 120.0, "tolerance_m": 40.0}}]}
    out = str(tmp_path / "ml")
    run_pipeline(PipelineConfig(plugins=plugins, out_dir=out, **_CFG))
    FZ.build(out)
    import pandas as pd
    rf = pd.read_parquet(f"{out}/ml/report_features.parquet")
    vf = pd.read_parquet(f"{out}/ml/vehicle_features.parquet")
    assert "detnorm_x_myorg_claimedRange" in rf.columns
    assert "detmax_x_myorg_claimedRange" in vf.columns   # the per-vehicle fusion fingerprint
    assert "reason_x_myorg_claimedRange" in rf.columns
    schema = json.loads(open(f"{out}/ml/schema.json", encoding="utf-8").read())
    entry, = [c for c in schema["report_features"]
              if c["name"] == "detnorm_x_myorg_claimedRange"]
    assert entry["kind"] == "fusion_feature"
    assert "myorg" in entry["desc"] and "claimedRange" in entry["desc"]


def test_a_third_party_fusion_replaces_the_report_rule(tmp_path, det_plugin):
    """Layer 2 is pluggable too: F2MD's `MDApplication`, with `ThresholdApp` swapped out. The
    built-in's streak gate and `report_prob` Bernoulli are gone, so strictly more reports are filed
    -- and the plugin never touches the global `rng` to do it."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "streak"), **_CFG))
    b = run_pipeline(PipelineConfig(plugins={"fusion": {"ref": "myorg_det:AlwaysFusion"}},
                                    out_dir=str(tmp_path / "always"), **_CFG))
    assert b.n_reports > a.n_reports
    assert b.data_digest != a.data_digest
    man = json.loads((tmp_path / "always" / "manifest.json").read_text(encoding="utf-8"))
    entry, = [e for e in man["plugins"]["loaded"] if e["slot"] == "fusion"]
    assert entry["ref"] == "myorg_det:AlwaysFusion"
    assert "legacy_global_rng" not in entry["capabilities"]


# ======================================================================= refusals =============== #
def test_reserved_capabilities_are_refused_from_a_third_party(tmp_path, det_plugin):
    """`legacy_global_rng` and `legacy_raw_compare` exist only so the built-ins keep their pinned
    digests. A third party that declares one is refused BEFORE step 0."""
    with pytest.raises(CapabilityError, match="reserved"):
        RM.build_checks(PipelineConfig(plugins={"check": [{"ref": "myorg_det:GreedyCheck"}]}))
    assert apidet.RESERVED_CAPABILITIES == {"legacy_global_rng", "legacy_raw_compare"}


def test_a_mistyped_or_duplicated_declaration_is_an_error_not_a_no_op():
    """An ignored plugin section replays as a different run with exit code 0 -- the exact failure
    the lock exists to prevent."""
    with pytest.raises(ValueError, match="unknown key"):
        RM.validate_config(PipelineConfig(plugins={"check": [{"ref": "positionJump", "prams": {}}]}))
    with pytest.raises(ValueError, match="more than once"):
        RM.validate_config(PipelineConfig(plugins={"check": ["positionJump", "positionJump"]}))
    with pytest.raises(ValueError, match="unknown check"):
        RM.validate_config(PipelineConfig(plugins={"check": ["noSuchCheck"]}))
    with pytest.raises(ValueError, match="BUILT-IN"):
        # a built-in's knobs ARE engine config fields; two spellings is how they drift apart
        RM.validate_config(PipelineConfig(
            plugins={"check": [{"ref": "positionJump", "params": {"z": 9.0}}]}))


def test_a_check_can_be_required_to_conform_before_step_0(tmp_path, det_plugin):
    """`plugins.check[].conformance = "required"` -- the design's third delivery route, as a CONFIG
    declaration (which replays) rather than a flag (which does not). A failing check is refused
    before step 0, and a passing one embeds its report in the manifest."""
    ok = {"check": ["@builtins", {"ref": "myorg_det:ClaimedRangeThreshold",
                                  "conformance": "required"}]}
    res = run_pipeline(PipelineConfig(plugins=ok, out_dir=str(tmp_path / "att"), **_CFG))
    man = json.loads((tmp_path / "att" / "manifest.json").read_text(encoding="utf-8"))
    entry, = [e for e in man["plugins"]["loaded"] if e["slot"] == "check" and "conformance" in e]
    assert entry["conformance"]["ok"] is True and entry["conformance"]["failed"] == 0
    # D6 needs a digest from before the plugin existed, so the in-run attestation excludes it and
    # SAYS SO -- the same treatment C12 gets on the channel slot, for the same reason.
    assert entry["conformance"]["excluded"] == ["D6_off_by_default_is_byte_identical"]
    assert res.n_reports > 0
    bad = {"check": ["@builtins", {"ref": "myorg_det:LeakyCheck", "conformance": "required"}]}
    with pytest.raises(ConfigError, match="does not conform"):
        run_pipeline(PipelineConfig(plugins=bad, out_dir=str(tmp_path / "no"), **_CFG))
    assert not (tmp_path / "no").exists()          # refused BEFORE any output directory is created


def test_third_party_knobs_merge_into_the_schema_and_validate(det_plugin):
    """One declaration in the plugin; the GUI panel, the copilot cheat-sheet and
    `--dump-config-schema` get the knob for free -- and a BUILT-IN contributes nothing here, because
    its knobs are already top-level config fields."""
    cfg = PipelineConfig(plugins={"check": [{"ref": "myorg_det:ClaimedRangeThreshold",
                                             "params": {"max_range_m": 320.0}}]})
    plain, merged = config_schema(), config_schema(cfg)
    assert set(merged) - set(plain) == {"plugins.check.max_range_m", "plugins.check.tolerance_m"}
    knob = merged["plugins.check.max_range_m"]
    assert (knob["default"], knob["unit"], knob["max"], knob["widget"]) == (250.0, "m", 5000.0,
                                                                           "float")
    assert set(config_schema(PipelineConfig(plugins={"check": ["@builtins"]}))) == set(plain)
    with pytest.raises(ValueError, match="above maximum"):
        RM.validate_config(PipelineConfig(
            plugins={"check": [{"ref": "myorg_det:ClaimedRangeThreshold",
                                "params": {"max_range_m": 9e9}}]}))
    with pytest.raises(ValueError, match="declares no field"):
        RM.validate_config(PipelineConfig(
            plugins={"check": [{"ref": "myorg_det:ClaimedRangeThreshold", "params": {"nope": 1}}]}))


def test_a_plugin_never_receives_the_global_rng(tmp_path, det_plugin):
    """D3, capability by omission. The engine's global stream is not reachable from a plugin's
    arguments; the built-in fusion's access to it is a DECLARED, reserved grandfathering."""
    suite = RM.build_checks(PipelineConfig(
        plugins={"check": ["@builtins", {"ref": "myorg_det:ClaimedRangeThreshold"}]}))
    third, = [c for c in suite.checks if not c.builtin]
    assert isinstance(third.rng, RngNamespace) and third.rng.plugin_id == "myorg"
    assert not isinstance(third.instance.env.get("legacy_rng"), random.Random)
    assert apidet.CAP_LEGACY_GLOBAL_RNG in suite.fusion.capabilities()   # the built-in's, declared


# ======================================================================= derived vocabulary ===== #
def test_featurize_vocabulary_is_derived_from_the_registry():
    """The four-way drift is gone: three literal lists in `featurize.py` are now one registry."""
    assert FZ.DETECTORS == list(apireg.builtin_names("check"))
    assert FZ.REASON_VOCAB == [n for n in apireg.builtin_names("check")
                               if not apireg.builtin("check", n).soft]
    # a SOFT check is a fusion feature but never a reason, so it is in one list and not the other
    assert "kalmanConsistency" in FZ.DETECTORS and "kalmanConsistency" not in FZ.REASON_VOCAB
    # the dead mock-pipeline alias is gone BY CONSTRUCTION -- no check declares it
    assert "positionSpeedConsistency" not in FZ.REASON_VOCAB
    # ...and the docs come from each check's own docstring rather than a fourth copy
    docs = FZ._DETECTOR_DOCS
    assert set(docs) == set(apireg.builtin_names("check"))
    assert all(v for v in docs.values())
    assert "signature" in docs["signatureVerification"].lower()


# ======================================================================= conformance ============ #
@pytest.mark.parametrize("ref", list(apireg.builtin_names("check")))
def test_every_builtin_check_conforms(ref):
    """The shipped checks are graded by their own suite rather than being exempt from it -- the
    property that made the channel suite's two defect findings possible."""
    rep = run_ref("check", ref)
    assert rep.ok, rep.to_text()
    assert rep.count("PASS") >= 5


def test_the_suite_catches_a_polarity_inversion_and_a_leak(det_plugin):
    """A suite is only worth what its violators prove. One deliberately F2MD-polarised check (LOW
    means implausible) and one oracle reader, each failing its OWN check and passing the rest."""

    class F2mdPolarity(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "inverted"
        reason_code = "inverted"

        def evaluate(self, obs, state, params, rng):
            return 1.0                     # "plausible" in F2MD's convention; VIOLATING in ours

    class OracleReader(apidet.CheckBase):
        interface_version = apidet.INTERFACE_VERSION
        plugin_id = "oracle"
        reason_code = "oracle"

        def evaluate(self, obs, state, params, rng):
            return 3.0 if getattr(obs, "is_attacker", False) else 0.0

    class Contract(CheckContract):
        pass

    for cls, expect in ((F2mdPolarity, "D4_firing_convention"),
                        (OracleReader, "D3_label_invariance")):
        c = Contract()
        c.make = lambda cls=cls, **p: cls(params={}, rng=RngNamespace(1, cls.plugin_id), env={})
        c._params, c._ns, c._pid = {}, RngNamespace(1, cls.plugin_id), cls.plugin_id
        c._ns.begin_step(0)
        from scms_sim_ref.conformance.runner import run_contract
        rep = run_contract(c, [r for r in ("D1_pure", "D3_label_invariance", "D4_firing_convention",
                                           "D5_monotone_in_attack_magnitude")])
        failed = [r["check"] for r in rep.rows if r["status"] == "FAIL"]
        assert failed == [expect], rep.to_text()
