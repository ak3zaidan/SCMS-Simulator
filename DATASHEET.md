# Datasheet — SCMS Global Misbehavior-Authority Dataset (work in progress)

Following *Datasheets for Datasets* (Gebru et al.). Sections are filled as the
dataset matures; the current build is the P1 proof-of-concept.

## Motivation
- **Purpose.** Train/evaluate centralized anomaly detection from the perspective a
  real Misbehavior Authority would have: misbehaviour reports + correlation +
  certificate/CRL status + investigation/revocation outcomes — not raw per-vehicle
  BSMs. Fills the gap left by VeReMi/MisbehaviorX (per-vehicle) and DARE (reports only).

## Composition
- **MA-visible tables** (features): `ma_reports`, `ma_evidence_messages`,
  `ma_cert_status`, `ma_investigations`, `ma_crl_events` (+ authorized resolution outputs).
- **Ground-truth tables** (labels/eval, ORACLE-only): `gt_vehicle`, `gt_identity_map`,
  `gt_kinematics`, `gt_attacks`, `gt_report_labels`, `gt_linkage_revocation`, causal chain.
- Identity in MA-visible data appears **only** as IEEE 1609.2 HashedId8 cert digests.

## Collection / generation
- Synthetic, from Eclipse MOSAIC + SUMO + VeReMi NextGen mobility with a new SCMS
  back-end (current PoC: Python reference pipeline). Reports shaped to ETSI TS 103 759;
  linkage values per CAMP SCP2; certs per IEEE 1609.2.

## Label leakage prevention
- Features come only from MA-visible tables; labels only from ground-truth tables,
  joined offline. A **build-breaking leakage linter** rejects any ground-truth field
  (real id, true kinematics, attack label, F2MD-style `senderRealId`) in a feature table.

## Splits
- Grouped, leakage-safe: vehicle-, route-, time-, scenario-, and attack-disjoint;
  plus an open-set "unseen maps/attacks" generalization split (design doc §20).

## Reproducibility
- Every build ships `manifest.json` (seed, config, tool/commit versions, per-file
  SHA-256, data digest). Same seed → byte-identical data (verified in CI).

## Distribution / license
- Code Apache-2.0; data **CC-BY-4.0**; Zenodo DOI planned (VeReMi-family convention).
