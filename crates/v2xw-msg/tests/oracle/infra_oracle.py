#!/usr/bin/env python3
"""The pycrate side of the J2735 SPaT and MAP oracle.

The companion of ``oracle.py``, which does the same job for the BSM. Reads a vector file
written by ``tests/j2735_infra_oracle.rs`` and, for each vector,

1. encodes the field values with pycrate and compares the octets against the Rust
   encoder's, which is the check that catches a misread constraint or a missing
   extension bit;
2. decodes the Rust encoder's octets with pycrate and compares the field values, which
   is the check that catches a field written in the wrong place;
3. does the same for the ``MessageFrame`` wrapper.

It then emits vectors of its own: messages built with the elements the Rust codec
deliberately **refuses** — a ``DescriptiveName``, a ``ComputedLane``, an
``AdvisorySpeedList``, a ``NodeAttributeSetXY``, a non-vehicle lane type. The Rust side
asserts that each is refused by name rather than stepped over. Nothing on the Rust side
can build one of those, which is exactly why they have to come from here.

pycrate compiles the real ASN.1 at run time, so it is an implementation of the standard
rather than of this project's reading of the standard. For SPaT and MAP that independence
is not a nicety: their structure was written from recall, because the J2735 modules are
git-ignored (build decision D3) and were not in the checkout. Until this script runs, the
Rust encoders are labelled unvalidated everywhere they are described.

Values travel as JSON with three markers, because JSON has no tuples or bytes:

* ``{"__hex__": "deadbeef"}``   an OCTET STRING
* ``{"__bits__": [value, n]}``  a BIT STRING of ``n`` bits, value right-aligned
* ``{"__open__": ["Name", v]}`` a CHOICE alternative or an open type

Usage: ``infra_oracle.py <vectors.json> <results.json>``.
"""

from __future__ import annotations

import json
import os
import sys
import traceback

sys.path.insert(0, os.environ.get("V2XW_J2735_ORACLE_DIR", os.path.dirname(os.path.abspath(__file__))))

import j2735_all  # noqa: E402


def load(type_name: str, candidates: list[str]):
    """Find a compiled ASN.1 type, whatever pycrate called the module it lives in.

    The 2024 release names its modules with a version suffix, and pycrate sanitises those
    names into Python identifiers, so the module holding ``SPAT`` cannot be predicted from
    the standard alone. Rather than guess once and fail obscurely, try the plausible names
    and say what was available if none of them has the type.
    """
    for name in candidates:
        module = getattr(j2735_all, name, None)
        if module is not None and hasattr(module, type_name):
            return getattr(module, type_name)
    # Last resort: scan every compiled module for the type.
    for name in dir(j2735_all):
        module = getattr(j2735_all, name)
        if hasattr(module, type_name) and hasattr(module, "__name__"):
            return getattr(module, type_name)
    available = ", ".join(n for n in dir(j2735_all) if not n.startswith("_"))
    raise ImportError(
        f"no compiled module holds the ASN.1 type {type_name}. "
        f"Tried {candidates}. Available modules: {available}"
    )


SPAT = load("SPAT", ["SPAT", "SPATmsg", "DSRC"])
MAP = load("MapData", ["MapData", "MAP", "MAPmsg", "DSRC"])
FRAME = load("MessageFrame", ["MessageFrame", "DSRC"])

#: ``DSRCmsgID`` values, which the Rust side also hard-codes; a disagreement here is a
#: finding in its own right, so they are named rather than inlined.
MAP_MESSAGE_ID = 18
SPAT_MESSAGE_ID = 19

#: The ``MessageFrame`` open-type alternative each message travels in.
FRAME_ALTERNATIVE = {"spat": "SPAT", "map": "MapData"}


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
    """Both directions for one vector, plus its MessageFrame."""
    kind = vector["kind"]
    pdu = SPAT if kind == "spat" else MAP
    message_id = SPAT_MESSAGE_ID if kind == "spat" else MAP_MESSAGE_ID
    expected = vector["value"]
    rust_hex = vector["rust_hex"]
    result = {"name": vector["name"], "kind": kind}

    # 1. pycrate encodes what Rust encoded.
    pdu.set_val(to_py(expected))
    result["py_hex"] = pdu.to_uper().hex()
    result["encode_match"] = result["py_hex"] == rust_hex

    # 2. pycrate decodes what Rust encoded.
    pdu.from_uper(bytes.fromhex(rust_hex))
    decoded = canon(pdu.get_val())
    result["py_decoded"] = decoded
    result["decode_match"] = decoded == expected

    # 3. the MessageFrame wrapper.
    if "rust_frame_hex" in vector:
        FRAME.set_val(
            {
                "messageId": message_id,
                "value": (FRAME_ALTERNATIVE[kind], to_py(expected)),
            }
        )
        result["py_frame_hex"] = FRAME.to_uper().hex()
        result["frame_match"] = result["py_frame_hex"] == vector["rust_frame_hex"]
    return result


#: A SPaT the Rust codec can build, used as the base for the refusal vectors so that only
#: the refused element differs.
BASE_SPAT = {
    "timeStamp": 123456,
    "intersections": [
        {
            "id": {"id": 1234},
            "revision": 3,
            "status": (0b0000010000000000, 16),
            "states": [
                {
                    "signalGroup": 1,
                    "state-time-speed": [
                        {
                            "eventState": "protected-Movement-Allowed",
                            "timing": {"startTime": 120, "minEndTime": 275, "maxEndTime": 275},
                        }
                    ],
                }
            ],
        }
    ],
}

#: Likewise for a MAP.
BASE_MAP = {
    "msgIssueRevision": 3,
    "intersections": [
        {
            "id": {"id": 1234},
            "revision": 3,
            "refPoint": {"lat": 407440000, "long": -739900000},
            "laneWidth": 350,
            "laneSet": [
                {
                    "laneID": 1,
                    "laneAttributes": {
                        "directionalUse": (0b10, 2),
                        "sharedWith": (0, 10),
                        "laneType": ("vehicle", (0, 8)),
                    },
                    "nodeList": (
                        "nodes",
                        [
                            {"delta": ("node-XY1", {"x": 0, "y": 0})},
                            {"delta": ("node-XY1", {"x": 120, "y": 340})},
                        ],
                    ),
                }
            ],
        }
    ],
}


def deep(value):
    """A deep copy of a plain JSON-ish structure."""
    return json.loads(json.dumps(value, default=lambda v: list(v) if isinstance(v, tuple) else v))


def refusal_vectors():
    """Messages carrying elements the Rust codec must refuse rather than skip.

    Each entry names the ``construct`` string the Rust refusal is expected to carry, so a
    failure says which element slipped through rather than only that something did.

    Every case is built independently and its own failure is recorded, because a field
    name that a later edition of the standard renamed should cost one vector rather than
    the whole check.
    """
    cases = []

    def spat_case(name, construct, mutate):
        value = deep(BASE_SPAT)
        # The bit string survived deep() as a list; put the tuple back.
        value["intersections"][0]["status"] = (0b0000010000000000, 16)
        mutate(value)
        return ("spat", name, construct, value)

    def map_case(name, construct, mutate):
        value = deep(BASE_MAP)
        lane = value["intersections"][0]["laneSet"][0]
        lane["laneAttributes"] = {
            "directionalUse": (0b10, 2),
            "sharedWith": (0, 10),
            "laneType": ("vehicle", (0, 8)),
        }
        lane["nodeList"] = (
            "nodes",
            [
                {"delta": ("node-XY1", {"x": 0, "y": 0})},
                {"delta": ("node-XY1", {"x": 120, "y": 340})},
            ],
        )
        mutate(value)
        return ("map", name, construct, value)

    def set_spat_name(v):
        v["name"] = "Main St / 1st Ave"

    def set_state_name(v):
        v["intersections"][0]["name"] = "Main St"

    def set_movement_name(v):
        v["intersections"][0]["states"][0]["movementName"] = "NB through"

    def set_speeds(v):
        event = v["intersections"][0]["states"][0]["state-time-speed"][0]
        event["speeds"] = [{"type": "greenwave", "speed": 500}]

    def set_layer_type(v):
        v["layerType"] = "intersectionData"

    def set_lane_name(v):
        v["intersections"][0]["laneSet"][0]["name"] = "NB curb lane"

    def set_computed_lane(v):
        v["intersections"][0]["laneSet"][0]["nodeList"] = (
            "computed",
            {
                "referenceLaneId": 1,
                "offsetXaxis": ("small", 100),
                "offsetYaxis": ("small", -100),
            },
        )

    def set_node_attributes(v):
        nodes = v["intersections"][0]["laneSet"][0]["nodeList"][1]
        nodes[1] = {
            "delta": ("node-XY1", {"x": 120, "y": 340}),
            "attributes": {"dWidth": 20},
        }
        v["intersections"][0]["laneSet"][0]["nodeList"] = ("nodes", nodes)

    def set_crosswalk_lane(v):
        v["intersections"][0]["laneSet"][0]["laneAttributes"]["laneType"] = (
            "crosswalk",
            (0, 16),
        )

    def set_road_segments(v):
        v["roadSegments"] = [
            {
                "id": {"id": 7},
                "revision": 1,
                "refPoint": {"lat": 407440000, "long": -739900000},
                "roadLaneSet": [
                    {
                        "laneID": 1,
                        "laneAttributes": {
                            "directionalUse": (0b10, 2),
                            "sharedWith": (0, 10),
                            "laneType": ("vehicle", (0, 8)),
                        },
                        "nodeList": (
                            "nodes",
                            [
                                {"delta": ("node-XY1", {"x": 0, "y": 0})},
                                {"delta": ("node-XY1", {"x": 10, "y": 10})},
                            ],
                        ),
                    }
                ],
            }
        ]

    cases.append(spat_case("python/spat-descriptive-name", "SPAT.name", set_spat_name))
    cases.append(
        spat_case(
            "python/spat-intersection-name", "IntersectionState.name", set_state_name
        )
    )
    cases.append(
        spat_case(
            "python/spat-movement-name", "MovementState.movementName", set_movement_name
        )
    )
    cases.append(
        spat_case("python/spat-advisory-speeds", "MovementEvent.speeds", set_speeds)
    )
    cases.append(map_case("python/map-layer-type", "MapData.layerType", set_layer_type))
    cases.append(map_case("python/map-lane-name", "GenericLane.name", set_lane_name))
    cases.append(
        map_case("python/map-computed-lane", "NodeListXY.computed", set_computed_lane)
    )
    cases.append(
        map_case("python/map-node-attributes", "NodeXY.attributes", set_node_attributes)
    )
    cases.append(
        map_case(
            "python/map-crosswalk-lane", "LaneAttributes.laneType", set_crosswalk_lane
        )
    )
    cases.append(
        map_case("python/map-road-segments", "MapData.roadSegments", set_road_segments)
    )

    out = []
    errors = []
    for kind, name, construct, value in cases:
        pdu = SPAT if kind == "spat" else MAP
        try:
            pdu.set_val(to_py(value))
            out.append(
                {
                    "name": name,
                    "kind": kind,
                    "construct": construct,
                    "hex": pdu.to_uper().hex(),
                }
            )
        except Exception as exc:  # noqa: BLE001 - one bad field name must not lose the rest
            errors.append(
                {
                    "name": name,
                    "construct": construct,
                    "error": f"{type(exc).__name__}: {exc}",
                }
            )
    return out, errors


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
                    "kind": vector.get("kind", "?"),
                    "error": f"{type(exc).__name__}: {exc}",
                    "traceback": traceback.format_exc(limit=4),
                }
            )

    report = {"pycrate_results": results}
    try:
        origin, origin_errors = refusal_vectors()
        report["python_origin"] = origin
        if origin_errors:
            report["python_origin_partial_errors"] = origin_errors
    except Exception as exc:  # noqa: BLE001
        report["python_origin_error"] = f"{type(exc).__name__}: {exc}"

    with open(sys.argv[2], "w", encoding="utf-8") as handle:
        json.dump(report, handle)

    failed = [
        r
        for r in results
        if r.get("error")
        or not all(r.get(k, True) for k in ("encode_match", "decode_match", "frame_match"))
    ]
    for kind in ("spat", "map"):
        of_kind = [r for r in results if r.get("kind") == kind]
        bad = [r for r in of_kind if r in failed]
        print(f"{len(of_kind) - len(bad)}/{len(of_kind)} {kind} vectors matched pycrate")
    for bad in failed[:5]:
        trimmed = {k: v for k, v in bad.items() if k != "py_decoded"}
        print(f"  MISMATCH {bad['name']}: {json.dumps(trimmed)[:400]}")
    if report.get("python_origin_partial_errors"):
        print(
            f"  note: {len(report['python_origin_partial_errors'])} refusal vectors could "
            "not be built by pycrate; see the report"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
