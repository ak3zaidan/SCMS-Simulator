//! Pseudonym, certificate-pool, linkability and backend-link metrics.
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `pseudonym_change_rate` | Σ pseudonym changes / Σ vehicle-hours observed | 1/(veh·h) | a vehicle's changes before its first `node.security` row |
//! | `cert_pool_valid` | distribution, over the window's `node.security` rows, of the certificates each vehicle holds valid now | count | certificates held for later periods (`pool_preloaded` on the row) |
//! | `linkability` | correct links / pseudonym changes, cumulative | ratio | links an observer with less coverage would miss: the observer hears every frame on the air (a global passive eavesdropper, the worst case) |
//! | `backend_link_up` | rows whose vehicle's backend access was usable / rows | ratio | how long a link was down between two rows |
//!
//! `linkability` is the quantity 07-threats-and-detection.md §6 and ETSI TR 103 415 §5
//! define: the share of pseudonym changes a passive observer bridges correctly. The
//! observer is `v2xw_threat::PrivacyObserver` (the Wiedersheim et al. WONS 2010 kinematic
//! tracker); its claims arrive on `privacy.link`, and the ground-truth join — which
//! pseudonyms were one vehicle — is made here from the vehicles' own `sec.pseudonym` and
//! `node.security` rows, never by the observer. A declined link counts as a change the
//! observer did not bridge, so the denominator is every change and not only the claimed
//! ones.

use std::collections::BTreeMap;

use v2xw_core::card::{ModelCard, Source, SourceKind};
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;
use v2xw_metrics::MetricProvider;
use v2xw_metrics::def::{Agg, DEFAULT_LEVEL, Dim, Dims, MetricDef, MetricSample, SampleValue};
use v2xw_metrics::quant::Quantum;
use v2xw_metrics::stats::{Distribution, Estimate, Proportion};

use crate::sec_records::{NodeSecurityView, SecPseudonymView};

/// The provider's id.
pub const ID: &str = "metric/security/pseudonym-privacy";

/// The provider.
pub struct PrivacyProvider {
    card: ModelCard,
    /// Every pseudonym digest a vehicle has used, to its node: the ground-truth join.
    owner: BTreeMap<String, NodeId>,
    changes: u64,
    /// Seconds each vehicle has been observed for, and when it was last seen.
    seen: BTreeMap<NodeId, (SimTime, SimTime)>,
    pool: Distribution,
    link_up: Proportion,
    /// Link claims waiting for their ground-truth join: `(predecessor, successor)`.
    claims: Vec<(String, String)>,
    correct: u64,
    rejected: u64,
}

impl Default for PrivacyProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl PrivacyProvider {
    /// A provider with nothing seen.
    #[must_use]
    pub fn new() -> Self {
        let mut card = v2xw_metrics::cards::provider_card(
            ID,
            "1.0.0",
            "Pseudonym changes per vehicle-hour, the certificate pool each vehicle holds, \
             the share of pseudonym changes a passive observer links, and the share of the \
             fleet whose backend link is usable.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "pseudonym_change_rate",
                "Σ changes / Σ vehicle-hours observed",
            ),
            v2xw_core::card::Equation::new(
                "linkability",
                "correct observer links / pseudonym changes (cumulative)",
            ),
        ];
        card.sources = vec![
            v2xw_metrics::cards::design("08-measurement-and-data.md §2.6"),
            Source::new(
                SourceKind::Paper,
                "Wiedersheim, Ma, Kargl, Papadimitratos, Privacy in inter-vehicular networks: \
                 why simple pseudonym change is not enough, WONS 2010",
            ),
            Source::new(
                SourceKind::Standard,
                "ETSI TR 103 415 V1.1.1 §5 (pseudonym change and linkability)",
            ),
        ];
        card.parameters = v2xw_metrics::cards::statistics_params();
        card.limitations = vec![
            "The observer hears every frame on the air: linkability is the upper bound a \
             global passive eavesdropper reaches, not what a roadside receiver network of a \
             given density would."
                .to_string(),
        ];
        card.ignores = vec![
            "Linking through anything but kinematics (radio fingerprints, message timing \
             patterns)."
                .to_string(),
        ];
        card.validation.tests = vec![
            "privacy_metrics::tests::a_link_is_correct_only_between_one_vehicles_pseudonyms"
                .to_string(),
        ];
        Self {
            card,
            owner: BTreeMap::new(),
            changes: 0,
            seen: BTreeMap::new(),
            pool: Distribution::new(),
            link_up: Proportion::new(),
            claims: Vec::new(),
            correct: 0,
            rejected: 0,
        }
    }

    fn defs_inner() -> Vec<MetricDef> {
        let src = v2xw_metrics::cards::design("08-measurement-and-data.md §2.6");
        vec![
            MetricDef::new(
                "pseudonym_change_rate",
                "1/(veh·h)",
                Agg::Mean,
                Visibility::Node,
                Quantum::RATIO,
                "Pseudonym changes per vehicle-hour: all changes over all vehicle-time \
                 observed.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("changes before a vehicle's first security row"),
            MetricDef::new(
                "cert_pool_valid",
                "count",
                Agg::Distribution,
                Visibility::Node,
                Quantum::COUNT,
                "Certificates each vehicle holds valid at the instant of its security row.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("certificates preloaded for later periods"),
            MetricDef::new(
                "linkability",
                "ratio",
                Agg::Ratio {
                    numerator: "pseudonym changes a passive observer linked correctly".to_string(),
                    denominator: "pseudonym changes".to_string(),
                },
                Visibility::Gt,
                Quantum::PROBABILITY,
                "The share of pseudonym changes a global passive observer bridges \
                 correctly, cumulative over the run.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("an observer with less than full coverage"),
            MetricDef::new(
                "backend_link_up",
                "ratio",
                Agg::Ratio {
                    numerator: "vehicle security rows with a usable backend link".to_string(),
                    denominator: "vehicle security rows".to_string(),
                },
                Visibility::Node,
                Quantum::PROBABILITY,
                "The share of vehicles whose backend access (cellular coverage, a relaying \
                 roadside unit in range) was usable, over the window.",
            )
            .with_dims([Dim::T])
            .with_source(src)
            .with_min_samples(1)
            .not_accounting_for("outages shorter than the telemetry window"),
        ]
    }

    fn def(name: &str) -> MetricDef {
        Self::defs_inner()
            .into_iter()
            .find(|d| d.name == name)
            .expect("declared above")
    }

    fn join(&mut self) {
        let mut pending = Vec::new();
        for (pred, succ) in core::mem::take(&mut self.claims) {
            match (self.owner.get(&pred), self.owner.get(&succ)) {
                (Some(a), Some(b)) => {
                    if a == b {
                        self.correct += 1;
                    }
                }
                _ => pending.push((pred, succ)),
            }
        }
        self.claims = pending;
    }
}

impl Model for PrivacyProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for PrivacyProvider {
    fn defs(&self) -> Vec<MetricDef> {
        Self::defs_inner()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            ChannelName("sec.pseudonym"),
            ChannelName("node.security"),
            ChannelName("privacy.link"),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            "sec.pseudonym" => match serde_json::from_slice::<SecPseudonymView>(&ev.json) {
                Ok(v) => {
                    self.changes += 1;
                    for d in [v.old_digest, v.new_digest].into_iter().flatten() {
                        self.owner.insert(d, v.node);
                    }
                }
                Err(_) => self.rejected += 1,
            },
            "node.security" => match serde_json::from_slice::<NodeSecurityView>(&ev.json) {
                Ok(v) => {
                    if let Some(d) = v.pseudonym.clone() {
                        self.owner.insert(d, v.node);
                    }
                    let e = self.seen.entry(v.node).or_insert((v.t, v.t));
                    e.1 = e.1.max(v.t);
                    let _ = self.pool.observe(f64::from(v.pool_valid));
                    self.link_up.observe(v.link_up);
                }
                Err(_) => self.rejected += 1,
            },
            "privacy.link" => {
                match serde_json::from_slice::<v2xw_threat::records::PrivacyLinkClaim>(&ev.json) {
                    Ok(c) => {
                        if !c.predecessor.is_empty() {
                            self.claims.push((c.predecessor, c.successor));
                        }
                    }
                    Err(_) => self.rejected += 1,
                }
            }
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        self.join();
        let mut out = Vec::new();
        let dims = Dims::new;
        let vehicle_s: f64 = self
            .seen
            .values()
            .map(|(a, b)| (b.saturating_sub(*a)) as f64 * 1e-9)
            .sum();
        let rate = if vehicle_s > 0.0 {
            Estimate::Value {
                point: self.changes as f64 * 3600.0 / vehicle_s,
                n: self.seen.len() as u64,
            }
        } else {
            Estimate::Insufficient { n: 0, required: 1 }
        };
        out.push(MetricSample::new(
            &Self::def("pseudonym_change_rate"),
            at,
            dims(),
            SampleValue::Scalar(rate),
        ));
        let pool = core::mem::replace(&mut self.pool, Distribution::new());
        out.push(MetricSample::new(
            &Self::def("cert_pool_valid"),
            at,
            dims(),
            SampleValue::Distribution(pool.summary(1)),
        ));
        let changes = self.changes;
        let linked = Proportion::from_counts(self.correct.min(changes), changes);
        out.push(MetricSample::new(
            &Self::def("linkability"),
            at,
            dims(),
            SampleValue::Ratio(linked.estimate(1, DEFAULT_LEVEL)),
        ));
        let up = core::mem::replace(&mut self.link_up, Proportion::new());
        out.push(MetricSample::new(
            &Self::def("backend_link_up"),
            at,
            dims(),
            SampleValue::Ratio(up.estimate(1, DEFAULT_LEVEL)),
        ));
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(channel: &'static str, json: String) -> v2xw_core::ctx::OwnedRecord {
        v2xw_core::ctx::OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: json.into_bytes(),
        }
    }

    #[test]
    fn a_link_is_correct_only_between_one_vehicles_pseudonyms() {
        let mut p = PrivacyProvider::new();
        let change = |node: u32, old: &str, new: &str| {
            rec(
                "sec.pseudonym",
                format!(
                    r#"{{"t":1,"node":{node},"reason":"scheduled","old_digest":"{old}","new_digest":"{new}","old_temp_id":null,"new_temp_id":null,"old_l2":null,"new_l2":null,"i":0,"j":1,"pool_valid":19,"changes":1}}"#
                ),
            )
        };
        let link = |pred: &str, succ: &str| {
            rec(
                "privacy.link",
                format!(
                    r#"{{"t":2,"observer":0,"predecessor":"{pred}","successor":"{succ}","posterior":1.0,"candidates":1,"anonymity_set_size":1,"effective_anonymity_set_bits":0.0,"degree_of_anonymity":0.0,"method":"m"}}"#
                ),
            )
        };
        for r in [
            change(1, "aa", "bb"),
            change(2, "cc", "dd"),
            link("aa", "bb"),
            link("cc", "bb"),
        ] {
            p.on_event(&r);
        }
        let out = p.flush(10);
        let l = out.iter().find(|s| s.metric == "linkability").unwrap();
        // One of two changes bridged correctly; the wrong link does not count.
        assert_eq!(l.value.point(), Some(0.5));
        // And the check can fail: had the second link been right, the share would be 1.
        let mut q = PrivacyProvider::new();
        for r in [
            change(1, "aa", "bb"),
            change(2, "cc", "dd"),
            link("aa", "bb"),
            link("cc", "dd"),
        ] {
            q.on_event(&r);
        }
        let out = q.flush(10);
        let l = out.iter().find(|s| s.metric == "linkability").unwrap();
        assert_eq!(l.value.point(), Some(1.0));
    }
}
