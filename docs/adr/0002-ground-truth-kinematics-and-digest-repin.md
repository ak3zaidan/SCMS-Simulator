# ADR 0002 — Emit true speed/heading in ground truth, and re-pin the golden digests

Status: accepted (2026-08-30)
Context: the realism push (docs/realism/ROADMAP.md); supersedes the implicit "never move the digest" reading of the determinism contract.

## Context

`gt_emissions_sample.jsonl` records `true_x`/`true_y` but **no true speed or heading**. Every
kinematic quantity in the realism harness — acceleration, time headway, the fundamental diagram,
speed distributions — is therefore reconstructed by differentiating position twice.

That reconstruction is the root cause of an entire class of false measurement:

- Before the Phase-1 corrections, derived acceleration reached **±275 m/s² (28 g)** on runs whose
  true accelerations were entirely plausible (p01/p99 of −4.18/+2.65). The cause was a lane change:
  `true_x` moves ~3.2 m — one SUMO lane width — in a single sample while speed is constant.
- The longitudinal/lateral decomposition added in the corrections pass cut that to ±39 m/s² on
  InTAS and ±6.7 m/s² on the smoke run, but cannot reach zero. Residuals concentrate at short
  sampling intervals (241 of 286 at pair dt ≤ 0.35 s; the (0.75, 1.5] s bucket scores exactly
  1.000000), which is the signature of differencing noise, not vehicle dynamics.
- Sampling is irregular by design now (0.1–1.0 s) because ETSI CAM triggering varies the rate, which
  makes double-differencing worse, not better.

The simulator knows the true speed exactly at emission time. Both engines already transmit a
*claimed* speed. We are reconstructing, badly, a quantity we have exactly.

## Decision

1. Add `true_speed` and `true_heading` to the ground-truth emission record in **both** engines
   (the Python `mock_pipeline` and the MOSAIC/Java backend). These are ORACLE-visibility fields in
   `ground_truth/`, never MA-visible, and must be asserted against the leakage linter.
2. The harness consumes them directly when present and keeps the differencing path as a documented
   fallback for older datasets.
3. Accept that this **moves `data_digest`** and re-pin every golden digest in one commit, with:
   - a `schema_version` increment recorded in `manifest.json`,
   - the superseded digests recorded here and in `docs/realism/PROGRESS.md`,
   - all pinned test digests updated in the same change so the suite is never left red.

## Rationale

The determinism contract exists to make *unintended* change detectable — "same seed + config →
byte-identical output". A deliberate, documented, versioned schema change does not weaken that
property; it changes the value the property is asserted against. Refusing ever to move the digest
would freeze the dataset schema permanently, which is a much larger cost than one re-pin.

The alternative — keep differencing position — was measured and rejected. It leaves a HARD gate
failing on every engine for reasons that are not the simulator's behaviour, which is worse than
useless: it trains readers to ignore a red gate.

Doing this **together with the Phase 2 channel work** means one re-pin rather than two, since Phase 2
also lands in `run.py`.

## Consequences

- Pre-bump reference values, for the record: default-config golden digest `04ae9736f519…`;
  reference run (`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15
  --traffic-lights --seed 42`) `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48`,
  593 vehicles / 7823 reports / 152 revoked / precision 0.599 / recall 0.91.
- Datasets generated before the bump remain readable; the harness branches on field presence.
- The `accel_within_hard_bound_frac` gate becomes meaningful: any residual failure after this is
  genuine simulator dynamics. The Python engine already shows a real one — 19 of 32,585 samples
  outside [−8, +4] m/s², all at exactly 1.0 s sampling — which is a true finding to fix on its
  merits, not a measurement artifact.
- Risk: `run.py` is determinism-critical. The change must add no RNG draws and no reordering; it
  only writes two additional already-known values into an oracle record.
