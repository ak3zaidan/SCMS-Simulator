# What the opt-in channel physics actually does — measured

**Date:** 2026-09-07 · **Branch:** `feat/realism` · **Model under test:** `GeometricChannel`
(`radio_model="geometric"`), `src/scms_sim_ref/mock_pipeline/run.py` · **Instrument:**
`tools/channel_physics.py` (new).

Six opt-in terms landed in the geometric channel — a per-link NLOSv blockage hold, a TR 37.885
vehicle antenna pattern, a measurement-based NLOSv level, a two-ray ground-reflection breakpoint,
per-vehicle-type blocker footprints, and a Clarke-correlated small-scale fade. Every one defaults
**off**, and both pinned digests are unmoved. Nothing had measured what any of them does when
**on**. This document does.

> **Read Revision 2 first.** Five of those six terms survive; the third — the "measurement-based
> NLOSv level" — is **retracted**, and three of this document's own conclusions are reversed. The
> revision table is immediately below.

It is written against the house rule that an honest failure is a real result, and it is not an
advocate for the change. Two of the six terms move essentially nothing and one of those two is the
most expensive in the set; one moves the metrics further than any other for a reason that turns out
to be a link-budget bookkeeping error; one moves them further still on the weakest evidence in the
set. Two are worth switching on by default, and neither of those two moves a graded number at all —
they earn their place on conformance and on burst structure, which the project had never measured.

---

## Revision 2 — 2026-09-07, later the same day

**Four adversarial verifiers attacked the work above. Their confirmed findings are acted on here,
and three of them contradict revision 1's own conclusions.** Read this section before any table
below it.

| # | What revision 1 said | What revision 2 does | Where |
|---|---|---|---|
| **A** | *(not considered)* | The colluder's fabricated `rssi_dbm` was computed by a **second, hand-rolled link budget**. It agreed with the real one to +0.036 dB *until* `radio_antenna_pattern` was switched on, at which point it disagreed by **+5.824 dB** — a single-threshold classifier separating fabricated from genuine at **AUC ≈ 0.79**. Closed structurally: one budget site, both callers. Re-measured at **≤ 0.142 dB on every arm and every geometry**. | §6 R9 |
| **B** | `radio_nlosv_model="measured_boban"` is "measurement-based… better-evidenced than the specification it replaces" | **RETRACTED**, knob and constants deleted. Its level anchor was GEMV² *simulator output* digitised from an IEEE-copyright figure; graded against the one independent 5.9 GHz measurement in its own citation set it was **+11 to +15 dB wrong where the specification was −0.96 / +4.04 dB**. | §6 R2 |
| **C** | `radio_nlosv_hold` is **CONFORMANT (TR 37.885 6.2.1)** | The standard states **no temporal scope for the draw at all**. The "one draw per blocked link" claim came from the task brief, not the document. The term **stays on its physics merits**; the label becomes *physics-motivated, not specified*. | §0, §9 |
| **D** | *(not considered; the refdata said the fix was blocked)* | The antenna height was TR 37.885's **pedestrian** row (1.5 m) while the fleet was declared Type 2 (1.6 m), and the Case 1 / Case 3 boundary was wrong at equality. **Both fixed.** The refdata's stated blocker — "changing `V2X_ANTENNA_HEIGHT_M` moves both pinned dataset digests" — was **imaginary**, and re-running both arms proves it: neither moves. | §6 R11 |
| **E** | Hold the antenna pattern **OFF** pending an "EIRP double count" | **INVERTED.** TR 37.885 Table 6.1.1-1's 23 dBm is **conducted**; the element gain is separate (Table 6.1.4-8). The pattern-**on** budget is the conformant one and the **default runs 6 dB below the standard's**. The bug was the *comment*. | §6 R1, §9 |
| **F** | "RSU links are out of scope" | The term gives a V2V link +6 dB and a V2I link +3 dB — a permanent 3 dB penalty on infrastructure, from a missing model. Now **refused loudly** rather than run silently. | §6 R10 |
| **G** | *(not considered)* | LOS/NLOSv is classified **geometrically**; TR 37.885 Table 6.2-1 specifies it **probabilistically**. Measured deviation: **3.53× overall and two-sided**, crossing over at ≈ 60 m. Documented as a named deviation. | §6 R12 |
| **H** | The antenna term "unlocks" Nilsson's 2–4 m decorrelation as "the applicable comparison" | **WITHDRAWN.** Nilsson's figure is measured on the *offset-subtracted* process, and the offset is where the *per-link* antenna gain goes; a flat +3 dBi element removes none of it. | §6 R1 |

> ### ⚠ EVERY MEASURED FIGURE BELOW PREDATES FINDING D, AND FINDING D MOVED THE MODEL
>
> The three scenes were measured with `V2X_ANTENNA_HEIGHT_M = 1.5 m` and the old branch rule. Both
> are now corrected, and the correction **changes the geometric channel's behaviour** — which is
> exactly why it required re-pinning `tests/test_geometric_channel.py`. The tables below have **not
> been re-measured**, and inventing refreshed numbers for them would be fabrication. What changed,
> and in which direction:
>
> * **A car-blocked link now loses 5.20 dB instead of 9.04 dB** (TR Case 3 instead of Case 2), so
>   every NLOSv-heavy row is now **optimistic**: real PDR is *higher* than the tables say, and the
>   gray zone *wider*. A truck-blocked link is unchanged at 9.04 dB.
> * **Blocker type now matters at all**, which it structurally could not before — so
>   `radio_blocker_width`'s "moves nothing measurable" (R4) is the row most likely to have changed,
>   since widening a truck corridor now changes *which* case a link lands in as well as whether it
>   is blocked.
> * **The two-ray breakpoint moved from 177.1 m to 201.5 m** (d_b is quadratic in antenna height),
>   so R3's "largest mover" is now a *smaller* mover: it switches on 24 m later.
> * **The antenna term's +6 dB is unaffected**, and so is everything in §6 R5 (the fade correlation
>   cannot move a marginal) and §6 R7 (the analytic instrument is blind either way).
>
> The **conclusions** that do not depend on those numbers — the retraction (B), the label
> corrections (C), the EIRP inversion (E), the RSU refusal (F), the classification deviation (G) and
> the Nilsson withdrawal (H) — stand as written. Re-running the campaign is the first item of
> outstanding work.

---

## 0. Headline

All figures are on the full-city scene unless stated. "Cost" is engine wall clock on the arm where
the channel dominates a step (§7.1).

| # | Term (knob) | Conformance | Does it move a graded metric? | Cost | Recommended default |
|---|---|---|---|---|---|
| 2 | `radio_nlosv_hold` | ~~CONFORMANT (6.2.1)~~ → **physics-motivated, NOT specified** (rev 2, finding C) | **PDR: no. Burst structure: yes, and it scales with CAM rate** — loss runs +6.3 % (full city, 1 Hz), **+7.4 % overall and +17 % in the 100–150 m band at 10 Hz** | **1.000×** | **ON**, on physics rather than on conformance |
| 1 | `radio_antenna_pattern` | **CONFORMANT** (Tables 6.1.4-8/-9) | **Yes, the largest PDR move of any term** — PDR@200 m +0.0923. ~~45 % of that is a double-counted 3 dB~~ → **none of it is: the default is 6 dB BELOW TR 37.885's own budget** (rev 2, finding E) | 1.125× | **ON where there are no RSUs** (rev 2); refused with `n_rsus > 0` |
| 4 | ~~`radio_nlosv_model="measured_boban"`~~ | **RETRACTED** (rev 2, finding B) | *(the knob no longer exists — §6 R2)* | — | — |
| 3 | `radio_breakpoint="two_ray"` | magnitude **not asserted** | **Yes — the biggest movement of all**: d20 489 m → 330 m, gray zone −159 m, loss runs **+56 %** *(measured with the breakpoint at 177.1 m; it is now 201.5 m)* | 0.990× | **OFF** — biggest metric movement on the weakest evidence |
| 5 | `radio_blocker_width` | **CONFORMANT** (clause 6.1.2) | **No.** It reclassifies **0.30 %** of link-steps; PDR@200 m −0.0020, loss runs −0.02 *(measured before blocker type could affect a link at all — see the staleness notice)* | 1.003× | ON *only* as conformance bookkeeping, documented as inert |
| 6 | `radio_fading_correlation="jakes"` | physics, not specified | **Almost no.** Nothing at 1 Hz. At 10 Hz it moves **only** the 0–50 m band, by +5.7 % loss-run length | **1.608×** | **OFF** — a 1.6× channel for a 5 % move in one band |
| — | all six together | — | PDR@200 m +0.0475, gray zone −72.6 m, loss runs +49 % | **1.727×** | — |

And two findings that are not about any single term:

> **The project's own graded instrument cannot see any of the six terms.**
> `datagen/awareness.py` computes delivery by exact quadrature —
> `propagation_pdr(state, d_m, tx_power_dbm, decode_floor_dbm, radio_env)` — a pure function of a
> state name and a distance. No channel object and no config reach it, so five of the six knobs
> cannot touch it. The sixth, `radio_blocker_width`, cannot touch it either:
> `link_state_composition` rebuilds `_VehicleBlockerIndex` from 4-tuples and so always takes the
> uniform corridor width. It returns LOS 0.9364 / NLOSv 0.5898 / NLOSb 0.0004 at 200 m **for every
> one of the ten arms** (`python tools/channel_physics.py blindness`). Demonstrated end to end as
> well as argued: the unmodified `python -m scms_sim_ref.datagen.awareness` over four separate
> engine runs — `off`, `blocker_width`, `antenna`, `all_on` — spans **0.0004 in PDR at 200 m and
> 0.2 m in gray-zone width**, with `d90 = 45.2 m` in all four, while the replay of those same four
> configurations spans **0.048 in PDR** (R7). A claim of the form "term X improved
> `comm.awareness_ratio_200m`" is, if measured with `awareness.py`, a statement about which vehicles
> were present. Any future work on these terms must be graded with an instrument that runs the
> channel.

> **The channel sits inside the misbehaviour-detection loop, and the coupling had never been
> measured.** Ten end-to-end runs of the reference arm produced ten different emission traces,
> because the channel decides which reports arrive → which decides what the MA revokes → and a
> revoked vehicle stops broadcasting. Report volume moves by up to **27 %** between arms. On that
> scene's 100 attackers, **recall is pinned at 0.90–0.92 in every arm while false revocations span
> 49 to 73**: what the radio changes is not whether attackers are caught but how many innocent
> vehicles are wrongly revoked (R8).

---

## 1. Method

### 1.1 What the instrument does

`tools/channel_physics.py measure <dataset>` replays a finished dataset's **own emission trace** —
true positions, every vehicle, every step — through the **real `GeometricChannel` object**, once per
arm, and records the per-link, per-step reception decision the channel takes.

The scene is held byte-identical across arms: the same positions, the same vehicle types, the same
buildings, and the same integer vids, so the replay draws on **the same per-link RNG stream keys the
engine itself uses** (`run.py:6394` writes `true_id = f"veh_{vid:03d}"`, so `veh_017` recovers vid
17, and the channel's streams are `f"{seed}:geo:{tx}:{rx}"`). Every difference between two arms is
therefore the physics term and nothing else.

That replay convention is `tools/xengine_radio.py`'s — hold the scene fixed so the residual is
physics. The metric arithmetic is `datagen/awareness.py`'s and is **imported, never re-derived**:
`curve_value_at` (the reference's annulus), `crossing_m` (the least-non-increasing-majorant
crossing), `gray_zone_ratio`, `nar_from_pdr` / `pdr_for_nar` / `z_for_engine` (the paper's eq. 4
shot multiplicity), `reference_nar90_distance_m` (Boban & d'Orey's own curve evaluated at *our* link
budget), `load_conditions`. A second copy of any of those would be a second opinion, not a
comparison.

### 1.2 What is measured

* **PDR vs distance** on the reference's own 25 m PDR bin, and PDR at 200 m over its annulus.
* **Effective range** — d90 / d50 / d20 on the absolute curve, plus the harness's own
  normalised-at-the-near-band forms (`comm.effective_range_m` reads a normalised curve), plus the
  **NAR-0.90-equivalent range** under `awareness_report`'s convention: the distance at which our
  per-packet curve crosses `pdr_for_nar(0.90, Z_urban = 5.4579) = 0.3442`.
* **The gray zone** — d20 − d90 in metres (`comm.pdr_gray_zone_width_m`'s definition) and the
  dimensionless d20/d90.
* **Consecutive-loss run length** — per ordered link, over contiguous co-presence, the run-length
  distribution of losses, plus `P(loss | previous step lost)` and the burstiness ratio
  `P(L|L) / P(loss)`. **This project had never measured it.**
* **Packet inter-reception time** — the gap between successive successful receptions on one link, in
  seconds, with its tail.
* **NAR the reference's own way** — per link, per 1 s window, was at least one message received —
  and hence an **empirical shot multiplicity Z** from `NAR = 1 − (1 − PDR)^Z`, which is the paper's
  own burstiness statistic ("Z ≤ N … discounted for the measured burstiness of CAM loss").
* Link-state composition, the channel's own `stats` counters, and the per-link-evaluation cost.

### 1.3 What is deliberately excluded

The replay measures the **PHY term only** — `rx_dbm ≥ decode_floor` after path loss, shadowing,
blockage, antenna gain and fading. The engine then composes it with
`(1 − packet_loss_base)(1 − nlos_loss)(1 − collision_loss)(1 − weather)` (`run.py:8653–8657`). This
is the same *propagation-only* quantity `awareness.py`, `CROSS-ENGINE-RADIO.md` and
`FULL-CITY-SCENE.md` all compare, because the reference simulation models no interference and is
explicitly an upper bound. Including congestion here would re-introduce the exact mismatch those
documents exist to remove.

### 1.4 The three scenes

| | A — reference arm | B — full city | C — the 10 Hz arm |
|---|---|---|---|
| network | `--road grid --grid 6`, `--traffic-lights` | `--road sumo`, InTAS, `--sumo-buildings` | as A |
| mobility | engine flow, arrival 2/s | `sumo_replay` of `intas_full_300s_dt1.trace` | engine flow, arrival 2/s |
| length | 300 s at dt 1.0 s | 300 s at dt 1.0 s | **120 s at dt 0.1 s (10 Hz CAM)** |
| vehicles | 593 (83.5 % TR Type 2) | 333 (83.8 % Type 2) | 250 (84.4 % Type 2) |
| buildings | **0 — synthetic canyon fallback at 4 blockages/km** | **21 717 real footprints** | 0 — canyon fallback |
| `radio_cap_max_mult` | 6.0 (CLI default) → reach 3000 m | 1.4 (flagship) → reach 700 m | 6.0 → reach 3000 m |

Scene A is the pinned reference arm's geometry **with `--radio-model geometric --radio-env urban`**;
the pinned reference digest itself runs `radio_model="disc"` and never constructs this class, which
is why the digests cannot move. Scene B reproduces `FULL-CITY-SCENE.md`'s arm A **exactly** — the
re-run's `data_digest_sha256 = 1af4eeacda8957d764048050cdd0d6e8e37af357728d10f8064ca6895c17284d`
matches that dataset byte-for-byte on today's tree. Scene C exists because two of the six terms
(the NLOSv hold and the fade correlation) are claims about **time**, and a 1 Hz engine cannot test
them: the standard's own CAM rate is 10 Hz.

Scene A's NLOSb comes from the **synthetic urban-canyon fallback**, not from geometry. The channel's
own counters give the link-state split of the classified population:

| scene | LOS | NLOSv | NLOSb |
|---|---|---|---|
| A — reference arm (canyon fallback) | 24.5 % | 8.5 % | **67.1 %** |
| B — full city (21 717 real footprints) | 26.3 % | 15.2 % | 58.5 % |
| C — 10 Hz arm (canyon fallback) | 23.7 % | 6.6 % | **69.7 %** |

Four of the six terms reach only part of that population. The three NLOSv terms (`nlosv_hold`,
`nlosv_model`, `blocker_width`) touch **only the NLOSv share** — 8.5 % on scene A against 15.2 % on
scene B, so scene A dilutes them by nearly a factor of two. The breakpoint is deliberately not
applied to NLOSb (`evaluate_raw`: a two-ray ground reflection is a line-of-sight effect), so it
reaches a third of scene A and two-fifths of scene B. The antenna gain and the fade correlation are
applied in every state. Scene B has both the real occlusion and the largest NLOSv share, and its
numbers should be read as the authoritative ones.

### 1.5 Sub-sampling, and what is never sub-sampled

Scene A offers 4 538 976 ordered link-steps under 1000 m; the budget is 3 000 000, so whole ordered
**pairs** are dropped by a deterministic `crc32` of the pair (keep fraction 0.6609) — identically in
every arm. **Steps are never dropped**: burst-length and inter-reception statistics are defined over
*consecutive* steps, and a step-sub-sampled trace has no consecutive steps in it. Scenes B and C run
their full pair populations (keep fraction 1.0000).

For the same reason the step index is `round(t / dt)` and not `awareness.snapshots`' 1 s awareness
bucket: at dt = 0.1 s that bucket would collapse ten CAMs on one link into one row. At dt = 1 s the
two agree, and the tool **measures** that agreement rather than assuming it (`bucket_check`).

### 1.6 The instrument, validated against the published analytic figures

Scene B `off` is the same configuration `FULL-CITY-SCENE.md` measured with the analytic
quadrature instrument. A full Monte-Carlo replay of the channel and a closed-form integration of it
are independent computations, and they agree:

| | `awareness.py` (analytic, published) | this replay (empirical, `off`) | agreement |
|---|---|---|---|
| PDR at 200 m | 0.6026 | **0.60108** | **0.25 %** |
| PDR 0.90 crossing | 83.1 m | 84.7 m | 1.9 % |
| PDR 0.50 crossing | 241.3 m | 237.6 m | 1.5 % |
| PDR 0.20 crossing | 508.1 m | 489.2 m | 3.7 % |
| NAR-0.90-equivalent range | 338.5 m | 328.4 m | 3.0 % |
| gray-zone ratio d20/d90 | 6.11 | 5.776 | 5.5 % |

That is the licence to read everything below as a measurement of the model rather than of the
instrument.

---

## 2. What each term does to one link's budget

Every value here is a call into `mock_pipeline.run`'s own functions
(`python tools/channel_physics.py terms`), not a re-derivation.

**Antenna element gain at the horizon, dBi (one end):**

| vehicle | 0° | 30° | 60° | 90° | 120° | 150° | 180° |
|---|---|---|---|---|---|---|---|
| car / motorcycle (TR Type 2, rooftop) | 3.0 | 3.0 | 3.0 | 3.0 | 3.0 | 3.0 | 3.0 |
| truck / bus (TR Type 3, front+rear) | 3.0 | 2.25 | 0.0 | **−3.75** | 0.0 | 2.25 | 3.0 |

`evaluate_raw` adds **both** ends. So a car–car link gains a **flat +6.0 dB at every bearing** — no
pattern at all — and only a truck–truck link sees the 13.5 dB bearing swing. At these fleet mixes
(≈ 84 % Type 2) roughly **70 % of links get a pure +6 dB gain and 2.6 % see the full pattern**.

**Two-ray excess loss beyond d_b:** ~~177.12 m~~ → **201.53 m** at the corrected 1.6 m antenna
height (d_b is quadratic in it). The dB figures below were computed at the old 177.12 m and are
therefore an over-estimate at every distance: 0 at 100/150/177 m, **+1.23 at 200 m**, +5.33 at
300 m, +8.24 at 400 m, +10.50 at 500 m, +13.91 at 700 m.

**NLOSv mean excess loss (dB), by the TR 37.885 case the geometry resolves to.** The
`measured_boban` column that stood here is **retracted** (§6 R2). What replaced it is not a second
model but the standard's own case rule, which at the corrected antenna height finally distinguishes
a car from a truck:

| distance | 10 m | 20 m | 26 m | 50 m | 100 m | 300 m | 541 m | 700 m |
|---|---|---|---|---|---|---|---|---|
| Case 3 — car/motorcycle blocker (σ 4.0 dB) | 5.0 | 5.0 | 5.0 | 5.0 | 5.0 | 5.0 | 5.0 | 6.68 |
| Case 2 — truck/bus blocker (σ 4.5 dB) | 9.0 | 9.0 | 9.0 | 9.0 | 9.0 | 9.0 | 9.0 | 10.68 |

Before the antenna-height fix **both rows were 9.0**: at 1.5 m antennas every blocker was taller
than both endpoints, so the standard's three-case rule collapsed to one case and a motorcycle
attenuated exactly as much as an articulated truck. `python tools/channel_physics.py terms`
regenerates this table from the shipped functions.

**Blocker half-width:** car/motorcycle 1.0 m either way; truck/bus 1.0 → **1.3 m**. That is the whole
of term 5.

**Clarke correlation ρ = J₀(2π f_D dt):**

| relative speed | 0 | 0.01 | 0.1 | 0.5 | 1 | 5 | 30 m/s |
|---|---|---|---|---|---|---|---|
| dt = 0.1 s | 1.0 | 0.9962 | 0.6528 | 0.1977 | 0.1232 | −0.0222 | 0.0358 |
| dt = 1.0 s | 1.0 | 0.6528 | 0.1232 | −0.0222 | −0.0675 | −0.0053 | −0.0027 |

At dt = 1 s the term is inert above 0.1 m/s of *relative* speed; at dt = 0.1 s, above ~1 m/s. It can
only ever change the **time correlation**, never the marginal — the probability-integral transform
makes the Nakagami marginal exact — so it cannot move PDR at all, by construction. Only burst
statistics can move.

---

## 3. Scene B — the full city, InTAS, 21 717 footprints

This is the authoritative scene: real occlusion, real traffic, the configuration
`FULL-CITY-SCENE.md` reports.

*300 steps at dt 1.0 s, 333 vehicles, 1 400 660 ordered link-steps per arm, keep fraction 1.0000.*

> **Reading the arm names in every table from here to §5.** Two of them no longer exist as shipped
> arms, and the rows are kept rather than deleted because they are measurements that were actually
> taken: **`nlosv_boban`** is the arm retracted in §6 R2 — its numbers are what the withdrawn model
> did, not a recommendation — and **`antenna_eirp_fix`** is now called `antenna_conducted_20dbm`,
> because §6 R1 establishes that it is the *non*-conformant arm rather than a fix. And every row in
> these tables predates the antenna-height correction: see the staleness notice at the top.

| arm | PDR@100m | PDR@200m | PDR@300m | PDR all | d90 m | d50 m | d20 m | gray width m | gray ratio | NAR-0.90 range m |
|---|---|---|---|---|---|---|---|---|---|---|
| `off` | 0.8659 | 0.6011 | 0.3730 | 0.3117 | 84.7 | 237.6 | 489.2 | 404.5 | 5.776 | 328.4 |
| `nlosv_hold` | 0.8629 | 0.6006 | 0.3726 | 0.3114 | 84.3 | 239.0 | 489.6 | 405.2 | 5.807 | 327.9 |
| `antenna` | 0.9281 | **0.6934** | 0.4636 | 0.3645 | 111.6 | 274.2 | 563.0 | 451.5 | 5.047 | 389.4 |
| `nlosv_boban` | 0.8115 | 0.5905 | 0.3696 | 0.3052 | **48.5** | 233.7 | 497.3 | 448.8 | **10.258** | 327.8 |
| `breakpoint` | 0.8659 | 0.5748 | **0.2511** | 0.2394 | 84.7 | 218.1 | **330.0** | **245.3** | 3.896 | 263.0 |
| `blocker_width` | 0.8640 | 0.5991 | 0.3716 | 0.3107 | 84.1 | 236.9 | 488.1 | 404.0 | 5.803 | 326.0 |
| `jakes` | 0.8632 | 0.5994 | 0.3719 | 0.3117 | 84.9 | 237.8 | 491.6 | 406.7 | 5.790 | 328.4 |
| `conformant` (hold+antenna+width) | 0.9279 | 0.6927 | 0.4615 | 0.3637 | 111.3 | 274.2 | 561.5 | 450.2 | 5.046 | 387.9 |
| `all_on` | 0.8776 | 0.6486 | 0.3754 | 0.2775 | 80.4 | 247.8 | 412.3 | 331.9 | 5.127 | 320.7 |
| `antenna_eirp_fix` *(diagnostic)* | 0.9014 | 0.6522 | 0.4238 | 0.3399 | 99.0 | 257.8 | 530.9 | 431.9 | 5.363 | 359.2 |

**Burst structure and inter-reception — the metrics this project had never measured:**

| arm | P(loss) | P(L\|L) | burstiness | run mean | run p95 | run max | IRT mean s | IRT p95 s | IRT p99 s | IRT max s | µs/eval | ×`off` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `off` | 0.6883 | 0.9248 | 1.344 | 11.133 | 61 | 300 | 1.554 | 3 | 11 | 171 | 28.46 | 1.00× |
| `nlosv_hold` | 0.6886 | 0.9302 | 1.351 | **11.831** | 63 | 300 | 1.543 | 3 | 11 | 171 | 29.13 | 1.02× |
| `antenna` | 0.6355 | 0.9401 | 1.479 | 13.274 | 66 | 300 | 1.373 | 2 | 8 | 170 | 30.05 | 1.06× |
| `nlosv_boban` | 0.6948 | 0.9241 | 1.330 | 11.075 | 61 | 300 | 1.584 | 3 | 12 | 171 | 27.13 | 0.95× |
| `breakpoint` | 0.7606 | 0.9569 | 1.258 | **17.321** | 81 | 300 | 1.535 | 3 | 11 | 216 | 28.48 | 1.00× |
| `blocker_width` | 0.6893 | 0.9246 | 1.341 | 11.111 | 61 | 300 | 1.557 | 3 | 11 | 171 | 29.07 | 1.02× |
| `jakes` | 0.6883 | 0.9253 | 1.344 | 11.186 | 62 | 300 | 1.550 | 3 | 11 | 172 | 38.85 | **1.37×** |
| `conformant` | 0.6363 | 0.9442 | 1.484 | 14.032 | 68 | 300 | 1.370 | 2 | 8 | 170 | 27.84 | 0.98× |
| `all_on` | 0.7225 | 0.9548 | 1.321 | 16.595 | 75 | 300 | 1.498 | 3 | 11 | 229 | 38.24 | 1.34× |
| `antenna_eirp_fix` | 0.6601 | 0.9314 | 1.411 | 11.956 | 63 | 300 | 1.450 | 2 | 9 | 171 | 27.63 | 0.97× |

*(Run lengths and inter-reception gaps are in steps, which at dt = 1 s are seconds. Note the shape of
the IRT distribution: a mean gap of 1.55 s and a p95 of 3 s, but a p99 of 11 s and a maximum of
171 s — a link that loses one CAM usually recovers immediately, and the tail is made entirely of the
long blocked episodes the run-length column counts. Note also that the run-length **maximum is 300
steps in every arm**: the whole 300 s window. On the full city there are links that never once
deliver, and no term changes that. The µs/eval column carries roughly ±5 % of measurement noise —
three arms that strictly add arithmetic read below 1.00×, which is how large the noise is.)*

**Change against `off`:**

| arm | ΔPDR@200m | ΔPDR all | Δgray width | Δd50 | ΔNAR-0.90 range | Δrun mean | ΔP(L\|L) |
|---|---|---|---|---|---|---|---|
| `nlosv_hold` | −0.0005 | −0.0003 | +0.7 m | +1.4 m | −0.5 m | **+0.699 (+6.3 %)** | +0.0054 |
| `antenna` | **+0.0923** | +0.0528 | +47.0 m | +36.6 m | +61.0 m | +2.142 | +0.0153 |
| `nlosv_boban` | −0.0106 | −0.0065 | +44.3 m | −3.9 m | −0.6 m | −0.058 | −0.0007 |
| `breakpoint` | −0.0262 | **−0.0723** | **−159.2 m** | −19.5 m | −65.4 m | **+6.188 (+56 %)** | +0.0321 |
| `blocker_width` | −0.0020 | −0.0010 | −0.5 m | −0.7 m | −2.4 m | −0.021 | −0.0002 |
| `jakes` | −0.0017 | −0.0000 | +2.2 m | +0.2 m | 0.0 m | +0.053 | +0.0005 |
| `conformant` | +0.0916 | +0.0520 | +45.7 m | +36.6 m | +59.5 m | +2.899 | +0.0194 |
| `all_on` | +0.0475 | −0.0342 | −72.6 m | +10.2 m | −7.7 m | +5.463 | +0.0299 |
| `antenna_eirp_fix` | +0.0511 | +0.0283 | +27.4 m | +20.2 m | +30.8 m | +0.823 | +0.0066 |

**Why `blocker_width` moves nothing, quantitatively.** The channel's own state counters: with the
uniform width, 184 415 LOS / 106 086 NLOSv / 409 829 NLOSb link-classifications; with the TR 37.885
widths, 182 338 / 108 163 / 409 829. Widening every truck and bus from a 2.0 m to a 2.6 m corridor
**reclassifies 2077 link-steps, 0.30 % of the population.** There is nothing for a metric to see.

**Why `nlosv_hold` moves burst length and nothing else.** It changes no mean and no variance — it
changes how long one draw survives. Its own counter says how long: **4.37 classification-steps per
draw** on this scene, i.e. the same blockage deviate is held across 4.4 CAMs at 1 Hz. That is the
whole mechanism, and it lands exactly where it should: `P(L|L)` +0.0054, run mean +6.3 %, PDR
unmoved.

---

## 4. Scene C — the same arm at 10 Hz, where the time-domain terms can be tested

*1198 steps at dt 0.1 s (120 s), 250 vehicles, 9.665 packets per 1 s window, 7 506 068 ordered
link-steps available, keep fraction 0.3997, 2 995 616 evaluated per arm.*

| arm | PDR@200m | PDR all | NAR@200m | d90 m | d50 m | d20 m | gray width m | gray ratio | run mean | P(L\|L) | **Z (200–250 m)** | µs/eval |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `off` | 0.3482 | 0.2685 | 0.4036 | 34.0 | 117.3 | 329.8 | 295.8 | 9.706 | 20.58 | 0.9560 | **1.2315** | 14.16 |
| `nlosv_hold` | 0.3484 | 0.2686 | 0.4002 | 34.0 | 117.6 | 328.6 | 294.6 | 9.662 | **22.10 (+7.4 %)** | 0.9594 | 1.2096 | 15.09 |
| `antenna` | 0.3906 | 0.3072 | 0.4557 | 48.6 | 145.4 | 378.5 | 329.9 | 7.786 | 26.27 | 0.9666 | 1.1709 | 16.30 |
| `nlosv_boban` | 0.3376 | 0.2620 | 0.3942 | 29.0 | 105.6 | 328.3 | 299.3 | 11.321 | 21.65 | 0.9584 | 1.2132 | 12.87 |
| `breakpoint` | 0.3367 | 0.2216 | 0.4026 | 34.0 | 117.3 | 269.8 | 235.8 | 7.940 | 20.14 | 0.9550 | 1.3680 | 12.81 |
| `blocker_width` | 0.3477 | 0.2679 | 0.4036 | 34.0 | 117.2 | 328.2 | 294.3 | 9.661 | 20.51 | 0.9558 | 1.2334 | 12.18 |
| `jakes` | 0.3473 | 0.2684 | 0.4032 | 34.0 | 117.3 | 329.2 | 295.2 | 9.691 | 20.69 | 0.9563 | 1.2272 | **23.57 (1.66×)** |
| `conformant` | 0.3896 | 0.3067 | 0.4547 | 48.7 | 145.0 | 377.6 | 328.9 | 7.755 | 27.54 | 0.9683 | 1.1667 | 15.61 |
| `all_on` | 0.3680 | 0.2681 | 0.4432 | 42.1 | 135.7 | 318.0 | 275.9 | 7.549 | 21.47 | 0.9580 | 1.2322 | 27.13 |
| `antenna_eirp_fix` | 0.3681 | 0.2855 | 0.4148 | 39.8 | 124.2 | 354.4 | 314.6 | 8.908 | 23.39 | 0.9619 | 1.1795 | 15.66 |

**The two time-domain terms, band by band** (this is the only place either of them is visible):

| band | `off` | `nlosv_hold` | `jakes` |
|---|---|---|---|
| 0–50 m | P(L\|L) 0.5079, run 1.94, Z 1.810 | 0.5150, run 1.97, Z 1.811 | **0.5313, run 2.05 (+5.7 %), Z 1.722** |
| 50–100 m | 0.8244, run 4.69 | **0.8381, run 5.07 (+8.1 %)** | 0.8246, run 4.63 |
| 100–150 m | 0.9551, run 13.11 | **0.9621, run 15.38 (+17.3 %)** | 0.9554, run 13.21 |
| 150–200 m | 0.9393, run 15.68 | **0.9466, run 17.65 (+12.6 %)** | 0.9397, run 15.73 |
| 200–250 m | 0.9441, run 16.81 | **0.9496, run 18.46 (+9.8 %)** | 0.9448, run 17.05 |
| 250–300 m | 0.9561, run 22.47 | **0.9588, run 23.76 (+5.7 %)** | 0.9562, run 22.35 |

`jakes` does exactly what its author scoped it to do and no more: it bites **only** in the 0–50 m
band, which is where two vehicles queued at the same red light sit, and there it lengthens loss runs
by 5.7 %. Everywhere else it is inside the noise. `nlosv_hold`, by contrast, is worth 10–17 % of
loss-run length across the whole 100–250 m band once the CAM rate is the standard's own 10 Hz.

### 4.1 The burstiness result, in the reference's own currency

Boban & d'Orey do not model NAR as `1 − (1 − PDR)^N`; they fit `1 − (1 − PDR)^Z` with **Z ≤ N**,
because "CAM losses arrive in bursts", and their fitted urban value is **Z = 5.4579** at 10 Hz
(`refdata/v2x_awareness_conditions.json`, `nar_shot_multiplicity_z`). Z is therefore a published,
measured burstiness statistic, and this replay measures the same quantity directly.

Our model, at ~9.67 shots per second, delivers an aggregate **Z = 1.23 at 200–250 m** and
**Z = 1.81 in the 0–50 m band**. Against the reference's fitted 5.46, **our 10 Hz loss process is far
burstier than the one they measured.**

Two honest caveats, both of which bound the claim rather than rescue it:

1. **Aggregation biases Z downward, and the direction is provable.** `(1 − p)^N` is convex in p, so
   by Jensen `E[(1−p)^N] ≥ (1 − E[p])^N`, i.e. a band that mixes links of different quality shows a
   lower apparent Z than any of its links has. Our band population mixes LOS and NLOSb links; the
   paper's is a 3–9 vehicle convoy on a shared route. The 0–50 m band, where PDR is 0.956 and the
   population is nearly all LOS, is the least contaminated read, and it still gives 1.81 of a
   possible 9.67.
2. This is a **model-versus-measurement** comparison across different scenes and different pair
   populations. It is a signpost, not a falsification.

What it does establish is the *ranking*: `nlosv_hold` moves Z **further from** the reference
(1.2315 → 1.2096) and `breakpoint` moves it toward (→ 1.3680), and neither effect is large.

---

## 5. Scene A — the reference arm

Scene A's NLOSb is the **synthetic canyon fallback**, not geometry, and it is 67.1 % of the
classified link-step population against the full city's 58.5 %; its NLOSv share is 8.5 % against
15.2 %. The three NLOSv terms are therefore diluted by nearly a factor of two here, which is why
this scene is reported after the full city rather than before it.

*299 steps at dt 1.0 s, 593 vehicles, 4 538 976 ordered link-steps available, keep fraction 0.6609,
2 998 814 evaluated per arm. Z is identically 1 here — a 1 Hz engine gets one shot per 1 s window,
so its NAR **is** its per-packet PDR, and the tool reproduces that exactly as a sanity check.*

| arm | PDR@100m | PDR@200m | PDR@300m | PDR all | d90 m | d50 m | d20 m | gray width m | gray ratio | NAR-0.90 range m | vs Boban curve |
|---|---|---|---|---|---|---|---|---|---|---|---|
| `off` | 0.6224 | 0.3810 | 0.2332 | 0.2810 | 42.2 | 150.7 | 331.1 | 288.9 | 7.845 | 218.3 | 2.51× |
| `nlosv_hold` | 0.6233 | 0.3814 | 0.2337 | 0.2811 | 42.1 | 150.8 | 331.1 | 289.0 | 7.869 | 218.4 | 2.51× |
| `antenna` | 0.7080 | 0.4286 | 0.2698 | 0.3192 | 60.4 | 170.6 | 366.2 | 305.8 | 6.059 | 244.0 | 2.80× |
| `nlosv_boban` | 0.5824 | 0.3667 | 0.2319 | 0.2734 | **32.4** | 142.3 | 330.9 | 298.6 | **10.219** | 213.4 | 2.45× |
| `breakpoint` | 0.6224 | 0.3668 | **0.1686** | 0.2327 | 42.2 | 150.1 | **283.8** | **241.6** | 6.724 | 208.2 | 2.39× |
| `blocker_width` | 0.6221 | 0.3801 | 0.2320 | 0.2801 | 42.2 | 150.5 | 329.9 | 287.7 | 7.818 | 217.8 | 2.50× |
| `jakes` | 0.6222 | 0.3807 | 0.2335 | 0.2809 | 42.2 | 150.5 | 330.9 | 288.7 | 7.849 | 218.5 | 2.51× |
| `conformant` | 0.7082 | 0.4280 | 0.2693 | 0.3186 | 60.5 | 170.5 | 365.7 | 305.2 | 6.041 | 243.4 | 2.80× |
| `all_on` | 0.6753 | 0.3993 | 0.2283 | 0.2759 | 49.2 | 162.6 | 321.3 | 272.1 | 6.536 | 221.8 | 2.55× |
| `antenna_eirp_fix` *(diagnostic)* | 0.6548 | 0.4038 | 0.2514 | 0.2977 | 48.8 | 159.3 | 348.6 | 299.8 | 7.145 | 227.0 | 2.61× |

| arm | P(loss) | P(L\|L) | burstiness | run mean | run p95 | run max | IRT mean s | IRT p95 s | IRT p99 s | IRT max s | µs/eval | ×`off` |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `off` | 0.7190 | 0.8586 | 1.194 | 5.956 | 25 | 139 | 2.343 | 7 | 23 | 127 | 15.08 | 1.00× |
| `nlosv_hold` | 0.7189 | 0.8601 | 1.196 | 6.006 | 25 | 139 | 2.339 | 7 | 23 | 128 | 15.68 | 1.04× |
| `antenna` | 0.6808 | 0.8507 | 1.250 | 5.696 | 23 | 139 | 2.191 | 7 | 22 | 127 | 17.55 | 1.16× |
| `nlosv_boban` | 0.7266 | 0.8612 | 1.185 | 6.047 | 25 | 139 | 2.389 | 8 | 23 | 127 | 15.51 | 1.03× |
| `breakpoint` | 0.7673 | 0.8924 | 1.163 | **7.397** | 32 | 139 | 2.191 | 6 | 21 | 118 | 15.45 | 1.02× |
| `blocker_width` | 0.7199 | 0.8588 | 1.193 | 5.964 | 25 | 139 | 2.346 | 7 | 23 | 127 | 15.03 | 1.00× |
| `jakes` | 0.7191 | 0.8586 | 1.194 | 5.959 | 25 | 139 | 2.344 | 7 | 23 | 128 | **24.22** | **1.61×** |
| `conformant` | 0.6814 | 0.8516 | 1.250 | 5.721 | 23 | 131 | 2.191 | 7 | 22 | 124 | 16.29 | 1.08× |
| `all_on` | 0.7241 | 0.8721 | 1.204 | 6.453 | 27 | 139 | 2.189 | 7 | 21 | 117 | 27.86 | **1.85×** |
| `antenna_eirp_fix` | 0.7023 | 0.8553 | 1.218 | 5.845 | 24 | 139 | 2.272 | 7 | 23 | 127 | 17.06 | 1.13× |

**Change against `off`:**

| arm | ΔPDR@200m | ΔPDR all | Δgray width | Δd50 | ΔNAR-0.90 range | Δrun mean | ΔP(L\|L) | cost × |
|---|---|---|---|---|---|---|---|---|
| `nlosv_hold` | +0.0004 | +0.0001 | +0.1 m | +0.1 m | +0.1 m | **+0.050 (+0.8 %)** | +0.0015 | 1.04× |
| `antenna` | +0.0476 | +0.0382 | +16.9 m | +19.9 m | +25.7 m | −0.260 | −0.0079 | 1.16× |
| `nlosv_boban` | −0.0143 | −0.0075 | +9.7 m | −8.4 m | −4.9 m | +0.091 | +0.0026 | 1.03× |
| `breakpoint` | −0.0142 | −0.0483 | **−47.3 m** | −0.6 m | −10.1 m | **+1.441 (+24 %)** | +0.0338 | 1.02× |
| `blocker_width` | −0.0009 | −0.0009 | −1.2 m | −0.2 m | −0.5 m | +0.008 | +0.0002 | 1.00× |
| `jakes` | −0.0003 | −0.0000 | −0.2 m | −0.2 m | +0.2 m | +0.003 | +0.0000 | **1.61×** |
| `conformant` | +0.0470 | +0.0376 | +16.3 m | +19.8 m | +25.1 m | −0.235 | −0.0070 | 1.08× |
| `all_on` | +0.0183 | −0.0051 | −16.8 m | +11.9 m | +3.5 m | +0.497 | +0.0135 | 1.85× |
| `antenna_eirp_fix` | +0.0228 | +0.0168 | +10.9 m | +8.6 m | +8.7 m | −0.111 | −0.0033 | 1.13× |

Two things are worth reading off this scene specifically.

* **`nlosv_hold` is essentially inert here** (+0.8 % loss-run length against +6.3 % on the full city
  and +7.4 % at 10 Hz). Scene A has the densest fleet of the three in the smallest area, so the
  identity of the tallest blocker on a given link changes almost every step: the channel's own
  counter says a held draw survives **1.58 steps** here against 4.37 on the full city. The term's
  effect is a function of *blocker dwell time measured in CAMs*, and nothing else — see R6.
* **`antenna` shortens loss runs here (−0.260) and lengthens them on the full city (+2.14).** Both
  are consequences of the same +6 dB: in scene A the extra budget rescues links that were failing
  inside an existing episode, so runs shorten; on the full city it also brings *new, marginal*
  long-range links above the reception threshold, and those links contribute long runs that did not
  exist before. This is why a burst statistic must never be read without its PDR beside it.

---

## 6. Regressions and non-effects, stated loudly

*R1–R8 are revision 1's; **R1 and R2 are struck through and rewritten** by revision 2, and R9–R13
are new. R9 is a **security** defect and is the most important thing in this document.*

### R1 — ~~`radio_antenna_pattern` double-counts the transmitter's antenna gain~~ **WITHDRAWN**

**Revision 1 got this backwards, and the recommendation it produced was the wrong one.** The
argument was: `run.py` documents the knob as EIRP (`radio_tx_power_dbm: float = 23.0  # EIRP`), an
EIRP already contains the transmitter's antenna gain, so adding the transmit element gain back on
top of it double-counts 3 dB on the ~70 % of links that are car–car. The `antenna_eirp_fix` arm
(transmit power dropped to a conducted 20 dBm) measured "the size of the double count" at **45 %**
of the term's effect on the full city and **53 %** at 10 Hz.

**The standard settles it, and it settles it the other way.** TR 37.885 V15.3.0 Table 6.1.1-1 lists
transmit powers in a single column:

> "BS Tx power — Macro BS: 49dBm … Micro BS: 24dBm"
> "UE Tx power — Vehicle/pedestrian UE or UE type RSU: 23dBm"

49 dBm is a macro base station's **conducted PA power**, and the antenna element gain is given
**separately**, in Table 6.1.4-8: "Max direct. gain of the antenna element — 3 dBi". The only
occurrence of "e.i.r.p." in the whole document is in clause 5, a 63–64 GHz *regulatory* survey, not
in the simulation assumptions. So the TR-conformant V2V budget is

```
23 dBm conducted  +  3 dBi (Tx element)  −  PL  +  3 dBi (Rx element)   =   29 − PL
```

which is exactly what the model computes **with the pattern on**. Three consequences:

1. **The bug was the comment**, `radio_tx_power_dbm: float = 23.0  # EIRP`, not the arithmetic.
2. **The shipped default is 6 dB below TR 37.885's own link budget** — a pre-existing conformance
   gap that nobody had named and no refdata entry recorded. It has one now
   (`link_budget_is_6db_below_tr37885_by_default`) and a test
   (`test_the_default_link_budget_is_6db_below_the_standards_own`).
3. **The `antenna_eirp_fix` arm is the NON-conformant one.** It is renamed
   `antenna_conducted_20dbm` and re-labelled as "what it costs to run 3 dB under the standard",
   which is a legitimate question and a different one.

**The caveat, stated rather than hidden.** `run.py` justifies 23 dBm in ETSI EIRP terms as well
("ETSI caps EIRP at 33"), so this repository genuinely holds two conventions that meet on one
number. Under the ETSI reading the value is an EIRP and the transmit element gain *would* be a
double count. 23 is defensible either way, which is precisely why the ambiguity survived unnoticed
— and why the fix is to say which convention the model is claiming, not to change the number.

**And a second withdrawal in the same term (finding H).** The block comment claimed the pattern
"unlocks" Nilsson et al.'s 2–4 m de-correlation distance as "the APPLICABLE comparison" for
`TR37885_SHADOW_DECORR_M`. It does not, on the source's own terms: that figure is measured on the
**zero-mean, offset-subtracted** large-scale process, and the paper says where the offset goes —
*"The mean of Ψσ … represents the differences in the particular traffic situation and **the gain of
the involved antennas** for the particular communication link."* Their conditional asks for a model
of the **per-link** antenna gain. This term supplies a **statistical element pattern**, and for the
Type 2 pair that is ~70 % of these fleets it is a *constant* +3 dBi at every bearing, removing no
per-link variation whatever. `RADIO-VS-REALITY.md` §2, in the same tree, already said so. The
decorrelation pin is unchanged and is not re-litigated.

### R2 — ~~`radio_nlosv_model="measured_boban"` collapses the reliable zone~~ **RETRACTED: the arm is gone**

Revision 1 measured the arm and kept it: d90 fell from **84.7 m to 48.5 m** on the full city (−43 %)
and the gray-zone ratio nearly doubled, 5.776 → **10.258**, but the verdict was "the measurement
behind it is sound and the specification it replaces cites none — so keep it selectable".

**The measurement behind it was not sound, and the arm is withdrawn.** Four reasons, any one
sufficient:

1. **It was not measurement-based.** Its level anchor — 5 dB for a car, 20 dB for a truck at 100 m —
   is GEMV² *simulator output*. Figure 12's own caption in the source says so: *"Received power
   distribution **as generated by GEMV²** … For each link, a single vehicle of a given type is
   placed between transmitter and receiver, both of which are passenger cars with the height of
   1.5 meters."* The body text says only that the results are *"in line with previous
   measurements"* — consistent with, not drawn from. **Only the slope was measured.** This tree
   had already recorded exactly that, in capitals, in
   `refdata/pathloss_3gpp_tr37885.json` and in `RADIO-VS-REALITY.md` §7.7 — and the arm was built
   and shipped anyway, the same day, in the same tree. *Two files asserting opposite things about
   one number is the failure this retraction exists to correct.*
2. **It was falsified by a measurement this repository already quotes, and the specification beat
   it by ~10 dB.** Segata et al. (IEEE VNC 2013 §IV; A12 freeway, 5.89 GHz 802.11p, rooftop omnis,
   truck blocker — *body text*, not a figure) measure a LOS-to-NLOS truck difference *"as high as
   10 dB"* at 80 m and *"in the order of 5 dB"* at 120 m:

   | distance | Segata, measured | `measured_boban` truck | TR 37.885 spec |
   |---|---|---|---|
   | 80 m | ≈ 10 dB | 21.26 dB (**+11.3**) | 9.04 dB (**−0.96**) |
   | 120 m | ≈ 5 dB | 20.00 dB (**+15.0**) | 9.04 dB (**+4.04**) |

   The arm existed because the spec's error *changes sign* across the band. Against the other
   5.9 GHz measurement in the same citation set the arm's error is **one-sided, +11 to +15 dB, an
   order of magnitude worse than the spec's**. §5 of `RADIO-VS-REALITY.md` had already extracted
   these numbers and graded the *specification* against them. Nobody graded the arm.
3. **No measurement in its chain was a car blocking a car link.** The "car: 5.0" anchor is for a
   GEMV² blocker whose mean height (1.5 m) *equals* the transmit and receive antenna height — the
   marginal case — and the code mapped it onto the both-antennas-below branch. Its supporting
   "5–7 dB at 100 m" pair is worse: the 7 dB is the 802.11p van and the 5 dB is the **802.11b/g van
   at 2412 MHz**. A 2.4 GHz figure was bracketing a 5.9 GHz model.
4. **Licence.** The level anchor digitised an IEEE-copyright figure into a source constant, which
   this project's standing rule forbids outright. `RADIO-VS-REALITY.md` §8 asserts *"Nothing under a
   licence-unclear or non-CC licence is committed anywhere in the tree"*; that sentence was false
   while `BOBAN_NLOSV_MU_AT_100M_DB` existed and is true again now.

**What the retraction leaves behind, and why that is the honest state.** The specified NLOSv term is
still *flat* where two independent campaigns say the loss *decays*, and `test_geometric_channel.py`
§8 still pins that falsification. The model is therefore wrong in a way we have measured, rather
than wrong in a way we replaced with something measurably worse. **The decay is real and a successor
arm is welcome** — but it must (a) be named for what it is rather than for a measurement it only
partly uses, (b) carry no constant digitised from a figure or taken from a licence-unclear source,
and (c) be graded against Segata et al. *before* it ships, not after.

### R3 — `radio_breakpoint="two_ray"` is the largest mover and the weakest evidence

It moves more metric than any other term: PDR beyond 300 m falls by a third (0.3730 → 0.2511), d20
comes in **159 m**, the gray zone narrows from 404.5 m to 245.3 m, and consecutive-loss runs lengthen
by **56 %** (11.13 → 17.32 steps). Its own author states the magnitude is *plausible, not confirmed*
— the 40 dB/decade slope is the classical asymptote, chosen because it is "the one
physics-not-fit value", and TR 37.885's urban fit may already embed canyon waveguiding.

**A term that moves the graded gray-zone width by 159 m must not default on while its magnitude is
explicitly unasserted.** The right next step is not to enable it but to bracket it: sweep
`radio_breakpoint_slope_db_per_decade` and publish the metric as a function of the slope, so the
reader sees how much of the movement is the breakpoint's *existence* (confirmed) and how much is its
*size* (not).

### R4 — `radio_blocker_width` moves nothing measurable

Widening every truck and bus corridor from 2.0 m to 2.6 m reclassifies **0.30 % of link-steps on
the full city (2077 of 700 330) and 0.21 % on the reference arm (3219 of 1 499 407)**; every metric
change on both scenes is inside the run-to-run noise. It is real conformance — TR 37.885 clause
6.1.2 does specify 2.6 m for Type 3 — but **it must never be cited as a realism improvement**,
because measured on two scenes it is not one. Its cost is also inside the noise, so it is not a
liability either; it is simply a box ticked.

### R5 — `radio_fading_correlation="jakes"` costs 61 % of a step and moves one band by 5 %

It cannot move PDR at all (the probability-integral transform makes the marginal exact), and at
dt = 1 s it moves nothing else either. At the standard's 10 Hz it moves loss-run length by **+5.7 %
in the 0–50 m band only**. For that it costs **1.608× of engine wall clock** on the reference arm
and 1.37–1.66× per link evaluation across the three scenes — the most expensive term in the set by
a wide margin, and the least effective.

This is not a criticism of the physics: the author's own scoping ("i.i.d. is *right* at 30 m/s") is
**confirmed** by this measurement, and where the term claims to bite — two vehicles stopped at the
same light — it does bite. It is a statement about cost-effectiveness. Of the six terms it buys the
least movement per unit of runtime by a wide margin, and it is the only one whose cost is large
enough to see above the engine-level noise (§7). Keep it for the study it was built for; do not pay
for it in a general run.

### R6 — `nlosv_hold`'s effect is an order of magnitude smaller in a scene than in an isolated probe

The builder reported "+32 % mean consecutive-loss run at 250 m". In a scene it is **+6.3 %** (full
city, 1 Hz), **+0.8 %** (reference arm, 1 Hz) and **+7.4 %** (10 Hz, rising to +17 % in the
100–150 m band). The isolated probe holds the blocker fixed; a scene does not. The channel's own
`nlosv_draw` counter measures exactly how long a draw survives, and it explains the whole spread:

| scene | NLOSv classifications | redraws | **steps per held draw** | Δ loss-run mean |
|---|---|---|---|---|
| A — reference arm, 1 Hz | 127 383 | 80 771 | **1.58** | +0.8 % |
| B — full city, 1 Hz | 108 163 | 24 753 | **4.37** | +6.3 % |
| C — 10 Hz arm | 98 569 | 17 198 | **5.73** | +7.4 % |

The term's effect is proportional to how many CAMs fit inside one blockage episode, and nothing
else. Scene A's fleet is the densest of the three in the smallest area, so the *identity* of the
tallest obstruction changes every step or two and the signature-keyed draw is refreshed almost as
often as it would have been redrawn anyway. Both numbers are correct; only the scene number predicts
what a run will show.

### R7 — the graded instrument is blind to all of it

Restated because it is the most consequential finding here: `awareness.propagation_pdr` takes
`(state, d_m, tx_power_dbm, decode_floor_dbm, radio_env)`. No channel, no config. It returns
LOS 0.9364 / NLOSv 0.5898 / NLOSb 0.0004 at 200 m **for every one of the ten arms**. Any claim that
one of these terms improved "the awareness ratio" or "PDR at 200 m", if measured with `awareness.py`,
is measuring the scene and not the physics.

**Demonstrated end to end, not just argued.** The unmodified CLI
`python -m scms_sim_ref.datagen.awareness <dataset>` was run over four of the reference arm's own
finished datasets — four separate engine runs, four different physics configurations — beside this
report's replay of the same four:

| arm | `awareness.py` PDR@200 m | `awareness.py` d90 / d50 / d20 | gray width | this replay's PDR@200 m |
|---|---|---|---|---|
| `off` | 0.3828 | 45.2 / 153.0 / 331.0 | 285.8 m | 0.3810 |
| `blocker_width` | 0.3828 | 45.2 / 153.0 / 331.1 | 285.9 m | 0.3801 |
| `antenna` | 0.3831 | 45.2 / 153.1 / 331.2 | 286.0 m | **0.4286** |
| `all_on` | 0.3832 | 45.2 / 153.1 / 331.3 | 286.0 m | **0.3993** |

The analytic instrument's whole spread across the four is **0.0004 in PDR and 0.2 m in gray-zone
width** — and even that is not the physics: those runs have slightly different vehicle populations
(R8), so it is the scene. The replay's spread over the same four is **0.048 in PDR**, 120× larger.
`d90 = 45.2 m` is identical to a tenth of a metre in all four.

This is not a criticism of `awareness.py`, which was built to answer a different question and
answers it well — §1.6 shows its quadrature reproduces a full Monte-Carlo replay of the same model
to 0.25 %, and its 0.3828 here is within 0.5 % of the replay's `off` figure. It is a statement about
coverage: the analytic instrument and the model have drifted apart, and the drift is invisible
because both still agree on the default configuration.

### R8 — the channel is not downstream-inert: it moves misbehaviour detection

The engine runs in §7 are **not** a controlled A/B, and finding that out was itself a result. The
ten arms produced **ten different emission traces**. The loop is real: the channel decides which
reports are received → which decides what the MA revokes → and `enforced(v, t)` then stops a revoked
vehicle broadcasting. So a channel term changes who is on the air.

On the full city, one seed:

| arm | reports | revoked | detection precision | recall |
|---|---|---|---|---|
| `off` | 3222 | 52 | 0.769 | 0.833 |
| `nlosv_hold` | 3294 | 53 | 0.774 | 0.854 |
| `antenna` | 3550 | 51 | 0.804 | 0.854 |
| `nlosv_boban` | 3206 | 55 | 0.727 | 0.833 |
| `breakpoint` | **2869** | 50 | 0.780 | **0.812** |
| `blocker_width` | 3215 | 52 | 0.769 | 0.833 |
| `jakes` | 3236 | 54 | 0.759 | 0.854 |
| `conformant` | **3656** | 53 | 0.792 | **0.875** |
| `all_on` | 3180 | 50 | 0.800 | 0.833 |
| `antenna_eirp_fix` | 3369 | 53 | 0.792 | 0.875 |

Report volume spans 2869–3656, a 27 % range, and it tracks the radio: the terms that deliver more
packets produce more reports. **Read the precision and recall columns with great care.** There are
48 attackers in this scene, so one attacker is ~2 pp of recall and the whole observed spread
(0.812–0.875) is three attackers on a single seed.

The reference arm has **100 attackers**, which resolves the same effect properly:

| arm | reports | revoked | precision | recall | implied true / false revocations |
|---|---|---|---|---|---|
| `off` | 12 935 | 158 | 0.576 | 0.91 | 91 / 67 |
| `nlosv_hold` | 12 814 | 156 | 0.583 | 0.91 | 91 / 65 |
| `antenna` | **15 214** | 161 | 0.565 | 0.91 | 91 / 70 |
| `nlosv_boban` | 12 534 | 159 | 0.572 | 0.91 | 91 / 68 |
| `breakpoint` | **11 107** | 139 | **0.647** | 0.90 | 90 / 49 |
| `blocker_width` | 12 889 | 160 | 0.569 | 0.91 | 91 / 69 |
| `jakes` | 12 980 | 162 | 0.568 | 0.92 | 92 / 70 |
| `conformant` | **15 273** | 160 | 0.569 | 0.91 | 91 / 69 |
| `all_on` | 13 182 | 153 | 0.595 | 0.91 | 91 / 62 |
| `antenna_eirp_fix` | 13 791 | 164 | 0.555 | 0.91 | 91 / 73 |

(The last column is `precision × revoked` and `revoked − that`; it is a derivation, but a checked
one — the implied true-positive count agrees with `recall × 100 attackers` in all ten rows.)

**Recall is pinned at 0.90–0.92 in every arm — the true-positive count never moves — while false
revocations span 49 to 73.** So on this scene the channel does not decide whether attackers are
caught; whatever it does, it does to **how many innocent vehicles are wrongly revoked**.

Nine of the ten arms sit in a flat 62–73 band with no visible ordering. **One separates: the
`breakpoint` arm, which makes the far field worse, sheds 1828 reports and with them 18 of 67 false
revocations (−27 %), at the cost of one true positive.** On a single seed that is one arm standing
out from a cluster, which is suggestive and not established — but it is the right shape for the
mechanism, and it says where to look. What *is* established is the coupling itself: a channel term
changes the report stream by up to 27 %, and no metric in §3–§5 would have revealed it.


### R9 — the antenna pattern opened an **RSSI oracle on fabricated collusion reports**

**This is a security defect, not a fidelity one, and it was invisible to every metric in §3–§5.**

A colluder files a misbehaviour report against a victim it never exchanged a frame with. That
report's `rssi_dbm` column therefore has to be *synthesised*, and the original design says exactly
why, in its own comment:

> "a NULL (or a missing key) here would be a perfect oracle … the honest synthesis is the value the
> channel model would produce for that TRUE link."

**The design was right and the implementation was one line short of it.** The collusion path
hand-rolled a *second* link budget — `geo_chan.tx_dbm - tr37885_pathloss_db(state, d)` — with no
antenna term, no NLOSv blockage and no breakpoint. With the shipped defaults that second budget
*was* the real one, and the two agreed:

| at d = 150 m, car–car, LOS, conditioned on decoding, n ≈ 60 000/arm | mean | sd |
|---|---|---|
| genuine, `radio_antenna_pattern="none"` | −67.504 dBm | 4.842 |
| fabricated (the hand-rolled budget) | −67.540 dBm | 4.842 |
| **offset** | **+0.036 dB** | — |
| genuine, `radio_antenna_pattern="tr37885_opt1"` | −61.716 dBm | 5.057 |
| **offset, with one knob switched on** | **+5.824 dB = 1.15 σ** | — |

At that separation a **single-threshold classifier distinguishes fabricated from genuine reports at
AUC ≈ 0.79**, on a column the misbehaviour-detection pipeline is explicitly allowed to read. Nothing
in the collusion code changed; a term was added to one side of the model.

**The fix is structural, because a fix that patched the antenna term would leave the next term to
reopen it.** The budget now has exactly one site — `GeometricChannel.mean_rx_dbm` — and both
callers go through it: `evaluate_raw` for a frame a receiver genuinely decoded, and a new
`synthesize_rx_dbm` for the fabricated column. The synthesis takes the same classification (from the
live per-link state where the reception loop has already classified this link this step, read-only
otherwise), and every random draw comes from the colluder's own stream, so it cannot perturb a
genuine link. A test asserts the single site *by source inspection* — `self.tx_dbm` may appear in
exactly one method outside the constructor — as well as behaviourally.

**Re-measured with every term on and off.** `python tools/channel_physics.py collusion --pairs
20000` reproduces this table exactly; the offset is *fabricated mean − genuine mean*, both
conditioned on decoding, and at n = 20 000 pairs one standard error of the difference is ≈ 0.05 dB.

| arm | A: d=150 LOS | B: d=300 LOS | C: d=150 NLOSv |
|---|---|---|---|
| `off` | −0.049 | +0.028 | −0.070 |
| `nlosv_hold` | −0.016 | +0.050 | +0.088 |
| **`antenna`** | **−0.035** | **−0.011** | **−0.093** |
| `antenna` at 0 dBi (the pattern's shape without its gain) | −0.004 | +0.071 | −0.083 |
| `breakpoint` | −0.053 | +0.053 | −0.114 |
| `breakpoint` at 28 dB/decade | +0.006 | +0.072 | −0.122 |
| `blocker_width` | −0.001 | +0.058 | −0.108 |
| `jakes` | +0.059 | +0.032 | −0.142 |
| `conformant` (hold + antenna + width) | +0.001 | +0.034 | −0.116 |
| **all of them at once** | **+0.055** | **+0.054** | **+0.073** |

*(genuine reference for the `off` arm: −67.521 dBm sd 4.789 at A, −72.119 sd 4.542 at B, −73.693
sd 4.711 at C.)*

**+5.824 dB → −0.035 dB on the arm that opened it**, and the worst |offset| anywhere in the table is
**0.142 dB, under three standard errors**. Three geometries because no single one exercises every
term: the breakpoint does not switch on below 201.5 m, and the blockage draw needs a blocker in the
way.

**A second, smaller oracle was found while fixing the first, and it is worth recording because it is
the subtler kind.** The first version of the synthesis conditioned on decoding by resampling only
the *fade*, on one unconditioned large-scale draw. That is not the channel law conditioned on
decoding: unfavourable shadowing and blockage draws survive in the fabricated column that the
genuine column has already lost to the floor. Measured on geometry C it left the fabricated
population **1.16 dB low**. The synthesis now draws K large-scale candidates and keeps one with
probability proportional to its own P(decode) — sampling-importance resampling, exact as K grows —
then draws the fade from its exactly-inverted truncated Gamma. The residual bias was *measured*
rather than assumed: K = 2 → −0.481 dB, 4 → −0.211, 8 → −0.097, 16 → −0.083, 32 → −0.044,
**64 → −0.029 dB**, inside one standard error at K = 64, which is what ships.

**And a third thing the existing test suite caught, which is the best argument in this section for
keeping falsification pins.** The exactly-correct tail branch produces values a *millionth of a dB*
above the decode floor on links that cannot decode at all — which, rounded to the two decimals the
dataset is written at, is a pile-up at exactly `-81.00`, i.e. the clamp fingerprint the whole design
exists to avoid, wearing better arithmetic. `test_rssi_absent_by_default_present_and_never_null_
under_geometric` failed on it immediately (118 of 1028 fabricated rows). Conditioning on decoding
conditions the link **state** as well as the draws: a genuine report of a 30 dB-under-the-floor link
does not exist *at any RSSI*, so on such a link the synthesis now falls back to the state a report
of that *length* does carry. That boundary is named in the code rather than hidden, and on the
collusion scene of R13 below — 90 s, grid 6×6, 50 % colluders, canyon NLOSb at 4/km — it fires on
**233 of 1028 syntheses (22.7 %)**.

### R10 — the antenna term was recommended having been measured on **zero RSU links**

`_station_antenna` returns "not modelled" for an RSU — TR 37.885 gives RSUs antenna **arrays**
(Tables 6.1.4-1 … 6.1.4-5) with panel bearings, tilt and TXRU mapping, and this channel has no array
factor to put one through. The first version counted the omission in `stats` and returned 0 dB. So
with the pattern on, a V2V link gains **2 × 3 = 6 dB** and a V2I link gains **3 dB**: a permanent
**3 dB relative penalty on every RSU link**, which permanently shifts the V2I/V2V balance for a
*modelling* reason rather than a physical one, and which no aggregate would show.

§10 of revision 1 disclosed the gap honestly — *"No scene here has RSUs"* — and then recommended the
term anyway. `n_rsus` defaults to 0, so it was latent; but `--rsus` is a shipped knob and the MA
report path uses RSUs.

**Refused rather than approximated.** `validate_config` now rejects `radio_antenna_pattern != "none"`
together with `n_rsus > 0`, with a message that says why, and the channel raises if it is ever
reached with validation bypassed. **A VRU is deliberately not refused**: Table 6.1.4-6 gives a
pedestrian UE an omnidirectional 0 dBi element, so a VRU link's asymmetry is the standard's own
answer, not a hole in ours.

### R11 — two conformance defects fixed, and the blocker on one of them was imaginary

`refdata/pathloss_3gpp_tr37885.json` recorded a 3.836 dB non-conformance and declined to fix it:

> "It is **NOT corrected here**: changing `V2X_ANTENNA_HEIGHT_M` moves both pinned dataset digests"

**That is false, and re-running both arms disproves it.** Both pinned arms use `radio_model="disc"`,
which never reads `StationSnapshot.ant_h_m`; only the opt-in geometric model does. With the constant
at 1.6 m the reference arm still hashes to `b25f2137cf14dd50…` and the default golden still hashes
to `0bd93655a2d5bebb…`. The defect was fixable at **zero pinned-digest cost** for as long as the
entry claimed otherwise.

**The defect.** `run.py` justified calling the fleet TR Type 2 on the grounds that it mounts
antennas at 1.5 m. But Table 6.1.4-1 reads *"UE antenna height — Vehicle UE: As defined in Subclause
6.1.2. **Pedestrian UE, cellular UE: 1.5 m**"*, and 6.1.2 gives Type 1 = 0.75 m, **Type 2 = 1.6 m**,
Type 3 = 3 m. 1.5 m is the standard's **pedestrian** row — and the RSU row (5 m) had been taken
correctly from the same table. The fleet was the one station class reading off the wrong line.

**And the trap waiting for whoever fixed it.** The branch rule was
`below = (tx_h < blocker_h) + (rx_h < blocker_h)`, so `below == 0` means `min(h) >= blocker`, while
the standard's Case 1 requires `min(h)` **strictly** greater. At equality the code returned 0 dB
where TR Case 3 says 5 dB. Unreachable at 1.5 m antennas — and under TR 37.885's own urban
Option A fleet (*"100 % vehicle type 2"*, antenna 1.6 m, body 1.6 m) **equality is the only NLOSv
geometry there is**. Fixing the height alone would have swapped a +3.836 dB error for a −5.2023 dB
one. Both were fixed together, in one function (`tr37885_nlosv_case`) that is now the only place the
branch is decided.

| | before | after |
|---|---|---|
| car blocker (1.6 m body) | Case 2, 9.0382 dB | **Case 3, 5.2023 dB** |
| truck blocker (3.0 m) | Case 2, 9.0382 dB | Case 2, 9.0382 dB (unchanged) |
| does blocker TYPE affect a V2V link? | **no, structurally** | **yes** |
| bias vs the spec at its own type-2 height | +3.836 dB | 0 |
| bias under urban Option A | −5.2023 dB *(latent)* | 0 |
| two-ray breakpoint d_b | 177.12 m | **201.53 m** |

**What was re-pinned, explicitly.** `tests/test_geometric_channel.py` §8 held four tests asserting
the *opposite* of what the model now does; they are rewritten and renamed, and each carries the name
of the test it replaces. Two of them now grade the engine's own rule against an independent
transcription of clause 6.2.1 and assert **agreement on every one of the 27 type triples**, where
they used to assert disagreement on 7. `test_the_breakpoint_sits_at_the_physical_distance` moves
177.1 → 201.5 m. **This is a legitimate re-pin of geometric-model behaviour and it is not a digest
re-pin: neither pinned dataset digest moves.**

### R12 — LOS/NLOSv is classified geometrically; the standard specifies it probabilistically

TR 37.885 Table 6.2-1 gives the LOS probability as a function of **distance alone** — urban
`P(LOS) = min{1, 1.05·exp(−0.0114·d)}`, with NLOSv taking the remainder — and clause 6.2.1 then
draws the blocker's height *statistically* from the fleet's type mix. This project resolves both
**geometrically**: it ray-tests the actual vehicles between the endpoints and uses the tallest one's
real height.

Measured over **4 261 310 classified link-steps** on the reference arm's own traffic, re-run under
`radio_model="geometric"` with the canyon fallback off so no NLOSb can absorb the difference:

| band | ours P(NLOSv) | TR 6.2-1 | spec / ours |
|---|---|---|---|
| 0–25 m | 0.2218 | 0.0895 | **0.40** |
| 25–50 m | 0.4582 | 0.3153 | 0.69 |
| 50–75 m | 0.4600 | 0.4851 | 1.05 |
| 75–100 m | 0.4359 | 0.6128 | 1.41 |
| 100–125 m | 0.2572 | 0.7088 | 2.76 |
| 150–175 m | 0.2173 | 0.8353 | 3.84 |
| 250–275 m | 0.2172 | 0.9473 | 4.36 |
| 475–500 m | 0.2820 | 0.9959 | 3.53 |
| **all** | **0.2527** | **0.8921** | **3.53×** |

**The deviation is not one-sided, which a single ratio hides.** Below ≈ 60 m we call *more* links
NLOSv than the standard's probability does — 2.5× as many at 0–25 m, where the standard says a 12 m
link is 91 % LOS while our own vehicles are 5 m long — and above ≈ 100 m we call **3.5–4.4× fewer**.

Recorded, not fixed. The geometric rule is arguably the better instrument: it responds to density,
to lane geometry and to a queue at a red light, none of which a distance-only probability can
express, and burst structure is exactly what the misbehaviour detectors read. But **an undocumented
divergence from the standard we claim to implement is a defect whatever its sign**, and anyone
reproducing a TR 37.885 evaluation-methodology result has to be told which half we kept.

### R13 — what was NOT fixed: a colluder frames victims it could not have heard

Found while closing R9, measured, and deliberately left alone. Victim selection picks any in-range
benign vehicle within `radio_range_m`, with no regard to whether the colluder could receive that
vehicle's CAMs — and it *needs* those CAMs, because the report is filed against the victim's active
**pseudonym digest**, which is only learnable from a decoded frame. The consequence is a
distributional signature the RSSI column inherits. Measured on a 90 s grid 6×6 collusion scene
(50 % colluders, `victim_pct` 0.15, `radio_range_m` 500, canyon NLOSb at 4/km, the antenna pattern
on): the 1028 framed links average **291.9 m** (p50 288.7, p90 459.7), while genuine reports come
from links that actually delivered — so **42.1 % of fabricated rows sit below −78 dBm against 4.8 %
of the 2612 honest ones**, and the two means differ by **9.3 dB with the budget exactly right**.

Not fixed here for two reasons, and the second is the binding one. It is a change to the **attack
model**, not to the channel — and gating victim selection on reception would change the collusion
report stream under *every* channel model, moving the third pinned golden
(`GOLDEN_COLLUSION_RSU_LOGDIST = 939b4faa…`, a `logdistance` run), which this work has no mandate to
re-pin. The right fix is to require that a colluder has decoded a CAM from its victim; it belongs
with whoever owns the attack model, and it should be done with the digest re-pin stated up front.

---

## 7. Cost

Cost is measured two ways, because neither on its own is trustworthy.

1. **Per link evaluation, inside one process.** All ten arms are timed sequentially in the same
   replay process over the identical 1.4–3.0 M link-steps, so the *ratio* is a clean comparison even
   though the absolute microseconds include this tool's own Python loop. Carries ~±5 % noise.
2. **Engine wall clock**, one full `mock_pipeline.run` per arm. This is the ms/step figure
   `FULL-CITY-SCENE.md` quotes (104 ms/step on the full city). It is the number that matters
   operationally and the noisier of the two, because the channel is only a fraction of a step.

### 7.1 Engine wall clock, one timed run per arm

Both sweeps are 300 steps. Another workload was running on the same 16-core host throughout, which
is part of why the full-city column is as noisy as it is.

| arm | full city: ms/step | ×`off` | reference arm: ms/step | ×`off` | replay µs/eval ×`off` (city / ref) |
|---|---|---|---|---|---|
| `off` | 106.2 | 1.000× | 233.5 | 1.000× | 1.00× / 1.00× |
| `nlosv_hold` | 116.9 | 1.101× | 233.5 | **1.000×** | 1.02× / 1.04× |
| `antenna` | 119.6 | 1.126× | 262.6 | **1.125×** | 1.06× / 1.16× |
| `nlosv_boban` | 99.7 | 0.939× | 232.7 | 0.997× | 0.95× / 1.03× |
| `breakpoint` | 95.8 | 0.902× | 231.1 | 0.990× | 1.00× / 1.02× |
| `blocker_width` | 100.0 | 0.942× | 234.2 | 1.003× | 1.02× / 1.00× |
| `jakes` | 128.7 | 1.212× | 375.4 | **1.608×** | 1.37× / 1.61× |
| `conformant` | 102.9 | 0.969× | 269.3 | 1.153× | 0.98× / 1.08× |
| `all_on` | 133.7 | 1.259× | 403.2 | **1.727×** | 1.34× / 1.85× |
| `antenna_eirp_fix` | 101.3 | 0.954× | 256.1 | 1.097× | 0.97× / 1.13× |

**Read the reference-arm column, not the full-city one.** The full-city column is noise-dominated:
three arms that strictly add arithmetic time *faster* than `off` (0.90–0.94×), and `conformant`
(three terms on at once) reads 0.969× while `nlosv_hold` alone reads 1.101×. Those cannot both be
true, so the resolvable band there is about ±10 %. The reason is structural: on the full city the
fleet is sparse (333 vehicles over 66 km², reach 700 m), so the channel is a small share of a step
and everything else — mobility, detectors, the MA, file I/O — dominates.

The reference arm resolves the same quantity cleanly. Its fleet is dense in a small grid with a
3000 m candidate window, so the channel *is* most of a step, and the wall clock and the replay's
per-link ratio then agree to within a few per cent — `jakes` 1.608× against 1.61×, `antenna` 1.125×
against 1.16×, `nlosv_hold` 1.000× against 1.04×. That agreement between two independent timers is
what licenses the following conclusions:

* **`nlosv_hold`, `nlosv_boban`, `breakpoint` and `blocker_width` are free.** Every one is inside
  ±1 % of baseline on the arm where the channel dominates.
* **`radio_antenna_pattern` costs 12.5 %.** It is the only term that adds per-link *geometry*
  (a bearing and a zenith angle per end, plus a two-panel maximum for Type 3).
* **`radio_fading_correlation` costs 61 %** — by far the most expensive term, and the only one whose
  cost is visible even in the noisy full-city column. It replaces one `gammavariate` with a Gaussian
  AR(1) step plus an inverse incomplete-gamma evaluation (`nakagami_power_from_normal`) on every
  packet.
* **All six together cost 73 %** on the reference arm. That is the number a "turn everything on"
  proposal has to earn, and §3–§5 say it does not.

Set against the project's own baseline: the full city runs at **106.2 ms/step** with everything off,
**116.9** with the recommended `nlosv_hold`, and **133.7** with all six. `FULL-CITY-SCENE.md`'s
104 ms/step figure is reproduced.

### 7.2 Where the cost actually goes

The reference arm costs 233.5 ms/step against the full city's 106.2 — the 66 km² city with real
buildings is **2.2× cheaper per step** than a 6×6 grid. That is the same finding
`FULL-CITY-SCENE.md` §"and it is cheaper, not dearer" reports, and this measurement is consistent
with it: cost is driven by the number of in-range pairs per step (593 dense vehicles behind a
3000 m candidate window here, 333 sparse ones behind a 700 m window there), not by map size or by
building count.

---

## 8. The anchor: how much weight can 0.90-at-200 m bear?

`refdata/v2x_awareness_conditions.json` records the anchor's provenance in full, and it is thin:

* it is **one cell of a nine-cell table** — `urban_v2v, Finland, 200 m` — from **three instrumented
  vehicles** on one shared route;
* the same table's within-environment dispersion is **4×** (highway V2V: 100 m Sweden, 400 m
  Finland), and the paper says outright that "qualitative separation of environments into urban,
  suburban, and highway cannot be generalized across test sites";
* it is a **crossing distance**, not a level at 200 m;
* its pair population is a convoy of instrumented vehicles — every unequipped car is invisible to
  the log — where ours is every co-present pair in a city;
* the receiver sensitivity of the measured arm is **not published at all**.

So the measured cell can bear an order of magnitude and a direction, and nothing finer. The
comparable quantity is the paper's **simulated** arm (all-pairs, GEMV², 2410 vehicles over Porto),
interpolated at **our** link budget — which is what `awareness.reference_nar90_distance_m` does and
what this report uses. That curve reads:

| link budget | reference NAR-0.90 range |
|---|---|
| 104 dB (23 dBm / −81 dBm, the shipped default — **6 dB under TR 37.885's own**, §6 R1) | **87.06 m** |
| 107 dB (`antenna_conducted_20dbm`, formerly `antenna_eirp_fix`: 20 dBm + 3 + 3) | 131.95 m |
| 110 dB (`antenna`, the **TR-conformant** budget: 23 dBm conducted + 3 + 3) | 200.00 m (a pinned point, not an interpolation) |

Log-linear interpolation of the three pinned Fig. 18 points — (100 dB, 50 m), (110 dB, 200 m),
(118 dB, 300 m) — all three arms sit inside the permitted 100–118 dB range, so nothing here is
extrapolated. The refdata's own note attaches at least **−17 %** of uncertainty to any distance
predicted from this curve, because the same section of the paper says 250 m at the 33 dBm ceiling
and 300 m at 23 dBm. Every ratio below therefore carries that band.

**Does the new physics move us toward it or away?** Graded honestly — each arm against the reference
curve *at that arm's own link budget*, because an arm that changes the budget must be compared at
the budget it actually runs — the answer is: **two terms move toward it, four leave it exactly where
it was, and none moves away.**

| arm (full city) | our NAR-0.90 range | its effective budget | reference at that budget | ratio | direction |
|---|---|---|---|---|---|
| `off` | 328.4 m | 104 dB | 87.06 m | **3.77×** | — |
| `antenna_conducted_20dbm` *(was `antenna_eirp_fix`)* | 359.2 m | 107 dB | 131.95 m | **2.72×** | **toward** |
| `antenna` (as implemented) | 389.4 m | 110 dB | 200.00 m | **1.95×** | toward — and ~~"at a budget 3 dB above the one configured"~~ is **withdrawn**: 110 dB *is* the budget TR 37.885 configures (§6 R1) |
| `nlosv_hold` | 327.9 m | 104 dB | 87.06 m | 3.77× | unchanged |
| `blocker_width` | 326.0 m | 104 dB | 87.06 m | 3.75× | unchanged |
| `jakes` | 328.4 m | 104 dB | 87.06 m | 3.77× | unchanged |
| `nlosv_boban` | 327.8 m | 104 dB | 87.06 m | 3.77× | unchanged |
| `breakpoint` | 263.0 m | 104 dB | 87.06 m | **3.02×** | toward |
| `all_on` | 320.7 m | 110 dB | 200.00 m | 1.60× | toward |

Two things follow, and they should be read together.

1. **The model is already far more optimistic in range than the reference curve, and the antenna
   term is the only one that closes the gap for a reason other than making the radio worse.** It
   closes it because the reference curve rises with link budget faster than our model does — which
   is itself a finding about the *shape* of our budget-to-range relation, not just its level.
2. **The project's two references point in opposite directions.** `CROSS-ENGINE-RADIO.md` grades us
   against MOSAIC's 0.7798 PDR at 200 m and finds us **1.29× pessimistic**; the Boban & d'Orey
   simulated curve at our budget finds us **3.77× optimistic in range**. Any statement of the form
   "this term makes the radio more realistic" has to say which reference it means. This document
   therefore reports movement, not improvement.

---

## 9. Recommendation on defaults

**Revised 2026-09-07. Two rows changed and one disappeared; the reasons are in §6 R1 and R2.**

| knob | recommend | why |
|---|---|---|
| `radio_nlosv_hold` | **ON**, on physics, with one caveat below | ~~It is what the standard says~~ — **it is not**, and that claim is withdrawn (finding C): clause 6.2.1 gives the blockage draw no temporal scope at all, and where the standard *is* explicit it disagrees with this implementation twice (a statistical blocker, and a hold for the whole link lifetime). What survives is the physics, and it is enough: a blockage is a large-scale effect, drawing it per packet demotes it to fast fading and double-counts against the Nakagami fade on the next line, it is **free** (1.000× of engine wall clock), it leaves every PDR row untouched, and it is the only term that acts directly on the burst structure misbehaviour detection consumes — +10–17 % of loss-run length across 100–250 m at 10 Hz. |
| `radio_blocker_width` | **ON, documented as inert** | Real conformance (clause 6.1.2), cost inside the noise, and measured effect indistinguishable from zero on two scenes. Switch it on so the conformance claim is true; never quote it as realism. **Caveat added in rev 2:** it was measured when blocker type could not affect a link at all; now that the antenna height is correct, widening a truck's corridor changes *which* TR case a link lands in as well as whether it is blocked, so "inert" is the row most in need of re-measurement. |
| `radio_antenna_pattern` | ~~NOT YET~~ → **ON, where the scene has no RSUs** | **The recommendation is inverted (finding E).** It was withheld pending an "EIRP double count"; TR 37.885 Table 6.1.1-1's 23 dBm is **conducted** and the element gain is separate (Table 6.1.4-8), so there is no double count — the pattern-on budget is the conformant one and **the shipped default runs 6 dB below the standard's**. Refused outright with `n_rsus > 0` until the RSU arrays are modelled (R10). |
| `radio_breakpoint` | **OFF** | Largest metric movement in the set (gray zone −159 m, loss runs +56 %) on the only magnitude in the set its own author declines to assert. Bracket it with a slope sweep before anyone considers it. Its onset has moved 177.1 → 201.5 m, so the measured movement is now an over-estimate. |
| ~~`radio_nlosv_model`~~ | **RETRACTED** | The knob is gone. Revision 1 kept it selectable on the grounds that "the measurement behind it is sound"; the level anchor was GEMV² output digitised from an IEEE figure, and against the one independent 5.9 GHz measurement in its own citation set it was **an order of magnitude further from the truth than the specification it replaced**. §6 R2. |
| `radio_fading_correlation` | **OFF** | **1.608× of engine wall clock** — the most expensive term measured, and the only one whose cost is visible even through the full city's ±10 % noise — for a 5.7 % change in one distance band at 10 Hz and nothing at all at 1 Hz. It cannot move PDR by construction. Keep it for a dedicated stop-line study; never pay for it in a general run. |

**The caveat on `radio_nlosv_hold`, stated because it cuts against the recommendation.** Making the
model burstier is *not* obviously an improvement. §4.1 measured our 10 Hz loss process at an
empirical Z of 1.2315 against Boban & d'Orey's fitted urban 5.4579 — we are already far burstier
than the only published measurement of this quantity — and the hold moves Z **further away**, to
1.2096. The case for switching it on is mechanism, not agreement with measurement — and after
finding C it is no longer conformance either, which weakens the case without removing it: a
large-scale term drawn per packet is a category error whichever way the resulting statistic moves.
The 4.4× burstiness gap is a separate and much larger problem that this term neither causes nor
fixes — its 0.0219 of Z is **0.5 % of the 4.2264 that separates us from the measured value**. The
likely candidate is the shadowing decorrelation distance, and note that revision 1's stated route to
re-pinning it is also withdrawn (finding H): a statistical element pattern does **not** make
Nilsson's 2–4 m the applicable comparison, so that avenue is still closed.

Net: the recommended default set is `radio_nlosv_hold=True, radio_blocker_width="tr37885",
radio_antenna_pattern="tr37885_opt1"` — one physics term and two conformance terms, the last of
which also closes a 6 dB link-budget gap and is refused wherever RSUs are present. Everything else
stays opt-in. **Switching the antenna pattern on is a digest-moving change for geometric runs and
should be taken as its own decision, with its own re-measurement.**

**The things worth doing next, in order.**

1. **Re-run the three-scene campaign.** Every measured figure in §3–§5 predates the antenna-height
   and Case-1 fixes, which changed the geometric model (staleness notice at the top). This is now
   the blocking item for every other recommendation here.
2. ~~Settle whether `radio_tx_power_dbm` is EIRP or conducted power~~ — **done** (R1). What remains
   is to decide whether to close the 6 dB gap by default, which is a digest-moving decision.
3. **Bracket the breakpoint** with a `radio_breakpoint_slope_db_per_decade` sweep (R3) so the
   confirmed part (the breakpoint exists, now at 201.5 m) can be separated from the unasserted part
   (how steep it is beyond it).
4. **Model the RSU antenna arrays** (Tables 6.1.4-1…-5) so the antenna pattern stops being refused
   on any scene with infrastructure (R10).
5. **Make a colluder frame only victims it has heard** (R13) — an attack-model fix that carries a
   pinned-golden re-pin, and the last known distributional signature on the fabricated column.
6. **Chase the Z gap** (§4.1). It is a factor of 4.4 on a statistic the reference actually published,
   which makes it a bigger realism target than any of the terms measured here.

---

## 10. What this does not show

* **Nothing here validates the channel against measured radio data.** The comparisons are against
  another simulation (MOSAIC/Java), against `awareness.py`'s analytic form of the same model, and
  against Boban & d'Orey's *simulated* curve at our budget. `ROADMAP-PERFECT.md` P7 is untouched.
* **The replay is propagation-only.** Congestion, hidden-terminal collision and weather compose on
  top of it in the engine and are excluded here, deliberately and for the same reason
  `awareness.py` excludes them.
* **Scene A has no real buildings.** Its NLOSb is a Poisson canyon expectation; the full-city scene
  is the one with geometry.
* **Z is compared across different pair populations.** §4.1 states the bound and its direction.
* **RSU links are out of scope, and that turned out to matter.** No scene here has RSUs, and the
  antenna term does not model TR 37.885's RSU arrays (Tables 6.1.4-1..-5). Revision 1 disclosed
  this and recommended the term anyway; revision 2 refuses the combination instead of counting it
  in `stats` (§6 R10). Recommending a term on the strength of scenes that cannot exercise its worst
  case is the process failure here, not the missing arrays.
* **The per-link-evaluation cost carries ~±5 % noise, and the full-city engine wall clock ~±10 %**;
  another workload was running on the same 16-core host throughout. The reference-arm wall clock is
  the resolvable one and it is the one the conclusions rest on (§7.1).
* **One run per arm, one seed.** Nothing was repeated, so small differences (everything under about
  ±0.002 in PDR, or one or two revocations in R8) should be read as unresolved rather than as zero.
* **The detection numbers in R8 are end-to-end, and end-to-end runs are not controlled.** The ten
  arms have ten different transmit schedules; the *mechanism* is established, the per-arm sizes are
  not.

---

## Appendix — reproducing this

```powershell
. C:\Users\Administrator\tools\env.ps1
$env:PYTHONPATH = 'src'

# what each term does to one link's budget, from the shipped functions
python tools/channel_physics.py terms

# the analytic instrument's blindness, measured rather than asserted
python tools/channel_physics.py blindness

# genuine vs FABRICATED rssi on the same true links, per arm and geometry (revision 2, section 6 R9)
python tools/channel_physics.py collusion --pairs 20000

# scene B: the full city (reproduces FULL-CITY-SCENE.md arm A, digest 1af4eeac...)
python -m scms_sim_ref.mock_pipeline.run --config <ds_A manifest> --out scene_city
python tools/channel_physics.py measure scene_city --arms all --json meas_city.json

# scene C: the same reference geometry at the standard's 10 Hz CAM rate
python tools/channel_physics.py measure scene_dt01 --arms all --json meas_dt01.json

# engine wall-clock cost and downstream consequence, one timed run per arm. The emission-trace
# digests it prints TEST whether the transmit schedule held; on this engine they show it does not
# (R8), which is why the physics is measured by `measure` over one fixed trace instead.
python tools/channel_physics.py cost --base-config <cfg> --out-root <dir> --arms all
```

The three scenes, the ten arms and every number above were produced on 2026-09-07 by
`tools/channel_physics.py`; the saved JSONs carry every per-band row this document summarises.
