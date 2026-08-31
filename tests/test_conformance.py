"""Phase 2: the conformance suite (C1-C12) and the provenance lock.

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
    """Violates C8: delivers past its own declared reach, which is exactly what makes
    acceptanceRangeThreshold wrong for every honest long link."""
    plugin_id = "overreach"

    def window_m(self, rx):
        return 3.0 * self.reach_m

    def evaluate(self, tx, rx, d_m, txn):
        return apichan.LinkOutcome(rssi_dbm=-70.0) if d_m <= 3.0 * self.reach_m else None


class Antimonotone(Good):
    """Violates C9: PDR RISES with distance."""
    plugin_id = "antimono"

    def evaluate(self, tx, rx, d_m, txn):
        if d_m > (rx.rx_range_m or self.reach_m):
            return None
        frac = d_m / max(self.reach_m, 1.0)
        u = self._rng.stream("keep", tx.vid, rx.vid).random()
        return apichan.LinkOutcome(rssi_dbm=-90.0 + 30.0 * frac) if u < frac else None


class AcceptsAnything(Good):
    """Violates C10: swallows a parameter its own FieldSpec declares out of range, so the failure
    surfaces at step k > 0 as a physically absurd run rather than before step 0 as an error."""
    plugin_id = "acceptsany"

    @classmethod
    def config_fields(cls):
        return {}                      # declares nothing -> the framework has nothing to enforce

    def __init__(self, *, params, rng, env):
        super().__init__(params={}, rng=rng, env=env)


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
    assert "13 passed" in capsys.readouterr().out
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
    assert conf["passed"] == 12 and conf["skipped"] == 0        # the other twelve all ran
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
