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

## Landed (Python engine, 2026-08-30)

`src/scms_sim_ref/mock_pipeline/run.py`:

- `gt_emissions_sample.jsonl` rows now carry `true_speed` (m/s) and `true_heading` (deg), written
  verbatim from the `tx.true_state(t)` tuple that the broadcast pre-pass already computed
  (`run.py:2553`); they ride to the sampler on the broadcast dict as `tspd`/`thdg`
  (`run.py:2618-2621`) and are written at `run.py:2664-2677`. No RNG draw, no reordering, no
  recomputation. Both names are already in `records.FORBIDDEN_FEATURE_KEYS`, so the leakage linter
  blocks them from any feature table with no change to `schemas/records.py`.
- `manifest.schema_versions.ground_truth` 1 → **2** (`ma_visible` stays 1 — no MA-visible field
  moved). The manifest also gained a `conventions` block, `{"heading": "deg_ccw_from_east",
  "speed": "m_s", "position": "m_local_xy"}`. The manifest is excluded from `data_digest`, so that
  block is digest-neutral.
- New `tests/test_gt_kinematics.py` (10 tests) pins the four promised properties: completeness (every
  sample of every vehicle), truth (`true_speed == claimed_speed` to the last recorded digit on an
  all-honest run), the heading convention, and the ORACLE firewall (`assert_ma_visible` rejects an
  emission row; no `ma/*.jsonl` row carries either name).
- End-to-end confirmation from the consumer side: `realism_bench` on the new reference dataset
  accepts both fields on **270/270 tracks** (`used: true`) and auto-detects the convention with
  `convention_residuals_deg = {deg_ccw_from_east: 0.0, deg_cw_from_north: 90.0,
  rad_ccw_from_east: 53.24, rad_cw_from_north: 90.0}` — i.e. it independently derives exactly what
  `manifest.conventions.heading` declares. Every kinematic metric now reports
  `kinematics_source: "ground truth true_speed"` / `"ground truth true_heading
  (deg_ccw_from_east)"` instead of a differenced reconstruction. (Scorecard on that run, emit_p
  0.03: 9 pass / 0 fail / 13 na.)
- `tools/verify_data.py` was run over three freshly generated schema-v2 datasets (the reference run
  twice plus the seed-43 control): **0 failures**, with `L1_feature_cols_clean`,
  `L2_ma_no_forbidden_keys`, `L3_no_true_id_in_ma`, `I1_file_digests_match_manifest`,
  `I2_aggregate_data_digest` and `V4_emissions_truth` all passing 3/3. The audit needed no change
  for the bump.
- Measured on an all-honest full-trace run (seed 7, grid 5×5, 60 s, `emit_sample_prob=1.0`,
  1925 samples / 72 vehicles / 1853 consecutive 1.0 s pairs), which quantifies what the field buys:
  `|chord_speed − true_speed|` has median **0.0000 m/s**, p95 **0.0010 m/s** — and max
  **12.7350 m/s**. The distribution is a spike at zero with a fat tail at turns and lane offsets, so
  differencing looks fine in aggregate and is catastrophically wrong exactly where the acceleration
  gate reads it. Bearing error against `true_heading` read as CCW-from-East: median **0.0000°**
  (p95 0.0000°, n=1835); read as CW-from-North: median **90.0000°**.

### Heading convention — divergence recorded, not resolved

The Python engine's heading is the math convention: `degrees(atan2(vy, vx)) % 360`, i.e. degrees
**counter-clockwise from +x/East**, `[0, 360)` (`run.py:667`, and the detector bearing at
`run.py:2823` matches it). The MOSAIC/Java engine writes the SUMO/ETSI convention (degrees clockwise
from North). `true_heading` therefore means different things in the two datasets. Rotating the
Python engine's convention would move every `claimed_heading`, every `HeadingOffset` attack claim and
the heading-consistency detector — a behavioural change, not a schema change, and out of scope for
this ADR. Instead the convention is now **declared** in `manifest.conventions.heading`, and the
realism harness detects it per-track (`realism_bench.py:399-500`). Unifying the two engines on
`deg_cw_from_north` is left as a follow-up with its own digest move.

### Digest re-pin (measured, this tree)

Verification that the move is exactly the schema change and nothing else: for all eight datasets
below the post-change dataset was rewritten with `true_speed`/`true_heading` stripped from every
emission row and re-hashed with `run._data_digest`'s exact rule — every one reproduced its pre-bump
digest **byte-for-byte**, so no other file, row order or RNG draw moved. Every emission row in every
dataset carried both fields (e.g. 1041/1041 on the reference run, 210/210 on the multilane run).

| Config (pinned in) | pre-bump | post-bump |
|---|---|---|
| default: seed 7, flow, grid 5×5, 60 s, 1.5/s, atk 0.25 (11 test files) | `04ae9736f519dffb426bb1acebfec95edf32e7127ebc71346279f754a69cee38` | `0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740` |
| multi-attack: seed 13, 8 attack types (`test_attack_magnitude`) | `8894cb268af3ab4f0833a371f34808539fb0521a565727aea8037e427693f63c` | `48013901c241b400e2bfaf02d393ab6e0026df8a23d20e139dec6693f2726e5d` |
| collusion+RSU+logdistance: seed 11 (`test_config_knobs`) | `53abb36711ae0e55ef10f754bb5086d87109af96237eda60916c1688091806f3` | `939b4faa726853675f81453e2891bc155e2fa18ea58cac4edbdcba865df8ae2c` |
| VRU+DENM: seed 13 (`test_config_knobs`) | `4628e01edeb30212f9da85a1cd3a5742d60eac5be2e469b1023acea1973144d1` | `b3a01d40354c838ffc04c10dcdebf60cf8652b0b67ef694007128011c6d11d46` |
| grid + traffic lights (`test_gap_acceptance`, `test_network_fidelity`) | `b0bae9e4fc04a5f5d43e4b8ab2714d23246bfcb502a27a4ed7646c3deb01a0b8` | `fe1a58002f468b3124aa24fc26681fb9e69bb63df71e543650289e2034f699e6` |
| multilane 6×6 ×3 lanes, lane_changes off (`test_lane_changes`) | `38845a32f35ee5c9cf148ad532b2c5e900322140c197cce0f03f62a7acd5a858` | `0a9e82ec549f876843ba39cce241ab28fdb39ea94900151d511e64e8c93f2277` |
| ring, 8 blocks (`test_network_fidelity`, ×2) | `ff1cddd82227d7aaa3b6d5f931ef0ee9313257cb1af7eb14752eda5aed4a989b` | `32133dd19efd90b280b7e6ea13e4dda6b23f33f605344c1fe3d740d95ea5da50` |
| reference run `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42` (docs only) | `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48` | `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` |

The MOSAIC/Java engine's own re-pin, landed in the same change set:
`gen_smoke` reference dataset `b1789aeb90a21d07…` → `c7efff8075ddba47…` (no test pins it today).

`test_network_fidelity.py:55` pins `460b4cd04b0be927…` in an **inequality** (the pre-`node_phase`
ring-lights noise value) and needed no update; the ring-lights digest is now
`205a0856f04c36e852fb6154b17624f4340909e3e30b7d768de7e366bbc8c8e2`.

### Reproducibility re-verified after the bump

- Reference run, seed 42, run twice into different output directories → `b25f2137cf14dd50…` both
  times; 593 vehicles / 7823 reports / 152 revoked / precision 0.599 / recall 0.91 / latency_med
  4.0 s — every count and metric identical to the pre-bump run, so the engine's behaviour did not
  change, only the record it writes.
- Same config, seed 43 → `0c54e935b11979ce5002efd2d229847ad6936bfab2fb578105817b77ca34515d`
  (599 vehicles / 6585 reports / 139 revoked): a different seed still yields a different digest.
- All seven re-pinned configs were each run twice; every pair matched.
- Full suite: **656 passed in 725 s** (`python -m pytest -q`, exit 0) — 646 on the tree before the
  new `test_gt_kinematics.py` was added, both runs green with the re-pinned values.

### Stale artefacts left for their owners

`datasets/realism_baseline/python_flow_grid6/manifest.json` still records the pre-bump
`f0ec3cc0baa55a2f…` and its `gt_emissions_sample.jsonl` predates the fields. Regenerating it (and
the scorecards mirrored into `docs/realism/baselines/`) is the baselines owner's step — and it will
also flip the harness's `kinematics_source` from differenced positions to ground truth, which is the
point of this ADR.
