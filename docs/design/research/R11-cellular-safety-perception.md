# R11 — Cellular Uu/Safety-Apps/Perception/Jamming/Misbehavior Fact Sheet

Access date for all web sources: **2026-09-18** (America, WebSearch/WebFetch tools). Cache sources are files under
`scratchpad/research/` (this session's research cache). `UNVERIFIED` = no primary or reliable secondary source was
found in the cache or via WebFetch/WebSearch this session; do not treat as a design input without further sourcing.
No value below was invented — every row is either quoted/derived from a cited document or explicitly flagged.

---

## Topic A — Cellular Uu (V2N) and backhaul

### A1. Measured LTE/5G latency — U.S. drive tests (Narayanan et al., "A First Look at Commercial 5G Performance on Smartphones," WWW'20)

| Item | Value | Source |
|---|---|---|
| 5G (Verizon mmWave) base RTT, first hop | 27.4 ± 6.4 ms | firstlook5g.txt Table 2; DOI [10.1145/3366423.3380169](https://doi.org/10.1145/3366423.3380169) |
| 4G LTE base RTT, first hop (same setup) | 29.2 ± 4.8 ms | firstlook5g.txt Table 2 |
| 5G base RTT, total, east-coast server | 54.0 ± 4.5 ms | firstlook5g.txt Table 2 |
| 4G base RTT, total, east-coast server | 58.0 ± 4.3 ms | firstlook5g.txt Table 2 |
| 5G base RTT, total, west-coast server | 81.9 ± 5.5 ms | firstlook5g.txt Table 2 |
| 4G base RTT, total, west-coast server | 88.9 ± 5.5 ms | firstlook5g.txt Table 2 |
| 5G packet-loss percentiles (VZ mmWave, stationary LoS) | 50th/75th/99th = 0.01% / 0.1% / 1.2% | firstlook5g.txt §5.1 |
| Median TCP throughput @ 8 parallel conns, 5G vs 4G | 1467 Mbps vs 167 Mbps | firstlook5g.txt §5.1, Fig. 6 |
| Handoffs during an 8-min urban walk (~5 km/h) | 31 primitive handoffs, 13 4G↔5G bounces, throughput 0–954 Mbps | firstlook5g.txt §6.1 |
| 4G→5G "ready" downgrade trigger (P1) | traffic inactivity for ~10 s | firstlook5g.txt §6.1 |
| Driving test (20–50 km/h), 3 US mmWave/mid-band carriers | Frequent throughput drops to ~0 from handoffs/blockage; mid-band (Sprint) more stable, mmWave (VZ/T-Mobile) more volatile | firstlook5g.txt §6.1, Fig. 11 |

### A2. Remote-driving latency, ITS-G5 vs cellular (MASA Living Lab, Cauchi et al. 2026)

| Item | Value | Source |
|---|---|---|
| DSRC (ITS-G5) vs 5G packet-arrival race | DSRC packet arrives first in 96.7% of paired transmissions (within RSU coverage) | masa.txt §IV.A; arXiv:[2606.13292](https://arxiv.org/abs/2606.13292) |
| Typical one-hop DSRC latency (cited, added to G2G budget) | ~2–3 ms | masa.txt §IV.E |
| Typical one-hop 5G latency (cited, added to G2G budget) | ~18–20 ms | masa.txt §IV.E |
| Glass-to-glass (sensor→display) latency, majority of samples | 150–160 ms | masa.txt §IV.E, Fig. 6 |
| DSRC coverage PDR (within RSU corridor) | ~100% (quasi-binary in/out of coverage) | masa.txt §IV.D |
| 5G coverage PDR | near 100% typical, localized drops to ~90% in dense urban canyons/intersections | masa.txt §IV.D |
| Bitrate/packet "late-or-lost" window used for TX/RX correlation | 0–100 ms | masa.txt §IV.C |

### A3. 5G V2N/V2N2V end-to-end latency model (Coll-Perales et al., IEEE TVT 2022)

| Item | Value | Source |
|---|---|---|
| V2X service requirement — Low Level of Automation (LLoA) | max E2E latency 25 ms, reliability 90% | e2ev2x.txt Table III; arXiv:[2201.06082](https://arxiv.org/abs/2201.06082) (DOI 10.1109/TVT.2022.3224614) |
| V2X service requirement — High LoA (HLoA) | max E2E latency 10 ms, reliability 99.99% | e2ev2x.txt Table III |
| Radio UL+DL latency, Low LoA, low load | 2.00 ms | e2ev2x.txt Table IV |
| Radio UL+DL latency, High LoA, load-dependent | 2.60–4.55 ms | e2ev2x.txt Table IV |
| Transport-network (TN) latency, MEC@gNB, α=0.1 | mean 0.402 ms; 99.99th pct 0.422 ms | e2ev2x.txt Table VI |
| TN latency, MEC@M1, α=0.1 | mean 0.835 ms; 99.99th pct 0.875 ms | e2ev2x.txt Table VI |
| TN latency, MEC@CN / Centralized, α=0.1 | mean 2.355 ms; 99.99th pct 2.396 ms | e2ev2x.txt Table VI |
| TN latency, MEC@M1, α=0.001 (undersized capacity) | 99.99th pct blows up to ~10.3 ms | e2ev2x.txt §VI.B |
| Core-network (CN) latency, Centralized, α=0.01–0.1 | mean ≈2.0000–2.0006 ms (dominated by 2 ms propagation, 200 km optical CN) | e2ev2x.txt Table VII |
| Internet latency (Centralized deployment only) | 90th pct 21 ms; 99.99th pct 43 ms | e2ev2x.txt §VIII |
| Peering-point latency, remote (multi-MNO) | mean 13.0 ms; 90th pct 29.9 ms; 99.99th pct 99.2 ms | e2ev2x.txt Table VIII |
| Peering-point latency, local (multi-MNO) | mean 0.306 ms; 90th pct 0.431 ms; 99.99th pct 1.493 ms | e2ev2x.txt Table VIII |
| V2X application-server (AS) latency, MEC@gNB, 2080→41600 pkt/s | 0.0027 → 0.0031 ms mean | e2ev2x.txt Table IX |
| V2X AS latency, Centralized, 2080→41600 pkt/s | 0.035–0.165 → 0.689–3.295 ms mean range | e2ev2x.txt Table IX |
| Min. processors to avoid AS queue backlog, Centralized @ 41600 pkt/s | 152 (vs 212 for MEC@CN, 1 for MEC@gNB/M1) | e2ev2x.txt Table X |
| Queueing abstraction used for TN/CN nodes | **M/M/1** per node (Poisson arrivals, exponential service, ρ=λ/µ<1 required); multi-node transit delay via **Jackson's theorem** | e2ev2x.txt §VI, citing refs [24],[25] therein |

### A4. 3GPP/ETSI evaluation-methodology assumptions — ISD, bandwidth, Tx power

| Item | Value | Source |
|---|---|---|
| TR 37.885 (NR V2X eval. methodology) urban ISD | Macro 500 m | tr37885.txt Table 6.1.3-1; 3GPP TR 37.885, ETSI-hosted mirror pattern `www.etsi.org/deliver/etsi_tr/137800_137899/137885/...` (not independently re-fetched this session) |
| TR 37.885 highway ISD | Macro 1732 m (500 m optional) | tr37885.txt Table 6.1.3-1/2 |
| TR 37.885 aggregated system BW, <6 GHz | up to 200 MHz (DL+UL), up to 100 MHz (SL) | tr37885.txt Table 6.1.1-1 |
| TR 37.885 aggregated system BW, >6 GHz | up to 1 GHz (DL+UL) and up to 1 GHz (SL) | tr37885.txt Table 6.1.1-2 |
| TR 37.885 Macro BS Tx power | 49 dBm (<6 GHz) / 43 dBm (>6 GHz, EIRP ≤78 dBm) | tr37885.txt Table 6.1.1-1/2 |
| TR 37.885 UE/RSU Tx power | 23 dBm (33 dBm "not precluded") | tr37885.txt Table 6.1.1-1 |
| TR 38.913 Highway scenario ISD | Macro 1732 m (500 m optional); inter-RSU 50 or 100 m | tr38913.txt Table 6.1.8-1; ETSI TR 138 913 V14.2.0, `www.etsi.org/deliver/etsi_tr/138900_138999/138913/14.02.00_60/tr_138913v140200p.pdf` (URL constructed from confirmed ETSI path pattern, not independently re-fetched) |
| TR 38.913 Urban-grid-for-connected-car ISD | Macro 500 m; RSU at each intersection (also 50/100 m option) | tr38913.txt Table 6.1.9-1 |
| TR 38.913 control-plane latency target | 10 ms | tr38913.txt §7.4 |
| TR 38.913 user-plane latency, URLLC | 0.5 ms UL, 0.5 ms DL | tr38913.txt §7.5 |
| TR 38.913 user-plane latency, eMBB | 4 ms UL, 4 ms DL | tr38913.txt §7.5 |
| TR 38.913 eV2X reliability/latency (300-byte packet) | reliability 1−10⁻⁵ at 3–10 ms latency, for direct sidelink or BS-relayed | tr38913.txt §7.9 |
| TR 38.913 URLLC generic reliability | 1−10⁻⁵ for 32 bytes at 1 ms user-plane latency | tr38913.txt §7.9 |
| TR 36.885 (LTE V2X eval. methodology, Rel-14) ISD baseline | **UNVERIFIED this session** — cached copy (`tr36885.txt`, 31 MB) is a corrupted/undecodable legacy `.doc` binary; `textutil`/pymupdf could not extract text. Cross-check: TR 37.885 explicitly cites TR 36.885 Annex A.1.3 figures for BS placement ("Baseline: Macro only ... BS placement as depicted in Figure A.1.3-1 in [13]", [13]=TR 36.885), implying the same 500 m urban / 1732 m highway ISD baseline carries over from LTE to NR V2X evaluation, but the exact TR 36.885 table was not independently read | tr37885.txt line ~90 (reference list), tr37885.txt lines 304/307/314 |

### A5. Handover interruption time / RLF (ETSI TS 136 133 spec limits; ns-3 5G-LENA implementation)

| Item | Value | Source |
|---|---|---|
| LTE intra/inter-frequency HO interruption time, target cell known | T_interrupt = T_IU + 20 ms (T_search = 0) | ts36133v8.txt §5.1.2.1.2; ETSI TS 136 133, `www.etsi.org/deliver/etsi_ts/136100_136199/136133/` (version-specific sub-path not independently re-fetched) |
| LTE intra/inter-frequency HO interruption, target cell unknown | adds T_search = 80 ms (if signal quality sufficient for first-attempt detection) | ts36133v8.txt §5.1.2.1.2 |
| RRC procedure delay on top of interruption time | +50 ms | ts36133v8.txt multiple sections (e.g., §5.3.1.1.2 context) |
| E-UTRA→UTRA HO interruption, target cell known | T_interrupt1 = T_IU + T_sync + 50 + 10·F_max ms | ts36133v8.txt §5.3.1.1.2 |
| E-UTRA→UTRA HO interruption, target cell unknown | T_interrupt2 = T_IU + T_sync + 150 + 10·F_max ms | ts36133v8.txt §5.3.1.1.2 |
| ns-3 5G-LENA handover architecture | X2-based, follows LTE architecture; X2 supports only "seamless (not lossless)" handover, **no dedicated handover-failure recovery** | lena5g.txt §2.x ("NR Module" docs, Release 5.0.0), https://5g-lena.cttc.es/ |
| ns-3 5G-LENA RLF handling | UE-side detection per TS 36.331/36.133 (T310 timer, out-of-sync counters on primary CC only); gNB-side RLF detection *not implemented* — gNB notified via direct SAP call instead | lena5g.txt §2.3.15 |
| ns-3 5G-LENA PDCP continuity during HO | sequence-number continuity preserved only for AM data radio bearers, source buffers while HANDOVER_LEAVING, target buffers while HANDOVER_JOINING | lena5g.txt §2.6.5 |

### A6. RSU backhaul — USDOT Connected Vehicle Pilot deployments

| Item | Value | Source |
|---|---|---|
| Tampa (THEA) initial RSU backhaul | 45 of 47 RSUs on **cellular**, interim carrier for areas without fiber | WebSearch: its.dot.gov/pilots/thea_cvp_wireless.htm; itskrs.its.dot.gov/2020-sc00466 |
| Tampa REL-corridor RSUs | Converted to **fiber** once available along the Reversible Express Lane | same |
| Tampa cellular backhaul cost evolution | $35/mo/RSU @ 5 GB (early) → $100/mo/RSU @ 20 GB (Oct 2018); ≈$4,500/mo total fleet cellular cost | itskrs.its.dot.gov/2020-sc00466 |
| Wyoming (WYDOT) I-80 backhaul | **Mix of fiber, microwave, and wireless**; satellite added for traveler-information dissemination outside DSRC coverage | WebSearch: rosap.ntl.bts.gov/view/dot/74648, /36648, /64854 (Connected Vehicle Pilot Deployment Program reports) |
| NYC CV Pilot scale | 470 RSUs + 3,000 vehicles (ASD) for V2V/V2I | WebSearch: c2smart.engineering.nyu.edu/nyc-connected-vehicle-pilot/ |
| NYC CV Pilot backhaul medium | UNVERIFIED this session (not found in accessible search snippets) | — |

### A7. MOSAIC Cell module vs ns-3 LENA/5G-LENA (coverage/delay/capacity modeling)

| Item | Value | Source |
|---|---|---|
| Eclipse MOSAIC Cell simulator architecture | Geocaster module (geo-aware addressing/routing) + UplinkModule + DownlinkModule | https://eclipse.dev/mosaic/docs/simulators/network_simulator_cell/ |
| MOSAIC delay models | `ConstantDelay` (fixed); `SimpleRandomDelay` (uniform, min/max + step count); `GammaRandomDelay` (Gamma dist., α=2, β=2, parameterized by expected delay); `GammaSpeedDelay` (adds velocity-based penalty) | same |
| MOSAIC example default delay | uplink ≈100 ms, downlink unicast ≈50 ms (example/default config) | same |
| MOSAIC example default capacity | global network cap 28 Mbps UL / 42.2 Mbps DL; per-application cap via `CellModuleConfiguration` (example default 100 Gbps) | same |
| MOSAIC region model | Custom geographic regions can override delay/loss/capacity (e.g., simulate poor-reception zones or congestion hotspots) | same |
| MOSAIC unicast loss/retry model | `LossProbability` + `maxRetries` parameters; multicast/MBMS uses single-attempt transmission with loss folded into delay | same |
| ns-3 5G-LENA modeling depth (contrast) | Full 3GPP-compliant NR PHY/MAC/RLC/PDCP/SDAP/RRC/NAS/EPC stack; 3GPP TR 38.901-based spatial channel model; per-resource-block effective-SINR-to-BLER mapping calibrated per MCS table; explicit beamforming, CSI-RS/CSI-IM, SRS models | lena5g.txt ("NR Module" documentation, Release 5.0.0, OpenSim CTTC/CERCA) |
| Practical distinction | MOSAIC Cell trades PHY fidelity for a lightweight, configurable delay/capacity/region abstraction suited to city-scale multi-domain co-simulation; 5G-LENA models the radio stack in detail but at higher simulation cost, suited to link/cell-level validation rather than city-scale traffic co-simulation | Synthesized from both docs above |

### A8. Ookla Speedtest Intelligence — U.S. mobile latency (2025–2026)

| Item | Value | Source |
|---|---|---|
| T-Mobile 5G median latency | 31 ms | techblog.comsoc.org/2025/12/16/ookla-fwa-speed-test-results-for-u-s-carriers-wireless-connectivity-performance-at-busy-airports/ (Ookla H2 2025 data) |
| Verizon 5G median latency | 32 ms | same |
| AT&T 5G median latency | 34 ms | same |
| AT&T Fixed Wireless Access (FWA) median multi-server latency | ≈67 ms (Q3 2025; improved from 78 ms in Q3 2024) | same |
| Verizon FWA median latency | 54 ms | same |
| T-Mobile FWA median latency | 50 ms | same |
| SFO airport Wi-Fi 6E (context/comparator, not cellular) | 8 ms multi-server latency, 364.74 Mbps median DL | same |
| p95 latency (mobile, US) | UNVERIFIED this session — Ookla source page did not report p95 figures, only medians | — |
| Ookla Research homepage (browsed, no numeric report) | Listing page only, no extractable latency figures | ookla.html (cached) |

---

## Topic B — Safety applications and thresholds

### B1. VSC-A / SAE-family application definitions and confirmed numeric triggers (CAMP VSC-A Final Report, DOT HS 811 492A)

| Item | Value | Source |
|---|---|---|
| EEBL definition | HV broadcasts a self-generated emergency-brake event; RV determines relevance and warns driver if appropriate | VSC-A Final Report, NHTSA DOT HS 811 492A, https://rosap.ntl.bts.gov/view/dot/43933 (fetched & text-extracted this session) |
| **EEBL hard-braking trigger (BSM Part II Event Flag)** | **deceleration ≥ 0.4 g** | VSC-A Final Report, BSM Part II "Event Flags" field description |
| ABS/stability-control/traction-control event-flag qualifying duration | activation for ≥100 ms | VSC-A Final Report, same section |
| FCW definition | warns HV driver of impending rear-end collision with RV ahead, same lane/direction of travel | VSC-A Final Report |
| BSW+LCW definition | warns during lane-change attempt if blind-spot zone is/will-be occupied; also gives advisory when not attempting a change | VSC-A Final Report |
| DNPW definition | warns during a passing maneuver if the passing zone is occupied by an oncoming vehicle | VSC-A Final Report |
| IMA definition | warns when unsafe to enter an intersection due to high collision probability; initial scope = stop-sign-controlled and uncontrolled intersections | VSC-A Final Report |
| CLW definition | HV broadcasts a self-generated control-loss event to surrounding RVs | VSC-A Final Report |
| VSC-A reference Forward-Looking Radar (FLR) minimum spec | 76 GHz; range 3–150 m (10 m² RCS); range rate −64 to +33 m/s; azimuth FOV ±7.5°; update rate 10 Hz | VSC-A Final Report Table 3 |
| Radar-only target re-acquisition delay after a lead-vehicle cut-out | ≈5 s before FLR re-acquires the revealed stationary vehicle (vs. continuous DSRC+positioning tracking) | VSC-A Final Report §3.3–3.4 |
| Track-test approach speeds used for FCW/EEBL/BSW/DNPW/IMA true-positive scenarios | 15–50 mph depending on scenario (e.g., FCW-T5 40 mph, IMA-T3 15/25/35/45 mph) | VSC-A Final Report, Test Scenario tables (EEBL-T1…T5, FCW-T1…T9, BSW/LCW-T1…T8, DNPW-T1…T3, IMA-T1…T5) |
| **Exact numeric FCW time-to-collision (TTC) algorithm threshold** | **UNVERIFIED** — not present in the accessible VSC-A Final Report text (appears to be specified in a companion/appendix volume or in the paywalled SAE J2945/1 standard, neither of which was accessible this session) | — |
| **Exact numeric IMA time-to-intersection algorithm threshold** | **UNVERIFIED** — same caveat as above | — |
| SAE J2945/1 tracking-error threshold (cited secondhand) | ~0.5 m mentioned in one secondary search summary | WebSearch synthesis only — **treat as UNVERIFIED**, not confirmed against the standard itself |

### B2. Surrogate safety metrics — TTC / PET / DRAC (FHWA Surrogate Safety Assessment Model, SSAM)

| Item | Value | Source |
|---|---|---|
| TTC definition | "The minimum time-to-collision value observed during the conflict... based on the current location, speed, and future trajectory of two vehicles at a given instant" | FHWA-HRT-08-051, SSAM validation final report, fetched via https://www.fhwa.dot.gov/publications/research/safety/08051/02.cfm |
| **TTC default threshold** | **1.5 s** ("a default TTC value of 1.5 seconds, as suggested in previous research") | same |
| PET definition | "the time between when the first vehicle last occupied a position and the time when the second vehicle subsequently arrived to the same position. A value of zero indicates a collision." | same |
| PET default threshold value | UNVERIFIED this session — the fetched chapter states PET is used as a measure but does not give a numeric default in the excerpt retrieved (commonly cited elsewhere in SSAM literature as <5 s, but that figure was not independently confirmed against the primary text this session) | FHWA-HRT-08-051 (partial excerpt only) |
| DR/DRAC definition | "The initial deceleration rate of the second vehicle... the first negative acceleration value observed during the conflict" (if the vehicle reacts) | same |
| DRAC numeric threshold | UNVERIFIED this session — not present in the retrieved excerpt | — |
| SSAM Software User Manual (installation/use) | FHWA-HRT-08-050 (May 2008), authors Pu & Joshi | https://www.fhwa.dot.gov/publications/research/safety/08050/08050.pdf |
| SSAM companion validation report | FHWA-HRT-08-051 (June 2008), "Surrogate Safety Assessment Model and Validation: Final Report" | https://www.fhwa.dot.gov/publications/research/safety/08051/ |

### B3. Ghost-vehicle → false-FCW chain

| Item | Value | Source |
|---|---|---|
| Ghost vehicle via pseudonym change (CAM) | When an ITS-S changes pseudonym, its old identity persists in neighbors' Local Dynamic Map (LDM) for a period; e.g. one Ego vehicle sees 3 entries for vehicle A + 2 for vehicle B (6 apparent neighbors when only 2 real vehicles exist) | ETSI TR 103 415 V2.1.1 (2025-03) §4.4.2 "Ghost vehicles", cached webfetch copy (`txt/webfetch-1789708464572-jcux3p.txt`) |
| "Missing vehicle" companion effect | Silent period after pseudonym change → sudden reappearance in neighbors' LDM → can itself look like anomalous/attack behavior | same, §4.4.2 |
| Ghost vehicle via CPM | Manipulating the Originating Vehicle Container or Perceived Object Container can fabricate a non-existent vehicle; because a single pseudonym certificate can back many "Perceived Objects," CPM makes Sybil/ghost-fleet attacks easier than CAM (which needs 1 certificate per ghost) | ETSI TR 103 460 V2.1.1 (2020-10) Annex C.3, https://www.etsi.org/deliver/etsi_tr/103400_103499/103460/02.01.01_60/tr_103460v020101p.pdf |
| Causal link to a false FCW alert | **Not found as an explicitly numbered/quantified study this session.** Reasoning chain (inference, not a cited result): a ghost/fabricated BSM or CPM PerceivedObject with plausible forward-path kinematics would be ingested by the FCW threat-assessment logic (per VSC-A's FCW definition — "impending rear-end collision with RV ahead in traffic in same lane/direction") and could trigger an unwarranted alert if not filtered by plausibility/misbehavior checks. Flag this row as **UNVERIFIED as a directly-cited "ghost→false-FCW" experimental result**; only the two attack-mechanism halves (ghost creation, FCW trigger logic) are independently sourced above. | Derived from TR 103 415 + TR 103 460 + VSC-A FCW definition |

---

## Topic C — Perception (abstract tier)

### C1. Radar/camera/LiDAR specs — Aumovio/Continental product line

| Item | Value | Source |
|---|---|---|
| **ARS 408-21** (77 GHz FMCW radar) | Freq 77 GHz; SR-mode range up to 70 m; FR-mode range up to 250 m; extended-range SW variant up to 1,200 m (high-RCS target, clear FOV only); scan rate 17 scans/s; classifies >120 single clusters; CAN-bus interface | `aumovio-ars-408.html` (cached copy of https://engineering-solutions.aumovio.com/components/ars-408/) |
| **ARS540** (4D premium long-range imaging radar) | Range ≈300 m (consistent across secondary sources); FOV reported inconsistently — one source says ±60°, another says 120° horizontal. **FOV: UNVERIFIED (conflicting secondary sources, no official datasheet retrieved)** | WebSearch: linkedin.com/pulse (Miyashita), boardor.com/blog (Decoding the Continental ARS540); official page (`continental-automotive.com/.../advanced-radar-sensor-ars540.html`) 301-redirects to a generic Aumovio landing page with no numeric spec |
| **SRR520** (77 GHz short-range radar) | Range 100 m at 0°; detection FOV ±90°, measurement FOV ±75°; update interval 50 ms; speed-measurement accuracy ±0.07 km/h; dimensions 83×68×22 mm, ~135 g | WebSearch secondary/reseller listings (indiamart, reverse-costing.com teardown) — **not an official Continental datasheet**, treat range/FOV as indicative only |
| **SRR320** | UNVERIFIED this session — cached page returned 404; web search found only resale/parts listings (eBay, etc.), no technical datasheet with range/FOV | — |
| **MFC 500** (multi-function mono camera) | FOV ≈125° (horizontal, per company social-media post); resolution up to 8 MP; dimensions 88×70×38 mm, <200 g; operating temp −40 °C to +95 °C; power dissipation <7 W. **Detection range: UNVERIFIED** (not found this session) | WebSearch: facebook.com/Continental post + continental.com press release (press-release URL 404'd on direct fetch; numbers taken from search-result snippet only) |
| **HFL110** (solid-state 3D Flash LiDAR) | Range 50 m; FOV 120°×30°; depth frame 128×32 px (4,096 pts); 25 fps; 1064 nm eye-safe Class-1 laser | WebSearch: oemoffhighway.com, mobilityengineeringtech.com, techinsights.com teardown coverage |
| Detection probability vs. range (any of the above) | UNVERIFIED this session — no manufacturer curve/table located for any of the 6 sensors | — |

### C2. ETSI TS 103 324 (Collective Perception Message, CPM) — perceived-object fields & confidence

| Item | Value | Source |
|---|---|---|
| Classification confidence level | "measure related to the certainty, generally a probability, with which a perceived object is assigned to a certain class"; sum of per-object class-confidence values may not exceed 100% | ts103324.txt §3 (definitions); ETSI TS 103 324 V2.1.1 (2023-06), https://www.etsi.org/deliver/etsi_ts/103300_103399/103324/02.01.01_60/ts_103324v020101p.pdf |
| Confidence value (generic) | "estimated absolute accuracy... of a measured value of a parameter with a specified confidence level (generally 95% in the present document)" | ts103324.txt §3 |
| Perception region confidence | "quantification of the estimated likelihood that objects or unoccupied regions may be [correctly detected]" within a `perceptionRegionShape` | ts103324.txt §3, §7.1.7 |
| PerceivedObject mandatory field | position (Cartesian or polar coordinates) | ts103324.txt §7.1.8 |
| PerceivedObject optional fields | velocity, acceleration, angles (incl. z-angle), angular velocity, up to 4 `LowerTriangularPositiveSemidefiniteMatrix` covariance components (provided at 95% confidence level) | ts103324.txt §7.1.8 |
| objectAge | time (discrete instants) the object has been known to the transmitting ITS-S | ts103324.txt §7.1.8.4 |
| objectPerceptionQuality — inputs | object age `oa` (ms); sensor-specific detection confidence `c_t` ∈[0,1]; binary detection-success `d_t` ∈{0,1} | ts103324.txt §7.1.8.6, citing [i.4] |
| objectPerceptionQuality — computation | EMA_t = α·c_t + (1−α)·EMA_(t-1); rating r_c = floor(EMA_t·15) (same process for d_t → r_d); age rating r_oa = min(floor(oa/100), 15); quality = floor( (w_d·r_d + w_c·r_c + w_oa·r_oa) / (w_d+w_c+w_oa) ) | ts103324.txt §7.1.8.6 |
| sensorType special values relevant to fused perception | `localAggregation` (12), `itssAggregation` (13) | ts103324.txt §7.1.8.5-adjacent |
| VRU group/cluster classification | `groupSubClass` used to report a VRU group (locally perceived) or VRU cluster (sourced from a received VAM, per TS 103 300-3) | ts103324.txt §7.1.8.7 |

---

## Topic D — Jamming/PHY attacks

### D1. Puñal, Aguiar & Gross — "In VANETs We Trust? Characterizing RF Jamming in Vehicular Networks" (ACM VANET 2012)

| Item | Value | Source |
|---|---|---|
| Jammer patterns implemented | Constant, Reactive, Constant-Pilot | Full text extracted this session from https://www.jamesgross.org/wp-content/uploads/2016/01/Punal_Aguiar_Gross_VANET_12.pdf |
| Jammer hardware | WARP boards, 802.11-like OFDM on FPGA, 10 MHz BW 802.11a/g transceiver, tunable up to 5.875 GHz (covers 802.11p ch. 172/174) | §5.2 |
| Legitimate Tx power (Linkbird device, full gain, measured) | 17.58 dBm (device spec max 21 dBm) | §5.1 |
| WARP jammer Tx power at 5.9 GHz band (measured) | 16.75 dBm (spec ≈18 dBm at 2.4 GHz band) | §5.2 |
| Constant-pilot jammer total power | 2.42 dBm (significantly lower than the other jammer types) | §5.3 "Constant Pilot Jammer" |
| Reactive jammer's sensing/trigger threshold | −75 dBm RSSI | §5.2 "Reactive Jammer" |
| Assumed receiver noise floor | −86 dBm (lowest receiver sensitivity) | §5.1 |
| RSSI→SINR mapping (linear model, least-squares) | γ [dB] = 0.8565·σ − 86.35, σ = RSSI sample | §5.1 |
| **Constant jammer effect — open space, platooning topology** | Created a **~250 m** communication blind area centered on the jammer | §7.1 |
| **Reactive jammer effect — open space, platooning topology** | Created a **~170 m** interference/blind area (shorter range than constant jammer) | §7.1 |
| **Constant jammer effect — dense urban crossroad** | Blind area of **167 m** at 30 km/h vehicle speed; jammer placed 33 m from the crossroads (indoors, behind a window) | §7.2 |
| **Reactive jammer effect — dense urban, Tx near jammer** | PDR dropped as low as **60%** even at high SINR | §7.2 |
| Qualitative summary | Constant jammer "dramatically disrupts communication regardless of scenario"; reactive jammer's success strongly depends on relative Tx/Rx/jammer position — "very effective in open-space" but "low impact in scenarios with reduced line-of-sight" | Abstract |
| Detection cue identified | Reactive jamming produces PDR=0 dropouts *not* correlated with SINR dips (unlike the constant jammer, whose PDR degradation tracks SINR); usable as a jammer-type discriminator | §8 |

### D2. Jammer models used in simulators (context, not a single numeric source)

| Item | Value | Source |
|---|---|---|
| Common simulated jammer archetypes | Constant/continuous, reactive/triggered-on-detection, random/duty-cycled — directly matching Puñal et al.'s taxonomy | Synthesized from D1 above |
| Effective range of RF jamming attacks on 802.11p/5.9 GHz | "restricted by the range of the attacker(s)... does not impact V2X communications everywhere," but within range "can increase the latency in V2X communications and reduce the reliability of the network" | `pdftxt/arxiv2003_07191_securing_v2x.txt` §on jamming (cached, "Securing V2X Communications" survey) |
| ns-3/Veins-specific jammer module parameters | UNVERIFIED this session — not located in cache or via WebFetch (the `veins-f2md` submodule referenced by the local F2MD checkout is empty/not cloned; see Topic E) | — |

---

## Topic E — MA/detection literature numbers

### E1. ETSI TR 103 460 — Pre-standardization study on Misbehaviour Detection

| Item | Value | Source |
|---|---|---|
| CCMS (harmonized US-EU-Australia) functional processes | Provisioning, Enrolment, Authorization, **Misbehaviour**, Revocation (5 processes) | tr103460.txt §4.3.2; ETSI TR 103 460 V2.1.1 (2020-10), https://www.etsi.org/deliver/etsi_tr/103400_103499/103460/02.01.01_60/tr_103460v020101p.pdf |
| Reporting approaches catalogued | Unicast MR → Misbehaviour Authority; Broadcast MR → neighbours (with stated pros/cons/alternatives) | tr103460.txt §5.2 |
| Attack types on CPM (ghost-vehicle relevant) | data-modification attacks, ghost-car attacks, Sybil attacks — see Topic B3 above for detail | tr103460.txt Annex C.3 |
| CAMP misbehavior-detection PoC quantitative results (detection latency, report volume) | **UNVERIFIED this session** — not present in TR 103 460's accessible text; targeted WebSearch for a CAMP/NHTSA misbehavior-detection PoC report with these metrics returned only patent filings and the general "CAMP V2V-CR Final Report" conceptual OBE/SCMS Misbehavior-Detection architecture description, with no extractable latency/volume numbers | WebSearch (multiple queries, 2026-09-18); no usable primary source found |

### E2. ETSI TS 103 759 — Misbehaviour Reporting service (Release 2, V2.2.1, 2026-01)

| Item | Value | Source |
|---|---|---|
| Trust-gated DENM validity check | DENM considered valid only if event trust ETR(E,j) ≥ TrustThreshold (predefined) | etsi103759.pdf.txt line ~1567 |
| EEBL plausibility window | The preceding vehicle's braking/deceleration event must be positive within a **500 ms** window before the detectionTime of a claimed EEBL event | etsi103759.pdf.txt line ~1912 |
| Detection-time consistency threshold reference | detectionTime check against "80% of the minimum threshold value" for certain event types (e.g., slow-down/dangerous-situation/EEBL) | etsi103759.pdf.txt line ~1667 |
| Speed-exceedance trigger example (upstreamTraffic check) | > 80 km/h | etsi103759.pdf.txt lines ~1837–1840 |
| Detection-time upper bound example | threshold ≤ 180 ms mentioned for one detection-time check | etsi103759.pdf.txt line ~1828 |
| Max-distance correlation thresholds for same-event reports | 1 km for `dangerousEndOfQueue`/`trafficCondition` event types; 100 m otherwise | etsi103759.pdf.txt lines ~1701–1703 |

### E3. F2MD — Framework for Misbehavior Detection (Kamel et al., IEEE TVT 2020)

| Item | Value | Source |
|---|---|---|
| Publication | "Simulation Framework for Misbehavior Detection in Vehicular Networks," IEEE Trans. Veh. Technol., vol. 69, no. 6, pp. 6631–6643, 2020 | DOI 10.1109/TVT.2020.2984878; https://ieeexplore.ieee.org/abstract/document/9056489; HAL mirror https://hal.science/hal-02527873 (PDF fetch blocked by an Anubis bot-challenge this session — could not extract text) |
| Supported network technologies | ITS-G5 (IEEE 802.11p), C-V2X (3GPP PC5 Mode 4) | `f2md/README.md` (local cached copy of github.com/josephkamel/F2MD) |
| Framework components | mdChecks (basic plausibility), mdApplications (node-level investigation), real-time ML plausibility server (HTTP), mdStats, mdReport (multi-mechanism), Misbehavior-Authority server (global report collection), mdPCPolicies (pseudonym-change policy), mdAttacks (local+global attack injection), attack-server (real-time attack launch) | `f2md/README.md` |
| Plausibility-check categories (from patent/secondary literature synthesis, **not primary-source-confirmed numeric thresholds**) | source-vehicle speed, position, acceleration, sudden-appearance, message-frequency, heading, and successive-message-consistency checks | WebSearch synthesis of patent filings referencing Kamel et al.'s CaTch approach — secondary source only |
| Numeric plausibility thresholds (max speed, max acceleration, max range, etc.) | **UNVERIFIED this session** | — |
| Detection rate / false-positive rate | **UNVERIFIED this session** | — |
| Misbehavior-Authority global-detection threshold values | **UNVERIFIED this session** | — |
| Why unverified | The local `f2md/` cache is a shallow git checkout with the `veins-f2md`, `inet`, and `simulte-f2md` submodules **not populated** (empty directories) — no C++ source with the actual numeric check thresholds is present locally. The primary paper (HAL PDF) was blocked by a bot-protection challenge (Anubis) on both `hal.science` and (separately) on the `punal.html` source domain during this session; IEEE Xplore abstract page was not fetched for full text (paywalled). | `f2md/` directory listing (this session); WebFetch failures on hal.science and Semantic Scholar paper page |

---

## Summary of UNVERIFIED items (do not use as design inputs without further sourcing)

1. TR 36.885 exact ISD/BW table values (cache file corrupted; only cross-referenced via TR 37.885 citation).
2. NYC CV Pilot RSU backhaul medium.
3. p95 (only median) mobile latency from Ookla for the US.
4. Exact FCW time-to-collision (TTC) and IMA time-to-intersection numeric algorithm thresholds from SAE J2945/1 / VSC-A (only the EEBL 0.4 g deceleration trigger was confirmed numerically).
5. SSAM PET and DRAC default numeric thresholds (TTC=1.5s was confirmed; PET/DRAC were not, in the retrieved excerpt).
6. An explicitly-cited/quantified "ghost-vehicle → false-FCW" experimental chain (only the two mechanism halves are sourced separately).
7. ARS540 field of view (conflicting secondary sources); SRR320 datasheet (not found); MFC500 detection range; detection-probability-vs-range curves for any Aumovio sensor.
8. ns-3/Veins-specific jammer module implementation parameters.
9. CAMP misbehavior-detection PoC numeric results (detection latency, report volume).
10. F2MD numeric plausibility-check thresholds, detection/false-positive rates, and MA global-detection thresholds.
