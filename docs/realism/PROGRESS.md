# Realism push — progress log

Mission: SOTA traffic + network realism (see [ROADMAP.md](ROADMAP.md)). Every phase gates on a
quantitative benchmark; determinism/manifest contract and config→GUI pipeline are hard invariants.

## Baseline (2026-08-29, main @ 2832d63)

- Test suite: **496 passed** in 607 s (Python 3.12.10, SUMO 1.25.0, JDK 17.0.20.1, MOSAIC 25.2).
- Reference run: `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15
  --traffic-lights --seed 42` → 593 vehicles, 7823 reports, data_digest
  `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48`, precision 0.599 / recall 0.91,
  latency_med 4.0 s.
- Realism scorecard: none yet (Phase 0 builds it). Known Day-0 realism defects are ranked G1–G16 in
  ROADMAP.md §2.

## Phase log

| Date | Phase | Status | Gate result |
|------|-------|--------|-------------|
| 2026-08-29 | Investigation (13 agents) | done | Roadmap adopted |
| 2026-08-29 | Phase 0 — realism bench harness | started | — |
| 2026-08-29 | Phase 1 — MOSAIC/SUMO flagship realism | started | — |
