# `v2xw` — the command line

```
v2xw run <scenario>            run a scenario; write its recording, metrics and manifest
v2xw validate <scenario>       load and check a scenario without building its world
v2xw import-osm <extract> <out> --speed-preset <name>
                               import an OpenStreetMap extract into the three world formats
v2xw info <recording>          print a recording's manifest, channels and verification report
v2xw experiment run <scenario>      expand the scenario's sweep, run it, aggregate it
v2xw experiment status <scenario>   say how far along a sweep is; run nothing
v2xw experiment resume <scenario>   continue a sweep that was started here
```

No model logic lives here (02-architecture.md §2, ADR 0010): every command is a call into a
library crate. The one thing the tool adds is **the clock**. `Engine::build` takes its
manifest timestamp as an argument and `ImportOptions` takes its import date as one,
precisely so that no engine code reads one; `src/wall.rs` is the only file in this crate
that may read a clock, and it says so at the top.

## `run`

```
v2xw run scenarios/phase1-grid.yaml --out runs/phase1 --build-utc 2026-09-22T00:00:00Z
```

writes five files:

| File | What it is | Reproducible between two runs of one scenario? |
|---|---|---|
| `recording.mcap` | the container: every record, the manifest metadata, the resolved scenario as an attachment | its **data section** is, byte for byte |
| `scenario.resolved.yaml` | the document the run actually executed, after base merge, migration and defaults | yes |
| `metrics.json` | every `metric.sample` record the run emitted | yes |
| `run-report.json` | event, frame and record counts, per channel, plus the recording's content digest | yes |
| `manifest.json` | the run manifest with those files digested and `data_digest` finalised | all but `build_utc` and the recording's own entry |

### Compare the content digest, not the file

`v2xw run` prints both. The **content digest** is SHA-256 over every message's topic,
instant and bytes in stored order, and two runs of one scenario must agree on it. The
**file** SHA-256 is allowed to differ, and does: the `mcap` 0.25 writer emits the summary
section's repeated channel and schema records from a `HashMap`, so their order moves
between runs. Measured on the Phase 1 grid scenario, the first 80,712 bytes of the 81,665
byte recording are identical across two runs and every differing byte lies in the final
953, which is the summary and footer.

### Overrides

`--duration-s` and `--rate-veh-per-h` override the scenario so that one file can be swept
without four near-identical copies on disk. Both change the scenario hash, because both
change the scenario the engine executes, and the run report says what the hash was.

`--no-recording` runs the engine into a counting sink. That is what a scaling measurement
wants: it times the engine and not the container.

## `validate`

Loads, merges `meta.base`, migrates, validates and hashes — and prints notes for the three
things the loader accepts that an author usually did not mean (no metrics, no demand, a
non-positive duration). It does not build the world, so it is instant even on a scenario
naming a 30 MB extract; whether the world can be built is what `run` finds out.

An invalid scenario names the key: `radio.tiers.phy: 'high' requires mac 'high' (mac is
'medium')`, with `the offending key is `radio.tiers.phy`` on the next line.

## `import-osm`

```
v2xw import-osm worlds/cache/manhattan.osm.xml out/ \
    --speed-preset urban-us-nyc --imported-at 2026-09-18T00:00:00Z
```

`--speed-preset` is **required and has no default**. The fallback speed of a road class is
a jurisdictional fact: under `sumo-german` a Midtown side street ends up at 100 km/h. The
preset's own citation is printed next to the choice, so the choice is on the record before
the world is written.

## `experiment`

```
v2xw experiment run scenarios/downtown-sweep.yaml --out runs/downtown --format parquet
```

Expands the scenario's `experiment` block into runs, executes each one through the same
`run` above, pools its metrics and aggregates across replications into `results.json` (plus
a Parquet, Arrow IPC or JSONL table with `--format`). `v2xw-experiment`'s README has the
sweep expansion, the seed derivation and the statistics; three things belong here.

### One simulation at a time

`--concurrency` **defaults to 1**, and on a small machine it should stay there. Each
concurrent run holds its own world, scheduler, node state and recording buffer, so `k` runs
cost `k` times the *peak* memory of one — and the engine already uses the machine's cores
inside a single run. This repository has lost a wave of work to several processes running at
once on an 8 GB machine; raising this is how a sweep is killed at run 340 of 600.

### Resume is the point

Every finished run is appended to `journal.jsonl` and flushed before the next one starts.
`v2xw experiment resume` continues from there and refuses a directory whose journal belongs
to a different sweep — the experiment changed, and appending would put two sweeps in one
results table. `v2xw experiment status` says how far along it is and names the run it
stopped on, without running anything.

### `--no-recording` for a long sweep

A sweep of six hundred cells writes six hundred MCAP files. The metrics, the run report and
the manifest are written either way, and the results table is built from the metrics, so a
sweep whose recordings you will not open is much cheaper without them.

### The clock is read once

`--build-utc` pins the manifest timestamp every run in the sweep is built with. Left out, it
is read once, in this crate, and handed to every run — so two runs of one cell differ in
their seed and in nothing else, and a manifest diff across a sweep shows the sweep rather
than the minute each run started in.
