# R2 — LTE-V2X PC5 Mode 4 (Rel-14) and NR-V2X PC5 Mode 2 (Rel-16): Cited Parameter Fact Sheet

STATUS: COMPLETE PASS. This revision replaces the PENDING placeholders from the prior partial pass
with findings verified directly against a locally cached corpus (3GPP spec text extracted from the
official `.doc`/`.docx` archives, plus full-text academic papers) and a small number of targeted
WebSearch/WebFetch lookups for regulatory facts not present in the corpus. Every row below carries a
source; rows that could not be verified are explicitly marked **UNVERIFIED** rather than filled with
an assumed number. Where a spec clause's exact algebraic formula was lost to a bad OLE/DOCX→text
conversion (the 3GPP `.doc` files store some equations as embedded `EMBED Equation.3` OLE objects,
which the local text extraction could not recover), the underlying prose/structure was still verified
directly against the spec text, and the specific numeric value is instead cross-confirmed from one or
more peer-reviewed secondary sources that quote the same clause. This is noted inline wherever it
applies.

## Corpus used (this pass)

All paths are relative to
`/private/tmp/claude-501/-Users-ahmedzaidan-Developer-SCMS-Simulator/9f0649d7-8535-468d-9a82-3700cd13b998/scratchpad/research/`.

- `src/ts36885.txt`, `src/tr36885_raw.txt` — 3GPP **TR 36.885 V14.0.0 (2016-06)**, "Study on LTE-based
  V2X Services" (Release 14). `tr36885_raw.txt` is a much cleaner line-oriented extraction of the same
  document (3864 lines) and was used for all Annex A (channel model / evaluation methodology) lookups.
- `src/ts36213.txt` (= `src/doc/36213/*.txt` concatenated) — 3GPP **TS 36.213 V14.4.0 (2017-09)**,
  "E-UTRA Physical layer procedures". Clause 14.1.1.6 (Mode-4 sensing/selection) and 14.2.x (PSCCH/PSSCH
  procedures) were located and read; the clause's own equations are OLE objects lost in conversion, so
  exact numeric ranges (T1, T2, RSRP threshold step, sensing window) are corroborated from secondary
  sources below rather than quoted directly from this file's prose.
- `src/ts36331.txt` (= `src/doc/36331/36331-e40.txt`) — 3GPP **TS 36.331 V14.4.0 (2017-09)**, "E-UTRA
  Radio Resource Control (RRC)". ASN.1 IE text (which survived conversion intact, unlike prose
  equations) was grepped directly: `sizeSubchannel-r14`, `thresPSSCH-RSRP-List`, `probResourceKeep-r14`,
  `SL-CBR-CommonTxConfigList`.
- `src/ts36212.txt` (= `src/doc/36212/36212-e40.txt`) — 3GPP **TS 36.212 V14.4.0**, "Multiplexing and
  channel coding". Clause 5.4.3.1.2 (SCI format 1) read directly and quoted verbatim below.
- `src/ts38211.txt`, `src/ts38212.txt`, `src/ts38213.txt`, `src/ts38214.txt`, `src/ts38215.txt`,
  `src/ts38321.txt`, `src/ts38331.txt` — 3GPP **TS 38.211/212/213/214/215/321/331**, each at the version
  cited by [Garcia2021] (v16.x, Release 16; see below), used both directly (ASN.1 IEs, MCS Table
  5.1.3.1-1) and as cross-references for [Garcia2021]'s claims.
- `src/tr37885.txt` (also `pdf/tr37885.txt`) — 3GPP **TR 37.885 V15.3.0 (2019-06)**, "Study on evaluation
  methodology of new V2X use cases for LTE and NR". Clause 6.1 (deployment/traffic model) and 6.2
  (channel model) read in full; formulas quoted verbatim below.
- `src/tutorial_nrv2x.txt` — **M. H. Castañeda Garcia, M. Boban, A. Kousaridas, T. Şahin, "A Tutorial on
  5G NR V2X Communications," IEEE Communications Surveys & Tutorials, 2021, DOI:
  10.1109/COMST.2021.3057017** (= arXiv:2102.04538; the paper the task named). Cited as **[Garcia2021]**
  below. This is the single richest source in the corpus, covering LTE V2X Mode 4 PHY/MAC (with the
  numeric detail lost from the raw TS 36.213 conversion) and NR V2X Mode 2 PHY/MAC/QoS/congestion
  control in depth, always citing exact 3GPP spec + version numbers in its own reference list (verified
  directly: `[19]`=TS 36.331, `[23]`=TS 36.213, `[24]`=TS 36.214, `[42]`=TS 38.214 v16.3.0,
  `[44]`=RAN1 R1-1913601 agreements, `[53]`=TS 38.212 v16.3.0, `[63]`=TS 38.331 v16.2.0,
  `[66]`=TS 38.213 v16.3.0, `[76]`=TS 38.215 v16.3.0).
- `src/nist_nrv2x.txt` — Z. Ali, S. Lagén, L. Giupponi, R. Rouil, "3GPP NR V2X Mode 2: Overview, Models
  and System-Level Evaluation," IEEE Access, 2021, DOI 10.1109/ACCESS.2021.3090855.
- `src/../nrv2xnum.txt` — Ali, Lagén, Giupponi, "On the impact of numerology in NR V2X Mode 2 with
  sensing and no-sensing resource selection," arXiv:2106.15303v1, 2021. Cited **[Ali2021]** (retained
  from the prior pass).
- `src/molina2017vtc.txt` — R. Molina-Masegosa, J. Gozalvez, "System Level Evaluation of LTE-V2V Mode 4
  Communications and its Distributed Scheduling," IEEE VTC-Spring 2017.
- `src/gonzalez2019.txt` — M. Gonzalez-Martin, M. Sepulcre, R. Molina-Masegosa, J. Gozalvez,
  "Analytical Models of the Performance of C-V2X Mode 4 Vehicular Communications," IEEE Trans. Veh.
  Technol., 2019 (arXiv:1807.06508).
- `src/todisco2021.txt` — V. Todisco, S. Bartoletti, C. Campolo, A. Molinaro, A. O. Berthet, A. Bazzi,
  "Performance Analysis of Sidelink 5G-V2X Mode 2 Through an Open-Source Simulator," IEEE Access, 2021,
  DOI 10.1109/ACCESS.2021.3121151.
- `src/phymac1807.txt` — A. Bazzi, G. Cecchini, A. Zanella, B. M. Masini, "Study of the Impact of PHY and
  MAC Parameters in 3GPP C-V2V Mode 4," IEEE Access, vol. 6, 2018, arXiv:1807.10699.
- `src/opencv2x.txt` — B. McCarthy, A. Burbano-Abril, V. Rangel Licea, A. O'Driscoll, "OpenCV2X:
  Modelling of the V2X Cellular Sidelink and Performance Evaluation for Aperiodic Traffic,"
  arXiv:2103.13212, 2021.
- `src/per/*.txt` + `src/wilab_readme.md` + `src/percurvesgen.m` — **WiLabV2Xsim** (University of
  Bologna/CNR/WiLab-CNIT) bundled PER-vs-SNR lookup tables (`PER_table.mat`, exported per-scenario by
  `percurvesgen.m`). Per the README, curves originate from the methodology described in A. Bazzi et al.,
  "Survey and Perspectives of Vehicular Wi-Fi Versus Sidelink Cellular-V2X in the 5G Era," Future
  Internet, 11(6):122, 2019 (results for simulator v3.5) and V. Todisco et al. 2021 (main reference for
  the current simulator version). Cited **[WiLabV2Xsim-PER]**.
- `src/fcc2020.txt` (= `txt/fcc2020factsheet.txt`) — FCC, "Modernizing the 5.9 GHz Band," Fact Sheet for
  the *First Report and Order, Further Notice of Proposed Rulemaking, and Order of Proposed
  Modification*, ET Docket No. 19-138, October 28, 2020.
- `txt/fcc24-123.txt` — FCC 24-123, *Second Report and Order*, ET Docket No. 19-138, adopted Nov 20,
  2024, released Nov 21, 2024.
- `pdftxt/5gaa_cv2x_devices_2024.txt` — 5GAA, "List of C-V2X Devices," Technical Report, v0.1, approved
  8 April 2024.
- WebSearch (2026-09-18, this pass) for facts absent from the local corpus: Volkswagen Golf 8 ITS-G5
  deployment date; China MIIT 5905-5925 MHz LTE-V2X spectrum allocation dates; Ford/GM C-V2X commitments
  and their actual (non-)fulfilment. Individual result URLs are cited inline in Topic F.
- **Files attempted but not usable**: `src/bazzi2019fi.pdf` (attempted download of the Bazzi et al. 2019
  Future Internet paper returned an MDPI CDN "Access Denied" page, confirmed by direct read — the file
  is an HTML error page, not the PDF); a direct WebFetch of `mdpi.com/2078-2489/11/6/122` in this pass
  also returned HTTP 403. The paper's title/DOI/venue are still independently confirmed via
  `wilab_readme.md`'s citation of it, and its associated numeric artifact (the PER-vs-SNR curves) was
  recovered from `src/per/`, but its own figure-level PDR/PRR-vs-distance numbers are **UNVERIFIED** in
  this pass (marked as such in Topic E).

---

## Topic A — LTE-V2X Mode 4 (Rel-14, PC5)

| Parameter | Value | Source |
|---|---|---|
| Channel bandwidth | 10 MHz or 20 MHz | [Garcia2021] §II.A ("LTE V2X ... supports 10 MHz and 20 MHz channels"); TR 36.885 V14.0.0 Annex A.1.1 lists "10 MHz" as the PC5 V2V evaluation bandwidth |
| Subframe / symbol structure | 1 ms subframe = 14 OFDM symbols (normal CP); 9 data symbols, 4 DMRS symbols (3rd, 6th, 9th, 12th), 1 guard symbol (14th) | [Garcia2021] §II.A, verbatim: "Each subframe has 14 OFDM symbols with normal cyclic prefix. Nine of these symbols are used to transmit data and four of them (3rd, 6th, 9th, and 12th) are used to transmit demodulation reference signals... The last symbol is used as a guard symbol" |
| PRB / RB structure | 180 kHz RB = 12 subcarriers × 15 kHz | [Garcia2021] §II.A |
| `sizeSubchannel` enumerated values (TS 36.331 IE `SL-CommResourcePoolV2X`, field `sizeSubchannel-r14`) | `{n4, n5, n6, n8, n9, n10, n12, n15, n16, n18, n20, n25, n30, n48, n50, n72, n75, n96, n100, spare13, spare12, spare11, spare10, spare9, spare8, spare7, ...}` | **Verified directly** by grep of `src/ts36331.txt`: `sizeSubchannel-r14 ENUMERATED {n4, n5, n6, n8, n9, n10, n12, n15, n16, n18, n20, n25, n30, n48, n50, n72, n75, n96, n100, spare13, spare12, spare11, spare10, spare9, spare8, spare...}`. Matches the exact value set the task asked to verify. |
| PSCCH size / placement | 2 PRBs per SCI transmission. Mode 3/4: "If a pool is (pre)configured such that a UE always transmits PSCCH and the corresponding PSSCH in adjacent resource blocks... the PSCCH resource m is the set of two contiguous resource blocks... If a pool is (pre)configured such that a UE may transmit PSCCH and PSSCH in non-adjacent resource blocks..." — i.e. adjacent/non-adjacent is a resource-pool (pre-)configuration choice | **Verified directly**, TS 36.213 V14.4.0 §14.2.4 (`src/ts36213.txt`, quoted verbatim above); corroborated by [Garcia2021] §II.A: "An SCI occupies 2 RBs" and by Molina-Masegosa & Gozalvez 2017 §III: "sub-channels of 12 RBs (2 RBs for SCI or TB, and 10 RBs only for TB)" |
| SCI format 1 size | **32 bits**, reserved bits zero-padded to reach exactly 32 | **Verified directly, verbatim**, TS 36.212 V14.4.0 §5.4.3.1.2 (`src/ts36212.txt` line 7864): "Reserved information bits are added until the size of SCI format 1 is equal to 32 bits. The reserved bits are set to zero." Field breakdown also verified directly in the same clause: Frequency resource location (variable bits per §14.1.1.4C of TS 36.213), Time gap between initial tx/retx = 4 bits, Retransmission index = 1 bit. |
| MCS/TBS for 190-byte / 300-byte packets | 300-byte packet: QPSK, code rate ½, TB occupies 20 RBs (2 adjacent 12-RB subchannels, 22 RBs allocated of which 20 carry the TB). 190-byte packet: QPSK, code rate 0.7, TB occupies 10 RBs (fits one 12-RB subchannel: 2 RBs SCI + 10 RBs TB). A 10 MHz channel (50 RBs of 180 kHz) then holds 4 subchannels of 12 RBs each. | Molina-Masegosa & Gozalvez 2017 (`src/molina2017vtc.txt`), §III "Simulation Setup", quoted near-verbatim: "The 300 bytes packets are coded with a MCS using QPSK and a ½ code rate (TBs occupy 20 RBs). The 190 bytes packets are coded with a MCS using QPSK and a 0.7 code rate (TBs occupy 10 RBs) ... This study considers sub-channels of 12 RBs ... The 10MHz channel (with 50 RBs of 180kHz per sub-frame) can then accommodate 4 sub-channels." Note this paper's own subchannel size (12 RBs) is stated as *its own simulation choice* because "The 3GPP Release 14 standards do not specify the value" for RBs/subchannel. |
| Sensing window | 1000 ms (1000 subframes) before the resource-selection trigger subframe | [Garcia2021] §II.B: "it senses the transmissions from other vehicles during the last 1000 subframes before tG (sensing window)"; TS 36.213 §14.1.1.6 defines this window structurally (equation text lost in conversion, cross-confirmed numerically here) |
| Selection window T1, T2 | T1 (subframes) ≤ 4, UE-implementation choice. T2 (subframes): if `T2min` is (pre-)configured, `T2min ≤ T2 ≤ 100`; else `20 ≤ T2 ≤ 100`. `T2min` itself ∈ [10, 20] subframes depending on transmission priority. T2 additionally constrained so `tG+T2` meets the packet's latency deadline (100/50/20 ms for 10/20/50 pps traffic). | [Garcia2021] §II.B, quoted near-verbatim; **independently corroborated** by Bazzi et al. 2018 (`src/phymac1807.txt`) Table 1: "First subframe for the next allocation (T1) ≤4" / "Last subframe for the next allocation (T2) ≥20, ≤100" (3GPP constraints column) |
| RSRP exclusion threshold | Structurally: a list of **64 thresholds** selected by the (Tx priority, Rx priority) pair, raised by **+3 dB** and the exclusion step repeated whenever fewer than 20% of candidate resources remain. The 3GPP-mandated range for the threshold is **[−128, −2] dBm**. Combining these two verified facts (64 evenly spaced values spanning exactly −128 to −2 dBm) is consistent with the commonly cited formula **Pth = −128 + 2·index dBm, index = 0..63**, but this exact algebraic form was not found verbatim in the available spec text (the TS 36.213 clause's own equations are unrecovered OLE objects) — treat the formula itself as *derived*, not directly quoted. | 64-threshold-list structure: **verified directly**, TS 36.331 V14.4.0 (`src/ts36331.txt`): "thresPSSCH-RSRP-List Indicates a list of 64 thresholds, and the threshold should be selected based on the priority in the decoded SCI and the priority in the SCI to be transmitted." Range [−128,−2] dBm and the 3 dB step-up/20% rule: Bazzi et al. 2018 Table 1 ("Minimum threshold to the power level (Pth) ∈ [−128,−2] dBm") and [Garcia2021] §II.B ("the RSRP threshold is increased by 3 dB ... until the number of available candidate resources is at least equal to 20%"). Simulator studies commonly use round baseline values within this range instead of the full per-priority table: −110 dBm (Molina-Masegosa & Gozalvez 2017), −126 dBm (OpenCV2X, `src/opencv2x.txt`), −128 dBm (Bazzi et al. 2018 "used if not specified" column, and phymac1807's "optimized Mode 4" variant). |
| Candidate set size (20% rule) | The vehicle keeps raising the RSRP threshold by 3 dB, re-evaluating, until ≥ 20% of all candidate resources in the selection window remain; a second list (by lowest average RSSI) must also be ≥ 20% of all SW candidates | [Garcia2021] §II.B, verbatim: "The vehicle checks then if the number of remaining available candidate resources is equal or higher than 20% of all candidate resources within the SW... The total number of candidate resources in L2 must be greater than or equal to 20%..."; TS 36.331/213 IE for this parameter is `Rsel`/"portion of beacon resources" per Bazzi et al. 2018 Table 1 ("Rsel = 0.2, mandated") |
| Ranking metric | Average RSSI (S-RSSI) over the candidate sub-channels, computed over the previous `tTX − T·j` subframes in the sensing window (T = the RRI value in ms: 100/50/20 ms) | [Garcia2021] §II.B |
| SPS reservation interval (RRI) | 0 ms (= "not reserving"), or 20/50/100 ms, or any multiple of 100 ms up to 1000 ms; up to 16 permitted values may be (pre-)configured, though "3GPP standards only define 12 possible RRI values higher than 0 ms for mode 4" | [Garcia2021] §II.B, verbatim |
| Reselection counter | Randomly chosen: **5–15** for RRI ≥ 100 ms; **10–30** for RRI = 50 ms; **25–75** for RRI = 20 ms. Decremented by 1 per transmission; at 0, the UE must select new resources with probability (1−P) | [Garcia2021] §II.B, verbatim; **independently corroborated**, Bazzi et al. 2018 Table 1: "Minimum number of beacon periods before evaluating a new reallocation (nmin) 5 (mandated)" / "Maximum ... (nmax) 15 (mandated)" (for the RRI ≥ 100 ms case) |
| `probResourceKeep` (P) | `{0, 0.2, 0.4, 0.6, 0.8}` (ENUMERATED `v0, v0dot2, v0dot4, v0dot6, v0dot8`, plus 3 spares) | **Verified directly**, TS 36.331 V14.4.0 (`src/ts36331.txt`): `probResourceKeep-r14 ENUMERATED {v0, v0dot2, v0dot4, v0dot6, v0dot8, spare3, spare2, spare1}`. Matches [Garcia2021]'s "P ∈ [0, 0.8]." |
| Half-duplex | A transmitting UE cannot sense/receive on the same subframe it transmits on; those subframes are excluded from the sensing history and handled via the `q·RRIi` exclusion rule | [Garcia2021] §II.B; TR 36.885 V14.0.0 Annex A.1 (`src/tr36885_raw.txt`): "Each vehicle UE's reception is subject to the half duplex constraint" |
| In-band emissions (IBE) model | `{W, X, Y, Z} = {3, 6, 3, 3}` for single-cluster SC-FDMA (reusing the model of TS 36.101 Table 6.5.2A.3-x-style parameterization, referenced via TR 36.843 in the TR's own citation `[4]`) | **Verified directly**, TR 36.885 V14.0.0 Annex A.1.1 (`src/tr36885_raw.txt`): "In-band emission model in Section A.2.1.5 in [4] is reused with {W, X, Y, Z} = {3, 6, 3, 3} for single cluster SC-FDMA." Exact TS 36.101 Table 6.5.2A.3.x numeric IBE mask values themselves were **not** located in readable form in this pass (58 MB `ts36101.txt` is heavily OLE-garbled); only the {W,X,Y,Z} parameterization used by the V2X study is confirmed. |
| UE Tx power | 23 dBm (vehicle/pedestrian UE and UE-type RSU); 33 dBm "not precluded" | **Verified directly**, TR 36.885 V14.0.0 Annex A.1.1: "UE Tx power: Vehicle/pedestrian UE or UE type RSU: 23dBm. Note: 33dBm is not precluded"; corroborated by [Garcia2021] (multiple table citations of "23 dBm") and Molina-Masegosa & Gozalvez 2017 ("all vehicles transmit at 23dBm") |
| Antenna gain / height (TR 36.885 Annex A baseline) | 3 dBi for vehicle UE and UE-type RSU, 0 dBi for pedestrian UE; antenna height 1.5 m for vehicle/pedestrian UE, 5 m for UE-type RSU | **Verified directly**, TR 36.885 V14.0.0 Annex A.1.1 |
| UE receiver noise figure | 9 dB | **Verified directly**, TR 36.885 V14.0.0 Annex A.1.1: "UE receiver noise figure 9 dB"; corroborated by Molina-Masegosa & Gozalvez 2017 and OpenCV2X (`src/opencv2x.txt`: "Noise figure 9 dB") |
| HARQ | Blind retransmission scheme (no feedback in Mode 4 broadcast); an SCI field flags "first transmission or blind retransmission of the TB." 3GPP Mode-4 studies commonly model **≤ 1 blind retransmission** (i.e. up to 2 total transmissions per TB) | [Garcia2021] §II.A: "an indication of whether it is a first transmission or a blind retransmission of the TB"; the "≤1 blind retransmission" convention is the common simulation setting in Todisco et al. 2021 ("considering a single blind retransmission") for the *NR* Mode-2 comparison baseline — for LTE Mode 4 itself the standard permits SCI-signalled retransmission via the "Time gap between initial transmission and retransmission" field (TS 36.212 §5.4.3.1.2, non-zero ⇒ one retransmission scheduled); a hard "≤1" cap is a common simulator convention rather than an explicit single-clause spec limit, so treat the specific ceiling as **simulator convention, not a verified spec MAX** |
| CBR (Channel Busy Ratio) definition | Ratio of sub-channels whose measured S-RSSI exceeds a (pre-)configured threshold, over the previous **100 subframes** | [Garcia2021] §II.B, verbatim: "The CBR is defined as the ratio of sub-channels that experience an RSSI higher than a (pre-)configured threshold to the total number of sub-channels in the previous 100 subframes" (citing TS 36.214 §5.1.30) |
| CR (Channel occupancy Ratio) definition | At subframe n, ratio of sub-channels used/selected by the TX UE within `[n−a, n+b]` to the total sub-channels in that window, where `a+b+1 = 1000` subframes and `a ≥ 500` | [Garcia2021] §II.B, verbatim (citing TS 36.214 §5.1.31) |
| CR limits per priority (`cbr-pssch-TxConfigList`) | Structure confirmed: up to **16 CBR ranges**, each mapped (via `SL-CBR-CommonTxConfigList` / IE `cbr-RangeCommonConfigList-r14` + `sl-CBR-PSSCH-TxConfigList-r14`, both **verified directly** in `src/ts36331.txt`) to a `CRLimit` that generally *increases* as the CBR range decreases, and that is a function of transmission priority. **Exact numeric CRLimit-vs-(CBR range, priority) table values are UNVERIFIED in this pass** — for LTE V2X these are defined regionally by ETSI (per [Garcia2021] footnote 44: "For LTE V2X, this table is defined in Europe by ETSI in ... Section 4.4 in [80]", i.e. an ETSI TS/TR outside the cached 3GPP corpus). | IE names/structure: TS 36.331 V14.4.0 direct grep. Qualitative behavior: [Garcia2021] §II.B |

**Cross-cutting note (verified):** 3GPP TR 38.913 V14.2.0, clause 7.9 "Reliability" (file
`tr38913.txt`, confirmed by direct read of that clause) states: "For eV2X, for communication
availability and resilience and user plane latency of delivery of a packet of size 300 bytes, the
requirements are as follows: Reliability = 1-10^-5, and user plane latency = 3-10 msec, for direct
communication via sidelink and communication range of (e.g., a few meters)... Reliability = 1-10^-5,
and user plane latency = 3-10 msec, when the packet is relayed via BS." Source: 3GPP/ETSI TR 138 913
V14.2.0 (2017-05), clause 7.9. (Retained from the prior pass; still the best available quantitative
reliability/latency target tied to the 300-byte packet size used elsewhere in this sheet.)

## Topic B — NR-V2X Mode 2 (Rel-16, PC5)

| Parameter | Value | Source |
|---|---|---|
| Numerologies (µ) | µ = 0, 1, 2 in FR1 for NR V2X Rel-16: SCS = 15×2^µ kHz (15/30/60 kHz), slot length = 1/2^µ ms | [Ali2021] §III, citing TS 38.211; [Garcia2021] §V.B corroborates with slot-based sensing/selection windows scaled by µ |
| Slot structure (PSCCH/PSSCH) | PSCCH: 2 or 3 symbols, occupying a (pre-)configurable number `M_PSCCH` of PRBs per resource pool, with `M_PSCCH < M_sub` (PSCCH confined to one sub-channel). PSSCH: remaining symbols in the slot, multiplexed in frequency with PSCCH where PSCCH doesn't span all `L_PSSCH` sub-channels. Each PSCCH/PSSCH slot carries 2nd-stage SCI and PSSCH DMRS (pattern depends on channel conditions). | [Garcia2021] §V.C.1, verbatim; Fig. 5 in that paper shows two concrete example slot layouts: "(a) Slot with 3 PSCCH, 12 PSSCH including 4 PSSCH DMRS symbols. (b) Slot with 2 PSCCH, 11 PSSCH including 2 PSSCH DMRS symbols." |
| Sub-channel size (`sl-SubchannelSize-r16`) | `{n10, n12, n15, n20, n25, n50, n75, n100}` PRBs | **Verified directly**, TS 38.331 (`src/ts38331.txt`): `sl-SubchannelSize-r16 ENUMERATED {n10, n12, n15, n20, n25, n50, n75, n100}`. (8 discrete values, not a continuous 10–100 range.) |
| PSFCH | Carries HARQ feedback only. (Pre-)configurable periodicity: a PSFCH-bearing slot every **1, 2, or 4 slots** in a resource pool. Structure per PSFCH occasion: 1 AGC symbol (copy of PSFCH symbol) + 1 PSFCH symbol + 1 guard symbol = 3 SL symbols; remaining PSCCH/PSSCH symbols in that slot ≤ 9. One PRB carries a Zadoff-Chu-sequence-based ACK/NACK; CDM between multiple UEs' PSFCH transmissions supported in the same PRB. PSFCH resource (symbol/PRB/cyclic-shift) is a function of the associated PSSCH's resources and the RX UE's ID. | [Garcia2021] §V.C.4 ("Physical Sidelink Feedback Channel"), verbatim |
| SCI 1-A / 2-A / 2-B | 1st-stage SCI (format **1-A**) carried on PSCCH: resource allocation, MCS, priority, resource-reservation info for retransmissions/next TB (occupies 2 RBs like LTE's SCI format 1, sized per resource-pool configuration rather than one fixed bit count — no single fixed bit-length equivalent to LTE's "32 bits" was found for NR SCI-1-A). 2nd-stage SCI carried on PSSCH, one of two formats: **format 2-A** (used with no HARQ feedback, or with unicast/groupcast HARQ feedback option 2 — all RX UEs feed back) and **format 2-B** (used with no HARQ feedback, or groupcast HARQ feedback option 1 — only RX UEs outside the communication range feed back). Both include a HARQ process ID and an NDI-equivalent field; a CRC of 24 parity bits is appended to the 2nd-stage SCI as with the 1st-stage SCI/TB. | [Garcia2021] §V.C.1–§V.C.2 (SCI structure), §III (HARQ feedback options), verbatim; exact fixed bit-length claims for SCI-1-A/2-A/2-B were **not found** — NR SCI field sizes vary by resource-pool config (TS 38.212 §8.3/8.4), unlike LTE's fixed 32-bit SCI format 1 |
| Sensing window (T0) | Slots `[n−T0, n−Tproc,0)`; **T0 = 100 ms or 1100 ms** (chosen by resource-pool (pre-)configuration), expressed as an integer number of slots depending on SCS | **Verified directly** (IE) + prose: TS 38.331 `sl-SensingWindow-r16 ENUMERATED {ms100, ms1100}` (`src/ts38331.txt`); prose per [Garcia2021] §V.B.1: "T0 is an integer defined in number of slots that depends on the SCS configuration but that must be set to a value ... equivalent to 1100 ms or 100 ms" |
| Sensing processing time (Tproc,0) | 1 slot for SCS = 15 or 30 kHz; 2 slots for SCS = 60 kHz; 4 slots for SCS = 120 kHz | [Garcia2021] §V.B.1, verbatim (footnote 32: "Tproc,0 is equivalent to 1 ms for a SCS of 15 kHz and 0.50 ms for the rest of SCS configurations") |
| Selection window (T1, T2) | Selection window `[n+T1, n+T2]` in slots. T1 is UE-implementation choice bounded by `Tproc,1`. T2 is UE-implementation choice within `T2min ≤ T2 ≤ PDB` (Packet Delay Budget); `T2min` is (pre-)configured. `SL-SelectionWindowConfig-r16` maps a priority (1..8) to a `sl-SelectionWindow-r16` value. | Prose: [Garcia2021] §V.B.1. IE **verified directly**, TS 38.331: `SL-SelectionWindowConfig-r16 ::= SEQUENCE { sl-Priority-r16 INTEGER (1..8), sl-SelectionWindow-r16 ENUMERATED {n1, n5, n10, n20} }` |
| Candidate resource set threshold (X%) | RRC-configured per resource pool: **20%, 35%, or 50%** of total resources in the selection window; if the retained candidate set falls below this, the RSRP exclusion threshold is raised by 3 dB and Step 1 is repeated | [Ali2021] §II.1, citing TS 38.214, corroborates [Garcia2021] §V.B.1's qualitative description. IE **verified directly**, TS 38.331: `SL-TxPercentageConfig-r16 ::= SEQUENCE { sl-Priority-r16 INTEGER (1..8), sl-TxPercentage-r16 ENUMERATED {p20, p35, p50} }` (this is the exact ASN.1 backing of "M = 20%" referenced in Todisco et al. 2021 as the "L2 list" candidate-set percentage) |
| RSRP threshold list (Rel-16) | `(−112 + n·2) dBm` for integer `n`, `0 ≤ n ≤ 45` — i.e. a threshold list spanning roughly **−112 dBm to −22 dBm** in 2 dB steps | [Garcia2021] §VI (Congestion Control section), footnote 43, verbatim: "Rel. 16 also specifies a range of values for this threshold that are defined as (−112 + n*2) dBm, where n is an integer in the range 0 ≤ n ≤ 45." (Note: this is a *different, narrower* range than LTE Mode 4's −128..−2 dBm 64-entry list — treat as the Rel-16-specific value, not a typo carried over from Topic A.) |
| Resource reservation period (`sl-ResourceReservePeriodList-r16`) | RRC-configurable list of up to 16 permitted reservation periods (in ms); reservation communicated via 1st-stage SCI; number of periods to reserve `Q = ⌈T2/RRIi⌉` when `RRIi < T2` and `n − si ≤ RRIi`, else `Q = 1` | IE **verified directly**, TS 38.331: `sl-ResourceReservePeriodList-r16 SEQUENCE (SIZE (1..16)) OF SL-ResourceReservePeriod-r16`. Formula: [Garcia2021] §V.B.1, verbatim |
| Max number of reserved resource instances (`sl-MaxNumPerReserve-r16`) | `{n2, n3}` — a single 1st-stage SCI can reserve resources for up to 2 or 3 transmission instances | **Verified directly**, TS 38.331: `sl-MaxNumPerReserve-r16 ENUMERATED {n2, n3}`; matches [Ali2021]'s simulation value `Nmax_reserve = 3` |
| Max PSSCH transmissions (`sl-MaxTransNum-r16`) | `INTEGER (1..32)` — combined count of initial transmission + blind/HARQ-triggered retransmissions | **Verified directly**, TS 38.331: `sl-MaxTransNum-r16 INTEGER (1..32)` |
| `probResourceKeep` (NR, `sl-ProbResourceKeep-r16`) | `{v0, v0dot2, v0dot4, v0dot6, v0dot8}` — identical value set to LTE's `[0, 0.8]` | **Verified directly**, TS 38.331: `sl-ProbResourceKeep-r16 ENUMERATED {v0, v0dot2, v0dot4, v0dot6, v0dot8}` |
| MCS tables (`SL-MinMaxMCS-Config-r16`, `sl-MCS-Table-r16`) | Three selectable MCS tables per resource pool: `qam64`, `qam256`, `qam64LowSE` (up to two configured in addition to a default). Underlying numeric table (shared with PDSCH/PUSCH, TS 38.214 Table 5.1.3.1-1 "MCS table 1", up to 64-QAM): MCS index 0 → Qm=2 (QPSK), target code rate R=120/1024, spectral efficiency **0.2344** bit/s/Hz; ... up to MCS 27/28 (~5.55 bit/s/Hz, 64-QAM). Table 5.1.3.1-2 ("MCS table 2") extends to 256-QAM. | Selectable-table IE **verified directly**, TS 38.331: `SL-MinMaxMCS-Config-r16 ::= SEQUENCE { sl-MCS-Table-r16 ENUMERATED {qam64, qam256, qam64LowSE}, sl-MinMCS-PSSCH-r16 INTEGER (0..27), sl-MaxMCS-PSSCH-r16 INTEGER (0..31) }`. Numeric table entries **verified directly**, TS 38.214 (`src/ts38214.txt`) Table 5.1.3.1-1: "MCS Index 0 \| Modulation Order 2 \| Target code Rate R x[1024] 120 \| Spectral efficiency 0.2344". [Garcia2021] §V.B.1 confirms these same tables (referenced there as [42]=TS 38.214) apply to PSSCH MCS selection, not only PDSCH. |
| Re-evaluation mechanism | A UE that pre-selected N resources at slot n re-checks (re-executes Step 1) shortly before using each of them; if M ≤ N of them are found no longer available, the UE re-runs Step 2 to pick M new resources within a new selection window SW′. Explicitly identified as **new in NR V2X Mode 2 vs. LTE V2X Mode 4** | [Garcia2021] §V.B.2, verbatim: "the re-evaluation is an important novelty introduced in NR V2X SL mode 2 compared to LTE V2X mode 4" |
| Pre-emption mechanism | A UE with low-priority traffic must free a reserved resource if it estimates a higher-priority UE (above an optional configured priority threshold) will use it; can be enabled/disabled per resource pool; applies to both dynamic and SPS schemes; the freeing UE re-runs Step 1 (at slot `r−T3` or later, UE-implementation-timed) and Step 2 with a new window `SW″` bounded by `T2min ≤ T2″ ≤ PDB−(n″−nG)` | [Garcia2021] §V.B.2, verbatim. Also explicitly identified as **new in NR V2X Mode 2 vs. LTE V2X Mode 4** |
| HARQ feedback vs. blind, max retransmissions | Blind retransmissions supported (as in LTE); additionally, NR V2X Mode 2 supports PSFCH-based HARQ ACK/NACK feedback for unicast and groupcast (2 groupcast options: (1) NACK-only from in-range receivers, (2) ACK/NACK from all receivers). Max transmissions bounded by `sl-MaxTransNum-r16` (1..32, see above); exact default/typical values are simulation-study choices, not a single spec-mandated number. | [Garcia2021] §V.C.1 ("Sidelink HARQ feedback"), verbatim: "blind retransmissions can also be considered [44] ... sidelink HARQ feedback can prevent unnecessary blind retransmissions" |
| Congestion control window sizes (TS 38.215, SL CBR/CR) | SL CBR measured over **100 slots** (any µ) or **100·2^µ slots** (per-resource-pool (pre-)config); SL CR measured over a window `a+b+1` = **1000 slots** or **1000·2^µ slots**, with `b < (a+b+1)/2`, `a` positive, and `n+b` not exceeding the UE's last selected resource. `CRLimit` looked up from up to 16 CBR ranges, function of TB priority and (new in Rel-16) absolute UE speed; TX UE evaluates SL CR/CBR at slot `n − Nproc` for a (re-)transmission at slot n, where `Nproc = 2 slots` (µ=0, any UE capability) or `2^µ` / `2·2^µ` slots (µ>0, processing capability 1/2 respectively). | [Garcia2021] §VI, verbatim, explicitly citing TS 38.215 (paper's own reference `[76]` = "3GPP, TS 38.215 NR; Physical layer measurements (v16.3.0, Release 16)" — **verified directly** in the paper's own reference list) |

**5G-LENA implementation note (retained, verified `lena5g.txt` §2.14.2 "NR V2X"):** the ns-3 NR module's
sidelink/NR-V2X extension implements broadcast, PSCCH/PSSCH multiplexing, Mode-2 UE-selected resource
allocation (sensing-based and random, with SPS), on the `nr-v2x-dev` branch of
https://gitlab.com/cttc-lena/nr, design doc at https://5g-lena.cttc.es/static/archive/NR_V2X_V0.1_doc.pdf
(not fetched in this pass).

## Topic C — Link-level (BLER vs. SINR)

A locally cached, **directly usable numeric artifact** was found: `src/per/*.txt`, exported from
WiLabV2Xsim's bundled `PER_table.mat` by `src/percurvesgen.m`. Of 45 exported (scenario, RAT, MCS,
packet-size) combinations, 26 contain real PER(=BLER)-vs-SNR curve data (the remaining 19 are stale
placeholder text — literally the string `404: Not Found` — apparently left over from a failed
re-download and **must not be used**). SINR-at-10%-PER (i.e., 10% BLER) values, linearly interpolated
between the two bracketing curve points, for every valid file:

| Scenario | RAT / MCS | Packet size | SINR @ 10% PER (dB) |
|---|---|---|---|
| Crossing NLOS | 11p MCS2 | 350 B | 5.92 |
| Crossing NLOS | LTE MCS4 | 350 B | 2.85 |
| Crossing NLOS | LTE MCS5 | 350 B | 3.85 |
| Crossing NLOS | LTE MCS7 | 350 B | 6.88 |
| Crossing NLOS | LTE MCS9 | 350 B | 13.71 |
| Highway LOS | 11p MCS0 | 190 B | −0.24 |
| Highway LOS | LTE MCS3 | 190 B | −0.53 |
| Highway LOS | LTE MCS4 | 350 B | 0.14 |
| Highway LOS | LTE MCS5 | 350 B | 1.20 |
| Highway LOS | LTE MCS7 | 190 B | 8.71 |
| Highway LOS | LTE MCS7 | 350 B | 4.14 |
| Highway LOS | LTE MCS9 | 350 B | 10.54 |
| Highway LOS | LTE MCS11 | 550 B | 7.16 |
| Highway NLOS | 11p MCS0 | 190 B | 3.10 |
| Highway NLOS | 11p MCS2 | 350 B | 6.78 |
| Highway NLOS | LTE MCS3 | 190 B | 2.35 |
| Highway NLOS | LTE MCS4 | 350 B | 3.31 |
| Highway NLOS | LTE MCS5 | 350 B | 4.37 |
| Highway NLOS | LTE MCS7 | 350 B | 7.29 |
| Highway NLOS | LTE MCS9 | 350 B | 14.63 |
| Highway NLOS | LTE MCS11 | 550 B | 10.70 |
| Urban LOS | 11p MCS2 | 350 B | 3.86 |
| Urban LOS | LTE MCS4 | 350 B | 0.83 |
| Urban LOS | LTE MCS5 | 350 B | 1.87 |
| Urban LOS | LTE MCS7 | 350 B | 4.89 |
| Urban LOS | LTE MCS9 | 350 B | 11.55 |

Source for all rows above: **[WiLabV2Xsim-PER]** — `src/per/*.txt` (raw SNR/PER pairs), values computed
in this pass by linear interpolation between the bracketing (SNR, PER) samples straddling PER = 0.1. As
expected, BLER decreases monotonically with SINR and with lower MCS (more robust coding); NLOS scenarios
need higher SINR than LOS for the same MCS/packet size (e.g. LTE MCS4/350B: 0.14 dB Highway-LOS vs.
3.31 dB Highway-NLOS).

**Methodology note (verified, `wilab_readme.md` + `todisco2021.txt`):** WiLabV2Xsim/OpenSim-style
simulators derive these curves from link-level simulations under the TR 36.885/TR 37.885 channel models
(Topic D) and use them, together with the estimated per-link SINR, to draw a Bernoulli outcome for
whether each received TB is decoded correctly — this is the standard "PHY abstraction" approach also
described qualitatively (without sidelink-specific numeric curves) in the 5G-LENA docs (`lena5g.txt`):
EESM effective-SINR mapping + per-MCS SINR→BLER lookup, TBS/MCS-driven LDPC code-block segmentation per
TS 38.212 §6.2.2/7.2.2.

**Not independently verified in this pass:** a direct fetch of Bazzi et al. 2019 (Future Internet,
11(6):122) — the paper the WiLabV2Xsim README ties these exact curves to — failed both as a cached PDF
(the cached file is an MDPI CDN "Access Denied" HTML page) and via a fresh WebFetch (HTTP 403). The
paper's own reported PDR/SINR figures are therefore **UNVERIFIED**; only the underlying numeric
PER-vs-SNR table (which the README attributes to it) was recovered.

## Topic D — TR 36.885 / TR 37.885 channel models

**Two generations of 3GPP V2X channel model exist in the corpus and give different levels of detail —
both are reported below, clearly separated, rather than merged.**

### D.1 — TR 36.885 V14.0.0 (Rel-14, LTE V2V study) — Annex A evaluation methodology

All rows **verified directly** by reading `src/tr36885_raw.txt`, Annex A.1:

| Parameter | Value | Clause |
|---|---|---|
| V2V pathloss + shadowing model | **WINNER+ B1 Manhattan grid layout** (LOS/NLOS), antenna height fixed at 1.5 m; pathloss at 3 m used if distance < 3 m. TR 36.885 references this as an *external* model (WINNER+ B1) rather than reproducing its formula inline. | Annex A.1.4, Table A.1.4-1 |
| Shadowing standard deviation | **3 dB for LOS, 4 dB for NLOS** | Annex A.1.4, Table A.1.4-1 |
| Shadowing decorrelation / update | Spatially/temporally correlated shadowing update at each 100 ms location-update tick: `S(n) = exp(−D/D_corr)·S(n−1) + sqrt(1−exp(−2D/D_corr))·N_S(n)`, `D` = per-link distance-change matrix. The V2V decorrelation distance `D_corr` itself is inherited from the referenced WINNER+ B1 external model (not given as a standalone number in this TR); for the eNB–UE link the TR gives **D_corr = 50 m** explicitly. | Annex A.1.4, Table A.1.4-2 body text |
| eNB–UE pathloss (Urban macro reference) | `PL = 128.1 + 37.6·log10(R)` dB, R in km | Annex A.1.4, Table A.1.4-2 |
| Road grid (urban) | **433 m × 250 m** grid, 3 m sidewalk reserved per direction; minimum simulation area 1299 m × 750 m; 2 lanes/direction (4 lanes/street) | Annex A.1.2, Table A.1.2-1 |
| Vehicle headway (both urban and freeway) | Average inter-vehicle distance in the same lane = **2.5 sec × absolute vehicle speed** | Annex A.1.2, Table A.1.2-1, verbatim: "Average inter-vehicle distance in the same lane is 2.5 sec * absolute vehicle speed" — this confirms the task's "2.5 s headway" figure exactly for this (Rel-14) TR |
| Vehicle drop process | Poisson-dropped on roads; location updated every 100 ms; urban intersection turning: straight 0.5 / left 0.25 / right 0.25 | Annex A.1.2 |
| In-band emission model | `{W, X, Y, Z} = {3, 6, 3, 3}` for single-cluster SC-FDMA | Annex A.1.1 (see Topic A) |
| Antenna gain / height | 3 dBi @ 1.5 m (vehicle UE, UE-type RSU); 0 dBi @ 1.5 m (pedestrian UE); RSU height 5 m | Annex A.1.1 |
| UE Tx power / noise figure | 23 dBm / 9 dB | Annex A.1.1 (see Topic A) |
| Own validation PRR results (§9, see also Topic E) | Freeway: PC5 with enhancements "exceeds or approaches 80% average PRR at 320m range." Urban 15 km/h: "90% at 50m range." Urban 60 km/h: "about 60% average PRR at 150m range." | §9.1.1, Table 9.1-1 |

### D.2 — TR 37.885 V15.3.0 (Rel-15, "eV2X"/NR study) — clause 6.1–6.2, explicit closed-form formulas

All rows **verified directly, quoted near-verbatim**, from `src/tr37885.txt` (also cross-checked against
`pdf/tr37885.txt`):

| Parameter | Value | Clause |
|---|---|---|
| Link states | LOS (same street, unblocked), **NLOSv** (same street, blocked by another *vehicle*), NLOS (different streets, blocked by buildings) | §6.2 |
| P(LOS) — Highway | `d ≤ 475 m: P(LOS)=min{1, a·d²+b·d+c}`, `a=2.1013e−6, b=−0.002, c=1.0193`; `d > 475 m: P(LOS)=max{0, 0.54 − 0.001·(d−475)}`. `P(NLOSv) = 1 − P(LOS)` | Table 6.2-1 |
| P(LOS) — Urban | `P(LOS) = min{1, 1.05·exp(−0.0114·d)}`; `P(NLOSv) = 1 − P(LOS)` | Table 6.2-1 |
| Pathloss, LOS & NLOSv (Highway) | `PL = 32.4 + 20·log10(d3D) + 20·log10(fc)` dB | Table 6.2.1-1 |
| Pathloss, LOS & NLOSv (Urban) | `PL = 38.77 + 16.7·log10(d3D) + 18.2·log10(fc)` dB | Table 6.2.1-1 |
| Pathloss, NLOS (both) | `PL = 36.85 + 30·log10(d3D) + 18.9·log10(fc)` dB | Table 6.2.1-1 |
| Shadowing σ | **3 dB for LOS/NLOSv, 4 dB for NLOS** — same numbers as the Rel-14 TR, now attached to an explicit formula | Table 6.2.1-1 |
| NLOSv extra vehicle-blockage loss | `max{0, lognormal RV}`, three cases by antenna height vs. blocker (vehicle) height: **Case 1** (both TX/RX antennas above blocker height): 0 extra loss. **Case 2** (both below blocker height): mean = `9 + max(0, 15·log10(d)−41)` dB, σ = 4.5 dB. **Case 3** (mixed): mean = `5 + max(0, 15·log10(d)−41)` dB, σ = 4 dB. Blocker height randomly drawn from the 3 vehicle types' heights, weighted by their population share. | §6.2.1, verbatim |
| Vehicle types / antenna heights | Type 1 (passenger, low antenna): 5×2.0×1.6 m, antenna 0.75 m. Type 2 (passenger, high antenna): 5×2.0×1.6 m, antenna 1.6 m. Type 3 (truck/bus): 13×2.6×3 m, antenna 3 m. | §6.1.2 |
| Vehicle drop / headway (eV2X, Rel-15) | Distance between rear bumper of leader and front bumper of follower = `max{2 m, Exp(mean = speed·2 sec)}` — **note: 2 sec, not 2.5 sec** (a revision from the Rel-14 TR 36.885 headway model above) | §6.1.2, verbatim |
| Antenna gain / RSU (this TR's own general assumptions, below-6-GHz table) | 23 dBi RSU (macro-comparable case) / vehicle 21 dBi (V2V) / 14 dBi (V2I); simplified sidelink assumption elsewhere in the doc uses 3 dBi (see §6.1.1 general tables) | §6.1.1, Table (line ~245, ~263) |
| UE Tx power / noise figure (below 6 GHz) | UE/RSU Tx power 23 dBm ("33dBm not precluded"); BS RX noise figure 5 dB; **UE RX noise figure 9 dB** | §6.1.1 |
| Traffic model (periodic, Model 1 — matches the 190/300-byte packets used throughout this sheet) | Inter-packet time 100 ms; packet size pattern **{300 B, 190 B, 190 B, 190 B, 190 B}** with random per-UE starting phase; latency requirement 100 ms | §6.1.5 |
| Performance metrics | PRR type 1 (X/Y over vehicles in a range band), PRR type 2 (S/Z over an intended-receiver set), PIR (packet inter-reception) type 1/2 | §6.1.6 |

**Simulator-relevant reconciliation:** the two TRs disagree slightly on vehicle headway (2.5 s in the
Rel-14 LTE study vs. 2 s in the Rel-15 eV2X study) — both numbers are independently verified against
their respective spec text, so this is a genuine 3GPP revision between studies, not a transcription
error; a simulator should pick one explicitly and cite which TR/release it follows rather than silently
averaging them.

## Topic E — Reference validation results (PDR/PRR vs. distance)

| Source | Setup | Result | Citation |
|---|---|---|---|
| TR 36.885 V14.0.0 §9.1.1 (3GPP's own RAN1 calibration results, PC5-based V2V, LTE Mode 4/3) | 100 ms latency target; PRR in `(n·20,(n+1)·20)` m bands | Freeway: PC5 "exceeds or approaches 80% average PRR at 320 m range." Urban 15 km/h: "90% at 50 m range." Urban 60 km/h: "about 60% average PRR at 150 m range." | **Verified directly**, `src/tr36885_raw.txt` §9.1.1, Table 9.1-1 |
| Molina-Masegosa & Gozalvez 2017 (VTC-Spring), Fig. 3/4/5 | LTE-V2V Mode 4, 10 MHz @ 5.9 GHz, 23 dBm, 9 dB NF, WINNER+ B1-style pathloss, 3/4 dB LOS/NLOS shadowing, 10 m decorrelation distance, 190/300-byte packets (0.7/0.5 code rate, 10/20 RB), RSRP threshold −110 dBm, P=0, selection window 100 subframes | Fig. 3: PDR vs. distance for all transmissions. Fig. 4/5: PDR vs. distance split by LOS/NLOS. Text: at 250 m under LOS, "around 20% of all transmitted TBs are incorrectly received due to collisions" — i.e. collisions, not propagation loss, dominate errors out to 250 m under LOS. Exact PDR percentage values at specific distances beyond this quoted figure are only in the paper's plotted figures (not reproduced as text) — **treat plotted-curve values themselves as UNVERIFIED numerically in this pass; only the text-quoted 20%-collisions-at-250m figure is a directly quoted number.** | `src/molina2017vtc.txt` §III–IV |
| Gonzalez-Martin, Sepulcre, Molina-Masegosa, Gozalvez 2019 (arXiv:1807.06508 / IEEE TVT) | First analytical (closed-form) model of average PDR for LTE-V2X Mode 4 as `PDR = 1 − (P̂_HD + P̂_SEN + P̂_PRO + P̂_COL)` (half-duplex, sensing, propagation, collision loss components), validated against simulation "for a wide range of transmission parameters and traffic densities" and against results in a cited companion source | Model structure and validation claim **verified directly** (`src/gonzalez2019.txt`, eq. (6) and surrounding text); the paper's own numeric accuracy plots (model vs. simulation curves) were not re-extracted as text in this pass — **UNVERIFIED as specific numbers**, only the model's existence/structure/validation claim is confirmed | `src/gonzalez2019.txt` |
| Todisco et al. 2021 (WiLabV2Xsim, NR-V2X Mode 2), Fig. 6–11 | 10 MHz @ 5.9 GHz, SCS ∈ {15,30,60} kHz, 350-byte (Fig. 6a/7) and 1000-byte (Fig. 6b) packets, MCS swept, "range" defined as max distance where PRR remains ≥ 0.9 | Fig. 6: SCS=30 kHz needs MCS ≥ 11 (350 B) / ≥ 22 (1000 B) to fit a packet in one slot; SCS=60 kHz needs MCS ≥ 19 (350 B), and *no* MCS fits a 1000 B packet in one slot at 60 kHz. Fig. 7 (MCS=21, 350 B, 1 subchannel = 10 PRBs): higher SCS reduces in-band-emission (IBE) self-interference from co-slot users on other sub-channels, materially improving PRR — confirmed by a with/without-IBE ablation in the same figure. Fig. 8: lowest MCS (best robustness) gives the best PRR/range at all densities tested; a single blind retransmission only helps at low density (50 veh/km) or high MCS, and hurts PRR at high density with low MCS due to added congestion. Fig. 9: at candidate-set (L2 list) percentage **M = 20%** (i.e. `sl-TxPercentage-r16 = p20`, Topic B), PRR improves vs. "Legacy Mode 2" (no L2 list) for 350 B and 1000 B (MCS 4 / MCS 11 respectively, both occupying 5 sub-channels). | **Verified directly**, `src/todisco2021.txt` §IV.B–C, quoted/paraphrased from the text (not from re-digitizing the figures themselves — so treat as qualitative/figure-referenced findings, not exact extracted percentages) |

## Topic F — Deployment status by region

| Region | Status | Date(s) | Source |
|---|---|---|---|
| **USA — FCC 5.9 GHz band reallocation** | First Report and Order: repurposed the lower 45 MHz (5.850–5.895 GHz) of the historic 75 MHz ITS band for unlicensed use; retained the upper 30 MHz (5.895–5.925 GHz) for "safety-related Intelligent Transportation Systems," directing that ITS operations there **transition to C-V2X** (ending DSRC's exclusive claim on the band) | First R&O adopted/circulated **October 28, 2020** (ET Docket No. 19-138) | **Verified directly**, `src/fcc2020.txt` (FCC Fact Sheet, quoted: "repurpose 45 megahertz ... for unlicensed use ... retain 30 megahertz of spectrum in the 5.895-5.925 GHz band ... allow for deployment of C-V2X in the 5.895-5.925 GHz band") |
| **USA — FCC Second Report and Order (final C-V2X technical rules)** | Finalized C-V2X band usage, message prioritization, channel bandwidth, and out-of-band-emission (OOBE) rules; retained **10 MHz channel bandwidths** (three 10 MHz channels in the upper 30 MHz, aggregable to 20 MHz); adopted EIRP PSD limits of **33 dBm/10 MHz, 33 dBm/20 MHz, and 33 dBm/30 MHz**; set OOBE limits at −20 dBm/100 kHz at 1 MHz from the channel edge, −30 dBm/100 kHz at 10 MHz from the edge, −40 dBm/100 kHz at 20 MHz from the edge; finalized the DSRC-to-C-V2X sunset/transition timeline | Adopted **November 20, 2024**, released **November 21, 2024** (FCC 24-123, ET Docket No. 19-138) | **Verified directly**, `txt/fcc24-123.txt` |
| **USA — automaker commitments** | Ford publicly committed (Jan 2019, CES) to deploy C-V2X on all new US vehicle models starting in **2022**; this commitment was **not fulfilled** on that timeline in the US (Ford instead prioritized C-V2X deployment in China over the following years). GM stated in June 2018 it would expand V2V capability starting **2023**, without committing to DSRC vs. C-V2X; this also did not materialize as a full C-V2X rollout on the original timeline. | Commitments made 2018–2019; both acknowledged as unfulfilled per subsequent industry reporting (as of the FCC's 2024 Second R&O) | WebSearch, 2026-09-18: [IEEE Connected Vehicles blog, "Ford commits to deploy C-V2X technology on all new vehicles in the US beginning in 2022"](https://site.ieee.org/connected-vehicles/2019/01/07/ford-commits-to-deploy-c-v2x-technology-on-all-new-vehicles-in-the-us-beginning-in-2022/); [Forbes, Jan 2019](https://www.forbes.com/sites/samabuelsamid/2019/01/07/ford-becomes-first-automaker-to-commit-production-c-v2x-communications/); [Ford Authority, Nov 2024, "FCC Adopts C-V2X Auto Spectrum Rules That Will Impact Ford"](https://fordauthority.com/2024/11/fcc-adopts-c-v2x-auto-spectrum-rules-that-will-impact-ford/) (states neither automaker hit its original all-new-vehicle timeline, and that Ford's C-V2X deployment has instead been "aggressive ... in China") |
| **USA — 5GAA device landscape** | 5GAA-maintained "List of C-V2X Devices" catalogs commercially available C-V2X chipsets, modules, RSUs, and OBUs from vendors incl. Qualcomm, Autotalks, Commsignia, Cohda Wireless, Kapsch, Unex | Report approved by 5GAA board **April 7–8, 2024** | **Verified directly**, `pdftxt/5gaa_cv2x_devices_2024.txt` |
| **Europe — ITS-G5** | Volkswagen's 8th-generation Golf was the first high-volume European production car equipped with V2X, using **ITS-G5 (IEEE 802.11p / "WLANp")**, not C-V2X — described by NXP/Dynniq coverage as "the largest global implementation of V2X in production cars" at the time. VW's subsequent ID.-series EVs continue with DSRC/ITS-G5-based V2X per the same sourcing. | Golf Mk8 launched **24 October 2019**; V2X-equipped variants followed in early production | WebSearch, 2026-09-18: [Dynniq Mobility, "Volkswagen chooses ITS-G5 in new Golf"](https://www.dynniqmobility.com/volkswagen-chooses-its-g5-in-new-golf/); [NXP, "NXP, Volkswagen and Partners Continue to Accelerate the V2X Rollout"](https://www.nxp.com/company/blog/nxp-volkswagen-and-partners-continue-to-accelerate-the-v2x-rollout:BL-THE-V2X-ROLLOUT); [Wikipedia, "Volkswagen Golf Mk8"](https://en.wikipedia.org/wiki/Volkswagen_Golf_Mk8) — **note:** these are secondary/industry-press sources, not a regulatory text or peer-reviewed paper; treat exact "first"/"largest" superlative claims as press framing rather than an independently audited fact |
| **China — MIIT LTE-V2X spectrum** | MIIT (Ministry of Industry and Information Technology) first allocated **20 MHz at 5905–5925 MHz** for LTE-V2X (PC5 direct communication) field trials in **November 2016**; formally issued the spectrum plan for LTE-V2X PC5 in **October 2018**, splitting the band into two 10 MHz sub-bands (lower for V2V, upper for V2I/I2V). Field trial licenses granted to Hainan Province and Tianjin; a stated goal of >30% LTE-V2X user penetration "in specific scenarios" by 2020. | Nov 2016 (trial allocation); Oct 2018 (formal PC5 spectrum plan) | WebSearch, 2026-09-18, synthesizing: [5GAA, "Update on C-V2X Deployment in China" (2019)](https://5gaa.org/content/uploads/2019/05/03.-Update_on_C-V2X_Deployment_in_China.pdf); [arXiv:2002.08736, "A Vision of C-V2X: Technologies, Field Testing and Challenges with Chinese Development"](https://arxiv.org/pdf/2002.08736) — **neither source was fetched/read directly in this pass** (WebSearch snippet synthesis only); treat the exact dates as needing a direct-source re-check before being relied on for anything safety-critical |

---

## Simulator modelling notes

1. **Mode 4 and Mode 2 share one sensing/SPS skeleton; parameterize, don't duplicate.** Both use: a
   sensing window (1000 subframes fixed for Mode 4; 100 or 1100 ms, in slots, (pre-)configurable for
   Mode 2), a selection window `[n+T1, n+T2]` bounded by UE implementation and a latency/PDB deadline, an
   RSRP-threshold exclusion pass that steps up by 3 dB until a target percentage of candidates survive
   (fixed at 20% for Mode 4; RRC-selectable 20/35/50% for Mode 2 — this maps directly onto the verified
   `sl-TxPercentage-r16` IE), an RSSI-based ranking/random-pick step, and reservation-based SPS with a
   probabilistic keep/reselect rule (`probResourceKeep` / `sl-ProbResourceKeep-r16`, identical
   `{0,0.2,0.4,0.6,0.8}` value set in both generations). A single parameterized engine covering both
   generations is well-supported by the verified spec text, not just by informal analogy.
2. **Mode 2 adds two genuinely new mechanisms that Mode 4 does not have** — re-evaluation (recheck
   already-selected-but-not-yet-used resources shortly before use, and reselect only the subset that
   became invalid) and pre-emption (a low-priority UE frees a reserved resource for an estimated
   higher-priority user, gated by an optional priority threshold, per-pool enable/disable). Both are
   explicitly called out in [Garcia2021] as Mode-2 novelties vs. Mode 4 (verified verbatim, Topic B) —
   a simulator that only ports the Mode-4 sensing/SPS loop to Mode 2 without these two checks is missing
   standardized Rel-16 behavior, not just an optional refinement.
3. **RSRP threshold ranges differ by generation and must not be conflated:** LTE Mode 4's 64-entry list
   spans **[−128, −2] dBm** in (derived) 2 dB steps; NR Mode 2's Rel-16 list spans **[−112, −22] dBm**
   in explicit 2 dB steps (`n=0..45`, `−112+2n`). These are independently verified, different numeric
   ranges — a shared "RSRP threshold" config knob in a simulator should carry generation-specific bounds
   rather than one shared min/max.
4. **Vehicle headway is a genuine cross-release discrepancy, not a bug:** TR 36.885 (Rel-14, LTE study)
   specifies 2.5 s mean headway; TR 37.885 (Rel-15, eV2X/NR study) revised this to 2 s mean headway for
   the same highway/urban scenario family. Both values are directly quoted from their respective specs
   in Topic D. Pick one and document which TR/release drives the simulator's default traffic-density
   calibration.
5. **BLER/SINR curves (Topic C) are real cached numeric data (WiLabV2Xsim), not placeholders** — but the
   cache is incomplete (19 of 45 scenario/MCS/size combinations are download failures, not zero-value
   curves) and the source paper's own PDR figures (Bazzi et al. 2019) could not be fetched in this pass
   (MDPI blocked both the cached PDF and a fresh fetch). A simulator adopting these curves should either
   (a) regenerate the missing MCS/scenario combinations from a link-level simulator under the Topic D
   channel models, or (b) explicitly document which (scenario, MCS, size) cells are backed by real data
   vs. interpolated/extrapolated from neighboring MCS indices.
6. **TR 38.913 clause 7.9's 300-byte / 1−10⁻⁵ / 3–10 ms eV2X target** (retained from the prior pass)
   remains a reasonable top-level reliability/latency sanity check independent of the exact MAC
   parameters, and ties directly to the 300-byte packet size that recurs throughout Topics A, D, and E
   (TR 37.885's own Model-1 traffic pattern is literally `{300 B, 190 B×4}`).
7. **In-band emission (IBE) is not a cosmetic detail — Todisco et al. 2021 (Topic E) show it materially
   changes which numerology looks best.** Their SCS ablation (Fig. 7) shows that going from 15 kHz to
   30/60 kHz SCS improves PRR mainly *because* it reduces the number of co-slot, different-subchannel
   transmitters contributing IBE self-interference — with IBE artificially removed, the SCS curves
   converge. A simulator that omits an IBE model (Topic A's `{W,X,Y,Z}={3,6,3,3}` parameterization, or
   an equivalent NR model) will systematically over-predict the benefit of higher numerologies/SCS.
8. **CR/CBR limit *tables* (exact numeric CRLimit-vs-range-vs-priority values) remain UNVERIFIED for
   both generations** — the mechanism, window sizes, and IE names are all directly verified (Topics A
   and B), but the actual numbers populating `SL-CBR-CommonTxConfigList` (LTE) live in an ETSI document
   outside this pass's corpus, and the NR Rel-16 equivalent table is noted by [Garcia2021] itself as "not
   yet defined for NR V2X" as of the paper's writing. Do not invent a CRLimit table; either cite a
   simulator's own documented default (e.g. OpenCV2X's or WiLabV2Xsim's config files, not fetched in this
   pass) or treat congestion control as a pluggable, unspecified-by-3GPP algorithm (which is explicitly
   true per clause text: "3GPP does not specify a particular congestion control mechanism").
9. Do not hardcode `ts36133v8.txt` as a source for anything sidelink-related — confirmed in the prior
   pass to be a Release-8 (2013) RRM spec with zero V2X/sidelink content.
