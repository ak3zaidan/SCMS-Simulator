# Real-world GEH validation of `gen_intas_urban_low` — result

<!-- VERDICT -->
*(verdict pending — results are being written into this document; see the run log in the task that
produced it)*
<!-- /VERDICT -->

This is the first comparison in this repository between simulated flows and **measured real-world
counts**. Everything before it was simulation-against-simulation
([`SEED-STABILITY-CALIBRATION.md`](SEED-STABILITY-CALIBRATION.md)), which can show reproducibility
but can never show realism. The unblocking work — the data source, the identifier mapping and the
four ways to get the comparison wrong — is in [`GEH-VALIDATION.md`](GEH-VALIDATION.md).

## What was compared

| | |
|---|---|
| Model | `scms-sim/scenarios/gen_intas_urban_low` — unmodified InTAS demand, **185,923** vehicle departures/day, `ingolstadt.net.xml` sha256 `9f16fd82…c217be` |
| Simulator | Eclipse SUMO **1.25.0**, `step-length` 0.1 s, EIDM car-following, sublane model (`lateral-resolution` 0.8), `device.rerouting.probability` 0.82 |
| Detectors | the scenario's own `InTAS_E1.add.xml` — 196 `e1Detector` entries = 194 loops in **25** named station groups + 2 unnamed gate counters, `freq="900.00"` |
| Measured counts | Stadt Ingolstadt signal-loop counts, published via SAVeNoW/TU München FROST SensorThings, 15-min bins |
| Reference day | **Tuesday 2023-11-14** — a working Tuesday in November, the InTAS paper's own validation month |
| Graded windows | **A**: SUMO 25200–28800 s = local **07:00–08:00** ↔ measured `06:00–07:00Z` (the AM peak on both sides)<br>**B**: SUMO 28800–32400 s = local **08:00–09:00** ↔ measured `07:00–08:00Z` |
| Warm-up | SUMO ran from **21600 s (06:00 local)**, so window A is preceded by a full hour of fill-up from an empty network and is not graded |
| Seeds | **42** (the scenario's own `randomSeed`) and **23423** — two independent runs of the identical scenario. A third (987654) was started and terminated at 27.7 % because three concurrent SUMO processes contend badly: killing one raised the survivors' real-time factor from ~0.7 to **1.43–1.44**, measured twice over 120 s and 180 s windows. |
| Criteria | FHWA Traffic Analysis Toolbox Vol III (FHWA-HRT-04-040, 2004) §5, via `src/scms_sim_ref/datagen/refdata/geh_criteria.json` |

`GEH = sqrt(2(m−c)²/(m+c))`, with `m` and `c` both **veh/h over the same clock hour**. Because the
graded window is exactly 3600 s and the reference bins cover exactly 3600 s, the counts and the
veh/h flows are numerically the same on both sides: no extrapolation, no √k inflation.

## The four things that had to be right

Each of these silently produces a wrong number rather than an error.

1. **Window length.** `intas_urban_low` normally runs 300 s. GEH is not scale-free —
   `GEH(k·m, k·c) = √k · GEH(m, c)` — so grading a 300 s window extrapolated to veh/h inflates every
   value by √12 = 3.4641 against an hourly criterion. A full clock hour was simulated instead.
   `sumo_realism.py` emits a `SHORT WINDOW` note below 1800 s; the runs here do not trigger it.
2. **Window clock.** The API's `phenomenonTime` is **UTC**; the InTAS SUMO clock is **local**
   Ingolstadt time (CET = UTC+1 in November). Three independent confirmations are recorded in
   `GEH-VALIDATION.md` pitfall 3: the local-stamped `countPeriodEndTime` on each observation, the
   measured day profile at station `1010` (peaks at 06Z and 15Z), and the InTAS demand's own
   departure histogram (peaks at 25200–28800 s and 57600–61200 s). Both sides peak at local 07–08
   and 16–17, which is the canonical German urban profile.
3. **Detector subset.** At 15 of the 25 stations SAVeNoW's detector set is larger than InTAS's, so
   the `whole_intersection` aggregate is not the InTAS station: on the AM peak hour it overstates by
   up to **+92.9 %** (`5060`). Only InTAS-matched `single_detector` streams are summed. Stations are
   graded by `comparability`:
   * **`exact`** — every InTAS loop at the station has a measured counterpart and every 15-min bin is
     present. **17 stations. These alone carry the headline.**
   * **`subset`** — some InTAS loop has no real counterpart (10 loops over 6 stations, mostly SUMO
     lane-splits). The measured value is a **lower bound**, so these are reported separately and
     additionally re-graded with `tools/e1_match_filter.py`, which restricts the *modelled* side to
     the same loops.
   * **`unusable`** — `3120` and `3130` have registered detectors but not one observation in the
     entire archive. Never emitted: a zero that means "no data" would silently corrupt the GEH.
4. **Licence.** The dataset carries **no formally stated licence** (TUM catalogue: "License Not
   Specified"). Nothing measured is committed: counts are fetched on demand into the gitignored
   `/.cache/`, and every tool prints the caveat on every run. See [Provenance and licence](#provenance-and-licence).

### Reference-data integrity on the graded day

Three archive defects were found while choosing the reference day, and all three were checked
against 2023-11-14 before it was used (details and worked examples in
[`GEH-VALIDATION.md`](GEH-VALIDATION.md), "Two traps in the archive itself"):

| defect | how it would corrupt the result | status on 2023-11-14 |
|---|---|---|
| **Coverage gaps** — Tue 2024-11-12 returns *zero* observations at all 25 controllers | 23 stations of zeros read as "the model over-predicts by 100 %" | clean: 23 stations usable, every graded station at full bin coverage |
| **Duplicated bins** — one bin repeated up to **100×**; the hour 2024-01-27T06:00Z carries 30,292 duplicate rows against 8,791 real vehicles (~50× inflation) | station counts multiplied by the duplication factor while every coverage check still reads healthy | clean: **0 duplicate observations discarded** at any graded station |
| **Dead controllers** — station `5012` reports 0 vehicles across all 32 bins of the 2025-10-21 peak hour while still classifying as `exact` | a zero meaning "no data" enters the GEH as a measurement | clean: no graded station reports zero; minimum is `5060` at 801 veh/h |

The first defect was already handled by the fetcher's `unusable` class; the second and third were
found in the course of this work and fixed in `tools/fetch_ingolstadt_counts.py`
(`dedupe_observations`, and the `all_matched_detectors_report_zero` refusal). **Both fixes are
no-ops on 2023-11-14** — all four reference files re-fetch byte-identically afterwards — which is
the positive control showing the headline was never contaminated.

<!-- RESULTS -->
*(results pending)*
<!-- /RESULTS -->

## The vintage gap — read this before quoting any number above

**InTAS demand is calibrated to November 2019. These counts are from November 2023.** The offset is
about four years and it spans COVID-19. Nothing in this comparison validates the scenario against
the data it was built from; it validates the scenario against **present-day reality**.

* The SAVeNoW archive begins **2023-05-17**. There is no way to obtain 2019 counts from it.
* A search of this tree confirms **no InTAS-era measured counts exist anywhere on disk** — the InTAS
  authors published network and calibrated demand but withheld the measurements
  ([`GEH-VALIDATION.md`](GEH-VALIDATION.md), "Ruled out"). The gap therefore cannot be closed here,
  only reported.
* The one available quantification of the drift is the station `5060` day-total cross-check in
  `GEH-VALIDATION.md`: 16,296 veh/day measured in Nov 2023 against the InTAS paper's ~18,414 for
  Nov 2019, i.e. **−11.5 %** at that station over four years.

### Measured demand is not static — it moves within the archive too

Re-fetching the **same local hour on the same weekday class** two years later, over the 15 stations
that are `exact` on both days:

| | 2023-11-14 (Tue) | 2025-10-21 (Tue) | change |
|---|---:|---:|---:|
| 15 common `exact` stations | **31,527** | **28,578** | **−9.35 %** |
| `1010` A01 Südl. Ringstr. / Münchener Str. | 3,824 | 3,087 | −19.3 % |
| `4050` D05 Theodor-Heuss-Str. / Hindenburgstr. | 2,965 | 2,176 | −26.6 % |
| `4250` D25 Hans-Stuck-Str. / Furtwänglerstr. | 1,393 | 914 | −34.4 % |
| `5060` E06 Westl. Ringstr. / Probierlweg | 801 | 1,069 | **+33.5 %** |
| `4140` D14 Richard-Wagner-Str. / Permoserstr. | 2,174 | 2,367 | +8.9 % |
| `5030` E03 Westl. Ringstr. / Neuburgerstr. | 2,497 | 2,500 | +0.1 % |

Two years of *observed* change already move the network total by ~9 % and individual stations by
−34 % to +34 %. Whatever the 2019→2023 gap is, it is not plausibly smaller than this. (Caveat: the
2025 sample is October, not November, so part of the difference is seasonal — `2025-11` lies outside
the archive. `5012` is excluded because it reports a dead-controller zero on the 2025 day; see the
archive-quality notes in [`GEH-VALIDATION.md`](GEH-VALIDATION.md).)

Consequently a GEH failure here has (at least) three candidate causes that this experiment does not
separate: real demand drift since 2019, error in the 2019 calibration itself, and error in the
simulation. A GEH *pass* would have been the stronger and more surprising claim, since it would
require the 2019 demand to still describe 2023 traffic.

**No demand, route file, scaling factor or detector selection was adjusted to improve any number in
this document.** The scenario is byte-identical to the one already in the tree.

## What this does and does not license anyone to claim

* It **does** establish that the real-world validation path exists end to end: measured counts →
  identifier mapping → matched detector subset → window-matched SUMO run → FHWA gates.
* It **does not** license quoting the ROADMAP Phase-1 gate ("GEH < 5 on ≥ 85 % of InTAS
  induction-loop stations") as met unless the gate rows below actually say `pass`.
* The seed-stability calibration in
  [`SEED-STABILITY-CALIBRATION.md`](SEED-STABILITY-CALIBRATION.md) is **not** involved here. It is
  scenario- and window-scoped (`intas_urban_low` @ 300 s) and would in any case be refused for a
  3600 s window; validation mode against measured counts uses the FHWA criteria only.

## Exact reproduction

Every path below is relative to the repository root. Activate the toolchain first
(`. C:/Users/Administrator/tools/env.ps1`). Nothing here writes into the scenario directory.

```powershell
# 1. Measured counts -> a --ref-counts file in the gitignored cache. NEVER commit the output.
#    06:00-07:00Z is local 07:00-08:00 (see pitfall 2). --include exact keeps only the 17
#    stations whose InTAS loop set is fully measured.
python tools/fetch_ingolstadt_counts.py `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z `
  --include exact --out .cache/savenow/ref_counts_20231114_0600Z_exact.json

# window B reference, and the 23-station exact+subset variants
python tools/fetch_ingolstadt_counts.py `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --begin 2023-11-14T07:00:00Z --end 2023-11-14T08:00:00Z `
  --include exact --out .cache/savenow/ref_counts_20231114_0700Z_exact.json
python tools/fetch_ingolstadt_counts.py `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --begin 2023-11-14T06:00:00Z --end 2023-11-14T07:00:00Z `
  --include exact+subset --out .cache/savenow/ref_counts_20231114_0600Z_all.json

# 2. SUMO: one hour of warm-up (21600) then both graded hours (25200-28800, 28800-32400).
#    --output-prefix is resolved relative to the CONFIG directory and keeps every output in the
#    gitignored cache, so the scenario directory is never written to.
#    TRAP: the output path must not contain a double dash -- SUMO embeds its command line in an
#    XML comment, where "--" is illegal.
Set-Location scms-sim/scenarios/gen_intas_urban_low/sumo
foreach ($s in 42, 23423, 987654) {
  sumo -c InTAS_buildings.sumocfg --seed $s `
    --output-prefix "../../../../.cache/geh_hour/run/s${s}_" `
    --begin 21600 --end 32400 --no-step-log --verbose false
}
Set-Location ../../../..

# 3. Grade window A (local 07:00-08:00) against the measured AM peak hour, exact stations only.
python tools/sumo_realism.py `
  --det-out .cache/geh_hour/run/s42_InTAS_Detectors_Output.xml `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --ref-counts .cache/savenow/ref_counts_20231114_0600Z_exact.json `
  --begin 25200 --end 28800 --seed 42 `
  --json .cache/geh_hour/geh_A_s42_exact.json

# window B: --begin 28800 --end 32400 with ref_counts_20231114_0700Z_exact.json

# 4. Subset stations, with the MODELLED side restricted to the same physical loops.
python tools/e1_match_filter.py `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml `
  --ref-counts .cache/savenow/ref_counts_20231114_0600Z_all.json `
  --out .cache/geh_hour/e1_matched_0600Z_all.add.xml
python tools/sumo_realism.py `
  --det-out .cache/geh_hour/run/s42_InTAS_Detectors_Output.xml `
  --det-add .cache/geh_hour/e1_matched_0600Z_all.add.xml `
  --ref-counts .cache/savenow/ref_counts_20231114_0600Z_all.json `
  --begin 25200 --end 28800 --seed 42 `
  --json .cache/geh_hour/geh_A_s42_all_aligned.json

# 5. Presentation and attribution.
python tools/geh_result_table.py --report .cache/geh_hour/geh_A_s42_exact.json `
  --ref-counts .cache/savenow/ref_counts_20231114_0600Z_exact.json
python tools/geh_diagnose.py --report .cache/geh_hour/geh_A_s42_exact.json
python tools/e1_interval_profile.py `
  --det-out .cache/geh_hour/run/s42_InTAS_Detectors_Output.xml `
  --det-add scms-sim/scenarios/gen_intas_urban_low/sumo/InTAS_E1.add.xml
```

`tools/sumo_realism.py` exits **1** when any gate fails (pass `--no-fail` to measure without
failing). Re-running step 1 after the cache is warm needs no network at all: add `--offline` and the
byte-identical file is rebuilt from the cached responses (verified).

## Provenance and licence

| | |
|---|---|
| Endpoint | `https://savenow.gis.lrg.tum.de/frost/v1.1/` (OGC SensorThings / FROST), no key, no registration |
| Attribution | Stadt Ingolstadt, Amt für Verkehrsmanagement und Geoinformation; published via the SAVeNoW project, hosted by TU München |
| Catalogue | `https://catalog.savenow.gis.lrg.tum.de/en/dataset/verkehrsdaten-von-ingolstadt` |
| Licence | **NOT FORMALLY STATED** — the TUM catalogue reads "License Not Specified" |

**No measured count from this source is committed to this repository, and none may be.** Every
fetched file lives under `/.cache/` (git-ignored at `.gitignore:41`); the fetch tool embeds the
caveat in each output as `provenance.licence_caveat` and a top-level `WARNING`, and prints it on
every run; `tools/sumo_realism.py` prints the same notice in its provenance block. A written licence
statement should be obtained from `b.willenborg@tum.de` (TUM Geoinformatics, catalogue maintainer),
`info@savenow.de`, or the Amt für Verkehrsmanagement und Geoinformation before any vendoring.

### One open decision for the maintainer

The per-station tables above quote **34 measured hourly values** (17 stations × 2 hours). They are
here because a validation result that hides the reference side is not a result: GEH is a function of
the modelled and the counted flow, so publishing the modelled count together with the GEH already
determines the counted one. Concealment would be cosmetic, not protective.

That is nevertheless a judgement call, and it is the maintainer's to ratify, not this document's:

* what is **not** in the repository is the dataset — every fetched file, every raw API response and
  every per-detector series stays under the git-ignored `/.cache/`, and no tool writes measured data
  anywhere else;
* what **is** in the repository is a 34-value aggregate extract quoted as the evidence for a stated
  result, with full attribution.

If the licence question is resolved as "no redistribution", delete the two per-station tables and
keep the gates, the aggregates and the reproduction commands — one command regenerates the tables
locally. Until a written statement is obtained from the contacts above, treat this file as internal.
