//! Reader-side views for the channels `v2xw-metrics` does not publish one for.
//!
//! Every other channel this module's exporters read comes from
//! [`v2xw_metrics::channels`], deliberately: 08-measurement-and-data.md §1 wants one
//! typed view per channel so that "a metric provider written in Python sees exactly what a
//! Rust one sees", and a second copy of a field list is the drift D11's one-type-per-channel
//! rule exists to prevent. These three channels have no metric that reads them yet, so
//! there is no view to reuse; when one appears in `v2xw-metrics`, these go away and the
//! exporters switch over.
//!
//! The optionality rule is the same as the metrics views': a field a producer at a low
//! tier may not fill is an `Option`, and an absent field is treated as absent rather than
//! as zero.

use serde::{Deserialize, Serialize};
use v2xw_core::ids::ActorId;
use v2xw_core::time::SimTime;

/// `gt.spawn` — an actor entered the simulation (GT).
///
/// 03-interfaces.md §14 gives this channel no payload in an `Event` frame
/// (`payload_bytes: None` in [`crate::channels::CHANNELS`]), so the serde record is the
/// only encoding, and the ORACLE tables are its only consumer. It carries the labels the
/// dataset's `gt_vehicle` table is made of, which is why they are here and nowhere a node
/// can reach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GtSpawnView {
    /// The instant the actor appeared.
    pub t: SimTime,
    /// The actor.
    pub actor: ActorId,
    /// Whether it is an attacker. The single most consequential label in the dataset.
    #[serde(default)]
    pub is_attacker: bool,
    /// Its attacker role, where it has one.
    #[serde(default)]
    pub attacker_role: Option<String>,
    /// Its collusion group, where it is in one.
    #[serde(default)]
    pub colluding_group_id: Option<String>,
    /// Whether it is faulty rather than malicious — a distinction the labels keep because
    /// a detector that flags a faulty device is not wrong.
    #[serde(default)]
    pub is_faulty: bool,
    /// Its class (`car`, `truck`, `bus`, `moto`, `vru`, `rsu`).
    #[serde(default)]
    pub class: Option<String>,
    /// Whether it is infrastructure. An RSU's certificate is not a vehicle pseudonym, so
    /// it is absent from the identity map, which is what the frozen audit's `R3` check
    /// allows for with its RSU allowance.
    #[serde(default)]
    pub is_rsu: bool,
}

/// `ma.case` — the authority opened a case (NODE).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaCaseView {
    /// When the case opened.
    pub t: SimTime,
    /// The subject, as the authority can name it: a pseudonym digest.
    pub subject: String,
    /// What opened the case.
    #[serde(default)]
    pub trigger: Option<String>,
    /// How many reports were clustered into it.
    #[serde(default)]
    pub cluster_size: Option<u64>,
    /// How many distinct reporters contributed.
    #[serde(default)]
    pub num_distinct_reporters: Option<u64>,
    /// `same` | `different` | `unknown`.
    #[serde(default)]
    pub linkage_result: Option<String>,
}

/// `app.warning` — a safety application's outcome (NODE, with a GT `truth` column).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppWarningView {
    /// The instant.
    pub t: SimTime,
    /// The warning's application.
    pub app: String,
    /// Whether the warning fired.
    #[serde(default)]
    pub fired: bool,
    /// Whether it should have — ground truth, and projected out of every NODE profile.
    #[serde(default)]
    pub truth: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spawn_record_defaults_a_benign_actor_rather_than_guessing() {
        let v: GtSpawnView =
            serde_json::from_str(r#"{"t":0,"actor":3}"#).expect("a minimal spawn record");
        assert!(!v.is_attacker);
        assert!(!v.is_faulty);
        assert!(!v.is_rsu);
        assert_eq!(v.attacker_role, None, "absent means absent, not \"none\"");
        assert_eq!(v.class, None);
    }

    #[test]
    fn a_case_record_reads_what_it_is_given_and_nothing_more() {
        let v: MaCaseView =
            serde_json::from_str(r#"{"t":5,"subject":"aabb","cluster_size":4,"something_new":1}"#)
                .expect("a case record with a field this view does not read");
        assert_eq!(v.cluster_size, Some(4));
        assert_eq!(v.num_distinct_reporters, None);
    }
}
