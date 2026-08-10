<#
  One-command end-to-end runner for the SCMS-aware V2X dataset framework:
  build the MOSAIC app -> generate a scenario -> simulate (MOSAIC + SUMO) ->
  featurize into ML-ready tables -> validate (leakage + revocation precision/recall).

  Usage:
    . C:\Users\Administrator\tools\env.ps1     # once per shell: JAVA_HOME/SUMO_HOME/MOSAIC_HOME/PATH
    .\run.ps1                                   # 'smoke' highway scenario (fast, ~50 vehicles)
    .\run.ps1 -Scenario intas                   # real Ingolstadt (InTAS) map (~333 vehicles)
    .\run.ps1 -Scenario intas -Duration 600s -Seed 42
#>
param(
    [ValidateSet('smoke', 'intas')][string]$Scenario = 'smoke',
    [string]$Duration,
    [int]$Seed
)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$repo = $PSScriptRoot

Write-Host "== [1/5] Build MOSAIC app (javac, no Maven) =="
& "$repo\scms-sim\mosaic-apps\scms-app\build.ps1"

Write-Host "== [2/5] Generate '$Scenario' scenario =="
if ($Scenario -eq 'smoke') {
    & "$repo\scms-sim\scenarios\make_scms_smoke.ps1" | Out-Null
    $cfg = "$repo\scms-sim\scenarios\scms_smoke\scenario_config.json"
    $out = "$repo\datasets\mosaic_poc"
} else {
    if ($Duration) { & "$repo\scms-sim\scenarios\make_scms_intas.ps1" -Duration $Duration | Out-Null }
    else { & "$repo\scms-sim\scenarios\make_scms_intas.ps1" | Out-Null }
    $cfg = "$repo\scms-sim\scenarios\scms_intas\scenario_config.json"
    $out = "$repo\datasets\mosaic_intas"
}

Write-Host "== [3/5] Simulate in MOSAIC + SUMO  ->  $out =="
$env:SCMS_OUT_DIR = $out
if ($Seed) { $env:SCMS_SEED = "$Seed" }
Push-Location $env:MOSAIC_HOME
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'   # MOSAIC/SUMO write progress to stderr; don't treat as fatal
try {
    & .\mosaic.bat -c $cfg -w 0 2>&1 | Select-Object -Last 3
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
Write-Host "DONE. Dataset at $out"
Write-Host "  ma/            MA-visible events (the only source of ML features)"
Write-Host "  ground_truth/  oracle labels (never used as features)"
Write-Host "  ml/            report/subject features + labels (parquet & csv) with train/val/test splits"
Write-Host "  manifest.json  seed, config, per-file sha256, data digest"
