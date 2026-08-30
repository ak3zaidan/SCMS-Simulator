# Realism push — progress log

Mission: SOTA traffic + network realism (see [ROADMAP.md](ROADMAP.md)). Every phase gates on a
quantitative benchmark; determinism/manifest contract and config→GUI pipeline are hard invariants.

## Baseline (2026-08-29, main @ 2832d63)

- Test suite: **496 passed** in 607 s (Python 3.12.10, SUMO 1.25.0, JDK 17.0.20.1, MOSAIC 25.2).
- Reference run: `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15
  --traffic-lights --seed 42` → 593 vehicles, 7823 reports, data_digest
  `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48`, precision 0.599 / recall 0.91,
  latency_med 4.0 s.
- Realism scorecard: none yet (Phase 0 builds it). Known Day-0 realism defects are ranked G1–G16 in
  ROADMAP.md §2.

## Phase log

| Date | Phase | Status | Gate result |
|------|-------|--------|-------------|
| 2026-08-29 | Investigation (13 agents) | done | Roadmap adopted |
| 2026-08-29 | Phase 0 — realism bench harness | started | — |
| 2026-08-29 | Phase 1 — MOSAIC/SUMO flagship realism | started | — |
| 2026-08-30 | Phase 0 + Phase 1 — integrated | done | see "Phase 0/1 integration" below |
| 2026-08-30 | Phase 0/1 review — GEH labelling, accel estimator, sublane | done | 3 defects corrected, baselines re-measured; 2 roadmap gates still not met, 1 blocked |

## Phase 0/1 integration (2026-08-30)

Determinism canary **held exactly**: the reference run above re-run on the integrated tree
reproduces `f0ec3cc0baa55a2f…` with 593 vehicles / 7823 reports / 152 revoked / precision 0.599 /
recall 0.91 / latency_med 4.0 s. `src/scms_sim_ref/mock_pipeline/` has a zero diff against the
committed tree, and the pinned golden-digest suites (`test_pipeline`, `test_config_knobs`) pass.

The MOSAIC layer's own `datasets/smoke` digest **changed by design** (`5e2497ee…` →
`7f64d347…`): the smoke run now carries 8 junction RSUs (the RSU application finally exists), a
100 ms MOSAIC↔SUMO sync and full emission tracing. Digest neutrality of the *refactor* was proved
separately — extracting the detector suite into `org.scms.app.CamDetector` was verified against a
pre-extraction control jar on an identical scenario/seed: both produce `619328ca…`
(50 vehicles / 244 reports / 6 revoked).

### Measured scorecards (`datasets/realism_baseline/`, mirrored to `docs/realism/baselines/`)

Re-measured 2026-08-30 after the three review corrections below. The panel is now **22 metrics**
(was 21): `traffic.lateral_discontinuity_events` is new. `datasets/` is gitignored, so every
scorecard is mirrored byte-identically into the tracked `docs/realism/baselines/`.

| Run | pass / fail / n-a | HARD failures | SOFT failures |
|---|---|---|---|
| `scorecard_python_flow_grid6.json` — the canary run itself, emit_p 0.03 | 7 / 0 / 15 | none | none |
| `scorecard_python_flow_grid6_fulltrace.json` — same config, emit_p 1.0 | 10 / 5 / 7 | `accel_within_hard_bound_frac` (0.999417), `overlap_events` (290) | `headway_ks` (0.174), `fd_capacity` (517 veh/h/ln), `pdr_gray_zone_width_m` (81 m) |
| `scorecard_mosaic_smoke.json` — MOSAIC `datasets/smoke`, highway regime, emit_p 1.0 | 9 / 5 / 8 | `accel_within_hard_bound_frac` (0.999826) | `lateral_discontinuity` (0.398 ev/veh-km), `headway_ks` (0.112), `fd_capacity` (2759 veh/h/ln), `pdr_gray_zone_width_m` (32 m) |
| `scorecard_mosaic_smoke_realismmode.json` — the pinned Phase-0 smoke baseline, emit_p 1.0 | 7 / 7 / 8 | `accel_within_hard_bound_frac` (0.999964), `teleport_events` (1) | `speed_max` (138.0 m/s), `lateral_discontinuity` (0.458), `headway_ks` (0.156), `fd_capacity` (3033 veh/h/ln), `pdr_gray_zone_width_m` (18 m) |
| `scorecard_intas_urban_low_300s.json` — InTAS gate run, emit_p 0.02 | 7 / 4 / 11 | none | `speed_p95` (55.3 m/s), `lateral_discontinuity` (0.680 — **not valid at this emit_p**, see below), `accel_within_comfort_frac` (0.941, n=68), `pdr_gray_zone_width_m` (51 m) |
| `scorecard_intas_urban_low_300s_fulltrace.json` — InTAS gate run, emit_p 1.0 | 9 / 7 / 6 | `accel_within_hard_bound_frac` (0.998744) | `speed_p95` (55.0), `lateral_discontinuity` (0.587), `headway_ks` (0.191), `fd_capacity` (927 veh/h/ln), `fd_wave` (−24.8 km/h), `pdr_gray_zone_width_m` (43 m) |
| `scorecard_intas_nosublane_300s.json` — InTAS seed 42, sublane **off** (control) | 9 / 7 / 6 | `accel_within_hard_bound_frac` (0.998671) | `speed_p95` (54.9), `lateral_discontinuity` (0.585), `headway_ks` (0.166), `fd_capacity` (928 veh/h/ln), `fd_wave` (−24.0 km/h), `pdr_gray_zone_width_m` (43 m) |
| `scorecard_intas_sublane_300s.json` — InTAS seed 42, sublane **on** (0.8 m) | 9 / 7 / 6 | `accel_within_hard_bound_frac` (0.998233) | `speed_p95` (55.0), `lateral_discontinuity` (**0.130**), `headway_ks` (0.187), `fd_capacity` (945 veh/h/ln), `fd_wave` (−24.8 km/h), `pdr_gray_zone_width_m` (48 m) |

The Phase-0 report predicted the MOSAIC path would be *unscoreable* (2 pass / 0 fail / 18 n-a on
`datasets/smoke`). It now scores 9/5/8, which is the concrete measure of what Phase 1 bought:
`emit_sample_prob`, `radio_range_m`, `art_max_m` and a net-derived `road_network` / `regime` are
recorded in the Java manifest, so the harness resolves its reference bands without a `--regime` flag.

Determinism canary re-run on this tree: `--flow --road grid --grid 6 --duration 300 --arrival-rate 2
--attacker-pct 0.15 --traffic-lights --seed 42` → 593 vehicles / 7823 reports / 152 revoked,
data_digest `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48` — **byte-identical**
to the reference. `git diff --stat -- src/scms_sim_ref/mock_pipeline/` is empty.

### Phase-1 gates

- **CAM rate** (roadmap gate: mean ≤ 400 ms, histogram spans 1–10 Hz): **MET** — 151 124 inter-CAM
  gaps over 263 vehicles, mean **0.202 s**, p50 0.200 s, min 0.100 s, max 1.000 s, histogram
  `{1 Hz: 131, 2: 31, 3: 4504, 5: 143 374, 10: 3084}`. The harness independently reports the CAM
  gap metric as 0.2 s (it read 20.0 s on the Phase-0 baseline, inflated by 1/emit_sample_prob).
- **SUMO health**: 0 teleports, 0 overlapping vehicles on the InTAS runs (both HARD metrics pass).
  `mosaic_smoke_realismmode` carries 1 genuine teleport and fails that HARD gate.
- **Acceleration plausibility**: **NOT MET.** See "Acceleration — corrected" below. The gate is
  100 % inside [−8, +4] m/s²; the best any full-trace run reaches is 99.9964 %. The ≥ 95 % within
  ±3 m/s² half of the gate **is** met on every full-trace run (0.959–0.996).
- **GEH on InTAS loops**: real-world validation is **BLOCKED, not failed** — see "GEH — this is a
  seed-stability check, not a validation" below. What exists today is a simulation-vs-simulation
  reproducibility check that passes 4/4 of its own gates. The roadmap gate "GEH < 5 on ≥ 85 % of
  InTAS induction-loop stations" (ROADMAP.md:101) **cannot be claimed** and must not be quoted.
- **Lane-change continuity** (new, Phase-1 follow-up): SUMO's sublane model is now on by default and
  cuts lane-change teleports 4.5× on a matched pair, but the metric's target of 0.0 events/vehicle-km
  is **not met** on any SUMO run. See "Lateral discontinuity + sublane" below.
- **RSU evidence**: `rsu_contribution` on the smoke run is 3 RSUs reporting, 1505 reports at
  precision 0.971 — the MOSAIC path finally has always-trusted infrastructure reporters.

### GEH — this is a seed-stability check, not a validation

`datasets/realism_baseline/geh_intas_urban_low_300s.json` now declares
`comparison_kind: "seed_stability"` and carries a top-level `warning` as its first key. Stated
plainly: **it compares one SUMO run against another SUMO run of the same scenario** (`gate_` at
SUMO's default seed 23423 vs `rep2_` at seed 987654). It measures the simulator's reproducibility
against itself. **Nothing in it says the model resembles real traffic.**

The earlier version of this file reported the same comparison in FHWA *validation* vocabulary
("GEH < 5 on ≥ 85 % of links", cited to FHWA Traffic Analysis Toolbox Vol III). That was wrong on
two counts and both are fixed:

1. **Wrong reference class.** No real-world measured count data exists anywhere in the repo. All 15
   vendored `InTAS_Detectors_Output.xml` copies under `third_party/veremi-nextgen` are config-echo
   stubs with **0 `<interval>` rows**, and the InTAS route files are *demand* — model input — so
   grading counts against them is circular. The loop **geometry** is genuine (`InTAS_E1.add.xml`,
   196 `e1Detector`s), but geometry is not counts.
2. **Wrong scale.** GEH is not scale-free: `GEH(k·m, k·c) = √k · GEH(m,c)`, and FHWA's `GEH < 5` is
   written for **hourly** volumes. Extrapolating a 300 s window to veh/h multiplied every GEH by
   `√(3600/300) = 3.4641`. Seed stability is now graded on window-native counts, with each station's
   `geh_veh_h` retained alongside for continuity.

Current result (`comparison_kind: seed_stability`, window-native counts, 25 shared stations, 3
both-zero stations `4070 / 4160 / 4210` excluded → 22 compared): **4/4 seed-stability gates pass** —
station pass fraction 1.0 ≥ 0.85 at `GEH_w < 2.7718`; worst station 2.2188 ≤ the N-dependent
Bonferroni bound 4.3163; total flow 243 vs 242 vehicles, relative error 0.00413 ≤ 0.03; count
tolerance 0.8636 ≥ 0.80. Thresholds are derived in-tool from the exact conditional null
(`m | m+c=n ~ Binomial(n, ½)`, under which window-native `GEH = √2·|Z|`), **not** taken from FHWA.
Three stations carry `geh_veh_h ≥ 5` (4140 = 7.69, 8002 = 6.57, 1011 = 5.24) while their
window-native GEH is 2.22 / 1.90 / 1.51 — those are pure seed noise on 8-vs-17, 13-vs-7 and 9-vs-5
vehicle counts, and the old FHWA-labelled gate would have branded them validation failures.

Layout correction: `InTAS_E1.add.xml` has 196 `e1Detector`s but only **25** `name=` station groups,
not the 27 previously reported. The other two (`income`, `outgoing`) carry no `name`, write to
`gate.xml`, and are scenario-gate counters. `detector_layout.n_stations` still reports 27 (it counts
grouping keys, pinned by tests/test_realism_bench.py:940); `n_named_stations: 25` is now reported
beside it, and only the 25 named groups are valid `--ref-counts` keys.

**To unblock real-world validation, what is needed is:** measured vehicle counts for the Ingolstadt
loops — ideally ≥ 1 h of them — keyed by the 25 named station ids, supplied through
`tools/sumo_realism.py --ref-counts` in the JSON/CSV schema documented at
`src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md`. That path has **never been
exercised against real data** because none is in the tree, and nothing has been synthesised to stand
in for it. Two further constraints on any future claim: the 300 s window is too short (~11 vehicles
per station, and the FHWA criteria should not be quoted at that window at all — the tool now emits a
SHORT WINDOW note below 1800 s), and `geh_criteria.json` already pins
`fhwa_2019_distribution_criteria` (FHWA-HOP-18-036), which needs ≥ 10 seeds; the current two-run
comparison is the weakest available form.

### Acceleration — corrected: the old failure was largely a lane-change artefact

`traffic.accel_within_hard_bound_frac` used to fail on every engine with `accel_min −275` /
`accel_max +272 m/s²` (28 g) while `p01`/`p99` were a perfectly realistic −4.18 / +2.65. Root cause:
`gt_emissions_sample` carries `true_x` / `true_y` but **no true speed**, so the harness derived speed
as `hypot(dx,dy)/dt` and acceleration by differencing that again. A SUMO lane change moves a vehicle
~3.2 m sideways — exactly one lane width — **inside one sample**, which the estimator read as a
33 m/s longitudinal speed and hence a ~276 m/s² acceleration.

The fix (`src/scms_sim_ref/datagen/realism_bench.py`) decomposes each step into longitudinal and
lateral components against a smoothed direction of travel, and relocates position *discontinuities*
out of the acceleration series into their own metrics rather than deleting the evidence: lane-change
teleports go to `traffic.lateral_discontinuity_events`, teleports to `traffic.teleport_events`, and a
track's first/last step is dropped as a partial insertion/arrival step. Every screened class is
counted in `details.screened_out` and the unscreened series is published alongside.

| Dataset | before | after | n before → after | accel min / max after |
|---|---|---|---|---|
| `python_flow_grid6_fulltrace` | 0.9977 | **0.999417** | 33 759 → 32 585 | −8.98 / +4.87 |
| `mosaic_smoke` (`datasets/smoke`) | 0.9948 | **0.999826** | 150 862 → 149 194 | −6.56 / +7.71 |
| `mosaic_smoke_realismmode` | — | **0.999964** | — → 54 843 | −6.66 / +6.94 |
| `intas_urban_low_300s_fulltrace` | 0.9947 | **0.998744** | 233 293 → 227 796 | −39.18 / +41.17 |
| `intas_nosublane_300s` | — | **0.998671** | — → 233 934 | −46.14 / +45.02 |
| `intas_sublane_300s` | — | **0.998233** | — → 234 882 | −61.11 / +65.93 |

The physically impossible extremes are gone (InTAS `accel_min` −275 → −39.2 on the same trace) and
`accel_p01`/`p99` barely moved (−4.18/+2.65 → −3.97/+2.62), which is the sign that the estimator was
wrong rather than the simulator. **The HARD gate is still NOT MET on any full-trace dataset**, and
the gate was deliberately kept at exactly 1.0 with no tolerance, because genuine violations remain:

- **Python engine**: 19 of 32 585 samples outside the band after every artefact class is removed, and
  **all 19 sit at `pair_dt = 1.0 s`** — they cannot be small-interval differentiation noise. That is
  the engine's own dynamics. `mock_pipeline/` is frozen by the digest invariant, so this is a PM
  decision, not a harness fix.
- **InTAS**: 311 (control) / 415 (sublane) of ~234 k samples, `|a|` median ≈ 7.2–7.7 m/s². These are
  single-sample **longitudinal** position remaps at junction/edge transitions — the same discrete
  remapping as the lateral teleport, on the other axis. Measured: across the two steps of a residual
  pair the median `|d_lat|` is **0.001 m** and only 4–6 % exceed 0.25 m, so this residual is not
  lateral and the sublane model cannot fix it.
- **Independent corroboration**: `sumo_accel_intas_urban_low_300s.json` reads SUMO's *own* reported
  `speed` from `--fcd-output` at a clean 0.1 s — no position double-differencing at all — and still
  reports 0.9999 within band with `accel_min −9.0 m/s²` over 781 091 samples / 334 vehicles. The
  residual is in SUMO's dynamics (emergency braking), not purely in the estimator.

### Lateral discontinuity + sublane model (defect C)

New SOFT metric `traffic.lateral_discontinuity_events` (events per vehicle-km) counts the artefact
the acceleration screen removes, so screening cannot hide it. Reference max is **0.0** — zero is the
physical target, not a tolerance, cited to the locally verified SUMO 1.25.0 default
`--lanechange.duration 0` (instantaneous lane change). It is SOFT because every current MOSAIC run is
non-zero by construction and gating CI on it would only wedge the pipeline.

The correctness check that matters: the pure-Python engine has **no lanes** and reads exactly
**0.0 ev/veh-km**, while both SUMO paths are clearly non-zero.

Matched pair, `intas_urban_low`, 300 s, seed 42, 100 ms sync, `emit_p 1.0`, 334 vehicles — identical
in every respect except `SCMS_LATERAL_RES`:

| Metric | sublane **off** (control) | sublane **on** (0.8 m) | change |
|---|---|---|---|
| `lateral_discontinuity_events` | 0.5851 ev/veh-km | **0.1302** | **4.49× fewer** |
| lane-change events | 552 | **123** | 4.49× fewer |
| raw full-lane-width (≥ 3.2 m) lateral steps | 134 | **5** | **26.8× fewer** |
| max single-step lateral offset | 6.4169 m | 4.5007 m | 1.43× lower |
| lateral offset p99 | 0.5704 m | 0.4852 m | 1.18× lower |
| longitudinal reversals | 5 | 1 | 5× fewer |
| `accel_within_hard_bound_frac` | 0.998671 | 0.998233 | **slightly worse** |

Reported honestly: the sublane model does **not** fix the acceleration gate and marginally worsens
it (311 → 415 out-of-band samples). That is consistent with the residual being longitudinal — SL2015
changes *where and when* vehicles change lanes, so the set of junction crossings differs between the
two runs, and the count of longitudinal remaps happened to rise. The metric it was aimed at
(`lateral_discontinuity_events`) improves 4.5×, and the raw one-lane-width teleport signature — the
thing a position-plausibility detector keys on — is down 26.8×. **The 0.0 ev/veh-km target is still
not met on any SUMO run.**

### Known unrealisms confirmed by measurement

1. Neither engine reaches 100 % acceleration plausibility, but the size of the miss was overstated
   ~4× before the estimator fix: it is now 0.126 % (InTAS, was 0.53 %) and 0.058 % (Python, was
   0.23 %) of samples outside [−8, +4] m/s². The residual is real (see "Acceleration — corrected").
2. **Lane changes are still discontinuous** on the SUMO path even with the sublane model on:
   0.130 ev/veh-km on the best run against a 0.0 target. A 3.2 m instantaneous lateral jump is
   exactly the signature a V2X position-plausibility detector keys on, so this matters directly for
   the misbehaviour dataset.
3. The Python engine has **no collision detection**: 290 distinct vehicle pairs sit < 1 m apart at an
   identical timestamp over a 300 s grid run. The MOSAIC/SUMO path has 0 (SUMO enforces it).
4. Fundamental-diagram capacity misses the 1800–2400 veh/h/ln anchor from *both* sides — 517 on the
   Python grid (under) and 2759 on the MOSAIC highway (over). Part model, part measurement artefact:
   the FD is a space-time *cell* approximation because `gt_emissions_sample` carries no edge id.
5. Headway shape is off the Cowan-M3 family fit on both engines (KS 0.174 Python vs a 0.15 gate,
   0.112 MOSAIC vs 0.10). Sub-floor headways are now essentially absent (0.17 % / 0.0 %) and the
   MOSAIC median is a plausible 3.0 s.
6. **Both radios are still hard cutoffs**: the 90 %→20 % PDR gray zone is 81 m (Python) and 32 m
   (MOSAIC) against the ≥ 100 m gate. That is the Phase-2 target, and it is the correct reading —
   levels are crossed on the non-increasing majorant of the measured curve, so a step-function radio
   cannot pass on Poisson noise in one distance bin.
7. **`gt_emissions_sample` still carries no true speed or heading field**, so every kinematic quantity
   is still reconstructed by double-differencing `true_x`/`true_y`. Adding one would fix defect (B) at
   the source, but it would change `data_digest` and is forbidden by the record-schema invariant. This
   is the standing reason the acceleration gate cannot be cleanly closed from the harness side.

### Harness corrections (2026-08-30, review follow-up)

The numbers above supersede the first Phase-0/1 measurement. Five harness defects were fixed and the
baselines re-measured; the differences are the harness, not the simulators:

| Metric | was | now | why |
|---|---|---|---|
| `comm.pdr_gray_zone_width_m` (MOSAIC) | 479 m, **pass** | 32 m, **fail** | the first downward crossing of a noisy curve put d90 on a single Poisson dip at 250–300 m; crossings now use the non-increasing majorant, and distance bins under 30 co-presence pairs are masked |
| `traffic.headway_below_floor_frac` (MOSAIC) | 0.56, fail | 0.00, pass | leaders were paired across ADJACENT LANES (99.95 % of sub-floor headways were between points > 1.5 m apart laterally, median offset 3.20 m = one SUMO lane); the leader is now the nearest vehicle ahead *within half a lane width* |
| `traffic.headway_p50_s` (MOSAIC) | 0.43 s | 3.01 s | headways were censored by the 50 m grouping cell (max observable 2.11 s at the median speed); no spatial cell is used any more |
| `traffic.fd_capacity_veh_h_lane` (MOSAIC) | 5201 | 2759 | a square cell summed parallel roads and opposing directions onto one lane; cells are directional and the per-lane divisor is measured from the lateral spread (median 2, max 7 lanes/cell) |
| `traffic.teleport_events` | pass on n ≥ 1 | `na` below 30 pairs | no sample floor, and jumps across gaps longer than 2 s were dropped entirely; the scan now covers every consecutive sample pair |
| *(new)* `traffic.moving_vehicle_frac` | — | HARD gate | the other three HARD metrics are impossibility checks that a permanently parked fleet passes |

### Harness corrections (2026-08-30, second review pass)

Three further defects were found and corrected. Again: these are the harness and the SUMO
configuration, not the Python engine — the determinism canary and the `mock_pipeline/` zero diff both
hold across all of it.

| Metric | was | now | why |
|---|---|---|---|
| GEH report labelling | `geh.*` gate ids, FHWA Vol III citations, "4/4 gates pass" | `seed_stability.*` gate ids, in-tool derivation, leading `warning` key | the comparison was simulation-vs-simulation all along; FHWA validation vocabulary was being applied to a reproducibility check (see "GEH" above) |
| GEH scale | veh/h extrapolated from a 300 s window, graded at `GEH < 5` | window-native counts, `GEH_w < 2.7718` | `GEH(k·m,k·c)=√k·GEH(m,c)`; the 300 s→veh/h extrapolation inflated every GEH by √12 = 3.4641 against a criterion defined for hourly volumes |
| `traffic.accel_within_hard_bound_frac` (InTAS) | 0.9947, `accel_min` −275 m/s² | 0.998744, `accel_min` −39.2 m/s² | a 3.2 m one-sample lane-change teleport was read as a 33 m/s longitudinal speed; steps are now decomposed against the direction of travel and discontinuities are relocated, not deleted |
| `traffic.speed_max_mps` (`datasets/smoke`) | 52.10 m/s | 42.93 m/s | the reported speed is now the longitudinal component; the raw chord value is kept beside it as `speed_max_raw_chord_mps` |
| *(new)* `traffic.lateral_discontinuity_events` | — | SOFT gate, ref max 0.0 ev/veh-km | the artefact the acceleration screen removes needed somewhere to go, so screening relocates evidence instead of destroying it |
| SUMO lane changes | instantaneous centreline snap (LC2013) | sublane model on by default, `--lateral-resolution 0.8` (SL2015 auto-selected) | real vehicles traverse laterally over ~2–4 s; measured 4.5× fewer lane-change teleports and 26.8× fewer full-lane-width jumps on a matched pair |

### Caveats on the numbers above

- **The lateral metric is only valid at `emit_sample_prob = 1.0`.** On the sparse InTAS gate run
  (emit_p 0.02, median sample gap 9.7 s) it reports 0.6799 ev/veh-km, but **473 of those 535 flagged
  events (88.4 %) sit on sample pairs wider than the harness's own 2 s finite-difference ceiling** —
  median gap 10.2 s, median "lateral offset" 43.7 m, max 891.7 m. Those are vehicles that drove round
  a corner between two widely spaced samples, not lane changes. Restricted to `dt ≤ 2 s` pairs the
  same run reads 0.0788 ev/veh-km, so the published figure is an **8.6× overstatement**. Cause: the
  lateral flag at `realism_bench.py:494` does not apply the `usable` (`dt ≤ MAX_FD_DT_S`) mask that
  the acceleration series applies at `realism_bench.py:518,522`, and the rate at
  `realism_bench.py:1045-1056` normalises over all pairs. **Only the full-trace rows in the tables
  above should be cited.** The same artefact shows in the raw diagnostics of the sparse Python run
  (`max_lateral_step_m` 443.9 m) although its heading guard there suppressed every event.
- `datasets/smoke` and `datasets/realism_baseline/mosaic_smoke_realismmode` are **both pre-sublane**
  (neither manifest carries `lateral_resolution_m`), so the difference between their lateral rates
  (0.458 → 0.398) is not a sublane effect and must not be read as one. The InTAS matched pair is the
  only controlled sublane comparison.
- `datasets/realism_baseline/sumo_accel_intas_urban_low_300s.json` records a `file:` path inside a
  scratchpad directory. Its numbers are sound but its input is not archived in the repo.

### Roadmap gate status after this pass

| Roadmap gate | Status | Evidence |
|---|---|---|
| Phase 0 — harness runs on `datasets/smoke` + a fresh python-flow run, complete scorecard JSON, baseline numbers recorded (ROADMAP.md:88) | **MET** | 8 scorecards in `docs/realism/baselines/`, 22 metrics each, expected failures documented above |
| Phase 1 — CAM rate: mean ≤ 400 ms, histogram spans 1–10 Hz (ROADMAP.md:100) | **MET** | mean 0.202 s, p50 0.200 s, histogram `{1 Hz: 131, 2: 31, 3: 4504, 5: 143 374, 10: 3084}` |
| Phase 1 — SUMO health: 0 teleports / 0 overlaps | **MET on InTAS**, not on `mosaic_smoke_realismmode` | InTAS runs 0/0; the pinned smoke baseline has 1 genuine teleport (HARD fail) |
| Phase 1 — accel ≥ 95 % within ±3 m/s² (ROADMAP.md:102) | **MET** | 0.9592 / 0.9597 / 0.9595 (InTAS), 0.9955 (smoke), 0.973 (Python) |
| Phase 1 — accel 100 % in [−8, +4] m/s² (ROADMAP.md:102) | **NOT MET** | best full-trace run 0.999964; genuine longitudinal remaps and Python-engine dynamics remain, corroborated by the SUMO-native FCD check |
| Phase 1 — GEH < 5 on ≥ 85 % of InTAS loop stations (ROADMAP.md:101) | **BLOCKED** | no real-world measured counts exist in the tree; only a simulation-vs-simulation seed-stability check is possible today. Needs `--ref-counts` data — see "GEH" above |
| Phase 1 — headway KS vs highD-derived urban reference improves ≥ 20 % relative to Day-0 (ROADMAP.md:102) | **NOT MET** | KS 0.191 / 0.166 / 0.187 (InTAS) against a 0.15 gate; no 20 % relative improvement demonstrated |
| Lane-change continuity (`lateral_discontinuity_events` = 0.0 ev/veh-km) | **NOT MET**, materially improved | 0.5851 → 0.1302 on the matched pair; Python engine reads 0.0 |
| Determinism / manifest contract (hard invariant) | **HELD** | digest `f0ec3cc0…f32c48` reproduced exactly; `mock_pipeline/` zero diff; 222 targeted tests pass |
| Phase 2 — PDR gray zone ≥ 100 m | **NOT MET** (Phase-2 target, not yet started) | 18–81 m across runs |
