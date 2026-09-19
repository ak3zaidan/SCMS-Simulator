# ADR 0003 — Core engine language and runtime

- **Status:** Proposed (2026-09-18)
- **Related:** ADR 0002, ADR 0004, ADR 0007, `02-architecture.md` §11, Appendix A (spike). Evidence rows cite the ecosystem research sheet (R8) and the spike.

## Context

The engine must be deterministic across macOS, Linux, Windows, x86-64, arm64 and the browser; handle 10,000 nodes in the abstract tier at real time and ~1,000 with a frame-level PHY at a tenth of real time; be extensible by researchers who mostly write Python; run in a browser for small scenarios and replay; build with one command on three platforms; and use only Apache-compatible dependencies.

## Decision

**Rust core** (`crates/v2xw-*`), **PyO3 bindings** packaged with `maturin` into the `v2xw` Python package, and a **WebAssembly build** (`wasm-bindgen`) of the engine and replay reader. Python is the plug-in language for control-plane families through the batched SDK; Rust for hot paths (ADR 0007). Floating-point rules: `f64`; transcendentals through the pure-Rust `libm` crate; no implicit FMA; id-ordered reductions (ADR 0004).

## Evidence

- **Determinism.** Rust's standard library documents that `sin`, `cos`, `exp`, `pow` and the other transcendentals have "non-deterministic" precision that "varies by platform, Rust version, and can even differ within the same execution", because they call the platform libm; `sqrt` and `mul_add` are IEEE-754-guaranteed, and Rust never contracts `a*b + c` into an FMA [R8 §B.1: doc.rust-lang.org f64]. Hence the `libm` crate (MIT/Apache-2.0, pure Rust) for transcendentals. WebAssembly's numerics are deterministic except NaN payload bits, with round-to-nearest-even only and FMA only as an explicit operator [R8 §B.1: WebAssembly spec numerics]. The spike showed bit-identical outcomes across Python, NumPy, single- and multi-threaded Rust with a counter-based RNG and identical operation order (Appendix A).
- **Performance.** Abstract-tier loop at 10,000 vehicles: pure Python ≈ 0.045× real time, NumPy 0.22×, Rust 1.7× single-threaded, 4.5× on 8 cores (Appendix A). SimPy's best case on this laptop is ≈ 2.1 M trivial timeout events/s [R8 §B.2, measured], comparable to a bare heap and far below the tens of millions of receptions per simulated second the high tier implies (02-architecture §11.2).
- **Python ergonomics.** PyO3 0.29 (MIT/Apache-2.0), `maturin` 1.15 for wheels on all three platforms [R8 §B.3]; PyO3 documents that attaching/detaching the interpreter costs under a millisecond and recommends batching work across the boundary [R8 §B.2: pyo3.rs performance]; the SDK batches per node-step (ADR 0007).
- **Ecosystem.** `rasn` 0.28 (MIT/Apache-2.0) supports UPER, COER and OER, with `rasn-its` covering IEEE 1609.2 and TS 103 097 [R8 §B.3; R4 §G]; `p256`, `ml-dsa`, `pqcrypto`, `oqs` bindings to liboqs (MIT) [R8 §B.3; R5 §D]; `rstar`, `kiddo`, `geo`, `rand_chacha`, `arrow`/`parquet` 60, `flatbuffers`, `mcap` 0.25, `tokio`/`axum`, `wasm-bindgen` all MIT/Apache-2.0 [R8 §B.3]. A/B Street and osm2streets (Rust, Apache-2.0) are reference implementations for OSM-to-lane import and browser builds [R8 §A.3].
- **Browser.** WASM build through `wasm-bindgen`; A/B Street demonstrates a Rust traffic simulator running natively and in the browser from one code base [R8 §A.1].

## Alternatives (decision matrix; weights in parentheses; scores 1–5)

| Criterion (weight) | Python + NumPy/Numba | Rust + PyO3 + WASM (chosen) | C++ + pybind11 + Emscripten | TypeScript core |
|---|---|---|---|---|
| Determinism across platforms (5) | 2 (BLAS/reduction order and per-platform RNG caveats [R8 §B.1]) | 5 | 4 (compiler flags for contraction/fast-math must be policed) | 3 (fdlibm convergence covers only some functions [R8 §B.1]) |
| Performance at 10k nodes (5) | 1 (spike) | 5 (spike) | 5 | 3 |
| Plug-in ergonomics for Python researchers (4) | 5 | 4 (batched SDK) | 3 | 2 (Node interop) |
| Browser story (4) | 1 | 5 | 4 (Emscripten, MIT/NCSA [R8 §B.4]) | 5 |
| Cross-platform builds (3) | 4 | 5 (cargo, maturin wheels) | 3 (toolchains, ABI) | 5 |
| Testability and memory safety (3) | 3 | 5 | 3 | 4 |
| Licensing of ecosystem (5) | 5 | 5 | 5 | 5 |
| Effort to Phase 2 (3) | 4 | 3 | 2 | 3 |
| **Weighted total** | **97** | **150** | **123** | **116** |

## Consequences

- Two-language core: contributor docs and scaffolds must make the Python path the default for researchers and the Rust path the default for models.
- CI builds native wheels and the WASM bundle on every commit; golden tests run on three OSes.
- The `libm` crate's bit-identity with musl is documented but its README does not promise identical results to any specific platform libm [R8 §B.1]; we therefore treat *its own* output as the reference on all platforms (it is the same code everywhere), which is what the golden tests check.
- OMNeT++-based stacks cannot be adopted in-process for a second reason beyond GPL: OMNeT++ itself is under the Academic Public License, which requires a commercial licence for for-profit use [R8 §A.1]; they remain references only (ADR 0006).
