# R4 — V2X message sets, generation rules, security envelopes, encoded sizes

Cited fact sheet for the V2X simulator design. Compiled 2026-09-17.

Legend for the "Status" column:
- **VERIFIED** — value read directly from the cited primary document (standard text, ASN.1 module, or paper), full text extracted locally from the PDF at the cited URL.
- **VERIFIED (secondary)** — value read from a cited paper/report that itself cites the standard; the standard is paywalled (SAE, IEEE 1609.3).
- **DERIVED** — computed here from cited ASN.1 modules + encoding rules; the arithmetic is shown. Not a measurement.
- **UNVERIFIED** — could not be confirmed from any source accessible in this session. Do not rely on it without checking.

Encoding note that matters for every size below: ETSI facilities-layer messages (CAM/DENM/CPM/VAM/SPATEM/MAPEM) and SAE J2735 messages are ASN.1 **UPER**; the IEEE 1609.2 / ETSI TS 103 097 security envelope and certificates are ASN.1 **COER** (ITU-T X.696). Source: ETSI TS 103 097 V2.1.1 §4.1 ("shall be encoded using the Canonical Octet Encoding Rules (COER) as defined in Recommendation ITU-T X.696"); TS 103 324 §6.1.3.1 ("ASN.1 UPER encoded CPM").

---

## A. Message generation rules

### A.1 SAE J2945/1 — BSM

J2945/1 itself is paywalled (editions: J2945/1_201603, J2945/1_202004, J2945/1B_202212 for non-light-duty vehicles and motorcycles — SAE catalogue pages listed in Sources). Values below come from papers that reproduce the standard's parameters.

| Parameter | Value | Source | Status |
|---|---|---|---|
| Nominal BSM rate | 10 Hz (one BSM at least every 100 ms) | Twardokus et al., NDSS 2024 §II ("broadcasts a digitally signed BSM at least once every 100 ms"); Bindel et al., NIST PQC 2021 slides p.5 ("10 BSMs per second"); Rostami/Krishnan/Gruteser 2018 (Rutgers/GM) abstract ("constant 10 Hz message rate ... 20 dBm") | VERIFIED (secondary) |
| vMaxITT (max inter-transmit time under congestion control) | 600 ms | Rostami et al. 2018, Table 1 ("parameter values used in the congestion control algorithm from the SAE J2945/1 standard") | VERIFIED (secondary) |
| vMinITT | 100 ms | implied by 10 Hz nominal; not explicitly in accessible text | UNVERIFIED |
| vRPMax / vRPMin / vRP (radiated power range, default) | 20 dBm / 10 dBm / 15 dBm | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| vMaxCU / vMinCU (channel-utilisation bounds for rate control) | 80 % / 50 % | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| vSUPRAGain | 0.5 | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| vTEMin / vTEMax (tracking-error thresholds) | 0.2 m / 0.5 m | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| vCBPMeasInt / vTxRateCntrInt | 100 ms / 100 ms | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| λ (smoothing), B (density coefficient) | 0.5, 25 | Rostami et al. 2018, Table 1 | VERIFIED (secondary) |
| CertAttachInt (full certificate vs digest) | 450 ms | Rostami et al. 2018, Table 1 and text ("time interval between attaching full certificate in the BSM versus a certificate digest is called CertAttachInt") | VERIFIED (secondary) |
| Full certificate cadence "per industry standards" | every 5th SPDU (= every 500 ms at 10 Hz); digest in the other 80 % | NDSS 2024 §II-A and §IV-A (citing their ref [39]) | VERIFIED (secondary) |
| vPERRange / vPERInterval / vPERMax / vPERSubInterval / vRescheduleThreshold / vDensityWeightFactor / vTxRand | not found in accessible sources | — | UNVERIFIED |
| Part I in every BSM; Part II VehicleSafetyExtensions with PathHistory and PathPrediction in every BSM; PathHistory ~300 m / max 23 points; events/lights conditional | J2945/1 §6.3.x (paywalled) — not confirmable this session. The only open hit was a USPTO patent text (US 10553112) stating each BSM includes Part I plus Part II VehicleSafetyExtension with PathHistory/PathPrediction | UNVERIFIED |

### A.2 ETSI EN 302 637-2 V1.4.1 (2019-04) — CAM (clause 6.1.3 unless noted)

| Parameter | Value | Source | Status |
|---|---|---|---|
| T_GenCamMin | 100 ms ("corresponds to ... 10 Hz") | EN 302 637-2 V1.4.1 §6.1.3 | VERIFIED |
| T_GenCamMax | 1 000 ms ("corresponds to ... 1 Hz") | §6.1.3 | VERIFIED |
| T_CheckCamGen | ≤ T_GenCamMin ("conditions ... shall be checked repeatedly every T_CheckCamGen") | §6.1.3; Annex C.2.2 | VERIFIED |
| T_GenCam_Dcc (ITS-G5 only) | minimum interval from DCC (TS 102 724); range T_GenCamMin ≤ T_GenCam_DCC ≤ T_GenCamMax; clamped to Max if above, set to Min if below or not provided; provided by management entity per §5.3.5 in ms | §6.1.3 | VERIFIED |
| LTE-V2X | "DCC and T_GenCam_Dcc are not applicable"; congestion control by access layer (TS 103 574) | §6.1.3 | VERIFIED |
| T_GenCam (current upper limit) | default = T_GenCamMax; set to elapsed time when condition 1 triggers; reset to T_GenCamMax after N_GenCam consecutive CAMs from condition 2 | §6.1.3 | VERIFIED |
| N_GenCam | default and maximum 3 (may be increased e.g. near intersections) | §6.1.3 | VERIFIED |
| Trigger 1 (dynamics, after ≥ T_GenCam_Dcc) | heading change > 4°; position change > 4 m; speed change > 0,5 m/s (vs. last transmitted CAM) | §6.1.3 | VERIFIED |
| Trigger 2 (periodic) | elapsed ≥ T_GenCam (and ≥ T_GenCam_Dcc on ITS-G5) | §6.1.3 | VERIFIED |
| Low-frequency container | in first CAM after activation, then when ≥ 500 ms since last CAM carrying it | §6.1.3 | VERIFIED |
| Special-vehicle container | same rule, ≥ 500 ms | §6.1.3 | VERIFIED |
| RSU CAM interval | ≥ 1 000 ms (max 1 Hz) | §6.1.4 | VERIFIED |
| CAM generation time budget | < 50 ms (trigger to delivery to N&T layer) | §6.1.5.1 | VERIFIED |
| C2C-CC profile: T_GenCam_Dcc = DCC Toff | RS_BSP_293; N_GenCam = pCamGenNumber (RS_BSP_297) | C2C-CC RS 2037 "Vehicle C-ITS station profile" R1.6.10 (2026-07-24) | VERIFIED |
| Measured mean CAM interval in field drives | 0.33–0.47 s | C2C-CC TR 2052 (2018-12-20) Observation #10 | VERIFIED |

Note: EN 302 637-2 §6.2.x only says CAMs are signed with certificates per TS 103 097 and defines the CAM SSP bitmap; the "certificate once per second / digest otherwise" rule is **not** in EN 302 637-2 — it is in ETSI TS 103 097 §7.1.1 (see D).

### A.3 ETSI EN 302 637-3 V1.3.1 (2019-04) — DENM

| Parameter | Value | Source | Status |
|---|---|---|---|
| Trigger / update / repetition / termination | §6.1.2.1–6.1.2.4: new DENM gets unused actionID; update increments referenceTime; repetition only if application supplies repetitionInterval and repetitionDuration ("If any of the above data are not provided ... shall not execute the DENM repetition"); termination = cancellation (originator) or negation (other ITS-S), "transmitted at least once ... may be repeated" | EN 302 637-3 V1.3.1 §6.1.2 | VERIFIED |
| repetitionInterval / repetitionDuration | application-provided; **not carried in the DENM** (NOTE 1, NOTE 3 of §8.2.1.5) | §8.2.1.5 | VERIFIED |
| transmissionInterval / validityDuration | carried in DENM management container; validityDuration is a time offset in seconds since detectionTime; optional | Annex B.55; §5.4.1 tables | VERIFIED |
| Default validityDuration (defaultValidity) | 600 s from detectionTime when not provided | §8.2.1.5 (T_O_Validity), §8.4.1.5 (T_R_Validity), Annex B.10 | VERIFIED |
| T_Repetition | = repetitionInterval; invalid ⇒ transmitted once | §8.2.1.5 | VERIFIED |
| T_RepetitionDuration, T_Repetition bound | shall not be greater than validityDuration | §8.2.1.5 | VERIFIED |
| Keep-alive forwarding (KAF) | optional sub-function; stores received DENM while valid, inside relevance/destination area, not cancelled/negated; forwards most recent referenceTime only | §6.1.4.2 | VERIFIED |
| KAF timers | T_F_Validity = validityDuration from detectionTime (invalid ⇒ no forwarding); T_Forwarding = 2 × transmissionInterval + random 0–150 ms, capped at validityDuration; invalid if transmissionInterval absent | §8.3.2.5 | VERIFIED |
| Security profile | signer shall be **certificate** (never digest); generationLocation shall be present | ETSI TS 103 097 V2.1.1 §7.1.2 | VERIFIED |

### A.4 SPaT / MAP (SPATEM / MAPEM), SRM/SSM (SREM/SSEM), IVIM, RTCMEM

| Parameter | Value | Source | Status |
|---|---|---|---|
| SPATEM generation (EU) | application-triggered; "The SPATEM is not repeated"; no numeric rate in the TS | ETSI TS 103 301 V2.1.1 (2021-03) §5.4.2 | VERIFIED |
| SPATEM BTP port / max latency | port 2 004; CSP_MaxLat 100 ms | TS 103 301 Table 3 (CSP_PortNo, CSP_MaxLat) | VERIFIED |
| MAPEM generation (EU) | content stable; "re-broadcasted continuously"; "shall be transmitted continuously together with the SPATEM"; no numeric rate | TS 103 301 §6.4.2, §6.4.3.1 | VERIFIED |
| MAPEM fragmentation | if size exceeds allowed length (e.g. MTU) the RLT service fragments at facilities layer; fragments identified by "layerID" (ISO/TS 19091 Annex G) | TS 103 301 §6.4.1 | VERIFIED |
| MAPEM port | 2 003 | TS 103 301 Table 8 | VERIFIED |
| IVIM | repeated at pre-defined interval between updates (application-requested); port 2 006 | TS 103 301 §7.4.2, Table | VERIFIED |
| SREM / SSEM | SREM triggered by vehicle application, "may be repeated"; SSEM is the RSU response to SREM, revised on status change, "may be repeated"; SREM port 2 007 | TS 103 301 §8.4.2; CSP_PortNo | VERIFIED |
| SSEM port | 2 008 (TS 103 248) | not in extracted text | UNVERIFIED |
| RTCMEM | application-triggered, "not repeated"; port 2 013 | TS 103 301 §9.4.2 | VERIFIED |
| SPaT rate (US) | connected intersection shall broadcast SPaT at average 10 msg/s ± 1 over 10 s (3.3.3.1.5.1); TSC→RSU 10/s ± 1 over 2 s, never > 0.3 s gap; SPaT latency ≤ 300 ms (3.3.3.1.5.2) | CTI 4501 v01.01 Connected Intersections Implementation Guide | VERIFIED |
| MAP rate (US) | average 1 msg/s ± 1 over 10 s (3.3.3.1.5.3) | CTI 4501 v01.01 | VERIFIED |
| RTCM (US) | MSM4 correction rate tunable 1–10 Hz | CTI 4501 Annex C.1 | VERIFIED |
| J2735 edition used by CTI 4501 | J2735_202007 UPER | CTI 4501 §3.3.2.1.1.3 | VERIFIED |
| ISO/TS 19091 numeric rates | ISO doc paywalled | — | UNVERIFIED |

### A.5 SAE J2945/9_201703 — PSM

| Parameter | Value | Source | Status |
|---|---|---|---|
| Scope | minimum performance requirements for PSM (J2735) from VRU devices (pedestrians, cyclists, public-safety personnel) over DSRC/1609 | SAE J2945/9 catalogue page; ITU-T CITS SAE update slides (Dec 2017) p.8 | VERIFIED (secondary, scope only) |
| PSM transmission-rate rules | not confirmable. A patent text surfaced in search claims "two to five times per second, depending on the speed of the VRU"; PASS paper (Islam et al., arXiv 1907.05284) generates PSMs every 100 ms in their own system, not per standard | — | UNVERIFIED |

### A.6 ETSI TS 103 300-3 V2.2.1 (2023-02) — VAM

| Parameter | Value | Source | Status |
|---|---|---|---|
| T_GenVamMin | 100 ms (Table 16 recommended) | TS 103 300-3 V2.2.1 Table 16, §6.2 | VERIFIED |
| T_GenVamMax | 5 000 ms | Table 16 | VERIFIED |
| T_AssembleVAM | 50 ms | Table 16 | VERIFIED |
| VRU low-frequency container | first VAM, then every ≥ 2 000 ms | §6.2 | VERIFIED |
| T_CheckVamGen | ≤ T_GenVamMin | §6.2, Annex C.2.2 | VERIFIED |
| T_GenVam (DCC, ITS-G5) | provided by VBS management entity; clamped to [T_GenVamMin, T_GenVamMax]; LTE-V2X PC5 per TS 103 574 | §6.2 | VERIFIED |
| Individual-VAM triggers | elapsed > T_GenVamMax; or position change > minReferencePointPositionChangeThreshold; or speed change > minGroundSpeedChangeThreshold; or velocity-orientation change > minGroundVelocityOrientationChangeThreshold | §6.4.1 | VERIFIED |
| minReferencePointPositionChangeThreshold | 4 m | Table 17 | VERIFIED |
| minGroundSpeedChangeThreshold | ±0,5 m/s | Table 17 | VERIFIED |
| minGroundVelocityOrientationChangeThreshold | ±4° | Table 17 | VERIFIED |
| minTrajectoryInterceptionProbChangeThreshold | 10 % | Table 17 | VERIFIED |
| numSkipVamsForRedundancyMitigation | [2 to 10] (e.g. 4) | Table 17, §6.4.3 | VERIFIED |
| minClusterDistanceChangeThreshold | 2 m | Table 17 | VERIFIED |
| Redundancy mitigation | skip individual VAM if elapsed ≤ numSkip × T_GenVamMax AND a peer VAM already reports position/speed/orientation within the three thresholds; or VRU in protected/pedestrian-only zone, in a cluster, or already reported by another ITS-S within T_GenVam | §6.4.3 | VERIFIED |
| Certificate attachment (individual VAM) | attach certificate if ≥ 1 s since last, or if a "new CAM signer" was observed; else digest | §6.5.3 | VERIFIED |
| Certificate attachment (cluster VAM) | every ≥ 500 ms ("twice a second") | §6.5.3 | VERIFIED |
| Newer edition | TS 103 300-3 V2.3.1 (2025-12) exists (URL in Sources); not checked | — | note |

### A.7 ETSI TS 103 324 V2.1.1 (2023-06) — CPM

| Parameter | Value | Source | Status |
|---|---|---|---|
| Generation events | periodic every T_GenCpm, T_GenCpmMin ≤ T_GenCpm ≤ T_GenCpmMax; each event may yield 0, 1 or several CPMs | TS 103 324 V2.1.1 §6.1.2.1 | VERIFIED |
| T_GenCpmMin | 100 ms (vehicle); RSU may go down to 50 ms | Annex F Table F.1 | VERIFIED |
| T_GenCpmMax | 1 000 ms | Table F.1 | VERIFIED |
| T_AddSensorInformation | 1 000 ms | Table F.1, §6.1.2.2 | VERIFIED |
| MaxPerceptionRegions | 8 | Table F.1 | VERIFIED |
| ObjectInclusionConfig | 1 (rules apply; 0 = sender's own rules) | Table F.1, §6.1.2.3 | VERIFIED |
| ObjectPerceptionQualityThreshold | 3 | Table F.1 | VERIFIED |
| minPositionChangeThreshold | 4 m | Table F.1 | VERIFIED |
| minGroundSpeedChangeThreshold | 0,5 m/s | Table F.1 | VERIFIED |
| minGroundVelocityOrientationChangeThreshold | 4° | Table F.1 | VERIFIED |
| Type-A objects (pedestrian, bicyclist/light VRU, animal, group, other) | include if first detected since last event; if any Type-A not included for ≥ T_GenCpmMax/2, include all Type-A | §6.1.2.3 | VERIFIED |
| Type-B objects (vehicles, motorcyclists) | include if new, or position/speed/orientation thresholds exceeded, or ≥ T_GenCpmMax since last inclusion; optional look-ahead prediction to next event | §6.1.2.3 | VERIFIED |
| Size limit | UPER CPM ≤ MTU_CPM = MTU_AL − HD_CPM − HD_NT; per channel with MCO | §6.1.3.1 | VERIFIED |
| Segmentation | data assembled into independently interpretable segments; messageSegmentInfo in management container when > 1 CPM assembled | §6.1.2.1 NOTE, §7.1.x | VERIFIED |
| GN maximum packet lifetime for CPM | ≤ 1 000 ms | §5.3.3 table | VERIFIED |
| Segmentation threshold used in ETSI simulations | 1 100 bytes | ETSI TR 103 562 V2.1.1 §5.5.x ("segmentation threshold of 1 100 bytes") | VERIFIED |

### A.8 IEEE 1609.3 — WSA

| Parameter | Value | Source | Status |
|---|---|---|---|
| WSA ASN.1 (module IEEE-1609-3-WSA version0) | SrvAdvMsg ::= SEQUENCE { version SrvAdvPrtVersion, body SrvAdvBody }; messageID always 0; c-rsvAdvPrtVersionNo INTEGER(0..7) ::= 3; ServiceInfo { serviceID VarLengthNumber, channelIndex, chOptions }; ChannelInfo { operatingClass, channelNumber, powerLevel, dataRate (adaptable BIT STRING(1) + INTEGER(0..127)), extensions }; RoutingAdvertisement { lifetime, ipPrefix, ipPrefixLength, defaultGateway, primaryDns, extensions } | TCI_ASN1 copy of IEEE 1609.3 ASN.1: `.../1609dot3/wsa.asn`, `wee.asn` | VERIFIED |
| RepeatRate type | INTEGER (0..255) | wee.asn | VERIFIED |
| RepeatRate semantics | 1609.3 defines it as the number of WSA transmissions per 5 s (search snippet only; standard paywalled) | — | UNVERIFIED |
| Network/transport options in 1609.3 | WSMP, UDP/IP, TCP/IP ("three options: a bandwidth efficient single-hop solution known as WSMP, UDP/IP, and TCP/IP") | ARC-IT standard page for IEEE 1609.3-2020/Cor 1 | VERIFIED (secondary) |

---

## B. Encoded sizes (bytes)

| Message / element | Size | What is included | Source | Status |
|---|---|---|---|---|
| BSM Part I only (UPER) | "~39 bytes" | — | no accessible encoder output or paper found | UNVERIFIED |
| BSM SPDU, digest signer | 180 | BSM + 1609.2 signed envelope with HashedId8 | Rostami et al. 2018 Table 1 ("BSM Payload Size (Certificate Digest) 180 bytes") | VERIFIED (secondary) |
| BSM SPDU, full certificate | 250 | as above with implicit pseudonym certificate | Rostami et al. 2018 Table 1 | VERIFIED (secondary) |
| BSM SPDU maximum under 1609.2 | ≤ 226 (implicit cert) / ≤ 330 (explicit cert) | signed BSM SPDU incl. certificate | NDSS 2024 §II-A | VERIFIED (secondary) |
| Certificate-bearing BSM SPDU "under current industry standards" | 330 | explicit-certificate SPDU | NDSS 2024 §IV-B | VERIFIED (secondary) |
| BSM sizes used by IACR ePrint 2022/133 | payload 50–300 + 117-byte pseudonym certificate + 64-byte signature ⇒ 122–481 total (digest 8 bytes in 4 of 5 messages) | cites DOT VSC-A final report | Cominetti et al., ePrint 2022/133 §I | VERIFIED (secondary) |
| BSM packet, V2Verifier testbed (ECDSA P-256) | 250 (BSM data + signature, no public key); 530 with explicit cert | testbed-specific framing | Bindel et al., NIST PQC 2021 slides p.24–27 | VERIFIED (secondary) |
| BSM digest/cert under the pre-ASN.1 1609.2 encoding vs ASN.1 | 182 / 255 | — | "Performance Analysis of Existing 1609.2 Encodings v ASN.1" (ResearchGate 275249582) — page not fetchable, values from search snippet only | UNVERIFIED |
| US BSM planning size | 380 | incl. security and higher-layer overhead | C2C-CC TR 2050 (2020-02-28) Fig. 17 | VERIFIED (planning assumption) |
| CAM, field-measured (ITS-G5, 2018) | mean 297–406 per drive, overall average 357; min 182 (Renault) / 199 (VW); max 500–807; "distribution starts around 190 Bytes"; 30 % < 300, > 50 % > 350, > 30 % > 450 | secured CAM as captured; 63–76 % of CAMs carried a certificate (Table 6-1); whether GN/BTP headers are included is not stated in the extracted text | C2C-CC TR 2052 "Survey on ITS-G5 CAM statistics" (2018-12-20) Tables 6-1, 6-2, Obs. #4–#6 | VERIFIED (layer scope UNVERIFIED) |
| CAM theoretical range | ~200 to 800 | "depending on ... security content like signatures and certificates" | TR 2052 §2 | VERIFIED |
| CAM low-frequency container elements | vehicle role 1 B; exterior lights 1 B; pathHistory 8–9 B per entry | — | TR 2052 §3.1 | VERIFIED |
| Certificate + signature (ETSI) | 100–150 | "Typical sizes of certificates and signatures" | TR 2052 §3.2; repeated by Yoshizawa & Preneel (KU Leuven) citing TR 2052 | VERIFIED |
| CAM planning size (EU) | 400 | incl. security and GN overhead | C2C-CC TR 2050 Annex A (Fig. 10) | VERIFIED (planning assumption) |
| DENM typical | "~300, range 100–800+" | — | only an unreviewed GitHub doc (lf-edge/instantx) surfaced | UNVERIFIED |
| SPaT typical | no measured value found | — | — | UNVERIFIED |
| SPaT + MAP planning size (EU) | 1 200 | "for several I2V messages like SPAT, MAP" incl. security | C2C-CC TR 2050 Fig. 14 | VERIFIED (planning assumption) |
| MAP size ceiling (US) | must be < 2 302 incl. signature, certificate and header; default WSM payload max 1 400 (IEEE 1609.3-2020 MIB), 2 302 supported; C-V2X max 8 000 incl. security | — | CTI 4501 v01.01 §4.3.3.1.3.1 | VERIFIED |
| MAP "1450-byte payload limit" | Virginia MAP guidance doc (403 Forbidden) | — | — | UNVERIFIED |
| CPM container sizes (UPER, mandatory DEs only, 10 000 generated messages) | ITS PDU header + Management + Station Data = 121; per Sensor Information = 35; per Perceived Object = 35 | — | ETSI TR 103 562 V2.1.1 (2019-12) Table 3 | VERIFIED |
| CPM with optional DFs (study S1) | station data ≈ 16 B (with orientation/accelerations/yaw); ≈ 31 B per object (with sensorID, objectAge, confidence, yaw, dimensions, dynamicStatus, classification) | — | TR 103 562 §5.3.2 | VERIFIED |
| CPM planning size (EU) | 1 000 incl. security (750 B payload ≈ 25 objects) | — | C2C-CC TR 2050 Fig. 13 | VERIFIED (planning assumption) |
| VAM planning size | 350 (C2C-CC TR 2050 Fig. 12, incl. security/overhead); Ostendorf/Garlichs/Wolf 2025 use 300 as midpoint between C2C-CC 235 and 5GAA 350 | — | TR 2050; arXiv 2506.22052 §IV (refs [16] C2C-CC, [17] 5GAA) | VERIFIED (planning assumption) |
| PSM | no source | — | — | UNVERIFIED |
| MCM / PCM planning sizes | 1 000 / 400 incl. security | — | C2C-CC TR 2050 Figs. 15–16 | VERIFIED (planning assumption) |

---

## C. IEEE 1609.2 security envelope

Structures quoted from the ETSI-forge mirror of the IEEE ASN.1 (`Ieee1609Dot2.asn`, module `Ieee1609Dot2 ... major-version-2 minor-version-3`, protocolVersion 3; `Ieee1609Dot2BaseTypes.asn` major-version-2 minor-version-2). Encoding: COER.

### C.1 Structures (VERIFIED from ASN.1)

```
Ieee1609Dot2Data ::= SEQUENCE { protocolVersion Uint8(3), content Ieee1609Dot2Content }
Ieee1609Dot2Content ::= CHOICE { unsecuredData Opaque, signedData SignedData,
                                 encryptedData EncryptedData, signedCertificateRequest Opaque, ... }
SignedData ::= SEQUENCE { hashId HashAlgorithm, tbsData ToBeSignedData,
                          signer SignerIdentifier, signature Signature }
ToBeSignedData ::= SEQUENCE { payload SignedDataPayload, headerInfo HeaderInfo }
SignedDataPayload ::= SEQUENCE { data Ieee1609Dot2Data OPTIONAL, extDataHash HashedData OPTIONAL, ... }
HeaderInfo ::= SEQUENCE { psid Psid, generationTime Time64 OPTIONAL, expiryTime Time64 OPTIONAL,
   generationLocation ThreeDLocation OPTIONAL, p2pcdLearningRequest HashedId3 OPTIONAL,
   missingCrlIdentifier MissingCrlIdentifier OPTIONAL, encryptionKey EncryptionKey OPTIONAL, ...,
   inlineP2pcdRequest SequenceOfHashedId3 OPTIONAL, requestedCertificate Certificate OPTIONAL,
   pduFunctionalType PduFunctionalType OPTIONAL, contributedExtensions ContributedExtensionBlocks OPTIONAL }
SignerIdentifier ::= CHOICE { digest HashedId8, certificate SequenceOfCertificate, self NULL, ... }
CertificateBase ::= SEQUENCE { version Uint8(3), type CertificateType, issuer IssuerIdentifier,
                               toBeSigned ToBeSignedCertificate, signature Signature OPTIONAL }
Certificate ::= CertificateBase (ImplicitCertificate | ExplicitCertificate)
IssuerIdentifier ::= CHOICE { sha256AndDigest HashedId8, self HashAlgorithm, ..., sha384AndDigest HashedId8 }
ToBeSignedCertificate ::= SEQUENCE { id CertificateId, cracaId HashedId3, crlSeries CrlSeries,
   validityPeriod ValidityPeriod, region GeographicRegion OPTIONAL, assuranceLevel SubjectAssurance OPTIONAL,
   appPermissions SequenceOfPsidSsp OPTIONAL, certIssuePermissions SequenceOfPsidGroupPermissions OPTIONAL,
   certRequestPermissions SequenceOfPsidGroupPermissions OPTIONAL, canRequestRollover NULL OPTIONAL,
   encryptionKey PublicEncryptionKey OPTIONAL, verifyKeyIndicator VerificationKeyIndicator, ... }
CertificateId ::= CHOICE { linkageData LinkageData, name Hostname, binaryId OCTET STRING(SIZE(1..64)), none NULL, ... }
VerificationKeyIndicator ::= CHOICE { verificationKey PublicVerificationKey, reconstructionValue EccP256CurvePoint, ... }
HashedId3 ::= OCTET STRING (SIZE(3));  HashedId8 ::= OCTET STRING (SIZE(8));  HashedId10 ::= OCTET STRING (SIZE(10))
Time32 ::= Uint32;  Time64 ::= Uint64
ThreeDLocation ::= SEQUENCE { latitude Latitude, longitude Longitude, elevation Elevation }  -- 4 + 4 + 2 octets
EccP256CurvePoint ::= CHOICE { x-only OCTET STRING(SIZE(32)), fill NULL, compressed-y-0 OCTET STRING(SIZE(32)),
                               compressed-y-1 OCTET STRING(SIZE(32)), uncompressedP256 SEQUENCE { x, y OCTET STRING(SIZE(32)) } }
EcdsaP256Signature ::= SEQUENCE { rSig EccP256CurvePoint, sSig OCTET STRING (SIZE (32)) }
Signature ::= CHOICE { ecdsaNistP256Signature EcdsaP256Signature, ecdsaBrainpoolP256r1Signature EcdsaP256Signature, ...,
                       ecdsaBrainpoolP384r1Signature EcdsaP384Signature }
HashAlgorithm ::= ENUMERATED { sha256, ..., sha384 }
Psid ::= INTEGER (0..MAX)
```

### C.2 Field facts and byte sizes

| Item | Value | Source | Status |
|---|---|---|---|
| HashedId8 / HashedId3 computation | SHA-256 of the (canonicalized) COER encoding; **low-order 8 / 3 bytes** ("last eight bytes of the 32-byte hash ... in network byte order"). Example SHA-256("") ⇒ HashedId8 = a495991b7852b855, HashedId3 = 52b855. For certificates the hash input uses compressed EC points and x-only r. | IEEE 1609.2a-2017 §6.3.25, §6.3.26 (full text on ETSI docbox) | VERIFIED |
| Hash algorithms | sha256 (SHA-256) and sha384; 1609.2-2022 adds SM2/SM3/SM4 (Chinese algorithms, CR18) and hash-based signature support (CR23) | base-types ASN.1; W. Whyte, "V2X IEEE 1609.2.1: Status and Deployment" slides (Qualcomm, 2022-08-31) p.12–15 | VERIFIED |
| generationTime | Time64 = Uint64 ⇒ 8 bytes; TS 103 097 requires it "always present" | base-types ASN.1; TS 103 097 V2.1.1 §5.2 | VERIFIED |
| Time64 epoch (µs since 2004-01-01 00:00:00 UTC, TAI) | not confirmable from extracted text | — | UNVERIFIED |
| expiryTime | Time64, 8 bytes, optional | ASN.1 | VERIFIED |
| generationLocation | ThreeDLocation = Latitude (NinetyDegreeInt, 4) + Longitude (OneEightyDegreeInt, 4) + Elevation (Uint16, 2) = 10 bytes | ASN.1 | DERIVED |
| psid | Psid INTEGER(0..MAX) — COER unconstrained integer = 1 length octet + value; BSM PSID 0x20 ⇒ 2 bytes; ITS-AIDs ≥ 128 ⇒ 3 bytes | ASN.1 + X.696 | DERIVED |
| ECDSA-P256 signature encoded | Signature CHOICE tag 1 + rSig (EccP256CurvePoint CHOICE tag 1 + 32) + sSig 32 = **66 bytes** (65 without the outer CHOICE tag). 1609.2a-2017 §6.3.29: point "encoded as an unsigned integer of length 32 octets ... for all values of the CHOICE". | ASN.1; IEEE 1609.2a-2017 §6.3.29; X.696 CHOICE tag rule | DERIVED |
| Signer = digest | CHOICE tag 1 + HashedId8 8 = 9 bytes | ASN.1 | DERIVED |
| Signer = certificate | CHOICE tag 1 + SequenceOfCertificate quantity field 2 + certificate | ASN.1 + X.696 | DERIVED |
| SignedData overhead with digest (excluding payload) | ≈ 93–94 bytes: Ieee1609Dot2Data (1 version + 1 choice tag) + hashId 1 + SignedDataPayload preamble 1 + inner Ieee1609Dot2Data (1 + 1 + 1–2 length) + HeaderInfo (preamble 1 + psid 2 + generationTime 8) + signer 9 + signature 66 | ASN.1 + X.696 | DERIVED |
| SignedData overhead with certificate | ≈ 87 + |certificate| bytes (replace 9-byte digest signer by 3 + cert) | ASN.1 + X.696 | DERIVED |
| Implicit (ECQV) pseudonym certificate, encoded | ≈ 80 bytes for: preamble 1, version 1, type 1, issuer 9, toBeSigned { preamble 1, id linkageData 13 (iCert 2 + linkage-value 9 + tag/preamble 2), cracaId 3, crlSeries 2, validityPeriod 7 (Time32 4 + Duration tag 1 + Uint16 2), appPermissions 8 (2 + one PsidSsp with 1-byte SSP 6), verifyKeyIndicator 34 (tag 1 + point tag 1 + 32) } — no signature. Cross-check: Rostami's 250 − 180 = 70 bytes delta ⇒ cert ≈ 70 + 8 − 2 ≈ 76–80 bytes; VSC-A reports 117 bytes; NDSS reports SPDU ≤ 226 (implicit) | ASN.1 arithmetic; Rostami 2018; ePrint 2022/133 | DERIVED (range 80–120 measured/reported) |
| Explicit certificate, encoded | implicit-size − 34 + 35 (verificationKey: tag 1 + PublicVerificationKey tag 1 + point tag 1 + 32) + 66 (signature) ≈ 147 bytes; NDSS computes ECDSA cert = 30 + |pk| + |sig| = **162 bytes** and a one-certificate P2PCD learning response = **172 bytes**; ECQV cert "at least 64 bytes smaller" than explicit (search snippet, UNVERIFIED) | ASN.1 arithmetic; NDSS 2024 §V-D, §IV-B | DERIVED / VERIFIED (secondary) |
| Frame accounting used by NDSS | cert = 30 + |pk| + |sig_pk|; SPDU = 24 + |BSM| + |cert| + |sig_BSM|; MAC frame = 40 + |SPDU| | NDSS 2024 §V-D | VERIFIED (secondary) |
| Total signed BSM: digest vs certificate | 180 vs 250 (Rostami); ≤ 226 implicit / ≤ 330 explicit (NDSS); 122–481 span (ePrint) | see B | VERIFIED (secondary) |
| "~150 with digest / ~280 with certificate" (task's prior) | not found in any source | — | UNVERIFIED |

---

## D. ETSI TS 103 097 secured message (V2.1.1, 2021-10; V2.2.1, 2026-03)

| Item | Value | Source | Status |
|---|---|---|---|
| Base | data structures "shall be encoded using ... COER"; EtsiTs103097Data-Signed is Ieee1609Dot2Data/signedData with ETSI constraints | TS 103 097 V2.1.1 §4.1, §5.1 | VERIFIED |
| headerInfo constraints (all profiles) | psid = ITS-AID; generationTime always present; expiryTime, generationLocation, encryptionKey, inlineP2pcdRequest, requestedCertificate, pduFunctionalType per profile; p2pcdLearningRequest and missingCrlIdentifier always absent; contributedExtensions only EtsiOriginatingHeaderInfoExtension (EtsiTs102941CrlRequest, EtsiTs102941DeltaCtlRequest for P2P CRL/CTL requests per TS 103 601) | §5.2 | VERIFIED |
| signer | digest (HashedId8) or certificate with exactly one EtsiTs103097Certificate | §5.2 | VERIFIED |
| Header/trailer overhead | identical to IEEE 1609.2 SignedData (no separate ETSI trailer); see C.2 DERIVED figures (≈ 93 bytes with digest; ≈ 87 + AT with certificate) | §5.1–5.2 + C.2 | DERIVED |
| **CAM profile — certificate inclusion rule** | "As default, the choice digest shall be included. The choice certificate shall be included once, one second after the last inclusion of the choice certificate. If the ITS-S receives a CAM signed by a previously unknown AT, it shall include the choice certificate immediately in its next CAM ... the timer ... shall be restarted. If an ITS-S receives a CAM that includes ... inlineP2pcdRequest ... [containing] the currently used authorization ticket ... it shall include the choice certificate immediately in its next CAM" | **TS 103 097 V2.1.1 §7.1.1** (unchanged in V2.2.1 §7.1.1) | VERIFIED |
| CAM profile — inlineP2pcdRequest | shall be included with digests of unknown ATs (received digest unknown) and unknown AA certificates (received cert with unknown issuer) | §7.1.1 | VERIFIED |
| CAM profile — requestedCertificate | if a received inlineP2pcdRequest lists a digest of a valid CA certificate the ITS-S knows, include that CA certificate in the next CAM (unless another CAM already answered, or the own signer is the certificate that round) | §7.1.1 | VERIFIED |
| CAM headerInfo | generationTime present; expiryTime/generationLocation absent ("All other components ... shall not be used and be absent") | §7.1.1 | VERIFIED |
| DENM profile | signer = certificate; generationLocation present | §7.1.2 | VERIFIED |
| Authorization Ticket profile | EtsiTs103097Certificate; issuer sha256AndDigest or sha384AndDigest; appPermissions = message-signing permissions; CertificateId = none; certIssuePermissions absent | §7.2.1 | VERIFIED |
| Enrolment credential / CA certs | explicit type; EC id = name; Root CA issuer = self, certIssuePermissions + CRL/CTL appPermissions; sub-CA carries encryptionKey | §7.2.2–7.2.4 | VERIFIED |
| Version history | V2.1.1 (2020-11/2021-10) "Amended V1.4.1 to support the use of implicit certificates (in addition to explicit)"; V2.2.1 (2026-03) updates reference [1] to IEEE 1609.2:2025 | TS 103 097 V2.2.1 history table | VERIFIED |
| AT encoded size | 100–150 bytes "certificates and signatures" (C2C-CC field survey); implicit AT with CertificateId none, 2 PsidSsp (CAM/DENM), validity, region ⇒ DERIVED ≈ 90–130 bytes | TR 2052 §3.2; ASN.1 arithmetic | VERIFIED (range) / DERIVED |
| Signed CAM sizes | field data: min 182–199 (digest), mean 297–406, max 500–807 (with AT + long path history); average 357 | C2C-CC TR 2052 Table 6-2 | VERIFIED |
| C2C-CC profile extras | RS_BSP_181: on collision of the least-significant 32 bits of the hashedId8 with another valid station's AT, initiate AT change; pSecMessageFutureToleranceTime 220 ms; pSecCamPastToleranceTime 2 s; pSecMaxAcceptDistance 10 km | C2C-CC RS 2037 R1.6.10 | VERIFIED |

---

## E. Certificate-distribution optimisation schemes

| Scheme | Mechanism | Measured / stated overhead | Source | Status |
|---|---|---|---|---|
| IEEE 1609.2 P2PCD (clause 8) | Triggered when a signed SPDU's topmost certificate has an unknown issuer ("trigger SPDU"). Two flavours: **out-of-band** (learning request = HashedId3 in p2pcdLearningRequest of own SPDUs; responses are separate broadcast PDUs from the P2PCD Entity, throttled) and **inline** (inlineP2pcdRequest = SequenceOfHashedId3; response carried in requestedCertificate of the next SPDU). Inline can also request **end-entity** certificates when SignerIdentifier is an unknown digest; out-of-band only CA certs. The two request fields are mutually exclusive in HeaderInfo. Responders pick a random backoff and stop once a threshold number of responses has been observed ("After the third response the threshold number is reached"). | Config parameters: p2pcd_useInteractiveForm, p2pcd_flavor {inline, out-of-band, none}, p2pcd_requestActiveTimeout, p2pcd_observedRequestTimeout, p2pcd_maxResponseBackoff, p2pcd_responseActiveTimeout, p2pcd_currentlyUsedTriggerCertificateTime, p2pcd_responseCountThreshold (recommended values in security profile C.2.1.3.1). NDSS 2024 restates the 1609.2-2022 values as backoff uniform 0–250 ms and respond only if fewer than 3 responses heard. | IEEE 1609.2a-2017 §6.3.9, §8.1–8.2.4 (ETSI docbox PDF); NDSS 2024 §II-A | VERIFIED / VERIFIED (secondary) |
| IEEE 1609.2-2022 additions | CR14 "Extended P2PCD": request a CA certificate in any message (not only the trigger SDEE's), via the new HeaderInfo contributedExtensions mechanism (CR15; 1-byte contributorId; ETSI uses it for CRL/CTL requests); CR13 P2P distribution of large security-management messages; CR20 sending standalone certificates; CR23 hash-based signature support; CR18 SM2/SM3/SM4; CR12 omitted payload; CR30 OperatingOrganizationId. 1609.2-2022 is now superseded by 1609.2-2025. | — | Whyte 2022 slides p.12–17; IEEE SA page for 1609.2-2022 | VERIFIED (secondary) |
| ETSI P2P CRL/CTL requests | EtsiTs102941CrlRequest / EtsiTs102941DeltaCtlRequest header extensions (TS 103 601 procedures) | — | TS 103 097 V2.1.1 §5.2 | VERIFIED |
| J2945/1 certificate omission | full certificate every CertAttachInt = 450 ms (Rostami) / every fifth SPDU = 500 ms (NDSS "current industry standards"); digest (8 bytes) otherwise | saves ≈ 70 bytes per digest SPDU (250 → 180) | Rostami 2018; NDSS 2024 | VERIFIED (secondary) |
| ETSI CAM certificate omission | digest by default, AT once per second, immediately on unknown-AT reception or on inline request | cert+sig 100–150 B vs 8 B digest, "up to 30 %" of CAM size | TS 103 097 §7.1.1; TR 2052; Yoshizawa & Preneel | VERIFIED |
| Redundant-certificate finding | VEINS simulation, Erlangen, 50 veh/km, 80–150 km/h, 100 runs × 5 min: median 99.3 % of received certificates already known ⇒ 500 ms cadence is wasteful | proposal: full-cert SPDU once per second + P2PCD extended to pseudonym certs: 10×330 = 3 300 B per 5 s → 5×330 + 3×172 = 2 166 B (−34 %) | NDSS 2024 §IV-A, §IV-B | VERIFIED (secondary) |
| Inline-P2PCD suppression (KU Leuven) | Yoshizawa & Preneel, "On Handling of Certificate Digest in V2X Communication" (imec-COSIC; IEEE conf. 2022): highway simulation shows "unknown certificate" condition in 76.6–83.9 % of in-range events; 85.5–90 % of encounters are short/low-impact; default inlineP2pcdRequest ≈ 58.7 / 55.5 per s, reduced to 8.3 / 6.6 per s with trajectory-aware suppression | Table V of the paper | KU Leuven PDF | VERIFIED (secondary) |
| VAM "new CAM signer" rule | attach AT when a not-previously-seen CAM signer is observed (in addition to 1 s timer) | — | TS 103 300-3 §6.5.3 | VERIFIED |
| C2C-CC hashedId8 collision handling | AT change on 32-bit digest collision | — | RS 2037 RS_BSP_181 | VERIFIED |
| Bindel, McCarthy, Rahbari, Twardokus, "Suitability of 3rd Round Signature Candidates for V2V Communication" (3rd NIST PQC Conf., June 2021) | PQ-V2Verifier SDR testbed (802.11p) with liboqs; BSM every 100 ms | Sizes (pk / sig bytes): ECDSA P-256 64/64; Dilithium-II 1 312/2 420; Falcon-512 897/666; Rainbow-I 157 800/66. BSM packet (BSM + sig, no pk): ECDSA 250; Dilithium 2 599 (> 2 304 max 802.11p MSDU); Falcon 845; Rainbow 245. With explicit cert: 530 / 4 127 / 1 958 / 158 261. Sign/verify (ms, from eBACS cycles): ECDSA 1.563/1.429; Dilithium 0.063/0.054; Falcon 0.266/0.068; Rainbow 1.526/1.664. Packet loss < 0.1 % for ECDSA/Falcon/Rainbow. Capacity note: ~2 500 ECDSA verifications/s (Qualcomm 9150) vs 3 600 BSM/s dense-highway example. Conclusion: "Dilithium and Falcon look like suitable replacements for ECDSA" on verification speed; Dilithium and Rainbow exceed the 2 304-byte limit. | NIST slides p.20, 24–31 | VERIFIED (secondary) |
| Twardokus, Bindel, Rahbari, McCarthy, "When Cryptography Needs a Hand: Practical Post-Quantum Authentication for V2V Communications" (NDSS 2024) — **Partially Hybrid** design | BSMs keep ECDSA signatures; a *hybrid certificate* (ECDSA cert + PQ signature over the ECDSA key, size 30 + |pk| + |sig|) is **fragmented** into α equal-size fragments carried in the first α SPDUs of each τ = 5 (500 ms) certificate-transmission cycle; remaining SPDUs carry the certificate hash. P2PCD learning responses are fragmented likewise (β fragments, each after a 0–250 ms random wait, mean 125 ms). Fully Hybrid (dual signatures) only feasible with Falcon; Pure-PQ infeasible. | Limits: DSRC payload cap 2 304 B (802.11 Table 9-25); C-V2X 437 B max payload at practical MCS in 10 MHz (3GPP Table A.8.3-1) ⇒ "current iteration of 5G C-V2X cannot practically support PQC". Frame sizes signed BSM+overhead+pk+sig (Fig. 4): ECDSA 350; Falcon 2 435; XMSS 5 610; Dilithium 6 310; Sphincs+ 15 844. ECDSA cert 162 B. Falcon hybrid cert 858 B ⇒ α = 1, first frame payload 1 026 B, other four 204 B each. Learning-response times: Falcon 250 ms, XMSS 375 ms, Dilithium ~500 ms, Sphincs+ ~1 000 ms (8 fragments) ⇒ Sphincs+ ruled out, Dilithium "on the edge ... likely not viable", XMSS and Falcon acceptable; Falcon "the only PQ algorithm we find to be viable"/"most viable". XMSS needs Level-5 XMSS-SHA2_16_256 to yield ≥ 3 000 signatures per 5-min pseudonym. Added per-BSM delay 0.25–0.39 ms. | NDSS 2024 §II-B, §III, §IV, §V-D, §VII; PQ-V2Verifier GitHub | VERIFIED (secondary) |
| "Partially Overlapping Certificates" / "Sending PQ signatures over multiple frames" (CAMP/NIST) | no document with these titles was located (RIT WISP publication list, Twardokus CV, NIST slides all checked) | — | — | UNVERIFIED / not located |
| Follow-ons | Twardokus & Rahbari, "Quantum-Resistant Safety Message Authentication for NextG C-V2X: A Cross-Layer Approach", IEEE TVT (accepted 2026) — content not accessed; "Demo: Open-Source Hardware-in-the-Loop Testbed for Post-Quantum V2V Security Research", VehicleSec 2024; PQCMC implicit-certificate proposal (arXiv 2401.13691) — content not accessed | — | geofftwardokus.com/resume; NSF PAR | UNVERIFIED (content) |

---

## F. Network / transport headers

### F.1 IEEE 1609.3 WSMP (v3 = 1609.3-2016/2020)

| Field | Size | Source | Status |
|---|---|---|---|
| WSMP-N-Header first octet | 1 byte: Subtype (4 bits) | WSMP-N-Header Option Indicator (1 bit) | Version (3 bits) | Wireshark `packet-wsmp.c` (dissect_wsmp_v3); ASN.1 ShortMsgNpdu { subtype, transport, body } | VERIFIED (secondary) |
| N-Header extensions (optional) | each = Element ID (1) + Length (1–2) + value; channel number (ID 15), data rate (ID 16), transmit power (ID 4) ⇒ 3 bytes each for 1-byte values; ASN.1 ShortMsgNextensions of TXpower80211 INTEGER(-128..127), ChannelNumber80211 INTEGER(0..255), DataRate80211 INTEGER(0..255) | packet-wsmp.c; wsm.asn, wee.asn | VERIFIED (secondary) |
| TPID | 1 byte (selects PSID-only vs. port-carrying transport PDU) | packet-wsmp.c; ShortMsgTpdus CHOICE(0..127), bcMode [0] | VERIFIED (secondary) |
| WSMP-T header: PSID | p-encoded VarLengthNumber, 1–4 bytes (values 0 … 0x1020407F) | packet-wsmp.c; wsm.asn (destAddress VarLengthNumber); IEEE PSID tutorial (max 270 549 119 = 0x1020407F) | VERIFIED |
| WSMP-T header: Length | 1–2 bytes (variable) | packet-wsmp.c | VERIFIED (secondary) |
| Minimum WSMP overhead | 4 bytes (N-hdr 1 + TPID 1 + PSID 1 + length 1); BSM with PSID 0x20 and payload ≥ 128 B ⇒ 5 bytes; with the three N-extensions ⇒ 14 bytes | DERIVED from the layout above | DERIVED |
| "minimum 5 bytes ... rarely exceeding 20" | search snippet attributed to IEEE 1609.3 text | — | UNVERIFIED |
| Legacy WSMP v2 layout | version 1, PSID p-encoded, optional TLVs, WAVE element ID 1, WSM length 2 | packet-wsmp.c (legacy path) | VERIFIED (secondary) |
| Fragmentation | none: no fragmentation field in ShortMsgNpdu/ShortMsgBcPDU; ShortMsgData ::= OCTET STRING "-- maximum size is given by access technology"; dissector has no reassembly | wsm.asn; packet-wsmp.c | VERIFIED (by absence; standard clause UNVERIFIED) |
| WSM maximum length | IEEE 1609.3-2020 MIB default max WSM payload 1 400 bytes; 2 302 bytes supported; DSRC MSDU cap 2 304 bytes | CTI 4501 §4.3.3.1.3.1; NDSS 2024 (802.11 Table 9-25) | VERIFIED (secondary) |
| EtherType | 0x88DC ("Wireless Access in a Vehicle Environment") | Wireshark `epan/etypes.h` | VERIFIED (secondary) |
| UDP/IP alternative | 1609.3 offers WSMP, UDP/IP and TCP/IP (IPv6) | ARC-IT standard page | VERIFIED (secondary) |

### F.2 ETSI GeoNetworking — EN 302 636-4-1 V1.4.1 (2020-01)

| Header | Size (octets) | Composition | Source | Status |
|---|---|---|---|---|
| Basic Header | 4 | §9.6 | Tables 11–17 ("Length: 4 octets") | VERIFIED |
| Common Header | 8 | §9.7 | Tables 11–17 | VERIFIED |
| Long Position Vector (LPV) | 24 | §9.5.2 | Tables 11–17 | VERIFIED |
| Short Position Vector (SPV) | 20 | §9.5.3 | Table 11 | VERIFIED |
| SHB (single-hop broadcast, used for CAM) | **40** | Basic 4 + Common 8 + SO PV 24 + media-dependent data 4 (octets 36–39) | Table 13 | VERIFIED |
| TSB | 40 | 4 + 8 + SN 2 + reserved 2 + LPV 24 | Table 12 | VERIFIED |
| GBC / GAC | **56** | 4 + 8 + SN 2 + reserved 2 + LPV 24 + lat 4 + long 4 + dist-a 2 + dist-b 2 + angle 2 + reserved 2 (octets 0–55) | Table 14 | VERIFIED |
| GUC | 60 | 4 + 8 + SN 2 + reserved 2 + LPV 24 + SPV 20 | Table 11 | VERIFIED |
| BEACON | 36 | 4 + 8 + LPV 24 | Table 15 | VERIFIED |
| MTU rule | MTU_GN ≤ MTU_AL − GEO_MAX (largest GN header incl. security) | §9.2.3 | VERIFIED |
| itsGnMaxSduSize | 1 398 = 1 500 − GN_MAX (88) − GNSEC_MAX (0) | Annex H, item 8 | VERIFIED |
| itsGnMaxGeoNetworkingHeaderSize (GN_MAX) | 88 (GUC header incl. security accounting) | Annex H, item 9 | VERIFIED |
| Fragmentation | none in GN: the full text of EN 302 636-4-1 V1.4.1 contains zero occurrences of "fragment"; oversize handled at facilities layer (MAPEM layerID, CPM segmentation) | grep of extracted text; TS 103 301 §6.4.1; TS 103 324 §6.1.3 | VERIFIED (by absence) |
| EtherType | 0x8947 ("GeoNetworking as defined in ETSI EN 302 636-4-1") | Wireshark `epan/etypes.h` | VERIFIED (secondary) |
| LLC/SNAP over ITS-G5 | IEEE 802.2 LLC (DSAP/SSAP 0xAA, ctrl 0x03) + SNAP (OUI 00-00-00 + EtherType) = 8 octets. Not present in TS 102 636-4-2 V1.1.1 text (checked); presumed in EN 302 663 | — | DERIVED / ETSI clause UNVERIFIED |

### F.3 ETSI BTP — EN 302 636-5-1 V2.2.1 (2019-05)

| Header | Size | Fields | Source | Status |
|---|---|---|---|---|
| BTP-A (interactive; NH = 1) | 4 octets | Destination port (2) + Source port (2) | §7.2, Table 2, Table 1 | VERIFIED |
| BTP-B (non-interactive; NH = 2) | 4 octets | Destination port (2) + Destination port info (2, default 0) | §7.3, Table 3 | VERIFIED |
| Well-known ports | SPATEM 2004, MAPEM 2003, IVIM 2006, SREM 2007, RTCMEM 2013 (from TS 103 301 CSP_PortNo); CAM 2001 / DENM 2002 / SSEM 2008 per TS 103 248 (not extracted) | TS 103 301 tables; TS 103 248 | VERIFIED / UNVERIFIED (CAM/DENM/SSEM) |
| Total below the secured CAM (ITS-G5) | LLC/SNAP 8 + GN SHB 40 + BTP-B 4 = **52 octets** | F.2 + F.3 | DERIVED |

---

## G. ASN.1 toolchains

| Tool | License | Codecs | ETSI ITS modules | SAE J2735 | IEEE 1609.2 / TS 103 097 | V2X track record | Source | Status |
|---|---|---|---|---|---|---|---|---|
| **asn1c** (vlm/asn1c) | BSD-2-Clause, © 2003–2017 Lev Walkin and contributors | BER/DER/XER/PER incl. UPER; OER in master | Vanetza generates all its ETSI messages (CAM/DENM/CPM/VAM/…) with the mouse07410 asn1c fork in Docker (`generate_asn1c` CMake target) | J2735-2016 example dir exists but "THERE IS NO J2735_201603.asn1 FILE THERE YET! Go to http://standards.sae.org/j2735_201603/ and download"; USDOT `asn1_codec` compiles J2735 2016/2020/2024 with asn1c, 2024 needs .patch edits ("a couple changes ... to compile with the current version of asn1c"); parametrised types and regional-extension open types are recurring problems (asn1c issues #121, #253, #420) | `examples/sample.source.1609.2` in asn1c; Vanetza ships `asn1/IEEE1609dot2.asn` | Vanetza (ETSI C-ITS stack), USDOT jpo-ode/asn1_codec | asn1c LICENSE; vanetza.org recipe; asn1c J2735 README; USDOT asn1_codec README | VERIFIED |
| **pycrate** | LGPL v2.1 (and later) | BER/CER/DER, PER aligned & unaligned (+canonical), OER/COER, JER | bundled: ETSI_ITS_CAM_EN302637_2, ETSI_ITS_DENM_EN302637_3, ETSI_ITS_IS_TS103301, ETSI_ITS_VAM_TS103300_3, ETSI_ITS_r1318 | via pyV2XLib (Michigan Traffic Lab, MIT): J2735_202309 and J3224_202208; SAE ASN.1 must be downloaded by the user | bundled ETSI_ITS_IEEE1609_2, ETSI_ITS_IEEE1609_2_1 | pyV2XLib (SDSM/J3224 encoder), Scapy C-ITS example | pycrate README; pycrate_asn1dir listing; pyV2XLib README | VERIFIED |
| **asn1tools** (eerimoq) | MIT (per PyPI metadata) | BER, DER, GSER, JER, OER, PER, UPER, XER; C generator for OER/UPER (limited types, no dynamic allocation) | no ETSI modules bundled; ETSI Release-2 CDD uses parametrised types → unsupported | fails: "CLASS ... not yet supported", "Parametrization (X.683) is not yet supported"; issue #18 (J2735 `TestExtension {{REGION.Reg-TestMessage}}` parse error) open | test suite ships `tests/files/ieee/ieee1609_2.asn` (parses) | none documented | asn1tools README; issue #18; libraries.io | VERIFIED |
| **rasn** (librasn) | MIT OR Apache-2.0 | BER/CER/DER/APER/UPER/JER/OER/COER/XER; `#[no_std]` | none in rasn-its | none | `rasn-its` 0.28.14 (2026-08-07): IEEE 1609.2-2022 (`ieee1609dot2`) and ETSI TS 103 097 (`ts103097`) | no V2X deployment documented; `rasn-compiler` can generate bindings from ETSI/SAE modules | rasn README; lib.rs rasn-its; standards/its README | VERIFIED (track record UNVERIFIED) |

Licensing of the modules themselves:

| Module set | Availability / license | Source | Status |
|---|---|---|---|
| ETSI ITS ASN.1 (CDD TS 102 894-2, CAM EN 302 637-2, DENM EN 302 637-3, VAM TS 103 300-3, CPM TS 103 324, TS 103 301, TS 103 097, IEEE 1609.2 mirror, MRS TS 103 759, POTI, SAEM …) | forge.etsi.org/rep/ITS/asn1/* under **BSD-3-Clause** ("Unless specified otherwise, the content of the repositories in the ETSI Forge is licensed under the terms of the BSD-3-Clause LICENSE"; LICENSE file at each repo root, © ETSI) | forge.etsi.org/index.php/legal-matters; repo LICENSE files | VERIFIED |
| IEEE 1609.2 ASN.1 | public copies: forge.etsi.org/rep/ITS/asn1/ieee1609.2 (branches 1609.2b-2019, 1609.2.1_synch), usdot-jpo-ode/scms-asn1, eabalea/1609dot2-asn, riebl/vanetza/asn1; IEEE also distributes the modules with the standard | GitLab/GitHub URLs in Sources | VERIFIED |
| SAE J2735 ASN.1 | sold/licensed by SAE as separate products (J2735ASN_202007; J2735SET_201603; J2735_202409 and J2735 2023 exist); "Redistribution of the ASN files is not permitted, so they are not included in this repository" (USDOT asn1_codec); pyV2XLib and asn1c example likewise require download from SAE; USDOT jpo-asn-pojos redistributes only generated Java classes for J2735 (2024) | USDOT README; SAE catalogue URLs (pages are JS-rendered and did not return text) | VERIFIED (redistribution ban) |
| J2735 2023 backwards-incompatible with 2020 | search snippet only | — | UNVERIFIED |

---

## Open items (UNVERIFIED — need the paywalled standard or a measurement)

1. J2945/1 Part II cadence (PathHistory/PathPrediction every BSM), vMinITT, vPERRange/vPERInterval/vPERMax/vPERSubInterval, vRescheduleThreshold, vTxRand.
2. BSM Part I UPER size ("~39 bytes") — run a real encoder (asn1c/pycrate with the SAE module) to settle.
3. J2945/9 PSM rate rules and PSM size.
4. Typical SPaT, DENM, PSM sizes; MAP 1450-byte guidance.
5. WSA RepeatRate units; WSMP "no fragmentation" and "5–20 bytes" statements in 1609.3 text; Time64 epoch wording in 1609.2 text.
6. LLC/SNAP clause in EN 302 663; SSEM/CAM/DENM BTP ports in TS 103 248.
7. Any CAMP/NIST "Partially Overlapping Certificates" document — not located.
8. rasn/asn1tools V2X deployments.

---

## Sources (all fetched 2026-09-17; ETSI/IEEE PDFs text-extracted locally)

Standards
- ETSI EN 302 637-2 V1.4.1 (2019-04) CAM: https://www.etsi.org/deliver/etsi_en/302600_302699/30263702/01.04.01_60/en_30263702v010401p.pdf
- ETSI EN 302 637-3 V1.3.1 (2019-04) DENM: https://www.etsi.org/deliver/etsi_en/302600_302699/30263703/01.03.01_60/en_30263703v010301p.pdf
- ETSI TS 103 300-3 V2.2.1 (2023-02) VAM: https://www.etsi.org/deliver/etsi_ts/103300_103399/10330003/02.02.01_60/ts_10330003v020201p.pdf (V2.3.1: https://www.etsi.org/deliver/etsi_ts/103300_103399/10330003/02.03.01_60/ts_10330003v020301p.pdf)
- ETSI TS 103 324 V2.1.1 (2023-06) CPM: https://www.etsi.org/deliver/etsi_ts/103300_103399/103324/02.01.01_60/ts_103324v020101p.pdf
- ETSI TR 103 562 V2.1.1 (2019-12) CPS analysis: https://www.etsi.org/deliver/etsi_tr/103500_103599/103562/02.01.01_60/tr_103562v020101p.pdf
- ETSI TS 103 301 V2.1.1 (2021-03) infrastructure services: https://www.etsi.org/deliver/etsi_ts/103300_103399/103301/02.01.01_60/ts_103301v020101p.pdf
- ETSI TS 103 097 V2.1.1 (2021-10): https://www.etsi.org/deliver/etsi_ts/103000_103099/103097/02.01.01_60/ts_103097v020101p.pdf ; V2.2.1 (2026-03): https://www.etsi.org/deliver/etsi_ts/103000_103099/103097/02.02.01_60/ts_103097v020201p.pdf
- ETSI EN 302 636-4-1 V1.4.1 (2020-01) GeoNetworking: https://www.etsi.org/deliver/etsi_en/302600_302699/3026360401/01.04.01_60/en_3026360401v010401p.pdf
- ETSI EN 302 636-5-1 V2.2.1 (2019-05) BTP: https://www.etsi.org/deliver/etsi_en/302600_302699/3026360501/02.02.01_60/en_3026360501v020201p.pdf
- ETSI TS 102 636-4-2 V1.1.1 (2013-10): https://www.etsi.org/deliver/etsi_ts/102600_102699/1026360402/01.01.01_60/ts_1026360402v010101p.pdf
- IEEE Std 1609.2a-2017 (full text, ETSI docbox): https://docbox.etsi.org/STF/Archive/STF538_TC_ITS/STFworkarea/libaries/IEEE_Std_1609_2a-2017.pdf
- IEEE 1609.2 ASN.1 (ETSI forge): https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/raw/master/Ieee1609Dot2.asn , https://forge.etsi.org/rep/ITS/asn1/ieee1609.2/raw/master/Ieee1609Dot2BaseTypes.asn
- IEEE 1609.2-2022 page: https://standards.ieee.org/ieee/1609.2/10258/ ; IEEE 1609.3-2020 page: https://standards.ieee.org/standard/1609_3-2020.html ; ARC-IT 1609.3: https://www.arc-it.net/html/standards/standard97.html
- IEEE WAVE and PSID tutorial (2016): https://standards.ieee.org/content/dam/ieee-standards/standards/web/documents/tutorials/psid.pdf
- IEEE 1609.3 ASN.1 (TCI mirror): https://github.com/certificationoperatingcouncil/TCI_ASN1/tree/master/TCI%20Interface/ASN1/1609dot3 (wsm.asn, wsa.asn, wee.asn)
- CTI 4501 v01.01 Connected Intersections Implementation Guide: https://www.ite.org/ITEORG/assets/File/Standards/CTI%204501v0101.pdf
- SAE catalogue: J2945/1_202004 https://standards.globalspec.com/std/14220512/sae-j2945-1 ; J2945/1B_202212 https://www.sae.org/standards/content/j2945/1b_202212/ ; J2945/9_201703 https://www.sae.org/standards/content/j2945/9_201703 ; J2735ASN_202007 https://www.sae.org/standards/content/j2735asn_202007/ ; J2735SET_201603 https://www.sae.org/standards/content/j2735set_201603/

Industry reports
- C2C-CC TR 2052 Survey on ITS-G5 CAM statistics (2018-12-20): https://www.car-2-car.org/fileadmin/documents/General_Documents/C2CCC_TR_2052_Survey_on_CAM_statistics.pdf
- C2C-CC TR 2050 Spectrum Needs (2020-02-28): https://www.car-2-car.org/fileadmin/documents/General_Documents/C2CCC_TR_2050_Spectrum_Needs.pdf
- C2C-CC RS 2037 Vehicle C-ITS station profile R1.6.10 (2026-07-24): https://www.car-2-car.org/fileadmin/documents/Basic_System_Profile/Release_1.6.10/C2CCC_RS_2037_Profile_R1610.pdf
- W. Whyte (Qualcomm), V2X IEEE 1609.2.1: Status and Deployment (2022-08-31): https://avstandard.or.kr/uploads/file_content/9f26bee8-2e55-4c38-9e41-198653b7b05f/_%EB%B0%9C%ED%91%9C%EC%9E%90%EB%A3%8C__Korea-1609.2-2022-08-31-v2_Qualcomm_William_Whyte.pdf
- ITU-T CITS meeting, SAE updates (2017-12): https://www.itu.int/en/ITU-T/extcoop/cits/Documents/Meeting-20171205-Arlington/16_SAE-Updates-CITS.pdf

Papers
- Rostami, Krishnan, Gruteser, "V2V Safety Communication Scalability Based on the SAE J2945/1 Standard" (2018): https://winlab.rutgers.edu/~rostami/files/pdf/rostami2018v2v.pdf
- Twardokus, Bindel, Rahbari, McCarthy, "When Cryptography Needs a Hand: Practical Post-Quantum Authentication for V2V Communications", NDSS 2024: https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf (ePrint 2022/483: https://eprint.iacr.org/2022/483.pdf ; code: https://github.com/twardokus/pq-v2verifier)
- Bindel, McCarthy, Rahbari, Twardokus, "Suitability of 3rd Round Signature Candidates for Vehicle-to-Vehicle Communication", NIST PQC Conf. 2021: https://csrc.nist.gov/CSRC/media/Presentations/suitability-of-3rd-round-signature-candidates-for/images-media/session-5-bindel-suitability-vehicle.pdf
- Yoshizawa, Preneel, "On Handling of Certificate Digest in V2X Communication" (imec-COSIC KU Leuven): https://cosicdatabase.esat.kuleuven.be/backend/publications/files/conferencepaper/3530 (IEEE Xplore 9941683)
- Cominetti et al., "Faster verification of V2X BSM messages via Message Chaining", IACR ePrint 2022/133: https://eprint.iacr.org/2022/133.pdf
- Ostendorf, Garlichs, Wolf, "Evaluating Redundancy Mitigation in VRU Awareness Messages for Bicycles" (2025): https://arxiv.org/pdf/2506.22052
- Islam et al., "Vision-based Pedestrian Alert Safety System (PASS)" (2019): https://arxiv.org/pdf/1907.05284
- Twardokus publication list: https://geofftwardokus.com/resume/ ; RIT WISP lab: https://www.rit.edu/wisplab/publications-topic
- "Performance Analysis of Existing 1609.2 Encodings v ASN.1" (not fetchable): https://www.researchgate.net/publication/275249582_Performance_Analysis_of_Existing_16092_Encodings_v_ASN1

Tools
- asn1c LICENSE: https://raw.githubusercontent.com/vlm/asn1c/master/LICENSE ; J2735 example README: https://github.com/vlm/asn1c/blob/master/examples/sample.source.J2735/README ; issues #121 #174 #253 #420
- USDOT asn1_codec README: https://github.com/usdot-jpo-ode/asn1_codec/blob/develop/asn1c_combined/README.md ; jpo-asn-pojos: https://github.com/usdot-jpo-ode/jpo-asn-pojos
- Vanetza ASN.1 recipe: https://www.vanetza.org/recipes/generate-asn1/ ; https://github.com/riebl/vanetza/blob/master/asn1/IEEE1609dot2.asn
- pycrate README: https://github.com/pycrate-org/pycrate ; asn1dir: https://github.com/pycrate-org/pycrate/tree/master/pycrate_asn1dir ; pyV2XLib: https://github.com/michigan-traffic-lab/pyV2XLib
- asn1tools README: https://github.com/eerimoq/asn1tools ; issue #18: https://github.com/eerimoq/asn1tools/issues/18 ; 1609.2 test file: https://github.com/eerimoq/asn1tools/blob/master/tests/files/ieee/ieee1609_2.asn
- rasn: https://github.com/librasn/rasn ; rasn-its: https://lib.rs/crates/rasn-its ; compiler: https://github.com/librasn/compiler
- ETSI Forge legal: https://forge.etsi.org/index.php/legal-matters ; e.g. https://forge.etsi.org/rep/ITS/asn1/vam-ts103300_3/-/blob/master/LICENSE
- Wireshark WSMP dissector: https://raw.githubusercontent.com/wireshark/wireshark/master/epan/dissectors/packet-wsmp.c ; etypes.h: https://raw.githubusercontent.com/wireshark/wireshark/master/epan/etypes.h
