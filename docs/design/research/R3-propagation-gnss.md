# R3 — Radio Propagation, Fading, Obstacles, Weather, Antennas & GNSS for 5.9 GHz V2X
Cited parameter fact sheet. Every value below carries a source; anything that could not be
verified in the source cache or via live fetch during this session is explicitly marked
**UNVERIFIED**. Local cache paths are relative to
`scratchpad/research/` (subdirs `txt/`, `pdf/`, `pdf_txt/`, `pdftxt/`, `src/`) unless a URL is given.

---

## A. Large-scale path loss: free-space, two-ray, log-distance, measured V2V

### A.1 Free-space and two-ray ground reflection

| Parameter | Value / Formula | Source |
|---|---|---|
| Free-space path loss | `L_fs[dB] = 20log10(d) + 20log10(f) + 32.44` (d in km, f in MHz); equivalently `Pr = Pt·Gt·Gr·(λ/4πd)²` | Standard Friis eq., used directly in Sommer 2011 Eq.(2)/(1); `pdf/sommer-shadowing` derivation, `txt/webfetch-1789708381925-lwzbjh.txt` |
| Two-ray ground (large-d asymptote) | `Pr/Pt = Gt·Gr·ht²·hr² / d⁴` → `L[dB] = 10log10(d⁴·L_sys / (ht²·hr²))` | ns-3 `TwoRayGroundPropagationLossModel` (`pdf/ns3_propagation_loss_model.cc` lines 359-390, ported from ns-2, Rappaport model) |
| Crossover (breakpoint) distance | `d_c = 4π·h_t·h_r / λ` (below d_c use Friis; above, use two-ray d⁻⁴ law) | `pdf/ns3_propagation_loss_model.cc` line 406: `dCross = (4*M_PI*txAntHeight*rxAntHeight)/m_lambda;` |
| Fresnel-corrected breakpoint variant (used in V2V measurement fitting) | `d_b = (4·h_TX·h_RX − λ²/4) / λ` | Abbas et al. 2015, `pdf/abbas2015.txt` line ~600: for h_TX=h_RX=1.47 m, λ=0.0536 m (5.6 GHz) → **d_b = 161 m** theoretically; the authors instead used **d_b = 104 m** to better fit the LOS/OLOS data (matched to Cheng et al.'s value) |
| Veins/OMNeT++ Two-Ray Interference Model | `Γ = (sinθ − √(ε_r−cos²θ)) / (sinθ + √(ε_r−cos²θ))`; attenuation from coherent sum of direct + ground-reflected ray using ground relative permittivity ε_r (not a fixed −1 reflection coefficient) | `pdf/veins_tworay.cc` (Veins `TwoRayInterferenceModel::filterSignal`, Joerer 2011) — **exact default numeric ε_r value not found in local cache; UNVERIFIED** (Veins default antenna offset for 802.11p PHY is `antennaOffsetZ = 1.895 m`, `pdf/veins_phy.ned` line 38) |
| 3GPP V2V antenna height assumption | **1.5 m** for vehicle-UE antennas in all V2V link-level/system-level evaluations (also 5 m for RSU/eNB-type RSU) | 3GPP TR 36.885 (raw extract) `src/tr36885_raw.txt`: "…the antenna height should be set to 1.5 m" (WINNER+ B1); "…antenna height at RSU changed to 5 m" |
| Shadowing spatial correlation distance (3GPP, cellular-derived, applied to V2X eNB shadowing) | `D_corr = 50 m` in `S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1−exp(−2D/D_corr))·R·N(n)` | `src/tr36885_raw.txt` |

### A.2 Log-distance + log-normal shadowing (generic model)

| Parameter | Value / Formula | Source |
|---|---|---|
| Generic model | `PL(d)[dB] = PL0 + 10·n·log10(d/d0) + X_σ`, `X_σ ~ N(0,σ²)` | Karedal et al. 2011 Eq.(5) (`pdf/karedal2011.txt` line ~410); Abbas et al. 2015 Eq.(4) (`pdf/abbas2015.txt` line ~526) |
| Reference distance | `d0 = 10 m` (few samples below 10 m in V2V field data) | Karedal 2011 & Abbas 2015, both use `d0 = 10 m` explicitly |

### A.3 Cheng et al. 2007 (IEEE JSAC 25(8), dual-slope, Pittsburgh suburban, 5.9 GHz)

**Note on sourcing:** the original Cheng et al. 2007 PDF could not be retrieved this session
(direct URL and IEEE/ResearchGate mirrors returned 404/paywall; see cache files
`pdf/cheng2007.pdf` = 404 page). All Cheng-2007 numbers below are therefore **secondary
citations** from papers that quote Cheng's results directly, cross-checked across two
independent citing papers for consistency.

| Parameter | Value | Source |
|---|---|---|
| Model type | Dual-slope (piecewise-linear) log-distance path loss + Nakagami-*m* small-scale fading, suburban Pittsburgh, PA, 5.9 GHz | `pdf/boban_realistic_efficient.txt` line 5602 ("Cheng et al. …fit the measurement data to a dual slope piecewise log-distance path loss model … two path loss exponents and two fading deviations") |
| n1 (highway, pre-breakpoint) | **1.9**, breakpoint at **220 m** | Karedal et al. 2011 comparing to "Cheng et al. [5]" (`pdf/karedal2011.txt` line 484: "highway result of [5] (n=1.9 up to a breakpoint at 220 m)") |
| n1 (suburban, pre-breakpoint), variant A | **2.0–2.1**, breakpoint at **100 m** | Karedal 2011, citing "Cheng et al. [3]" (line 486-487) |
| n1 (suburban, pre-breakpoint), variant B | **2.3**, breakpoint at **226 m** | Karedal 2011, citing "Cheng et al. [5]" (line 487-488) |
| Path-loss exponent range (suburban, corroborating) | **2.3 ≤ γ ≤ 2.75** | `pdf/boban_realistic_efficient.txt` line 4692, citing Cheng et al. [24] |
| n2 (post-breakpoint), σ1/σ2 | **UNVERIFIED** — not recoverable from any cached/fetched copy of the paper | — |
| Nakagami-*m* distance dependence (secondary, via Yin et al. — see §B) | reported *m* ≈ **1–1.8 for d<100 m**, ≈ **0.7–1 for d≥100 m** (10-m bins), i.e. approx. Rician-like near, Rayleigh-like far | `pdf/alpha_mu_dsrc.txt` lines 330-338 — **caveat: this specific number is attributed to ref. [37] = J. Yin, G. Holland, T. ElBatt, F. Bai, H. Krishnan, "DSRC Channel Fading Analysis from Empirical Measurement" — NOT Cheng et al. 2007.** Included here because it is the closest verified distance-binned Nakagami-*m* dataset found in the cache for the same 5.9 GHz DSRC context; do not attribute to Cheng 2007 in the simulator without separately confirming. |

### A.4 Karedal et al. 2011 (IEEE TVT 60(1), highway/rural/urban/suburban, 5.2 GHz)

| Parameter | Value | Source |
|---|---|---|
| Rural model | Two-ray ground model (Eq. 3), valid for d≥20 m | `pdf/karedal2011.txt` lines 375-400 |
| Highway/urban/suburban model | Classical power law, Eq.(5), `d0=10 m` | same, lines 408-423 |
| Qualitative finding | **All four environments have path-loss exponent n < 2** ("better than free-space propagation," attributed to multipath reinforcement) | `pdf/karedal2011.txt` line 461 |
| Comparison point (highway) | Kunisch & Pamp reported highway **n=1.85, σ=3.2**; urban **n=1.61, σ=3.4** — Karedal states their own highway exponent "compares well" with this | `pdf/karedal2011.txt` lines 463-467 |
| Exact Table I values (PL0, G12, PLc, σ1, σ2, n, h per environment) | **UNVERIFIED** — Table I's numeric grid is not present in the extracted text of *any* of the three independently obtained copies of this paper (local cache PDF, USC WiDeS mirror, Lund University portal mirror all fail identically on the table body, likely a text-extraction/table-rendering artifact common to this specific IEEE two-column PDF). Only the qualitative comparisons above survived extraction. | `pdf/karedal2011.txt`; `/tmp/karedal_usc.txt` (fetched from `https://wides.usc.edu/Updated_pdf/Path%20loss%20modeling%20for%20vehicle-to-vehicle%20communications.pdf`, same gap) |

### A.5 Abbas et al. 2015 (arXiv:1203.3370, publ. IEEE/Int. J. Antennas Propag. 2015 — dual-slope LOS/OLOS)

Table II, "Parameters for the Dual-Slope Path Loss Model" (channel-gain convention, so exponents are shown as the paper prints them, i.e. negative of path-loss exponent magnitude):

| Scenario | n1 | n2 | PL0 [dB] | σ [dB] |
|---|---|---|---|---|
| LOS – Highway | −1.66 | −2.88 | −66.1 | 3.95 |
| LOS – Urban | −1.81 | −2.85 | −63.9 | 4.15 |
| OLOS – Highway | (not modeled, too few short-range samples) | −3.18 | −76.1 | 6.12 |
| OLOS – Urban | −1.93 | −2.74 | −72.3 | 6.67 |

Source: `pdf/abbas2015.txt` lines 565-576 (Table II).

| Parameter | Value | Source |
|---|---|---|
| Breakpoint distance used | `d_b = 104 m` (theoretical Fresnel-based value was 161 m for h=1.47 m, λ=0.0536 m @ 5.6 GHz; 104 m chosen to match Cheng et al.) | `pdf/abbas2015.txt` lines 598-605 |
| LOS↔OLOS offset (vehicle-obstruction attenuation) | **8.6–10 dB** (measured); cross-checked against "9.6 dB" in ref. [17] and "10–20 dB" in Meireles et al. [18] | `pdf/abbas2015.txt` lines 618-624 |
| NLOS (intersection, via Mangel's model, reused since insufficient direct NLOS data) | `n_NLOS = 2.69`, `σ = 4.1 dB` | `pdf/abbas2015.txt` lines 686-704 (Eq. 6, Mangel et al. model) |
| Validity range | `d > 10 m`, `d0 = 10 m` | `pdf/abbas2015.txt` line 576 |

### A.6 Veins defaults

| Parameter | Value | Source |
|---|---|---|
| PHY antenna offset (z) | `1.895 m` default | `pdf/veins_phy.ned` line 38 |
| CCA threshold | `−65 dBm` default | `pdf/veins_phy.ned` line 39 |
| Two-Ray Interference Model ground permittivity ε_r, Simple Obstacle Shadowing defaults | **UNVERIFIED** in local cache (not present in any cached `.ned`/`.cc` file; would require pulling `PhyLayer80211p.ned` / `SimpleObstacleShadowing.ned` directly from the Veins source repo, which was not among the cached/fetchable files this session) | — |

---

## B. Fast (small-scale) fading: Nakagami-*m*, Rician K

| Parameter | Value | Source |
|---|---|---|
| Nakagami-*m* PDF | `f(x;m,Ω) = 2mᵐ/(Γ(m)Ωᵐ)·x^(2m−1)·exp(−m x²/Ω)`, m≥½ | Torrent-Moreno et al. 2009 Eq.(2), `pdf/torrent_moreno_tvt09.txt` lines 963-966 |
| m=1 ⇔ Rayleigh (severe fading / NLOS-like); m>1 ⇔ increasingly LOS-like | Qualitative equivalence stated explicitly | `pdf/torrent_moreno_tvt09.txt` lines 985-993 |
| Fixed-m values used in D-FPAV / EMDV simulation study | m ∈ {1, 3, 5}, labeled "severe," "medium," "low" fading | `pdf/torrent_moreno_tvt09.txt` lines 991-993, 1318 (this paper implements, but does not itself derive, the Taliwal et al. Nakagami-model port into ns-2.28) |
| **Taliwal/Torrent-Moreno distance-dependent m=3 (d<50m) / m=1.5 (50–150m) / m=1 (>150m) claim** | **UNVERIFIED.** Torrent-Moreno et al. 2009 (`pdf/torrent_moreno_tvt09.txt`) states that "Taliwal et al. implemented the [Nakagami] model into ns-2.28" (line 949) but does **not** reproduce Taliwal's distance thresholds; the original Taliwal, Jiang, Mangold, Chen & Sengupta, "Empirical Determination of Channel Characteristics for DSRC Vehicle-to-Vehicle Communication" (VANET '04) was not retrievable in the cache and web search located only its bibliographic record, not full text. **Do not hard-code the specific 50 m / 150 m thresholds without independently confirming against the VANET'04 paper or its ns-2.28 code.** | Bibliographic confirmation only: WebSearch result citing "Taliwal, V., Jiang, D., Mangold, H., Chen, C., Sengupta, R., 'Empirical determination of channel characteristics for DSRC vehicle-to-vehicle communication,' VANET '04" |
| Distance-binned Nakagami-*m* (empirical, DSRC 5.9 GHz freeway, 10-m bins) — closest verified analogue | m ≈ **1.0–1.8** for d < 100 m; m ≈ **0.7–1.0** for d ≥ 100 m | `pdf/alpha_mu_dsrc.txt` lines 330-338, citing J. Yin et al., "DSRC Channel Fading Analysis from Empirical Measurement" (**not** Cheng 2007 — see §A.3 caveat) |
| Cheng et al. 2007 own Nakagami-*m* values (by distance) | **UNVERIFIED** — not recoverable; only the qualitative statement that "fading intensities … are different depending on the distance between TX and RX" survives via secondary citation | `pdf/boban_realistic_efficient.txt` line ~3406 context |
| Rician K-factor for V2V | **UNVERIFIED** — no explicit K-factor numeric value for 5.9 GHz V2V was found in any locally cached source in this pass (Karedal 2011, Abbas 2015, Torrent-Moreno 2009, and the alpha-mu/composite papers were all checked; none report a Rician K value, they use Nakagami-*m* or two-ray/log-distance instead). ETSI TR 103 257-1 (`txt/tr103257-1.txt` lines 464-466) confirms qualitatively that "Rician distribution is used when the communication contains a LOS component" but gives no K number for 5.9 GHz V2V. | `txt/tr103257-1.txt` |

---

## C. Obstacle (building) shadowing and terrain diffraction

### C.1 Sommer et al. 2011 empirical shadowing model

Model: `L_obs[dB] = β·n + γ·d_m`, where *n* = number of exterior-wall crossings, *d_m* = total
in-building path length (m). Combined with free-space PL: `Pr = Pt + 10log10(GtGrλ²/16π²d^α) − βn − γd_m`.

| Building type | β [dB/wall] | γ [dB/m] | Source |
|---|---|---|---|
| Free-standing warehouse (countryside) | 9.2 | 0.32 | `txt/sommer-shadowing.txt` lines 415-417 |
| Suburban house | 9.6 | 0.45 | `txt/sommer-shadowing.txt` lines 446-448 |
| **Default / typical (used for majority of dataset)** | **≈9** | **≈0.4** | `txt/sommer-shadowing.txt` line 456 |
| Light-construction house | 2.4 | 0.63 | `txt/sommer-shadowing.txt` line 477 |
| Urban residential home (per-building fit, building 1) | 2.38 | 0.10 | `txt/sommer-shadowing.txt` line 484 |
| Urban residential garage (per-building fit, building 2) | 6.26 | 0.41 | `txt/sommer-shadowing.txt` line 484 |

Fitting method: Gauss-Newton nonlinear least squares, tolerance 1×10⁻⁵ (`txt/sommer-shadowing.txt` lines 390-393).
Full paper: C. Sommer, D. Eckhoff, R. German, F. Dressler, "A Computationally Inexpensive
Empirical Model of IEEE 802.11p Radio Shadowing in Urban Environments," WONS 2011 (fetched
in full this session, cached at `txt/sommer-shadowing.txt` / `txt/webfetch-1789708381925-lwzbjh.txt`).
**Confirms the task's β≈9 dB/wall, γ≈0.4 dB/m values.**

### C.2 ITU-R P.526 knife-edge diffraction (terrain)

| Parameter | Formula | Source |
|---|---|---|
| Diffraction parameter ν (Eq. 26) | `ν = h·√(2(d1+d2)/(λ·d1·d2))` (self-consistent units); practical-units variant Eq.(33): `ν = 0.0316·h·√(2(d1+d2)/(λ d1 d2))`, h & λ in m, d1/d2 in km | `pdf/itu_p526c.txt` lines ~1195-1330 |
| Knife-edge loss J(ν) — exact | `J(ν) = 20log10[√((1−C(ν)−S(ν))² + (1−S(ν)... )²)/2]` (via Fresnel integrals C,S) | `pdf/itu_p526c.txt` Eq.(30), lines 1300-1310 |
| Knife-edge loss J(ν) — approximation, valid for ν > −0.78 | `J(ν) = 6.9 + 20log10(√((ν−0.1)²+1) + ν−0.1)` dB | `pdf/itu_p526c.txt` Eq.(31), line 1319. **This exact formula is independently reproduced (matching to the constant) in Boban's thesis Eq.(3.8)** (`pdf/boban_realistic_efficient.txt` line ~3626), cross-confirming correctness. |
| Multiple knife-edge / obstacle methods | ITU-R method (modified Epstein-Peterson, "more optimistic"), Deygout method ("more pessimistic"), Giovaneli approximation | `pdf/itu_p526c.txt` (§ multiple-edge, referenced generically); enumerated explicitly in `pdf/boban_realistic_efficient.txt` lines 3628-3640, citing Deygout 1966 and Giovaneli 1984 |
| ITU-R document version | Rec. ITU-R P.526-14 (this cached copy); current edition per itu.int is **P.526-15** | `pdf/itu_p526c.txt` header |

---

## D. Vehicles as obstacles

### D.1 Boban et al. — measured attenuation

| Parameter | Value | Source |
|---|---|---|
| Single obstructing vehicle, generic | reduces received power by **>20 dB** | Boban, Vinhoza, Barros, Ferreira, Tonguz, "Impact of Vehicles as Obstacles in Vehicular Ad Hoc Networks," **IEEE JSAC 29(1):15-28, Jan 2011** — cited verbatim in `pdf/boban_tvr.txt` line 6 ("a single obstructing vehicle can reduce the power at the receiver by more than 20 dB") and `pdf/boban_realistic_efficient.txt` line 5691-5692 (identical statement, same author group) |
| Single large truck (specific measurement) | **27 dB** attenuation vs. LOS, at the smallest recorded distance (26 m = truck length) | Meireles, Boban, Steenkiste, Tonguz, Barros, "Experimental study on the impact of vehicular obstructions in VANETs," IEEE VNC 2010 — quoted in `pdf/boban_tvr.txt` line 22 and reproduced with full context in `pdf/boban_realistic_efficient.txt` lines 2908-2912 |
| Van (comparison) | **12 dB** attenuation at 20 m | `pdf/boban_realistic_efficient.txt` line 2912 |
| Bumper-to-bumper obstruction (closest case) | **>20 dB** attenuation | `pdf/boban_realistic_efficient.txt` lines 3271-3272 |
| Effect on range / PDR | Effective communication range reduced up to **60%**; PDR reduced up to **30%**, depending on environment; static (building) obstructions found **even more severe** than vehicular ones | `pdf/boban_realistic_efficient.txt` lines 5701-5703, 3268-3274 |
| Tall-vehicle height differential | Commercial/public-transport vehicles (vans, buses, trucks) are **>1.5 m taller**, on average, than passenger cars | `pdf/boban_tvr.txt` lines ~60-63, `pdf/boban_realistic_efficient.txt` line 5724, both citing Boban et al. IEEE JSAC 2011 |
| Tall-vehicle relaying benefit | Up to **50%** increase in effective communication range when using tall vehicles as relays (in certain scenarios) | `pdf/boban_tvr.txt` abstract |

### D.2 Measured vehicle dimensions (height/width/length, m)

| Vehicle | Height | Width | Length | Source |
|---|---|---|---|---|
| 2002 Lincoln LS (passenger) | 1.453 | 1.859 | 4.925 | `pdf/boban_realistic_efficient.txt` Table 3.7 |
| 2009 Pontiac Vibe (passenger) | 1.547 | 1.763 | 4.371 | same |
| 2010 Ford E-250 (van, obstruction) | 2.085 | 2.029 | 5.504 | same |
| 2007 Kia Cee'd (passenger) | 1.480 | 1.790 | 4.260 | Table 4.2 |
| 2002 Honda Jazz (passenger) | 1.525 | 1.676 | 3.845 | Table 4.2 |
| 2010 Mercedes Sprinter (tall/commercial) | 2.591 | 1.989 | 6.680 | Table 4.2 / 5.2 |
| 2010 Fiat Ducato (tall/commercial) | 2.524 | 2.025 | 5.943 | Table 4.2 / 5.2 |
| 2011 Citroen C4 (passenger) | 1.491 | 1.789 | 4.329 | Table 5.2 |
| 2011 Opel Astra (passenger) | 1.510 | 1.814 | 4.419 | Table 5.2 |
| Highway dataset "tall vehicle" fraction | A28 (Porto): 14.36% of 404 vehicles tall (32.3 veh/km); A3: 18.18% of 55 vehicles (7.3 veh/km) | Table 3.1 |

### D.3 Knife-edge model for vehicles

| Parameter | Value | Source |
|---|---|---|
| Single knife-edge attenuation (vehicle) | `A_sk = 6.9 + 20log10[√((v−0.1)²+1) + v−0.1]` for v>−0.7, else 0; `v = √(2H/r_f)`, H = obstacle height above Tx-Rx line, r_f = Fresnel radius | `pdf/boban_realistic_efficient.txt` Eq.(3.8), lines 3620-3626 — **identical form to ITU-R P.526 Eq.(31)**, confirming ITU-R P.526 is the underlying source (cited explicitly as ref. [56] = ITU-R recommendation) |
| Wavelength vs. vehicle size validity check | λ_DSRC ≈ 5 cm ≪ vehicle dimensions → knife-edge approximation valid | `pdf/boban_realistic_efficient.txt` lines 3597-3600 |
| Foliage attenuation (used alongside vehicle/building diffraction in the same model) | `MEL = 0.79·f^0.61` dB/m (f in GHz) → **2.3 dB/m** at 5.9 GHz for deciduous trees | `pdf/boban_realistic_efficient.txt` Eq.(4.1), lines 4468-4470 |
| Max communication ranges used in Boban's V2V simulator (calibrated from measurements + [16,79,81,93]) | r_LOS-highway = **1000 m**; r_LOS-urban = **500 m**; r_NLOSv = **400 m**; r_NLOSb = **300 m** | `pdf/boban_realistic_efficient.txt` Table 4.5, lines 4982-4990 |

### D.4 3GPP TR 37.885 — NLOSv extra loss / LOS-NLOSv state model

| Parameter | Value | Source |
|---|---|---|
| P(LOS), highway, d≤475 m | `min{1, a·d² + b·d + c}`, a=2.1013×10⁻⁶, b=−0.002, c=1.0193 | `pdf/tr37885.txt` Table 6.2-1, lines 1533-1540 |
| P(LOS), highway, d>475 m | `max{0, 0.54 − 0.001·(d−475)}` | same |
| P(LOS), urban | `min{1, 1.05·exp(−0.0114·d)}` | same |
| P(NLOSv) | `1 − P(LOS)` (link is either LOS or NLOSv within the same street) | same |
| Path loss, LOS/NLOSv, highway | `PL = 32.4 + 20log10(d3D) + 20log10(fc)` [GHz, m], σ_SF = 3 dB | `pdf/tr37885.txt` Table 6.2.1-1, lines 1553-1562 |
| Path loss, LOS/NLOSv, urban | `PL = 38.77 + 16.7log10(d3D) + 18.2log10(fc)`, σ_SF = 3 dB | same |
| Path loss, NLOS (building-blocked) | `PL = 36.85 + 30log10(d3D) + 18.9log10(fc)`, σ_SF = 4 dB | same |
| **NLOSv additional blockage loss — Case 1** (min antenna height of Tx/Rx > blocker height) | **0 dB** additional loss | `pdf/tr37885.txt` lines 1568-1575 |
| **NLOSv additional blockage loss — Case 2** (max antenna height of Tx/Rx < blocker height) | mean = `9 + max(0, 15·log10(d) − 41)` dB, **σ = 4.5 dB** | same |
| **NLOSv additional blockage loss — Case 3** (otherwise, straddling case) | mean = `5 + max(0, 15·log10(d) − 41)` dB, **σ = 4 dB** | same |
| Blocker height | Randomly drawn from the 3 simulated vehicle-type heights, weighted by their scenario population fraction | same |

---

## E. Weather effects at 5.9 GHz

### E.1 Rain — ITU-R P.838-3

Specific attenuation: `γ_R[dB/km] = k·R^α` (R = rain rate in mm/h).

| Freq | k_H | α_H | k_V | α_V | Source |
|---|---|---|---|---|---|
| 5.5 GHz | 0.0003909 | 1.6499 | 0.0003115 | 1.5882 | `pdf/itu_p838.txt` Table 5 |
| **6 GHz** (closest tabulated point to 5.9 GHz) | **0.0007056** | **1.5900** | **0.0004878** | **1.5728** | `pdf/itu_p838.txt` Table 5, lines 177-186 |

**Computed** (from the cited formula/coefficients, R=50 mm/h "heavy rain," using the 6 GHz row):

| Polarization | γ_R [dB/km] | Attenuation over 300 m | 
|---|---|---|
| Horizontal | 0.0007056 × 50^1.59 ≈ **0.35** | **≈0.11 dB** |
| Vertical (typical for vehicular monopole/dipole antennas) | 0.0004878 × 50^1.573 ≈ **0.23** | **≈0.07 dB** |

*(50^1.59 ≈ 502.7, 50^1.573 ≈ 470.1, computed via `exp(α·ln50)`.)*

**Honest conclusion:** even in a 50 mm/h downpour (very heavy rain, rare event), rain adds roughly a
**tenth of a decibel** over a 300 m V2X link at 5.9 GHz — utterly negligible next to the 6–10+ dB
link margins consumed by shadow fading, NLOSv blockage, or a single obstructing truck (§C, §D).
Rain attenuation only becomes a real link-budget factor at mmWave/5G-NR-V2X frequencies (28/39/60+ GHz), not at 5.9 GHz.

### E.2 Fog/cloud — ITU-R P.840-8

| Parameter | Value | Source |
|---|---|---|
| Specific attenuation | `γ_c[dB/km] = K_l(f,T)·M` | `pdf/itu_p840.txt` Eq.(1), lines 113-121 |
| K_l formula | `K_l(f,T) = 0.819f / [ε''(1+η²)]`, double-Debye model for water permittivity ε(f,T) | `pdf/itu_p840.txt` Eq.(2)-(11), lines 125-145 |
| Liquid water density, medium fog (visibility ≈300 m) | M = **0.05 g/m³** | `pdf/itu_p840.txt` lines 122-124 |
| Liquid water density, thick fog (visibility ≈50 m) | M = **0.5 g/m³** | same |
| Qualitative validity statement (direct quote) | "At frequencies of the order of **100 GHz and above**, attenuation due to fog **may be significant**." | `pdf/itu_p840.txt` line 122 |
| Exact K_l(5.9 GHz) numeric value | **UNVERIFIED / not computed** — the double-Debye formula requires ε'(f) (real part) which was not fully recoverable from the extracted text (only the imaginary-part ε'' components were reconstructable); rather than hand-derive a number that could be wrong, this is left unverified. Qualitatively, since P.836/P.840 itself states fog only matters "at 100 GHz and above," fog attenuation at 5.9 GHz is expected to be **orders of magnitude below the rain values above (§E.1)** | `pdf/itu_p840.txt` |

### E.3 Atmospheric gases — ITU-R P.676-12

| Parameter | Value | Source |
|---|---|---|
| Specific attenuation formula | `γ = γ_o + γ_w = 0.1820·f·N''(f)` dB/km (line-by-line sum of oxygen + water-vapor absorption lines) | `pdf/itu_p676.txt` Eq.(1), lines 187-206 |
| Terrestrial path attenuation | `A = (γ_o+γ_w)·r0` (r0 = path length, km) | `pdf/itu_p676.txt` Eq.(29) |
| Nearest resonance lines to 5.9 GHz | Water vapor line at 22.235 GHz; oxygen complex centered ≈60 GHz — 5.9 GHz is far from both | General P.676 structure (Fig. 1/10 description, `pdf/itu_p676.txt` lines 1029-1035) |
| Exact γ(5.9 GHz) numeric value | **UNVERIFIED / not computed** — Fig. 1/Fig. 10 in the Recommendation are graphs, not tables, at this frequency; the line-by-line calculation (Annex 1, ~44 oxygen + water-vapor lines) was not run in this session. Commonly-cited rule of thumb in the propagation literature is that combined gaseous attenuation below 10 GHz is on the order of **0.01–0.02 dB/km**, but this specific number is **not sourced from the cache and should be treated as indicative only, not authoritative**, until computed directly from Annex 1. | `pdf/itu_p676.txt` |

### E.4 Weather — overall conclusion for the simulator

Rain, fog, and gaseous absorption are all **negligible at 5.9 GHz** over the distance scales relevant
to V2X (tens to hundreds of meters): rain contributes a fraction of a dB even in extreme downpours,
and both fog and gaseous absorption are explicitly stated by ITU-R to only become relevant at
≥60–100 GHz. **A physically honest V2X simulator can treat weather as a near-zero-impact term at
5.9 GHz** — the dominant loss/impairment mechanisms are shadowing (§C), vehicle/building
obstruction (§D), and fast fading (§B), not weather. This matches the general engineering consensus
in the DSRC/802.11p literature (no cached source claims a weather-dominated V2X link budget at 5.9 GHz).

---

## F. Antennas and receiver

### F.1 Receiver sensitivity per MCS, 10 MHz channel (802.11p/OCB)

ETSI EN 302 663 V1.3.1 Table 1 — Static receiver sensitivity (10 MHz channel spacing), which
mirrors the IEEE 802.11-2016 OFDM PHY sensitivity requirements for half-clocked (10 MHz) operation:

| Data rate [Mbit/s] | Modulation | Coding rate | Min. sensitivity [dBm] |
|---|---|---|---|
| 3 | BPSK | 1/2 | −91 |
| 4.5 | BPSK | 3/4 | −90 |
| 6 | QPSK | 1/2 | −88 |
| 9 | QPSK | 3/4 | −86 |
| 12 | 16-QAM | 1/2 | −83 |
| 18 | 16-QAM | 3/4 | −79 |
| 24 | 64-QAM | 2/3 | −75 |
| 27 | 64-QAM | 3/4 | −74 |

Source: `pdf/etsi_en302663.txt` lines 384-393 (Table 1). Dynamic (interference-present) sensitivity
is 3 dB higher at each rate (Table 2, line 401: 6 Mbps/QPSK/1/2 → −85 dBm).

Real product cross-check — Cohda MK5 Module receive sensitivity, 5.9 GHz, 10 MHz DSRC (Table 2,
"No Multipath" 1-antenna / typical): BPSK 1/2: **−98 dBm**; BPSK 3/4: −96; QPSK 1/2: −95; QPSK
3/4: −93; 16-QAM 1/2: −90; 16-QAM 3/4: −86; 64-QAM 2/3: −82; 64-QAM 3/4: −80 dBm (`pdftxt/cohda_mk5_module_datasheet.txt`
lines 218-230) — i.e. commercial hardware **beats the ETSI minimum by ~5–7 dB**, and degrades by
**3–7 dB under a synthetic "Highway NLoS" multipath channel** (same table, NLoS columns).

### F.2 Noise figure & thermal noise floor

| Parameter | Value | Source |
|---|---|---|
| Thermal noise floor, 10 MHz BW | `N = kT₀B → N[dBm] = −174 dBm/Hz + 10log10(10×10⁶) = −174+70 = −104 dBm` | Standard `kTB` formula (T₀=290 K, Boltzmann k=1.38×10⁻²³ J/K); numerically reproduces the task's given −104 dBm constant |
| Typical V2X chipset noise figure | **6 dB** (5.9 GHz) | NXP SAF5400 factsheet, `pdftxt/nxp_saf5400_factsheet.txt` line 74 |
| Implied receiver noise floor (NF=6 dB, B=10 MHz) | −104 + 6 = **−98 dBm** | Derived from the two rows above |

### F.3 Antenna gain / height

| Parameter | Value | Source |
|---|---|---|
| Typical OBU DSRC antenna | 5 dBi omni dipole (×2, diversity) | Unex OBU-301E/OBU-351U info sheets, `pdftxt/unex_obu301e.txt` line 129 |
| Typical OBU GNSS antenna | Active patch, separate FAKRA-C connector | same, lines 108-110 |
| Typical OBU/vehicle antenna height (simulation convention) | **1.5 m** | 3GPP TR 36.885 V2V assumption, `src/tr36885_raw.txt` (§A above) |
| RSU antenna height, 3GPP convention | **5 m** | `src/tr36885_raw.txt` |
| RSU antenna height, deployment guidance (qualitative) | "the height of the antenna over ground should be at least vehicle height… ideally… mounted much higher, e.g. on a mast arm or the top of the pole of a traffic light" — **no single fixed meter value given** | CTI 4501 v01.01 Connected Intersections Implementation Guide, `pdf_txt/cti4501.txt` lines 14034-14037 |

### F.4 Typical ranges achieved (field literature)

| Scenario | Range | Source |
|---|---|---|
| Simulator max-range calibration, LOS highway | 1000 m | `pdf/boban_realistic_efficient.txt` Table 4.5 |
| Simulator max-range calibration, LOS urban | 500 m | same |
| Simulator max-range calibration, NLOSv | 400 m | same |
| Simulator max-range calibration, NLOSb | 300 m | same |
| Field test, "long" car-following distance | 150 m, PDR ≈**0.5–0.6** under intermittent LOS obstruction | Martelli, Renda, Santi, "Measuring IEEE 802.11p Performance for Active Safety Applications…," `pdf/martelli_vtc2011.txt` lines 442-444 |
| Field test, "short" car-following distance | ≈25 m, PDR **≈1.0** (near-optimal, clear LOS) | same, lines 439-441 |
| Field test, obstructing truck between Tx/Rx | PDR dropped to **0** in one recorded case | same, line 486 |

---

## G. GNSS

### G.1 Open-sky accuracy — GPS SPS Performance Standard, 5th Ed. (April 2020)

| Metric | Standard (committed) | Source |
|---|---|---|
| Global Average Position Accuracy | **≤8 m 95% horizontal**, **≤13 m 95% vertical** | `pdf/gps_sps2020.txt` Table 3.8-3 |
| Worst-Site Position Accuracy | ≤15 m 95% horizontal, ≤33 m 95% vertical | same |
| Global Average Velocity Accuracy | ≤0.2 m/s, 95%, any axis | same |
| Time Transfer Accuracy | ≤30 ns, 95% (SIS only) | same |
| **Actually achieved (2018 real-world)** | **≈3 m horizontal, ≈5 m vertical, 95%**, "well-designed GPS receivers" | `pdf/gps_sps2020.txt` line 310-312 (Executive-summary statement) |

### G.2 Real-world highway measurement (Reid et al., Ford, ION GNSS+ 2019, 30,000 km NA highways)

| Positioning system | Lateral [m] 68/95/99% | Longitudinal [m] 68/95/99% | Horizontal [m] 68/95/99% | Vertical [m] 68/95/99% |
|---|---|---|---|---|
| Production automotive GNSS (single-freq) | 1.92 / 3.88 / 5.74 | 2.11 / 4.44 / 7.95 | 3.07 / 5.30 / 9.38 | 4.59 / 9.42 / 12.83 |
| OxTS RT3000 (multi-freq RTK reference) | 0.18 / 0.73 / 2.60 | 0.18 / 0.75 / 2.88 | 0.26 / 1.05 / 3.91 | 0.31 / 1.34 / 2.56 |

Source: `pdf/gnss_highway_rtk.txt` (Reid, Pervez, Ibrahim, Houts, Pandey, Alla, Hsia, "Standalone
and RTK GNSS on 30,000 km of North American Highways") Table (lines 288-291).

| Metric | Value | Source |
|---|---|---|
| Road-determination (<5 m) achieved | single-freq automotive GNSS: **98%** of the time | `pdf/gnss_highway_rtk.txt` conclusion, lines 683-688 |
| Lane-determination (<1.5 m) achieved | single-freq automotive GNSS: only **57%**; multi-freq RTK: **98%** | same |
| RTK-fixed availability (GPS+GLONASS L1+L2) | **50%** of the time (rest float/differential/SPS) | `pdf/gnss_highway_rtk.txt` lines 690-693 |
| SPS outage duration, 95th pct | **<7 s** | `pdf/gnss_highway_rtk.txt` line 700-702 |
| RTK fixed/float outage duration, 95th pct | **can exceed 1 minute** | same |

### G.3 Urban-canyon degradation (Hong Kong measurement studies, Wen & Hsu et al.)

| Condition | Mean error | Std dev | Max error | Availability / fix rate | Source |
|---|---|---|---|---|---|
| Standalone GNSS (u-blox, WLS), deep urban canyon | 31.02 m | 37.69 m | 177.59 m | 100% | `pdf/hk_gnss_nlos.txt` lines 1037-1041 |
| Same, + NLOS-exclusion WLS | 9.57 m | 7.32 m | <50 m | 96.01% | `pdf/hk_gnss_nlos.txt` lines 1047-1056, Table (line 1086) |
| Same, + correction+re-weighting (CR-WLS) | 9.01 m (2D) / 7.92 m | 5.27 m | — | ~100% | `pdf/hk_gnss_nlos.txt` lines 1062-1066 |
| Second urban canyon, standalone u-blox | 30.68 m | 25.14 m | 92.32 m | — | `pdf/hk_gnss_nlos.txt` lines 1160-1168 |
| GNSS-RTK, urban canyon (UrbanNav dataset) | 2D 1.81 m / 3D 3.65 m | 5.27 m (3D) | 55.59 m (3D) | **conventional RTK fix rate ≈14%** | `pdf/hk_gnss_rtk_nlos.txt` lines 1463-1465 |
| GNSS-RTK + proposed 3D-LiDAR-aided (3DLA) method, same dataset | 2D 0.36–1.76 m depending on variant | 0.15–1.44 m | — | **fix rate improved to ≈30%** | `pdf/hk_gnss_rtk_nlos.txt` lines 41-42, 1474-1494 |

Sources: W. Wen, L.T. Hsu et al. (Hong Kong PolyU), 3D-LiDAR-aided GNSS NLOS papers,
`pdf/hk_gnss_nlos.txt` and `pdf/hk_gnss_rtk_nlos.txt` (full text fetched this session).

### G.4 Positioning accuracy requirements

| Standard/requirement | Value | Source |
|---|---|---|
| **NHTSA V2V FMVSS requirement** (basis for SAE J2945/1) | position reported to accuracy of **1.5 m (1σ, 68%)**, "believed to provide lane-level information" | NHTSA/DOT, "Federal Motor Vehicle Safety Standards; V2V Communications," Docket NHTSA-2016-0126 (2017) — quoted directly in `pdf/gnss_highway_rtk.txt` lines 73-76 (Reid et al. cite it as their ref. [8]); local NPRM text cached at `nprm2017.txt` but the exact sentence was not located by string search in that 1M-line extraction — **cited via the Reid et al. secondary quotation, cross-checked against the general WebSearch record of SAE J2945's "1.5 m @ 68%" figure** |
| Road determination | <5 m | `pdf/gnss_highway_rtk.txt` lines 38-39 |
| Which-lane (V2X/ADAS) | <1.5 m, 1 s update | `pdf/gnss_highway_rtk.txt` lines 61-64 |
| Where-in-lane (highway / city) | <0.5 m / <0.3 m, integrity 10⁻⁸/hr | `pdf/gnss_highway_rtk.txt` lines 65-79 |
| 3GPP Release 18 sidelink positioning (separate, cellular-based, not GNSS) | lane-level: <1.5 m horiz. / <3 m vert., 90% of time; sub-meter tier: <0.5 m / <2 m, 90% | "Vehicular Wireless Positioning – A Survey," arXiv:2601.20547 (fetched this session), lines 1170-1178 |

### G.5 Temporal correlation models (Gauss-Markov) used in simulators

| Parameter | Value | Source |
|---|---|---|
| Generic 1st-order Gauss-Markov position-error process | `ẋ = −(1/τ)x + w`; autocorrelation `R(Δt) = σ²·e^(−|Δt|/τ)` | Standard GNSS-error simulation formulation (general GNSS/INS literature; τ = correlation time, σ = steady-state std dev) |
| MATLAB/Simulink `gpsSensor` object (Automated Driving/UAV/Sensor Fusion Toolboxes) | Position noise modeled as 1st-order Gauss-Markov; **default HorizontalPositionAccuracy = 1.6 m**, **default VerticalPositionAccuracy = 3 m**, **default DecayFactor = 0.999** (discrete-time AR(1) coefficient; 0=white noise, 1=random walk) | mathworks.com/help/uav/ref/gpssensor-system-object.html (fetched this session) |
| Note | This is a widely-used, concrete "simulator default" for the Gauss-Markov GNSS error model; it is **not** the same thing as the classic Selective-Availability Gauss-Markov dither model (τ≈127 s) from the pre-2000 GPS literature, which some VANET papers historically re-used as a generic bias generator even after SA was turned off — **that SA-era 127 s figure could not be independently re-verified in this session's cache/fetches and should be treated as a well-known-but-unverified-here legacy convention if used.** | — |

### G.6 Jamming / spoofing

| Parameter | Value | Source |
|---|---|---|
| Effective spoofing range (SDR-based attack, Monte-Carlo, HackRF One) | **≈91–547 m**; beyond ≈4.8 km, ~50% of simulated attack scenarios lose the spoofing link | "GNSS Spoofing Threat for V2X communications," arXiv:2606.20215v1 (fetched this session) |
| Civilian GPS vulnerability | Civilian C/A-code GPS has no signal authentication and publicly-known spreading codes → trivially spoofable; jamming hardware available for <$50 | same source, general finding (WebSearch summary); no cached primary DHS/FCC report was directly fetched this session — **treat the "<$50" figure as UNVERIFIED pending a primary-source check** |
| Mitigation in GNSS domain | Galileo OSNMA (Open Service Navigation Message Authentication) — "still at a very early stage" for V2X deployment | same arXiv source |

### G.7 Time sync / oscillator holdover (when GNSS is lost)

| Parameter | Value | Source |
|---|---|---|
| Automotive-grade (AEC-Q100) TCXO frequency stability | **±0.5 to ±5.0 ppm** over −40 to +85 °C (grade G3) or −40 to +105 °C (grade G2) | TXC Corporation automotive TCXO product page, txccorp.com/en/product/crystal-oscillators/tcxo/ (fetched this session) |
| PPS-disciplined OCXO stability | **≤0.5 ppb** (some models **≤0.1 ppb**) peak-to-peak | Rakon PPS-OCXO product page, rakon.com/products/families/ocxo-ocso/pps-ocxo (fetched this session) |
| OCXO holdover phase-error spec | **≤1.5 µs** phase error over 24–48 hour holdover, disciplined via 1PPS from a GNSS module | same |
| 1PPS GNSS timing input, real V2X hardware | Present as standard I/O on commercial OBUs/chipsets (e.g., "One 1PPS input" on Unex OBU-301E; "1PPS GPS input for channel synchronization" on Autotalks CRATON) | `pdftxt/unex_obu301e.txt` line 95; `pdftxt/autotalks_craton_datasheet.txt` line 283 |
| GPS SPS Time Transfer Accuracy standard (relates to what a 1PPS derived from GNSS can be trusted to) | ≤30 ns, 95% (SIS only, see §G.1) | `pdf/gps_sps2020.txt` Table 3.8-3 |
| Qualitative consequence | A plain automotive TCXO (≈1–5 ppm) free-running after GNSS loss accumulates on the order of **1–5 µs of clock error per second** (1 ppm = 1 µs/s), i.e. holdover of more than a few seconds without discipline can already exceed the ~30 ns SIS-level time-transfer budget by 2 orders of magnitude; an OCXO (≈0.1–0.5 ppb) is 3–4 orders of magnitude better and is the appropriate choice for any RSU/roadside infrastructure requiring sustained holdover. | Derived from the ppm/ppb figures above; not itself a cited number, shown for design context only |

---

## H. Validation curves for a PDR/CBR simulator test suite

### H.1 PDR vs. distance — field measurements

| Source | Scenario | Approx. values | Notes |
|---|---|---|---|
| Martelli, Renda, Santi (IIT-CNR Pisa), "Measuring IEEE 802.11p Performance for Active Safety Applications in Cooperative Vehicular Systems" | Car-following, ~25 m (LOS-dominant) | **PDR ≈ 0.95–1.0** | `pdf/martelli_vtc2011.txt` lines 439-441; 100/500-byte beacons, 3/6 Mbps, 100 ms period |
| same | Car-following, ~150 m (intermittent LOS obstruction) | **PDR ≈ 0.5–0.6** | `pdf/martelli_vtc2011.txt` lines 442-449; degradation attributed mainly to LOS obstruction events, not SNR/distance alone |
| same | Obstructing truck between Tx/Rx | **PDR → 0** | `pdf/martelli_vtc2011.txt` line 486 |
| Bai & Krishnan, "Reliability Analysis of DSRC Wireless Communication for Vehicle Safety Applications," IEEE ITSC 2006, pp. 355-362 | Open-field & freeway field trial, PDR vs. distance, consecutive-packet-loss analysis | **UNVERIFIED numeric curve** — paper's existence, title, venue, and page numbers confirmed via WebSearch bibliographic record, but the full text (and therefore its actual PDR-vs-distance numbers) was not retrievable in this session (no cache hit, no accessible open copy found) | Bibliographic record only |
| Sommer, Eckhoff, German, Dressler, WONS 2011 | Measured/modeled **RSS** vs. distance (Figs. 6-9), not PDR directly | See §C.1 for the β/γ shadowing values that directly parameterize a PDR-vs-distance curve once combined with a receiver-sensitivity threshold (§F.1) and Nakagami-*m* fading (§B) | `txt/sommer-shadowing.txt` |
| Boban thesis (calibration ranges, usable as a PDR "cliff-edge" validation target) | r_LOS-highway=1000 m / r_LOS-urban=500 m / r_NLOSv=400 m / r_NLOSb=300 m taken as the assumed PDR≈0 cutoff per link type | `pdf/boban_realistic_efficient.txt` Table 4.5 | See §D.3 |

**Suggested validation test construction:** build the simulator's PDR(d) curve from (i) the
ETSI/Cohda sensitivity threshold (§F.1), (ii) Sommer's β/γ obstacle loss or Abbas's LOS/OLOS
dual-slope model (§A.5/§C.1) for the mean, and (iii) Nakagami-*m* or log-normal shadowing (§A.2,
§B) for the outage probability at each distance; then check the resulting curve reproduces
**PDR≈1 at ≤25 m, PDR≈0.5–0.6 at ~150 m under partial obstruction, and PDR→0 by the
300–1000 m link-type-dependent range** from the table above.

### H.2 CBR vs. vehicle density / DCC state machine

ETSI TS 102 687 V1.2.1 defines the authoritative CBR state thresholds a simulator's DCC
implementation must reproduce (Annex A, two example Ton configurations):

| State | CBR range | Packet rate (config A) | Toff (config A) | Packet rate (config B) | Toff (config B) |
|---|---|---|---|---|---|
| Relaxed | <30% | 10 Hz | 100 ms | 20 Hz | 50 ms |
| Active 1 | 30–39% | 5 Hz | 200 ms | 10 Hz | 100 ms |
| Active 2 | 40–49% | 2.5 Hz | 400 ms | 5 Hz | 200 ms |
| Active 3 | 50–60% (config A) / 50–65% (config B) | 2 Hz | 500 ms | 4 Hz | 250 ms |
| Restrictive | >60% | 1 Hz | 1000 ms | — | — |

Source: `txt/ts102687.txt` lines 363-378 (Tables A.1/A.2).

| Parameter | Value | Source |
|---|---|---|
| Adaptive-approach CBR target | `CBR_target = 0.68` | `txt/ts102687.txt` line 338 |
| Lower bound on medium-usage fraction (anti-starvation) | `δ_min = 0.0006` | `txt/ts102687.txt` line 340 |
| CBR measurement interval | `T_CBR = 100 ms` (δ updated every 2×T_CBR) | `txt/ts102687.txt` line 343 |
| Empirical CBR-vs-vehicle-density curve (numeric veh/km ↔ CBR% pairs) | **UNVERIFIED** — ETSI TR 101 612 (`txt/tr101612.txt`) describes the DCC framework and CBR mechanism in detail but no explicit density-vs-CBR measurement table was found in the cached extraction; a secondary source (ResearchGate figure "Channel busy ratio (CBR) over vehicle density," DCC Plain/RORA/Advanced/EDCA comparison) exists but returned HTTP 403 and could not be fetched this session. | `txt/tr101612.txt`; ResearchGate fig. reference (inaccessible) |

**Suggested validation test construction:** drive the simulator at increasing vehicle densities
with fixed beacon size/rate, measure CBR, and check that (i) the reactive DCC state machine
transitions match the Relaxed/Active-1/2/3/Restrictive thresholds above at the correct CBR
crossing points, and (ii) an adaptive-DCC implementation converges its offered load so that
steady-state CBR tracks `CBR_target ≈ 0.68` rather than drifting to saturation (~1.0, the
uncontrolled-EDCA case) or over-throttling (well below 0.55).

---

## Source list (full citations, cache-path or URL)

- **[KAREDAL11]** J. Karedal, N. Czink, A. Paier, F. Tufvesson, A. Molisch, "Path Loss Modeling for Vehicle-to-Vehicle Communications," IEEE Trans. Veh. Technol. 60(1):323-328, Jan 2011. `pdf/karedal2011.txt`; alt. mirror `https://wides.usc.edu/Updated_pdf/Path%20loss%20modeling%20for%20vehicle-to-vehicle%20communications.pdf` (fetched, same table-extraction gap).
- **[ABBAS15]** T. Abbas, K. Sjöberg, J. Karedal, F. Tufvesson, "A Measurement Based Shadow Fading Model for Vehicle-to-Vehicle Network Simulations," Int. J. Antennas Propag. 2015 / arXiv:1203.3370v5. `pdf/abbas2015.txt`.
- **[CHENG07]** L. Cheng, B.E. Henty, D.D. Stancil, F. Bai, P. Mudalige, "Mobile Vehicle-to-Vehicle Narrow-Band Channel Measurement and Characterization of the 5.9 GHz DSRC Frequency Band," IEEE JSAC 25(8):1501-1516, 2007. Primary text unavailable (404 in cache and via direct fetch this session); cited secondhand per §A.3/§B notes.
- **[SOMMER11]** C. Sommer, D. Eckhoff, R. German, F. Dressler, "A Computationally Inexpensive Empirical Model of IEEE 802.11p Radio Shadowing in Urban Environments," WONS 2011. `txt/sommer-shadowing.txt`.
- **[BOBAN-JSAC11]** M. Boban, T.T.V. Vinhoza, J. Barros, M. Ferreira, O.K. Tonguz, "Impact of Vehicles as Obstacles in Vehicular Ad Hoc Networks," IEEE JSAC 29(1):15-28, 2011. Quoted via [BOBAN-THESIS]/[BOBAN-TVR].
- **[BOBAN-THESIS]** M. Boban, "Realistic and Efficient Channel Modeling for Vehicular Networks," PhD thesis / arXiv:1405.1008. `pdf/boban_realistic_efficient.txt`.
- **[BOBAN-TVR]** M. Boban, R. Meireles, J. Barros, P. Steenkiste, O.K. Tonguz, "TVR – Tall Vehicle Relaying in Vehicular Networks." `pdf/boban_tvr.txt`.
- **[MEIRELES10]** R. Meireles, M. Boban, P. Steenkiste, O.K. Tonguz, J. Barros, "Experimental study on the impact of vehicular obstructions in VANETs," IEEE VNC 2010, pp. 338-345. Quoted via [BOBAN-TVR] ref. [11].
- **[TR37885]** 3GPP TR 37.885 V15.3.0, "Study on evaluation methodology of new V2X use cases for LTE and NR." `pdf/tr37885.txt`.
- **[TR36885]** 3GPP TR 36.885, V2V evaluation methodology (antenna heights, D_corr). `src/tr36885_raw.txt`.
- **[ITU-P526]** ITU-R P.526-14, "Propagation by diffraction." `pdf/itu_p526c.txt`. (Current edition P.526-15 per itu.int/rec/R-REC-P.526.)
- **[ITU-P838]** ITU-R P.838-3, "Specific attenuation model for rain for use in prediction methods." `pdf/itu_p838.txt`.
- **[ITU-P840]** ITU-R P.840-8, "Attenuation due to clouds and fog." `pdf/itu_p840.txt`.
- **[ITU-P676]** ITU-R P.676-12, "Attenuation by atmospheric gases." `pdf/itu_p676.txt`.
- **[EN302663]** ETSI EN 302 663 V1.3.1 (2020-01), "ITS-G5 Access layer specification." `pdf/etsi_en302663.txt`.
- **[TS102687]** ETSI TS 102 687 V1.2.1 (2018-04), "Decentralized Congestion Control Mechanisms." `txt/ts102687.txt`.
- **[TR101612]** ETSI TR 101 612 V1.1.1 (2014-09), "Report on Cross Layer DCC." `txt/tr101612.txt`.
- **[NXP-SAF5400]** NXP SAF5400 V2X chipset factsheet. `pdftxt/nxp_saf5400_factsheet.txt`.
- **[COHDA-MK5MOD]** Cohda Wireless MK5 Module Datasheet V1.2.0. `pdftxt/cohda_mk5_module_datasheet.txt`.
- **[UNEX-OBU301E]** Unex OBU-301E Information Sheet. `pdftxt/unex_obu301e.txt`.
- **[AUTOTALKS-CRATON]** Autotalks CRATON product brief. `pdftxt/autotalks_craton_datasheet.txt`.
- **[GPS-SPS20]** GPS Standard Positioning Service Performance Standard, 5th Ed., April 2020 (gps.gov). `pdf/gps_sps2020.txt`.
- **[REID19]** T.G.R. Reid, N. Pervez, U. Ibrahim, S.E. Houts, G. Pandey, N.K.R. Alla, A. Hsia, "Standalone and RTK GNSS on 30,000 km of North American Highways," ION GNSS+ 2019. `pdf/gnss_highway_rtk.txt`.
- **[NHTSA17]** NHTSA/DOT, "Federal Motor Vehicle Safety Standards; V2V Communications," Docket NHTSA-2016-0126, RIN 2127-AL55, Federal Register 2017. Cache: `nprm2017.txt` (exact sentence not located by search); quoted via [REID19].
- **[WEN-HK-NLOS]** W. Wen, L.T. Hsu et al., 3D-LiDAR-aided GNSS NLOS mitigation, Hong Kong PolyU. `pdf/hk_gnss_nlos.txt`.
- **[WEN-HK-RTK]** W. Wen et al., 3D-LiDAR-aided GNSS-RTK NLOS mitigation, UrbanNav dataset, Hong Kong PolyU. `pdf/hk_gnss_rtk_nlos.txt`.
- **[MATLAB-GPSSENSOR]** MathWorks `gpsSensor` System Object documentation. `https://www.mathworks.com/help/uav/ref/gpssensor-system-object.html`.
- **[TXC-TCXO]** TXC Corporation automotive TCXO product page. `https://www.txccorp.com/en/product/crystal-oscillators/tcxo/`.
- **[RAKON-OCXO]** Rakon PPS Disciplined OCXO product page. `https://www.rakon.com/products/families/ocxo-ocso/pps-ocxo`.
- **[SPOOF-V2X]** "GNSS Spoofing Threat for V2X communications," arXiv:2606.20215v1, 2026.
- **[3GPP-REL18-POS]** "Vehicular Wireless Positioning – A Survey," arXiv:2601.20547, 2026 (Sec. on 3GPP Release 18 sidelink positioning targets).
- **[MARTELLI12]** F. Martelli, M.E. Renda, P. Santi, "Measuring IEEE 802.11p Performance for Active Safety Applications in Cooperative Vehicular Systems." `pdf/martelli_vtc2011.txt`.
- **[BAI-KRISHNAN06]** F. Bai, H. Krishnan, "Reliability Analysis of DSRC Wireless Communication for Vehicle Safety Applications," IEEE ITSC 2006, pp. 355-362. Bibliographic record only (full text not retrieved this session).
- **[TALIWAL04]** V. Taliwal, D. Jiang, H. Mangold, C. Chen, R. Sengupta, "Empirical Determination of Channel Characteristics for DSRC Vehicle-to-Vehicle Communication," VANET '04. Bibliographic record only (full text not retrieved this session).
- **[TORRENT-MORENO09]** M. Torrent-Moreno, J. Mittag, P. Santi, H. Hartenstein, "Vehicle-to-Vehicle Communication: Fair Transmit Power Control for Safety-Critical Information," IEEE TVT 58(7), 2009. `pdf/torrent_moreno_tvt09.txt`.
- **[YIN-DSRC]** J. Yin, G. Holland, T. ElBatt, F. Bai, H. Krishnan, "DSRC Channel Fading Analysis from Empirical Measurement." Quoted via `pdf/alpha_mu_dsrc.txt` ref. [37].
- **[CTI4501]** CTI 4501 v01.01, "Connected Intersections Implementation Guide." `pdf_txt/cti4501.txt`.
- **[TR103257]** ETSI TR 103 257-1 V1.1.1 (2019-05), "Channel Models for the 5,9 GHz frequency band." `txt/tr103257-1.txt`.
- **[NS3-TWORAY]** ns-3 `TwoRayGroundPropagationLossModel` source. `pdf/ns3_propagation_loss_model.cc`.
- **[VEINS-TWORAY]** Veins `TwoRayInterferenceModel` source (S. Joerer, 2011). `pdf/veins_tworay.cc`, `pdf/veins_phy.ned`.
- **[ALPHA-MU]** "Composite α-μ Based DSRC Channel Model Using Large Data Set of RSSI Measurements," arXiv:1808.00509. `pdf/alpha_mu_dsrc.txt`.

---

## Summary of UNVERIFIED items (do not hard-code into the simulator without further confirmation)

1. Karedal et al. 2011 Table I exact numeric values (n, σ1, σ2, PL0, G12, PLc, h) per environment — extraction failed across 2 independent PDF mirrors.
2. Cheng et al. 2007 n2 (post-breakpoint exponent) and σ1/σ2 values — primary source unreachable (404 everywhere tried); only n1/breakpoint survived via secondary citation.
3. Cheng et al. 2007's own Nakagami-*m* vs. distance values — not recoverable; the m≈1–1.8/0.7–1 figure found in cache is from Yin et al., a different paper, not Cheng.
4. Taliwal/Torrent-Moreno (VANET'04) exact distance-dependent Nakagami-m thresholds (m=3 <50m / 1.5 for 50–150m / 1 beyond) — could not verify; only the paper's existence and its later reuse (fixed m=1/3/5, not distance-binned) by Torrent-Moreno et al. 2009 was confirmed.
5. Rician K-factor for 5.9 GHz V2V — no numeric value found anywhere in the cache.
6. Veins `TwoRayInterferenceModel` default ε_r and `SimpleObstacleShadowing` default β/γ as shipped in the Veins framework itself (as opposed to the Sommer et al. paper's fitted values) — source `.ned` files not in cache.
7. ITU-R P.840 K_l(5.9 GHz) exact numeric value — formula verified, number not computed (would need the full double-Debye real-part equation, not fully recovered from extracted text).
8. ITU-R P.676 γ(5.9 GHz) exact numeric value — formula/structure verified, line-by-line computation not performed.
9. Bai & Krishnan (2006) PDR-vs-distance numeric curve — bibliographic record only, full text unavailable.
10. Empirical CBR-vs-vehicle-density numeric curve (as opposed to the ETSI TS 102 687 *threshold* table, which is fully verified) — secondary source found but inaccessible (HTTP 403).
11. NHTSA NPRM exact sentence establishing the "1.5 m @ 68%" J2945/1-adjacent requirement — the number itself is corroborated by two independent secondary sources (Reid et al. 2019, general web record) but the primary Federal Register text was not pinpointed in the cached `nprm2017.txt`.
12. "<$50 GPS jammer" and general DHS/FCC threat-assessment claims in §G.6 — WebSearch-summary level only, no primary report fetched.
