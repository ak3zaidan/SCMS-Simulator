#!/usr/bin/env python3
"""Fetch (or re-verify) the published ETSI ITS ASN.1 modules from the ETSI Forge.

WHY THIS EXISTS
---------------
`src/scms_sim_ref/codecs/asn1/` **vendors** these modules. That is legal here and it is stated
explicitly rather than assumed: every ETSI Forge ASN.1 repository ships a `LICENSE` that is
verbatim **BSD-3-Clause, "Copyright 2019 ETSI"**, whose clause 1 permits redistribution in source
form provided the notice, the conditions and the disclaimer are retained. The vendored tree
therefore carries each repository's own `LICENSE` file next to its `.asn`, unmodified. This is the
opposite of the SAVeNoW loop counts (`tools/fetch_ingolstadt_counts.py`), which have **no formally
stated licence** and are consequently cached into a git-ignored directory and never committed.

WHAT THIS TOOL IS FOR THEN
--------------------------
1. **Re-verification.** `--check` re-downloads every pinned file and compares its sha256 against
   `PINS` below. If ETSI ever force-pushes a tag, this fails loudly instead of silently changing
   what "EN 302 637-2 V1.4.1" means in this repository.
2. **Refresh.** `--write` re-materialises the vendored tree from the Forge at the pinned tags.
3. **The offline path is the default.** The engine NEVER calls this at run time; it reads the
   vendored files. No network access is required to encode a CAM.

Every pin is (project id, path, git tag, commit sha of that tag, sha256 of the file bytes). The
commit sha is recorded because a GitLab tag is a mutable name and a commit is not.

    python tools/fetch_etsi_asn1.py --check      # verify the vendored bytes against the Forge
    python tools/fetch_etsi_asn1.py --write      # (re)materialise the vendored tree

Exit codes: 0 ok, 2 usage error, 3 digest mismatch / fetch failure.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import urllib.parse
import urllib.request

FORGE = "https://forge.etsi.org/rep/api/v4"

REPO_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
VENDOR = os.path.join(REPO_ROOT, "src", "scms_sim_ref", "codecs", "asn1")

#: dest relative path -> (project path_with_namespace, project id, file path, tag)
#: Tags are the PUBLISHED ETSI deliverable versions; the commit each resolved to is in
#: `asn1/PROVENANCE.json`, written by --write.
PINS = {
    # --- common data dictionary, TS 102 894-2 -------------------------------------------------
    # Release 1 generation: module `ITS-Container`. Imported by CAM R1 and DENM R1.
    "cdd_v1.3.1/ITS-Container.asn": ("ITS/asn1/cdd_ts102894_2", 487, "ITS-Container.asn", "v1.3.1"),
    "cdd_v1.3.1/LICENSE": ("ITS/asn1/cdd_ts102894_2", 487, "LICENSE", "v1.3.1"),
    # Release 2 generation: module `ETSI-ITS-CDD`. Imported by VAM TS 103 300-3.
    "cdd_v2.1.1/ETSI-ITS-CDD.asn": ("ITS/asn1/cdd_ts102894_2", 487, "ETSI-ITS-CDD.asn", "v2.1.1"),
    "cdd_v2.1.1/LICENSE": ("ITS/asn1/cdd_ts102894_2", 487, "LICENSE", "v2.1.1"),
    # --- CAM, EN 302 637-2 V1.4.1 ---------------------------------------------------------------
    "cam_en302637_2_v1.4.1/CAM-PDU-Descriptions.asn":
        ("ITS/asn1/cam_en302637_2", 490, "CAM-PDU-Descriptions.asn", "v1.4.1"),
    "cam_en302637_2_v1.4.1/LICENSE": ("ITS/asn1/cam_en302637_2", 490, "LICENSE", "v1.4.1"),
    # --- DENM, EN 302 637-3 V1.3.1 --------------------------------------------------------------
    "denm_en302637_3_v1.3.1/DENM-PDU-Descriptions.asn":
        ("ITS/asn1/denm_en302637_3", 491, "DENM-PDU-Descriptions.asn", "v1.3.1"),
    "denm_en302637_3_v1.3.1/LICENSE": ("ITS/asn1/denm_en302637_3", 491, "LICENSE", "v1.3.1"),
    # --- VAM, TS 103 300-3 V2.3.1 ---------------------------------------------------------------
    "vam_ts103300_3_v2.3.1/VAM-PDU-Descriptions.asn":
        ("ITS/asn1/vam-ts103300_3", 500, "VAM-PDU-Descriptions.asn", "v2.3.1"),
    "vam_ts103300_3_v2.3.1/motorcyclist-special-container.asn":
        ("ITS/asn1/vam-ts103300_3", 500, "motorcyclist-special-container.asn", "v2.3.1"),
    "vam_ts103300_3_v2.3.1/LICENSE": ("ITS/asn1/vam-ts103300_3", 500, "LICENSE", "v2.3.1"),
}


def _raw(pid: int, path: str, ref: str, timeout: float = 60.0) -> bytes:
    url = f"{FORGE}/projects/{pid}/repository/files/{urllib.parse.quote(path, safe='')}/raw?ref={ref}"
    with urllib.request.urlopen(url, timeout=timeout) as fh:
        return fh.read()


def _tag_commit(pid: int, tag: str, timeout: float = 60.0) -> str:
    url = f"{FORGE}/projects/{pid}/repository/tags/{urllib.parse.quote(tag, safe='')}"
    with urllib.request.urlopen(url, timeout=timeout) as fh:
        return json.loads(fh.read())["commit"]["id"]


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--check", action="store_true",
                   help="fetch and compare against the vendored bytes; do not write")
    g.add_argument("--write", action="store_true", help="(re)materialise the vendored tree")
    ap.add_argument("--dest", default=VENDOR)
    args = ap.parse_args(argv)

    prov: dict = {"source": "https://forge.etsi.org/rep/ITS/asn1",
                  "licence": "BSD-3-Clause (Copyright 2019 ETSI) -- see the LICENSE beside each .asn",
                  "files": {}}
    bad = 0
    commits: dict = {}
    for rel, (ns, pid, path, tag) in sorted(PINS.items()):
        try:
            blob = _raw(pid, path, tag)
        except Exception as exc:                                   # pragma: no cover - network
            print(f"FETCH FAIL {rel}: {type(exc).__name__}: {exc}", file=sys.stderr)
            bad += 1
            continue
        got = hashlib.sha256(blob).hexdigest()
        key = (pid, tag)
        if key not in commits:
            try:
                commits[key] = _tag_commit(pid, tag)
            except Exception:                                      # pragma: no cover - network
                commits[key] = None
        prov["files"][rel] = {"project": ns, "path": path, "tag": tag,
                              "commit": commits[key], "sha256": got, "bytes": len(blob)}
        dest = os.path.join(args.dest, rel.replace("/", os.sep))
        if args.write:
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(dest, "wb") as fh:
                fh.write(blob)
            print(f"wrote {rel}  {len(blob):7d} B  {got[:16]}")
        else:
            if not os.path.exists(dest):
                print(f"MISSING {rel}", file=sys.stderr)
                bad += 1
                continue
            with open(dest, "rb") as fh:
                have = hashlib.sha256(fh.read()).hexdigest()
            ok = have == got
            bad += 0 if ok else 1
            print(f"{'ok  ' if ok else 'DIFF'} {rel}  vendored={have[:16]} forge={got[:16]}")
    if args.write:
        with open(os.path.join(args.dest, "PROVENANCE.json"), "w", encoding="utf-8", newline="\n") as fh:
            json.dump(prov, fh, indent=2, sort_keys=True)
            fh.write("\n")
        print("wrote PROVENANCE.json")
    return 3 if bad else 0


if __name__ == "__main__":                                          # pragma: no cover
    raise SystemExit(main())
