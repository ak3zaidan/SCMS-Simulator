# Build status

Living record of what is built and what has actually been *measured*, as against
the plan in `10-roadmap.md` and the decisions in `12-build-decisions.md`. Claims
here carry their evidence; anything unmeasured says so.

Last updated 2026-09-18. **The crate table below is stale**: `v2xw-record`,
`v2xw-metrics`, `v2xw-node` and `v2xw-engine` are no longer stubs, and the line counts
predate several waves. It is left as written rather than rewritten from memory, because a
status file whose numbers were re-estimated rather than re-measured is worse than one that
says it is out of date.

For the current release position — what must be true for a 1.0 tag, what is not true, and
what each gap would take — see [`docs/RELEASE-CHECKLIST.md`](../RELEASE-CHECKLIST.md)
(2026-09-22). The Phase 1 acceptance table below is still accurate and the checklist cites
it.

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

Three of seven are now met with independently reproduced evidence. "Independent"
means a second agent re-derived the number from the standard or the literature
rather than re-running the builder's test.

| # | Criterion | Status |
|---|---|---|
| 1 | Golden determinism, identical digests on each operating system | **Gate built, never run.** CI imports a committed fixture on all three platforms and fails unless the digests agree. Determinism itself is repeatedly proven *within* a platform: a double import of the real 30 MB extract is byte-identical across all four artefacts in separate processes, and two recordings of one run have identical data sections. No CI run has executed. |
| 2 | Envelope size equals the size model, zero bytes tolerance | **Met.** Exactly 93 bytes for a digest signer and 87 plus the certificate for a certificate signer, confirmed by decomposing a real signed message octet by octet against the derivation. The derivation's own byte-count threshold was wrong and is corrected (D12.1). |
| 3 | Modelled and real crypto produce identical logs | **Not met.** Genuine but incomplete: a verifier broke equivalence three ways, through signature malleability, invalid key material and unsupported post-quantum primitives. Repairs in flight. |
| 4 | Seek at most 100 ms at the 95th percentile | **Met, 57x margin.** 1.763 ms independently measured on a 9,001-frame, 600-second recording, using a type-7 quantile rather than nearest-rank so the figure is not a ranking artefact. |
| 5 | 60 fps with the heads-up display, every value resolving to a model card | **Met and exceeded.** 604 fps on a real GPU at 5,000 actors against ADR 0011's 60 fps target. Every heads-up value is keyboard reachable and opens its provenance; the focus indicator measures 9.10:1 contrast against a 3:1 floor. |
| 6 | Manifest lists engine, plug-in, world and card hashes | Pending. The world hash exists; manifest assembly is owed by `v2xw-engine`. |
| 7 | Manual map-to-chase fly-down | The automated fly-down passes end to end against the mock engine. Needs a human to judge. |

## Correction, 2026-09-22 — the end-to-end run is a stub at the message layer

An earlier entry here and my report to the owner both described the Phase 1 run
as producing "970 signed messages". That was wrong, and an independent audit
caught it. Nothing is encoded and nothing is signed: the node returns a size from
a model and hands the engine a byte count. The layers below messaging are real
and verifiably deterministic; the messaging layer is a faithful size model with
no payload and no signature behind it.

Recorded rather than quietly fixed, because the reason it passed unnoticed is
instructive. Every number in the run report was plausible and self-consistent,
the recording verified, and the digest reproduced. What gave it away was one
comparison nobody had made: two different message formats came out at exactly the
same size. Full detail in `findings/slice-verification.md`.

## Verification standard used

Every crate was built, then adversarially validated by an agent told to re-derive
rather than review. That produced results worth recording, because in several
cases the independent derivation was the only thing that could have caught the
defect:

- The packet-error model was re-implemented from scratch in Python and agrees
  with the crate to 5e-10 dB across all 24 cells.
- The message encoder was checked against two independent implementations: a
  Python oracle compiled from the real standards modules, and an encoder the
  verifier wrote from the encoding rules. 235 vectors, both directions.
- Signature determinism was confirmed by reimplementing the relevant standard in
  Python and reproducing the exact 64 bytes.
- The cryptographic port was checked by re-running the legacy Python to capture
  fresh vectors rather than trusting committed ones.
- The wire specification's worked hex dumps were re-extracted from the
  specification text at run time and shown byte-identical to the checked-in
  fixture, so the golden test really is the specification's bytes.
- Timing fixes were mutation-verified: reintroducing each bug reproduced the
  original failure signature.

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
