# Regenerate the 'scms_smoke' MOSAIC scenario from the bundled (EPL-2.0) HelloWorld
# scenario, wiring in our ScmsBeaconApp. The generated scenario is git-ignored
# because it derives from EPL-licensed MOSAIC assets; regenerate it locally.
#
#   . C:\Users\Administrator\tools\env.ps1
#   .\scms-sim\mosaic-apps\scms-app\build.ps1
#   .\scms-sim\scenarios\make_scms_smoke.ps1
#   cd $env:MOSAIC_HOME; .\mosaic.bat -c <repo>\scms-sim\scenarios\scms_smoke\scenario_config.json -w 0
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$src = "$env:MOSAIC_HOME\scenarios\HelloWorld"
$dst = "$PSScriptRoot\scms_smoke"
if (Test-Path $dst) { Remove-Item -Recurse -Force $dst }
Copy-Item -Recurse -Force $src $dst
Copy-Item "$PSScriptRoot\..\mosaic-apps\scms-app\build\ScmsApp-0.1.0.jar" "$dst\application\" -Force
$enc = New-Object System.Text.UTF8Encoding $false
$map = "$dst\mapping\mapping_config.json"
[IO.File]::WriteAllText($map, ((Get-Content $map -Raw) -replace 'org\.eclipse\.mosaic\.fed\.application\.app\.etsi\.VehicleCamSendingApp', 'org.scms.app.ScmsBeaconApp'), $enc)
$sc = "$dst\scenario_config.json"
$s = (Get-Content $sc -Raw) -replace '"id":\s*"HelloWorld"', '"id": "scms_smoke"' -replace '"duration":\s*"1000s"', '"duration": "200s"'
[IO.File]::WriteAllText($sc, $s, $enc)
Write-Host "Generated $dst"
