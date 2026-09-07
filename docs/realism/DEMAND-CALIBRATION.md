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

*Basis:* components 1 and 2 are differences measured on the **calibration** set (the set on which
the decomposition is defined); component 3 is the **held-out** residual, which is the headline number
of §5. On the AM band the two sets agree to 0.1 pp so the choice is immaterial; on the noisier PM
band the same two components read +19.2 pp and +12.5 pp if measured on the held-out set instead.
Every individual figure is reproduced per-window in Appendix A.

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
are the mean across the windows in that set. **Appendix A grades all 36 windows individually** —
every window's own totals, relative error, median GEH, ratio quartiles and four gate values — since
a mean across a set can hide a window that behaves differently, and the PM held-out set contains
exactly such a window (Wed 22 Nov).

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
The single most informative case is **station 8005, which has no counting edge at all**
(`station_constrained_fraction` 0.00), and it cuts both ways:

| Band | held-out window | baseline ratio | calibrated ratio |
|---|---|---|---|
| AM | 21 Nov / 23 Nov | 0.32 / 0.32 | **0.31 / 0.30** |
| PM | 21 Nov / 22 Nov / 23 Nov | 0.25 / 0.30 / 0.23 | **0.52 / 0.64 / 0.48** |

In the AM band routeSampler changed nothing where it could see nothing — the calibration behaving
exactly as its mathematics says it must. **In the PM band the same unconstrained station roughly
doubles.** That is not a fitting artefact but an *indirect* one: PM candidate routes are much longer
(91.9 edges against 69.9 in the AM pool), so routes selected to satisfy counting edges elsewhere
sweep through 8005's edges as a side effect.

This is worth stating against interest. The AM result alone would read as clean evidence that the
method only moves what it is entitled to move; the PM result shows the calibration also makes large,
**unverifiable** changes to flow at locations where no measurement exists to check them. A doubling
at an unconstrained station could be right or badly wrong, and nothing in this exercise can tell
which. That is a limitation of the method, not a defect of this particular run, and it applies to
the whole ~30 % of measured flow the counting edges do not constrain.

**Does matching some loops push others away? Yes, measurably.** Counting stations whose `|ratio−1|`
fell versus rose, on held-out days (repaired layout, 21 Nov):

| Band | improved | worsened | of |
|---|---|---|---|
| AM held-out 21 Nov | 16 | **7** | 23 |
| PM held-out 21 Nov | 17 | **6** | 23 |

Individual AM movements, largest first: `5060` 1.90 → 1.09 (an overshoot corrected, GEH 19.4 → 2.2),
`8604` 0.49 → 0.95 (GEH 23.3 → 2.1), `4070` 0.57 → 0.87, `1010` 0.43 → 0.68, `4240` 0.78 → 1.00.
Against those, `6030` 0.77 → 0.56 (GEH 9.5 → 18.8) and `3150` 0.69 → 0.55 (GEH 15.9 → 24.2) got
**worse**. In the PM band the calibration also *creates* an overshoot where none existed: `5012`
0.84 → **1.27**, the only held-out station above 1.25× in that window. So the fit is genuinely
over-determined — 55 counting edges against one degree of freedom per candidate route — and
satisfying some counting locations demonstrably costs accuracy at others. It is not a free lunch
that simply scales every station toward its target.

---

## 6. Why the calibration cannot close the deficit

routeSampler solved its problem. SUMO then failed to execute the solution.

| | routeSampler's own accounting | delivered by SUMO at the loops |
|---|---|---|
| AM graded hour | 31,040 / 31,046 (**99.98 %**) | 24,071 / 31,046 (**−22.5 %**) |
| PM graded hour | 34,364 / 34,948 (98.33 %) | 24,250 / 34,948 (**−30.6 %**) |

**Two failure modes could explain a shortfall — the candidate pool being unable to supply the missing
flow, or matching some loops forcing others to overshoot — and the mismatch output distinguishes
them.** routeSampler writes a per-edge residual (`calibrated_*.rou_mismatch.xml`); across all 55
counting edges it reports:

| Band | interval | edges with a non-zero deficit | total deficit | overflow |
|---|---|---|---|---|
| AM | warm-up / graded | 1 / 1 (`26180725#0`) | +4 / +6 veh | **0** |
| PM | warm-up / graded | 1 / 1 (`172517198#0`) | +685 / +584 veh | **0** |

So: *(a)* **the candidate pool can supply the requested flow** almost everywhere — the AM residual is
6 vehicles in 31,046 (0.02 %). The one genuine pool-insufficiency is a single PM edge,
`172517198#0`, short 584 vehicles, and it accounts for the entire PM assignment shortfall (34,948 −
34,364 = 584). *(b)* **Overflow is exactly zero in every interval** — routeSampler never had to
overshoot one counting location in order to satisfy another. At the *assignment* stage there is no
loop-versus-loop conflict at all.

That matters, because the station-level trade-off documented in §5 (7 of 23 AM stations get worse) is
therefore **not** a property of the fit — it appears only after SUMO executes the route set. The
deficit is created between a solved assignment and its execution, which is what the rest of this
section pins down.

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

The network state confirms it. Running vehicles from the warm-up start to the **final step of the
graded hour** (`running`, `halting` and the end-state network mean speed from `summary-output`;
trip-mean speed and teleports from `statistic-output`):

| Run | running (start → end) | peak running | halting at end | mean speed at end | trip-mean speed | teleports (jam) |
|---|---|---|---|---|---|---|
| baseline AM | 5 → 3,921 | 3,944 — **plateau** | 2,089 (53 %) | 4.29 m/s | 7.88 m/s | 365 (49) |
| **calibrated AM** | 2 → **7,843** | 7,843 = the end value — **still rising** | 5,028 (64 %) | 2.90 m/s | 7.16 m/s | 541 (144) |
| baseline PM | 6 → 3,298 | 3,411 — **plateau** | 1,411 (43 %) | 5.49 m/s | 9.35 m/s | 400 (82) |
| **calibrated PM** | 3 → **9,704** | 9,704 = the end value — **still rising** | 6,803 (70 %) | 2.26 m/s | 6.32 m/s | **1,016 (400)** |

The distinction is exact and not a matter of degree: in both baseline runs the peak running count is
reached *before* the end of the graded hour and the network then holds level (AM peaks at 3,944 and
ends at 3,921). In both calibrated runs the peak **is** the final step — the vehicle count is still
climbing when the graded hour ends, so no steady state was ever reached. Vehicles enter (28,146 of
28,461 loaded, AM) and accumulate. In the calibrated PM run 70 % of vehicles are halted at the end
and the instantaneous network mean speed has fallen to 2.26 m/s.

So: **the InTAS network, driven by InTAS's own route pool, saturates at roughly 24,000–24,500
vehicles/hour summed over these 55 counting edges, against a measured 31,046 (AM) and 34,948 (PM).**
Beyond that ceiling, extra demand converts into queue length, not into counted vehicles. This is the
residual −27 % / −24 %, and no demand calibration can touch it.

> **§12 supersedes the mechanism this section infers, and corrects one premise of it.** The ceiling
> is real and §12 measures it per vehicle, but the "decisive control" argument above rests on the
> claim that the warm-up hour has the *same* route concentration as the graded hour. It does not:
> the warm-up assignment peaks at 1,071 veh/h on a single car lane and puts **nothing** above 1,200,
> where the graded assignment reaches **1,778 veh/h/lane** with 24 edges at or above 1,200. The two
> hours differ in feasibility, not only in volume. §12 also shows that what SUMO does with the
> infeasible hour is not queue at the loops but **replace 75 % of the routes**, and that the
> reported 24,071 exists only because it does.

---

## 7. Corrections to the documented baseline

**The documented −53.9 % was slightly flattered.** Grading the *identical* detector output from the
run behind `GEH-RESULT.md` (`gehA_InTAS_Detectors_Output.xml`), this tool reads 20,267 modelled where
`sumo_realism.py` read 21,095. The difference is 828 vehicles from **5 InTAS loops that the reference
never measured** — `6030_9` (471), `8005_6` (262), `3022_7` (64), `3100_4_2` (27), `3100_7_2` (4).
Summing modelled loops with no measured counterpart inflates the modelled side. Restricting to the
reference's own `matched_detector_ids` is the correct like-for-like rule, and it makes the as-shipped
AM baseline **−55.7 %**, not −53.9 %.

The baseline runs here reproduce the documented runs **exactly** at detector level, so this is a
grading-rule difference and not a run difference. Comparing the graded hour of
`gehA_InTAS_Detectors_Output.xml` (the run behind `GEH-RESULT.md`) against
`bAM_InTAS_Detectors_Output.xml` loop by loop:

| Band | documented run | baseline run here | loops differing |
|---|---|---|---|
| AM `25200–28800` | 23,642 over 194 loops | 23,642 over 196 loops | **0** |
| PM `57600–61200` | 22,474 over 194 loops | 22,474 over 196 loops | **0** |

Not one loop differs by a single vehicle (the 196 vs 194 is only the two unnamed InTAS gate counters
that the merged layout also emits). Median GEH is likewise identical: 27.072 AM / 30.894 PM.

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
4. **Uninstrumented roads are not validated, and the calibration still changes them.** ~30 % of
   measured station flow sits on edges with no counting constraint, and the entire network away from
   these 25 junctions has none at all. Station 8005 (zero counting edges) shows both failure modes:
   unchanged in the AM band (0.32 → 0.31) but roughly **doubled** in the PM band (0.23 → 0.48) as a
   side effect of routes fitted elsewhere. Flow at unconstrained locations moves without any
   measurement able to say whether it moved toward reality or away from it.
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

### Independent re-verification

The whole grading chain was re-run from the stored detector outputs into a separate directory
(`.cache/calib/verify/`) and checked against the originals:

- All 12 input hashes above re-computed and matched.
- `layout` re-derived from `InTAS_E1.add.xml` + the net: `calib_layout.add.xml`,
  `InTAS_E1_fixed.add.xml` and `InTAS_E1_cov.add.xml` byte-identical; the `detectors` and `edges`
  maps compare equal (`layout_map.json` differs only in the embedded output paths). Same counts:
  196 detectors / 194 named / 25 stations / 96 edges, 74 → 0 on non-car lanes, 159 moved, 0 problems.
- All **8 grade reports byte-identical** to the stored ones, so all 36 window-gradings reproduce.
- `report` reproduces the same calibration/held-out/gap table.
- The §6 within-hour bin profile recomputed independently from the `fx_` detector output: identical
  to the table above in every cell.
- `make_calibrated_intas.ps1` is idempotent — re-running it leaves all four sumocfgs byte-identical,
  and `InTAS_buildings.sumocfg` unchanged.
- §3's defect measurement re-derived: the 74 flagged loops record **exactly 0** in the baseline AM
  graded hour, and the loops they map to measured 16,673 of 45,713 (36.5 %).
- §4's pool, coverage and bus figures re-derived from the meta side-cars; §6's per-lane target
  intensities (median 226 / 243 veh/h/lane, max 692 / 903) and the occupancy-versus-speed table
  recomputed from the `cov_` and `fx_` outputs — all identical.
- §4's seed check re-read: seeds 7 / 42 / 123 give 28,272 / 28,229 / 28,014 vehicles, all at 99.98 %
  and GEH<5 = 100 %.
- §7's control re-run loop by loop: **0 of 194 loops differ** between the runs behind `GEH-RESULT.md`
  and the baseline runs here, in both bands.
- The recorded routeSampler invocations carry no `--optimize`, `--total-count` or
  `--minimize-vehicles`: the only free parameters were the window, the seed and the targets.

Scripts for each of these live in `.cache/calib/verify/scripts/`.

Three things were **corrected** during that pass: the §6 network-state table (its running/halting
values had been read at arbitrary mid-run timesteps rather than at the end of the graded hour); the
station-8005 discussion in §5 (which quoted only the AM band, where the unconstrained station does
not move, and omitted the PM band, where it roughly doubles); and the §10 claim that this document
contains no count data, which was not true as written.

---

## 10. Licence and data hygiene

The Stadt Ingolstadt / SAVeNoW loop counts carry **no formally stated licence** (TUM catalogue:
"License Not Specified"). **No count data and no per-detector derivative is committed** — only the
aggregate validation statistics this document reports, delimited precisely below. Every stage of
`calibrate_demand.py` that touches counts refuses to write inside the repository unless the
destination is under a git-ignored cache root; the `--i-know` guard exists for a future written
licence and **was not used**. Verified:

- `.cache/` is git-ignored (`.gitignore:41`) — all 13 reference windows, both target files, both
  candidate pools, both calibrated route sets, all detector outputs and all grade reports live there.
- `scms-sim/scenarios/gen_*/` is git-ignored (`.gitignore:27`) — the opt-in sumocfgs are not
  committed either, which is why `make_calibrated_intas.ps1` exists.
- The only tracked files this work adds are this document and that script. The script contains no
  count data at all — it writes configuration that *points* at the cache.

**What this document itself contains, stated precisely.** It is not free of count-derived numbers,
and claiming otherwise would be false. It carries **aggregate validation statistics**: per-window
measured totals over 23 stations, per-station ratios and GEH values, and quartiles of those
distributions. It does **not** reproduce the dataset: no per-detector counts, no 15-minute bins, no
per-station absolute measured volumes, and nothing from which the original series could be
reconstructed. That is the irreducible minimum needed to state a validation result at all — a
validation report that quoted no measured quantity would assert its conclusion without evidence —
and it matches the level of detail already published in `GEH-RESULT.md` and `GEH-VALIDATION.md`.
The raw and per-detector data stay in `.cache/`. If a licence is ever formally stated, this is the
paragraph to revisit.

## 11. What to do next

1. **Do not present the calibrated scenario as validated.** It fails all four FHWA gates. It is an
   opt-in alternative for work that needs more realistic volumes, with the caveats of §8 attached.
2. **Fix the detector layout upstream.** §3 is a bug in the shipped `InTAS_E1.add.xml`, independent
   of demand, and it has been silently corrupting every loop-based measurement in this repository.
   It is worth reporting to InTAS.
3. **The next question is the assignment, not the demand and not (yet) the network.** §12 replaces
   what this item used to say. The binding constraint is that routeSampler's solution puts up to
   1,778 veh/h on single give-way lanes that pass ~515, and that SUMO then replaces the routes of
   75 % of the vehicles rather than driving into it. The next run is a **capacity-constrained
   assignment** — `duaIterate.py`, or `marouter` with volume-delay functions, seeded by the same
   counts and the same held-out split — not a signal-plan investigation and not a demand change.
   Only after that does a residual deficit measure the network.
4. **Anything that reads these runs must know the routes are not the fitted ones.** §12.2. A
   downstream consumer that assumes the calibrated scenario drives routeSampler's assignment is
   wrong about it: only **37.9 %** of the fitted counting-edge passages are driven as assigned.
5. **Get turn counts if the deficit matters.** Loop counts alone cannot constrain routing, and §8.2–3
   will remain open without them.

---

## 12. P2 — where the demand goes between assignment and the loops

**Verdict: the ceiling is genuine, and the mechanism is not the one §6 inferred.** §6 measured a
throughput ceiling by elimination and attributed the shortfall to "the network cannot carry it". A
per-vehicle ledger built from SUMO's own `vehroute-output` says something sharper and less
comfortable: **SUMO does not drive the route set routeSampler solved for.** It replaces the route of
**75.0 %** of the graded-hour cohort before or during the trip, and **44.0 %** of routeSampler's
assigned counting-edge passages are lost to that replacement rather than to congestion. Forcing
SUMO to execute the assignment does not recover them — it gridlocks the city and delivers a third
less. So the ceiling stands, but the 24,071 figure is not "what the network could carry"; it is
what the network carries *after its own rerouting device has escaped an infeasible assignment*.

Everything below is measured on the same seed-42 runs as §5–§6 (`y0AM`/`y0PM` reproduce the §5
detector output exactly: 24,071 AM and 24,250 PM counting-edge passages, identical to `cAM`/`cPM`),
re-run only to add `vehroute-output`, `edgeData` and un-suppressed warnings, none of which perturb
the simulation.

### 12.1 It is not insertion — DISCARDED and DELAYED are different rows, and both are small

The two candidates that would make this a configuration bug rather than a traffic result are
insertion capacity at the boundary and `max-depart-delay` silently deleting vehicles. SUMO reports
each separately. `statistic-output`, verbatim:

| run | `<vehicles …>` | discarded = loaded − inserted − waiting | `departDelay` |
|---|---|---|---|
| **calibrated AM** | `loaded="28461" inserted="28146" running="7843" waiting="44"` | **271 (0.95 %)** | **4.25 s** |
| **calibrated PM** | `loaded="30838" inserted="30412" running="9704" waiting="166"` | **260 (0.84 %)** | 6.66 s |
| baseline AM | `loaded="28883" inserted="25271" running="3921" waiting="240"` | **3,372 (11.7 %)** | **24.26 s** |
| baseline PM | `loaded="24280" inserted="21305" running="3298" waiting="212"` | **2,763 (11.4 %)** | 25.23 s |

Per vehicle, over the 16,655 calibrated cars departing in the AM graded hour, the delay between the
departure routeSampler wrote and the departure SUMO achieved is **median 0.05 s, mean 6.0 s, p95
16.6 s, max 300.04 s — and exactly one vehicle of 16,655 reaches the 300 s `max-depart-delay` cap.**
Set against the route file vehicle by vehicle, **315** of the 28,229 calibrated AM cars never appear
in `vehroute-output` at all (exactly SUMO's `loaded − inserted`); 44 of those are the ones still
listed as `waiting` at the horizon and the other 271 are the discards. **266** of the 315 belong to
the graded-hour cohort, and they account for **718 of its 31,040 assigned counting passages
(2.3 %)**.
`--no-warnings false` produces 3,800 warning lines in the AM run and **not one is an aborted
departure**; they are 2,686 emergency-braking, 526 end-of-teleport, 352 yield teleports, 144 jam
teleports, 44 wrong-lane teleports, 24 "no connection to the next edge" emergency stops and 8
red-light emergency stops.

So the calibrated scenario does **not** refuse to insert its demand. The *baseline* partly does —
11.7 % of InTAS's own AM vehicles are discarded and its mean departure delay is 5.7× the calibrated
run's — which means a slice of the documented −53.9 %/−55.7 % baseline deficit is a configuration
limit and not demand at all. That is a separate finding, and it makes the baseline worse as a
reference point, not better.

### 12.2 The ledger: what happened to every assigned counting-edge passage

`vehroute-output` with `--vehroute-output.exit-times --vehroute-output.write-unfinished` records,
per vehicle, the edge sequence actually driven and the time it left each edge. Matching that against
the route routeSampler assigned classifies every one of the 31,040 assigned AM passages into exactly
one bucket (`tools/demand_ceiling.py ledger`):

| | AM warm-up | **AM graded** | PM warm-up | **PM graded** |
|---|---|---|---|---|
| assigned to the cohort by routeSampler | 22,706 | **31,040** | 32,989 | **34,364** |
| driven as assigned, inside the window | 12,720 (56.0 %) | **11,774 (37.9 %)** | 10,895 (33.0 %) | **8,055 (23.4 %)** |
| **lost — SUMO replaced the route** | 7,764 (34.2 %) | **13,668 (44.0 %)** | 18,185 (55.1 %) | **18,947 (55.1 %)** |
| lost — vehicle had not reached it at the horizon | 5 (0.0 %) | 4,880 (15.7 %) | 353 (1.1 %) | 6,430 (18.7 %) |
| lost — passage fell after the window | 2,047 (9.0 %) | 0 | 3,425 (10.4 %) | 0 |
| lost — vehicle never inserted | 170 (0.7 %) | 718 (2.3 %) | 131 (0.4 %) | 932 (2.7 %) |
| credit — driven onto a counting edge it was *not* assigned | +7,700 | +8,698 | +10,893 | +8,605 |
| credit — spillover from the previous hour's departures | +0 | +3,472 | +0 | +7,547 |
| **= counting passages inside the window** | **20,548** | **24,104** | **21,916** | **24,330** |
| the loops' own count for the same window | 20,571 | 24,071 | 21,894 | 24,250 |

The ledger closes against the detector output to **0.11 %, 0.14 %, 0.10 % and 0.33 %** — it is an
accounting of the same vehicles, not a model of them.

Read the graded AM column. **Only 37.9 % of routeSampler's solution is executed.** The single
largest term is not queueing, not insertion and not the horizon: it is `device.rerouting` replacing
the route. 12,488 of the 16,655 graded-hour vehicles (**75.0 %**) have at least one route
replacement; SUMO records each one in the `vehroute` output as a `<routeDistribution>` whose first
entry carries `reason="device.rerouting"`. A raw structural scan of that file, independent of the
ledger's matching logic, finds a `<routeDistribution>` on **20,676 of the 28,146 vehicles inserted
across both AM hours (73.5 %)** — the same answer from the other direction. The shipped InTAS config
assigns the device with
`device.rerouting.probability 0.82`, re-optimises before insertion, and repeats every
`device.rerouting.period 300` seconds against live travel times.

Note what the two credit rows mean. Rerouting is not deleting flow, it is *moving* it: 8,698
passages arrive on counting edges the vehicle was never assigned to. In the warm-up hour the two
deviation terms nearly cancel (−7,764 against +7,700, a net −64 on 22,706). In the graded hour they
do not (−13,668 against +8,698, a net **−4,970**). Rerouting is neutral while the corridors are
free and systematically *away* from them once they are not — which is exactly what a travel-time
router is supposed to do, and exactly what destroys a count-fitted assignment.

### 12.3 The counterfactual — make SUMO execute the assignment

`calibrate_demand.py scenario --execute-assigned-routes` writes `device.rerouting.period 0` and
`.pre-period 0`, which is the only way to make SUMO drive the routes it was calibrated on. The
ledger confirms the switch bites: **route replacements 0, route-deviation loss 0**. The rest of the
run is the answer to whether rerouting was hiding a deliverable flow:

| AM graded hour | as shipped (`y0AM`) | assignment executed (`y1AM`) |
|---|---|---|
| passages lost to route replacement | 13,668 (44.0 %) | **0** |
| passages not reached at the horizon | 4,880 (15.7 %) | **13,176 (42.4 %)** |
| vehicles never inserted | 315 | **2,255** |
| teleports (jam) | 541 (144) | **3,011 (1,322)** |
| **delivered at the 55 counting edges** | **24,071 (−22.5 %)** | **17,395 (−44.0 %)** |
| PM equivalent | 24,250 (−30.6 %) | **11,078 (−68.3 %)**, 8,614 teleports (3,366 jam) |

**The rerouting device is the only reason the calibrated scenario delivers 24,071 rather than
17,395.** It is a mitigation of an infeasible assignment, not the cause of the deficit. Both facts
matter: the calibration's headline gain is real *and* it is achieved by a route set SUMO overwrites.

### 12.4 The network is at its production ceiling, and `edgeData` shows the shape of it

Network-wide `edgeData` at 900 s, summed as vehicle-kilometres (production) against vehicle-hours
(accumulation) — the network fundamental diagram, measured:

| bin | as shipped: veh-km | veh-h | mean m/s | | executed: veh-km | veh-h | mean m/s |
|---|---|---|---|---|---|---|---|
| warm-up 3 | 17,333 | 685.5 | 7.02 | | 17,351 | 691.6 | 6.97 |
| warm-up 4 | 17,880 | 733.8 | 6.77 | | 16,603 | 781.8 | 5.90 |
| graded 1 | 19,678 | 901.3 | 6.07 | | 17,432 | 1,034.4 | 4.68 |
| graded 2 | 22,175 | 1,227.6 | 5.02 | | 17,415 | 1,466.7 | 3.30 |
| graded 3 | 22,964 | 1,494.5 | 4.27 | | 15,306 | 1,834.0 | 2.32 |
| graded 4 | **22,848** | **1,718.4** | **3.69** | | **13,061** | **2,174.0** | **1.67** |

As shipped, production rises to 22,964 veh-km per 900 s and then stops rising while accumulation
grows 2.3× and mean speed halves: the saturated branch. With the assignment executed, production
*falls* 17,432 → 13,061 while accumulation keeps climbing and 1,073 edges sit below 20 % of their
free-flow speed: the gridlock branch. The PM pair has the same shape and is worse (as shipped
23,084 → 23,658 veh-km, flat, ending at 2.97 m/s; executed 19,962 → 10,647 veh-km at **0.855 m/s**
with 1,599 jammed edges).

### 12.5 Why: the graded-hour assignment is infeasible on 24 links, and the warm-up hour's is not

§6's "decisive control" — same route set, same concentration, only the volume differs — is wrong on
the middle term. Per-lane assigned flow, counted over the routes routeSampler wrote for each hour
(`tools/demand_ceiling.py bottleneck`; car lanes only, sidewalk lanes excluded):

| assigned veh/h per **car lane** | AM warm-up: edges | AM graded: edges | PM graded: edges |
|---|---|---|---|
| < 450 | 6,390 | 6,254 | 6,160 |
| 450–700 | 161 | 351 | 454 |
| 700–900 | 23 | 61 | 114 |
| 900–1,200 | 14 | 63 | 41 |
| **≥ 1,200** | **0** | **24** | **15** |
| **maximum** | **1,071** | **1,778** | **1,738** |
| cohort that must cross an edge ≥ 900 veh/h/lane | 29.4 % | **58.0 %** | **66.6 %** |
| assigned counting passages on such a route | 36.6 % | **61.9 %** | **71.7 %** |

The worst AM link is `653473569#3`: one car lane (its lane 0 is a sidewalk), 77.7 m of
`highway.tertiary` at 13.89 m/s, ending at junction `274041341`, which is a **`priority`** — an
unsignalised give-way — junction. What SUMO does with it:

| | assigned | entered | speed | occupancy | waiting/bin at the end |
|---|---|---|---|---|---|
| AM warm-up hour | 1,071 | **516** | 9.59 → 2.15 m/s | 7.2 → 34.7 % | 3,082 s |
| AM graded hour | 1,778 | **513** | 2.52 → 1.31 m/s | 37.0 → 54.6 % | 5,387 s |

**Its throughput is invariant at ≈515 veh/h while the demand placed on it rises 66 %.** That is a
directly measured link capacity, and routeSampler assigns it 3.45×. `653473569#2` behind it reads
502 / 479 on 1,018 / 1,677 assigned. The PM twin `201963533#4` is worse: 805 vehicles at 9.61 m/s in
the warm-up hour and **212 at 0.47 m/s** in the graded hour on essentially the same assigned load —
a capacity *drop*, the link's discharge collapsing once it gridlocks.

What the 87 overloaded AM edges have in common says where the ceiling physically sits:

| of the 87 edges assigned ≥ 900 veh/h/car lane | |
|---|---|
| have **one** car lane | **84** |
| end at an unsignalised **`priority`** (give-way) junction | **81** (4 at a traffic light, 1 `right_before_left`, 1 `zipper`) |
| road class | 48 `highway.tertiary`, 35 `highway.secondary`, 4 other |
| **are one of the 55 counting edges** | **0** |
| assigned passages / actually entered in the hour | **100,660 / 53,644** — served fraction median **0.521**, min 0.215 |

The counting edges themselves are not the constraint and never were (§6: max target 692 veh/h/lane
AM, and the undershooting loops run at 22 % occupancy) — **not one of the 87 is a counting edge**.
The constraint is on the **paths to them**: 62 % of the graded hour's assigned counting passages
ride a route that must first cross a single-lane give-way link the assignment overloads by a median
factor of two. The warm-up hour succeeds because its assignment is feasible — nothing above
1,200 veh/h/lane — not because it is smaller.

### 12.6 Re-graded, calibration and held-out reported separately

Re-measured on the runs above, repaired layout, `--id-prefix fx_`, the same nine reference windows,
the held-out windows still untouched by any fitting:

| run | set | n | modelled | **rel. error** | **median GEH** | GEH<5 | in FHWA tol. |
|---|---|---|---|---|---|---|---|
| **calibrated AM, as shipped** | calibration | 2 | 32,885 | −27.0 % | 14.86 | 0.348 | 0.348 |
| **calibrated AM, as shipped** | **held-out** | 2 | 32,885 | **−27.1 %** | **14.07** | 0.348 | 0.348 |
| calibrated AM, assignment executed | calibration | 2 | 23,355 | −48.1 % | 22.07 | 0.000 | 0.000 |
| calibrated AM, assignment executed | **held-out** | 2 | 23,355 | **−48.2 %** | **22.38** | 0.000 | 0.022 |
| **calibrated PM, as shipped** | calibration | 2 | 33,094 | −31.9 % | 16.11 | 0.130 | 0.152 |
| **calibrated PM, as shipped** | **held-out** | 3 | 33,094 | **−24.1 %** | **11.74** | 0.188 | 0.232 |
| calibrated PM, assignment executed | calibration | 2 | 14,999 | −69.1 % | 38.79 | 0.000 | 0.000 |
| calibrated PM, assignment executed | **held-out** | 3 | 14,999 | **−65.6 %** | **33.74** | 0.029 | 0.029 |

The as-shipped rows reproduce §5 to the digit. The calibration-to-held-out gap stays inside the
reference's own day-to-day spread in every variant (AM +0.2 pp and +0.1 pp against a 1.5 % noise
floor; PM +7.8 pp and +3.5 pp against 8.6 %), so nothing here was fitted to the test set. **All four
FHWA gates still fail in every window**, and the executed-assignment variant fails them by two to
three times the margin. `--execute-assigned-routes` is therefore a **diagnostic switch, not a
scenario**: it is the honest way to run the routes that were calibrated, and it is much less like
Ingolstadt. The shipped default stays as it is, and `InTAS_buildings.sumocfg` remains untouched.

### 12.7 What this does and does not establish about InTAS

**Established.** Loading 31,046 veh/h through these 55 counting edges *along routeSampler's paths*
drives the InTAS network past its production ceiling: production flattens at ~22,900 veh-km per
900 s while accumulation doubles, and the individual links carrying the concentration meter at a
measured ~515 veh/h against 1,778 assigned. No configuration change recovers the flow — the two
knobs that could have (`max-depart-delay`, and the rerouting device) account for 2.3 % and are
already load-bearing in the other direction.

**Not established, and this is the part that must not be over-claimed.** This is *not* a measurement
that InTAS cannot carry Ingolstadt's measured traffic. It is a measurement that InTAS cannot carry
*this assignment*, and the assignment is demonstrably infeasible by construction: routeSampler is a
static sampler with no capacity model and no travel-time feedback (§8.7), it samples uniformly from
a pool of modal routes that share long sub-paths, and nothing in it prevents 1,778 veh/h being put
on one give-way lane. The honest next step is not a longer warm-up or a looser `max-depart-delay`
but a **capacity-constrained assignment** — an equilibrium method (`duaIterate.py`, or `marouter`
with volume-delay functions) seeded by the same counts — after which a residual ceiling would mean
what §6 claims this one means. Until that is run, "the network cannot carry the measured flow"
remains one plausible reading of a number at least as well explained by "the route set cannot be
driven".

**Also not established: that the §5 improvement comes from where §5 says it does.** The gain is real
and it transfers to unseen days, but only 37.9 % of the assigned counting-edge passages are actually
driven; 44 % are replaced by SUMO's own shortest paths, and 8,698 passages arrive on counting edges
the vehicle was never assigned to. §8.2's "routes are not validated" is stronger than it reads: the
routes SUMO drives are not the routes that were fitted.

### 12.8 Reproduce

All of §12 runs off two SUMO runs per band and one new tool, `tools/demand_ceiling.py`. Nothing
count-derived is committed; every artefact stays under the git-ignored `.cache/calib/`. The route
sets and the network are the §9 ones unchanged — `calibrated_am.rou.xml` `3855d8beca100f28`,
`calibrated_pm.rou.xml` `a8c87a2273bdb584`, `ingolstadt.net.xml` `9f16fd821d0d1772` — plus
`tools/demand_ceiling.py` `47defef16a81e223` and the passive probe file
`.cache/calib/probe/edgedata.add.xml` `188fdd92e99521d5` (sha256, first 16 hex).

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$S = "scms-sim/scenarios/gen_intas_urban_low/sumo"; $C = ".cache/calib"

# 0. the passive probe additional-file: network-wide 900 s edgeData
#    (.cache/calib/probe/edgedata.add.xml -- output only, no effect on dynamics)

# 1. the two AM runs -- identical except for the rerouting device
cd $S
sumo -c InTAS_calibrated_am.sumocfg --begin 21600 --end 28800 --output-prefix y0AM_ --seed 42 `
     --no-step-log true --no-warnings false `
     --additional-files "BusStations.add.xml,../../../../.cache/calib/layout/calib_layout.add.xml,buildings.poly.xml,../../../../.cache/calib/probe/edgedata.add.xml" `
     --vehroute-output ../../../../.cache/calib/probe/vehroute.xml `
     --vehroute-output.exit-times true --vehroute-output.write-unfinished true `
     --tripinfo-output.write-unfinished true 2> ../../../../.cache/calib/probe/y0AM_err.log
#    y1AM_ is the same line plus:  --device.rerouting.period 0 --device.rerouting.pre-period 0
#    (or generate the cfg with: calibrate_demand.py scenario --execute-assigned-routes)
cd ../../../..

# 2. the per-vehicle ledger, and its cross-check against the loops
python tools/demand_ceiling.py ledger --targets-meta $C/targets_am.edg.meta.json `
    --routes $C/calibrated_am.rou.xml --vehroute $C/probe/y0AM_vehroute.xml `
    --begin 25200 --end 28800 --json $C/probe/ledger_am_graded.json
python tools/calibrate_demand.py edges --cov-out $C/layout/y0AM_calib_cov_det.xml `
    --fixed-out $C/layout/y0AM_calib_fixed_det.xml --layout-map $C/layout/layout_map.json `
    --targets-meta $C/targets_am.edg.meta.json --begin 25200 --end 28800

# 3. network production per 900 s bin, and the worst links
python tools/demand_ceiling.py network --edgedata $C/probe/y0AM_edgedata900_all.xml `
    --targets-meta $C/targets_am.edg.meta.json --top 12

# 4. is the ASSIGNMENT itself feasible?  run it for BOTH hours and compare
python tools/demand_ceiling.py bottleneck --net $S/ingolstadt.net.xml `
    --routes $C/calibrated_am.rou.xml --edgedata $C/probe/y0AM_edgedata900_all.xml `
    --targets-meta $C/targets_am.edg.meta.json --begin 21600 --end 25200
python tools/demand_ceiling.py bottleneck --net $S/ingolstadt.net.xml `
    --routes $C/calibrated_am.rou.xml --edgedata $C/probe/y0AM_edgedata900_all.xml `
    --targets-meta $C/targets_am.edg.meta.json --begin 25200 --end 28800

# 5. re-grade -- calibration and held-out in one invocation, then the gap
python tools/calibrate_demand.py grade --det-out $C/layout/y0AM_calib_fixed_det.xml `
    --layout-map $C/layout/layout_map.json --id-prefix fx_ --begin 25200 --end 28800 `
    --label y0AM --ref "$C/ref_20231114_0600Z.json=calibration" `
    --ref "$C/ref_20231116_0600Z.json=calibration" --ref "$C/ref_20231121_0600Z.json=held-out" `
    --ref "$C/ref_20231123_0600Z.json=held-out" --json $C/probe/g_y0AM_fixed.json
python tools/calibrate_demand.py report --grade $C/probe/g_y0AM_fixed.json `
    $C/probe/g_y1AM_fixed.json $C/probe/g_y0PM_fixed.json $C/probe/g_y1PM_fixed.json `
    --out $C/probe/summary_p2.json
```

---

## Appendix A — every window, graded individually

The tables of §5 report the mean across the windows of each set. This appendix reports **each of the 18 window-gradings per layout on its own**, which is what the four FHWA gates are actually evaluated on. `n` is 23 comparable stations throughout; modelled is one number per run (the same simulated hour is graded against every reference day).


### Repaired layout — the honest instrument

| Run | Set | Window (`15:00–16:00Z` = PM, `06:00–07:00Z` = AM) | modelled | measured | rel. err | med GEH | GEH<5 | min | p25 | med | p75 | max | <0.75× | >1.25× | gates |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| baseline-AM | calibration | Thu 16 Nov `0600Z` | 29,098 | 44,370 | -34.4 % | 16.69 | 3/23 | 0.31 | 0.54 | 0.62 | 0.82 | 2.06 | 14 | 1 | **0/4** |
| baseline-AM | calibration | Tue 14 Nov `0600Z` | 29,098 | 45,713 | -36.4 % | 17.68 | 3/23 | 0.32 | 0.53 | 0.63 | 0.78 | 1.60 | 16 | 1 | **0/4** |
| baseline-AM | **held-out** | Thu 23 Nov `0600Z` | 29,098 | 45,522 | -36.1 % | 15.16 | 5/23 | 0.32 | 0.53 | 0.65 | 0.79 | 1.11 | 15 | 0 | **0/4** |
| baseline-AM | **held-out** | Tue 21 Nov `0600Z` | 29,098 | 44,724 | -34.9 % | 16.46 | 3/23 | 0.32 | 0.56 | 0.67 | 0.76 | 1.90 | 15 | 1 | **0/4** |
| calibrated-AM | calibration | Thu 16 Nov `0600Z` | 32,885 | 44,370 | -25.9 % | 14.19 | 8/23 | 0.30 | 0.60 | 0.74 | 0.97 | 1.17 | 12 | 0 | **0/4** |
| calibrated-AM | calibration | Tue 14 Nov `0600Z` | 32,885 | 45,713 | -28.1 % | 15.52 | 8/23 | 0.30 | 0.59 | 0.71 | 0.91 | 1.04 | 12 | 0 | **0/4** |
| calibrated-AM | **held-out** | Thu 23 Nov `0600Z` | 32,885 | 45,522 | -27.8 % | 15.02 | 8/23 | 0.30 | 0.59 | 0.70 | 0.92 | 1.11 | 13 | 0 | **0/4** |
| calibrated-AM | **held-out** | Tue 21 Nov `0600Z` | 32,885 | 44,724 | -26.5 % | 13.13 | 8/23 | 0.31 | 0.61 | 0.76 | 0.94 | 1.12 | 11 | 0 | **0/4** |
| baseline-PM | calibration | Thu 16 Nov `1500Z` | 27,619 | 48,988 | -43.6 % | 22.76 | 0/23 | 0.23 | 0.52 | 0.58 | 0.65 | 1.46 | 20 | 1 | **0/4** |
| baseline-PM | calibration | Wed 15 Nov `1500Z` | 27,619 | 48,169 | -42.7 % | 21.37 | 1/23 | 0.23 | 0.49 | 0.61 | 0.69 | 1.32 | 20 | 1 | **0/4** |
| baseline-PM | **held-out** | Thu 23 Nov `1500Z` | 27,619 | 42,735 | -35.4 % | 14.41 | 3/23 | 0.23 | 0.57 | 0.68 | 0.80 | 1.03 | 15 | 0 | **0/4** |
| baseline-PM | **held-out** | Tue 21 Nov `1500Z` | 27,619 | 49,034 | -43.7 % | 20.52 | 1/23 | 0.25 | 0.50 | 0.58 | 0.69 | 1.08 | 19 | 0 | **0/4** |
| baseline-PM | **held-out** | Wed 22 Nov `1500Z` | 27,619 | 39,950 | -30.9 % | 10.74 | 8/23 | 0.30 | 0.55 | 0.77 | 0.95 | 1.20 | 10 | 0 | **0/4** |
| calibrated-PM | calibration | Thu 16 Nov `1500Z` | 33,094 | 48,988 | -32.4 % | 16.75 | 3/23 | 0.45 | 0.54 | 0.66 | 0.83 | 1.29 | 13 | 1 | **0/4** |
| calibrated-PM | calibration | Wed 15 Nov `1500Z` | 33,094 | 48,169 | -31.3 % | 15.47 | 3/23 | 0.45 | 0.56 | 0.69 | 0.80 | 1.44 | 14 | 1 | **0/4** |
| calibrated-PM | **held-out** | Thu 23 Nov `1500Z` | 33,094 | 42,735 | -22.6 % | 12.25 | 3/23 | 0.48 | 0.63 | 0.78 | 0.88 | 1.42 | 10 | 2 | **0/4** |
| calibrated-PM | **held-out** | Tue 21 Nov `1500Z` | 33,094 | 49,034 | -32.5 % | 15.31 | 2/23 | 0.44 | 0.53 | 0.68 | 0.83 | 1.27 | 14 | 1 | **0/4** |
| calibrated-PM | **held-out** | Wed 22 Nov `1500Z` | 33,094 | 39,950 | -17.2 % | 7.65 | 8/23 | 0.46 | 0.65 | 0.90 | 1.11 | 1.82 | 9 | 2 | **0/4** |

### As-shipped layout — for continuity with `GEH-RESULT.md`

| Run | Set | Window (`15:00–16:00Z` = PM, `06:00–07:00Z` = AM) | modelled | measured | rel. err | med GEH | GEH<5 | min | p25 | med | p75 | max | <0.75× | >1.25× | gates |
|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| baseline-AM | calibration | Thu 16 Nov `0600Z` | 20,267 | 44,370 | -54.3 % | 25.70 | 1/23 | 0.12 | 0.34 | 0.46 | 0.60 | 0.95 | 21 | 0 | **0/4** |
| baseline-AM | calibration | Tue 14 Nov `0600Z` | 20,267 | 45,713 | -55.7 % | 27.07 | 0/23 | 0.12 | 0.33 | 0.47 | 0.57 | 0.74 | 23 | 0 | **0/4** |
| baseline-AM | **held-out** | Thu 23 Nov `0600Z` | 20,267 | 45,522 | -55.5 % | 25.88 | 0/23 | 0.12 | 0.35 | 0.46 | 0.55 | 0.76 | 22 | 0 | **0/4** |
| baseline-AM | **held-out** | Tue 21 Nov `0600Z` | 20,267 | 44,724 | -54.7 % | 26.91 | 1/23 | 0.12 | 0.35 | 0.49 | 0.57 | 0.88 | 22 | 0 | **0/4** |
| calibrated-AM | calibration | Thu 16 Nov `0600Z` | 22,458 | 44,370 | -49.4 % | 27.16 | 1/23 | 0.16 | 0.39 | 0.50 | 0.68 | 0.88 | 20 | 0 | **0/4** |
| calibrated-AM | calibration | Tue 14 Nov `0600Z` | 22,458 | 45,713 | -50.9 % | 27.05 | 1/23 | 0.17 | 0.39 | 0.49 | 0.61 | 0.89 | 21 | 0 | **0/4** |
| calibrated-AM | **held-out** | Thu 23 Nov `0600Z` | 22,458 | 45,522 | -50.7 % | 27.82 | 1/23 | 0.17 | 0.36 | 0.47 | 0.63 | 0.95 | 21 | 0 | **0/4** |
| calibrated-AM | **held-out** | Tue 21 Nov `0600Z` | 22,458 | 44,724 | -49.8 % | 25.09 | 1/23 | 0.17 | 0.39 | 0.51 | 0.61 | 0.93 | 21 | 0 | **0/4** |
| baseline-PM | calibration | Thu 16 Nov `1500Z` | 19,269 | 48,988 | -60.7 % | 32.88 | 0/23 | 0.06 | 0.32 | 0.40 | 0.49 | 0.71 | 23 | 0 | **0/4** |
| baseline-PM | calibration | Wed 15 Nov `1500Z` | 19,269 | 48,169 | -60.0 % | 30.89 | 0/23 | 0.06 | 0.33 | 0.41 | 0.50 | 0.64 | 23 | 0 | **0/4** |
| baseline-PM | **held-out** | Thu 23 Nov `1500Z` | 19,269 | 42,735 | -54.9 % | 25.30 | 0/23 | 0.06 | 0.39 | 0.45 | 0.52 | 0.79 | 22 | 0 | **0/4** |
| baseline-PM | **held-out** | Tue 21 Nov `1500Z` | 19,269 | 49,034 | -60.7 % | 31.94 | 0/23 | 0.07 | 0.33 | 0.40 | 0.49 | 0.62 | 23 | 0 | **0/4** |
| baseline-PM | **held-out** | Wed 22 Nov `1500Z` | 19,269 | 39,950 | -51.8 % | 22.37 | 0/23 | 0.09 | 0.37 | 0.49 | 0.63 | 0.86 | 20 | 0 | **0/4** |
| calibrated-PM | calibration | Thu 16 Nov `1500Z` | 22,004 | 48,988 | -55.1 % | 31.81 | 1/23 | 0.19 | 0.36 | 0.43 | 0.54 | 0.92 | 21 | 0 | **0/4** |
| calibrated-PM | calibration | Wed 15 Nov `1500Z` | 22,004 | 48,169 | -54.3 % | 30.14 | 1/23 | 0.20 | 0.39 | 0.43 | 0.53 | 0.92 | 22 | 0 | **0/4** |
| calibrated-PM | **held-out** | Thu 23 Nov `1500Z` | 22,004 | 42,735 | -48.5 % | 24.83 | 0/23 | 0.18 | 0.47 | 0.51 | 0.61 | 0.82 | 22 | 0 | **0/4** |
| calibrated-PM | **held-out** | Tue 21 Nov `1500Z` | 22,004 | 49,034 | -55.1 % | 27.77 | 0/23 | 0.20 | 0.36 | 0.42 | 0.55 | 0.81 | 22 | 0 | **0/4** |
| calibrated-PM | **held-out** | Wed 22 Nov `1500Z` | 22,004 | 39,950 | -44.9 % | 20.12 | 2/23 | 0.25 | 0.40 | 0.52 | 0.76 | 1.16 | 17 | 0 | **0/4** |

**Every one of the 36 window-gradings fails all four FHWA gates.** The best value reached by any window on any gate:

| Gate | threshold | best over all 36 windows | still fails by |
|---|---|---|---|
| `geh.link_pass_fraction` | ≥ 0.85 | 0.3478 | 0.50 |
| `geh.total_flow_geh` | ≤ 4.0 | 35.88 | 9× |
| `geh.total_flow_rel_error` | ≤ 0.05 | 0.1716 | 3.4× |
| `geh.link_flow_tolerance_pass_fraction` | ≥ 0.85 | 0.3478 | 0.50 |

The two best-case rows are both `calibrated-PM` against **Wed 22 Nov**, the lightest day in the reference (39,950 vehicles against ~48,600 on the calibration days) — that is, the model comes closest to reality on the day reality came closest to the model.
