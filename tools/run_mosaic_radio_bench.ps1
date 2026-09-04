<#
  Run ONE arm of the cross-engine radio benchmark: the MOSAIC/Java path on an existing generated
  scenario, with the geometric radio and the per-link reception trace switched on.

  This deliberately does NOT go through run.ps1: run.ps1 regenerates the scenario first, which would
  rewrite scms-sim/scenarios/gen_intas_urban_low (the GEH-validated traffic input) and move its
  input hashes. Here the scenario on disk is READ ONLY -- only the jar is refreshed into it -- so
  every arm of the sweep sees byte-identical traffic and the only thing that changes between arms
  is the SCMS_* radio configuration.

  Usage (toolchain must be active):
    . C:\Users\Administrator\tools\env.ps1
    .\tools\run_mosaic_radio_bench.ps1 -Arm geo_p23_dcc0 -TxPowerDbm 23 -Dcc 0
    .\tools\run_mosaic_radio_bench.ps1 -Arm geo_p23_dcc1 -TxPowerDbm 23 -Dcc 1
#>
param(
    [Parameter(Mandatory = $true)][string]$Arm,
    [string]$Scenario = 'gen_intas_urban_low',
    [double]$TxPowerDbm = 23.0,
    [double]$RxSensitivityDbm = -81.0,
    [ValidateSet('geometric', 'sns')][string]$RadioModel = 'geometric',
    [ValidateSet('0', '1')][string]$Dcc = '0',
    [ValidateSet('0', '1')][string]$Buildings = '1',
    [double]$TraceProb = 0.3,
    [long]$Seed = 20260809,
    [string]$Root = 'datasets/xengine',
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$env:PYTHONHASHSEED = '0'
$repo = Split-Path -Parent $PSScriptRoot
$scen = Join-Path $repo "scms-sim\scenarios\$Scenario"
if (-not (Test-Path (Join-Path $scen 'scenario_config.json'))) { throw "no scenario at $scen" }

if (-not $SkipBuild) { & "$repo\scms-sim\mosaic-apps\scms-app\build.ps1" | Out-Null }
Copy-Item "$repo\scms-sim\mosaic-apps\scms-app\build\ScmsApp-0.1.0.jar" `
          (Join-Path $scen 'application\ScmsApp-0.1.0.jar') -Force

$out = Join-Path $repo (Join-Path $Root $Arm)
New-Item -ItemType Directory -Force $out | Out-Null

# --- radio / channel configuration for this arm -------------------------------------------------
$env:SCMS_OUT_DIR          = $out
$env:SCMS_INPUTS_JSON      = (Join-Path $scen 'scms_inputs.json')
$env:SCMS_SEED             = "$Seed"
$env:SCMS_EMIT_SAMPLE      = '1.0'          # full emission trace: the CAM-gap distribution needs it
$env:SCMS_RADIO_MODEL      = $RadioModel
$env:SCMS_RADIO_REGIME     = 'urban'
$env:SCMS_BUILDINGS        = $Buildings
$env:SCMS_TX_POWER_DBM     = "$TxPowerDbm"
$env:SCMS_RX_SENSITIVITY_DBM = "$RxSensitivityDbm"
$env:SCMS_WEATHER          = 'clear'        # isolate the propagation model from the weather table
$env:SCMS_DCC              = $Dcc
$env:SCMS_LINK_TRACE       = (Join-Path $out 'link_trace.csv')
$env:SCMS_LINK_TRACE_PROB  = "$TraceProb"

$cfg = Join-Path $scen 'scenario_config.json'
Write-Host "== arm '$Arm' : model=$RadioModel tx=$TxPowerDbm dBm sens=$RxSensitivityDbm dBm dcc=$Dcc buildings=$Buildings"
Write-Host "   scenario $cfg  ->  $out"
$sw = [Diagnostics.Stopwatch]::StartNew()
Push-Location $env:MOSAIC_HOME
$prevEAP = $ErrorActionPreference
$ErrorActionPreference = 'Continue'    # MOSAIC/SUMO write progress to stderr
try {
    & .\mosaic.bat -c $cfg -w 0 2>&1 | Select-Object -Last 6
} finally {
    $ErrorActionPreference = $prevEAP
    Pop-Location
    foreach ($v in 'SCMS_OUT_DIR', 'SCMS_INPUTS_JSON', 'SCMS_SEED', 'SCMS_EMIT_SAMPLE',
                   'SCMS_RADIO_MODEL', 'SCMS_RADIO_REGIME', 'SCMS_BUILDINGS', 'SCMS_TX_POWER_DBM',
                   'SCMS_RX_SENSITIVITY_DBM', 'SCMS_WEATHER', 'SCMS_DCC', 'SCMS_LINK_TRACE',
                   'SCMS_LINK_TRACE_PROB') {
        Remove-Item "Env:\$v" -ErrorAction SilentlyContinue
    }
}
$sw.Stop()
if (-not (Test-Path (Join-Path $out 'manifest.json'))) { throw "no manifest.json at $out - MOSAIC failed" }
Write-Host ("   done in {0:N1} s" -f $sw.Elapsed.TotalSeconds)
