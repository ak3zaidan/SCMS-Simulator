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
