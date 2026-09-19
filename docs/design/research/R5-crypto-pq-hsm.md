# R5 — Cryptographic primitives for V2X: sizes, verify/sign costs, HSM throughput, libraries, SCMS batch/CRL facts

Compiled 2026-09-17. Every number carries a source. Items that could not be confirmed against a fetched primary source are tagged **UNVERIFIED**. "Derived" = arithmetic on cited values, not a published figure.

Method note: WebSearch budget was exhausted mid-task; the remaining facts were pulled with direct fetches and local text extraction of PDFs (pypdf). Where a fetch was blocked (403/404) that is stated in the source column.

---

## A. Exact sizes (bytes)

### A.1 Classical ECC (IEEE 1609.2 encodings)

| Item | Value | Source |
|---|---|---|
| EC point-to-octet-string length rule | compressed: ⌈(log2 q)/8⌉ + 1; uncompressed: 2⌈(log2 q)/8⌉ + 1 | SEC 1 v2.0 §2.3.3 ("mlen = ⌈(log2 q)/8⌉+1 if P ≠ O and point compression is used, and mlen = 2⌈(log2 q)/8⌉ + 1 if ... uncompressed"), https://www.secg.org/sec1-v2.pdf |
| ECDSA P-256 public key | 33 (compressed) / 65 (uncompressed) | Derived from SEC 1 §2.3.3 with 256-bit q; 1609.2 encodes as `EccP256CurvePoint ::= CHOICE { x-only OCTET STRING(32), fill NULL, compressed-y-0 OCTET STRING(32), compressed-y-1 OCTET STRING(32), uncompressedP256 ...}` (32 B + 1 OER choice byte = 33 B) — Ieee1609Dot2BaseTypes.asn, ETSI forge mirror https://forge.etsi.org/rep/ITS/asn1/ieee1609.2 |
| ECDSA P-256 signature | 64 (r ‖ s, 32+32); 1609.2 structure `EcdsaP256Signature ::= SEQUENCE { rSig EccP256CurvePoint, sSig OCTET STRING (SIZE (32)) }` → 64 B payload + 1 B choice tag for rSig in OER | Ieee1609Dot2BaseTypes.asn (same repo) |
| ECDSA brainpoolP256r1 | same as P-256: pk 33/65, sig 64 (256-bit prime field curve, 128-bit security) | RFC 5639 §3.4 (brainpoolP256r1) and §A (security level table: brainpoolP256r1 → 128), https://www.rfc-editor.org/rfc/rfc5639.txt |
| ECDSA P-384 / brainpoolP384r1 | pk 49 (compressed) / 97 (uncompressed); sig 96 (`EcdsaP384Signature ::= SEQUENCE { rSig EccP384CurvePoint, sSig OCTET STRING (SIZE (48)) }`; `EccP384CurvePoint` x-only/compressed = 48 B) | Ieee1609Dot2BaseTypes.asn; SEC 1 §2.3.3 rule with 384-bit q (derived) |
| Ed25519 public key / signature | 32 / 64 | RFC 8032 §7: "Ed25519, Ed25519ctx, and Ed25519ph private and public keys are 32 octets; signatures are 64 octets." https://www.rfc-editor.org/rfc/rfc8032.html |
| ECQV implicit-certificate public-key reconstruction value (P-256) | 33 bytes (an EC point, encoded with SEC 1 point-to-octet-string, compressed) | SEC 4 v1.0 (Certicom, January 24, 2013) §3.4/3.5: "Convert PU to the octet string PU using the Elliptic-Curve-Point-to-Octet-String conversion ... [SEC 1, §2.3]", https://www.secg.org/sec4-1.0.pdf ; in 1609.2 the reconstruction value is carried as `EccP256CurvePoint` (33 B) |
| 1609.2 SPDU size, implicit vs explicit cert | "one SPDU is at most 226 bytes using implicit vs. 330 bytes using explicit certificates" | Bindel, McCarthy, Twardokus, Rahbari, NDSS 2024, §III, https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf |
| 1609.2 certificate + signature size (ETSI/IEEE) | "between 100 bytes and 150 bytes"; digest reduces to 8 B | COSIC (KU Leuven), "On Handling of Certificate Digest in V2X", §II, https://cosicdatabase.esat.kuleuven.be/backend/publications/files/conferencepaper/3530 |
| Signed BSM SPDU with full cert (ECDSA P-256) | 301 bytes total (Table 2); with Falcon-512: 1,735 bytes | arXiv 2608.05087 (PQ signatures in C-V2X), Tables 1–2, https://arxiv.org/html/2608.05087 |
| SHA-256 output | 256 bits = 32 bytes | FIPS 180-4, Figure 1 "Secure Hash Algorithm Properties": SHA-256 message digest size 256 bits, https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.180-4.pdf |
| HashedId8 | `HashedId8 ::= OCTET STRING (SIZE(8))` = low-order 8 bytes of the SHA-256 hash; example in the ASN.1 comment: SHA-256("") → HashedId8 = a495991b7852b855 | Ieee1609Dot2BaseTypes.asn (ETSI forge mirror of IEEE 1609.2) |
| HashedId10 (used in hash-based CRL entries) | 10 bytes | Ieee1609Dot2CrlBaseTypes.asn `HashBasedRevocationInfo ::= SEQUENCE { id HashedId10, expiry Time32 }` |
| LinkageValue / GroupLinkageValue | `LinkageValue ::= OCTET STRING (SIZE(9))`; `GroupLinkageValue ::= SEQUENCE { jValue OCTET STRING (SIZE(4)), value OCTET STRING (SIZE(9)) }` | Ieee1609Dot2BaseTypes.asn |
| LinkageSeed / LaId / IValue | `LinkageSeed ::= OCTET STRING (SIZE(16))`; `LaId ::= OCTET STRING (SIZE(2))`; `IValue ::= Uint16` | Ieee1609Dot2BaseTypes.asn |

### A.2 ML-DSA (FIPS 204, published August 13, 2024)

| Parameter set | Private key | Public key | Signature | Source |
|---|---|---|---|---|
| ML-DSA-44 | 2560 | 1312 | 2420 | FIPS 204 Table 2 "Sizes (in bytes) of keys and signatures of ML-DSA", https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.204.pdf |
| ML-DSA-65 | 4032 | 1952 | 3309 | FIPS 204 Table 2 |
| ML-DSA-87 | 4896 | 2592 | 4627 | FIPS 204 Table 2 |
| Private-key seed option | "one can store only the 32-byte seed ξ, which is sufficient to generate the other parts of the private key" | FIPS 204 p.16 (text preceding Table 2) |
| Cross-check | PQClean `api.h`: ML-DSA-44 pk 1312 / sk 2560 / sig 2420; -65: 1952/4032/3309; -87: 2592/4896/4627 | https://raw.githubusercontent.com/PQClean/PQClean/master/crypto_sign/ml-dsa-{44,65,87}/clean/api.h (fetched 2026-09-17) |

### A.3 SLH-DSA (FIPS 205, published August 13, 2024) — all 12 parameter sets

| Parameter set (SHA2 and SHAKE variants share sizes) | n | h | d | h' | a | k | lg_w | m | Sec. cat. | pk bytes | sig bytes |
|---|---|---|---|---|---|---|---|---|---|---|---|
| SLH-DSA-SHA2-128s / SLH-DSA-SHAKE-128s | 16 | 63 | 7 | 9 | 12 | 14 | 4 | 30 | 1 | 32 | 7 856 |
| SLH-DSA-SHA2-128f / SLH-DSA-SHAKE-128f | 16 | 66 | 22 | 3 | 6 | 33 | 4 | 34 | 1 | 32 | 17 088 |
| SLH-DSA-SHA2-192s / SLH-DSA-SHAKE-192s | 24 | 63 | 7 | 9 | 14 | 17 | 4 | 39 | 3 | 48 | 16 224 |
| SLH-DSA-SHA2-192f / SLH-DSA-SHAKE-192f | 24 | 66 | 22 | 3 | 8 | 33 | 4 | 42 | 3 | 48 | 35 664 |
| SLH-DSA-SHA2-256s / SLH-DSA-SHAKE-256s | 32 | 64 | 8 | 8 | 14 | 22 | 4 | 47 | 5 | 64 | 29 792 |
| SLH-DSA-SHA2-256f / SLH-DSA-SHAKE-256f | 32 | 68 | 17 | 4 | 9 | 35 | 4 | 49 | 5 | 64 | 49 856 |

Source: FIPS 205 Table 2 "SLH-DSA parameter sets", https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.205.pdf. Private key = 4n bytes (64 / 96 / 128), public key = 2n bytes: FIPS 205 §3.1 key-length checks ("the public key is 2n bytes in length ... the private key is 4n bytes"). Cross-check: PQClean sphincs-sha2-128s-simple sk 64 / pk 32 / sig 7856; 128f-simple sig 17088.

### A.4 Falcon / FN-DSA (FIPS 206)

| Item | Value | Source |
|---|---|---|
| Falcon-512 pk / sig (fixed "padded" size) / sk | 897 / 666 / 1281 | PQClean `crypto_sign/falcon-padded-512/clean/api.h`: PUBLICKEYBYTES 897, BYTES 666, SECRETKEYBYTES 1281; falcon-sign.info table: pk 897, sig 666 |
| Falcon-1024 pk / sig (padded) / sk | 1793 / 1280 / 2305 | PQClean `falcon-padded-1024/clean/api.h`; falcon-sign.info: pk 1793, sig 1280 |
| Falcon variable-length (compressed) signature max buffer | Falcon-512: 752; Falcon-1024: 1462 | PQClean `falcon-512/clean/api.h` CRYPTO_BYTES 752; `falcon-1024/clean/api.h` CRYPTO_BYTES 1462 (the non-padded encoding is variable length; 666/1280 are the fixed padded lengths "used in signature verification" per the same headers) |
| Falcon private key size | "about three times that of a signature" (spec text); PQClean concrete: 1281 / 2305 | https://falcon-sign.info/ ; PQClean api.h |
| FIPS 206 status (as of 2026-09-17) | NIST PQC project page: "FALCON was also selected and will be published in FIPS 206 (in development)"; Perlner slides (Sept 25, 2025, 6th PQC Standardization Conf.): "We expect to release an Initial Public Draft soon"; "made pseudorandom seeds uniformly 40 bytes (except keygen uses 32 bytes)"; DigiCert blog (2025-09-05): "On August 28, 2025, NIST submitted the draft standard for FN-DSA (FIPS 206) for approval." The CSRC URL csrc.nist.gov/pubs/fips/206/ipd returned 404 on 2026-09-17, so **whether the IPD has been published is UNVERIFIED** | https://csrc.nist.gov/projects/post-quantum-cryptography/post-quantum-cryptography-standardization ; https://csrc.nist.gov/csrc/media/presentations/2025/fips-206-fn-dsa-(falcon)/images-media/fips_206-perlner_2.1.pdf ; https://www.digicert.com/blog/quantum-ready-fndsa-nears-draft-approval-from-nist |
| FN-DSA sizes quoted in pqc-forum status thread | Falcon-512 pk 897 / sig 666 (total 1563); Falcon-1024 pk 1793 / sig 1280 (total 3073); ML-DSA-44 1312/2420; ML-DSA-65 1952/3309 | pqc-forum "FIPS 206 Status Update" (J. Mattsson summary of Perlner talk, 2025-10-13), https://groups.google.com/a/list.nist.gov/g/pqc-forum/c/1HXzjlMUU6Y |

### A.5 ML-KEM (FIPS 203, published August 13, 2024) — backend encryption

| Parameter set | Encapsulation key | Decapsulation key | Ciphertext | Shared secret | Source |
|---|---|---|---|---|---|
| ML-KEM-512 | 800 | 1632 | 768 | 32 | FIPS 203 Table 3 "Sizes (in bytes) of keys and ciphertexts of ML-KEM", https://nvlpubs.nist.gov/nistpubs/FIPS/NIST.FIPS.203.pdf |
| ML-KEM-768 | 1184 | 2400 | 1088 | 32 | FIPS 203 Table 3 |
| ML-KEM-1024 | 1568 | 3168 | 1568 | 32 | FIPS 203 Table 3 |

### A.6 Hybrid / composite sizes

| Combination | Public key | Private key | Signature | Source |
|---|---|---|---|---|
| id-MLDSA44-ECDSA-P256-SHA256 | 1377 | 83 | 2492 (max*) | draft-ietf-lamps-pq-composite-sigs-19 (April 21, 2026), Appendix A size table; ECDSA keys "uncompressed X9.62 including the leading byte 0x04", DER-encoded; * = maximum (ECDSA DER signature is variable length). https://www.ietf.org/archive/id/draft-ietf-lamps-pq-composite-sigs-19.txt |
| id-MLDSA44-Ed25519-SHA512 | 1344 | 64 | 2484 | same |
| id-MLDSA65-ECDSA-P256-SHA512 | 2017 | 83 | 3381* | same |
| id-MLDSA65-ECDSA-brainpoolP256r1-SHA512 | 2017 | 84 | 3381* | same |
| id-MLDSA65-ECDSA-P384-SHA512 | 2049 | 96 | 3413* | same |
| id-MLDSA65-Ed25519-SHA512 | 1984 | 64 | 3373 | same |
| id-MLDSA87-ECDSA-P384-SHA512 | 2689 | 96 | 4731* | same |
| id-MLDSA87-ECDSA-brainpoolP384r1-SHA512 | 2689 | 100 | 4731* | same |
| Raw concatenation for a 1609.2-style hybrid (no DER): ML-DSA-44 + ECDSA P-256 | pk 1312 + 33 = 1345; sig 2420 + 64 = 2484 | Derived from FIPS 204 Table 2 and A.1 |
| Raw concatenation: Falcon-512 (padded) + ECDSA P-256 | pk 897 + 33 = 930; sig 666 + 64 = 730 | Derived |

---

## B. Verification and signing costs

### B.1 x86 (primary specs / reference implementations)

| Scheme | Platform | KeyGen | Sign | Verify | Source |
|---|---|---|---|---|---|
| Dilithium2 (≈ML-DSA-44) ref C | Skylake, unoptimized reference | 300,751 cyc (median) | 1,081,174 cyc median / 1,355,434 avg | 327,362 cyc median | Dilithium round-3 spec (2021-02-08) Table 1, https://pq-crystals.org/dilithium/data/dilithium-specification-round3-20210208.pdf |
| Dilithium2 AVX2 | Skylake | 124,031 | 259,172 median / 333,013 avg | 118,412 | same |
| Dilithium3 (≈ML-DSA-65) ref / AVX2 | Skylake | 544,232 / 256,403 | 1,713,783 / 428,587 (median) | 522,267 / 179,424 | same |
| Dilithium5 (≈ML-DSA-87) ref / AVX2 | Skylake | 819,475 / 298,050 | 2,383,399 / 538,986 (median) | 871,609 / 279,936 | same. Note: round-3 Dilithium sig sizes (2420/3293/4595) differ slightly from FIPS 204 ML-DSA (2420/3309/4627); cycle counts are indicative only |
| Falcon-512 (avx2) | Intel i7-6567U Skylake, 3.6 GHz | 26,604,000 cyc (7,390 µs) | sign(dynamic) 948,132 cyc (263.37 µs); sign(tree) 467,964 cyc | 81,036 cyc (22.51 µs) | Pornin, "New Efficient, Constant-Time Implementations of Falcon", ePrint 2019/893 §5.2 tables, https://eprint.iacr.org/2019/893.pdf |
| Falcon-512 (fpemu, no FPU) | same | 60,066,000 | 18,694,872 (dynamic) | 97,416 | same |
| Falcon-1024 (avx2) | same | 79,164,000 | 1,926,252 (dynamic); 942,768 (tree) | 160,596 cyc (44.61 µs) | same |
| Falcon-512 | Intel Core i5-8259U @ 2.3 GHz, TurboBoost off | 8.64 ms | 5,948.1 sig/s | 27,933.0 verify/s | https://falcon-sign.info/ (fetched 2026-09-17) |
| Falcon-1024 | same | 27.45 ms | 2,913.0 sig/s | 13,650.0 verify/s | same |
| SPHINCS+-SHA2-128s-simple ref C | Xeon E3-1220 (Haswell) 3.1 GHz | 358,061,994 | 2,721,595,944 | 2,712,044 | SPHINCS+ r3.1 spec Table 4, https://sphincs.org/data/sphincs+-r3.1-specification.pdf |
| SPHINCS+-SHA2-128f-simple ref C | same | 5,590,602 | 138,610,500 | 7,757,942 | same |
| SPHINCS+-SHAKE-128s-simple / 128f-simple ref C | same | 616,484,336 / 9,649,130 | 4,682,570,992 / 239,793,806 | 4,764,084 / 12,909,924 | same |
| SPHINCS+-SHA2-128s-simple / 128f-simple AVX2 | same | 84,964,790 / 1,334,220 | 644,740,090 / 33,651,546 | 861,478 / 2,150,290 | SPHINCS+ r3.1 spec Table 6 |
| liboqs reference-code ops/s ("Reference code type (unoptimised), 2024-04-02, OQS"): Falcon-512 sign 176.55/s, verify 17,246/s; Falcon-1024 80.56 / 8,341; Dilithium2 sign 2,099.33/s, verify 8,752.33/s; Dilithium3 1,320 / 5,519; SPHINCS+-SHA2-128f-s sign 23.86/s, verify 404.73/s | **host CPU not stated in the table — UNVERIFIED platform** | | | | Berger, Lemoudden, Buchanan, "Post Quantum Migration of Tor", arXiv 2503.10238 Table 6, https://arxiv.org/pdf/2503.10238 |
| OQS continuous benchmarking | The OQS `profiling` repo (speed_sig / speed_kem) is **archived and deprecated** ("Use of this code is not recommended"); the visualization page openquantumsafe.org/benchmarking/visualization/speed_sig.html returned 404 on 2026-09-17 | | | | https://github.com/open-quantum-safe/profiling ; https://openquantumsafe.org/benchmarking/ |

### B.2 ARM Cortex-A (Raspberry Pi 4 / Cortex-A72, Pi 5 / Cortex-A76, V2X ARMv8)

| Scheme | Platform | KeyGen | Sign | Verify | Source |
|---|---|---|---|---|---|
| Falcon-512 (liboqs) | Raspberry Pi 4B (BCM2711, Cortex-A72), Ubuntu Server 24.04 | 23.6 /s | 615.3 /s | 7,866.3 /s | arXiv 2503.10238 Table 8 ("Code retrieved from liboqs"), §4.1 hardware |
| ML-DSA-44 (liboqs) | Raspberry Pi 4B | 2,642.7 /s | 286.1 /s | 3,213.9 /s | same |
| Falcon-512 (liboqs) | Raspberry Pi 5 (Cortex-A76) | 95.4 /s | 3,360.6 /s | 19,831.3 /s | same |
| ML-DSA-44 (liboqs) | Raspberry Pi 5 | 8,986.3 /s | 1,885.0 /s | 8,139.0 /s | same |
| ML-DSA-65/87, Falcon-1024, SLH-DSA on Pi 4 | **not found in fetched sources — UNVERIFIED** | | | | — |
| Cortex-A53 liboqs numbers | **not found — UNVERIFIED** (the NIST-2022 paper below used a Raspberry Pi 3 / Cortex-A53 only for constant-time checks, no throughput table) | | | | NIST 4th PQC conf. paper, https://csrc.nist.gov/csrc/media/Events/2022/fourth-pqc-standardization-conference/documents/papers/benchmarking-and-analysiing-nist-pqc-lattice-based-pqc2022.pdf |
| ECDSA (Botan), Falcon, Dilithium, SPHINCS+, XMSS on commercial V2V device | Cohda MK6, "Qualcomm ARMv8 V2V chipsets", 1000 executions, no ARMv8 optimizations. **Sign (ms)**: ECDSA 7.820; Falcon 2.152; Sphincs+ 5.485; Dilithium 2.634; XMSS 1405.408. **Verify (ms)**: ECDSA 0.001; Falcon 0.446 (2,243 verify/s); Sphincs+ 5.436; Dilithium 0.189 (5,299/s); XMSS 2.780 | | | | NDSS 2024 Table V, https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf. Caveat (paper Appendix): "our results are not expected to be reproducible" without the devices; the 0.001 ms ECDSA verify implies a hardware/accelerated path the paper does not explain — treat with care |
| Ed25519 (OpenSSL) | Raspberry Pi 4B | — | 2,939.1 /s | 1,327.0 /s | arXiv 2503.10238 Table 7 |

### B.3 ARM Cortex-M4 (pqm4) and Cortex-M7

pqm4 numbers: `benchmarks.csv` at master (fetched 2026-09-17), board NUCLEO-L4R5ZI (Cortex-M4), "All cycle counts were obtained at 24MHz", `arm-none-eabi-gcc 11.3.1`. Mean cycles; sign is the average over 1000 executions (rejection sampling makes sign highly variable — min/max shown).

| Scheme / impl | KeyGen | Sign (mean; min–max) | Verify | Source |
|---|---|---|---|---|
| ml-dsa-44 `m4f` | 1,426,025 | 3,943,121 (1,812,557–17,009,165) | 1,421,623 | https://raw.githubusercontent.com/mupq/pqm4/master/benchmarks.csv |
| ml-dsa-44 `clean` | 1,874,405 | 7,925,955 | 2,063,096 | same |
| ml-dsa-65 `m4f` | 2,516,006 | 6,193,171 (2,918,295–26,008,621) | 2,415,944 | same |
| ml-dsa-87 `m4f` | 4,275,859 | 7,947,380 (4,880,711–29,357,607) | 4,193,104 | same |
| sphincs-sha2-128s-simple `clean` (10 exec.) | 1,007,731,522 | 7,657,558,168 | 7,471,794 | same |
| sphincs-sha2-128f-simple `clean` | 15,742,990 | 368,575,228 | 21,923,628 | same |
| sphincs-shake-128s-simple `clean` | 3,231,401,965 | 24,553,696,412 | 24,366,771 | same |
| sphincs-shake-128f-simple `clean` | 50,505,025 | 1,182,422,563 | 70,501,834 | same |
| Falcon in pqm4 | **not present in current master benchmarks.csv** (Falcon rows absent) | | | same |
| Falcon-512 (Pornin impl., STM32F407 "discovery", Cortex-M4 @168 MHz, GCC 6.3.1 -O2, integer FP emulation w/ inline asm) | 171,294,112 cyc (1,019.61 ms) | sign(dynamic) 43,301,915 (257.75 ms); sign(tree) 21,155,551 (125.93 ms); expand-sk 16,191,379 | **504,051 cyc (3.00 ms)** | ePrint 2019/893 §5.3 table |
| Falcon-1024 (same) | 513,950,073 (3,059.23 ms) | sign(dynamic) 93,985,426 (559.43 ms); sign(tree) 44,985,653 (267.77 ms) | 1,032,261 cyc (6.14 ms) | same |
| Dilithium-2 / -3 / -5 (Cortex-M7, STM32F767ZI NUCLEO-144) | 1,437 / 2,566 / 4,368 kcyc | 3,658 / 6,009 / 8,157 kcyc avg | 1,429 (6.6 ms) / 2,453 (11.4 ms) / 4,287 (19.8 ms) kcyc | NIST 4th PQC conf. paper Table I (URL above) |
| Falcon-512-FPU / -EMU (Cortex-M7) | 77,475 / 128,960 kcyc | sign dyn 4,778 (22.1 ms) / 29,447 (136.3 ms) kcyc | 559 (2.6 ms) / 565 kcyc | same, Table II |
| Falcon-1024-FPU / -EMU (Cortex-M7) | 193,707 / 342,533 kcyc | sign dyn 10,243 (47.4 ms) / 64,681 (299.5 ms) | 1,136 (5.3 ms) / 1,130 kcyc | same, Table II |

### B.4 PQClean

| Item | Value | Source |
|---|---|---|
| Role | Clean, portable C reference implementations; sizes in `api.h` (see A.2–A.4); not a benchmark publisher | https://github.com/PQClean/PQClean README |
| License | Per-implementation `LICENSE` files; `common/` public domain or MIT; other repo code CC0 | README "License" section |

### B.5 ECDSA P-256 on Cortex-A / Cortex-M and HSM throughput

| Item | Value | Source |
|---|---|---|
| P-256 ECDSA on Cortex-M4 (nRF52840 @ 64 MHz, GCC -O2) | KeyGen 327k cyc (5.1 ms); Sign 375k cyc (5.9 ms); **Verify 976k cyc (15.3 ms)**; ECDH 906k cyc | Emill/P256-Cortex-M4 README (MIT), https://github.com/Emill/P256-Cortex-M4 |
| wolfSSL benchmark page, Cortex-M rows | NUCLEO-F446ZE (Cortex-M4, 168 MHz, "Single Precision ASM Cortex-M3+ Math", wolfSSL 5.6.3): ECDSA sign "avg 277.750 ms, 3.600 ops/sec", verify "avg 549.500 ms, 1.820 ops/sec" — **UNVERIFIED**: the summarizer returned identical rows for STM32F777NI and Pico-W and these M4 numbers are far slower than Emill/wolfSSL's own claims; re-check on the page before use | https://www.wolfssl.com/docs/benchmarks/ |
| wolfSSL vs mbedTLS (June 2026; wolfSSL 5.9.1, mbedTLS 3.6.6) ECDSA P-256 **verify** ops/s | Intel i9-11950H: 61,357 vs 1,244; Raspberry Pi 5 (Cortex-A76 @ 2.4 GHz): 14,933 vs 592; STM32H563 (Cortex-M33 @ 250 MHz): 167 vs 12.1; PolarFire U54 (RISC-V @ 600 MHz): 256 vs 24. Sign (i9): 64,194 vs 4,227 | https://www.wolfssl.com/wolfssl-vs-mbedtls-an-apples-to-apples-benchmark-across-intel-arm-cortex-a-and-cortex-m-and-risc-v-targets/ |
| OpenSSL 1.1.1d `openssl speed` on Raspberry Pi 4 (Cortex-A72) | "256 bits ecdsa (nistp256) 0.0002s 0.0006s 4097.4 1550.7" (sign/s, verify/s); nistp384 123.4 / 174.8; brainpoolP384r1 123.5 / 162.8 | Gist "Raspberry Pi 4 OpenSSL speed", https://gist.github.com/HimaJyun/f05d3017dfb05a4ccb0def010bb2c91a (OS/bitness not stated) |
| CAMP planning figure for software ECDSA | "ECDSA-256 ... can generate about 1500 signatures per second on a 2 GHz processor and can verify only about 300 signatures per second." | CAMP, SCMS PoC EE Requirements and Specifications Supporting SCMS Software Release 1.1 (May 04, 2016), Crypto Basics section (p. 75), mirror https://ccv.eng.wayne.edu/reference/SCMS_POC_EE_Requirements20160111_1655.pdf (fetched via pronto-core-cdn mirror) |
| Network HSM throughput (ECC P-256 sign) | Thales Luna Network HSM 7: A700 "ECC P256: 2,000 tps"; A750 "10,000 tps"; A790 "22,000 tps" (RSA-2048: 1,000 / 5,000 / 10,000 tps); headline "20,000 ECC and 10,000 RSA operations per second" | Thales Luna Network HSM 7 product brief (Dec 2019), https://cpl.thalesgroup.com/sites/default/files/content/product_briefs/field_document/2020-04/thales-luna-network-7-hsm-pb-a.pdf |
| AWS CloudHSM / nShield ECDSA rates | **UNVERIFIED** (no numeric spec located) | — |

### B.6 PQ-on-V2X-hardware papers (Bindel / McCarthy / Twardokus / Rahbari)

| Item | Value | Source |
|---|---|---|
| Paper | "When Cryptography Needs a Hand: Practical Post-Quantum Authentication for V2V Communications", NDSS 2024 (ePrint 2022/483) | https://www.ndss-symposium.org/wp-content/uploads/2024-267-paper.pdf ; https://eprint.iacr.org/2022/483 |
| Testbed | PQ-V2Verifier: USRP SDRs + laptops; sign/verify timings measured on Cohda MK6 ("Qualcomm ARMv8 V2V chipsets"); libraries: Botan (ECDSA, Dilithium, XMSS), liboqs (Falcon, SPHINCS+) | NDSS 2024 §VII; GitHub https://github.com/twardokus/pq-v2verifier |
| Measured verify latencies (ms, mean of 1000) | ECDSA 0.001; Falcon 0.446; Dilithium 0.189; XMSS 2.780; Sphincs+ 5.436 → Falcon "2,243 BSM signatures per second", v_max = 224 vehicles | NDSS 2024 Table V |
| Measured sign latencies (ms) | ECDSA 7.820; Falcon 2.152; Dilithium 2.634; Sphincs+ 5.485; XMSS 1405.408 | NDSS 2024 Table V |
| Verification-rate requirement used | "a viable PQ algorithm must be capable of verifying at least 100 signatures per 100 ms interval (i.e., a rate of ≥ 1 kHz)" (100-vehicle assumption) | NDSS 2024 §VII-B |
| Frame limits | DSRC/802.11 MPDU payload "capped at 2,304 bytes regardless of data rate"; C-V2X "in a standard 10 MHz channel is a mere 437 bytes [44, Table A.8.3-1]" | NDSS 2024 §III |
| End-to-end per-BSM medians (µs), SDR testbed | ECDSA: verify 272.0, end-to-end 902.4; Partially-Hybrid Falcon: verify 555.0, end-to-end 1568.4 (Δ 666.0); PH+AI 1307.7 (Δ 250.2) at 60 veh/km | NDSS 2024 Table VI |
| Certificate-bearing SPDU sizes | 330 B (explicit cert SPDU); P2PCD learning response with one cert 172 B | NDSS 2024 §VI |
| Demo paper | "An Open-Source Hardware-in-the-Loop Testbed for Post-Quantum V2V Security Research", VehicleSec 2024 — SDRs + Cohda MK6; no numeric results in the demo abstract | https://www.ndss-symposium.org/wp-content/uploads/vehiclesec2024-7-demo.pdf |
| C-V2X transport-block ceiling | 2,481 bytes at MCS 11 with 10 subchannels (SAE J3161) | arXiv 2608.05087 (simulation study; no OBU measurements) |

---

## C. Automotive HSM / OBU crypto throughput and verification-rate requirements

| Device | Published figure | Source |
|---|---|---|
| Autotalks CRATON (gen-1) in Unex OBU-201U | "Hardware verification engine in Craton supports over 2,000 ECDSA 256-bit verifications per second."; "HSM supports less than 50ms latency on ECDSA 256-bit signing." (Infineon SLE97 HSM) | Unex OBU-201U Specification Ver. 2.03 (2017-03-06), FCC filing https://apps.fcc.gov/els/GetAtt.html?id=201591&x=. |
| Autotalks CRATON2 / SECTON | Product pages: "Line-rate ECDSA and V2X-embedded HSM", "On-chip accelerator for ECDSA verification" — **no numeric rate on the pages**. The ">2000 NIST / >1500 Brainpool verifications/s" figures surfaced only in a search snippet of the Autotalks "V2X Security Portfolio" white paper v1.3 (mirror returned 403) → **UNVERIFIED** | https://auto-talks.com/products/craton2/ ; https://auto-talks.com/products/secton/ |
| Autotalks CRATON2/SECTON eHSM certification | FIPS 140-2 Overall Security Level 3; HW P/N ATK66610, version 2.1.2 | CMVP Security Policy #3556, https://csrc.nist.gov/CSRC/media/projects/cryptographic-module-validation-program/documents/security-policies/140sp3556.pdf |
| NXP SAF5400 (RoadLINK 802.11p modem) | "ECDSA verification: 2000 messages/sec (Brainpool/NIST curves 256 bits)" | NXP fact sheet SAF5400V2XFS REV 2 (©2018, 2020), https://www.nxp.com/docs/en/fact-sheet/SAF5400V2XFSA4.pdf |
| NXP SXF1800 (V2X secure element, signing side) | Arm SC300 + public-key coprocessor; "ECDSA signature generation, ECIES"; "Signature generation performance exceeding single and dual channel requirements" (no number); CC EAL5+ platform, CC EAL4+ vs C2C HSM PP 1.4.0, FIPS 140-2 Level 3 | https://www.nxp.com/products/SXF1800 |
| Cohda MK5 OBU | CPU NXP i.MX6 DL (4000 DMIPS); DSRC NXP RoadLink SAF5100; Security SXF1700; MK5 module datasheet: "The MAC runs on the ARM processor of the SAF5100." **Verify rate not published → UNVERIFIED** (a search snippet cited "35 verifications per second in the case of the software module" from a research paper, not confirmed) | Cohda MK5 OBU product brief (2024), https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK5-OBU.pdf ; MK5 module datasheet v1.2.0 (May 2015), https://fccid.io/2AEGPMK5RSU/Users-Manual/User-Manual-2618067.pdf |
| Cohda MK6C EVK | "Based on the MDM 9150 chipset developed by Qualcomm"; C-V2X PC5 (Qualcomm 9150); application processor i.MX 8QXP; Security "SXF1800 FIPS 140-2 level 3 compliant". Verify rate not published | Cohda MK6C EVK product brief, https://cohdawireless.com/wp-content/uploads/2024/06/CW_DL-Product-Brief-sheet-MK6C-EVK.pdf |
| Commsignia ITS-RS4 (RSU; same platform family as OB4) | "Marvell / Qualcomm 9150 V2X chipset"; "Hardware Security Module (HSM) SLI97 — ECDSA verification (> 2000 verifications), encryption (< 50 usec signing delay) — NIST and Brainpool verification, encryption" | ITS-RS4 Product Brief v0.9.3 (2020-04-22), https://omniair.org/wp-content/uploads/2025/10/Commsignia_ITS_RS4_ProductBrief_v.09.3_22042020.pdf |
| Commsignia ITS-OB4 | "built-in Tamper-proof Hardware Security Module"; no rate published → **UNVERIFIED** | https://commsignia.com/products/obu (via search snippet) |
| Qualcomm 9150 C-V2X | Product page returned no body text; HSM/verify-rate claims **UNVERIFIED** | https://www.qualcomm.com/products/automotive/qualcomm-c-v2x-9150 |
| Infineon SLI 97 (V2X HSM) | Datasheet not retrievable (404); only vendor-integrator claim above (Commsignia) → **UNVERIFIED** | — |
| Infineon AURIX TC3xx HSM, OPTIGA TPM | PKC "up to 256 bit" curves (training doc via search snippet); no ECDSA verify rate found → **UNVERIFIED** | https://www.infineon.com/dgdl/Infineon-AURIX_TC3xx_Hardware_Security_Module_Quick-Training-v01_00-EN.pdf (not fetched) |
| Kapsch OBU, Escrypt CycurHSM, Danlaw AutoLink, Savari MobiWAVE | No published verify rates located (search budget exhausted) → **UNVERIFIED / not found** | — |
| Software ECDSA on ARMv8 V2X device | ECDSA sign 7.820 ms / verify 0.001 ms (Cohda MK6, Botan, no NEON opts) — see B.6 caveat | NDSS 2024 Table V |

### C.2 Verification-throughput requirements / verify-on-demand

| Item | Value | Source |
|---|---|---|
| BSM rate | 10 Hz ("The expected frequency in which messages are sent by each vehicle is around 10 Hz ... [10]") | Simplicio et al., "Faster verification of V2X BSM messages via Message Chaining", ePrint 2022/133 §I, https://eprint.iacr.org/2022/133.pdf |
| Receive load | "In a busy road, this may translate to more than 1,000 messages received per second by each vehicle [7], [12]." | ePrint 2022/133 §I |
| Verify-on-Demand (VoD) | "vehicles may be required to prioritize the verifications of messages considered more critical, using a Verify-on-Demand (VoD) approach [7]" ([7] = Krishnan & Weimerskirch, "Verify-on-Demand", SAE 2011); J2945/1 [37] referenced as the security profile | ePrint 2022/133 §I–II |
| Certificate attachment policy | "a vehicle typically includes its full pseudonym certificate only in every fifth SPDU and transmits a hash of that certificate in the other 80% of messages" ([39] = SAE J2945/1) | NDSS 2024 §III |
| NDSS design requirement | ≥ 100 verifications per 100 ms (≥ 1 kHz) for 100 neighbours | NDSS 2024 §VII-B |
| CAMP software baseline | ~1500 sign/s, ~300 verify/s ECDSA-256 on a 2 GHz processor | CAMP EE Req. (2016) p. 75 |
| "Verify 100 signed SPDUs per second" requirement; "14.089 ms / 70.977 verifications/s on Android" | Appeared only in a search snippet (AVoD paper, Hindawi SCN 2021, article 2890132; fetch returned 403) → **UNVERIFIED** | https://www.hindawi.com/journals/scn/2021/2890132/ |

---

## D. Real-crypto libraries usable under Apache-2.0-compatible terms

| Library | What it gives the simulator | License (as fetched 2026-09-17) | Source |
|---|---|---|---|
| liboqs (Open Quantum Safe) | ML-DSA, Falcon (incl. padded), SLH-DSA, ML-KEM, `speed_sig` tool | MIT ("The MIT license ... applies to liboqs in general"; some third-party subfolders differ) | https://raw.githubusercontent.com/open-quantum-safe/liboqs/main/LICENSE.txt |
| oqs-provider (OpenSSL 3 provider) | PQ/hybrid algorithms in OpenSSL 3 | MIT | https://raw.githubusercontent.com/open-quantum-safe/oqs-provider/main/LICENSE.txt |
| PQClean | Reference C code, sizes | Per-implementation LICENSE; common/ public-domain or MIT; rest CC0 | PQClean README §License |
| pqm4 | Cortex-M4 implementations + benchmarks | Per-implementation; other code Apache-2.0 / CC0 dual | pqm4 README §License |
| pqcrypto (Rust, rustpq) 0.18.1 | Rust bindings to PQClean | "MIT OR Apache-2.0" | https://raw.githubusercontent.com/rustpq/pqcrypto/main/pqcrypto/Cargo.toml |
| RustCrypto `p256` 0.14.0 | ECDSA P-256, ECDH, point compression | "Apache-2.0 OR MIT" | https://raw.githubusercontent.com/RustCrypto/elliptic-curves/master/p256/Cargo.toml |
| pyca/cryptography | ECDSA P-256/P-384/brainpool (via OpenSSL), SHA-256, ECIES building blocks | Dual "Apache-2.0 OR BSD-3-Clause" ("either of the licenses found in LICENSE.APACHE or LICENSE.BSD") | https://raw.githubusercontent.com/pyca/cryptography/main/LICENSE |
| Bouncy Castle (bc-java) | Java ECC/ECDSA/PQC. **ECQV implicit-certificate support: UNVERIFIED** (could not confirm a SEC 4 ECQV API in bc-java; GitHub code search rate-limited) | MIT ("Copyright (c) 2000-2026 The Legion of the Bouncy Castle Inc.") | https://raw.githubusercontent.com/bcgit/bc-java/main/LICENSE.html |
| `ecqv` (Rust crate 0.1.0, 2026-08-08) | "SEC 4 ECQV implicit certificates on P-256: reconstruct a public key from a certificate and the issuer's key" | "MIT OR Apache-2.0" | crates.io API; repo https://github.com/Abdk4Moura/ecqv |
| V2Verifier | Open-source IEEE 1609.2 signing/verification over SDR ("first open-source implementation of the IEEE 1609.2 standard for V2V security" — project's own claim) | MIT (© 2020-2023 Geoff Twardokus et al.) | https://raw.githubusercontent.com/twardokus/v2verifier/master/LICENSE |
| PQ-V2Verifier | PQ extension (Falcon/Dilithium/SPHINCS+/XMSS in 1609.2 SPDUs) | MIT (© 2023 Twardokus, Bindel, Rahbari, McCarthy) | https://raw.githubusercontent.com/twardokus/pq-v2verifier/main/LICENSE |
| IEEE 1609.2 / 1609.2.1 ASN.1 modules (ETSI forge mirror) | Authoritative type definitions (HashedId8, LinkageValue, CRL types, 1609.2.1 ACPC/RA interfaces) | Repo metadata shows no license field; README: "published as a part of delivery ETSI TS 103 097 v1.4.1" → **license UNVERIFIED** (IEEE copyright applies to the modules) | https://forge.etsi.org/rep/ITS/asn1/ieee1609.2 (branch `ieee`), https://forge.etsi.org/rep/ITS/asn1/ieee1609.2.1 |
| Emill/P256-Cortex-M4 | Embedded P-256 reference numbers | MIT | https://github.com/Emill/P256-Cortex-M4 |
| Falcon reference implementation (Pornin) | Constant-time Falcon incl. M4 asm | MIT ("The new implementation is open source (MIT license)") | ePrint 2019/893 §1 |
| CAMP SCMS PoC source (EE/RA reference code), "scms-pki"-style repos | **Not located** (search budget exhausted) → UNVERIFIED | — |

---

## E. SCMS pseudonym-certificate batch and CRL modelling facts

### E.1 Batch parameters

| Item | Value | Source |
|---|---|---|
| Certificates per week (minimum) and initial batch | "An initial request is for 3,000 (3,120 to be exact) certificates and is assumed to be the default for a batch request. (20 pseudonym certificates per week x 52 weeks per year x 3 years). Note: 20 pseudonym certificates is minimum number of certificates per week. Each OEM can decide to have more certificates per week." | CAMP, SCMS PoC EE Requirements and Specifications Supporting SCMS Software Release 1.1 (May 04, 2016), §2.2.7.6.1 Use Case "RA – Request Pseudonym Certificate Batch Provisioning"; mirror https://pronto-core-cdn.prontomarketing.com/2/wp-content/uploads/sites/2896/2019/04/SCMS_POC_EE_Requirements.pdf |
| i-period length | "The length of the i-period should be the number of minutes in a week, 10080." | CAMP EE Req. §2.1.5.3.2 "Pseudonym Certificate Validity" |
| Validity overlap / certificate lifetime | "The lifetime of the certificate is the i-period plus an overlap period. In the old design, the overlap period is 1 minute ... we are extending the overlap period to 1 hour ... the lifetime of a pseudonym certificate is 10140 minutes." Requirement SCMS-1416 applies t_overlap = one hour to OBE identification certs "in line with pseudonym certificates" | CAMP EE Req. §2.1.5.3.2; SCMS-1416 |
| Start-validity epoch | seconds since 1609.2 epoch 00:00:00 UTC, January 1, 2004 | CAMP EE Req. §2.1.5.3.2 |
| Design rationale (SCMS paper) | "Certificate validity time period: 1 week; Number of certificates valid simultaneously (batch size): minimum 20; Overall covered time-span: 1–3 years"; RA "will provide the certificates in batches worth one week (e.g., as a zip file)"; automatic top-off "throughout the life-time of the device, until the device stops picking-up certificates" | Brecht et al., "A Security Credential Management System for V2X Communications", arXiv 1802.05323 §IV/V, https://arxiv.org/pdf/1802.05323 |
| USDOT design/cost study | "The batch size of 3,000 certificates is based on a set of approximately 20 certificates being used per week, which equates to three years' worth of weeks." | Booz Allen Hamilton for USDOT ITS-JPO, "SCMS Design and Analysis for the Connected Vehicle System: Draft" (Dec 27, 2013), https://rosap.ntl.bts.gov/view/dot/32051/dot_32051_DS1.pdf |
| Batch download size | No primary figure found. **Derived / UNVERIFIED**: 3,120 certs × ~100–150 B (cert+sig, COSIC) ≈ 312–468 KB of certificate bytes, excluding the per-certificate encrypted response wrapper and butterfly-key material |  |
| Linkage value length | "The linkage values and pre-linkage values are chosen to be 9 bytes in length." (72-bit; collision analysis for 2.5×10^8 cars × 40 certs/week) | arXiv 1802.05323 §V-B.3; 1609.2 `LinkageValue ::= OCTET STRING (SIZE(9))` |
| Linkage seed length | 1609.2 `LinkageSeed ::= OCTET STRING (SIZE(16))`; CAMP: "To reduce the size of certificate revocation list (CRL), which contains the LSs of the revoked vehicles, the LSs are truncated to 16 bytes."; ACPC: "LA_i picks a random, 128-bit linkage seed ls_i(0)" | Ieee1609Dot2BaseTypes.asn; CAMP EE Req. §2.1.5.3.5; Simplicio et al. ACPC, ePrint 2018/324 §2.3, https://eprint.iacr.org/2018/324.pdf |
| Linkage-seed chain | ls_i(t) = Hash(la_id_i ‖ ls_i(t−1)); pre-linkage value plv_i(t,c) = Enc(ls_i(t), la_id_i ‖ c) truncated; lv = plv_1 ⊕ plv_2 | ePrint 2018/324 §2.3; arXiv 1802.05323 §V-B |

### E.2 CRL structure (IEEE 1609.2 ASN.1, module `Ieee1609Dot2CrlBaseTypes` major-version-3 minor-version-3)

| Item | Value | Source |
|---|---|---|
| CRL header | `CrlContents ::= SEQUENCE { version Uint8 (1), crlSeries CrlSeries, crlCraca HashedId8, issueDate Time32, nextCrl Time32, priorityInfo CrlPriorityInfo, typeSpecific TypeSpecificCrlContents }` | https://forge.etsi.org/rep/api/v4/projects/ITS%2Fasn1%2Fieee1609.2/repository/files/Ieee1609Dot2CrlBaseTypes.asn/raw?ref=ieee |
| CRL types | `TypeSpecificCrlContents ::= CHOICE { fullHashCrl, deltaHashCrl (ToBeSignedHashIdCrl), fullLinkedCrl, deltaLinkedCrl (ToBeSignedLinkageValueCrl), ..., fullLinkedCrlWithAlg, deltaLinkedCrlWithAlg }` | same |
| Hash-based entry | `HashBasedRevocationInfo ::= SEQUENCE { id HashedId10, expiry Time32 }` → 14 B payload/entry | same |
| Linkage-based CRL body | `ToBeSignedLinkageValueCrl ::= SEQUENCE { iRev IValue, indexWithinI Uint8, individual SequenceOfJMaxGroup OPTIONAL, groups SequenceOfGroupCrlEntry OPTIONAL, ..., groupsSingleSeed ..., iPeriodInfo ... }` | same |
| Individual entry nesting | `JMaxGroup { jmax Uint8, contents SequenceOfLAGroup }` → `LAGroup { la1Id LaId, la2Id LaId, contents SequenceOfIMaxGroup }` → `IMaxGroup { iMax Uint16, contents SequenceOfIndividualRevocation, ..., singleSeed SequenceOfLinkageSeed OPTIONAL }` → `IndividualRevocation { linkageSeed1 LinkageSeed, linkageSeed2 LinkageSeed }` | same |
| Group entry | `GroupCrlEntry ::= SEQUENCE { iMax Uint16, la1Id LaId, linkageSeed1 LinkageSeed, la2Id LaId, linkageSeed2 LinkageSeed }` | same |
| Per-revoked-vehicle payload | 2 × 16 B linkage seeds = 32 B per `IndividualRevocation` (+ shared la1Id/la2Id 2+2 B, iMax 2 B, jmax 1 B per group, OER length prefixes) — **derived** | derived from the ASN.1 above |
| CRL growth rule | "each revoked device contributes with 2 pre-linkage values to the CRL. Hence, the CRL grows linearly with the number of revoked vehicles, not with the number of revoked certificates." (entries must persist until the batch expires — up to the 3-year batch horizon) | ePrint 2018/324 §2.3 |

### E.3 CRL size and distribution cadence

| Item | Value | Source |
|---|---|---|
| Storage assumption / size | "It is assumed that all OEMs will provide at least enough storage for 10,000 entries, which translates to a file size of approximately 400 KB." (≈ 40 B/entry) | arXiv 1802.05323 §VI-F "CRL Size and CRL Distribution" |
| Minimum k entries | "CRL entries will contain some indicator of priority ... minimum value of k system-wide set to 10,000" | arXiv 1802.05323 §III (revocation) |
| CAMP 2013 cap | "As of December 2013, CAMP was considering limiting the size of the CRL to 10,000 entries." | USDOT/Booz Allen 2013 design report, CRL section |
| Distribution cadence (2013 working assumption) | "The current working assumption is that a full CRL will be distributed to all OBE on a daily basis." (with delta-CRL option discussed) | USDOT/Booz Allen 2013 |
| Distribution cadence (SCMS paper) | "It may be appropriate to update the end-entity CRL daily, but the SCMS component CRL once a month. Alternatively, it might be desirable to publish CRLs continually as new revocation information becomes available" | arXiv 1802.05323 §VI-G |
| CAMP PoC CRL download mechanism | "The EE is able to download the CRL by issuing a CRL HTTP get request to the CRL Store"; CRL Store does not authenticate the EE | CAMP EE Req. §2.2.9 Use Case 6: CRL Download |
| ACPC effect on CRL lifetime | With activation codes "the CRL needs to cover only the current activation period (at most 3 weeks); otherwise, the CRL needs to cover the next activation period (3 weeks) and the remainder of the current one (at most 1 week)" | ePrint 2018/324 §4 |
| ACPC "16-byte activation code vs 117-byte pseudonym certificate" | Appeared in a search snippet only; not present in the ePrint text extracted → **UNVERIFIED** | — |
| Kumar, Petit, Whyte, "Binary hash tree based certificate access management for connected vehicles" (WiSec 2017) | Exists (BCAM); CRL-size figures not retrieved → **UNVERIFIED** | https://dl.acm.org/doi/10.1145/3098243.3098257 ; https://wisecdata.ccs.neu.edu/papers/2017/1-kumar.pdf |
| "On the design of a CRL for the SCMS" | Not located → **UNVERIFIED** | — |

---

## Quick modelling defaults (all traceable to the rows above)

- Message-level: ECDSA-P256 sig 64 B; implicit cert (ECQV) reconstruction value 33 B; SPDU ≤ 226 B implicit / 330 B explicit; digest 8 B (HashedId8); full cert every 5th BSM at 10 Hz.
- PQ alternatives: ML-DSA-44 sig 2420 / pk 1312; Falcon-512 sig 666 / pk 897; SLH-DSA-128s sig 7856 / pk 32; DSRC MPDU cap 2,304 B; C-V2X TB ceiling 437 B (10 MHz, NDSS) or 2,481 B (MCS 11, 10 subchannels, J3161).
- Verify cost anchors: HW OBU ECDSA ≈ 2,000 verifies/s (NXP SAF5400 fact sheet; Autotalks CRATON via Unex spec; Commsignia/SLI97); software ECDSA on Cortex-A72 ≈ 1,550/s (OpenSSL 1.1.1d); Cortex-M4 P-256 verify ≈ 976k cycles; Falcon-512 verify ≈ 504k cycles (M4) / 81k cycles (Skylake AVX2) / 7,866 per s (Pi 4, liboqs); ML-DSA-44 verify ≈ 1.42M cycles (M4 m4f) / 118k cycles (Skylake AVX2) / 3,214 per s (Pi 4).
- Backend: 3,120 certs per 3-year batch (20/week); i-period 10,080 min; lifetime 10,140 min (1 h overlap); CRL ≈ 40 B per revoked vehicle (10,000 entries ≈ 400 KB), 32 B of which are the two 16-B linkage seeds; daily CRL cadence as the working assumption.
