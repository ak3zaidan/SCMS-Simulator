# Build the SCMS MOSAIC application jar with javac (no Maven).
# Requires the toolchain env: . C:\Users\Administrator\tools\env.ps1  (sets JAVA_HOME + MOSAIC_HOME)
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$app = $PSScriptRoot
$lib = "$env:MOSAIC_HOME\lib\mosaic\*;$env:MOSAIC_HOME\lib\third-party\*"
# Clean the class output first: javac never deletes, so a renamed/removed class (or a nested class
# that moved to another file) would otherwise linger in out\ and be packed into the jar forever.
Remove-Item -Recurse -Force "$app\out" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$app\out" | Out-Null
New-Item -ItemType Directory -Force "$app\build" | Out-Null
$srcs = (Get-ChildItem "$app\src" -Recurse -Filter *.java).FullName
& "$env:JAVA_HOME\bin\javac.exe" -cp $lib -d "$app\out" $srcs
& "$env:JAVA_HOME\bin\jar.exe" --create --file "$app\build\ScmsApp-0.1.0.jar" -C "$app\out" .
Write-Host "Built $app\build\ScmsApp-0.1.0.jar"
