# ADR 0001 — Base stack, SCMS model, and dataset approach

- **Status:** Accepted (2026-08-09)
- **Deciders:** Project owner (approved Option B and "start P0 + P1")
- **Design document:** <https://claude.ai/code/artifact/9a391e86-95f0-421a-aadd-f2459ddb93f3>

## Context

We are building an SCMS-aware V2X simulator that emits a global
Misbehavior-Authority-perspective dataset. Three foundations were compared:
(A) legacy F2MD/`veins-f2md`, (B) VeReMi NextGen on Eclipse MOSAIC, (C) a clean
build on modern Veins/Artery + Vanetza. The project repository is Apache-2.0.

## Decision

1. **Base stack = Option B: Eclipse MOSAIC + VeReMi NextGen** (+ a new SCMS
   back-end federate). Rationale, in priority order:
   - **Licensing.** NextGen is Apache-2.0 and MOSAIC/SUMO are EPL-2.0 →
     combinable while keeping our code Apache-2.0. Veins/Artery/F2MD are GPL-2.0
     (F2MD's repo has *no* license), which would foreclose Apache-2.0.
   - **Maintenance.** NextGen is actively developed (pushed 2026-08-09); F2MD's
     simulation code is frozen at 2022.
   - **Architecture fit.** MOSAIC's federation model is a native, isolation-
     preserving seam for the SCMS back-end.
   - Decision-matrix score: B 166 vs C 117 vs A 106 (see design doc §4).
2. **Primary SCMS model = US SCMS** (IEEE 1609.2 / 1609.2.1, butterfly keys,
   linkage-seed CRLs, LA1/LA2/MA/CRLG). ETSI TS 102 941 kept as a config variant.
3. **Licensing.** Code Apache-2.0; datasets released CC-BY-4.0.
4. **Crypto fidelity.** Abstract signing by default (deterministic Ed25519 in the
   reference; ECDSA/ECQV P-256 in the MOSAIC layer), with an optional real-crypto
   mode; **butterfly keys and linkage values implemented faithfully** (they are the
   scientific core). Reports shaped to ETSI TS 103 759; certs to IEEE 1609.2.
5. **F2MD is a reference to mine, not a base.** Its detectors/attacks/report
   structure are re-implemented (minus its `senderRealId`/`reportedRealId` leakage).

## Consequences

- Application and back-end logic is **Java** (MOSAIC), with **Python** for the
  dataset pipeline; the network is modeled behind a federate (SNS/Cell default,
  OMNeT++/INET for fidelity subsets). Per-packet C++/NED fidelity is traded away.
- Because no JDK/SUMO/MOSAIC toolchain is installed yet, P1's testable core (SCMS
  crypto, schemas, leakage linter, determinism, closed-loop reference pipeline) is
  implemented first in Python under `src/scms_sim_ref/`. The Java layer mirrors it.
- The Python `scms_core` is the **validated reference**; the Java `crypto/` must
  match its test vectors.

## Open items (defaults chosen; revisit if needed)

- Download and align to DARE's exact report schema before freezing (design doc §29 Q8).
- Group linkage values stubbed behind the optional IEEE 1609.2 field (Q7).
- Network federate: SNS/Cell default, OMNeT++ for validation subsets (Q4).
