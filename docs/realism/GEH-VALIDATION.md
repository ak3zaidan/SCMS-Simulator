# Unblocking real-world GEH validation

The only *blocked* gate in the roadmap was GEH against measured counts: the loop geometry in
`InTAS_E1.add.xml` is genuine, but no measured counts existed anywhere on disk, so every comparison
was simulation-against-simulation. **Real counts exist and were verified live against the API.**

The repo's earlier assumption — that counts would have to be requested from the city traffic
department — is out of date. The City of Ingolstadt has published its signal-loop counts as open
data since 2023 through the SAVeNoW project, hosted by TU München.

## The source

| | |
|---|---|
| Endpoint | `https://savenow.gis.lrg.tum.de/frost/v1.1/` (FROST SensorThings API) |
| Docs / map | `https://savenow.github.io/sta-docs/` |
| Catalogue | `https://catalog.savenow.gis.lrg.tum.de/en/dataset/verkehrsdaten-von-ingolstadt` |
| Owner | Stadt Ingolstadt, Amt für Verkehrsmanagement und Geoinformation |
| Resolution | **15 min** — exactly matches InTAS `freq="900.00"` |
| Coverage | 2023-05-17 → 2025-10-28; 88 intersections, 681 detectors |
| Access | HTTP 200, **no registration, no API key**; native CSV via `$resultFormat=csv` |
| Licence | **Not formally stated** — see the blocker below |

The owning office is the *same* department that supplied InTAS's original validation data, which is
why the identifiers line up.

## The mapping is exact

The `name=` groups in `InTAS_E1.add.xml` are Ingolstadt `lsa_id` values (Lichtsignalanlage /
signal-controller numbers). InTAS detector IDs are the real `det_id` with the hardware label
stripped: `1010_1` ↔ `1010_1(DA1)`, `5060_7` ↔ `5060_7(DE1)`.

Verified live:

- **All 25 station IDs matched 1:1** as `properties/lsa_id`, with real street names — e.g.
  `5060 (E06 Westliche Ringstrasse / Probierlweg)` and `4070 (D07 Nördliche Ringstr. /
  Eckstallerstr.)`, which are precisely the best- and worst-case validation points named in the
  InTAS paper.
- **182 of 194** named InTAS loops correspond to a real detector. The 12 misses are SUMO lane-splits
  (`3100_4_1` / `3100_4_2`).
- Real data retrieved: station 1010, Tue 2023-11-07 07:00–08:00Z → bins 843/788/794/747 = **3172 veh/h**.
- Cross-check against the paper: station 5060 on 2023-11-14, summing **only the InTAS-matched
  detectors**, gives 16,296 veh/day against the paper's ~18,414 for Nov 2019 — within 11.5%, which
  is plausible drift over four years and confirms both the mapping and the method.

**23 of 25 stations are usable.** `3120` and `3130` have registered detectors but zero observations
in the whole archive. Best single day: **Tue 2023-11-14**, 23/25 stations at 95/96 bins — and
November is the paper's own validation month.

## Three ways to get this wrong

1. **Do not use `whole_intersection` blindly.** Only 10 stations (`1010, 1011, 3150, 4050, 4140,
   4240, 5012, 5030, 5050, 8604`) have detector sets identical to InTAS's. For the other 15, sum only
   the matched detectors — using the whole intersection inflates counts by up to **46%**.
2. **Match the window.** `intas_urban_low` runs 300 s. Since GEH is not scale-free, extrapolating
   that to veh/h inflates every value by √12. Run a full clock hour (e.g. `begin=25200 end=28800`)
   against the measured hour, matching weekday class. Upstream InTAS is a 24 h configuration.
3. **State the vintage gap.** InTAS demand is calibrated to **Nov 2019**; the open counts begin
   **May 2023**. That ~3.5-year offset spans COVID. This is validation against *present-day* reality,
   not against the scenario's own calibration epoch, and must be reported as such. The 5060
   cross-check quantifies the drift at one station (−11.5%).

## The blocker: licence

The data is served openly and described in city/THI/SAVeNoW material as "Open Data (#odin)", and the
SAVeNoW *site content* is CC BY 4.0 — but **no machine-readable or explicit licence is attached to
the data itself**, and the TUM catalogue reads "License Not Specified". It could not be found on
`open.bydata.de`.

**Therefore: do not vendor these counts into the repository.** The right shape is a fetch tool that
pulls them on demand into a gitignored cache, so the capability ships without redistributing
unlicensed data. A written licence statement should be requested before any vendoring
(`b.willenborg@tum.de` — TUM Geoinformatics, catalogue maintainer; `info@savenow.de`; and the Amt
für Verkehrsmanagement und Geoinformation).

## The licence-clean companion: BASt

For an unimpeachable demonstration of the GEH machinery, BASt motorway counts are **CC BY 4.0**,
explicitly stated, no registration, attribute "Bundesanstalt für Straßen- und Verkehrswesen".

- Per-station hourly: `https://www.bast.de/videos/{YEAR}/zst{ZSTNR}.zip` → one CSV, 8,760 hourly
  rows, directional, 9 vehicle classes with per-value quality flags, 2003–2024.
- Near Ingolstadt: **Zst 9552 "Ingolstadt-Nord (S)"** (A9, 2.5 km, DTV 100,149), **Zst 9554
  "Manching (N)"** (A9, 5.6 km), **Zst 9282 "Neustadt a.d. Donau"** (B16, 24.7 km). No BASt station
  sits on the B16 inside Ingolstadt.
- These tie naturally to InTAS stations `3150 (C15 Römerstrasse / BAB-AS-Nord)` and
  `6030 (F03 Manchinger Strasse)`, so the motorway portion can be validated licence-cleanly.
- Note the BASt URLs currently in our README are dead (404) after a site relaunch; the working index
  is `https://www.bast.de/DE/Themen/Digitales/HF_1/Massnahmen/verkehrszaehlung/Stundenwerte.html`.

## Ruled out

- **UTD19** (ETH Zurich) — definitively **does not include Ingolstadt** (40 cities enumerated;
  Augsburg and Munich are present, Ingolstadt is not). Registration-gated, academic use only. Drop
  this line; the README should stop citing it as the candidate.
- **LuST, MoST, TAPASCologne, Bologna, TuST, HaTS, TUM-VT `sumo_ingolstadt`** — in every case the
  network and calibrated demand are open and the underlying measurements are withheld. TAPASCologne
  is additionally CC BY-**NC**-SA.
- **Wildau** (`DLR-TS/sumo-scenarios`, EPL-2.0) is the only scenario shipping network *and* measured
  counts together, but with ~18 points over a single ~2 h aggregate window and no provenance —
  methodology demo only.
- **BeST (Berlin)** is the strongest city-scale alternative (network CC BY 4.0, counts dl-de/by-2-0,
  hourly 2015–2025, unauthenticated) but is a different scenario entirely.
