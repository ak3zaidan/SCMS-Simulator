# Demand calibration against measured Ingolstadt loop counts — result

**Verdict: the calibration FAILS its gate, and the failure is the finding.** Fitting InTAS's own
route pool to measured loop counts with SUMO's `routeSampler.py` closes only about a third of the
documented flow deficit. On days routeSampler never saw, the calibrated demand still delivers
**−27.1 %** (AM peak) and **−24.1 %** (PM peak) of measured flow, median station GEH **14.07** and
**11.74** against a criterion of < 5. **All four FHWA gates fail in every window, calibrated and
held-out alike.**

The interesting part is *why*. The deficit decomposes into three components of very different
character, and only the middle one is a demand problem at all:

| Component | AM effect | PM effect | Fixable by demand calibration? |
|---|---|---|---|
| 1. Detector-layout defect (74 loops on sidewalk lanes) | +19.6 pp | +17.2 pp | No — it is an instrument bug |
| 2. Demand deficit routeSampler can address | +8.4 pp | +11.2 pp | Yes, and it transfers to unseen days |
| 3. Network cannot carry the measured flow | −27.1 pp residual | −24.1 pp residual | **No** |

Component 3 is a hard ceiling. Loading more vehicles does not produce more counted vehicles; it
produces queues. That is a statement about the InTAS **network and signal plans**, not about its
demand, and no demand calibration can reach it.

---

## 1. What was compared, and how the like-for-like rules were kept

| | |
|---|---|
| Scenario | `scms-sim/scenarios/gen_intas_urban_low/sumo`, SUMO 1.25.0, `step-length` 0.1 s, EIDM, sublane (`lateral-resolution` 0.8), seed 42 |
| Reference | Stadt Ingolstadt loop counts via SAVeNoW/TUM FROST (`https://savenow.gis.lrg.tum.de/frost/v1.1/`), 15-min bins, retrieved 2026-09-01 |
| Stations | 23 comparable (3120/3130 excluded: registered but zero observations in the entire archive) |
| Criteria | FHWA Traffic Analysis Toolbox Vol III (FHWA-HRT-04-040, 2004) §5, read from `src/scms_sim_ref/datagen/refdata/geh_criteria.json` |

Four traps, and how each was handled:

**Clocks.** The API stamps `phenomenonTime` in **UTC**; the InTAS SUMO clock is local **CET**
(UTC+1 in November). The AM peak is measured `06:00–07:00Z` = SUMO `25200–28800`; the PM peak is
measured `15:00–16:00Z` = SUMO `57600–61200`. Every run has a **full warm-up hour** ahead of the
graded hour (`21600–25200` / `54000–57600`), which is fitted but never graded.

**Full clock hour.** Every graded window is exactly 3600 s and every reference window is exactly
3600 s, so counts and veh/h are numerically identical on both sides. GEH is not scale-free —
`GEH(k·m, k·c) = √k · GEH(m, c)` — so a short window extrapolated to veh/h would inflate every
value by √k. No extrapolation is used anywhere.

**Detector-subset asymmetry.** Only 10 of 25 stations have detector sets identical to InTAS's. For
the rest, only InTAS-matched detectors are summed on the measured side; otherwise the reference
over-counts by up to +92.9 %. This grader is *stricter still*: it sums a modelled loop only if that
same loop appears in the reference's `matched_detector_ids` (see §7 — this is why the baseline here
reads −55.7 % where `GEH-RESULT.md` documents −53.9 %).

**Buses.** Buses cross the same loops and exist on both sides of the comparison, so the scheduled
bus passages per counting edge (121 departures in the AM graded hour, 117 in the PM) are subtracted
from the routeSampler car targets. Computed exactly from `BusRoutes.flow.xml`, not simulated.

---

## 2. Calibration and held-out window design

routeSampler saw the **calibration** windows only. The **held-out** windows were fetched, then left
untouched until grading. Both sets span AM and PM peaks and more than one weekday.

| Set | AM peak (`06:00–07:00Z`) | PM peak (`15:00–16:00Z`) |
|---|---|---|
| **Calibration** (routeSampler fitted these) | Tue **2023-11-14**, Thu **2023-11-16** | Wed **2023-11-15**, Thu **2023-11-16** |
| **Held-out** (never seen) | Tue **2023-11-21**, Thu **2023-11-23** | Tue **2023-11-21**, Wed **2023-11-22**, Thu **2023-11-23** |

Warm-up hours (`05:00–06:00Z` / `14:00–15:00Z` of the same calibration days) were also fitted, so
the network is primed with a fitted inflow rather than a cold InTAS one.

Nine reference windows in total: 4 calibration, 5 held-out.

**The reference's own day-to-day spread** — the noise floor any claimed improvement must beat:

| Band | mean | min | max | spread | CV |
|---|---|---|---|---|---|
| AM `06:00–07:00Z`, 4 days | 45,082 | 44,370 | 45,713 | 3.0 % | 0.012 |
| PM `15:00–16:00Z`, 5 days | 45,775 | 39,950 (22 Nov) | 49,034 | 19.8 % | 0.082 |

The PM band is genuinely noisy: Wed 22 Nov measured 39,950 against 49,034 the day before. This
matters when reading the PM held-out numbers — see §5.

---

## 3. The detector-layout defect, found before any demand was touched

74 of the 194 named `e1Detector` entries in `InTAS_E1.add.xml` sit on SUMO **sidewalk** lanes —
lane index 0 of their edge, `allow="pedestrian"`, 2.00 m wide. A passenger car can never drive there.
Measured directly in the baseline AM run: **all 74 recorded exactly 0 vehicles across the graded
hour** — not one of them registered a single passage. The real loops they map to measured **16,673
of the 45,713 reference vehicles (36.5 %)** in window A.

`calibrate_demand.py layout` detects this structurally (a lane that forbids `passenger`), repairs it
by shifting every detector on such an edge up by the number of leading non-car lanes, and proves the
repair: all 74 move onto a car lane, **0 collisions, 0 overflows, 0 unrepairable edges**. That clean
signature is what an off-by-one looks like when sidewalks were added to the network *after* the loops
were placed. 159 detectors move in total (the shift applies per edge, not per detector). Original ids
are never modified; repaired copies are emitted as `fx_<id>` into a separate file, so **one run
measures both layouts** and the comparison is exact.

This is worth stating plainly: **roughly 19 points of the documented ~54 % "demand deficit" was a
measurement-instrument bug, not demand.** With demand completely untouched, repairing the layout
moves the baseline from −55.7 % to −36.4 % (AM, 14 Nov) and −60.0 % to −42.7 % (PM, 15 Nov).

Everything below is graded on the **repaired** layout, which is the honest instrument. As-shipped
numbers are reported alongside for continuity with `GEH-RESULT.md`.

---

## 4. The pipeline

**Candidate pool.** One representative route per InTAS vehicle departing in the band (AM
`18000–32400`, PM `50400–64800`) — the highest-probability alternative of each vehicle's
`routeDistribution`, i.e. the modal choice of InTAS's own calibrated 2019 assignment. Duplicates are
kept: their multiplicity *is* InTAS's OD prior, and routeSampler samples the pool uniformly, so
keeping them makes that prior the sampling distribution.

| Band | vehicles scanned | routes kept | distinct edge sequences | mean route length |
|---|---|---|---|---|
| AM | 185,923 | 48,069 | 22,460 | 69.9 edges |
| PM | 185,923 | 45,833 | 26,226 | 91.9 edges |

**Count targets.** A counting edge must be **fully instrumented** (one loop per car lane) *and*
every one of its loops must be measured in *every* calibration window — otherwise the target is a
lower bound and routeSampler would faithfully reproduce an under-count. That leaves **55 of 96
detector edges**.

This is the single most important structural limitation of the exercise:

| | AM graded hour | PM graded hour |
|---|---|---|
| Reference total (23 stations) | 45,042 | 48,578 |
| Covered by the 55 counting edges | 31,228 | 35,115 |
| **Fraction of measured flow routeSampler can constrain** | **69.3 %** | **72.3 %** |

Roughly **30 % of the measured flow is not constrained at all**. Station **8005 is entirely
spatially held out** — it has no counting edge whatsoever (`station_constrained_fraction` 0.00), and
stations 8001 (0.18) and 4160 (0.25) are barely constrained. §5 shows this predicts exactly where the
calibration works and where it does nothing.

**routeSampler.** Fitted per 3600 s interval, seed 42 (the tool default, fixed before any result was
seen). Its *own* accounting reports an almost perfect fit:

| Band | interval | routes written | target achieved | GEH<5 at counting locations |
|---|---|---|---|---|
| AM | 21600 (warm-up) | 11,574 | 22,706 / 22,710 (**99.98 %**) | **100.00 %** |
| AM | 25200 (graded) | 16,655 | 31,040 / 31,046 (**99.98 %**) | **100.00 %** |
| PM | 54000 (warm-up) | 14,866 | 32,989 / 33,674 (97.97 %) | 98.18 % |
| PM | 57600 (graded) | 15,749 | 34,364 / 34,948 (98.33 %) | 98.18 % |

Seed sensitivity is nil: seeds 7 and 123 also reach 99.98 % / 100.00 % GEH<5 on the AM band
(28,272 and 28,014 vehicles vs 28,229 at seed 42). **The seed was not chosen to flatter anything,
and could not have been.**

6,313 (AM) / 5,146 (PM) candidate routes pass no counting location and are dropped. This is itself a
bias worth naming: the calibrated demand systematically over-represents routes through instrumented
corridors and under-represents everything else.

**Opt-in scenario.** `InTAS_buildings.sumocfg` is left **byte-identical** (verified by hash) — the
unmodified InTAS demand stays the default. The calibrated demand is a separate config,
`InTAS_calibrated_am.sumocfg` / `_pm.sumocfg`. Because `scms-sim/scenarios/gen_*/` is git-ignored and
regenerated by other tooling, `scms-sim/scenarios/make_calibrated_intas.ps1` re-emits these configs
on demand.

---

## 5. Results

Four SUMO runs, each a warm-up hour plus a graded hour, seed 42, run in parallel.
Actual wall clock: baseline AM **73 min**, baseline PM **65 min**, calibrated AM **93 min**,
calibrated PM **123 min**.

### The headline: held-out

**Repaired layout — the honest instrument.** Modelled totals are one number per run; measured totals
are the mean across the windows in that set.

| Run | Set | n | modelled | measured | **rel. error** | **median GEH** | GEH<5 | in FHWA tol. |
|---|---|---|---|---|---|---|---|---|
| baseline AM | calibration | 2 | 29,098 | 45,042 | −35.4 % | 17.18 | 3.0/23 | 0.152 |
| baseline AM | **held-out** | 2 | 29,098 | 45,123 | **−35.5 %** | **15.81** | 4.0/23 | 0.174 |
| **calibrated AM** | calibration | 2 | 32,885 | 45,042 | −27.0 % | 14.86 | 8/23 | 0.348 |
| **calibrated AM** | **held-out** | 2 | 32,885 | 45,123 | **−27.1 %** | **14.07** | **8/23** | 0.348 |
| baseline PM | calibration | 2 | 27,619 | 48,578 | −43.1 % | 22.06 | 0.5/23 | 0.043 |
| baseline PM | **held-out** | 3 | 27,619 | 43,906 | **−36.6 %** | **15.22** | 4.0/23 | 0.174 |
| **calibrated PM** | calibration | 2 | 33,094 | 48,578 | −31.9 % | 16.11 | 3/23 | 0.152 |
| **calibrated PM** | **held-out** | 3 | 33,094 | 43,906 | **−24.1 %** | **11.74** | 4.3/23 | 0.232 |

**The calibration–held-out gap:**

| Band | calibration | held-out | **gap** | reference noise floor | verdict |
|---|---|---|---|---|---|
| AM | −27.0 % | −27.1 % | **0.2 pp** | 1.5 % | inside the noise floor — **no overfitting** |
| PM | −31.9 % | −24.1 % | **7.8 pp** | 8.6 % | inside the noise floor — **no overfitting** |

The AM gap is essentially zero. The PM gap is larger but points the *wrong way for overfitting* —
held-out is **better** than calibration, because two of the three held-out PM days (22 and 23 Nov)
simply had less traffic (39,950 and 42,735 against ~48,600 on the calibration days). Correcting for
that, the PM fit transfers as cleanly as the AM one.

**So the fit generalises perfectly — and that is not good news.** It generalises because it barely
moved. A calibration that gains 8 points and then fails by 27 has nothing to overfit.

### Per-station ratio distribution (held-out, repaired layout)

| | min | p25 | median | p75 | max | under 0.75× | over 1.25× |
|---|---|---|---|---|---|---|---|
| baseline AM | 0.32 | 0.55 | 0.66 | 0.78 | 1.50 | 15.0 / 23 | 0.5 / 23 |
| **calibrated AM** | 0.31 | 0.60 | **0.73** | 0.93 | 1.11 | **12 / 23** | **0 / 23** |
| baseline PM | 0.26 | 0.54 | 0.68 | 0.81 | 1.10 | 14.7 / 23 | 0 / 23 |
| **calibrated PM** | 0.46 | 0.61 | **0.79** | 0.94 | 1.50 | **11.0 / 23** | 1.7 / 23 |

### Against the documented baseline

`GEH-RESULT.md` documents −53.9 % / −58.3 %, median ratios 0.48 / 0.42, **23 of 23 stations
under-producing, none over-producing**. On the **as-shipped** layout, which is what that document
used, this pipeline reproduces it and the calibration barely helps:

| | documented | measured here (baseline, as-shipped) | calibrated (as-shipped) |
|---|---|---|---|
| AM rel. error | −53.9 % | −55.0 % cal / **−55.1 % held-out** | −50.1 % / **−50.2 % held-out** |
| AM median ratio | 0.48 | 0.47 | 0.49 |
| PM rel. error | −58.3 % | −60.3 % cal / **−55.8 % held-out** | −54.7 % / **−49.5 % held-out** |
| PM median ratio | 0.42 | 0.40 | 0.43 |

The one-sided signature the documented result rests on is **confirmed as-shipped and then broken by
the layout repair**, not by the calibration. As-shipped, 22 of 23 AM stations still under-produce and
**none** over-produces (max ratio 0.85) — the documented finding reproduces. On the **repaired**
layout the same run already crosses 1.0: max ratio 1.50, one station above 1.25×, and the AM
held-out spread widens to 0.32–1.50. After calibration the AM distribution tightens around a median
of 0.73 with max 1.11 — still 12 of 23 under 0.75×, but no longer the uniform one-sided deficit the
documented result describes.

This matters for interpretation. `GEH-RESULT.md` argues, correctly, that a *uniform* one-sided result
across all 23 stations is the signature of systematic bias rather than a mapping or window error. The
argument is sound — but the bias it detected was substantially **the 74 zeroed loops**, not demand
alone. A detector-mapping error that zeroes 36.5 % of the measured flow produces exactly the uniform
under-count that was observed.

### Where the calibration works — and where it does nothing

Held-out windows only, repaired layout, stations grouped by the fraction of their measured flow that
routeSampler was actually able to constrain:

| Constrained fraction | n (station-windows) | mean \|ratio−1\| | mean station GEH |
|---|---|---|---|
| ≥ 0.9 (AM) | 22 | 0.325 → **0.171** | 14.3 → **8.0** |
| 0.5–0.9 (AM) | 12 | 0.358 → 0.388 | 18.8 → 19.9 |
| < 0.5 (AM) | 12 | 0.414 → 0.301 | 23.5 → 16.2 |
| ≥ 0.9 (PM) | 33 | 0.324 → **0.241** | 15.7 → **10.7** |
| 0.5–0.9 (PM) | 24 | 0.362 → 0.350 | 20.3 → 17.7 |
| < 0.5 (PM) | 12 | 0.360 → 0.316 | 18.6 → 15.7 |

Fully constrained stations improve substantially **on days routeSampler never saw** — mean GEH 14.3
→ 8.0 (AM). Partially constrained stations do not improve and in the AM band get slightly worse.
And the cleanest single case: **station 8005, which has no counting edge at all**, goes 0.32 → 0.31
(AM held-out) — routeSampler changed nothing where it could see nothing. That is the calibration
behaving exactly as its mathematics says it must, and it is the strongest evidence that the method
was applied honestly.

Individual AM held-out (21 Nov) movements, largest first: `8604` 0.49 → 0.95, `4070` 0.57 → 0.87,
`4240` 0.78 → 1.00, `1010` 0.43 → 0.68, `5060` 1.90 → 1.09 (an overshoot corrected). Against those:
`6030` 0.77 → 0.56 and `3150` 0.69 → 0.55 got **worse** — matching some loops does push others away.

---

## 6. Why the calibration cannot close the deficit

routeSampler solved its problem. SUMO then failed to execute the solution.

| | routeSampler's own accounting | delivered by SUMO at the loops |
|---|---|---|
| AM graded hour | 31,040 / 31,046 (**99.98 %**) | 24,071 / 31,046 (**−22.5 %**) |
| PM graded hour | 34,364 / 34,948 (98.33 %) | 24,250 / 34,948 (**−30.6 %**) |

Three diagnostics identify the mechanism, and they rule out the obvious explanations.

**It is not that individual loops are asked for impossible flow.** Target per car lane is median
226 veh/h (AM) and 243 veh/h (PM); the maximum over all 55 counting edges is 692 (AM) and 903 (PM).
**Zero of 55 AM edges and one of 55 PM edges ask for more than 900 veh/h/lane** — comfortably below
what a signalised lane passes. The counting locations themselves are individually feasible.

**It is not queueing at the loops.** Grouping counting edges by how well they met target, the ones
that undershoot are *not* more congested than the ones that succeed:

| | mean occupancy | flow-weighted loop speed |
|---|---|---|
| AM, ratio < 0.70 (undershoot, n=18) | 22.3 % | 23.6 km/h |
| AM, ratio ≥ 1.00 (met/over, n=14) | 20.6 % | 27.2 km/h |
| PM, ratio < 0.70 (n=24) | 23.0 % | 24.8 km/h |
| PM, ratio ≥ 1.00 (n=15) | 25.2 % | 25.8 km/h |

The traffic is not stuck *at* the undershooting loops. It never arrives.

**It is a network throughput ceiling.** The decisive evidence is the within-hour profile of total
flow across the 55 counting edges, per 900 s bin:

| bin | AM modelled | AM target/bin | ratio | | PM modelled | PM target/bin | ratio |
|---|---|---|---|---|---|---|---|
| warm-up 1 | 3,443 | 5,678 | 0.61 | | 3,961 | 8,418 | 0.47 |
| warm-up 2 | 5,662 | 5,678 | **1.00** | | 6,201 | 8,418 | 0.74 |
| warm-up 3 | 5,759 | 5,678 | **1.01** | | 5,931 | 8,418 | 0.71 |
| warm-up 4 | 5,707 | 5,678 | **1.01** | | 5,801 | 8,418 | 0.69 |
| graded 1 | 5,743 | 7,762 | 0.74 | | 6,429 | 8,737 | 0.74 |
| graded 2 | 6,122 | 7,762 | 0.79 | | 6,287 | 8,737 | 0.72 |
| graded 3 | 6,217 | 7,762 | 0.80 | | 6,122 | 8,737 | 0.70 |
| graded 4 | 5,989 | 7,762 | 0.77 | | 5,412 | 8,737 | **0.62** |

Read the AM column. In the warm-up hour, whose target is 22,710 veh/h, the network hits it **exactly**
(1.00, 1.01, 1.01) once primed. In the graded hour the target rises 37 % to 31,046 — and delivered
flow rises only 7 % (≈5,700 → ≈6,100 per bin), then **plateaus at 0.77–0.80**. It does not climb
toward target; it is flat. The PM column is worse: already saturated during warm-up, and *declining*
across the graded hour as gridlock deepens (0.74 → 0.62).

**The AM warm-up hour is the control that makes this conclusive.** It uses the *same* calibrated
route set, with the *same* concentration onto instrumented corridors, drawn from the *same* pool by
the *same* routeSampler run — and it reproduces its target to within 1 %. The only thing that changes
between it and the graded hour is the **volume asked for**. So the binding constraint is not
routeSampler's route concentration, nor a cold start, nor the loss of long-route passages past the
window edge: all of those are present in the warm-up hour too, and it succeeded. What fails is
carrying 31,046 veh/h where 22,710 veh/h is carried perfectly. The ceiling sits between those two
numbers.

The network state confirms it. Running vehicles, warm-up start → graded-hour end:

| Run | running (start → end) | mean speed | halting at end | teleports (jam) |
|---|---|---|---|---|
| baseline AM | 5 → 3,768 (**plateaus ~3,800**) | 7.88 m/s | 1,554 | 365 (49) |
| **calibrated AM** | 2 → **6,959, still rising** | 7.16 m/s | 4,026 | 541 (144) |
| baseline PM | 6 → 3,404 (plateaus) | 9.35 m/s | 1,170 | 400 (82) |
| **calibrated PM** | 3 → **9,021, still rising** | 6.32 m/s | 5,737 | **1,016 (400)** |

The baseline reaches steady state. **Neither calibrated run ever does.** Vehicles enter (28,146 of
28,461 loaded, AM) and accumulate. PM mean speed falls to 6.32 m/s with 64 % of vehicles halted.

So: **the InTAS network, driven by InTAS's own route pool, saturates at roughly 24,000–24,500
vehicles/hour summed over these 55 counting edges, against a measured 31,046 (AM) and 34,948 (PM).**
Beyond that ceiling, extra demand converts into queue length, not into counted vehicles. This is the
residual −27 % / −24 %, and no demand calibration can touch it.

---

## 7. Corrections to the documented baseline

**The documented −53.9 % was slightly flattered.** Grading the *identical* detector output from the
run behind `GEH-RESULT.md` (`gehA_InTAS_Detectors_Output.xml`), this tool reads 20,267 modelled where
`sumo_realism.py` read 21,095. The difference is 828 vehicles from **5 InTAS loops that the reference
never measured** — `6030_9` (471), `8005_6` (262), `3022_7` (64), `3100_4_2` (27), `3100_7_2` (4).
Summing modelled loops with no measured counterpart inflates the modelled side. Restricting to the
reference's own `matched_detector_ids` is the correct like-for-like rule, and it makes the as-shipped
AM baseline **−55.7 %**, not −53.9 %.

The baseline runs here reproduce the documented runs exactly at detector level (identical modelled
totals, median GEH 27.072 AM / 30.894 PM), so this is a grading-rule difference, not a run difference.

**The much larger correction is the layout defect of §3**, which the documented result did not
account for at all.

---

## 8. What this does and does not validate

**Validated.** On the 55 fully instrumented counting edges, on days it never saw, the calibrated
demand reproduces measured flow better than InTAS's own demand does, and the improvement is
concentrated precisely where routeSampler had counts to fit. That is a real, transferable gain.

**Not validated — and this list is not a formality.**

1. **The gate is red.** All four FHWA criteria fail in every window. The calibrated scenario must
   **not** be described as reproducing Ingolstadt traffic. It is less wrong, not right.
2. **Routes are not validated.** routeSampler matches *counts at instrumented loops*. Many different
   route sets reproduce the same loop counts. Nothing here constrains which one is correct.
3. **Turning proportions are not validated.** No turn-count data was used. The loops are approach
   counters; how flow splits at each junction is unconstrained.
4. **Uninstrumented roads are not validated.** ~30 % of measured station flow sits on edges with no
   counting constraint, and the entire network away from these 25 junctions has none at all. Station
   8005 demonstrates the consequence directly: no constraint, no improvement.
5. **The route pool is biased by construction.** 6,313 AM / 5,146 PM candidate routes touch no
   counting location and were dropped. The calibrated demand over-represents instrumented corridors.
6. **Vintage.** InTAS demand is calibrated to **November 2019**; every count here is **November
   2023** — a four-year gap spanning COVID. Some of the residual is genuine change, and this exercise
   cannot separate that from model error.
7. **Congestion feedback is unmodelled in the fit.** routeSampler is a static assignment with no
   travel-time model. It cannot know that its own solution gridlocks the network — which is exactly
   what happened.
8. **The throughput ceiling of §6 is measured for this route set.** It is a joint property of the
   network, the signal plans, the car-following model and the (concentrated) calibrated route mix.
   Attributing it to network capacity alone would over-claim.

---

## 9. Reproduce

All stages write a `.meta.json` side-car carrying the sha256 of every input, so the chain
count window → target → route set → simulated detector output is auditable end to end.

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"

# 0. measured counts -- NEVER COMMITTED (licence not formally stated). One per window;
#    4 calibration + 5 held-out. Times are UTC.
python tools/fetch_ingolstadt_counts.py --det-add $S/InTAS_E1.add.xml `
    --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z `
    --include exact+subset --out .cache/calib/ref_20231114_0600Z.json
#    ... likewise 20231116_0600Z, 20231121_0600Z, 20231123_0600Z (AM),
#        20231115_1500Z, 20231116_1500Z, 20231121_1500Z, 20231122_1500Z, 20231123_1500Z (PM),
#        and the warm-up windows 20231114_0500Z, 20231116_0500Z, 20231115_1400Z, 20231116_1400Z

# 1. repair + extend the detector layout
python tools/calibrate_demand.py layout --det-add $S/InTAS_E1.add.xml `
    --net $S/ingolstadt.net.xml --out-dir .cache/calib/layout

# 2. candidate route pool (AM band shown; PM uses --begin 50400 --end 64800)
python tools/calibrate_demand.py candidates --route-files $S/routes/InTAS_0*.rou.xml `
    --begin 18000 --end 32400 --out .cache/calib/cand_am.rou.xml

# 3. count targets from the CALIBRATION windows only
python tools/calibrate_demand.py targets --layout-map .cache/calib/layout/layout_map.json `
    --interval "21600:25200=.cache/calib/ref_20231114_0500Z.json,.cache/calib/ref_20231116_0500Z.json" `
    --interval "25200:28800=.cache/calib/ref_20231114_0600Z.json,.cache/calib/ref_20231116_0600Z.json" `
    --bus-flows $S/routes/BusRoutes.flow.xml --out .cache/calib/targets_am.edg.xml

# 4. routeSampler
python tools/calibrate_demand.py sample --candidates .cache/calib/cand_am.rou.xml `
    --targets .cache/calib/targets_am.edg.xml --out .cache/calib/calibrated_am.rou.xml `
    --begin 21600 --end 28800 --interval 3600 --seed 42 --prefix calAM

# 5. opt-in scenario (leaves InTAS_buildings.sumocfg byte-identical)
./scms-sim/scenarios/make_calibrated_intas.ps1

# 6. the runs -- ~65-125 min each; they may be run in parallel
cd $S
sumo -c InTAS_baseline_calibgrade.sumocfg --begin 21600 --end 28800 --output-prefix bAM_ --seed 42 --no-step-log true
sumo -c InTAS_calibrated_am.sumocfg       --begin 21600 --end 28800 --output-prefix cAM_ --seed 42 --no-step-log true
sumo -c InTAS_baseline_calibgrade.sumocfg --begin 54000 --end 61200 --output-prefix bPM_ --seed 42 --no-step-log true
sumo -c InTAS_calibrated_pm.sumocfg       --begin 54000 --end 61200 --output-prefix cPM_ --seed 42 --no-step-log true
cd ../../../..

# 7. grade -- calibration AND held-out in one invocation; --id-prefix fx_ selects the repaired layout
python tools/calibrate_demand.py grade --det-out .cache/calib/layout/cAM_calib_fixed_det.xml `
    --layout-map .cache/calib/layout/layout_map.json --id-prefix fx_ --begin 25200 --end 28800 `
    --label calibrated-AM `
    --ref ".cache/calib/ref_20231114_0600Z.json=calibration" `
    --ref ".cache/calib/ref_20231116_0600Z.json=calibration" `
    --ref ".cache/calib/ref_20231121_0600Z.json=held-out" `
    --ref ".cache/calib/ref_20231123_0600Z.json=held-out" `
    --json .cache/calib/reports/g_cAM_fixed.json

# 8. calibration vs held-out, and the gap
python tools/calibrate_demand.py report --grade .cache/calib/reports/g_*.json `
    --out .cache/calib/reports/summary.json

# 9. why it fails: per-edge residuals and lane shares
python tools/calibrate_demand.py edges --cov-out .cache/calib/layout/cAM_calib_cov_det.xml `
    --fixed-out .cache/calib/layout/cAM_calib_fixed_det.xml `
    --layout-map .cache/calib/layout/layout_map.json `
    --targets-meta .cache/calib/targets_am.edg.meta.json --begin 25200 --end 28800 `
    --json .cache/calib/reports/e_cAM.json
```

### Input hashes (sha256, first 16 hex)

| File | sha256[:16] |
|---|---|
| `tools/calibrate_demand.py` | `e575e90f40f322e9` |
| `$S/InTAS_E1.add.xml` | `add79329f869b214` |
| `$S/ingolstadt.net.xml` | `9f16fd821d0d1772` |
| `$S/routes/BusRoutes.flow.xml` | `afaac443cb37c656` |
| `.cache/calib/layout/calib_layout.add.xml` | `b9ee00c5efe8157b` |
| `.cache/calib/layout/layout_map.json` | `eac441f22013aeda` |
| `.cache/calib/cand_am.rou.xml` | `10416fa6c8fd0028` |
| `.cache/calib/cand_pm.rou.xml` | `7497b4720626cc43` |
| `.cache/calib/targets_am.edg.xml` | `74508c214638ec50` |
| `.cache/calib/targets_pm.edg.xml` | `4781cd9e05069d89` |
| `.cache/calib/calibrated_am.rou.xml` | `3855d8beca100f28` |
| `.cache/calib/calibrated_pm.rou.xml` | `a8c87a2273bdb584` |

The 22 InTAS route-file hashes are recorded in `.cache/calib/cand_am.rou.meta.json`. The reference
window hashes are recorded in `.cache/calib/targets_*.edg.meta.json` and in each grade report.

---

## 10. Licence and data hygiene

The Stadt Ingolstadt / SAVeNoW loop counts carry **no formally stated licence** (TUM catalogue:
"License Not Specified"). Nothing derived from them is committed. Every stage of
`calibrate_demand.py` that touches counts refuses to write inside the repository unless the
destination is under a git-ignored cache root; the `--i-know` guard exists for a future written
licence and **was not used**. Verified:

- `.cache/` is git-ignored (`.gitignore:41`) — all 13 reference windows, both target files, both
  candidate pools, both calibrated route sets, all detector outputs and all grade reports live there.
- `scms-sim/scenarios/gen_*/` is git-ignored (`.gitignore:27`) — the opt-in sumocfgs are not
  committed either, which is why `make_calibrated_intas.ps1` exists.
- The only tracked files this work adds are this document and that script. Neither contains count
  data; the script writes configuration that *points* at the cache.

## 11. What to do next

1. **Do not present the calibrated scenario as validated.** It fails all four FHWA gates. It is an
   opt-in alternative for work that needs more realistic volumes, with the caveats of §8 attached.
2. **Fix the detector layout upstream.** §3 is a bug in the shipped `InTAS_E1.add.xml`, independent
   of demand, and it has been silently corrupting every loop-based measurement in this repository.
   It is worth reporting to InTAS.
3. **The next question is capacity, not demand.** §6 shows the network cannot carry measured flow.
   Investigate signal plans and junction capacity at the saturating corridors before spending more
   effort on demand.
4. **Get turn counts if the deficit matters.** Loop counts alone cannot constrain routing, and §8.2–3
   will remain open without them.
