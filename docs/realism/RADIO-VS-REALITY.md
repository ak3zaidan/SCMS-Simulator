# The radio versus reality — a per-effect confrontation

**Date:** 2026-09-07 · **Branch:** `feat/realism` · **Model under test:** `GeometricChannel`
(`radio_model="geometric"`), `src/scms_sim_ref/mock_pipeline/run.py`.

**Nothing in the channel model was changed by this work.** Both pinned digests are byte-identical
before and after — reference `b25f2137cf14dd504d56bb88cd67cce273b6a6ac348f7c59ee6d3b4372257815`,
default golden `0bd93655a2d5bebb4172191fab0940a5ff90c6be685cfa033f5edcfd7c1fb740`, each re-measured
on this tree by the suite. Every disagreement below is *recorded*, never tuned away.

> ### Revision 2 — 2026-09-07. Three load-bearing claims in revision 1 were withdrawn.
>
> An adversarial verifier read the primary sources behind revision 1 of this document and found
> that its arithmetic was exact and its **provenance was not**. Every number had been recomputed
> correctly; three of them were labelled as something they are not. Revision 2 withdraws those
> claims, keeps what survives, and states the refuting quote in each case. The withdrawals are
> collected in [§0](#0-what-revision-1-claimed-and-revision-2-withdraws) so that nobody who read
> the earlier version has to diff two documents to find out what changed.

> ### ⚠ SUPERSEDED IN PART, 2026-09-07 (later the same day) — four of the gaps below are CLOSED
>
> This document records the model **as it stood when it was written**, and its opening sentence
> ("Nothing in the channel model was changed by this work") is still true *of this work*. But a
> later workstream acted on several of its findings, so four sections now describe a model that no
> longer exists. See `docs/realism/CHANNEL-PHYSICS.md` revision 2 for the changes and their
> measurements; **both pinned digests are still byte-identical**, because everything here concerns
> the opt-in `radio_model="geometric"` path and both pinned arms run `disc`.
>
> | section | what it says | status |
> |---|---|---|
> | **§4a** — "Our antenna height is not a TR 37.885 value" | 1.5 m is the standard's *pedestrian* row; 3.836 dB of over-attenuation on car-blocked links | **FIXED.** `V2X_ANTENNA_HEIGHT_M` is now 1.6 m, the standard's Type 2. The refdata's stated blocker ("changing it moves both pinned digests") was **imaginary** and was disproved by re-running both arms. |
> | **§4b** — "Our Case-1 boundary test is off by the equality case" | `min(h) >= blocker` where the standard needs `>` | **FIXED**, in the same change and necessarily so: at 1.6 m antennas against 1.6 m car bodies the equality case becomes the commonest urban NLOSv geometry, so fixing §4a alone would have swapped +3.84 dB for −5.20 dB. |
> | **§5** — "row 5 is FALSIFIED" (NLOSv flat where measurement decays) | the specified term is falsified by two campaigns | **STILL TRUE, AND NOW LOAD-BEARING.** The opt-in arm that claimed to fix it (§7.7) has been **retracted**: graded against the Segata et al. numbers quoted in this very section it was +11 to +15 dB wrong where the specification is −0.96 / +4.04 dB. The specified term survives its own falsification as the best available option. |
> | **§7 G2 / §2** — the antenna term "unlocks" Nilsson's 2–4 m as the applicable comparison | stated in `run.py`'s block comment, not in this document | **WITHDRAWN in `run.py`.** This document's §2 was right all along: a *statistical element* pattern that is a flat +3 dBi for 70 % of links removes no per-link gain variation, so it does not move a model of this class out of Nilsson's "otherwise" branch. |
> | **§7.7** — a provenance defect in the new opt-in arm | recorded, "not fixed here, because that code is in flight" | **ACTED ON.** `radio_nlosv_model` and every `BOBAN_*` constant are deleted. §8's claim that "nothing under a licence-unclear or non-CC licence is committed anywhere in the tree" was false while they existed and is **true again now**. |
> | **§7 G1 / scorecard row 10** — TR 37.885 6.2.1 mandates "**one draw per blocked link**" | the per-packet redraw is called a conformance defect, "no modelling judgement required" | **OVER-CLAIMED; WITHDRAWN AS A CONFORMANCE READING.** Clause 6.2.1 read in full states **no temporal scope for the blockage draw at all** — the "one draw per blocked link" reading entered from the task brief that commissioned the term, not from the document. Where the clause *is* explicit it cuts the other way twice: its blocker is drawn **statistically** from the fleet's type mix, not identified geometrically, and its own baseline ("state is **not updated** between LOS and NLOSv") holds the state for the whole link lifetime — a **stronger** hold than the one implemented. G1's three physics consequences (category error, shortened burst runs, double count against the Nakagami fade) all stand, and `radio_nlosv_hold` keeps its rank **on them**; its label is now *physics-motivated, not specified* (`run.py` GAP 2 comment; CHANNEL-PHYSICS.md finding C). Row 10's "our defect — active today" therefore over-states: the default is **unspecified**, not non-conformant. |
> | **§7 G3 / scorecard row 9** — "d_b = **177.1 m** at our own geometry" | the two-ray breakpoint at the antenna heights the model then carried | **STALE NUMBER after the §4a fix.** d_b = 4·h_TX·h_RX/λ is quadratic in antenna height, so correcting 1.5 m → 1.6 m moves it to **201.5 m** (+13.8 %). Still inside the operating range, so G3's argument survives with the onset 24 m later; the Abbas-fitted 104 m column is a measured value and is unaffected. |

## Method, and the four rules it is written under

1. **An honest failure is a real result.** Where the model agrees with a measurement, this document
   says so as plainly as where it fails. It now contains one agreement against a measurement and
   two outright falsifications against measurements.
2. **Nothing is done until re-measured.** Every arithmetic claim here was recomputed against the
   constants *imported from* `run.py`, not against a second transcription of them.
3. **A source's provenance is part of its number.** Revision 1 failed this rule, which is why
   revision 2 exists. Every reference value below now carries an explicit **provenance** —
   `measured`, `model output`, or `specification` — and no row mixes them silently. A simulator's
   output is never entered in a column called "measured", however well that simulator is validated.
4. **Licence discipline.** [§8](#8-licence-status-of-every-number-in-this-document) states the
   licence of every number and how it was established. Values from CC BY sources may be pinned in
   `refdata/`; values under publisher copyright or under the non-CC arXiv licence are quoted here
   with attribution and are **not** committed as refdata. No number anywhere was digitised out of a
   figure — every measured value quoted is a sentence or a table cell in its source.

The findings below are pinned as tests in `tests/test_geometric_channel.py` § 8, so that a later
change to the model or to the recorded gap breaks the build rather than silently erasing a known
limitation.

---

## 0. What revision 1 claimed and revision 2 withdraws

| # | Revision 1 claimed | Status | Refuted by |
|---|---|---|---|
| W1 | "TR 37.885 predicts 5.20 dB against GEMV²'s **measured** 5.0 dB — agreement to **0.20 dB**", presented as the main result of §1 | **WITHDRAWN** | The figure's own caption: *"Received power distribution **as generated by GEMV²**…"* The 5/13/20 dB triple is a **simulator's output**, not a measurement. §1 |
| W2 | Decorrelation is "the sharpest falsification"; "our pin is wrong"; urban error "2.2–5.9× too long" | **WITHDRAWN** | The source's very next sentence: *"**Otherwise**, the de-correlation distance **has to be much longer**."* Our model is in that "otherwise" class. The sharp end of the range (5.9×) came from antenna-pattern-corrected numbers that do not apply to us. §2 |
| W3 | Row 5, blockage-versus-distance: "**Unresolved** — could not verify" | **WITHDRAWN, and the verdict is worse than unresolved** | Two measured sources state the distance dependence **in text**, both openly reachable and both text-extractable. The model is **falsified** there, and our error changes sign inside the band our scenarios occupy. §5 |
| W4 | Abbas et al. are "5.9 GHz V2V measurements" | **CORRECTED** | *"…centered around a **carrier frequency of 5.6 GHz**"*, and *"λ = 0.0536 m at 5.6 GHz"*. §8 |
| W5 | The Abbas licence is "PARTIALLY VERIFIED… could not be retrieved" — and the numbers were committed anyway | **RESOLVED** | CrossRef publisher-deposited metadata for `10.1155/2015/190607` returns `creativecommons.org/licenses/by/3.0/`. **CC BY 3.0.** Retrieved and parsed first-hand. §8 |
| W6 | The −2.539 dB Case-1 bias is weighted by "**the** standard's own urban type mix" | **CORRECTED** | TR 37.885 6.1.2 offers urban-grid **Option A** (100 % type 2) *and* **Option B** (20/60/20). Revision 1 used Option B, which **halves** the reported bias. Both are now reported. §4b |
| W7 | Vehicle types are defined in "clause 5" | **CORRECTED** | They are in **clause 6.1.2, "UE drop and mobility modeling"**. The quoted text itself was verbatim-exact. §4a |

**One verifier finding did not survive its own check, and is recorded because it nearly caused a
correct citation to be deleted.** The verifier reported that arXiv:1305.0124 "DOES NOT CONTAIN THE
QUOTE — that PDF is a 23-page document with ZERO occurrences of 'truck'". That is true of **v1 and
v2** (both 23 pages, both zero hits, verified). It is false of **v3** (22 Apr 2014, 18 pages), which
is the version matching the published IEEE TVT paper and which contains the quoted sentence and
Fig. 12 exactly as cited. The citation was right and the *version pin* was missing; every reference
to that paper in this document and in `refdata/` now says **v3**. This is the reason rule 2 exists
in both directions: a refutation is a claim too, and it gets checked.

---

## Scorecard

The **Provenance** column is the change revision 1 needed most. `measured` means a number a person
observed with an instrument; `model output` means a number a simulator produced; `specification`
means a number a standards body wrote down.

| # | Effect | Our model | Reference | Provenance | Verdict |
|---|---|---|---|---|---|
| 1a | NLOSv extra loss, **car** blocker @100 m | 9.04 dB | ≈5 dB | **model output** (GEMV²) | Model-vs-model, **+4.04 dB**. *Not* a measurement failure |
| 1b | NLOSv extra loss, **truck** blocker @100 m | 9.04 dB | ≈20 dB | **model output** (GEMV²) | Model-vs-model, **−10.96 dB**. TR 37.885's Case-2 cap |
| 1c | Blocker-type dependence of the **mean** | **none** on any V2V link | 5 / 13 / 20 dB by type | model output, corroborated by **measurement** | **Falsified** on structure |
| 1d | NLOSv mean vs a real measurement | 9.04 dB | "about 10 dB" | **measured** (Abbas, CC BY 3.0) | **Agrees to 0.96 dB.** The one clean agreement |
| 2a | Shadowing decorrelation, **urban** | 10 / 13 m | 4.25 / 4.5 m | **measured** (Abbas) | **2.35–2.89× too long** — but see the confound in §2 |
| 2b | Shadowing decorrelation, **highway** | 10 / 13 m | 23.3 / 32.5 m | **measured** (Abbas) | **2.33–2.50× too short**. Error changes sign |
| 2c | Decorrelation carries an **environment** term | no term at all | urban→highway ratio 5.48× (LOS) / 7.22× (OLOS) | **measured**, one campaign, one processing | **Structurally falsified** |
| 3 | Shadow-fading σ on a blocked link | 3.0 dB (NLOSv) | 4.28–7.38 dB | **measured** (Abbas, Nilsson) | **Under-dispersed** |
| 4a | Antenna height | 1.5 m, all vehicles | 0.75 / 1.6 / 3.0 m | **specification** (6.1.2) | **Our defect** — non-conformance |
| 4b | Case-1 boundary test | `min(h) ≥ blk` | `min(h) > blk` | **specification** (6.2.1) | **Our defect** (latent) |
| 5 | NLOSv loss **vs distance**, 10–200 m | **flat** 9.04 dB | 20 dB@10 m → 7 dB@100 m; 10 dB@80 m → 5 dB@120 m | **measured**, two campaigns, both in text | **FALSIFIED. Our error changes sign** |
| 6 | Urban NLOS at a corner | f(d3D) alone | f(d_t, d_r) separately | published model | **Cannot represent** |
| 7 | Multiple blockers on one link | tallest only | additive per blocker | model output (GEMV²) | **Cannot represent** |
| 8 | **Antenna gain / pattern** | **no term by default** | per-vehicle-type directional pattern | **specification** (6.1.4) | **Missing term — the largest omission** |
| 9 | Two-ray breakpoint | none by default; single slope | d_b = 177.1 m at our own geometry | **measured** (Abbas) | **Missing term** |
| 10 | NLOSv blockage draw | **per packet** by default | one draw per blocked link | **specification** (6.2.1) | **Our defect — active today** |
| 11 | Small-scale fade correlation | i.i.d. per packet, always | coherence time 215 ms at 0.1 m/s | derived | **Optimistic at a stop line** |
| 12 | Blockage **variance** vs blocker size | fixed σ, every arm | "the bigger the obstacle, the higher the variance" | **measured** (Segata) | **Falsified; blocked on evidence** |
| 13 | Blocker footprint vs vehicle type | fixed 1.0 m half-width by default | length 5 / 5 / 13 m, width 2.0 / 2.0 / 2.6 m | **specification** (6.1.2) | **Our defect** |

Rows 8–13 are new in revision 2. They are the effects for which our model has **no term at all** —
the class of gap that a per-constant audit structurally cannot find, because there is no constant to
audit. They are ranked as work items in [§7](#7-the-ranked-gap-list).

**Rows 8–13 describe the DEFAULT configuration**, which is the one both pinned digests measure and
the one every gate grades. While revision 2 was being written, a concurrent workstream landed
**opt-in arms** for five of the six in the same working tree — `radio_nlosv_hold`,
`radio_antenna_pattern`, `radio_nlosv_model`, `radio_blocker_width`, `radio_breakpoint`, every one
defaulting to the historic behaviour. [§7](#7-the-ranked-gap-list) names the knob beside each gap,
and [§7.7](#77-a-provenance-defect-in-the-new-opt-in-arm) records one provenance defect those arms
inherited from revision 1 of this document.

---

## 1. The NLOSv blockage magnitude

### What the code actually does — verified, not assumed

`_link_state` (run.py ~line 916) selects the branch as

```python
below = (tx_h < blocker_h) + (rx_h < blocker_h)
mu_base, sig_v = TR37885_NLOSV[("both_above", "one_below", "both_below")[below]]
```

with `TR37885_NLOSV = {"both_below": (9.0, 4.5), "one_below": (5.0, 4.0), "both_above": (0.0, 0.0)}`
and `tr37885_nlosv_mu_db(base, d) = base + max(0, 15*log10(d) - 41)`. `evaluate_raw` then adds
`max(0.0, prng.gauss(mu, sigma))` — a censored Gaussian, so the **realised** mean is slightly above
`mu`: E[max(0, N(9.0, 4.5))] = **9.038208 dB**, and E[max(0, N(5.0, 4.0))] = **5.202347 dB**.

Two claims about this code were checked and both hold:

- **The distance term is inert across our whole operating range.** `max(0, 15·log₁₀d − 41)` is zero
  below 10^(41/15) = **541.1695 m**. The realised mean takes exactly **one** distinct value over
  1 m – 541 m: 9.0382 dB. It is flat, then increases (13.0025 dB at 1000 m).
- **The mean carries no blocker-type dependence on any V2V link.** With
  `V2X_ANTENNA_HEIGHT_M = 1.5` and `TR37885_BLOCKER_HEIGHT_M = {car: 1.6, motorcycle: 1.6,
  truck: 3.0, bus: 3.0}`, every blocker is taller than both antennas, so *every* V2V pair resolves
  to `both_below`. A motorcycle and an articulated truck attenuate identically, to the bit.

### The 5 / 13 / 20 dB triple is GEMV²'s output, not a measurement

Revision 1 put this triple in a column headed "Measured" and built its headline on it. Here is the
sentence it comes from, and then the caption of the figure that sentence points at:

> "To illustrate the impact of different types of vehicular obstructions, Fig. 12 shows the received
> power when the LOS is blocked by a car, a van, and a truck. For a mean distance of 100 meters
> between transmitter and receiver, different obstructing vehicle types attenuate the power in a
> distinct fashion, with the mean attenuation compared to LOS of approximately 5, 13, and 20 dB for
> car, van, and truck, respectively. […] **These results, which are in line with previous
> measurements reported in [3], [6], [36]**, demonstrate the ability of GEMV² to […]"

> "Fig. 12. Received power distribution **as generated by GEMV²** for LOS links and NLOSv links due
> to three vehicle types: passenger car (mean height: 1.5 meters), commercial van (mean height:
> 2 meters), and truck (mean height: 3 meters); standard deviation for the height of each vehicle
> type was set to 0.15 meters. For each link, a single vehicle of a given type is placed between
> transmitter and receiver, **both of which are passenger cars with the height of 1.5 meters**.
> Distance between transmitter and receiver is uniformly distributed between 75 and 125 meters.
> Transmit power is set to 10 dBm and gains at both transmit and receive antenna are 1 dBi."
>
> — M. Boban, J. Barros and O. K. Tonguz, *Geometry-Based Vehicle-to-Vehicle Channel Modeling for
> Large-Scale Simulation*, IEEE Trans. Veh. Technol. 63(9):4146–4164, 2014; preprint
> **arXiv:1305.0124v3** (18 pp.). Quoted with attribution; **not** committed to refdata.

"In line with previous measurements" is a statement of **consistency with**, not **derivation
from**. [3], [6] and [36] are Boban et al. JSAC 2011, Meireles et al. VNC 2010, and Abbas et al.
arXiv:1203.3370 — the paper is saying its simulator lands near those campaigns, not that these three
numbers came out of them.

**Three separate things were wrong with the withdrawn headline, and each alone would sink it:**

1. **It is model-vs-model.** "TR 37.885 predicts 5.20 dB vs GEMV²'s measured 5.0 dB" compares a
   specification against a simulator and calls the result agreement with reality.
2. **"Agreement to 0.20 dB" is false precision in both digits.** The source says *approximately* 5.
   And the 0.20 dB is entirely an artefact of applying the censoring correction to one side only:
   E[max(0, N(5.0, 4.0))] = 5.2023 against a number the source rounded to one significant figure.
   Two rounding conventions were subtracted and the difference was reported as a physical result.
3. **The geometries are not comparable, which the caption says outright.** In GEMV²'s figure the
   transmitter and receiver are **passenger cars 1.5 m tall** and the car blocker has **mean height
   1.5 m, σ 0.15 m** — a blocker at the same height as the antennas, cutting the line of sight only
   about half the time. TR 37.885's Case 3 at type-2 heights is a **1.6 m antenna against a 1.6 m
   body**. Those are different physical situations that happen to produce nearby numbers. Our own
   model, at 1.5 m antennas against a 1.6 m car body, differs from GEMV²'s car case by **10 cm of
   assumed car height** — the +4.04 dB "error" is mostly that 10 cm crossing a branch boundary, not
   evidence about reality.

**What survives from revision 1 in this section:** the observation that transmit power and antenna
gains **cancel**, because the quantity compared is an NLOSv-minus-LOS *difference* within one
experiment. That was correct and it is what makes the comparison arithmetically legitimate. Only the
provenance label failed.

| Blocker | GEMV² *(model output)* | TR 37.885 at its own type-2 heights *(specification)* | Ours | ours − GEMV² |
|---|---|---|---|---|
| car | ≈5.0 dB | 5.2023 dB (Case 3) | 9.0382 dB | +4.04 |
| van | ≈13.0 dB | *(no van type)* | 9.0382 dB | −3.96 |
| truck | ≈20.0 dB | 9.0382 dB (Case 2) | 9.0382 dB | −10.96 |

The **structural** reading of that table is what matters and it does not depend on provenance at
all: GEMV² spreads 5 → 13 → 20 dB across blocker types, while TR 37.885 — and therefore we — put
van and truck in the same Case 2 and cap both at 9 dB. **The standard cannot express blocker-size
dependence above the antenna line**, and neither can we. That is row 1c.

### The one clean agreement, against an actual measurement

> "It is observed that vehicles obstructing the LOS induce an additional average attenuation of
> **about 10 dB** in the received signal power."
>
> — T. Abbas, K. Sjöberg, J. Karedal and F. Tufvesson, *A Measurement Based Shadow Fading Model for
> Vehicle-to-Vehicle Network Simulations*, Int. J. Antennas Propag. 2015, Art. 190607,
> doi:10.1155/2015/190607. **Measured**, RUSK-LUND channel sounder, 200 MHz bandwidth centred on a
> **5.6 GHz** carrier. **CC BY 3.0** (§8).

Our 9.0382 dB agrees with this to **0.96 dB**, well inside the 6.12–6.67 dB per-link scatter the
same paper reports. Taken as a single blocker-type-agnostic average, our NLOSv mean is defensible
against measurement. It is the *spread across blocker types* around that average — and, as §5 now
shows, its *behaviour with distance* — that we do not reproduce.

### Is the type-independence rescuable by scatter? No, and a measurement says so

> "different vehicle types not only affect the average received power, but also its distribution,
> suggesting that the attenuation characteristics of the simulation model need to be tailored to the
> type of vehicle that obstructs the communication path."
>
> — M. Segata, B. Bloessl, S. Joerer, C. Sommer, R. Lo Cigno and F. Dressler, *Short Paper: Vehicle
> Shadowing Distribution Depends on Vehicle Type: Results of an Experimental Study*, IEEE VNC 2013,
> pp. 242–245. **Measured**: A12 freeway near Innsbruck, two Cohda MK2 802.11p radios, 5.89 GHz,
> 10 MHz, BPSK R=½, 20 dBm, rooftop Mobile Mark ECOM9-5500 9 dBi omnis. IEEE copyright; quoted with
> attribution, **not** committed to refdata.

Row 1c is falsified on structure, not on magnitude, and no scatter argument reaches it.

---

## 2. Shadowing decorrelation distance — the withdrawal, and what survives it

We pin `shadowing_decorrelation_distance_m = [7.0, 13.0]` as the Phase-2 gate
(`refdata/v2x_awareness.json`) and drive the AR(1) process with
`TR37885_SHADOW_DECORR_M = {LOS: 10.0, NLOSv: 13.0, NLOSb: 13.0}`.

### W2: the "falsification" was refuted by the source's own next sentence

Revision 1 called this "the sharpest falsification", wrote "our pin is wrong", and marked rows 2a
and 2b **Falsified**. It quoted Nilsson et al.'s conditional clause and then drew the conclusion
that clause forbids. Here is the whole sentence pair, verbatim from the abstract:

> "**In cases where a proper model for the path loss and the antenna pattern is included**, the
> de-correlation distance for the auto-correlation is as low as 2–4 m, and the cross-correlation for
> the large scale fading between different links can be neglected. **Otherwise, the de-correlation
> distance has to be much longer** and the cross-correlation between the different communication
> links needs to be considered separately, causing the computational complexity to be unnecessarily
> large."
>
> — M. G. Nilsson, C. Gustafson, T. Abbas and F. Tufvesson, *A Path Loss and Shadowing Model for
> Multilink Vehicle-to-Vehicle Channels in Urban Intersections*, **Sensors 18(12):4433, 2018**,
> doi:10.3390/s18124433. **CC BY 4.0**, article's own statement, read first-hand.

**Our `GeometricChannel` has no antenna-pattern term of any kind** — §7 gap G2 establishes this
against the source. So the model is squarely in the source's "otherwise" class, and the source
therefore **prescribes a longer decorrelation distance for a model of our class**. Read correctly,
Nilsson et al. **defend** the 10/13 m pin rather than falsify it.

The paper is even more specific about the mechanism, and it names the exact term we lack:

> "The mean of Ψσ has a Gaussian distribution that represents the differences in the particular
> traffic situation and **the gain of the involved antennas** for the particular communication link."

The 2.2 m and 3.7 m medians are the decorrelation of the **zero-mean, offset-subtracted** process —
the paper's Figure 10 shows the two curves side by side, before and after that subtraction, and only
the subtracted one decays in 2–3 m. That subtraction is where the antenna gains go. Our AR(1)
shadowing is a zero-mean Gaussian with **no per-link offset and no antenna pattern**, so the
comparable measured quantity is the *un*-subtracted curve, which the paper says is much longer.
Revision 1 compared our process against the wrong one of two curves the source plots together.

**Rows 2a and 2b are downgraded from "Falsified" to a quantified disagreement carrying a named
confound.** The disagreement is real and is still recorded; it is not a falsification, because the
source supplies a mechanism that predicts exactly this direction of difference for a model with no
antenna pattern.

### What survives, and is the actual finding: the environment ratio

Abbas et al. measure ordinary urban streets and highway **in one campaign under identical
processing** — the same sounder, the same Gudmundson 1/e definition of `d_c` — so the *ratio*
between their environments is free of every processing confound that sinks the absolute comparison:

| Environment | State | Measured d_c | Ours | Ratio ours / measured |
|---|---|---|---|---|
| urban | LOS | 4.25 m | 10 m | **2.35× too long** |
| urban | OLOS | 4.5 m | 13 m | **2.89× too long** |
| highway | LOS | 23.3 m | 10 m | **0.43× — 2.33× too short** |
| highway | OLOS | 32.5 m | 13 m | **0.40× — 2.50× too short** |

— *Table III, "Decorrelation distances d_c for highway and urban scenarios", Abbas et al., IJAP
2015 / arXiv:1203.3370. Measured. CC BY 3.0 (§8).*

**Highway/urban = 23.3/4.25 = 5.48× (LOS) and 32.5/4.5 = 7.22× (OLOS).** Whatever a processing
choice does to the absolute values, it cannot manufacture a 5–7× spread between two environments
measured and processed the same way. **A single constant cannot express that ratio**, and our error
changes sign between the two environments.

**The defect is structural, and it is visible in our own source.** `TR37885_PATHLOSS` *is* keyed on
urban vs highway; `TR37885_SHADOW_DECORR_M` is keyed on **link state only** and has no environment
term whatsoever. That — row 2c — is the finding, and it is pinned by
`test_shadowing_decorrelation_carries_no_environment_dependence`.

**The defensible urban statement is "2.35–2.89× longer than Abbas's urban rows".** Revision 1's
"2.2–5.9×" is withdrawn: its sharp end came from Nilsson's antenna-pattern-corrected intersection
medians (2.2 m), which by the source's own conditional do not describe a model like ours.

**The pin was not changed.** `[7.0, 13.0]` still stands as the Phase-2 gate. Changing a gate to
match data is the failure mode the house rules exist to prevent — and after W2 there is no longer
even a claim that the data falsifies it.

---

## 3. Shadow-fading standard deviation — and a correction to our own record

TR 37.885 assigns σ = 3.0 dB to LOS *and to NLOSv*, and 4.0 dB to NLOS. Against measurement:

| Source | Condition | Measured σ | Ours (NLOSv 3.0) |
|---|---|---|---|
| Abbas et al. (Table II) | urban OLOS | 6.67 dB | 0.45× |
| Abbas et al. (Table II) | highway OLOS | 6.12 dB | 0.49× |
| Nilsson et al. (Table 3) | intersection, min of 10 | 4.28 dB | 0.70× |
| Nilsson et al. (Table 3) | intersection, max of 10 | 7.38 dB | 0.41× |

Our blocked-link shadowing is **under-dispersed by roughly a factor of two** against every measured
value found. The refdata note defending σ = 3.0 for NLOSv ("its blockage spread is a separate random
variable") is a real argument — the NLOSv draw adds its own 4.5 dB — but even combining the two in
quadrature gives √(3.0² + 4.5²) = 5.41 dB, still below the 6.12–6.67 dB measured on comparable
links. *(And §7 gap G1 shows that in our implementation those two variances are not even composed
the way the argument assumes: the 4.5 dB term is redrawn every packet.)*

**A defect in our own record, found while checking this.** Our
`intersection_measured_shadowing_sigma_db` pinned the band **[4.39, 5.57] dB** and described it as
"4.39 dB (car-obstructed, car-to-car) up to 5.57 dB (truck-obstructed)". Nilsson et al.'s Table 3
actually spans **4.28 dB** (Xerxes car-to-car) to **7.38 dB** (Yngve truck-car), and lists 4.39 dB
as *car-to-car*, not "car-obstructed". Our band was a subset of one intersection's column, and its
effect was flattering: it let the note claim TR 37.885's 4.0 dB was "0.4–1.6 dB optimistic" when the
true span is **0.28–3.38 dB optimistic**, against *every one* of the ten fitted values. Corrected to
the full table on 2026-09-07; no model input changed.

---

## 4. Two defects in our own implementation of the standard

Both were found by reading TR 37.885 V15.3.0 directly rather than the project's summaries of it.
Both are recorded in `refdata/pathloss_3gpp_tr37885.json` and pinned as tests. **Neither is fixed**,
because either fix moves both pinned digests.

### 4a. Our antenna height is not a TR 37.885 value

The standard defines three vehicle types in **clause 6.1.2, "UE drop and mobility modeling"**
(revision 1 said "clause 5"; the quoted text below was and is verbatim-exact):

- *"Type 1 (passenger vehicle with lower antenna position): length 5 meters, width 2.0 meters, height 1.6 meters, antenna height 0.75 meters"*
- *"Type 2 (passenger vehicle with higher antenna position): length 5 meters, width 2.0 meters, height 1.6 meters, antenna height 1.6 meters"*
- *"Type 3 (truck/bus): length 13 meters, width 2.6 meters, height 3 meters, antenna height 3 meters"*

We take the **blocker** height from this table (1.6 / 3.0 m) but give **every** vehicle a single
`V2X_ANTENNA_HEIGHT_M = 1.5 m`. That value does appear in TR 37.885 — in Table 6.1.4-1, as the
antenna height of a **pedestrian or cellular UE**, against *"Vehicle UE: As defined in
Subclause 6.1.2"*. We took the RSU height from the same table correctly (`RSU_ANTENNA_HEIGHT_M =
5.0`, matching *"UE-type-RSU: 5 m"*) and gave our vehicles the pedestrian row.

The consequence: 1.5 m sits below *both* blocker heights, which collapses the standard's three-case
rule to one case for all V2V traffic (§1). Bias against the standard evaluated at its own type-2
height: **+3.8359 dB** for a car or motorcycle blocker; exactly zero for a truck or bus.

### 4b. Our Case-1 boundary test is off by the equality case

TR 37.885 **clause 6.2.1**, verbatim:

> - The blocker height is the vehicle height which is randomly selected out of the three vehicle types according to the portion of the vehicle types in the simulated scenario.
> - **The additional blockage loss is max {0 dB, a log-normal random variable}.**
> - Case 1: Minimum antenna height value of TX and RX **>** Blocker height → No additional blockage loss
> - Case 2: Maximum antenna height value of TX and RX **<** Blocker height → Mean: 9 + max(0, 15·log₁₀(d)−41) dB, standard deviation: 4.5 dB
> - Case 3: Otherwise → Mean: 5 dB + max(0, 15·log₁₀(d)−41), standard deviation: 4 dB

Our `below = (tx_h < blk) + (rx_h < blk)` maps `below == 0` to zero loss. But `below == 0` means
`min(h) ≥ blk`, whereas Case 1 requires `min(h)` **strictly** greater. The equality input belongs to
Case 3 (5 dB), and we return 0 dB. Enumerating all 27 ordered (tx, rx, blocker) triples over the
standard's own three types, the two rules disagree on **7**, always in that direction.

**W6 — the weighting, corrected.** Revision 1 weighted those 27 triples by "**the** standard's own
urban type mix (20/60/20)" and reported **−2.5387 dB**. TR 37.885 6.1.2 defines **two** urban-grid
dropping options, and the definite article hid the flattering choice. Recomputed over every option
the standard defines, against the live constants:

| TR 37.885 6.1.2 dropping option | Type mix | Spec rule | Our rule | Our bias |
|---|---|---|---|---|
| **urban grid Option A** | 100 % type 2 | 5.2023 dB | 0.0000 dB | **−5.2023 dB** |
| **urban grid Option B** | 20 / 60 / 20 | 5.6496 dB | 3.1109 dB | **−2.5387 dB** |
| highway Option A | 100 % type 2 | 5.2023 dB | 0.0000 dB | −5.2023 dB |
| highway Option B | 20 / 60 / 20 | 5.6496 dB | 3.1109 dB | −2.5387 dB |
| highway Option C | 0 / 67 / 33 | 5.3910 dB | 2.0981 dB | **−3.2930 dB** |

Under urban Option A — *"Vehicle type distribution: 100% vehicle type 2"* — every vehicle has a
1.6 m antenna and every car blocker a 1.6 m body, so **every single NLOSv link is the equality
case**: the standard charges 5.2023 dB and we charge nothing. **The reported bias doubles.** Option
B was the flattering half of a choice revision 1 did not disclose was a choice.

**This is latent today and becomes live the moment 4a is fixed** — and it bites hardest there,
because the standard's type-2 antenna height (1.6 m) *equals* the car body height (1.6 m). Under
Option A that boundary is not merely the most common urban NLOSv geometry; it is the only one.
Correcting the antenna heights without also correcting this comparison would trade a +3.84 dB error
for a −5.20 dB one. That is why the two are pinned together.

---

## 5. Blockage versus distance — revision 1 said "unresolved"; the model is falsified

Our NLOSv mean is **flat at 9.0382 dB from 1 m to 541 m**, then rises. Revision 1 reported the
distance dependence as unverifiable: *"The only openly reachable copy of the paper is an image-only
PDF that no text extractor available here could read."* That was a failure of retrieval, not of the
literature. **Two measured sources state the distance dependence in running text**, both openly
reachable, both text-extractable, and neither requiring a figure to be read.

### Source 1 — Boban's thesis, §2.3.1. Apples-to-apples with our `both_below` branch

> "Blocking the LOS has clear negative effects on the RSSI. […] **At 10 m, the van reduced the RSSI
> by approximately 20 dB in both cases. As the distance between communicating nodes increased, the
> effect of the van was gradually reduced. At 100 m, the RSSI in the NLOSv case was approximately 5
> and 7 dB below the LOS case for 802.11b/g and 802.11p, respectively.**"

> "The truck had a large impact on RSSI, with a loss of approximately **27 dB at the smallest
> recorded distance of 26 m** (the length of the truck) when compared with the LOS case. […] **The
> RSSI drop caused by the truck decreased as the cars move further away from it**, an indication
> that the angle of the antennas' field of view that gets blocked makes a difference."
>
> — M. Boban, *Realistic and Efficient Channel Modeling for Vehicular Networks*, PhD thesis,
> **arXiv:1405.1008**, §2.3.1. **Measured.** arXiv non-exclusive licence — **not CC**; quoted with
> attribution, **not** committed to refdata (§8).

**Why this is comparable to our model, condition by condition** — all from the thesis's own Table
2.1 and §2.2.1, so none of it is inferred:

| Condition | Boban §2.3.1 | Ours | Comparable? |
|---|---|---|---|
| Carrier | 802.11p, centre frequency **5900 MHz**, 20 MHz, 6 Mbps | 5.9 GHz | ✅ |
| Antennas | 26 cm omnis, **roof-mounted centrally**, 5 dBi both ends | 1.5 m roof, gain absent | ✅ *(gains cancel in a difference)* |
| Geometry | *"the van sits around **37 cm taller than the tip of the antennas** on the sedans"* | blocker above both antennas | ✅ **exactly our `both_below` branch** |
| Quantity | NLOSv **minus** LOS in the same experiment | NLOSv extra loss over LOS | ✅ path loss and gains cancel |
| Scatter | *"The standard deviation was under 1 dB and the 95 % confidence intervals were too small to represent"* | — | ✅ **not absorbable by scatter** |

| Distance | Measured (802.11p) | Ours | Our error |
|---|---|---|---|
| 10 m | ≈20 dB | 9.0382 dB | **−10.9618 dB** |
| 100 m | ≈7 dB | 9.0382 dB | **+2.0382 dB** |

### Source 2 — Segata et al., VNC 2013, independently, with a truck

Revision 1 said these two numbers "could not be verified" and "appear to be figure readings". Both
statements are wrong. They are in the body text, and the paper is text-extractable from the openly
hosted TU Berlin copy:

> "For the experiment at 80 m (Figure 4a) […] the difference between LOS and NLOS caused by a truck
> is **as high as 10 dB**. In the 120 m scenario (Figure 4b), the difference is less pronounced. The
> difference between LOS and the truck measurements is **in the order of 5 dB**. This is in line with
> the results shown in [22], where **the impact of different obstructions decreases as the distance
> between sender and receiver increases**."
>
> — Segata et al., IEEE VNC 2013, §IV. **Measured**, A12 freeway, 5.89 GHz 802.11p, rooftop 9 dBi
> omnis, truck blocker. IEEE copyright; quoted with attribution, **not** committed to refdata.

Our error against that pair: **−0.96 dB at 80 m, +4.04 dB at 120 m.**

### Verdict: row 5 is FALSIFIED, and the error changes sign inside our operating band

Two independent campaigns, two different blocker types, two different environments (parking lot and
freeway), two different bandwidths, both at ≈5.9 GHz with roof-mounted omnis, agree that **NLOSv
excess loss decays strongly with link distance between 10 m and 120 m**. Our model is *flat by
construction* over that entire range, because `max(0, 15·log₁₀d − 41)` does not switch on until
541 m — and when it finally does, it makes the loss *increase*, which is the wrong sign.

The consequence is worse than a bias: **our error changes sign across the band our scenarios
occupy.** We under-predict blockage badly at short range (−11 dB at 10 m) and over-predict it at
medium range (+2 dB at 100 m, +4 dB at 120 m). No single additive correction fixes both ends, and no
scatter argument reaches it — the Boban measurement's own standard deviation is under 1 dB.

This is a limitation of **TR 37.885's NLOSv term**, which we transcribe faithfully; it is not a
transcription error of ours. It is recorded here because the project claims "realistic radio", and a
term whose error reverses sign across the operating range is a real limit on that claim.

---

## 6. What remains structurally unrepresentable, and what is simply unvalidated

**Structural — the model cannot express these, at any parameter setting:**

- **Street corners (row 6).** TR 37.885 urban NLOS is `36.85 + 30·log₁₀(d3D) + 18.9·log₁₀(fc)` — a
  function of the **single** distance `d3D`. Nilsson et al.'s measured intersection model is a
  function of the two arm distances **separately**:
  `G(d_t, d_r) = 10log₁₀(m²) + 10log₁₀[g₁λ/(4π(d_t+d_r)²)] + [g₂Nλ/(4π(d_t+d_r)²)] + Ψσ`, with
  `N = max{2·d_t·d_r/(w_t·w_r) − 1, 0}`. Two geometries with the same `d3D` but different
  `(d_t, d_r)` splits are the same link to us and measurably different links in reality. The error is
  *signed and predictable*, and no choice of our constants removes it.
- **Multiple blockers (row 7).** `_VehicleBlockerIndex.tallest_blocker` returns the height of the
  **tallest** vehicle on the segment and nothing else. Five cars in a row on a link cost exactly what
  one car costs. GEMV² accumulates loss per blocker.
- **Blocker position along the link.** `tallest_blocker` computes the projection parameter `s` and
  then uses it **only** as a `0 < s < 1` gate — the value is discarded. Boban et al. report a
  systematic dependence: *"The attenuation varies depending on the position of the obstructing
  vehicle: it is lowest when the obstructing vehicle is near the middle of the transmit-receive
  distance, while it increases as the vehicle gets closer to either the transmitter or the
  receiver."* We are position-blind by construction. *(This is GEMV² output, same provenance caveat
  as §1; the structural point does not depend on the magnitude.)*

**Unvalidated — no measurement has been applied, and why:**

| Quantity | Why not validated |
|---|---|
| PDR / RSSI **vs distance, per link state** | This is P7's real content. No licence-clean V2V set at comparable antenna heights has been identified. The named candidate is excluded by its own authors — see §8. |
| SINR→PER waterfall | `refdata/nakagami_fading.sinr_to_per_mapping` is recorded UNAVAILABLE. The model uses a hard decode step instead, which is honest but is not a validated curve. |
| Nakagami *m* banding (3 / 1.5 / 1.0) | A project adoption, marked `coarse`. TR 37.885 specifies no small-scale fading distribution at all. Never compared with measurement. |
| Building-blocked (NLOSb) path loss | Only ever checked against the standard's own constants and one derived range cross-check. No measurement. |
| The awareness gate's sensitivity to §2 | Requires an A/B at a shorter decorrelation distance, which moves the digests. Not run. |
| The awareness gate's sensitivity to G1 | Requires an A/B with per-link blockage draws, which moves the digests. Not run. |

---

## 7. The ranked gap list

Rows 8–13 of the scorecard are effects for which `GeometricChannel` has **no term at all**. A
constant-by-constant audit cannot find these, because there is no constant to audit — which is why
revision 1 missed all six while getting the arithmetic on the constants exactly right.

Each was confirmed against `run.py`. **Every one of them is still the DEFAULT behaviour**, so both
pinned digests are untouched.

**Status of the fixes, as of this writing.** A concurrent workstream landed **opt-in arms** for five
of the six in the same working tree, all defaulting off. This section names the knob beside each
gap. Nothing below was changed by *this* work — this document's job is the evidence, and the fact
that the two workstreams reached the same six gaps from the same sources independently is worth
recording rather than eliding. Where the two disagree, [§7.7](#77-a-provenance-defect-in-the-new-opt-in-arm)
says so.

Ranked by (strength of evidence) × (effect on the metrics our gates actually read) ÷ (cost to fix).

### G1 — The NLOSv blockage loss is redrawn **per packet**, not held per link. *Fix first.*

*Opt-in arm now exists: `radio_nlosv_hold=True`. Default `False`.*

Inside `evaluate_raw`:

```python
pl += max(0.0, prng.gauss(tr37885_nlosv_mu_db(mu_base, d), sig_v))
```

TR 37.885 clause 6.2.1: *"The additional blockage loss is max {0 dB, **a log-normal random
variable**}"* — **one draw per blocked link**, a large-scale effect belonging to the same class as
shadow fading. *(Correction, 2026-09-07: the bolded reading is **not in the document** — clause
6.2.1 states no temporal scope for the draw at all, and the standard's own baseline holds the
LOS/NLOSv state for the link lifetime, a stronger hold. See the "SUPERSEDED IN PART" table at the
top; the physics argument below stands unchanged.)* Here a link parked behind the same truck receives an independent draw with σ = 4.5 dB
on **every CAM**. Note that the AR(1) shadowing sitting immediately above it in the same function
*is* correctly carried per link across steps; only the blockage term is not.

Three consequences, all in the wrong direction for a project whose gates read burst statistics:

1. It converts a large-scale shadowing term into **fast fading**, which is a category error.
2. It **shortens burst-loss runs**: a blocked link that should be reliably 15 dB down for a second
   instead flickers, flattering the packet-inter-reception tail and hence the awareness ratio.
3. It **double-counts against the per-packet Nakagami fade** on line 971, so blocked links carry two
   independent per-packet random terms where the standard specifies one per-packet and one per-link.

**Highest rank**: unambiguous conformance defect, no modelling judgement required, active on every
NLOSv link today, and cheap to fix (carry the draw in the existing per-link `st` dict beside `s`,
behind a flag).

### G2 — There is **no antenna gain or pattern term** by default. *The largest omission.*

*Opt-in arm now exists: `radio_antenna_pattern="tr37885_opt1"` with `radio_antenna_gain_dbi`.
Default `"none"`.*

By default the entire link budget is:

```python
mean_rx = self.tx_dbm - pl + shadow_db
```

The tokens `gain`, `dbi`, `azimuth`, `bearing` and `pattern` are **absent from `GeometricChannel`
altogether**. Both endpoints radiate isotropically at all angles.

TR 37.885 clause 6.1.4 specifies the opposite, per vehicle type, in two alternative options:

- **Option 1** (Tables 6.1.4-8, 6.1.4-9): the element's *horizontal* gain pattern is given
  separately for *"Vehicle Type 2"* and *"Vehicle Type 1 and Type 3"*; the array configuration puts
  types 1 and 3 on **front and rear antennas** with bearing angles *"Ω_Front = 0°"* and
  *"Ω_Rear = 180°"*, and type 2 on a **rooftop** antenna. Max element gain 3 dBi at 6 GHz.
- **Option 2** (Tables 6.1.4-10A…10D, 6.1.4-12): *"For vehicle type 1, one panel at the front bumper
  and one panel at the rear bumper"*; for types 2 and 3, front and rear **rooftop** panels. Max
  element gain **13 dBi** (front bumper), **11 dBi** (rear bumper), 3 dBi (rooftop). The standard
  notes that *"self-blockage effect is captured [in] Option 2 in the antenna pattern"*.

This is simultaneously **a conformance gap** and **the confound underlying §2's withdrawal**: it is
precisely the missing term that puts us in Nilsson et al.'s "otherwise" class. It is also why
correcting the decorrelation constant *first* would be the wrong order of work — the measured 2–4 m
is only reachable by a model that has this term.

Highest impact, lowest tractability: a real fix needs a per-vehicle azimuth pattern, link bearings,
and a decision between the two options. Worth scoping before attempting.

### G3 — No two-ray ground reflection and **no breakpoint**

*Opt-in arm now exists: `radio_breakpoint="two_ray"`. Default `"none"`.*

`TR37885_PATHLOSS` is a single-slope path loss for all distances:

```python
TR37885_PATHLOSS = {"urban_los": (38.77, 16.7, 18.2), ...}   # PL = a + b*log10(d) + c*log10(fc)
```

Abbas et al. fit a **dual-slope** model with a physical breakpoint, verbatim:

> "For the measurement setup the height of the TX/RX antennas was h_TX = h_RX = 1.47 m, thus, d_b can
> be calculated as, d_b = (4·h_TX·h_RX − λ²/4)/λ = **161 m** for λ = 0.0536 m at 5.6 GHz carrier
> frequency. A d_b of **104 m** was selected to match the values with the path loss model presented
> in [20], implying a somewhat better fit to the measurement data."

Evaluated at **our own** geometry — 1.5 m antennas, 5.9 GHz, λ = 0.050812 m — the physical
breakpoint is **d_b = 177.1 m**, which is **inside our operating range**. *(Correction, 2026-09-07:
the §4a antenna-height fix moved "our own geometry" to 1.6 m, so the physical breakpoint is now
**201.5 m** — d_b is quadratic in the height. Still inside the operating range; the table below is
kept as computed at 177.1 m and slightly overstates today's under-prediction.)* Our urban LOS decays at
**16.7 dB/decade** at all distances; Abbas measures **28.5 dB/decade** beyond the breakpoint (urban
LOS `n₂ = −2.85`; urban OLOS `n₂ = −2.74`, highway LOS `n₂ = −2.88`). The excess slope we are
missing is **11.8 dB/decade**.

Under-prediction of loss, for the two defensible breakpoint choices:

| d | with d_b = 177.1 m (our own physical breakpoint) | with d_b = 104 m (Abbas's fitted value) |
|---|---|---|
| 200 m | 0.62 dB | 3.35 dB |
| 300 m | 2.70 dB | 5.43 dB |
| 500 m | 5.32 dB | 8.05 dB |

**The absence of any breakpoint term is CONFIRMED. The magnitude above is PLAUSIBLE, not
confirmed** — it spans a factor of five across two legitimate breakpoint choices, and TR 37.885's
urban fit may legitimately embed street-canyon waveguiding that a two-ray model does not. What is
not in doubt is the *sign*: we under-predict path loss beyond ~180 m, which inflates long-range
awareness and interference alike.

### G4 — Blockage **variance** does not grow with blocker size, and **falsified** by measurement

*Still open, in every arm.* The `radio_nlosv_model="measured_boban"` arm makes the blockage **mean**
blocker-dependent and resizes σ to Abbas's measured OLOS total, but its σ is still one value for all
blockers — correctly, and its own docstring says why: no licence-clean source found states a fitted
σ per blocker class, and inventing a split would be fabrication. **This gap is the one that has no
fix available, only a missing source.**

Every V2V link resolves to `both_below`, so σ is **4.5 dB for a motorcycle and an articulated truck
alike** — verified against the live constants for all four entries of `TR37885_BLOCKER_HEIGHT_M`.
Revision 1 recorded Segata et al. only for the *mean*. Their result is about the **distribution**,
which is what their title claims and what this gap is:

> "For the 80 m experiment (Figure 6a) it can be seen that not only the average received power is
> affected by different obstacle types, but also its distribution. In particular, **the "bigger" the
> obstacle, the higher the variance**. This suggests that different received power **distribution
> parameters** should be employed in simulations when different types of vehicles obstruct the LOS."

They even name the fix: *"the model described in [22] takes into account the height of the vehicles
obstructing the LOS […] The same information could be used to decide the distribution (and relative
parameters) to be used in order to extract a randomized attenuation value."* We already carry the
blocker height; we discard it for σ as well as for μ.

### G5 — Blocker **footprint** ignores vehicle type

*Opt-in arm now exists: `radio_blocker_width="tr37885"`. Default `"uniform"`.*

`GEO_BLOCKER_HALF_WIDTH_M = 1.0` — one constant, every vehicle. TR 37.885 6.1.2 gives
*"length 5 meters, width 2.0 meters"* for types 1 and 2 and *"length 13 meters, width 2.6 meters"*
for type 3. A 13 m truck and a motorcycle occlude the same lateral band in our geometry test, so
blocker type affects neither *whether* a link is blocked, nor *how much* it loses (§1), nor with
*what spread* (G4). Cheap to fix; pairs naturally with G4.

### G6 — No temporal correlation in the small-scale fade

*No arm exists. Still open in every configuration.*

`evaluate_raw` draws Nakagami i.i.d. per packet:

```python
fade_db = 10.0 * math.log10(max(prng.gammavariate(m, 1.0 / m), 1e-12))
```

At 5.9 GHz (λ = 0.0508 m), with coherence time taken as 0.423/f_D:

| Relative speed | Doppler f_D | Coherence time | vs 100 ms CAM period |
|---|---|---|---|
| 30 m/s (highway closing) | 590 Hz | 0.72 ms | i.i.d. is **defensible** |
| 1 m/s | 19.7 Hz | 21.5 ms | marginal |
| 0.1 m/s (queue at a light) | 2.0 Hz | 215 ms | i.i.d. is **optimistic** |

Two vehicles stopped at the same signal have near-zero *relative* speed, and the channel between
them stays in the same fade across several CAMs. We give them independent draws, which
systematically under-states burst loss exactly where a queue forms — **which is the reference arm**
(`--traffic-lights`). Lowest rank of the six only because a fix needs a correlated-fading process
and its own validation, not because the effect is small.

---

### 7.7 A provenance defect in the new opt-in arm

**The concurrent workstream's `radio_nlosv_model="measured_boban"` arm inherits W1.** This is
recorded here, not fixed here, because that code is in flight and belongs to another workstream —
but a config enum is machine-readable output, and a machine-readable claim is a published claim.

The arm is named `measured_boban`, its block comment heads with *"NLOSv BLOCKAGE, MEASUREMENT-BASED
ARM"*, and it builds a per-blocker-class curve from two anchors:

```python
BOBAN_NLOSV_MU_AT_100M_DB = {"car": 5.0, "truck": 20.0}   # the 100 m level, per class
BOBAN_NLOSV_SLOPE_DB_PER_DECADE = 13.0                    # fitted to 20 dB @10 m, 7 dB @100 m
```

The **slope** anchor is a genuine measurement — the thesis's van experiment, §5 above. The
**100 m level** anchor is not. `{"car": 5.0, "truck": 20.0}` is the GEMV² Fig. 12 triple, and the
arm's comment describes its source as *"the only source found that reports all blocker classes at
one distance"* without recording that those three numbers are *"as generated by GEMV²"*. One of the
two anchors of a term called "measurement-based" is a simulator's output.

**Three concrete consequences, in ascending order of importance:**

1. **The name over-claims.** `measured_boban` mixes one measured anchor with one modelled anchor.
   Something like `boban_shape` or `measured_slope_gemv2_level` would be honest; failing a rename,
   the docstring and the refdata entry must say which anchor is which.
2. **The arm's own "known miss" has an explanation it does not give.** Its comment records that
   *"the two Boban sources disagree with each other about the van at 100 m (7 dB thesis, 13 dB
   TVT)"* and treats that as an unexplained inconsistency between two peers. It is not: 7 dB is a
   measurement of a 2010 Ford E-250 in a Pittsburgh car park, and 13 dB is GEMV²'s output for a
   2 m generic van over links of 75–125 m. **They are not the same kind of number and there is no
   reason to expect them to agree.** Better, both documents state enough to predict the *direction*
   of the gap, from the **clearance** each van has over the antennas it blocks:

   | | Blocker height | Antenna height | Clearance blocked | Excess loss @100 m |
   |---|---|---|---|---|
   | Thesis §2.3.1 (**measured**) | 2.085 m (Table 3.7, "2010 Ford E-250") | tips ≈1.72 m; the text says the van "sits around **37 cm** taller than the tip of the antennas" | **≈0.37 m** | ≈7 dB |
   | TVT Fig. 12 (**GEMV² output**) | 2.0 m mean, σ 0.15 m | 1.5 m ("both of which are passenger cars with the height of 1.5 meters") | **0.50 m** | ≈13 dB |

   The modelled van obstructs about a third more clearance than the measured one, which is the
   right direction for 13 dB against 7 dB. The two numbers are not in conflict; they describe two
   different geometries, and the arm's comment should say so rather than record a puzzle.
3. **The truck cross-check is real, and stronger than the arm claims.** The comment reports that the
   fit predicts 27.6 dB at 26 m against a measured 27 dB. That check *is* independent — a GEMV²
   level plus a measured slope reproducing a different measured point to 0.6 dB — and it is the
   best evidence in the arm. It should be stated as what it is rather than as one measurement
   agreeing with another.

**None of this makes the arm wrong to ship.** It is opt-in, it is better-evidenced than the
specification it replaces, and its shape is measured. What is wrong is the label. This is exactly
the defect this revision exists to correct, reappearing in new code on the same day — which is the
strongest argument available that the "provenance is part of the number" rule belongs in the
project's standing method and not just in this document.

---

## 8. Licence status of every number in this document

| Source | Licence | Established how | In refdata? |
|---|---|---|---|
| **3GPP TR 37.885 V15.3.0** | 3GPP specification text | `37885-f30.docx` retrieved from the 3GPP spec archive and read first-hand, 2026-09-07 | Yes — constants, as before |
| **Nilsson et al., Sensors 18(12):4433, 2018** | **CC BY 4.0** | Article's own licence statement: *"…distributed under the terms and conditions of the Creative Commons Attribution (CC BY) license"* | **Yes** — σ table and decorrelation medians |
| **Abbas et al., IJAP 2015, Art. 190607** | **CC BY 3.0** — *resolved, no longer partial* | CrossRef publisher-deposited metadata for `10.1155/2015/190607`, retrieved and parsed 2026-09-07: `license[0].URL = http://creativecommons.org/licenses/by/3.0/`, `start = 2015-01-01`, `delay-in-days = 0`, publisher Wiley. The article-level page is 403 to us and DOAJ carries no licence field, which is what stalled revision 1; the publisher's own deposit answers it. | **Yes**, and the house-rule violation is discharged |
| **Boban, Barros & Tonguz, IEEE TVT 63(9), 2014 (arXiv:1305.0124**<span>**v3**</span>**)** | IEEE copyright | — | **No.** Quoted as attributed text. *The numbers are GEMV² output in any case* |
| **Boban, PhD thesis, arXiv:1405.1008** | **arXiv.org perpetual non-exclusive licence — NOT Creative Commons** | arXiv abstract page, read 2026-09-07 | **No.** Quoted as attributed text only. §5's 20 dB / 7 dB **must not be pinned** |
| **Segata et al., IEEE VNC 2013** | IEEE copyright (`978-1-4799-2687-9/13/$31.00 ©2013 IEEE` on the paper) | Paper's own copyright line | **No.** Quoted as attributed text only |
| **Boban & d'Orey, IEEE TVT 65(6), 2016** | IEEE copyright | — | Conditions already recorded in `v2x_awareness_conditions.json` from a prior pass |
| **FLOURISH Bristol dataset** | **Non-Commercial Government Licence v2** (the *paper* is CC BY 4.0 — different licences) | Bristol repository record + Data in Brief, both read 2026-09-07 | **No, and now explicitly blocked** |

Every retrieved source is cached under the gitignored `/.cache/papers/`. Nothing under a
licence-unclear or non-CC licence is committed anywhere in the tree.

**W5, discharged.** Revision 1 recorded the Abbas licence as unverifiable **and committed the
numbers anyway**, which is a direct violation of the standing rule that numbers from licence-unclear
sources are never committed. The rule was not wrong and the numbers did not have to be withdrawn —
the licence was simply resolvable from a source revision 1 did not try. It is CC BY 3.0. The entry
in `refdata/pathloss_3gpp_tr37885.json` now says so, and the violation no longer exists.

**No number in this document was digitised from a figure.** Every measured value quoted is a
sentence or a table cell in its source, including the four numbers revision 1 declined on the
grounds that they "appear to be figure readings" (§5) — they are body text.

### The FLOURISH entry remains withdrawn

`refdata/v2x_awareness.json`'s `pdr_vs_rssi_curve` named the Bristol dataset and gave a recipe:
"download the dataset under its DOI, bin MAC-layer delivery by RSSI, and pin the resulting points."
Three things were missing from that recipe and are recorded beside it:

1. **The authors exclude this use.** Data in Brief abstract, verbatim: *"The dataset is not intended
   to be used for signal propagation modelling."* The old entry quoted the neighbouring sentence
   ("suitable for calibrating physical layers of vehicular simulators") and omitted this one.
2. **The geometry is wrong for us.** Four RSUs mast-mounted at ~5 m, ~8 m, ~12 m and ~25 m — V2I
   links with one endpoint 3–17× higher than our 1.5 m V2V. Under the NLOSv branch rule an antenna
   at ≥5 m clears every blocker we model, so those links sit in a different regime entirely.
3. **The dataset is non-commercial.** NCGL v2, not "open data" as the entry said.

---

## 9. What was and was not changed

**Changed — documentation and recorded evidence only:**

- `docs/realism/RADIO-VS-REALITY.md` — this file, revision 2. Three claims withdrawn (W1–W3), four
  corrected (W4–W7), six new gaps recorded (G1–G6).
- `docs/realism/ROADMAP-PERFECT.md` — P7a's "near-exact agreement with measurement" withdrawn; new
  P7b carries the ranked gap list.
- `refdata/pathloss_3gpp_tr37885.json` — clause references corrected to 6.1.2; Abbas carrier
  frequency corrected to 5.6 GHz; Abbas licence resolved to CC BY 3.0; the decorrelation entry
  re-framed from "falsified" to "environment-dependent, with the antenna-pattern confound named";
  both urban dropping options recorded; four new entries for G1, G2, G3 and G4/G5.
- `refdata/v2x_awareness.json` — the decorrelation gate note re-stated without the withdrawn
  falsification.
- `tests/test_geometric_channel.py` — § 8 falsification pins, extended for the new gaps.

**Not changed by this work:**

- Every constant and every default in `GeometricChannel`. `TR37885_NLOSV`,
  `TR37885_SHADOW_DECORR_M`, `TR37885_SHADOW_SIGMA_DB`, `V2X_ANTENNA_HEIGHT_M` and the branch rule
  are exactly as found — including the two we know to be wrong against the standard (§4) and the
  per-packet draw (G1). **This work touched no line of `run.py`.**
- The `[7.0, 13.0]` decorrelation gate.
- Both pinned digests, re-measured by the suite on the finished tree and byte-identical.

**Gate: full suite, 1688 passed / 1 failed in 3463 s.** The single failure is
`test_dataset_integrity.py::test_all_datasets_pass_integrity_audit` on `datasets/poc_run`, and it is
**not this work's**. Reported as such rather than waved away, with the evidence: the failing checks
are all file-digest and count mismatches (`I1_file_digests_match_manifest`,
`I2_aggregate_data_digest`, `CNT2_reports: manifest=63 ma=396`), and `ls` on that directory shows
`manifest.json` dated 2026-08-29 beside three payload files rewritten **during this session** — with
`gt_emissions_sample.jsonl` truncated to **0 bytes**. A concurrent process wrote into that
gitignored dataset while the audit was reading it. This work changed no generator code and no line
of `run.py`. The 310 tests that pin the two digests and the refdata invariants
(`test_bounded_population`, `test_conformance`, `test_protocol_stack`, `test_protocol_profile`,
`test_crl_sanity_index`, `test_realism_bench`, `test_geometric_channel`) were re-run afterwards on
their own: **310 passed**, both digests byte-identical.

**Changed by a concurrent workstream, in the same tree, not by this work:** opt-in arms for G1, G2,
G3, G5 and row 5 (`radio_nlosv_hold`, `radio_antenna_pattern` + `radio_antenna_gain_dbi`,
`radio_breakpoint` + `radio_breakpoint_slope_db_per_decade`, `radio_blocker_width`,
`radio_nlosv_model`). All five default to the historic behaviour, which is why both digests hold.
§7.7 records the one provenance defect those arms inherited from revision 1 of this document.

**The single highest-value fix this work identifies has changed.** Revision 1 nominated §4a + §4b
(adopt the standard's antenna heights, correct the boundary) on the strength of a claim that this
would "restore the standard's near-exact agreement with GEMV²'s measured 5 dB" — a claim withdrawn
as W1. That fix is still worth making as **conformance**, and its bias is now correctly bracketed at
−2.54 dB (Option B) to −5.20 dB (Option A), but it buys no demonstrated agreement with reality.

The highest-value fix is now **G1**: hold the NLOSv blockage draw per link instead of per packet.
It is a pure conformance defect against the clause we already implement, it needs no new physics, it
is active on every blocked link today, and it directly distorts the burst-loss statistics that the
Phase-2 awareness gate reads.
