# ADR 0002 — Supersede ADR 0001's base-stack decision: new Rust core, legacy Python as validated reference

- **Status:** Proposed (2026-09-18), supersedes ADR 0001 decision 1 (base stack); confirms ADR 0001 decisions 2 (primary SCMS model), 3 (licensing) and 5 (F2MD as reference only); refines decision 4 (crypto fidelity).
- **Deciders:** project owner (pending review of `docs/design/`).
- **Related:** `docs/design/01-inventory.md`, `02-architecture.md`, ADR 0003–0011.

## Context

ADR 0001 (2026-08-09) chose Eclipse MOSAIC + VeReMi NextGen as the base stack for an SCMS-aware **dataset generator**, with a Python reference core to be mirrored in Java. Eight weeks later the facts are:

1. All scientific work landed in the Python reference (`src/scms_sim_ref/`, ~10k lines); the MOSAIC layer is a thin, diverging Java duplicate that runs only from a Windows toolchain and was never used to produce results (01-inventory §3.7). The VeReMi NextGen submodule was never checked out.
2. The goal has changed from "dataset generator" to a 2D/3D world simulator with frame-level radio, node hardware models, pluggable credential-management protocols, and a live UI (`00-design-brief.md`). MOSAIC's federation model (SUMO, SNS/Cell, OMNeT++/ns-3 federates) does not provide a custom PHY/MAC that we control, a browser build, or a single world model shared by propagation and rendering, and its EPL-2.0 modules would have to stay out of process anyway.
3. The Python engine cannot reach the performance target: 10,000 vehicles run at ≈ 0.045× real time in pure Python and 0.22× with NumPy, versus 1.7× single-threaded and 4.5× on 8 cores in Rust for the same abstract loop (Appendix A).
4. The valuable parts of the legacy code are separable: `scms_core` (butterfly, linkage, HashedId8), the record schemas and leakage rules, the detector formulas, the attack renderings, the MA operating point, `datagen` (featurize, benchmark, validate, calibration, datasheet, massive, foundry), and `verify_data` (01-inventory §3).

## Decision

1. **Base stack = a new Rust core** (`crates/v2xw-*`, ADR 0003) with Python bindings and a WebAssembly build. Eclipse MOSAIC is no longer the base. SUMO remains an optional, out-of-process high-fidelity mobility tier (ADR 0005). ns-3/OMNeT++ are not runtime dependencies (ADR 0006).
2. **The legacy Python package is frozen under `legacy/scms_sim_ref/` as the validated reference.** Its engine-independent tests (butterfly and linkage vectors, leakage rules, ML contract, dataset integrity, end-to-end semantics) become the conformance suite the new engine must pass; its fixed golden digests are retired.
3. **Science is ported, not rewritten:** every formula and constant in 01-inventory §3.3 is carried over behind the new interfaces with a model card that cites the legacy code as its source (`kind: code`) plus `TODO: calibrate` where no external source exists.
4. **Confirmed from ADR 0001:** US SCMS remains the primary protocol (now one plug-in among several, `05-protocols.md`); code Apache-2.0 and datasets CC-BY-4.0; F2MD is a reference only; VeReMi compatibility is an exporter format, not a base.
5. **Refined from ADR 0001 (crypto):** the "abstract signing with identical outcomes" idea becomes the formal `Modeled | Real` crypto mode pair with the equivalence guarantee I-S1; Ed25519 as a stand-in is replaced by ECDSA-P256 and ECQV in real mode.
6. **Backward compatibility:** the MA dataset family is preserved as one exporter with a v1 profile (08-measurement §6).

## Alternatives considered (decision matrix)

Criteria and weights: determinism across platforms (5), performance at 10k nodes (5), plug-in ergonomics for Python-writing researchers (4), browser/UI story (4), custom PHY/MAC control (4), licensing (5), effort to Phase 2 (3). Scores 1–5.

| Option | Det. | Perf. | Ergon. | Browser | PHY ctrl | Licence | Effort | Weighted |
|---|---|---|---|---|---|---|---|---|
| A. Evolve the Python engine (refactor `run.py`, add Numba) | 3 | 1 | 5 | 1 | 3 | 5 | 4 | 89 |
| B. Keep MOSAIC as base, add federates (OMNeT++/ns-3 for PHY) | 3 | 3 | 2 | 1 | 2 | 3 | 2 | 72 |
| C. New Rust core + Python bindings + WASM; SUMO optional (chosen) | 5 | 5 | 4 | 5 | 5 | 5 | 2 | 137 |
| D. New C++ core + pybind11 + Emscripten | 4 | 5 | 3 | 4 | 5 | 5 | 2 | 124 |
| E. TypeScript core shared with the UI | 3 | 3 | 2 | 5 | 5 | 5 | 3 | 108 |

Language-level details of C vs D vs E are in ADR 0003.

## Consequences

- Two languages in the core path (Rust for the engine, Python for researcher plug-ins) plus TypeScript for the UI; ADR 0010 makes this a one-command setup.
- The MOSAIC layer and PowerShell tooling are deleted after Phase 0 (kept in git history); `LinkageEngine.java` may be archived as a reference port.
- The `.gitmodules` entry for VeReMi NextGen is removed.
- Phase 1 must demonstrate the port of `scms_core` with passing test vectors before any other science moves (10-roadmap).
