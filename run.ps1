<#
  One-command end-to-end runner: build the MOSAIC app -> generate a scenario ->
  simulate (MOSAIC + SUMO) -> featurize into ML-ready tables -> validate.

  Usage:
    . C:\Users\Administrator\tools\env.ps1     # once per shell: JAVA_HOME/SUMO_HOME/MOSAIC_HOME/PATH
    .\run.ps1                                   # 'smoke' synthetic highway
    .\run.ps1 -Scenario intas_urban_rush        # real Ingolstadt, 7-9am rush
    .\run.ps1 -Scenario highway -MaxVehicles 200 -Lanes 3
    .\run.ps1 -Scenario barnim -Scale 1.5 -Duration 300s -Seed 42
    .\run.ps1 -Scenario tiergarten -Visualize   # also open MOSAIC's live 2D map

  Scenarios: smoke, highway, barnim, tiergarten,
             intas_urban_low, intas_urban_rush, intas_highway_low, intas_highway_rush
             (or any generated map key: grid_6x6 / spider_8a4c / rand_150 / osm_manhattan)

  REALISM DEFAULTS AND THEIR COST (all set through SCMS_* environment variables, all recorded
  in <dataset>\scenario_provenance.json so a run stays replayable):

    SCMS_SYNC_MS=100        MOSAIC<->SUMO sync period. 100 ms is the default on EVERY map now,
                            including the curated + InTAS ones that used to run at 1000 ms.
                            This is what lets the ETSI EN 302 637-2 CAM rules fire between
                            1 and 10 Hz (measured: mean CAM interval 0.20 s instead of 1.0 s)
                            and gives the app a real 100 ms channel-busy window.
                            COST: ~10x the MOSAIC steps, so roughly 10x the wall-clock on the
                            big InTAS maps (a 300 s InTAS window goes from seconds to minutes).
                            Mitigate with -Duration / -Scale, or set SCMS_SYNC_MS=1000 to get
                            the old 1 Hz behaviour back.
                            NB: MOSAIC's SumoAmbassador always launches SUMO with
                            --step-length <sync/1000>, which overrides the sumocfg, so this
                            knob IS the SUMO integration step (SCMS_SIM_STEP only sets the
                            step written into a generated map's own sumocfg). The generator
                            rewrites every sumocfg to match and records the effective value as
                            resolved.sumo_step_ms in the scenario manifest.
    SCMS_CF_MODEL=eidm      SUMO car-following model (eidm|krauss|idm|acc|w99|...). EIDM is
                            SUMO's human-like extended IDM; 'krauss' restores the old default.
                            COST: EIDM is a few % slower than Krauss and uses log(), which is a
                            documented cross-platform reproducibility caveat (same-host runs
                            stay deterministic; the SUMO version is pinned in the manifest).
    SCMS_LATERAL_RES=0.8    SUMO --lateral-resolution: turns ON the SUBLANE MODEL, so a lane change
                            becomes a continuous lateral traverse instead of SUMO's default
                            single-step teleport between lane centrelines. Without it (SUMO's own
                            default, and what this repo shipped before) a vehicle jumps a whole lane
                            width (3.2 m) between two consecutive samples while its speed stays
                            flat -- physically impossible, and exactly the signature a V2X
                            position-plausibility detector keys on, so a misbehaviour dataset built
                            that way teaches the wrong lesson. It also poisons any finite-difference
                            kinematics: differencing hypot(dx,dy)/dt across that jump reads as
                            ~33 m/s and ~275 m/s^2 (28 g).
                            EFFECT (measure it yourself with tools\lateral_jump.py <dataset>...):
                            on intas_urban_low / 300 s, cross-heading steps > 1.5 m fall 1112 -> 306
                            and the one-lane-width band (2.9-3.5 m) collapses 536 -> 5; the derived
                            acceleration extreme falls 276 -> 95 m/s^2.
                            WHY 0.8 m: SUMO's default lane is 3.2 m wide, so 0.8 splits it into
                            exactly 4 equal sublanes (the SUMO manual's own worked example -- a
                            value that does not divide the lane evenly leaves a narrow leftmost
                            stripe), and 0.8 is still at/below the narrowest motorised vehicle SUMO
                            models (motorcycle 0.9 m, moped 0.8 m). It is the COARSEST value that
                            satisfies both rules, which is what keeps the cost down.
                            Enabling it also switches the lane-change model to SL2015 AUTOMATICALLY
                            (no vType edit needed); on vTypes we own we additionally set
                            maxSpeedLat and lcMaxSpeedLatStanding=0 so a stopped car cannot slide
                            sideways. InTAS/route maps keep their own vTypes and use SL2015's
                            defaults.
                            COST (measured on this box, intas_urban_low / 300 s / 0.1 s step /
                            334 vehicles). SUMO alone: 6.17 s -> 10.91 s of simulation time, i.e.
                            +77% (real-time factor 48.7 -> 27.5). Whole .\run.ps1 pipeline
                            (MOSAIC + SUMO + featurize + validate + datasheet): 74.6 s -> 79.4 s,
                            i.e. +6% -- the sublane cost is mostly hidden behind the MOSAIC/app
                            layer. Runtime scales with lane_width / resolution, so HALVING the
                            resolution roughly doubles the sublane part of the bill.
                            SCMS_LATERAL_RES=off restores the old instantaneous lane changes.
    SCMS_LATERAL_SPEED=1.0  vType maxSpeedLat (m/s) under the sublane model: how fast a vehicle may
                            move sideways. 1.0 = SUMO's default, ~3 s to cross a 3.2 m lane, inside
                            the 2-4 s a real lane change takes. Only reaches vTypes we generate
                            (procedural/OSM maps) or push through MOSAIC (curated flow maps).
    SCMS_VTYPE_SAMPLES=8    Jittered driver prototypes per fleet class on generated maps
    SCMS_VTYPE_JITTER=0.15  (=1 for one uniform type). Also drives MOSAIC's per-vehicle
                            parameter deviations on the curated flow maps.
    SCMS_SPEED_DEV=0.1      Per-driver desired-speed spread, speedFactor="normc(1,0.1,0.7,1.3)"
                            (0 = everybody drives exactly the speed limit, as before).
    SCMS_OD=gravity         Generated maps: gravity-weighted origin/destination sampling
                            (uniform = the old randomTrips behaviour).
    SCMS_DEPART_PROFILE=    Generated maps: per-interval departure-rate profile, either a name
                            (morning|evening|diurnal|peak) or a comma list like "1,3,2,1".
    SCMS_TLS=guess          Generated maps: guess/join traffic signals (off = no signals).
    SCMS_RSUS=auto          Road-side units: 'auto' places 8 once the app jar contains an RSU
                            application, a number forces it, 0 disables. SCMS_RSU_PLACEMENT
                            (junction|grid|keep) and SCMS_RSU_APP tune it.

  Usage:
    .\run.ps1 -Scenario intas_urban_rush -Duration 300s          # 100 ms sync (slow, realistic)
    $env:SCMS_SYNC_MS=1000; .\run.ps1 -Scenario intas_urban_rush # fast 1 Hz screening run
#>
param(
    # Curated maps: smoke, highway, barnim, tiergarten, intas_urban_low/rush, intas_highway_low/rush.
    # Or a generated map key: grid_6x6 / spider_8a4c / rand_150 (+_s2 seed) / osm_manhattan (see mapgen.py).
    [string]$Scenario = 'smoke',
    [string]$Duration,
    [long]$Seed,
    [string]$Scale,          # route maps: SUMO traffic-density multiplier
    [int]$MaxVehicles,       # flow maps: cap on concurrent vehicles
    [int]$TargetFlow,        # flow maps: vehicles/hour
    [int]$Lanes,             # flow maps: number of lanes used
    [string]$OutDir,         # override the dataset output directory (used by the campaign generator)
    [switch]$SkipBuild,      # skip the javac build (campaign builds once up front)
    [switch]$Visualize
)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
# PYTHONHASHSEED must be set BEFORE the interpreter starts -- CPython reads it at startup, before
# any user code, so nothing inside python can pin it for its own process. Setting it here is the
# only mechanism that works, and it reaches EVERY python and java child this script launches.
# It does not move any digest (the engine's keyed streams are sha512-seeded from string keys, hence
# PYTHONHASHSEED-independent); it pins manifest["runtime"]["hash_randomization"] and removes
# set/dict iteration order as a latent source of run-to-run variation.
$env:PYTHONHASHSEED = '0'
$repo = $PSScriptRoot

Write-Host "== [1/6] Build MOSAIC app (javac, no Maven) =="
if ($SkipBuild) { Write-Host "   (skipped)" } else { & "$repo\scms-sim\mosaic-apps\scms-app\build.ps1" }

Write-Host "== [2/6] Generate '$Scenario' scenario =="
$genArgs = @($Scenario)
$hasSeed = $PSBoundParameters.ContainsKey('Seed')   # so -Seed 0 is honoured (0 is falsy)
if ($Duration)    { $genArgs += @('--duration', $Duration) }
if ($hasSeed)     { $genArgs += @('--seed', "$Seed") }
if ($Scale)       { $genArgs += @('--scale', "$Scale") }
if ($MaxVehicles) { $genArgs += @('--max-vehicles', "$MaxVehicles") }
if ($TargetFlow)  { $genArgs += @('--target-flow', "$TargetFlow") }
if ($Lanes)       { $genArgs += @('--lanes', "$Lanes") }
# The generator logs progress (RSU placement, OSM import, OD weights) to STDERR so that STDOUT
# stays a single clean JSON line for ConvertFrom-Json. Under $ErrorActionPreference='Stop' any
# native-command stderr becomes a TERMINATING error, so the run must drop to 'Continue' here or a
# purely informational line ("8 RSU(s) -> org.scms.app.ScmsRsuApp") aborts the whole pipeline.
# The generator's real failure signal is its exit code, which is now checked explicitly.
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'
try {
    $genJson = & python "$repo\scms-sim\scenarios\gen_scenario.py" @genArgs
} finally {
    $ErrorActionPreference = $prevEAP
}
if ($LASTEXITCODE -ne 0) { throw "Scenario generation failed (gen_scenario.py exit $LASTEXITCODE)." }
$gen = $genJson | ConvertFrom-Json
$cfg = $gen.scenario_config
$out = $gen.dataset_dir
if ($OutDir) {
    # resolve relative -OutDir against the repo (the Java back-end runs with cwd = MOSAIC_HOME)
    $out = if ([System.IO.Path]::IsPathRooted($OutDir)) { $OutDir } else { Join-Path $repo $OutDir }
}
Write-Host "   $($gen.kind) scenario -> $cfg"

Write-Host "== [3/6] Simulate in MOSAIC + SUMO  ->  $out =="
$env:SCMS_OUT_DIR = $out
if ($hasSeed) { $env:SCMS_SEED = "$Seed" }
# Scenario input hashes (schema scms.inputs/1). ScmsBackend can auto-discover this beside
# scenario_config.json, but pointing at it explicitly also works when MOSAIC hands the app a
# different configuration path, and it is what lifts road_network / radio_range_m into the dataset
# manifest so datagen.realism_bench can score the run without a --regime flag.
if ($gen.inputs_json -and (Test-Path $gen.inputs_json)) { $env:SCMS_INPUTS_JSON = $gen.inputs_json }
Push-Location $env:MOSAIC_HOME
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'   # MOSAIC/SUMO write progress to stderr; don't treat as fatal
try {
    $mosaicArgs = @('-c', $cfg, '-w', '0')
    if ($Visualize) { $mosaicArgs += '-v' }
    & .\mosaic.bat @mosaicArgs 2>&1 | Select-Object -Last 3
} finally {
    $ErrorActionPreference = $prevEAP
    Pop-Location
    Remove-Item Env:\SCMS_OUT_DIR -ErrorAction SilentlyContinue
    Remove-Item Env:\SCMS_INPUTS_JSON -ErrorAction SilentlyContinue
    if ($hasSeed) { Remove-Item Env:\SCMS_SEED -ErrorAction SilentlyContinue }
}

if (-not (Test-Path (Join-Path $out 'manifest.json'))) {
    throw "Simulation produced no dataset (no manifest.json at $out) - MOSAIC/SUMO likely failed."
}

# Scenario provenance: the Java back-end writes the dataset manifest at JVM exit and has no view
# of how the scenario was generated. gen_scenario records the effective SCMS_* environment, the
# resolved realism knobs and a sha256 per scenario input; park it next to the dataset so a run
# can be reproduced from the dataset alone.
if ($gen.manifest -and (Test-Path $gen.manifest)) {
    Copy-Item $gen.manifest (Join-Path $out 'scenario_provenance.json') -Force
    Write-Host "   provenance -> $(Join-Path $out 'scenario_provenance.json')"
}

$env:PYTHONPATH = "$repo\src"
Write-Host "== [4/6] Featurize -> ML-ready tables =="
python -m scms_sim_ref.datagen.featurize $out
Write-Host "== [5/6] Validate (leakage + revocation precision/recall) =="
python -m scms_sim_ref.datagen.validate $out
Write-Host "== [6/6] Datasheet + baseline ML benchmark =="
python -m scms_sim_ref.datagen.datasheet $out

Write-Host ""
Write-Host "DONE. Dataset at $out  (ma/  ground_truth/  ml/  DATASHEET.md  manifest.json)"
