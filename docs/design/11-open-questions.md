# 11 — Open questions for the project owner

Status: design draft for review (2026-09-18). None of these blocked the design; each states the assumption the documents proceed under. Grouped by how much the answer changes the design.

## A. Answers that change the architecture or the plan

| # | Question | Assumption used | What changes if the answer differs |
|---|---|---|---|
| A1 | **Threshold / umbrella-threshold specifications** (brief §8.3, still owed): entity roles, key generation, signing rounds, credential formats, revocation mechanism, expected sizes. | The protocol interface supports distributed roles, multi-round flows over the backend network, variable-size credentials, multiple verifications per message, umbrella credentials covering many pseudonyms, and PQ primitives; a placeholder plug-in sketch with cited building blocks is in 05-protocols §5. | If the scheme needs verification semantics the `VerifyPlan` cannot express (e.g., aggregate verification across messages, or receiver-side interactive checks), the `SecurityEnvelope`/`VerifyPlan` interface gains an aggregation step; if revocation is neither a list nor starvation (e.g., epoch-based key rotation), `RevocationMechanism` gets a third variant. Neither changes the engine core. |
| A2 | **SAE J2735 ASN.1 licensing.** SAE does not permit redistribution of the J2735 ASN.1 modules (R4 §G), so the engine cannot ship real UPER encoders for BSM/SPaT/MAP/PSM/SRM/SSM. | Ship the ETSI (BSD-3) and IEEE 1609.2 (ETSI-forge mirror) modules with real encoders; ship a **validated size model** for J2735 messages; provide a build-time importer for a user-supplied J2735 module that turns the size model into real UPER. Phase 1's "real signed BSM" uses a real 1609.2 COER envelope over a size-modeled BSM unless the module is supplied. | If you hold a J2735 licence that allows a private build, Phase 1 uses real UPER from day one; if the size model's tolerance is unacceptable for a paper, the fallback is to use CAM (fully real) as the Phase 1 message. |
| A3 | **Reference OBU profile** (brief §20). | The hardware sheet found the Unex OBU-301/CRATON2 profile to be the most completely documented public OBU (dual Cortex-A7 600 MHz, eHSM > 110 sign/s < 9 ms, hardware verify engine > 2,500 ECDSA/s); it is proposed as the reference profile, with Cohda MK5 (DSRC) and MK6C (C-V2X) as the commercial-unit profiles whose verify rates are NOT PUBLISHED and proxied by NXP SAF5400's 2,000/s (06-node-models §7). | A different reference unit only changes a data file, unless it has no published numbers at all, in which case the `TODO: calibrate` plan (bench the device) is the path. |
| A4 | **Priority among the canonical research questions** (brief §20). | RQ1, RQ3, RQ4 first, as the brief suggests; the roadmap's Phase 3–4 order follows them (fragmentation and node compute before C-V2X). | If RQ5 (DSRC vs LTE-V2X vs NR-V2X) comes first, Phase 5's C-V2X work moves ahead of Phase 4's protocol breadth. |
| A5 | **Dataset licensing with OSM-derived worlds.** The OSM Foundation guideline leaves "lane-geometry export = produced work or derivative database" unresolved; a database preserving OSM geometry is very likely ODbL (R10 §A1). | World bundles carry their own licence (ODbL for OSM/Overture-derived); simulation data stays CC-BY-4.0 and references the world by hash (08-measurement §9). | If legal review says coordinate-only simulation data on an OSM network is itself derivative, published datasets become ODbL or must use procedural/licensed worlds. |
| A6 | **Working name and package name** (`v2xw` used as a placeholder). | `v2xw` for crates, Python package and CLI; repository name unchanged. | Rename is mechanical before Phase 0 ends. |
| A7 | **Two-language core acceptable?** (Rust engine + Python plug-ins, ADR 0003/0007.) | Yes; researchers write Python against the batched SDK and never need Rust unless they write a hot-path model. | If Rust is unacceptable for the team, the alternative with the next-best matrix score is C++ + pybind11 + Emscripten (ADR 0003), with the same architecture. |

## B. Answers that change defaults or a model, not the architecture

| # | Question | Assumption used | Effect |
|---|---|---|---|
| B1 | Certificate attachment default for SCMS: 450 ms (`CertAttachInt`, Rostami et al.) or "every fifth SPDU" (NDSS 2024's reading)? The J2945/1 clause is paywalled (R4 §A.1). | 450 ms, cited to Rostami 2018 Table 1; flagged secondary. | Changes `full_cert_share` and air bytes by a few percent; a purchased J2945/1 copy settles it. |
| B2 | CRL publication cadence for the SCMS plug-in: daily (USDOT 2013 working assumption), continuous, or weekly? The PoC value is UNVERIFIED (R6 §A.10). | Daily default; scenario-selectable. | Revocation-latency figures in RQ3/RQ4 shift by up to a day. |
| B3 | Top-up download horizon (weeks of certificates a vehicle keeps): OEM-configurable per the primer; no number published. | 4 weeks, `TODO: calibrate`. | Passive-revocation lag and top-up traffic. |
| B4 | Mobility step: 100 ms default, or 50 ms to match the legacy Java layer's 50 ms floor? | 100 ms (TR 36.885 location-update interval); 10–100 ms allowed. | Compute cost roughly linear in 1/Δt. |
| B5 | Weather effects on radio: the ITU-R numbers show rain/fog attenuation is negligible over 300 m at 5.9 GHz (R3), while the legacy engine applied 2–6 % packet-loss penalties. | Keep the physical model (near-zero attenuation) and offer the legacy penalties only as a labelled `code (legacy)` abstract-tier option. | Papers comparing with the legacy datasets should state which option was used. |
| B6 | Pseudonym-change default: SCMS 5 min / 2 km, C2C-CC BSP, or the C2C-CC segment strategy? | Protocol-specific defaults (05-protocols §2.4); strategies are plug-ins. | Privacy metrics only. |
| B7 | ETSI AT pool default: 100 (EU CP maximum) or 20 (C2C-CC profile)? | 20 (C2C-CC profile) as the realistic default; 100 as an option. | Sybil surface and passive-revocation lag. |
| B8 | Verification policy default on OBUs: verify-all or on-demand? | verify-all in Phase 1–2 (measures the raw load); on-demand and prioritized available. | HSM utilisation and `unverified_ratio` baselines. |
| B9 | Keyframe cadence for recordings: 1 s (default) or 0.5 s for very dense scenes? | 1 s. | File size vs seek latency. |
| B10 | Focus-region tolerance (PDR discontinuity at the boundary ≤ 5 percentage points). | 5 points. | Whether mixed tiers are allowed for a given experiment. |

## C. Answers that only affect documentation or tooling

| # | Question | Assumption used |
|---|---|---|
| C1 | Docs site generator: mkdocs (assumed) vs another. | mkdocs with the registry-generated model pages. |
| C2 | Post-run figure library: Plotly.js (assumed) vs ECharts. | Plotly.js for parity with Python notebooks (ADR 0009). |
| C3 | Desktop packaging: Tauri 2 (assumed, optional) vs Electron. | Tauri later; Electron fallback if webview parity bites. |
| C4 | Whether to keep `LinkageEngine.java` as an archived reference port. | Kept under `legacy/java-reference/` read-only. |
| C5 | Whether to publish the throwaway spike code. | Not published; the appendix records commands and results. |

## D. Facts the research could not verify (to settle with purchased standards or measurements)

| Item | Status | How to settle |
|---|---|---|
| J2945/1 parameter names `vDensityWeightFactor`, `vRescheduleThreshold`, the "attach certificate on new neighbour" rule, the 1.5 m position-accuracy clause | UNVERIFIED (R1, R4) | purchase SAE J2945/1_202004 |
| IEEE 1609.4 channel switching timing (50/50 ms, 4 ms guard) | secondary sources only (R1) | purchase IEEE 1609.4 |
| IEEE 1609.2.1 CRL ASN.1 names for linkage entries, 1609.3 fragmentation and length statements | UNVERIFIED (R4, R6) | IEEE 1609.2.1/1609.3 texts |
| TS 36.101 in-band emission table values; TS 36.331 CBR-PSSCH-TxConfigList numeric values; NR-V2X CR-limit table (not standardised) | UNVERIFIED (R2, R2e) | 3GPP spec zips; RAN1 contributions |
| Cheng 2007 dual-slope parameters and Nakagami-m values; Taliwal/Torrent-Moreno m thresholds; Rician K for V2V; Karedal 2011 Table I | UNVERIFIED (R3) | obtain the papers; or calibrate against the measured Abbas 2015 set already verified |
| Empirical CBR-vs-density field curve | not found (R3) | use the DCC evaluation reports or run the high-tier model as the reference |
| Cohda MK5/MK6C verification throughput; Infineon SLI 97 datasheet; Microchip ATECC608 timing | NOT PUBLISHED (R5, R7) | bench on device; vendor NDA |
| SUMO per-step overhead; Rust→WASM slowdown; PyO3 per-call microseconds | unpublished (R8) | measure in Phase 3/5/2 |
| CAMP PoC RA processing SLA; end-to-end SCMS/ETSI revocation-latency measurements | none published (R6) | the simulator will be the first to produce them; report as model outputs, not calibrated facts |
| Overture building heights attribute coverage; OSM height-tag coverage; level-to-metre default | UNVERIFIED (R10) | inspect the Overture schema; measure on the Phase 1 city |
