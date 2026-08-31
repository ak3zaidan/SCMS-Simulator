# Standards-compliance audit (2026-08-30)

Commissioned after the directive that the simulator "should be compliant with the standards". This
is the evidence-based verdict, not a claim inventory. **The headline: the SCMS *structure* is real,
but the message and security layers are not standards-compliant, and the manifest currently claims
more than the code supports.**

## What is genuinely there

| Element | Evidence |
|---|---|
| **CAMP SCP2 linkage values** — real, wired, asserted | `scms_core/linkage.py`: seed hash-chain `ls_x(i)=trunc128(SHA256(la_id‖ls‖0^112))` (:71-77), Davies-Meyer AES pre-linkage trunc-72 (:90-97), `lv = plv1 XOR plv2` (:100-104), forward-only `CrlLinkageEntry.matches` (:156-172). Used in the dataset path (`run.py:2110`, 2700-2701) and assert-checked at `run.py:3655-3658`. Java mirror `crypto/LinkageEngine.java`. |
| **SCMS entity separation** — structural, not cosmetic | Python: LinkageAuthority / PseudonymCA / RegistrationAuthority, with the RA the *only* holder of `request_hash → true_id` (`run.py:1015-1016`). Java models 14 entities (`Scms.java:31-53`) incl. RootCA, ICA, Electors, DCM, ECA, LA1/LA2, MA, CRLGenerator, LocationObscurerProxy. |
| **`hashed_id8`** — the one real IEEE 1609.2 primitive | `crypto_abstract.py:58-60` (SHA-256 low 8 bytes), used at `run.py:2100, 2155, 2193, 2348`. |
| **ETSI EN 302 637-2 CAM triggering** — but Java only | `ScmsBeaconApp.java:229-233`: `dt ≥ CAM_INTERVAL_S ‖ Δpos > 4 m ‖ Δheading > 4° ‖ Δspeed > 0.5 m/s`, floored at 0.1 s. |
| **ETSI TS 102 687 reactive DCC** — competent, Java only, default off | `radio/Dcc.java`: exact ETSI step table (:48-49), 802.11p 10 MHz airtime `40 µs + ceil((16+8L+6)/N_DBPS)·8 µs` (:99-104), 100 ms probes with drift-free boundaries (:128-150), NDL_minDccSampling hold (:189-199). |
| **Externalised standards constants** | `datagen/refdata/etsi_cam_dcc.json` pins the CAM interval range, the 4 m/4°/0.5 m/s triggers, the CBR definition (100 ms, −85 dBm) and the DCC table, each with citation and stated disagreements. |
| **A natural detector output contract** | `records.py:104` already carries `detector_outputs[{check_id, score, verdict}]` plus `detnorm_<key>` fan-out (`run.py:2641-2642`). |

## What is missing, and it is load-bearing

**1. No signing happens. Anywhere.** `run.py` imports `crypto_abstract` but calls only
`keypair_from_seed` / `public_bytes` / `hashed_id8` / `canonical_bytes` — there are **zero** calls to
`ca.sign(` or `ca.verify(`. Signature validity is a boolean `sig_ok` set by the attack switch
(`run.py:3142, 3156-3157`) and read as a flag (`:3395`). The Java layer has no cryptography at all:
`send(...)` passes a literal `true` for `sigValid` (`ScmsBeaconApp.java:294`), and certificate
digests are `hex(sha("cert|"+MASTER_SEED+"|"+unitId), 8)` (`ScmsBackend.java:312`) — a hash of a
*string*, not of a public key. Grepping the Java tree for `ECDSA|secp256|Signature` returns nothing.

**2. Nothing is byte-encodable.** Grepping `asn1|uper|oer|psid|itsAid` across `src/` and `scms-sim/`
returns **zero hits**. There is no message class, no container model, no codec. A CAM is a bare dict
built at one site (`run.py:3186-3189`); the Java equivalent is a 12-field `SignedCam.java`. Without
an encoder, no interoperability claim is testable even in principle.

**3. Butterfly key expansion is orphaned.** `scms_core/butterfly.py` implements CAMP SCP1 correctly
(f1/f2 AES-128, RA cocoon `B = A + f1·G`, `Q = P + f2·G`, PCA certify, device re-derivation) — and
nothing in the dataset path imports it. Actual provisioning is
`pk = ca.keypair_from_seed(cfg.derive(f"key:{vid}:{k}"))` (`run.py:2098-2100`), i.e. every pseudonym
of a device derives from **one label** — the exact opposite of the unlinkability butterfly provides.
`scms_core/ec.py` (real secp256r1) is likewise orphaned.

**4. Certificates are not certificates.** A hex string plus `{i, j, lv, valid_from, valid_to,
issuing_pca}` (`run.py:2109-2111`). No `ToBeSignedCertificate`, issuer, `cracaId`, `crlSeries`,
region, `assuranceLevel`, `appPermissions`/`PsidSsp`, or `verifyKeyIndicator`. The linkage value is
real but lives in a side dict rather than inside a certificate structure.

**5. Units and frames are engine-private and mutually incompatible.** Python uses degrees CCW from
East, local metres, float m/s, float seconds. The Java/SUMO side uses degrees **CW from North**.
ETSI requires 0.1° CW from North, WGS84 in 1/10 µdeg, 0.01 m/s, and `generationDeltaTime` as
ms mod 65536 since the 2004 TAI epoch. Any standards profile is therefore also a unit-conversion layer.

**6. Position confidence is a scalar** — `conf = 2.448·sqrt(σ² + bias_x² + bias_y²)`
(`run.py:2427`) — not ETSI's `PosConfidenceEllipse{semiMajor, semiMinor, semiMajorOrientation}`.

**7. `station_type` is two-valued** (`"vehicle"|"vru"`, opt-in) rather than the ETSI `StationType`
enum 0..15. The real fleet classes (car/motorcycle/truck/bus) are ORACLE-only and never reach the wire.

**8. CAM emission in Python is a fixed per-step loop**, not a triggering rule: one broadcast per
active vehicle per step at `dt = 1.0` — a rigid 1 Hz. The 4 m/4°/0.5 m/s thresholds are pinned in
refdata but have **no consumer in Python**.

## The claim that must change — **DONE, 2026-08-30**

Landed as PLUGIN-ARCHITECTURE.md phase 0. The Python engine now emits `run.STANDARDS_PROFILE`
(five keys: `linkage` / `cert` / `security_envelope` / `message` / `report`) and the Java backend
emits the matching block plus two claims that are genuinely stronger there (EN 302 637-2 CAM
generation rules, TS 102 687 reactive DCC). `manifest.json` is excluded from `data_digest_sha256`
by construction, so the correction moved **zero** digests. `DATASHEET.md`, `README.md`,
`docs/FEATURES.md` and `schemas/records.py:MaReport` were corrected in the same change, split into
IMPLEMENTED / INSPIRED-BY / ABSENT. The analysis below is preserved as the record of why.

`run.py:3794-3795` and `ScmsBackend.java:1028-1029` used to write
`standards_profile = {"report": "ETSI TS 103 759 (shape)", "cert": "IEEE 1609.2", "linkage": "CAMP SCP2"}`.

- `linkage: CAMP SCP2` — **supportable**, the implementation is real and asserted.
- `report: ETSI TS 103 759 (shape)` — defensible *because it says "(shape)"*, but should name which
  fields correspond and which do not.
- `cert: IEEE 1609.2` — **not supportable**. There is no 1609.2 certificate structure and no
  signature. `hashed_id8` alone does not make a certificate profile. This must be downgraded to
  something like `"cert": "hashed_id8 identifier only; not a 1609.2 certificate"` until the
  structure exists.

## What this means for the directive

"Standards-compliant" is a substantially larger programme than adding plugin seams, and the two are
related: the honest path is to make the **message layer** a plugin boundary — a `MessageCodec` /
standards-profile plugin that owns field names, units, coordinate frames and (optionally) real
ASN.1 UPER encoding — so that the internal simulation keeps its private representation while the
wire format becomes swappable and testable against the real ASN.1 modules.

Priority order implied by this audit:

1. **Correct the manifest claim** (cheap, immediate, protects credibility).
2. **Real signing** — wire `butterfly.py` and `ec.py` into the provisioning path so pseudonyms are
   genuinely unlinkable and `sig_ok` reflects an actual verification. The code already exists and is
   tested; it is simply not connected.
3. **A `MessageCodec` plugin seam** with an ETSI profile: units, coordinate frames, `StationType`,
   `PosConfidenceEllipse`, `generationDeltaTime`.
4. **ASN.1 UPER encoding** against the published ETSI modules (`asn1tools` is pure Python), which is
   what makes any interoperability claim testable.
5. **ETSI TS 103 759 alignment** for the misbehaviour report — the most relevant standard to this
   project's actual output.
6. **Port CAM triggering and DCC to the Python engine** (both already exist in Java).
