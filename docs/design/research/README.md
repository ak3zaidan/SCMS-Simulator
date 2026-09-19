# Research fact sheets backing the design

Compiled 2026-09-17/18 by reading primary documents (standards, papers, datasheets, licence files) and recording every number with its source. Values that could not be confirmed carry an `UNVERIFIED` tag; vendor figures that do not exist carry `NOT PUBLISHED`. The design documents cite these sheets as `[R<n> §<section>]`.

| Sheet | Topic |
|---|---|
| R1-80211p-dcc.md | IEEE 802.11p / ITS-G5 PHY and MAC, channel plans, PER models, ETSI DCC, SAE J2945/1 congestion control, 1609.4 |
| R2-cv2x.md | LTE-V2X Mode 4 and NR-V2X Mode 2 (full sheet) |
| R2b-nrv2x-mode2.md | NR-V2X Mode 2 from TS 38.212/214/215 and the Garcia et al. tutorial |
| R2c-3gpp-channel-models.md | TR 36.885 / TR 37.885 channel and drop models |
| R2d-cv2x-validation-deployment.md | C-V2X PDR/PRR validation targets and regional deployment status |
| R2e-ltev2x-mode4.md | LTE-V2X Mode 4 parameters and the R1-160284 BLER lookup table |
| R3-propagation-gnss.md | propagation, fading, obstacle shadowing, weather, antennas, GNSS and clock error |
| R4-messages-envelopes.md | message generation rules, encoded sizes, 1609.2 / TS 103 097 envelopes, network headers, ASN.1 toolchains and module licences |
| R5-crypto-pq-hsm.md | primitive sizes (FIPS 203/204/205, Falcon), verification costs on x86/ARM/Cortex-M, HSM throughput, libraries, SCMS batch and CRL facts |
| R6-scms-etsi-flows.md | SCMS and ETSI PKI flows, timings, revocation-latency building blocks, privacy metrics, threshold-signature building blocks |
| R7-hardware.md | OBU, RSU, HSM, backend and base-station hardware facts |
| R8-ecosystem-engine.md | simulator licences and integration modes, float determinism, Rust ecosystem, tooling |
| R9-ui-stack.md | Three.js and alternatives, 2D layers, plotting, app frameworks, MCAP, jevpilot, XVIZ |
| R10-maps-mobility-veremi.md | map data licences and formats, mobility model parameters, VeReMi/F2MD/ETSI reporting facts |
| R11-cellular-safety-perception.md | cellular Uu and backhaul, safety applications, perception sensors, jamming, MA literature |
