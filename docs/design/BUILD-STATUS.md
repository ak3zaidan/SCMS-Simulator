# Build status

Living record of what is built and what has actually been *measured*, as against
the plan in `10-roadmap.md` and the decisions in `12-build-decisions.md`. Claims
here carry their evidence; anything unmeasured says so.

Last updated 2026-09-18.

## Crates

| Crate | Lines | State | Evidence |
|---|---:|---|---|
| `v2xw-core` | 13,487 | complete | 215 tests, zero clippy warnings. Determinism kernel independently reviewed: no defect in the RNG algorithm, math routing, event ordering, manifest digest or float reduction order. 1 critical + 10 major API defects found and fixed, then 7 more in a completion pass. |
| `v2xw-world` | 20,336 | complete | Imports real Midtown Manhattan in 377 ms. Verified below. |
| `v2xw-msg` | 7,956 | ETSI done, J2735 BSM in progress | ETSI stack generates from the forge modules and compiles (9,811 lines). |
| `v2xw-mobility` | 2,409 | in progress | — |
| `v2xw-radio` | 1,866 | in progress | — |
| `v2xw-net` | 1,374 | in progress | — |
| `v2xw-sec` | 555 | in progress | — |
| `v2xw-record` | stub | in progress | — |
| `v2xw-metrics` | stub | in progress | — |
| `v2xw-node` | stub | not started | Blocked on the radio, net, msg and sec trait definitions. |
| `v2xw-engine` | absent | **not started** | Owed by build-decision D8. Gates the headless end-to-end run. |
| `v2xw-cli`, `-server`, `-proto`, `-py`, `-wasm`, `-threat` | stubs | not started | |

UI: `ui/packages/{protocol,mock-server,viewer}` and `ui/apps/studio` exist; the
conformance and quality review is in progress.

## World import — verified

The importer was checked against the real city, not only against itself.

**Correct.** Projection error is 0.110 m worst case over an 856 m baseline
(0.013 %), cross-checked against three surveyed landmarks by geodesic distance.
The modal drivable heading is 60–61° with 90.5 % of lane length within ±4° of
it, which matches Manhattan's grid being rotated about 29° from true north;
Broadway correctly falls out as the 81° diagonal. Lane ordering is right-hand
traffic in 610 of 610 multi-lane edges and 178 of 178 two-way pairs, with none
wrong. Rendering confirms per-lane centrelines, crossings with stop lines,
sidewalks, turn connections and one-way chevrons. Building heights top out at
443 m with a median of 45 m.

**Scale.** 3,421 junctions (303 signalised), 13,760 lanes (5,450 drivable,
3,029 junction connectors, 8,291 sidewalk), 27,497 connections with 78 banned by
34 turn restrictions, 7,390 buildings, 963 crossings. 295 anomalies across 18
named categories, every one counted with example way ids, no panics.

**Defects found.** Six, recorded with evidence in
`findings/world-import-defects.md`. Two are major and affect any traffic result:
the speed-limit class defaults are SUMO's German rural values, giving 16 % of
drivable lanes a 100 km/h limit on Manhattan side streets, and lane width is a
single global constant so every lane is exactly 3.50 m. A third is worse for
routing: only 52.8 % of driving lanes lie in a strongly connected component.

## Phase 1 acceptance criteria

| # | Criterion | Status |
|---|---|---|
| 1 | Golden determinism, identical digests on each OS | **gate built, not yet run.** CI now imports a committed fixture on all three platforms, publishes the engine's own world and VWP payload digests, and fails unless all three agree. Previously CI ran tests per-platform, which proves nothing about agreement. The fixture digest is stable across repeated local runs on macOS arm64. No CI run has executed yet. |
| 2 | Envelope size equals the size-model prediction, 0 bytes tolerance | pending `v2xw-sec` |
| 3 | `modeled` and `real` crypto produce identical logs | pending `v2xw-sec` |
| 4 | Seek ≤ 100 ms p95 over a 10-minute recording | pending `v2xw-record`; a benchmark is specified as part of it |
| 5 | 60 fps with the HUD, every value resolving to a model card | pending the UI review |
| 6 | Manifest lists engine, plug-in, world and card hashes | world hash exists; manifest assembly is owed by `v2xw-engine` |
| 7 | Manual map-to-chase fly-down | pending Studio |

## Corrections made to the design during the build

The design is not treated as infallible. Where implementation disproved it, the
document was amended and the reason recorded:

- **ADR 0008's pose quantisation was impossible as written.** Int16 millimetres
  spans ±32.767 m and cannot address a square-kilometre world. Corrected to i32
  millimetre keyframes about a per-run origin with i16 millimetre deltas about
  the previously *transmitted* quantised value, which is also what stops error
  accumulating, plus an absolute escape for teleports.
- **FlatBuffers dropped for VWP v1** in favour of a flat fixed layout, so the
  recorder can store the exact bytes that went over the wire and live and replay
  are provably identical.
- **ADR 0004 gained an evidence section** from the legacy digest forensics, which
  produced build-decisions D9 and D10.
- **D11** arbitrates five places where the design and the implementation
  disagreed.
- **CI contradicted D1**, pinning Rust 1.86.0 against the file's 1.98.1. Fixed,
  with an assertion so they cannot drift again.
- **The staged Phase 0 cleanup would have deleted three files** that
  `01-inventory.md` §3.7 explicitly preserves. Rescued into `legacy/reference/`.
