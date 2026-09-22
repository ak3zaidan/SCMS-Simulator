//! The records this crate writes, on the channels `v2xw-metrics` already reads.
//!
//! # No second scoring scheme
//!
//! `v2xw_metrics::detection` builds its confusion matrices from four channels —
//! `det.observation`, `ma.report`, `ma.decision` and `gt.attack.action` — and its
//! reader-side views (`v2xw_metrics::channels::DetObservationView` and friends) fix the
//! field names. The types below are the writer side of exactly those views, field for
//! field, so a detector firing in this crate lands in the metrics crate's matrix without
//! anything in between deciding what a true positive is. 07-threats §3 asks for the
//! confusion-matrix inputs; these are they, and there is no other scorer.
//!
//! The one thing this crate does **not** supply is the subject-to-actor link. A report
//! names its subject by pseudonym digest, and whether that digest belongs to an attacker
//! is ground truth: the run declares it to the metrics provider
//! (`DetectionProvider::declare_subject`) from the identity map, exactly as the legacy
//! `gt_idmap` did. A detector that could resolve a digest to an actor would be reading
//! through the firewall.
//!
//! # Quantisation
//!
//! Every float here is quantised at construction to [`SCORE_Q`] (build decision D9: no
//! float reaches a recorded artefact in raw IEEE-754 form). The quantum is the legacy
//! engine's own `round(x, 3)`, so a ported detector's score serialises to the same digits
//! the legacy corpus carries.

use serde::{Deserialize, Serialize};
use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::ids::{ActorId, NodeId};
use v2xw_core::math::quantize_to;
use v2xw_core::time::SimTime;

/// The quantum every score and magnitude in this module is rounded to: 1e-3.
///
/// The legacy engine wrote every detector score as `round(x, 3)`
/// (`legacy/scms_sim_ref/mock_pipeline/run.py :: file_report`), and ADR 0004 decision 7
/// names 1e-3 the legacy quantum. Keeping it means a ported score and a legacy score are
/// comparable digit for digit.
pub const SCORE_Q: f64 = v2xw_core::math::LEGACY_QUANTUM;

/// Rounds a score to [`SCORE_Q`].
#[must_use]
pub fn q(x: f64) -> f64 {
    quantize_to(x, SCORE_Q)
}

/// `det.observation` — one local detector firing at one node (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetObservation {
    /// The instant.
    pub t: SimTime,
    /// The observing node.
    pub node: NodeId,
    /// The detector's id, e.g. `positionSpeedInconsistency`.
    pub detector: String,
    /// The subject's pseudonym digest, lowercase hex — what the node can see, not who it
    /// really is.
    pub subject: String,
    /// The detector's normalised score, `≈ 1` at its firing threshold.
    #[serde(default)]
    pub score: Option<f64>,
}

impl Record for DetObservation {
    const CHANNEL: &'static str = "det.observation";
    const VISIBILITY: Visibility = Visibility::Node;
}

impl DetObservation {
    /// A record with its score quantised to [`SCORE_Q`].
    #[must_use]
    pub fn new(t: SimTime, node: NodeId, detector: &str, subject: &str, score: f64) -> Self {
        Self {
            t,
            node,
            detector: detector.to_string(),
            subject: subject.to_string(),
            score: Some(q(score)),
        }
    }
}

/// `ma.report` — a misbehaviour report as it reached the authority (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaReportRecord {
    /// The instant the report reached the authority.
    pub t: SimTime,
    /// The reporting node.
    #[serde(default)]
    pub reporter: Option<NodeId>,
    /// The subject, as the reporter could name it: a pseudonym digest in hex.
    pub subject: String,
    /// The detector that produced the report — the highest-scoring fired check.
    #[serde(default)]
    pub detector: Option<String>,
}

impl Record for MaReportRecord {
    const CHANNEL: &'static str = "ma.report";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// `ma.decision` — the authority's decision about a subject (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaDecisionRecord {
    /// The instant of the decision.
    pub t: SimTime,
    /// The subject.
    pub subject: String,
    /// The decision: `revoke`, `dismiss` or `investigate`.
    pub decision: String,
}

impl Record for MaDecisionRecord {
    const CHANNEL: &'static str = "ma.decision";
    const VISIBILITY: Visibility = Visibility::Node;
}

/// `gt.attack.action` — an attacker's action with the true actor behind it (GT).
///
/// Invariant I-T3: every action that changes bytes on the air is logged here, with the
/// true actor id, on a ground-truth channel only. This is the record the per-message
/// `falsified` label of 07-threats §4 is derived from, and it is the reason an attacker
/// may not compute that label itself: the attacker does not know which actor it is.
///
/// The [`Visibility::Gt`] tag is what keeps it out of every NODE export and away from
/// every detector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtAttackAction {
    /// The instant.
    pub t: SimTime,
    /// The true actor behind the action — the field I-T3 requires.
    pub actor: ActorId,
    /// The attacker model's id, e.g. `threat/attacker/legacy-catalog`.
    pub attacker: String,
    /// The action, as [`crate::attack::AttackAction::name`] spells it.
    pub action: String,
    /// The fields the action changed, sorted, e.g. `["heading", "position"]`.
    #[serde(default)]
    pub fields: Vec<String>,
    /// Whether the action changed bytes on the air.
    #[serde(default = "crate::records::yes")]
    pub changed_bytes_on_air: bool,
    /// The message id the action produced, for joining to `node.tx`.
    #[serde(default)]
    pub msg: Option<u64>,
    /// The magnitude of the falsification, in the units of the field it changed:
    /// metres for a position edit, m/s for a speed edit, degrees for a heading edit,
    /// seconds for a timing edit, a count for a repetition edit.
    ///
    /// Not in the reader-side view the metrics crate decodes today, which ignores unknown
    /// fields; it is here because 07-threats §4 asks the channel to carry "the fields
    /// changed **and the magnitude**", and because the per-message `falsified` label is a
    /// threshold on exactly this number.
    #[serde(default)]
    pub magnitude: Option<f64>,
}

impl Record for GtAttackAction {
    const CHANNEL: &'static str = "gt.attack.action";
    const VISIBILITY: Visibility = Visibility::Gt;
}

/// `serde` default for [`GtAttackAction::changed_bytes_on_air`], matching the reader-side
/// view's own default.
#[must_use]
pub const fn yes() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detection_record_lands_on_the_channel_the_metrics_crate_reads() {
        assert_eq!(DetObservation::CHANNEL, "det.observation");
        assert_eq!(MaReportRecord::CHANNEL, "ma.report");
        assert_eq!(MaDecisionRecord::CHANNEL, "ma.decision");
        assert_eq!(GtAttackAction::CHANNEL, "gt.attack.action");
    }

    /// The firewall as a unit test: the attack channel is ground-truth tainted and may
    /// not be written to a NODE channel, and every detection channel may.
    #[test]
    fn the_attack_channel_is_ground_truth_and_the_detection_channels_are_not() {
        assert!(GtAttackAction::VISIBILITY.is_gt_tainted());
        assert!(!GtAttackAction::VISIBILITY.allowed_on_node_channel());
        for v in [
            DetObservation::VISIBILITY,
            MaReportRecord::VISIBILITY,
            MaDecisionRecord::VISIBILITY,
        ] {
            assert!(!v.is_gt_tainted());
            assert!(v.allowed_on_node_channel());
        }
    }

    #[test]
    fn scores_are_quantised_at_the_writer() {
        let r = DetObservation::new(1, NodeId::new(1), "positionJump", "ab", 1.234_567_8);
        assert_eq!(r.score, Some(1.235));
        assert!(v2xw_core::math::is_on_grid(r.score.unwrap(), SCORE_Q));
    }

    /// The field names are the contract with `v2xw_metrics::channels`; a rename here
    /// would silently stop every detection metric counting.
    #[test]
    fn the_serialised_field_names_match_the_reader_side_view() {
        let json = serde_json::to_value(DetObservation::new(
            7,
            NodeId::new(2),
            "sybilCoLocation",
            "ff00",
            2.0,
        ))
        .unwrap();
        for k in ["t", "node", "detector", "subject", "score"] {
            assert!(json.get(k).is_some(), "missing {k}");
        }
        let gt = serde_json::to_value(GtAttackAction {
            t: 1,
            actor: ActorId::new(4),
            attacker: "threat/attacker/legacy-catalog".to_string(),
            action: "FalsifyOutgoing".to_string(),
            fields: vec!["position".to_string()],
            changed_bytes_on_air: true,
            msg: Some(9),
            magnitude: Some(25.0),
        })
        .unwrap();
        for k in [
            "t",
            "actor",
            "attacker",
            "action",
            "fields",
            "changed_bytes_on_air",
            "msg",
        ] {
            assert!(gt.get(k).is_some(), "missing {k}");
        }
    }
}
