# Phase 2 gate — the geometric channel, measured

The Phase 2 benchmark never ran: its agent was lost to a session limit after the code landed. This
is that measurement, run directly. **Verdict: the channel model fixes the defect it was built to fix
and introduces a new one. It is not yet calibrated and must not be made the default.**

## Method

Identical config in every respect except `radio_model`, so the comparison is like-for-like:
`--flow --road grid --grid 6 --duration 300 --arrival-rate 2 --attacker-pct 0.15 --traffic-lights
--seed 42`, re-run from the manifest with `emit_sample_prob = 1.0` for a full emission trace.
Scored with `python -m scms_sim_ref.datagen.realism_bench`.

Link budget in all geometric runs: `radio_tx_power_dbm = 23.0`, `radio_rx_sensitivity_dbm = −81.0`,
`radio_env = urban`, `shadowing_sigma_db = 4.0`, `pathloss_exponent = 2.7`.

## Result

| Metric | Gate | disc (before) | geometric, canyon 4.0/km | geometric, canyon 0 |
|---|---|---|---|---|
| `comm.pdr_gray_zone_width_m` | ≥ 100 m | **80.6 fail** | **271.3 pass** | **509.4 pass** |
| `comm.awareness_ratio_200m` | ≥ 0.90 | **0.902 pass** | **0.279 fail** | **0.771 fail** |
| `comm.awareness_ratio_100m` | (no gate) | 0.976 | 0.539 | 0.913 |
| `comm.awareness_ratio_300m` | (no gate) | 0.996 | 0.214 | 0.619 |
| `comm.effective_range_m` | (informational) | 494.8 | 121.1 | 398.1 |
| `comm.honest_links` | (sample size) | 1540 | 3023 | 8456 |

Detection quality moved only slightly: precision 0.599 → 0.576, recall 0.91 unchanged, 593 vehicles
in every run (mobility is untouched).

## What this means

**The step-function defect is genuinely fixed.** The disc model's PDR falls from 90% to 20% over
80.6 m — a cliff. The geometric model spreads that transition over 271 m, which is what a real
channel does, and it is the metric Phase 2 existed to move. The `effective_range_m` of 494.8 m under
`disc` is simply the configured `radio_range_m` of 500 m read back, which is the tell that the old
model was a geometric primitive rather than a channel.

**But the model is now too pessimistic.** Boban & d'Orey's measurement campaign — the pinned
reference — reports ~90% cooperative awareness out to 200 m in urban settings. We produce 27.9%.
That is not a stricter model, it is a wrong one.

**The cause is the urban-canyon fallback, and it is quantified above.** Synthetic grids carry no OSM
building footprints, so the model falls back to `radio_nlosb_density_per_km = 4.0`, applying
blockage as a uniform per-kilometre probability. Setting that to zero lifts awareness at 200 m from
0.279 to 0.771 and effective range from 121 m to 398 m. So the fallback dominates the result.

A uniform density is the wrong abstraction for a Manhattan grid: two vehicles on the same street
have clear line of sight regardless of how many buildings line it, and only cross-street links are
blocked. The fallback charges every link for blockage as a function of distance alone.

Note also that even with blockage disabled, awareness at 200 m is 0.771 — short of 0.90. So there is
a second, smaller contributor: the configured `shadowing_sigma_db = 4.0` is the 3GPP TR 37.885 value
for **NLOS**, while LOS links should use **3.0** dB; and 23 dBm is at the low end of deployed OBU
transmit power (ETSI permits up to 33 dBm).

## The gray-zone gate is weaker than it looks

A ≥100 m absolute width can be passed by lowering transmit power or raising path loss until the
curve is broad and low — the canyon-0 run scores 509 m while *failing* awareness. Width alone does
not distinguish a realistic channel from an over-attenuating one.

The physically meaningful quantity is the dimensionless **ratio d20/d90**, which from shadowing alone
is 2.41 (urban LOS), 2.08 (highway LOS), 1.92 (urban NLOS) — already computed and pinned in refdata.
The gate should be that ratio plus an awareness floor, so a model cannot pass by simply getting
quieter. **Recommend replacing the absolute-width gate.**

## Correction (2026-08-31): real buildings are *worse*, so the fallback was not the cause

The section above concluded that the uniform canyon-density fallback was "the dominant cause" of the
awareness collapse, and predicted that a real OSM map — where the fallback is never used — was the
configuration the model was designed for. **That prediction was wrong and the measurement refutes it.**

Ingolstadt imported from OSM with 1519 real building polygons (`--buildings --attrs --signals`;
341 nodes, 402 edges, 177 one-way, 52 signalled junctions; buildings sharing the road graph's exact
projection, `centroid_inside_road_bbox_frac = 1.0`):

| Metric | disc | geometric, synthetic grid + canyon 4/km | geometric, canyon 0 | **geometric, real OSM buildings** |
|---|---|---|---|---|
| awareness @100 m | 0.976 | 0.539 | 0.913 | **0.413** |
| awareness @200 m | 0.902 | 0.279 | 0.771 | **0.115** |
| awareness @300 m | 0.996 | 0.214 | 0.619 | **0.048** |
| effective range | 494.8 m | 121.1 m | 398.1 m | **90.4 m** |
| gray zone | 80.6 (fail) | 271.3 (pass) | 509.4 (pass) | **113.5 (pass)** |

So the over-attenuation is **intrinsic to the parameterisation, not an artefact of the fallback**.
In a dense European old town almost every link that is not along the same street is NLOS, and the
3GPP urban-NLOS term (`36.85 + 30·log10 d`) is steep.

### But the gate itself is now suspect

The measured effective range of 90 m is *consistent with the physics we configured*: at
`tx_power 23 dBm` and `sensitivity −81 dBm`, the refdata computation gives an urban-NLOS median
range of ~57 m, rising to **121.9 m at the 33 dBm ETSI ceiling** — and that 100–120 m band
independently reproduces the measured "~100 m urban NLOS" anchor. Our 90 m sits inside it.

Meanwhile the gate demands ≥ 0.90 awareness at 200 m, anchored on Boban & d'Orey. That campaign's
figure is very unlikely to be an **all-pairs** average over every vehicle pair in a dense city
including through-building links — which is exactly what our metric computes. Comparing an all-pairs
average against a measurement whose selection conditions we have not reproduced is the same class of
error as the GEH gate that graded a simulation against itself: the number is real, the comparison is
not like-for-like.

**Therefore: do not tune the model to reach 0.90.** That would be fitting to a mis-specified target.
The next step is to establish what Boban & d'Orey actually measured — LOS-only pairs, same-road
pairs, or all pairs — and either restate the metric to match those conditions or replace the anchor
with one whose conditions we can reproduce. Only then is the awareness number meaningful.

The one conclusion that survives unchanged: **the disc model's step-function edge is genuinely
fixed** — the gray zone passes in every geometric configuration, and `disc`'s 494.8 m "effective
range" was only ever `radio_range_m` read back.

## Actions

1. Replace the uniform canyon-density fallback with street-geometry-aware classification on synthetic
   networks: same-edge links are LOS, cross-street links are blocked. Density remains only as a
   coarse last resort for networks with neither buildings nor usable edge geometry.
2. Use the per-state shadowing σ (3.0 LOS / 4.0 NLOS) rather than one global value.
3. Calibrate transmit power against the awareness anchor rather than assuming it.
4. Re-gate on the d20/d90 ratio plus an awareness floor.
5. Only after awareness and gray zone pass together should `geometric` be considered for default.
6. Validate on a real OSM map with genuine building footprints (1482 are already cached), where the
   fallback is not used at all — that is the configuration the model was actually designed for, and
   it is untested so far.
