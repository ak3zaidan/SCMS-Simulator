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

7. **Writer-side quantisation of every exported float.** No floating-point value reaches a recorded, exported or digested artefact in raw IEEE-754 form. A single writer-side encoder quantises every float to its field's declared grid (the legacy convention is 3 decimals for metres and seconds; each field's quantum is declared in its schema), and a test scans every output file for any value that is off its grid. The digest is therefore computed over quantised values only.

### Why: evidence from the legacy engine

This rule was added after diagnosing why the legacy Python engine's frozen golden digests stopped reproducing. The cause was found and proven, and it is instructive.

Exactly one field in the entire legacy dataset escapes rounding: `st_bbox` in `ma/ma_reports.jsonl`, written at `run.py:2089` as `[min(cx,px), min(cy,py), max(cx,px), max(cy,py)]` while all 39 other float writes pass through `round(x, 3)`. Those coordinates descend from `sin`/`cos` calls, and `sin`, `cos`, `tan`, `exp` and `pow` are not correctly rounded in any libm. The digests were pinned on Windows x86-64; on macOS arm64 the same seed produces values differing by 1 to 16 units in the last place (for example `math.sin(3.9999999999999996)` differs by exactly 1 ULP between the two), which changes one file's SHA-256 and therefore the aggregate digest.

The proof is necessary and sufficient: rounding **only** `st_bbox` to three decimals makes the digest identical across architectures for all four configurations tested. Nothing else in the dataset differs — row counts, ordering, identifiers, revocation sets and the other nine files are byte-identical, the maximum deviation is 5.7e-14 m, and no detector, feature or audit check reads `st_bbox` at all. The legacy engine remains perfectly self-consistent on one machine (same seed twice, across three Python versions and with hash randomisation enabled, always agrees); it simply disagrees with a different machine's last float bit.

Two consequences for this project:

- **A digest over raw doubles is not a portable conformance artefact.** ADR 0002's retirement of the legacy fixed digests is correct, and this is the reason to record.
- **Owning our transcendentals is necessary but not sufficient.** ADR 0003's pure-Rust `libm` makes every platform agree *within* this engine, but quantising at the writer makes the digest survive a future change of math library, compiler, or language. Approximately twenty lines of encoder plus one scanning test eliminate the entire bug class.

A related caution, also measured: perturbing *every* transcendental result by one ULP shifted one configuration's report count from 3,082 to 3,135, because detector thresholds and event ordering can flip at a boundary. Raw transcendental values must therefore not drive cross-engine control-flow comparisons; quantise before threshold tests, and make cross-engine validation tolerant of boundary flips rather than demanding exact equality.

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
