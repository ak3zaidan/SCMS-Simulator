//! Expanding an `experiment` block into the runs it stands for.
//!
//! 08-measurement-and-data.md §4: "Runs are cells of the cartesian product × seeds …
//! deterministic cell ordering so partial reruns resume." This module is that sentence.
//!
//! # The expansion is a function, not a procedure
//!
//! [`ExperimentPlan::expand`] reads the scenario and returns the whole list of runs. It
//! draws nothing, reads no clock and touches no filesystem, so the same `experiment`
//! block always yields the same runs in the same order — which is what makes [`crate::journal`]
//! able to say "these are done, do the rest" rather than "this many were done".
//!
//! Three orderings are fixed, and all three are checked by tests:
//!
//! 1. **Axis order** is the sweep map's key order. The schema stores the sweep in a
//!    [`BTreeMap`], so it is lexicographic in the dotted path, whatever order the YAML
//!    listed the axes in.
//! 2. **Cell order** is the odometer with the *last* axis varying fastest, so a sweep
//!    whose last axis is the interesting one produces consecutive runs that differ in one
//!    parameter.
//! 3. **Run order within a cell** is seed slot, then replication.
//!
//! # Seeds are derived, never drawn
//!
//! A run's master seed is
//!
//! ```text
//! SHA-256("v2xw/experiment/seed/1" ‖ master_le ‖ slot_le ‖ declared_le ‖ replication_le)[0..8]
//! ```
//!
//! read as a little-endian `u64` ([`derive_run_seed`]). Nothing is sampled, so a plan
//! expanded twice has the same seeds, and a run rerun on its own reproduces.
//!
//! **The cell index is deliberately not in the derivation.** Every cell therefore uses the
//! *same* seed for the same (slot, replication), which is common random numbers: comparing
//! two protocols at slot 3 compares two runs whose vehicle arrivals, fading draws and
//! attacker decisions came from the same streams, so the difference between them is not
//! inflated by between-run variance. It is the standard variance-reduction arrangement for
//! a simulation study, and it is only available if the seed is a function of the
//! replication and not of the cell.
//!
//! # A declared seed is an ingredient, not the seed
//!
//! `experiment.seeds: [1, 2, 3]` asks for three replication slots, not for a run whose
//! `scenario.seed` is literally `1`. A literal 1 would make the whole run's stream keys a
//! function of a number an author picked for looking tidy, and two experiments that both
//! chose `[1, 2, 3]` would share every stream. Mixing the declared value with the master
//! seed keeps the slot label meaningful — slot 1 is slot 1 in every cell — without that.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use v2xw_engine::Scenario;

use crate::error::{ExperimentError, Result};
use crate::path::set_path;

/// The domain separator the run-seed derivation is prefixed with.
///
/// Present for the same reason every other derivation in this project has one: two
/// derivations that hash the same bytes for different purposes are one refactor away from
/// colliding, and a collision between an RNG stream key and a run seed would be invisible.
pub const SEED_DOMAIN: &[u8] = b"v2xw/experiment/seed/1";

/// The schema id the plan document carries.
pub const PLAN_SCHEMA: &str = "v2xw/experiment-plan/1";

/// One point of the cartesian product: what every run in it holds fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellKey {
    /// The cell's position in plan order, from zero.
    pub index: usize,
    /// The swept parameter values, keyed by dotted path and therefore in path order.
    pub values: BTreeMap<String, Value>,
}

impl CellKey {
    /// A one-line label, `path=value,path=value` with each value as compact JSON.
    ///
    /// Stable: the map is a [`BTreeMap`] and the values go through `serde_json`'s compact
    /// form, so the label of a cell is the same string in every process.
    #[must_use]
    pub fn label(&self) -> String {
        if self.values.is_empty() {
            return "(single point)".to_string();
        }
        let mut parts = Vec::with_capacity(self.values.len());
        for (path, value) in &self.values {
            parts.push(format!("{path}={value}"));
        }
        parts.join(",")
    }
}

/// One run the plan calls for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedRun {
    /// The run's id, `c<cell>-s<slot>-r<replication>`. Also its directory name.
    pub run_id: String,
    /// The cell this run belongs to.
    pub cell: CellKey,
    /// Which declared seed slot this run is in.
    pub seed_slot: usize,
    /// The value that slot declared, or the scenario's master seed when the block
    /// declared no seeds.
    pub declared_seed: u64,
    /// Which replication within the slot, from zero.
    pub replication: u32,
    /// The master seed the engine will actually run under, derived by [`derive_run_seed`].
    pub seed: u64,
}

impl PlannedRun {
    /// The derived master seed as `0x…` hexadecimal, the form a scenario writes it in.
    #[must_use]
    pub fn seed_hex(&self) -> String {
        format!("0x{:016x}", self.seed)
    }
}

/// An expanded experiment: every cell, every run, and the digest that identifies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExperimentPlan {
    /// The schema id, [`PLAN_SCHEMA`].
    pub schema: String,
    /// The scenario's name.
    pub name: String,
    /// The scenario's master seed, the root of every derived run seed.
    pub master_seed: u64,
    /// The swept axes, in path order.
    pub axes: Vec<String>,
    /// The declared seed slots, in declaration order.
    pub seed_slots: Vec<u64>,
    /// Replications per (cell, seed slot); at least one.
    pub replications: u32,
    /// The cells, in plan order.
    pub cells: Vec<CellKey>,
    /// The runs, in plan order.
    pub runs: Vec<PlannedRun>,
    /// SHA-256 over the canonical JSON of everything above but the runs, which are a
    /// function of it. Two scenarios that expand to the same sweep share this digest, and
    /// a journal refuses to be resumed under a different one.
    pub digest: String,
}

impl ExperimentPlan {
    /// Expands a scenario's `experiment` block.
    ///
    /// # Errors
    /// [`ExperimentError::NoExperiment`] if the scenario declares no experiment,
    /// [`ExperimentError::EmptySweepAxis`] for an axis with no values,
    /// [`ExperimentError::SweepTooLarge`] if the product overflows, and
    /// [`ExperimentError::Core`] if the plan will not canonicalise.
    pub fn expand(base: &Scenario) -> Result<ExperimentPlan> {
        let block = base
            .experiment
            .as_ref()
            .ok_or(ExperimentError::NoExperiment)?;

        let axes: Vec<String> = block.sweep.keys().cloned().collect();
        let columns: Vec<&Vec<Value>> = block.sweep.values().collect();
        for (path, values) in &block.sweep {
            if values.is_empty() {
                return Err(ExperimentError::EmptySweepAxis { path: path.clone() });
            }
        }

        // The product of the axis lengths. An empty sweep is one cell — the base scenario
        // itself — which is the shape "replications of one configuration" takes.
        let mut total: usize = 1;
        for (i, column) in columns.iter().enumerate() {
            total =
                total
                    .checked_mul(column.len())
                    .ok_or_else(|| ExperimentError::SweepTooLarge {
                        path: axes[i].clone(),
                    })?;
        }

        let mut cells = Vec::with_capacity(total);
        for index in 0..total {
            let mut remainder = index;
            let mut values = BTreeMap::new();
            // Last axis first, so it takes the lowest-order digit and varies fastest.
            for (i, path) in axes.iter().enumerate().rev() {
                let column = columns[i];
                let pick = remainder % column.len();
                remainder /= column.len();
                values.insert(path.clone(), column[pick].clone());
            }
            cells.push(CellKey { index, values });
        }

        let seed_slots: Vec<u64> = if block.seeds.is_empty() {
            vec![base.seed]
        } else {
            block.seeds.clone()
        };
        let replications = block.replications.max(1);

        let mut runs = Vec::with_capacity(cells.len() * seed_slots.len() * replications as usize);
        for cell in &cells {
            for (slot, declared) in seed_slots.iter().enumerate() {
                for replication in 0..replications {
                    runs.push(PlannedRun {
                        run_id: run_id(cell.index, slot, replication),
                        cell: cell.clone(),
                        seed_slot: slot,
                        declared_seed: *declared,
                        replication,
                        seed: derive_run_seed(base.seed, slot, *declared, replication),
                    });
                }
            }
        }

        let mut plan = ExperimentPlan {
            schema: PLAN_SCHEMA.to_string(),
            name: base.meta.name.clone(),
            master_seed: base.seed,
            axes,
            seed_slots,
            replications,
            cells,
            runs,
            digest: String::new(),
        };
        plan.digest = plan.compute_digest()?;
        Ok(plan)
    }

    /// The digest of everything that defines the sweep, the runs excluded.
    ///
    /// # Errors
    /// [`ExperimentError::Core`] if the identity document will not canonicalise.
    fn compute_digest(&self) -> Result<String> {
        let identity = serde_json::json!({
            "schema": PLAN_SCHEMA,
            "name": self.name,
            "master_seed": self.master_seed,
            "axes": self.axes,
            "seed_slots": self.seed_slots,
            "replications": self.replications,
            "cells": self.cells,
        });
        let bytes = v2xw_core::hash::canonical_json(&identity)?;
        Ok(v2xw_core::hash::sha256_hex(&bytes))
    }

    /// How many runs the plan calls for.
    #[must_use]
    pub fn len(&self) -> usize {
        self.runs.len()
    }

    /// True if the plan calls for no runs at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The scenario one planned run executes.
    ///
    /// Three edits are made to the base document, and each is deliberate:
    ///
    /// * every swept path is replaced with the cell's value;
    /// * `seed` becomes the run's derived seed;
    /// * `experiment` is removed, so the run's scenario hash is the hash of the ordinary
    ///   scenario it is, and rerunning a cell's file on its own gives the same hash.
    ///
    /// `meta.name` gains the run id, so a run report, a manifest and a recording all name
    /// the run they came from rather than all claiming to be the sweep.
    ///
    /// # Errors
    /// [`ExperimentError::BadSweepPath`] if a swept path is not writable in the document,
    /// [`ExperimentError::Engine`] if the edited document is not a valid scenario, and
    /// [`ExperimentError::Json`] if the base will not serialise.
    pub fn materialise(&self, base: &Scenario, run: &PlannedRun) -> Result<Scenario> {
        let mut document =
            serde_json::to_value(base).map_err(|e| ExperimentError::json("the scenario", e))?;
        for (path, value) in &run.cell.values {
            set_path(&mut document, path, value.clone())?;
        }
        let mut scenario = Scenario::from_document(document)?;
        scenario.seed = run.seed;
        scenario.experiment = None;
        scenario.meta.name = format!("{}-{}", base.meta.name, run.run_id);
        scenario
            .validate()
            .map_err(v2xw_engine::EngineError::from)?;
        Ok(scenario)
    }
}

/// The run id a cell, slot and replication get: `c0003-s00-r001`.
///
/// Fixed width so that the ids sort in plan order as text, which is what makes a directory
/// listing of `runs/` readable and a journal diff meaningful.
#[must_use]
pub fn run_id(cell: usize, seed_slot: usize, replication: u32) -> String {
    format!("c{cell:04}-s{seed_slot:02}-r{replication:03}")
}

/// Derives the master seed one run executes under.
///
/// `SHA-256(domain ‖ master_le ‖ slot_le ‖ declared_le ‖ replication_le)`, first eight
/// bytes read little-endian. See this module's header for why the cell index is not an
/// input.
#[must_use]
pub fn derive_run_seed(
    master_seed: u64,
    seed_slot: usize,
    declared_seed: u64,
    replication: u32,
) -> u64 {
    let mut material = Vec::with_capacity(SEED_DOMAIN.len() + 28);
    material.extend_from_slice(SEED_DOMAIN);
    material.extend_from_slice(&master_seed.to_le_bytes());
    material.extend_from_slice(&(seed_slot as u64).to_le_bytes());
    material.extend_from_slice(&declared_seed.to_le_bytes());
    material.extend_from_slice(&replication.to_le_bytes());
    let digest = v2xw_core::hash::sha256(&material);
    let head: [u8; 8] = digest[0..8]
        .try_into()
        .expect("a SHA-256 digest is 32 bytes, so its first 8 are an 8-byte array");
    u64::from_le_bytes(head)
}
