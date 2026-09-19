# R2e — LTE-V2X (Rel-14) PC5 Mode 4 parameters and link-level LUT (verified)

Sources: [S1] Bazzi et al. 2018 IEEE Access 6:71685 (arXiv:1807.10699); [S2] Molina-Masegosa & Gozalvez 2017 IEEE VT Mag 12(4); [S3] Gonzalez-Martin et al. 2019 IEEE TVT (arXiv:1807.06508) + code https://github.com/msepulcre/C-V2X (get_BLER.m); [S4] Molina-Masegosa & Gozalvez VTC2017-Spring; [S5] Mansouri, Martinez, Härri WONS 2019 (https://dl.ifip.org/db/conf/wons/wons2019/130.pdf); [S7] LTEV2Vsim issue #1 quoting TS 36.331 §6.3.8 ASN.1; [S8] 3GPP TS 36.214 V16.1.0 (ARIB mirror) §5.1.28–5.1.31; [S9] TR 36.885 V14.0.0 recovered text; [S10] Qualcomm R1-1611594 (via S5); [S11] Huawei R1-160284 (via S2/S3).

| Parameter | Value | Source |
|---|---|---|
| Bandwidth / subframe | 10 or 20 MHz; 1 ms TTI; RB pair = 12 × 15 kHz, 14 symbols (9 data, 4 DMRS at symbols 3, 6, 9, 12, 1 switching) | [S1] §II-A; [S2] p.2 |
| sizeSubchannel (TS 36.331 SL-CommResourcePoolV2X) | ENUM {n4, n5, n6, n8, n9, n10, n12, n15, n16, n18, n20, n25, n30, n48, n50, n72, n75, n96, n100, spare…} PRBs | [S7] |
| PSCCH | 2 RBs; adjacent (first 2 RBs of the first sub-channel) or non-adjacent (separate SCI pool) | [S2]; [S5] §II.A |
| SCI format 1 | 32 bits: priority, resource reservation, frequency resource location, time gap, MCS, retransmission index, reserved (per-field widths UNVERIFIED) | [S5] §III.B |
| MCS | 0–28, QPSK/16-QAM only in Rel-14 (clause UNVERIFIED); 190 B: MCS 9 QPSK r0.7 → 10 RBs or MCS 7 r0.5 → 17-RB sub-channel; 300 B: QPSK r0.5 → 20 RBs (22 RBs across two 12-RB sub-channels); Bazzi 300 B beacons MCS 4 (1 BR/TTI) or MCS 7 (2 BR/TTI) with 10-PRB sub-channels | [S4] p.4; [S3] Table III; [S1] Table 2 |
| Sensing window | 1,000 ms (mandated) | [S1] Table 1; [S3] |
| Selection window | T1 ≤ 4 (used 1), 20 ≤ T2 ≤ 100 (used 100) | [S1] Table 1, §VI |
| RSRP threshold | P_th = −128 + 2·(a·8 + b) dBm, a,b ∈ 0..7 priorities; +3 dB per retry until ≥ 20 % candidates remain | [S1] Eq. 1; [S2] p.3; [S3] p.3 |
| Candidate set | ≥ 20 % (R_sel = 0.2 mandated); then 20 % lowest average S-RSSI over T − 100·j (j = 1..10) subframes; random pick | [S1] Table 1, §II-B; [S2]; [S3]; [S4]; S-RSSI per TS 36.214 §5.1.28 |
| SPS reservation intervals | 20, 50, 100, 200, 300, …, 1000 ms (50, 20, 10, 5, …, 1 Hz) | [S5] §II.C |
| Reselection counter | 5–15 at 100 ms; 10–30 at 50 ms; 25–75 at 20 ms | [S2]; [S3]; [S4] |
| probResourceKeep | continuous [0, 0.8] (used 0.4 in S1, 0 in S2/S4); discrete enumeration {0, 0.2, …, 0.8} UNVERIFIED | [S1] Table 1 |
| Half-duplex | cannot sense while transmitting | [S1] §II-B; [S2] fn 3 |
| In-band emissions | TS 36.101 §6.5.2A.3 mask (W/X/Y/Z), used as K_IBE in [S1] App. A; numeric table UNVERIFIED | [S1] App. A |
| Tx power / sensitivity | 23 dBm; sensitivity −90.4 dBm; max input −22 dBm (TS 36.101 v14.4.0) | [S2] p.2; [S1] Table 2; [S9] |
| HARQ | up to 1 blind retransmission per TB (±15 ms window) | [S2] p.3; [S3]; [S4]; [S5] |
| CBR | fraction of sub-channels with S-RSSI above threshold over [n−100, n−1] (TS 36.214 §5.1.30) | [S8] |
| CR | (used in [n−a, n−1] + granted in [n, n+b]) / configured over [n−a, n+b], a+b+1 = 1000, a ≥ 500 (TS 36.214 §5.1.31) | [S8] |
| Illustrative CR limits vs CBR (RAN1 contribution, not normative) | CBR ≤ 0.65: none; 0.65–0.675: 1.6e−3; 0.675–0.70: 1.5e−3; 0.70–0.725: 1.4e−3; 0.725–0.75: 1.3e−3; 0.75–0.80: 1.2e−3; 0.80–0.825: 1.1e−3; 0.825–0.85: 1.0e−3; 0.85–0.875: 0.9e−3; > 0.875: 0.8e−3 | [S5] Table III citing [S10] |
| BLER LUT (190 B, 280 km/h relative speed, from R1-160284) | QPSK r0.7: SNR 0,2,4,6,8,10,12,14,16,18,20 dB → BLER 1, 0.9, 0.7, 0.4, 0.13, 0.045, 0.017, 0.007, 1e−3, 1e−3, 1e−3. QPSK r0.5: SNR −2,0,2,4,6,8,10,12,14 → 1, 0.9, 0.7, 0.3, 0.09, 0.02, 0.002, 1e−3, 1e−3 | [S3] get_BLER.m (verbatim); origin [S11] |
| SINR at 10 % BLER (interpolated from the LUT, not stated in a source) | ≈ 8.7 dB (r0.7); ≈ 5.9 dB (r0.5) | derived |
| Hard-threshold model (300 B) | MCS 4: 2.76 dB; MCS 7: 7.30 dB | [S1] Table 2 |
| Not found | BLER curves for MCS 10/20 and 300 B TBs at LTE numerology; TS 36.331 CBR-PSSCH-TxConfigList numeric values | UNVERIFIED |
