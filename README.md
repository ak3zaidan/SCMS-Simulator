# V2X World Simulator

> **Status: Phase 0/1 under construction.** The Rust workspace is a skeleton —
> every crate exists with its responsibility and dependency edges fixed, but the
> engine does not run scenarios yet. The validated Python reference in
> [`legacy/`](legacy/README.md) is the only thing that produces results today.

A simulator for V2X traffic on any world, at any scale from one vehicle to a
congested city, in which every node behaves as it would in a real deployment:
ground-truth traffic, frame-level radio, node hardware limits (CPU, HSM, queues,
storage), real or modeled cryptography, and pluggable credential-management
protocols (US SCMS, ETSI ITS PKI, threshold and post-quantum variants). It is
built as a research instrument — no black boxes: every model states its
equations, defaults, the citation behind each default, and what its fidelity
tier ignores. Runs are deterministic and reproducible across macOS, Linux and
Windows, recorded to MCAP, replayable in 2D/3D, and exportable as publishable
datasets.

## Restructured (ADR 0002)

The original Eclipse MOSAIC + Python stack was superseded on 2026-09-18 by a new
Rust core with Python and WebAssembly bindings
([ADR 0002](docs/adr/0002-supersede-base-stack-new-rust-core.md)):

- the Python engine is **frozen** at [`legacy/scms_sim_ref/`](legacy/README.md)
  as the validated reference — its butterfly, linkage, leakage, ML-contract and
  dataset-integrity vectors are the conformance suite the new engine must pass;
- the Java/MOSAIC layer, the PowerShell launchers and the unused
  `veremi-nextgen` submodule were deleted — they ran only from a Windows
  toolchain and never produced results (`docs/design/01-inventory.md` §3.7, §6);
- science is **ported, not rewritten**: each formula carries a model card citing
  the legacy file and line range it came from.

## Quickstart

Requires Rust 1.86 (pinned by `rust-toolchain.toml`), [`just`](https://just.systems),
and — for the legacy reference — [`uv`](https://docs.astral.sh/uv/).

```bash
just setup    # fetch Rust deps; Python/UI deps once those trees exist
just build    # cargo build --workspace
just test     # cargo test --workspace
just --list   # every recipe
```

Not yet working, by design: `just run <scenario>` (Phase 1) and `just studio`
(Phase 6) print what they are waiting on.

To run the frozen reference and its conformance vectors:

```bash
just legacy-setup
just legacy-conformance
```

## Repository layout

| Path | Contents |
|---|---|
| `Cargo.toml` | workspace root: shared dependency versions, one entry per crate |
| `crates/v2xw-core` | DES kernel: clock, event heap, RNG streams, registry, provenance |
| `crates/v2xw-world` | geometry model, importers, static spatial indices |
| `crates/v2xw-mobility` | ground-truth kinematics, demand, signal state |
| `crates/v2xw-radio` | propagation, fading, shadowing, PHY, MAC, DCC |
| `crates/v2xw-net` | WSMP/GN/BTP, fragmentation, backhaul, cellular Uu |
| `crates/v2xw-msg` | ASN.1 encoders and message envelopes |
| `crates/v2xw-sec` | primitive descriptors, crypto backends, cost tables |
| `crates/v2xw-node` | node runtimes: queues, CPU/HSM servers, stores, telemetry |
| `crates/v2xw-proto` | protocol host: entity state machines, flows, revocation |
| `crates/v2xw-threat` | attackers, jammers, detectors, misbehavior-authority host |
| `crates/v2xw-metrics` | metric providers |
| `crates/v2xw-record` | MCAP and Parquet recording, dataset exporters, leakage linter |
| `crates/v2xw-server` | JSON-RPC control surface and the VWP WebSocket stream |
| `crates/v2xw-cli` | the `v2xw` binary |
| `crates/v2xw-py`, `crates/v2xw-wasm` | binding surfaces (PyO3 / wasm-bindgen arrive later) |
| `python/v2xw/` | Python package: bindings, SDK, ported libraries (Phase 5) |
| `ui/` | pnpm workspace: protocol, viewer, Studio app (Phase 6) |
| `plugins/examples/` | one worked example per plug-in interface, with model cards |
| `scenarios/` | scenario files (YAML/JSON, schema v1) |
| `profiles/hardware/` | OBU/RSU/backend hardware profiles, each number cited |
| `worlds/cache/` | content-addressed world builds (git-ignored) |
| `tests/` | cross-crate golden determinism, conformance, validation, benchmarks |
| `legacy/` | the frozen Python reference and its conformance vectors |
| `docs/` | the design and the architecture decision records |

## The design

The full design is in [`docs/design/`](docs/design/README.md) — brief, inventory,
architecture, interfaces, models, protocols, node models, threats, measurement,
UI, roadmap and open questions — with the decisions recorded as ADRs in
[`docs/adr/`](docs/adr/). Start with
[`00-design-brief.md`](docs/design/00-design-brief.md) for the goal,
[`02-architecture.md`](docs/design/02-architecture.md) for the component map, and
[`10-roadmap.md`](docs/design/10-roadmap.md) for what each phase delivers.

## Licensing

Code is Apache-2.0 ([`LICENSE`](LICENSE)); generated datasets are CC-BY-4.0.
The design deliberately avoids GPL dependencies (Veins, Artery, F2MD, in-process
ns-3): they are read as references and re-implemented. SUMO is optional and runs
out of process ([ADR 0005](docs/adr/0005-mobility-provider.md)).
