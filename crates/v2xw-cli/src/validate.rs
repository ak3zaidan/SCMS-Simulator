//! `v2xw validate` — load a scenario, check it, and say what is wrong in the author's
//! terms.
//!
//! 03-interfaces.md §13 asks for errors that name the field and the conflict rather than a
//! line number and a serde message, and the engine's [`v2xw_engine::ScenarioError`] is
//! already that shape. This command's whole job is to print it without flattening it, and
//! to say what the merge produced, because the commonest surprise in a scenario with a
//! `meta.base` is a key the author believes they overrode and did not.
//!
//! Validation does **not** build the world. `v2xw validate` on a scenario naming a
//! 30 MB OSM extract is instant, and that is the point: it is the check an author runs on
//! every edit. Whether the world can be built is what `v2xw run` finds out.

use std::path::Path;

use v2xw_engine::Scenario;

use crate::error::Result;

/// What validating a scenario found.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ValidateOutcome {
    /// The scenario's name after merging.
    pub name: String,
    /// The schema version it declares.
    pub schema: String,
    /// The hash of the document the engine would execute.
    pub scenario_hash: String,
    /// Its master seed, as hex.
    pub master_seed_hex: String,
    /// The world source, as a label.
    pub world_source: String,
    /// Simulated seconds.
    pub duration_s: f64,
    /// The mobility step, milliseconds.
    pub mobility_step_ms: u64,
    /// The metrics it asks for.
    pub metrics: Vec<String>,
    /// Notes worth printing that are not errors.
    pub notes: Vec<String>,
}

/// Loads and validates a scenario without running it.
///
/// # Errors
/// [`crate::CliError::Engine`] carrying the [`v2xw_engine::ScenarioError`], whose
/// [`v2xw_engine::ScenarioError::field`] names the offending key.
pub fn validate(path: &Path) -> Result<ValidateOutcome> {
    // `Scenario::load` already validates — it parses, migrates, merges the base and then
    // calls `validate`, returning the first conflict. This command therefore *is* the
    // loader plus a report, and there is no second validation pass here: a redundant call
    // would look like a check while guarding nothing, which is the failure mode this
    // project has produced four times. If the loader ever stops validating,
    // `tests/cli.rs::an_invalid_scenario_names_the_offending_field` goes red.
    let scenario = Scenario::load(path)?;

    let mut notes = Vec::new();
    // Three things the loader accepts that an author usually did not mean. Each is a note
    // and not an error, because each is a legitimate scenario.
    if scenario.metrics.is_empty() {
        notes.push(
            "metrics: empty, so the run emits no metric.sample records; `[all]` selects \
             every provider this build ships"
                .to_string(),
        );
    }
    if scenario.actors.vehicles.demand.kind == "mobility/demand/none" {
        notes.push(
            "actors.vehicles.demand.kind: 'mobility/demand/none' spawns no vehicles, so \
             the run produces no traffic and no messages"
                .to_string(),
        );
    }
    if scenario.time.duration_s <= 0.0 {
        notes.push(format!(
            "time.duration_s: {} stops the run before its first mobility step",
            scenario.time.duration_s
        ));
    }
    if let Some(base) = &scenario.meta.base {
        notes.push(format!(
            "meta.base: merged from '{base}'; the values above are the merged ones"
        ));
    }

    Ok(ValidateOutcome {
        name: scenario.meta.name.clone(),
        schema: scenario.schema.clone(),
        scenario_hash: scenario
            .content_hash()
            .map_err(v2xw_engine::EngineError::from)?,
        master_seed_hex: format!("{:#x}", scenario.seed),
        world_source: scenario.world.source.label(),
        duration_s: scenario.time.duration_s,
        mobility_step_ms: scenario.time.mobility_step_ms,
        metrics: scenario.metrics.clone(),
        notes,
    })
}
