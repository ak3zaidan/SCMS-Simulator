# Launch the SCMS-Simulator control-panel GUI and open it in the browser.
#   .\gui.ps1
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) { . C:\Users\Administrator\tools\env.ps1 }
Start-Process 'http://127.0.0.1:8710'
python "$PSScriptRoot\gui\server.py"
