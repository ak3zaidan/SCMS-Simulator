# The awareness gate was measuring the wrong thing. Here is what the reference measured.

`docs/realism/PHASE2-GATE.md` ends by refusing to tune the channel: `comm.awareness_ratio_200m`
read **0.115** on real Ingolstadt geometry against a `>= 0.90` gate credited to Boban & d'Orey, and
the note says *"do not tune the model to reach 0.90 — that would be fitting to a mis-specified
target"*. That refusal was correct. This document establishes the target.

**Verdict: the geometric channel is broadly right at the power it is configured for, and the gate
was wrong.** Measured under conditions that match the reference's own, the model's
90%-awareness-equivalent range on Ingolstadt is **103.5 m** against **87.1 m** predicted by the
reference's own published urban curve *at our link budget* — 1.19x, consistent. The 0.115 is
explained, not excused, by the link-state composition: **only 6.0% of co-present pairs at 200 m
have line of sight**, and 90.9% are blocked by a building.

---

## 1. What Boban & d'Orey actually measured

Source read in full: M. Boban and P. M. d'Orey, *"Exploring the Practical Limits of Cooperative
Awareness in Vehicular Communications"*, **IEEE Transactions on Vehicular Technology 65(6):
3904–3916, 2016**; preprint **arXiv:1503.06590v3** (21 Mar 2016). Everything below is transcribed
and pinned, with the exact quotations, in
[`refdata/v2x_awareness_conditions.json`](../../src/scms_sim_ref/datagen/refdata/v2x_awareness_conditions.json).

### The metric (Section III-A.2)

> "Neighborhood Awareness Ratio (NAR): the proportion of vehicles in a specific range from which a
> message was received in a defined time interval. Formally, for vehicle *i*, range *r*, and time
> interval *t*, `NAR(i,r,t) = ND(i,r,t) / NT(i,r,t)`, where `ND` is the number of vehicles within
> *r* around *i* from which *i* received a message in *t* and `NT` is the total number of vehicles
> within *r* around *i* in *t* (**we use t = 1 second**)."

and

> "when representing PDR and NAR, we consider a set of uniformly spaced distance bins (i.e., an
> **annulus** — region between two concentric circles around the vehicle)."

NAR bins are 50 m; PDR bins 25 m; bins with fewer than 40 data points are discarded.

### Where "90% at 200 m urban" comes from — Table III, one cell

**Table III, "Distance above which Neighborhood Awareness Ratio (NAR) falls below 90%":**

| | Sweden | Netherlands | Finland | Italy |
|---|---|---|---|---|
| Highway V2V | 100 m | 250 m | **400 m** | 200 m |
| Suburban V2V | 100 m | 150 m | 350 m | — |
| **Urban V2V** | — | — | **200 m** | — |
| Highway V2I | — | — | — | 650 m |

Four things follow immediately, and all four matter:

1. **The project's entire empirical basis is one cell** — Tampere, Finland, urban, V2V.
2. **It is a crossing distance, not a level.** The comparable model quantity is *the distance at
   which awareness falls below 0.90*, not the awareness value read at a fixed 200 m.
3. **The same nominal environment spans 4x across sites** (highway 100–400 m). The paper says so:
   *"qualitative separation of environments into urban, suburban, and highway cannot be generalized
   across test sites."* Sweden's **suburban** figure (100 m) is worse than Finland's **urban** one.
4. Nothing in the paper supports the abstract's ">500 m highway". The largest measured highway
   figure is 400 m and the largest simulated one is 400 m at 20 dBm. `v2x_awareness.
   awareness_ratio_500m_highway_min` inherits that overstatement from the ROADMAP.

### The pair population (Table II, Section V)

| | Gothenburg SE | Helmond NL | Tampere FI | Trento IT |
|---|---|---|---|---|
| Vehicles | 6 | 9 | **3** | 3/4 |
| Max route length | 11 km | 5.5 km | 22 km | 60 km |
| RSUs | 0 | 0 | 0 | 5 |

> "the number of communicating nodes is below 10 on all test sites"

`NT` counts **only instrumented DRIVE-C2X vehicles** — unequipped traffic emits nothing and is
invisible to the log. The urban 200 m figure is therefore a **three-vehicle convoy on a shared test
route**: overwhelmingly same-street, i.e. LOS or vehicle-blocked. Our metric's denominator is every
co-present pair in a city, most of them on parallel streets behind a block of buildings.

### The radio (Section III-B/C)

ITS-G5 at 5.9 GHz; nominal Tx **21 dBm**; **effective** vehicle Tx **10–20 dBm** in Sweden and
Finland, 27 dBm vehicles / 32 dBm RSUs in Italy. Rooftop omni antennas at **1.44–1.66 m**. CAM
**10 Hz**, 100 B. Receiver sensitivity is not published for the measured arm; the only sensitivity
figure in the paper is the **−95 dBm** used in the simulations, stated as *"in line with the
sensitivity of devices used for DRIVE C2X measurements"*.

### The shot multiplicity — eq. (4), and the single largest error in the old gate

NAR counts a neighbour as "aware" if **at least one** of the CAMs sent in the 1 s window arrives.
Section IV models it as

> `NAR(r,t) = 1 − (1 − PDR_r)^Z,  Z <= N`

with `Z` fitted per dataset (Figs. 11/12/14): **8.2886** Finland highway, 4.1157 Finland suburban,
**5.4579 Finland urban**, 2.1365 Italy highway, 4.4798 Sweden, 3.5713 Netherlands, 6.4821 Finland
overall, 4.2768 Finland best fit. The paper's own recommendation when Z is unknown is to bracket
with Z = 2 and Z = 8.

Inverting it, **NAR = 0.90 corresponds to a per-packet PDR of `1 − 0.1^(1/Z)`**:

| Z | 2.1365 | 4.2768 | **5.4579 (urban)** | 8.2886 |
|---|---|---|---|---|
| per-packet PDR at NAR 0.90 | 0.660 | 0.416 | **0.344** | 0.243 |

**The reference's "90% awareness" asks for roughly one packet in three to arrive, not nine in ten.**
And `mock_pipeline` emits **one CAM per second** (`dt = 1.0`), so N = 1 and Z = 1 by construction:
`comm.awareness_ratio_200m` *is* a per-packet PDR. Grading it at 0.90 demanded ~2.6x the delivery
the reference did.

### The simulated arm (Section V) — the part that IS reproducible

GEMV² over the **core of Porto, 2410 vehicles**, real OSM building outlines, links classified
LOS / NLOSv / NLOSb, vehicle positions from aerial photography; **−95 dBm** receiver; explicitly
**no interference modelled** (*"the results in this section are an upper bound"*). That is
structurally the same experiment as a geometric-channel run on an OSM import: an all-pairs urban
average including through-building links. **It, not Table III, is the right anchor.**

Its published urban result, with the budget column derived using the stated −95 dBm receiver:

| effective Tx | link budget | distance at which urban NAR falls below 0.90 |
|---|---|---|
| 5 dBm | 100 dB | 50 m |
| 15 dBm | **110 dB** | **200 m** |
| 23 dBm | 118 dB | 300 m |

The paper explicitly licenses trading sensitivity against power dB-for-dB:

> "given different receiver sensitivity thresholds of radios, the results we show in this section
> would be equivalent to changing the transmit power level by the same amount"

which is exactly what makes the curve transferable to a different receiver — and what the retired
gate never did.

*Recorded, not suppressed:* the same section says urban reaches only **250 m** at the 33 dBm
regulatory ceiling, which is shorter than the 300 m it reports at 23 dBm. The three Fig. 18 points
are pinned; the 250 m and 400 m statements are recorded and excluded from the interpolation.

---

## 2. The four ways the comparison was not like-for-like

| # | The reference | `comm.awareness_ratio_200m` | Effect |
|---|---|---|---|
| 1 | 3–9 instrumented vehicles on a shared route (measured arm) | every co-present pair in a city | not quantifiable from the paper — the measured arm is **not reproducible**; use the simulated arm instead |
| 2 | ≥1-of-Z over a 1 s window at 10 Hz, Z ≈ 2–8 | one shot (`dt = 1.0` ⇒ Z = 1) | the 0.90 threshold should be **0.344**, not 0.90 |
| 3 | −95 dBm receiver; 110 dB budget at the 200 m point | −81 dBm receiver; **104 dB** budget | **6 dB** charged to the model that it was never configured to have |
| 4 | *distance at which* NAR crosses 0.90 | value read at a fixed 200 m | different quantity entirely |

The annulus convention is the one thing the harness already had right: `_curve_at` averages the
bins overlapping `[d − bin/2, d + bin/2]`, which is the paper's 50 m NAR bin. That code is unchanged.

---

## 3. What we now measure instead

`src/scms_sim_ref/datagen/awareness.py`, wired into `realism_bench.comm_panel`:

* **`comm.link_state_los_fraction`** and **`comm.link_state_los_fraction_{100,200,300}m`** — the
  LOS / NLOSv / NLOSb composition of the co-present pair population, per 50 m band, on the
  scenario's own geometry. Classified with **the channel's own `_BuildingRaster` ray march and
  `_VehicleBlockerIndex` body test, imported rather than re-implemented** — a second implementation
  of the geometry would be a second opinion, not a measurement.
* **`comm.pdr_absolute_200m`** — an **absolute** per-packet delivery probability, by exact
  quadrature over the TR 37.885 shadowing marginal, the censored-Gaussian NLOSv blockage and the
  Nakagami-m fade against the engine's hard decode floor. Propagation only: congestion, collision
  and weather are excluded *because the reference simulation models no interference*. The existing
  `comm.awareness_ratio_*` is normalised by an unknown constant and so cannot be compared to any
  threshold; this one can.
* **`comm.nar90_equivalent_range_m`** — Table III's own quantity: the distance at which awareness
  falls below the reference's 0.90, read at the reference's own per-packet equivalent (0.344 at the
  urban Z), graded against the reference's urban curve **interpolated at our link budget** with a
  band taken from the source's own fitted Z range rather than from a tolerance anybody chose (§5).
* **`comm.pdr_gray_zone_ratio`** — the dimensionless d20/d90, gated at `>= 1.9191` (the smallest of
  the three pinned shadowing-only ratios, which `v2x_awareness.gray_zone_ratio_from_shadowing`
  states are lower bounds). Unlike the absolute width in metres, it **cannot be passed by getting
  quieter** — the canyon-0 run scored a 509 m width while failing awareness.
* **`comm.awareness_ratio_{100,200,300}m`** is unchanged and still published, but **ungated**, with
  its retired reference and the reason recorded in `details`.

### Validation of the new machinery, before any conclusion is drawn from it

| check | result |
|---|---|
| `propagation_pdr` vs a Monte-Carlo of the **engine's own draw sequence** (18 state×distance points, n = 4×10⁵) | worst absolute difference **0.0019**, ≈ 2 MC standard errors |
| A&S 26.2.17 normal CDF vs `math.erf` over [−9, 9] | max abs error **7.45×10⁻⁸** |
| Link-state mix vs the mix **the engine itself recorded** (`datasets/phase2_geometric/_runs.json`) | highway_geometric LOS 0.1542 vs 0.1449; urban_grid_geometric LOS 0.5719/NLOSv 0.1718/NLOSb 0.2563 vs 0.5475/0.1879/0.2646; urban_osm_geometric (≤700 m cap) 0.0461/0.0203/0.9336 vs 0.0559/0.0494/0.8947 — **worst deviation 3.9 pp** |
| Analytic normalised curve vs the harness's report-reconstructed one | see §4 |

The residual on `urban_osm` is expected and named: the engine classifies only links that were
*candidates* in a given step, weighted by transmissions; this module samples co-presence pairs
uniformly over 1 s snapshots.

---

## 4. Re-measured: every Phase-2 run under the corrected definition

Link budget = `radio_tx_power_dbm − decode_floor_dbm`. "norm" columns are the analytic absolute PDR
divided by its own near-band value — i.e. the quantity `comm.awareness_ratio_*` estimates, derived
independently from geometry and physics instead of from report reconstruction.

| run | budget | LOS@200 m | PDR@200 m abs | norm@100 | old aw100 | norm@200 | old aw200 | analytic d50 | old eff. range |
|---|---|---|---|---|---|---|---|---|---|
| `urban_osm_geometric` | 104 dB | **0.060** | 0.0754 | 0.342 | 0.408 | 0.076 | 0.110 | 79.7 m | 87.8 m |
| `urban_osm_geometric_p33` | 114 dB | 0.063 | 0.1701 | 0.677 | 0.661 | 0.170 | 0.198 | 128.4 m | 124.8 m |
| `urban_grid_geometric` | 104 dB | 0.644 | 0.7033 | 0.859 | 0.744 | 0.704 | 0.543 | 373.3 m | 217.5 m |
| `highway_geometric` | 104 dB | 0.228 | 0.5527 | 0.892 | 0.996 | 0.553 | 0.551 | 217.4 m | 228.1 m |
| `highway_geometric_p13` | 94 dB | 0.231 | 0.1402 | 0.571 | 0.556 | 0.142 | 0.100 | 109.9 m | 104.8 m |
| `highway_geometric_p33` | 114 dB | 0.234 | 0.8974 | 0.991 | 1.123 | 0.897 | 0.949 | 562.8 m | 497.1 m |

*"old aw*" and "old eff. range" are read from the scorecard JSONs currently on disk under
`datasets/phase2_geometric/`. They differ in the third decimal from the figures quoted in
PHASE2-GATE.md (0.413 / 0.115 / 0.048 and 90.4 m against 0.408 / 0.110 / 0.054 and 87.8 m) — the
same drift on both, from a harness revision between the two measurements. Nothing here turns on it.*

**The independent analytic curve reproduces the harness's report-reconstructed effective range to
within 3–13% on every run that uses real building geometry or no blockage model** (79.7 vs 87.8,
−9.2%; 128.4 vs 124.8, +2.9%; 217.4 vs 228.1, −4.7%; 109.9 vs 104.8, +4.9%; 562.8 vs 497.1,
+13.2%). It diverges by 1.7x only on `urban_grid_geometric`
— the one configuration that uses the **synthetic urban-canyon fallback**, whose per-band distance
dependence the analytic expectation does not reproduce even though the overall mix agrees to 2.5 pp.
That is a third, independent argument that the fallback is the wrong abstraction, alongside the two
already in PHASE2-GATE.md.

The two curves do **not** agree everywhere, and the disagreement is systematic: on Ingolstadt the
analytic value is 0.84x the reconstructed one at 100 m, 0.69x at 200 m and 0.55x at 300 m. The
reconstruction runs on ever-thinner Poisson counts as distance grows (its far bins are a handful of
honest report links over a co-presence denominator capped at 400 vehicles per bucket), so it reads
high out there. This is a reason to quote the **absolute** curve rather than the normalised one at
long range, not a discrepancy that changes any conclusion below: every crossing that matters here
sits inside 200 m.

### The Ingolstadt composition, band by band — this is what explains 0.115

`urban_osm_geometric`, 1519 OSM building polygons, 239 snapshots, 400 000 pairs classified:

| band | pairs | LOS | NLOSv | NLOSb | absolute PDR |
|---|---|---|---|---|---|
| 0–50 m | 5 352 | 0.620 | 0.159 | 0.222 | 0.995 |
| 50–100 m | 9 375 | 0.277 | 0.171 | 0.552 | 0.531 |
| 100–150 m | 13 075 | 0.131 | 0.080 | 0.790 | 0.203 |
| 150–200 m | 15 696 | 0.075 | 0.039 | 0.886 | 0.097 |
| 200–250 m | 17 454 | 0.046 | 0.024 | 0.930 | 0.056 |
| 250–300 m | 18 896 | 0.034 | 0.011 | 0.955 | 0.035 |
| 300–350 m | 20 864 | 0.025 | 0.007 | 0.968 | 0.025 |
| 400–450 m | 27 756 | 0.015 | 0.002 | 0.983 | 0.013 |

Overall, over all bands to 1 km: **LOS 3.4%, NLOSv 1.3%, NLOSb 95.3%.** In a dense European old
town, a pair drawn at random at 200 m is nine times out of ten separated by a building. An
all-pairs awareness ratio in that scene is *measuring the street layout*, and it always was.

### The comparison that is now like-for-like

| | `urban_osm_geometric` (23 dBm) | `urban_osm_geometric_p33` (33 dBm) |
|---|---|---|
| link budget | 104 dB | 114 dB |
| model's 0.90-awareness-equivalent range | **103.5 m** | 155.8 m |
| Z-sensitivity bracket (Z = 2.14 … 8.29) | 61.1 – 119.0 m | 106.3 – 173.7 m |
| reference's urban curve **at that budget** | **87.1 m** | 244.9 m |
| gate band (= "reference inside the Z bracket") | 75.8 – 147.5 m | 219.7 – 358.9 m |
| ratio | **1.19x — CONSISTENT** | 0.64x — short by 5.9 dB equivalent |

And an independent corroboration that nobody tuned for: the paper's one environment-independent
claim is

> "applications requiring high awareness levels (e.g., 90%) up to 100 m can be satisfied in
> virtually all environments"

Converting our Ingolstadt per-packet PDR at 100 m to NAR at the reference's own 10 Hz rate and
urban Z gives **0.896**. The universal floor is 0.90. On the reference's own terms, at the
reference's own beacon rate, the model lands on it.

### Where the model *is* short, stated plainly

At the 33 dBm ETSI ceiling the model reaches 155.8 m against the reference's 244.9 m — **0.64x, a
5.9 dB equivalent through the TR 37.885 urban-NLOS slope of 30 dB/decade**. Two live explanations,
and the paper does not publish what would separate them:

1. **Scene density.** Ingolstadt's historic core is not Porto's. Our LOS share at 200 m is 6.0%;
   the paper never publishes Porto's link-state composition, so the difference cannot be attributed.
2. **The NLOS term.** TR 37.885's urban-NLOS is a *statistical* fit (`36.85 + 30·log10 d`) applied
   uniformly to every blocked link. GEMV²'s NLOSb is a *deterministic* computation of reflections
   off building walls and corner diffractions — it finds real paths around a corner that a
   log-distance term cannot. A statistical NLOS term will be the more pessimistic of the two at
   intermediate range, and that is exactly where the 0.64x appears.

Note also that our range grows with power more slowly than the reference's (1.51x for 10 dB against
their 2.81x), which is the signature of a **geometry ceiling**: at 33 dBm, LOS and NLOSv links are
already delivered with near-certainty, so awareness at a distance is bounded by the LOS+NLOSv
*share* at that distance — 3.1% at 200 m here. More power cannot buy what the street layout does not
offer. The paper reaches the same conclusion in its own scene: *"To achieve the same performance in
urban would require over 33 dB EIRP, which is not allowed."*

### The disc model, for contrast

`urban_osm_disc` scores `comm.awareness_ratio_200m` = **1.342** — reception at 200 m as likely as at
25 m — on a scene where **93.0% of the pairs in that band are building-blocked**. `urban_grid_disc`
scored **0.9016**, the value that passed the retired gate. Reporting LOS composition next to
awareness makes both unmistakable; reporting awareness alone made the second one look like a pass.

---

## 5. Recommended gate, replacing `awareness_ratio_200m_urban_min >= 0.90`

1. **`comm.nar90_equivalent_range_m`** — the reference's predicted distance, evaluated at the run's
   **own** link budget, must fall inside the model's **Z-sensitivity bracket**. Nobody picks the
   tolerance: the source fits Z anywhere in [2.1365, 8.2886], so NAR 0.90 is a per-packet PDR
   anywhere in [0.243, 0.660], and reading our own curve at both ends gives the admissible range of
   crossing distances. Written as a band on the measured value (the shape the scorecard grades) that
   is `ref × d/d_hi … ref × d/d_lo`.

   | run | value | band | |
   |---|---|---|---|
   | `urban_osm_geometric` (23 dBm, real buildings) | 103.5 m | 75.8 – 147.5 m | **pass** |
   | `urban_osm_geometric_p33` (33 dBm) | 155.8 m | 219.7 – 358.9 m | fail (short) |
   | `urban_grid_geometric` (synthetic canyon fallback) | 539.5 m | 70.5 – 198.7 m | fail (**6.2x too permissive**) |

   That last row is worth stating on its own: against a reference urban curve, the synthetic
   urban-canyon fallback is not pessimistic, it is **wildly optimistic** — a fourth independent
   argument that it is the wrong abstraction, and the opposite sign from what the retired gate
   implied. Only defined for `radio_env = urban` and 100 ≤ budget ≤ 118 dB; outside that the
   reference is not extrapolated and the metric degrades to `na` with the reason printed. A run
   whose own `radio_model` is not `geometric` is reported but **not graded** — the modelled curve is
   a counterfactual there, and grading it would be the same category error one level down.
2. **`comm.pdr_gray_zone_ratio` >= 1.9191**, replacing the absolute `>= 100 m` width. Dimensionless,
   so it cannot be passed by lowering transmit power. Ingolstadt scores **3.59**.
3. **`comm.link_state_los_fraction_*` reported, never gated.** It is a property of the map, not of
   the model; gating it would gate the choice of city. But an awareness number published without it
   is uninterpretable.
4. **Do not restore a fixed-distance awareness threshold.** The reference reports crossing
   distances precisely because NAR saturates at 1.0 to double precision once the per-packet PDR is
   high — pinned as a test.

### What still cannot be validated, and what would fix it

* The **measured** arm of Boban & d'Orey (Table III) is unreproducible: a 3-vehicle fleet, an
  unpublished route topology, unpublished per-link LOS state, and an effective transmit power given
  only as a range ("between 10 and 20 dBm"). No amount of work on our side closes that.
* Porto's link-state composition is not published, so the 0.64x at 33 dBm cannot be split between
  scene density and the NLOS term. **The one experiment that would settle it**: run the same
  Ingolstadt geometry through a reflection/diffraction NLOSb model (GEMV²'s, or a two-bounce
  approximation) and see whether the 33 dBm crossing moves from 156 m toward 245 m. If it does, the
  TR 37.885 statistical NLOS term is the cause and the scene is exonerated.
* An absolute PDR-vs-RSSI calibration is still missing (`v2x_awareness.pdr_vs_rssi_curve` is a
  deliberate empty placeholder); the FLOURISH Bristol ITS-G5 dataset under DOI
  `10.5523/bris.eupowp7h3jl525yxhm3521f57` is the open, licence-clean way to close it.

---

## Reproduce

```powershell
. C:\Users\Administrator\tools\env.ps1
$env:PYTHONPATH = "$PWD\src"
# the like-for-like report on its own
python -m scms_sim_ref.datagen.awareness datasets\phase2_geometric\urban_osm_geometric --markdown
# the same rows inside the full scorecard
python -m scms_sim_ref.datagen.realism_bench datasets\phase2_geometric\urban_osm_geometric --markdown
# the definition's own tests (synthetic geometry, answers known by construction)
python -m pytest tests\test_awareness.py -q
```
