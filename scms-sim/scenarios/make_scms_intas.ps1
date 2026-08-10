# Generate a standalone 'scms_intas' MOSAIC scenario from NextGen's InTAS (Ingolstadt),
# wired to our ScmsBeaconApp and MOSAIC's built-in SNS radio (NextGen ships omnetpp).
#
# To avoid copying the ~1 GB InTAS route set, sumo/ is a DIRECTORY JUNCTION to the
# vendored NextGen scenario. The routing DB (application/InTAS.db) is copied from
# NextGen's docker variant. The generated scenario is git-ignored (derives from
# GPL-3.0 InTAS + EPL MOSAIC assets); regenerate it locally.
#
#   . C:\Users\Administrator\tools\env.ps1
#   .\scms-sim\mosaic-apps\scms-app\build.ps1
#   .\scms-sim\scenarios\make_scms_intas.ps1            # optionally -Variant / -Duration
#   $env:JAVA_TOOL_OPTIONS = '-Dscms.outDir=<repo>\datasets\mosaic_intas'
#   cd $env:MOSAIC_HOME; .\mosaic.bat -c <repo>\scms-sim\scenarios\scms_intas\scenario_config.json -w 0
param(
    [string]$Variant = 'InTAS_highway_2_4_test',
    [string]$Duration = '300s'
)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
$srcRoot = "$repo\third_party\veremi-nextgen\Generator"
$srcScenario = "$srcRoot\simulation\mosaic\scenarios\$Variant"
$dockerDb = "$srcRoot\docker\scenarios\$Variant\application\InTAS.db"
$dst = "$PSScriptRoot\scms_intas"
if (-not (Test-Path $srcScenario)) { throw "InTAS variant not found: $srcScenario" }
if (-not (Test-Path $dockerDb)) { throw "InTAS.db not found: $dockerDb" }
if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
New-Item -ItemType Directory -Force "$dst\application", "$dst\mapping", "$dst\output" | Out-Null
# sumo/ as a junction to the vendored InTAS sumo dir (no ~1 GB copy)
New-Item -ItemType Junction -Path "$dst\sumo" -Target "$srcScenario\sumo" | Out-Null
$enc = New-Object System.Text.UTF8Encoding $false
# scenario_config.json: switch omnetpp->sns, set duration
$sc = (Get-Content "$srcScenario\scenario_config.json" -Raw) `
    -replace '"omnetpp":\s*true', '"omnetpp": false' `
    -replace '"sns":\s*false', '"sns": true' `
    -replace '"duration":\s*"24h"', ('"duration": "' + $Duration + '"')
[IO.File]::WriteAllText("$dst\scenario_config.json", $sc, $enc)
# mapping: our app
[IO.File]::WriteAllText("$dst\mapping\mapping_config.json",
    ((Get-Content "$srcScenario\mapping\mapping_config.json" -Raw) -replace 'etsi\.VehicleCamSendingApp', 'org.scms.app.ScmsBeaconApp'), $enc)
Copy-Item "$srcScenario\output\output_config.xml" "$dst\output\" -Force
Copy-Item "$srcScenario\application\application_config.json" "$dst\application\" -Force
Copy-Item $dockerDb "$dst\application\" -Force
Copy-Item "$repo\scms-sim\mosaic-apps\scms-app\build\ScmsApp-0.1.0.jar" "$dst\application\" -Force
Write-Host "Generated $dst (sumo/ -> junction; InTAS.db + ScmsApp jar in application/)"
Write-Host "Run: cd `$env:MOSAIC_HOME; .\mosaic.bat -c $dst\scenario_config.json -w 0"
