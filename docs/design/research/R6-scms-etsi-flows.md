# R6 — Credential-management protocols as network exchanges: SCMS (US) and ETSI ITS PKI (EU), with revocation-latency, privacy-metric and threshold-crypto building blocks

Compiled 2026-09-17. Every row carries a source tag; tags resolve in §0. Facts that could not be confirmed against a primary text in this session are tagged **UNVERIFIED**. Nothing in this sheet is invented; where a number was not found it is said so.

Method note: the CAMP wiki (wiki.campllc.org) and web.archive.org were unreachable during this session (ECONNREFUSED / blocked). The CAMP requirements text used here is the full PDF *"SCMS PoC Implementation — EE Requirements and Specifications Supporting SCMS Software Release 1.1"* (May 4, 2016), which is the direct predecessor of the Release 1.2.2 edition the brief names; section numbers below are from that PDF. The IEEE 1609.2.1 standard itself is paywalled; its public summaries (W. Whyte's 2022 decks, ETSI TS 102 941 clause 6.2.3.5 which imports it, and the PoPETs 2024 analysis) are used instead.

---

## 0. Source key

| Tag | Source |
|---|---|
| [BRECHT] | B. Brecht, D. Therriault, A. Weimerskirch, W. Whyte, V. Kumar, T. Hehn, R. Goudy, "A Security Credential Management System for V2X Communications," IEEE Trans. ITS, 2018. arXiv:1802.05323v1 — https://arxiv.org/abs/1802.05323 (full PDF text read; section numbers cited) |
| [CAMP-EE] | CAMP VSC5 Consortium, "Security Credential Management System Proof-of-Concept Implementation — EE Requirements and Specifications Supporting SCMS Software Release 1.1," May 4 2016 (Coop. Agreement DTNH22-14-H-00449/0003). Index page: https://www.campllc.org/security-credential-management-system-scms-proof-of-concept-poc-implementation/ ; wiki (unreachable this session): https://wiki.campllc.org/display/SCP |
| [PRIMER] | USDOT ITS-JPO, "Connected Vehicle Deployment Technical Assistance — SCMS Technical Primer," FHWA-JPO-19-775, Nov 2019 — https://rosap.ntl.bts.gov/view/dot/43635/dot_43635_DS1.pdf |
| [WHYTE22a] | W. Whyte (Qualcomm), "V2X Certificate Management with IEEE 1609.2.1: Status and Deployment," 2022-01-20 — https://avstandard.or.kr/uploads/file_content/64d56913-37ea-4248-b273-9b5d59d2b2cd/_%EB%B0%9C%ED%91%9C%EC%9E%90%EB%A3%8C__Korea-SCMS-Presentation-2022-01-20_Qualcomm_William_Whyte.pdf |
| [WHYTE22b] | W. Whyte, "V2X IEEE 1609.2.1: Status and Deployment," 2022-08-31 — https://avstandard.or.kr/uploads/file_content/9f26bee8-2e55-4c38-9e41-198653b7b05f/_%EB%B0%9C%ED%91%9C%EC%9E%90%EB%A3%8C__Korea-1609.2-2022-08-31-v2_Qualcomm_William_Whyte.pdf |
| [BKM24] | "Provable Security Analysis of Butterfly Key Mechanism Protocol in IEEE 1609.2.1 Standard," PoPETs 2024(4) — https://eprint.iacr.org/2024/1674 (abstract only) |
| [BCAM] | V. Kumar, J. Petit, W. Whyte, "Binary Hash Tree based Certificate Access Management for Connected Vehicles," ACM WiSec 2017 — https://eprint.iacr.org/2017/744 (full text) |
| [ACPC] | M. Simplicio, E. Cominetti, H. Kupwade Patil, J. Ricardini, M. Silva, "ACPC: Efficient revocation of pseudonym certificates using activation codes," (Ad Hoc Networks 2019; preprint text read; exact ePrint number not verified) |
| [KHODAEI] | M. Khodaei, P. Papadimitratos, "Efficient, Scalable, and Resilient Vehicle-Centric CRL Distribution in VANETs," ACM WiSec 2018 — https://arxiv.org/abs/1807.02706 (full text) |
| [HAAS11] | J. Haas, Y.-C. Hu, K. Laberteaux, "Efficient Certificate Revocation List Organization and Distribution," IEEE JSAC 29(3):595–604, 2011 — https://dblp.org/rec/journals/jsac/HaasHL11.html (abstract/metadata only; PDF not fetchable) |
| [HAAS09] | J. Haas et al., "Design and Analysis of a Lightweight Certificate Revocation Mechanism for VANET," ACM VANET 2009 (cited as ref. [38] in [KHODAEI]; not fetched) |
| [TS102941] | ETSI TS 102 941 V2.2.1 (2022-11), Trust and Privacy Management, Release 2 — https://www.etsi.org/deliver/etsi_ts/102900_102999/102941/02.02.01_60/ts_102941v020201p.pdf |
| [TS102940] | ETSI TS 102 940 V2.1.1 (2021-07), ITS communications security architecture and security management — https://www.etsi.org/deliver/etsi_ts/102900_102999/102940/02.01.01_60/ts_102940v020101p.pdf |
| [TS103759] | ETSI TS 103 759 V2.1.1 (2023-01), Misbehaviour Reporting service — https://www.etsi.org/deliver/etsi_ts/103700_103799/103759/02.01.01_60/ts_103759v020101p.pdf ; ASN.1: https://forge.etsi.org/rep/ITS/asn1/mrs_ts103759 (V2.2.1 (2026-01) exists but was not fetched) |
| [TR103415] | ETSI TR 103 415 V2.1.1 (2025-03), Pre-standardization study on pseudonym change management — https://www.etsi.org/deliver/etsi_tr/103400_103499/103415/02.01.01_60/tr_103415v020101p.pdf |
| [EUCP] | "Certificate Policy for Deployment and Operation of European C-ITS," Release 1.1, June 2018 — https://transport.ec.europa.eu/system/files/2018-05/c-its_certificate_policy-v1.1.pdf |
| [C2C-POS15] | C2C-CC, "Position paper on C-ITS security and privacy," C2C602002100, 2015-03-10 — https://www.car-2-car.org/fileadmin/documents/General_Documents/C2C602002100_Position_paper_on_C-ITS_security_and_privacy.pdf |
| [C2C-MBD21] | C2C-CC, "White Paper on Misbehaviour Detection and Reporting to Misbehaviour Authority," C2CCC_WP_2092 V1.0, 2021-12-17 — https://www.car-2-car.org/fileadmin/documents/General_Documents/C2CCC_WP_2092_MisbehaviourDetection_and_Reporting_V1.0.pdf |
| [WIED10] | B. Wiedersheim, Z. Ma, F. Kargl, P. Papadimitratos, "Privacy in Inter-Vehicular Networks: Why simple pseudonym change is not enough," WONS 2010, pp. 176–183, DOI 10.1109/WONS.2010.5437115 — https://www.uni-ulm.de/fileadmin/website_uni_ulm/iui.inst.100/institut/verz-ma-ehedem/wiedersheim/wons2010-tracking.pdf (full text) |
| [PETIT15] | J. Petit, F. Schaub, M. Feiri, F. Kargl, "Pseudonym Schemes in Vehicular Networks: A Survey," IEEE Comm. Surveys & Tutorials 17(1):228–255, 2015 — https://research.utwente.nl/en/publications/pseudonym-schemes-in-vehicular-networks-a-survey/ (abstract only) |
| [BS03] | A. Beresford, F. Stajano, "Location Privacy in Pervasive Computing," IEEE Pervasive Computing 2(1):46–55, 2003, DOI 10.1109/MPRV.2003.1186725 (metadata only) |
| [FREUD07] | J. Freudiger, M. Raya, M. Félegyházi, P. Papadimitratos, J.-P. Hubaux, "Mix-Zones for Location Privacy in Vehicular Networks," ACM WiN-ITS 2007 (metadata only) |
| [KHAN20] | "Cooperative Location Privacy in Vehicular Networks: Why Simple Mix Zones are Not Enough," IEEE IoT Journal 2020 — https://arxiv.org/abs/2012.06666 (abstract) |
| [COMINETTI22] | E. Cominetti et al., "Faster verification of V2X BSM messages via Message Chaining," — https://eprint.iacr.org/2022/133 (full text) |
| [RFC9591] | RFC 9591, "The FROST Protocol for Two-Round Schnorr Signatures," 2024 — https://www.rfc-editor.org/rfc/rfc9591.html |
| [FROST20] | C. Komlo, I. Goldberg, "FROST: Flexible Round-Optimized Schnorr Threshold Signatures," SAC 2020 — https://eprint.iacr.org/2020/852 (abstract) |
| [GG18] | R. Gennaro, S. Goldfeder, "Fast Multiparty Threshold ECDSA with Fast Trustless Setup," CCS 2018 — https://eprint.iacr.org/2019/114 (full text) |
| [GG20] | R. Gennaro, S. Goldfeder, "One Round Threshold ECDSA with Identifiable Abort," — https://eprint.iacr.org/2020/540 (full text) |
| [CGGMP21] | R. Canetti, R. Gennaro, S. Goldfeder, N. Makriyannis, U. Peled, "UC Non-Interactive, Proactive, Distributed ECDSA with Identifiable Aborts," CCS 2020 / ePrint 2021/060 (Oct 2024 revision) — https://eprint.iacr.org/2021/060 (full text) |
| [TRACCOON] | R. del Pino, S. Katsumata, M. Maller, F. Mouhartem, T. Prest, M.-J. Saarinen, "Threshold Raccoon: Practical Threshold Signatures from Standard Lattice Assumptions," EUROCRYPT 2024 — https://eprint.iacr.org/2024/184 (full text) |
| [CELI25] | S. Celi, R. del Pino, T. Espitau, G. Niot, T. Prest, "Efficient Threshold ML-DSA up to 6 parties," CCS 2025 (poster) / NIST 6th PQC conf. — https://csrc.nist.gov/csrc/media/events/2025/sixth-pqc-standardization-conference/efficient%20threshold%20ml-dsa%20up%20to%206%20parties.pdf (full text) |
| [QUORUS] | A. Bienstock, L. de Castro, D. Escudero, A. Polychroniadou, A. Takahashi, "Quorus: Efficient, Scalable Threshold ML-DSA Signatures from MPC," USENIX Security 2026 — https://eprint.iacr.org/2025/1163 (full text) |
| [TALUS] | L. Kao, R. Chang, "TALUS: FIPS-204-Exact Threshold ML-DSA via Boundary Clearance," arXiv:2603.22109v5 (Aug 2026) — https://arxiv.org/abs/2603.22109 (full text) |
| [SNDKG26] | "FIPS 204-Compatible Threshold ML-DSA via Shamir Nonce DKG," arXiv:2601.20917 — https://arxiv.org/abs/2601.20917 (abstract only) |
| [PED91] | T. Pedersen, "Non-Interactive and Information-Theoretic Secure Verifiable Secret Sharing," CRYPTO 1991, LNCS 576, pp. 129–140 — https://link.springer.com/chapter/10.1007/3-540-46766-1_9 (metadata only) |
| [GJKR] | R. Gennaro, S. Jarecki, H. Krawczyk, T. Rabin, "Secure Distributed Key Generation for Discrete-Log Based Cryptosystems," EUROCRYPT 1999; J. Cryptology 20(1), 2007 — https://link.springer.com/article/10.1007/s00145-006-0347-3 (paywalled; structure taken from https://en.wikipedia.org/wiki/Distributed_key_generation) |
| [HJKY95] | A. Herzberg, S. Jarecki, H. Krawczyk, M. Yung, "Proactive Secret Sharing Or: How to Cope With Perpetual Leakage," CRYPTO 1995, LNCS 963, pp. 339–352 — https://link.springer.com/chapter/10.1007/3-540-44750-4_27 (mechanism taken from https://en.wikipedia.org/wiki/Proactive_secret_sharing) |

---

## A. US SCMS (CAMP PoC / IEEE 1609.2.1)

### A.1 Entity set and trust boundaries

Definitions ([BRECHT] §II): "an SCMS component is *intrinsically-central* if it can have exactly one distinct instance for proper functioning. A component is *central* if it is chosen to have exactly one distinct instance in the considered instantiation." Distinct instances have different identifiers and share no cryptographic material.

| Component | Role (one line) | Central? | Source |
|---|---|---|---|
| SCMS Manager | policy (organizational + technical), review guidelines for misbehavior/revocation | **intrinsically central** | [BRECHT] §II-B, Fig. 1 |
| Policy Generator (PG) | signs Global Policy File (GPF) and Global Certificate Chain File (GCCF) | **intrinsically central** | [BRECHT] §II-B |
| Misbehavior Authority (MA) | processes misbehavior reports; sub-components Global Misbehavior Detection + CRL Generator (CRLG); [PRIMER] adds Internal Blacklist Manager | **intrinsically central** | [BRECHT] §II-B ("the MA, PG, and the SCMS Manager are the only intrinsically-central components"); [PRIMER] p.5 |
| Electors | sign ballots endorsing/revoking Root CAs or other electors (quorum vote) | top-level trust anchors, ≥3 with quorum 2 in PoC | [BRECHT] §II-B, Table III |
| Root CA (RCA) | issues ICA, PG, MA certs; self-signed; endorsed by elector quorum; typically offline | non-central (multiple allowed) | [BRECHT] §II-B |
| Intermediate CA (ICA) | shields RCA; issues ECA/PCA/RA/LA certs | non-central | [BRECHT] §II-B; [CAMP-EE] §2.1.2.6 (V2V-ICA, V2I-ICA subtrees) |
| Enrollment CA (ECA) | issues enrollment certificates ("passport") | non-central | [BRECHT] §II-B |
| Device Configuration Manager (DCM) | attests device eligibility to ECA; buffers config/certs at bootstrap | non-central | [BRECHT] §II-B |
| Certification Services | which device models are certified | — | [BRECHT] §II-B |
| Registration Authority (RA) | validates requests, butterfly expansion, shuffling, batching, blacklist, LPF/LCCF distribution; "intrinsically non-central" | non-central | [BRECHT] §II-B; [PRIMER] p.7 |
| Pseudonym CA (PCA) (= ACA in 1609.2.1) | issues pseudonym/identification/application certs | non-central | [BRECHT] §II-B; [WHYTE22a] p.6 |
| Linkage Authority LA1, LA2 | pre-linkage values; two LAs so one operator cannot link | exactly two | [BRECHT] §II-B |
| Location Obscurer Proxy (LOP) | strips IP/MAC; "single LOP is sufficient" | — | [BRECHT] §II-B |
| CRL Store (CRLS) | pass-through store; HTTP GET, no EE authentication | — | [BRECHT] §II-B; [CAMP-EE] §2.2.9 |
| CRL Broadcast (CRLB) | pass-through broadcast via RSEs, satellite radio | — | [BRECHT] §II-B |
| Distribution Center (DC) (1609.2.1 name) | distributes CRLs/CTLs/system config if not via RA | — | [WHYTE22a] p.6 |
| Certificate Access Manager (CAM) | optional ACPC/BCAM component | optional | [WHYTE22a] p.4,6; [BCAM] |

Chain of trust ([BRECHT] §II-B): enrollment certs verify against ECA; pseudonym/application/identification certs against PCA; CRLs against the CRLG certificate (part of MA). All online components use TLS; Root CA and Electors are air-gapped; data forwarded through a component not meant to read it is additionally encrypted at application layer (e.g., LA→PCA via RA).

Mandatory organizational separation ([BRECHT] §X): PCA ≠ RA; PCA ≠ either LA; LA1 ≠ LA2; LOP ≠ (RA or MA); MA ≠ (RA, LA or PCA). Reason given: no single organization may be able to map pseudonym certificates to a device.

CRL series in the PoC ([BRECHT] §VI-G): one main CRLG with series 1 = vehicle pseudonym certs, 2 = SCMS components, 3 = vehicle identification + RSE application certs, 4 = enrollment certs; Root CA manages series 256 (revokes PG, CRLG, MA). The CRACA (Certificate Revocation Authority CA) ID + CRL Series field appear in both the certificate and the CRL (IEEE 1609.2 structure). All current CRLs are packaged into one composite file at the CRL Store.

1609.2.1 adds ([WHYTE22a] p.4,6,13): X.509 enrollment certs allowed; OAuth "supplementary authorization" for RA access; Root Management Function = Electors signing an SCMS-Manager CTL, trusted if m-of-N elector signatures verify; each certificate is coupled to a single CRL signer.

### A.2 Flow 1 — Bootstrap / enrollment (Device ↔ DCM ↔ ECA)

Sequence ([BRECHT] §V-A, Fig. 3):
1. Device → DCM (out-of-band, secure environment): request {device type, public key}.
2. DCM ↔ Certification Services: check device-type certification.
3. DCM → ECA: forward request.
4. ECA: issue enrollment certificate.
5. DCM → Device: {enrollment certificate, ECA certificate, RA certificate + RA contact info}.

Initialization payload delivered alongside ([BRECHT] §V-A; [CAMP-EE] §2.2.6.2): certificates of all Electors, Root CAs, optionally ICAs and PCAs; MA, PG and CRLG certificates; CRL Store contact; RA FQDN.

| Item | Value | Source |
|---|---|---|
| Two provisioning methods | (1) request/response through DCM; (2) certificate injection: key pair generated outside OBE (e.g., in DCM), DCM requests cert from ECA on OBE's behalf | [CAMP-EE] §2.2.6.2 |
| PoC bootstrap | manual 9-step process (request → USDOT review → verify certification → generate init+enrollment data → encrypted bootstrap ZIP → upload to devices → request pseudonym certs); no automated bootstrapping for CV Pilots | [CAMP-EE] §2.2.6.2 |
| Enrollment cert validity (CV Pilot) | "Variable, 7 years maximum", 1 concurrently valid, "issued with an expiration at year 7 regardless of the date they are issued" | [CAMP-EE] Table 2.1.2.6.2 |
| Enrollment cert validity (design intent) | "extremely long validity periods expected to cover OBE's full operational lifetime"; "30 years, intended to last the lifetime of a vehicle" | [CAMP-EE] §2.1.2; [BCAM] fn.5 |
| Enrollment cert revocation | "done through internal blacklist at RA" (no broadcast) — but a CRL series 4 for enrollment certs also exists in the PoC | [CAMP-EE] §2.1.2; [BRECHT] §VI-G |
| Re-enrollment | needed after revocation; no undo of revocation | [BRECHT] §VI-F, §VIII |

### A.3 Flow 2 — Pseudonym certificate provisioning (EE → LOP → RA → LA1/LA2, PCA → RA → EE)

Sequence ([BRECHT] §V-E, Fig. 5; [CAMP-EE] §2.2.7.6–§2.2.7.8):
1. **EE → LOP → RA** (Step 3.1): Certificate Provisioning Request = {version, enrollment certificate, butterfly public seeds A (signing) and H (encryption) with expansion functions f_k, f_e, current device time (Time32), requested certificate start time}. Signed with enrollment cert, encrypted to RA cert in a 1609.2 encrypted frame. LOP strips IP/MAC. Sent **once**; no further requests needed for later batches.
2. **RA**: decrypt, verify signature, check enrollment cert valid and not on internal blacklist, check "only one request by the device" (one set per time period). Reply **Request Acknowledgement** = {version, low-order 8 bytes of SHA-256 of ToBeSigned request, Time32 when first batches will be available, repository URL}; reject = HTTP 500 (error detail hidden). RA performs butterfly expansion B_ι = A + f_k(ι)·G, J_ι = H + f_e(ι)·G for every (i, j).
3. **RA ↔ LA1, LA2**: RA requests pre-linkage values plv_x(i, j) for the device's linkage chain (identified by LCI = ls_x(0) encrypted by LA_x to itself). LAs pre-compute chains; each plv is individually encrypted for the PCA (RA cannot read them).
4. **RA → PCA** (one request per certificate, after shuffling): {to-be-signed certificate with cocoon key B_ι, response-encryption public key J_ι, encrypted plv1(i,j), encrypted plv2(i,j), hash of RA-to-PCA request}. PoC shuffle threshold: "10,000 requests or a day's worth of requests, whatever is reached first" ([BRECHT] §V-B).
5. **PCA**: decrypt plvs, lv(i,j) = plv1 ⊕ plv2, insert lv, generate random c_ι, butterfly public key B_ι + c_ι·G, **implicitly sign** (ECQV) → certificate + private-key reconstruction value; encrypt both to J_ι; **sign the encrypted packet**; → RA.
6. **RA**: collects one week's worth (i-period) per device → **batch** (zip); stores in per-device repository.
7. **EE ← RA** (Step 3.3 / 3.5 download): EE downloads LPF, LCCF, then batch files `X_Y.zip` (X = low 8 bytes of SHA-256 of request, hex; Y = i-value, hex), "either all available files or as many as possible", then `X.info` containing one Time32 = time at which the next batches will be available (value 0 = RA stopped generation). EE derives private keys b'_ι = a + f_k(ι) + c_ι.

Message-content facts:

| Item | Value | Source |
|---|---|---|
| Butterfly expansion function | f_k(ι) = int(AES_k(x+1)⊕(x+1) ‖ AES_k(x+2)⊕(x+2) ‖ AES_k(x+3)⊕(x+3)) mod l, x = (0^32 ‖ i ‖ j ‖ 0^32) for signing, (1^32 ‖ i ‖ j ‖ 0^32) for encryption; curve NIST P-256 | [BRECHT] §IV eq.(1)–(2) |
| 1609.2.1 BKM parties/keys | three parties EE, RA, ACA; EE holds two caterpillar key pairs and two PRF keys; produces two sets of cocoon keys | [BKM24] abstract |
| Upload size motivation | one signing seed + one encryption seed + two expansion functions replace thousands of keys | [BRECHT] §IV |
| Initial request size | 3,120 certificates = 20/week × 52 × 3 years ("3,000 (3,120 to be exact)") | [CAMP-EE] §2.2.7.6.1 |
| Encoding of batch download | HTTP, header `Batch-Download-Req`, SignedAuthenticatedDownloadRequest; TLS_ECDHE_ECDSA_WITH_AES_128_CCM minimum | [CAMP-EE] §2.1.7.3, SCMS-341 |
| What RA stores | enrollment cert + validity, hash of each RA-to-PCA request | [BRECHT] Table II |
| What PCA stores | encrypted plvs + (i, j), lv, certificate, hash of RA-to-PCA request | [BRECHT] Table II |
| What LA stores | initial linkage seed, pre-linkage values | [BRECHT] Table II |
| RA-side delay | "New devices may experience some delay between the initial request and the time the first certificate batches are available … to accommodate … shuffling, certificate generation, and certificate encryption." No numeric bound given. | [CAMP-EE] §2.2.7.6.7.1 |
| Initial-provisioning shuffle | "Due to the time constraints imposed by the OEMs, shuffling requirements for the initial provisioning may be relaxed." | [CAMP-EE] §2.2.7 |

### A.4 Top-up (re-download) behaviour

| Item | Value | Source |
|---|---|---|
| Certificates pre-generated in advance | "RA and ACA cooperate to generate certificates up to three years in advance of the current time"; EE "downloads up to three years' worth … when connectivity is available" | [WHYTE22a] p.11 |
| Steady state | PCA generates 3 years' worth, "then add a new weeks' worth of certificates every week to ensure there are always 3 years of certificates available"; OBUs download "however many weeks' worth … the device vendor configures, up to 3 years" | [PRIMER] pp.5,7 |
| Schedule communicated to EE | `.info` Time32 = "date and time the RA is predicted to update certificate batches in the device repository"; EE "checks that, and if necessary waits until, the current time matches or is after the timestamp" | [CAMP-EE] §2.2.7.7.7, §2.2.7.8.4 |
| Design-level cadence example | "The RA will provide information when to expect new certificate batches (e.g., once per month)" | [BRECHT] §II-C |
| Top-up is incremental | "this is an incremental download, not a full download of all available certificate files"; EE skips files already held | [CAMP-EE] §2.2.7.8.8 |
| LPF/LCCF | must be downloaded "every time it connects to RA" before any request/download | [CAMP-EE] §2.2.7.6.2 |
| Inactivity | RA records last connection time; stopping pre-generation after inactivity is "Not Doing" for PoC; RA resumes pre-generation when OBE reconnects | [CAMP-EE] §2.2.7.8.8–.9 |
| 1609.2.1 stance | "does not hardwire advance generation time, download time"; SCMS Manager policy pending; "shorter download periods reduce CRL size" | [WHYTE22a] p.11 |
| Storage guidance | OBE "may terminate the certificate batch download process if sufficient storage is not available" | [CAMP-EE] §2.2.7.8.5 |
| Interruption | downloads resumable; batches organized as files named by device info and time period | [BRECHT] §II-C |

### A.5 Pseudonym-certificate parameters

| Item | Value | Source |
|---|---|---|
| Validity period (i-period) | 1 week; i-period length = 10,080 minutes (encoded in minutes because 1609.2 duration ≤ 2^16 units) | [BRECHT] §II-C; [CAMP-EE] §2.1.5.3.2 |
| Overlap | extended from 1 minute (old design) to **1 hour**; lifetime = 10,140 minutes ("1 week + 1 hour") | [CAMP-EE] §2.1.5.3.2, Table 2.1.2.6.2 |
| Certificates concurrently valid | minimum 20 per week; "20 + 20 (for just 1 hour)" during overlap | [BRECHT] §II-C; [CAMP-EE] Table 2.1.2.6.2 |
| j index | reset to 0 at start of each i-period; "fixing the range of j as 1-20 would imply that 20 certificates are valid simultaneously" | [CAMP-EE] §2.1.5.3.1, §2.1.5.3 |
| Start of validity | seconds since 1609.2 epoch 00:00:00 UTC 1 Jan 2004; each batch starts 60·10,080 s after the previous (leap-second alignment out of PoC scope) | [CAMP-EE] §2.1.5.3.2 |
| Rollover instant (US) | "Rollover at 5 am Eastern on Tuesday (least traffic)" | [WHYTE22a] p.10 |
| Covered time span | 1–3 years | [BRECHT] §II-C |
| Per-vehicle variants | NYC CV Pilot taxis: 60/week (12 h/day driving); 20/week sized for ~2 h/day driving | [PRIMER] p.7 |
| Linkage value length | 9 bytes (72 bits); sized for 2.5×10^8 cars × 40 certs/week | [BRECHT] §V-C.3 |
| EU comparison (in same deck) | 60–100 certs/week; 90 with duration exactly a week + 10 with 2-hour duration overlapping the transition | [WHYTE22a] p.10 |

### A.6 Pseudonym change strategy (US guidance)

| Item | Value | Source |
|---|---|---|
| SCMS design assumption | "frequent changes in the certificates accompanying BSMs (e.g., every 5 minutes, or as specified in [SAE J2945/1 201603])"; change strategy "ongoing research" | [BRECHT] §II-A |
| SAE J2945/1 CERTCHG | change every 5 minutes, exception if "separated by less than 2 kilometers in absolute distance from the location at which the last certificate change occurred"; NYC pilot changed to "every 2 KM traveled or every 5 minutes – whichever comes first" | [PRIMER] pp.7–8 |
| SAE J2945/1 (as summarized by ETSI) | at startup then every 5 minutes; change DE_MsgCount, TemporaryID, MAC; lock while Critical Event Flags set; vCertChangeDistance rule | [TR103415] §4.3.1, Table A.3 |
| Whyte summary | "Every five minutes unless device hasn't moved; No significant protection against reuse" | [WHYTE22a] p.10 |
| C2C-CC-style usage example in SCMS | "use a certificate for 5 minutes after start-up, then switch to another certificate, and use that for a longer time-period … or even until the end of the journey" | [BRECHT] §II-C |
| Linkability consequence of 20/week | certs not used in a week cannot be linked; reuse is "linkable only within a week" | [BRECHT] §II-C |

### A.7 Flow 3 — Misbehavior report (EE → LOP → RA → MA)

Sequence ([CAMP-EE] §2.2.8 "Use Case 5"; [BRECHT] §VI-A):
1. Reporting condition met (local detection; algorithms not defined in PoC).
2. EE creates report, **signs with a pseudonym certificate**.
3. EE **encrypts report to the MA** certificate (obtained at bootstrap).
4. EE submits to RA; 4.1 LOP removes identifiers (MAC, IP) and forwards; 4.2 RA **shuffles** and sends to MA individually — shuffle threshold **10,000 reports or one day**, whichever first (PoC value; SCMS-765/686).
5. Unsent reports older than one week may be deleted by the EE if memory is short.

| Item | Value | Source |
|---|---|---|
| Report content (design) | reported (suspicious and alert-related) BSMs, their pseudonym certificates, reporter's pseudonym cert + signature; "format … not fully defined"; preliminary EE-MA ASN.1 at stash.campllc.org ee-ma.asn | [BRECHT] §VI-A, ref [26] |
| PoC ASN.1 status | "interface given is to be handled as draft" pending "Misbehavior Authority Integration" sub-project | [CAMP-EE] §2.2.8.5 |
| 1609.2.1 stance | provides upload interface + encrypted encapsulation for MBRs; "does not provide misbehaviour report formats"; EE authenticates with current AT (cl. 6.3.5.6); payload encrypted for MA at generation (cl. 4.1.5) | [C2C-MBD21] §3.2 quoting IEEE 1609.2.1-2020 |
| Format direction | "Misbehavior reporting based on ETSI TS 103 759 (almost complete)" | [WHYTE22a] p.14 |
| When uploaded | "whenever they are in range of an RSU that supports SCMS connections" | [PRIMER] p.5 |
| Pre-processing stage (1609.2.1 model) | "validates / aggregates / shuffles misbehavior reports"; reporting subsystem manages creation / storage / transmission budgets | [WHYTE22b] p.21 |

### A.8 Flow 4 — MA investigation (MA ↔ PCA ↔ LA ↔ RA)

Investigation ([BRECHT] §VI-C, Fig. 6):
1. MA receives reports containing reported cert with lv = plv1 ⊕ plv2.
2. MA runs global misbehavior detection to select certs of interest.
3. MA → PCA (signed): map lv → encrypted (plv1, plv2) from PCA database; PCA → MA.
4. MA → LA1 (or LA2): "do these two encrypted plv point to the same device?" → binary answer (LA may reveal correlation "in a controlled manner").

Collaboration matrix ([BRECHT] §VI-B): (1) MA + PCA + one LA reconstruct linkage information; (2) MA + PCA + RA + both LAs produce CRL revocation information; (3) MA + PCA + RA determine the enrollment cert for RA blacklisting. PCA/LA should require proof of MA authorization, rate-limit, and log every request.

### A.9 Flow 5 — Revocation + blacklisting (MA → PCA → RA → LA1/LA2 → CRLG)

Sequence ([BRECHT] §VI-D, Fig. 6):
3. MA → PCA (signed): lv → hash h of RA-to-PCA request + RA hostname.
4. MA → RA (signed): h. RA adds the corresponding enrollment certificate to its internal blacklist (does **not** reveal it); RA → MA: LA hostnames + LCI array (lci1, lci2).
5. MA → LA1, LA2: lci_x → ls_x(i) for the current period i + la_id_x. Only forward seeds computable → backward privacy.
6. CRLG adds {ls1(i), ls2(i)} to CRL; CRL states current i and the la_id pair; entries with same LA-ID pair grouped; CRLG signs and publishes.

Non-pseudonym certificates ([BRECHT] §VI-E): MA → PCA hash of request → MA → RA blacklist + list of hashes of non-expired certs → MA adds CertIDs (truncated hash, e.g., 8 bytes) of all non-expired certs to CRL.

| Item | Value | Source |
|---|---|---|
| Passive revocation via RA | blacklisted enrollment cert ⇒ RA refuses new batch generation ("raBlacklisted"); pre-generation stops when "device is blacklisted at the RA due to misbehavior or malfunction" | [BRECHT] §VI-D; [CAMP-EE] §2.2.7.6.7.1, error code raBlacklisted |
| Reinstatement | "no way to undo a revocation"; device must repeat bootstrapping | [BRECHT] §VI-F |
| PoC placeholder | "every reported pseudonym certificate leads automatically to a revocation of all pseudonym certificates belonging to this OBE for testing purposes" (until MA Integration, PoC 2.0) | [CAMP-EE] §2.2.10 |
| Operational MA in pilots | "An MA operator will then review those reports, and … add misbehaving devices to the CRL" (manual MVP) | [PRIMER] p.7 |

### A.10 CRL content, OBU processing, cost, size, cadence, distribution

| Item | Value | Source |
|---|---|---|
| CRL entry (pseudonym) | ls1(i), ls2(i), la_id1, la_id2, current period i, j_max | [BRECHT] §VII |
| Seed chain | ls_x(i) = H_16(la_id_x ‖ ls_x(i−1)) (16 MSB of SHA-256); initial seed 128-bit random | [BRECHT] §V-C |
| Pre-linkage value | plv_x(i,j) = [AES_{ls_x(i)}(la_id_x ‖ j) ⊕ (la_id_x ‖ j)]_9 (Davies-Meyer, 9 bytes) | [BRECHT] §V-C; [CAMP-EE] §2.1.5.3 (plv(i,j) = AES_ls(i)(j)) |
| OBU expansion | "forward hashes both seed values individually, calculates the pre-linkage values of the current time-period and then XORs"; before end of each i-period compute next i-period's LVs; drop entries of devices with no more valid certs; if OBU finds itself on CRL it stops transmitting | [BRECHT] §VII; [CAMP-EE] §2.2.10.2 (Step 8.4) |
| Cost per entry | seeds: 2·(t_c − t_s) hashes (one chain per LA) to reach current period, i.e., **2 hashes per entry per i-period** if kept current; then up to σ (=j_max, 20) AES per LA per period to enumerate all LVs (≈40 AES per entry per week), or a lookup table | [ACPC] §2 items (a),(b); [BRECHT] §VII |
| Why (i,j) refinement | without it a vehicle off for 100 days would need 100 × 288 hashes per entry (5-minute certs) at key-on | [CAMP-EE] §2.1.5.3 |
| CRL size | ≥10,000 entries storage assumed by all OEMs ≈ **400 KB** (≈40 B/entry); "grows linearly with the number of revoked entities"; entries tagged for prioritization (location, severity) — prelim design in IEEE 1609.2-2016 | [BRECHT] §VI-F |
| Revocation-rate implication | 10,000-entry CRL ⇒ "revocation rate of only 3,000" (per year, with 3-year batches) | [BCAM] §1 |
| Entry lifetime | equals batch coverage (1–3 years; up to 30 years in BCAM) | [ACPC] §1 |
| Cadence (design discussion) | "may be appropriate to update the end-entity CRL daily, but the SCMS component CRL once a month … or publish CRLs continually" | [BRECHT] §VI-G |
| Cadence (PoC requirement) | not stated numerically in [CAMP-EE] Release 1.1 (CRL Store "shall provide a CRL for download at any given time", SCMS-340) — **UNVERIFIED** for a weekly/daily PoC schedule | [CAMP-EE] §2.2.9.5 |
| Download trigger | "CRL updates are downloaded whenever a device connects to the SCMS"; "Devices download new CRLs through the RA every time they connect" | [PRIMER] pp.4,7 |
| Download protocol | EE HTTP GET to CRL Store; no EE authentication; TLS ECDHE-ECDSA-AES128-CCM | [CAMP-EE] §2.2.9, SCMS-335/340/341 |
| Broadcast | CRLB via RSEs, satellite; composite single file of all CRL series at central CRL Store | [BRECHT] §II-B, §VI-G |
| Epidemic distribution | "collaborative distribution": seeded by RSEs/cellular, then V2V; "[HAAS11] shows that the area of Zurich can be provided with a CRL using a single RSE within a few hours"; open issue: 6 Mbit/s channel | [BRECHT] §VII |

---

## B. ETSI ITS PKI (TS 102 940 / TS 102 941 V2.2.1 / TS 103 759 / EU CP)

### B.1 Entities

| Entity | Role | Source |
|---|---|---|
| Policy Authority | top-level governance; designates TLM and CPOC; approves/removes Root CAs | [TS102940] §7.1 Table 9 |
| Trust List Manager (TLM) | creates and signs list of Root CA certs + TLM certs = ECTL | [TS102940] Table 9; [TS102941] §6.3.1 |
| C-ITS Point Of Contact (CPOC) | collects Root CA certs, gives to TLM, distributes ECTL; mandatory in EU CP | [TS102940] Table 9, §7.5 NOTE |
| Root CA (RCA) | trust anchor; issues EA/AA certs; issues CRL and RCA-CTL | [TS102941] §6.3.2–6.3.3 |
| Enrolment Authority (EA) | issues enrolment credentials (EC); validates AT requests; in butterfly mode requests ATs from AA on ITS-S behalf | [TS102940] Table 8 |
| Authorization Authority (AA) | issues Authorization Tickets (ATs) | [TS102940] Table 8 |
| Distribution Centre (DC) (optional) | publishes CTL and CRL of an RCA | [TS102940] Table 8; [TS102941] §6.3.2–6.3.3 |
| Misbehaviour Authority (MA) | collection, investigation, response (no action / alert / initiate revocation e.g. blocking EC); talks to EA and AA; interface to manufacturer SOC; needs RCA-signed cert | [TS102940] §7.6 |
| Misbehaviour Pre-processing (optional) | acts on MRs before MA (privacy, context) | [TS103759] §4.1 |

Transport ([TS102941] §6.2.2): messages are media-agnostic; over IP, **HTTP/1.1 over TCP/IP at S2/S3, "No supplementary cryptographic layer such as TLS is required"** (all messages are ETSI TS 103 097 signed/encrypted). Connectivity options listed: ITS-G5 via roadside station (GeoNetworking one/multi-hop, relaying vehicles), WLAN hotspot, cellular, EV charging station, OBD at garage.

### B.2 Flow — Enrolment (ITS-S → EA, reference point S3)

Sequence ([TS102941] §6.1.3, §6.2.3.2, Fig. 13):
1. Manufacture: ITS-S has canonical key pair + canonical identifier (direct enrolment) or EA has out-of-band assurance (indirect enrolment).
2. ITS-S generates a fresh verification key pair per request.
3. Build InnerECRequest {itsId = canonical ID (initial) or HashedId8 of current EC (re-enrolment), certificateFormat = ts103097v131, verificationKey, requestedSubjectAttributes (no certIssuePermissions; validity/region optional)}.
4. Inner signature for proof-of-possession with the new key (InnerECRequestSignedForPOP); outer signature with canonical private key (initial) or current EC key (re-enrolment).
5. Encrypt whole EnrolmentRequest to EA public key (ECIES, TS 103 097 algorithms) → EA.
6. EA → ITS-S: EnrolmentResponse signed by EA; contains EC (or error).

### B.3 Flow — Authorization, standard variant (ITS-S → AA (S2); AA → EA (S4))

Sequence ([TS102941] §6.2.3.3–6.2.3.4, Figs. 16, 20):
1. Privacy prerequisite: ITS-S performs an **ID change** (IPv6/MAC/GN address) before each AT request unless one is pending ([TS102941] §6.2.2 Itss_WithPrivacy).
2. ITS-S generates new verification key (+ optional encryption key), a random 32-octet hmac-key, keyTag = HMAC-SHA256(hmac-key, verificationKey‖encryptionKey) truncated to 128 bits.
3. SharedATRequest {eaId, keyTag, certificateFormat, requestedSubjectAttributes}.
4. Detached EC signature over hash of SharedATRequest (psid "secured certificate request"); **encrypted to the EA** so the AA never sees the EC (Itss_WithPrivacy).
5. InnerATRequest {publicKeys, hmac-key, sharedAtRequest, encrypted ecSignature} → EtsiTs102941Data → encrypted to AA → AuthorizationRequest (S2).
6. **AA → EA**: AuthorizationValidationRequest {sharedAtRequest, ecSignature}, signed by AA, encrypted to EA (S4).
7. EA validates the EC (and its internal blacklist), checks that the keyTag matches without learning the keys; → AuthorizationValidationResponse (signed by EA, encrypted to AA).
8. AA issues AT, → AuthorizationResponse (encrypted to ITS-S) (S2).
9. Repeat per AT; "an ITS-S may perform several Authorization Requests to fill a pool of ATs … pool management out of scope" ([TS102941] §6.1.4 NOTE 1).

### B.4 Flow — Authorization with butterfly keys (ITS-S → EA (S3); EA → AA (S4)); "based on the solution described in IEEE 1609.2.1"

Sequence ([TS102941] §6.2.3.5, Fig. 23):
1. ITS-S → EA: ButterflyAuthorizationRequest (or X509SignedButterflyAuthorizationRequest), encrypted to EA; a **new caterpillar key pair per request**; expansion parameters either "original" (ButterflyParamsOriginal) or "unified" (fresh 16-byte expansion key). No encryption keys in butterfly ATs (NOTE 1).
2. EA → ITS-S: ButterflyAuthorizationResponse {version = 2, generationTime, **currentI**, requestHash (left 16 octets of SHA-256 of the request), **nextDlTime** = time after which to attempt download, acpcTreeId absent}.
3. EA expands keys (cocoon keys) and sends **multiple ButterflyCertRequest** messages to the AA (S4), each encrypted to the AA.
4. AA → EA: ButterflyCertResponse containing encrypted AT (AcaEeCertResponsePrivateSpdu per IEEE 1609.2.1 §7.3.4), encrypted with the AES key from the request.
5. ITS-S → EA: ButterflyAtDownloadRequest (signed with EC; may be plain REST); download "shall be authorized … e.g. using an **internal blocklist at the EA** or an OAuth token, to ensure that the ITS-S has not been revoked".
6. EA → ITS-S: collection of AcaEeCertResponsePrivateSpdu (the AT batch).

Interoperability: an implementation must support one of the two AT provisioning variants; V2V interoperability is unaffected ([TS102941] §6.2.3.1). Whyte's summary of the ETSI model: "Each certificate is requested individually directly from the ACA and immediately issued; Devices can download up to three months of certificates in advance (no end entity revocation)" ([WHYTE22a] p.11).

### B.5 Trust lists, CA-CRL, transmission

| Item | Value | Source |
|---|---|---|
| ECTL (TLM CTL) | signed by TLM; ctlSequence monotonic 1..255 (wraps); "valid when received and shall contain a validity end (nextUpdate)"; contents: TLM cert (+link cert), Root CA certs (+link certs), CPOC URL; Full + Delta CTL each update; published via CPOC | [TS102941] §6.3.1, §6.3.4 |
| RCA CTL | signed by RCA; EA certs + URLs (AA-facing and optionally ITS-S-facing), AA certs + URLs, DC URLs; published via DC | [TS102941] §6.3.2, §6.3.4 |
| CRL (CA-level only) | "issued and signed by the RCA … contains the CA certificates identifiers (HashedID8) that are no longer worthy of being trusted"; may contain the RCA itself; expired IDs removed; via DC | [TS102941] §6.3.3 |
| Per-vehicle CRL | none in ETSI: "revocation of authorization tickets is not possible as passive revocation is preferred"; AT-CRLs "not supported by ETSI ITS security standards and … not backward compatible" | [TS102941] §6.1.4 NOTE 4; [C2C-MBD21] §2.2.7 |
| Passive revocation | "the EA shall reject the authorization of all the next Authorization Ticket requests for this specific ITS-S"; EC revocation "implemented by an internal blacklist … never published" | [TS102941] §6.1.6; [EUCP] §7.3.2 |
| Update channels | request from DC; RSU broadcast on ITS-G5; trusted maintenance entity | [TS102941] §6.1.5 |
| G5 broadcast profile | RSU "proxy application which periodically transmits updated trust list information"; single-hop SHB GeoNetworking; Delta CTL only; "No data segmentation/reassembling"; ITS-S re-transmits received TLM/RCA CTL and CRL messages unmodified | [TS102941] §6.3.5, Annex D.3 |
| CPOC interfaces | Annex E: request TLM cert, TLM link cert, full ECTL, delta ECTL (HTTP) | [TS102941] Annex E |
| Delta CTL semantics | generated w.r.t. previous full list (ctlSequence − 1); add = CtlEntry, delete = CtlDelete (HashedId8 or DC URL) | [TS102941] §6.3.4 |

### B.6 Time constants (EU Certificate Policy 1.1 + C2C-CC + projects)

| Item | Value | Source |
|---|---|---|
| Root CA | private-key usage 3 y, max validity 8 y | [EUCP] Table 11 |
| EA | 2 y / 5 y | [EUCP] Table 11 |
| AA | 4 y / 5 y | [EUCP] Table 11 |
| Enrolment Credential (EC) | 3 y / 3 y | [EUCP] Table 11 |
| TLM | 3 y / 4 y | [EUCP] Table 11 |
| Validity constraint | maxvalidity(AA) = privatekeyusage(AA) + preloadingperiod(AT), etc. | [EUCP] §7.2 |
| AT validity | "shall not exceed 1 week" | [EUCP] §7.2.1 |
| AT preloading | "shall not exceed 3 months" (from request to latest end of validity) | [EUCP] §7.2.1 |
| Parallel ATs | ≤ **100** per vehicle ITS-S; ≤ 2 per roadside ITS-S | [EUCP] §7.2.1–7.2.2 |
| AT usage period | ≥ operational time per validity period ÷ number of parallel ATs | [EUCP] §7.2.1 |
| Regular publication | ECTL/CRL composition changes: "period of maximum 3 months"; root CAs publish CRLs "as soon as possible"; stations updated "within 1 week after their publication" under normal operation | [EUCP] §2.2 |
| TLM/Root CA cert preload | on ECTL "a maximum of 3 months and at least 1 month before their validity starts" | [EUCP] §7.2 |
| CA revocation | on CRL "as soon as possible and without undue delay"; TLM removes root from list and issues new list | [EUCP] §7.3.1 |
| Incident reporting | corruption of CA resources reported to root CA within 24 h | [EUCP] §5.7 (line context) |
| C2C-CC pseudonym lifetime | "Maximum 1 week + overlap period"; pool ≈ 1,040/year; tank (preload) 3 years; **20 parallel** pseudonyms; unlimited reuse until validity end | [TR103415] Table A.2 (citing C2C-CC PKI Memo v1.7 and BSP 1.1.0) |
| C2C-CC change rule (BSP) | at ignition-on within 1 min (unless restarted within last 10 min), then random 10–30 min; lock max 15 min (proposal to cut to 5 min); ETSI max lock 255 s | [TR103415] Table A.2, §4.4.5 |
| C2C-CC newer strategy (privacy position paper) | trip split in ≥3 unlinkable segments for ≥95% of trips: change at trip start (ignition off ≥10 min AND on AND movement); next change at random 800–1,500 m; further changes ≥800 m apart AND within 2–6 min | [TR103415] §4.2.3.2 |
| SCOOP@F | 1-week slot; 10 parallel; 6-month preload; round-robin; change after 40,000 signatures or 1 h, and at startup | [TR103415] Table A.1 |
| C2C-CC 2015 SLA-style figures | revocation status available 24/7; pseudonym-resolution data retained validity + 3 months; MBRs on CAs within 1 h; vulnerabilities reported within 1 work day | [C2C-POS15] §3.3.1–3.3.2 |
| DC availability | no numeric availability figure found in [TS102941]/[EUCP] — **UNVERIFIED** | — |
| Certificate in CAM | AT inserted in security header every 1 s or after AT change; digest otherwise; signature per CAM | [TR103415] §4.3.2.3 (summarizing TS 103 097) |

### B.7 Misbehaviour reporting (TS 103 759) and MA actions

Sequence ([TS103759] §4–7):
1. Local MDS detectors (Classes 1–5: implausible values; inconsistency with previous same-type messages from same station; with local environment/LDM; with on-board sensors; with other-type/other-station messages) → Event Categorization → Decision whether to generate MR (budgets: creation, storage, transmission).
2. Build EtsiTs103759Data {version, generationTime, observationLocation, report = AidSpecificReport {aid, content = TemplateAsr {observations (ObservationsByTarget {tgtId, observations}), v2xPduEvidence (V2xPduStream {id, type, v2xPdus, certificate, subjectPduIndex}), nonV2xPduEvidence}}}, COER-encoded.
3. Wrap: EtsiTs103097Data-Signed (signer = digest HashedId8 of reporter's **AT**, psid = MRS, generationTime) → EtsiTs103097Data-Encrypted (ECIES to **MA certificate**) → EtsiTs103097Data-SignedAndEncrypted-Unicast.
4. Store; transmit "when connectivity is available"; "A vehicle does not wait for a decision response"; paths: wired (RSU), G5 via RSU gateway (S1→S5), cellular (S5), Wi-Fi, EV charger, OBD.
5. Optional Misbehaviour Pre-processing; MA: collection (validate/filter/correlate/store), investigation, response.
6. MA → EA/AA: obtain ITS-S info, trigger reaction: no action / alert user or manufacturer / **initiate revocation (e.g., blocking the EC)**; inform manufacturer/operator on every malicious/faulty classification.

| Item | Value | Source |
|---|---|---|
| Evidence rule | MR must include original received message(s) with AT so MA can re-verify (non-repudiation); Class 2/5 need ≥2 messages | [TS103759] §4.2.4, §6.2 Table 3 |
| Common observation IDs (examples) | Beacon-IntervalTooSmall(1); Static-Change(1); Security-* (1–7: msgId/PSID/SSP/time/location vs certificate); Position-ChangeTooLarge(4); Speed-ValueTooLarge-VehicleType(3); Speed-ChangeTooLarge(5); LongAcc-ValueTooLarge(4) | ETSI forge EtsiTs103759CommonObservations.md (v2.1.1) |
| MA reaction menu (C2C-CC/SCA) | passive revocation (revocation by expiry), active revocation, deactivation; SCA protocol: malicious ⇒ EA passive revocation via internal blocking list (IBL); faulty ⇒ suspension pending EA + manufacturer investigation | [C2C-MBD21] §2.2.7 |
| Broadcast "self-deletion" alternatives | CoPRA, PUCA, REWIRE / O-Token (formally analysed) — research, not standardized | [C2C-MBD21] §2.2.7 |
| Report size / rate limits | not specified numerically in [TS103759] ("MRs should not overload the communication channel") — **UNVERIFIED** | [TS103759] §4.2.4 |

---

## C. Revocation-latency literature (measurements / models)

**Finding:** no published end-to-end measurement (report → MA decision → CRL → enforcement) for the SCMS PoC or the ETSI PKI was located in this session; the CAMP SCMS PoC "did not include misbehavior detection, and was discontinued during phase 2 of the [CV] Pilots" (USDOT CV Pilot Phase 2 report, https://rosap.ntl.bts.gov/view/dot/42690 — from search summary, **UNVERIFIED**, not read). The pieces below are the cited building blocks for a latency model.

### C.1 Component delays with sources

| Stage | Quantity / model | Source |
|---|---|---|
| Report generation → upload | store-and-forward; "not a real time process"; EE may delete unsent reports > 1 week old | [TS103759] §5.1; [CAMP-EE] §2.2.8 Step 5 |
| RA shuffling delay (US) | ≤ 10,000 reports or ≤ 1 day, whichever first (PoC) | [CAMP-EE] SCMS-765 |
| MA decision | manual operator review in pilots; no SLA published | [PRIMER] p.7 |
| CRL publication cadence (US) | design options: daily (EE CRL), monthly (component CRL), or continuous; PoC numeric cadence **UNVERIFIED** | [BRECHT] §VI-G |
| CRL fetch by vehicle (US) | on every SCMS/RA connection; also RSU/satellite broadcast; epidemic V2V | [PRIMER] p.7; [BRECHT] §II-B, §VII |
| Epidemic V2V CRL, city scale | "area of Zurich … a CRL using a single RSE within a few hours" (Brecht's reading of [HAAS11]); [HAAS11] abstract: V2V exchange "quicker than distributing CRLs through RSUs alone" | [BRECHT] §VII; [HAAS11] |
| Vehicle-centric CRL pieces | ≤ 25 KB/s load ⇒ latest CRL to 95 % of vehicles in a 50×50 km region within 15 s (τP = 60 s), ">40 times faster than the state-of-the-art"; cognizant nodes at t = 50 s: 39 % (pseudonym lifetime 30 s) vs 76 % (600 s); avg latency 6.91 → 6.23 s as RSUs 25 → 100 (R = 1 %) | [KHODAEI] abstract, §5.2.3 (note: the arXiv abstract webpage renders the region as "15 x 15 KM"; the PDF text says 50×50) |
| CRL size example (ETSI-style pseudonyms) | 1 % of vehicles revoked, 6 pseudonyms/Γ (30 min), 5-min pseudonyms ⇒ 720,000 entries/day ≈ 22 MB (256-bit serials); batching 12 entries/vehicle ⇒ 7.3 MB; per 30-min Γ_CRL ≈ 10,000 entries ≈ 625 KB | [KHODAEI] §3 |
| CRL entry cost model (SCMS) | size ≈ 40 B/entry (400 KB / 10,000); entry stays on CRL for remaining batch coverage (≤ 3 y); check cost linear in #entries: 2 hashes/entry/period + up to 2σ AES | [BRECHT] §VI-F; [ACPC] §1–2 |
| Broadcast budget example (BCAM) | 10,000 hard-revoked vehicles: DSVs downloadable in < 10 min; 100,000: "a little over an hour" (64 kbps channel); "one broadcast distribution cycle (typically a week)"; CRL removable if "a 1-week delay in removing HSM-compromised devices were deemed acceptable" | [BCAM] §3.3.2, §4.4 |
| ETSI passive revocation lag | bounded by AT stock: preload ≤ 3 months + AT validity ≤ 1 week (EU CP); C2C-CC tank = 3 years ⇒ passive-only eviction could lag up to the preloaded horizon | [EUCP] §7.2.1; [TR103415] Table A.2 |
| Formal analysis of revocation protocols | J. Whitefield et al., "Formal Analysis of V2X Revocation Protocols," 2017 (cited; not fetched) | [C2C-MBD21] ref RD-8 |
| CRL organisation alternatives | Haas 2009 lightweight revocation (cited in [KHODAEI] ref [38]); Bloom-filter CRLs, activation codes (IFAL, ACPC), BCAM hash trees | [KHODAEI] §2; [ACPC] §3; [BCAM] |

Note on "Kumar & Petit CRL distribution analysis": no separate Kumar & Petit CRL-distribution paper was found; the closest primary work by those authors is [BCAM] (Kumar, Petit, Whyte, WiSec 2017), whose CRL-size and download-time numbers are used above. **UNVERIFIED** that another such paper exists.

---

## D. Privacy metrics and pseudonym-change findings

| Item | Value | Source |
|---|---|---|
| Attack model (Wiedersheim) | passive observer with full-area coverage; Multi-Hypothesis Tracking with Kalman filter; tracking success = mean correctly tracked duration over 1000 s simulations (TIGER-based city traces) | [WIED10] §III–V |
| Fresh pseudonym per message | at 1 Hz beacons and low density, mean tracked ≈ 800 s of 1000 s; ≈ 700 s at higher densities | [WIED10] §V-B, Fig. 3 |
| Pseudonym-change interval | "pseudonym change intervals of 4 seconds and above lead to almost 100 % tracking success" (1 Hz beacons) | [WIED10] §V-B, Fig. 4 |
| Penetration rate | 10–20 % equipped ⇒ mean tracking > 900 s (tracks rarely cross) | [WIED10] Fig. 5 |
| Position noise | σ = 1–2 m random noise "already reduces the tracking success … very effectively" (but incompatible with safety accuracy) | [WIED10] Fig. 6 |
| Headline conclusion | 1 Hz, change every 10 s, 20 % equipped ⇒ tracking "with an accuracy of almost 100 %" | [WIED10] §VI |
| Effective anonymity set size | S = −Σ_u p_u log₂ p_u, 0 ≤ S ≤ log₂|Ψ| (Ψ = anonymity set; p_u = attacker's posterior) | [TR103415] §5.1.2.3 |
| Degree of anonymity | d = S / S_M, S_M = log₂|Ψ|; applied as: Ψ = target + nearby vehicles at the change event, p_u = attacker's linking posterior | [TR103415] §5.1.2.4–5.1.2.5, Table 2 |
| User-centric location privacy | privacy loss function of time since last successful change (linear decay model) | [TR103415] §5.1.3 |
| PRESERVE evaluation of SAE J2735 rule | change every 120 s + random silent period 3–13 s: "decent privacy but drastically decreases safety" | [TR103415] §4.2.1 |
| Mix zones (origin) | mix zone construct + anonymity-set metrics on 3-million-sample Active Bat data | [BS03] (metadata; 770 citations) |
| Vehicular cryptographic mix zones | proposed at appropriate places (intersections) using cryptography | [FREUD07] (metadata; 521 citations) |
| Mix-zone linkability measured | eavesdropper links ≈ 73 % of pseudonyms (non-rush) / ≈ 62 % (rush hour) across cryptographic mix zones using standard messages + road layout; decoy relaying by 50 % of vehicles reduces linking 68 % → 18 %; overhead 4.67 ms/s + 1.46 KB/s per vehicle, 28 ms/s + 45 KB/s per RSU | [KHAN20] abstract |
| Survey taxonomy | abstract pseudonym lifecycle; PKI-, ID-based, group-signature and symmetric schemes | [PETIT15] abstract |
| SCMS 20/week linkability | unused certs unlinkable; reuse linkable within a week; CAMP recommends ~5-min changes (see §A.6) | [BRECHT] §II-C |
| TR 103 415 recommendation on Sybil | concurrently valid pseudonyms "would be very low … Ideally the minimum value … would be 2" | [TR103415] §8 |
| Emara et al. (CAPS) | not fetched this session — **UNVERIFIED** | — |

---

## E. Threshold / multi-party building blocks (round counts and sizes)

Notation: n parties, threshold t (t+1 signers) unless stated; κ = group-element bits (256 for secp256k1/P-256), ν = Paillier plaintext bits (2048).

### E.1 Distributed key generation and share refresh

| Scheme | Rounds / structure | Per-party communication | Source |
|---|---|---|---|
| Pedersen VSS (1991) | dealer → parties over private links; non-dealer parties speak only via broadcast ("non-interactive") | shares + commitments | [PED91] (metadata; description via search summary) |
| Pedersen DKG (Joint-Feldman) | every party acts as Feldman-VSS dealer; key = product of qualified contributions; known bias attack fixed by GJKR | — | Wikipedia DKG summary; [GJKR] — **UNVERIFIED** round count |
| GJKR DKG | phases: (1) each party Pedersen-VSS-shares two random polynomials; (2) verify, broadcast complaints, accused reveal shares; (3) non-disqualified parties broadcast g^{a_ik}; (4) verify/complain; (5) key = product of qualified contributions; synchronous | n−1 private shares + broadcasts per phase | Wikipedia DKG summary of [GJKR]; primary paywalled — **UNVERIFIED** exact round count |
| GG18 key generation | 3 phases: (1) commit to g^{u_i} + broadcast Paillier key; (2) decommit + (t,n) Feldman-VSS of u_i; (3) ZK Schnorr proof of x_i + square-free proof of N_i | — | [GG18] §4.1 |
| CGGMP21 key generation | 3 rounds; total incoming communication n × (4κ) per party | ≈ n × 128 B | [CGGMP21] Table 1 |
| CGGMP21 aux-info / **key refresh** | 3 rounds; n × (2nκ + (2m+5)ν) incoming | dominated by Paillier proofs | [CGGMP21] Table 1, §1.1 ("three-round key refresh"); epoch security if ≥1 party honest throughout each epoch |
| Herzberg et al. proactive refresh | each party i picks random δ_i(z) with δ_i(0)=0, sends u_{i,j}=δ_i(j) to each other party (n−1 private messages) + broadcast of verification values; x_i^{t+1} = x_i^t + Σ_j u_{j,i}; old shares become useless | n−1 private + 1 broadcast per party per period | [HJKY95] (via Wikipedia PSS) |
| FROST DKG | out of scope in RFC 9591 (trusted dealer Appendix C: Shamir + VSS); paper's DKG is Pedersen-DKG-with-PoK — round count **UNVERIFIED** here | — | [RFC9591] §1, App. C; [FROST20] abstract |

### E.2 Threshold ECDSA

| Scheme | Signing rounds | Per-party communication | Other | Source |
|---|---|---|---|---|
| GG18 | 5 phases, **9 rounds** (Phase 2 MtA pairwise sub-protocol is multi-round) | d(t) = 2,328 + t × 5,024 bytes total sent+received per player; t = 1 ⇒ 3,976 B; CGGMP estimate 10κ + 20ν ≈ 7 KiB pairwise | compute 29 + 24·t ms single core | [GG18] §7.1–7.2; [CGGMP21] Fig. 1; [GG20] §6 ("[27] has 9 rounds") |
| GG20 (anonymous abort) | 6 rounds = 5 offline + **1 online** | "less bandwidth than [GG18]" (no figure) | online = 1 EC mult + add, 0.0008 ms | [GG20] §1, §6 |
| GG20 (identifiable abort) | 7 rounds = 6 offline + 1 online | as above | full run 2.5–4.3 s for 7–10 players, single thread (Table 1) | [GG20] §6, Table 1 |
| CGGMP21 (2024 rev.) interactive | **3 rounds** | 65κ + 50ν ≈ **15 KiB** pairwise (κ=256, ν=2048); group ops 25n, ring ops 75n | UC, proactive, identifiable abort | [CGGMP21] Fig. 1 |
| CGGMP21 non-interactive | 3 presign rounds + 1 online round (single field element, 256 bits) | presign n × (65κ + 48ν) incoming; sign n × κ | broadcast, synchronous model | [CGGMP21] Table 1, §1.1 |
| Lindell et al. (Paillier / OT) | 8 rounds | 7.5 KiB / 190 KiB | — | [CGGMP21] Fig. 1 |
| Doerner et al. 2018 / 2023 | log(n) / 3 rounds | 90 KiB / 50 KiB | OT-based | [CGGMP21] Fig. 1 |
| Castagnos et al. 2020 | 4 rounds | 100κ ≈ 3.5 KiB | class groups | [CGGMP21] Fig. 1 |

### E.3 Threshold Schnorr / EdDSA — FROST

| Item | Value | Source |
|---|---|---|
| Rounds | **2** (Round 1 commitment, Round 2 signature share); or 1 round with preprocessed commitments | [RFC9591] §1, §5; [FROST20] abstract |
| Round 1 message | two group elements (hiding, binding nonce commitments): 64 B (Ed25519, ristretto255), 66 B (P-256, secp256k1), 114 B (Ed448) | [RFC9591] §5.1 + ciphersuites |
| Round 2 message | one scalar: 32 B (Ed25519/ristretto255/P-256/secp256k1), 57 B (Ed448) | [RFC9591] §5.2 |
| Coordinator | required: selects participants, relays commitments, aggregates shares, publishes signature | [RFC9591] §5, §5.3 |
| Signature | 64 B (Ed25519/ristretto255), 65 B (P-256/secp256k1), 114 B (Ed448) | [RFC9591] |
| Threshold | true t-of-n; no trusted dealer at signing; unlimited concurrency | [FROST20] abstract |

### E.4 Post-quantum threshold signatures

| Scheme | Rounds | Sizes / communication | Parties | Key gen | Source |
|---|---|---|---|---|---|
| Threshold Raccoon (EUROCRYPT'24) | **3** (folklore Schnorr-style + Shamir) | sig ≈ 13 KiB (12.4 KB in Quorus Table 7), vk ≈ 4 KiB, **≈ 40 KiB per user per signature**; 116 ms/signer at T = 1024, verify 0.23 ms | 1 ≤ T ≤ N ≤ 1024 | Shamir by trusted dealer; "design of a suitable DKG is outside of the scope" | [TRACCOON] abstract, §1, §5, Table 2–3; [QUORUS] Table 7 |
| Raccoon 2-round follow-ups (EKT24, BKL+25, ZT25) | 1–2 | e.g., EKT24: 1 round, 14 KB comm, pk 5.5 KB, sig 11.1 KB | ≤ 1024 | — | [QUORUS] §1.2, Table 7 |
| del Pino–Niot "Finally!" (PKC'25) | 3 | sig 2.7 KB, pk 2.6 KB, ≤ 8 parties, ≤ 2^64 sigs | ≤ 8 | — | [QUORUS] §1.2 |
| Threshold ML-DSA, Celi et al. (N ≤ 6) | **3 rounds per attempt** (commit hash; reveal w_i; response z_i) | 10.5 kB – 525 kB per protocol run depending on (T,N); "a few hundred milliseconds in a WAN"; FIPS-204-verifiable | N ≤ 6, any T ≤ N; replicated secret sharing (each secret held by N−T+1 parties) | not specified in text read — **UNVERIFIED** | [CELI25] abstract, §3; [TALUS] Table 1 |
| Quorus (USENIX Sec'26) | online: comm-optimised 149 rounds (1 parallel SIGN) → 35 (8 parallel); round-optimised 81 → 19; ≈ 16–29 rounds per rejection-sampling iteration (TALUS Table 1); offline 81–108 rounds | online ≥ 150 KB per party per rejection round; expected per party for a signature (15 parties, ML-DSA-65): comm-opt 2.2–4.2 MB, round-opt 7.6–14.5 MB; offline 4.1 MB (n=3) … 369.9 MB (n=63); E[iterations] 5.09 (sequential) → 1.20 (8 parallel); WAN RTT 20 ms ⇒ ≈ 160 ms (round-opt) / 290 ms network overhead per rejection round | any n, honest majority, t = (n−1)/2 | MPC-based, UC-secure | [QUORUS] abstract, Tables 3–5, §5 |
| Trilithium (2-party ML-DSA) | 14 rounds per attempt | — | N = 2 (both required); trusted correlated-randomness provider | — | [TALUS] Table 1; [QUORUS] §1.2 |
| TALUS (arXiv Aug 2026, v5) | TEE profile **1 online round**; MPC profile **2 online rounds** (open w1; send z_h); preprocessing O(log N) rounds | single-box compute ≈ 1.5 ms (ML-DSA-65, T = 3); first-attempt success ≈ 99.6 % (65), 98.0 % (44), 99.2 % (87); output = stock FIPS-204 signature | arbitrary N (MPC: N ≥ 2T−1 honest majority; TEE: any T-of-N) | committee nonce-DKG (Shamir) offline | [TALUS] §1 Table 1, §4, §7, §8 (q_s-bounded security) |
| Shamir-Nonce-DKG threshold ML-DSA (arXiv 2601.20917) | not stated in abstract | standard 3.3 KB signatures; "arbitrary thresholds"; coordinator profiles P1/P2/P3+ (P2 needs ≥2 honest parties outside a coalition) | arbitrary | Shamir nonce DKG | [SNDKG26] abstract — rounds **UNVERIFIED** |
| ML-DSA rejection sampling baseline | ≈ 4–5 SIGN attempts expected per signature (same failure rate as FIPS 204) | — | — | — | [QUORUS] §5 |

### E.5 Modelling hints (derived, not new facts)

- A t-of-n threshold ECDSA signer for a CA/RA role can be modelled as: DKG once (3 rounds, [CGGMP21] Table 1), refresh per epoch (3 rounds), presign batches (3 rounds, ≈ 15 KiB per pair), online sign (1 round, 32 B per party) — all values from [CGGMP21].
- A Schnorr/EdDSA threshold signer: 2 rounds, ≈ 66 B + 32 B per participant per signature, or 1 round after commitment preprocessing ([RFC9591]).
- A PQ threshold signer that must remain FIPS-204-verifiable costs either ≤ 6 parties × 3 rounds × ≤ 525 kB ([CELI25]) or tens-to-hundreds of MPC rounds and MBs per signature ([QUORUS]); a threshold-friendly lattice scheme (Raccoon) costs 3 rounds × 40 KiB per signer ([TRACCOON]).

---

## F. Open items / not found (explicit)

| Item | Status |
|---|---|
| Numeric CRL publication cadence of the CAMP PoC (weekly/daily) | not stated in [CAMP-EE] Rel. 1.1; **UNVERIFIED** |
| RA processing-time SLA (request → first batch) | qualitative only ([CAMP-EE] §2.2.7.6.7.1) |
| IEEE 1609.2 CRL ASN.1 field names for linkage entries (e.g., ToBeSignedLinkageValueCrl/iRev/JMaxGroup/LAGroup) | standard not fetched; **UNVERIFIED** |
| End-to-end SCMS/ETSI revocation-latency measurement | none found |
| Emara et al. (CAPS) tracking numbers | not fetched; **UNVERIFIED** |
| Haas 2011 quantitative epidemic-coverage figures | PDF not fetchable; only Brecht's paraphrase ("Zurich … single RSE within a few hours") |
| GJKR / Pedersen-DKG exact round counts; FROST DKG round count | primary texts not fetched; **UNVERIFIED** |
| TS 103 759 V2.2.1 (2026-01) changes | not fetched |
| Exact CAMP Release 1.2.2 edition | wiki unreachable; Release 1.1 PDF used |
