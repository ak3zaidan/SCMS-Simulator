# ADR 0004 — Time model, determinism, parallelism, spatial indexing

- **Status:** Proposed (2026-09-18)
- **Related:** `02-architecture.md` §5–6, `03-interfaces.md` §1, Appendix A.

## Context

The engine must run frame-level radio (µs timing), backend flows (days), and fixed-step mobility (10–100 ms) in one deterministic run whose outputs are byte-identical across macOS, Linux, Windows, x86-64, arm64, native and WASM, and independent of thread count. It must also query neighbors and interferers for 10,000 nodes many times per simulated second.

## Decision

1. **One discrete-event kernel** with `SimTime = u64` nanoseconds (1 µs guaranteed to models), a binary heap ordered by `(time, priority, seq)`, fixed priorities per event class (02-architecture §5.1), and a monotonic `seq` assigned at scheduling.
2. **Mobility is a periodic event** (`Δt_mob` default 100 ms, 10–100 ms allowed; SUMO ≥ 10 ms) that publishes kinematics; between steps the published constant-velocity extrapolation rule gives positions to radio events. Time-dilation windows let long backend experiments skip radio events explicitly (02-architecture §5.4).
3. **Random numbers** come only from counter-based per-entity streams (ChaCha12 keyed by `H(seed ∥ domain ∥ entity)`); no shared sequential RNG; distribution samplers are implemented in-crate. This generalises the legacy per-vehicle string-keyed streams and removes their order dependence.
4. **Floating point:** `f64`, no fast-math, no implicit FMA (Rust does not contract), transcendental functions through the pure-Rust `libm` crate, reductions in id-sorted order.
5. **Parallelism is phase-parallel only:** pure maps over actors (mobility), receivers (reception), nodes (verification, detectors, metrics), merged in id order; the event loop is single-threaded; node-local queues are processed inside the parallel phases so the global heap holds only cross-node events (the spike showed the global heap becomes the bottleneck otherwise, Appendix A).
6. **Spatial indexing:** uniform grid hash rebuilt per mobility step with cell size = maximum modeled communication range (3 × 3 query), per-lane ordered lists for car-following, an R-tree over building footprints with a cached LOS table for the focus region, and a DEM raster for terrain.

## Alternatives

| Option | Determinism | Multi-rate fit | Parallel scaling | Complexity | Verdict |
|---|---|---|---|---|---|
| A. Pure fixed-step loop (legacy style), radio decided per step | high | poor (µs timing impossible) | easy | low | rejected: cannot model CSMA/SPS timing |
| B. DES with fixed-step mobility inside it (chosen) | high with counter RNG | good | phase-parallel | medium | chosen |
| C. Optimistic parallel DES (Time Warp) | hard (rollback) | good | best | high | rejected: determinism and debuggability |
| D. Conservative parallel DES with lookahead by region | high if carefully done | good | good for large worlds | high | deferred: revisit if Phase 5 needs > 8 cores |
| Float: platform libm vs `libm` crate vs fixed-point | libm crate is bit-identical across platforms; fixed-point complicates every model | | | | `libm` crate |
| RNG: sequential PCG per run vs per-entity counter streams | per-entity streams make outcomes independent of event order and thread count | | | | per-entity streams |
| Spatial: grid hash vs kd-tree vs R-tree per step | grid is O(1) rebuild and query for uniform ranges; kd-tree rebuild per 100 ms for 10k points is affordable but unnecessary | | | | grid hash for actors, R-tree for static geometry |

## Consequences

- Plug-ins cannot own RNGs or read wall time (conformance kit enforces).
- Every event class has a documented priority; adding a class requires an entry in the table.
- Golden determinism tests run on all three OSes in CI; a platform difference is a bug, not a tolerance.
- The extrapolation rule between mobility steps is part of the interface contract and appears on model cards as an assumption.
