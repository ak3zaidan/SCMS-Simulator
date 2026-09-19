# ADR 0007 — Plug-in mechanism, model cards, version pinning

- **Status:** Proposed (2026-09-18)
- **Related:** `03-interfaces.md`, `02-architecture.md` §8.

## Context

Researchers who mostly write Python must be able to add protocols, attacks, detectors, metrics and exporters without touching engine code, while hot paths (propagation, PHY/MAC, car-following) must run at engine speed. Every model must be explainable (model card) and every run must pin what it ran.

## Decision

1. **Three mechanisms, one registry.** (a) In-process Rust trait objects for hot paths. (b) In-process Python through PyO3 with **batched** calls (per node-step or per flow message, Arrow record batches), allowed for the control-plane families listed in 03-interfaces §15 and for hot families only at the `abstract` tier (flagged `python-hot-path` in the manifest). (c) Out-of-process gRPC (protobuf) for external tools and other languages, with the engine hashing replies into the run digest.
2. **Model cards are mandatory** (schema in 03-interfaces §12): id, family, version, API version, tiers, purpose, equations, parameters with unit/default/source, assumptions, limitations, what the tier ignores, sources, validation status, determinism declaration. Registration fails without one; parameters read at runtime but not declared fail the conformance tracer.
3. **Docs are generated from cards** (mkdocs), never hand-written for models; a `todo-calibrate` page lists every uncited default.
4. **Version pinning:** scenario references `id@^semver`; the manifest freezes `id@version+content-hash`; replays refuse a different hash unless `--allow-plugin-drift` is set and recorded.
5. **Licensing gate:** the registry stores each plug-in's licence; GPL-licensed code can only be loaded out of process.

## Alternatives

| Option | Ergonomics (Python) | Speed | Determinism | Isolation | Verdict |
|---|---|---|---|---|---|
| A. Python-only plug-ins, per-event callbacks | best | worst (µs per call × millions of events) | ok | none | rejected |
| B. Rust-only plug-ins | worst for researchers | best | best | none | rejected |
| C. Rust hot paths + batched Python + gRPC (chosen) | good | good | good (hashing remote replies) | good | chosen |
| D. WASM component model for all plug-ins | good in principle, immature toolchains for Python | good | good | best | deferred; revisit when Python→WASM component tooling is stable |
| E. Out-of-process only (every plug-in a service) | ok | poor (serialisation per batch) | ok | best | rejected for in-loop models |

Evidence: PyO3 call overhead is on the order of a microsecond per call, so per-event Python at 10⁶–10⁷ events per simulated second is infeasible while per-node-step batches (10⁵ per simulated second at 10k nodes) are affordable (02-architecture §8); a per-family conformance kit makes plug-in quality checkable in CI.

## Consequences

- The SDK ships `v2xw plugin new <family>` scaffolds with a passing conformance test and an example plug-in per interface (tutorial, 10-roadmap Phase 4).
- Python plug-ins receive `NodeView`/`AttackerView` objects, never the world, so ground-truth separation is enforced by the API surface.
- Adding a family means adding a trait, a card family enum value, a conformance suite, and a Python base class.
