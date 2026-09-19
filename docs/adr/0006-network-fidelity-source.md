# ADR 0006 — Network fidelity source: native PHY/MAC, external simulators for cross-validation only

- **Status:** Proposed (2026-09-18)
- **Related:** `04-models.md` §3–7, `02-architecture.md` §7.3. Evidence: R8 §A.1, R1, R2*, R4.

## Context

Options are native PHY/MAC implementations validated against literature, integration with ns-3 or OMNeT++/INET as separate processes, or both. Constraints: Apache-2.0 code, determinism, WASM build, mixed fidelity per region, and byte-accurate sizes flowing from our own message and security encoders.

## Decision

1. **Native implementations** in `v2xw-radio`/`v2xw-net` of: IEEE 802.11p OCB PHY (10 MHz OFDM, MCS table, sensitivity, NIST-style PER model) and EDCA MAC with CSMA/CA, hidden terminals and capture; LTE-V2X Mode 4 and NR-V2X Mode 2 sidelink (sub-channels, SCI, sensing-based SPS, HARQ, IBE, CBR/CR); ETSI DCC and J2945/1 congestion control; WSMP and GeoNetworking/BTP; application-layer fragmentation strategies. Parameters and validation targets come from the standards and papers catalogued in `04-models.md` (R1, R2b–R2e, R4).
2. **External simulators are cross-validation harnesses only**, run out of process on reference scenarios in CI (nightly), never runtime dependencies and never linked in-process: ns-3 (GPLv2-only [R8 §A.1]) with VaN3Twin (formerly ms-van3t, GPL-2.0 [R8 §A.1]) for 802.11p/ETSI stacks; WiLabV2Xsim (GPL-3.0, MATLAB/Octave [R8 §A.1]) results are used as published curves, not executed.
3. **OMNeT++-based stacks (Veins, Artery, Vanetza harness, F2MD, PLEXE, OpenCV2X) are read as references only**: OMNeT++ is under the Academic Public License (non-commercial without a paid licence) and the stacks are GPL-2.0; F2MD has no licence file at all [R8 §A.1]. Their models (Sommer obstacle shadowing, Veins PER handling, Artery's DCC and GN implementations) are re-implemented from the papers and standards with citations.

## Alternatives

| Option | Licence fit | Determinism/WASM | Mixed tiers per region | Byte accuracy from our encoders | Effort | Verdict |
|---|---|---|---|---|---|---|
| A. Native (chosen) | Apache-2.0 | yes | yes | yes | high | chosen |
| B. ns-3 in-process (Python bindings) | GPLv2 contaminates | no WASM; determinism ok | no | would need bridging | medium | rejected |
| C. ns-3/OMNeT++ out of process as the runtime PHY | isolation ok, but OMNeT++ licence for commercial use | socket per frame kills performance; no WASM | no | no | medium | rejected as runtime; kept as validation harness |
| D. Both (native default, ns-3 optional runtime) | ok if optional | partial | partial | partial | highest | rejected: two PHYs to keep validated; harness gives the same assurance |

## Consequences

- Validation is a first-class deliverable: PDR vs distance, CBR vs density, and C-V2X PRR curves from R1/R2d/R3 are encoded as tests with tolerances (04-models §13).
- The native MAC must be written against the standards' clause-level parameters (EDCA table per EN 302 663 / IEEE 802.11-2016 Table 9-138 [R1]; TS 38.214 §8.1.4 sensing procedure [R2b]) with model cards listing what each tier ignores.
- Licensing gate in the registry (ADR 0007) refuses in-process loading of any plug-in tagged GPL or Academic Public License.
