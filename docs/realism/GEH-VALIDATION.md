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
- **182 of 194** named InTAS loops correspond to a real detector. The 12 misses, enumerated in full
  by `tools/e1_match_filter.py` on 2026-08-30, are `3022_7`, `3100_4_1`, `3100_4_2`, `3100_7_1`,
  `3100_7_2`, `4160_2`, `6030_9`, `8002_2`, `8005_4`, `8005_6` at the six `subset` stations, plus one
  each at the two stations with no data at all (`3120`, `3130`). Most are SUMO lane-splits of a
  single real loop.
- Real data retrieved: station 1010, Tue 2023-11-07 07:00–08:00Z → bins 843/788/794/747 = **3172 veh/h**.
- Cross-check against the paper: station 5060 on 2023-11-14, summing **only the InTAS-matched
  detectors**, gives 16,296 veh/day against the paper's ~18,414 for Nov 2019 — within 11.5%, which
  is plausible drift over four years and confirms both the mapping and the method.

**23 of 25 stations are usable.** `3120` and `3130` have registered detectors but zero observations
in the whole archive. Best single day: **Tue 2023-11-14**, 23/25 stations at 95/96 bins — and
November is the paper's own validation month.

## Four ways to get this wrong

1. **Do not use `whole_intersection` blindly.** Only 10 stations (`1010, 1011, 3150, 4050, 4140,
   4240, 5012, 5030, 5050, 8604`) have detector sets identical to InTAS's. For the other 15, sum only
   the matched detectors — using the whole intersection inflates counts badly. Re-measured live on
   2023-11-14 07:00–08:00Z with `tools/fetch_ingolstadt_counts.py`, the worst inflation is **+83.2%**
   at `5060` (970 matched vs 1777 whole-intersection) and **+76.5%** at `6030` — i.e. the earlier
   "up to 46%" figure *understates* the error; +48.7% (`8001`) and +51.0% (`4250`) are mid-table.
   The same measurement independently confirms the 10-station list: those 10 are exactly the
   stations where whole-intersection minus matched-sum is **0.00%**. Re-measured again on the AM
   peak hour (`06:00–07:00Z`, i.e. local 07:00–08:00) the worst inflation is larger still:
   **+92.9%** at `5060`, **+75.9%** at `6030`, **+55.0%** at `4250`, **+52.1%** at `4070`. There are
   also **no `det_id` collisions** in either hour, so the matched sums double-count nothing.
2. **Match the window *length*.** `intas_urban_low` runs 300 s. Since GEH is not scale-free,
   extrapolating that to veh/h inflates every value by √12 = 3.4641. Run a full clock hour against
   the measured hour, matching weekday class. Upstream InTAS is a 24 h configuration, so the hour is
   selected with `--begin`/`--end` on the SUMO command line, not by editing the scenario.
3. **Match the window *clock* — the API is UTC, the scenario is local.** This one is silent and it
   bit the example that used to stand here. Observation `phenomenonTime` is genuine **UTC**; the
   SUMO clock of InTAS is **local Ingolstadt time**. In November that is CET = UTC+1, so a window
   fetched as `07:00–08:00Z` is the local **08:00–09:00** hour and pairs with `begin=28800
   end=32400`, *not* `begin=25200`. Getting this wrong compares the model's peak hour against
   reality's post-peak hour — a ~14 % error at the network total that no gate would attribute to the
   clock. Three independent confirmations, all measured 2026-08-30:
   * an observation's `parameters.countPeriodEndTime` is local: the bin whose `phenomenonTime` is
     `2023-11-14T07:00:00Z` carries `countPeriodEndTime = 2023-11-14T08:15:00.000+01:00`, i.e. the
     bin spans local 08:00–08:15;
   * the measured day profile at station `1010` on 2023-11-14 peaks at **06:00–07:00Z** (3824 veh)
     with a second peak at **15:00–16:00Z** (4116 veh) — the canonical German urban 07–08 / 16–17
     *local* pattern;
   * the InTAS demand's own departure histogram peaks at **25200–28800 s** (14,942 of exactly
     185,923 departures, its daily maximum) with a secondary peak at 57600–61200 s — the same
     07–08 / 16–17 shape on the scenario clock.

   So the AM peak comparison is measured `06:00–07:00Z` ↔ SUMO `25200–28800`.
4. **State the vintage gap.** InTAS demand is calibrated to **Nov 2019**; the open counts begin
   **May 2023**. That ~3.5-year offset spans COVID. This is validation against *present-day* reality,
   not against the scenario's own calibration epoch, and must be reported as such. The 5060
   cross-check quantifies the drift at one station (−11.5%). No InTAS-era (2019) counts exist
   anywhere in this tree, so the gap cannot be closed locally — only reported.

The result of doing all four correctly is [`GEH-RESULT.md`](GEH-RESULT.md).

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

## The fetcher: `tools/fetch_ingolstadt_counts.py`

That "right shape" now exists. The tool takes a window and `InTAS_E1.add.xml` and writes a
`--ref-counts` JSON (and optionally CSV) in the schema of
`src/scms_sim_ref/datagen/refdata/geh_reference_counts.README.md`.

```bash
# 06:00-07:00Z == local 07:00-08:00 CET == SUMO 25200-28800. See pitfall 3 above.
python tools/fetch_ingolstadt_counts.py \
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml \
  --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z \
  --include exact \
  --out .cache/savenow/ref_counts_20231114_0600Z_exact.json
```

(The layout file is byte-identical — SHA-256 `add79329…b56860` — to the
`third_party/veremi-nextgen/…/InTAS_urban_2_4_test/sumo/InTAS_E1.add.xml` the mapping was first
verified against, so either path gives the same result.)

* **Never redistributes.** Cache and default output live under `/.cache/` (git-ignored); the
  licence caveat is printed on every run and embedded in the output as `provenance.licence_caveat`
  and a top-level `WARNING`.
* **Enforces the subset rule.** It only ever sums the InTAS-matched `single_detector` streams. The
  `whole_intersection` stream is recorded as `whole_intersection_count` /
  `whole_intersection_inflation_pct` — a diagnostic, never the count.
* **Per-station `comparability`**, on top of `matched_detectors` / `intas_detectors` /
  `api_detectors`:
  * `exact` — every InTAS loop at the station has a measured counterpart and every 15-min bin is
    present. Directly comparable to a simulated station total.
  * `subset` — some InTAS loop has no real `det_id`, or a bin is missing. The count is a **lower
    bound** on the InTAS station.
  * `unusable` — no matching detector, or no observation in the window. **Never emitted**, because a
    zero that really means "no data" silently corrupts a GEH.
  * `detector_sets_identical` is the separate, stricter flag (SAVeNoW's detector set == InTAS's),
    i.e. the 10 stations where `whole_intersection` would also have been safe.
* **Bin semantics.** Observation `phenomenonTime` is the **start** of a 15-min bin, **in UTC** (its
  `parameters.countPeriodEndTime` is that plus 15 min, expressed in local time with an explicit
  `+01:00`/`+02:00` offset), so the window filter is `phenomenonTime ge begin and lt end` and the
  window must be 15-min aligned; the tool warns if not. The tool does **not** convert to the
  scenario clock — pitfall 3 above is the caller's job.
* **Paging and failures.** Both `@iot.nextLink` and the nested `Observations@iot.nextLink` are
  followed. Any failed query is recorded in `provenance.fetch_errors`, sets
  `provenance.complete = false`, forces every affected station to `unusable`/`fetch_failed`, and
  exits **3** unless `--allow-partial`. A partial fetch can never masquerade as a complete one.
* **Polite.** One query per batch of 5 controllers (catalogue and observations come back in the same
  response) — 25 stations cost **5 HTTP calls, ~133 KiB**. Every response is cached by URL hash, so
  re-runs are free; `--offline` forbids the network entirely.

Measured 2023-11-14 07:00–08:00Z (local 08:00–09:00), 23 usable stations, **40,008 veh** total;
`1010` = **3269**, which reproduces the independent hand-measurement exactly. Measured
06:00–07:00Z (local 07:00–08:00, the AM peak), 23 usable stations, **45,713 veh**; `1010` = **3824**,
which reproduces the independent full-day profile probe exactly. Station-count table and archive
extents live in the generated file's `station_details`.

### Two traps in the archive itself (measured 2026-08-30)

The catalogue advertises an extent of `2023-05-17 .. 2025-10-28`. An *extent* is a first/last pair,
not a promise of continuity or of one row per bin. Both assumptions fail.

**1. Coverage is not continuous.** Observation counts per month on datastream `35`
(`1010_4(DAL1)`, 96 bins/day expected):

| month | rows | of expected | | month | rows | of expected |
|---|---:|---:|---|---|---:|---:|
| 2023-05 | 1,402 | 47.1 % (starts mid-month) | | 2024-11 | 1,412 | **49.0 %** |
| 2023-06 … 2023-11 | ~2,800–2,970 | 96.7–99.8 % | | 2024-12 | 2,657 | 89.3 % |
| 2024-01 | 3,389 | 113.9 % (duplicates) | | 2025-06 | 2,397 | 83.2 % |
| 2024-05 | 3,528 | 118.5 % (duplicates) | | 2025-07 | 1,935 | 65.0 % |
| 2024-07 | 3,420 | 114.9 % (duplicates) | | 2025-09 | 90,700 | **3149 %** (duplicates) |

**Tuesday 2024-11-12 returns zero observations at all 25 controllers.** The fetcher marks every
station `unusable`/`no_observations_in_window` and emits nothing — which is exactly the behaviour
that stops a 23-station wall of zeros from becoming a spectacular fake result. Always read the
`emitted N station(s)` line.

**2. The archive contains duplicated bins, and they are not rare.** On the same datastream, January
2024 holds **3,389 rows over only 2,798 distinct timestamps**; the bin `2024-01-27T05:15:00Z` is
repeated **100 times**. All copies of a bin carry the *same* value, so the duplication is an
ingestion artefact rather than conflicting measurements — but summing raw rows multiplies the count
by the multiplicity.

Worked example, the hour `2024-01-27T06:00:00Z`: **30,292 duplicate rows against 8,791 real
vehicles**, every detector carrying 195 spare rows for its 4 real bins. Summed naively, station
`1010` reads roughly **47,600 veh/h instead of 956** — a ~50× inflation, at a station that every
coverage check still calls healthy, because the old code only ever tested for *too few* bins.

`tools/fetch_ingolstadt_counts.py` now collapses repeated `phenomenonTime` values
(`dedupe_observations`, first row in ascending order wins), records
`duplicate_observations_discarded` per detector and per station, and — if copies of a bin ever
*disagree* — records the conflict and forces the station down to `subset`, because a conflicted bin
may not sit inside an `exact` headline. **This changes nothing on 2023-11-14**: both graded hours
re-fetch byte-identically (34,948 / 45,713 / 30,576 / 40,008 veh) with **zero** duplicates
discarded, which is the positive control for the fix.

### Four companion tools

* `tools/e1_match_filter.py` — writes a mapping-only copy of `InTAS_E1.add.xml` restricted to the
  loops a given reference file actually measured. Needed because `sumo_realism.py` sums *every*
  InTAS loop in a `@name` group: at a `subset` station that compares 9 modelled loops against a
  5-loop reference and biases the model high. Filtering makes both sides sum the same physical
  loops. It is a no-op on `exact` stations, so the headline is unaffected.
* `tools/e1_interval_profile.py` — per-interval network loop total from an E1 output. This is the
  evidence that a warm-up was long enough: the graded hour has to sit on the plateau, not on the
  fill-up transient of a network that started empty.
* `tools/geh_diagnose.py` — splits a GEH failure into a **level** component (demand total wrong,
  assignment right) and an **assignment** component, via the spread of the per-station
  modelled/measured ratio. Its rescaled figures are an explicitly-labelled counterfactual and must
  never be quoted as a result.
* `tools/geh_result_table.py` renders a validation report as the Markdown tables used in
  `GEH-RESULT.md`.

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
