# Supplying real-world reference counts to the GEH gate

`tools/sumo_realism.py` has **two** reference modes. They answer different questions and are not
interchangeable. This file documents the one that produces a genuine validation result.

| Flag | `comparison_kind` | What the reference is | Vocabulary allowed |
| --- | --- | --- | --- |
| `--ref-counts` | `fhwa_validation` | **Real-world measured** loop counts | FHWA calibration/validation (`geh_criteria.json`) |
| `--ref-det-out` | `seed_stability` / `regression_same_seed` | **Another SUMO run** of the same scenario | Reproducibility only — never FHWA |

> **No real-world measured count data for the Ingolstadt (InTAS) loops is vendored in this
> repository.** Every `--ref-det-out` report is therefore a simulator-against-itself reproducibility
> check, and it says so in its own top-level `warning`. Nothing in the repo currently licenses the
> claim "the traffic model matches Ingolstadt". Obtaining that claim requires the data described
> below.

---

## 1. Why the loop geometry is real but the counts are not

The layout is genuine and reusable:

* `InTAS_E1.add.xml` (identical in `third_party/veremi-nextgen/.../InTAS_urban_2_4_test/sumo/` and in
  every generated scenario) declares **196 `<e1Detector>` elements** at `freq="900.00"`.
* 194 of them carry a `name=` attribute and group into **25 counting stations** —
  `1010 1011 3021 3022 3100 3120 3130 3150 4050 4070 4140 4160 4210 4240 4250 5012 5030 5050 5060
  6022 6030 8001 8002 8005 8604` — which are the real Ingolstadt detector-station ids from the InTAS
  scenario (Lobo et al., *InTAS – The Ingolstadt Traffic Scenario for SUMO*, arXiv:2011.11995).
* The remaining 2 (`income`, `outgoing`) carry **no** `name` and write to `gate.xml`; they are
  scenario-gate counters, not InTAS stations. The tool's `detector_layout.n_stations` counts
  *distinct grouping keys* and therefore reports **27**; the field to read is
  `detector_layout.n_named_stations` (**25**), and only those 25 ids are valid `--ref-counts` keys.

What is missing is the *measured* side:

* All **15** vendored `InTAS_Detectors_Output.xml` copies under `third_party/veremi-nextgen/`
  (`Generator/docker/scenarios/*/sumo/`, `Generator/simulation/mosaic/scenarios/*/sumo/`,
  `Generator/simulation/mosaic/tmp/sumo/`) contain **zero `<interval>` rows** — they are
  config-echo stubs SUMO writes at startup, not recorded data.
* The `routes/InTAS_*.rou.xml` files are **demand**, i.e. model *input*. Grading modelled counts
  against them would be circular and must not be done.
* There is no `--ref-counts` JSON or CSV anywhere in the tree.

## 2. `--ref-counts` file schema

Station keys **must** be the 25 `name=` groups listed above (string keys; `"1010"`, not `1010.0`).
Unmatched keys are reported in `sections.geh.stations_only_in_reference` and excluded from the gates,
so a key typo shows up as a shrinking `n_stations_compared` rather than as a silent pass.

### JSON — flows already in veh/h

```json
{
  "unit": "veh_per_h",
  "stations": { "1010": 132.0, "1011": 84.0, "3021": 12.0 }
}
```

A bare `{ "1010": 132.0, ... }` mapping is also accepted and assumed to be `veh_per_h`.

### JSON — raw counts plus the observation window

```json
{
  "unit": "count",
  "duration_s": 3600,
  "stations": { "1010": 132, "1011": 84, "3021": 12 }
}
```

`unit` starting with `count` converts to veh/h as `value * 3600 / duration_s`.

### CSV

```csv
station,count,duration_s
1010,132,3600
1011,84,3600
```

* Station column: the first of `station` / `id` / `name`, else the first column.
* Value column: `flow` / `veh_per_h` / `veh_h` (already veh/h), **or** `count` / `nVehContrib` /
  `vehicles` (converted using the row's `duration_s`, or `--ref-duration-s`, default 3600 s).
* Repeated station rows (one per lane) are summed, matching how the tool aggregates the E1 loops.

Parsing lives in `load_reference_counts()`, `tools/sumo_realism.py`.

### Invocation

```bash
python tools/sumo_realism.py \
  --det-out  <scenario>/sumo/InTAS_Detectors_Output.xml \
  --det-add  <scenario>/sumo/InTAS_E1.add.xml \
  --ref-counts ref_counts.json \
  --json datasets/realism_baseline/geh_intas_urban_low_validation.json
```

`--ref-counts` and `--ref-det-out` are mutually exclusive; passing both is a usage error (exit 2).

## 3. Two things that will otherwise make the result wrong

**(a) Match the aggregation windows.** GEH is *not* scale-free: `GEH(k·m, k·c) = sqrt(k)·GEH(m, c)`.
The FHWA `GEH < 5` criterion is written for **hourly** volumes. Grading a 300 s simulated window that
has been extrapolated to veh/h against an hourly measured count inflates GEH by
`sqrt(3600/300) = 3.464` and will fail links that are in fact fine. The tool emits a `SHORT WINDOW`
note in `sections.geh.notes` whenever the modelled window is under 1800 s. **Run at least one full
hour of simulated time, over the same clock hour and the same weekday class as the measurement,
before quoting the FHWA gates.**

**(b) Match the observation period.** Loop counts are strongly time-of-day dependent. Record the
measurement date/hour, weekday, and any lane closures alongside the file; a count from an evening
peak cannot validate a scenario generated for `urban_2_4` (02:00–04:00). Pass the run's window
labels through `--seed` / `--ref-seed` and `--begin` / `--end` so the report records what was
compared.

**(c) One run is not a distribution.** `geh_criteria.json` already pins
`fhwa_2019_distribution_criteria` (FHWA-HOP-18-036): the modern guidance replaces single-number
matching with a check against a multi-day field variation envelope, which needs ≥10 seeds per
scenario on the simulated side and multiple measurement days on the real side. A single run against
a single day's counts is the weakest form of the FHWA gate, so report it as such.

Both-zero stations (no vehicle on either side) are excluded from every pass fraction and listed in
`sections.geh.both_zero_stations`, because scoring them as passes inflates the rate.

## 4. Where real counts might come from — NOT vendored, licences unverified

**None of the following are present in this repository and none have been licence-checked. Verify
terms before vendoring anything, and record the licence next to the data file. Do not fabricate,
synthesise, interpolate, or back-fill count values under any circumstances — a synthetic
`--ref-counts` file would reintroduce exactly the circularity this document exists to prevent.**

* **UTD19 (ETH Zurich)** — <https://utd19.ethz.ch/>. Loop-detector flow/occupancy from ~40 cities,
  the largest openly published multi-city urban loop dataset. Ingolstadt is *not* known to be among
  the included cities; treat it as a source for a comparable German mid-size city rather than as a
  drop-in for InTAS, and say so explicitly if you use it that way. Loeder, Ambühl, Menendez &
  Axhausen, *Understanding traffic capacity of urban networks*, Scientific Reports 9, 16283 (2019).
* **Stadt Ingolstadt open data / Bavarian open data** — the municipal portal and
  <https://open.bydata.de/> are the natural home for city-operated detector counts. Availability of
  per-station loop counts keyed to the InTAS station ids has **not** been confirmed; expect to have
  to request them from the city's traffic department and to map their station naming onto the 25
  `name=` groups by hand.
* **Bundesanstalt für Straßenwesen (BASt)** — <https://www.bast.de/> publishes automatic
  counting-station data ("Automatische Zählstellen") for federal roads. This covers Autobahn and
  Bundesstraße cross-sections near Ingolstadt (relevant to the `InTAS_highway_*` scenarios) but not
  the urban loops.
* **The InTAS authors** — <https://github.com/silaslobo/InTAS>. Per this repo's own research notes
  (`docs/realism/investigation/research-traffic-sota.json`, section 6), InTAS built its demand with
  `activitygen` from demographic data plus real traffic information and was **validated against 24
  real measurement points**. Those measured values are the closest thing that exists to a
  station-id-aligned ground truth, and they are *not* part of the released scenario — only the
  calibrated demand that resulted from them is. Requesting them from the authors or from Stadt
  Ingolstadt is the most direct route to a genuine `--ref-counts` file. Note the scenario is
  GPL-3.0 (`THIRD_PARTY_LICENSES.md`), which does not by itself say anything about the licence of
  the underlying municipal measurements.

When a file does arrive, put it under `datasets/` (git-ignored) or vendor it under
`third_party/<source>/` with its licence, and record the provenance in the report's `reference`
block — the tool copies `--ref-counts`' path into `reference.file` and sets
`reference.is_real_world_counts = true`.

## 5. Related files

* `src/scms_sim_ref/datagen/refdata/geh_criteria.json` — the pinned FHWA thresholds and citations,
  applied **only** in `--ref-counts` mode.
* `tools/sumo_realism.py` — `SEED_STABILITY_BASIS` documents the separate, in-tool-derived
  thresholds used for `--ref-det-out`, which deliberately do **not** reuse the FHWA numbers.
* `datasets/realism_baseline/geh_intas_urban_low_300s.json` — a `seed_stability` report, i.e. an
  example of what this document says is **not** a validation result.
