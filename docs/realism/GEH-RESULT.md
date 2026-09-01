# Real-world GEH validation — result

**Verdict: FAIL, and the failure is informative.** Against measured Ingolstadt loop counts, the
InTAS scenario produces **21,095 vehicles where reality recorded 45,713** in the same clock hour —
a **−53.9%** shortfall. Median station GEH is **25.5** against a criterion of < 5. Zero of 23
stations pass.

This is the first comparison in this repository between simulated flows and measured real-world
counts. Everything before it was simulation-against-simulation, which can demonstrate
reproducibility but never realism.

## What was compared

| | |
|---|---|
| Model | `gen_intas_urban_low`, **unmodified InTAS demand** — no `--scale` is applied (verified: the sumocfg contains no scale key, and `gen_scenario.py:383-387` only writes one when ≠ 1.0) |
| Simulator | SUMO 1.25.0, `step-length` 0.1 s, EIDM, sublane model (`lateral-resolution` 0.8), `device.rerouting.probability` 0.82 |
| Warm-up | 21600–25200 s (06:00–07:00 local), **not graded** |
| Graded window | 25200–28800 s = local **07:00–08:00**, a full clock hour |
| Measured | Stadt Ingolstadt loop counts via SAVeNoW/TUM FROST, 15-min bins, **Tue 2023-11-14 06:00–07:00Z** (= 07:00–08:00 CET) |
| Stations | 23 comparable (3120/3130 excluded — registered detectors, zero observations in the entire archive) |
| Criteria | FHWA Traffic Analysis Toolbox Vol III (FHWA-HRT-04-040) §5 |

Because the graded window is exactly 3600 s and the reference bins cover exactly 3600 s, counts and
veh/h are numerically identical on both sides — no extrapolation, no √k inflation.

## Result

| Gate | Value | Threshold | |
|---|---|---|---|
| Stations with GEH < 5 | **0.00** | ≥ 0.85 | FAIL |
| GEH on summed flow | **134.7** | ≤ 4 | FAIL |
| Relative error on total flow | **−53.9%** | ≤ 5% | FAIL |
| Stations inside FHWA flow tolerance | **0.00** | ≥ 0.85 | FAIL |

Median GEH 25.5, p85 43.9.

### Every station under-produces. Not one over-produces.

| ratio (modelled/measured) | min | p25 | median | p75 | max |
|---|---|---|---|---|---|
| across 23 stations | 0.17 | 0.32 | **0.48** | 0.63 | 0.74 |

Under 0.75×: **23 of 23**. Between 0.75× and 1.25×: 0. Over 1.25×: 0.

Worst: `4210` 274 vs 1571 (0.17), `4160` 435 vs 2216 (0.20), `8005` 465 vs 1689 (0.28).
Best: `5060` 594 vs 801 (0.74), `6030` 1030 vs 1408 (0.73), `8001` 1881 vs 2611 (0.72).

## Why this is a demand finding, not a measurement artefact

The uniformity is the evidence. A detector-mapping error, a window misalignment or a unit error
produces a *mixture* — some stations high, some low. A one-sided result across all 23 stations, with
no exceptions, is the signature of a systematic demand deficit.

Four candidate artefacts were checked and eliminated:

1. **Demand scaling** — ruled out by inspection; no `--scale` is applied.
2. **Window misalignment (the dangerous one).** The API stamps `phenomenonTime` in **UTC**; the
   InTAS clock is **local CET**. Verified independently from the run's own summary output:
   concurrent vehicles climb 1589 → 3548 across the warm-up hour and plateau at ~3870 through
   07:10–07:40. The graded window sits exactly on the AM peak, matching both the InTAS departure
   profile and the measured day profile.
3. **Double-counting on the measured side.** Ruled out by detector taxonomy: suffixes are approach
   letters (`DA`/`DB`/`DC`/`DD`…) with per-lane numbering and `L`/`R` turn-lane variants — station
   1010's 16 detectors are one per approach lane across four arms. Each vehicle entering crosses
   exactly one loop, so the station sum is a legitimate inflow, not a duplicated passage.
4. **Detector-subset asymmetry.** Only InTAS-matched detectors are summed on the measured side. For
   the 15 stations where SAVeNoW instruments *more* loops than InTAS, this makes the reference a
   **lower bound** — which would flatter the model, not penalise it. The shortfall is therefore if
   anything understated.

## What it does and does not say

**It does not say our simulator is wrong.** SUMO faithfully executed the demand it was given; the
mobility, car-following and network handling are not implicated. This validates the **InTAS demand
input** against present-day reality.

**It does say the scenario should not be described as reproducing Ingolstadt traffic.** At these 23
loops in the morning peak it delivers roughly half the vehicles the city actually measures.

**The vintage gap is real and must be quoted with the number.** InTAS demand is calibrated to
**November 2019**; the counts are from **November 2023**, a four-year gap spanning COVID. Part of the
difference is genuine change. But a median ratio of 0.48 is far larger than four years of plausible
traffic growth, so vintage cannot be the whole explanation. Note also that InTAS's own published
validation reported NRMSE 0.33 — a loose fit, and one that does not constrain systematic bias, since
a uniform under-estimate can sit inside that error.

## Second window — the shortfall is systematic, not day-specific

The first result used one hour of one day, which cannot distinguish a systematic demand deficit from
an unrepresentative morning. A second window was graded: a **different day, a different peak, and the
opposite time of day**.

| | Window A | Window B |
|---|---|---|
| Reference | Tue 2023-11-14, 06:00–07:00Z (local 07:00–08:00, **AM** peak) | Wed 2023-11-15, 15:00–16:00Z (local 16:00–17:00, **PM** peak) |
| SUMO window | 25200–28800 s (warm-up from 21600) | 57600–61200 s (warm-up from 54000) |
| Measured | 45,713 veh | 48,169 veh |
| Modelled | 21,095 veh | 20,096 veh |
| **Relative error** | **−53.9%** | **−58.3%** |
| Median station GEH | 25.5 | 30.9 |
| Stations passing GEH < 5 | 0 / 23 | 0 / 23 |
| Ratio min / median / max | 0.17 / **0.48** / 0.74 | 0.21 / **0.42** / 0.66 |
| Stations under 0.75× | **23 / 23** | **23 / 23** |
| Stations over 1.25× | 0 | 0 |

Both windows: every one of 23 stations under-produces, none over-produces, and the model delivers
roughly 42–48% of measured flow. The measured side is internally consistent too — the city records
45.7k and 48.2k vehicles in the two peaks, as expected for AM and PM peaks in the same week.

**This settles the day-specificity question.** A one-sided ~2× deficit reproducing across two
different days, two different peaks and 46 station-observations is a property of the demand model,
not of the hour that was sampled.

## Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
# 1. measured counts (never committed -- licence not formally stated)
python tools\fetch_ingolstadt_counts.py `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z --out refcounts_A.json
# 2. the graded hour, with a warm-up hour ahead of it
cd scms-sim\scenarios\gen_intas_urban_low\sumo
sumo -c InTAS_buildings.sumocfg --begin 21600 --end 28800 --output-prefix gehA_ --seed 42
# 3. grade it
python tools\sumo_realism.py --det-out gehA_InTAS_Detectors_Output.xml `
  --det-add InTAS_E1.add.xml --ref-counts refcounts_A.json `
  --begin 25200 --end 28800 --seed 42
```

## Next

1. **Do not tune demand to make this pass.** A calibrated-to-the-test scenario would destroy the
   value of having a real reference at all.
2. Re-run on a second day and a second hour to separate day-specific effects from systematic bias.
   The archive supports multi-day sampling; FHWA's own distribution criteria want several days.
3. Quantify the vintage component where possible — the same loops in 2019, if any archive reaches
   back that far.
4. Consider whether the shortfall is concentrated in particular movements (through vs turning) by
   grading per approach rather than per station, which the detector taxonomy now makes possible.
5. This gate is now real and it is red. That is a better state than the green it showed when it was
   silently comparing the simulator against itself.
