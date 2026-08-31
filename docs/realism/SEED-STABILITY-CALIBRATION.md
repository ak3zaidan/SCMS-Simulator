# Seed-stability calibration — how the thresholds stopped being invented

This is the fix for the two critical statistical defects in `docs/realism/REVIEW-FINDINGS.md`.
Both said the same thing in two places: **`tools/sumo_realism.py` asserted a null distribution for a
quantity it could simply have measured.** The fix is to measure it.

| | Before (until 2026-08-30) | After |
|---|---|---|
| Per-station bound | `sqrt(2)·z₀.₉₇₅ = 2.7718`, from an assumed `m\|n ~ Bin(n, ½)` | `1.5370`, from the **measured** index of dispersion |
| Worst-station bound | `4.3163` (Bonferroni on the same assumed null) | `2.7735` |
| Network-total tolerance | `0.03`, from "demand is a fixed route file" | `0.0918`, from the **measured** seed-to-seed spread |
| Required station pass fraction | `0.85` (round number) | `0.9091` (α-quantile of the measured null) |
| Required count-band pass fraction | `0.80` (round number) | `0.8636` |
| Report false-alarm rate on unchanged runs | **86/190 = 45.3 %** | **6/190 = 3.2 %** in-sample, **26/380 = 6.8 %** leave-one-seed-out |
| Power of the two GEH gates against a halved station | **0–2 of 190 pairs** | **151 of 190** (worst-station gate alone) |

Everything in that table is measured on 20 SUMO 1.25.0 runs of `gen_intas_urban_low` over a 300 s
window, and is reproducible with the command in [Deriving one](#deriving-one).

## Where the calibration lives, and why not in `refdata/`

| | |
|---|---|
| Calibration file | `tools/calibration/seed_stability_<scenario_id>.json` |
| Raw per-seed SUMO output | `.realism_cache/seed_runs/<scenario_id>/` — **gitignored** |

`src/scms_sim_ref/datagen/refdata/` holds **externally cited literature values** — FHWA criteria,
ETSI DCC parameters, kinematic bounds — each with a citation to a document outside this repository.
A seed-stability calibration is the opposite kind of number: it is a property of *one scenario* on
*one SUMO build*, it has no external source, and it goes stale the moment the network, the demand or
the simulator changes. Mixing the two would let a locally derived threshold inherit the authority of
a cited one. So the calibration sits next to the tool that derives it and carries its own
provenance: seed list, SUMO version banner, scenario content hash, derivation timestamp.

The **raw** per-seed detector output stays out of git entirely. It is bulky, it is regenerable in
~12 s per seed, and the same cache directory is where any future *measured* count fetch must land —
the Ingolstadt/SAVeNoW open counts carry **no formally stated licence** ("License Not Specified" in
the TUM catalogue), so nothing fetched from them may ever be committed. See
[`GEH-VALIDATION.md`](GEH-VALIDATION.md). The calibration file itself is this repository's *own*
SUMO output and carries no such restriction, which is why the per-seed station counts it was derived
from are embedded in it: that makes the thresholds auditable and the power tests runnable offline.

## What is actually measured

For K seeds there are K(K−1)/2 same-scenario run pairs. Each pair is scored with **exactly** the
statistics the live gate uses (`pair_statistics`, the single definition shared by both paths), and
each threshold is read off the resulting distribution.

**The dispersion.** The discarded null said `m | n ~ Bin(n, ½)`, which forces the index of
dispersion `φ = E[(m−c)²/(m+c)]` to **1.0**. Measured on `intas_urban_low`:

```
φ = 0.2182   (pooled over 190 pairs × ~22 stations = 4269 station tests)
```

Route choice is a Poisson-binomial over shared vehicles (`Var = Σp(1−p) ≤ Σp`), so counts are
strongly **under**-dispersed. The bound is the same formula with the measured value substituted:

```
GEH_bound(a) = sqrt(2·φ) · z_(1−a/2)        # φ = 1 reproduces the old 2.7718 exactly
```

**The network total.** 20 seeds, window-native loop totals:

```
226 230 231 231 233 234 236 236 237 239 240 242 243 244 244 244 245 245 246 247
mean 238.65   cv 0.0262   (max−min)/mean = 0.0880
```

The premise behind `0.03` — "demand is a fixed route file, only crossing times shift" — is false:
every InTAS vehicle carries a `<routeDistribution>` and `InTAS_buildings.sumocfg` sets
`device.rerouting.probability = 0.82`, so **the seed changes route choice**. The tolerance is
`sqrt(2)·cv·z_(1−a/2)` instead.

**Robustness.** Each threshold is the *looser* of that moment form and the direct empirical
(1−a) quantile of the same statistic, so a heavier-than-normal tail cannot hide behind the moment
form and a short calibration cannot be over-fitted to its own extremes. `φ` and `cv` are evaluated
at a **seed-level resampling upper confidence limit** (γ = 0.05): the resampling unit is the seed,
not the pair, because each run appears in K−1 pairs.

**Choosing the level.** The four gates are positively correlated, so splitting α by Bonferroni
over-corrects and throws away power. Instead the tool takes the *largest* per-gate level `a` in
`CALIB_GATE_LEVEL_DIVISORS` whose measured **report-level** false-alarm rate over the calibration
pairs still meets α. On `intas_urban_low` that is `a = α/1.75 = 0.0286` for α = 0.05.

**Honesty about the estimate.** The in-sample rate is optimistic by construction, so the tool also
computes a **leave-one-seed-out** rate: re-derive the thresholds without one seed, grade that seed's
pairs against them, repeat. That is the number to quote.

```
in-sample            6/190  = 0.0316
leave-one-seed-out  26/380  = 0.0684      (target α = 0.05)
```

## The null is heavy-tailed, and K = 10 is not enough

Two disjoint 10-seed halves of the same 20 seeds give **φ = 0.2713** and **φ = 0.1619** — a 1.68×
swing. The cause is visible in the raw data: single runs occasionally double a station's count
through a route-choice flip.

- Station **4240** records **18** vehicles at seed 3 against 7–9 at every other seed. Every pair
  containing seed 3 produces the calibration's worst GEH (3.1113).
- Station **4140** records **17** at seed 987654 against 8–11 elsewhere — this is exactly the
  `9 vs 17` pair the review flagged as "reported pass".

So `--calibrate` requires K ≥ 10 and stamps `low_confidence: true` below K = 20, with the swing
quoted in the file's own `caveats`. `heavy_tailed_stations` lists the stations whose pairwise φ
exceeds twice the pooled value: `4070, 4140, 4210, 4240, 5012`.

## What the gate can and cannot see

C2's real complaint was that nobody had ever asked what the gate could detect. Every seed-stability
report now answers it. `detectability.min_detectable_ratio` is the smallest `m/c` that leaves the
per-station band, solved from the bound:

| | old bound 2.7718 | calibrated 1.5370 |
|---|---|---|
| at a 10-vehicle station | ×2.089 — **a 2× change is invisible** | ×1.549 |
| median over the 22 compared stations | — | **×1.6267** |
| best / worst station | — | ×1.2908 / ×3.2372 |

Stations at which a 2× change is still invisible are named in the report
(`3021, 4050, 5012, 5030, 5060` on the baseline pair — all with 1–3 vehicles per 300 s). That is a
statement about a 300 s window, not a defect: the fix is a longer window, and the report says so
rather than implying coverage it does not have.

Measured power over all 190 pairs, injecting one regression at a time:

| Injection | old GEH gates | calibrated gates |
|---|---|---|
| halve station 3120 (27.8 veh) | 0/190 | **151/190** worst-station, 158/190 report |
| halve station 8005 (27.2 veh) | 2/190 | 120/190 worst-station, 132/190 report |
| network total ×0.85 | — | **182/190** total-flow and report |
| network total ×0.50 | — | **190/190** |

The old gate set's apparent 80 % "detection" of a halved station was **not power**: every one of
those hits came from the 0.03 total-flow gate, which also fires on 45 % of *unchanged* pairs.

## Deriving one

```powershell
. C:/Users/Administrator/tools/env.ps1
python tools/sumo_realism.py --calibrate `
  --sumocfg scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_buildings.sumocfg `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --end 300 --seeds "1-18,23423,987654" --scenario-id intas_urban_low
```

~12 s per seed; cached runs are reused unless `--calib-no-reuse` is passed. Offline variant, from
detector outputs you already have: `--calib-det-out SEED=PATH`, repeated.

Grading then picks the calibration up automatically by scenario id (or `--calibration PATH`):

```powershell
python tools/sumo_realism.py --det-out after.xml --ref-det-out before.xml `
  --det-add .../InTAS_E1.add.xml --seed 1 --ref-seed 5 --scenario-id intas_urban_low
```

A calibration whose `group_by` or `window_s` does not match the comparison is **refused**, not
silently applied — the thresholds are window-native and do not transfer.

## Running without a calibration

The parametric `Bin(n, ½)` fallback is retained so the tool still says *something* on an
uncalibrated scenario, but the result is stamped `uncalibrated` and:

- a would-be **pass** becomes `na`, with the verdict preserved in `provisional_status` and rendered
  as `[UNC ]`. The fallback is ~2× looser than the calibrated bound, so a pass under it is
  uninformative and must never be quotable;
- a **failure** stays a failure, carrying `UNCALIBRATED_FAIL_NOTE`. A conservative bound that still
  trips is real evidence;
- the **network-total gate reports no threshold at all** (`na` in both directions). There is no
  defensible parametric null for it — the premise that carried `0.03` is false and nothing replaces
  it without measurement. `SEED_STABILITY_TOTAL_REL_TOL` is now `None` so a stale import fails loudly
  instead of silently reading `0.03`.
- `--require-calibration` turns the absence into an outright gate failure (exit 1) for CI.

`regression.*` (same-seed, must reproduce exactly) is **not** calibration-dependent: a zero-tolerance
identity check needs no null model and stays quotable.

## Two documented rates that were wrong

Both are in the fallback's own gate note now, stated correctly:

1. `tools/sumo_realism.py:526` said the 0.85 pass-fraction gate had a "~1.5 % at N=22" false-alarm
   budget. With N = 22 the gate trips at ≥ 4 station failures, and
   **P(Bin(22, 0.05) ≥ 4) = 0.0222**, not 0.015.
2. `:523-524` said each station fails with probability 0.05. It does not, even inside the binomial
   model, because `Bin(n, ½)` is discrete:

   | n | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 10 | 13 | 17 |
   |---|---|---|---|---|---|---|---|---|---|---|---|
   | P(GEH ≥ 2.7718) | 0 | 0 | 0 | 0.1250 | 0.0625 | 0.0313 | 0.0156 | 0.0703 | 0.0215 | 0.0225 | 0.0490 |

   Stations recording ≤ 3 vehicles in total — `3021` (n = 2) and `5060` (n = 3) in the baseline —
   **could never fail**. Under the calibrated bound 1.5370 they can: P = 0.50 at n = 2, 0.25 at
   n = 3.

Both corrections are asserted in `tests/test_sumo_realism.py`, so the strings cannot silently drift
back.

## Tests

`tests/test_sumo_realism.py` (51 tests). The load-bearing ones are the power tests, because C2
existed precisely because **nothing ever tested that the gate could fail**:

- `test_power_halving_one_station_fails_the_gate_synthetic` / `..._on_real_intas_runs`
- `test_power_total_flow_shift_fails_the_gate_synthetic` / `..._on_real_intas_runs`
- `test_power_the_gate_can_fail_even_uncalibrated`
- `test_no_false_alarm_on_a_genuine_seed_pair`, `test_measured_false_alarm_rate_over_every_seed_pair`
- `test_c2_a_2x_flow_change_at_c10_passes_the_old_bound_and_is_flagged_by_the_new_one`
- `test_c2_the_baseline_station_4140_flips_from_pass_to_flagged`
- `test_uncalibrated_report_demotes_every_pass_to_na`,
  `test_require_calibration_turns_a_missing_calibration_into_a_failure`
- `test_shipped_calibration_reproduces_from_its_own_recorded_runs` — the persisted thresholds must
  regenerate from the persisted per-seed counts, so no number in the file can be hand-edited.

Tests that need real SUMO output read the per-seed counts embedded in the calibration and skip if it
is absent; everything else is synthetic and runs offline.
