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

## Reproducibility
- Every build ships `manifest.json`: seed, full config, generator + schema versions,
  per-file SHA-256, an aggregate data digest, and the standards profile. Same seed + config
  → byte-identical data (CI-verified).

## Distribution / license
- Code: **Apache-2.0** (see `LICENSE`). Redistribute generated datasets under the terms you
  choose; because everything is deterministic, a dataset can also be reproduced from its
  `manifest.json` rather than redistributed.
