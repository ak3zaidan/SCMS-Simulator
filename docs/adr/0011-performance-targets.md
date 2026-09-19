# ADR 0011 — Performance targets and the evidence for them

- **Status:** Proposed (2026-09-18)
- **Related:** `02-architecture.md` §11, Appendix A, `09-ui.md` §4, `10-roadmap.md` Phase 3.

## Context

The brief proposes at least 10,000 vehicles at real time or faster in the abstract tier headless on a laptop, about 1,000 vehicles with full PHY/MAC at a tenth of real time, 60 fps with 5,000 rendered vehicles, and sub-100 ms scrubbing, and asks for push-back with numbers if they are wrong.

## Decision (targets, reference machine = 8-core laptop of the Apple M2 class, 8 GB RAM)

| Target | Value | Basis |
|---|---|---|
| Abstract tier, headless | ≥ 1× real time at 10,000 vehicles; stretch 3× | spike: 1.7× single-threaded, 4.5× on 8 cores for a harsher-than-city density (≈ 786 in-range neighbours); node-local queues moved into the parallel phase to remove the heap bottleneck the spike exposed (Appendix A) |
| Medium radio tier | ≥ 1× real time at 5,000 vehicles | analytical budget: ~10× cheaper than high per candidate reception (02-architecture §11.2) |
| High radio tier (802.11p or C-V2X) | ≥ 0.1× real time at 1,000 vehicles in a dense downtown (k ≈ 300 in range) | budget ≈ 3.3 µs per candidate reception single-core, ~25 µs on 8 cores; a SINR/PER evaluation over ≤ 30 concurrent interferers is 1–3 µs in Rust (02-architecture §11.2); to be measured in Phase 3 |
| Backend-only long runs (time-dilation windows) | ≥ 1,000× real time for 7 simulated days of certificate lifecycle | flows are thousands of events per simulated hour; no radio events in dilated windows |
| UI | 60 fps with 5,000 instanced vehicles on an integrated GPU; 10,000 actors with off-screen aggregation | instancing evidence and the count-based culling result (09-ui §4) |
| Seek | ≤ 100 ms p95 for any time in a 1-hour recording | keyframe every 1 s + ≤ 10 deltas; MCAP chunk index (ADR 0008); measured in Phase 1 CI |
| Python plug-in overhead | ≤ 20 % wall-time increase for one batched Python detector at 10,000 nodes in the abstract tier | batching per node-step (ADR 0007); measured in Phase 2 |

## Push-back on the brief's numbers

- "10,000 at real time in the abstract tier" is conservative on this hardware: the spike reached 4.5× with rayon, so the target is kept at ≥ 1× and 3× is a stretch goal rather than raising the floor, because the spike excluded mobility interaction, message encoding and node-runtime accounting.
- "1,000 vehicles with full PHY/MAC at a tenth of real time" is achievable but tight in the densest case; if Phase 3 measurements land below 0.05×, the fallback is the focus-region mixed tier (02-architecture §7.3), which keeps the followed neighbourhood at high fidelity.
- Two numbers were **not** adopted as targets because no evidence supports them yet: SUMO's per-step overhead (unpublished [R8 §A.2]) and the WASM slowdown relative to native (only blog claims exist [R8 §B.2]); both are measured in Phase 3 and Phase 5 respectively and recorded in model cards.

## Consequences

- Benchmarks are part of CI with regression thresholds (nightly, Linux); a 10 % regression blocks the merge.
- Targets are re-baselined per reference machine; the manifest records the machine class for any published performance claim.
