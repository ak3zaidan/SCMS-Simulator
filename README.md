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
| Toolchain: JDK 17 / SUMO 1.25 / MOSAIC 25.2 / git | ✅ installed under `C:\Users\Administrator\tools` |
| VeReMi NextGen vendored (pinned submodule) | ✅ `third_party/veremi-nextgen` @ `acda994b` |
| SCMS closed loop in MOSAIC (sign → detect → report → correlate → 2-LA resolve → revoke → CRL → enforce) | ✅ real ITS-G5 AdHoc; distributed detection |
| Runs on the real InTAS (Ingolstadt) map | ✅ `run.ps1 -Scenario intas` |
| Attacks / detectors | ✅ ConstPos + ConstPosOffset / frozen-position + ART |
| ML featurizer + leakage-safe splitter | ✅ report + subject tables, vehicle-disjoint splits |
| One-command end-to-end runner | ✅ `run.ps1` |

The system is runnable end-to-end. On the real InTAS Ingolstadt map the generated
dataset is **leakage-free**, with revocation **precision 1.0 / recall 0.986**, and is
**byte-identical across runs** (deterministic). See
[`docs/adr/0001-stack-and-approach.md`](docs/adr/0001-stack-and-approach.md).

## Run the full simulation (one command)

```powershell
. C:\Users\Administrator\tools\env.ps1     # once per shell (JAVA_HOME/SUMO_HOME/MOSAIC_HOME/PATH)
.\run.ps1                                   # 'smoke' highway scenario (~50 vehicles, seconds)
.\run.ps1 -Scenario intas                   # real Ingolstadt (InTAS) map (~333 vehicles)
.\run.ps1 -Scenario intas -Duration 600s -Seed 42
```

`run.ps1` builds the MOSAIC app, generates the scenario, simulates in MOSAIC + SUMO,
featurizes the output into ML-ready tables, and validates it (leakage + precision/recall).
The dataset lands in `datasets/<name>/`:

- `ma/` — Misbehavior-Authority-visible events (the only source of ML features)
- `ground_truth/` — oracle labels (never used as features)
- `ml/` — `report_features`/`report_labels` + `subject_features`/`subject_labels`
  (Parquet + CSV) with vehicle-disjoint `train`/`val`/`test` splits
- `manifest.json` — seed, config, per-file SHA-256, and a dataset digest

## SCMS entities (all 14 modeled)

All 14 SCMS entities from the design (§7) are modeled as distinct modules with real trust
boundaries in [`org.scms.entities.Scms`](scms-sim/mosaic-apps/scms-app/src/main/java/org/scms/entities/Scms.java):

- **Trust anchors (offline):** Root CA · Intermediate CA · Electors · Policy Generator
- **Enrollment:** Device Configuration Manager · Enrollment CA
- **Provisioning:** Registration Authority · Pseudonym CA · Linkage Authority 1 & 2
- **Enforcement:** Misbehavior Authority · CRL Generator · CRL Store/Broadcast
- **Privacy:** Location Obscurer Proxy

The boundary is **structural, not conventional**: the Misbehavior Authority never holds a
true identity — it resolves a suspect via PCA → LA1/LA2 (forward linkage seeds) → RA, and the
**RA is the only entity that maps a provisioning request to an enrollment identity**. True
identities exist only in `ground_truth/` and are never used as features. The Python reference
(`src/scms_sim_ref/mock_pipeline/run.py`) mirrors the operational entities.

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

## MOSAIC layer — build & run the custom app

The toolchain lives under `C:\Users\Administrator\tools` (JDK 17, SUMO 1.25.0,
MOSAIC 25.2). Activate it, build our app with `javac` (no Maven), generate the
scenario, and run:

```powershell
. C:\Users\Administrator\tools\env.ps1                       # JAVA_HOME, SUMO_HOME, MOSAIC_HOME, PATH
.\scms-sim\mosaic-apps\scms-app\build.ps1                    # -> ScmsApp-0.1.0.jar (javac + jar)
.\scms-sim\scenarios\make_scms_smoke.ps1                     # derive scms_smoke from MOSAIC HelloWorld + wire our app
cd $env:MOSAIC_HOME
.\mosaic.bat -c C:\Users\Administrator\SCMS-Simulator\scms-sim\scenarios\scms_smoke\scenario_config.json -w 0
# app logs: $env:MOSAIC_HOME\logs\log-*-scms_smoke\...  (grep "SCMS beacon app")
```

`mosaic.bat` builds a relative classpath, so always run it with the MOSAIC bundle
as the working directory. `ScmsBeaconApp` receives SUMO-driven vehicle updates —
the hook point for signing, detection, and reporting.

## Licensing

Code: **Apache-2.0**. Generated datasets are intended for release under
**CC-BY-4.0** (matching the VeReMi family). The MOSAIC/SUMO/NextGen dependencies
are EPL-2.0 / Apache-2.0 — deliberately chosen so this project stays Apache-2.0
(Veins/Artery/F2MD are GPL and were rejected as the base for that reason). See the
design document's licensing section.
