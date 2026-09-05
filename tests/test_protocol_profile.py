"""The PROTOCOL PROFILE seam and the REPORT FORMAT slot.

The user's requirement was that the network side "function like an exact protocol in the real world"
and be "modular and support different protocols such as thresholding schemes" -- a third party must
be able to supply a different protocol STACK without forking, the same way they can already supply a
detector or a channel model. Four things have to be true for that, and each one is a section here:

1. **The seam exists and the built-in goes through it.** `api/profile.py` bundles the five decisions
   a V2X stack makes; `codecs/profiles.py::EtsiItsG5Profile` is the ITS-G5 stack expressed through
   it. If the built-in were a special case the seam would not be real, so
   :func:`test_the_step_loop_reaches_no_etsi_constant_directly` asserts by SOURCE that `run.py`'s
   step loop calls no generation, congestion, airtime or latency function directly any more.
2. **The empty slot is filled.** `report_format` had been registered with no interface, no built-in
   and no consumer. `ma_report_v1` is the historic row through the seam, and it is graded the only
   way that means anything: it reproduces the pinned default golden byte-identically.
3. **A third party really can supply a stack.** `scms-cv2x-profile` is a separate DISTRIBUTION,
   installed into site-packages, importing `scms_sim_ref.api` and nothing else. It runs with zero
   edits to `run.py`, `featurize.py` or any test file, and it measurably changes the message rate,
   the CBR, the delivered-frame count and the latency.
4. **The conformance suite can FAIL a bad stack.** That distribution also ships six deliberately
   broken plugins, each broken in one way, and every one is refused on the check named in its own
   docstring. A suite that has never been shown to fail a bad plugin has not been shown to do
   anything -- and this project has already shipped one powerless gate.

Both pinned digests are asserted unchanged with every new feature off.
"""
import json
import os
import subprocess
import sys

import pytest

from scms_sim_ref.api import profile as AP
from scms_sim_ref.api import registry as REG
from scms_sim_ref.api import report as ARP
from scms_sim_ref.api.errors import ApiError, ConfigError
from scms_sim_ref.codecs import etsi_rules as ER
from scms_sim_ref.codecs import profiles as PROF
from scms_sim_ref.codecs import reports as RPT
from scms_sim_ref.conformance.runner import CONTRACTS, run_ref
from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline, validate_config
from scms_sim_ref.mock_pipeline import run as RM
from scms_sim_ref.mock_pipeline.run import main
from scms_sim_ref.schemas.records import FORBIDDEN_FEATURE_KEYS, is_forbidden_feature_key

#: The two pinned digests this whole workstream is held to.
REFERENCE_DIGEST = "b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815"
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)

#: The third-party distribution. Its absence SKIPS rather than fails: the engine must be usable and
#: testable without it, which is the whole claim.
try:
    import scms_cv2x_profile                                       # noqa: F401
    from scms_cv2x_profile.bad import MUST_FAIL
    HAVE_CV2X = True
except ImportError:                                                # pragma: no cover
    MUST_FAIL = {}
    HAVE_CV2X = False

needs_cv2x = pytest.mark.skipif(
    not HAVE_CV2X,
    reason="third-party distribution `scms-cv2x-profile` is not installed "
           "(python <dist>/install_local.py)")

CV2X_PROFILE = "scms_cv2x_profile.profile:Cv2xMode4Profile"
CV2X_FORMAT = "scms_cv2x_profile.report:CompactReportFormat"


def _manifest(d):
    with open(os.path.join(str(d), "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def _protocol(d):
    return _manifest(d)["counts"].get("protocol", {})


def _rows(d):
    with open(os.path.join(str(d), "ma", "ma_reports.jsonl"), encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


# =========================================================================== #
# 1. THE SEAM EXISTS, AND THE BUILT-IN GOES THROUGH IT
# =========================================================================== #
def test_the_two_new_slots_are_registered_and_resolvable():
    assert "protocol_profile" in REG.SLOTS and "report_format" in REG.SLOTS
    cls, how, iv, shape = REG.resolve("protocol_profile", "etsi_its_g5")
    assert (cls, how, iv, shape) == (PROF.EtsiItsG5Profile, "builtin", "ProtocolProfile/1.0",
                                     "profile")
    cls, how, iv, shape = REG.resolve("report_format", "ma_report_v1")
    assert (cls, how, iv, shape) == (RPT.MaReportV1Format, "builtin", "ReportFormat/1.0", "report")


def test_the_signature_check_is_what_gates_the_new_slots():
    """Neither `Protocol` nor `ABC` checks signatures at runtime; `registry._check_signature` does.

    Graded by handing the resolver a class that satisfies the Protocol structurally but names a
    parameter wrong -- the exact mistake an implementer working from the docstring makes.
    """
    class WrongName(PROF.EtsiItsG5Profile):
        def frame_airtime_s(self, nbytes):                         # should be `size_bytes`
            return 0.001

    REG.register_builtin("protocol_profile", "_test_wrong_name", WrongName)
    try:
        with pytest.raises(ApiError) as e:
            REG.resolve("protocol_profile", "_test_wrong_name")
        assert "size_bytes" in str(e.value)
    finally:
        REG._BUILTINS["protocol_profile"].pop("_test_wrong_name", None)


def test_the_step_loop_reaches_no_etsi_constant_directly():
    """**If the built-in is a special case, the seam is not real.**

    Every ETSI state machine, airtime and latency function the engine uses must arrive through a
    profile method. Asserted on the SOURCE, because that is the only way to state "there is no other
    path" -- a behavioural test can only show that the path taken happened to be the right one.

    The ONE `_etsi_rules` reference that legitimately remains is named: the `dt <= T_GenCamMax`
    CONFIG gate, which validates the built-in's own flag against the built-in's own constant, before
    any profile object exists.
    """
    src = open(RM.__file__, encoding="utf-8").read()
    forbidden = ("_etsi_rules.CamGenerationState", "_etsi_rules.ReactiveDcc",
                 "_etsi_rules.frame_airtime_s", "_etsi_rules.link_latency_s",
                 "_etsi_rules.channel_busy_ratio", "_etsi_rules.ppdu_airtime_s",
                 "_etsi_rules.T_GEN_CAM_MIN_S", "_etsi_rules.DYNAMICS_TRIGGERS",
                 "_etsi_rules.STACK_LATENCY_S", "_etsi_rules.LATENCY_REFERENCE_BAND_S",
                 "_etsi_rules.CAM_TRIGGER_POSITION_M", "_etsi_rules.DCC_REACTIVE_STATES")
    hits = [name for name in forbidden if name in src]
    assert not hits, (f"run.py still reaches ETSI rules directly: {hits}. Every one of these must "
                      f"arrive through a ProtocolProfile method, or a third-party stack cannot "
                      f"replace it")
    assert "_etsi_rules.T_GEN_CAM_MAX_S" in src, \
        "the one documented remaining reference disappeared; update this test's docstring"


def test_a_third_party_stacks_manifest_carries_no_etsi_constant(tmp_path):
    """A C-V2X run's `cbr` block must not publish TS 102 687's first breakpoint.

    Small, and exactly the kind of thing a seam is for: the engine MEASURES the CBR, but only a
    profile can say what the number means, so the reactive machine's breakpoint travels with the
    ETSI profile's own block and a different stack's manifest simply does not carry it. A constant
    published beside a run that does not speak that standard is a finding-shaped fabrication.
    """
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "e"), message_codec="native_v1",
                                cam_generation_rules=False, protocol_profile="etsi_its_g5",
                                **_DEFAULT))
    assert "dcc_breakpoint" in _protocol(tmp_path / "e")["cbr"]
    if HAVE_CV2X:
        run_pipeline(PipelineConfig(out_dir=str(tmp_path / "c"), message_codec="native_v1",
                                    plugins={"protocol_profile": {"ref": CV2X_PROFILE}},
                                    **_DEFAULT))
        assert "dcc_breakpoint" not in _protocol(tmp_path / "c")["cbr"]


def test_the_profiles_airtime_constants_are_still_the_engines():
    """`etsi_rules` holds its own copies of the two ASSUMED airtimes so a profile can report the
    disagreement without importing the engine. Copies drift; this is what stops them."""
    assert ER.PYTHON_ASSUMED_FRAME_AIRTIME_S == RM.PHY_FRAME_AIRTIME_S
    assert ER.JAVA_ASSUMED_MPDU_BYTES == 300
    assert PROF.LEGACY_WIRE_SIZE_BYTES == RM.NATIVE_WIRE_SIZE_BYTES


def test_capabilities_are_what_switch_the_engines_layers_on():
    """The engine asks the PROFILE which layers are active, never the config flags -- which is what
    lets a third-party stack supply generation rules with no engine flag set at all."""
    off = PROF.EtsiItsG5Profile(params={}, env={})
    assert AP.CAP_GENERATION not in off.capabilities()
    assert AP.CAP_CONGESTION not in off.capabilities()
    on = PROF.EtsiItsG5Profile(params={"generation": True, "congestion": True, "latency": True},
                               env={})
    caps = on.capabilities()
    assert {AP.CAP_GENERATION, AP.CAP_CONGESTION, AP.CAP_LATENCY, AP.CAP_AIRTIME} <= caps
    assert on.new_generation_state() is not None and on.new_congestion_state() is not None


def test_the_builtin_refuses_congestion_without_generation():
    with pytest.raises(ValueError) as e:
        PROF.EtsiItsG5Profile(params={"congestion": True}, env={})
    assert "generation=True" in str(e.value)


def test_the_builtin_generation_state_is_the_standards_state_machine():
    """`EtsiCamGenerationState` must BE `CamGenerationState` with the seam's signature -- not a
    reimplementation of clause 6.1.3 that could drift from the one the refdata grades."""
    assert issubclass(PROF.EtsiCamGenerationState, ER.CamGenerationState)
    assert issubclass(PROF.EtsiReactiveDccState, ER.ReactiveDcc)
    st = PROF.EtsiCamGenerationState()
    ego = AP.GenerationInput(t=0.0, x=0.0, y=0.0, speed=10.0, heading=0.0, dt=0.1)
    assert st.evaluate(ego) == ER.TRIGGER_FIRST
    # 0.0 -- the seam's "no congestion control" -- must land on the CAM service's own floor.
    too_soon = AP.GenerationInput(t=0.05, x=0.5, y=0.0, speed=10.0, heading=0.0, dt=0.1)
    assert st.evaluate(too_soon) == ER.TRIGGER_NONE
    far = AP.GenerationInput(t=0.6, x=6.0, y=0.0, speed=10.0, heading=0.0, dt=0.1)
    assert st.evaluate(far) == ER.TRIGGER_POSITION


def test_the_congestion_state_exposes_the_seams_method():
    dcc = PROF.EtsiReactiveDccState()
    dcc.update(0.0)
    assert dcc.min_interval_s() == dcc.t_gen_cam_floor == ER.T_GEN_CAM_MIN_S
    dcc.update(0.95)
    assert dcc.min_interval_s() == 1.0 and dcc.state == "restrictive"


# =========================================================================== #
# 2. THE PINNED DIGESTS, WITH EVERY NEW FEATURE OFF
# =========================================================================== #
def test_default_golden_is_untouched_by_the_profile_seam(tmp_path):
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


def test_reference_digest_is_untouched_by_the_profile_seam(tmp_path):
    res = run_pipeline(PipelineConfig(
        seed=42, traffic_flow=True, road_network="grid", grid_w=6, grid_h=6, duration_s=300,
        arrival_rate=2.0, attacker_pct=0.15, traffic_lights=True, out_dir=str(tmp_path / "r")))
    assert res.data_digest == REFERENCE_DIGEST


def test_no_profile_object_is_constructed_by_default(tmp_path):
    """The default must be free, not merely equal. Asserted through the artifact: no profile means
    no lock entry, no `protocol` block and no `standards_profile` claim."""
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), **_DEFAULT))
    m = _manifest(tmp_path / "d")
    assert "protocol" not in m["counts"]
    slots = {e["slot"] for e in (m["plugins"]["loaded"] or [])}
    assert "protocol_profile" not in slots and "report_format" not in slots
    assert "protocol_profile" not in m["standards_profile"]
    assert "report_format" not in m["standards_profile"]


def test_the_builtin_profile_with_every_layer_off_is_still_the_golden(tmp_path):
    """Naming the profile explicitly constructs it, records it, and changes nothing."""
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "p"), protocol_profile="etsi_its_g5",
                                      **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN
    lock = [e for e in _manifest(tmp_path / "p")["plugins"]["loaded"]
            if e["slot"] == "protocol_profile"]
    assert len(lock) == 1 and lock[0]["ref"] == "etsi_its_g5"
    assert lock[0]["resolved_via"] == "builtin"


# =========================================================================== #
# 3. THE REPORT-FORMAT SLOT IS FILLED, AND THE BUILT-IN IS GRADED
# =========================================================================== #
def test_ma_report_v1_through_the_seam_reproduces_the_pinned_golden(tmp_path):
    """**The only grading of a built-in that means anything.**

    `ma_report_v1` is not "close to" the historic row -- it produces the identical dataset, so the
    seam is a refactor of the format's LOCATION and nothing else. A built-in that improved anything
    could not be graded this way, and a seam whose built-in cannot be graded against the artifact it
    replaces is a rewrite wearing a plugin's clothes.
    """
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "f"), report_format="ma_report_v1",
                                      **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


def test_ma_report_v1_matches_the_inline_row_field_for_field(tmp_path):
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **_DEFAULT))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), report_format="ma_report_v1",
                                    **_DEFAULT))
    assert a.data_digest == b.data_digest
    ra, rb = _rows(tmp_path / "a"), _rows(tmp_path / "b")
    assert ra and ra == rb


def test_ts103759_shape_carries_real_evidence_octets(tmp_path):
    """The gap the previous stage left open: `evidence_pdu()` produced real octets and nothing
    carried them into the report. `v2xPduEvidence` is `SEQUENCE (SIZE(1..MAX)) OF V2xPduStream` --
    mandatory, minimum one -- so a report without it is structurally not a report."""
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "t"), message_codec="native_v1",
                                report_format="ts103759_shape", **_DEFAULT))
    rows = _rows(tmp_path / "t")
    assert rows
    with_pdu = [r for r in rows if r["v2xPduEvidence_present"]]
    assert len(with_pdu) > 0.5 * len(rows), "most reports should carry the subject's own PDU"
    ev = with_pdu[0]["v2xPduEvidence"][0]
    assert ev["octets"] > 0 and ev["profile_id"] == "native_v1"
    assert bytes.fromhex(ev["pdu_hex"]) and len(bytes.fromhex(ev["pdu_hex"])) == ev["octets"]
    # ...and it really is a decodable PDU, not a hex-shaped blob.
    from scms_sim_ref.codecs.native import NativeV1Codec
    claim = NativeV1Codec().decode_cam(bytes.fromhex(ev["pdu_hex"]))
    assert claim.cert_digest == with_pdu[0]["subject_cert_digest"]


def test_evidence_costs_nothing_when_no_format_asks_for_it(tmp_path):
    """`ma_report_v1` does not declare `evidence_pdu`, so the engine must never fill the store."""
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "n"), message_codec="native_v1",
                                report_format="ma_report_v1", **_DEFAULT))
    rows = _rows(tmp_path / "n")
    assert rows and all("v2xPduEvidence" not in r for r in rows)
    assert all(r["evidence_msg_refs"] == [f"{r['report_id']}-m"] for r in rows)


def test_a_report_format_row_carries_no_ground_truth(tmp_path):
    for fmt in ("ma_report_v1", "ts103759_shape"):
        d = tmp_path / fmt
        run_pipeline(PipelineConfig(out_dir=str(d), message_codec="native_v1", report_format=fmt,
                                    **_DEFAULT))
        for row in _rows(d):
            bad = sorted(k for k in row if is_forbidden_feature_key(k))
            assert not bad, (fmt, bad)


def test_report_format_standards_claims_are_honest():
    for cls in (RPT.MaReportV1Format, RPT.Ts103759ShapeFormat):
        claim = cls()
        assert claim.standards_claim()["conformant_to"] is None
        assert claim.standards_claim()["deviations"]


# =========================================================================== #
# 4. SELF-DESCRIPTION AND THE CLI
# =========================================================================== #
def test_both_new_fields_are_self_describing():
    sch = config_schema()
    for name in ("protocol_profile", "report_format"):
        assert name in sch, name
        assert sch[name]["group"] == "Protocol", (name, sch[name]["group"])
        assert sch[name]["help"], name
        assert sch[name]["options"][0] == "", name
    assert "etsi_its_g5" in sch["protocol_profile"]["options"]
    assert "ma_report_v1" in sch["report_format"]["options"]
    assert "ts103759_shape" in sch["report_format"]["options"]


def test_new_cli_flags_parse_and_wire_through(tmp_path):
    cfg_path = tmp_path / "eff.json"
    rc = main(["--seed", "3", "--steps", "3", "--vehicles", "12",
               "--protocol-profile", "etsi_its_g5", "--report-format", "ma_report_v1",
               "--out", str(tmp_path / "o"), "--dump-config", str(cfg_path)])
    assert rc == 0
    eff = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert eff["protocol_profile"] == "etsi_its_g5"
    assert eff["report_format"] == "ma_report_v1"


def test_unknown_names_are_refused_by_name():
    with pytest.raises(ValueError) as e:
        validate_config(PipelineConfig(protocol_profile="nope"))
    assert "plugins.protocol_profile" in str(e.value)
    with pytest.raises(ValueError) as e:
        validate_config(PipelineConfig(report_format="nope"))
    assert "plugins.report_format" in str(e.value)


def test_bad_profile_params_are_refused_at_config_time():
    """Before step 0 and before an output directory exists -- conformance C10's rule for every slot,
    and reachable WITHOUT constructing the profile so `--check-config` and the GUI can use it."""
    with pytest.raises(ValueError) as e:
        validate_config(PipelineConfig(
            plugins={"protocol_profile": {"ref": "etsi_its_g5", "params": {"nonsense": 1}}}))
    assert "nonsense" in str(e.value)
    with pytest.raises(ValueError) as e:
        validate_config(PipelineConfig(
            plugins={"protocol_profile": {"ref": "etsi_its_g5",
                                          "params": {"stack_latency_s": 99.0}}}))
    assert "stack_latency_s" in str(e.value)


def test_the_two_spellings_may_not_disagree():
    with pytest.raises(ConfigError):
        RM._profile_selection(PipelineConfig(
            protocol_profile="etsi_its_g5",
            plugins={"protocol_profile": {"ref": "somebody.else:Stack"}}))


def test_a_builtin_name_cannot_be_hijacked(tmp_path):
    """`register_builtin` overwrites per (slot, name) and importing a distribution is enough to call
    it, so a third party could otherwise bind its class to `etsi_its_g5` and be resolved AS a
    built-in -- inheriting the built-in's exemption from the source gate and from attestation."""
    class Impostor(PROF.EtsiItsG5Profile):
        pass

    for slot, name in (("protocol_profile", "etsi_its_g5"), ("report_format", "ma_report_v1")):
        original = REG.builtin(slot, name)
        REG.register_builtin(slot, name, Impostor if slot == "protocol_profile"
                             else type("F", (RPT.MaReportV1Format,), {}))
        try:
            builder = RM.build_profile if slot == "protocol_profile" else RM.build_report_format
            cfg = PipelineConfig(**{slot: name})
            with pytest.raises(ConfigError) as e:
                builder(cfg, None) if slot == "protocol_profile" else builder(cfg)
            assert "BUILT-IN" in str(e.value)
        finally:
            REG.register_builtin(slot, name, original)


def test_a_profile_may_not_silently_discard_a_declared_codec(tmp_path):
    """A profile is entitled to bring its OWN wire format -- that is the point of the seam -- but a
    run whose manifest says `message_codec: native_v1` while nothing was ever encoded is the
    "replays as a different run at exit 0" failure the plugin lock exists to prevent."""
    class CodecDiscarding(PROF.EtsiItsG5Profile):
        def codec(self):
            return None

    REG.register_builtin("protocol_profile", "_test_discard", CodecDiscarding)
    try:
        with pytest.raises(ConfigError) as e:
            run_pipeline(PipelineConfig(out_dir=str(tmp_path / "z"), message_codec="native_v1",
                                        protocol_profile="_test_discard", seed=3, n_vehicles=12,
                                        n_steps=2))
        assert "returned None from codec()" in str(e.value)
    finally:
        REG._BUILTINS["protocol_profile"].pop("_test_discard", None)


def test_an_unconsumed_slot_is_still_refused():
    with pytest.raises(ValueError) as e:
        validate_config(PipelineConfig(plugins={"mobility": {"ref": "x.y:Z"}}))
    assert "not yet consumed" in str(e.value)
    assert "protocol_profile" in str(e.value) and "report_format" in str(e.value)


# =========================================================================== #
# 5. THE CONFORMANCE SUITE -- ON THE BUILT-INS
# =========================================================================== #
def test_the_new_slots_have_contracts():
    assert set(CONTRACTS) >= {"channel_model", "check", "protocol_profile", "report_format"}


@pytest.mark.parametrize("params", [
    {},
    {"generation": True},
    {"generation": True, "congestion": True, "latency": True},
])
def test_the_builtin_profile_passes_its_own_contract(params):
    rep = run_ref("protocol_profile", "etsi_its_g5", params)
    assert rep.ok, rep.to_text()
    assert rep.count("FAIL") == 0 and rep.count("ERROR") == 0


@pytest.mark.parametrize("ref", ["ma_report_v1", "ts103759_shape"])
def test_the_builtin_report_formats_pass_their_contract(ref):
    rep = run_ref("report_format", ref)
    assert rep.ok, rep.to_text()
    assert rep.count("PASS") == len(rep.rows)


def test_the_contract_refuses_a_profile_that_declares_a_layer_it_does_not_supply():
    """A profile claiming `generation` and returning None would make the engine silently keep its own
    cadence while the manifest recorded a generation rule that never ran."""
    class Liar(PROF.EtsiItsG5Profile):
        def capabilities(self):
            return frozenset(super().capabilities() | {AP.CAP_GENERATION})

    REG.register_builtin("protocol_profile", "_test_liar", Liar)
    try:
        rep = run_ref("protocol_profile", "_test_liar")
        assert not rep.ok
        failed = [r["check"] for r in rep.rows if r["status"] == "FAIL"]
        assert "P1_constructs_and_declares" in failed, rep.to_text()
    finally:
        REG._BUILTINS["protocol_profile"].pop("_test_liar", None)


def test_the_contract_refuses_a_dishonest_standards_claim():
    class Boastful(PROF.EtsiItsG5Profile):
        def standards_claim(self):
            return {"profile": "ETSI ITS-G5, conformance-tested and certified at Plugtests"}

    REG.register_builtin("protocol_profile", "_test_boastful", Boastful)
    try:
        rep = run_ref("protocol_profile", "_test_boastful")
        failed = [r["check"] for r in rep.rows if r["status"] == "FAIL"]
        assert "P1_constructs_and_declares" in failed, rep.to_text()
    finally:
        REG._BUILTINS["protocol_profile"].pop("_test_boastful", None)


def test_the_conformance_cli_reaches_the_new_slots():
    env = dict(os.environ)
    src_root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(RM.__file__))))
    env["PYTHONPATH"] = os.pathsep.join(
        [src_root] + [p for p in (env.get("PYTHONPATH", "").split(os.pathsep)) if p])
    for slot, ref in (("protocol_profile", "etsi_its_g5"), ("report_format", "ma_report_v1")):
        proc = subprocess.run(
            [sys.executable, "-m", "scms_sim_ref.mock_pipeline.run", "conformance",
             "--slot", slot, "--ref", ref],
            capture_output=True, text=True, env=env, timeout=300)
        assert proc.returncode == 0, proc.stdout + proc.stderr
        assert "0 failed" in proc.stdout


# =========================================================================== #
# 6. THE THIRD-PARTY DISTRIBUTION
# =========================================================================== #
@needs_cv2x
def test_the_third_party_stack_imports_only_the_public_api():
    """"No fork" is only true if the plugin's install closure is the API and nothing else.

    Asserted on the module SOURCE, because an import test would pass for a distribution that
    imported `scms_sim_ref.mock_pipeline` lazily inside a method -- which is exactly the shape a
    creeping engine dependency takes.
    """
    import scms_cv2x_profile as pkg
    root = os.path.dirname(os.path.abspath(pkg.__file__))
    offenders = {}
    for name in sorted(os.listdir(root)):
        if not name.endswith(".py"):
            continue
        src = open(os.path.join(root, name), encoding="utf-8").read()
        bad = [line.strip() for line in src.splitlines()
               if "scms_sim_ref" in line and line.strip().startswith(("import ", "from "))
               and "scms_sim_ref.api" not in line]
        if bad:
            offenders[name] = bad
    assert not offenders, f"the third-party stack reaches past the public api: {offenders}"


@needs_cv2x
def test_the_third_party_stack_passes_the_contract():
    rep = run_ref("protocol_profile", CV2X_PROFILE)
    assert rep.ok, rep.to_text()
    assert rep.count("FAIL") == 0 and rep.count("SKIP") == 0, rep.to_text()
    rep = run_ref("report_format", CV2X_FORMAT)
    assert rep.ok, rep.to_text()


@needs_cv2x
def test_the_third_party_stack_runs_with_zero_engine_edits(tmp_path):
    """It is activated ENTIRELY by a config declaration -- no engine flag, no enum value, no import
    anywhere in the engine, and no edit to `run.py`, `featurize.py` or any test file."""
    cfg = PipelineConfig(out_dir=str(tmp_path / "c"), message_codec="native_v1",
                         plugins={"protocol_profile": {"ref": CV2X_PROFILE}}, **_DEFAULT)
    validate_config(cfg)
    res = run_pipeline(cfg)
    assert res.n_reports > 0
    p = _protocol(tmp_path / "c")
    # ITS own words, in ITS own manifest block -- not a DCC state table it does not have.
    assert p["sps_generation"]["scheme"] == "c-v2x semi-persistent scheduling"
    assert "dcc" not in p and "cam_generation" not in p
    assert p["limeric"]["control_law"].startswith("delta(n)")
    assert p["wire"]["subframes_per_frame"] >= 1
    lock = [e for e in _manifest(tmp_path / "c")["plugins"]["loaded"]
            if e["slot"] == "protocol_profile"][0]
    assert lock["ref"] == CV2X_PROFILE and lock["resolved_via"] == "dotted_path"
    # The provenance lock's strongest identity, present because this really is an installed
    # distribution rather than a file on sys.path.
    assert lock["distribution"] == "scms-cv2x-profile" and lock["version"]
    assert lock["dist_sha256"] and lock["package_sha256"]
    assert _manifest(tmp_path / "c")["standards_profile"]["protocol_profile"]["distribution"]


@needs_cv2x
def test_the_third_party_stack_measurably_changes_behaviour(tmp_path):
    """Rate, CBR, delivered frames and latency -- all four move, and each for a stated reason."""
    base = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), message_codec="native_v1",
                                       net_latency_model=True, **_DEFAULT))
    cv2x = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "c"), message_codec="native_v1",
                                       plugins={"protocol_profile": {"ref": CV2X_PROFILE}},
                                       **_DEFAULT))
    assert base.data_digest != cv2x.data_digest
    pb, pc = _protocol(tmp_path / "b"), _protocol(tmp_path / "c")
    # RATE: SPS keeps a 2 s reservation where the engine's default cadence is one message a step.
    assert pc["sps_generation"]["mean_rate_hz"] < 0.75
    # FRAMES: fewer messages on the air.
    assert pc["wire"]["pdus"]["cam"] < 0.75 * pb["wire"]["pdus"]["cam"]
    # CBR: HIGHER despite fewer frames, because a PC5 subframe costs more than an OFDM burst.
    assert pc["wire"]["mean_frame_airtime_us"] > 2.0 * pb["wire"]["mean_frame_airtime_us"]
    assert pc["cbr"]["mean"] > pb["cbr"]["mean"]
    # LATENCY: the Mode 4 selection window dominates, and it is tens of milliseconds.
    assert pc["latency_ms"]["mean"] > 8.0 * pb["latency_ms"]["mean"]
    # DELIVERY: fewer frames on the air means fewer delivered frames to score.
    assert pc["latency_ms"]["samples"] < 0.75 * pb["latency_ms"]["samples"]


@needs_cv2x
def test_the_third_party_report_format_runs_and_carries_real_evidence(tmp_path):
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "r"), message_codec="native_v1",
                                plugins={"report_format": {"ref": CV2X_FORMAT}}, **_DEFAULT))
    rows = _rows(tmp_path / "r")
    assert rows
    assert all(k in rows[0] for k in ARP.REQUIRED_ROW_KEYS)
    assert "detected_ms" in rows[0] and isinstance(rows[0]["detected_ms"], int)
    assert not any(k.startswith("detnorm_") for k in rows[0]), \
        "the compact format drops the private score vector; that is the point of it"
    assert any(r["v2x_pdu_present"] for r in rows)


# =========================================================================== #
# 7. THE SUITE HAS POWER -- every bad plugin is refused, on its named check
# =========================================================================== #
@needs_cv2x
@pytest.mark.parametrize("name", sorted(MUST_FAIL) if MUST_FAIL else ["-"])
def test_conformance_refuses_every_bad_plugin(name):
    """**The whole value of a gate is that it can say no.**

    `scms_cv2x_profile.bad.MUST_FAIL` maps each deliberately broken plugin to the check that is
    supposed to catch it, and this reads that mapping rather than a second copy of it -- so adding a
    trap without a check to catch it is immediately a failing test rather than a quiet gap.
    """
    slot, expected = MUST_FAIL[name]
    rep = run_ref(slot, f"scms_cv2x_profile.bad:{name}")
    assert not rep.ok, f"{name} PASSED conformance -- the gate is powerless\n{rep.to_text()}"
    failed = {r["check"] for r in rep.rows if r["status"] in ("FAIL", "ERROR")}
    missing = [c for c in expected if c not in failed]
    assert not missing, (f"{name} was refused, but NOT by {missing} -- it must fail the check it "
                         f"was built to fail\n{rep.to_text()}")


@needs_cv2x
def test_the_oracle_sniffing_format_passes_the_name_check_and_fails_the_value_check():
    """Why `R3` exists as a separate check from `R2`.

    `OracleSniffingReportFormat` writes the answer key into `duplicate_flag` -- a legitimate report
    field. No key name is forbidden, so a name-based linter (and `R2`) sees nothing at all. Only
    rendering the same declared input twice, once with ground truth attached, catches it.
    """
    rep = run_ref("report_format", "scms_cv2x_profile.bad:OracleSniffingReportFormat")
    by_id = {r["check"]: r["status"] for r in rep.rows}
    assert by_id["R2_row_carries_no_oracle_field"] == "PASS"
    assert by_id["R3_oracle_input_does_not_change_the_row"] == "FAIL"


@needs_cv2x
def test_the_engine_refuses_a_bad_stack_when_conformance_is_required(tmp_path):
    """End to end, through the ENGINE, with attestation run out of process.

    The suite passing in-process is a measurement; this is the enforcement. `conformance:
    "required"` attests the candidate in a CHILD interpreter -- so a hostile `__init__` cannot edit
    the process that judges it -- and a failing verdict is a refusal BEFORE step 0, with no output
    directory written.
    """
    cfg = PipelineConfig(
        out_dir=str(tmp_path / "x"), message_codec="native_v1",
        plugins={"protocol_profile": {"ref": "scms_cv2x_profile.bad:CongestionAmplifierProfile",
                                      "conformance": "required"}}, **_DEFAULT)
    with pytest.raises(ConfigError) as e:
        run_pipeline(cfg)
    assert "does not conform" in str(e.value)
    assert "P10_congestion_never_speeds_up_under_load" in str(e.value)
    assert not os.path.exists(os.path.join(str(tmp_path / "x"), "ma", "ma_reports.jsonl"))


@needs_cv2x
def test_a_conformant_third_party_stack_is_attested_into_the_manifest(tmp_path):
    """The other half of the same mechanism: a PASSING attestation lands in the lock, which is what
    turns "this dataset was produced by a conformant stack" into a property of the artifact."""
    res = run_pipeline(PipelineConfig(
        out_dir=str(tmp_path / "y"), message_codec="native_v1",
        plugins={"protocol_profile": {"ref": CV2X_PROFILE, "conformance": "required"}},
        **_DEFAULT))
    assert res.n_reports > 0
    lock = [e for e in _manifest(tmp_path / "y")["plugins"]["loaded"]
            if e["slot"] == "protocol_profile"][0]
    assert lock["conformance"]["ok"] is True
    assert lock["conformance"]["suite"] == "v1"
    assert lock["conformance"]["attested_out_of_process"] is True
    assert lock["conformance"]["failed"] == 0


# =========================================================================== #
# 8. THE TRUST MODEL IS RECORDED, AND IT IS THE ONE THE OTHER SLOTS RECORD
# =========================================================================== #
@pytest.mark.parametrize("mod", [AP, ARP])
def test_the_trust_model_is_stated_in_the_module_that_publishes_the_seam(mod):
    """Consistent with `api/__init__` section 10 and the existing plugin docs: what is ENFORCED,
    what is CONVENTION, and what needs the isolated mode. A seam whose limits are only in a design
    document is a seam whose limits nobody reads."""
    doc = (mod.__doc__ or "")
    for phrase in ("ENFORCED", "CONVENTION", "NOT ENFORCEABLE", "capability by omission",
                   "out of process"):
        assert phrase.lower() in doc.lower(), (mod.__name__, phrase)
    # The honest sentence, in as many words.
    assert "not sandboxed" in doc.lower()


def test_the_new_slots_reserve_no_capability():
    """Reserved capabilities exist so a built-in can keep a pinned digest it is grandfathered
    against (`legacy_global_rng`). Every profile and every format is off by default, so there is
    nothing to grandfather -- and a slot that reserved capabilities it did not need would be
    refusing third parties for no reason."""
    assert AP.RESERVED_CAPABILITIES == frozenset()
    assert ARP.RESERVED_CAPABILITIES == frozenset()
    known, reserved = REG.CAPABILITIES["protocol_profile"]
    assert known == AP.KNOWN_CAPABILITIES and reserved == frozenset()


def test_an_unknown_capability_is_refused_from_a_third_party():
    class Greedy(PROF.EtsiItsG5Profile):
        def capabilities(self):
            return frozenset({"oracle_access"})

    with pytest.raises(ApiError) as e:
        REG.check_capabilities("protocol_profile", "x.y:Greedy", Greedy, "dotted_path",
                               Greedy(params={}, env={}).capabilities())
    assert "unknown capability" in str(e.value)


def test_the_generation_input_carries_no_ground_truth():
    """The firewall, asserted by name against the leakage registry -- the same assertion `Claim` and
    `Observation` carry, on the DTO a transmit decision is made from."""
    for field in AP.GenerationInput.__slots__:
        assert not is_forbidden_feature_key(field), field
    assert not (set(AP.GenerationInput.__slots__) & FORBIDDEN_FEATURE_KEYS)
    for field in ARP.ReportInput.__slots__:
        assert not is_forbidden_feature_key(field), field
