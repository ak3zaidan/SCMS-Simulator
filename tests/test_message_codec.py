"""The `MessageCodec` seam: real ASN.1 UPER, the unit/frame conversion, and INDEPENDENT decoding.

Five things are graded here, in order of how easy they are to fake:

1. **The engine is unchanged.** The default golden and the reference digest still hold, the codecs
   package imports without `asn1tools`, and registering a codec is not enabling one.
2. **The firewall.** `Claim` has no field naming ground truth, and the `StationType` mapping is
   two-valued *because* the real fleet class is ORACLE.
3. **Units and frames.** Every convertible field round-trips inside half an LSB, MEASURED by a
   deterministic sweep rather than asserted.
4. **Real UPER octets.** Byte-stable across processes, exactly 41 B for a CAM, and the header
   prefix is verified BY HAND from the ASN.1 -- no library involved in that assertion at all.
5. **Independent decoding.** `tools/asn1_interop.py` runs OUT OF PROCESS with a separately
   constructed compilation and, when available, `pycrate` -- a different runtime, by a different
   author, using its own independently derived copy of the ETSI modules.

Point 5 is the only one that can prove interoperability, and the tests record precisely what it
did and did not establish. See `test_no_free_normative_cam_vector_exists`.
"""

import binascii
import json
import os
import subprocess
import sys

import pytest

from scms_sim_ref.api import registry as _registry
from scms_sim_ref.api.codec import (CODEC_SPEC, DEFAULT_EPOCH_UNIX, GeoFrame, INTERFACE_VERSION,
                                    Claim, StationView)
from scms_sim_ref.codecs import units as U
from scms_sim_ref.schemas.records import is_forbidden_feature_key

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

#: The determinism-contract default golden, identical to `tests/test_radio_propagation.py:23`.
DEFAULT_GOLDEN = "0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740"


def _has_asn1tools() -> bool:
    try:
        import asn1tools                                          # noqa: F401
        return True
    except ImportError:
        return False


needs_asn1 = pytest.mark.skipif(not _has_asn1tools(),
                                reason="optional extra `asn1tools` is not installed")


def _claim(**kw):
    base = dict(station_id=1234567, cert_digest="deadbeefcafe0001", msg_type="cam",
                gen_time=12.345, x=1234.5, y=-987.25, speed=13.89, heading=37.5,
                pos_conf=3.21, station_type="vehicle")
    base.update(kw)
    return Claim(**base)


# --------------------------------------------------------------------------- #
# 1. The engine is unchanged
# --------------------------------------------------------------------------- #
def test_default_golden_unmoved(tmp_path):
    """The whole seam is opt-in. Nothing on the default path constructs a codec, so the pinned
    default digest must be bit-for-bit what it was before this package existed."""
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "default")))
    assert res.data_digest == DEFAULT_GOLDEN


def test_registering_is_not_enabling(tmp_path):
    """Importing the codecs package registers four codecs. That must not change one byte of a run
    that never names one -- the `test_radio_propagation.py:78-86` pattern, applied to this slot."""
    import scms_sim_ref.codecs                                    # noqa: F401  (registers)
    assert "native_v1" in _registry.builtin_names_sorted("message_codec")
    from scms_sim_ref.mock_pipeline import PipelineConfig, run_pipeline
    res = run_pipeline(PipelineConfig(
        seed=7, traffic_flow=True, road_network="grid", duration_s=60, arrival_rate=1.5,
        grid_w=5, grid_h=5, attacker_pct=0.25, out_dir=str(tmp_path / "after_import")))
    assert res.data_digest == DEFAULT_GOLDEN


def test_imports_cleanly_without_asn1tools():
    """The hard constraint: `asn1tools` is an OPTIONAL extra and the engine must import and run
    without it. Simulated by hiding the module from a CHILD interpreter -- not by monkeypatching
    `sys.modules` in this one, which would leave the real module importable through a cached
    parent package and prove nothing."""
    prog = (
        "import sys\n"
        # `sys.modules[name] = None` is the documented way to make `import name` raise
        # ImportError. The legacy find_module/load_module finder protocol was REMOVED in
        # Python 3.12, so a meta-path blocker written that way silently does nothing -- which is
        # how an earlier version of this test passed while proving the opposite.
        "sys.modules['asn1tools'] = None\n"
        "import scms_sim_ref.codecs as C\n"
        "import scms_sim_ref.mock_pipeline.run as R\n"
        "n = C.NativeV1Codec()\n"
        "from scms_sim_ref.api.codec import Claim, StationView\n"
        "c = Claim(station_id=1, cert_digest='x', msg_type='cam', gen_time=0.0, x=0.0, y=0.0,\n"
        "          speed=0.0, heading=0.0, pos_conf=1.0)\n"
        "assert n.decode_cam(n.encode_cam(c, StationView())) == c\n"
        "try:\n"
        "    C.EtsiCamCodec()\n"
        "    raise SystemExit('etsi codec constructed without asn1tools')\n"
        "except C.CodecDependencyError as e:\n"
        "    assert 'asn1tools' in str(e)\n"
        "print('OK')\n")
    env = dict(os.environ, PYTHONPATH=os.path.join(REPO_ROOT, "src"))
    out = subprocess.run([sys.executable, "-c", prog], capture_output=True, text=True, env=env,
                         cwd=REPO_ROOT)
    assert out.returncode == 0, out.stdout + out.stderr
    assert "OK" in out.stdout


def test_asn1tools_is_an_extra_not_a_requirement():
    """`requirements.txt` is deliberately just cryptography/pydantic/pytest. The optional
    dependency belongs in `pyproject.toml` under `[project.optional-dependencies]`."""
    with open(os.path.join(REPO_ROOT, "requirements.txt"), encoding="utf-8") as fh:
        reqs = fh.read().lower()
    assert "asn1tools" not in reqs
    with open(os.path.join(REPO_ROOT, "pyproject.toml"), encoding="utf-8") as fh:
        proj = fh.read()
    assert "asn1tools" in proj and "optional-dependencies" in proj


# --------------------------------------------------------------------------- #
# 2. The firewall
# --------------------------------------------------------------------------- #
def test_claim_carries_no_oracle_field():
    """`Claim` is built to the same rule as `Observation`: MA-visible only. A codec that could see
    ground truth would launder it into every encoded PDU -- and encoded PDUs are exactly what a
    TS 103 759 `v2xPduEvidence` entry hands to the MA."""
    import dataclasses
    bad = [f.name for f in dataclasses.fields(Claim) if is_forbidden_feature_key(f.name)]
    assert bad == []
    for forbidden in ("veh", "true_x", "true_y", "falsified", "ghost", "tspd", "thdg",
                      "is_attacker", "attack_type", "veh_type"):
        assert not hasattr(Claim, forbidden)


def test_station_type_mapping_is_two_valued_by_construction():
    """13 ETSI classes exist and the engine has a real car/motorcycle/truck/bus fleet -- on
    `GtVehicle`, which is ORACLE. The MA-visible declaration is two-valued, so the mapping is too.
    This test exists to make that a DECISION with a reason rather than an omission."""
    assert set(U.ENGINE_STATION_TYPE) == {"vehicle", "vru"}
    assert U.station_type_to_etsi("vehicle") == 5              # passengerCar
    assert U.station_type_to_etsi("vru") == 1                  # pedestrian
    assert U.station_type_to_etsi("vehicle", is_rsu=True) == 15  # roadSideUnit
    # The finer classes are reachable only when a caller supplies an MA-visible name explicitly.
    assert U.station_type_to_etsi("vru", vru_station_type="cyclist") == 2
    with pytest.raises(ValueError):
        U.station_type_to_etsi("truck")                        # NOT an engine-declared value


def test_claim_is_frozen():
    c = _claim()
    with pytest.raises((AttributeError, TypeError)):
        c.x = 0.0


# --------------------------------------------------------------------------- #
# 3. Units and frames
# --------------------------------------------------------------------------- #
def test_heading_frame_flip_at_the_cardinals():
    """The single most error-prone conversion in the whole profile: the engine is degrees CCW from
    East, ETSI is 0.1 degree CW from North. Graded at the four cardinals, where a sign error or a
    swapped zero cannot hide."""
    assert U.heading_to_etsi(0.0) == 900        # engine East    -> wgs84East (900)
    assert U.heading_to_etsi(90.0) == 0         # engine North   -> wgs84North (0)
    assert U.heading_to_etsi(180.0) == 2700     # engine West    -> wgs84West (2700)
    assert U.heading_to_etsi(270.0) == 1800     # engine South   -> wgs84South (1800)
    for h in (0.0, 90.0, 180.0, 270.0, 45.0, 359.9):
        assert U.heading_from_etsi(U.heading_to_etsi(h)) == pytest.approx(h, abs=0.05)


def test_generation_delta_time_is_ms_mod_65536_from_the_2004_epoch():
    """`generationDeltaTime = TimestampIts mod 65536`, `TimestampIts` = ms since 2004-01-01 on a
    clock that does not pause for leap seconds."""
    # 2024-01-01T00:00:00Z is 631 152 000 s after 2004-01-01T00:00:00Z, plus 5 leap seconds.
    assert U.timestamp_its_ms(0.0, DEFAULT_EPOCH_UNIX) == (631152000 + 5) * 1000
    assert U.generation_delta_time(0.0, DEFAULT_EPOCH_UNIX) == ((631152005 * 1000) % 65536)
    # The wrap is a property of the STANDARD. 65.536 s apart, two instants are indistinguishable.
    a = U.generation_delta_time(1.0, DEFAULT_EPOCH_UNIX)
    b = U.generation_delta_time(1.0 + 65.536, DEFAULT_EPOCH_UNIX)
    assert a == b, "65.536 s apart, two CAMs carry the identical generationDeltaTime"
    ts_a = U.timestamp_its_ms(1.0, DEFAULT_EPOCH_UNIX)
    ts_b = U.timestamp_its_ms(1.0 + 65.536, DEFAULT_EPOCH_UNIX)
    assert ts_a != ts_b
    # A receiver with a good clock recovers the absolute time from the residue...
    assert U.resolve_generation_delta_time(a, ts_a) == ts_a
    assert U.resolve_generation_delta_time(b, ts_b) == ts_b
    # ...and a receiver whose clock is a whole cycle out recovers the WRONG one, silently. That is
    # the standard's ambiguity, not this codec's, and it is why `decode_cam` takes a reference.
    assert U.resolve_generation_delta_time(b, ts_a) == ts_a


def test_leap_second_table_matches_tai_utc():
    """TAI-UTC was 32 s at the 2004 ITS epoch and is 37 s now: exactly five insertions."""
    assert len(U.LEAP_SECONDS_AFTER_2004) == 5
    assert U.leap_seconds_since_2004(U.UNIX_EPOCH_2004) == 0
    assert U.leap_seconds_since_2004(1704067200.0) == 5          # 2024-01-01
    assert U.leap_seconds_since_2004(1483228799.0) == 4          # one second before 2017-01-01


def test_pos_confidence_becomes_a_real_ellipse_with_a_stated_assumption():
    """The engine's scalar is an isotropic 95 % radius, so the honest ellipse is a CIRCLE and the
    orientation is arbitrary, not unavailable."""
    ell = U.pos_confidence_ellipse(3.21)
    assert ell == {"semiMajorConfidence": 321, "semiMinorConfidence": 321,
                   "semiMajorOrientation": 0}
    assert ell["semiMajorOrientation"] != U.HEADING_UNAVAILABLE
    assert U.pos_confidence_from_ellipse(ell) == pytest.approx(3.21, abs=0.005)
    # Out of range is ETSI's own escape, not a clamp we invented.
    assert U.pos_confidence_ellipse(500.0)["semiMajorConfidence"] == U.SEMI_AXIS_OUT_OF_RANGE
    assert U.pos_confidence_from_ellipse(U.pos_confidence_ellipse(500.0)) is None
    assert "isotropic" in U.POS_CONFIDENCE_ASSUMPTION


def test_unavailable_is_used_instead_of_a_fabricated_zero():
    """Rule 1 of the units module. A field the engine does not measure gets ETSI's explicit
    `unavailable` code; encoding a zero would be a fabricated measurement."""
    assert U.accel_to_etsi(None) == 161
    assert U.vehicle_length_to_etsi(None) == 1023
    assert U.vehicle_width_to_etsi(None) == 62
    assert U.altitude_to_etsi(None) == 800001
    assert U.speed_to_etsi(None) == 16383
    assert U.heading_to_etsi(None) == 3601
    for f in (U.accel_from_etsi, U.vehicle_length_from_etsi, U.vehicle_width_from_etsi,
              U.altitude_from_etsi):
        assert f(None) is None


def test_iround_is_half_away_from_zero_not_bankers():
    """The bound "half an LSB" is a property of the ENCODER only if the tie rule is fixed."""
    assert U.iround(0.5) == 1 and round(0.5) == 0
    assert U.iround(1.5) == 2 and U.iround(2.5) == 3 and round(2.5) == 2
    assert U.iround(-0.5) == -1 and U.iround(-1.5) == -2


def test_quantisation_report_every_field_within_half_an_lsb():
    """THE per-field error report. Measured by a deterministic golden-ratio sweep (no RNG), 20 001
    samples per field, over the engine's real domain.

    Also a regression guard for a real defect this sweep found: computing `TimestampIts` as
    `1000 * (epoch + t - EPOCH2004)` forms a ~6.3e8 s float whose ulp (~2.4e-7 s) pushed the
    measured `gen_time` error to 0.50029 ms -- over the bound. The fix is the exact-integer split
    in `units.timestamp_its_ms`, and this assertion is what keeps it."""
    rep = U.quantisation_report()
    for field, r in sorted(rep.items()):
        assert r["measured"] <= r["bound"] + 1e-12, (
            f"{field}: measured {r['measured']!r} exceeds the half-LSB bound {r['bound']!r}")
    # And the bounds themselves are the standard's, not ours.
    assert rep["heading"]["lsb"] == 0.1                          # HeadingValue: 0.1 degree
    assert rep["speed"]["lsb"] == 0.01                           # SpeedValue: 0.01 m/s
    assert rep["gen_time"]["lsb"] == 0.001                       # GenerationDeltaTime: 1 ms
    assert rep["pos_confidence"]["lsb"] == 0.01                  # SemiAxisLength: 1 cm
    # 1/10 microdegree, projected into metres through the frame's own scale factors.
    assert rep["position_y"]["lsb"] == pytest.approx(1e-7 * 110540.0)
    assert rep["station_type"]["measured"] == 0.0 and rep["station_id"]["measured"] == 0.0


def test_geo_frame_is_the_engine_s_own_frame():
    """Not a frame invented for the codec: the same construction `osm._frame` derives and
    `netimport._assert_frame` gates."""
    from scms_sim_ref.mock_pipeline import osm
    pts = [(48.75, 11.40), (48.79, 11.46)]
    f = osm._frame(pts)
    g = GeoFrame.from_dict(f)
    assert g.ky == 110540.0
    assert 0.0 < g.kx <= 111320.0
    x, y = g.to_local(48.77, 11.43)
    assert g.to_wgs84(x, y) == pytest.approx((48.77, 11.43), abs=1e-12)


# --------------------------------------------------------------------------- #
# 4. Real UPER octets
# --------------------------------------------------------------------------- #
@needs_asn1
def test_cam_encodes_to_41_bytes_of_real_uper():
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    blob = k.encode_cam(_claim(), StationView(frame=k.frame, epoch_unix=k.epoch_unix))
    assert isinstance(blob, bytes) and len(blob) == 41


@needs_asn1
def test_its_pdu_header_prefix_verified_by_hand():
    """No library is involved in this assertion.

    `ItsPduHeader ::= SEQUENCE { protocolVersion INTEGER(0..255), messageID INTEGER(0..255),
    stationID INTEGER(0..4294967295) }` with AUTOMATIC TAGS and no OPTIONAL members and no
    extension marker, so UPER emits no preamble and lays the three constrained integers down as
    8 + 8 + 32 bits, octet-aligned from bit 0. The first six octets of any CAM are therefore
    exactly `02 02` + stationID big-endian; a DENM's are `02 01`; a Release-2 VAM's are `03 10`
    (protocolVersion 3, `MessageId ::= ... vam(16)`).

    This is the one check in the file that would survive `asn1tools` and `pycrate` sharing a bug.
    """
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    sid = (0x12D687).to_bytes(4, "big")                          # 1234567
    assert k.encode_cam(_claim(), sv)[:6] == b"\x02\x02" + sid
    assert k.encode_denm(_claim(msg_type="denm", event_type="stationaryVehicle"),
                         sv)[:6] == b"\x02\x01" + sid
    assert k.encode_vam(_claim(msg_type="vam", station_type="vru", speed=1.8),
                        sv)[:6] == b"\x03\x10" + sid


@needs_asn1
def test_uper_is_byte_stable_across_processes():
    """UPER is a deterministic encoding, which is what makes it compatible with the pinned-digest
    contract. Checked across a FRESH interpreter, not a second call in this one."""
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    here = k.encode_cam(_claim(), StationView(frame=k.frame, epoch_unix=k.epoch_unix))
    prog = (
        "import binascii\n"
        "from scms_sim_ref.codecs import EtsiCamCodec\n"
        "from scms_sim_ref.api.codec import Claim, StationView\n"
        "k = EtsiCamCodec()\n"
        "c = Claim(station_id=1234567, cert_digest='deadbeefcafe0001', msg_type='cam',\n"
        "          gen_time=12.345, x=1234.5, y=-987.25, speed=13.89, heading=37.5,\n"
        "          pos_conf=3.21, station_type='vehicle')\n"
        "print(binascii.hexlify(k.encode_cam(c, StationView(frame=k.frame,\n"
        "      epoch_unix=k.epoch_unix))).decode())\n")
    env = dict(os.environ, PYTHONPATH=os.path.join(REPO_ROOT, "src"))
    out = subprocess.run([sys.executable, "-c", prog], capture_output=True, text=True, env=env,
                         cwd=REPO_ROOT)
    assert out.returncode == 0, out.stdout + out.stderr
    assert out.stdout.strip() == binascii.hexlify(here).decode()


@needs_asn1
@pytest.mark.parametrize("kind", ["cam", "denm", "vam"])
def test_round_trip_restores_every_carried_field(kind):
    """`decode(encode(claim))` for the fields the PDU ACTUALLY carries.

    `cert_digest`, `sig_ok` and `cert_valid_*` are deliberately NOT among them: they belong to the
    TS 103 097 security envelope, and a bare CAM has none. Asserting their return would be a
    tautology dressed as a conformance result."""
    from scms_sim_ref.codecs import (CAM_CARRIED_FIELDS, DENM_CARRIED_FIELDS, VAM_CARRIED_FIELDS,
                                     EtsiCamCodec)
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    if kind == "cam":
        c, fields = _claim(), CAM_CARRIED_FIELDS
        back = k.decode_cam(k.encode_cam(c, sv), reference_t=c.gen_time)
    elif kind == "denm":
        c = _claim(msg_type="denm", event_type="emergencyElectronicBrakeLight",
                   sequence_number=7)
        fields = DENM_CARRIED_FIELDS
        back = k.decode_denm(k.encode_denm(c, sv))
    else:
        c = _claim(msg_type="vam", station_type="vru", speed=1.8)
        fields = VAM_CARRIED_FIELDS
        back = k.decode_vam(k.encode_vam(c, sv), reference_t=c.gen_time)
    tol = {"x": 0.004, "y": 0.006, "speed": 0.005, "heading": 0.05, "pos_conf": 0.005,
           "gen_time": 0.0005}
    for f in fields:
        a, b = getattr(c, f), getattr(back, f)
        if isinstance(a, (int, str)) and not isinstance(a, bool) and f not in tol:
            assert a == b, f
        else:
            assert b == pytest.approx(a, abs=tol.get(f, 1e-9)), f


@needs_asn1
def test_denm_event_names_map_to_normative_cause_codes():
    """A finding, not a translation: `emergencyElectronicBrakeLight` is not an ETSI DENM cause code
    at all -- it is a CAM `ExteriorLights` name. The DENM that announces the same event is
    `dangerousSituation(99)` / `emergencyElectronicBrakeEngaged(1)`."""
    assert U.denm_cause_code("stationaryVehicle") == (94, 0)
    assert U.denm_cause_code("emergencyElectronicBrakeLight") == (99, 1)
    with pytest.raises(ValueError):
        U.denm_cause_code("notAnEtsiEvent")
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    d = k.denm_dict(_claim(msg_type="denm", event_type="emergencyElectronicBrakeLight"))
    assert d["denm"]["situation"]["eventType"] == {"causeCode": 99, "subCauseCode": 1}


@needs_asn1
def test_wire_size_replaces_the_hard_coded_300_bytes():
    """`SignedCam.java` and `Dcc.java` assume 300 B for every frame. A real digest-signed CAM is
    41 + 93 = 134 B, so the Java CBR model over-states airtime by ~2.2x; a certificate-signed one
    is 260 B. Both envelope figures were MEASURED with an independent 1609.2 COER encoder."""
    from scms_sim_ref.codecs import EtsiCamCodec, NativeV1Codec, SECURITY_ENVELOPE_BYTES
    k, c = EtsiCamCodec(), _claim()
    assert k.wire_size_bytes(c, "none") == 41
    assert k.wire_size_bytes(c, "digest") == 41 + SECURITY_ENVELOPE_BYTES["digest"] == 134
    assert k.wire_size_bytes(c, "certificate") == 260
    assert k.wire_size_bytes(c, "digest") < 300 < k.wire_size_bytes(c, "certificate") * 2
    # `native_v1` keeps the legacy assumption on purpose, so the difference is visible.
    assert NativeV1Codec().wire_size_bytes(c, "digest") == 300
    with pytest.raises(ValueError):
        k.wire_size_bytes(c, "nonsense")


@needs_asn1
def test_unavailable_decodes_to_nan_never_to_zero():
    """"Unavailable" and "zero" are different statements: a station that did not report its speed
    is not a stationary station. An RSU CAM takes `rsuContainerHighFrequency`, which carries no
    kinematics at all, so it is the natural case where the distinction bites."""
    import math
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    rsu = StationView(frame=k.frame, epoch_unix=k.epoch_unix, is_rsu=True)
    back = k.decode_cam(k.encode_cam(_claim(speed=13.89, heading=37.5), rsu), reference_t=12.345)
    assert math.isnan(back.speed) and math.isnan(back.heading)
    assert back.station_type == "vehicle"          # roadSideUnit(15) is not a VRU class
    # ... while a station that DID report a genuine zero gets a genuine zero back.
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    zero = k.decode_cam(k.encode_cam(_claim(speed=0.0), sv), reference_t=12.345)
    assert zero.speed == 0.0 and not math.isnan(zero.speed)


@needs_asn1
def test_pdu_length_is_content_independent_under_this_profile():
    """Every member this profile emits is a fixed-width constrained INTEGER or ENUMERATED -- no
    length-prefixed type, no present/absent OPTIONAL -- so the UPER bit length does not depend on
    the values. Measured over 972 combinations of station id, time, position, speed, heading and
    confidence: **always 41 octets**.

    That is not trivia; it is what lets the airtime/CBR path memoise `wire_size_bytes` per
    (msg_type, is_rsu, signer) instead of encoding every broadcast. It also fails loudly the day
    someone adds an OPTIONAL container, which is exactly when the memoisation would become wrong.
    """
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    sizes = set()
    for sid in (0, 1, 2 ** 31, 2 ** 32 - 1):
        for t in (0.0, 12.345, 3599.999):
            for x in (-3000.0, 0.0, 3000.0):
                for spd in (0.0, 13.89, 60.0):
                    for hd in (0.0, 180.0, 359.9):
                        for pc in (0.0, 3.21, 500.0):
                            sizes.add(len(k.encode_cam(
                                _claim(station_id=sid, gen_time=t, x=x, y=-x, speed=spd,
                                       heading=hd, pos_conf=pc), sv)))
    assert sizes == {41}
    assert len(k.encode_cam(_claim(station_type="vru"), sv)) == 41
    assert len(k.encode_cam(_claim(accel=1.5, length_m=4.5, width_m=1.8), sv)) == 41
    # An RSU takes the `rsuContainerHighFrequency` alternative, which carries no kinematics.
    rsu = StationView(frame=k.frame, epoch_unix=k.epoch_unix, is_rsu=True)
    assert len(k.encode_cam(_claim(), rsu)) == 26
    assert len(k.encode_denm(_claim(msg_type="denm", event_type="stationaryVehicle"), sv)) == 43
    assert len(k.encode_vam(_claim(msg_type="vam", station_type="vru"), sv)) == 34


@needs_asn1
def test_evidence_pdu_is_the_real_octets():
    """What a TS 103 759 `v2xPduEvidence` entry needs and `evidence_msg_refs=[f'{rid}-m']` cannot
    be: the actual PDU."""
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    c = _claim()
    assert k.evidence_pdu(c, sv) == k.encode_cam(c, sv)
    assert k.decode_cam(k.evidence_pdu(c, sv), reference_t=c.gen_time).station_id == c.station_id


# --------------------------------------------------------------------------- #
# 5. Independent decoding -- the only proof that counts
# --------------------------------------------------------------------------- #
def _run_interop(cases, tmp_path, brief=False):
    job = tmp_path / "job.json"
    job.write_text(json.dumps({"cases": cases}, default=list), encoding="utf-8")
    env = dict(os.environ, PYTHONPATH=os.path.join(REPO_ROOT, "src"))
    argv = [sys.executable, os.path.join(REPO_ROOT, "tools", "asn1_interop.py"), "--job", str(job)]
    if brief:
        argv.append("--brief")
    out = subprocess.run(argv, capture_output=True, text=True, env=env, cwd=REPO_ROOT)
    assert out.stdout, out.stderr
    return json.loads(out.stdout), out.returncode


@needs_asn1
def test_independent_decode_out_of_process(tmp_path):
    """Encode here; decode THERE, with a compilation this process never touched.

    `fresh_asn1tools` rebuilds the specification from the vendored `.asn` in a separate
    interpreter, so no encoder object, cache entry or module-level state is shared. When `pycrate`
    is installed the same octets are additionally decoded by a completely separate ASN.1 runtime
    using its OWN pre-compiled ETSI modules -- and re-encoded, byte for byte.
    """
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    c = _claim()
    d = _claim(msg_type="denm", event_type="emergencyElectronicBrakeLight", sequence_number=7)
    v = _claim(msg_type="vam", station_type="vru", speed=1.8)
    ignore_default = {
        ".denm.management.validityDuration":
            "ASN.1 DEFAULT: asn1tools materialises `defaultValidity`, pycrate returns None. "
            "Neither changes a bit; `reencodes_identically` proves the octets agree."}
    cases = [
        {"name": "cam", "spec": "cam_r1", "pdu": "CAM",
         "hex": binascii.hexlify(k.encode_cam(c, sv)).decode(), "expect": k.cam_dict(c, sv)},
        {"name": "denm", "spec": "denm_r1", "pdu": "DENM",
         "hex": binascii.hexlify(k.encode_denm(d, sv)).decode(), "expect": k.denm_dict(d, sv),
         "ignore_paths": ignore_default},
        {"name": "vam", "spec": "vam_r2", "pdu": "VAM",
         "hex": binascii.hexlify(k.encode_vam(v, sv)).decode(), "expect": k.vam_dict(v, sv),
         "expect_decoder_skip": {
             "pycrate": "pycrate 0.8.1 ships TS 103 300-3 RELEASE 1 (module OID "
                        "{0 4 0 5 1 103300 1 1}; VruHighFrequencyContainer.heading typed as the "
                        "R1 `Heading`, BasicContainer.referencePosition as the R1 "
                        "`ReferencePosition`), while the vendored Forge module is "
                        "major-version-3 with `Wgs84Angle` and `ReferencePositionWithConfidence`. "
                        "A standards-EDITION mismatch, not an encoder defect."}},
    ]
    report, rc = _run_interop(cases, tmp_path)
    by_name = {r["name"]: r for r in report["results"]}
    assert rc == 0, json.dumps(report, indent=1)[:4000]
    for name in ("cam", "denm", "vam"):
        r = by_name[name]
        assert r["verdict"] == "PASS", r
        assert "fresh_asn1tools" in r["agreed"], r
    # The strong result: a foreign runtime agreed on CAM and DENM, values AND octets.
    if by_name["cam"]["decoders"]["pycrate"].get("ok"):
        for name in ("cam", "denm"):
            assert "pycrate" in by_name[name]["agreed"], by_name[name]
            assert by_name[name]["decoders"]["pycrate"]["reencodes_identically"] is True


@needs_asn1
def test_independent_decode_over_a_200_pdu_corpus(tmp_path):
    """N = 3 proves the happy path; N = 200 across the whole engine domain proves the CONVERSION.

    The corpus is generated by the same deterministic golden-ratio recurrence
    `codecs.units.quantisation_report` uses -- no RNG anywhere -- so the 200 PDUs are the same 200
    on every host and every run, and a failure is reproducible by index. Every one is decoded in
    the separate process, by the fresh compilation and (when installed) by pycrate, and compared
    field by field against the value tree this process built.
    """
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    sv = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    cases, phi, u = [], 0.6180339887498949, 0.5
    for i in range(200):
        u = (u + phi) % 1.0
        v = (u + phi * 0.5) % 1.0
        c = _claim(station_id=int(u * 4294967295), gen_time=v * 3600.0,
                   x=-3000.0 + u * 6000.0, y=-3000.0 + v * 6000.0,
                   speed=v * 60.0, heading=u * 360.0, pos_conf=v * 40.0,
                   station_type=("vru" if i % 5 == 0 else "vehicle"))
        cases.append({"name": f"corpus{i:03d}", "spec": "cam_r1", "pdu": "CAM",
                      "hex": binascii.hexlify(k.encode_cam(c, sv)).decode(),
                      "expect": k.cam_dict(c, sv)})
    report, rc = _run_interop(cases, tmp_path, brief=True)
    bad = [r for r in report["results"] if r["verdict"] != "PASS"]
    assert rc == 0 and not bad, json.dumps(bad[:2], indent=1)[:3000]
    agreed = {n for r in report["results"] for n in r["agreed"]}
    assert "fresh_asn1tools" in agreed
    if any(r["decoders"].get("pycrate", {}).get("ok") for r in report["results"]):
        assert all("pycrate" in r["agreed"] for r in report["results"])
        assert all(r["decoders"]["pycrate"]["reencodes_identically"]
                   for r in report["results"])


def test_interop_harness_does_not_import_the_engine_s_module_table(tmp_path):
    """The harness transcribes `MODULE_SETS` rather than importing it, so it cannot inherit the
    engine's mistake. `--self-test` is the check that the transcription still agrees."""
    env = dict(os.environ, PYTHONPATH=os.path.join(REPO_ROOT, "src"))
    out = subprocess.run([sys.executable, os.path.join(REPO_ROOT, "tools", "asn1_interop.py"),
                          "--self-test"], capture_output=True, text=True, env=env, cwd=REPO_ROOT)
    assert out.returncode == 0, out.stdout + out.stderr
    assert json.loads(out.stdout)["module_sets_agree"] is True


@needs_asn1
def test_no_free_normative_cam_vector_exists(tmp_path):
    """WHAT COULD NOT BE VALIDATED, recorded as a test rather than as a caveat in prose.

    There is no free normative CAM byte-vector set: ETSI Plugtests are attendance-based and the
    TTCN-3 conformance suites drive a live SUT over BTP/GeoNetworking, which a dataset generator
    does not have. The only freely downloadable third-party CAM UPER blob found --
    `bastibl/its-g5-cam`'s `sample_cam.uper`, 41 B, sha256 pinned below -- is **not a valid CAM
    under EN 302 637-2 V1.4.1**: both independent decoders reject it at the identical field,
    `yawRate.yawRateConfidence`, where it carries enumeration index 15 against a 9-value
    ENUMERATED. Two independent implementations agreeing on a REJECTION is itself a cross-check of
    the decoders; it just is not a positive vector.

    The blob is inlined as hex rather than fetched, so this test is offline and pinned.
    """
    third_party = ("01020000000000010006b49d214d693a41400200200030d4000000000000000000"
                   "02840ba9800fffc0")
    report, rc = _run_interop([{"name": "third_party_bastibl", "spec": "cam_r1", "pdu": "CAM",
                                "hex": third_party, "expect": None, "expect_reject": True}],
                              tmp_path)
    r = report["results"][0]
    assert r["verdict"] == "EXPECTED_REJECT", r
    errs = [d.get("error", "") for d in r["decoders"].values()]
    assert any("yawRateConfidence" in e for e in errs), r
    if r["decoders"].get("pycrate", {}).get("ok") is False:
        assert all("yawRateConfidence" in e for e in errs if e), r


# --------------------------------------------------------------------------- #
# Registry wiring
# --------------------------------------------------------------------------- #
def test_codecs_are_registered_and_resolve():
    import scms_sim_ref.codecs as C
    names = _registry.builtin_names_sorted("message_codec")
    assert set(names) == {"native_v1", "etsi_cam_en302637_2", "etsi_denm_en302637_3",
                          "etsi_vam_ts103300_3"}
    for name in names:
        obj, how, iv, shape = _registry.resolve("message_codec", name)
        assert how == "builtin" and iv == INTERFACE_VERSION and shape == "codec"
        assert obj is C.BUILTIN_CODEC_BY_NAME[name]
    assert not C.is_hijacked("native_v1")


def test_lazy_registration_works_from_a_cold_process():
    """`run.py` imports `detectors` to register checks; nothing imports the codecs. The registry's
    lazy registrar is what makes `resolve('message_codec', ...)` work anyway, and it must not
    require the caller to have imported the package first."""
    prog = ("import scms_sim_ref.api.registry as R\n"
            "assert 'scms_sim_ref.codecs' not in __import__('sys').modules\n"
            "obj, how, iv, shape = R.resolve('message_codec', 'native_v1')\n"
            "assert how == 'builtin' and shape == 'codec', (how, shape)\n"
            "print('OK')\n")
    env = dict(os.environ, PYTHONPATH=os.path.join(REPO_ROOT, "src"))
    out = subprocess.run([sys.executable, "-c", prog], capture_output=True, text=True, env=env,
                         cwd=REPO_ROOT)
    assert out.returncode == 0, out.stdout + out.stderr


def test_signature_contract_is_enforced():
    """Neither `Protocol` nor `ABC` checks signatures at runtime; the resolver does."""
    class Wrong:
        interface_version = INTERFACE_VERSION
        plugin_id = "wrong"
        profile_id = "wrong"

        def capabilities(self):
            return frozenset()

        def standards_claim(self):
            return {}

        def conventions(self):
            return {}

        def encode_cam(self, message, station):        # WRONG: first parameter must be `claim`
            return b""

        def decode_cam(self, blob):
            return None

        def wire_size_bytes(self, claim, signer):
            return 0

        def evidence_pdu(self, claim, station):
            return b""

    from scms_sim_ref.api.errors import SignatureError
    _registry.register_builtin("message_codec", "_test_wrong", Wrong)
    try:
        with pytest.raises(SignatureError) as exc:
            _registry.resolve("message_codec", "_test_wrong")
        assert "claim" in str(exc.value)
    finally:
        _registry._BUILTINS["message_codec"].pop("_test_wrong", None)
    assert set(CODEC_SPEC["encode_cam"]) == {"claim", "station"}


def test_capabilities_are_declared_and_known():
    import scms_sim_ref.codecs as C
    for name, cls in C.BUILTIN_CODECS:
        obj, how, iv, _ = _registry.resolve("message_codec", name)
        inst = cls() if name == "native_v1" or _has_asn1tools() else None
        if inst is None:
            continue
        caps = _registry.check_capabilities("message_codec", name, obj, how, inst.capabilities())
        assert caps
    assert _registry.CAPABILITIES["message_codec"][1] == frozenset()   # nothing reserved


# --------------------------------------------------------------------------- #
# The vendored ASN.1, and its licence
# --------------------------------------------------------------------------- #
def test_vendored_asn1_is_bsd3_and_pinned():
    """Vendoring is legal here and the evidence ships with it: each `.asn` sits beside the ETSI
    Forge repository's own BSD-3-Clause `LICENSE`, and `PROVENANCE.json` pins project, tag, commit
    and sha256 for every file."""
    import hashlib
    from scms_sim_ref.codecs.etsi import ASN1_DIR, provenance
    prov = provenance()
    assert prov.get("files"), "PROVENANCE.json missing or empty"
    assert "BSD-3-Clause" in prov["licence"]
    for rel, meta in sorted(prov["files"].items()):
        path = os.path.join(ASN1_DIR, rel.replace("/", os.sep))
        assert os.path.exists(path), rel
        with open(path, "rb") as fh:
            assert hashlib.sha256(fh.read()).hexdigest() == meta["sha256"], rel
        assert meta["tag"] and meta["commit"], rel
    for sub in ("cdd_v1.3.1", "cdd_v2.1.1", "cam_en302637_2_v1.4.1",
                "denm_en302637_3_v1.3.1", "vam_ts103300_3_v2.3.1"):
        lic = os.path.join(ASN1_DIR, sub, "LICENSE")
        assert os.path.exists(lic), sub
        with open(lic, encoding="utf-8") as fh:
            text = fh.read()
        assert "Copyright 20" in text and "ETSI" in text
        assert "Redistributions of source code must retain the above copyright notice" in text


@needs_asn1
def test_standards_claim_names_the_module_and_the_tag_and_no_more():
    """Section 6.5's permitted/forbidden table, asserted."""
    from scms_sim_ref.codecs import EtsiCamCodec, NativeV1Codec
    claim = EtsiCamCodec().standards_claim()
    msg = claim["message"]
    assert "EN 302 637-2 V1.4.1" in msg and "UPER" in msg and "v1.4.1" in msg
    assert claim["asn1_modules"]["cam"]["tag"] == "v1.4.1"
    assert claim["asn1_modules"]["cdd_r1"]["standard"] == "ETSI TS 102 894-2 V1.3.1"
    # The three words section 6.5 forbids may appear ONLY as an explicit disclaimer.
    for forbidden in ("conformance-tested", "certified", "Plugtests-validated"):
        assert f"NOT {forbidden}" in msg, forbidden
        assert msg.replace(f"NOT {forbidden}", "").count(forbidden) == 0, forbidden
    assert "none" in claim["security_envelope"] and "no signature" in claim["security_envelope"]
    assert NativeV1Codec().standards_claim()["asn1_module"] is None


def test_conventions_declare_both_sides_of_every_conversion():
    from scms_sim_ref.api.codec import ENGINE_CONVENTIONS
    from scms_sim_ref.codecs import NativeV1Codec
    nat = NativeV1Codec().conventions()
    assert nat["heading"] == "deg_ccw_from_east" and nat["speed"] == "m_s"
    assert ENGINE_CONVENTIONS["position"] == "m_local_xy"
    if _has_asn1tools():
        from scms_sim_ref.codecs import EtsiCamCodec
        conv = EtsiCamCodec().conventions()
        assert conv["engine"]["heading"] == "deg_ccw_from_east"
        assert "clockwise from North" in conv["wire"]["heading"]
        assert "1/10 microdegree" in conv["wire"]["position"]
        assert "65536" in conv["wire"]["time"]
        assert "ORACLE" in conv["assumptions"]["station_type"]


# --------------------------------------------------------------------------- #
# native_v1
# --------------------------------------------------------------------------- #
def test_native_codec_is_lossless_and_canonical():
    from scms_sim_ref.codecs import NativeV1Codec
    n = NativeV1Codec()
    c = _claim()
    blob = n.encode_cam(c, StationView())
    assert n.decode_cam(blob) == c                                # exactly, no quantisation
    assert blob == n.encode_cam(c, StationView())                 # canonical: stable
    assert json.loads(blob.decode())["heading"] == 37.5
    assert b'"accel":null' in blob                                # absent stays absent
    from scms_sim_ref.api.codec import CAP_LOSSLESS
    assert CAP_LOSSLESS in n.capabilities()


def test_native_codec_rejects_unknown_params():
    from scms_sim_ref.codecs import NativeV1Codec
    with pytest.raises(ValueError):
        NativeV1Codec(params={"nope": 1})


@needs_asn1
def test_etsi_codec_fails_fast_on_bad_params():
    """Conformance rule C10, applied to this slot: invalid params raise at CONSTRUCTION."""
    from scms_sim_ref.codecs import EtsiCamCodec
    with pytest.raises(ValueError):
        EtsiCamCodec(params={"not_a_param": 1})
    with pytest.raises(ValueError):
        EtsiCamCodec(params={"vru_station_type": "unicyclist"})
    with pytest.raises(ValueError):
        EtsiCamCodec(params={"lat0": 200.0})
    k = EtsiCamCodec(params={"lat0": 52.0, "lon0": 13.0, "kx": 68000.0, "ky": 110540.0})
    assert k.frame.lat0 == 52.0
    with pytest.raises(ValueError):
        k.encode_cam(_claim(station_id=2 ** 32), StationView(frame=k.frame))


@needs_asn1
def test_a_station_view_with_a_different_frame_is_refused():
    """`decode_*` has only the CODEC's frame to invert with -- a CAM carries WGS84, not a frame.
    A `StationView` with a different origin would encode fine and decode hundreds of metres away,
    silently and only for the stations that passed that view."""
    from scms_sim_ref.codecs import EtsiCamCodec
    k = EtsiCamCodec()
    other = GeoFrame.centred_on(52.52, 13.405)                   # Berlin, not the default origin
    with pytest.raises(ValueError) as exc:
        k.encode_cam(_claim(), StationView(frame=other))
    assert "differs from this codec's frame" in str(exc.value)
    # ... and the matching frame is accepted, and round-trips.
    ok = StationView(frame=k.frame, epoch_unix=k.epoch_unix)
    back = k.decode_cam(k.encode_cam(_claim(), ok), reference_t=12.345)
    assert back.x == pytest.approx(1234.5, abs=0.004)
