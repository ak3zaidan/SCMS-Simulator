#!/usr/bin/env python3
"""The pycrate side of the J2735 BSM oracle.

Reads a vector file written by ``tests/j2735_oracle.rs``, and for each vector

1. encodes the field values with pycrate and compares the octets against the Rust
   encoder's, which is the check that catches a misread constraint;
2. decodes the Rust encoder's octets with pycrate and compares the field values,
   which is the check that catches a field written in the wrong place;
3. does the same for the ``MessageFrame`` wrapper.

It also emits a few vectors of its own — containers the Rust codec deliberately does
not model — so the Rust side can prove it carries them through byte for byte.

pycrate compiles the real ASN.1 at run time, so it is an implementation of the
standard rather than of this project's reading of the standard. That independence is
the whole value of the exercise.

Values travel as JSON with three markers, because JSON has no tuples or bytes:

* ``{"__hex__": "deadbeef"}``   an OCTET STRING
* ``{"__bits__": [value, n]}``  a BIT STRING of ``n`` bits, value right-aligned
* ``{"__open__": ["Type", v]}`` an open type

Usage: ``oracle.py <vectors.json> <results.json>``.
"""

from __future__ import annotations

import json
import os
import sys
import traceback

sys.path.insert(0, os.environ.get("V2XW_J2735_ORACLE_DIR", os.path.dirname(os.path.abspath(__file__))))

from j2735_all import BasicSafetyMessage as BSM_MOD  # noqa: E402
from j2735_all import MessageFrame as MF_MOD  # noqa: E402

BSM = BSM_MOD.BasicSafetyMessage
FRAME = MF_MOD.MessageFrame


def to_py(value):
    """JSON with markers -> a pycrate value."""
    if isinstance(value, dict):
        if "__hex__" in value:
            return bytes.fromhex(value["__hex__"])
        if "__bits__" in value:
            bits, length = value["__bits__"]
            return (bits, length)
        if "__open__" in value:
            name, inner = value["__open__"]
            return (name, to_py(inner))
        return {k: to_py(v) for k, v in value.items()}
    if isinstance(value, list):
        return [to_py(v) for v in value]
    return value


def canon(value):
    """A pycrate value -> JSON with markers."""
    if isinstance(value, (bytes, bytearray)):
        return {"__hex__": bytes(value).hex()}
    if isinstance(value, dict):
        return {k: canon(v) for k, v in value.items()}
    if isinstance(value, tuple):
        if len(value) == 2 and isinstance(value[0], int) and isinstance(value[1], int):
            return {"__bits__": [value[0], value[1]]}
        if len(value) == 2 and isinstance(value[0], str):
            return {"__open__": [value[0], canon(value[1])]}
        return [canon(v) for v in value]
    if isinstance(value, list):
        return [canon(v) for v in value]
    return value


def run_vector(vector):
    """Both directions for one vector."""
    name = vector["name"]
    expected = vector["value"]
    rust_hex = vector["rust_hex"]
    result = {"name": name}

    # 1. pycrate encodes what Rust encoded.
    BSM.set_val(to_py(expected))
    py_bytes = BSM.to_uper()
    result["py_hex"] = py_bytes.hex()
    result["encode_match"] = result["py_hex"] == rust_hex

    # 2. pycrate decodes what Rust encoded.
    BSM.from_uper(bytes.fromhex(rust_hex))
    decoded = canon(BSM.get_val())
    result["py_decoded"] = decoded
    result["decode_match"] = decoded == expected

    # 3. the MessageFrame wrapper, if the vector carries one.
    if "rust_frame_hex" in vector:
        FRAME.set_val({"messageId": 20, "value": ("BasicSafetyMessage", to_py(expected))})
        result["py_frame_hex"] = FRAME.to_uper().hex()
        result["frame_match"] = result["py_frame_hex"] == vector["rust_frame_hex"]
    return result


def python_origin_vectors():
    """Messages pycrate builds that the Rust codec must carry through unchanged.

    Part II containers 1 and 2 are open types the Rust codec keeps as opaque octets. If
    its open-type length handling were wrong, re-encoding one of these would not
    reproduce the input, and nothing in a Rust-only round trip would say so.
    """
    core = {
        "msgCnt": 3,
        "id": b"\x11\x22\x33\x44",
        "secMark": 25_000,
        "lat": 407_440_000,
        "long": -739_900_000,
        "elev": 250,
        "accuracy": {"semiMajor": 12, "semiMinor": 8, "orientation": 1_234},
        "transmission": "forwardGears",
        "speed": 1_000,
        "heading": 14_400,
        "angle": -10,
        "accelSet": {"long": 25, "lat": -12, "vert": 50, "yaw": 300},
        "brakes": {
            "wheelBrakes": (0b01000, 5),
            "traction": "on",
            "abs": "off",
            "scs": "engaged",
            "brakeBoost": "on",
            "auxBrakes": "off",
        },
        "size": {"width": 190, "length": 480},
    }
    out = []

    supplemental = {
        "classification": 60,
        "vehicleData": {"height": 30, "mass": 120},
    }
    BSM.set_val(
        {
            "coreData": core,
            "partII": [
                {
                    "partII-Id": 2,
                    "partII-Value": ("SupplementalVehicleExtensions", supplemental),
                }
            ],
        }
    )
    out.append({"name": "python/supplemental-vehicle-ext", "hex": BSM.to_uper().hex()})

    special = {"description": {"typeEvent": 7938, "priority": b"\x02"}}
    BSM.set_val(
        {
            "coreData": core,
            "partII": [
                {
                    "partII-Id": 0,
                    "partII-Value": (
                        "VehicleSafetyExtensions",
                        {"lights": (0b100000000, 9)},
                    ),
                },
                {"partII-Id": 1, "partII-Value": ("SpecialVehicleExtensions", special)},
            ],
        }
    )
    out.append({"name": "python/special-vehicle-ext-after-safety", "hex": BSM.to_uper().hex()})

    return out


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    with open(sys.argv[1], encoding="utf-8") as handle:
        vectors = json.load(handle)

    results = []
    for vector in vectors:
        try:
            results.append(run_vector(vector))
        except Exception as exc:  # noqa: BLE001 - the report is the point
            results.append(
                {
                    "name": vector["name"],
                    "error": f"{type(exc).__name__}: {exc}",
                    "traceback": traceback.format_exc(limit=4),
                }
            )

    report = {"pycrate_results": results}
    try:
        report["python_origin"] = python_origin_vectors()
    except Exception as exc:  # noqa: BLE001
        report["python_origin_error"] = f"{type(exc).__name__}: {exc}"

    with open(sys.argv[2], "w", encoding="utf-8") as handle:
        json.dump(report, handle)

    failed = [r for r in results if r.get("error") or not all(
        r.get(k, True) for k in ("encode_match", "decode_match", "frame_match")
    )]
    print(f"{len(results) - len(failed)}/{len(results)} vectors matched pycrate")
    for bad in failed[:5]:
        print(f"  MISMATCH {bad['name']}: {json.dumps({k: v for k, v in bad.items() if k != 'py_decoded'})[:400]}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
