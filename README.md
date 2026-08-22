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

## Quick start (pure Python — no MOSAIC toolchain needed)

```powershell
python -m pip install -r requirements.txt      # cryptography, pydantic, pytest
python -m pytest -q                            # full test suite
$env:PYTHONPATH = "src"
python -m scms_sim_ref.mock_pipeline.run --out datasets/poc_run   # small reference run
```

The built-in generator is a full microscopic traffic simulator: routed trips on a road network
(grid, ring, radial spider city, or a fully **custom node/edge map** via `--road custom
--custom-network map.json`) with IDM car-following (queues/congestion), signalized intersections,
time-of-day demand, a mixed fleet (car/moto/truck/bus), weather, range-limited lossy radio,
21 attack types across 8 families, and a 13-signal detector suite with a windowed
Misbehavior-Authority. A JSON `--events` timeline adds deterministic mid-run dynamics: demand
surges, weather fronts, road closures, and coordinated attack waves. In the GUI, the AI Copilot
designs custom maps and timelines from plain language. Deterministic (same seed + config →
byte-identical data) and memory-bounded via streaming.

```powershell
# long-running routed traffic-flow simulation (spawn/despawn over time)
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --grid 8 --duration 1800 `
    --arrival-rate 2 --attacker-pct 0.15 --collude-pct 0.3 --traffic-lights --demand rush `
    --featurize --out datasets/long_run
# a multi-domain training corpus (scenario x permutation, merged with domain_id). grids: quick|medium|full
python -m scms_sim_ref.datagen.massive --grid medium --flow --out datasets/massive   # --dry-run to preview size

# one-command named scenario (flags still override); reproduce any past run byte-for-byte from its manifest
python -m scms_sim_ref.mock_pipeline.run --preset urban_rush --featurize --out datasets/urban
python -m scms_sim_ref.mock_pipeline.run --config datasets/urban/manifest.json --out datasets/replay
```

Presets: `urban_rush`, `highway`, `night_rain`, `gridlock`, `stealth_hard`. Other realism knobs:
`--od-model gravity` (distance-decay trip lengths), `--turn-slowdown` (slow into corners),
`--attack-duty-cycle 0.3` (pulsed/intermittent attackers), `--attack-delay-jitter 20` (varied onset),
`--boundary-origins` (trips enter at the network edge), `--n-rsus 12` (fixed always-trusted Road-Side
Units — infrastructure-assisted detection that lifts recall in sparse traffic). Long runs are
interruptible — Ctrl-C finalizes a valid partial dataset.

### Full control (RSUs / traffic / network)

Every simulation parameter is directly controllable. Highlights:

```powershell
# RSUs: how many, where, and how far they hear (or place them by hand)
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --n-rsus 12 `
    --rsu-placement perimeter --rsu-range 350 --out datasets/rsu_demo
python -m ... --flow --road grid --rsu-coords "300,300;600,300;300,600" --out datasets/rsu_manual

# Traffic: custom fleet composition, speeds, and car-following (IDM) dynamics
python -m ... --flow --road grid --fleet-mix "car:0.6,truck:0.3,bus:0.1" `
    --trip-speed-min 10 --trip-speed-max 22 --idm-accel 1.2 --idm-time-headway 1.6

# Network: topology (grid / ring / linear), non-square grid, block spacing, lanes, lane width, signals
python -m ... --flow --road grid --grid 10 --grid-h 4 --grid-block 160 `
    --lanes 3 --lane-width 3.25 --traffic-lights --light-cycle 30
python -m ... --flow --road ring --grid 24 --grid-block 120   # circular beltway of 24 intersections

# Attacks: control the scenario composition (per-type weights)
python -m ... --flow --road grid --attacker-pct 0.2 --attack-mix "ConstPos:0.5,Sybil:0.3,SlowDrift:0.2"
```

`--rsu-placement` ∈ {spread, perimeter, center, corners, all}. The GUI mirrors all of this, and its
**"⚙ Advanced: all fields"** panel exposes *every* config field (runs the exact config via replay).
`--dump-config-schema out.json` lists the full field surface; `--check-config cfg.json` validates one.

Each run writes `ma/*.jsonl` (MA-visible features), a **separate** `ground_truth/*.jsonl`
(oracle-only labels), `ml/*` (train/val/test ML tables via `--featurize`), a `DATASHEET.md`, and
`manifest.json` (seed, config, per-file SHA-256, data digest, standards profile).

## GUI control panel

```powershell
.\gui.ps1      # http://127.0.0.1:8710 — pick the "python-flow" generator (default), tweak/preset, Start
```

A dependency-free web panel: choose the generator, use one-click presets (urban rush / highway /
sparse night / gridlock), watch the live congestion map, and read the full results dashboard
(precision/recall, per-task ROC-AUC with GBDT + CIs, calibration, generalization).

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
