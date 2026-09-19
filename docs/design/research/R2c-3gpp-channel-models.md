# R2c — 3GPP V2X evaluation channel models (TR 36.885 V14.0.0 Annex A; TR 37.885 V15.3.0 §6)

Sources verified from the cached documents (36885-e00.doc byte-level; ATIS reprint of TR 37.885 V15.3.0, 2019-06).

## Path loss (TR 37.885 §6.2.1 Table 6.2.1-1; fc in GHz, d3D in m)
| Scenario/state | PL (dB) | σ_SF |
|---|---|---|
| Highway LOS or NLOSv | 32.4 + 20 log10(d3D) + 20 log10(fc) | 3 dB |
| Urban LOS or NLOSv | 38.77 + 16.7 log10(d3D) + 18.2 log10(fc) | 3 dB |
| Urban NLOS (different streets) | 36.85 + 30 log10(d3D) + 18.9 log10(fc) | 4 dB |
TR 36.885 Table A.1.4-1 instead names WINNER+ B1 (Manhattan grid, antenna 1.5 m; PL at 3 m used below 3 m); its constants are in the WINNER II D1.1.2 report (not cached, UNVERIFIED here). Carrier frequency in both TRs' assumptions is 6 GHz as a proxy for 5.9 GHz (TR 36.885 Table A.1.1-1 note; TR 37.885 Table 6.1.1-1).

## LOS / NLOSv probability (TR 37.885 Table 6.2-1)
- Urban: P(LOS) = min{1, 1.05·exp(−0.0114·d)}; P(NLOSv) = 1 − P(LOS) for same-street links; NLOS = different streets (geometric).
- Highway: d ≤ 475 m: P(LOS) = min{1, 2.1013e−6·d² − 0.002·d + 1.0193}; d > 475 m: P(LOS) = max{0, 0.54 − 0.001·(d − 475)}; no NLOS state on highway.
- States re-evaluated at every 100 ms location update (§6.2).

## NLOSv additional blockage loss (TR 37.885 §6.2.1, v15.3.0 after CRs RP-182530 and RP-191274)
max{0 dB, log-normal}: Case 1 (min antenna height of Tx/Rx > blocker height): 0; Case 2 (max antenna height < blocker height): mean 9 + max(0, 15 log10(d) − 41) dB, σ 4.5 dB; Case 3 (otherwise): mean 5 + max(0, 15 log10(d) − 41) dB, σ 4 dB. Not present in TR 36.885.

## Shadowing (TR 36.885 Annex A.1.4; TR 37.885 §6.2.2 refers to it)
Log-normal; urban σ 3 dB LOS / 4 dB NLOS, freeway 3 dB; decorrelation distance urban 10 m, freeway 25 m; spatial update S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1 − exp(−2D/D_corr))·N_S(n). NLOSv reuses the LOS shadowing model.

## Antennas, noise, power (TR 36.885 Table A.1.1-1; TR 37.885 §6.1.3)
- Antenna gain 3 dBi vehicle UE and UE-type RSU, 0 dBi pedestrian; antenna height 1.5 m (36.885); TR 37.885 vehicle types: Type 1 (5 × 2.0 × 1.6 m, antenna 0.75 m), Type 2 (5 × 2.0 × 1.6 m, antenna 1.6 m), Type 3 truck/bus (13 × 2.6 × 3 m, antenna 3 m).
- UE noise figure 9 dB (below 6 GHz); 13 dB baseline above 6 GHz.
- UE Tx power 23 dBm (33 dBm not precluded in 37.885).

## Vehicle drop and scenarios
- TR 36.885 Table A.1.2-1: spatial Poisson drop; same-lane inter-vehicle distance mean = 2.5 s × speed; urban 2 lanes per direction (3.5 m), freeway 3 per direction (4 m); speeds urban 15 / 60 km/h, freeway 70 / 140 km/h; Manhattan grid 433 m × 250 m blocks, 3 m sidewalks, minimum area 1,299 × 750 m; location update 100 ms; pedestrian 3 km/h, antenna 1.5 m.
- TR 37.885 §6.1.2: bumper-to-bumper gap = max{2 m, Exp(mean = 2 s × speed)}; highway Option A 140 km/h (70 optional), Option B per-lane 80/100/140/40/30/20 km/h, Option C clustered Type-3 platoons (6 per cluster, 2 m gap); urban Option A 60 km/h, Option B E–W lanes 60/50/25/15 km/h; street width 20 m (Annex A Fig. A-2).

## Other
- eNB–UE (Uu) PL (TR 36.885 Table A.1.4-2): 128.1 + 37.6 log10(R[km]); σ 8 dB; decorrelation 50 m; SCM NLOS fast fading.
- Infrastructure (B2V/B2R) in TR 37.885 Table 6.2.1-2 reuses TR 38.901 UMa (urban) and RMa (highway) with hE = 0.25 m (38.901 not cached).
