<#
  Run the test suite with a PINNED hash seed.

    .\test.ps1                      # whole suite
    .\test.ps1 -q tests/test_pipeline.py -k golden      # any pytest arguments pass straight through

  WHY THIS SCRIPT EXISTS. `PYTHONHASHSEED` is read by CPython AT STARTUP, before any user code runs,
  so neither `conftest.py` nor `[tool.pytest.ini_options]` can set it for the interpreter running the
  tests -- only the launcher can. This is the launcher. `conftest.py` exports the variable so every
  SUBPROCESS the suite spawns inherits it; this script is what pins the parent.

  It does not change any pinned digest and is not what protects them: the engine's keyed RNG streams
  are `random.Random(<str>)`, which CPython seeds from sha512 of the key and is therefore
  PYTHONHASHSEED-independent. What it pins is `manifest["runtime"]["hash_randomization"]` and
  set/dict iteration order. See docs/realism/PLUGIN-ARCHITECTURE.md phase 0.
#>
$ErrorActionPreference = 'Stop'
if (-not $env:MOSAIC_HOME) {
    # env.ps1 prints `java -version` (which writes to STDERR); under EAP='Stop' a native command's
    # stderr becomes a TERMINATING error, so sourcing it must drop to 'Continue' -- the same guard
    # run.ps1 applies around the scenario generator.
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { . C:\Users\Administrator\tools\env.ps1 | Out-Null } finally { $ErrorActionPreference = $prevEAP }
}
$env:PYTHONHASHSEED = '0'
Push-Location $PSScriptRoot
try {
    if ($args.Count -eq 0) { & python -m pytest -q } else { & python -m pytest @args }
    exit $LASTEXITCODE
} finally {
    Pop-Location
}
