# R2b — NR-V2X Mode 2 PHY/MAC facts (verified from TS 38.212 v16.6.0, TS 38.214 v16.4.0, TS 38.215 v16.3.0, Garcia et al. tutorial)

Sources: Garcia et al., "A Tutorial on 5G NR V2X Communications," IEEE COMST 2021, arXiv:2102.04538 (full text extracted); 3GPP TS 38.212 v16.6.0 (ETSI TS 138 212), TS 38.214 v16.4.0 (ETSI TS 138 214), TS 38.215 v16.3.0 (ETSI TS 138 215), PDF text extracted locally 2026-09-18; arXiv:2106.15303 (Ali, Lagén, Giupponi 2021) in cache as nrv2xnum.txt.

## Numerologies (Garcia Table IV, citing TS 38.211)
| μ | SCS | Slot | Symbols/slot | Range |
|---|---|---|---|---|
| 0 | 15 kHz | 1 ms | 14 | FR1 |
| 1 | 30 kHz | 0.5 ms | 14 | FR1 |
| 2 | 60 kHz | 0.25 ms | 14 (12 ext. CP) | FR1+FR2 |
| 3 | 120 kHz | 0.125 ms | 14 | FR2 |
Rel-16 sidelink design focus FR1 (μ = 0,1,2); one numerology per resource pool (Garcia §V.A).

## Slot structure (Garcia §V.B)
- First SL symbol = AGC duplicate of the second; PSCCH 2 or 3 symbols from the 2nd SL symbol; PSSCH from 2nd symbol to second-to-last; guard symbol after last PSSCH; PSFCH (when configured) = 1 symbol + 1 AGC + 1 guard; 7–14 consecutive SL symbols per slot configurable; at most 9 PSSCH symbols when PSFCH present.

## Sub-channels and pools (Garcia §V.A.3, citing TS 38.214; RAN1 #101-e)
- Sub-channel size ∈ {10, 12, 15, 20, 25, 50, 75, 100} PRBs; pool bitmap length 10..160; pool periodicity 10,240 ms.
- PSCCH PRBs ∈ {10, 12, 15, 20, 25}, < sub-channel size (Garcia §V.B.1, citing RAN1 #99 draft report — clause UNVERIFIED in 38.331).

## SCI format 1-A (TS 38.212 §8.3.1.1) — configuration-dependent total
Priority 3 b; frequency resource assignment ⌈log2(N(N+1)/2)⌉ or ⌈log2(N(N+1)(2N+1)/6)⌉ b (sl-MaxNumPerReserve 2 or 3); time resource assignment 5 or 9 b; reservation period ⌈log2 N_rrp⌉ b; DMRS pattern ⌈log2 N_pat⌉ b; 2nd-stage format 2 b; beta_offset 2 b; DMRS ports 1 b; MCS 5 b; additional MCS table 0–2 b; PSFCH overhead 0–1 b; reserved sl-NumReservedBits. CRC 24 b; polar coded; QPSK.

## SCI format 2-A / 2-B (TS 38.212 §8.4.1.1, §8.4.1.2)
2-A: HARQ process 4, NDI 1, RV 2, source ID 8, destination ID 16, HARQ feedback enabled 1, cast type 2, CSI request 1 = 35 bits (+24 CRC). 2-B: HARQ 4, NDI 1, RV 2, source 8, dest 16, HARQ enabled 1, zone ID 12, communication range requirement 4 = 48 bits (+24 CRC).

## PSSCH / PSFCH (Garcia §V.B.2–V.B.4, §V.C)
- PSSCH: LDPC; QPSK/16/64/256-QAM; 1–2 DMRS ports; MCS tables TS 38.214 Table 5.1.3.1-1 (default, MCS 0–28, e.g. MCS0 Qm 2 R 120/1024 SE 0.2344; MCS28 Qm 6 R 948/1024 SE 5.5547), -2 (256-QAM), -3 (low SE); selection via sl-Additional-MCS-Table and the 1st-stage SCI indicator (TS 38.214 §8.1.3.1).
- PSFCH: period 1, 2 or 4 slots; Zadoff–Chu (PUCCH format 0 based); HARQ for a PSSCH ending in slot n is sent in slot n+a, a = smallest integer ≥ K with PSFCH, K ∈ {2, 3}; unicast ACK/NACK; groupcast option 1 NACK-only (distance-based, range from 16-entry list chosen out of {20,50,80,100,120,150,180,200,220,250,270,300,320,350,370,400,420,450,480,500,550,600,700,1000} m); option 2 ACK/NACK from all; broadcast no feedback.

## Sensing and selection (TS 38.214 §8.1.4; Garcia §VI.B)
- Sensing window [n − T0, n − T_proc,0), T0 ∈ {100, 1100} ms (pool config).
- T_proc,0 (Table 8.1.4-1): 1, 1, 2, 4 slots for μ = 0..3. T_proc,1 (Table 8.1.4-2): 3, 5, 9, 17 slots (3, 2.5, 2.25, 2.125 ms).
- Selection window [n + T1, n + T2], T1 ≤ T_proc,1; T2min ≤ T2 ≤ PDB; T2min ∈ {1, 5, 10, 20}·2^μ slots (sl-SelectionWindowList).
- Exclusion by RSRP threshold per priority pair (sl-ThresPSSCH-RSRP-List); candidate set must be ≥ X % ∈ {20, 35, 50} of the window, else threshold +3 dB and repeat (§8.1.4 step 7).
- Re-evaluation at slot m − T3 (T3 = T_proc,1) before transmitting on a selected resource; pre-emption by higher-priority reservations (sl-PreemptionEnable, optional priority threshold).
- Up to N_MAX ≤ 32 resources per selection (shrinkable by congestion control); an SCI can announce 2 or 3 resources within a 32-slot window.
- Reservation periods: {0, 1..99 (integer ms), 100, 200, …, 1000} ms, ≤ 16 entries per pool (TS 38.331); reselection counter [5, 15] for RRI ≥ 100 ms, [5C, 15C] with C = 100/max(20, RRI) otherwise (TS 38.321 §5.22.1); probResourceKeep 0..0.8.
- HARQ: a single budget of N ≤ N_MAX transmissions per TB; blind retransmission when no PSFCH; minimum gap t_GAP between retransmissions when PSFCH configured. LTE Mode 4 allowed only 1 blind retransmission (Garcia fn. 35).

## Measurements (TS 38.215)
- SL RSSI (§5.1.25): linear average received power over the configured sub-channel from the 2nd symbol of a PSCCH/PSSCH slot.
- SL CR (§5.1.26): (sub-channels used in [n−a, n−1] + granted in [n, n+b]) / (all configured sub-channels over [n−a, n+b]), a + b + 1 = 1000·2^μ slots, b < (a+b+1)/2.
- SL CBR (§5.1.27): fraction of sub-channels whose SL RSSI exceeds a (pre)configured threshold over [n−a, n−1], a = 100·2^μ slots. Threshold range (−112 + 2n) dBm, 0 ≤ n ≤ 45 (Garcia fn. 43, secondary).
- CR limits: up to 16 CBR ranges → CR_limit by priority; NR-V2X table not standardised at the tutorial's time (only LTE-V2X ETSI TS 103 574 table exists) — do not invent.
- Congestion-control levers: fewer sub-channels/lower MCS, smaller L_PSSCH, smaller N_MAX, lower Tx power.
