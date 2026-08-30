# Adversarial review of the Phase 0/1 corrections (2026-08-30)

An independent reviewer re-derived every statistical claim and re-measured every reported number
against the raw traces. Most reproduced exactly. **Four did not**, and two of those are critical.
This file is the work list; nothing here is fixed yet.

Method for the null-distribution work: 10 SUMO runs of the same scenario (seeds 23423, 987654, 1–8),
E1 output parsed through the shipped `parse_e1_additional`/`to_station_flows`, then the shipped
`seed_stability_report` over all 45 seed pairs.

## Reproduced (no action)

- `GEH(k·m, k·c) = √k·GEH(m,c)` and √12 = 3.4641016 — exact to ≤9e-16 over 5 cases.
- `GEH = √2·|Z|` with `Z = (m−c)/√(m+c)` — exact.
- The constants 2.7718 = √2·z₀.₉₇₅ and 4.3163 = √2·z₍₁₋₀.₀₅/₄₄₎ at N=22 — exact.
- 25 named station groups + 2 unnamed (`income`/`outgoing` → `gate.xml`), 196 detectors.
- Sublane improvement: lateral steps >1.5 m **1112 → 306**; the 2.9–3.5 m lane-width band
  **536 → 5**. Scorecard side: 0.5851 → 0.1302 ev/veh-km, 552 → 123 events.
- `--lateral-resolution` is valid for SUMO 1.25.0 and SL2015 is auto-selected when the sublane model
  is enabled (vendored doc `Simulation/SublaneModel.html`), so not pinning `laneChangeModel` is right.
- The longitudinal decomposition does **not** distort steady cornering: 8–20 m/s, 90/180/270°, yaw
  30–225 °/s, dt 0.1–1.0 s including a full 90° inside one sample → max |d_lat| 0.0000 m, 0.00 %
  speed error, 0.00 m/s² fabricated acceleration.
- No invented refdata numbers; threshold-vs-measurement labelling is honest throughout.

## C1 (critical) — the seed-stability flow gate false-alarms 47 % of the time

`tools/sumo_realism.py:110` sets `SEED_STABILITY_TOTAL_REL_TOL = 0.03`, justified at `:550-554` with
*"demand is a fixed route file, so the network-wide loop total is not free to drift with the seed —
only crossing times shift."*

**That premise is false.** Every one of the 185,923 InTAS vehicles carries a `<routeDistribution>`
(859,305 route alternatives) and `InTAS_buildings.sumocfg` sets
`device.rerouting.probability = 0.82`. The seed changes **route choice**, not just crossing times.

Measured network loop total across 10 seeds: `[230, 231, 233, 236, 239, 242, 243, 244, 245, 247]`,
(max−min)/mean = **0.0711**. Running the shipped gates over all 45 pairs, `total_flow_rel_error`
**fails 21/45 (47 %)**, median 0.0261, max 0.0739. The published baseline
(`docs/realism/baselines/geh_intas_urban_low_300s.json:310`, 0.0041) happens to use one of the two
luckiest pairs in the family.

Consequence: `docs/realism/PROGRESS.md:79-80` claims the check "passes 4/4 of its own gates". Re-run
the identical unchanged scenario at seeds 1 vs 5 and it fails. In CI this is a 47 % flake rate on an
unchanged tree, and the natural response would be to loosen the gate rather than fix the model.

## C2 (critical) — the binomial null is the wrong model, so the GEH gates have almost no power

`tools/sumo_realism.py:98-106`, `:123-125`, `:522-528`, `:537-541`.

The algebra is exact; the *generative model* is wrong. `m|n ~ Bin(n, ½)` implies `Var(m) = E[m]`
(index of dispersion 1.0). Measured per-station dispersion across 10 seeds is **median 0.204, mean
0.282** over 23 non-zero stations (e.g. `3120` 0.059, `1010` 0.111, `4250` 0.082). Route choice is a
Poisson-binomial over shared vehicles (`Var = Σp(1−p) ≤ Σp`), so counts are strongly **under**-dispersed.

Measured over 45 seed pairs × ~22 stations = 1014 tests:

| Quantity | Documented | Measured |
|---|---|---|
| Per-station exceedance of 2.7718 | 0.0500 | **0.0039** (12.8× conservative) |
| Exceedance of the family-wise bound | 0.05 | **0/1014**; worst GEH 3.111 vs a 4.3702 bound |
| `link_geh_pass_fraction` / `max_link_geh` failures | — | **0/45** |

Conservative on false alarms means **anti-conservative as a quality claim**: passing is nearly
uninformative. At the shipped bound, against a reference of c=10 vehicles any m in [3, 20] passes —
a **2× flow change goes undetected**. The baseline already contains that shape: station `4140`,
9 vs 17 vehicles, GEH 2.2188, reported "pass". A correctly calibrated α=0.05 window-native bound at
the measured φ ≈ 0.20 is **≈1.24**, not 2.7718.

## M1 (major) — the "correct" lateral-rate value is itself wrong; the metric is not estimable from a thinned trace

The defect is real and confirmed: `src/scms_sim_ref/datagen/realism_bench.py:494` builds `lateral`
with no `dt ≤ MAX_FD_DT_S` mask while the accel series applies it at `:518`/`:522`. All the reported
symptoms reproduce (535 events, 473 on dt > 2 s, max |d_lat| 891.7 m, max dt 115.6 s).

But the **denominator has the same defect**: `path_m` at `:588` sums `|d_long|` over all pairs —
786.93 veh-km, of which only 12.60 veh-km lies on dt ≤ 2 s pairs. So:

| Variant | Value | vs ground truth |
|---|---|---|
| Shipped (no mask) | 0.6799 | 1.16× |
| Numerator masked only — the figure named "correct" in PROGRESS.md:274-275 | 0.0788 | **0.13× (7.5× understatement)** |
| Numerator **and** denominator masked | 4.9224 | 8.4× overstatement |
| Ground truth, same scenario at `emit_sample_prob = 1.0` | **0.5872** | — |

**Masking the numerator alone would make the published number materially worse.** At emit_p = 0.02
the surviving dt ≤ 2 s pairs still span up to 20 base steps, so they are a biased subsample enriched
in lane changes. The correct fix is the `thin` reason already computed at `:1011-1014`: `lat_few` at
`:1050` tests only `n_pair >= MIN_SAMPLES and v_km > 0` and omits `thin`, unlike headway and FD which
both use `few or thin`. The metric should return `na` on thinned traces.

## M2 (major) — a lane-change teleport inside a sharp corner leaks into the acceleration metric

`realism_bench.py:473-493`. The `stable` witness requires raw headings either side to agree within
`HEADING_STABLE_TOL_DEG = 45`. Injecting a 3.2 m centreline teleport into corners at 15 m/s, dt 0.2 s:

- 30 °/s corner → `lateral=1`, screen active, accel clean (max 0.00 m/s²) — correct;
- 225 °/s kink → `lateral=0`, **screen disabled**, accel min/max **−34.66 / +34.66 m/s²**;
- single-step 90° kink → `lateral=0` while `max|d_lat|` still measures the full 3.200 m, same leak.

`lateral_screen` is gated on the same `stable` flag at `:493`, so suppressing the event also disables
the acceleration screen — and junctions are exactly where SUMO performs centreline snaps, so the two
conditions co-occur by construction. Live exposure is currently small (10/562 candidates on
`intas_nosublane_300s`, 1/124 on the sublane run), so this is latent, not a live number error.

## M3 (major) — the residual lateral steps are mostly *not* cornering

The corrections report attributed the worst residuals to "high-speed junction curvature". Measured
local turn angle across the event on `intas_nosublane_300s` (552 events): **71.9 % under 5°** —
i.e. straight-line, genuine teleports — 16.3 % at 5–20°, 6.7 % at 20–45°, 5.1 % ≥45°. Cornering
explains **at most 11.8 %**, not the bulk. On the sublane run (123 events): 47.2 / 43.9 / 8.1 / 0.8 %.

The two largest residuals split: |d_lat| 6.42 m has a 45.6° turn (genuinely curvature), but
|d_lat| 6.32 m has a **2.9°** turn at a **36.1 m/s** chord speed. Both chord speeds (35.4, 36.1 m/s)
are implausible for the location — these are position discontinuities.

Also: `tools/lateral_jump.py` and `realism_bench.py` disagree on the worst residual (6.637 → 6.680 m,
essentially unimproved, versus 6.4169 → 4.5007 m) because they use different heading references, and
`PROGRESS.md:200` quotes only the more favourable pair.

## Minor

- `docs/FEATURES.md:144` — still claims "GEH statistic + the four FHWA calibration gates" with no
  caveat, contradicting `PROGRESS.md:78-81`. `README.md:212-214` is only loose ("traffic-calibration
  gate"); it does already say reference counts are not on disk, so it claims no result.
- Stale metric counts introduced by the corrections: `docs/FEATURES.md:136` and `DATASHEET.md:77` say
  21 metrics (14 traffic + 7 comm); the panel is **22 (15 + 7)**. `PROGRESS.md:42` is correct.
- `tools/sumo_realism.py:526` — "~1.5 % at N=22" is wrong; P(Bin(22, 0.05) ≥ 4) = **0.0222**.
- `tools/sumo_realism.py:523-524` — "each station fails with probability 0.05" is false even inside
  the binomial model, from discreteness at these station counts: 0.000 for n ≤ 3, 0.125 at n=4,
  0.0625 at n=5, 0.0703 at n=8, 0.0215 at n=10. Baseline stations `3021` (n=2) and `5060` (n=3) can
  never fail.
- Stale generated scenarios without `<lateral-resolution>`: `gen_grid_4x4_s1/sumo/map.sumocfg`,
  `scms_smoke/sumo/highway.sumocfg`.
- `PROGRESS.md:281-284` labels `datasets/smoke` and `mosaic_smoke_realismmode` as pre-sublane but
  omits `mosaic_intas_urban_low_gate` and `..._fulltrace`, whose provenance also lacks
  `sublane_model`/`lateral_resolution_m` while their scorecards are listed unlabelled at `:52-53`.
