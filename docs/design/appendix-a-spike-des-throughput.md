# Appendix A — Throwaway spike: abstract-tier loop throughput, Rust vs Python

Status: evidence for ADR 0003 (core language) and ADR 0011 (performance targets). The spike code is throwaway and lives outside the repository; this appendix records what was run and what came out, so the decision is auditable. Run on 2026-09-17/18.

## Setup

Machine: Apple M2 (8 cores), 8 GB RAM, macOS. Toolchain: rustc/cargo 1.86.0; system Python 3.12.8 (pure-Python runs); a venv with Python 3.13.14 + NumPy 2.5.3 (NumPy runs). Seed 42 in all runs.

Workload, identical in all four implementations (same formulas, same operation order, same RNG):

- N vehicles uniformly placed in a 3,000 × 3,000 m area with random headings, constant 14 m/s, mirror-bounce at the boundary; 100 ms mobility step; 10 simulated seconds (100 steps).
- Every vehicle transmits one beacon per step. Neighbors within 300 m are found with a fixed 10 × 10 grid of 300 m cells (3 × 3 neighborhood, ascending id within a cell).
- Abstract reception: `p = exp(−2·(d/300)²) · (1 − min(0.9, n_in_range/200))`, one uniform draw per candidate from splitmix64 with a counter derived from `(seed, step, i, k)`, so the draw sequence is independent of evaluation order or threading.
- Each reception enqueues a verification event on the receiver's single-server FIFO (0.5 ms + queueing + 1 ms service) into a binary heap that is drained to the step end.
- Implementations: pure Python (`heapq`), NumPy-vectorised Python (heap loop stays scalar), Rust single-threaded, Rust with `rayon` over transmitters (neighbor search and reception decision only; grid build, mobility, and the heap stay sequential). Rust release profile: opt-level 3, LTO fat, codegen-units 1.

## Results (two repetitions per cell; wall time shown as a / b)

| Version | N | Steps | Wall s | Real-time factor | Receptions | DES events/s | Peak RSS MB |
|---|---|---|---|---|---|---|---|
| Pure Python 3.12 | 1,000 | 100 | 2.86 / 2.88 | 3.50 | 1,083,841 | 379 k | 18 |
| Pure Python 3.12 | 5,000 | 100 | 59.0 / 60.4 | 0.17 | 8,119,414 | 138 k | 33 |
| Pure Python 3.12 | 10,000 | 20 (capped) | 44.4 / 44.1 | ≈ 0.045 (extrapolated to 100 steps ≈ 222 s) | 2,629,771 (20 steps) | 59 k | 38 |
| Python + NumPy | 1,000 | 100 | 1.21 / 1.17 | 8.30 | 1,083,841 | 900 k | 68 |
| Python + NumPy | 5,000 | 100 | 14.2 / 13.0 | 0.70 | 8,119,414 | 571 k | 550 |
| Python + NumPy | 10,000 | 100 | 44.9 / 41.8 | 0.22 | 13,177,812 | 294 k | 1,395 |
| Rust, 1 thread | 1,000 | 100 | 0.163 / 0.148 | 61.2 | 1,083,841 | 6.6 M | 1 |
| Rust, 1 thread | 5,000 | 100 | 1.98 / 2.01 | 5.05 | 8,119,414 | 4.1 M | 3 |
| Rust, 1 thread | 10,000 | 100 | 5.90 / 5.91 | 1.69 | 13,177,812 | 2.2 M | 6 |
| Rust + rayon (8 cores) | 1,000 | 100 | 0.117 / 0.104 | 85.8 | 1,083,841 | 9.3 M | 2 |
| Rust + rayon (8 cores) | 5,000 | 100 | 0.991 / 0.965 | 10.1 | 8,119,414 | 8.2 M | 4 |
| Rust + rayon (8 cores) | 10,000 | 100 | 2.22 / 2.18 | 4.50 | 13,177,812 | 5.9 M | 7 |

Candidate pair checks scale as N² in this fixed-area setup (7.85 M → 197 M → 786 M for N = 1,000 → 5,000 → 10,000), i.e., mean in-range neighbors ≈ 79, 394, 786. A real city holds density roughly constant as the world grows, so absolute numbers do not transfer to 50,000 vehicles in the same 9 km²; the per-candidate cost does.

## Determinism

- Same version, same seed, two processes: all twelve cells identical in reception counts, per-step reception sequence, and final position checksum, including the rayon build.
- Across languages at the same N: pure Python, NumPy, Rust and Rust+rayon produce the same reception count, position checksum and per-step reception hash (N = 1,000: 1,083,841 receptions, checksum 2976508.288424961, hash `a2ced3e81cb9`; N = 5,000: 8,119,414, `b29f42adb564`; N = 10,000: 13,177,812, `076f3e78f4af`). The capped pure-Python N = 10,000 run equals the sum of the first 20 entries of the Rust per-step list (2,629,771).

This confirms that with a counter-based RNG and identical IEEE 754 double operation order, outcomes are bit-identical across languages and thread counts. One caveat is platform luck: on macOS arm64, Rust's `f64::exp`/`cos`/`sin`, CPython's `math.*` and NumPy 2.5 all resolve to Apple's libm (a 1 M-sample spot check found 0 ULP difference between `np.exp` and `math.exp`), whereas glibc, musl, the MSVC UCRT and NumPy's SIMD paths on x86 differ in the last bits for `exp`/`cos`, and a C/C++ build with `-ffp-contract=fast` would add FMAs. A core that promises cross-platform determinism must therefore own its transcendental functions (the pure-Rust `libm` crate, ADR 0003), keep the RNG counter-based, and checksum integer event counts rather than floats (ADR 0004). Measured unit costs in Rust: ≈ 7.5 ns per candidate check and ≈ 90 ns per heap push/pop pair; pure Python ≈ 300 ns and ≈ 600 ns respectively.

## Interpretation

- The abstract tier at 10,000 vehicles runs faster than real time in single-threaded Rust and 4.5× real time on 8 cores; the target in ADR 0011 (≥ 1× real time) holds with margin on a laptop.
- Parallel speed-up shrinks as N grows (1.4×, 2.0×, 2.7× on 8 cores) because the sequential heap drain becomes the bottleneck at high reception rates (1.19 s of 2.2 s at N = 10,000); the engine design therefore partitions node-local queues (verification FIFOs) per node and processes them in the phase-parallel map (02-architecture §6.4), keeping only cross-node events in the global heap.
- Pure Python cannot approach the target (≈ 22× slower than real time at 10,000; ≈ 6× slower at 5,000); NumPy helps the vectorisable part but the event loop stays interpreted (≈ 4.5× slower at 10,000, with 1.4 GB peak RSS from materialised candidate arrays). A Python-hosted core would need to move the event loop out of Python, which is the Rust-core decision by another route.

## Caveats

Abstract reception only (no propagation, fading, interference, MAC); no mobility interaction; single machine; two repetitions per cell (run-to-run wall time varies 1–9 %); NumPy ran on a different interpreter build than pure Python; peak RSS is macOS maximum resident size over the run.
