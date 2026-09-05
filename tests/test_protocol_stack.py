"""The real protocol stack: a real PDU on a modelled access layer, under the real ETSI rules.

Five opt-ins, each inert at its default, each graded here against the AUDITED refdata copies of the
constants (`datagen/refdata/{etsi_cam_dcc,phy_80211p_profile,v2x_awareness}.json`) rather than
against a second transcription of the same numbers -- which is what makes these tests
non-tautological:

  1. `message_codec`        every CAM/DENM/VAM is real octets, and the LENGTH drives the channel
  2. `cam_generation_rules` ETSI EN 302 637-2 clause 6.1.3 triggering
  3. `dcc`                  ETSI TS 102 687 reactive DCC over the measured CBR
  4. `net_latency_model`    per-packet propagation + access + stack latency
  5. `security_model`       butterfly provisioning, real ECDSA, `sig_ok` as a verification RESULT

THE TWO HARD CONSTRAINTS, asserted here and not merely intended: both pinned digests are
byte-identical with everything off, and `asn1tools` stays an OPTIONAL extra that the engine imports
and runs without.
"""
import json
import math
import os
import subprocess
import sys

import pytest

from scms_sim_ref.api.codec import Claim, StationView
from scms_sim_ref.codecs import etsi_rules as ER
from scms_sim_ref.mock_pipeline import PipelineConfig, config_schema, run_pipeline, validate_config
from scms_sim_ref.mock_pipeline import run as RM
from scms_sim_ref.mock_pipeline.run import main
from scms_sim_ref.schemas.records import FORBIDDEN_FEATURE_KEYS

REFDIR = os.path.join(os.path.dirname(os.path.abspath(RM.__file__)), "..", "datagen", "refdata")

#: The two pinned digests this whole workstream is held to.
REFERENCE_DIGEST = "b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815"
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"

_DEFAULT = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
                grid_w=5, grid_h=5, attacker_pct=0.25)
#: A sub-second step, which is the only regime where EN 302 637-2 can fire faster than its own
#: heart-beat, hence the only one where the generation rules mean anything.
_FAST = dict(_DEFAULT, duration_s=20, dt=0.1)

#: The Java engine's own measurement of the same rules on InTAS, quoted so the Python side is
#: compared against something rather than against itself. Source: the MOSAIC-side `ScmsBeaconApp`
#: run at 100 ms sync -- 0.3399 s mean inter-CAM gap, 2.942 Hz, 89.6 % dynamics-triggered.
JAVA_CAM = {"mean_gap_s": 0.3399, "rate_hz": 2.942, "dynamics_share": 0.896}


def _ref(name):
    with open(os.path.normpath(os.path.join(REFDIR, name)), encoding="utf-8") as fh:
        return json.load(fh)["entries"]


def _has_asn1tools() -> bool:
    try:
        import asn1tools                                            # noqa: F401
        return True
    except ImportError:
        return False


needs_asn1 = pytest.mark.skipif(not _has_asn1tools(),
                                reason="optional extra `asn1tools` is not installed")


def _manifest(d):
    with open(os.path.join(str(d), "manifest.json"), encoding="utf-8") as fh:
        return json.load(fh)


def _protocol(d):
    return _manifest(d)["counts"].get("protocol", {})


# ================================================================================================
# 1. The access-layer arithmetic, graded against refdata rather than against itself
# ================================================================================================
def test_ppdu_airtime_reproduces_every_refdata_row():
    """`phy_80211p_profile.frame_airtime_us` has five rows of [MPDU, bits, symbols, us]. All five
    must come out of the OFDM timing, exactly -- including the symbol-boundary ceil that makes a
    300 B frame 448 us and not the ROADMAP's round 0.4 ms."""
    for mpdu, bits, symbols, us in _ref("phy_80211p_profile.json")["frame_airtime_us"]["points"]:
        m = int(mpdu)
        assert ER.ppdu_bits(m) == bits, m
        assert ER.ppdu_symbols(m) == symbols, m
        assert round(ER.ppdu_airtime_s(m) * 1e6, 1) == us, m


def test_mac_overhead_matches_refdata():
    v = _ref("phy_80211p_profile.json")["mac_overhead_us"]["value"]
    assert ER.SLOT_US == v["slot_us"]
    assert ER.SIFS_US == v["sifs_us"]
    assert ER.DIFS_US == v["difs_us"]
    assert ER.AIFS_AC_BE_US == v["aifs_ac_be_us"]
    assert ER.MEAN_BACKOFF_US == v["mean_backoff_us"]
    assert ER.MAC_OVERHEAD_US == v["total_added_us"]


def test_airtime_settles_the_two_engines_disagreement():
    """The engines split 2.05x on one CAM's airtime and BOTH readings are pinned in the same file.

    Neither was measured. A real UPER CAM is 41 octets and the measured TS 103 097 digest envelope
    is 93, so the frame is 134 B and its airtime is 431.5 us with AIFS+backoff counted. This test
    pins all three numbers together so the comparison cannot quietly drift.
    """
    assert round(ER.ppdu_airtime_s(300) * 1e6, 1) == 448.0            # the Java constant
    assert round(ER.frame_airtime_s(500) * 1e6, 1) == 919.5           # the Python constant
    assert round(RM.PHY_FRAME_AIRTIME_S * 1e6, 1) == 919.5            # ...and it IS the engine's
    assert round(ER.frame_airtime_s(41 + 93) * 1e6, 1) == 431.5       # the measured frame
    # The engines' split, and the real number's position inside it.
    assert 2.05 < 919.5 / 448.0 < 2.06


def test_dcc_state_table_matches_refdata_including_the_half_open_bands():
    rows = _ref("etsi_cam_dcc.json")["dcc_reactive_states"]["points"]
    assert len(rows) == len(ER.DCC_REACTIVE_STATES)
    for (name, lo, hi, rate, t_off_ms), mine in zip(rows, ER.DCC_REACTIVE_STATES):
        assert mine[0] == name
        assert mine[1] == lo
        assert mine[3] == rate
        assert round(mine[4] * 1000.0, 6) == t_off_ms
    # The derivation field's rule: bounds are half-open, so a breakpoint selects the HIGHER state.
    assert ER.dcc_state_for(0.29)[0] == "relaxed"
    assert ER.dcc_state_for(0.30)[0] == "active_1"
    assert ER.dcc_state_for(0.60)[0] == "restrictive"
    assert ER.dcc_state_for(0.0)[0] == ER.DCC_DEFAULT_STATE


def test_cam_thresholds_and_bounds_match_refdata():
    e = _ref("etsi_cam_dcc.json")
    thr = e["cam_trigger_thresholds"]["value"]
    assert ER.CAM_TRIGGER_POSITION_M == thr["position_m"]
    assert ER.CAM_TRIGGER_HEADING_DEG == thr["heading_deg"]
    assert ER.CAM_TRIGGER_SPEED_MPS == thr["speed_mps"]
    assert [ER.T_GEN_CAM_MIN_S, ER.T_GEN_CAM_MAX_S] == e["cam_interval_s"]["range"]


def test_latency_stack_constant_is_derived_from_the_anchor_not_chosen():
    """`STACK_LATENCY_S` must be exactly the DLR 5 ms anchor at 200 B minus that frame's air time.

    If someone retunes it by hand this fails, which is the point: the constant is a subtraction, not
    a taste.
    """
    band = _ref("v2x_awareness.json")["latency_p50_ms_80211p"]["range"]
    assert list(ER.LATENCY_REFERENCE_BAND_S) == [band[0] / 1e3, band[1] / 1e3]
    derived = band[0] / 1e3 - ER.frame_airtime_s(200)
    assert abs(ER.STACK_LATENCY_S - derived) < 1e-9
    # ...and therefore a 200 B frame on an idle channel lands exactly on the anchor.
    assert abs(ER.link_latency_s(0.0, 0.0, 200) - band[0] / 1e3) < 1e-12


def test_access_delay_is_inert_on_an_idle_channel_and_grows_with_load():
    idle = ER.access_delay_s(0.0, 134)
    assert abs(idle - (ER.AIFS_AC_BE_US + ER.MEAN_BACKOFF_US) * 1e-6
               - ER.ppdu_airtime_s(134)) < 1e-15
    assert ER.access_delay_s(0.5, 134) > idle
    assert ER.access_delay_s(0.95, 134) > ER.access_delay_s(0.5, 134)
    assert math.isfinite(ER.access_delay_s(1.0, 134))          # the 0.95 cap keeps it finite


def test_angle_delta_is_convention_free():
    assert ER.angle_delta_deg(359.0, 1.0) == pytest.approx(2.0)
    assert ER.angle_delta_deg(10.0, 350.0) == pytest.approx(20.0)
    assert ER.angle_delta_deg(0.0, 180.0) == pytest.approx(180.0)


# ================================================================================================
# 2. THE HARD CONSTRAINT: everything off is byte-identical
# ================================================================================================
def test_defaults_are_all_inert():
    c = PipelineConfig()
    assert c.message_codec == ""
    assert c.cam_generation_rules is False
    assert c.dcc is False
    assert c.net_latency_model is False
    assert c.security_model == "none"
    assert c.ma_backhaul_s == 0.0


def test_default_golden_is_untouched_by_the_protocol_stack(tmp_path):
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN



def test_reference_digest_is_untouched_by_the_protocol_stack(tmp_path):
    res = run_pipeline(PipelineConfig(
        seed=42, traffic_flow=True, road_network="grid", grid_w=6, grid_h=6, duration_s=300,
        arrival_rate=2.0, attacker_pct=0.15, traffic_lights=True, out_dir=str(tmp_path / "r")))
    assert res.data_digest == REFERENCE_DIGEST


def test_no_protocol_block_in_a_default_manifest(tmp_path):
    run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), **_DEFAULT))
    assert "protocol" not in _manifest(tmp_path / "d")["counts"]


def test_every_new_field_is_self_describing(tmp_path):
    sch = config_schema()
    for name in ("message_codec", "message_signer", "cam_generation_rules", "dcc",
                 "net_latency_model", "ma_backhaul_s", "security_model"):
        assert name in sch, name
        assert sch[name]["group"] == "Protocol", (name, sch[name]["group"])
        assert sch[name]["help"], name
    assert sch["message_codec"]["options"][0] == ""
    assert "etsi_cam_en302637_2" in sch["message_codec"]["options"]


def test_new_cli_flags_parse_and_wire_through(tmp_path):
    cfg_path = tmp_path / "eff.json"
    argv = ["--steps", "2", "--vehicles", "12", "--out", str(tmp_path / "cli"),
            "--dump-config", str(cfg_path),
            "--message-codec", "native_v1", "--message-signer", "certificate",
            "--net-latency", "--ma-backhaul", "0.25", "--security-model", "none"]
    assert main(argv) == 0
    eff = json.loads(cfg_path.read_text(encoding="utf-8"))
    assert eff["message_codec"] == "native_v1"
    assert eff["message_signer"] == "certificate"
    assert eff["net_latency_model"] is True
    assert eff["ma_backhaul_s"] == 0.25


# ================================================================================================
# 3. The codec on the wire, and the size driving the channel
# ================================================================================================
@needs_asn1
def test_real_uper_octets_reach_the_wire(tmp_path):
    """The engine must put REAL octets on the air, and the manifest must say how many.

    41 is not a magic number: it is what `EtsiCamCodec` produces for this engine's CAM, and the
    test re-derives it from the codec rather than hard-coding it, so a codec change fails here
    instead of silently redefining the airtime.
    """
    from scms_sim_ref.codecs import EtsiCamCodec, SECURITY_ENVELOPE_BYTES
    codec = EtsiCamCodec()
    expect = len(codec.encode_cam(Claim(1, "", "cam", 0.0, 10.0, 20.0, 12.0, 45.0, 1.5),
                                  StationView()))
    d = tmp_path / "w"
    run_pipeline(PipelineConfig(out_dir=str(d), message_codec="etsi_cam_en302637_2", **_DEFAULT))
    w = _protocol(d)["wire"]
    assert w["profile"] == "etsi_cam_en302637_2"
    assert w["mean_payload_bytes"]["cam"] == float(expect)
    assert w["mean_wire_bytes"] == expect + SECURITY_ENVELOPE_BYTES["digest"]
    assert w["mean_frame_airtime_us"] == round(
        ER.frame_airtime_s(expect + SECURITY_ENVELOPE_BYTES["digest"]) * 1e6, 3)


@needs_asn1
def test_pdu_size_drives_cbr_and_the_dataset(tmp_path):
    """Not a manifest field: a different wire format must change the CHANNEL.

    `native_v1` reproduces the legacy 300 B assumption on purpose; the ETSI profile carries the
    measured 134 B frame. CBR must fall by the airtime ratio, and the produced dataset must
    actually differ -- otherwise the size is decorative.
    """
    dense = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=15, dt=0.1,
                 arrival_rate=6.0, grid_w=3, grid_h=3, attacker_pct=0.15,
                 radio_model="geometric", radio_range_m=500.0, cam_generation_rules=True)
    out = {}
    for name in ("native_v1", "etsi_cam_en302637_2"):
        d = tmp_path / name
        res = run_pipeline(PipelineConfig(out_dir=str(d), message_codec=name, **dense))
        out[name] = (_protocol(d), res.data_digest)
    nat, etsi = out["native_v1"][0], out["etsi_cam_en302637_2"][0]
    assert nat["wire"]["mean_wire_bytes"] == 300.0
    assert etsi["wire"]["mean_wire_bytes"] < nat["wire"]["mean_wire_bytes"]
    assert etsi["cbr"]["mean"] < nat["cbr"]["mean"]
    ratio = etsi["cbr"]["mean"] / nat["cbr"]["mean"]
    expected = etsi["wire"]["mean_frame_airtime_us"] / nat["wire"]["mean_frame_airtime_us"]
    # CBR is a linear functional of airtime for a fixed offered frame set; the two runs' frame
    # sets differ slightly (the channel diverges), so this is a band, not an equality.
    assert abs(ratio - expected) < 0.05, (ratio, expected)
    assert out["native_v1"][1] != out["etsi_cam_en302637_2"][1], \
        "a different wire size produced an identical dataset -- the size is not driving anything"


@needs_asn1
def test_codec_encodes_denm_and_vam_for_the_right_stations(tmp_path):
    """A DENM is a DENM and a VRU-declared station sends a VAM, not a CAM."""
    d = tmp_path / "mix"
    run_pipeline(PipelineConfig(
        out_dir=str(d), message_codec="etsi_cam_en302637_2", vru_pct=0.3, denm_rate=40.0,
        **dict(_DEFAULT, attacker_pct=0.2)))
    pdus = _protocol(d)["wire"]["pdus"]
    assert pdus.get("cam", 0) > 0 and pdus.get("vam", 0) > 0 and pdus.get("denm", 0) > 0
    means = _protocol(d)["wire"]["mean_payload_bytes"]
    # The three PDU types are genuinely different encodings, not one encoder relabelled.
    assert means["cam"] != means["vam"] and means["cam"] != means["denm"]


def test_wire_encoder_claim_carries_no_oracle_field():
    """The firewall, at the one site where engine state becomes octets a report could carry."""
    b = dict(veh=None, digest="0123456789abcdef", cx=1.0, cy=2.0, cs=3.0, ch=4.0, conf=1.5,
             ghost=True, x=99.0, y=98.0, falsified=True, msg_count=1, cg=0.5, sig_ok=True,
             cvf=0.0, cvt=10.0, station_type="vehicle", tspd=42.0, thdg=17.0)
    claim = RM.WireEncoder.__dict__["claim_for"](
        RM.WireEncoder.__new__(RM.WireEncoder), b, "cam")
    for f in claim.__slots__:
        assert f not in FORBIDDEN_FEATURE_KEYS, f
    assert claim.x == 1.0 and claim.y == 2.0            # the CLAIM, never b["x"]/b["y"]


def test_station_id_is_derived_from_the_pseudonym_not_from_the_vehicle():
    a = RM.WireEncoder.station_id("0123456789abcdef")
    b = RM.WireEncoder.station_id("fedcba9876543210")
    assert a != b and 0 <= a <= 0xFFFFFFFF and 0 <= b <= 0xFFFFFFFF
    assert a == 0x01234567


# ================================================================================================
# 4. EN 302 637-2 CAM generation
# ================================================================================================
def test_cam_state_machine_honours_t_gen_cam_min():
    st = ER.CamGenerationState()
    assert st.evaluate(0.0, 0.0, 0.0, 10.0, 0.0) == ER.TRIGGER_FIRST
    # 50 ms later, and 100 m away: the dynamics condition holds, the timer does not.
    assert st.evaluate(0.05, 100.0, 0.0, 10.0, 0.0) == ER.TRIGGER_NONE
    assert st.evaluate(0.10, 100.0, 0.0, 10.0, 0.0) == ER.TRIGGER_POSITION


def test_cam_state_machine_heartbeat_at_t_gen_cam_max():
    st = ER.CamGenerationState()
    st.evaluate(0.0, 0.0, 0.0, 0.0, 0.0)
    t = 0.0
    fired = []
    for _ in range(30):                          # a parked station: nothing moves at all
        t = round(t + 0.1, 6)
        r = st.evaluate(t, 0.0, 0.0, 0.0, 0.0)
        if r:
            fired.append((t, r))
    assert [r for _, r in fired] == [ER.TRIGGER_HEARTBEAT] * len(fired)
    assert [round(t, 3) for t, _ in fired] == [1.0, 2.0, 3.0]


def test_cam_state_machine_each_dynamics_trigger_fires_on_its_own_field():
    for kw, want in ((dict(x=5.0), ER.TRIGGER_POSITION),
                     (dict(heading=5.0), ER.TRIGGER_HEADING),
                     (dict(speed=0.6), ER.TRIGGER_SPEED)):
        st = ER.CamGenerationState()
        st.evaluate(0.0, 0.0, 0.0, 0.0, 0.0)
        got = st.evaluate(0.2, kw.get("x", 0.0), 0.0, kw.get("speed", 0.0),
                          kw.get("heading", 0.0))
        assert got == want, (kw, got)
    # ...and NOT just below each threshold.
    st = ER.CamGenerationState()
    st.evaluate(0.0, 0.0, 0.0, 0.0, 0.0)
    assert st.evaluate(0.2, 3.9, 0.0, 0.4, 3.9) == ER.TRIGGER_NONE


def test_cam_rules_are_a_no_op_at_the_default_step(tmp_path):
    """dt = 1.0 IS T_GenCamMax, so every step is a heart-beat and the rules cannot bind. The
    dataset must therefore be byte-identical to the run without them -- which is what makes the
    `dt <= 1.0` validation an honest gate rather than an arbitrary one."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **_DEFAULT))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), cam_generation_rules=True,
                                    **_DEFAULT))
    assert a.data_digest == b.data_digest == DEFAULT_GOLDEN
    # ...and the reason it is a no-op is that the rules fire on EVERY broadcasting vehicle-step:
    # one second of driving clears the 4 m position threshold and, failing that, IS the heart-beat.
    cam = _protocol(tmp_path / "b")["cam_generation"]
    surv = _manifest(tmp_path / "b")["counts"]["mobility_survivorship"]
    assert cam["cams"] == surv["vehicle_steps_broadcast"]
    assert cam["mean_gap_s"] == 1.0 and cam["max_gap_s"] == 1.0


def test_cam_rate_lands_in_the_etsi_band_and_near_the_java_reference(tmp_path):
    """The rate distribution, against BOTH the standard's band and the other engine's measurement.

    The Java side measured 0.3399 s / 2.942 Hz / 89.6 % dynamics on InTAS at 100 ms sync. The
    position trigger dominates, so the gap is essentially `4 m / v` -- the two engines must agree
    once their fleet speeds are accounted for, and the assertion is written as that relation rather
    than as a magic number.
    """
    d = tmp_path / "cam"
    run_pipeline(PipelineConfig(out_dir=str(d), cam_generation_rules=True, **_FAST))
    cam = _protocol(d)["cam_generation"]
    assert ER.T_GEN_CAM_MIN_S <= cam["mean_gap_s"] <= ER.T_GEN_CAM_MAX_S
    assert cam["max_gap_s"] <= ER.T_GEN_CAM_MAX_S + 1e-9
    assert 1.0 <= cam["mean_rate_hz"] <= 10.0            # the CAM service's own limits
    # Dynamics-dominated, like the Java measurement (89.6 %).
    assert cam["dynamics_share"] > 0.80
    # Within 40 % of the Java mean gap: the residual is the fleet's mean speed, and the two maps
    # are different cities. A wider miss means the RULES differ, not the traffic.
    assert 0.6 < cam["mean_gap_s"] / JAVA_CAM["mean_gap_s"] < 1.4, cam["mean_gap_s"]
    assert cam["triggers"]["position"] > cam["triggers"].get("heartbeat", 0)


def test_faster_traffic_produces_a_shorter_gap(tmp_path):
    """The mechanism, not just the number: the gap is 4 m / speed, so raising the fleet speed must
    shorten it. This is what makes the Java comparison an explanation rather than a coincidence."""
    slow = tmp_path / "slow"
    fast = tmp_path / "fast"
    run_pipeline(PipelineConfig(out_dir=str(slow), cam_generation_rules=True, **_FAST))
    run_pipeline(PipelineConfig(out_dir=str(fast), cam_generation_rules=True,
                                **dict(_FAST, trip_speed_min=11.0, trip_speed_max=20.0)))
    g_slow = _protocol(slow)["cam_generation"]["mean_gap_s"]
    g_fast = _protocol(fast)["cam_generation"]["mean_gap_s"]
    assert g_fast < g_slow, (g_fast, g_slow)


# ================================================================================================
# 5. TS 102 687 reactive DCC
# ================================================================================================
def test_dcc_requires_the_cam_service():
    with pytest.raises(ValueError, match="dcc"):
        validate_config(PipelineConfig(dcc=True))


def test_dcc_does_nothing_at_low_density(tmp_path):
    """The property that matters most: implemented correctly, it correctly does NOTHING below the
    0.30 breakpoint. Byte-identical output, and every station-step in `relaxed`."""
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "off"),
                                    cam_generation_rules=True, **_FAST))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "on"),
                                    cam_generation_rules=True, dcc=True, **_FAST))
    assert a.data_digest == b.data_digest
    p = _protocol(tmp_path / "on")
    assert p["cbr"]["max"] < ER.DCC_REACTIVE_STATES[0][2]
    assert set(p["dcc"]["states"]) == {"relaxed"}



def test_dcc_engages_and_cuts_the_offered_load_at_density(tmp_path):
    dense = dict(seed=7, traffic_flow=True, road_network="grid", duration_s=20, dt=0.1,
                 arrival_rate=6.0, grid_w=3, grid_h=3, attacker_pct=0.5, attack_type="DoS",
                 dos_burst=12, radio_range_m=500.0, cam_generation_rules=True,
                 message_codec="etsi_cam_en302637_2", message_signer="certificate")
    off = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "hoff"), **dense))
    on = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "hon"), dcc=True, **dense))
    a, b = _protocol(tmp_path / "hoff"), _protocol(tmp_path / "hon")
    assert a["cbr"]["mean"] > 0.15, a["cbr"]
    assert off.data_digest != on.data_digest
    assert b["cam_generation"]["cams"] < a["cam_generation"]["cams"]
    assert b["cam_generation"]["mean_gap_s"] > a["cam_generation"]["mean_gap_s"]
    # It reached at least the first restrictive band, i.e. it really engaged.
    assert set(b["dcc"]["states"]) - {"relaxed"}


def test_reactive_dcc_entity_maps_cbr_to_the_cam_floor():
    d = ER.ReactiveDcc()
    assert d.state == "relaxed"
    assert d.t_gen_cam_floor == ER.T_GEN_CAM_MIN_S       # relaxed T_off IS T_GenCamMin
    d.update(0.35)
    assert d.state == "active_1" and d.t_gen_cam_floor == 0.2
    d.update(0.9)
    assert d.state == "restrictive" and d.t_gen_cam_floor == ER.T_GEN_CAM_MAX_S


# ================================================================================================
# 6. Per-packet latency
# ================================================================================================
@needs_asn1
def test_latency_distribution_against_the_80211p_profile(tmp_path):
    d = tmp_path / "lat"
    run_pipeline(PipelineConfig(out_dir=str(d), net_latency_model=True,
                                message_codec="etsi_cam_en302637_2",
                                radio_model="geometric", **_DEFAULT))
    lat = _protocol(d)["latency_ms"]
    assert lat["samples"] > 100
    # Every delivered frame is a 134 B CAM, so the whole distribution must sit just under the
    # DLR band's 5 ms floor -- the anchor is 5 ms at 200 B and this frame is smaller.
    assert 4.0 < lat["min"] <= lat["p50"] <= lat["max"] < 6.0, lat
    assert lat["p50"] < lat["reference_band_ms"][0]
    assert lat["max"] - lat["min"] > 0.0                 # distance and load really do move it
    # Ordering sanity on the quantiles.
    assert lat["min"] <= lat["p50"] <= lat["p90"] <= lat["p99"] <= lat["max"]


def test_latency_model_draws_no_random_number(tmp_path):
    """Deterministic by construction: two runs of the same config are byte-identical, and the
    latency figures are reproduced exactly."""
    kw = dict(net_latency_model=True, radio_model="geometric", **_DEFAULT)
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **kw))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), **kw))
    assert a.data_digest == b.data_digest
    assert _protocol(tmp_path / "a")["latency_ms"] == _protocol(tmp_path / "b")["latency_ms"]


def test_latency_replaces_the_uniform_ingest_draw(tmp_path):
    """`ingest_time - detection_time` becomes the DECLARED backhaul, exactly, instead of a
    uniform [0, net_delay_max) draw. That is the substance of 'replacing the ingest delay'."""
    d = tmp_path / "ing"
    run_pipeline(PipelineConfig(out_dir=str(d), net_latency_model=True, ma_backhaul_s=0.25,
                                radio_model="geometric", **_DEFAULT))
    rows = [json.loads(x) for x in
            open(os.path.join(str(d), "ma", "ma_reports.jsonl"), encoding="utf-8")]
    assert rows
    for r in rows[:200]:
        assert abs((r["ingest_time"] - r["detection_time"]) - 0.25) < 1e-3
        assert r["detection_time"] >= r["generation_time"]


# ================================================================================================
# 7. Real signatures
# ================================================================================================
def test_security_off_is_byte_identical(tmp_path):
    res = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "d"), security_model="none",
                                      **_DEFAULT))
    assert res.data_digest == DEFAULT_GOLDEN


def test_pseudonyms_become_butterfly_derived_and_unlinkable_to_the_pca(tmp_path):
    """The defect the audit named: `derive(f"key:{vid}:{k}")` gives the PCA a perfect
    device -> certificates map through one shared `request_hash`. Under the real scheme the PCA's
    ledger carries an opaque per-certificate token and NO device identifier at all."""
    from scms_sim_ref.scms_core.engine_security import SecurityLayer
    from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
    sec = SecurityLayer(PipelineConfig(seed=7).derive)
    ctx = DeviceLinkageContext(1, 2, b"\x01" * 16, b"\x02" * 16)
    a = sec.provision("veh_001", ctx, [(0, j, 0.0, 100.0) for j in range(4)])
    b = sec.provision("veh_002", ctx, [(0, j, 0.0, 100.0) for j in range(4)])
    assert len({c.digest for c in a.credentials} | {c.digest for c in b.credentials}) == 8
    ledger = sec.prov.pca.ledger
    assert len(ledger) == 8
    for row in ledger:
        assert "veh_001" not in json.dumps(row) and "veh_002" not in json.dumps(row)
        assert set(row) == {"arrival_index", "request_token", "i_cert", "j_index", "cocoon_x",
                            "certified_key", "linkage_value", "cert_digest", "valid_from",
                            "valid_to"}
    # Every request token is distinct -- the single shared `request_hash` is gone.
    assert len({r["request_token"] for r in ledger}) == 8
    # ...and the RA, and only the RA, can still resolve one (investigation must keep working).
    tok = bytes.fromhex(ledger[0]["request_token"])
    assert sec.prov.ra.resolve(tok) is not None


def test_sig_ok_is_the_result_of_a_verification(tmp_path):
    d = tmp_path / "inv"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa",
                                attack_type="InvalidSignature",
                                **dict(_DEFAULT, attacker_pct=0.4)))
    s = _protocol(d)["security"]
    assert s["signing_mode"] in ("rfc6979", "random_k")
    assert s["signatures_computed"] > 0
    assert s["logical_verifications"] > 0
    # The verdict vocabulary is eight-valued where the boolean was two-valued, and the FORGED
    # signature really fails an ECDSA check.
    assert s["verdicts"].get("signature_invalid", 0) > 0
    assert s["verdicts"].get("ok", 0) > 0


def test_revoked_certificates_are_caught_receiver_side(tmp_path):
    d = tmp_path / "rev"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa",
                                **dict(_DEFAULT, attacker_pct=0.4)))
    s = _protocol(d)["security"]
    # `cert_revoked` can only come from recomputing the CRL's linkage values against the
    # certificate -- there is no vid in a SignedMessage.
    assert s["verdicts"].get("cert_revoked", 0) > 0


@pytest.mark.parametrize("attack,verdict", [("ExpiredCert", "cert_expired"),
                                            ("NotYetValid", "cert_not_yet_valid")])
def test_credential_attacks_need_a_credential_the_attacker_really_holds(tmp_path, attack, verdict):
    """The honest form of the attack. WITHOUT rotation the attacker has no other certificate and
    genuinely cannot mount it -- and the engine records the refusal instead of falling back to
    editing a signed field. WITH rotation it can, and the receiver says so."""
    no_rot = tmp_path / "norot"
    rot = tmp_path / "rot"
    run_pipeline(PipelineConfig(out_dir=str(no_rot), security_model="ecdsa", attack_type=attack,
                                **dict(_DEFAULT, attacker_pct=0.4)))
    run_pipeline(PipelineConfig(out_dir=str(rot), security_model="ecdsa", attack_type=attack,
                                rotate_period_s=15.0, **dict(_DEFAULT, attacker_pct=0.4)))
    a, b = _protocol(no_rot)["security"], _protocol(rot)["security"]
    assert a["verdicts"].get(verdict, 0) == 0
    assert a["attack_refusals"], "an impossible attack must be RECORDED, not silently skipped"
    assert b["verdicts"].get(verdict, 0) > 0, b["verdicts"]


def test_replay_keeps_a_valid_signature(tmp_path):
    """A replay is a FRESHNESS failure, not a crypto failure. A scheme that reported it as an
    invalid signature would be wrong about what a signature does."""
    d = tmp_path / "rep"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa", attack_type="DataReplay",
                                **dict(_DEFAULT, attacker_pct=0.4)))
    s = _protocol(d)["security"]
    assert s["verdicts"].get("signature_invalid", 0) == 0
    assert s["verdicts"].get("ok", 0) > 0


def test_sybil_ghosts_carry_genuine_credentials(tmp_path):
    """A Sybil under a working SCMS is not forgery: it is a station running more of its OWN
    legitimate pseudonyms at once than it is entitled to. Every ghost must therefore verify."""
    d = tmp_path / "syb"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa", attack_type="Sybil",
                                **dict(_DEFAULT, attacker_pct=0.4)))
    s = _protocol(d)["security"]
    assert s["verdicts"].get("signature_invalid", 0) == 0
    assert s["certificates_issued"] > s["devices_provisioned"]


def test_forged_certificate_is_rejected_on_its_issuer():
    """The attack the boolean cannot express at all: a certificate that is internally perfect and
    that no trust store has ever heard of."""
    from scms_sim_ref.scms_core import secured as SEC
    from scms_sim_ref.scms_core.engine_security import SecurityLayer
    from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
    sec = SecurityLayer(PipelineConfig(seed=3).derive)
    ctx = DeviceLinkageContext(1, 2, b"\x03" * 16, b"\x04" * 16)
    creds = sec.provision("veh_009", ctx, [(0, 0, 0.0, 100.0)], attacker=True)
    sec.provision("veh_010", ctx, [(0, 1, 0.0, 100.0)])      # the graft victim
    dig = creds.credentials[0].digest
    good = sec.sign("veh_009", dig, b"payload", 1.0)
    assert sec.verify(good, 1.0).status == SEC.OK
    forged = sec.sign("veh_009", dig, b"payload", 1.0, attack="ForgedCertificate")
    assert sec.verify(forged, 1.0).status == SEC.UNKNOWN_ISSUER
    bad_sig = sec.sign("veh_009", dig, b"payload", 1.0, attack="InvalidSignature")
    assert sec.verify(bad_sig, 1.0).status == SEC.SIGNATURE_INVALID
    grafted = sec.sign("veh_009", dig, b"payload", 1.0, attack="CertificateGrafting")
    assert sec.verify(grafted, 1.0).status != SEC.OK


def test_signature_bytes_are_deterministic():
    """RFC 6979 nonces, or the run is not replayable once a signature reaches an output record."""
    from scms_sim_ref.scms_core.ecdsa_p256 import DETERMINISTIC
    from scms_sim_ref.scms_core.engine_security import SecurityLayer
    from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
    if not DETERMINISTIC:
        pytest.skip("this cryptography build has no RFC 6979 deterministic signing")
    ctx = DeviceLinkageContext(1, 2, b"\x05" * 16, b"\x06" * 16)
    sigs = []
    for _ in range(2):
        sec = SecurityLayer(PipelineConfig(seed=11).derive)
        c = sec.provision("veh_000", ctx, [(0, 0, 0.0, 100.0)])
        sigs.append(sec.sign("veh_000", c.credentials[0].digest,
                             b"same", 0.0).message.wire_octets())
    assert sigs[0] == sigs[1]


@needs_asn1
def test_airtime_uses_the_measured_coer_envelope_not_our_own_serialisation(tmp_path):
    """Real cryptography and measured airtime, with neither borrowing the other's error.

    `SignedMessage.wire_octets()` is this repository's own canonical serialisation and
    `certificate.py` says outright that it is NOT COER. Measured: the AT certificate comes out
    200 B against the 132 B a real COER encoder produces, so the certificate-attached frame is 335 B
    against 260 B -- a 28.8 % over-statement that would land straight in CBR. So the SECURITY layer
    decides which signer ARM a frame carries and the CODEC supplies that arm's measured SIZE.
    """
    from scms_sim_ref.codecs import SECURITY_ENVELOPE_BYTES
    from scms_sim_ref.scms_core.engine_security import SecurityLayer
    from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
    sec = SecurityLayer(PipelineConfig(seed=7).derive)
    ctx = DeviceLinkageContext(1, 2, b"\x01" * 16, b"\x02" * 16)
    c = sec.provision("veh_000", ctx, [(0, 0, 0.0, 1000.0)])
    dig = c.credentials[0].digest
    cert_frame = sec.sign("veh_000", dig, b"x" * 41, 0.0)          # attaches the certificate
    dgst_frame = sec.sign("veh_000", dig, b"x" * 41, 0.1)          # within 1 s -> digest arm
    assert cert_frame.signer_form == "certificate"
    assert dgst_frame.signer_form == "digest"
    # Our serialisation really is bigger, which is why the choice matters.
    assert cert_frame.wire_bytes > 41 + SECURITY_ENVELOPE_BYTES["certificate"]
    # ...and the engine charges the measured number instead.
    d = tmp_path / "env"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa",
                                message_codec="etsi_cam_en302637_2", **_DEFAULT))
    w = _protocol(d)["wire"]
    cam = w["mean_payload_bytes"]["cam"]
    lo = cam + SECURITY_ENVELOPE_BYTES["digest"]
    hi = cam + SECURITY_ENVELOPE_BYTES["certificate"]
    assert lo <= w["mean_wire_bytes"] <= hi, (lo, w["mean_wire_bytes"], hi)


@pytest.mark.parametrize("attack", ["DelayedMessages", "OutOfOrder", "DataReplay"])
def test_backdated_claims_stay_representable(tmp_path, attack):
    """The three attacks that put a timestamp BEFORE the run's start on the wire.

    `Time64` is UNSIGNED microseconds since the 2004 epoch, so a `time_base` of 0 -- "engine second
    zero IS the epoch" -- makes `cg = t - 6.0` unencodable and killed the run eight frames in with
    `OverflowError: can't convert negative int to unsigned`. `ENGINE_TIME_BASE` maps t = 0 onto
    2024-01-01 instead, which is both the codec's own pinned epoch and ~20 years of headroom.
    """
    from scms_sim_ref.scms_core.engine_security import ENGINE_TIME_BASE
    assert ENGINE_TIME_BASE > 6.0e8
    res = run_pipeline(PipelineConfig(
        out_dir=str(tmp_path / attack), security_model="ecdsa", attack_type=attack,
        message_codec="etsi_cam_en302637_2",
        **dict(_DEFAULT, duration_s=30, attacker_pct=0.5)))
    assert res.data_digest
    s = _protocol(tmp_path / attack)["security"]
    assert s["signatures_computed"] > 0


def test_time_base_zero_refuses_a_backdated_claim_by_name():
    """...and when it IS unrepresentable, the refusal names the fix instead of raising OverflowError."""
    from scms_sim_ref.scms_core.engine_security import SecurityLayer
    from scms_sim_ref.scms_core.linkage import DeviceLinkageContext
    sec = SecurityLayer(PipelineConfig(seed=1).derive, time_base=0)
    ctx = DeviceLinkageContext(1, 2, b"\x07" * 16, b"\x08" * 16)
    c = sec.provision("veh_000", ctx, [(0, 0, 0.0, 100.0)])
    with pytest.raises(ValueError, match="before the .*epoch"):
        sec.sign("veh_000", c.credentials[0].digest, b"p", 1.0, claimed_gen_time=-5.0)


def test_security_is_reproducible_end_to_end(tmp_path):
    kw = dict(security_model="ecdsa", rotate_period_s=20.0, **_DEFAULT)
    a = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "a"), **kw))
    b = run_pipeline(PipelineConfig(out_dir=str(tmp_path / "b"), **kw))
    assert a.data_digest == b.data_digest


def test_manifest_stops_claiming_no_signature_when_there_is_one(tmp_path):
    d = tmp_path / "sp"
    run_pipeline(PipelineConfig(out_dir=str(d), security_model="ecdsa", **_DEFAULT))
    sp = _manifest(d)["standards_profile"]
    assert "simulated boolean" not in sp["security_envelope"]
    assert "ECDSA" in sp["security_envelope"]
    # ...and it still refuses the claims it has not earned.
    for banned in ("compliant", "conformance-tested", "certified", "interoperable"):
        assert banned not in sp["security_envelope"].lower().replace("not interoperable", "")
    assert "provisioning" in sp


@needs_asn1
def test_manifest_carries_the_codecs_own_standards_claim(tmp_path):
    d = tmp_path / "sc"
    run_pipeline(PipelineConfig(out_dir=str(d), message_codec="etsi_cam_en302637_2", **_DEFAULT))
    sp = _manifest(d)["standards_profile"]
    assert "no ASN.1 encoding" not in sp["message"]
    assert "UPER" in sp["message"] and "EN 302 637-2" in sp["message"]
    assert "NOT conformance-tested" in sp["message"]
    assert sp["asn1_licence"].startswith("BSD-3-Clause")


# ================================================================================================
# 8. Configuration refusals, and the optional-dependency posture
# ================================================================================================
@pytest.mark.parametrize("kw,match", [
    (dict(message_codec="nope"), "message_codec"),
    (dict(message_signer="hmac"), "message_signer"),
    (dict(security_model="rsa"), "security_model"),
    (dict(dcc=True), "dcc"),
    (dict(cam_generation_rules=True, dt=2.0), "cam_generation_rules"),
    (dict(ma_backhaul_s=-1.0), "ma_backhaul_s"),
])
def test_bad_protocol_config_is_refused(kw, match):
    with pytest.raises(ValueError, match=match):
        validate_config(PipelineConfig(**kw))


def test_ambiguous_codec_selection_is_refused():
    with pytest.raises(Exception, match="ambiguous"):
        validate_config(PipelineConfig(message_codec="native_v1",
                                       plugins={"message_codec": {"ref": "etsi_cam_en302637_2"}}))


def test_message_codec_slot_is_now_consumed():
    assert "message_codec" in RM._CONSUMED_SLOTS
    validate_config(PipelineConfig(plugins={"message_codec": {"ref": "native_v1"}}))


def test_codec_plugin_section_typos_are_refused_at_config_time():
    with pytest.raises(Exception, match="unknown key"):
        validate_config(PipelineConfig(
            plugins={"message_codec": {"ref": "native_v1", "parms": {}}}))
    with pytest.raises(Exception, match="conformance"):
        validate_config(PipelineConfig(
            plugins={"message_codec": {"ref": "native_v1", "conformance": "maybe"}}))


def test_codec_params_are_refused_at_construction_before_step_zero(tmp_path):
    """A bad param must be fatal BEFORE step 0 and before an output directory exists -- conformance
    check C10's rule, applied to this slot."""
    d = tmp_path / "bad"
    with pytest.raises(Exception, match="bogus"):
        run_pipeline(PipelineConfig(
            out_dir=str(d), n_steps=2, n_vehicles=12,
            plugins={"message_codec": {"ref": "native_v1", "params": {"bogus": 1}}}))
    assert not d.exists()


def test_engine_imports_and_runs_without_asn1tools(tmp_path):
    """THE HARD CONSTRAINT. Run in a CHILD interpreter: masking `asn1tools` in this process would
    poison `sys.modules` for the rest of the session."""
    # .../src/scms_sim_ref/mock_pipeline/run.py -> .../src
    src = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(RM.__file__))))
    code = (
        "import sys\n"
        "sys.modules['asn1tools'] = None\n"
        "from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline\n"
        "import scms_sim_ref.codecs as C\n"
        "res = run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network='grid',\n"
        "    duration_s=20, arrival_rate=1.5, grid_w=5, grid_h=5, attacker_pct=0.25,\n"
        f"    out_dir=r'{tmp_path / 'noasn1'}'))\n"
        "assert res.data_digest\n"
        "run_pipeline(PipelineConfig(seed=7, traffic_flow=True, road_network='grid',\n"
        "    duration_s=10, arrival_rate=1.5, grid_w=5, grid_h=5, message_codec='native_v1',\n"
        f"    out_dir=r'{tmp_path / 'nativeonly'}'))\n"
        "try:\n"
        "    run_pipeline(PipelineConfig(seed=7, duration_s=5, n_steps=3,\n"
        "        message_codec='etsi_cam_en302637_2',\n"
        f"        out_dir=r'{tmp_path / 'etsi'}'))\n"
        "except ImportError as e:\n"
        "    assert 'asn1tools' in str(e), e\n"
        "else:\n"
        "    raise SystemExit('the ETSI codec was constructed without asn1tools')\n"
        "print('OK')\n")
    env = {**os.environ, "PYTHONPATH": src, "PYTHONHASHSEED": "0"}
    r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stdout + r.stderr
    assert "OK" in r.stdout


def test_requirements_still_do_not_carry_asn1tools():
    root = os.path.dirname(os.path.dirname(os.path.abspath(RM.__file__)))
    root = os.path.dirname(os.path.dirname(root))
    req = os.path.join(root, "requirements.txt")
    if os.path.exists(req):
        with open(req, encoding="utf-8") as fh:
            assert "asn1tools" not in fh.read()
