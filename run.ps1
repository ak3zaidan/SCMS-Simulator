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
#>
param(
    [ValidateSet('smoke', 'highway', 'barnim', 'tiergarten',
        'intas_urban_low', 'intas_urban_rush', 'intas_highway_low', 'intas_highway_rush')]
    [string]$Scenario = 'smoke',
    [string]$Duration,
    [int]$Seed,
    [string]$Scale,          # route maps: SUMO traffic-density multiplier
    [int]$MaxVehicles,       # flow maps: cap on concurrent vehicles
    [int]$TargetFlow,        # flow maps: vehicles/hour
    [int]$Lanes,             # flow maps: number of lanes used
    [switch]$Visualize
)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$repo = $PSScriptRoot

Write-Host "== [1/5] Build MOSAIC app (javac, no Maven) =="
& "$repo\scms-sim\mosaic-apps\scms-app\build.ps1"

Write-Host "== [2/5] Generate '$Scenario' scenario =="
$genArgs = @($Scenario)
if ($Duration)    { $genArgs += @('--duration', $Duration) }
if ($Seed)        { $genArgs += @('--seed', "$Seed") }
if ($Scale)       { $genArgs += @('--scale', "$Scale") }
if ($MaxVehicles) { $genArgs += @('--max-vehicles', "$MaxVehicles") }
if ($TargetFlow)  { $genArgs += @('--target-flow', "$TargetFlow") }
if ($Lanes)       { $genArgs += @('--lanes', "$Lanes") }
$gen = & python "$repo\scms-sim\scenarios\gen_scenario.py" @genArgs | ConvertFrom-Json
$cfg = $gen.scenario_config
$out = $gen.dataset_dir
Write-Host "   $($gen.kind) scenario -> $cfg"

Write-Host "== [3/5] Simulate in MOSAIC + SUMO  ->  $out =="
$env:SCMS_OUT_DIR = $out
if ($Seed) { $env:SCMS_SEED = "$Seed" }
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
    if ($Seed) { Remove-Item Env:\SCMS_SEED -ErrorAction SilentlyContinue }
}

$env:PYTHONPATH = "$repo\src"
Write-Host "== [4/5] Featurize -> ML-ready tables =="
python -m scms_sim_ref.datagen.featurize $out
Write-Host "== [5/5] Validate (leakage + revocation precision/recall) =="
python -m scms_sim_ref.datagen.validate $out

Write-Host ""
Write-Host "DONE. Dataset at $out  (ma/  ground_truth/  ml/  manifest.json)"
