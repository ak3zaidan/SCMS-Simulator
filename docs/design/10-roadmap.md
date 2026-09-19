# 10 — Roadmap, acceptance criteria, risks, effort

Status: design draft for review (2026-09-18). Effort is given as person-months (PM) for one experienced engineer plus review, with a range; calendar time assumes one to two people. Estimates are judgement, not measurement, and are revisited at each phase gate.

## Phase 0 — Foundations (2–3 weeks, 0.5–0.75 PM)

Scope: monorepo layout (ADR 0010), `mise` + `just` one-command setup on macOS/Linux/Windows, CI matrix, workspace crates with empty interfaces and model-card schema, the hand-written binary layouts (the `world-1` world container of 04-models §1.1, the event channel records, and the VWP frame header and `Hello` of `docs/protocol/vwp-v1.md`, with that specification's hex vectors as their golden tests), MCAP writer/reader smoke test, Python package skeleton with `maturin`, UI skeleton with Vite, `legacy/scms_sim_ref` moved and its conformance tests running in CI, PowerShell/MOSAIC tooling removed, VeReMi submodule removed.

Acceptance: `just setup && just test` green on all three OSes from a fresh clone; `legacy` tests pass; ADR set accepted or amended.

## Phase 1 — Vertical slice (6–8 weeks, 2–3 PM)

Scope (from the brief §18.10): one vehicle on a small real-city map with buildings (OSM import with lanes inferred, building footprints and heights or default heights, no terrain yet), native mobility (`medium` tier: IDM on lanes, signals static), one radio tier (`medium`: log-distance + shadowing + building obstacle shadowing, SINR with no interferers, PER curve), real IEEE 1609.2 signed BSMs at 10 Hz (COER envelope from the ETSI-forge 1609.2 module, real ECDSA-P256 and an implicit certificate; BSM payload through the validated size model unless the user supplies the SAE J2735 ASN.1 module, see 11-open-questions), one hardware profile (`cohda-mk5`) with the OBU runtime (queues, CPU and HSM servers, stores) and the live HUD in 2D and 3D (Studio with map view, fly-down, chase camera, HUD, inspector "why" tab), MCAP recording and replay through VWP, and one metric plotted (`bytes_air` or `verify_rate`).

Acceptance (all automated except the last):
1. Golden determinism: two runs on each OS produce identical MCAP records and digests.
2. Envelope size: the real COER encoder's SPDU size equals the size-model prediction for digest and full-certificate signers (tolerance 0 bytes).
3. Crypto equivalence: `modeled` and `real` modes produce identical event logs except the manifest field.
4. Replay: seek to any time in a 10-minute recording ≤ 100 ms (p95) on the reference laptop.
5. UI: 60 fps in map and chase views with the single vehicle; the HUD shows every field of `NodeTelemetry`; every HUD value resolves to a model card through the "why" tab.
6. Provenance: the manifest lists engine hash, plug-in hashes, world hash, and model card versions; the generated docs site builds from the registry.
7. Manual: a reviewer follows the vehicle from map to chase view without a visible scene change.

## Phase 2 — Two vehicles, an RSU, the SCMS path, a revocation (8–10 weeks, 3–4 PM)

Scope: second vehicle (reception, neighbor table, verification policy `verify-all`, P2PCD inline), an RSU with backhaul, the SCMS backend as nodes on a backend network (RA, PCA, LA1, LA2, MA, CRLG, LOP, CRL Store) with the ported `scms_core`, provisioning and top-up flows, a misbehavior report from the second vehicle, the `legacy-window` MA pipeline, a revocation with CRL issuance, distribution through the RSU and cellular (`abstract` Uu tier), OBU CRL download and expansion cost, enforcement; the `ma-dataset` exporter in the v1 profile; the legacy 12-detector suite and the ConstPos/Sybil attackers ported; the experiment runner for seeds.

Acceptance:
1. All engine-independent legacy conformance tests pass on the v1-profile export (`verify_data`, leakage linter, ML contract, end-to-end semantics).
2. Revocation stages: every stage timestamp in 05-protocols §8 is emitted and the latency decomposition renders in the Studio.
3. Standards conformance checklist for RQ6 ("one, two, three vehicles"): BSM cadence, certificate attachment interval, envelope fields, CRL entry format and expansion, verified by assertions on the event log.
4. Equivalence: the same scenario with `Python` and `Rust` MA pipeline implementations yields identical decisions.

## Phase 3 — Fidelity and scale (10–12 weeks, 4–5 PM)

Scope: `high` radio tier for 802.11p (EDCA, CSMA/CA, hidden terminals, capture, per-MCS PER, CBR measurement), ETSI DCC and J2945/1 congestion control, WSMP and GeoNetworking/BTP headers, application-layer fragmentation strategies including the Partially-Hybrid PQ scheme, the `abstract` radio tier calibrated against `high`, mixed tiers with the focus region, hardware profiles for the initial set (DSRC, C-V2X, SoC without HSM, PQ-capable), verification policies (on-demand, prioritized), node lifecycle, weather effects, GNSS error model, SUMO co-simulation tier, procedural worlds and the world editor, terrain (DEM) with LOS, validation suite against literature curves (04-models §13), performance benchmarks with regression tracking, 1,000 vehicles at `high` ≥ 0.1× real time and 10,000 at `abstract` ≥ 1× real time.

Acceptance: validation tests within the stated tolerances for PDR vs distance, CBR vs density, flow–density; performance targets met on the reference laptop in CI (nightly); focus-region boundary bias ≤ 5 percentage points PDR at the calibration density.

## Phase 4 — Protocols, threats, extensibility (10–12 weeks, 4–5 PM)

Scope: ETSI ITS PKI plug-in (enrolment, authorization standard and butterfly, ECTL/CTL/CRL, TS 103 759 reports, passive revocation), threshold/umbrella/PQ plug-in skeleton over liboqs with the interactive-flow machinery (DKG, t-of-n rounds, refresh) parameterised by the user's specification, PQ primitives and hybrid modes, the full attack catalog (ported 28 + the new families), jamming, compromised RSU, report poisoning, privacy observer and metrics, TS 103 759 detector classes and perception cross-check, the foundry port, the example plug-in per interface with the tutorial, the copilot on the JSON-RPC registry, the `ma-dataset` v2 profile and the other exporters.

Acceptance: RQ1, RQ3, RQ4 run end to end from scenario file to figure with the presets in 08-measurement §7; a researcher outside the team adds a detector plug-in from the tutorial in under a day (usability test); conformance kit passes for every shipped plug-in.

## Phase 5 — C-V2X, cellular, VRUs, breadth (10–12 weeks, 4–5 PM)

Scope: LTE-V2X Mode 4 and NR-V2X Mode 2 PHY/MAC (`medium` and `high`), hybrid RAT, cellular Uu with measured latency distributions, handover and outages, store-and-forward, backhaul models, VRUs with PSM/VAM, CPM, SPaT/MAP/SRM/SSM/WSA, RSU roles, safety applications with surrogate safety measures, WASM build of the engine and replay reader, comparison views, figure presets for RQ2 and RQ5, Tauri packaging (optional).

Acceptance: RQ5 (DSRC vs LTE-V2X vs NR-V2X) reproduces the qualitative ordering of published comparisons at the cited densities; C-V2X PDR vs distance validated against the WiLabV2Xsim/Todisco curves within tolerance.

## Phase 6 — Hardening and release (6–8 weeks, 2–3 PM)

Scope: documentation site (architecture, methodology, extension tutorial, glossary), model-card completeness gate (no `todo-calibrate` on a `high`-tier default without a calibration issue), validation campaign report, dataset release with datasheets, 1.0 tag.

Total: 20–26 PM over roughly 12–15 months with one to two engineers; Phases 3–5 can overlap with two people.

## Risk register

| # | Risk | Likelihood | Impact | Mitigation | Owner phase |
|---|---|---|---|---|---|
| R1 | SAE J2735 ASN.1 cannot be redistributed; real UPER for BSM/SPaT/MAP requires the user to supply the module | high (confirmed, R4 §G) | medium | validated size model as the shipped tier; build-time import of a user-supplied module; ETSI messages fully real (BSD-3 modules) | 1 |
| R2 | Cross-platform float determinism breaks through a dependency (platform libm, SIMD auto-vectorisation) | medium | high | `libm` crate, CI golden tests on three OSes and two CPU architectures, conformance tracer forbidding std transcendental calls | 0–1 |
| R3 | High-tier 802.11p model diverges from published curves | medium | high | validation suite from day one of Phase 3; ns-3 out-of-process cross-check harness for the reference scenarios | 3 |
| R4 | Performance targets missed at `high` tier | medium | medium | analytical budget (02-architecture §11.2), profiling gates, phase-parallel design, tier fallback per region | 3 |
| R5 | Hardware profile numbers are thin (vendors publish few verify rates) | high (R5/R7) | medium | profiles carry `NOT PUBLISHED` fields with `TODO: calibrate` plans; benchmarks on Raspberry Pi class hardware as proxies; sensitivity analysis in experiments | 3 |
| R6 | User's threshold/umbrella specification arrives late | medium | medium | interface designed against cited building blocks; skeleton plug-in parameterised by rounds/sizes; no engine change expected | 4 |
| R7 | OSM ODbL obligations complicate dataset publication | medium | medium | world bundle separated from simulation data with its own licence; legal question tracked (11-open-questions) | 1 |
| R8 | Python plug-in overhead makes large abstract runs slow | low | medium | batched SDK, `python-hot-path` flag, Rust ports of hot detectors | 2–4 |
| R9 | UI scope creep | high | medium | Studio panels tied to JSON-RPC methods; anything without a method is out of scope | 1–5 |
| R10 | SUMO version drift breaks determinism of the `high` mobility tier | medium | low | version pinned in the manifest; SUMO optional; golden tests per SUMO version | 3 |
| R11 | Two-language core (Rust + Python) raises the contribution barrier | medium | medium | tutorial plug-ins, scaffolds, conformance kit, Python-first SDK | 4 |
| R12 | Rate-limited or unavailable primary standards (IEEE 1609.x, SAE) leave defaults secondary-sourced | high | low–medium | model cards mark `secondary` sources; open-questions list the clauses to verify with purchased copies | all |

## Phase gates

Each phase ends with: the acceptance list green in CI, the model-card completeness report, an updated 11-open-questions with resolved items, and a short decision log. A phase does not start until the previous gate is signed off by the project owner.
