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
| Custom MOSAIC app (ScmsBeaconApp) build + run | ✅ `javac` build; runs in MOSAIC 25.2 + SUMO 1.25 |
| Wiring SCMS (signing / detection / reporting / back-end) into MOSAIC | ⏳ next |

The Python **P1 vertical-slice core** is complete and tested. The toolchain is
installed and a custom MOSAIC application (`ScmsBeaconApp`) has been built and run
end-to-end in MOSAIC + SUMO — establishing the extension seam. The SCMS logic
(signing, local detection, report emission, back-end federate) is being wired into
that MOSAIC app next, mirroring the validated Python reference. See
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
