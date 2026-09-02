"""`python -m scms_sim_ref.codecs` -- inspect the message-codec seam without running the engine.

    python -m scms_sim_ref.codecs quantisation [--json] [--samples N]
    python -m scms_sim_ref.codecs claim [--profile etsi_cam_en302637_2]
    python -m scms_sim_ref.codecs encode [--profile ...] [--json]

`quantisation` needs NOTHING beyond the standard library -- no ASN.1 library, no run, no network --
because the whole point of keeping `units.py` separate is that the unit and frame layer is testable
on its own. `encode` needs the optional `asn1tools` extra.
"""
from __future__ import annotations

import argparse
import binascii
import json
import sys

from ..api.codec import Claim, StationView
from . import units as U


def _demo_claim() -> Claim:
    """A fixed, documented sample. No clock, no RNG: the octets it produces are stable forever and
    can be pasted into any third-party decoder."""
    return Claim(station_id=1234567, cert_digest="deadbeefcafe0001", msg_type="cam",
                 gen_time=12.345, x=1234.5, y=-987.25, speed=13.89, heading=37.5,
                 pos_conf=3.21, station_type="vehicle")


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(prog="python -m scms_sim_ref.codecs", description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    q = sub.add_parser("quantisation", help="per-field round-trip error, measured")
    q.add_argument("--json", action="store_true")
    q.add_argument("--samples", type=int, default=20001)

    c = sub.add_parser("claim", help="print the standards claim and conventions of a profile")
    c.add_argument("--profile", default="etsi_cam_en302637_2")

    e = sub.add_parser("encode", help="encode the demo claim and print the octets")
    e.add_argument("--profile", default="etsi_cam_en302637_2")
    e.add_argument("--json", action="store_true", help="also print the ASN.1 value tree")

    args = ap.parse_args(argv)

    if args.cmd == "quantisation":
        rep = U.quantisation_report(n=args.samples)
        print(json.dumps(rep, indent=2, sort_keys=True) if args.json
              else U.format_quantisation_report(rep))
        return 0

    from . import BUILTIN_CODEC_BY_NAME, CodecDependencyError
    cls = BUILTIN_CODEC_BY_NAME.get(args.profile)
    if cls is None:
        print(f"unknown profile {args.profile!r}; known: {sorted(BUILTIN_CODEC_BY_NAME)}",
              file=sys.stderr)
        return 2
    try:
        codec = cls()
    except CodecDependencyError as exc:
        print(str(exc), file=sys.stderr)
        return 4

    if args.cmd == "claim":
        print(json.dumps({"profile_id": codec.profile_id,
                          "capabilities": sorted(codec.capabilities()),
                          "standards_claim": codec.standards_claim(),
                          "conventions": codec.conventions()}, indent=2, sort_keys=True,
                         default=str))
        return 0

    claim = _demo_claim()
    station = StationView(frame=getattr(codec, "frame", StationView().frame),
                          epoch_unix=getattr(codec, "epoch_unix", StationView().epoch_unix))
    blob = codec.encode_cam(claim, station)
    print(f"{len(blob)} octets: {binascii.hexlify(blob).decode()}")
    print(f"wire_size_bytes(signer=digest) = {codec.wire_size_bytes(claim, 'digest')}")
    if args.json and hasattr(codec, "cam_dict"):
        print(json.dumps(codec.cam_dict(claim, station), indent=2, sort_keys=True, default=str))
    return 0


if __name__ == "__main__":                                          # pragma: no cover
    raise SystemExit(main())
