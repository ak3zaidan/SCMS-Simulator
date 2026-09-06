"""What a real PDU weighs, and what that weight costs on the air.

Encodes claims through the shipped ETSI codecs and reports the *distribution* of the encoded
length -- the manifest publishes only a mean -- then turns each length into IEEE 802.11p airtime and
into the CBR one station-second of it offers. The two engines' assumed frame times are evaluated on
the same arithmetic so the 2.05x disagreement is settled on measured octets rather than on either
assumption.

Claims are swept over the ranges the engine actually produces (speed 0-40 m/s, heading 0-360 deg,
acceleration -8..+4 m/s^2, position confidence 0.5-25 m, the InTAS local frame's coordinate span,
every station type and every message type), because a UPER length that is constant has to be shown
to be constant rather than asserted from one sample.

    python tools/codec_size_probe.py --json C:/Temp/pstack/codec_sizes.json
"""
from __future__ import annotations

import argparse
import itertools
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_SRC = os.path.join(os.path.dirname(_HERE), "src")
if _SRC not in sys.path:
    sys.path.insert(0, _SRC)

from scms_sim_ref.api.codec import Claim                      # noqa: E402
from scms_sim_ref.codecs import etsi, etsi_rules as ER        # noqa: E402


def _claims():
    """The sweep. Every combination the engine can hand a codec, not one specimen of it."""
    xs = (0.0, 213816.2, -4820.7, 999999.9)
    speeds = (0.0, 0.4, 13.9, 27.8, 40.0, float("nan"))
    headings = (0.0, 4.1, 89.9, 180.0, 359.99)
    accels = (None, -8.0, -2.5, 0.0, 3.9)
    confs = (0.5, 2.142, 7.7, 25.0)
    dims = ((None, None), (4.5, 1.8), (12.0, 2.55))
    stypes = ("vehicle", "vru")
    i = 0
    for x, sp, hd, ac, pc, (ln, wd), st in itertools.product(
            xs, speeds, headings, accels, confs, dims, stypes):
        i += 1
        yield Claim(station_id=(i * 2654435761) % (2 ** 32), cert_digest=f"{i:016x}",
                    msg_type="cam", gen_time=(i % 600) * 0.1, x=x, y=x * 0.7 + 451713.0,
                    speed=sp, heading=hd, pos_conf=pc, station_type=st,
                    accel=ac, length_m=ln, width_m=wd)


def _hist(codec, claims, station=None) -> dict:
    sizes: dict[int, int] = {}
    for c in claims:
        n = len(codec.encode_cam(c, station))
        sizes[n] = sizes.get(n, 0) + 1
    return dict(sorted(sizes.items()))


def airtime_row(mpdu: int) -> dict:
    return {"mpdu_bytes": mpdu,
            "ppdu_symbols": ER.ppdu_symbols(mpdu),
            "ppdu_us": round(ER.ppdu_airtime_s(mpdu) * 1e6, 1),
            "frame_us": round(ER.frame_airtime_s(mpdu) * 1e6, 1),
            "cbr_per_station_at_10hz": round(ER.frame_airtime_s(mpdu) * 10.0, 6),
            "cbr_per_station_at_measured_rate": None}


def main(argv=None) -> int:
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("--json", dest="json_out", default="")
    p.add_argument("--rate-hz", type=float, default=0.0,
                   help="measured CAM rate; fills cbr_per_station_at_measured_rate")
    a = p.parse_args(argv)

    codec = etsi.EtsiCamCodec()
    claims = list(_claims())
    out: dict = {"n_claims_swept": len(claims),
                 "codec": codec.profile_id,
                 "asn1_backend": f"asn1tools {__import__('asn1tools').__version__} (UPER)",
                 "security_envelope_bytes": dict(etsi.SECURITY_ENVELOPE_BYTES)}

    out["cam_payload_hist"] = {str(k): v for k, v in _hist(codec, claims).items()}

    # An RSU takes the `rsuContainerHighFrequency` alternative -- no kinematics at all.
    from scms_sim_ref.api.codec import StationView
    rsu = StationView(frame=codec.frame, epoch_unix=codec.epoch_unix, is_rsu=True)
    out["cam_payload_hist_rsu"] = {str(k): v for k, v in _hist(codec, claims[:200], rsu).items()}

    # DENM and VAM, over the same claim population.
    dn = etsi.EtsiDenmCodec()
    vm = etsi.EtsiVamCodec()
    den = [c.replace(msg_type="denm", event_type="stationaryVehicle") for c in claims[:400]]
    vam = [c.replace(msg_type="vam", station_type="vru") for c in claims[:400]]
    out["denm_payload_hist"] = {str(k): v for k, v in _hist(dn, den).items()}
    out["vam_payload_hist"] = {str(k): v for k, v in _hist(vm, vam).items()}

    # Framed sizes: payload + TS 103 097 envelope, per signer arm, and what each costs on the air.
    base = max(int(k) for k in out["cam_payload_hist"])
    lo = min(int(k) for k in out["cam_payload_hist"])
    out["cam_payload_bytes"] = {"min": lo, "max": base,
                                "constant": lo == base}
    frames = {}
    for signer, env in sorted(etsi.SECURITY_ENVELOPE_BYTES.items()):
        row = airtime_row(lo + env)
        if a.rate_hz:
            row["cbr_per_station_at_measured_rate"] = round(
                ER.frame_airtime_s(lo + env) * a.rate_hz, 6)
        row["envelope_bytes"] = env
        frames[signer] = row
    out["framed"] = frames

    # The two ASSUMPTIONS, on the same arithmetic.
    out["assumed"] = {
        "java_300B_ppdu_only_us": round(ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES) * 1e6, 1),
        "java_300B_with_mac_us": round(ER.frame_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES) * 1e6, 1),
        "python_500B_plus_mac_us": round(ER.PYTHON_ASSUMED_FRAME_AIRTIME_S * 1e6, 1),
        "python_over_java": round(ER.PYTHON_ASSUMED_FRAME_AIRTIME_S
                                  / ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES), 4),
        "measured_digest_over_java_ppdu": round(
            ER.frame_airtime_s(lo + etsi.SECURITY_ENVELOPE_BYTES["digest"])
            / ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES), 4),
        "python_assumption_over_measured_digest": round(
            ER.PYTHON_ASSUMED_FRAME_AIRTIME_S
            / ER.frame_airtime_s(lo + etsi.SECURITY_ENVELOPE_BYTES["digest"]), 4),
        "legacy_native_300B_frame_us": round(ER.frame_airtime_s(300) * 1e6, 1),
        # PPDU against PPDU, which is the only apples-to-apples comparison with the Java figure:
        # it counts no MAC overhead, so comparing it to a frame time that does understates its
        # error. On the payload alone the Java assumption is 300 B against a measured 134 B.
        "java_ppdu_over_measured_digest_ppdu": round(
            ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES)
            / ER.ppdu_airtime_s(lo + etsi.SECURITY_ENVELOPE_BYTES["digest"]), 4),
        "java_mpdu_over_measured_digest_mpdu": round(
            ER.JAVA_ASSUMED_MPDU_BYTES / (lo + etsi.SECURITY_ENVELOPE_BYTES["digest"]), 4),
        "java_ppdu_over_measured_certificate_ppdu": round(
            ER.ppdu_airtime_s(ER.JAVA_ASSUMED_MPDU_BYTES)
            / ER.ppdu_airtime_s(lo + etsi.SECURITY_ENVELOPE_BYTES["certificate"]), 4),
    }
    txt = json.dumps(out, indent=1, sort_keys=True)
    if a.json_out:
        with open(a.json_out, "w", encoding="utf-8", newline="\n") as fh:
            fh.write(txt + "\n")
    print(txt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
