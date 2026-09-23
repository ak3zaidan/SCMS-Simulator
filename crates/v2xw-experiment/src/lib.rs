//! `v2xw-experiment` — how a researcher gets from one run to a result.
//!
//! An `experiment` block in a scenario (08-measurement-and-data.md §4) declares a sweep
//! over parameters, a seed list and a replication count. This crate expands it into runs,
//! executes them, pools each run's metrics, aggregates across replications and writes one
//! results table — and it records what it has finished as it goes, so an interruption costs
//! the run that was in flight and nothing else.
//!
//! ```yaml
//! experiment:
//!   sweep:
//!     security.protocol.id: [scms-camp, etsi-ts102941]
//!     actors.vehicles.demand.rate_veh_per_h: [1500.0, 15000.0]
//!   seeds: [1, 2, 3]
//!   replications: 2
//! ```
//!
//! Four axes values × three seed slots × two replications is 24 runs, in a fixed order,
//! with 24 seeds derived from the scenario's master seed.
//!
//! | Module | What it does |
//! |---|---|
//! | [`plan`] | expands the block into cells and runs, and derives each run's seed |
//! | [`path`] | writes a swept value into the scenario document at its dotted path |
//! | [`runner`] | executes the plan through a [`runner::RunExecutor`], serially by default |
//! | [`journal`] | records what finished, so a restart skips it |
//! | [`reduce`] | pools one run's `metric.sample` windows into one value per metric per bin |
//! | [`aggregate`] | means and intervals across replications, Wilson for proportions |
//! | [`table`] | the results table, written through the exporters `v2xw-record` already has |
//! | [`figures`] | the figure presets of 08-measurement-and-data.md §7: a results table in, Plotly JSON out |
//! | [`foundry`] | the MAP-Elites misbehaviour foundry (07-threats §2.3), over the same executor seam |
//! | [`foundry_eval`] | whether a proposed mutation operator actually beats random search |
//!
//! # Five properties, each of which is a test
//!
//! 1. **The expansion is a function.** The same `experiment` block yields the same runs in
//!    the same order, and each run's seed is *derived* from the master seed — hashed, never
//!    drawn ([`plan::derive_run_seed`]).
//! 2. **No result stands on one replication.** Every row carries its replication count, its
//!    underlying sample count and an interval, and a row with one usable replication carries
//!    [`aggregate::CiMethod::None`] rather than an error bar it has not earned.
//! 3. **A proportion's interval is the Wilson score interval**, computed by `v2xw-metrics`.
//!    This crate implements no interval for a proportion and never will; the one that exists
//!    is the one this repository already reasoned about.
//! 4. **Resume is by run id, not by count.** [`journal`] records a line per finished run and
//!    refuses to append to a journal that belongs to a different sweep.
//! 5. **Serial by default.** [`runner::DEFAULT_CONCURRENCY`] is 1. Running many simulations
//!    at once on a small machine exhausts memory, and this project has already lost a wave
//!    of work to exactly that.
//!
//! # Determinism
//!
//! * Nothing is drawn. Run seeds are `SHA-256` of a domain-separated key; the plan digest is
//!   `SHA-256` of the canonical JSON of the sweep.
//! * No `HashMap` reaches an ordering: every map here is a [`std::collections::BTreeMap`],
//!   so cells, runs, metric keys and table rows come out in one fixed order.
//! * Every float reduction goes through `v2xw_core::math::sort_total_order` and
//!   `sum_ordered`, and the only non-arithmetic operation in the crate is
//!   `v2xw_core::math::sqrt` inside the standard deviation. No `std` transcendental is
//!   called.
//! * **No wall clock is read.** The runner never reads one; the wall-clock seconds in a
//!   journal entry are a number the executor handed over, and nothing reads them back.
//! * Every exported float is quantised at the writer, by `v2xw-record`'s exporters, onto the
//!   grid [`table`] declares for its column (build decision D9).
//!
//! # Using it
//!
//! ```no_run
//! use std::path::Path;
//! use v2xw_engine::Scenario;
//! use v2xw_experiment::runner::{RunnerOptions, run_experiment};
//! # use v2xw_experiment::runner::{RunArtifacts, RunExecutor};
//! # use v2xw_experiment::plan::PlannedRun;
//! # struct MyExecutor;
//! # impl RunExecutor for MyExecutor {
//! #     fn execute(&self, _r: &PlannedRun, _s: &Path, _o: &Path)
//! #         -> Result<RunArtifacts, String> { unimplemented!() }
//! # }
//! let scenario = Scenario::load("scenarios/sweep.yaml")?;
//! let options = RunnerOptions::new("runs/sweep");   // serial: concurrency 1
//! let outcome = run_experiment(&scenario, &MyExecutor, &options)?;
//! println!("{} of {} runs done", outcome.runs_executed, outcome.runs_planned);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod aggregate;
pub mod error;
pub mod figures;
pub mod foundry;
pub mod foundry_eval;
pub mod journal;
pub mod path;
pub mod plan;
pub mod reduce;
pub mod runner;
pub mod table;

pub use aggregate::{AggregateOptions, CellAggregate, CiMethod, aggregate_cell};
pub use error::{ExperimentError, Result};
pub use journal::{Journal, JournalEntry, JournalHeader};
pub use plan::{CellKey, ExperimentPlan, PlannedRun, derive_run_seed};
pub use reduce::{RunMetric, RunMetrics, RunValue, reduce_run};
pub use runner::{
    DEFAULT_CONCURRENCY, ExperimentOutcome, ExperimentStatus, RunArtifacts, RunExecutor,
    RunnerOptions, rebuild_table, run_experiment, status,
};
pub use table::ResultsTable;

pub use figures::{Axis, Figure, FigurePreset, FigureSet, PanelSpec, all_presets, preset, render};
pub use foundry::{
    Archive, Descriptor, Elite, FoundryOptions, Genome, MutationOperator, Objective,
    RandomMutation, Signals, Validity, search,
};
pub use foundry_eval::{Comparison, EvalOptions, Verdict, compare_operators};
