"""Phase-1 plugin seam: `scms_sim_ref.api` + the channel registry (docs/realism/PLUGIN-ARCHITECTURE.md).

What is being pinned here, in the order the design's gates list it:

* **V1 / D6 -- registering is not enabling.** The default (disc) golden must not move, and a
  third-party channel model that is INSTALLED but not DECLARED must not move it either.
* **The phase-1 acceptance gate.** Declaring `plugins.channel_model = {"ref": "geometric"}` must
  produce a digest IDENTICAL to `--radio-model geometric`. Same physics, different selection
  mechanism, byte-for-byte the same dataset.
* **V4 -- schema equality.** `config_schema()` grows exactly one key (`plugins`) and nothing else
  changes, because the GUI, the copilot cheat-sheet and `--dump-config-schema` all read it.
* **The two ABI shapes agree.** A `BatchChannelModel` (the canonical per-step ABI, the only shape an
  out-of-process ns-3/OMNeT++ backend can implement) and a `LinkChannelModel` with the same physics
  must produce the same dataset -- otherwise the phase-4 switch to a batch-consuming loop would
  silently re-derive every digest.
* **D3 -- capability by omission.** A plugin never receives the engine's global `rng`.
* **D4 -- the content-hash lock.** Replaying a manifest whose plugin source has changed fails
  LOUDLY, before step 0, instead of quietly producing a different run with exit code 0.
* **No fork.** The third-party models below live in a file written at test time, outside `src/`,
  selected by dotted path, with zero edits to `run.py`.
"""

import dataclasses
import importlib
import json
import random
import sys

import pytest

from scms_sim_ref import api
from scms_sim_ref.api import channel as apichan
from scms_sim_ref.api import registry as apireg
from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline
from scms_sim_ref.mock_pipeline import run as RM

# The determinism-contract default digest, pinned identically in 6 other test files.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
# A denser, shorter config for the third-party-model tests: enough links to be a real exercise,
# fast enough to run several times.
_PLUGIN_CFG = dict(seed=17, traffic_flow=True, road_network="grid", duration_s=40,
                   arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25)


# --------------------------------------------------------------------------- #
# A third-party plugin distribution, written OUTSIDE src/ and reached by dotted path.
# Both classes implement the same hard-range physics through the two different ABI shapes.
# --------------------------------------------------------------------------- #
_PLUGIN_SRC = '''\
"""A third-party channel model. Imports nothing from the engine -- only the published api package."""
from scms_sim_ref.api.channel import (DELIVERED, INTERFACE_VERSION, LinkOutcome,
                                      LOSS_INDEPENDENT_SURVIVAL)
from scms_sim_ref.api.fields import FieldSpec

EXCHANGES = []          # one entry per deliver() call: (step, candidates offered) -- D1 evidence


class HardRangeLink:
    """LinkChannelModel shape: one call per (tx, rx) pair."""

    interface_version = INTERFACE_VERSION
    plugin_id = "hardrange"

    def __init__(self, *, params, rng, env):
        self.reach_m = float(params.get("range_m", env["radio_range_m"]))
        self._rng = rng

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


class HardRangeBatch:
    """BatchChannelModel shape: ONE exchange per step. Identical physics to HardRangeLink."""

    interface_version = INTERFACE_VERSION
    plugin_id = "hardrangeb"

    def __init__(self, *, params, rng, env):
        self.reach_m = float(params.get("range_m", env["radio_range_m"]))
        self._rng = rng

    def capabilities(self):
        return frozenset({"reach", "batch", LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame):
        self._stations = frame.stations

    def reach_m_for(self, rx):
        return rx.rx_range_m or self.reach_m

    def deliver(self, frame, candidates):
        EXCHANGES.append((frame.step, len(candidates)))
        out = []
        for tx_index, rx_vid, d_m in candidates:
            rx = frame.stations[rx_vid]
            if d_m <= (rx.rx_range_m or self.reach_m):
                out.append(LinkOutcome(tx_index, rx_vid))
        return out

    def channel_busy_ratio(self, rx_vid, offered):
        return 0.0

    def delivery_coin(self, tx_vid, rx_vid):
        return self._rng.persistent("deliver", tx_vid, rx_vid).random()


class Declarative(HardRangeLink):
    """Declares its own knobs once -- ns-3's AddAttribute idea on this repo's own schema machinery."""

    plugin_id = "declarative"

    @classmethod
    def config_fields(cls):
        return {"range_m": FieldSpec("float", 300.0, "hard delivery range", lo=1.0, hi=2000.0,
                                     unit="m")}

    @classmethod
    def validate_params(cls, params):
        if params.get("range_m") == 1234.0:
            raise ValueError("range_m 1234 is the plugin's own rejected value")


SEEN_STEPS = []         # (frame.step, rng.step) once per begin_step -- the CRITICAL-1 instrument
FADE_KEYS = set()       # every STATELESS stream key drawn, so the step term can be read back out


class StepRecorder(HardRangeLink):
    """Records what the ENGINE actually did to this model's `RngNamespace`.

    `RngNamespace.begin_step` is called by the ADAPTER, never by the model, and a plugin has no
    other way to notice whether it happened -- which is exactly why it went uncalled for a whole
    phase. When it is not called, `_step` stays at -1 for the entire run, every `stream()` key ends
    `:s-1`, and a draw documented as a pure function of `(seed, plugin, label, ids, STEP)` is a
    fixed per-link constant instead: an advertised per-packet fade that never fades.
    """

    plugin_id = "steprec"

    def begin_step(self, frame):
        super().begin_step(frame)
        SEEN_STEPS.append((frame.step, self._rng.step))

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        FADE_KEYS.add(self._rng.key("fade", tx.vid, rx.vid, with_step=True))
        return DELIVERED if self._rng.stream("fade", tx.vid, rx.vid).random() > 0.02 else None


class WrongSignature(HardRangeLink):
    plugin_id = "wrongsig"

    def evaluate(self, tx, rx, distance, txn):      # `distance` should be `d_m`
        return None


class FutureVersion(HardRangeLink):
    plugin_id = "futurever"
    interface_version = "ChannelModel/2.0"


class Grandfatherer(HardRangeLink):
    """Claims a capability reserved for built-ins."""

    plugin_id = "grandfath"

    def capabilities(self):
        return frozenset({"reach", "legacy_global_rng", "loss_composition:additive_legacy"})
'''


@pytest.fixture(scope="module")
def plugin_pkg(tmp_path_factory):
    """Install the third-party module on sys.path -- INSTALLED, but not DECLARED anywhere."""
    root = tmp_path_factory.mktemp("thirdparty")
    src = root / "myorg_radio.py"
    src.write_text(_PLUGIN_SRC, encoding="utf-8")
    sys.path.insert(0, str(root))
    importlib.invalidate_caches()
    try:
        yield src
    finally:
        sys.path.remove(str(root))
        sys.modules.pop("myorg_radio", None)


def _digest(tmp_path, name, **kw):
    base = dict(_DEFAULT)
    base.update(kw)
    return run_pipeline(PipelineConfig(out_dir=str(tmp_path / name), **base)).data_digest


# --------------------------------------------------------------------------- V1 / D6 ------------ #
def test_default_golden_unchanged(tmp_path):
    """The channel seam is a refactor. The determinism contract does not notice it."""
    assert _digest(tmp_path, "g") == DEFAULT_GOLDEN


def test_registering_is_not_enabling(tmp_path, plugin_pkg):
    """D6: the third-party model is importable RIGHT NOW and the golden still holds. Only a config
    declaration activates anything -- which is why activation may never be discovery-driven."""
    importlib.import_module("myorg_radio")
    assert _digest(tmp_path, "d6") == DEFAULT_GOLDEN


# --------------------------------------------------------------------------- the phase-1 gate --- #
def test_plugin_ref_geometric_is_byte_identical_to_radio_model_geometric(tmp_path):
    """THE acceptance gate. Same built-in reached through the registry instead of the enum."""
    base = dict(seed=13, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
    enum = run_pipeline(PipelineConfig(radio_model="geometric",
                                       out_dir=str(tmp_path / "enum"), **base))
    plug = run_pipeline(PipelineConfig(plugins={"channel_model": {"ref": "geometric", "params": {}}},
                                       out_dir=str(tmp_path / "plug"), **base))
    assert plug.data_digest == enum.data_digest
    # ...and the rssi column gate follows the DECLARED CAPABILITY, not the model name
    rows = [json.loads(ln) for ln in
            open(f"{plug.out_dir}/ma/ma_reports.jsonl", encoding="utf-8") if ln.strip()]
    assert rows and all("rssi_dbm" in r for r in rows)


@pytest.mark.parametrize("ref", ["disc", "logdistance", "geometric"])
def test_every_builtin_is_reachable_through_both_selectors(tmp_path, ref):
    """V5: each built-in now reached through the registry produces the same run either way."""
    base = dict(seed=19, traffic_flow=True, road_network="grid", duration_s=30, arrival_rate=1.2,
                grid_w=4, grid_h=4, attacker_pct=0.25)
    a = run_pipeline(PipelineConfig(radio_model=ref, out_dir=str(tmp_path / f"e_{ref}"), **base))
    b = run_pipeline(PipelineConfig(plugins={"channel_model": {"ref": ref}},
                                    out_dir=str(tmp_path / f"p_{ref}"), **base))
    assert a.data_digest == b.data_digest


# --------------------------------------------------------------------------- V4 schema ---------- #
def test_config_schema_grows_exactly_one_key():
    sch = config_schema()
    names = {f.name for f in dataclasses.fields(PipelineConfig)}
    assert set(sch) == names
    assert "plugins" in sch
    assert sch["plugins"]["default"] == {}          # default_factory rendered, not a bare null
    assert sch["plugins"]["group"] == "Plugins"
    json.dumps(sch)                                  # still JSON-safe for /api/schema


def test_declared_plugin_knobs_merge_into_the_schema_and_validate(tmp_path, plugin_pkg):
    """One declaration in the plugin; the GUI panel, the copilot cheat-sheet and
    --dump-config-schema all get the knob for free. Called with no argument the schema is untouched,
    which is what keeps the phase-1 gate and the GUI contract intact."""
    ref = "myorg_radio:Declarative"
    cfg = PipelineConfig(plugins={"channel_model": {"ref": ref, "params": {"range_m": 320.0}}})
    plain, merged = config_schema(), config_schema(cfg)
    assert set(merged) - set(plain) == {"plugins.channel_model.range_m"}
    knob = merged["plugins.channel_model.range_m"]
    assert (knob["default"], knob["unit"], knob["max"], knob["widget"]) == (300.0, "m", 2000.0, "float")
    # the FieldSpec's range is enforced at CONFIG time, with no `if` added to validate_config
    with pytest.raises(ValueError, match="above maximum"):
        RM.validate_config(PipelineConfig(
            plugins={"channel_model": {"ref": ref, "params": {"range_m": 9e9}}}))
    with pytest.raises(ValueError, match="declares no field"):
        RM.validate_config(PipelineConfig(
            plugins={"channel_model": {"ref": ref, "params": {"nope": 1}}}))
    # ...and the plugin's OWN validator speaks with its OWN message
    with pytest.raises(ValueError, match="plugin's own rejected value"):
        RM.validate_config(PipelineConfig(
            plugins={"channel_model": {"ref": ref, "params": {"range_m": 1234.0}}}))
    assert run_pipeline(PipelineConfig(out_dir=str(tmp_path / "decl"),
                                       plugins=cfg.plugins, **_PLUGIN_CFG)).n_reports >= 0


def test_plugins_accepts_a_json_string(tmp_path):
    """A CLI flag, a GUI text field and a copilot tool call all deliver a string -- the same
    precedent `custom_network` and `events` already set."""
    cfg = RM.validate_config(PipelineConfig(plugins='{"channel_model": {"ref": "geometric"}}'))
    assert cfg.plugins == {"channel_model": {"ref": "geometric"}}
    assert RM.validate_config(PipelineConfig(plugins="")).plugins == {}
    with pytest.raises(ValueError, match="not valid JSON"):
        RM.validate_config(PipelineConfig(plugins="{not json"))


def test_radio_model_options_come_from_the_builtin_registry():
    """One source of truth: the closed enum that used to be hand-repeated in four places."""
    assert RM._ENUM_OPTIONS["radio_model"] == list(apireg.builtin_names("channel_model"))
    assert config_schema()["radio_model"]["options"] == ["disc", "logdistance", "geometric"]
    assert apireg.builtin_names_sorted("channel_model") == ("disc", "geometric", "logdistance")


# --------------------------------------------------------------------------- no fork ------------ #
def test_third_party_channel_model_runs_with_zero_edits_to_the_engine(tmp_path, plugin_pkg):
    # packet_loss_base > 0 so the engine's composed survival probability is < 1 and the model's OWN
    # delivery coin is actually consulted -- which is what makes `declared_streams` non-empty below.
    res = run_pipeline(PipelineConfig(
        plugins={"channel_model": {"ref": "myorg_radio:HardRangeLink", "params": {}}},
        packet_loss_base=0.05, out_dir=str(tmp_path / "tp"), **_PLUGIN_CFG))
    assert res.n_reports > 0
    man = json.loads((tmp_path / "tp" / "manifest.json").read_text(encoding="utf-8"))
    # The lock records EVERY loaded component -- since phase 3 that is the channel model plus the
    # run's whole detection layer (its checks, in DET_KEYS order, and its fusion).
    entry, = [e for e in man["plugins"]["loaded"] if e["slot"] == "channel_model"]
    assert entry["ref"] == "myorg_radio:HardRangeLink"
    assert entry["resolved_via"] == "dotted_path"
    assert len(entry["module_sha256"]) == 64
    assert man["plugins"]["provenance_digest"]
    # D3's third property: the plugin DECLARES the stream labels it consumed, and the manifest
    # records them. Captured at the END of the run -- at construction it has consumed none, and an
    # always-empty list would be fabrication by omission.
    assert entry["declared_streams"] == ["deliver"]


def test_the_two_abi_shapes_produce_the_same_dataset(tmp_path, plugin_pkg):
    """D1's correctness precondition: a per-step BatchChannelModel and a per-link LinkChannelModel
    with identical physics must produce identical output, or batching is not a free choice."""
    link = run_pipeline(PipelineConfig(
        plugins={"channel_model": {"ref": "myorg_radio:HardRangeLink"}},
        out_dir=str(tmp_path / "lnk"), **_PLUGIN_CFG))
    batch = run_pipeline(PipelineConfig(
        plugins={"channel_model": {"ref": "myorg_radio:HardRangeBatch"}},
        out_dir=str(tmp_path / "bat"), **_PLUGIN_CFG))
    assert link.data_digest == batch.data_digest


def test_batch_model_is_called_exactly_once_per_step(tmp_path, plugin_pkg):
    """D1, measured. Per-link IPC is ~1e4x too slow (~12 h vs ~7 s on a 3 600-step run at a 2 ms
    round trip); per-RECEIVER batching would still be ~200 round trips per step. The engine hands
    the whole fleet's candidate set over in ONE call, so an out-of-process backend is affordable."""
    mod = importlib.import_module("myorg_radio")
    mod.EXCHANGES.clear()
    run_pipeline(PipelineConfig(plugins={"channel_model": {"ref": "myorg_radio:HardRangeBatch"}},
                                out_dir=str(tmp_path / "x"), **_PLUGIN_CFG))
    steps = [s for s, _n in mod.EXCHANGES]
    assert steps == sorted(steps) and len(steps) == len(set(steps))     # one call, in step order
    assert len(steps) == int(_PLUGIN_CFG["duration_s"] / PipelineConfig().dt)
    assert max(n for _s, n in mod.EXCHANGES) > 50   # whole-fleet candidate sets, not per-receiver


def test_third_party_model_is_deterministic_across_two_runs(tmp_path, plugin_pkg):
    a = run_pipeline(PipelineConfig(plugins={"channel_model": {"ref": "myorg_radio:HardRangeLink"}},
                                    out_dir=str(tmp_path / "a"), **_PLUGIN_CFG))
    b = run_pipeline(PipelineConfig(plugins={"channel_model": {"ref": "myorg_radio:HardRangeLink"}},
                                    out_dir=str(tmp_path / "b"), **_PLUGIN_CFG))
    assert a.data_digest == b.data_digest


# --------------------------------------------------------------------------- load-time refusals - #
def test_signature_validator_names_the_offending_parameter(plugin_pkg):
    """Neither Protocol nor ABC checks signatures at runtime. This is the component that does."""
    with pytest.raises(api.SignatureError) as e:
        apireg.resolve("channel_model", "myorg_radio:WrongSignature")
    assert "'distance'" in str(e.value) and "'d_m'" in str(e.value)


def test_future_interface_major_is_refused(plugin_pkg):
    with pytest.raises(api.InterfaceVersionError):
        apireg.resolve("channel_model", "myorg_radio:FutureVersion")


def test_reserved_capabilities_are_refused_from_third_parties(tmp_path, plugin_pkg):
    """`legacy_global_rng` / `additive_legacy` exist only so disc and logdistance keep 0bd93655...;
    they are grandfathering, not an API."""
    with pytest.raises(api.CapabilityError):
        run_pipeline(PipelineConfig(
            plugins={"channel_model": {"ref": "myorg_radio:Grandfatherer"}},
            out_dir=str(tmp_path / "gf"), **_PLUGIN_CFG))


def test_unknown_ref_and_unknown_slot_fail_at_config_time(tmp_path):
    with pytest.raises(ValueError, match="unknown channel_model"):
        RM.validate_config(PipelineConfig(plugins={"channel_model": {"ref": "nope"}}))
    with pytest.raises(ValueError, match="unknown slot"):
        RM.validate_config(PipelineConfig(plugins={"not_a_slot": {"ref": "x"}}))
    with pytest.raises(ValueError, match="ambiguous"):
        RM.validate_config(PipelineConfig(radio_model="geometric",
                                          plugins={"channel_model": {"ref": "disc"}}))


def test_radio_model_enum_still_rejects_garbage():
    with pytest.raises(ValueError, match="radio_model"):
        RM.validate_config(PipelineConfig(radio_model="nope"))


# --------------------------------------------------------------------------- D3 rng ------------- #
def test_plugin_never_receives_the_global_rng(plugin_pkg):
    """C3: neither the module-level `random` state nor a Random handed to the engine moves."""
    mod = importlib.import_module("myorg_radio")
    ns = api.RngNamespace(7, "hardrange")
    model = mod.HardRangeLink(params={}, rng=ns, env={"radio_range_m": 300.0})
    random.seed(999)
    before = random.getstate()
    g = random.Random(1)
    gs = g.getstate()
    for k in range(5):
        model.delivery_coin(k, k + 1)
    assert random.getstate() == before and g.getstate() == gs


def _at_step(seed, pid, step, replicate=0):
    ns = api.RngNamespace(seed, pid, replicate)
    ns.begin_step(step)
    return ns


def test_rng_namespace_keys_are_reserved_stable_and_step_scoped():
    ns = _at_step(7, "myplug", 3)
    # the `plugin:<id>:` segment is RESERVED, so a third-party stream can never collide with a core
    # label ("shadow2", "geo", "shadow", "denm", "lc", ...)
    assert ns.key("lbl", 1, 2, with_step=True) == "7:plugin:myplug:lbl:1:2:s3"
    # STATELESS: a pure function of (seed, plugin, label, ids, step) -- immune to call ORDER and
    # call COUNT, which is what makes a stateless plugin unable to perturb anything at all
    for _ in range(4):
        ns.stream("lbl", 1, 2)                        # burn calls; the value must not move
    assert ns.stream("lbl", 1, 2).random() == _at_step(7, "myplug", 3).stream("lbl", 1, 2).random()
    assert ns.stream("lbl", 1, 2).random() != _at_step(7, "myplug", 4).stream("lbl", 1, 2).random()
    # STATEFUL: the framework owns the cache, and the key EXCLUDES the step
    a = ns.persistent("s", 1)
    ns.begin_step(4)
    assert ns.persistent("s", 1) is a
    assert ns.declared_streams() == ("lbl", "s")      # recorded in the manifest, sorted
    assert ns.advance_counts()["7:plugin:myplug:s:1"] == 2
    # `replicate` is ns-3's RngRun doctrine: independent replications advance the RUN NUMBER, not
    # the seed -- and it changes NOTHING at replicate == 0, so no existing digest moves.
    assert api.RngNamespace(7, "myplug", 0).key("l", with_step=False) == \
        api.RngNamespace(7, "myplug").key("l", with_step=False)
    assert api.RngNamespace(7, "myplug", 1).key("l", with_step=False) != \
        api.RngNamespace(7, "myplug").key("l", with_step=False)


# ------------------------------------- the per-step dimension of the namespace must be LIVE ------ #
# `RngNamespace.begin_step` was, for a whole phase, called by NOBODY. `_step` stayed at -1 for an
# entire run, every `stream()` key ended `:s-1`, and the documented "pure function of (seed,
# replicate, plugin, label, ids, step)" quietly lost its last term. Nothing failed, because nothing
# asserted it. These three tests are that assertion, at the three levels it can be made.
class _StepProbe:
    """Minimal LinkChannelModel that reports the namespace step it was driven at."""

    interface_version = apichan.INTERFACE_VERSION
    plugin_id = "stepprobe"
    reach_m = 500.0

    def __init__(self, ns):
        self._rng = ns
        self.seen = []

    def capabilities(self):
        return frozenset({"reach", apichan.LOSS_INDEPENDENT_SURVIVAL})

    def begin_step(self, frame):
        self.seen.append(self._rng.step)

    def evaluate(self, tx, rx, d_m, txn):
        return apichan.DELIVERED

    def draw(self, tx_vid, rx_vid):
        return self._rng.stream("fade", tx_vid, rx_vid).random()


class _BatchStepProbe(_StepProbe):
    plugin_id = "bstepprobe"

    def deliver(self, frame, candidates):
        return [apichan.LinkOutcome(t, r) for t, r, _d in candidates]


def _frame(step):
    stations = {v: apichan.StationSnapshot(v, 40.0 * v, 0.0, 1.6, 1.6) for v in range(3)}
    txns = [apichan.Transmission(i, i, "cam", 1, 300, "d%02d" % i) for i in range(3)]
    return apichan.StepFrame(step, float(step), 1.0, stations, txns, sorted(stations), 0.0, {})


@pytest.mark.parametrize("probe_cls,adapter", [(_StepProbe, apichan.PerLinkAdapter),
                                               (_BatchStepProbe, apichan.BatchAdapter)])
def test_both_adapters_advance_the_rng_namespace_once_per_step(probe_cls, adapter):
    """THE regression test for the dead step dimension, at the adapter.

    The adapter -- not the engine and not the model -- owns advancing the namespace, because it is
    the one object every driver of a model already calls `begin_step` on (the engine loop,
    `conformance.v1.harness.trace`, C4 and C8). Delete `self.rng_ns.begin_step(frame.step)` from
    either adapter and this fails: `seen` becomes `[-1, -1, -1, -1]` and the four keys collapse to
    one.
    """
    ns = api.RngNamespace(11, probe_cls.plugin_id)
    model = probe_cls(ns)
    ad = adapter(model, ns)
    keys, draws = [], []
    for step in range(4):
        ad.begin_step(_frame(step))
        keys.append(ns.key("fade", 1, 2, with_step=True))
        draws.append(model.draw(1, 2))          # the same link, four steps apart
    assert model.seen == [0, 1, 2, 3], (
        f"the adapter did not advance the RngNamespace: it saw {model.seen}. A model driven at a "
        f"frozen step is a DIFFERENT MODEL from the one the engine runs.")
    assert keys == ["11:plugin:%s:fade:1:2:s%d" % (probe_cls.plugin_id, k) for k in range(4)]
    assert len(set(draws)) == 4, (
        f"one link drew {sorted(set(draws))} across four steps: a stateless per-packet draw that "
        f"does not move with the step IS the defect, not a symptom of it")
    # ...and a namespace that is never advanced is exactly the -1 the defect produced
    frozen = api.RngNamespace(11, probe_cls.plugin_id)
    assert frozen.key("fade", 1, 2, with_step=True).endswith(":s-1")


def test_a_frozen_step_turns_a_per_packet_draw_into_a_per_link_constant():
    """WHY it mattered, as a number rather than an argument.

    A stateless `stream()` is keyed on the step. Freeze the step and the same (tx, rx) pair draws
    the SAME value at every step for the whole run -- so a model advertising a per-packet Rayleigh
    fade actually applies one fixed offset per link and the channel never fades. This is the
    mechanism behind the measured PDR ladder flattening (0.18 of range loss recovered at the far
    rung once the step was live).
    """
    live, frozen = api.RngNamespace(5, "fadeprobe"), api.RngNamespace(5, "fadeprobe")
    live_draws, frozen_draws = [], []
    for step in range(8):
        live.begin_step(step)                       # what the adapter does
        live_draws.append(live.stream("fade", 3, 7).random())
        frozen_draws.append(frozen.stream("fade", 3, 7).random())   # begin_step never called
    assert len(set(frozen_draws)) == 1, "a frozen namespace must be constant -- that is the defect"
    assert len(set(live_draws)) == 8, "a live namespace must give one value per step"
    assert live_draws[0] != frozen_draws[0]         # ...and s0 is not s-1


def test_the_engine_gives_a_plugin_a_LIVE_per_step_rng_dimension(tmp_path, plugin_pkg):
    """The same statement END TO END, which is the level the defect actually lived at.

    The adapter-level test above passes even if `build_channel` stops handing the namespace to the
    adapter (`PerLinkAdapter(model)` instead of `PerLinkAdapter(model, rng_ns)`) -- which is exactly
    the shape the bug had. This one drives a real `run_pipeline` and reads back, from inside the
    plugin, both the namespace step at every `begin_step` and the full set of stream keys it
    produced. Before the fix: `sorted({ns.step}) == [-1]` and every key ended `:s-1`.
    """
    mod = importlib.import_module("myorg_radio")
    mod.SEEN_STEPS.clear()
    mod.FADE_KEYS.clear()
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "steps"),
                                plugins={"channel_model": {"ref": "myorg_radio:StepRecorder"}},
                                **_PLUGIN_CFG))
    assert mod.SEEN_STEPS, "the plugin was never driven"
    mismatched = [(f, n) for f, n in mod.SEEN_STEPS if f != n]
    assert not mismatched, (
        f"{len(mismatched)} step(s) where the RngNamespace disagreed with the frame it was driven "
        f"with, e.g. (frame.step, ns.step) = {mismatched[:5]}. `build_channel` must pass `rng_ns` "
        f"to the adapter it builds, or the whole per-step dimension is dead.")
    steps = sorted({n for _f, n in mod.SEEN_STEPS})
    assert steps[0] == 0 and len(steps) > 1 and steps == list(range(steps[-1] + 1)), steps
    assert -1 not in steps
    suffixes = {k.rsplit(":", 1)[-1] for k in mod.FADE_KEYS}
    assert "s-1" not in suffixes, "the stream keys are frozen at the sentinel step"
    assert suffixes <= {"s%d" % k for k in steps}, sorted(suffixes)[:5]
    # Every step that offered a candidate link contributed its OWN key term. The count can be one
    # short of the step count and no more: on this scenario step 0 has no two stations in range yet,
    # so it draws nothing. Before the fix this number was 1 for the whole run.
    assert len(steps) - 1 <= len(suffixes) <= len(steps), (
        f"{len(mod.FADE_KEYS)} stream keys spread over only {len(suffixes)} distinct step terms "
        f"across {len(steps)} steps: {sorted(suffixes)[:5]}")


def test_plugin_id_namespace_is_enforced():
    with pytest.raises(api.ConfigError):
        api.RngNamespace(1, "Bad Id")
    with pytest.raises(api.ConfigError):
        api.RngNamespace(1, "x")                      # too short


# --------------------------------------------------------------------------- D4 the lock -------- #
def test_manifest_records_runtime_and_the_plugin_lock(tmp_path):
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "m"), **_PLUGIN_CFG))
    man = json.loads((tmp_path / "m" / "manifest.json").read_text(encoding="utf-8"))
    rt = man["runtime"]
    assert rt["python"] == sys.version
    assert rt["hash_randomization"] is bool(sys.flags.hash_randomization)
    assert rt["platform"] and rt["implementation"] == "CPython"
    entry = man["plugins"]["loaded"][0]
    assert (entry["slot"], entry["ref"], entry["resolved_via"]) == ("channel_model", "disc", "builtin")
    assert "legacy_global_rng" in entry["capabilities"]        # the grandfathering is RECORDED
    assert "loss_composition:additive_legacy" in entry["capabilities"]
    # The detection layer is locked the same way, in the order it is evaluated in.
    checks = [e for e in man["plugins"]["loaded"] if e["slot"] == "check"]
    assert [e["ref"] for e in checks] == list(RM.default_check_refs())
    assert [e["order"] for e in checks] == list(range(len(checks)))
    fusion, = [e for e in man["plugins"]["loaded"] if e["slot"] == "fusion"]
    assert fusion["ref"] == "streak_v1" and "legacy_global_rng" in fusion["capabilities"]
    assert man["plugins"]["interface_versions"]["Detector"] == "1.0"
    assert man["plugins"]["api_version"] == "1.0"
    assert res.data_digest                                     # digest is over data files only


def test_replay_of_a_drifted_plugin_fails_before_step_0(tmp_path, plugin_pkg):
    """D4: mutate one byte of the plugin's source and the manifest no longer replays. Contrast the
    old behaviour, which dropped the unknown key with a warning and exited 0."""
    run_pipeline(PipelineConfig(
        plugins={"channel_model": {"ref": "myorg_radio:HardRangeLink"}},
        out_dir=str(tmp_path / "drift"), **_PLUGIN_CFG))
    man = json.loads((tmp_path / "drift" / "manifest.json").read_text(encoding="utf-8"))
    assert RM.config_from_dict(man).plugins["channel_model"]["ref"] == "myorg_radio:HardRangeLink"
    plugin_pkg.write_text(_PLUGIN_SRC + "\n# one more byte\n", encoding="utf-8")
    with pytest.raises(api.PluginDriftError) as e:
        RM.config_from_dict(man)
    assert e.value.field == "module_sha256"
    # ...and --allow-plugin-drift proceeds AND RECORDS it rather than silencing it
    assert RM.config_from_dict(man, allow_plugin_drift=True).plugins


def test_config_from_dict_refuses_an_unintelligible_plugin_key():
    with pytest.raises(api.ConfigError, match="plugin"):
        RM.config_from_dict({"seed": 3, "plugin_backends": {"x": 1}})


def test_plugins_field_round_trips(tmp_path):
    cfg = PipelineConfig(plugins={"channel_model": {"ref": "geometric", "params": {}}},
                         out_dir=str(tmp_path / "rt"))
    back = RM.config_from_dict(json.loads(json.dumps(
        {k: (list(v) if isinstance(v, tuple) else v) for k, v in cfg.__dict__.items()})))
    assert back.__dict__ == cfg.__dict__


# --------------------------------------------------------------------------- the adapter -------- #
def test_perlink_adapter_deliver_agrees_with_the_per_link_path():
    """The phase-4 switch to a batch-consuming loop must be provable, not hopeful."""
    cfg = PipelineConfig(seed=5, radio_model="geometric", radio_nlosb_density_per_km=0.0)
    stations = {v: apichan.StationSnapshot(v, 40.0 * v, 0.0, RM.V2X_ANTENNA_HEIGHT_M, 1.6)
                for v in range(6)}
    txns = [apichan.Transmission(i, i, "cam", 1, 300, "d%02d" % i) for i in range(6)]
    frame = apichan.StepFrame(0, 0.0, 1.0, stations, txns, sorted(stations), 0.0, {})
    cands = [(i, r, abs(40.0 * i - 40.0 * r)) for r in range(6) for i in range(6) if i != r]
    cands.sort(key=lambda c: (c[1], c[0]))

    a = apichan.PerLinkAdapter(RM.GeometricChannel(cfg, buildings=None, dt=1.0))
    a.begin_step(frame)
    batched = [(o.tx_index, o.rx_vid, o.rssi_dbm, o.link_state)
               for o in apichan.sort_outcomes(a.deliver(frame, cands))]

    b = apichan.PerLinkAdapter(RM.GeometricChannel(cfg, buildings=None, dt=1.0))
    b.begin_step(frame)
    per_link = []
    for tx_index, rx_vid, d in cands:
        out = b.evaluate_link(stations[tx_index], stations[rx_vid], d, txns[tx_index])
        if out is not None:
            per_link.append((tx_index, rx_vid, out.rssi_dbm, out.link_state))
    assert batched == per_link and batched


def test_geometric_reach_alias_and_declared_capabilities():
    cfg = PipelineConfig(seed=1, radio_model="geometric")
    ch = RM.GeometricChannel(cfg, buildings=None, dt=1.0)
    assert ch.cap_m == ch.reach_m > 0.0                 # cap_m kept as an alias for one minor ver
    caps = ch.capabilities()
    assert {"rssi", "link_state", "reach", "cbr", "stateful"} <= caps
    assert apichan.LOSS_INDEPENDENT_SURVIVAL in caps
    assert apichan.loss_composition(ch) == "independent_survival"
    assert apichan.loss_composition(RM.DiscChannel(range_m=100.0)) == "additive_legacy"


def test_logdistance_window_exceeds_its_declared_reach():
    """The distinction the memo's single `reach_m` cannot express, and 939b4faa... depends on: the
    candidate SEARCH window is widened by the shadow headroom, the DELIVERY reach is not."""
    m = RM.LogDistanceChannel(seed=1, range_m=200.0, pathloss_exponent=2.7,
                              shadowing_sigma_db=4.0, rx_sensitivity_margin_db=0.0,
                              cap_sigma=4.0, cap_max_mult=6.0)
    rx = apichan.StationSnapshot(1, 0.0, 0.0, 1.6, 1.6)
    assert m.window_m(rx) > m.reach_m_for(rx) == 200.0


def test_outcome_vocabulary_is_closed():
    with pytest.raises(api.ConfigError):
        apichan.check_outcome(apichan.LinkOutcome(0, 0, None, "LOS_ISH"))
    with pytest.raises(api.ConfigError):
        apichan.check_outcome(apichan.LinkOutcome(0, 0, -999.0, "LOS"))
    with pytest.raises(api.ConfigError):
        apichan.check_outcome(apichan.LinkOutcome(0, 0, None, "LOS", delay_s=-1.0))
    apichan.check_outcome(apichan.LinkOutcome(0, 0, -80.0, "NLOSv", delay_s=0.0))


def test_dtos_are_frozen():
    s = apichan.StationSnapshot(1, 0.0, 0.0, 1.6, 1.6)
    with pytest.raises(dataclasses.FrozenInstanceError):
        s.x = 5.0
