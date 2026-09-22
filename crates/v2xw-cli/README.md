# `v2xw` — the command line

```
v2xw run <scenario>            run a scenario; write its recording, metrics and manifest
v2xw validate <scenario>       load and check a scenario without building its world
v2xw import-osm <extract> <out> --speed-preset <name>
                               import an OpenStreetMap extract into the three world formats
v2xw info <recording>          print a recording's manifest, channels and verification report
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
