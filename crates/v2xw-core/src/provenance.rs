//! Provenance: which model and which parameters produced a value.
//!
//! The `why` service of 03-interfaces.md §1.1 lets a plug-in attach `(model, parameters)`
//! to any value the UI or an exporter may show, so the inspector can answer "where does
//! this number come from?" — *"this shadowing value came from
//! `radio/propagation/log-distance-shadowing@1.2.0` with σ = 4 dB (source: …)"*
//! (02-architecture.md §6.5).
//!
//! It has to be cheap enough to call on a hot path. A record is three small handles —
//! a [`ProvSubject`], a [`crate::registry::ModelRef`] and a
//! [`crate::registry::ParamSetId`] — and the log deduplicates by the whole triple, so
//! calling `why` once per sample of a metric that is sampled a million times stores one
//! entry, not a million.

use indexmap::IndexSet;
use serde::{Deserialize, Serialize};

use crate::ids::{ActorId, LinkKey, NodeId};
use crate::registry::{ModelRef, ParamSetId};

/// The kind of geometry object a provenance record can describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum GeometryKind {
    /// A lane centreline or its attributes.
    Lane,
    /// A junction, its internal lanes or its conflict matrix.
    Junction,
    /// A building footprint or its height.
    Building,
    /// A terrain cell.
    Terrain,
    /// A land-use zone.
    Landuse,
    /// An RSU or cell site.
    Site,
}

/// What a provenance record is *about*.
///
/// Field names are short strings (`"cbr"`, `"shadow_db"`, `"height_m"`) chosen by the
/// model that emits them and shown verbatim in the inspector. They are cloned only the
/// first time a given triple is recorded.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ProvSubject {
    /// A sample of a named metric (the `metric.sample` channel, 03-interfaces.md §14).
    Metric {
        /// Metric name, e.g. `pdr`.
        name: String,
    },
    /// A field of a node's state or belief, e.g. its CBR or its position estimate.
    Node {
        /// The node.
        node: NodeId,
        /// Field name.
        field: String,
    },
    /// A field of a directed link, e.g. its path loss or fading sample.
    Link {
        /// The directed link.
        link: LinkKey,
        /// Field name.
        field: String,
    },
    /// A field of an actor's ground-truth state.
    Actor {
        /// The actor.
        actor: ActorId,
        /// Field name.
        field: String,
    },
    /// A geometry object produced by a world importer.
    Geometry {
        /// What kind of object.
        kind: GeometryKind,
        /// Its id within that kind.
        id: u32,
        /// Field name.
        field: String,
    },
    /// A scenario-level or run-level value, e.g. the weather timeline.
    Global {
        /// Field name.
        field: String,
    },
}

impl ProvSubject {
    /// A metric-sample subject.
    pub fn metric(name: impl Into<String>) -> Self {
        ProvSubject::Metric { name: name.into() }
    }

    /// A node-field subject.
    pub fn node(node: NodeId, field: impl Into<String>) -> Self {
        ProvSubject::Node {
            node,
            field: field.into(),
        }
    }

    /// A link-field subject.
    pub fn link(link: LinkKey, field: impl Into<String>) -> Self {
        ProvSubject::Link {
            link,
            field: field.into(),
        }
    }

    /// An actor-field subject.
    pub fn actor(actor: ActorId, field: impl Into<String>) -> Self {
        ProvSubject::Actor {
            actor,
            field: field.into(),
        }
    }

    /// A geometry-object subject.
    pub fn geometry(kind: GeometryKind, id: u32, field: impl Into<String>) -> Self {
        ProvSubject::Geometry {
            kind,
            id,
            field: field.into(),
        }
    }

    /// A run-level subject.
    pub fn global(field: impl Into<String>) -> Self {
        ProvSubject::Global {
            field: field.into(),
        }
    }
}

/// One `(subject, model, parameters)` triple.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    /// What the record is about.
    pub subject: ProvSubject,
    /// The model that produced it.
    pub model: ModelRef,
    /// The parameter set it used.
    pub params: ParamSetId,
}

/// The deduplicated set of provenance triples for a run.
///
/// Insertion-ordered, so exports are deterministic; deduplicated by the whole triple, so
/// a hot path can call [`ProvenanceLog::record`] unconditionally. When two models write
/// the same subject (a value with two contributing models, or a subject re-computed after
/// a parameter change) both triples are kept, and [`ProvenanceLog::for_subject`] returns
/// them in the order they were first recorded.
#[derive(Debug, Clone, Default)]
pub struct ProvenanceLog {
    entries: IndexSet<ProvenanceEntry>,
}

impl ProvenanceLog {
    /// Creates an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `model` with `params` produced `subject`.
    ///
    /// Returns `true` if this triple had not been recorded before. This is the
    /// implementation of `Ctx::why` (03-interfaces.md §1.1).
    pub fn record(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) -> bool {
        self.entries.insert(ProvenanceEntry {
            subject,
            model,
            params,
        })
    }

    /// True if this exact triple has been recorded.
    pub fn contains(&self, subject: &ProvSubject, model: ModelRef, params: ParamSetId) -> bool {
        self.entries.contains(&ProvenanceEntry {
            subject: subject.clone(),
            model,
            params,
        })
    }

    /// Every triple recorded for a subject, in first-recorded order.
    pub fn for_subject<'a>(
        &'a self,
        subject: &'a ProvSubject,
    ) -> impl Iterator<Item = &'a ProvenanceEntry> {
        self.entries.iter().filter(move |e| &e.subject == subject)
    }

    /// Every triple, in first-recorded order.
    pub fn iter(&self) -> impl Iterator<Item = &ProvenanceEntry> {
        self.entries.iter()
    }

    /// Number of distinct triples.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drops every record.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link() -> LinkKey {
        LinkKey::new(NodeId::new(1), NodeId::new(2))
    }

    #[test]
    fn records_are_deduplicated_by_the_whole_triple() {
        let mut log = ProvenanceLog::new();
        assert!(log.is_empty());
        let m = ModelRef::new(3);
        let p = ParamSetId::new(0);
        let subject = ProvSubject::link(link(), "shadow_db");

        assert!(log.record(subject.clone(), m, p), "first record is new");
        for _ in 0..1_000 {
            assert!(!log.record(subject.clone(), m, p), "repeats are dropped");
        }
        assert_eq!(log.len(), 1);
        assert!(log.contains(&subject, m, p));

        // A different parameter set is a different triple…
        assert!(log.record(subject.clone(), m, ParamSetId::new(1)));
        // …as is a different model…
        assert!(log.record(subject.clone(), ModelRef::new(4), p));
        // …as is a different subject.
        assert!(log.record(ProvSubject::link(link(), "path_db"), m, p));
        assert_eq!(log.len(), 4);
    }

    #[test]
    fn subjects_distinguish_entities_and_fields() {
        let mut log = ProvenanceLog::new();
        let m = ModelRef::new(0);
        let p = ParamSetId::new(0);
        assert!(log.record(ProvSubject::node(NodeId::new(1), "cbr"), m, p));
        assert!(log.record(ProvSubject::node(NodeId::new(2), "cbr"), m, p));
        assert!(log.record(ProvSubject::node(NodeId::new(1), "rssi"), m, p));
        assert!(log.record(ProvSubject::actor(ActorId::new(1), "speed"), m, p));
        assert!(log.record(ProvSubject::metric("pdr"), m, p));
        assert!(log.record(ProvSubject::global("weather"), m, p));
        assert!(log.record(
            ProvSubject::geometry(GeometryKind::Building, 7, "height_m"),
            m,
            p
        ));
        // The link direction matters, like everywhere else.
        assert!(log.record(ProvSubject::link(link(), "fading_db"), m, p));
        assert!(log.record(ProvSubject::link(link().reversed(), "fading_db"), m, p));
        assert_eq!(log.len(), 9);
    }

    #[test]
    fn lookup_by_subject_keeps_first_recorded_order() {
        let mut log = ProvenanceLog::new();
        let subject = ProvSubject::metric("pdr");
        log.record(subject.clone(), ModelRef::new(2), ParamSetId::new(0));
        log.record(
            ProvSubject::metric("cbr"),
            ModelRef::new(9),
            ParamSetId::new(0),
        );
        log.record(subject.clone(), ModelRef::new(1), ParamSetId::new(0));
        let models: Vec<ModelRef> = log.for_subject(&subject).map(|e| e.model).collect();
        assert_eq!(models, vec![ModelRef::new(2), ModelRef::new(1)]);
        assert_eq!(log.iter().count(), 3);

        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn entries_serialise() {
        let e = ProvenanceEntry {
            subject: ProvSubject::link(link(), "shadow_db"),
            model: ModelRef::new(3),
            params: ParamSetId::new(1),
        };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<ProvenanceEntry>(&s).unwrap(), e);
        assert!(s.contains("\"link\""), "{s}");
    }
}
