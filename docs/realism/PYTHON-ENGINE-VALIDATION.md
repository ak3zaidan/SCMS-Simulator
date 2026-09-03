# The Python engine, measured against reality

**This is the first comparison in this repository between the engine that writes the datasets and a
real-world measurement.** Everything in `GEH-RESULT.md` graded the MOSAIC/Java path. The Python
`mock_pipeline` — the producer of every `datasets/` directory a researcher would actually train on —
had never been compared to anything measured, because it emits vehicle POSITIONS and the grading
tool reads SUMO's `<e1Detector>` output, which only SUMO writes.

Two things close that gap:

* **`tools/engine_detectors.py`** (new) counts induction-loop crossings GEOMETRICALLY from
  trajectories and writes an E1-shaped XML, so `tools/sumo_realism.py --ref-counts` grades the
  Python engine with the same station aggregation, the same FHWA criteria and the same reference
  file, unchanged and unaware of the producer.
* **Four default-inert options on `sumo_trace.freeze()`** — `warmup_steps`, `substeps`,
  `time_to_teleport`, `split_on_gap` — without which a real city's peak hour cannot be frozen at
  all. Each was forced by a measured failure, not by taste; see section 6.

**KEEP TWO QUESTIONS APART.** *Is the mobility adapter faithful?* and *is the demand right?* are
different questions with different answers. The adapter is measured here and it is exact. The demand
is a known, separately documented deficit being calibrated in another workstream, and every count
number below inherits it.

---

## 1. The headline

<!--HOUR-HEADLINE-->

---

## 2. The adapter is bit-exact, and that is measured, not asserted

Same run, two files: the frozen SUMO artifact and the engine's own ground truth.

| | |
|---|---|
| Scenario | InTAS `InTAS_buildings.sumocfg`, 0–300 s, SUMO 1.25.0, seed 42, `--step-length 0.1` |
| Frozen artifact | `intas_300s_dt01.trace`, sha256 `5bfd4791762195cd0fa84855a9d349e0b36a89b05627a072e562a4bac9c8e0ec` — **334 vehicles, 781,575 vehicle-steps**, 0 teleports, 0 collisions |
| Engine dataset | `datasets/py_intas_300s`, `emit_sample_prob=1.0` — 334 vehicles, **709,454 emissions** |
| **Position agreement** | **max \|Δx\|, \|Δy\| = 0.000 m** over all 709,454 matched samples |
| **Speed agreement** | **max \|Δv\| = 0.000 m/s** |

Every emitted ground-truth sample is the frozen SUMO state, to the artifact's 3-decimal
quantisation. There is no drift, no interpolation and no re-integration: the adapter is a pass-through.

Two things the comparison also pins down, neither of which is drift:

**The time LABEL is one step behind SUMO's own clock.** The engine's step loop is `t = step * dt`
and `_replay.advance(active_list, step, t)` hands it trace step `step`; the artifact's own
`step0_sim_time` declares that trace step *k* is SUMO time `begin + (k+1)*dt`. So engine time *t*
carries the state SUMO stamped `t + dt`. Aligning on `k = round(t/dt)` matches all 709,454 samples
exactly; `k = round(t/dt) - 1` leaves a mean 1.102 m residual, which is one 0.1 s step of motion.
Irrelevant to an hour-long count, load-bearing if you align a dataset against a SUMO output file.

**9.23% of the vehicle-steps are missing, and enforcement is why.** 781,575 − 709,454 = 72,121
absent samples, and they belong to exactly **27 vehicles — precisely the 27 the MA revoked**
(`gt_linkage_revocation.jsonl` has 27 rows; all 27 appear in the dataset). A revoked vehicle stops
broadcasting (`enforced(tx, t)` skips it in the broadcast pre-pass), so its kinematic record ends at
revocation while it keeps driving in the simulation. The truncation is not marginal per vehicle:
a revoked vehicle's record spans **15.90 s on average against 229.59 s** for a vehicle that is never
revoked — 160 emissions against 2,297. That is the SCMS layer working, but it means **a traffic
count taken from the dataset undercounts by the enforced fraction** — measured at −7.3% on the 300 s
window (228 crossings from the dataset against 246 from the trace). Anyone measuring flow from
`gt_emissions_sample.jsonl` needs to know that. **At the peak hour this term stops being a footnote
and becomes the largest single difference between the dataset and the traffic it replays; see
section 1.**

---

## 3. The crossing counter, and what it costs

`tools/engine_detectors.py` reduces each `<e1Detector>` to a gate on its own lane: the point *P* at
the detector's `pos` along the lane polyline, the unit tangent *T* there, and a half-width
*W* = min(1.6 m, half the lane). One sample pair (A, B) of one vehicle counts once when
`(A−P)·T < 0 ≤ (B−P)·T` (half-open, so a vehicle stopped on the loop is counted once), the
interpolated crossing point is within *W* laterally, and the direction of travel is within 60° of
*T*. The direction gate is not decoration: without it every vehicle crossing a signalised junction
on the conflicting arm trips the loop.

**The counter is validated against the thing it imitates** — SUMO's own loops on the same run.

| control | trajectory count | SUMO `nVehContrib` | delta |
|---|---|---|---|
| InTAS 0–300 s, step 0.1 s, seed 42 | 246 | 233 | **+5.58%** |
| InTAS 25200–25500 s, step 1.0 s, seed 257318856 | 325 | 312 | **+4.31%** (median station GEH **0.0011**, p85 0.269) |

**Over a full clock hour the counter is far better than either 300 s row suggests, and the residual
had a findable cause.** Same three controls, all graded through `sumo_realism.py` against the SUMO
run's own loops (so every figure below is duration-normalised to veh/h — that basis makes the first
row read +5.61% where the raw ratio 246/233 above reads +5.58%):

| control | window | vehicles | count | SUMO's loops | delta | with `--exclusive-gates` | delta |
|---|---|---|---|---|---|---|---|
| 0–300 s, dt 0.1, seed 42 (local midnight) | 300 s | 334 | 246 | 232.9 | +5.61% | **238** | **+2.18%** |
| 25200–25500 s, dt 1.0, seed 257318856 (AM, entered cold) | 300 s | 1,188 | 325 | 311.0 | +4.52% | **320** | **+2.91%** |
| **25201–28800 s, dt 1.0, seed 42, warmed (the AM peak hour)** | 3,599 s | 14,896 | 24,180 | 23,635.4 | **+2.30%** | **23,733** | **+0.41%** |

**The +2.30% was not spread over the network: 24 of 25 stations were already inside ±3.1%**
(median station GEH 0.132) **and one station, 1010, was +29.93%** (1,938 against 1,491.6, GEH
10.78 — the only station outside the seed-stability band). Localising it per detector: of station
1010's 16 loops, **two carry the entire excess** — `1010_7` counted 448 against SUMO's 216, and
`1010_9` counted 446 against SUMO's 232. Note what those numbers are: 216 + 232 = **448**. Each of
the two loops was counting the union of both.

**The cause is that an `<e1Detector>` is LANE-BOUND and a position is not.** SUMO's loop sees a
vehicle only if the vehicle's `laneID` is the detector's lane, so two loops on different roads never
share a vehicle however close the roads run. This counter has positions and no lane identity. Loops
`1010_7` (edge `172515813`) and `1010_9` (edge `201278218#0`) sit 2.514 m apart with tangents 13.6°
apart — and decomposed on the direction of travel that gap is **2.427 m ALONG and only 0.656 m
ACROSS**, so each loop's ±1.6 m strip covers the other road's centreline and both count every
vehicle on either road. (`1010_6`/`1010_8` are the same pair one lane over: 2.466 m apart, 1.179 m
across.)

**It is not double-counting and the census proves it.** Streaming the hour and recording crossings
per (detector, vehicle): all 24,180 crossings are distinct (detector, vehicle) pairs, multiplicity
histogram `{1: 24180}`, zero repeats anywhere in the network. Every excess crossing is a *different*
vehicle — one that was on the other road.

**And it is not a widespread hazard.** Over the whole InTAS layout, gate pairs that sit on different
edges, closer than the sum of their half-widths, and within the direction gate's 60°, number
**exactly 2 of the 19,110 pairs** — and both are this location. (Nine pairs overlap on the *same*
edge, which is the case the half-lane cap already handles.)

`--exclusive-gates` (default OFF, so every number published before it is unchanged, and the default
freeze path re-verified byte-identical at sha256 `78fd92bf…`) awards a vehicle to at most one loop
of such a group — the one it passed closest to laterally, which is what SUMO gets for free from lane
membership. On the hour it moves **exactly one station**: 1010 goes 1,938 → **1,491** against SUMO's
1,491.6, ratio 0.9996, GEH 10.78 → **0.015**. Network total +2.30% → **+0.41%**, median station GEH
0.1231, p85 0.3914, and **every one of the 25 stations inside the band** (was 24).

**What the residual +0.41% is, and why it shrinks with the window.** The excess is a per-VEHICLE
boundary term, not a per-crossing one: 97.6 excess crossings over 14,896 vehicles = **0.0066 per
vehicle** on the hour, against 8 over 1,188 = **0.0067 per vehicle** on the cold AM 300 s — the same
number. What differs is how many crossings a vehicle contributes: 1.59 each over an hour against
0.26 each over 300 s. So the same front-crossing-versus-completely-passed edge effect is +2.9% of a
300 s count and +0.41% of an hour's. **A full clock hour is not merely the right window for GEH; it
is the window where this counter is nearly unbiased.**



**The bias is the gate definition, not the sample rate.** Re-counting the same 0.1 s trajectory at
progressively coarser sampling:

| sample interval | 0.1 s | 0.2 s | 0.5 s | 1.0 s | 2.0 s |
|---|---|---|---|---|---|
| crossings | 246 | 246 | 246 | 248 | 248 (1,403 pairs too long to interpolate) |
| vs SUMO's 233 | +5.58% | +5.58% | +5.58% | +6.44% | +6.44% |

Going from SUMO's own integration step to 1 Hz costs **0.86 percentage points**. The remainder is
the two effects taken apart above — lane-blind cross-talk, and FRONT-crossing against
`nVehContrib`'s COMPLETELY-passed.

Half-width sensitivity on the 25200–25500 s control: 318 / 319 / 321 / 325 / 325 crossings at
W = 0.8 / 1.0 / 1.2 / 1.6 / 2.0 m (the last is capped by the lane's own half-width). The gate can
never be widened past half a lane, because a station's count is the SUM over its per-lane loops and
a wider gate lets one vehicle be counted by its neighbour's loop too. Note what this sweep does
*not* fix: narrowing to W = 0.8 m still leaves the 1010 cross-talk, because that pair's lateral
separation is 0.656 m. Only lane identity — or `--exclusive-gates`, which reconstructs it — removes
it.

**Direction of the bias.** The counter over-counts, so a model that under-produces looks better than
it is, and **every deficit reported below is therefore a LOWER BOUND on the true deficit.** On the
graded AM peak hour that bias is **+2.30% with the default gate and +0.41% with `--exclusive-gates`**
— which is to say it is now roughly two orders of magnitude smaller than the deficit it sits on top
of, and cannot be the explanation for anything in section 1.

---

## 4. Python engine vs MOSAIC on the same mobility

### What "the same mobility" could and could not be made to mean

MOSAIC drives its own SUMO through TraCI. It injects `mosaic_types.add.xml` (its own vehicle-type
overrides) and its `SumoAmbassador` log records no `--seed`, so the realization cannot be forced to
match a `freeze()` of the same scenario. Measured: at *t* = 100.0 s the frozen trace and MOSAIC's
dataset agree on **1** vehicle position exactly, and the nearest-neighbour distance from a MOSAIC
vehicle to the closest frozen one is p50 **9.75 m**. Same demand realization (both insert exactly
**334** vehicles in 0–300 s), different micro-simulation.

So the comparison is **same scenario, same net, same demand files, same window, same SUMO 1.25.0,
same car-following configuration, different realization** — and this repository has already
CALIBRATED how much a realization is worth: `tools/calibration/seed_stability_intas_urban_low.json`,
20 seeds, 190 same-scenario pairs, network-total loop-count **cv = 2.62%** (upper CL 2.97%),
dispersion φ = 0.218. Anything larger than that band is the engines differing, not the seed.

**And at the AM peak hour it could not be made to mean anything at all, for three separate reasons,
each measured rather than assumed.** (i) `scms-sim/scenarios/gen_intas_urban_low/scenario_config.json`
declares `"duration": "300s"` and its `sumo_config.json` points at a `.sumocfg` whose `<begin>` is
`0`; MOSAIC's `SumoAmbassador` steps SUMO forward from there, so reaching SUMO 25200 s means
federating **seven hours** of Ingolstadt with the application federate live on every vehicle — and
the peak carries 3,781 concurrent vehicles against the 300 s window's 334 total. There is no
`--begin` seam on the MOSAIC side to skip it, which is precisely what `warmup_steps` gave the freeze.
(ii) The upstream scenario that *does* cover this window,
`third_party/veremi-nextgen/…/scenarios/InTAS_urban_7_9_trainval`, sets `"duration": "24h"` and
requires the **`omnetpp` federate, which is not installed in this toolchain**; its vendored
`InTAS_Detectors_Output.xml` contains **0 `<interval>` rows**, so it carries no counts either.
(iii) Even if both were solved, the missing ambassador seed still makes it a different realization.

**So the hour's "same mobility" comparison is a different and stronger one than the 300 s
Python-vs-MOSAIC panel below: three independent readings of LITERALLY THE SAME TRAJECTORIES** —
SUMO's own induction loops, the frozen artifact, and the engine's emitted dataset. Nothing there is
a realization difference, so every gap is attributable. It is in section 1.

### MOSAIC's "full trace" is not a full trace

At `emit_sample_prob = 1.0` / `SCMS_EMIT_SAMPLE = 1.0`:

| | Python engine | MOSAIC |
|---|---|---|
| vehicles, 0–300 s | 334 | 334 |
| ground-truth emission rows | **709,454** | **240,203** |
| present vs emitting at *t* = 100.0 s | 255 present / 255 emitting | 255 present / **57 emitting** |
| effective per-vehicle rate | 1/dt, regular | ~3 Hz, irregular (ETSI CAM triggering) |

The Python engine emits one record per vehicle per step. MOSAIC emits an ETSI-triggered CAM stream,
so its kinematic record is 3.25× sparser and unevenly spaced. Every finite-difference metric in the
traffic panel is taken over wider, ragged gaps on the MOSAIC side.

### Loop counts, all four sources, 0–300 s

| source | crossings | `--exclusive-gates` | vs SUMO's own loops (seed 42) |
|---|---|---|---|
| SUMO's own `<e1Detector>` output | 233 | — | — |
| frozen trace (SUMO's positions, 0.1 s) | 246 | **238** | +5.61%, median station GEH 0.0011, **4/4 seed-stability gates PASS** |
| **Python engine dataset** | **228** | **221** | **−2.11%**, median station GEH 0.0018, station pass fraction **1.00**, **4/4 gates PASS** |
| MOSAIC dataset | 251 | 242 | +7.76%, median station GEH 0.534, station pass fraction 0.826, **2/4 gates FAIL** |

The Python engine's dataset reproduces the loop counts of the SUMO run it replays to inside the
calibrated same-scenario band, at every station. (Its −2.11% is the counter's over-count and
enforcement's −7.3% partially cancelling — under the exclusive gate the same two terms read
238 → 221 = **−7.14%** enforcement against a +2.18% counter bias, which is the same story with the
two terms cleanly separated.) MOSAIC's dataset does not reproduce a *different* SUMO run's counts
station by station — which is exactly what a different realization looks like, and is the reason the
seed-stability calibration exists.

### The traffic panel, side by side

`realism_bench`, InTAS 0–300 s, `emit_sample_prob = 1.0` on all three, `--regime urban`.

| metric | reference | **Python + SUMO mobility** | Python + internal IDM | MOSAIC (sublane) |
|---|---|---|---|---|
| sample pairs | — | 709,120 | 435,873 | 239,869 |
| speed p50 (m/s) | 3.0–14.0 | 12.02 pass | 9.386 pass | 13.14 pass |
| speed p95 (m/s) | 8.0–22.0 | 28.74 fail | 13.90 pass | 54.98 fail |
| speed max (m/s) | ≤ 60 | 55.60 pass | 19.30 pass | 56.34 pass |
| lateral discontinuities (/veh-km) | 0 | 0.0047 fail | 0 pass | 0.1302 fail |
| accel in hard bound | = 1.0 | 0.99988 fail | 0.99952 fail | 0.99823 fail |
| accel in comfort band | ≥ 0.95 | 0.980 pass | 0.9991 pass | 0.9595 pass |
| teleports | 0 | 0 pass | 0 pass | 0 pass |
| **overlap events** | 0 | **0 pass** | **20 FAIL (hard)** | 0 pass |
| moving fraction | ≥ 0.5 | 0.997 pass | 1.000 pass | 0.997 pass |
| headway p50 (s) | (informational) | 2.597 | 4.954 | 3.306 |
| headway below floor | ≤ 0.05 | 0 pass | 0.0004 pass | 0 pass |
| **headway KS** | ≤ 0.15 | **0.1936 fail** | **0.1498 pass** | 0.1865 fail |
| **FD capacity (veh/h/lane)** | 1800–2400 | **811.7 fail** | **182.5 fail** | 945.4 fail |
| FD backward wave (km/h) | 15–20 | na (0 congested cells) | na (4 cells) | −24.85 fail |

**The speed gap is the emission SCHEMA, not the mobility.** The Python engine writes `true_speed` /
`true_heading` (ADR 0002) so its speed metrics are the simulator's own; MOSAIC does not, so
`realism_bench` finite-differences its positions. The Python scorecard also reports the raw chord
number for exactly this comparison: **56.351 m/s** against MOSAIC's **56.34 m/s**. The two engines
carry the *same* chord artifact; only one of them publishes a measured speed instead. Read the
28.74 / 54.98 p95 row as a schema difference, and the same caveat covers the lateral-discontinuity
row (Python measures it on `true_heading`, MOSAIC on differenced positions).

**What is genuinely the adapter:** nothing in this table exceeds the calibrated realization band in
a way attributable to the replay. The adapter measurement is section 2 — 0.000 m — and the loop
counts above corroborate it.

---

## 5. Did SUMO mobility fix the three failures blamed on the hand-rolled car-following model?

The prediction was that `overlap_events`, `fd_capacity` and `headway_ks` were artefacts of the
engine's own IDM. This is the clean A/B: **identical config, identical network (InTAS via
`--road sumo`), identical dt (0.1 s), identical seed, identical duration and identical
`emit_sample_prob`; the only field that differs is `mobility_source`.**

| metric | internal IDM | SUMO replay | reference | verdict |
|---|---|---|---|---|
| `traffic.overlap_events` | **20 (HARD FAIL)** | **0 (pass)** | ≤ 0 | **CONFIRMED — fixed** |
| `traffic.fd_capacity_veh_h_lane` | 182.5 (fail) | 811.7 (fail) | 1800–2400 | **partly — 4.4× better, still fails** |
| `traffic.headway_ks_shifted_exponential` | 0.1498 (pass) | 0.1936 (fail) | ≤ 0.15 | **REFUTED — worse** |

One of three. Specifically:

**Overlap events: the prediction was right.** Two distinct vehicles closer than 1 m at the same
instant is a modelling failure, and the engine's IDM produces 20 of them in 240 sampled instants on
this network. SUMO produces none, because SUMO has lanes, junction conflict resolution and a
collision model, and the engine has one centreline per road.

**FD capacity: the prediction was directionally right and quantitatively insufficient.** 182.5 →
811.7 veh/h/lane is a 4.4× improvement and still less than half the 1800 veh/h/lane floor. This
particular window (InTAS 0–300 s = local midnight, ~290 concurrent vehicles on a 3,289-junction
network) cannot reach capacity on any engine — MOSAIC scores 945.4 on the same window. Section 1's
AM-peak hour is the window where this metric can actually be tested.

**Headway KS: the prediction was wrong, and the direction is informative.** The internal model
passes (0.1498) and SUMO mobility fails (0.1936). A shifted exponential (Cowan M3) is what an
unstructured arrival process looks like; the engine's IDM, spawning from a Poisson process and
following a single leader, produces close to exactly that. Real signalised traffic does not — it
platoons, so the headway distribution is bimodal (within-platoon at saturation headway, between-platoon
long gaps) and fits a shifted exponential worse. **The metric is scoring the wrong thing: the
engine's better score comes from having less structure, not more realism.** That is a defect in the
gate, not in the mobility, and it should be re-derived against a platoon-aware model before it is
quoted again.

---

## 6. Four things had to change before a real city's peak hour could be frozen at all

All four are in `sumo_trace.freeze()`. All four are default-inert, and the default freeze path is
re-verified byte-identical: the same 5-step InTAS artifact hashes
`9d694583c2102c2b0c5195111646b092e3487b31304171f8a11c61593238e4c9` before and after. The `meta`
block is inside the hashed artifact, so an unconditional new key would re-hash every trace ever
frozen and turn `--sumo-trace-sha256` pins into false alarms; every key is therefore emitted only
when its option is engaged, and `--time-to-teleport`'s default is still rendered as the integer
`-1` the CLI was given before it was a parameter.

**`warmup_steps` — a window opened cold is not the traffic you asked for.** `--begin 25200` makes
SUMO discard every vehicle departing before 25200, so the AM peak starts on an EMPTY city.
Measured: **36 vehicles ten seconds after `--begin 25200`**, against ~3,400 with an hour of warm-up
ahead of it. The 300 s trace this repository already had (`intas.trace`, 1,188 vehicles) is entirely
inside that fill transient. `warmup_steps` runs the warm-up without recording it, so the artifact is
the same window entered already full.

**`substeps` — the scenario's integration step and the artifact's sample rate are not the same
number.** Freezing InTAS at `--step-length 1.0` does not coarsen the AM peak, it breaks it: SUMO
1.25.0 aborts with

```
libsumo.libsumo.FatalTraCIError: Request lateral offset of vehicle 'carIn3023:1'
                                 for invalid lane ':474375812_1_0'
```

at sim time 23660 (2,060 steps in), after producing 34 collisions and 34 collision-teleports that
the calibrated configuration does not have. InTAS is calibrated at 0.1 s with EIDM and
`lateral-resolution 0.8`; re-integrating it at 1 s is a different model, not a coarser view of the
same one. `substeps=10` steps SUMO at 0.1 s and records every tenth state, so the mobility is
exactly the calibrated one and the artifact is at the engine's (and a CAM stream's) rate. Teleport
and collision counters are polled on every SUMO step, not only recorded ones.

**`time_to_teleport` — `-1` is a change to the SCENARIO, not just to the artifact.** The freeze
forced `--time-to-teleport -1` on the principle that a teleport is a discontinuity the replay would
reproduce as a physically impossible jump. On a congested real network that principle is expensive:
InTAS's AM peak under its own 300 s policy **teleports 365 times (49 jam, 289 yield, 23 wrongLane),
with 4 collisions, in 25,271 vehicles**, and suppressing all of them leaves those vehicles stuck
instead — which depresses exactly the flows a count validation measures. The hour below is frozen
at InTAS's own 300, which is also what the `GEH-RESULT.md` baseline used.

**And a SUMO crash that had to be attributed correctly before it could be worked around.** Three
long freezes of this window died with no Python traceback:

```
FREEZE  EXIT rc=139   # SIGSEGV at SUMO time 23544.1  (--time-to-teleport -1)
FREEZE2 EXIT rc=139   # SIGSEGV at SUMO time 23904.7  (--time-to-teleport 300)
sumo.exe    rc=-1073741819  # 0xC0000005 at 23904.7 -- plain binary, no libsumo,
                            # no --ignore-route-errors, otherwise the baseline's own config
```

The obvious suspects were wrong. It is not `libsumo` (the plain binary reproduces it), not
`--ignore-route-errors` (removing it reproduces it), and not the teleport policy (which moves the
crash from 23544.1 to 23904.7 but does not remove it). **It is the SUMO seed.** The same window at
seed 42 — the seed `GEH-RESULT.md` used — runs to 28800 cleanly, so the hour is frozen at that seed
via `--seed 42` rather than at the one `derive_sumo_seed(42)` produces. If a long freeze aborts
with no traceback, re-seed before changing anything else.

**`split_on_gap` — what makes such a run writable.** A teleporting vehicle leaves
`vehicle.getIDList()` and comes back later somewhere else (measured: `carIn5871:1` absent at step
25, present again at 26), and the format's contiguity guard refuses the trace — correctly, because
writing the gap as a contiguous run would silently mislabel every later step. `split_on_gap`
records the return as a SEPARATE trajectory `<id>#<n>`. That is the better model, not a workaround:
a 500 m jump in one step is exactly the discontinuity every position-plausibility detector in this
repository is built to flag, while a second trajectory is just another vehicle appearing — which is
what a teleported vehicle physically resembles. Each segment's `route_length_m` is its own driven
distance (`getDistance` is cumulative, so the segment start is subtracted), and `meta["gap_splits"]`
counts them.

---

## 7. Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$env:PYTHONPATH = "$PWD\src"

# --- 0. measured counts. NEVER COMMITTED: licence not formally stated (see GEH-VALIDATION.md) ---
python tools\fetch_ingolstadt_counts.py `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z `
  --out .realism_cache\pyeng\refcounts_A.json
# -> 23 stations, 45,713 veh, duration_s 3600.

# --- 1. freeze the AM-peak hour: SUMO integrates at 0.1 s, the artifact samples at 1 Hz ---
#     every flag below is read back off the artifact's own `#meta` line, which is the authority.
#     --seed (not --run-seed): the derived seed SIGSEGVs on this window, see section 6.
python -m scms_sim_ref.mock_pipeline.sumo_trace `
  --net C:/Temp/smob/ingolstadt.net.xml `
  --sumocfg scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_buildings.sumocfg `
  --out C:/Temp/smob2/intas_hour.trace --seed 42 `
  --steps 3600 --dt 1.0 --begin 21600 --warmup 3600 --substeps 10 `
  --time-to-teleport 300 --split-on-gap
# -> 14,896 trajectories, 13,589,568 vehicle-steps, 301 teleports, 156 gap splits, 2 collisions,
#    sha256 5169914b942b4ad495256354ea9c87068b8c1426e23445b90effd6f4a5daf2ab
# the same process writes InTAS_Detectors_Output.xml (SUMO's own loops for this exact run) into
# the scenario dir -- COPY IT OUT, generated scenario dirs are regenerated by other tooling.

# --- 2. drive the Python engine over it (dt/emit_sample_prob have no CLI flag: dump and patch) ---
python -m scms_sim_ref.mock_pipeline.run --config <cfg>.json --out datasets\py_intas_hour
# cfg: mobility_source=sumo_replay, road_network=sumo, dt=1.0, duration_s=3600,
#      emit_sample_prob=1.0, sumo_trace + sumo_trace_sha256 as above, seed 42.

# --- 3. count loop crossings from the engine's own dataset, and from the trace as a control ---
#     NOTE --time-offset, NOT --begin/--end: the engine labels this window t = 0..3599, so a
#     --begin 25201 window filter would discard every sample.
python tools\engine_detectors.py --net C:/Temp/smob/ingolstadt.net.xml `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --dataset datasets\py_intas_hour --time-offset 25201 --exclusive-gates `
  --out .realism_cache\pyeng\hour_engine.det.xml --json .realism_cache\pyeng\hour_engine.json
python tools\engine_detectors.py --net C:/Temp/smob/ingolstadt.net.xml `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --trace C:/Temp/smob2/intas_hour.trace --exclusive-gates `
  --out .realism_cache\pyeng\hour_trace.det.xml --json .realism_cache\pyeng\hour_trace.json

# --- 4. grade against reality with the UNCHANGED FHWA path ---
python tools\sumo_realism.py --det-out .realism_cache\pyeng\hour_engine.det.xml `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --ref-counts .realism_cache\pyeng\refcounts_A.json --seed 42

# --- 4b. the counter's OWN error: same mobility, SUMO's own loops as the reference ---
python tools\sumo_realism.py --det-out .realism_cache\pyeng\hour_trace.det.xml `
  --det-add scms-sim\scenarios\gen_intas_urban_low\sumo\InTAS_E1.add.xml `
  --ref-det-out .realism_cache\pyeng\hour_InTAS_Detectors_Output.xml --begin 25200 --end 28800

# --- 5. the realism scorecard ---
python -m scms_sim_ref.datagen.realism_bench datasets\py_intas_hour --regime urban `
  --json .realism_cache\pyeng\scorecard_py_intas_hour.json
```

`engine_detectors.py` writes an E1-shaped XML on purpose: `sumo_realism.py` reads it through its
existing `parse_e1_output`, so the Python engine is graded by the same code path, the same station
grouping and the same criteria as the MOSAIC engine, with no producer-specific branch anywhere.

### Window and time-base alignment

The reference API stamps UTC; the InTAS clock is local CET. **2023-11-14 06:00–07:00Z = SUMO
25200–28800 s.** The artifact's step *k* is SUMO time `step0_sim_time + k*dt` = `25201 + k`, and the
engine labels it *t* = *k*, so the engine's *t* ∈ [0, 3599] covers SUMO 25201–28800 — 3,599 s of
movement inside the 3,600 s graded window. `engine_detectors.py` writes that span into the XML so
`sumo_realism.py` normalises to veh/h against the right duration.

### The detector-subset rule, applied

Only **10 of the 25 named InTAS stations** have a detector set identical to the city's
(`detector_sets_identical`); the archive instruments MORE loops than InTAS at **12 of the remaining
15**. `fetch_ingolstadt_counts.py` sums only the InTAS-MATCHED detectors, which makes the reference
a **lower bound** at those stations and therefore flatters the model. The size of that choice,
measured on this window from the archive's own whole-intersection stream against the matched-subset
sum (`whole_intersection_inflation_pct`):

| station | InTAS loops | archive loops | matched | naive whole-intersection over-count |
|---|---|---|---|---|
| 5060 | 4 | 7 | 4 | **+92.88%** |
| 6030 | 4 | 9 | 3 | +75.85% |
| 4250 | 6 | 9 | 6 | +54.99% |
| 4070 | 5 | 7 | 5 | +52.14% |
| 8001 | 9 | 11 | 9 | +39.49% |
| **all 23 comparable** | — | — | — | **45,713 → 52,074 = +13.92%** |

So taking the archive at face value would have credited Ingolstadt with 52,074 vehicles instead of
45,713 and shrunk the reported deficit by about a sixth, for free and wrongly. Two stations
(3120, 3130) are registered but have zero observations in the entire archive and are never emitted —
a zero there would silently corrupt a GEH.

---

## 8. Open

1. **The Python-vs-MOSAIC comparison is same-scenario, not same-trajectory, and at the AM peak it
   is not runnable at all.** MOSAIC drives its own SUMO, injects `mosaic_types.add.xml`, and its
   `SumoAmbassador` log records no `--seed`, so the realization cannot be forced to match a
   `freeze()`. At the peak there is the further obstacle that MOSAIC has no `--begin` seam — its
   scenario declares `"duration": "300s"` over a `.sumocfg` beginning at 0, so SUMO 25200 s is seven
   federated hours away — and the one upstream scenario covering 07:00–09:00
   (`InTAS_urban_7_9_trainval`) needs the `omnetpp` federate, which is not installed here, and ships
   a detector output with 0 `<interval>` rows. A literal same-trajectory comparison needs a replay
   seam on the MOSAIC side; a peak-hour one needs that plus a warm-up seam plus OMNeT++.

2. **MOSAIC does not write `true_speed` / `true_heading`** (ADR 0002), so `realism_bench`
   finite-differences its positions and its speed and lateral-discontinuity metrics are not
   comparable with the Python engine's. The comparable pair is the chord-derived maximum —
   56.351 m/s (Python) against 56.34 m/s (MOSAIC). Closing this is a MOSAIC-side emission-schema
   change, not an analysis change.

3. **`traffic.headway_ks_shifted_exponential` appears to reward the absence of structure.** The
   engine's Poisson-spawned single-leader IDM fits a shifted exponential BETTER (0.1498, pass) than
   real signalised traffic does (0.1936, fail), because platooning makes the real distribution
   bimodal. The gate should be re-derived against a platoon-aware headway model before it is quoted
   as a realism result in either direction.

4. **A revoked vehicle stops emitting, so the dataset's kinematic record ends at revocation.**
   Measured on the 300 s run: 27 of 334 vehicles, −9.23% of vehicle-steps, −7.3% of loop crossings.
   Anything computed from `gt_emissions_sample.jsonl` — including every traffic-panel metric and
   every flow count — is low by the enforced fraction. Either state it per dataset or emit ground
   truth independently of enforcement.

5. **The crossing counter's bias is now measured over a full hour and mostly removed — the residual
   is +0.41%.** The "about +5%" from the 300 s windows was two effects stacked, and section 3 takes
   them apart. (a) *Lane-blind cross-talk*: two roads running 0.656 m apart across the direction of
   travel let each loop count the other's traffic; it occurs at exactly 2 of the 19,110 gate pairs
   in the InTAS layout, both at station 1010, and `--exclusive-gates` removes it (station 1010
   1,938 → 1,491 against SUMO's 1,491.6). (b) *Front-crossing versus completely-passed*: a
   per-vehicle boundary term of **0.0066 excess crossings per vehicle**, which is +2.9% of a 300 s
   count and +0.41% of an hour's. Still open: `--exclusive-gates` is not the default, because
   turning it on would silently change every number published before it; and (b) could be closed by
   offsetting the gate a vehicle length, which is not done because the bias is identical for every
   trajectory source and cancels in every engine-to-engine comparison.

6. **The engine's emission timestamp is one `dt` behind the SUMO time the artifact declares for the
   same state.** Positions are exact; only the label differs. Harmless for an hour-long count,
   load-bearing for anyone aligning a dataset against a SUMO output file.

7. **`dt` and `emit_sample_prob` have no CLI flag on `run.py`**, so a full-trace run at a non-default
   step has to be driven from a dumped-and-patched config. `--dump-config` also writes AFTER the
   run completes, so it cannot be used to prepare a config without first completing a throwaway run.

8. **The mobility A/B is at 300 s and midnight density.** A full-hour internal-mobility control at
   AM-peak concurrency was not run; the FD-capacity comparison at peak is therefore one-sided.
