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

## Measured realism
Every dataset's own `DATASHEET.md` carries a **measured realism scorecard**
(`datagen.realism_bench`): 22 metrics (15 traffic + 7 comm) scored against reference summaries pinned
with citations in `datagen/refdata/` (10 sets / 98 entries at the time of writing; the loader reads
whatever is on disk). Metrics are split by severity — **HARD**
metrics are physical plausibility (acceleration inside [−8, +4] m/s², zero teleports, zero
overlapping vehicles) plus one **liveness** gate (at least half the fleet actually moves — every
other hard gate is an impossibility check that a frozen dataset passes trivially), and are the CI
gate; everything else warns. A metric the dataset cannot support reports `na` **with a
machine-readable reason**, never a guess — including when its own sample size is below the floor, and
including when the emission trace is too sparse to finite-difference honestly.

Score any dataset yourself:

```bash
python -m scms_sim_ref.datagen.realism_bench <dataset_dir> --markdown --fail-on-hard
```

Measured hard failures are printed in each datasheet's "Realism benchmark" section rather than
hidden.

**Reading the numbers.** Kinematic metrics are only comparable across datasets that used the same
estimator, so the scorecard names it: `kinematics_source` (and each metric's
`details.kinematics_source`) says whether speed/heading came from the ground-truth record
(`true_speed` / `true_heading`, ADR 0002) or were reconstructed by differencing `true_x`/`true_y`.
Finite differences are taken only across sample pairs no wider than 2 s **and normalised over exactly
that subset**; anything sparser is `na`. Numbers quoted below are from **full emission traces**
(`emit_sample_prob = 1.0`) — a sub-sampled run does not score these metrics at all.

## Known unrealisms

Measured, current as of the Phase-0/1 baseline (`docs/realism/PROGRESS.md`), not aspirational:

1. **No collision detection in the Python engine.** `mock_pipeline` never resolves vehicle-vehicle
   conflicts, so distinct vehicles can occupy the same point: **290 overlapping pairs** (< 1 m apart
   at an identical timestamp) on the reference 300 s `--flow --road grid --grid 6` run at
   `emit_sample_prob = 1.0` — a HARD failure of `traffic.overlap_events`. The MOSAIC/SUMO path scores
   **0** (SUMO enforces separation). Any model trained on Python-engine mobility must not treat
   spatial exclusivity as a learnable invariant.
2. **Both radios are hard cutoffs; the geometric channel is Phase 2, not shipped.** Reception is a
   unit-disc / range-threshold model, so the 90 %→20 % PDR gray zone measures **18–81 m** across runs
   against a ≥ 100 m reference gate — Python 81 m, MOSAIC smoke 32 m, InTAS full traces 43–48 m. This
   failure is
   *by construction* and is the intended baseline: level crossings are read off the non-increasing
   majorant of the measured curve precisely so a step-function radio cannot pass on Poisson noise in
   one distance bin. 3GPP TR 37.885 path loss, correlated shadowing, NLOSv/NLOSb blockage, Nakagami
   fading, a closed-form 802.11p PDR/CBR model, ETSI DCC and an `rssi_dbm` observable are the Phase-2
   design (`docs/realism/PHASE2-DESIGN.md`) and are **not in any dataset yet**.
3. **Real-world traffic (GEH) validation is BLOCKED — not met, and not failed.** No measured
   induction-loop counts exist anywhere in the tree: all 15 vendored `InTAS_Detectors_Output.xml`
   files are config-echo stubs with zero `<interval>` rows, and the InTAS route files are *demand*
   (model input), so grading counts against them is circular. `tools/sumo_realism.py --ref-det-out`
   therefore produces a **seed-stability** check — one SUMO run against another SUMO run of the same
   scenario, gate ids `seed_stability.*`, thresholds derived in-tool from the exact conditional null
   rather than borrowed from FHWA. It measures the simulator's reproducibility against itself and
   says **nothing** about resemblance to real traffic. Do not quote "GEH < 5 on ≥ 85 % of InTAS loop
   stations". Supplying measured counts via `--ref-counts` is what unblocks it (schema:
   `src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md`).
4. **Lane changes are still discontinuous on the SUMO path.** With the sublane model on
   (`--lateral-resolution 0.8`, default) the best run measures **0.1302** lane-change teleports per
   vehicle-km against a 0.0 target; with it off, 0.5851 on the same seed and scenario. A 3.2 m
   instantaneous lateral jump is exactly the signature a position-plausibility detector keys on, so
   this is directly load-bearing for the misbehaviour labels. The pure-Python engine has no lanes and
   reads 0.0.
5. **Acceleration plausibility is not 100 %.** After every artefact class is screened, 0.058 %
   (Python, `accel_within_hard_bound_frac` 0.999417) to 0.126–0.177 % (InTAS runs, 0.998744–0.998233)
   of samples still fall outside [−8, +4] m/s². The Python residual is 19 of 32 585 samples, **all at
   exactly 1.0 s sampling** — so it cannot be small-interval differencing noise; it is the engine's
   own dynamics, and `mock_pipeline/` is frozen by the digest invariant. The InTAS residual is
   single-sample *longitudinal* position remaps at junction/edge transitions (median |d_lat| across
   the pair is 0.001 m, so it is not lateral and the sublane model cannot fix it), corroborated
   independently by reading SUMO's own `--fcd-output` speed with no position differencing at all
   (0.9999 within band, `accel_min` −9.0 m/s² over 781 091 samples / 334 vehicles). Note the size of
   this miss was overstated ~4× before the estimator was corrected — a 3.2 m one-sample lane-change
   snap used to read as a 33 m/s longitudinal speed and a −275 m/s² acceleration.
6. **Fundamental diagram** — capacity misses the 1800–2400 veh/h/lane anchor from both sides (517 on
   the Python grid, 2759 on the MOSAIC highway). It is a space-time *cell* approximation (the emission
   schema carries no edge id): cells are directional and the per-lane divisor is measured from the
   lateral spread inside each cell, but parallel lanes of the same carriageway inside one cell are
   still only estimated. Treat it as a tracked trend, not an absolute.
7. **The comm panel is proportional to PDR, not absolute PDR** — reconstructed from honest report
   links normalised at the nearest populated band, because no per-reception observable (RSSI,
   delivered-vs-attempted) exists in the schema yet. Effective range is tracked with no pass/fail
   band.
8. **Single radio stack, no real RF** — one analytic/SNS channel model, no measured interference, no
   hardware-in-the-loop. Sim-to-real transfer must be argued, not assumed.
9. **Reference bands for speed are coarse envelopes** (posted-limit / corpus-provenance derived), not
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
