# Realism push — progress log

Mission: SOTA traffic + network realism (see [ROADMAP.md](ROADMAP.md)). Every phase gates on a
quantitative benchmark; determinism/manifest contract and config→GUI pipeline are hard invariants.

## Baseline (2026-08-29, main @ 2832d63)

- Test suite: **496 passed** in 607 s (Python 3.12.10, SUMO 1.25.0, JDK 17.0.20.1, MOSAIC 25.2).
- Reference run: `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15
  --traffic-lights --seed 42` → 593 vehicles, 7823 reports, data_digest
  `f0ec3cc0baa55a2fdbc3b445455dda26baba0303725bd2cf8e17375081f32c48`, precision 0.599 / recall 0.91,
  latency_med 4.0 s. **Superseded 2026-08-30 by ADR 0002** (ground-truth kinematics): the same run
  now digests `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` with every count and
  metric unchanged. See "ADR 0002 — ground-truth kinematics + digest re-pin" below.
- Realism scorecard: none yet (Phase 0 builds it). Known Day-0 realism defects are ranked G1–G16 in
  ROADMAP.md §2.

## Adversarial review of the directed-road workstream (2026-08-31)

The channel-plugin and directed-road workstreams both landed without their verifier agents. The
plugin findings and their fixes are in
[PLUGIN-CONFORMANCE-EVIDENCE.md §7](PLUGIN-CONFORMANCE-EVIDENCE.md). The road findings, all
reproduced before they were fixed:

**Gates after the whole review.** Full suite **912 passed in 990.85 s**, exit 0. The reference run
`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights
--seed 42` still digests `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` with
593 vehicles / 7823 reports / 152 investigations / 152 revoked and precision 0.599 / recall 0.91 /
latency_med 4.0 s — every count and metric unchanged. Nothing in this review moves a pinned digest:
the three new road knobs default off, and no built-in channel model touches `RngNamespace`.

- **The directed carriageway geometry was unreachable from any config or CLI invocation.**
  `roads.enable_directed_lanes()` and `roads.edges_from_directed()` existed, were tested, and were
  correct — and `grep` over `run.py` found **zero** occurrences of `enable_directed_lanes`,
  `directed_edges`, `carriageway`, `largest_strong_component` or `drive_side`, while
  `PipelineConfig.__dataclass_fields__` had no field matching `direct*`/`carriage*`/`oneway`/
  `drive_side`/`lanes_per`. **Every** producer of the headline "head-on overlaps → 0" result — the
  commit message, `tests/test_directed_roads.py`, `datasets/_netfidelity/_ab_directed.py` — installed
  a `class _Directed(GridNetwork)` subclass over `roads.GridNetwork`, each stating in its own
  docstring that "run.py is owned by a parallel workflow". The measurement was real; the shipped
  product could not produce it. **Fixed:** `directed_lanes`, `drive_side` and
  `custom_network_directed` are config fields with CLI flags, `config_schema()` entries and
  `validate_config` rules, all default-inert. The end-to-end test now sets `directed_lanes=True` on
  a `PipelineConfig` instead of monkeypatching `roads`, so it fails if the wiring is ever removed
  rather than quietly measuring its own subclass.
- **The `custom_network` loader read only the UNDIRECTED `edges` array.** `osm.network_document`
  writes `{nodes, edges, directed_edges, signal_nodes}`; `run._parse_custom_network` returned
  `doc["nodes"], doc["edges"]` and dropped the rest, so every one-way flag, per-direction lane count
  and shape polyline `netimport.py` and `osm.py` extract was discarded on the only path a user can
  take. Measured on the real Ingolstadt extract (`osm_to_network(attrs=True)`): the document carries
  **627** directed records with lane counts 1..3 and `oneway_share 0.2823` (the netconvert importer's
  own reading of the same city is 433 records at 0.2794), and the network the engine built from that
  document reported `directed=False`, **0** one-way edges, **0** lane-specified edges — the
  pre-change model. **Fixed:** `custom_network_directed=true` builds from
  `directed_edges` via `roads.edges_from_directed`, then trims to `largest_strong_component` (a
  bbox clip leaves junctions you can enter and never leave, which `CustomNetwork` refuses). Same
  document, re-measured: **326 nodes / 610 edges, `directed=True`, 160 one-way, 385 lane-specified.**
  The default stays undirected so every pre-schema document loads exactly as before.
- **`tools/network_fidelity.py compare`'s `netconvert` row graded the importer against the exact
  `.net.xml` it had just parsed** — `_resolve_gt_net` and `_candidate_for(..., "netconvert")` both
  resolve to `net_816d9f25303fea7e_636359.net.xml` (`os.path.abspath` equality `True`) — producing a
  guaranteed `6 pass / 0 fail` with `oneway_share_pp`, `degree_ks`, `intersection_rel_err` and
  `lane_ks` all exactly `0.0`, and nothing in the output saying so. **Fixed:** the tool detects the
  identity, prints a banner **above** the numbers, marks every metric informational, and ends the row
  `SELF-GRADED, NOT fidelity evidence (parser round-trip against its own input file)`. The row that
  does measure something — raw OSM — still **fails** the hard `degree_ks` gate (0.1973 vs 0.10) and
  misses signalised-node count by 3.5× (50 vs 11); that is a real open defect, not a regression.
- **The projection alignment gate was calibrated wider than the city it guards.** `tol = max(250,
  0.5 × expected diagonal)` around the whole bbox = **952.9 m** for Ingolstadt, and the test was
  "median node inside the bbox WIDENED BY that". On the real 217-junction cloud (median 134.4 m west
  and 52.4 m south of the bbox centre, half-extent 734 × 608 m) that accepts an east-west translation
  of **(734 + 134.4) + 953 = 1821 m** and a diagonal one of **1613 × √2 = 2281 m** — against a
  1468 × 1216 m extent. A city could be projected entirely off itself and pass. It also could not
  catch the trap its own error message names (`ky = 110540, not 111320`), which displaces nodes by
  only 10.6–18.7 m, because `expect_bbox_xy` is derived from the same projection tuple under test.
  **Fixed:** two independent arms, calibrated on the seven cached cities rather than guessed — the
  median node against the bbox **centre** per axis at `max(250 m, 0.25 × extent)` (worst real offset
  fraction 0.104, so ~2.4× headroom; undetected translation now 233–499 m), and the containment
  fraction the function already computed and threw away, floored at 0.60 (correct imports measure
  0.838–0.899). The scale constant is now checked directly by `_assert_frame`, called from
  `_transformer` before any point is projected, so the named trap fires regardless of geometry.

## Phase log

| Date | Phase | Status | Gate result |
|------|-------|--------|-------------|
| 2026-08-29 | Investigation (13 agents) | done | Roadmap adopted |
| 2026-08-29 | Phase 0 — realism bench harness | started | — |
| 2026-08-29 | Phase 1 — MOSAIC/SUMO flagship realism | started | — |
| 2026-08-30 | Phase 0 + Phase 1 — integrated | done | see "Phase 0/1 integration" below |
| 2026-08-30 | Phase 0/1 review — GEH labelling, accel estimator, sublane | done | 3 defects corrected, baselines re-measured; 2 roadmap gates still not met, 1 blocked |
| 2026-08-30 | ADR 0002 — ground-truth kinematics + digest re-pin (Python engine) | done | 22 pinned digest sites moved in 13 test files; 656 passed; reproducibility re-verified |

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
to the reference. `git diff --stat -- src/scms_sim_ref/mock_pipeline/` is empty. *(Historical: this
was measured before ADR 0002. The same command now yields `b25f2137cf14dd50…` with identical counts
— see the ADR 0002 section at the end of this file.)*

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
  reproducibility check that passes 4/4 of its own gates, whose thresholds are now **empirically
  calibrated from 20 seeds** rather than asserted (REVIEW-FINDINGS C1 + C2; see
  [`SEED-STABILITY-CALIBRATION.md`](SEED-STABILITY-CALIBRATION.md)). Without a calibration on disk
  the same check reports `uncalibrated` and no gate of it may be quoted as passing. The roadmap gate
  "GEH < 5 on ≥ 85 % of InTAS induction-loop stations" (ROADMAP.md:101) **cannot be claimed** and
  must not be quoted.
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

**Thresholds are no longer invented (2026-08-30, REVIEW-FINDINGS C1 + C2).** The result quoted here
until 2026-08-30 — "4/4 gates pass" at `GEH_w < 2.7718`, Bonferroni bound 4.3163, total-flow
tolerance 0.03 — was produced by two *asserted* null distributions, and both were measured to be
wrong: the 0.03 tolerance false-alarmed on **86 of 190** seed pairs of the unchanged scenario
(45.3 %), and the `Binomial(n, ½)` per-station bound was ~24× too conservative (measured exceedance
9/4269 = 0.0021 against a documented 0.05), leaving the GEH gates with almost no power. Both
thresholds are now **derived from 20 SUMO runs of the scenario**; see
[`SEED-STABILITY-CALIBRATION.md`](SEED-STABILITY-CALIBRATION.md) and
`tools/calibration/seed_stability_intas_urban_low.json`.

Current result on the same pair (seed 23423 vs 987654, window-native counts, 25 shared stations, 3
both-zero stations `4070 / 4160 / 4210` excluded → 22 compared): **4/4 calibrated seed-stability
gates pass** — station pass fraction 0.9091 ≥ 0.9091 at the calibrated `GEH_w < 1.5370`; worst
station 2.2188 ≤ the calibrated family-wise bound 2.7735; total flow 243 vs 242 vehicles, relative
error 0.00413 ≤ the calibrated 0.0918; count tolerance 0.8636 ≥ 0.8636. Measured report-level
false-alarm rate on unchanged runs: **6/190 = 3.2 %** in-sample, **26/380 = 6.8 %**
leave-one-seed-out — against 45.3 % before.

Two stations are now individually flagged as **outside** the per-station band while the aggregate
gates still pass: `4140` (9 vs 17 vehicles, GEH 2.2188) and `8002` (13 vs 7, 1.8974). Under the old
2.7718 bound both were reported as passes — `4140` is the exact exhibit REVIEW-FINDINGS C2 named.
Three stations carry `geh_veh_h ≥ 5` (4140 = 7.69, 8002 = 6.57, 1011 = 5.24) while their
window-native GEH is 2.22 / 1.90 / 1.51; the old FHWA-labelled gate would have branded those
validation failures, which is a scale error, but they are *not* uninteresting either — the first two
are exactly the ones the calibrated per-station band now flags.

What the gate can see is now reported instead of assumed: at the calibrated bound a station must
change by **×1.63** (median over the 22 compared stations; best ×1.29, worst ×3.24) to leave the
band, and the five 1–3-vehicle stations at which even a 2× change stays invisible over a 300 s
window are named in the report. Under the old bound the median was ×2.09 at a 10-vehicle station,
i.e. **a doubling of flow was undetectable**.

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
`gt_emissions_sample` carried `true_x` / `true_y` but **no true speed** (ADR 0002 added
`true_speed`/`true_heading` on 2026-08-30 — datasets generated after that no longer have this root
cause; the numbers in this section were measured before it), so the harness derived speed
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
7. ~~**`gt_emissions_sample` still carries no true speed or heading field**, so every kinematic
   quantity is still reconstructed by double-differencing `true_x`/`true_y`.~~ **FIXED 2026-08-30 by
   ADR 0002** — the record now carries `true_speed`/`true_heading` and the digest was deliberately
   re-pinned (see "ADR 0002 — ground-truth kinematics + digest re-pin" at the end of this file). The
   numbers in the tables above were all measured on differenced positions and predate the fix.

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
| Determinism / manifest contract (hard invariant) | **HELD** | digest `f0ec3cc0…f32c48` reproduced exactly; `mock_pipeline/` zero diff; 222 targeted tests pass. *(Re-pinned to `b25f2137cf14…` by ADR 0002 on 2026-08-30 — deliberate, documented, counts unchanged.)* |
| Phase 2 — PDR gray zone ≥ 100 m | **NOT MET** (Phase-2 target, not yet started) | 18–81 m across runs |

## ADR 0002 — ground-truth kinematics + digest re-pin (2026-08-30, Python engine)

Caveat 7 above ("`gt_emissions_sample` still carries no true speed or heading field") is **closed**.
`ground_truth/gt_emissions_sample.jsonl` now carries `true_speed` (m/s) and `true_heading` (deg),
written verbatim from the simulator's own state at emission time — `run.py:2664-2677`, fed by the
`tx.true_state(t)` tuple already computed at `run.py:2553` and carried on the broadcast at
`run.py:2618-2621`. `manifest.schema_versions.ground_truth` is now **2** (`ma_visible` stays 1), and
a `conventions` block records `heading: deg_ccw_from_east`, `speed: m_s`, `position: m_local_xy`.
Both field names were already in `FORBIDDEN_FEATURE_KEYS`, so the leakage firewall needed no change.

What the field buys, measured on an all-honest full-trace run (seed 7, grid 5×5, 60 s,
`emit_sample_prob=1.0`; 1925 samples / 72 vehicles / 1853 consecutive 1.0 s pairs):
`|chord_speed − true_speed|` has median **0.0000 m/s**, p95 **0.0010 m/s**, max **12.7350 m/s** —
a spike at zero with a fat tail exactly at turns and lane offsets, which is why differencing looked
acceptable in aggregate while blowing up the acceleration gate. Bearing error against `true_heading`
read CCW-from-East: median **0.0000°** (n=1835); read CW-from-North: median **90.0000°**.
New `tests/test_gt_kinematics.py` (10 tests) pins completeness, truth, convention and the ORACLE
firewall. `tools/verify_data.py` over three fresh schema-v2 datasets: 0 failures (L1/L2/L3 leakage,
I1/I2 digest integrity and V4 emissions-truth all 3/3).

Consumer-side confirmation: `realism_bench` on the new reference dataset accepts both fields on
**270/270 tracks** and independently detects the convention —
`convention_residuals_deg = {deg_ccw_from_east: 0.0, deg_cw_from_north: 90.0,
rad_ccw_from_east: 53.24, rad_cw_from_north: 90.0}`, matching `manifest.conventions.heading`. Every
kinematic metric on that run now reads `kinematics_source: "ground truth true_speed"` /
`"ground truth true_heading (deg_ccw_from_east)"` rather than a differenced reconstruction
(scorecard at emit_p 0.03: 9 pass / 0 fail / 13 na).

**The engine's behaviour did not change; only the record it writes did.** Proof: for all eight
datasets (seven re-pinned configs + the reference run) the post-change dataset was rewritten with the
two keys stripped from each emission row and re-hashed under `run._data_digest`'s exact rule — each
one reproduced its pre-bump digest byte-for-byte, so no other file, no row order and no RNG draw
moved. The reference run's counts are also
unchanged: 593 vehicles / 7823 reports / 152 revoked / precision 0.599 / recall 0.91 / latency_med
4.0 s, exactly as before.

### Superseded → new digests

| Config | pre-bump | post-bump |
|---|---|---|
| default (seed 7, grid 5×5, 60 s, 1.5/s, atk 0.25) — 11 test files | `04ae9736f519…cee38` | `0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740` |
| multi-attack (seed 13, 8 types) — `test_attack_magnitude` | `8894cb268af3…3f63c` | `48013901c241b400e2bfaf02d393ab6e0026df8a23d20e139dec6693f2726e5d` |
| collusion+RSU+logdistance (seed 11) — `test_config_knobs` | `53abb36711ae…806f3` | `939b4faa726853675f81453e2891bc155e2fa18ea58cac4edbdcba865df8ae2c` |
| VRU+DENM (seed 13) — `test_config_knobs` | `4628e01edeb3…44d1` | `b3a01d40354c838ffc04c10dcdebf60cf8652b0b67ef694007128011c6d11d46` |
| grid + traffic lights — `test_gap_acceptance`, `test_network_fidelity` | `b0bae9e4fc04…a0b8` | `fe1a58002f468b3124aa24fc26681fb9e69bb63df71e543650289e2034f699e6` |
| multilane 6×6×3, lane_changes off — `test_lane_changes` | `38845a32f35e…a858` | `0a9e82ec549f876843ba39cce241ab28fdb39ea94900151d511e64e8c93f2277` |
| ring (8 blocks) — `test_network_fidelity` ×2 | `ff1cddd82227…a989b` | `32133dd19efd90b280b7e6ea13e4dda6b23f33f605344c1fe3d740d95ea5da50` |
| **reference run** `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42` | `f0ec3cc0baa55a2f…f32c48` | `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815` |
| MOSAIC `gen_smoke` reference dataset (Java engine, same change set) | `b1789aeb90a2…c135a` | `c7efff8075ddba47724fb7d48306e94b7f127f2f781724e509866fb896e0d862` |

22 pinned digest sites across 13 test files were re-pinned in the same change so the suite is never
left red; each constant carries an inline `# ADR 0002 re-pin` note with its superseded value.
`test_network_fidelity.py:55` pins `460b4cd04b0b…` in an *inequality* and was left alone; the
ring-with-lights digest is now `205a0856f04c36e852fb6154b17624f4340909e3e30b7d768de7e366bbc8c8e2`.

### Reproducibility contract re-verified

- Reference config run twice into different directories → `b25f2137cf14dd50…` both times.
- Same config, seed 43 → `0c54e935b11979ce5002efd2d229847ad6936bfab2fb578105817b77ca34515d`
  (599 vehicles / 6585 reports / 139 revoked): a different seed still gives a different digest.
- All seven re-pinned configs run twice each: every pair matched.
- Full suite green: **656 passed in 725 s** (646 before `test_gt_kinematics.py` was added).

### Heading convention (open, cross-engine)

The Python engine writes `true_heading` in its native math convention — degrees **counter-clockwise
from East**, `[0, 360)` (`run.py:667`; the detector bearing at `run.py:2823` matches). The
MOSAIC/Java engine writes degrees **clockwise from North** (SUMO / ETSI EN 302 637-2). Both are
self-consistent, but `true_heading`/`claimed_heading` mean different things in the two datasets.
Rotating the Python convention would move every claimed heading, the `HeadingOffset` attack and the
heading detector — behaviour, not schema — so it was deliberately **not** done here. The convention
is instead declared in `manifest.conventions.heading`, and `realism_bench` detects it per track.
Unifying on `deg_cw_from_north` remains a follow-up with its own digest move.

### Stale artefacts for their owners

`datasets/realism_baseline/python_flow_grid6/` (manifest still records `f0ec3cc0…`, emissions lack
the new fields) needs regeneration by the baselines owner; that regeneration will also flip the
harness's `kinematics_source` from differenced positions to ground truth, which is the point of the
ADR. The scorecards in `docs/realism/baselines/` follow from it.

## The traffic panel was measuring enforcement (2026-09-03)

Full write-up: [TRAFFIC-PANEL-SURVIVORSHIP.md](TRAFFIC-PANEL-SURVIVORSHIP.md). Short version:
`PYTHON-ENGINE-VALIDATION.md` §1 found that `gt_emissions_sample.jsonl` from a long run is not a
traffic sample — it is written inside the broadcast pre-pass, so a revoked vehicle's kinematic
record ends at revocation while the vehicle keeps driving — and `realism_bench` computed its whole
traffic panel from it. Fixed and re-measured.

**What changed.** (1) `PipelineConfig.emit_mobility_oracle` / `--emit-mobility-oracle`, default OFF,
writes `ground_truth/gt_mobility_oracle.jsonl` from *before* the enforcement gate: one row per
active station per step, whatever the CRL says. (2) `manifest["counts"]["mobility_survivorship"]`
now carries the step-loop tallies on **every** run — `counts` is outside `data_digest` by
construction, so it is unconditional and moves nothing. (3) `realism_bench.resolve_mobility_source`
picks oracle → frozen SUMO trace (only when asked for) → truncated emissions, names the choice in
the scorecard, publishes survivorship as two first-class metrics, and withholds the
density-dependent metrics below `SURVIVORSHIP_MIN_FRAC = 0.90`.

**Digests.** Reference run still `b25f2137cf14dd50…`; default golden still
`0bd93655a2d5bebb…`. Switching the opt-in on over `py_intas_300s` and `py_intas_300s_internal`
leaves **10 of 10 pre-existing files byte-identical and adds exactly one**.

**Survivorship measured.** Default golden **0.8863**; reference golden **0.8126** (the 300 s window
this repo calibrates on was already losing 18.7% of its vehicle-steps); InTAS 300 s replay
**0.9077**; **InTAS AM peak hour 0.4378** (5,949,526 of 13,589,568). The obvious in-dataset
estimator reads 0.7106 there — it overstates survival **1.62×**, because exposure is what earns a
false positive so never-revoked vehicles are systematically short-trip vehicles.

**Verdicts that change** (unbiased source, same code, same settings): peak-hour replay
`speed_p50` **4.772 pass → 1.432 FAIL**, `fd_capacity` **763.6 → 1130.0**, `overlap_events`
**28 → 169**, `teleport_events` **1 → 11**, FD density p99 **24.83 → 53.83 veh/km/lane**,
congested cells **240 → 2,043**; 300 s internal IDM `headway_ks` **0.1498 pass → 0.1562 FAIL**.
The headway question is settled: the SUMO-vs-internal ordering at midnight survives the correction
(0.2036 vs 0.1562, both now failing), and the peak-hour reversal strengthens (replay 0.1139 →
**0.0830**).

**The mechanism is a congestion filter.** Halting vehicle-steps survive into the dataset at
**0.3638**, moving ones at **0.4898** — a stopped vehicle sits in a jam surrounded by neighbours,
collects the co-located benign reports that trip the revocation threshold, and takes the rest of its
congested trip out of the record. The dataset is biased toward free flow, which is why the congested
branch of the fundamental diagram was the part that moved most.

**The false-positive rate is DENSITY-driven, and measured rather than tuned.** At fixed 300 s,
revocation precision falls 0.9167 → 0.3061 over a 31× concurrency range while **recall stays flat at
0.90**; along the duration axis it plateaus at 0.54–0.58 from 300 s to 2400 s. Over a 12×
concurrency range the *per-report* false-positive rate moves −7% while *revocation* precision
collapses 59%, so what degrades is not the detector's judgement but an absolute
`report_threshold_k = 3` inside a neighbourhood whose size is not held constant. The synthetic grid
at 937 concurrent reproduces the real city's peak (precision 0.3061 vs 0.3077, 2.27 vs 2.25 false
revocations per true one). No detector was touched.
