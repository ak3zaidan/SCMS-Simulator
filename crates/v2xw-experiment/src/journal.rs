//! The resume journal: which runs are done, written as they finish.
//!
//! This project has now lost a long job twice — once to a machine memory limit, once to an
//! interrupted wave — so a sweep that has to start over is not an inconvenience, it is the
//! failure mode. The journal is the smallest thing that fixes it: an append-only JSONL
//! file, one line per finished run, flushed before the runner moves on.
//!
//! # Why a line per run and not a progress count
//!
//! A count only resumes correctly if the run order is the same, which it is — but it
//! cannot notice that the *plan* changed. A line per run, keyed by [`crate::plan::run_id`],
//! resumes correctly even if runs completed out of order, and the header's plan digest
//! makes a changed sweep a refusal ([`ExperimentError::PlanChanged`]) rather than a table
//! that quietly mixes two experiments.
//!
//! # The journal is a log, not a digested artefact
//!
//! Each entry carries the run's wall-clock seconds, which is a measurement of the machine
//! and not of the simulation. Nothing in the journal reaches the results table, the plan
//! digest or any manifest; it exists so that `experiment status` can answer "how far along,
//! and how long has it been taking" without opening a recording. Every line is written
//! through `canonical_json`, so the file is otherwise byte-reproducible.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{ExperimentError, Result};

/// The file name a journal lives under inside an experiment's output directory.
pub const JOURNAL_FILE: &str = "journal.jsonl";

/// The schema id the header carries.
pub const JOURNAL_SCHEMA: &str = "v2xw/experiment-journal/1";

/// The journal's first line: what sweep these entries belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalHeader {
    /// The schema id, [`JOURNAL_SCHEMA`].
    pub schema: String,
    /// The experiment's name, from `meta.name`.
    pub experiment: String,
    /// The plan digest these entries were produced under.
    pub plan_digest: String,
    /// How many runs the plan called for when the journal was opened.
    pub runs_planned: usize,
}

/// One finished run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The run's id, which is the key resume matches on.
    pub run_id: String,
    /// Which cell it belonged to.
    pub cell_index: usize,
    /// The derived master seed it ran under, hexadecimal.
    pub seed_hex: String,
    /// The scenario hash the run executed under.
    pub scenario_hash: String,
    /// The world's content hash.
    pub world_hash: String,
    /// Where the run's outputs are, relative to the experiment directory.
    pub out_dir: String,
    /// SHA-256 over the recording's data section, when one was written.
    pub content_digest: Option<String>,
    /// Wall-clock seconds the run took. A measurement of the machine; nothing reads it
    /// back into the simulation.
    pub wall_s: f64,
}

/// One line of the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "kebab-case")]
pub enum JournalLine {
    /// The header, written once when the journal is created.
    Header(JournalHeader),
    /// A finished run.
    Run(JournalEntry),
}

/// An open journal: the entries already on disk, and a handle to append to.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    header: JournalHeader,
    completed: BTreeMap<String, JournalEntry>,
    file: File,
}

impl Journal {
    /// Opens the journal in `dir`, creating it with `header` if there is none.
    ///
    /// An existing journal whose header names a different plan digest is refused: it
    /// belongs to a different sweep, and appending to it would put two experiments in one
    /// results table.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if the file cannot be read, created or appended to,
    /// [`ExperimentError::BadJournal`] if a line will not parse, and
    /// [`ExperimentError::PlanChanged`] if the header names a different plan.
    pub fn open(dir: &Path, header: JournalHeader) -> Result<Journal> {
        let path = dir.join(JOURNAL_FILE);
        let mut completed = BTreeMap::new();
        let existing = if path.exists() {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| ExperimentError::io("cannot read the journal", &path, e))?;
            let mut recorded: Option<JournalHeader> = None;
            for (index, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let parsed: JournalLine =
                    serde_json::from_str(line).map_err(|e| ExperimentError::BadJournal {
                        path: path.clone(),
                        line: index + 1,
                        problem: e.to_string(),
                    })?;
                match parsed {
                    JournalLine::Header(h) => recorded = Some(h),
                    JournalLine::Run(entry) => {
                        completed.insert(entry.run_id.clone(), entry);
                    }
                }
            }
            recorded
        } else {
            None
        };

        if let Some(recorded) = &existing
            && recorded.plan_digest != header.plan_digest
        {
            return Err(ExperimentError::PlanChanged {
                journal: path.clone(),
                recorded: recorded.plan_digest.clone(),
                current: header.plan_digest.clone(),
            });
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| ExperimentError::io("cannot open the journal", &path, e))?;
        if existing.is_none() {
            let line = canonical_line(&JournalLine::Header(header.clone()))?;
            write_line(&mut file, &path, &line)?;
        }

        Ok(Journal {
            path,
            header: existing.unwrap_or(header),
            completed,
            file,
        })
    }

    /// Reads a journal without opening it for writing, for `experiment status`.
    ///
    /// # Errors
    /// [`ExperimentError::NoJournal`] if there is none, [`ExperimentError::Io`] if it
    /// cannot be read and [`ExperimentError::BadJournal`] if a line will not parse.
    pub fn read(dir: &Path) -> Result<(JournalHeader, BTreeMap<String, JournalEntry>)> {
        let path = dir.join(JOURNAL_FILE);
        if !path.exists() {
            return Err(ExperimentError::NoJournal { path });
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| ExperimentError::io("cannot read the journal", &path, e))?;
        let mut header: Option<JournalHeader> = None;
        let mut completed = BTreeMap::new();
        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: JournalLine =
                serde_json::from_str(line).map_err(|e| ExperimentError::BadJournal {
                    path: path.clone(),
                    line: index + 1,
                    problem: e.to_string(),
                })?;
            match parsed {
                JournalLine::Header(h) => header = Some(h),
                JournalLine::Run(entry) => {
                    completed.insert(entry.run_id.clone(), entry);
                }
            }
        }
        let header = header.ok_or_else(|| ExperimentError::BadJournal {
            path: path.clone(),
            line: 1,
            problem: "the journal has no header line, so it names no experiment".to_string(),
        })?;
        Ok((header, completed))
    }

    /// The header this journal was opened under.
    #[must_use]
    pub fn header(&self) -> &JournalHeader {
        &self.header
    }

    /// Where the journal is.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The runs already finished, keyed by run id.
    #[must_use]
    pub fn completed(&self) -> &BTreeMap<String, JournalEntry> {
        &self.completed
    }

    /// True if this run is already done.
    #[must_use]
    pub fn is_done(&self, run_id: &str) -> bool {
        self.completed.contains_key(run_id)
    }

    /// Appends a finished run and flushes, so an interruption one instant later still
    /// finds it recorded.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if the line cannot be written or flushed, and
    /// [`ExperimentError::Core`] if it will not canonicalise.
    pub fn record(&mut self, entry: JournalEntry) -> Result<()> {
        let line = canonical_line(&JournalLine::Run(entry.clone()))?;
        write_line(&mut self.file, &self.path, &line)?;
        self.completed.insert(entry.run_id.clone(), entry);
        Ok(())
    }
}

fn canonical_line(line: &JournalLine) -> Result<Vec<u8>> {
    Ok(v2xw_core::hash::canonical_json(line)?)
}

fn write_line(file: &mut File, path: &Path, line: &[u8]) -> Result<()> {
    file.write_all(line)
        .map_err(|e| ExperimentError::io("cannot append to the journal", path, e))?;
    file.write_all(b"\n")
        .map_err(|e| ExperimentError::io("cannot append to the journal", path, e))?;
    // Flushed rather than buffered: the whole point is that the line survives whatever
    // stops the process a moment later.
    file.flush()
        .map_err(|e| ExperimentError::io("cannot flush the journal", path, e))
}
