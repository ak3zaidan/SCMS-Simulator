# Datasheet — SCMS Global Misbehavior-Authority Dataset

Following *Datasheets for Datasets* (Gebru et al.). This root file is a **stable overview
of the dataset family**. Every generated run also ships its own
`datasets/<run>/DATASHEET.md` with the concrete, reproducible figures (composition,
feature distributions, revocation precision/recall, realism scorecard, and baseline ML
benchmark) for that exact seed + config — **that per-run file is the source of truth**.
Regenerate it any time with:

```powershell
python -m scms_sim_ref.datagen.datasheet datasets/<run>   # writes datasets/<run>/DATASHEET.md
```

(the one-command runner `run.ps1` does this automatically).

## Motivation
- **Purpose.** Train/evaluate centralized anomaly detection from the perspective a real
  Misbehavior Authority (MA) would have: misbehaviour **reports** + correlation +
  certificate/CRL status + investigation/revocation outcomes — not raw per-vehicle BSMs.
  It complements per-vehicle corpora (VeReMi / VeReMi NextGen) and report-only views by
  modelling the full SCMS credential lifecycle and the MA's global decision problem.

## Collection / generation
- **Synthetic and fully deterministic.** The default generator is a built-in microscopic
  traffic simulator (`scms_sim_ref.mock_pipeline`): routed trips on a road network
  (linear / grid / ring / spider / custom node-edge map), IDM car-following, signalised
  intersections, time-of-day demand, a mixed fleet, weather, and a range-limited lossy
  radio channel, with a multi-signal detector suite feeding a windowed MA. An optional
  Eclipse MOSAIC + SUMO layer exists for higher-fidelity mobility.
- Same seed + config → **byte-identical** data (verified in CI); runs are memory-bounded
  via streaming and interruptible into a valid partial dataset.
- **Standards profile** (recorded per run in `manifest.json → standards_profile`):
  certificates profiled to **IEEE 1609.2**, linkage values to **CAMP SCP2**, and
  misbehaviour reports **shaped to ETSI TS 103 759**.

## Composition — tables actually written
Identifiers in MA-visible data appear **only** as opaque pseudonym-certificate digests;
true identities live exclusively in the ORACLE ground-truth tables.

- **MA-visible tables** (features), under `ma/`:
  `ma_reports`, `ma_cert_status`, `ma_investigations`, `ma_crl_events` (JSONL).
- **Ground-truth tables** (labels / evaluation only, ORACLE), under `ground_truth/`:
  `gt_vehicle`, `gt_identity_map`, `gt_attacks`, `gt_report_labels`,
  `gt_linkage_revocation`, `gt_emissions_sample` (JSONL).
- **ML-ready tables** (written by `--featurize`), under `ml/` as both `.parquet` and `.csv`:
  `report_features`/`report_labels`, `subject_features`/`subject_labels`,
  `vehicle_features`/`vehicle_labels` (oracle-linked, an optimistic upper bound),
  `vehicle_features_ma`/`vehicle_labels_ma` (MA-realistic co-revocation linkage),
  `graph_edges` (the directed reporter→subject report graph over opaque entities), and
  `subject_windows` (per-entity temporal activity windows). `ml/schema.json` labels every
  column by kind (`feature` / `fusion_feature` / `reason_flag_feature` / `label` / `id` /
  `split`) so consumers pick features without touching labels.

## Label leakage prevention
- Features and labels are written to **separate files** and joined offline. A
  **build-breaking leakage linter** rejects any ground-truth field name (real id, true
  kinematics, attack/fault label, F2MD-style `senderRealId`) appearing in a feature table.
- `tools/verify_data.py` re-audits every dataset for privacy/leakage, referential
  integrity, label correctness, count reconciliation, split integrity, graph integrity,
  schema/benchmark consistency, and per-file cryptographic digests.

## Splits
Leakage-safe and **grouped by true vehicle** — no vehicle's reports span splits:
- **`split` ∈ {train, val, test}** — a deterministic hash of the *true vehicle id*
  (≈ 70 / 15 / 15). Vehicle-disjoint by construction.
- **`time_split` ∈ {train, test}** (vehicle tables) — a **forward-in-time** split placing
  each entity by its first report time; the latest ≈ 30% form the test set (the deployment
  question: detect misbehaviour in the future after training on the past).

There are **no** separate route- / scenario- / attack-disjoint dataset partitions. Instead
the shipped benchmark evaluates generalization *as protocols over these splits/labels*:
leave-one-attack-family-out (novel-attack), forward-in-time, and — for multi-condition
campaign corpora that carry a `domain_id` — leave-one-domain-out.

## Measured realism, and known unrealisms
Every dataset's own `DATASHEET.md` carries a **measured realism scorecard**
(`datagen.realism_bench`): 14 traffic + 7 comm metrics scored against reference summaries pinned
with citations in `datagen/refdata/`. Metrics are split by severity — **HARD** metrics are physical
plausibility (acceleration inside [−8, +4] m/s², zero teleports, zero overlapping vehicles) plus one
**liveness** gate (at least half the fleet actually moves — every other hard gate is an
impossibility check that a frozen dataset passes trivially), and are the CI gate; everything else
warns. A metric the dataset cannot support reports `na` **with a machine-readable reason**, never a
guess — including when its own sample size is below the floor.

Score any dataset yourself:

```bash
python -m scms_sim_ref.datagen.realism_bench <dataset_dir> --markdown --fail-on-hard
```

Measured hard failures are printed in each datasheet's "Realism benchmark" section rather than
hidden. As of the Phase-0/1 baseline (`docs/realism/PROGRESS.md`) the standing ones are:

- **Acceleration plausibility** — ~0.2–0.5 % of finite-difference accelerations fall outside
  [−8, +4] m/s² on both engines.
- **Vehicle overlap** — the pure-Python engine has **no collision detection**; distinct vehicles can
  occupy the same point. The MOSAIC/SUMO path scores 0 (SUMO enforces separation).
- **Fundamental diagram** — capacity misses the 1800–2400 veh/h/lane anchor from both sides. It is a
  space-time *cell* approximation (the emission schema carries no edge id): cells are directional
  and the per-lane divisor is measured from the lateral spread inside each cell, but parallel lanes
  of the same carriageway inside one cell are still only estimated. Treat it as a tracked trend, not
  an absolute.
- **Comm panel** — the PDR curve is *proportional* to PDR (reconstructed from honest report links
  normalised at the nearest band), not an absolute PDR: there is no per-reception observable (RSSI,
  delivered-vs-attempted) in the schema yet. Effective range is tracked without a pass/fail band.
- **PDR gray zone** — both engines still ship a hard-cutoff (unit-disc / range-threshold) radio, so
  the 90 %→20 % band is a few tens of metres against the ≥ 100 m gate. That failure is the intended
  Phase-0 baseline: level crossings are read off the non-increasing majorant of the measured curve
  precisely so a step-function radio cannot pass on Poisson noise in one distance bin.
- **Single radio stack, no real RF** — one analytic/SNS channel model, no measured interference, no
  hardware-in-the-loop. Sim-to-real transfer must be argued, not assumed.
- Reference bands for speed are **coarse envelopes** (posted-limit / corpus-provenance derived), not
  measured percentile tables; licence-gated corpora are pinned as explicit `available: false`
  placeholders rather than invented numbers.

## Reproducibility
- Every build ships `manifest.json`: seed, full config, generator + schema versions,
  per-file SHA-256, an aggregate data digest, and the standards profile. Same seed + config
  → byte-identical data (CI-verified).
- MOSAIC/SUMO datasets additionally ship `scenario_provenance.json` — the effective `SCMS_*`
  environment, the resolved realism knobs (sync period, car-following model, speed-factor
  distribution, OD mode, RSU count) and a SHA-256 per scenario input file — and the Java manifest
  inlines the same input hashes under `inputs`, so a MOSAIC run is replayable from the dataset alone.

## Distribution / license
- Code: **Apache-2.0** (see `LICENSE`). Redistribute generated datasets under the terms you
  choose; because everything is deterministic, a dataset can also be reproduced from its
  `manifest.json` rather than redistributed.
