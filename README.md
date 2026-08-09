# SCMS-Simulator

An **SCMS-aware V2X simulation & global Misbehavior-Authority (MA) dataset framework.**
It models the full Security Credential Management System credential lifecycle
(enrollment → pseudonym provisioning → signed BSM/CAM → local detection →
misbehaviour reporting → MA correlation → two-Linkage-Authority identity
resolution → revocation → CRL → enforcement) and generates a dataset from the
**global Misbehavior-Authority perspective** — with strict separation between
MA-visible data (features) and simulation ground truth (labels).

> **Why this exists.** Existing datasets (VeReMi family, MisbehaviorX) are
> per-vehicle BSM observations. Only **DARE** takes an MA perspective, and it is
> just a bag of reports — no correlation, no pseudonym→identity linkage, no CRL,
> no revocation, no trust boundaries, and its reports leak true identities. This
> project closes the full loop with correct SCMS privacy separation and a
> build-breaking leakage firewall. See the design document for the full rationale.

**Design document (all 27 planning deliverables, diagrams, decision matrix):**
<https://claude.ai/code/artifact/9a391e86-95f0-421a-aadd-f2459ddb93f3>

## Status

| Layer | State |
|---|---|
| SCMS crypto core — linkage values, CRL matching (CAMP SCP2) | ✅ implemented + tested |
| Deterministic signing (Ed25519) + IEEE 1609.2 HashedId8 | ✅ implemented |
| Trust-separated schemas (MA-visible / ground-truth) | ✅ implemented |
| Build-breaking leakage linter | ✅ implemented + tested |
| Closed-loop reference pipeline (attack→…→CRL→enforce) | ✅ runs + tested |
| Determinism (same seed → byte-identical data) | ✅ verified |
| MOSAIC + SUMO + VeReMi NextGen integration (Java) | ⛔ blocked — needs toolchain (see below) |

This is the **P1 vertical-slice core** implemented in Python. The mobility/radio
layer is currently abstracted; it is replaced by **Eclipse MOSAIC + SUMO +
VeReMi NextGen** once the toolchain is installed. See
[`docs/adr/0001-stack-and-approach.md`](docs/adr/0001-stack-and-approach.md).

## Quick start

```powershell
python -m pip install -r requirements.txt      # cryptography, pydantic, pytest
python -m pytest -q                            # 21 tests
$env:PYTHONPATH = "src"
python -m scms_sim_ref.mock_pipeline.run --out datasets/poc_run
```

The run writes `datasets/poc_run/ma/*.jsonl` (MA-visible), a **separate**
`datasets/poc_run/ground_truth/*.jsonl` (oracle-only), and `manifest.json`
(seed, config, per-file SHA-256, data digest, standards profile).

## Layout

```
src/scms_sim_ref/
  scms_core/      linkage.py (CAMP SCP2), crypto_abstract.py (Ed25519, HashedId8)
  schemas/        records.py (MA-visible vs ORACLE, forbidden-field registry)
  datagen/        leakage_linter.py (build-breaking)
  mock_pipeline/  run.py (closed-loop reference)
tests/            test_linkage.py, test_leakage.py, test_pipeline.py
docs/adr/         architecture decision records
scenarios/        scenario specs (P4)
```

## Toolchain needed for the MOSAIC layer (not yet installed here)

- **git** (repo is not yet under version control), **JDK 17**, **Maven/Gradle**
- **Eclipse SUMO 1.25**, **Eclipse MOSAIC 25.x**, (optional) **OMNeT++ 6.1 + INET 4.5.4**
- **VeReMi NextGen** generator (Apache-2.0), vendored at a pinned commit

## Licensing

Code: **Apache-2.0**. Generated datasets are intended for release under
**CC-BY-4.0** (matching the VeReMi family). The MOSAIC/SUMO/NextGen dependencies
are EPL-2.0 / Apache-2.0 — deliberately chosen so this project stays Apache-2.0
(Veins/Artery/F2MD are GPL and were rejected as the base for that reason). See the
design document's licensing section.
