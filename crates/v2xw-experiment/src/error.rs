//! What the runner can fail with, and how it says so.
//!
//! Every variant names the run, the path or the sweep axis it is about. A sweep that dies
//! on run 340 of 600 with "error" costs an afternoon; one that says which run, which
//! parameter value and which seed costs a minute.

use std::path::PathBuf;

use thiserror::Error;

/// Anything the experiment runner can fail with.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExperimentError {
    /// A file could not be read or written.
    #[error("{what} {path}: {source}")]
    Io {
        /// What the runner was doing, e.g. `cannot write`.
        what: &'static str,
        /// The path it was doing it to.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// The scenario carries no `experiment` block.
    #[error(
        "this scenario has no `experiment` block, so there is no sweep to expand \
         (08-measurement-and-data.md §4 has the shape)"
    )]
    NoExperiment,

    /// A sweep axis declares no values, so the cartesian product is empty.
    #[error(
        "experiment.sweep.{path}: has no values, so the sweep over it is empty and the \
         whole experiment collapses to nothing"
    )]
    EmptySweepAxis {
        /// The dotted path of the axis.
        path: String,
    },

    /// A sweep path does not name a field of the scenario, or names one that cannot be
    /// replaced.
    #[error("experiment.sweep.{path}: {problem}")]
    BadSweepPath {
        /// The dotted path.
        path: String,
        /// What is wrong with it.
        problem: String,
    },

    /// The cartesian product does not fit in a `usize`.
    #[error(
        "the sweep's cartesian product overflows a machine word at axis `{path}`: \
         reduce the number of swept values"
    )]
    SweepTooLarge {
        /// The axis at which the running product overflowed.
        path: String,
    },

    /// The journal on disk was written for a different plan.
    #[error(
        "{journal} records plan {recorded}, but this scenario expands to plan {current}: \
         the experiment changed since it was started, so resuming would mix two sweeps in \
         one results table. Start a new output directory, or restore the scenario."
    )]
    PlanChanged {
        /// The journal file.
        journal: PathBuf,
        /// The plan digest the journal was opened with.
        recorded: String,
        /// The plan digest the scenario expands to now.
        current: String,
    },

    /// There is no journal to resume from.
    #[error(
        "{path} does not exist: `experiment resume` continues a sweep that \
         `experiment run` started, and nothing has been started here"
    )]
    NoJournal {
        /// Where the journal was expected.
        path: PathBuf,
    },

    /// A journal line will not parse.
    #[error("{path} line {line}: {problem}")]
    BadJournal {
        /// The journal file.
        path: PathBuf,
        /// The one-based line number.
        line: usize,
        /// What is wrong with it.
        problem: String,
    },

    /// One run failed. The runner reports which one, with whatever the executor said.
    #[error("run {run_id} failed: {message}")]
    Run {
        /// The run's id, e.g. `c0003-s00-r000`.
        run_id: String,
        /// What the executor reported.
        message: String,
    },

    /// A worker thread panicked. Only reachable with `--concurrency` above one.
    #[error("the worker running {run_id} panicked; rerun with --concurrency 1 to see why")]
    WorkerPanicked {
        /// The run the worker was on.
        run_id: String,
    },

    /// The engine refused the scenario a sweep point produced.
    #[error(transparent)]
    Engine(#[from] v2xw_engine::EngineError),

    /// A core contract failed, such as a document that will not canonicalise.
    #[error(transparent)]
    Core(#[from] v2xw_core::error::CoreError),

    /// The results table could not be exported.
    #[error(transparent)]
    Record(#[from] v2xw_record::RecordError),

    /// JSON could not be produced or parsed.
    #[error("cannot handle {what} as JSON: {source}")]
    Json {
        /// What failed.
        what: &'static str,
        /// The serde error.
        #[source]
        source: serde_json::Error,
    },
}

impl ExperimentError {
    /// An I/O failure with the context of what the runner was attempting.
    #[must_use]
    pub fn io(what: &'static str, path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ExperimentError::Io {
            what,
            path: path.into(),
            source,
        }
    }

    /// A JSON failure with the context of what was being read or written.
    #[must_use]
    pub fn json(what: &'static str, source: serde_json::Error) -> Self {
        ExperimentError::Json { what, source }
    }
}

/// The runner's result alias.
pub type Result<T> = core::result::Result<T, ExperimentError>;
