# Re-emit the OPT-IN calibrated-demand sumocfgs into the generated InTAS scenario.
#
# WHY THIS SCRIPT EXISTS.  scms-sim/scenarios/gen_*/ is git-ignored and is rebuilt by
# gen_scenario.py, which wipes anything written into it.  The calibrated sumocfgs are
# therefore not durable artifacts; this tracked script rebuilds them on demand.
#
# WHAT IT DOES NOT DO.  It does not touch InTAS_buildings.sumocfg.  The unmodified InTAS
# demand stays the default; the calibrated route set is an ALTERNATIVE config, never a
# replacement.  See docs/realism/DEMAND-CALIBRATION.md for the result -- the calibrated
# demand does NOT pass the FHWA gates and must not be presented as a validated scenario.
#
# LICENCE.  The calibrated route sets under .cache/calib/ are derived from Stadt Ingolstadt
# / SAVeNoW loop counts whose licence is NOT formally stated.  They stay in the git-ignored
# cache.  This script only writes configuration that POINTS at them; it never copies count
# data into the tree.  If .cache/calib is empty, re-run the pipeline in DEMAND-CALIBRATION.md.
#
#   . C:\Users\Administrator\tools\env.ps1
#   .\scms-sim\scenarios\make_calibrated_intas.ps1
#   cd scms-sim\scenarios\gen_intas_urban_low\sumo
#   sumo -c InTAS_calibrated_am.sumocfg --begin 21600 --end 28800 --output-prefix cAM_ --seed 42
$ErrorActionPreference = 'Stop'
if (-not $env:SUMO_HOME) { . C:\Users\Administrator\tools\env.ps1 }
$repo = (Resolve-Path "$PSScriptRoot\..\..").Path
$S = "$repo\scms-sim\scenarios\gen_intas_urban_low\sumo"
$C = "$repo\.cache\calib"
if (-not (Test-Path "$S\InTAS_buildings.sumocfg")) { throw "InTAS scenario not generated: $S" }

# 1. the baseline config, but observing through the repaired detector layout, so the
#    baseline and the calibrated run are graded with the SAME instrument.
python "$repo\tools\calibrate_demand.py" scenario --sumocfg "$S\InTAS_buildings.sumocfg" `
    --additional "$S\BusStations.add.xml" "$C\layout\calib_layout.add.xml" "$S\buildings.poly.xml" `
    --name InTAS_baseline_calibgrade.sumocfg

# 2 + 3. the two opt-in calibrated configs (AM and PM peak bands)
foreach ($band in 'am', 'pm') {
    $rou = "$C\calibrated_$band.rou.xml"
    if (-not (Test-Path $rou)) { Write-Warning "missing $rou -- skipping $band"; continue }
    python "$repo\tools\calibrate_demand.py" scenario --sumocfg "$S\InTAS_buildings.sumocfg" `
        --routes $rou `
        --additional "$S\BusStations.add.xml" "$C\layout\calib_layout.add.xml" "$S\buildings.poly.xml" `
        --name "InTAS_calibrated_$band.sumocfg"

    # 4 + 5. the DIAGNOSTIC pair for docs/realism/DEMAND-CALIBRATION.md sect. 12: the same
    #        demand with device.rerouting disabled, i.e. SUMO actually driving the route set
    #        routeSampler solved for.  InTAS's shipped 0.82 rerouting probability replaces the
    #        route of 75 % of the calibrated vehicles, so the as-shipped config grades a route
    #        set it does not execute.  Executing it is MUCH WORSE against the loops (AM
    #        -22.5 % -> -44.0 %, PM -30.6 % -> -68.3 %) because the assignment is infeasible
    #        and the city gridlocks.  This config exists to MEASURE that, not to be run as a
    #        scenario -- see sect. 12.3 and 12.6.
    python "$repo\tools\calibrate_demand.py" scenario --sumocfg "$S\InTAS_buildings.sumocfg" `
        --routes $rou --execute-assigned-routes `
        --additional "$S\BusStations.add.xml" "$C\layout\calib_layout.add.xml" "$S\buildings.poly.xml" `
        --name "InTAS_calibrated_${band}_execroutes.sumocfg"
}
Write-Host ""
Write-Host "InTAS_buildings.sumocfg is UNCHANGED -- unmodified InTAS demand remains the default."
Write-Host "AM band: --begin 21600 --end 28800 (graded hour 25200-28800 = local 07:00-08:00)"
Write-Host "PM band: --begin 54000 --end 61200 (graded hour 57600-61200 = local 16:00-17:00)"
Write-Host "*_execroutes.sumocfg are DIAGNOSTICS (sect. 12), not scenarios: they run the"
Write-Host "calibrated routes without SUMO's rerouting device and score far worse."
