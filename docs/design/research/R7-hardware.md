# R7 — V2X Hardware Fact Sheet (cited)

Compiled 2026-09-18. Rules followed: every number carries a source; local-cache citations point into
`/private/tmp/claude-501/-Users-ahmedzaidan-Developer-SCMS-Simulator/9f0649d7-8535-468d-9a82-3700cd13b998/scratchpad/research/`
(paths given relative to that directory); web citations carry URL + access date (2026-09-18). Where a vendor
does not publish a number, the cell says **NOT PUBLISHED**. Where a number could not be corroborated or looks
internally inconsistent, it is flagged **UNVERIFIED** with the discrepancy explained. Nothing below is invented.

---

## A. DSRC / ITS-G5 On-Board Units

### A1. Cohda Wireless MK5 OBU

| Field | Value | Source |
|---|---|---|
| CPU | NXP i.MX6 DL (dual-core), rated 4000 DMIPS | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| RAM | 1 GB SDRAM | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Flash / storage | 4 GB eMMC + 4 GB storage | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| OS | Ubuntu 20.04 LTS, Linux 4.14.98 | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| DSRC radio chipset | NXP RoadLINK SAF5100 | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Secure element | NXP SXF1700 | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Max TX power | +22 dBm (ETSI Mask C) | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| RX sensitivity | −99 dBm @ 3 Mbps | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Bandwidth | 10 MHz | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| GNSS | −167 dBm navigation sensitivity, 2.5 m accuracy | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Doppler / delay-spread tolerance | 800 km/h Doppler, 1500 ns delay spread | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Operating temp | −40 °C to +85 °C | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Power supply | 7–36 V DC | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Dimensions | 130 × 120 × 35 mm | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Standalone radio module TX power | −10 to +23 dBm/antenna port (+26 dBm effective, 2-antenna) | `pdftxt/cohda_mk5_module_datasheet.txt` |
| Standalone radio module RX sensitivity | −97 dBm (5.9 GHz) | `pdftxt/cohda_mk5_module_datasheet.txt` |
| Standalone radio module power draw | 4 W max | `pdftxt/cohda_mk5_module_datasheet.txt` |
| HSM verify/sign throughput | **NOT PUBLISHED** by Cohda for the SXF1700; see D. NXP secure-element family for platform-class figures | — |

Note: the OBU brief's system-level TX-power/sensitivity figures (+22 dBm / −99 dBm) differ from the bare radio
module datasheet's per-port figures (+23 dBm / −97 dBm) — both are genuine but describe different measurement
points (system spec vs. antenna-port spec); both are reported rather than reconciled.

### A2. Cohda Wireless MK6 OBU (dual DSRC + C-V2X)

| Field | Value | Source |
|---|---|---|
| CPU | NXP i.MX 8, rated 8544 DMIPS, operating at 800 MHz | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| RAM | 1 GB SDRAM | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Flash | 16 GB eMMC (+ microSD slot) | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| OS | Debian-based Linux (2025 brief); Ubuntu LTS (2023 brief) | `pdftxt/cohda_mk6_obu_brief_2025.txt`, `pdftxt/cohda_mk6_obu_brief_2023.txt` |
| DSRC chipset | 2× NXP RoadLINK SAF5400 | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| C-V2X chipset | Qualcomm SA515 (LTE-V2X PC5, 3GPP R14) | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Cellular | 5G NR with 4G(LTE Cat 19)/3G/2G fallback | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Secure element | NXP SXF1800, FIPS 140-2 Level 3, CC EAL4+ (Mizar TTM2000 for China variant) | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Max TX power | DSRC +22 dBm (ETSI Mask C); C-V2X +21.5 dBm (Class 3) | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| RX sensitivity | −99 dBm @ 3 Mbps (DSRC) | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Bandwidth | 10 & 20 MHz | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| GNSS | Advanced GNSS with RTK capability | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Operating temp | −40 °C to +74 °C | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Backup power draw | <1 mA @ 12 V input | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| Dimensions | 172 × 168 × 51 mm | `pdftxt/cohda_mk6_obu_brief_2025.txt` |
| HSM verify/sign throughput | **NOT PUBLISHED** by Cohda for SXF1800 itself; see D4 for NXP's own SXF1800 material and D5 for the paired SAF5400 verification-engine figure (2000 msgs/s) | — |

### A3. Cohda MK6C EVK (C-V2X evaluation kit)

| Field | Value | Source |
|---|---|---|
| Application processor | NXP i.MX 8QXP | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| C-V2X chipset | Qualcomm 9150 (PC5) | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| OS | Linux 4.9.88 | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| Secure element | SXF1800, FIPS 140-2 Level 3 | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| C-V2X RX sensitivity | −93.4 dBm | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| C-V2X max TX power | Class 3, 21.5 dBm | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| Bandwidth | 20 MHz | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| Operating temp | −40 °C to +85 °C | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |
| Power supply | 6–24 V | `pdftxt/cohda_mk6c_evk_brief_2024.txt` |

### A4. Autotalks CRATON2 + PLUTON2 (as integrated in Unex OBU-301E/351U)

Autotalks does not appear to publish its own chip-level product datasheet page publicly; the numbers below come
from Unex's Information Sheets for OBUs that embed the CRATON2 chipset, which quote CRATON2 specs verbatim.

| Field | Value | Source |
|---|---|---|
| Communication processor | Autotalks CRATON2, dual 600 MHz ARM Cortex-A7 cores, 1140 DMIPS per core | `pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt` |
| Supervisor core | ARM Cortex-M3, memory-protection unit, ECC-protected memory | `pdftxt/unex_obu301e.txt` |
| eHSM | Dedicated ARM Cortex-M0 CPU (embedded HSM) | `pdftxt/unex_obu301e.txt` |
| RF transceiver | Autotalks PLUTON2 | `pdftxt/unex_obu301e.txt` |
| HSM signing throughput | >110 signatures/s, <9 ms signing latency, ECDSA NIST P-256 or Brainpool P256r1 | `pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt` |
| HW verification engine | >2500 ECDSA NIST P-256 verifications/s ("line-rate") | `pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt` |
| Security certification | FIPS 140-2 Level 3 (targeted/granted per doc revision) | `pdftxt/unex_obu301e.txt` |
| System memory (as integrated) | 128 MB NAND, 128 MB DDR3 | `pdftxt/unex_obu301e.txt` |
| eHSM internal hardware (from FIPS security policy) | ARM Cortex-M0 CPU, 32 KB RAM, 128 KB ROM, dedicated crypto accelerator, OTP memory | `pdftxt/autotalks_craton2_secton_fips.txt` (also `autotalks_fips.txt`, identical text) |
| eHSM ECC support | NIST P-224/P-256/P-384 and Brainpool P256t1/P384t1/P256r1/P384r1 | `pdftxt/autotalks_craton2_secton_fips.txt` |
| FIPS 140-2 level (eHSM) | Overall Level 3 | `pdftxt/autotalks_craton2_secton_fips.txt` |
| Note on verification engine | The FIPS security-policy document explicitly *excludes* "Security – Verification Engine" from the certified eHSM boundary — it is a separate on-chip block, which is why the >2500 verifications/s figure is a chip-level (not certified-HSM-level) spec | `pdftxt/autotalks_craton2_secton_fips.txt` |
| TEKTON3 (3rd-gen, adds 5G-V2X/802.11bd) | Embeds "ultra-low-latency eHSM and hardware verification"; no numeric ECDSA rate published in accessible marketing pages | https://auto-talks.com/products/tekton3/ (accessed 2026-09-18) |
| SECTON / SECTON3 | Dual-mode DSRC+C-V2X companion chipset to CRATON2/TEKTON3; numeric verify rate **NOT PUBLISHED** in any source found | 5GAA device list, `pdftxt/5gaa_cv2x_devices_2024.txt` |

### A5. Autotalks CRATON (original, 2013) — historical baseline

| Field | Value | Source |
|---|---|---|
| Main core | ARM Cortex-R4F, 240 MHz (360 MHz on later ATK4100A1), ECC-protected 32 KB I/D cache, FPU | `pdftxt/autotalks_craton_datasheet.txt` |
| DSP cores | 2× ARC 625D RISC controllers, 240 MHz, 8 KB I/D cache each | `pdftxt/autotalks_craton_datasheet.txt` |
| On-die SRAM | 96 KB, ECC-protected | `pdftxt/autotalks_craton_datasheet.txt` |
| ECC crypto accelerator | 3 identical hardware acceleration engines; each performs one ECDSA-256 verification in <2 ms | `pdftxt/autotalks_craton_datasheet.txt` |
| Sign / ECIES support | Same subsystem also supports ECDSA sign and ECIES (IEEE 1363a) | `pdftxt/autotalks_craton_datasheet.txt` |
| Paired HSM (Unex OBU-201 era product) | Infineon SLE97, ECDSA-256 signing <50 ms latency; hardware verification engine in CRATON does >2000 ECDSA-256 verifications/s | `unex_fcc.txt` |
| Per-MCS RX sensitivity (typical) | 3 Mbps: −97 dBm · 4.5 Mbps: −97 dBm · 6 Mbps: −95 dBm · 9 Mbps: −93 dBm · 12 Mbps: −90 dBm · 18 Mbps: −86 dBm · 24 Mbps: −80 dBm · 27 Mbps: −78 dBm (values vary slightly by column set in source table) | `unex_fcc.txt` |
| Fading-channel sensitivity (10% PER, 6 Mbps, 1000 B) | Rural LOS −92.5 dBm · Highway LOS −91.5 dBm · Urban-approach LOS −91.5 dBm · Crossing NLOS −89.5 dBm · Highway NLOS −88.5 dBm | `unex_fcc.txt` |
| GNSS sensitivity | −135 dBm | `unex_fcc.txt` |
| TX power range | 4.5–25 dBm dynamic; >+20 dBm Class C mask compliant | `unex_fcc.txt` |
| Package/process | Fujitsu 65 nm Automotive ASIC process (Autotalks part ATK4100Ax / Fujitsu MB8AC2060) | `pdftxt/autotalks_craton_datasheet.txt` |

### A6. Commsignia ITS-OB4 (OBU)

Commsignia's own OB4 *product brief* was not present in the cache; the RS4 RSU brief (A/D and C sections)
documents Commsignia's shared HSM (SLI97) and radio-variant story, which the vendor's own marketing states is
common across its OBU/RSU line. Independent web sourcing for OB4 gave only marketing-level detail (no CPU/HSM
datasheet numbers were published on Commsignia's public site or distributor pages found).

| Field | Value | Source |
|---|---|---|
| Radio | Dual-mode DSRC/ETSI-G5 + C-V2X/3GPP R14 | WebSearch snippet, https://commsignia.com/products/obu (accessed 2026-09-18) |
| HSM | Built-in tamper-proof HSM (unnamed in public marketing) | WebSearch snippet, ITS America product directory (accessed 2026-09-18) |
| Certification | DSRC V2V conformance certified 2018-05-08 (v1.17.45-b186782) | omniair.org certified-product listing (accessed 2026-09-18) |
| CPU / RAM / Flash / verify-rate | **NOT PUBLISHED** in any source located | — |

### A7. Danlaw AutoLink OBU

| Field | Value | Source |
|---|---|---|
| Radio variants | DSRC or C-V2X (dual-channel-with-diversity for DSRC; single-channel-with-diversity for C-V2X) | `pdftxt/danlaw_autolink.txt` |
| Main processor | Dual-core @ 800 MHz; secondary MCU handles supervisor/vehicle-interface duties | `pdftxt/danlaw_autolink.txt` |
| Memory | 8 GB eMMC, 1 GB RAM | `pdftxt/danlaw_autolink.txt` |
| Power | 12 or 24 V DC, 400 mA | `pdftxt/danlaw_autolink.txt` |
| Operating temp | −40 °C to +85 °C | `pdftxt/danlaw_autolink.txt` |
| Dimensions | 133 × 84 × 28.5 mm | `pdftxt/danlaw_autolink.txt` |
| RF connectors | 2× FAKRA(C) V2X, 1× FAKRA(Z) GNSS | `pdftxt/danlaw_autolink.txt` |
| Positioning | GNSS with built-in dead reckoning | `pdftxt/danlaw_autolink.txt` |
| Certification | OmniAir Connected Vehicle Certification (DSRC variant) | `pdftxt/danlaw_autolink.txt` |
| HSM / verify-sign rate | **NOT PUBLISHED** | — |

### A8. Ficosa C-V2X OBU

| Field | Value | Source |
|---|---|---|
| Radio | C-V2X, 3GPP Release 14, PC5 sidelink, LTE TDD Band 46D/47 | `pdftxt/ficosa_cv2x_obu.txt` |
| Max TX power | 20 dBm | `pdftxt/ficosa_cv2x_obu.txt` |
| RX sensitivity | −97 dBm typ. @ 6 Mbps | `pdftxt/ficosa_cv2x_obu.txt` |
| Power consumption | <7 W peak, <3 W typical, <24 mW standby | `pdftxt/ficosa_cv2x_obu.txt` |
| Operating temp | −40 °C to +85 °C | `pdftxt/ficosa_cv2x_obu.txt` |
| Security stack | Green Hills Integrity OS for SCMS client; "pilot/test certificates" only per datasheet | `pdftxt/ficosa_cv2x_obu.txt` |
| CPU / HSM part number | **NOT PUBLISHED** (datasheet names "Main, HMI, and CV2X processors" without model numbers) | `pdftxt/ficosa_cv2x_obu.txt` |

### A9. Savari MobiWAVE (Harman) OBU

| Field | Value | Source |
|---|---|---|
| C-V2X chipset | Qualcomm 9150 platform (MobiWAVE MW2000) | WebSearch (auto-connected-car / OmniAir listings), accessed 2026-09-18 |
| HSM | Infineon embedded Secure Element (eSE) — same family as SLI97/SLE97 | https://www.infineon.com/dgdl/Infineon-ISPN-Use-Case-Savari-Securing-V2X+communications-ABR-v01_00-EN.pdf (accessed 2026-09-18); no numeric performance figures found in this PDF |
| Certification | OmniAir certified (dual-mode C-V2X/DSRC MW2000) | omniair.org (accessed 2026-09-18) |
| CPU / RAM / verify-sign rate | **NOT PUBLISHED** — vendor site (savari.net) now redirects into HARMAN's domain and no numeric datasheet could be retrieved (TLS cert mismatch on legacy savari.net PDF link) | attempted fetch of `http://savari.net/wp-content/uploads/2016/10/CN-Savari-OBU-DataSheet.pdf`, failed (accessed 2026-09-18) |

### A10. Unex OBU-301E / OBU-351U — full platform specs

(Chipset performance numbers already tabulated in A4; remaining platform facts below.)

| Field | Value (OBU-301E, DSRC) | Value (OBU-351U, C-V2X) | Source |
|---|---|---|---|
| GNSS module | Telit SL869-V3 | Telit SL869-V3 | `pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt` |
| GNSS sensitivity | Acquisition −146 dBm, Navigation −158 dBm, Tracking −162 dBm | same | both |
| GNSS accuracy | 1.5 m CEP50 with SBAS | same | both |
| Max TX power | >+20 dBm, Class C mask | max +20 dBm, Class C mask | both |
| RX threshold | <−92 dBm, SAE J2945-compliant | typ. <−92 dBm | both |
| Bandwidth | 10 MHz (5/20 MHz by project) | 10/20 MHz | both |
| Operating temp | −40 °C to +85 °C | −40 °C to +85 °C | both |
| Antenna | 2× detachable FAKRA-Z, 5 dBi omni dipole | same connector family | `pdftxt/unex_obu301e.txt` |
| Power input | 6–48 V DC | 6–48 V DC | both |
| Dimensions | 103 × 95 × 31 mm | similar | `pdftxt/unex_obu301e.txt` |

---

## B. C-V2X chipsets / modules

### B1. Qualcomm 9150 C-V2X chipset

| Field | Value | Source |
|---|---|---|
| Announcement | First-announced C-V2X commercial solution, 3GPP Release 14 PC5 direct comms; sampling 2H 2018 | `pdftxt/qualcomm_9150_pr_2017.txt` (Qualcomm press release, Sept 1 2017) |
| Reference design | Includes integrated GNSS, an application processor running the ITS V2X stack, and an HSM | `pdftxt/qualcomm_9150_pr_2017.txt` |
| CPU cores / clock / verify-rate | **NOT PUBLISHED** in the press release (Qualcomm did not release a public technical datasheet for 9150; specs are embedded only in downstream OEM module docs, e.g. Cohda MK6C's "MDM 9150" reference and Commsignia's "Qualcomm 9150" radio-variant option) | `pdftxt/cohda_mk6c_evk_brief_2024.txt`, `pdftxt/commsignia_rs4_brief.txt` |

### B2. Quectel AG15 (LTE-V2X module)

| Field | Value | Source |
|---|---|---|
| Application processor | 1.28 GHz ARM Cortex-A7 | `pdftxt/quectel_ag15_hwdesign.txt` |
| Radio | 3GPP Release 14 LTE-V2X direct comms (PC5), no SIM needed | `pdftxt/quectel_ag15_hwdesign.txt` |
| Bands | C-V2X TDD B47, B46D | `pdftxt/quectel_ag15_hwdesign.txt` |
| Max TX power | Class 3, 23 dBm ± 2 dB | `pdftxt/quectel_ag15_hwdesign.txt` |
| RX sensitivity | B47/B46D (10 MHz): Primary −93 dBm, Diversity −93 dBm, SIMO −96 dBm, 3GPP-SIMO-spec −90.4 dBm (typ.) | `pdftxt/quectel_ag15_hwdesign.txt` |
| Current draw | 240 mA @ 23 dBm (B47), 230 mA @ 23 dBm (B46D) TX; GNSS tracking 86 mA | `pdftxt/quectel_ag15_hwdesign.txt` |
| GNSS | GPS, GLONASS, BeiDou/Compass, Galileo, QZSS; reacquisition/tracking sensitivity −155 dBm | `pdftxt/quectel_ag15_hwdesign.txt` |
| Package | 188-pin LGA, 28.0 × 32.0 × 2.85 mm | `pdftxt/quectel_ag15_hwdesign.txt` |
| Host interface | PCIe (to external app processor) | `pdftxt/quectel_ag15_hwdesign.txt` |
| Max data rate | 26 Mbps TX / 26 Mbps RX (C-V2X TDD) | `pdftxt/quectel_ag15_hwdesign.txt` |
| HSM / verify-sign rate | **NOT PUBLISHED** (AG15 is a modem module; security is left to the host application processor / external SE) | — |

### B3. Quectel AG520R / AG521R (5G + C-V2X module)

| Field | Value | Source |
|---|---|---|
| Positioning in Quectel line | LTE-A Cat 19 & C-V2X automotive module, 3GPP Release 14 compliant, IATF 16949:2016 manufactured | WebSearch snippet of Quectel's own product page (accessed 2026-09-18) |
| LTE peak rate | Up to 1.6 Gbps DL / 75 Mbps UL (LTE side, not the C-V2X sidelink) | WebSearch snippet (accessed 2026-09-18) |
| CPU / RAM / TX power / RX sensitivity / power consumption | **NOT PUBLISHED here** — the official Quectel spec PDF (quectel.com…AG520RAG521R…V1.0.pdf) and its Arrow mirror both failed to load (HTTP 404 / fetch timeout during this research pass); only marketing-page text was retrievable | attempted `https://www.quectel.com/wp-content/uploads/2021/05/Quectel_AG520RAG521R_Series_Automotive_Module_Specification_V1.0.pdf` and Arrow mirror, both failed 2026-09-18 |

### B4. Autotalks dual-mode (CRATON2/SECTON, TEKTON3/SECTON3)

See A4. Autotalks is the vendor most consistently cited for a *dual-mode* (DSRC+C-V2X) chipset family; Qualcomm's
9150 and Quectel's AG-series are C-V2X-only. Commsignia's ITS-RS4 explicitly lists "Autotalks / NXP / Marvell /
Qualcomm 9150" as interchangeable V2X radio variants on one RSU board (`pdftxt/commsignia_rs4_brief.txt`), which
independently corroborates that Autotalks chipsets are used as a drop-in alternative to the Qualcomm 9150 for
C-V2X.

---

## C. Roadside Units (RSUs)

### C1. Cohda Wireless MK5 RSU

| Field | Value | Source |
|---|---|---|
| Chipset | Same as MK5 OBU (NXP SAF5100-based RoadLINK) | `pdftxt/cohda_mk5_rsu_brief.txt` |
| OS | Linux 4.1.15 | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Bandwidth | 10 MHz | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Data rates | 3–27 Mbps | `pdftxt/cohda_mk5_rsu_brief.txt` |
| RX sensitivity | −99 dBm @ 3 Mbps | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Max TX power | +22 dBm (ETSI Mask C) | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Antenna diversity | CDD TX diversity, MRC RX diversity | `pdftxt/cohda_mk5_rsu_brief.txt` |
| GNSS accuracy | 2.5 m ("best-in-class") | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Operating temp | −40 °C to +85 °C | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Enclosure | NEMA 4, 240 × 165 × 67 mm | `pdftxt/cohda_mk5_rsu_brief.txt` |
| Power | PoE (regular) | `pdftxt/cohda_mk5_rsu_brief.txt` |
| CPU / RAM / HSM | **NOT PUBLISHED** in this brief (module-level radio specs only; presumably shares MK5 OBU's i.MX6 DL / SXF17xx, but the RSU brief itself does not restate this) | — |

### C2. Commsignia ITS-RS4

| Field | Value | Source |
|---|---|---|
| CPU | NXP i.MX 6, quad-core, 792 MHz | `pdftxt/commsignia_rs4_brief.txt` |
| RAM | 2 GB DDR3 SDRAM | `pdftxt/commsignia_rs4_brief.txt` |
| Flash | 4 GB eMMC | `pdftxt/commsignia_rs4_brief.txt` |
| Storage | Dual micro-SD | `pdftxt/commsignia_rs4_brief.txt` |
| OS | Linux / RTOS (V2X) | `pdftxt/commsignia_rs4_brief.txt` |
| Ethernet | 10/100/1000 Mbps PoE | `pdftxt/commsignia_rs4_brief.txt` |
| Radio variants (interchangeable) | Autotalks Secton · NXP TEF5100(RF)/SAF5100(BB) · Marvell 88W8987PA (SDIO) · Qualcomm 9150 | `pdftxt/commsignia_rs4_brief.txt` |
| HSM | Infineon **SLI97** | `pdftxt/commsignia_rs4_brief.txt` |
| HSM verify throughput | ">2000 verifications" (units/second implied by context, not stated numerically per-second in this exact phrase) | `pdftxt/commsignia_rs4_brief.txt` |
| HSM signing latency | "<50 usec signing delay" **UNVERIFIED / likely OCR error** — see note below | `pdftxt/commsignia_rs4_brief.txt` |
| Secure flash | Up to 1 MB "SOLID FLASH", EAL6+ certified | `pdftxt/commsignia_rs4_brief.txt` |
| ARM TrustZone | Yes (TZ architecture) | `pdftxt/commsignia_rs4_brief.txt` |
| Enclosure | NEMA4X / IP67 | `pdftxt/commsignia_rs4_brief.txt` |
| IMU | 3-axis gyro (Bosch), 3-axis accel (BMI160), 3-axis mag (BMM150) | `pdftxt/commsignia_rs4_brief.txt` |
| Power | 8–32 V DC / PoE | `pdftxt/commsignia_rs4_brief.txt` |

**Discrepancy note on SLI97 signing latency:** the ITS-RS4 brief's extracted text reads "<50 usec signing delay,"
but two independent Infineon/Autotalks sources state the SLI97's performance profile requirement is "**less than
50 milliseconds**" signing latency (`unex_fcc.txt`: "HSM supports less than 50ms latency on ECDSA 256-bit
signing"; WebSearch of auto-talks.com's own SLI97 announcement: "exceeds the standard performance profile
requirement of less than 50 milliseconds signing latency," accessed 2026-09-18). The "usec" in the Commsignia
PDF-to-text extraction is almost certainly a text-extraction artifact for "ms" — reported here as-extracted per
the no-invention rule, with the corroborating ms-scale sources given for comparison. Treat the RS4 PDF's exact
unit as **UNVERIFIED**; treat 50 ms as the corroborated cross-vendor figure for the SLI97/SLE97 family.

### C3. Kapsch RIS-9260 (dual-mode DSRC + C-V2X RSU)

| Field | Value | Source |
|---|---|---|
| Computer platform | 1.33 GHz dual-core x86, ECC RAM | `pdftxt/kapsch_ris9260.txt` |
| RAM | 1 GB ECC | `pdftxt/kapsch_ris9260.txt` |
| Flash | 4 GB | `pdftxt/kapsch_ris9260.txt` |
| DSRC RX sensitivity | typ. −92 dBm @ 6 Mbps | `pdftxt/kapsch_ris9260.txt` |
| DSRC max TX power | 20 dBm | `pdftxt/kapsch_ris9260.txt` |
| C-V2X RX sensitivity | typ. −95 dBm | `pdftxt/kapsch_ris9260.txt` |
| C-V2X max TX power | 20 dBm (power class 3) | `pdftxt/kapsch_ris9260.txt` |
| C-V2X band | LTE B47 (5.895–5.925 GHz), 10/20 MHz PC5 sidelink | `pdftxt/kapsch_ris9260.txt` |
| Security | HSM, ECC; FIPS 140-2 Level 3, CC EAL4+ | `pdftxt/kapsch_ris9260.txt` |
| Power | PoE 802.3at (<25 W), 24/48 V DC | `pdftxt/kapsch_ris9260.txt` |
| Environmental | −40 °C to +74 °C operating; NEMA 4X/IP67 | `pdftxt/kapsch_ris9260.txt` |
| MTBF | >100,000 h | `pdftxt/kapsch_ris9260.txt` |
| Dimensions | 290 × 200 × 78 mm, ~3 kg | `pdftxt/kapsch_ris9260.txt` |
| Verify/sign rate (numeric) | **NOT PUBLISHED** | — |

### C4. Kapsch RIS-9360 (C-V2X-only RSU)

Same computer platform, power, environmental, security and mechanical specs as RIS-9260 (`pdftxt/kapsch_ris9360.txt`).
C-V2X-specific: 20/10 MHz PC5 sidelink, 20 dBm output (Class 3), typ. −95 dBm sensitivity, LTE B47.
No dedicated Kapsch OBU product datasheet was found in cache or via web search — Kapsch's public V2X hardware
line, as documented, is RSU-only (RIS-9x60 family); **NOT PUBLISHED / no Kapsch OBU product found**.

### C5. Siemens / Yunex Traffic Connected Vehicle RSU

| Field | Value | Source |
|---|---|---|
| CPU | Dual-core @ 800 MHz | `pdftxt/siemens_rsu_datasheet.txt`, `pdftxt/yunex_rsu.txt` (identical spec, rebrand) |
| RAM | 1 GB | both |
| DSRC RX sensitivity | −97 dBm (802.11p) | both |
| Interfaces | 2× DSRC/WAVE, 2× RJ45 10/100, 1× 802.11 b/g/n Wi-Fi/BT, 1× RS232, 1× LTE Cat4 | both |
| Power | 48 V PoE+ (802.3at) | both |
| Max power draw | 12 W (Yunex-branded doc states this explicitly; Siemens-branded doc does not) | `pdftxt/yunex_rsu.txt` |
| GPS accuracy | 2.0 m CEP with WAAS corrections | both |
| Range | ~8000 ft (2500 m), open-field LOS | both |
| Operating temp | −40 °C to +74 °C | both |
| Enclosure | NEMA 6P | both |
| Certification | Meets USDOT FHWA v4.1 RSU spec; Yunex-branded unit explicitly "OmniAir certified" | `pdftxt/yunex_rsu.txt` |
| Radio | DSRC (both docs); Yunex-branded doc additionally lists 1× C-V2X (PC5) interface | `pdftxt/yunex_rsu.txt` |
| HSM | "Hardware Security Module for secure storage of V2X private keys and signature generation" — no part number or throughput given | both |
| Note | The Siemens- and Yunex-branded documents are near-verbatim duplicates (Siemens Intelligent Traffic → Yunex Traffic rebrand); treated as one product line here | `pdftxt/siemens_rsu_datasheet.txt`, `pdftxt/yunex_rsu.txt` |

### C6. Danlaw RouteLink RSU

Danlaw's datasheet mentions RouteLink only as a companion product to AutoLink OBU, without a technical specs
table (`pdftxt/danlaw_autolink.txt`). **NOT PUBLISHED** — no CPU/HSM/radio figures given.

### C7. USDOT Connected Vehicle Pilot deployment facts (NYC / Tampa-THEA / Wyoming)

| Site | RSUs | OBU-equipped vehicles | Backhaul | Sources |
|---|---|---|---|---|
| New York City | ~400–470 RSUs deployed (figures vary slightly by document vintage) | ~3,000 vehicles equipped with Aftermarket Safety Devices (ASD/OBU); some early planning documents cite a 10,000-ASD goal that was not the final deployed count | AT&T FirstNet cellular for RSU backhaul; existing NYCWiN wireless infrastructure also used between RSE and back-office systems | WebSearch of tti.tamu.edu NYC CV Pilot presentation, its.dot.gov, and NYU C2SMART NYC CV Pilot page (accessed 2026-09-18) |
| Tampa (THEA) | 47 RSU locations along the Reversible Express Lane (REL) and Central Business District | Up to 1,000 vehicles equipped with OBUs | Fiber backhaul where available (existing fiber along REL; City of Tampa CBD fiber project); cellular used as interim backhaul where fiber was not yet available | WebSearch of its.dot.gov (thea_cvp_wireless.htm) and FDOT THEA CVP page (accessed 2026-09-18) |
| Wyoming (WYDOT, I-80 corridor) | ~75 RSUs planned; ~76 reported as deployed in some sources | WYDOT planned ~400 vehicles, but ultimately equipped ~325 vehicles: ~170 heavy trucks (regular I-80 users) + ~150 WYDOT fleet vehicles (snowplows, highway patrol, etc.) | **NOT PUBLISHED** in sources reviewed — no explicit fiber/cellular backhaul percentage found; WYDOT lessons-learned material notes rural RSU encounter frequency was a design constraint for certificate lifetime (see R6 for the SCMS angle) | WebSearch of itskrs.its.dot.gov Wyoming executive briefings and traffictechnologytoday.com (accessed 2026-09-18) |
| National aggregate (context, pre-pilot-consolidation) | 6,182 DSRC RSUs + 15,506 OBU-equipped vehicles actively deployed nationally, plus 1,916 more RSUs and 3,371 more OBUs "planned," per AASHTO data cited by the FCC | — | — | FCC, "Modernizing the 5.9 GHz Band," First Report and Order, ET Docket No. 19-138, October 28, 2020, p.14 (`txt/fcc2020factsheet.txt`, local cache) |

Note: pilot-site RSU/OBU counts above are corroborated across 2–3 independent secondary sources each (conference
presentations, ITS JPO/FHWA pages, and news coverage) rather than a single primary USDOT count table, because the
primary ITS JPO/FHWA PDF executive briefings returned HTTP 403 (blocked) during this research session and could
not be fetched directly. Treat exact figures as **directionally reliable, not to-the-unit certified**.

---

## D. HSMs / Secure Elements / Generic Software Crypto

### D1. Infineon AURIX TC3xx HSM (embedded in TriCore MCUs)

| Field | Value | Source |
|---|---|---|
| HSM core | 32-bit ARM Cortex-M3, up to 100 MHz | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| HSM RAM | 96 KB (2nd generation, all TC3xx family); 40 KB on some TC29x variants | `pdftxt/infineon_cybersec_compendium.txt` |
| Crypto accelerators | PKC ECC-256 hardware accelerator, SHA-224/256 hardware accelerator, AES-128 hardware accelerator, TRNG | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| ECC curve support | NIST P-192/P-224/P-256, K-163, B-163, K-233, B-233, Brainpool P160r1/P192r1/P224r1/P256r1, Curve25519, Ed25519 (all ≤256-bit) | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| **ECDSA-256 sign throughput** | **200 signatures/s @ 100 MHz** | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| **ECDSA-256 verify throughput** | **100 verifications/s @ 100 MHz** | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| SHA-256 hash | <2 µs latency per 512-bit block; 65 clock cycles/block (~98 MB/s theoretical peak) | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| TRNG throughput | ~360 kb/s typical @ 100 MHz clock | `pdftxt/aurix_tc3xx_hsm_training.txt` |
| Secure key storage | Dedicated 128 KB HSM DFlash partition | `pdftxt/aurix_tc3xx_hsm_training.txt` |

### D2. Infineon SLI 97 / SLE 97 (V2X secure element / eSE)

| Field | Value | Source |
|---|---|---|
| Product family | 32-bit security controller, security-controllers-for-automotive-applications line (SLI-97CSINFX1M00PE, SLI-97CSINFX8000PE parts) | infineon.com part pages (accessed 2026-09-18) |
| Certification | Common Criteria EAL5+ (hardware platform); EAL5+ "High" cited in some marketing copy | WebSearch of infineon.com/auto-talks.com (accessed 2026-09-18) |
| **Signing latency requirement** | **<50 ms** ECDSA-256 signing latency (Autotalks: "exceeds the standard performance profile requirement of less than 50 milliseconds signing latency") | auto-talks.com press release, "Autotalks Introduces Highly Integrated V2X Security…on Infineon SLI 97" (accessed 2026-09-18); corroborated by `unex_fcc.txt` |
| Role split | SLI97 performs signing/key-management; line-rate *verification* is done by the companion V2X communication processor (e.g., CRATON), not the SLI97 itself | `unex_fcc.txt`, auto-talks.com |
| Verify throughput (numeric, SLI97 alone) | **NOT PUBLISHED** — throughput figures found (">2000 verifications/s") describe the paired radio-chip verification engine, not the SLI97 in isolation | `pdftxt/commsignia_rs4_brief.txt`, `unex_fcc.txt` |
| Full datasheet (clock speed, RAM, detailed ECC timing table) | **NOT PUBLISHED** — Infineon's public part pages for SLI-97CSINFX1M00PE returned only navigation shell content on fetch, no numeric datasheet body | https://www.infineon.com/part/SLI-97CSINFX1M00PE (accessed 2026-09-18) |

### D3. Infineon OPTIGA Trust M (SLS32AIA)

Test conditions: I2C Fast Mode (400 kHz), 25 °C, VCC = 3.3 V, NIST P-256, comparing OPTIGA Trust M1 vs M3 variants,
shielded vs. unshielded I2C connection.

| Operation | M1 | M1 (shielded I2C) | M3 | M3 (shielded I2C) | Source |
|---|---|---|---|---|---|
| ECDSA-256 sign | ~60 ms | ~65 ms | ~65 ms | ~70 ms | github.com/Infineon/optiga-trust-m Wiki, "Crypto Performance" (accessed 2026-09-18) |
| **ECDSA-256 verify** | **~85 ms** | ~90 ms | ~85 ms | ~95 ms | same |
| ECC-256 key-pair generation | ~75–80 ms | — | ~55–60 ms (in session) | — | same |
| ECDH (P-256) | ~60–65 ms | — | ~55–60 ms | — | same |
| SHA-256 hashing | ~12 kB/s | — | ~15 kB/s | — | same |

### D4. NXP SXF1800 (V2X secure element)

| Field | Value | Source |
|---|---|---|
| Core | Arm SC300 | `pdftxt/nxp_saf5400_factsheet.txt` (block diagram callout), NXP block-diagram PDF (`nxp.com/assets/block-diagram/en/SXF1800.pdf`, accessed 2026-09-18) |
| Flash | 2 MB (software uses ~1 MB, leaving 1 MB for customer data) | NXP block-diagram PDF (accessed 2026-09-18) |
| OS | NXP JCOP (Java Card OS); functionality split into a "V2X applet" (ECDSA sign, ECIES, key mgmt) and a "GS applet" (secure generic-data / certificate storage) | NXP block-diagram PDF (accessed 2026-09-18) |
| Host interface | SPI, up to 5 Mbit/s (mode 0) | NXP product page, www.nxp.com/products/SXF1800 (accessed 2026-09-18) |
| Certifications | Common Criteria EAL5+ (hardware platform); CC EAL4+ against C2C-CC HSM Protection Profile v1.4.0; FIPS 140-2 Level 3 (Level 4 physical security) | NXP product page (accessed 2026-09-18) |
| Operating temp | −40 °C to +105 °C, single 1.8 V supply | NXP product page (accessed 2026-09-18) |
| Data retention | 15 years | NXP product page (accessed 2026-09-18) |
| ECC curves | NIST and Brainpool short-Weierstrass curves ("crypto agile") | NXP product page (accessed 2026-09-18) |
| **Verification throughput** | Vendor language: "ultra-fast verification on incoming messages at greater than 1000 messages per second" | NXP marketing copy found via WebSearch of nxp.com/products/SXF1800 (accessed 2026-09-18) — note: verification for SXF1800-equipped systems is typically offloaded to the paired baseband chip's HW engine (e.g., SAF5400's 2000 msgs/s, see D5), so this SXF1800-attributed figure should be read as a system-level, not necessarily SE-silicon-alone, claim |
| Signing latency (numeric) | **NOT PUBLISHED** — vendor states "signature generation performance exceeding single and dual channel requirements, with low latency" but gives no ms/sig figure | NXP product page (accessed 2026-09-18) |
| Common Criteria Security Target (SXF1800HN/V102B) | Confirms ECDSA sign key mgmt architecture (P-256/P-384), does not publish timing figures (it is a security-functional document, not a performance datasheet) | `pdftxt/nxp_sxf1800_security_target.txt` |

### D5. NXP SAF5100 / SAF5400 (baseband + integrated verification engine)

| Field | Value | Source |
|---|---|---|
| **ECDSA verification throughput** | **2000 messages/s** (Brainpool or NIST 256-bit curves) | `nxp_saf5400.txt`, `pdftxt/nxp_saf5400_factsheet.txt` |
| RX packet rate | 2000+ packets/s | `nxp_saf5400.txt` |
| Host interface | SDIO / SPI, 100 Mbit/s host bus | `nxp_saf5400.txt` |
| TX power | 0 dBm linear OFDM (chip output before PA); 33 dB TX gain control range | `nxp_saf5400.txt` |
| Noise figure | 6 dB @ 5.9 GHz | `nxp_saf5400.txt` |
| RX gain control range | 78 dB | `nxp_saf5400.txt` |
| RX gain settling time | <100 ns | `nxp_saf5400.txt` |
| TX EVM | better than −32 dB @ 5.9 GHz | `nxp_saf5400.txt` |
| Reference clock | 40 MHz nominal | `nxp_saf5400.txt` |
| Secure boot time | 500 ms (secure mode) | `nxp_saf5400.txt` |
| Supply rails | 1.6 V analog, 1.2 V digital, 1.8–3.3 V I/O | `nxp_saf5400.txt` |
| Qualification | AEC-Q100 Grade 2 | `nxp_saf5400.txt` |
| Package | LFBGA249, 12 × 12 mm | `nxp_saf5400.txt` |
| Standards | IEEE 802.11p, IEEE 1609.4, ETSI EN 302663, ETSI EN 302571 | `nxp_saf5400.txt` |
| Variants | SAF5300/V110 (single channel/antenna), SAF5400/V110 (single channel/dual antenna); pin/package/software compatible | `nxp_saf5400.txt` |

### D6. Microchip ATECC608 (A/B/C variants)

| Field | Value | Source |
|---|---|---|
| ECDSA support | FIPS 186-3 sign, verify, key agreement (ECDH), NIST P-256 (secp256r1) | `pdftxt/atecc608a_summary.txt`, `pdftxt/atecc608c_summary.txt` |
| I2C clock | 1 MHz standard interface (also Single-Wire Interface variant) | `pdftxt/atecc608a_summary.txt`; confirmed in full TFLXTLS datasheet (DS40002249B, accessed 2026-09-18) |
| EEPROM | 1,400 bytes total (128 B config + 1,208 B data slots + 64 B OTP) | Microchip ATECC608B-TFLXTLS datasheet DS40002249B (accessed 2026-09-18) |
| Supply voltage | 2.0–5.5 V | same |
| Active current | 2–3 mA idle-for-I/O; up to 14 mA during ECC command execution (clock divider = 0x0) | same |
| Sleep current | 30 nA typ. (VCC≤3.6V, ≤55 °C); ≤2 µA over full range | same |
| Watchdog timeout | 0.7–1.7 s (typ. 1.3 s) forces sleep if host doesn't idle/refresh | same |
| Write endurance | 400,000 cycles/byte @ 85 °C; data retention 10 yr @ 55 °C / 30–50 yr @ 35 °C | same |
| **ECDSA-256 Sign/Verify/GenKey execution-time table (ms)** | **NOT PUBLISHED in any publicly accessible datasheet variant checked.** The full command-timing table (referenced elsewhere as "Table 10-5" of the *full, non-preconfigured* ATECC608A/B datasheet) is omitted from every publicly downloadable variant examined: the ATECC608A/B "CryptoAuthentication Device Summary Data Sheet" (DS40001977/DS40002239) and the TrustFLEX/Trust&GO preconfigured datasheets (e.g., ATECC608B-TFLXTLS DS40002249B, which explicitly states in its revision history "Reduction in information provided about commands"). Widely-repeated third-party figures (e.g., "~50–165 ms depending on command") could not be traced to a citable Microchip primary source in this research pass and are therefore **UNVERIFIED / omitted** rather than reported as fact. | Attempted: `https://ww1.microchip.com/downloads/aemDocuments/documents/SCBU/ProductDocuments/DataSheets/ATECC608B-TFLXTLS-CryptoAuthentication-Data-DS40002249.pdf` (fully fetched and read, confirms the omission), plus WebSearch of ATECC608A DS40001977B (2026-09-18) |

### D7. Autotalks eHSM / CRATON verification engine

Already tabulated in A4/A5. Summary of the two generations:
- **Original CRATON (2013):** 3 parallel HW ECDSA engines, each <2 ms/verification (`pdftxt/autotalks_craton_datasheet.txt`); paired external Infineon SLE97 HSM does signing <50 ms (`unex_fcc.txt`).
- **CRATON2 (current):** eHSM (dedicated Cortex-M0) signs >110 sig/s at <9 ms latency; a separate on-chip HW verification engine does >2500 ECDSA-256 verifications/s (`pdftxt/unex_obu301e.txt`, `pdftxt/unex_obu351u.txt`, `pdftxt/autotalks_craton2_secton_fips.txt`).

### D8. Generic ARM Cortex-A software ECDSA P-256 (OpenSSL `speed`, Raspberry Pi)

These are third-party community benchmarks (public GitHub gists running `openssl speed`), not vendor-published
figures. OpenSSL version/build flags are not stated in the sources; treat as indicative, not lab-certified.

| Platform | SoC / core | Sign ops/s | Verify ops/s | Source |
|---|---|---|---|---|
| Raspberry Pi 3 Model B | BCM2837, 4× Cortex-A53 @ 1.2 GHz | 1,631.1 | 775.3 | github.com/dchest gist "Raspberry Pi 3B+ and 3B openssl speed" (accessed 2026-09-18) |
| Raspberry Pi 3 Model B+ | BCM2837B0, 4× Cortex-A53 @ 1.4 GHz | 1,914.7 | 908.4 | same gist |
| Raspberry Pi 4 Model B | BCM2711, 4× Cortex-A72 @ 1.5 GHz | 4,097.4 | 1,550.7 | github.com/HimaJyun gist "Raspberry Pi 4 OpenSSL speed" (accessed 2026-09-18) |

Note: neither gist states the exact Raspbian/OpenSSL version, so these numbers should be read as order-of-magnitude
reference points for single-core software ECDSA-P256 on Cortex-A53/A72, not as a controlled A/B benchmark.

### D9. ESCRYPT CycurHSM

**NOT PUBLISHED / NOT FOUND.** No CycurHSM datasheet or performance figures were present in the local cache, and
targeted web search was not run for this item within the research budget of this pass (deprioritized after the
higher-value items above were exhausted). Treat as an open item if the simulator needs a CycurHSM profile.

---

## E. PQ-capable hypothetical OBU

### E1. Cortex-M7 benchmark of Dilithium (ML-DSA precursor) and Falcon — Howe & Westerbaan (2022)

Board: STM32 Nucleo-144, STM32F767ZI MCU, ARMv7E-M, **216 MHz** clock, 2 MB flash, 512 KB SRAM. 1000-iteration
averages; ms figures computed at 216 MHz.

| Algorithm / parameter set | Operation | Avg. KCycles | Avg. time (ms) | Source |
|---|---|---|---|---|
| Dilithium-2 | Key Gen | 1,437 | 6.7 | `nist2022_arm.txt` |
| Dilithium-2 | Sign | 3,658 | 16.9 | `nist2022_arm.txt` |
| Dilithium-2 | Verify | 1,429 | 6.6 | `nist2022_arm.txt` |
| Dilithium-3 | Key Gen | 2,566 | 11.9 | `nist2022_arm.txt` |
| Dilithium-3 | Sign | 6,009 | 20.7 | `nist2022_arm.txt` |
| Dilithium-3 | Verify | 2,453 | 11.4 | `nist2022_arm.txt` |
| Dilithium-5 | Key Gen | 4,368 | 20.2 | `nist2022_arm.txt` |
| Dilithium-5 | Sign | 8,157 | 37.8 | `nist2022_arm.txt` |
| Dilithium-5 | Verify | 4,287 | 19.8 | `nist2022_arm.txt` |
| Falcon-512 (native FPU) | Key Gen | 77,475 | 358.7 | `nist2022_arm.txt` |
| Falcon-512 (native FPU) | Sign (dynamic tree) | 4,778 | 22.1 | `nist2022_arm.txt` |
| Falcon-512 (native FPU) | **Verify** | 559 | **2.6** | `nist2022_arm.txt` |
| Falcon-1024 (native FPU) | Key Gen | 193,707 | 896.8 | `nist2022_arm.txt` |
| Falcon-1024 (native FPU) | Sign (dynamic tree) | 10,243 | 47.4 | `nist2022_arm.txt` |
| Falcon-1024 (native FPU) | **Verify** | 1,136 | **5.3** | `nist2022_arm.txt` |

Full citation: J. Howe and B. Westerbaan, "Benchmarking and Analysing NIST PQC Lattice-Based Signature Scheme
Standards on the ARM Cortex M7," 2022 (local cache: `nist2022_arm.txt` / `nist2022_arm.pdf`).

### E2. pqm4 Cortex-M4 cycle counts (official pqm4 benchmark suite CSV)

Source: `pqm4_benchmarks.csv` (local cache; this is the standard pqm4 project's own published cycle-count
table — clock frequency of the reference Cortex-M4 board is **not restated inside the CSV itself**, so only
raw cycle counts are reported here, not derived milliseconds).

| Scheme / implementation | KeyGen (cycles) | Sign (cycles, mean) | Verify (cycles, mean) |
|---|---|---|---|
| ml-dsa-44 (clean) | 1,874,405 | 7,925,955 | 2,063,096 |
| ml-dsa-44 (m4f, optimized) | 1,426,025 | 3,943,121 | 1,421,623 |
| ml-dsa-65 (clean) | 3,205,533 | 12,359,056 | 3,377,305 |
| ml-dsa-65 (m4f) | 2,516,006 | 6,193,171 | 2,415,944 |
| ml-dsa-87 (clean) | 5,341,863 | 15,579,513 | 5,610,203 |
| ml-dsa-87 (m4f) | 4,275,859 | 7,947,380 | 4,193,104 |
| fndsa_provisional-512 (Falcon-512, m4f) | 67,693,338 | 22,469,685 | **396,949** |
| fndsa_provisional-1024 (Falcon-1024, m4f) | 308,608,613 | 48,321,135 | **793,856** |
| sphincs-sha2-128f-simple (clean) | 15,742,990 | 368,575,228 | 21,923,628 |

(Sign-cycle figures reflect rejection-sampling variance for lattice schemes — pqm4 reports mean over many runs
including rejected attempts.)

### E3. Real hardware measurement on a Cohda MK6 device (Qualcomm ARMv8 V2V chipset) — Twardokus, Bindel, McCarthy, Rahbari

This is the most directly relevant data point for a "PQ-capable OBU": actual PQ and ECDSA signature timings
measured on production Cohda V2V hardware (not a generic dev board), using Botan (ECDSA/Dilithium/XMSS) and
liboqs (Falcon/SPHINCS+), 1000-execution averages.

| Algorithm | Sign avg (ms) | Sign σ | Sign rate (Hz) | Verify avg (ms) | Verify σ | Verify rate (Hz) | 100-vehicle-viable? |
|---|---|---|---|---|---|---|---|
| ECDSA (baseline) | 7.820 | 0.141 | 128 | 0.001 | 0.001 | ~675,000 (see note) | Yes |
| Falcon | 2.152 | 0.036 | 465 | 0.446 | 0.023 | 2,243 | **Yes** |
| Dilithium | 2.634 | 1.741 | 380 | 0.189 | 0.184 | 5,299 | Yes (verify), marginal (frame duration) |
| SPHINCS+ | 5.485 | 0.002 | 182 | 5.436 | 0.191 | 184 | No |
| XMSS | 1,405.408 | 31.150 | 0.7 | 2.780 | 0.381 | 359 | No (sign too slow) |

Note on the ECDSA verify figure: the paper's Table V lists x̄ = 0.001 ms with a derived rate of 675,219 Hz — this
implies a true mean nearer 1.5 µs than a rounded 1 ms, and is unusually fast for pure-software ECDSA verify on an
embedded ARMv8 core; it may reflect Botan-library precomputation/caching effects in the test harness rather than
raw uncached verify cost. Reported exactly as published, flagged **anomalous but as-published**, not corrected.

System-capacity (v_max) implication, from the same paper (100-vehicle-density urban scenario, Erlangen traffic
model, frame-duration-constrained *and* verify-time-constrained):

| Design | Frame-duration v_max | Verify-time v_max | Binding constraint |
|---|---|---|---|
| Pure ECDSA | 165 | 67,521 | frame duration |
| Partially-Hybrid Falcon | 101 | 224 | frame duration |
| Partially-Hybrid Dilithium | 53 | 529 | frame duration |
| Partially-Hybrid SPHINCS+ | 21 | 18 | verify time |
| Partially-Hybrid XMSS | 49 | 35 | verify time |

Full citation: G. Twardokus, N. Bindel, H. Rahbari, S. McCarthy, "When Cryptography Needs a Hand: Practical
Post-Quantum Authentication for V2V Communications," NDSS 2024 (local cache: `pdftxt/eprint2022_483_pqv2v.txt`
≡ `pdftxt/ndss2024_pq_v2v.txt`, eprint.iacr.org/2022/483).

### E4. PQ-V2Verifier open testbed (same research group)

Open-source hardware-in-the-loop testbed integrating NIST PQC signatures into IEEE 1609.2 on top of SDR + Cohda
MK6 hardware; used to demonstrate replay/forgery attacks enabled by a future large quantum computer and PQC
countermeasures. github.com/twardokus/pq-v2verifier. Source: `vehiclesec_demo.txt` / `pdftxt/vehiclesec2024_demo.txt`
(Twardokus & Rahbari, VehicleSec 2024 demo paper).

### E5. Announced PQ hardware accelerators (NXP / Infineon)

**NOT PUBLISHED.** Neither the NXP SXF1800/SAF5400 collateral nor the Infineon AURIX/OPTIGA/SLI-97 collateral
gathered in this research pass mentions a dedicated post-quantum (ML-DSA/ML-KEM/Falcon) hardware accelerator.
Infineon's Luna-HSM-adjacent competitor Thales *does* publish PQC roadmap language for its Luna HSM family (ML-DSA,
ML-KEM, LMS/HSS support — see F2), but that is a backend appliance, not an automotive/V2X-class accelerator.
No vendor-announced automotive-grade PQ silicon accelerator was found; treat this as an open gap for the
simulator's "future hardware" hypothesis rather than a documented fact.

---

## F. Backend / SCMS-server-class hardware

### F1. OpenSSL `speed` ECDSA P-256 on modern x86 (software baseline for backend RA/PCA/MA nodes)

| Source | Sign ops/s | Verify ops/s | CPU stated? |
|---|---|---|---|
| OpenSSL Cookbook (Ivan Ristić), "Performance" chapter | 20,508.1 | 6,566.2 | Not stated in the retrieved excerpt | feistyduck.com/library/openssl-cookbook/online/openssl-command-line/performance.html (accessed 2026-09-18) |
| Community benchmark (search snippet, unverified provenance) | 45,069.6 | 14,166.6 | "Intel Core i7" (unspecified model) | WebSearch snippet, exact source page not independently re-fetched (accessed 2026-09-18) — treat as **UNVERIFIED** secondary citation |

Both rows are software (no HSM) benchmarks representative of an unaccelerated backend core; actual SCMS RA/PCA/MA
throughput in production is normally bounded by the HSM (see F2), not the host CPU.

### F2. Thales Luna HSM family (Network, PCIe, and government "T-series")

**Luna Network HSM (A-series and S-series, standard vs. multi-factor auth), per-appliance performance tiers:**

| Model tier | RSA-2048 (tps) | ECC P-256 (tps) | AES-GCM (tps) | Source |
|---|---|---|---|---|
| A700 / S700 (Standard) | 1,000 | 2,000 | 2,000 | `thales_luna7.txt` / `pdftxt/thales_luna_network.txt` |
| A750 / S750 (Enterprise) | 5,000 | 10,000 | 10,000 | same |
| A790 / S790 (Maximum) | 10,000 | **22,000** | 17,000 | same |

**Luna PCIe HSM:** identical per-tier numbers to the Network HSM table above (`pdftxt/thales_luna_pcie.txt`).
General claim across both product briefs: "over 20,000 ECC and 10,000 RSA operations/second for high-performance
use cases" (`pdftxt/thales_luna_network.txt`, `pdftxt/thales_luna_pcie.txt`).

**Luna Network HSM physical/electrical:** 1U rack appliance, 100 W max / 84 W typical power, MTBF 171,308 h,
FIPS 140-2 Level 3 and FIPS 140-3 Level 3 validated, Common Criteria EAL4+ (`pdftxt/thales_luna_network.txt`).
**Luna PCIe HSM:** 18 W max / 14 W typical, MTBF 997,508 h (`pdftxt/thales_luna_pcie.txt`).

**Thales TCT "Government" T-series (`pdftxt/thales_hsm_family_dlt.txt`):**

| Model | RSA-2048 (tps) | RSA-4096 (tps) | ECC P-256 (tps) | ECC P-384 (tps) | ML-DSA-87 (tps) |
|---|---|---|---|---|---|
| T-2000 (Standard) | 1,400 | 350 | 3,000 | 2,000 | — |
| T-5000 (Enterprise) | 14,000 | 3,500 | 16,000 | 16,000 | — |
| Luna Tablet & Backup (use-case specific) | 62 | 8 | 383 | — | **20** |

Note: this T-series document is the only one in the cache that publishes a **post-quantum ML-DSA-87 throughput
figure (20 tps)**, alongside classical RSA/ECC numbers — directly useful for a PQ-capable backend hardware
profile. Thales also states support for ML-KEM and the stateful hash-based LMS/HSS schemes on the same Luna HSM
line, without a published numeric throughput for those algorithms.

**Thales Luna HSM for 5G (separate performance brief, `pdftxt/thales_5g_perf.txt`):** demonstrates the same silicon
family's throughput on ECIES/Milenage workloads (up to 56,000 TPS ECIES P-256 decrypt with 8 HSMs in a cluster;
test system: Intel Xeon E5-2640 v4 @ 2.40 GHz × 40 cores, CentOS 8, Luna Network HSM A790) — useful as a
cross-check that the A790's 22,000 ECC-tps rating is achieved on realistic backend server hardware, not just a
marketing ceiling.

### F3. Utimaco HSM

**Largely NOT PUBLISHED in this pass.** A WebSearch snippet (unconfirmed by direct datasheet fetch — the
CryptoServer CSe datasheet URL 404'd) suggested "100 tps for 256-bit [ECC] keys" for one CSe model and "up to
40,000 RSA-2048 sig/s" for the u.trust GP HSM Se-series, but neither figure could be corroborated against a
primary Utimaco document in this session. **Treat both numbers as UNVERIFIED.**

### F4. CAMP SCMS Proof-of-Concept — software ECDSA reference numbers

| Field | Value | Source |
|---|---|---|
| ECDSA-256 sign rate (era: CAMP VSC3 design, ~2016 doc referencing older reference implementation) | ~1,500 signatures/s on a 2 GHz processor | camp_ee_req.txt — "Security Credential Management System Proof-of-Concept Implementation, EE Requirements and Specifications Supporting SCMS Software Release 1.1," CAMP VSC5 Consortium, submitted to NHTSA, May 4, 2016 |
| ECDSA-256 verify rate (same context) | ~300 verifications/s on a 2 GHz processor | same |
| AES symmetric throughput (same doc, for comparison) | 81 MB/s encryption on a 2 GHz processor; 1,000,000 100-byte MAC operations/s | same |
| RA/PCA/MA appliance-level throughput (numeric, dedicated) | **NOT PUBLISHED** — the EE-Requirements document gives only the generic ECDSA/AES software-library numbers above, not a per-component RA/PCA/MA transaction-rate specification | camp_ee_req.txt |

Note: the ~1,500 sign/s vs. ~300 verify/s asymmetry (verify slower than sign) is unusual relative to modern
optimized libraries (which are typically comparable or verify-faster) and likely reflects the specific, era-
appropriate (~2010s) unoptimized ECDSA implementation referenced by the CAMP VSC3 design — reported as published,
not adjusted.

### F5. PQ signature throughput on modern x86 (AVX2/AVX-512), for a PQ-ready backend profile

| Algorithm | Metric | Value | CPU | Source |
|---|---|---|---|---|
| Dilithium2 (round-3, AVX2) | KeyGen / Sign / Verify (cycles) | 106,000 / 251,050 / 107,338 | Intel Core i7-11700F | arxiv.org/pdf/2306.01989, "Optimized Vectorization Implementation of CRYSTALS-Dilithium" (accessed 2026-09-18) |
| Dilithium3 (round-3, AVX2) | KeyGen / Sign / Verify (cycles) | 246,988 / 406,248 / 174,218 | Intel Core i7-11700F | same |
| Falcon-512 | Sign throughput | >9,000 signatures/s | Intel Core i7-6567U @ 3.3 GHz (AVX2) | falcon-sign.info reference implementation notes, via WebSearch (accessed 2026-09-18) |
| Falcon-512 | Verify latency | 3.6 µs (claimed 2.6× faster than prior optimized baseline) | AMD Zen5 core, AVX-512 | eprint.iacr.org/2026/1539, "Falcon Verify on AVX-512: Speed Records" (accessed 2026-09-18) |

Clock speed for the i7-11700F was not restated alongside the cycle counts in the retrieved excerpt, so cycles are
reported rather than a derived time; at that CPU's ~2.5 GHz base clock, Dilithium2 verify (107,338 cycles) would
be on the order of 43 µs, but this conversion is **not** stated by the source and is offered here only as an
order-of-magnitude sanity check, not a cited fact.

---

## G. Radio-chipset PHY facts

| Fact | Value | Source |
|---|---|---|
| SAF5100/SAF5400 supported band | 5.850–5.925 GHz | `nxp_saf5400.txt` |
| SAF5100/SAF5400 modulation BW | 10 MHz | `nxp_saf5400.txt` |
| SAF5100/SAF5400 TX gain control | 33 dB | `nxp_saf5400.txt` |
| SAF5100/SAF5400 noise figure | 6 dB @ 5.9 GHz | `nxp_saf5400.txt` |
| SAF5100/SAF5400 RX gain control | 78 dB | `nxp_saf5400.txt` |
| SAF5100/SAF5400 TX EVM | better than −32 dB | `nxp_saf5400.txt` |
| Cohda MK5 module TX power | −10 to +23 dBm/port (+26 dBm effective, 2-antenna) | `pdftxt/cohda_mk5_module_datasheet.txt` |
| Cohda MK5 module RX sensitivity | −97 dBm | `pdftxt/cohda_mk5_module_datasheet.txt` |
| Cohda MK5 shark-fin/kit antenna | 1× DSRC antenna bundled with OBU kit (gain not stated in this brief) | `pdftxt/cohda_mk5_obu_brief_2024.txt` |
| Unex OBU-301E bundled antenna gain | 2× FAKRA-Z DSRC, **5 dBi omni dipole** | `pdftxt/unex_obu301e.txt` |
| Autotalks CRATON-based OBU per-MCS sensitivity | 3/4.5 Mbps: −97 dBm · 6 Mbps: −95 dBm · 9 Mbps: −93 dBm · 12 Mbps: −90 dBm · 18 Mbps: −86 dBm · 24 Mbps: −80 dBm · 27 Mbps: −78 dBm | `unex_fcc.txt` |
| Same unit, fading-channel sensitivity (10% PER) | Rural LOS −92.5 dBm, Highway LOS −91.5 dBm, Urban-approach LOS −91.5 dBm, Crossing NLOS −89.5 dBm, Highway NLOS −88.5 dBm | `unex_fcc.txt` |
| Quectel AG15 C-V2X TX power | Class 3, 23 dBm ± 2 dB | `pdftxt/quectel_ag15_hwdesign.txt` |
| Quectel AG15 C-V2X RX sensitivity (B47/B46D, 10 MHz) | Primary/Diversity −93 dBm, SIMO −96 dBm, 3GPP-SIMO spec −90.4 dBm | `pdftxt/quectel_ag15_hwdesign.txt` |
| Kapsch RIS-9260 DSRC sensitivity/power | −92 dBm typ. @ 6 Mbps / 20 dBm max | `pdftxt/kapsch_ris9260.txt` |
| Kapsch RIS-9260/9360 C-V2X sensitivity/power | −95 dBm typ. / 20 dBm (Class 3) | `pdftxt/kapsch_ris9260.txt`, `pdftxt/kapsch_ris9360.txt` |
| Siemens/Yunex RSU DSRC sensitivity | −97 dBm (802.11p) | `pdftxt/siemens_rsu_datasheet.txt`, `pdftxt/yunex_rsu.txt` |
| Ficosa OBU C-V2X sensitivity/power | −97 dBm typ. @ 6 Mbps / 20 dBm max | `pdftxt/ficosa_cv2x_obu.txt` |

---

## H. OBU compute budgets in practice

### H1. The "verify-on-demand" (VOD) rationale

The verify-on-demand strategy — verifying a signature only when a safety application's threat-level assessment
flags a message as needing authentication, rather than verifying every incoming BSM — originates from:

> H. Krishnan and A. Weimerskirch, "'Verify-on-Demand': A Practical and Scalable Approach for Broadcast
> Authentication in Vehicle-to-Vehicle Communication," *SAE International Journal of Passenger Cars — Mechanical
> Systems*, vol. 4, pp. 536–546, June 2011.

(cited within `eprint2022-133.txt`, ref. [7]). The CAMP EE-Requirements document defines "VOD = Verify on Demand"
in its acronym glossary (`camp_ee_req.txt`, line ~46924) but does not restate a specific numeric verify-rate
requirement in the excerpt captured; the *rationale* (vehicles may receive "thousands of messages per second"
in dense scenarios, `eprint2022-133.txt` line 19) is the documented driver, not a single hard number.

### H2. Real hardware verify-rate constraint (cross-reference to E3)

The most concrete OBU-compute-budget numbers found are the Twardokus et al. Cohda-MK6 measurements in E3: a
100-vehicle-density scenario requires ≥1 kHz aggregate verify throughput to avoid queueing BSMs
(`pdftxt/eprint2022_483_pqv2v.txt`, "a viable PQ algorithm must be capable of verifying at least 100 signatures
per 100 ms interval (i.e., a rate of ≥ 1 kHz)"). Measured on real Cohda hardware, ECDSA and Falcon clear this bar
comfortably; Dilithium clears it; SPHINCS+ and XMSS do not (see E3 table).

### H3. Congestion-control context (SAE J2945/1)

A. Rostami, H. Krishnan, M. Gruteser, "V2V Safety Communication Scalability Based on the SAE J2945/1 Standard"
(`pdftxt/rutgers_j2945_scalability.txt` / `pdf_txt/rutgers_j2945_1.txt`, identical text) documents the SAE
J2945/1 congestion-control algorithm (adapting BSM transmit power and inter-transmission time to keep Channel
Busy Percentage bounded) via a calibrated ns-3 simulator, comparing against a 10 Hz-constant-rate, 20 dBm
baseline. This paper is about *channel*-load scalability (how many vehicles can share the RF channel), not a
direct CPU-cycles-vs-neighbor-count measurement; it is included here because it is the standards-context paper
most directly tied to J2945/1, but it should **not** be read as an OBU CPU-load benchmark. No paper in the cache
or located via web search directly measures "OBU CPU load (%) vs. neighbor count" as a plotted curve; this
remains an open gap. **NOT FOUND.**

### H4. Latency budgets from the literature

- "Maximum latency for processing critical messages (e.g., crash-avoidance-related) can be as low as 20 ms."
  (`eprint2022-133.txt`, citing NHTSA VSC-A HS 810 591 Final Report)
- BSM transmission interval: 10 Hz (100 ms) nominal (`eprint2022-133.txt`, multiple locations; also
  `pdftxt/eprint2022_483_pqv2v.txt`)
- Signature-verification time budget derived from the above: to avoid delaying a 100 ms-interval BSM stream from
  up to 100 in-range neighbors, an OBU needs ≥1 kHz aggregate verify throughput (see H2).

---

## Summary comparison table

| Device | Class | CPU / Core | RAM | Flash | HSM / SE | ECDSA-256 Verify | ECDSA-256 Sign | Max TX power | RX sensitivity | Source(s) |
|---|---|---|---|---|---|---|---|---|---|---|
| Cohda MK5 OBU | DSRC OBU | NXP i.MX6 DL, 4000 DMIPS | 1 GB | 4 GB eMMC | NXP SXF1700 | NOT PUBLISHED (device-level) | NOT PUBLISHED | +22 dBm | −99 dBm @ 3 Mbps | A1 |
| Cohda MK6 OBU | DSRC+C-V2X OBU | NXP i.MX8, 8544 DMIPS @800MHz + Qualcomm SA515 | 1 GB | 16 GB eMMC | NXP SXF1800 (FIPS140-2 L3) | see D5 (SAF5400: 2000/s) | NOT PUBLISHED (SXF1800 itself) | DSRC +22 dBm / C-V2X +21.5 dBm | −99 dBm @3Mbps (DSRC) | A2, D4, D5 |
| Unex OBU-301E/351U | DSRC or C-V2X OBU | Autotalks CRATON2, 2×600MHz Cortex-A7 (1140 DMIPS/core) | 128 MB DDR3 | 128 MB NAND | eHSM (Cortex-M0) | **>2500/s** (chip HW engine) | **>110/s, <9ms** | >+20 dBm | <−92 dBm (DSRC); typ<−92dBm(C-V2X) | A4, A10 |
| Danlaw AutoLink | DSRC or C-V2X OBU | Dual-core @800MHz | 1 GB | 8 GB eMMC | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | A7 |
| Ficosa C-V2X OBU | C-V2X OBU | NOT PUBLISHED (named only "Main/HMI/CV2X processors") | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | 20 dBm | −97 dBm typ @6Mbps | A8 |
| Cohda MK5 RSU | DSRC RSU | (module-level only; see MK5 OBU) | NOT PUBLISHED in RSU brief | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED | +22 dBm | −99 dBm @3Mbps | C1 |
| Commsignia ITS-RS4 | DSRC/C-V2X RSU | NXP i.MX6 quad @792MHz | 2 GB DDR3 | 4 GB eMMC | Infineon SLI97 | ">2000" (units/s implied) | <50 ms (corroborated cross-vendor; RS4 PDF text says "usec", flagged UNVERIFIED) | radio-variant dependent | radio-variant dependent | C2 |
| Kapsch RIS-9260 | DSRC+C-V2X RSU | x86 dual-core @1.33GHz | 1 GB ECC | 4 GB | HSM, ECC (FIPS140-2 L3, CC EAL4+) | NOT PUBLISHED | NOT PUBLISHED | DSRC 20dBm / C-V2X 20dBm(Cl.3) | DSRC −92dBm / C-V2X −95dBm | C3 |
| Kapsch RIS-9360 | C-V2X RSU | x86 dual-core @1.33GHz | 1 GB ECC | 4 GB | HSM, ECC | NOT PUBLISHED | NOT PUBLISHED | 20 dBm (Class 3) | typ. −95 dBm | C4 |
| Siemens/Yunex RSU | DSRC (+C-V2X, Yunex variant) | Dual-core @800MHz | 1 GB | NOT PUBLISHED | Unnamed HSM | NOT PUBLISHED | NOT PUBLISHED | NOT PUBLISHED (DSRC) | −97 dBm (802.11p) | C5 |
| Infineon AURIX TC3xx HSM | Automotive MCU HSM | Cortex-M3 @100MHz | 96 KB (HSM RAM) | 128 KB HSM DFlash | (is the HSM) | **100/s @100MHz** | **200/s @100MHz** | n/a | n/a | D1 |
| Infineon OPTIGA Trust M | Discrete SE | (SLS32AIA, core undisclosed) | n/a | n/a | (is the SE) | **~85 ms/op (≈11.8/s)** | ~60-65 ms/op (≈15-17/s) | n/a | n/a | D3 |
| NXP SXF1800 | Discrete V2X SE | Arm SC300 | n/a | 2 MB (1MB free) | (is the SE) | vendor claim >1000/s (system-level) | NOT PUBLISHED (ms) | n/a | n/a | D4 |
| NXP SAF5400 | DSRC baseband+verify engine | Arm subsystem (undisclosed core) | n/a | n/a | integrated ECDSA verify engine | **2000/s** | n/a (signing is on paired SE) | 0 dBm chip-level (PA external) | n/a (see G) | D5 |
| Microchip ATECC608 | Discrete SE (IoT-class) | n/a | n/a | 1,400 B EEPROM | (is the SE) | **NOT PUBLISHED** (timing table omitted from all public datasheet variants found) | NOT PUBLISHED | n/a | n/a | D6 |
| Autotalks CRATON (2013) | DSRC chipset | Cortex-R4F@240-360MHz + 2×ARC625D@240MHz | 96 KB SRAM | n/a | 3× HW ECDSA engines | **<2 ms/verify per engine** (3 parallel) | via ext. SLE97, <50 ms | 4.5–25 dBm | see G table | A5, D7 |
| Raspberry Pi 3B (Cortex-A53) | Generic SW crypto ref. | 4×Cortex-A53@1.2GHz | n/a | n/a | none (software) | 775.3/s | 1,631.1/s | n/a | n/a | D8 |
| Raspberry Pi 3B+ (Cortex-A53) | Generic SW crypto ref. | 4×Cortex-A53@1.4GHz | n/a | n/a | none | 908.4/s | 1,914.7/s | n/a | n/a | D8 |
| Raspberry Pi 4 (Cortex-A72) | Generic SW crypto ref. | 4×Cortex-A72@1.5GHz | n/a | n/a | none | 1,550.7/s | 4,097.4/s | n/a | n/a | D8 |
| x86 backend (OpenSSL Cookbook ref.) | Backend software crypto | unspecified modern x86 | n/a | n/a | none | 6,566.2/s | 20,508.1/s | n/a | n/a | F1 |
| Thales Luna Network HSM A790 | Backend HSM appliance | n/a | up to 64 MB | n/a | (is the HSM) | **22,000 tps** | (same tps class, combined op) | n/a | n/a | F2 |
| Thales Luna T-5000 (gov) | Backend HSM appliance | n/a | n/a | n/a | (is the HSM) | 16,000 tps (P-256) | (same) | n/a | n/a | F2 |
| Cohda MK6 real HW — ECDSA | OBU (measured) | Qualcomm ARMv8 | n/a | n/a | n/a | ~675,000/s (anomalous, see E3 note) | 128/s (7.82 ms) | n/a | n/a | E3 |
| Cohda MK6 real HW — Falcon | OBU (measured) | Qualcomm ARMv8 | n/a | n/a | n/a | 2,243/s | 465/s (2.15 ms) | n/a | n/a | E3 |
| Cohda MK6 real HW — Dilithium | OBU (measured) | Qualcomm ARMv8 | n/a | n/a | n/a | 5,299/s | 380/s (2.63 ms) | n/a | n/a | E3 |
| Cortex-M7 @216MHz — Falcon-512 | PQ MCU reference | Cortex-M7 | 512 KB SRAM | 2 MB flash | n/a | 385/s (2.6 ms) | 45/s (22.1 ms) | n/a | n/a | E1 |
| Cortex-M7 @216MHz — Dilithium-2 | PQ MCU reference | Cortex-M7 | 512 KB SRAM | 2 MB flash | n/a | 152/s (6.6 ms) | 59/s (16.9 ms) | n/a | n/a | E1 |

---

## Notable UNVERIFIED / NOT PUBLISHED items (rollup for quick scanning)

1. Commsignia ITS-RS4 HSM signing latency: PDF text says "<50 usec"; cross-vendor sources for the same silicon
   (Infineon SLI97) say "<50 ms." **UNVERIFIED unit** — see C2.
2. Microchip ATECC608 A/B/C ECDSA sign/verify/GenKey execution-time table: omitted from every public datasheet
   variant checked, including the full ATECC608B-TFLXTLS datasheet. **NOT PUBLISHED** — see D6.
3. NXP SXF1800 signature-generation latency (ms): vendor states only qualitative "low latency," no number.
   **NOT PUBLISHED** — see D4.
4. Quectel AG520R/AG521R full hardware-design datasheet: could not be fetched in this session (404/timeout on
   all mirrors tried). **NOT PUBLISHED here** — see B3.
5. Kapsch OBU: no standalone Kapsch on-board-unit product was found; Kapsch's public V2X hardware line is
   RSU-only as documented. **NOT FOUND** — see C4.
6. ESCRYPT CycurHSM: not researched in this pass (budget prioritization). **NOT PUBLISHED** — see D9.
7. Utimaco HSM ECDSA/RSA throughput: only unconfirmed WebSearch snippets, no primary datasheet reached.
   **UNVERIFIED** — see F3.
8. Announced automotive-grade PQ hardware accelerators (NXP/Infineon): none found in any vendor collateral
   gathered. **NOT PUBLISHED** — see E5.
9. Direct "OBU CPU load (%) vs. neighbor count" measurement paper: not located; nearest available proxies are
   the Twardokus et al. v_max framework (E3/H2) and the J2945/1 channel-congestion paper (H3), neither of which
   is a direct CPU-load-vs-neighbor-count curve. **NOT FOUND** — see H3.
10. Wyoming CV Pilot backhaul technology (fiber vs. cellular split): not found in sources reviewed. **NOT
    PUBLISHED** — see C7.
