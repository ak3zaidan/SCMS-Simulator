# ADR 0010 — Repository layout, one-command setup, CI on three platforms

- **Status:** Proposed (2026-09-18)
- **Related:** `02-architecture.md` §10, ADR 0003, ADR 0005. Evidence: R8 §C.

## Context

Development moved to macOS; the README's PowerShell scripts and `C:\Users\Administrator` toolchain paths are stale; the new system spans Rust, Python and TypeScript; SUMO is optional; CI must cover macOS, Linux and Windows.

## Decision

1. **Monorepo** in the existing repository: `crates/` (Cargo workspace), `python/v2xw/` (PyO3 package + SDK + ported libraries), `ui/` (pnpm workspace), `plugins/examples/`, `scenarios/`, `profiles/hardware/`, `docs/`, `legacy/scms_sim_ref/`, `tests/`.
2. **Toolchain pinning with `mise`** (MIT; single `mise.toml` for Rust, Python, Node, `uv`, `pnpm` versions and tasks; macOS/Linux/Windows [R8 §C]); **tasks with `just`** (CC0-1.0 [R8 §C]); **`uv`** for Python environments (Apache-2.0, Rust-based, universal lockfile [R8 §C]); **`maturin`** for wheels; **`pnpm` + Vite** for the UI (MIT [R8 §C]).
3. **One command:** `mise install && just setup` builds the engine, installs the Python package in development mode, installs UI dependencies, and runs the smoke test; `just run <scenario>` and `just studio` are the daily entry points. SUMO is optional (`just sumo-install` → PyPI wheels `eclipse-sumo`/`libsumo`, apt/PPA, or official installer [R8 §C]).
4. **CI:** GitHub Actions matrix on `ubuntu-latest`, `macos-latest` (arm64), `windows-latest` [R8 §C] running unit, conformance and golden determinism tests on every push; nightly full validation, benchmarks with regression thresholds, and the external cross-validation harness (ADR 0006) on Linux only. Public-repository runners are free; private-repository minutes bill macOS at 10× and Windows at 2× Linux [R8 §C], which favours keeping the heavy suites on Linux.
5. **Artifacts:** wheels for the three platforms, the WASM bundle, and the Studio build are attached to releases; the docs site is built from the model registry.

## Alternatives

| Option | Cross-platform | Reproducible toolchain | Friction | Verdict |
|---|---|---|---|---|
| A. Multiple repositories (engine, python, ui) | yes | harder to keep the wire layout in sync | high | rejected: one hand-written VWP layout must be implemented, and conformance-tested against one set of golden vectors, in three languages |
| B. Monorepo + mise + just + uv + pnpm (chosen) | yes | good | low | chosen |
| C. Nix/devenv | yes (Windows via WSL only) | best | high on Windows and for newcomers | rejected as the default; a `devenv` file may be offered later (Apache-2.0 [R8 §C]) |
| D. conda/pixi | yes | good | SUMO not on conda-forge (the `sumo` package is unrelated [R8 §C]) | rejected as primary |
| E. Docker-only dev | Linux only in practice | good | poor for the UI and macOS GPU | rejected |

## Consequences

- The PowerShell scripts, the MOSAIC layer and the Windows absolute paths are removed in Phase 0 (01-inventory §6).
- Contributors need `mise` only; everything else is pinned by it.
- Windows CI runs the same golden tests; any platform difference is a bug (ADR 0004).
