//! Schema migration: one migrator per version step (03-interfaces.md §13).
//!
//! A scenario declares its schema version explicitly and the loader walks a **chain** of
//! single-step migrators until it reaches [`CURRENT_SCHEMA`]. One migrator per step, never
//! one that jumps two: a two-step migrator has to reimplement the first step's decisions,
//! and the two copies drift. If a step is missing the load fails with
//! [`ScenarioError::MigrationGap`] naming the version that has no successor, rather than
//! guessing.
//!
//! # Why the shipped chain is empty
//!
//! `v2xw/scenario/1` is the only schema version that has ever existed, so there is no
//! step to migrate. The chain is built anyway, with its own tests, because the moment
//! there *is* a version 2 the mechanism has to already work and already be covered — and
//! because an empty chain is the honest thing to ship: inventing a version 0 to have
//! something to migrate would put a fictional schema in the repository.
//!
//! [`Chain::migrate`] is exercised by [`Chain::with_steps`] in the tests below, which
//! drives the real walk over a two-step test chain, so the machinery is tested rather
//! than merely written.

use serde_json::Value;

use crate::error::ScenarioError;
use crate::scenario::schema::CURRENT_SCHEMA;

/// One version step.
pub struct Migration {
    /// The version this step reads.
    pub from: &'static str,
    /// The version it produces.
    pub to: &'static str,
    /// The transformation, on the untyped document.
    ///
    /// It runs *before* deserialisation, because the whole point is that the document does
    /// not deserialise into the current schema yet.
    pub apply: fn(&mut Value),
}

impl core::fmt::Debug for Migration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Migration")
            .field("from", &self.from)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// The ordered chain of migrations this build knows.
#[derive(Debug)]
pub struct Chain {
    steps: Vec<Migration>,
    target: &'static str,
}

impl Chain {
    /// The chain this build ships: empty, targeting [`CURRENT_SCHEMA`].
    pub fn shipped() -> Chain {
        Chain {
            steps: Vec::new(),
            target: CURRENT_SCHEMA,
        }
    }

    /// A chain over explicit steps, for tests and for an out-of-tree loader that carries
    /// its own schema history.
    pub fn with_steps(steps: Vec<Migration>, target: &'static str) -> Chain {
        Chain { steps, target }
    }

    /// The version this chain migrates to.
    pub fn target(&self) -> &'static str {
        self.target
    }

    /// Every version this chain can start from, oldest first, including the target.
    pub fn known_versions(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self.steps.iter().map(|s| s.from).collect();
        v.push(self.target);
        v
    }

    /// Migrates `doc` in place until its `schema` reads [`Chain::target`].
    ///
    /// # Errors
    /// [`ScenarioError::UnknownSchema`] if the document's version is not in the chain,
    /// and [`ScenarioError::MigrationGap`] if the chain stops before the target — which
    /// is what a missing step looks like from here.
    pub fn migrate(&self, doc: &mut Value) -> Result<(), ScenarioError> {
        let mut version = match doc.get("schema").and_then(Value::as_str) {
            Some(s) => s.to_string(),
            None => {
                return Err(ScenarioError::UnknownSchema {
                    found: "<missing>".to_string(),
                    known: self.known_versions().join(", "),
                });
            }
        };
        if version == self.target {
            return Ok(());
        }
        if !self.steps.iter().any(|s| s.from == version) {
            return Err(ScenarioError::UnknownSchema {
                found: version,
                known: self.known_versions().join(", "),
            });
        }
        // Bounded by the chain length: each step's `to` is distinct from its `from` and
        // the chain is finite, so a cycle would be a build-time mistake, which
        // `the_shipped_chain_is_acyclic_and_reaches_its_target` catches.
        for _ in 0..=self.steps.len() {
            if version == self.target {
                return Ok(());
            }
            let Some(step) = self.steps.iter().find(|s| s.from == version) else {
                return Err(ScenarioError::MigrationGap { from: version });
            };
            (step.apply)(doc);
            version = step.to.to_string();
            if let Some(obj) = doc.as_object_mut() {
                obj.insert("schema".to_string(), Value::String(version.clone()));
            }
        }
        if version == self.target {
            Ok(())
        } else {
            Err(ScenarioError::MigrationGap { from: version })
        }
    }
}

impl Default for Chain {
    fn default() -> Self {
        Chain::shipped()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rename_duration(doc: &mut Value) {
        let Some(time) = doc.get_mut("time").and_then(Value::as_object_mut) else {
            return;
        };
        if let Some(v) = time.remove("duration") {
            time.insert("duration_s".to_string(), v);
        }
    }

    fn add_codec_tier(doc: &mut Value) {
        let obj = doc.as_object_mut().expect("object");
        let messages = obj
            .entry("messages")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .expect("object");
        messages
            .entry("codec_tier")
            .or_insert_with(|| json!("size-model"));
    }

    fn two_step_chain() -> Chain {
        Chain::with_steps(
            vec![
                Migration {
                    from: "test/scenario/1",
                    to: "test/scenario/2",
                    apply: rename_duration,
                },
                Migration {
                    from: "test/scenario/2",
                    to: "test/scenario/3",
                    apply: add_codec_tier,
                },
            ],
            "test/scenario/3",
        )
    }

    /// The walk runs every step between the document's version and the target, in order.
    #[test]
    fn the_chain_walks_one_step_at_a_time_to_the_target() {
        let mut doc = json!({"schema": "test/scenario/1", "time": {"duration": 30}});
        two_step_chain().migrate(&mut doc).expect("migrates");
        assert_eq!(doc["schema"], json!("test/scenario/3"));
        assert_eq!(doc["time"]["duration_s"], json!(30));
        assert_eq!(doc["messages"]["codec_tier"], json!("size-model"));
    }

    /// Starting midway runs only the remaining steps.
    #[test]
    fn a_document_already_partway_runs_only_the_remaining_steps() {
        let mut doc = json!({"schema": "test/scenario/2", "time": {"duration": 30}});
        two_step_chain().migrate(&mut doc).expect("migrates");
        // The step that renames it was not run, because the document was past it.
        assert_eq!(doc["time"]["duration"], json!(30));
        assert_eq!(doc["messages"]["codec_tier"], json!("size-model"));
    }

    /// A version with no successor is a gap, and the error names it.
    #[test]
    fn a_missing_step_is_a_gap_naming_the_version_that_has_none() {
        let chain = Chain::with_steps(
            vec![Migration {
                from: "test/scenario/1",
                to: "test/scenario/2",
                apply: rename_duration,
            }],
            "test/scenario/3",
        );
        let mut doc = json!({"schema": "test/scenario/1"});
        let err = chain.migrate(&mut doc).expect_err("gap");
        assert_eq!(
            err,
            ScenarioError::MigrationGap {
                from: "test/scenario/2".to_string()
            }
        );
        assert!(err.to_string().contains("test/scenario/2"));
    }

    /// An unknown version is refused and the message lists what is known.
    #[test]
    fn an_unknown_version_lists_the_versions_the_chain_covers() {
        let mut doc = json!({"schema": "v2xw/scenario/99"});
        let err = Chain::shipped().migrate(&mut doc).expect_err("unknown");
        assert!(matches!(err, ScenarioError::UnknownSchema { .. }));
        assert!(err.to_string().contains(CURRENT_SCHEMA));
        assert_eq!(err.field(), Some("schema"));
    }

    /// A document at the target needs nothing done to it.
    #[test]
    fn the_current_version_migrates_to_itself_without_change() {
        let mut doc = json!({"schema": CURRENT_SCHEMA, "seed": 7});
        let before = doc.clone();
        Chain::shipped().migrate(&mut doc).expect("no-op");
        assert_eq!(doc, before);
    }

    /// The shipped chain reaches its target from every version it claims to know, which
    /// is what stops a later step being added with the wrong `from`.
    #[test]
    fn the_shipped_chain_is_acyclic_and_reaches_its_target() {
        let chain = Chain::shipped();
        for v in chain.known_versions() {
            let mut doc = json!({"schema": v});
            chain
                .migrate(&mut doc)
                .unwrap_or_else(|e| panic!("{v} does not reach {}: {e}", chain.target()));
            assert_eq!(doc["schema"], json!(chain.target()));
        }
    }
}
