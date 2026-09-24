# ETSI ITS ASN.1 modules — provenance

These are the ASN.1 modules `crates/v2xw-msg/build.rs` compiles into Rust with
`rasn-compiler`. They are **committed on purpose**: every file here comes from the ETSI ITS
ASN.1 forge (<https://forge.etsi.org/rep/ITS/asn1>), every repository there ships a
BSD-3-Clause licence, and a copy of that licence sits next to the modules it covers.

Nothing under this directory may be edited. The build reads these files; a change to one
changes every encoded message in the engine, and a run's manifest pins the modules by the
hashes below.

Build decision references are to `docs/design/12-build-decisions.md`.

---

## 1. Why these and not others

| Decision | Reason |
|---|---|
| CAM is **TS 103 900 (Release 2)**, not EN 302 637-2 (Release 1) | Release 1 CAM imports `ITS-Container`, the Release 1 dictionary. Release 2 CAM imports `ETSI-ITS-CDD`. The two dictionaries give different spellings and different encodings of shared data elements, so a build must pick one family; this one is Release 2 throughout. |
| DENM is **TS 103 831 (Release 2)** | Same reason. Note the ASN.1 module inside `DENM-PDU-Descriptions.asn` is named `DENM-PDU-Description`, singular — upstream's spelling, which the generated Rust module name follows. |
| IEEE 1609.2 comes from the **ETSI forge mirror**, not the copy bundled with SAE J2735 | The J2735 mirror's copies differ in line endings and comments, its `EtsiTs103097ExtensionModule.asn` is an older revision, and the whole J2735 bundle is unlicensed (build decision D3). The forge copies carry the forge's BSD-3-Clause repository licence. |
| **No** ETSI TS 102 941 (PKI) | `rasn-compiler` 0.16 cannot parse `WITH COMPONENTS` inner subtyping on a CHOICE (`EtsiTs102941MessagesItss.asn:105:6`). PKI is a Phase 4 need, so it is deferred rather than worked around (build decision D5). |
| VAM is **TS 103 300-3 V2.2.1** (forge `master`) | The VRU devices' awareness message. It imports `ETSI-ITS-CDD` major-version-3 `WITH SUCCESSORS`, which the Release 2 CDD here (major-version-4) satisfies: every one of its 34 imported types is defined in it. Added to the `FACILITIES` unit on 2026-09-23. |
| **No** CPM, TS 103 301 | Out of this crate's current scope. CPM generates and compiles cleanly and can be added by extending the `FACILITIES` unit in `build.rs`; TS 103 301 does **not** compile (it shares the J2735 regional-extension problem and needs the ISO TS 19321 IVI module, which ETSI does not publish). |
| **No** SAE J2735 anywhere in this repository | Its embedded licence forbids redistribution, and the generated Rust does not compile anyway (build decisions D2 and D3). `third_party/asn1/j2735/` is git-ignored. |

## 2. The CP1252 problem and the `normalized-utf8/` copies

Three upstream modules are **not valid UTF-8**: they contain CP1252 bytes inside comments
(an acute accent in the CDD, curly quotes in the IEEE text). `rasn-compiler` reads sources
as UTF-8 and aborts with `Failed to read ASN.1 source. stream did not contain valid UTF-8`.

Both forms are committed: the byte-exact original, so the hash matches upstream and the
provenance is checkable, and a UTF-8 copy under `normalized-utf8/`, which is what the build
actually reads (build decision D4).

Each normalized copy is **exactly** the CP1252-to-UTF-8 transcode of its original, which is
one command to check:

    iconv -f CP1252 -t UTF-8 cdd_ts102894_2/ETSI-ITS-CDD.asn \
      | diff - normalized-utf8/cdd_ts102894_2/ETSI-ITS-CDD.asn

(and likewise for the two IEEE modules; all three produce no output). Every non-ASCII
character in all three files sits inside a comment — 10 such lines in the CDD, 8 in
`Ieee1609Dot2`, 5 in `Ieee1609Dot2BaseTypes`, none of them a declaration, a constraint or a
value — so the transcode cannot have changed what the modules mean.

| Module | Original sha256 / bytes | `normalized-utf8/` sha256 / bytes |
|---|---|---|
| `cdd_ts102894_2/ETSI-ITS-CDD.asn` | `a15d9e7d1f498e0a382d81b03051c666b20eea31f52ae55c0abd3e49f905a70a` / 369 531 | `673bc7b374719336c1d2783f285d18171f4f71dbe55a93a78f0185c45dfc352f` / 369 565 |
| `ieee1609.2/Ieee1609Dot2.asn` | `82b5e35cbaadae1c6b2f087626b10d52afc73779c9ac6700c6445830d8824e0b` / 72 608 | `24d1f1b64ec853462fe31082e63618b5bfb1a293a69f8ff80a607e7432618a62` / 72 629 |
| `ieee1609.2/Ieee1609Dot2BaseTypes.asn` | `bf2b3d66d394449319323f8a59d8084d963c3ad55d6790e1436ab897221690fe` / 62 490 | `076dd9818f691b1c892fae16309bab5686edd37d5eafb1eb705310f73a76fc30` / 62 502 |

## 3. The files

Every row is pinned to the upstream commit that was fetched on 2026-09-18, so a re-fetch is
reproducible. `sha256` is of the file as committed here.

### 3.1 Common Data Dictionary — ETSI TS 102 894-2, Release 2

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `cdd_ts102894_2/ETSI-ITS-CDD.asn` | `ETSI-ITS-CDD` | 369 531 | `a15d9e7d1f498e0a382d81b03051c666b20eea31f52ae55c0abd3e49f905a70a` | <https://forge.etsi.org/rep/ITS/asn1/cdd_ts102894_2/-/raw/release2/ETSI-ITS-CDD.asn> (commit `615593c445d0`, 2026-08-31) |
| `normalized-utf8/cdd_ts102894_2/ETSI-ITS-CDD.asn` | same, transcoded | 369 565 | `673bc7b374719336c1d2783f285d18171f4f71dbe55a93a78f0185c45dfc352f` | derived from the row above (CP1252 → UTF-8) |
| `cdd_ts102894_2/LICENSE.txt` | — | 1 475 | `8f102e377f1ad7ed9fa90d9dd6cb9ab75e071949dcdb8cb413a10dc98d80b9d4` | repository `LICENSE`, BSD-3-Clause, "Copyright 2019 ETSI" |

### 3.2 CAM — ETSI TS 103 900, Release 2

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `cam_ts103900/CAM-PDU-Descriptions.asn` | `CAM-PDU-Descriptions` | 22 643 | `4abce4126042b6f702c1f80ff1dfe9f9cc1248950c9f505d68f8b79dac418800` | <https://forge.etsi.org/rep/ITS/asn1/cam_ts103900/-/raw/release2/CAM-PDU-Descriptions.asn> (commit `649ada78da45`, 2026-03-16) |
| `cam_ts103900/LICENSE.txt` | — | 1 475 | `ef1f3c32cdbba894937f2d394c7875d2a1999475c4e50404d71861fd2d9b7cb4` | repository `LICENSE`, BSD-3-Clause, "Copyright 2024 ETSI" |

### 3.3 DENM — ETSI TS 103 831, Release 2

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `denm_ts103831/DENM-PDU-Descriptions.asn` | `DENM-PDU-Description` | 19 606 | `c0fa1aef4cf89606582e6322064cebc9fcae4a8a702aa6d7ff6c7098b1af716f` | <https://forge.etsi.org/rep/ITS/asn1/denm_ts103831/-/raw/release2/DENM-PDU-Descriptions.asn> (commit `58472e2644a6`, 2025-10-17) |
| `denm_ts103831/LICENSE.txt` | — | 1 476 | `a11e927c092fd9dd2de2129a76b3186f092760a06aee561a73425a79e9c60668` | repository `LICENSE`, BSD-3-Clause, "Copyright 2022 ETSI" |

### 3.3a VAM — ETSI TS 103 300-3 V2.2.1

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `vam_ts103300_3/VAM-PDU-Descriptions.asn` | `VAM-PDU-Descriptions` | 12 589 | `5631c6d2534a7883fe329f9fe40f45b039e3c9d0611560ea7ef20d3a9ba3d11f` | <https://forge.etsi.org/rep/ITS/asn1/vam-ts103300_3> file `VAM-PDU-Descriptions.asn`, branch `master` (commit `c6db4d084c27`, 2023-01-24), fetched 2026-09-23 |
| `vam_ts103300_3/LICENSE.txt` | — | 1 476 | `affd06519a9ec9ad6f0a3f6457a3772a12d609632e91ddfeb0ea5cab94b553e5` | repository `LICENSE`, BSD-3-Clause, "Copyright 2020 ETSI" |

The file is UTF-8 as fetched, so it needs no `normalized-utf8/` copy. It still lists
`SequenceOfTrajectoryInterceptionIndication` twice in its `IMPORTS`; `rasn-compiler` 0.16
generates and compiles it regardless, so no patch is carried for it (the build would fail if
that stopped being true).

### 3.4 Security header — ETSI TS 103 097, Release 2

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `sec_ts103097/EtsiTs103097Module.asn` | `EtsiTs103097Module` | 5 891 | `245a3c10c176497f658c6a4d9ec804d3226dda2ac10a4351e382ddb5afe9ff72` | <https://forge.etsi.org/rep/ITS/asn1/sec_ts103097/-/raw/release2/EtsiTs103097Module.asn> (commit `6e01cab9f15c`, 2025-11-30) |
| `sec_ts103097/EtsiTs103097ExtensionModule.asn` | `EtsiTs103097ExtensionModule` | 1 717 | `b94c9b373567dd9bfb15d61c8c206d5630f6b0c481ef41ddd574c96784a9eb1a` | <https://forge.etsi.org/rep/ITS/asn1/sec_ts103097/-/raw/release2/EtsiTs103097ExtensionModule.asn> (commit `6e01cab9f15c`, 2025-11-30) |
| `sec_ts103097/LICENSE.txt` | — | 1 475 | `cef08f53c72a0871b711a77a725182ea2b0aac57e394c5f6d81dd10a00e58024` | repository `LICENSE`, BSD-3-Clause, "Copyright 2020 ETSI" |

### 3.5 IEEE 1609.2 — ETSI forge mirror (`ieee` branch)

| File | ASN.1 module | Bytes | sha256 | Source (pinned) |
|---|---|---:|---|---|
| `ieee1609.2/Ieee1609Dot2.asn` | `Ieee1609Dot2` | 72 608 | `82b5e35cbaadae1c6b2f087626b10d52afc73779c9ac6700c6445830d8824e0b` | <https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/-/raw/ieee/Ieee1609Dot2.asn> (commit `77e2c822a11b`, 2025-11-30) |
| `ieee1609.2/Ieee1609Dot2BaseTypes.asn` | `Ieee1609Dot2BaseTypes` | 62 490 | `bf2b3d66d394449319323f8a59d8084d963c3ad55d6790e1436ab897221690fe` | <https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/-/raw/ieee/Ieee1609Dot2BaseTypes.asn> (commit `77e2c822a11b`, 2025-11-30) |
| `ieee1609.2/Ieee1609Dot2Crl.asn` | `Ieee1609Dot2Crl` | 2 378 | `8a8545f8c712671c689a223f8a342711c2571287bb83d11669ea4e61084b3484` | <https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/-/raw/ieee/Ieee1609Dot2Crl.asn> (commit `77e2c822a11b`, 2025-11-30) |
| `ieee1609.2/Ieee1609Dot2CrlBaseTypes.asn` | `Ieee1609Dot2CrlBaseTypes` | 22 089 | `1c6833702820c9f00718b0b3b49890b2ae7c1fd34e787125490964713c1b7ba9` | <https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/-/raw/ieee/Ieee1609Dot2CrlBaseTypes.asn> (commit `77e2c822a11b`, 2025-11-30) |
| `normalized-utf8/ieee1609.2/Ieee1609Dot2.asn` | same, transcoded | 72 629 | `24d1f1b64ec853462fe31082e63618b5bfb1a293a69f8ff80a607e7432618a62` | derived (CP1252 → UTF-8) |
| `normalized-utf8/ieee1609.2/Ieee1609Dot2BaseTypes.asn` | same, transcoded | 62 502 | `076dd9818f691b1c892fae16309bab5686edd37d5eafb1eb705310f73a76fc30` | derived (CP1252 → UTF-8) |

**Licence note for this directory.** Unlike the other four, no `LICENSE` file was captured
next to these modules when they were fetched. The ETSI forge `ieee1609.2` repository carries
the same BSD-3-Clause repository licence as its siblings (see
<https://forge.etsi.org/rep/ITS/asn1/ieee1609.2>), which is why they are treated the same
way here; the *module text itself* is IEEE copyright, republished by ETSI so that
TS 103 097 can import it. No licence file has been invented to fill the gap: this paragraph
is the record, and anyone republishing this repository should fetch the repository's own
`LICENSE` rather than assume the text above.

## 4. Patches

`crates/v2xw-msg/patches/` holds fixes for known defects, applied to the **generated Rust**,
not to the ASN.1 above (build decision D5). Each patch file names the defect it works
around, and the build fails if a patch stops applying, so an upstream fix is noticed rather
than silently ignored.

| Patch | Defect |
|---|---|
| `0001-ieee1609dot2-endentitytype-default.patch` | `rasn-compiler` 0.16 emits `EndEntityType([true, false].into_iter().collect())` for `PsidGroupPermissions.eeType DEFAULT {app}`; `FixedBitString<8>` is a `BitArray` and has no `FromIterator<bool>`. |

Build decision D5 also named the duplicated `SequenceOfTrajectoryInterceptionIndication`
import in `VAM-PDU-Descriptions.asn`; with VAM generated (2026-09-23) it turned out to need
no patch. The TS 102 941 `WITH COMPONENTS` parse failure is still out of scope (PKI is not
generated); adding that module means adding its patch.
