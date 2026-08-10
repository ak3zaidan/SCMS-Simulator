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
