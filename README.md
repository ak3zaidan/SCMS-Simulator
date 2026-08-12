# SCMS-Simulator

An **SCMS-aware V2X simulation & global Misbehavior-Authority (MA) dataset framework.**
It models the full Security Credential Management System credential lifecycle
(enrollment → pseudonym provisioning → signed BSM/CAM → local detection →
misbehaviour reporting → MA correlation → two-Linkage-Authority identity
resolution → revocation → CRL → enforcement) and generates a dataset from the
**global Misbehavior-Authority perspective** — with strict separation between
MA-visible data (features) and simulation ground truth (labels).

## Run the full simulation (one command)

```powershell
. C:\Users\Administrator\tools\env.ps1     # once per shell (JAVA_HOME/SUMO_HOME/MOSAIC_HOME/PATH)
.\run.ps1                                   # 'smoke' highway scenario (~50 vehicles, seconds)
.\run.ps1 -Scenario intas_urban_rush        # real Ingolstadt (InTAS) map, 7-9am rush (~333 vehicles)
.\run.ps1 -Scenario highway -MaxVehicles 200 -Lanes 3     # flow map: control number of cars
.\run.ps1 -Scenario intas_highway_low -Scale 0.4 -Seed 42 # route map: control density
```

```powershell
.\run.ps1 -Scenario grid_8x8                 # procedural grid network
.\run.ps1 -Scenario spider_10a5c -Scale 1.5  # procedural spider network
.\run.ps1 -Scenario osm_tokyo                # real Tokyo streets (OpenStreetMap)
```

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
