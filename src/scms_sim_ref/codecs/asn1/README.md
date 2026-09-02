# Vendored ETSI ITS ASN.1 modules

These are the **published** ETSI ITS ASN.1 modules, fetched from the ETSI Forge at pinned tags and
committed here unmodified. `scms_sim_ref/codecs/etsi.py` compiles them with `asn1tools` to encode
CAM / DENM / VAM to real UPER octets.

## Licence — why vendoring is permitted, stated explicitly

Every ETSI Forge ASN.1 repository ships a `LICENSE` that is verbatim **BSD-3-Clause**
("Copyright 2019 ETSI", "Copyright 2020 ETSI" for VAM). Clause 1 permits redistribution in source
form provided the copyright notice, the list of conditions and the disclaimer are retained. Each
`.asn` therefore sits in a directory next to that repository's own `LICENSE` file, byte-identical
to the upstream copy.

This is the *opposite* of the SAVeNoW/Ingolstadt loop counts (`tools/fetch_ingolstadt_counts.py`),
whose licence is **not formally stated** and which are consequently cached into a git-ignored
directory and never committed. The distinction is deliberate: the rule is "vendor only what the
licence permits", not "vendor what is convenient".

## Provenance

`PROVENANCE.json` records, for every file: the Forge project path, the file path, the git **tag**,
the **commit sha** that tag resolved to, the file's **sha256** and its byte length. A tag is a
mutable name; the commit is not, and the sha256 is not.

```
python tools/fetch_etsi_asn1.py --check     # re-fetch and compare against these bytes
python tools/fetch_etsi_asn1.py --write     # re-materialise the tree from the Forge
```

`tests/test_message_codec.py::test_vendored_asn1_is_bsd3_and_pinned` re-hashes every file offline
on every test run, so a silent edit here fails the suite.

## What is here

| directory | standard | module | tag |
|---|---|---|---|
| `cdd_v1.3.1/` | ETSI TS 102 894-2 V1.3.1 (CDD, Release 1) | `ITS-Container` | `v1.3.1` |
| `cdd_v2.1.1/` | ETSI TS 102 894-2 V2.1.1 (CDD, Release 2) | `ETSI-ITS-CDD` | `v2.1.1` |
| `cam_en302637_2_v1.4.1/` | ETSI EN 302 637-2 V1.4.1 (CAM) | `CAM-PDU-Descriptions` | `v1.4.1` |
| `denm_en302637_3_v1.3.1/` | ETSI EN 302 637-3 V1.3.1 (DENM) | `DENM-PDU-Descriptions` | `v1.3.1` |
| `vam_ts103300_3_v2.3.1/` | ETSI TS 103 300-3 V2.3.1 (VAM) | `VAM-PDU-Descriptions` | `v2.3.1` |

**Two CDD generations, and both are needed.** CAM R1 and DENM R1 import from `ITS-Container`;
VAM imports from `ETSI-ITS-CDD`, which is a different module with different type names
(`ReferencePositionWithConfidence` rather than `ReferencePosition`, `Wgs84AngleValue` rather than
`HeadingValue`, `SemiAxisLength` with `doNotUse(0)` added). The CDD tag each PDU module needs is
not a choice: it is the `cdd` **git submodule commit** recorded at that PDU module's own tag —
`2d2450e7` = `v1.3.1` for CAM/DENM, `55ae879d` = `v2.1.1` for VAM. The vendored pairs are exactly
those.

**A caveat that must not be buried.** `VAM-PDU-Descriptions.asn` at tag `v2.3.1` opens with its own
header line: `-- Draft V0.0.4_2.2.1 ... Modified to import from the CDD module V2.1.1`. The Forge's
published VAM module at that tag is a draft that was retargeted at CDD v2.1.1; that is what is
vendored, and `EtsiItsCodec.standards_claim()["caveats"]` says so at run time as well as here.

## Release 1 or Release 2 for CAM?

Release 1 (`EN 302 637-2 V1.4.1`). ETSI has since renumbered CAM to **TS 103 900** and DENM to
**TS 103 831**, and those modules are also on the Forge (`cam_ts103900`, `denm_ts103831`) and also
compile. R1 is vendored first because the engine's own refdata (`datagen/refdata/etsi_cam_dcc.json`)
and the MOSAIC-side triggering rules cite EN 302 637-2, so encoding to R1 keeps the message layer
and the generation layer describing the same document. Adding the R2 modules is a new directory
plus a new `MODULE_SETS` entry — no code change — and the profile id would be
`etsi_cam_ts103900`, which `api/codec.py` already lists as an example `profile_id`.
