# R2d — C-V2X validation targets (PDR/PRR vs distance) and deployment status

Method note from the researcher: first-pass AI summaries of the PDFs fabricated some numbers; every value below was re-read from the extracted PDF text or pixel-calibrated from rendered figures (pymupdf); bibliography cross-checked via Crossref.

## Validation targets

### Molina-Masegosa & Gozálvez 2017, "LTE-V for Sidelink 5G V2X Vehicular Communications," IEEE VT Mag 12(4):30–39, DOI 10.1109/MVT.2017.2752798 (https://dspace.umh.es/bitstream/11000/5078/1/11-LTE-V%20for%20Sidelink....pdf)
- Setup: Highway Slow 120 veh/km at 70 km/h; Highway Fast 60 veh/km at 140 km/h; packets 190 B (every 5th 300 B) at 10/20/50 pps; 10 MHz at 5.9 GHz; 4 subchannels of 12 RBs; WINNER+ B1; 23 dBm; NF 9 dB; RSRP threshold −110 dBm; MCS QPSK r0.7 (190 B, 10 RBs) / QPSK r0.5 (300 B, 20 RBs).
- Fig. 3, LTE-V Mode 4 PDR vs distance, Highway Slow, 10 pps: ≈0.97 @0 m, 0.95 @100 m, 0.90 @200 m, 0.79 @300 m, 0.66 @400 m, 0.45 @500 m (digitised).
- Fig. 3, 50 pps: ≈0.91 @25 m, 0.58 @100 m, 0.27 @200 m, 0.11 @300 m.
- Text: 802.11p at 18 Mbps ≈ LTE-V up to ~160 m at 10 pps; LTE-V better beyond.
- Table 2 (exact): Highway Slow no redundancy: 10 pps 32.46 % sub-channels occupied, 3.38 % collisions; 50 pps 80.91 % / 56.64 %. Highway Fast: 10 pps 17.08 % / 0.78 %; 50 pps 62.08 % / 23.33 %.

### Bazzi, Cecchini, Zanella, Masini 2018, "Study of the Impact of PHY and MAC Parameters in 3GPP C-V2V Mode 4," IEEE Access 6:71685–71698, DOI 10.1109/ACCESS.2018.2883401 (arXiv:1807.10699)
- Scenarios: Cologne (urban medium, 1.85×1.85 km², 925 veh, 14.8±8.8 neighbours/100 m); Bologna (urban congested, 1.6×1.3 km², 667 veh, 25.4±25.4 neighbours/100 m); Highway (16 km, 3+3 lanes, 2,015 veh, 49.4±12.5 neighbours/200 m). 300 B at 10 Hz; WINNER+ B1; min SINR MCS4 2.76 dB, MCS7 7.30 dB.
- Fig. 3 average PRR within reference distance (100 m urban / 200 m highway): MCS 4: Cologne ≈0.63, Bologna ≈0.58, Highway ≈0.66; MCS 7: 0.56 / 0.61 / 0.74; MCS 14: 0.50 / 0.60 / 0.70.

### Todisco et al. 2021, "Performance Analysis of Sidelink 5G-V2X Mode 2 Through an Open-Source Simulator," IEEE Access 9:145648–145661, DOI 10.1109/ACCESS.2021.3121151
- Setup: 2 km highway, 3 lanes per direction, wrap-around, speed N(70, 7) km/h, 100 veh/km default; 350 B every 100 ms (1,000 B for CPM-like); 10 MHz; 13 dBm/MHz PSD; 3 dBi; NF 9 dB; WINNER+ B1 LOS; default SCS 15 kHz, MCS 4 (QPSK, Rc 0.3).
- Fig. 7, PRR vs distance at MCS 21, 350 B, no retransmission: worst (SCS 15 kHz with IBE) ≈0.98 @10 m, 0.89 @60 m, 0.74 @90 m, 0.62 @100 m, 0.30 @120 m, 0.04 @150 m; best (SCS 60 kHz without IBE) ≈1.0 @10 m, 0.85 @90 m, 0.76 @100 m, 0.46 @120 m, 0.09 @150 m.
- Fig. 8, range at PRR 0.9 (MCS 4, SCS 15 kHz, no retransmission): 50 veh/km ≈190 m; 100 veh/km ≈155 m; 200 veh/km ≈100 m.
- Fig. 10(a): 350 B, RSRP threshold −110 dBm: range 110 m without the L2 list, 170 m with L2 list (M = 20 %).

### Supplementary: Toghi et al. 2019, arXiv:1904.00071 (C-V2X Mode 4 DCC): qualitative PDR collapse at high density without DCC; not digitised.

## Deployment status
- US: FCC 20-164 (adopted 2020-11-18, released 2020-11-20): 5.850–5.895 GHz unlicensed; 5.895–5.925 GHz ITS, transition DSRC → C-V2X. FCC 24-123 (adopted 2024-11-20): final C-V2X rules (EIRP/OOBE limits, RSU antenna heights), DSRC sunset two years after Federal Register publication (≈ Dec 2026). DA 23-343 (2023-04-24): joint waiver for early C-V2X in 5905–5925 MHz at up to 33 dBm EIRP (Audi, Ford, JLR, Utah DOT, Virginia DOT, AAEON, Advantech, Applied Information, Cohda, Commsignia, Danlaw, HARMAN, Kapsch, Panasonic).
- EU: Council objection (2019-07-08) to the ITS-G5-mandating delegated act → technology-neutral stance; C-Roads ITS-G5 pilots continue (5GAA statement; Agence Europe).
- China: MIIT designated 5905–5925 MHz for LTE-V2X PC5 in October 2018 (IEEE Access survey DOI 10.1109/ACCESS.2020.3012788); Wuxi and Tianjin pilot zones with hundreds of intersections/units as of end 2024 (5GAA "V2X State of Play in China II," 2025).
- Device availability: 5GAA "List of C-V2X Devices" (April 2024).
