# `v2xw-experiment` — from one run to a result

An `experiment` block in a scenario (08-measurement-and-data.md §4) declares a sweep, a
seed list and a replication count. This crate expands it into runs, executes them, pools
each run's metrics, aggregates across replications, and writes one results table — recording
what it finished as it goes, so an interruption costs the run that was in flight and nothing
more.

```yaml
experiment:
  sweep:
    security.protocol.id: [scms-camp, etsi-ts102941]
    actors.vehicles.demand.rate_veh_per_h: [1500.0, 15000.0]
  seeds: [1, 2, 3]
  replications: 2
```

Four sweep points × three seed slots × two replications = **24 runs**, in a fixed order,
under 24 seeds derived from the scenario's master seed.

## Run it

```
v2xw experiment run    scenarios/downtown-sweep.yaml --out runs/downtown
v2xw experiment status scenarios/downtown-sweep.yaml --out runs/downtown
v2xw experiment resume scenarios/downtown-sweep.yaml --out runs/downtown
```

`run` and `resume` are the same code path. The difference is that `resume` refuses to start
a sweep that was never started here, so a typo in `--out` is an error rather than six hundred
runs beginning again in the wrong directory.

## Run one simulation at a time

**`--concurrency` defaults to 1, and on a small machine it should stay there.**

This is not a placeholder. Each concurrent run holds its own world, scheduler, node state
and recording buffer, so `k` runs cost `k` times the *peak* memory of one — and the engine
already uses the machine's cores inside a single run, so a second concurrent run mostly
competes with the first for the same cores while doubling the memory. This repository has
already lost a whole wave of work to exactly that: several processes running at once on an
8 GB machine, the OOM killer, and a job that had to start over.

If you do raise it, budget by peak memory of one run rather than average, leave the operating
system a couple of gigabytes, and use `--no-recording` for cells whose MCAP you will not open.
The resume journal is what makes an OOM survivable, not what makes it acceptable.

## What it writes

```
runs/downtown/
  experiment.json          the expanded plan: axes, cells, runs, seeds, plan digest
  journal.jsonl            one line per finished run — the resume record
  results.json             the aggregated table
  results.parquet          …and the tabular export, with --format
  results.schema.json      the declared grid of every float column
  runs/c0000-s00-r000/
    scenario.yaml          the document this run executed
    metrics.json           its metric samples
    run-metrics.json       those samples pooled to one value per metric per bin
    recording.mcap, run-report.json, manifest.json
```

## Seeds are derived, never drawn

```
run seed = SHA-256("v2xw/experiment/seed/1" ‖ master ‖ slot ‖ declared ‖ replication)[0..8]
```

Two consequences, both deliberate:

- **A declared seed is an ingredient, not the seed.** `seeds: [1, 2, 3]` asks for three
  replication slots. A literal `1` reaching `scenario.seed` would make every stream key in
  the run a function of a number chosen for looking tidy, and two unrelated experiments that
  both picked `[1, 2, 3]` would share every stream.
- **The cell index is not an input.** Every cell therefore uses the *same* seed for the same
  (slot, replication) — common random numbers. Comparing two protocols at slot 3 compares two
  runs whose arrivals, fading draws and attacker decisions came from the same streams, so the
  difference between them is not inflated by between-run variance.

## Statistics

Every row carries its replication count, its underlying sample count and an interval, and
names which rule produced the interval:

| Shape | Interval | Why |
|---|---|---|
| proportion (`pdr`, `det_recall`, …) | **Wilson score**, over the *pooled* successes and trials | a proportion has a binomial sampling distribution; `v2xw-metrics` computes it, this crate does not |
| ratio of sums, scalar, distribution mean, count | normal approximation over the replications, `mean ± z·s/√k` | not a count of Bernoulli trials, so Wilson does not apply and a Wilson bound on it would be fabricated |
| one usable replication | **none** | the mean is a fact; the error bar would not be |

Windows are pooled by **adding the counts**, not by averaging the windows' ratios: a window
with four trials and a window with four thousand are not equally informative.

The honest weakness is written down rather than hidden. The normal approximation is narrower
than a Student-`t` interval below roughly thirty replications; a `t` quantile needs either a
transcendental (which build decision D10 forbids on a path whose output is compared across
engines) or a table this crate does not ship. Every row therefore reports `replications` and
`stddev` next to the interval, so a reader can judge it or recompute it.

## Resume

`journal.jsonl` is append-only and flushed before the runner moves on, with a header naming
the **plan digest**. A journal whose digest disagrees with the scenario's sweep is refused:
the experiment changed under the directory, and appending would put two sweeps in one table.

Resume matches on run id, not on a count, so it is correct even if runs finished out of order.

## Determinism

- Nothing is drawn; seeds and the plan digest are SHA-256 of canonical inputs.
- Every map is a `BTreeMap`, so cells, runs, metric keys and rows come out in one order.
- Float reductions go through `sort_total_order` then `sum_ordered`; the only non-arithmetic
  operation in the crate is `v2xw_core::math::sqrt`, inside the standard deviation.
- **No wall clock is read.** The wall-clock seconds in a journal entry are a number the
  executor handed over, and nothing reads them back.
- Machine-dependent diagnostics (`events_per_second`, `wall_clock_per_sim_second`,
  `memory_high_water_mark`) never reach the results table.
- Every exported float is quantised at the writer, by `v2xw-record`'s exporters, onto the grid
  this crate declares for its column (D9).
