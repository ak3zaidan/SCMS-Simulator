#!/usr/bin/env python3
"""Independent-decoder harness: decode bytes this repository produced with decoders it did not.

**Round-tripping through your own encoder proves nothing.** `encode` and `decode` in
`scms_sim_ref/codecs/etsi.py` share a process, a compiled specification object and a cache; a
symmetric bug is invisible to them by construction. This script is the check that is not.

It runs as a SEPARATE PROCESS and applies up to three independent decoders to each blob:

`fresh_asn1tools`
    A **separately constructed** `asn1tools.compile_files(...)` built here, in this process, from
    the vendored `.asn` sources -- never the engine's `_SPEC_CACHE`, never an object the encoder
    touched. Catches a poisoned or stale compilation cache and any encoder/decoder state coupling.
    It does NOT catch a bug in `asn1tools` itself, which is why the next one exists.

`pycrate`
    `pycrate` >= 0.8, a **completely separate ASN.1 runtime** by a different author, using its own
    **pre-compiled** `pycrate_asn1dir.ITS_CAM_2` / `ITS_DENM_3` / `ITS_VAM_3` modules -- generated
    by the pycrate project from ETSI's published ASN.1, not from this repository's vendored copies.
    Agreement here is the strongest evidence available offline: two independent implementations,
    two independently obtained copies of the standard, same field values, and (checked) identical
    re-encoded octets.

`vector`
    A byte blob obtained from somewhere else entirely, decoded with both of the above. See
    `--vector` and the honest finding recorded in the test suite: the only freely downloadable
    third-party CAM UPER blob found (`bastibl/its-g5-cam`'s `sample_cam.uper`) is **not decodable**
    under EN 302 637-2 V1.4.1 -- both decoders reject it at the same field. There is no free
    normative CAM byte-vector set: ETSI Plugtests are attendance-based.

Usage:
    python tools/asn1_interop.py --job job.json > report.json
    python tools/asn1_interop.py --self-test

`job.json` is `{"cases": [{"name": ..., "spec": "cam_r1", "pdu": "CAM", "hex": "...",
"expect": {<asn1tools value tree>}}]}`. The report is JSON on stdout; exit 0 iff every case had at
least one independent decoder agree and no independent decoder disagree.
"""
from __future__ import annotations

import argparse
import binascii
import json
import os
import sys

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASN1_DIR = os.path.join(REPO_ROOT, "src", "scms_sim_ref", "codecs", "asn1")

#: Deliberately duplicated from `codecs/etsi.MODULE_SETS` rather than imported. Importing the
#: engine's table would make this harness depend on the thing it is checking; a transcription that
#: disagrees is itself a finding, and `--self-test` compares the two.
MODULE_SETS = {
    "cam_r1": ("cdd_v1.3.1/ITS-Container.asn",
               "cam_en302637_2_v1.4.1/CAM-PDU-Descriptions.asn"),
    "denm_r1": ("cdd_v1.3.1/ITS-Container.asn",
                "denm_en302637_3_v1.3.1/DENM-PDU-Descriptions.asn"),
    "vam_r2": ("cdd_v2.1.1/ETSI-ITS-CDD.asn",
               "vam_ts103300_3_v2.3.1/VAM-PDU-Descriptions.asn",
               "vam_ts103300_3_v2.3.1/motorcyclist-special-container.asn"),
}

#: spec name -> (pycrate module attribute, ASN.1 module attribute inside it).
PYCRATE_MODULES = {
    "cam_r1": ("ITS_CAM_2", "CAM_PDU_Descriptions"),
    "denm_r1": ("ITS_DENM_3", "DENM_PDU_Descriptions"),
    "vam_r2": ("ITS_VAM_3", "VAM_PDU_Descriptions"),
}


def _fresh_spec(spec_name: str):
    import asn1tools
    paths = [os.path.join(ASN1_DIR, r.replace("/", os.sep)) for r in MODULE_SETS[spec_name]]
    return asn1tools.compile_files(paths, "uper")


def _normalise(v):
    """asn1tools yields tuples for CHOICE; pycrate yields tuples too but JSON round-trips them to
    lists. Normalise both to lists so the comparison is about VALUES, not container types."""
    if isinstance(v, tuple):
        return [_normalise(x) for x in v]
    if isinstance(v, list):
        return [_normalise(x) for x in v]
    if isinstance(v, dict):
        return {k: _normalise(x) for k, x in sorted(v.items())}
    if isinstance(v, (bytes, bytearray)):
        return binascii.hexlify(bytes(v)).decode()
    return v


def decode_fresh_asn1tools(spec_name, pdu, blob):
    return _normalise(_fresh_spec(spec_name).decode(pdu, blob))


def decode_pycrate(spec_name, pdu, blob):
    import importlib
    mod_name, asn_mod = PYCRATE_MODULES[spec_name]
    mod = importlib.import_module(f"pycrate_asn1dir.{mod_name}")
    obj = getattr(getattr(mod, asn_mod), pdu)
    obj.from_uper(blob)
    val = obj.get_val()
    reencoded = obj.to_uper()
    return _normalise(val), reencoded == blob


def _flat(v, prefix=""):
    if isinstance(v, dict):
        for k, x in v.items():
            yield from _flat(x, f"{prefix}.{k}")
    elif isinstance(v, list):
        for i, x in enumerate(v):
            yield from _flat(x, f"{prefix}[{i}]")
    else:
        yield prefix, v


def _differences(got, expect, ignore_paths):
    """Dotted-path differences between two normalised value trees, minus the declared exclusions.

    Exclusions exist for exactly one reason and each carries its justification in the job file:
    the two runtimes spell an ABSENT-BECAUSE-DEFAULT component differently. `asn1tools` materialises
    the ASN.1 DEFAULT (`validityDuration: "defaultValidity"`); `pycrate` returns `None`. Neither is
    wrong and neither changes a single bit -- `reencodes_identically` proves that independently --
    so comparing them as a value difference would report a representation choice as an interop
    failure. Every exclusion is listed in the report rather than applied silently.
    """
    g, e = dict(_flat(got)), dict(_flat(expect))
    ignore = set(ignore_paths or ())
    return sorted(k for k in set(g) | set(e) if k not in ignore and g.get(k) != e.get(k))


def run_case(case: dict) -> dict:
    spec, pdu = case["spec"], case.get("pdu", "CAM")
    blob = binascii.unhexlify(case["hex"])
    out = {"name": case.get("name", "?"), "spec": spec, "pdu": pdu, "bytes": len(blob),
           "decoders": {}, "ignored_paths": dict(case.get("ignore_paths") or {}),
           "declared_skips": dict(case.get("expect_decoder_skip") or {})}
    expect = _normalise(case["expect"]) if case.get("expect") is not None else None
    ignore = set(out["ignored_paths"])

    try:
        got = decode_fresh_asn1tools(spec, pdu, blob)
        diffs = [] if expect is None else _differences(got, expect, ignore)
        out["decoders"]["fresh_asn1tools"] = {
            "ok": True, "matches_expected": not diffs, "differences": diffs, "value": got}
    except Exception as exc:
        out["decoders"]["fresh_asn1tools"] = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}

    try:
        got, same_bytes = decode_pycrate(spec, pdu, blob)
        diffs = [] if expect is None else _differences(got, expect, ignore)
        out["decoders"]["pycrate"] = {
            "ok": True, "matches_expected": not diffs, "differences": diffs,
            "reencodes_identically": same_bytes, "value": got}
    except ImportError as exc:
        out["decoders"]["pycrate"] = {"ok": None, "skipped": f"pycrate unavailable: {exc}"}
    except Exception as exc:
        out["decoders"]["pycrate"] = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}

    # A DECLARED skip is a decoder this case does not expect to be able to validate against, with
    # the reason recorded. The VAM case uses it: pycrate 0.8.1 ships TS 103 300-3 RELEASE 1
    # (module OID {0 4 0 5 1 103300 1 1}, `VruHighFrequencyContainer.heading` typed as the R1
    # `Heading`), while the vendored Forge module is `major-version-3`. That is a standards-edition
    # mismatch, not an encoder defect, and calling it a failure would be wrong.
    for name, reason in out["declared_skips"].items():
        if name in out["decoders"]:
            out["decoders"][name]["declared_skip"] = reason

    def _state(n, r):
        if r.get("declared_skip") or r.get("skipped"):
            return "skipped"
        if r.get("ok") and r.get("matches_expected") is not False:
            return "agreed"
        return "disagreed"

    states = {n: _state(n, r) for n, r in out["decoders"].items()}
    agreed = [n for n, s in states.items() if s == "agreed"]
    disagreed = [n for n, s in states.items() if s == "disagreed"]
    skipped = [n for n, s in states.items() if s == "skipped"]
    if case.get("expect_reject"):
        # The case asserts the blob is NOT decodable. Passing means every decoder refused it.
        out["verdict"] = "EXPECTED_REJECT" if agreed == [] and disagreed else "FAIL"
    else:
        out["verdict"] = "PASS" if agreed and not disagreed else "FAIL"
    out["agreed"], out["disagreed"], out["skipped"] = (sorted(agreed), sorted(disagreed),
                                                       sorted(skipped))
    return out


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--job", help="JSON job file; '-' for stdin")
    ap.add_argument("--brief", action="store_true",
                    help="omit each decoder's full value tree from the report. The verdict, the "
                         "dotted-path differences and `reencodes_identically` are unaffected -- "
                         "this only drops the bulk, which for a 200-PDU corpus is ~99 %% of it.")
    ap.add_argument("--self-test", action="store_true",
                    help="cross-check this file's MODULE_SETS against codecs/etsi.MODULE_SETS")
    args = ap.parse_args(argv)

    if args.self_test:
        sys.path.insert(0, os.path.join(REPO_ROOT, "src"))
        from scms_sim_ref.codecs.etsi import MODULE_SETS as ENGINE_SETS
        mine = {k: tuple(v) for k, v in MODULE_SETS.items()}
        theirs = {k: tuple(v[0]) for k, v in ENGINE_SETS.items()}
        ok = mine == theirs
        print(json.dumps({"module_sets_agree": ok, "harness": mine, "engine": theirs}, indent=1))
        return 0 if ok else 3

    if not args.job:
        ap.error("one of --job or --self-test is required")
    raw = sys.stdin.read() if args.job == "-" else open(args.job, "r", encoding="utf-8").read()
    job = json.loads(raw)
    results = [run_case(c) for c in job["cases"]]
    if args.brief:
        for r in results:
            for d in r["decoders"].values():
                d.pop("value", None)
    bad = [r for r in results if r["verdict"] == "FAIL"]
    print(json.dumps({"results": results, "failed": len(bad)}, indent=1, sort_keys=True))
    return 3 if bad else 0


if __name__ == "__main__":                                          # pragma: no cover
    raise SystemExit(main())
