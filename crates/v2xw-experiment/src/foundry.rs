//! The misbehaviour-scenario foundry: a quality-diversity search over scenario space for
//! the cases a detector misses.
//!
//! Ported from `legacy/scms_sim_ref/datagen/foundry.py` against the experiment runner, per
//! 07-threats-and-detection.md §2.3: *"the genome becomes a scenario overlay …, the
//! descriptor keeps the four legacy axes (family, density band, topology, attacker band)
//! and gains `rat` and `protocol`, the objective reads `det_recall`, `time_to_detect`, or
//! `residual_harm` from the metrics, and the LLM mutation operator hook is unchanged.
//! Elite replay is a scenario file plus a seed."*
//!
//! # What it is, in one paragraph
//!
//! MAP-Elites. The search keeps **one elite per descriptor cell** rather than one global
//! best, so the archive it produces is *diverse by construction* — a representative of
//! every corner of the space it reached — and *hard by construction* — within each corner,
//! the most evasive scenario found. The result is a detector blind-spot map: where the
//! **fixed** detector fails, and where the coverage is thin.
//!
//! ```no_run
//! use v2xw_experiment::foundry::{FoundryOptions, Objective, RandomMutation, search};
//! use v2xw_engine::Scenario;
//! # use v2xw_experiment::runner::RunExecutor;
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! # let executor: &dyn RunExecutor = unimplemented!();
//! let base = Scenario::load("scenarios/revocation-latency.yaml")?;
//! let options = FoundryOptions::new("runs/foundry").budget(60).objective(Objective::Evade);
//! let archive = search(&base, executor, &RandomMutation, &options)?;
//! println!("{} of {} cells filled", archive.coverage(), options.grid_size());
//! # Ok(()) }
//! ```
//!
//! # The search never weakens the detector
//!
//! Every mutation operator in [`RandomMutation`] perturbs an **adversary, environment or
//! topology** knob. None of them touches `detection.local`, `detection.ma`,
//! `security.verification_policy` or any detector threshold. That is not a convention: it
//! is the difference between an archive that maps the blind spots of the production
//! detector and an archive that maps the blind spots of a detector the search quietly
//! switched off, and the second is worthless. [`MutationOperator::propose`]'s output goes
//! through [`RandomMutation::TOUCHES`]'s path list before it is run, so an *injected*
//! operator — a language model, say — cannot weaken the detector either, however it was
//! prompted.
//!
//! # The validity gate
//!
//! A candidate is archived only if it was a real misbehaviour scenario:
//!
//! 1. **at least one true attacker existed** — `det_tp` + `det_fn` at `level=vehicle`; and
//! 2. **the reporting pipeline actually fired** — `det_tp` + `det_fp` at `level=report`.
//!
//! The second is the one that matters, and it is the legacy gate's own hard-won lesson: a
//! scenario with attackers that nobody ever observed scores `recall = 0` and therefore
//! `evade` fitness 1.0, and would be archived as the "hardest" elite — so the search would
//! then actively chase scenarios in which *nobody looked*. A genuine blind spot (reports
//! did fire, and missed the attackers) is preserved, because that is the jackpot the
//! search exists to find.
//!
//! # Three honest divergences from the Python foundry
//!
//! 1. **`family:<F>` is a *gate*, not a per-family recall.** The legacy objective read
//!    `recall_by_family[F]` out of `validate.py`. This engine's `det_recall` carries a
//!    `level` dimension and no family dimension, so there is no per-family recall to read.
//!    [`Objective::TargetFamily`] therefore scores `1 − det_recall` and *gates out* any
//!    candidate whose attacker family is not the target — which tests family F, by
//!    restricting the population rather than by slicing the metric. Owed, to close it
//!    properly: a `Dim::Family` on the detection provider, fed from `gt.attack.action`.
//! 2. **The density band comes from the *declared* demand rate, not the realised vehicle
//!    count.** The legacy descriptor binned `summary["vehicles"]`. There is no run-level
//!    vehicle-count or density metric in `v2xw-metrics` (`density` is per lane, keyed by
//!    `Dim::Region`), and [`crate::runner::RunArtifacts`] carries no count either. So the
//!    axis is a configuration axis. It is a weaker descriptor — two cells in the same band
//!    can have realised densities that differ by a factor of two — and it is stated rather
//!    than papered over. Owed: a `vehicles` count metric, or a field on `RunArtifacts`.
//! 3. **`residual_harm` is not an available objective.** §2.3 names it and
//!    `v2xw-metrics` has no provider for it. Rather than ship an objective that can never
//!    be valid, it is absent, and this is the note that says so.
//!
//! # Determinism
//!
//! * Nothing is drawn outside the [`v2xw_core::rng`] registry. The driver's own randomness
//!   comes from one **ephemeral** stream per iteration, keyed by
//!   `(RngDomain::plugin("experiment/foundry/map-elites"), EntityRef::custom(.., iteration))`,
//!   so the sequence a given iteration sees is a function of the master seed and the
//!   iteration index alone — not of how many candidates happened to be infeasible before
//!   it.
//! * Every per-candidate engine seed is derived by SHA-256, never sampled
//!   ([`derive_candidate_seed`]).
//! * The archive is a [`BTreeMap`], so `archive.json` is byte-identical for one
//!   `(budget, seed, objective)` triple.
//! * **No wall clock is read.** The wall-clock seconds an executor reports are recorded
//!   and never read back.
//!
//! An *injected* operator that consults external state — a language model — makes the run
//! non-deterministic by design. [`crate::foundry_eval`] is what measures whether that buys
//! anything.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry, RngStream};
use v2xw_engine::Scenario;
use v2xw_threat::AttackKind;

use crate::error::{ExperimentError, Result};
use crate::path::set_path;
use crate::reduce::{RunMetric, RunValue, read_samples, reduce_run};
use crate::runner::RunExecutor;

/// The model id the foundry's RNG domain and its report are keyed by.
pub const FOUNDRY_MODEL_ID: &str = "experiment/foundry/map-elites";

/// The schema id `archive.json` carries.
pub const ARCHIVE_SCHEMA: &str = "v2xw/experiment-foundry-archive/1";

/// The domain separator the per-candidate seed derivation is prefixed with.
pub const SEED_DOMAIN: &[u8] = b"v2xw/experiment/foundry/seed/1";

/// The file the archive is written to.
pub const ARCHIVE_FILE: &str = "archive.json";

/// The file the human-readable blind-spot map is written to.
pub const REPORT_FILE: &str = "FOUNDRY_REPORT.md";

/// The subdirectory each candidate's throwaway run goes into.
pub const WORK_DIR: &str = "_work";

/// A scenario overlay: dotted paths to the values that replace the base scenario's.
///
/// The same shape as a sweep cell's values ([`crate::plan::CellKey::values`]), and applied
/// by the same function ([`crate::path::set_path`]), which is what makes an elite replayable
/// as an ordinary scenario file.
pub type Genome = BTreeMap<String, Value>;

/// The prefix every legacy attacker id carries.
const ATTACKER_PREFIX: &str = "threat/attacker/legacy/";

// ---------------------------------------------------------------------------
// Descriptor axes
// ---------------------------------------------------------------------------

/// The density bands, and the demand rates that separate them, veh/h.
///
/// The edges are this engine's own, not the legacy engine's: the legacy bands were over a
/// *realised* vehicle count (60 and 120 vehicles) and this axis is over an offered rate,
/// so the numbers cannot be carried across. They are chosen as the order-of-magnitude
/// steps `scenarios/density-sweep-congestion.yaml` measured on the Manhattan import — 6,000
/// veh/h is where the channel busy ratio becomes visible and 24,000 is where the
/// verification queue does — and they are a **design choice**, recorded here rather than
/// cited to anything.
pub const DENSITY_BANDS: [&str; 3] = ["sparse", "medium", "dense"];

/// The two demand rates that separate [`DENSITY_BANDS`], veh/h.
pub const DENSITY_EDGES: [f64; 2] = [6000.0, 24000.0];

/// The attacker bands, and the declared fractions that separate them.
///
/// The edges are the legacy foundry's, unchanged: `attacker_pct` below 0.15, below 0.35,
/// and the rest. They transfer because both engines mean the same thing by an attacker
/// fraction.
pub const ATTACKER_BANDS: [&str; 3] = ["low", "med", "high"];

/// The two fractions that separate [`ATTACKER_BANDS`].
pub const ATTACKER_EDGES: [f64; 2] = [0.15, 0.35];

/// The topology bins: the world-source variants a scenario can name.
///
/// Derived from [`v2xw_world::WorldSourceSpec::label`]'s prefix rather than from a match on
/// the enumeration, so a source added upstream lands in `other` instead of failing to
/// compile — which is the right trade for a descriptor axis.
pub const TOPOLOGY_BINS: [&str; 4] = ["procedural", "osm-xml", "osm-bbox", "other"];

/// Every attack family that can appear on the family axis, plus `mixed` and `none`.
#[must_use]
pub fn family_bins() -> Vec<String> {
    // Derived from `AttackKind::ALL` rather than written out, so a kind added to
    // `v2xw-threat` widens the axis without an edit here.
    let mut families: Vec<String> = AttackKind::ALL
        .iter()
        .map(|k| k.family().as_str().to_string())
        .collect();
    families.sort();
    families.dedup();
    families.push("mixed".to_string());
    families.push("none".to_string());
    families
}

/// One archive cell: the six axes 07-threats §2.3 asks for.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Descriptor {
    /// The attacker family, or `mixed` when the scenario spans several, or `none`.
    pub attack_family: String,
    /// The density band, from the declared demand rate. See the module header, divergence 2.
    pub density_band: String,
    /// The world source's kind.
    pub topology: String,
    /// The attacker band, from the declared fraction.
    pub attacker_band: String,
    /// The radio access technology the scenario names.
    pub rat: String,
    /// The credential protocol, or `none`.
    pub protocol: String,
}

impl Descriptor {
    /// The cell's key: the six axes joined, which is how the archive is indexed and how
    /// `archive.json` names a cell.
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.attack_family,
            self.density_band,
            self.topology,
            self.attacker_band,
            self.rat,
            self.protocol
        )
    }

    /// The descriptor of a materialised scenario.
    ///
    /// Read from the **scenario**, not from the genome: a genome that does not mention an
    /// axis still has to land in the right cell, and the scenario is where the base
    /// value and the overlay have already been combined.
    #[must_use]
    pub fn of(scenario: &Scenario) -> Descriptor {
        Descriptor {
            attack_family: family_of(scenario),
            density_band: band(
                scenario
                    .actors
                    .vehicles
                    .demand
                    .rate_veh_per_h
                    .unwrap_or(0.0),
                DENSITY_EDGES,
                DENSITY_BANDS,
            ),
            topology: topology_of(scenario),
            attacker_band: band(
                scenario
                    .threats
                    .attackers
                    .first()
                    .and_then(|a| a.fraction)
                    .unwrap_or(0.0),
                ATTACKER_EDGES,
                ATTACKER_BANDS,
            ),
            rat: rat_of(scenario),
            protocol: scenario
                .actors
                .backend
                .protocol
                .clone()
                .unwrap_or_else(|| "none".to_string()),
        }
    }
}

/// Which band `value` falls in, given two ascending edges and three labels.
fn band(value: f64, edges: [f64; 2], labels: [&str; 3]) -> String {
    if value < edges[0] {
        labels[0].to_string()
    } else if value < edges[1] {
        labels[1].to_string()
    } else {
        labels[2].to_string()
    }
}

/// The attacker family the scenario's attackers span.
fn family_of(scenario: &Scenario) -> String {
    let mut families: Vec<&'static str> = scenario
        .threats
        .attackers
        .iter()
        .filter_map(|a| a.id.strip_prefix(ATTACKER_PREFIX))
        .filter_map(AttackKind::parse)
        .map(|k| k.family().as_str())
        .collect();
    families.sort_unstable();
    families.dedup();
    match families.len() {
        0 => "none".to_string(),
        1 => families[0].to_string(),
        _ => "mixed".to_string(),
    }
}

/// The topology bin, from the world source's label prefix.
fn topology_of(scenario: &Scenario) -> String {
    let label = scenario.world.source.label();
    let kind = label.split(':').next().unwrap_or("other");
    if TOPOLOGY_BINS.contains(&kind) {
        kind.to_string()
    } else {
        "other".to_string()
    }
}

/// The radio access technology, by its serde spelling.
fn rat_of(scenario: &Scenario) -> String {
    serde_json::to_value(scenario.radio.rat)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "unknown".to_string())
}

/// How many cells the descriptor space has: the coverage denominator.
///
/// The product of the six axes' bin counts. The `rat` axis has four values
/// (`v2xw_engine::scenario::Rat`) and the `protocol` axis two (`protocol/scms/camp` and
/// `none`), which is what this build can express rather than what a deployment could.
#[must_use]
pub fn grid_size() -> usize {
    family_bins().len() * DENSITY_BANDS.len() * TOPOLOGY_BINS.len() * ATTACKER_BANDS.len() * 4 * 2
}

// ---------------------------------------------------------------------------
// Objective and fitness
// ---------------------------------------------------------------------------

/// What the search maximises. Higher is a **more evasive** scenario, and still valid.
///
/// Not `Copy`: [`Objective::TargetFamily`] carries the family's name, so the enumeration
/// owns a `String`. Clone it where a copy is needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Objective {
    /// `1 − det_recall` at `level=vehicle`: the authority missed more of the attackers.
    Evade,
    /// `min(1, median(time_to_detect) / duration_s)`: slow to catch.
    Latency,
    /// [`Objective::Evade`], gated to candidates whose attacker family is this one.
    ///
    /// See the module header, divergence 1: this is a restriction of the *population*, not
    /// a slice of the metric, because `det_recall` carries no family dimension.
    TargetFamily {
        /// The family, by its `AttackFamily::as_str` spelling.
        family: String,
    },
}

impl Objective {
    /// The objective named `name`: `evade`, `latency`, or `family:<F>`.
    ///
    /// # Errors
    /// [`ExperimentError::BadObjective`] for an unknown name, or for a `family:<F>` whose
    /// family no attack kind belongs to — which is a typo, not a legitimately empty
    /// family, and is refused **before** the budget is spent rather than after.
    pub fn parse(name: &str) -> Result<Objective> {
        match name {
            "evade" => Ok(Objective::Evade),
            "latency" => Ok(Objective::Latency),
            other => match other.strip_prefix("family:") {
                Some(family) => {
                    let known: Vec<&'static str> = AttackKind::ALL
                        .iter()
                        .map(|k| k.family().as_str())
                        .collect();
                    if known.contains(&family) {
                        Ok(Objective::TargetFamily {
                            family: family.to_string(),
                        })
                    } else {
                        Err(ExperimentError::BadObjective {
                            objective: other.to_string(),
                            problem: format!(
                                "no attack kind belongs to family {family:?}; the families \
                                 are {}",
                                {
                                    let mut k = known;
                                    k.sort_unstable();
                                    k.dedup();
                                    k.join(", ")
                                }
                            ),
                        })
                    }
                }
                None => Err(ExperimentError::BadObjective {
                    objective: other.to_string(),
                    problem: "want evade, latency or family:<F>".to_string(),
                }),
            },
        }
    }

    /// The objective's name, as [`Objective::parse`] accepts it.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Objective::Evade => "evade".to_string(),
            Objective::Latency => "latency".to_string(),
            Objective::TargetFamily { family } => format!("family:{family}"),
        }
    }

    /// The formula, for the archive document and the report.
    ///
    /// Not `const`, although every arm returns a literal: `Objective` owns a `String` in
    /// one variant, and whether a `const fn` may take `&self` on such a type is a rule
    /// this file should not be depending on for no gain.
    #[must_use]
    pub fn formula(&self) -> &'static str {
        match self {
            Objective::Evade => "1 - det_recall[level=vehicle]",
            Objective::Latency => "min(1, mean(time_to_detect) / time.duration_s)",
            Objective::TargetFamily { .. } => {
                "1 - det_recall[level=vehicle], gated to the target family"
            }
        }
    }
}

/// What a candidate's metrics said about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Validity {
    /// A real misbehaviour scenario: attackers existed and the reporting pipeline fired.
    Valid,
    /// No true attacker was in the population.
    NoAttackers,
    /// Attackers existed and no report about anybody reached the authority, so the run
    /// says nothing about whether the detector *missed* them.
    NoReports,
    /// The objective needs a metric this run did not produce.
    MetricMissing,
    /// The candidate's attacker family is not the objective's target.
    WrongFamily,
}

impl Validity {
    /// True only for [`Validity::Valid`].
    #[must_use]
    pub const fn is_valid(self) -> bool {
        matches!(self, Validity::Valid)
    }

    /// Why the candidate was gated out, for the report.
    #[must_use]
    pub const fn because(self) -> &'static str {
        match self {
            Validity::Valid => "archived",
            Validity::NoAttackers => "no true attacker in the population",
            Validity::NoReports => "attackers present and no report ever filed",
            Validity::MetricMissing => "the objective's metric was not produced",
            Validity::WrongFamily => "not the objective's target family",
        }
    }
}

/// The metrics the gate and the objective read, pulled out of one run's reduced table.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Signals {
    /// True attackers in the population: `det_tp` + `det_fn` at `level=vehicle`.
    pub attackers: u64,
    /// Subjects any report reached the authority about: `det_tp` + `det_fp` at
    /// `level=report`.
    pub reported_subjects: u64,
    /// `det_recall` at `level=vehicle`, if the run produced it.
    pub recall_vehicle: Option<f64>,
    /// `det_recall` at `level=report`, if the run produced it.
    pub recall_report: Option<f64>,
    /// The pooled mean of `time_to_detect`, seconds, if any attacker was detected.
    pub time_to_detect_s: Option<f64>,
}

impl Signals {
    /// Reads the signals out of a run's reduced metrics.
    ///
    /// The keys are [`v2xw_metrics::MetricSample::key`]'s spelling: the metric name, then
    /// the dimension values in `Dim` declaration order, which puts `level` before `cell`.
    #[must_use]
    pub fn read(metrics: &BTreeMap<String, RunMetric>) -> Signals {
        let count = |key: &str| -> u64 {
            match metrics.get(key).map(|m| &m.value) {
                Some(RunValue::Count { count }) => *count,
                // A count metric that reduced to something else is a schema surprise, not
                // a zero; but the gate's job is to be conservative, so it reads as absent.
                _ => 0,
            }
        };
        let point = |key: &str| -> Option<f64> { metrics.get(key).and_then(|m| m.value.point()) };
        Signals {
            attackers: count("det_tp|level=vehicle|cell=tp")
                + count("det_fn|level=vehicle|cell=fn"),
            reported_subjects: count("det_tp|level=report|cell=tp")
                + count("det_fp|level=report|cell=fp"),
            recall_vehicle: point("det_recall|level=vehicle"),
            recall_report: point("det_recall|level=report"),
            time_to_detect_s: point("time_to_detect"),
        }
    }
}

/// Scores one candidate: `(fitness, validity)`.
///
/// Fitness is zero for anything the gate rejects, and a rejected candidate is never
/// archived — so a zero in the archive means "measured as perfectly detected", and a
/// rejected candidate is absent rather than sitting at the bottom of a cell.
#[must_use]
pub fn fitness(
    objective: &Objective,
    signals: &Signals,
    descriptor: &Descriptor,
    duration_s: f64,
) -> (f64, Validity) {
    if signals.attackers == 0 {
        return (0.0, Validity::NoAttackers);
    }
    if signals.reported_subjects == 0 {
        return (0.0, Validity::NoReports);
    }
    match objective {
        Objective::Evade => match signals.recall_vehicle {
            Some(recall) => (clamp01(1.0 - recall), Validity::Valid),
            None => (0.0, Validity::MetricMissing),
        },
        Objective::TargetFamily { family } => {
            if &descriptor.attack_family != family {
                return (0.0, Validity::WrongFamily);
            }
            match signals.recall_vehicle {
                Some(recall) => (clamp01(1.0 - recall), Validity::Valid),
                None => (0.0, Validity::MetricMissing),
            }
        }
        Objective::Latency => match signals.time_to_detect_s {
            // No sample means nobody was caught, so there is no latency to measure and
            // the candidate is gated out rather than scored 0 — which would be
            // indistinguishable from "caught instantly".
            None => (0.0, Validity::MetricMissing),
            Some(t) => {
                let horizon = if duration_s.is_finite() && duration_s > 0.0 {
                    duration_s
                } else {
                    60.0
                };
                (clamp01(t / horizon), Validity::Valid)
            }
        },
    }
}

/// `x` clamped to `[0, 1]`, with a non-finite value read as zero.
fn clamp01(x: f64) -> f64 {
    if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// The archive
// ---------------------------------------------------------------------------

/// One cell's elite: the most evasive valid scenario found in that cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Elite {
    /// Which cell.
    pub descriptor: Descriptor,
    /// Its fitness, quantised onto [`Q_FITNESS`].
    pub fitness: f64,
    /// The candidate's index in the search, which is what its seed was derived from.
    pub candidate: u64,
    /// The engine seed it ran under, as a scenario writes one.
    pub seed_hex: String,
    /// The scenario hash the run reported, so the elite can be checked against a replay.
    pub scenario_hash: String,
    /// The overlay that produced it: the compact reproducer.
    pub genome: Genome,
    /// The signals the gate and the objective read.
    pub signals: Signals,
}

/// The grid every fitness and every reported ratio in the archive sits on.
///
/// 1e-6, the finest grid build decision D9 lists, matching the results table's own float
/// columns. `archive.json` is an exported artefact, so every float in it is quantised at
/// the writer.
pub const Q_FITNESS: f64 = v2xw_record::grid::Q_METRIC_VALUE;

/// The MAP-Elites archive: one elite per descriptor cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Archive {
    /// The elites, keyed by [`Descriptor::key`] and therefore in a fixed order.
    pub cells: BTreeMap<String, Elite>,
    /// How many candidates were evaluated.
    pub evaluated: u64,
    /// How many were gated out, by reason.
    pub gated: BTreeMap<String, u64>,
    /// How many failed to run at all, with the first failure's message.
    pub failures: u64,
    /// The first failure the search survived, so an operator can see what went wrong
    /// without reading a log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_failure: Option<String>,
}

impl Archive {
    /// An empty archive.
    #[must_use]
    pub fn new() -> Archive {
        Archive::default()
    }

    /// Keeps `candidate` if its cell is empty or it strictly beats the incumbent.
    ///
    /// Strictly: a tie keeps the earlier find, which is what makes the archive a function
    /// of the search order and therefore reproducible.
    pub fn insert_if_better(&mut self, candidate: Elite) -> bool {
        let key = candidate.descriptor.key();
        match self.cells.get(&key) {
            Some(incumbent) if incumbent.fitness >= candidate.fitness => false,
            _ => {
                self.cells.insert(key, candidate);
                true
            }
        }
    }

    /// Records a gated-out candidate.
    pub fn record_gated(&mut self, validity: Validity) {
        *self
            .gated
            .entry(validity.because().to_string())
            .or_insert(0) += 1;
    }

    /// How many cells are filled.
    #[must_use]
    pub fn coverage(&self) -> usize {
        self.cells.len()
    }

    /// The coverage as a percentage of [`grid_size`], quantised.
    #[must_use]
    pub fn coverage_pct(&self) -> f64 {
        let size = grid_size();
        if size == 0 {
            return 0.0;
        }
        quantise(100.0 * self.cells.len() as f64 / size as f64)
    }

    /// The quality-diversity score: the sum of the elites' fitnesses.
    ///
    /// Summed with `v2xw_core::math::sum_ordered` over the cell-key order, so the score is
    /// a function of the archive and not of the order the cells were filled in.
    #[must_use]
    pub fn qd_score(&self) -> f64 {
        quantise(v2xw_core::math::sum_ordered(
            self.cells.values().map(|e| e.fitness),
        ))
    }

    /// The hardest elite, and its cell.
    #[must_use]
    pub fn hardest(&self) -> Option<&Elite> {
        // `max_by` over a `BTreeMap`'s values takes the *last* maximum; the cells are in
        // key order, so a tie resolves to the lexicographically greatest cell, which is
        // deterministic. Reversing the iterator would pick the smallest; either is fine
        // as long as it is fixed, and this one is stated.
        self.cells
            .values()
            .max_by(|a, b| a.fitness.total_cmp(&b.fitness))
    }

    /// The compact snapshot a mutation operator is handed.
    ///
    /// Pure data: it carries no knowledge of how an operator uses it, which is what lets
    /// the same structure serve the built-in operator, a language-model operator and a
    /// graphical one.
    #[must_use]
    pub fn summary(&self, max_cells: usize) -> ArchiveSummary {
        let mut hardest: Vec<&Elite> = self.cells.values().collect();
        // Highest fitness first, then by cell key, so the list is a function of the
        // archive rather than of the map's iteration accident.
        hardest.sort_by(|a, b| {
            b.fitness
                .total_cmp(&a.fitness)
                .then_with(|| a.descriptor.key().cmp(&b.descriptor.key()))
        });
        ArchiveSummary {
            axes: axes_doc(),
            grid_size: grid_size(),
            coverage_cells: self.cells.len(),
            filled_cells: self
                .cells
                .values()
                .take(max_cells)
                .map(|e| e.descriptor.clone())
                .collect(),
            hardest_cells: hardest
                .into_iter()
                .take(max_cells)
                .map(|e| (e.descriptor.clone(), e.fitness))
                .collect(),
            families_filled: {
                let mut f: Vec<String> = self
                    .cells
                    .values()
                    .map(|e| e.descriptor.attack_family.clone())
                    .collect();
                f.sort();
                f.dedup();
                f
            },
            families_empty: {
                let filled: Vec<String> = self
                    .cells
                    .values()
                    .map(|e| e.descriptor.attack_family.clone())
                    .collect();
                family_bins()
                    .into_iter()
                    .filter(|f| !filled.contains(f))
                    .collect()
            },
        }
    }

    /// Writes `archive.json` into `dir`.
    ///
    /// # Errors
    /// [`ExperimentError::Io`] if the file cannot be written and [`ExperimentError::Json`]
    /// if the document will not serialise.
    pub fn write(&self, dir: &Path, objective: &Objective, options: &FoundryOptions) -> Result<()> {
        let document = json!({
            "schema": ARCHIVE_SCHEMA,
            "objective": objective.label(),
            "fitness": objective.formula(),
            "validity_gate": "det_tp+det_fn at level=vehicle > 0 AND det_tp+det_fp at \
                              level=report > 0",
            "seed": format!("0x{:016x}", options.seed),
            "budget": options.budget,
            "descriptor_axes": axes_doc(),
            "grid_size": grid_size(),
            "coverage_cells": self.coverage(),
            "coverage_pct": self.coverage_pct(),
            "qd_score": self.qd_score(),
            "quantum": Q_FITNESS,
            "evaluated": self.evaluated,
            "gated": self.gated,
            "failures": self.failures,
            "first_failure": self.first_failure,
            "cells": self.cells,
        });
        let path = dir.join(ARCHIVE_FILE);
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|e| ExperimentError::json("the foundry archive", e))?;
        std::fs::write(&path, &bytes)
            .map_err(|e| ExperimentError::io("cannot write the foundry archive", &path, e))?;
        Ok(())
    }
}

/// The descriptor axes, as a document for the archive and the operator summary.
#[must_use]
pub fn axes_doc() -> Value {
    json!({
        "attack_family": {
            "bins": family_bins(),
            "from": "threats.attackers[].id, mapped through AttackKind::family; `mixed` \
                     when the scenario spans several and `none` when it has no attacker",
        },
        "density_band": {
            "bins": DENSITY_BANDS,
            "from": "actors.vehicles.demand.rate_veh_per_h, DECLARED and not realised",
            "edges": DENSITY_EDGES,
            "caveat": "the legacy descriptor binned the realised vehicle count; no \
                       run-level count metric exists, so this axis is weaker",
        },
        "topology": {
            "bins": TOPOLOGY_BINS,
            "from": "world.source's kind, from WorldSourceSpec::label",
        },
        "attacker_band": {
            "bins": ATTACKER_BANDS,
            "from": "threats.attackers[0].fraction",
            "edges": ATTACKER_EDGES,
        },
        "rat": {"bins": ["dsrc-80211p", "lte-v2x-pc5", "nr-v2x-pc5", "hybrid"], "from": "radio.rat"},
        "protocol": {"bins": ["protocol/scms/camp", "none"], "from": "actors.backend.protocol"},
    })
}

/// What an operator is told about the archive's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveSummary {
    /// The axes and their bins.
    pub axes: Value,
    /// The coverage denominator.
    pub grid_size: usize,
    /// How many cells are filled.
    pub coverage_cells: usize,
    /// A sample of the filled cells.
    pub filled_cells: Vec<Descriptor>,
    /// The hardest cells, highest fitness first — the failure clusters to intensify.
    pub hardest_cells: Vec<(Descriptor, f64)>,
    /// The attack families with at least one elite.
    pub families_filled: Vec<String>,
    /// The attack families with none — the coverage gaps to fill.
    pub families_empty: Vec<String>,
}

// ---------------------------------------------------------------------------
// Mutation
// ---------------------------------------------------------------------------

/// What proposes the next candidate.
///
/// The **only** pluggable component of the search: parent selection, validation, scoring
/// and the insert-if-better rule are identical whatever the operator, which is what makes
/// [`crate::foundry_eval`]'s head-to-head a measurement of the operator and of nothing
/// else.
pub trait MutationOperator {
    /// The operator's name, for the report.
    fn name(&self) -> &str;

    /// Proposes a child of `parent`, given what the archive looks like.
    ///
    /// Returning `None` — or a genome the feasibility oracle refuses — falls back to
    /// [`RandomMutation`], so a misbehaving operator cannot kill an expensive search.
    fn propose(
        &self,
        parent: &Genome,
        summary: &ArchiveSummary,
        rng: &mut RngStream,
    ) -> Option<Genome>;
}

/// The built-in operator: one weighted-random perturbation of one knob.
///
/// Deterministic given its stream, and **structurally incapable of weakening the
/// detector**: every path it writes is in [`RandomMutation::TOUCHES`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RandomMutation;

impl RandomMutation {
    /// Every scenario path any built-in operator writes.
    ///
    /// This list is also the *filter* applied to an injected operator's output
    /// ([`sanitise`]), so the guarantee "the search never weakens the detector" holds for
    /// an operator this crate did not write. Note what is absent: `detection.*`,
    /// `security.verification_policy`, `nodes.*` and every metric or exporter key.
    ///
    /// `radio.tiers.*` is absent too, for a different reason. A tier is the *fidelity of
    /// the measurement*, not a property of the scenario: two elites measured at different
    /// tiers are not comparable, so mutating one would make the archive's fitnesses mean
    /// different things in different cells. A tier study is a sweep
    /// (`scenarios/pdr-vs-distance.yaml`), not a search.
    ///
    /// Every entry here must be reachable from some branch of
    /// [`MutationOperator::propose`], which
    /// `the_built_in_operator_only_writes_paths_it_declares` checks in both directions.
    pub const TOUCHES: &'static [&'static str] = &[
        "threats.attackers[0].fraction",
        "threats.attackers[0].id",
        "threats.attackers[0].params",
        "threats.attackers[0].schedule.from_s",
        "actors.vehicles.demand.rate_veh_per_h",
        "security.pseudonym_change.period_s",
        "weather.initial",
        "weather.intensity",
    ];

    /// The operators and their weights.
    ///
    /// Cell-*defining* operators — the attacker family, the density band, the attacker
    /// band — are weighted higher, so a modest budget spreads across descriptor cells
    /// (coverage) before it tunes difficulty within one (depth). The weights are the
    /// legacy foundry's shape, renumbered for this operator set.
    const WEIGHTS: [f64; 7] = [5.0, 3.0, 3.0, 2.0, 2.0, 1.0, 1.0];
}

impl MutationOperator for RandomMutation {
    fn name(&self) -> &str {
        "random"
    }

    fn propose(
        &self,
        parent: &Genome,
        _summary: &ArchiveSummary,
        rng: &mut RngStream,
    ) -> Option<Genome> {
        let mut child = parent.clone();
        match rng.choose_index(&RandomMutation::WEIGHTS) {
            // 0 — the attack family: pick a kind, which moves the family axis. The main
            // coverage driver.
            0 => {
                let index = rng.below(AttackKind::ALL.len() as u64) as usize;
                let kind = AttackKind::ALL[index];
                child.insert(
                    "threats.attackers[0].id".to_string(),
                    json!(format!("{ATTACKER_PREFIX}{}", kind.as_str())),
                );
            }
            // 1 — the attacker fraction, which is both a band and a difficulty knob.
            1 => {
                let current = number(parent, "threats.attackers[0].fraction", 0.05);
                let next = quantise3((current + rng.uniform(-0.15, 0.20)).clamp(0.01, 0.60));
                child.insert("threats.attackers[0].fraction".to_string(), json!(next));
            }
            // 2 — the demand rate, which is the density band.
            2 => {
                let current = number(parent, "actors.vehicles.demand.rate_veh_per_h", 6000.0);
                // Multiplicative, because the band edges are an order of magnitude apart
                // and an additive step would never cross both of them.
                let next = quantise3((current * rng.uniform(0.4, 3.0)).clamp(300.0, 120_000.0));
                child.insert(
                    "actors.vehicles.demand.rate_veh_per_h".to_string(),
                    json!(next),
                );
            }
            // 3 — the falsification magnitude, through the attacker's own params. Read by
            // `v2xw_engine::phase2` as `params.intensity`.
            3 => {
                // Floored at 0.3 so an "attacker" that never actually falsifies cannot be
                // a way to game the recall — the legacy foundry's own note.
                let next = quantise3(rng.uniform(0.3, 3.0));
                child.insert(
                    "threats.attackers[0].params".to_string(),
                    json!({"intensity": next}),
                );
            }
            // 4 — when the attack starts. A later onset leaves less of the run for the
            // detector, which is difficulty within a cell.
            4 => {
                let current = number(parent, "threats.attackers[0].schedule.from_s", 30.0);
                let next = quantise3((current + rng.uniform(-30.0, 60.0)).max(1.0));
                child.insert(
                    "threats.attackers[0].schedule.from_s".to_string(),
                    json!(next),
                );
            }
            // 5 — the weather, which is benign difficulty: it changes how the honest fleet
            // drives and therefore how much natural inconsistency the detectors see.
            5 => {
                let index = rng.below(v2xw_core::weather::WeatherKind::ALL.len() as u64) as usize;
                let kind = v2xw_core::weather::WeatherKind::ALL[index];
                let name = serde_json::to_value(kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| "clear".to_string());
                child.insert("weather.initial".to_string(), json!(name));
                child.insert(
                    "weather.intensity".to_string(),
                    json!(quantise3(rng.uniform(0.0, 1.0))),
                );
            }
            // 6 — the pseudonym period, which is evasion through unlinkability: every
            // detector keys its history on the signer digest, so a short period erases it.
            _ => {
                let current = number(parent, "security.pseudonym_change.period_s", 60.0);
                let next = quantise3((current * rng.uniform(0.3, 3.0)).clamp(5.0, 900.0));
                child.insert(
                    "security.pseudonym_change.period_s".to_string(),
                    json!(next),
                );
            }
        }
        Some(child)
    }
}

/// A genome's numeric value at `path`, or `fallback`.
fn number(genome: &Genome, path: &str, fallback: f64) -> f64 {
    genome
        .get(path)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
        .unwrap_or(fallback)
}

/// Three decimals: the legacy convention, and coarse enough that two genomes that differ
/// only in floating-point noise are the same genome.
fn quantise3(x: f64) -> f64 {
    v2xw_core::math::quantize_to(x, 1e-3)
}

/// A float on the archive's declared grid.
fn quantise(x: f64) -> f64 {
    v2xw_core::math::quantize_to(x, Q_FITNESS)
}

/// Drops every path an operator is not allowed to write.
///
/// Applied to **every** proposed genome, the built-in operator's included, so the
/// guarantee holds for an injected operator whatever it returns. Returns the kept genome
/// and the paths that were refused, so a caller can report an operator that keeps trying.
#[must_use]
pub fn sanitise(genome: &Genome) -> (Genome, Vec<String>) {
    let mut kept = Genome::new();
    let mut refused = Vec::new();
    for (path, value) in genome {
        if RandomMutation::TOUCHES.contains(&path.as_str()) {
            kept.insert(path.clone(), value.clone());
        } else {
            refused.push(path.clone());
        }
    }
    (kept, refused)
}

// ---------------------------------------------------------------------------
// The search driver
// ---------------------------------------------------------------------------

/// How a foundry search is run.
#[derive(Debug, Clone)]
pub struct FoundryOptions {
    /// Where `archive.json` and the report go.
    pub out: PathBuf,
    /// How many mutation-and-evaluation iterations after the base genomes.
    pub budget: u64,
    /// The objective.
    pub objective: Objective,
    /// The master seed every candidate's seed is derived from. Defaults to the base
    /// scenario's own seed, which makes the search reproducible from the scenario alone.
    pub seed: u64,
    /// The genomes the search starts from.
    pub base_genomes: Vec<Genome>,
    /// How many retries a mutation gets before the search falls back to the parent.
    pub max_tries: u32,
    /// How many cells an operator's summary carries.
    pub summary_cells: usize,
    /// Keep each candidate's run directory instead of deleting it. Off by default: a
    /// sixty-candidate search keeps sixty recordings otherwise.
    pub keep_work: bool,
}

impl FoundryOptions {
    /// The defaults: sixty candidates, the `evade` objective, the built-in base genomes.
    #[must_use]
    pub fn new(out: impl Into<PathBuf>) -> FoundryOptions {
        FoundryOptions {
            out: out.into(),
            budget: 60,
            objective: Objective::Evade,
            seed: 0,
            base_genomes: default_base_genomes(),
            max_tries: 24,
            summary_cells: 24,
            keep_work: false,
        }
    }

    /// The same options with another budget.
    #[must_use]
    pub fn budget(mut self, budget: u64) -> Self {
        self.budget = budget;
        self
    }

    /// The same options with another objective.
    #[must_use]
    pub fn objective(mut self, objective: Objective) -> Self {
        self.objective = objective;
        self
    }

    /// The same options with an explicit master seed.
    #[must_use]
    pub fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// The coverage denominator, for a caller that wants it without importing the free
    /// function.
    #[must_use]
    pub fn grid_size(&self) -> usize {
        grid_size()
    }
}

/// The genomes a search starts from.
///
/// Three, as the legacy foundry had three, and for the same reason: one base genome makes
/// the first few iterations explore one corner. These three differ in the axis that drives
/// coverage hardest — the attack family — and in the density band, so the initial archive
/// spans more than one cell before the mutation operator is asked for anything.
///
/// Every path they set is one [`RandomMutation::TOUCHES`] lists, so a base genome cannot
/// weaken the detector either.
#[must_use]
pub fn default_base_genomes() -> Vec<Genome> {
    let mut out = Vec::new();
    for (kind, rate, fraction) in [
        (AttackKind::ConstPos, 1500.0, 0.05),
        (AttackKind::Sybil, 6000.0, 0.10),
        (AttackKind::SlowDrift, 24000.0, 0.25),
    ] {
        let mut genome = Genome::new();
        genome.insert(
            "threats.attackers[0].id".to_string(),
            json!(format!("{ATTACKER_PREFIX}{}", kind.as_str())),
        );
        genome.insert(
            "actors.vehicles.demand.rate_veh_per_h".to_string(),
            json!(rate),
        );
        genome.insert("threats.attackers[0].fraction".to_string(), json!(fraction));
        out.push(genome);
    }
    out
}

/// The seed one candidate runs under.
///
/// `SHA-256(domain ‖ master_le ‖ candidate_le)`, first eight bytes little-endian. Derived
/// and never sampled, so a candidate rerun on its own reproduces — which is what makes
/// "elite replay is a scenario file plus a seed" true.
#[must_use]
pub fn derive_candidate_seed(master_seed: u64, candidate: u64) -> u64 {
    let mut material = Vec::with_capacity(SEED_DOMAIN.len() + 16);
    material.extend_from_slice(SEED_DOMAIN);
    material.extend_from_slice(&master_seed.to_le_bytes());
    material.extend_from_slice(&candidate.to_le_bytes());
    let digest = v2xw_core::hash::sha256(&material);
    let head: [u8; 8] = digest[0..8]
        .try_into()
        .expect("a SHA-256 digest is 32 bytes, so its first 8 are an 8-byte array");
    u64::from_le_bytes(head)
}

/// The scenario a genome stands for.
///
/// Three edits, the same three [`crate::plan::ExperimentPlan::materialise`] makes and for
/// the same reasons: the overlay is applied, the seed is set, and `experiment` is removed
/// so the run's scenario hash is the hash of the ordinary scenario it is. `meta.name`
/// gains the candidate id, so a manifest names the candidate it came from.
///
/// # Errors
/// [`ExperimentError::BadSweepPath`] if a genome path is not writable in the document,
/// [`ExperimentError::Engine`] if the result is not a valid scenario, and
/// [`ExperimentError::Json`] if the base will not serialise.
pub fn materialise(
    base: &Scenario,
    genome: &Genome,
    seed: u64,
    candidate: u64,
) -> Result<Scenario> {
    let mut document =
        serde_json::to_value(base).map_err(|e| ExperimentError::json("the scenario", e))?;
    for (path, value) in genome {
        set_path(&mut document, path, value.clone())?;
    }
    let mut scenario = Scenario::from_document(document)?;
    scenario.seed = seed;
    scenario.experiment = None;
    scenario.meta.name = format!("{}-foundry-c{candidate:05}", base.meta.name);
    scenario
        .validate()
        .map_err(v2xw_engine::EngineError::from)?;
    Ok(scenario)
}

/// The cheap feasibility oracle: can this genome be a scenario at all?
///
/// The same call [`materialise`] makes, without the run. A mutation that produces an
/// impossible knob combination — an attacker schedule outside the run, a tier pair the
/// validator refuses — is caught here for the price of a serialisation rather than for the
/// price of a simulation.
#[must_use]
pub fn is_feasible(base: &Scenario, genome: &Genome) -> bool {
    materialise(base, genome, 1, 0).is_ok()
}

/// One perturbation of `parent` that is guaranteed feasible.
///
/// Asks `operator` for a child, sanitises it, and checks it against the oracle; on an
/// infeasible or refused result it retries up to `max_tries` and finally falls back to the
/// built-in operator and then to the parent itself. So the return value is always a
/// scenario the engine will accept, and a badly behaved operator costs retries rather than
/// the search.
#[must_use]
pub fn mutate<M: MutationOperator + ?Sized>(
    base: &Scenario,
    parent: &Genome,
    operator: &M,
    summary: &ArchiveSummary,
    rng: &mut RngStream,
    max_tries: u32,
) -> Genome {
    for attempt in 0..max_tries.max(1) {
        // After half the budget of retries, stop asking the injected operator and use the
        // built-in one: an operator that has produced nothing feasible in a dozen tries is
        // not going to, and the search should not stall on it.
        let proposal = if attempt * 2 < max_tries {
            operator.propose(parent, summary, rng)
        } else {
            RandomMutation.propose(parent, summary, rng)
        };
        let Some(child) = proposal else { continue };
        let (child, _refused) = sanitise(&child);
        if child != *parent && is_feasible(base, &child) {
            return child;
        }
    }
    parent.clone()
}

/// Runs the MAP-Elites loop and writes `archive.json` and `FOUNDRY_REPORT.md`.
///
/// The base genomes are evaluated first, then `budget` iterations each pick a parent — a
/// base genome while the archive is thin, a random elite thereafter — mutate it, run it,
/// score it and insert it if it beats its cell.
///
/// A candidate that **fails to run** is isolated: the failure is counted, the first
/// message is kept, and the search continues. A candidate that runs and is gated out is
/// counted separately, because "the engine refused it" and "it was not a misbehaviour
/// scenario" are different findings.
///
/// # Errors
/// [`ExperimentError::Io`] if the output directory cannot be written. Everything else is
/// survived and counted.
pub fn search<E: RunExecutor + ?Sized, M: MutationOperator + ?Sized>(
    base: &Scenario,
    executor: &E,
    operator: &M,
    options: &FoundryOptions,
) -> Result<Archive> {
    std::fs::create_dir_all(&options.out)
        .map_err(|e| ExperimentError::io("cannot create the foundry directory", &options.out, e))?;
    let work_root = options.out.join(WORK_DIR);
    std::fs::create_dir_all(&work_root).map_err(|e| {
        ExperimentError::io("cannot create the foundry work directory", &work_root, e)
    })?;

    let master = if options.seed == 0 {
        base.seed
    } else {
        options.seed
    };
    let registry = RngRegistry::new(master);
    let domain = RngDomain::plugin(FOUNDRY_MODEL_ID);
    let mut archive = Archive::new();
    let mut counter: u64 = 0;

    // The base genomes.
    for genome in &options.base_genomes {
        counter += 1;
        evaluate_into(
            base,
            executor,
            options,
            &work_root,
            master,
            counter,
            genome,
            &mut archive,
        );
    }

    // The search. `base_genomes` may legitimately be empty — a caller that wants the
    // search to start from the base scenario alone passes none — so the parent when the
    // archive is still empty falls back to the empty overlay rather than indexing a vector
    // that has nothing in it.
    for iteration in 0..options.budget {
        counter += 1;
        // One ephemeral stream per iteration, keyed by the iteration index. Ephemeral
        // because the key already embeds the counter, so the stream is derived, used and
        // dropped rather than interned — and because a fresh key per iteration makes the
        // draws a function of the iteration index alone, not of how many candidates were
        // infeasible before it.
        let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, counter));
        let parent: Genome = if archive.cells.is_empty() {
            match options.base_genomes.len() {
                0 => Genome::new(),
                n => options.base_genomes[(iteration as usize) % n].clone(),
            }
        } else {
            // A random elite, chosen by index into the key-ordered cell list, so the
            // choice is a function of the stream and not of a hash order.
            let keys: Vec<&String> = archive.cells.keys().collect();
            let pick = rng.below(keys.len() as u64) as usize;
            archive.cells[keys[pick]].genome.clone()
        };
        let summary = archive.summary(options.summary_cells);
        let child = mutate(
            base,
            &parent,
            operator,
            &summary,
            &mut rng,
            options.max_tries,
        );
        evaluate_into(
            base,
            executor,
            options,
            &work_root,
            master,
            counter,
            &child,
            &mut archive,
        );
    }

    if !options.keep_work {
        // Best effort: a work directory that will not delete is not a reason to lose the
        // archive.
        let _ = std::fs::remove_dir_all(&work_root);
    }
    archive.write(&options.out, &options.objective, options)?;
    let report = render_report(&archive, &options.objective, options, operator.name());
    let path = options.out.join(REPORT_FILE);
    std::fs::write(&path, report.as_bytes())
        .map_err(|e| ExperimentError::io("cannot write the foundry report", &path, e))?;
    Ok(archive)
}

/// Evaluates one candidate and folds it into the archive, surviving any failure.
#[allow(clippy::too_many_arguments)]
fn evaluate_into<E: RunExecutor + ?Sized>(
    base: &Scenario,
    executor: &E,
    options: &FoundryOptions,
    work_root: &Path,
    master: u64,
    candidate: u64,
    genome: &Genome,
    archive: &mut Archive,
) {
    archive.evaluated += 1;
    match evaluate(
        base, executor, options, work_root, master, candidate, genome,
    ) {
        Ok((elite, validity)) => {
            if validity.is_valid() {
                archive.insert_if_better(elite);
            } else {
                archive.record_gated(validity);
            }
        }
        Err(e) => {
            archive.failures += 1;
            if archive.first_failure.is_none() {
                archive.first_failure = Some(format!("candidate {candidate}: {e}"));
            }
        }
    }
}

/// Materialises, runs, reduces and scores one candidate.
#[allow(clippy::too_many_arguments)]
fn evaluate<E: RunExecutor + ?Sized>(
    base: &Scenario,
    executor: &E,
    options: &FoundryOptions,
    work_root: &Path,
    master: u64,
    candidate: u64,
    genome: &Genome,
) -> Result<(Elite, Validity)> {
    let seed = derive_candidate_seed(master, candidate);
    let scenario = materialise(base, genome, seed, candidate)?;
    let dir = work_root.join(format!("c{candidate:05}"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| ExperimentError::io("cannot create a candidate directory", &dir, e))?;
    let yaml = scenario.to_yaml().map_err(v2xw_engine::EngineError::from)?;
    let scenario_path = dir.join("scenario.yaml");
    std::fs::write(&scenario_path, yaml.as_bytes()).map_err(|e| {
        ExperimentError::io("cannot write a candidate's scenario", &scenario_path, e)
    })?;

    // The executor seam is the runner's, unchanged: the foundry does not know how to run
    // the engine either, which is what lets a test drive a whole search without one.
    let planned = crate::plan::PlannedRun {
        run_id: format!("foundry-c{candidate:05}"),
        cell: crate::plan::CellKey {
            index: candidate as usize,
            values: genome.clone(),
        },
        seed_slot: 0,
        declared_seed: master,
        replication: 0,
        seed,
    };
    let artifacts = executor
        .execute(&planned, &scenario_path, &dir)
        .map_err(|message| ExperimentError::Run {
            run_id: planned.run_id.clone(),
            message,
        })?;

    let samples = read_samples(&artifacts.metrics_path)?;
    let metrics = reduce_run(&samples)?;
    let signals = Signals::read(&metrics);
    let descriptor = Descriptor::of(&scenario);
    let (score, validity) = fitness(
        &options.objective,
        &signals,
        &descriptor,
        scenario.time.duration_s,
    );
    if !options.keep_work {
        let _ = std::fs::remove_dir_all(&dir);
    }
    Ok((
        Elite {
            descriptor,
            fitness: quantise(score),
            candidate,
            seed_hex: format!("0x{seed:016x}"),
            scenario_hash: artifacts.scenario_hash,
            genome: genome.clone(),
            signals,
        },
        validity,
    ))
}

/// Renders the blind-spot map as Markdown.
#[must_use]
pub fn render_report(
    archive: &Archive,
    objective: &Objective,
    options: &FoundryOptions,
    operator: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# Foundry blind-spot map\n\n");
    out.push_str(&format!(
        "Detector-in-the-loop MAP-Elites archive for objective **{}** (`{}`), operator \
         **{operator}**, budget {}, master seed `0x{:016x}` (zero means the base \
         scenario's own seed was used).\n\n",
        objective.label(),
        objective.formula(),
        options.budget,
        options.seed
    ));
    out.push_str(
        "Every cell below is the **most evasive valid scenario found in that corner of \
         the space**, not the best overall: that is what makes the archive a map rather \
         than a single answer. The search perturbs only adversary, environment and \
         topology knobs — it never weakens the detector — so these are the blind spots of \
         the *shipped* detector.\n\n",
    );

    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- Coverage: **{} / {} cells ({} %)**\n",
        archive.coverage(),
        grid_size(),
        archive.coverage_pct()
    ));
    out.push_str(&format!(
        "- QD-score (sum of elite fitnesses): **{}**\n",
        archive.qd_score()
    ));
    out.push_str(&format!(
        "- Candidates evaluated: **{}**, of which {} failed to run\n",
        archive.evaluated, archive.failures
    ));
    if let Some(best) = archive.hardest() {
        out.push_str(&format!(
            "- Hardest elite: **{}** at fitness **{}**, seed `{}`\n",
            best.descriptor.key(),
            best.fitness,
            best.seed_hex
        ));
    }
    out.push('\n');

    if !archive.gated.is_empty() {
        out.push_str("## Candidates gated out\n\n");
        out.push_str(
            "A gated candidate ran and was **not** archived. The second row is the one \
             that matters: a scenario whose attackers nobody ever reported scores a \
             perfect evasion for the wrong reason, and archiving it would make the search \
             chase scenarios in which nobody looked.\n\n",
        );
        out.push_str("| Reason | Candidates |\n|---|---:|\n");
        for (reason, count) in &archive.gated {
            out.push_str(&format!("| {reason} | {count} |\n"));
        }
        out.push('\n');
    }

    if let Some(failure) = &archive.first_failure {
        out.push_str("## First failure\n\n");
        out.push_str(&format!(
            "{} candidate(s) failed to run. The first said:\n\n```\n{failure}\n```\n\n",
            archive.failures
        ));
    }

    out.push_str("## Hardest cells\n\n");
    if archive.cells.is_empty() {
        out.push_str(
            "The archive is empty. Either every candidate was gated out — look at the \
             table above — or the run executor failed. An empty archive is a finding \
             about the search, not about the detector.\n\n",
        );
    } else {
        out.push_str(
            "| family | density | topology | attacker | rat | protocol | fitness | \
             attackers | reported | recall (vehicle) | seed |\n\
             |---|---|---|---|---|---|---:|---:|---:|---:|---|\n",
        );
        let summary = archive.summary(15);
        for (descriptor, _) in &summary.hardest_cells {
            let Some(elite) = archive.cells.get(&descriptor.key()) else {
                continue;
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | `{}` |\n",
                descriptor.attack_family,
                descriptor.density_band,
                descriptor.topology,
                descriptor.attacker_band,
                descriptor.rat,
                descriptor.protocol,
                elite.fitness,
                elite.signals.attackers,
                elite.signals.reported_subjects,
                elite
                    .signals
                    .recall_vehicle
                    .map_or_else(|| "-".to_string(), |r| format!("{}", quantise(r))),
                elite.seed_hex,
            ));
        }
        out.push('\n');
    }

    let summary = archive.summary(usize::MAX);
    out.push_str("## Coverage gaps\n\n");
    out.push_str(&format!(
        "- Attack families with an elite: {}\n",
        if summary.families_filled.is_empty() {
            "none".to_string()
        } else {
            summary.families_filled.join(", ")
        }
    ));
    out.push_str(&format!(
        "- Attack families with none: {}\n\n",
        if summary.families_empty.is_empty() {
            "none".to_string()
        } else {
            summary.families_empty.join(", ")
        }
    ));
    out.push_str(
        "A family with no elite is **not** evidence that the detector handles it. It is \
         evidence that the search never produced a valid scenario in it — which at a \
         modest budget is the usual reason, and is why coverage is reported beside the \
         QD-score rather than instead of it.\n\n",
    );

    out.push_str("## Reproduce an elite\n\n");
    out.push_str(
        "Each cell carries its `genome` — the scenario overlay, as dotted paths — and its \
         `seed_hex`. There is **no `v2xw foundry` subcommand**: the search is a library \
         (`v2xw_experiment::foundry`) and wiring it to the command-line tool is owed work \
         in `v2xw-cli`, so a replay is three lines of Rust or one edit by hand.\n\n\
         In Rust:\n\n\
         ```rust\n\
         let base = Scenario::load(\"scenarios/revocation-latency.yaml\")?;\n\
         let scenario = foundry::materialise(&base, &cell.genome, seed, cell.candidate)?;\n\
         std::fs::write(\"runs/replay/scenario.yaml\", scenario.to_yaml()?)?;\n\
         ```\n\n\
         By hand: copy the base scenario, apply each `genome` path, set `seed` to the \
         cell's `seed_hex`, and `v2xw run` it.\n\n\
         Either way, **the `scenario_hash` in the cell is what the replay has to \
         reproduce.** If it does not, the base scenario has changed underneath the archive \
         and every fitness in it is a measurement of something else.\n\n",
    );

    out.push_str("## What this archive does not tell you\n\n");
    out.push_str(
        "- **The density axis is the declared demand rate, not the realised density.** Two \
         cells in one band can differ by a factor of two in realised vehicles. No \
         run-level density metric exists to bin on.\n\
         - **`family:<F>` restricts the population rather than slicing the metric**, \
         because `det_recall` carries no family dimension.\n\
         - **`residual_harm` is not an available objective**, so the archive maps evasion \
         and latency and not harm.\n\
         - **A high fitness is a missed *detection*, not a missed *revocation*.** Read \
         `recall (vehicle)` beside it: the vehicle level is the authority's decision and \
         the report level is the detectors'.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(attackers: u64, reported: u64, recall: Option<f64>, ttd: Option<f64>) -> Signals {
        Signals {
            attackers,
            reported_subjects: reported,
            recall_vehicle: recall,
            recall_report: recall,
            time_to_detect_s: ttd,
        }
    }

    fn descriptor(family: &str) -> Descriptor {
        Descriptor {
            attack_family: family.to_string(),
            density_band: "medium".to_string(),
            topology: "procedural".to_string(),
            attacker_band: "low".to_string(),
            rat: "dsrc-80211p".to_string(),
            protocol: "none".to_string(),
        }
    }

    fn elite(family: &str, fitness: f64, candidate: u64) -> Elite {
        Elite {
            descriptor: descriptor(family),
            fitness,
            candidate,
            seed_hex: format!("0x{candidate:016x}"),
            scenario_hash: "hash".to_string(),
            genome: Genome::new(),
            signals: signals(1, 1, Some(0.0), None),
        }
    }

    #[test]
    fn the_objective_names_round_trip_and_a_typo_is_refused() {
        assert_eq!(Objective::parse("evade").unwrap(), Objective::Evade);
        assert_eq!(Objective::parse("latency").unwrap(), Objective::Latency);
        assert_eq!(
            Objective::parse("family:stealth").unwrap().label(),
            "family:stealth"
        );
        // Refused before the budget is spent, which is the whole point: the legacy version
        // of this check was missing and an unknown objective left a silently empty archive
        // and exit 0.
        assert!(Objective::parse("family:nonesuch").is_err());
        assert!(Objective::parse("evasion").is_err());
    }

    #[test]
    fn a_scenario_with_no_attacker_is_gated_out() {
        let (score, validity) = fitness(
            &Objective::Evade,
            &signals(0, 5, Some(0.0), None),
            &descriptor("position"),
            60.0,
        );
        assert_eq!(score, 0.0);
        assert_eq!(validity, Validity::NoAttackers);
    }

    /// The gate the legacy foundry learned the hard way, and the reason it exists.
    ///
    /// **Shown to fail:** removing the `reported_subjects == 0` arm from [`fitness`] makes
    /// this candidate score 1.0 and be archived as the hardest elite in its cell, after
    /// which the search actively chases scenarios in which nobody ever looked.
    #[test]
    fn attackers_nobody_reported_are_gated_out_not_scored_as_perfect_evasion() {
        let (score, validity) = fitness(
            &Objective::Evade,
            &signals(4, 0, Some(0.0), None),
            &descriptor("position"),
            60.0,
        );
        assert_eq!(validity, Validity::NoReports);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn a_genuine_blind_spot_is_the_jackpot_and_is_kept() {
        // Reports fired, and every attacker was missed: recall 0 with reports > 0 is
        // exactly what the search is for, and it must score 1.0.
        let (score, validity) = fitness(
            &Objective::Evade,
            &signals(4, 9, Some(0.0), None),
            &descriptor("position"),
            60.0,
        );
        assert!(validity.is_valid());
        assert_eq!(score, 1.0);
    }

    #[test]
    fn perfect_detection_scores_zero_and_is_still_archived() {
        let (score, validity) = fitness(
            &Objective::Evade,
            &signals(4, 9, Some(1.0), None),
            &descriptor("position"),
            60.0,
        );
        assert!(validity.is_valid());
        assert_eq!(score, 0.0);
    }

    #[test]
    fn the_family_objective_gates_on_the_family() {
        let target = Objective::TargetFamily {
            family: "stealth".to_string(),
        };
        let (_, wrong) = fitness(
            &target,
            &signals(4, 9, Some(0.0), None),
            &descriptor("position"),
            60.0,
        );
        assert_eq!(wrong, Validity::WrongFamily);
        let (score, right) = fitness(
            &target,
            &signals(4, 9, Some(0.25), None),
            &descriptor("stealth"),
            60.0,
        );
        assert!(right.is_valid());
        assert_eq!(score, 0.75);
    }

    #[test]
    fn the_latency_objective_gates_on_having_a_measurement() {
        // Nobody caught: there is no latency to measure, and scoring 0 would be
        // indistinguishable from "caught instantly".
        let (_, none) = fitness(
            &Objective::Latency,
            &signals(4, 9, Some(0.0), None),
            &descriptor("position"),
            60.0,
        );
        assert_eq!(none, Validity::MetricMissing);
        let (score, some) = fitness(
            &Objective::Latency,
            &signals(4, 9, Some(0.5), Some(30.0)),
            &descriptor("position"),
            60.0,
        );
        assert!(some.is_valid());
        assert_eq!(score, 0.5);
        // …and a latency longer than the run saturates rather than exceeding one.
        let (saturated, _) = fitness(
            &Objective::Latency,
            &signals(4, 9, Some(0.5), Some(600.0)),
            &descriptor("position"),
            60.0,
        );
        assert_eq!(saturated, 1.0);
    }

    #[test]
    fn a_band_is_read_off_its_edges() {
        assert_eq!(band(0.0, DENSITY_EDGES, DENSITY_BANDS), "sparse");
        assert_eq!(band(5999.0, DENSITY_EDGES, DENSITY_BANDS), "sparse");
        assert_eq!(band(6000.0, DENSITY_EDGES, DENSITY_BANDS), "medium");
        assert_eq!(band(23999.0, DENSITY_EDGES, DENSITY_BANDS), "medium");
        assert_eq!(band(24000.0, DENSITY_EDGES, DENSITY_BANDS), "dense");
        assert_eq!(band(0.14, ATTACKER_EDGES, ATTACKER_BANDS), "low");
        assert_eq!(band(0.15, ATTACKER_EDGES, ATTACKER_BANDS), "med");
        assert_eq!(band(0.35, ATTACKER_EDGES, ATTACKER_BANDS), "high");
    }

    #[test]
    fn the_family_axis_is_derived_from_the_attack_catalog() {
        let bins = family_bins();
        // Every family an attack kind belongs to, plus the two synthetic bins.
        assert!(bins.contains(&"position".to_string()));
        assert!(bins.contains(&"stealth".to_string()));
        assert!(bins.contains(&"mixed".to_string()));
        assert!(bins.contains(&"none".to_string()));
        // Derived and deduplicated, so it is the family count and not the kind count.
        assert!(bins.len() < AttackKind::ALL.len());
        let mut sorted = bins.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), bins.len(), "the axis has a duplicate bin");
    }

    #[test]
    fn the_archive_keeps_the_better_elite_and_the_earlier_tie() {
        let mut archive = Archive::new();
        assert!(archive.insert_if_better(elite("position", 0.4, 1)));
        assert!(archive.insert_if_better(elite("position", 0.9, 2)));
        assert_eq!(archive.cells.len(), 1);
        assert_eq!(archive.hardest().unwrap().candidate, 2);
        // A tie keeps the incumbent, which is what makes the archive a function of the
        // search order rather than of a comparison accident.
        assert!(!archive.insert_if_better(elite("position", 0.9, 3)));
        assert_eq!(archive.hardest().unwrap().candidate, 2);
        // A worse one never displaces a better one.
        assert!(!archive.insert_if_better(elite("position", 0.1, 4)));
        assert_eq!(archive.hardest().unwrap().fitness, 0.9);
    }

    #[test]
    fn coverage_and_the_qd_score_are_functions_of_the_archive() {
        let mut archive = Archive::new();
        archive.insert_if_better(elite("position", 0.5, 1));
        archive.insert_if_better(elite("stealth", 0.25, 2));
        assert_eq!(archive.coverage(), 2);
        assert_eq!(archive.qd_score(), 0.75);
        assert!(archive.coverage_pct() > 0.0);
        assert!(grid_size() > 100, "the grid is the product of six axes");
    }

    #[test]
    fn a_candidate_seed_is_derived_and_not_drawn() {
        let a = derive_candidate_seed(0xC0FFEE, 7);
        let b = derive_candidate_seed(0xC0FFEE, 7);
        assert_eq!(a, b, "the derivation must be a function");
        assert_ne!(a, derive_candidate_seed(0xC0FFEE, 8));
        assert_ne!(a, derive_candidate_seed(0xC0FFEF, 7));
    }

    /// The guarantee the whole search rests on.
    ///
    /// **Shown to fail:** adding `"detection.local"` to `RandomMutation::TOUCHES` makes
    /// this test go red, which is how it was checked. Without the filter an injected
    /// operator could switch the detector off and the archive would map the blind spots of
    /// a detector nobody ships.
    #[test]
    fn an_operator_cannot_weaken_the_detector() {
        let mut hostile = Genome::new();
        hostile.insert("detection.local".to_string(), json!([]));
        hostile.insert(
            "security.verification_policy".to_string(),
            json!("on-demand"),
        );
        hostile.insert("nodes.default_obu".to_string(), json!("obu/cohda-mk5"));
        hostile.insert("threats.attackers[0].fraction".to_string(), json!(0.3));
        let (kept, refused) = sanitise(&hostile);
        assert_eq!(kept.len(), 1, "only the attacker knob survives");
        assert!(kept.contains_key("threats.attackers[0].fraction"));
        assert_eq!(refused.len(), 3);
        // And the built-in operator never proposes one of them in the first place.
        for path in RandomMutation::TOUCHES {
            assert!(
                !path.starts_with("detection.")
                    && !path.starts_with("nodes.")
                    && *path != "security.verification_policy",
                "{path} is a defence knob"
            );
        }
    }

    #[test]
    fn the_built_in_operator_only_writes_paths_it_declares() {
        // Every branch of the weighted choice, driven by a real stream rather than by a
        // hand-picked index, so a branch added without a `TOUCHES` entry fails here.
        let registry = RngRegistry::new(0x5EED);
        let domain = RngDomain::plugin(FOUNDRY_MODEL_ID);
        let summary = Archive::new().summary(4);
        let parent = Genome::new();
        let mut seen: Vec<String> = Vec::new();
        for i in 0..400u64 {
            let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, i));
            let child = RandomMutation
                .propose(&parent, &summary, &mut rng)
                .expect("the built-in operator always proposes");
            for path in child.keys() {
                assert!(
                    RandomMutation::TOUCHES.contains(&path.as_str()),
                    "{path} is written and not declared"
                );
                if !seen.contains(path) {
                    seen.push(path.clone());
                }
            }
        }
        // Four hundred draws over seven weighted branches, the rarest of which has
        // probability 1/17: every branch is taken with overwhelming probability. So the
        // check runs in both directions — nothing undeclared is written (above), and
        // nothing declared is unreachable (here). The second half is what catches a
        // `TOUCHES` entry left behind by a deleted operator.
        for path in RandomMutation::TOUCHES {
            assert!(
                seen.contains(&(*path).to_string()),
                "{path} is declared and never written"
            );
        }
    }

    #[test]
    fn the_operator_is_a_function_of_its_stream() {
        let registry = RngRegistry::new(11);
        let domain = RngDomain::plugin(FOUNDRY_MODEL_ID);
        let summary = Archive::new().summary(4);
        let parent = Genome::new();
        let once = {
            let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, 3));
            RandomMutation.propose(&parent, &summary, &mut rng)
        };
        let twice = {
            let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, 3));
            RandomMutation.propose(&parent, &summary, &mut rng)
        };
        assert_eq!(once, twice);
        let other = {
            let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, 4));
            RandomMutation.propose(&parent, &summary, &mut rng)
        };
        assert_ne!(once, other, "two iterations must not see one stream");
    }

    #[test]
    fn every_genome_value_is_on_the_three_decimal_grid() {
        // Two genomes that differ only in floating-point noise would be two genomes, and
        // the archive would fill with near-duplicates.
        let registry = RngRegistry::new(3);
        let domain = RngDomain::plugin(FOUNDRY_MODEL_ID);
        let summary = Archive::new().summary(4);
        for i in 0..120u64 {
            let mut rng = registry.ephemeral(domain, EntityRef::custom(FOUNDRY_MODEL_ID, i));
            let child = RandomMutation
                .propose(&Genome::new(), &summary, &mut rng)
                .expect("a proposal");
            for (path, value) in &child {
                if let Some(x) = value.as_f64() {
                    assert!(
                        v2xw_core::math::is_on_grid(x, 1e-3),
                        "{path} = {x} is off the 1e-3 grid"
                    );
                }
            }
        }
    }

    #[test]
    fn the_base_genomes_span_more_than_one_cell_axis() {
        let bases = default_base_genomes();
        assert_eq!(bases.len(), 3);
        let mut ids: Vec<&Value> = bases
            .iter()
            .filter_map(|g| g.get("threats.attackers[0].id"))
            .collect();
        let before = ids.len();
        ids.sort_by_key(|v| v.to_string());
        ids.dedup();
        assert_eq!(before, ids.len(), "two base genomes share an attacker");
        // …and every path they set is one the filter admits.
        for base in &bases {
            let (kept, refused) = sanitise(base);
            assert!(refused.is_empty(), "a base genome writes {refused:?}");
            assert_eq!(kept.len(), base.len());
        }
    }

    #[test]
    fn the_signals_are_read_from_the_metric_keys_the_provider_actually_writes() {
        // The keys are `MetricSample::key`'s spelling, and `Dim`'s declaration order puts
        // `level` before `cell`. A key written the other way round reads as absent, which
        // would silently gate every candidate out — so this test pins the spelling.
        let mut metrics: BTreeMap<String, RunMetric> = BTreeMap::new();
        let mut put = |key: &str, value: RunValue| {
            metrics.insert(
                key.to_string(),
                RunMetric {
                    metric: key.split('|').next().unwrap_or(key).to_string(),
                    unit: "count".to_string(),
                    dims: "{}".to_string(),
                    agg: "count".to_string(),
                    value,
                    windows: 1,
                    dropped_windows: 0,
                },
            );
        };
        put("det_tp|level=vehicle|cell=tp", RunValue::Count { count: 2 });
        put("det_fn|level=vehicle|cell=fn", RunValue::Count { count: 3 });
        put("det_tp|level=report|cell=tp", RunValue::Count { count: 4 });
        put("det_fp|level=report|cell=fp", RunValue::Count { count: 1 });
        put(
            "det_recall|level=vehicle",
            RunValue::Proportion {
                successes: 2,
                trials: 5,
            },
        );
        put("time_to_detect", RunValue::Scalar { value: 12.5, n: 2 });
        let read = Signals::read(&metrics);
        assert_eq!(read.attackers, 5);
        assert_eq!(read.reported_subjects, 5);
        assert_eq!(read.recall_vehicle, Some(0.4));
        assert_eq!(read.time_to_detect_s, Some(12.5));
    }

    #[test]
    fn missing_signals_read_as_absent_and_gate_conservatively() {
        let read = Signals::read(&BTreeMap::new());
        assert_eq!(read.attackers, 0);
        assert_eq!(read.reported_subjects, 0);
        assert_eq!(read.recall_vehicle, None);
        let (score, validity) = fitness(&Objective::Evade, &read, &descriptor("position"), 60.0);
        assert_eq!(score, 0.0);
        assert_eq!(validity, Validity::NoAttackers);
    }

    #[test]
    fn the_report_names_what_the_archive_cannot_tell_you() {
        let mut archive = Archive::new();
        archive.insert_if_better(elite("position", 0.9, 1));
        archive.evaluated = 10;
        archive.record_gated(Validity::NoReports);
        let options = FoundryOptions::new("runs/foundry");
        let report = render_report(&archive, &Objective::Evade, &options, "random");
        assert!(report.contains("Foundry blind-spot map"));
        assert!(report.contains("Coverage gaps"));
        assert!(report.contains("does not tell you"));
        // The gated table must name the reason rather than just counting.
        assert!(report.contains("attackers present and no report ever filed"));
        // And an empty archive must say that an empty archive is a finding about the
        // search, which is the sentence a reader needs most.
        let empty = render_report(&Archive::new(), &Objective::Evade, &options, "random");
        assert!(empty.contains("archive is empty"));
    }
}
