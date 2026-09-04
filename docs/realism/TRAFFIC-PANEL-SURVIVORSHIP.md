# The traffic panel was measuring enforcement

`PYTHON-ENGINE-VALIDATION.md` section 1 found that `gt_emissions_sample.jsonl` from a long run is
not a traffic sample. This document is the repair and the re-measurement. **Every traffic number
that document, and every document before it, published for the Python engine was read off a stream
the misbehaviour authority truncates**, and the correction is large enough to change verdicts:

* on the **InTAS AM peak hour** the median true speed is **1.432 m/s, not 4.772** — the truncated
  stream read 3.33 m/s fast and **turned a `speed_p50` PASS into a FAIL**;
* **`fd_capacity` 763.6 → 1130.0 veh/h/lane** (+48%), with per-lane density **24.83 → 53.83
  veh/km/lane** (×2.17) and congested cells **240 → 2,043** (×8.5);
* the two HARD sim-health counters were understated by most of their value: **`overlap_events`
  28 → 169** (×6.0) and **`teleport_events` 1 → 11** (×11);
* the 300 s internal-IDM arm's **`headway_ks` 0.1498 PASS → 0.1562 FAIL**.

And the mechanism turned out to be sharper than "some vehicles go missing". **The MA is a
congestion filter.** Halting vehicle-steps survive into the dataset at **0.3638** and moving ones at
**0.4898** — a stopped vehicle sits in a jam surrounded by neighbours, collects the co-located
benign reports that trip the revocation threshold, and takes the rest of its congested trip out of
the record with it. That is why the loss is not a scale factor on the flow: it is a **biased sample
of traffic states**, and it biases the dataset toward free flow.

---

## 1. The mechanism, in one paragraph

`run.py`'s step loop writes the ground-truth emission inside the **broadcast pre-pass**, whose first
statement is `if enforced(tx, t): continue`. So the emission stream is a record of *what the MA
could hear*, which is exactly right for a detection dataset and exactly wrong for a traffic
measurement. `realism_bench.traffic_panel` computed its entire panel from it. The error is not a
constant and cannot be divided out: it grows with **run length** (a revoked vehicle loses all of its
remaining trip, and a longer run gives each vehicle more trip to lose) and with **density** (which
is what drives the MA's false-positive rate — section 6).

---

## 2. What changed

Three parts, and the split matters: only the first one touches the engine, and it is default-off.

### 2.1 An un-enforced ORACLE mobility record (`run.py`, opt-in)

`PipelineConfig.emit_mobility_oracle` / `--emit-mobility-oracle` writes
`ground_truth/gt_mobility_oracle.jsonl` from **before** the enforcement gate: one row per active
station per step, whatever the CRL says, whatever `emit_sample_prob` is, whatever the GNSS-jam draw
did. Each row carries `true_x/true_y/true_speed/true_heading` plus `revoked` and `broadcasting`, so
a consumer can reconstruct the truncated stream exactly by filtering on `broadcasting` and measure
the loss without a second file.

**Why this rather than only reading the frozen trace.** A trace exists only for a `sumo_replay` run.
The internal-IDM arm — which is half of every A/B in section 5 of `PYTHON-ENGINE-VALIDATION.md`, and
the whole of the default dataset corpus — has no external mobility artifact at all, so a
trace-only fix would have left the engine's own mobility permanently unmeasurable. The record is
also producer-local: it needs no path on the consumer's machine.

**A side effect worth knowing about: the traffic panel now works at the DEFAULT
`emit_sample_prob`.** The record is written per STEP, not per sampled message, so
`probe["full_trace"]` is true for it whatever the sampling probability is. A default run
(`emit_sample_prob = 0.03`) previously degraded every headway and fundamental-diagram metric to `na`
as a lossy trajectory; with the record on it scores all of them — measured on a 120 s grid run,
11,973 usable trajectory segments and a full panel at `emit_sample_prob = 0.03`. Raising
`emit_sample_prob` to 1.0 purely to get a traffic panel is no longer necessary, which is what
produced the 1.72 GiB `gt_emissions_sample.jsonl` on the peak-hour run.

**It is ORACLE and stays ORACLE.** It lives under `ground_truth/`, every row carries
`_visibility=ORACLE`, every row trips `leakage_linter.find_forbidden_keys`, and it takes the same
`_WithheldStream` path as `gt_report_labels` and `gt_emissions_sample` — an isolated third-party
detector cannot read it while it runs, which matters *more* for this file than for the others: it
carries true kinematics for vehicles the MA has already revoked, which is precisely the motion an
MA-side consumer is not entitled to.

**Digest safety is measured, not argued.** Same seed, same config, plus the one boolean:

| | |
|---|---|
| reference golden `--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42` | **`b25f2137cf14dd50…` unchanged** |
| default golden seed 7, flow, grid 5×5, 60 s, 1.5/s, atk 0.25 | **`0bd93655a2d5bebb…` unchanged** |
| `py_intas_300s` (SUMO replay, 300 s) with `emit_mobility_oracle=true` | **10 of 10 pre-existing files byte-identical, exactly 1 file added** |
| `py_intas_300s_internal` (internal IDM, 300 s) with it | **10 of 10 byte-identical, exactly 1 added** |

The record draws no RNG (`true_state(t)` is a pure read of the position the mobility provider just
wrote — the identical call the pre-pass makes four lines later) and mutates nothing, which is what
makes the insertion byte-identical rather than merely lucky.

### 2.2 Survivorship counters (`run.py`, UNCONDITIONAL)

`manifest["counts"]["mobility_survivorship"]` now carries the step-loop tallies on **every** run:
`vehicle_steps_simulated`, `vehicle_steps_broadcast`, `vehicle_steps_enforced_out`,
`vehicle_steps_emitted`, `vehicle_steps_survival_frac`, `vehicles_revoked`, and the mean **record**
and **simulated** spans split by revoked / never-revoked. `_data_digest` excludes `manifest.json` by
construction, so this is unconditional and moves no digest — which is the only reason it could be
made mandatory rather than opt-in.

It has to come from the engine because **a truncated stream cannot report what it did not record.**
Section 3 measures how badly the obvious in-dataset estimator fails.

### 2.3 A named source, and a degradation rule (`realism_bench.py`)

`resolve_mobility_source` picks the traffic panel's input and puts the choice in the scorecard:
`gt_mobility_oracle.jsonl` → a frozen SUMO trace → the broadcast emissions, marked `truncated`.
`--traffic-source {auto,oracle,trace,emissions}` forces one; forcing an unavailable one raises
rather than falling back silently, because *which stream produced this number* is the whole question.

**The trace is never picked up implicitly**, and that is deliberate: `manifest.config.sumo_trace` is
an absolute path on whatever machine froze the artifact, so honouring it under `auto` would make one
dataset score differently on two hosts depending on whether that file happens to still be there —
host-dependence, inside a module whose stated contract is determinism. It is used only when asked
for: `--sumo-trace <path>`, or `--traffic-source trace` to follow the manifest's pin.

The **COMM panel deliberately keeps reading the broadcast stream** — what the MA could actually hear
is the honest input to a reception measurement. The two panels now read different files on purpose.

Two new first-class metrics, `traffic.survivorship_vehicle_steps_frac` (graded against a stated
`SURVIVORSHIP_MIN_FRAC = 0.90` floor) and `traffic.revoked_vehicle_frac`, and **every** traffic row
carries `details.mobility_source` + `details.vehicle_steps_survival_frac`. Below the floor the
**density-dependent** metrics — headways, fundamental diagram, overlaps — are published as `na` with
the measured fraction instead of a number. The per-sample distributional metrics (speed quantiles,
acceleration fractions, lateral discontinuities per vehicle-km, moving fraction) are *not* withheld:
they are distorted only through which steps survive, that distortion is second order at the
durations where it can be checked, and it is now measurable (section 4 measures it, and at peak it
is not small — see the caveat there).

**A legacy dataset is withheld too, on a labelled proxy.** A dataset generated before this change
carries neither the record nor the tallies, so its exact surviving fraction is gone — but its
revoked-vehicle fraction is two file lengths and always available. When survivorship is
unmeasurable, `1 − revoked_vehicle_frac` decides whether to withhold. It is explicitly **not** a
bound in either direction (0.386 against a true 0.4378 on the peak hour; 0.919 against 0.9077 on the
300 s replay) and is never published as a survivorship value — it only chooses between publishing a
number and saying why there isn't one. Without this rule `datasets/py_intas_hour` would go on
reporting `fd_capacity` 763.6 under the new harness, which is the exact failure the change exists to
stop.

**One exception, and it keeps the CI gate armed.** Truncation removes vehicles, so it can only
*decrease* a co-presence count: the truncated `overlap_events` is a valid **lower bound**. A value
that already breaches its `≤ 0` reference is therefore a real breach that more traffic can only make
worse — measured on the peak hour, 28 truncated against 169 unbiased, and both fail. So a **failing**
one-sided count keeps its failure (labelled `LOWER BOUND` in the row's details) while a **passing**
one from the same stream is still withheld, because a pass is exactly the verdict truncation can
manufacture. Without this carve-out the change would have disarmed a HARD gate on precisely the
datasets that need it most.

**The two unbiased sources agree exactly.** On `py_intas_300s` re-run with the oracle record, the
engine's own un-enforced stream and the frozen SUMO trace give **781,575 rows each** and
*bit-identical* values on every traffic metric — `fd_capacity` 874.7827 both, `headway_ks` 0.2036
both, `speed_p95` 27.439 both, `speed_p50` 12.055 both. That is an independent cross-check of the
trace reader (time base, heading convention, id mapping) and of the record, and it is why the
peak-hour numbers below, which are trace-sourced, are exactly what an oracle re-run would give.

---

## 3. Survivorship, measured

| run | vehicles | revoked | steps simulated | steps broadcast | **survival** |
|---|---|---|---|---|---|
| default golden (grid 5×5, 60 s, 1.5/s) | 73 | 12 (16.44%) | 1,952 | 1,730 | **0.8863** |
| **reference golden** (grid 6×6, 300 s, 2/s, lights) | 593 | 152 (25.63%) | 42,993 | 34,938 | **0.8126** |
| InTAS 300 s, internal IDM | 334 | 13 (3.89%) | 455,584 | 436,207 | **0.9575** |
| InTAS 300 s, SUMO replay | 334 | 27 (8.08%) | 781,575 | 709,454 | **0.9077** |
| **InTAS AM peak hour, SUMO replay** | 14,896 | 9,143 (61.38%) | **13,589,568** | **5,949,526** | **0.4378** |

Note the reference golden: **the 300 s window everything in this repository was calibrated on was
already losing 18.7% of its vehicle-steps**, not the ~7% the 300 s InTAS window suggested. The
enforced fraction is a property of the *density and the detector*, not of the duration alone.

### The in-dataset estimator is biased, and the sign is against you

The obvious repair — scale each revoked vehicle's record up to the never-revoked mean — does not
work, and fails in the flattering direction:

| InTAS peak hour | |
|---|---|
| mean **record** span, revoked | **296.332 s** (297.3 samples, n = 9,143) |
| mean **record** span, never revoked | **560.623 s** (561.6 samples, n = 5,753) |
| record-span truncation ratio | **0.5286** |
| naive survival estimate from those two numbers | **0.7106** |
| **true survival** | **0.4378** |
| **the estimator overstates survival by** | **1.62×** |

Why: **exposure is what earns a false positive**, so a never-revoked vehicle is systematically a
*short-trip* vehicle. Its 560.6 s record mean sits well below the true 912.3 s mean simulated span,
so it is the wrong counterfactual. The engine measures the same inversion on every run — on the
reference golden, revoked vehicles average an 82.3 s simulated span against 68.2 s for the
never-revoked. This is why survivorship is stamped into the manifest by the producer and reported as
`basis: "unmeasurable"` when a legacy dataset carries neither the record nor the tallies, rather than
estimated.

---

## 4. Old versus new, on every affected metric

`realism_bench --regime urban`, identical settings, identical code; the only thing that differs is
which stream the traffic panel read. **OLD** is what the harness published before this change (the
broadcast stream, ungated). **NEW** is the unbiased source named in the header.

### 4.1 InTAS AM peak hour — SUMO replay (`datasets/py_intas_hour`, unbiased = frozen trace)

Survivorship **0.4378**.

| metric | reference | OLD (truncated) | **NEW (unbiased)** | change |
|---|---|---|---|---|
| sample pairs | — | 5,934,630 | **13,574,672** | ×2.29 |
| `speed_p50_mps` | 3–14 | 4.772 **pass** | **1.432 FAIL** | **−70%, verdict flips** |
| `speed_p95_mps` | 8–22 | 17.158 pass | **15.681** pass | −8.6% |
| `speed_max_mps` | ≤ 60 | 55.603 pass | 55.605 pass | — |
| `accel_within_hard_bound_frac` | = 1.0 | 0.999964 fail | 0.999957 fail | — |
| `accel_within_comfort_frac` | ≥ 0.95 | 0.9857 pass | 0.9879 pass | — |
| `lateral_discontinuity_events` | 0 | 0.6311 fail | **0.6546** fail | +3.7% (26,087 → 50,663 events over 41,338 → 77,391 veh-km) |
| **`teleport_events`** | ≤ 0 | **1** HARD fail | **11** HARD fail | **×11** |
| **`overlap_events`** | ≤ 0 | **28** HARD fail | **169** HARD fail | **×6.0** |
| `moving_vehicle_frac` | ≥ 0.5 | 0.9583 pass | 0.9904 pass | +3.3% |
| `headway_p50_s` | (info) | 5.5116 | **7.0067** | +27% (p15 1.85 → 2.47, p85 18.6 → 25.7) |
| `headway_below_floor_frac` | ≤ 0.05 | 0.0001 pass | 0.0001 pass | — |
| **`headway_ks_shifted_exponential`** | ≤ 0.15 | **0.1139** pass | **0.0830** pass | **−27%** |
| **`fd_capacity_veh_h_lane`** | 1800–2400 | **763.6** fail | **1130.0** fail | **+48%** |
| `fd_backward_wave_speed_kmh` | 15–20 | −1.9541 fail (240 congested cells) | **3.4848** fail (**2,043** cells) | **sign flip** |
| FD density p99 (veh/km/lane) | — | 24.83 | **53.83** | **×2.17** |
| FD flow max (veh/h/lane) | — | 2,002.9 | **2,764.9** | ×1.38 |

### 4.2 InTAS 300 s — SUMO replay (`datasets/py_intas_300s`, unbiased = frozen trace)

Survivorship **0.9077**.

| metric | OLD | **NEW** |
|---|---|---|
| sample pairs | 709,120 | **781,241** |
| `speed_p50_mps` | 12.018 | 12.055 |
| `speed_p95_mps` | 28.74 fail | **27.439** fail |
| `accel_within_hard_bound_frac` | 0.99988 | 0.999849 |
| `lateral_discontinuity_events` | 0.0047 | 0.0043 |
| `overlap_events` | 0 pass | 0 pass |
| `headway_p50_s` | 2.597 | **2.3416** |
| **`headway_ks_shifted_exponential`** | **0.1936** fail | **0.2036** fail |
| **`fd_capacity_veh_h_lane`** | **811.7** fail | **874.8** fail |

### 4.3 InTAS 300 s — internal IDM (re-run with the oracle record)

Survivorship **0.9575**.

| metric | OLD | **NEW** |
|---|---|---|
| sample pairs | 435,873 | **455,250** |
| `speed_p50_mps` | 9.386 | 9.386 |
| `speed_p95_mps` | 13.90 | 13.90 |
| `overlap_events` | 20 HARD fail | **23** HARD fail |
| `headway_p50_s` | 4.9538 | **4.6715** |
| **`headway_ks_shifted_exponential`** | **0.1498 PASS** | **0.1562 FAIL** |
| **`fd_capacity_veh_h_lane`** | **182.5** fail | **190.2** fail |

### 4.4 What moves, and what does not

At **0.91–0.96 survival** (the 300 s windows) the per-sample distributional metrics barely move —
`speed_p50` 12.018 → 12.055, 9.386 → 9.386 — while the counting metrics move by roughly the missing
fraction. That is the expected shape, and it is why those metrics are flagged rather than withheld.

**At 0.4378 survival the selection effect is no longer second order and the panel says so on the
row**: `speed_p50` moves 4.772 → 1.432. The gate is on the density-dependent metrics only, so this
number is still published — with its survivorship beside it — which is the intended behaviour: the
number is reported, the reader can see what it is worth, and the *verdict* the old panel reached
(pass) is visibly not the one the traffic supports.

---

## 5. The headway question, settled

> *Did `headway_ks` genuinely get worse under SUMO mobility, or was that survivorship?*

**Genuinely worse. Survivorship is not the explanation, and the correction runs the other way.**

| window | arm | OLD | **NEW (unbiased)** | survival |
|---|---|---|---|---|
| 300 s (midnight) | internal IDM | 0.1498 **pass** | **0.1562 FAIL** | 0.9575 |
| 300 s (midnight) | SUMO replay | 0.1936 fail | **0.2036 fail** | 0.9077 |
| AM peak hour | SUMO replay | 0.1139 pass | **0.0830 pass** | 0.4378 |
| AM peak hour | internal IDM | 0.1741 fail | *(section 8)* | — |

Three things follow, and they are separable:

1. **The direction of the 300 s A/B is unchanged.** Unbiased, SUMO replay (0.2036) is still worse
   than internal IDM (0.1562) at midnight. Truncation moved *both* arms in the *same* direction and
   by a *similar* amount (+0.0100 and +0.0064), so it never had the power to create that gap. The
   `PYTHON-ENGINE-VALIDATION.md` section 5 verdict "REFUTED — worse" at midnight **survives**.
2. **But the internal arm's PASS does not survive.** 0.1498 was a truncated number; the traffic it
   was hiding pushes it to 0.1562, over the 0.15 gate. The superseded table's "internal IDM 0.1498
   (pass)" cell should be read as **0.1562 (fail)**. At midnight, unbiased, **both** arms fail.
3. **The peak-hour reversal is real and strengthens.** SUMO replay at peak goes 0.1139 → **0.0830**,
   comfortably inside the gate. So the withdrawal of open item 3 ("the metric rewards the absence of
   structure") stands, and stands more strongly than the truncated measurement suggested.

The unresolved objection is unchanged by any of this: the two peak arms are not density-matched
(`arrival_rate` is inert under `sumo_replay`), so "reversed" is still the right word rather than
"settled".

---

## 6. The false-positive problem is its own finding, and DENSITY is what drives it

Precision 0.308 at the peak hour means the MA revokes **2.25 benign vehicles per attacker caught**
(6,330 false against 2,813 true, over 2,889 attackers, recall 0.9737). Measured across every dataset
on this machine:

| dataset | vehicles | attackers | revoked | TP | FP | **precision** | recall | mean concurrent |
|---|---|---|---|---|---|---|---|---|
| InTAS peak hour, SUMO replay | 14,896 | 2,889 | 9,143 | 2,813 | 6,330 | **0.3077** | 0.9737 | ~3,775 |
| InTAS peak hour, internal IDM | 18,079 | 3,642 | 12,393 | 3,514 | 8,879 | **0.2835** | 0.9649 | (climbing to 10,764) |
| InTAS 300 s, SUMO replay | 334 | 66 | 27 | 7 | 20 | 0.2593 | 0.1061 | ~305 |
| InTAS 300 s, internal IDM | 334 | 85 | 13 | 4 | 9 | 0.3077 | 0.0471 | ~152 |
| **reference golden, 300 s grid** | 593 | 100 | 152 | 91 | 61 | **0.5987** | 0.9100 | 143 |
| default golden, 60 s grid | 73 | 15 | 12 | 10 | 2 | 0.8333 | 0.6667 | 33 |

### The controlled sweep

Same config throughout (grid 6×6, signalised, seed 42, `attacker_pct` 0.15 — the repository's own
reference arm), one knob moved per row.

**Run length, at `arrival_rate` 2.0:**

| duration | vehicles | revoked | FP | precision | recall | FP per TP | mean concurrent | survival |
|---|---|---|---|---|---|---|---|---|
| 60 s | 124 | 24 | 4 | **0.8333** | 0.9091 | 0.20 | 63.7 | 0.8545 |
| 120 s | 250 | 47 | 14 | 0.7021 | 0.8250 | 0.42 | 101.8 | 0.8368 |
| 300 s | 593 | 152 | 61 | 0.5987 | 0.9100 | 0.67 | 143.3 | 0.8126 |
| 600 s | 1,204 | 314 | 133 | 0.5764 | 0.9188 | 0.73 | 151.7 | 0.8169 |
| 1200 s | 2,400 | 636 | 268 | 0.5786 | 0.9534 | 0.73 | 157.2 | 0.8165 |
| 2400 s | 4,794 | 1,275 | 581 | **0.5443** | 0.9679 | 0.84 | 160.6 | 0.8264 |

**Density, at 300 s:**

| arrival rate | mean concurrent | vehicles | revoked | FP | precision | recall | FP per TP | survival |
|---|---|---|---|---|---|---|---|---|
| 0.5 /s | 30.4 | 152 | 24 | 2 | **0.9167** | 0.8462 | 0.09 | 0.8646 |
| 1.0 /s | 68.6 | 321 | 59 | 15 | 0.7458 | 0.8980 | 0.34 | 0.8705 |
| 2.0 /s | 143.3 | 593 | 152 | 61 | 0.5987 | 0.9100 | 0.67 | 0.8126 |
| 4.0 /s | 366.2 | 1,204 | 477 | 299 | 0.3732 | 0.9036 | 1.68 | 0.7433 |
| 8.0 /s | 937.3 | 2,400 | 1,140 | 791 | **0.3061** | 0.9041 | **2.27** | 0.6657 |

**Density is the driver; run length is mostly the fill transient.** Along the duration axis precision
falls from 0.833 to 0.599 while the network fills (mean concurrency 64 → 143), then **plateaus at
0.54–0.58 from 300 s to 2400 s** while concurrency saturates at ~160 — an 8× longer run costs only
0.055 more precision. Along the density axis, at *fixed* duration, precision falls **monotonically
0.9167 → 0.3061 over a 31× concurrency range**, roughly −0.19 per e-fold of density up to ~400
concurrent, then flattening. **Recall is flat at 0.90 ± 0.05 across both axes**: density costs the
MA precision and costs it nothing in recall.

The extrapolation checks out against the real city. The synthetic grid at 937 concurrent gives
precision **0.3061** and **2.27** false revocations per true one; the InTAS AM peak at ~3,775
concurrent gives **0.3077** and **2.25**. Two different networks, two different demand models, the
same saturation value — so this is a property of the *detector*, not of InTAS.

### What drives it — measured, not asserted

No detector, threshold or window was touched in this task. But the *shape* of the failure is
measurable from the sweep, and it distinguishes two candidate explanations: the detector's
per-report accuracy degrading with density, versus an absolute threshold becoming easier to cross in
a bigger neighbourhood. The density family at fixed 300 s separates them:

| arrival rate | mean concurrent | reports | **per-REPORT false-positive rate** | **REVOCATION precision** | benign subjects with ≥1 false report |
|---|---|---|---|---|---|
| 0.5 /s | 30.4 | 3,836 | **0.2672** | **0.9167** | 0.5439 |
| 1.0 /s | 68.6 | 9,425 | 0.2058 | 0.7458 | 0.5484 |
| 2.0 /s | 143.3 | 7,823 | 0.1969 | 0.5987 | 0.6966 |
| 4.0 /s | 366.2 | 21,338 | **0.2476** | **0.3732** | 0.7661 |
| 8.0 /s | 937.3 | 116,847 | 0.4641 | 0.3061 | 0.7853 |

**Over a 12× concurrency range (30 → 366) the per-report false-positive rate moves −7% (0.2672 →
0.2476, and it is not even monotone) while revocation precision collapses 59% (0.9167 → 0.3732).**
The detector is not getting worse at judging a report. What changes is that revocation needs
`report_threshold_k = 3` *distinct trusted reporters* inside a `revoke_window_s = 15` s window
spanning `revoke_persist_s = 3` s — an **absolute count inside a neighbourhood whose size is not
held constant**. Denser traffic means more co-located reporters, so the chance that three of them
independently flag the same benign subject inside one window rises far faster than the per-report
rate does; the share of reported benign subjects picking up at least one false report rises 0.544 →
0.766 across the same range, and the coincidence of three rises much faster than that. A genuine
attacker only ever needed the same three, which is why recall does not move.

Only at the extreme arm (937 concurrent) does the per-report rate itself rise, to 0.4641 — there
both terms contribute. The correction direction this suggests (scale the threshold with the observed
neighbour count) is a detection change with its own digest consequences and is deliberately left
alone here.

**And this is what deletes the traffic.** Survival falls with density on exactly the same axis
(0.8646 → 0.6657 over the sweep, 0.4378 at the peak hour), because a false revocation removes a
*benign* vehicle — one that would otherwise have kept driving and kept emitting for the whole of its
remaining trip. Of the peak hour's 7,640,042 missing vehicle-steps, the false-positive share is the
majority: 6,330 of 9,143 revocations were benign.

### The congestion filter

The bias is not uniform across traffic states, and this is the sharpest form of the finding.
Comparing the 13,589,568 un-enforced vehicle-steps against the 5,949,526 that survived:

| | unbiased | truncated | delta |
|---|---|---|---|
| speed p50 (m/s) | **1.432** | **4.772** | **+3.340** |
| speed p75 (m/s) | 11.428 | 12.357 | +0.929 |
| speed p95 (m/s) | 15.681 | 17.158 | +1.477 |
| speed **mean** (m/s) | 5.709 | 6.971 | +1.262 |
| fraction below 0.1 m/s | **0.4128** | **0.3431** | −0.0698 |
| fraction below 2 m/s | 0.5218 | 0.4320 | −0.0898 |
| **halting steps surviving** (v < 0.1 m/s) | 2,041,035 / 5,609,926 | | **0.3638** |
| **moving steps surviving** (v ≥ 0.1 m/s) | 3,908,491 / 7,979,642 | | **0.4898** |

A halting vehicle-step is **35% less likely to reach the dataset than a moving one**. The dataset is
therefore biased toward free flow in a way no scale factor can undo — which is the real reason
`fd_capacity`, the density p99 and the backward wave speed all moved so much: the congested branch
of the fundamental diagram is precisely the part enforcement deletes (240 → 2,043 congested cells).

*(As a by-product this settles a units question: the "median network speed 5.71 m/s" quoted for this
window in `PYTHON-ENGINE-VALIDATION.md` section 1 is the **mean** over vehicle-steps — measured here
at 5.7093 on the trace. The median is 1.432.)*

---

## 7. Which earlier conclusions change

**Superseded** (`PYTHON-ENGINE-VALIDATION.md` sections 4 and 5; that file is owned elsewhere and is
not edited here):

| claim | status |
|---|---|
| `fd_capacity` 182.5 (internal) / 811.7 (replay) at 300 s | **restated: 190.2 / 874.8.** Both still far below the 1800 floor |
| `fd_capacity` 459.3 (internal) / 763.6 (replay) at peak | **replay restated: 1130.0.** Internal arm: section 8 |
| "`fd_capacity` is inapplicable to InTAS" | **UNCHANGED and if anything firmer.** 1130.0 is still below the 1800 floor, and the real city's busiest instrumented lane carries 960 veh/h. The correction moved the number 48% and did not come close to reaching the gate |
| `headway_ks` internal IDM 0.1498 **pass** at 300 s | **RETRACTED → 0.1562 FAIL** |
| `headway_ks` SUMO replay 0.1936 fail at 300 s | **restated: 0.2036 fail** (same verdict) |
| "REFUTED — worse" (headway A/B at midnight) | **UNCHANGED**: 0.2036 vs 0.1562, same ordering |
| `headway_ks` peak 0.1139 **pass** (replay) | **restated: 0.0830 pass** — reversal confirmed, margin larger |
| open item 3 withdrawn | **stays withdrawn** |
| `overlap_events` 28 at peak (replay) | **restated: 169.** The "1,986× fewer than internal IDM" ratio needs the internal arm re-measured before it can be requoted |
| `overlap_events` 20 at 300 s (internal) | **restated: 23** |
| `overlap_events` 0 at 300 s (replay) | **UNCHANGED at 0** — that arm loses only 9% of its steps and none of them overlapped |
| `teleport_events` 1 at peak (replay) | **restated: 11.** Open item 4b reasoned from "one displacement above the speed bound" out of the 145 teleports that left no gap to split on; the un-enforced record shows **11**, so the argument for detecting a teleport by displacement as well as by absence is 11× stronger than it looked |
| speed p50 4.772 **pass** at peak (replay) | **RETRACTED → 1.432 FAIL** |
| "the mobility adapter is exact, match fraction 1.000" | **UNCHANGED.** This was never an emission-stream claim; the trace-vs-oracle cross-check in section 2.3 re-confirms it from a second direction |
| the demand deficit 0.4630 | **UNCHANGED.** It is measured on the trace and SUMO's own loops, neither of which passes through the SCMS layer |
| the dataset deficit 0.4677 on loop crossings | **UNCHANGED as a measurement of the dataset** — and this document is the explanation of it, plus the repair |

**Not superseded but newly qualified:** every traffic number in every earlier document is a
*broadcast-stream* number. The ones taken at 60–300 s on the grid sit at 0.81–0.89 survival, so they
are wrong by roughly that fraction wherever they count something, and approximately right wherever
they are a per-sample distribution. Nothing measured at peak density should be quoted without
re-measuring.

---

## 8. Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
cd C:\Users\Administrator\Documents\SCMS-Simulator
$env:PYTHONPATH = "$PWD\src"

# --- a run that carries its own un-enforced mobility (default is OFF, and OFF is byte-identical) --
python -m scms_sim_ref.mock_pipeline.run --flow --road grid --grid 6 --duration 300 `
  --arrival-rate 2 --attacker-pct 0.15 --traffic-lights --seed 42 --emit-mobility-oracle `
  --out datasets\ref_oracle
#   -> ground_truth/gt_mobility_oracle.jsonl, and manifest.counts.mobility_survivorship on ANY run

# --- score it from the unbiased record (the default when one is present) -------------------------
python -m scms_sim_ref.datagen.realism_bench datasets\ref_oracle --regime urban --markdown
# ... and from the truncated one, for the A/B
python -m scms_sim_ref.datagen.realism_bench datasets\ref_oracle --regime urban `
  --traffic-source emissions --markdown

# --- re-measure an ALREADY-GENERATED replay dataset from the trace it replayed (no re-run) --------
python -m scms_sim_ref.datagen.realism_bench datasets\py_intas_hour --regime urban `
  --traffic-source trace --sumo-trace C:/Temp/smob2/intas_hour.trace `
  --json .realism_cache\pyeng\scorecard_hour_unbiased.json
```

**Measured wall clock.** The trace-sourced peak-hour A/B (both scorecards, 13.6 M trace rows plus
5.9 M emission rows) took **~6 min** and peaked at **~17 GB** — against the ~2 h a
freeze-plus-replay arm costs, which is the point of the trace path. The 300 s InTAS runs re-run with
the record took 3–9 min each; the peak-hour internal arm ~50 min. The false-positive sweep (ten
grid runs, 60 s to 2400 s and 0.5/s to 8/s) took **~9.5 min** in total.

---

## 9. Open

1. **The internal-IDM peak-hour arm is re-running as this is written** (section 4 has no row for it).
   Its OLD numbers are `fd_capacity` 459.3, `headway_ks` 0.1741, `overlap_events` 55,608,
   `speed_p50` 5.60; its revoked fraction is 68.55% so its survival will be below the replay arm's,
   and the "1,986× fewer overlaps" ratio in `PYTHON-ENGINE-VALIDATION.md` section 5 cannot be
   requoted until both arms are unbiased.

2. **The false-positive rate is measured here, not fixed.** No detector, threshold or window was
   touched. The obvious direction — make `report_threshold_k` a function of the observed neighbour
   count rather than an absolute 3 — is a detection change with its own digest consequences and
   belongs in its own task. Until then, **a dataset generated at high density is mostly a record of
   benign vehicles being revoked**, and the `emit_mobility_oracle` record is what keeps its traffic
   measurable anyway.

3. **`--exclusive-gates` and the loop-crossing counts are not re-done here.** `tools/engine_detectors.py`
   counts crossings from the *dataset*, so the 11,100-of-23,733 (0.4677) figure inherits exactly this
   truncation. Re-counting from `gt_mobility_oracle.jsonl` should recover the trace's own 23,733 to
   within the counter's +0.41% bias, which would be a clean independent confirmation of the whole
   mechanism. It needs a `--dataset` variant that reads the oracle record and was out of scope here.

4. **The survivorship floor is 0.90 and is not calibrated.** It separates "a few vehicles went quiet"
   from "most of the traffic is missing"; it is not a tolerance anyone has shown a metric survives.
   A per-metric floor (the fundamental diagram is more fragile than the headway median) would be
   better and needs a sensitivity study.

5. **MOSAIC datasets have no equivalent.** The Java engine has no un-enforced emission path, so its
   traffic panel still reads whatever its CAM stream carries. It has no SCMS revocation of the same
   shape, so the specific bias may not apply — but that is an assumption, not a measurement.
