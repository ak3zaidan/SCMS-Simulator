//! Verification policies (06-node-models.md §2.1, 03-interfaces.md §6).
//!
//! A receiver in dense traffic cannot verify everything. The reference OBU of §7.2
//! publishes a verification engine at 2,500/s and would manage; the AURIX variant of §7.4
//! publishes 100/s against a system-level requirement of "at least 100 signatures per
//! 100 ms interval (>= 1 kHz)" for a hundred neighbours [R7 §H2: NDSS 2024 §VII-B], and
//! would not. What such a node does instead is the policy, and it is a swappable model
//! with a card because the three published answers disagree about what to give up:
//!
//! * **verify-all** verifies in arrival order and drops what will not fit. Nothing
//!   unverified reaches an application, and the load is whatever the channel delivers.
//! * **on-demand** verifies only what a safety application says it cares about, and
//!   delivers the rest flagged. [Krishnan & Weimerskirch, "Verify-on-Demand", SAE Int. J.
//!   Passenger Cars 4:536-546, 2011, via R7 §H1.]
//! * **prioritized** orders the queue by relevance and proximity and sheds the oldest
//!   when it overflows, on the argument that a stale safety message is worth less than a
//!   fresh one.
//!
//! Every decision is reported, with its reason, so that `unverified_ratio` and
//! `verify_drops` are exact rather than inferred — which is why [`VerifyDecision`] carries
//! a reason even when the answer is "verify".

use v2xw_core::belief::PositionEstimate;
use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;
use v2xw_msg::MsgType;

use crate::queue::DropCause;
use crate::stores::NeighborTable;

/// What a policy is allowed to know about a message before deciding.
///
/// Everything here is either on the wire or in the node's own stores. There is no true
/// sender, no true position and no actor id, because a policy runs as the node
/// (invariant I-C2).
#[derive(Debug, Clone, PartialEq)]
pub struct RxSummary {
    /// The signer's certificate digest, or `None` when the SPDU named a certificate this
    /// node does not hold.
    pub signer: Option<v2xw_msg::sec_types::HashedId8>,
    /// What kind of message it claims to be.
    pub msg_type: MsgType,
    /// How many bytes arrived.
    pub bytes: u32,
    /// When the node believes it received it.
    pub received_at: SimTime,
    /// The position the message claims, when it carries one.
    pub claimed_pos: Option<v2xw_core::geom::Vec3>,
    /// A relevance score in `0..=1` set by a safety application, when one has looked.
    pub relevance: Option<f64>,
}

/// The node state a policy may consult.
#[derive(Debug, Clone, Copy)]
pub struct PolicyView<'a> {
    /// The node's own belief about where it is.
    pub position: &'a PositionEstimate,
    /// What it has heard so far.
    pub neighbors: &'a NeighborTable,
    /// How full the verify queue is.
    pub queue_depth: usize,
    /// How full it may get.
    pub queue_capacity: usize,
}

/// What a policy decided, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyDecision {
    /// Verify it. Higher `priority` is served first.
    Verify {
        /// Service order within the queue.
        priority: u32,
        /// Why it was chosen.
        reason: VerifyReason,
    },
    /// Deliver it to the applications without a signature check, flagged.
    DeliverUnverified {
        /// Why the check was skipped.
        reason: SkipReason,
    },
    /// Discard it.
    Drop {
        /// Which drop counter this lands in.
        cause: DropCause,
    },
}

/// Why a message was chosen for verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerifyReason {
    /// The policy verifies everything.
    PolicyVerifiesAll,
    /// A safety application marked it relevant.
    ApplicationRelevant,
    /// It came from close enough to matter.
    Proximate,
    /// The signer is unknown, so the node cannot trust it without checking.
    UnknownSigner,
}

/// Why a signature check was skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// No application asked for it.
    NotRelevant,
    /// The signer is already a verified neighbour and this is a routine update.
    KnownVerifiedSigner,
}

/// One verification task and its fate — the `node.verify` record (03-interfaces.md §14,
/// 06-node-models.md §2.1).
///
/// # Its shape is the reader's
///
/// The record decodes as `v2xw_metrics::channels::NodeVerifyView`: `t_enqueue`, the
/// optional `t_start` and `t_done`, and an `outcome` of `valid`, `invalid`, `dropped` or
/// `skipped`. It used to be a *decision* log — `t_ns` and an outcome of `verify`,
/// `unverified` or `drop` — which no reader of the channel could decode: every metric on
/// `node.verify` read nothing, and the live server counted every record as undecodable.
///
/// A task is recorded once, when its fate is known: a policy's skip or drop at the
/// decision, a verification when it runs ([`VerifyDecisionRecord::verified`]). Every
/// instant is on the node's own clock, which is what the node can know; durations
/// between them are unaffected by its offset.
#[derive(Debug, Clone, serde::Serialize)]
pub struct VerifyDecisionRecord {
    /// Which node decided.
    pub node: NodeId,
    /// When the message was offered to the policy, on the node's own clock.
    pub t_enqueue: SimTime,
    /// When the signature check started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t_start: Option<SimTime>,
    /// When it finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t_done: Option<SimTime>,
    /// Which policy.
    pub policy: &'static str,
    /// What kind of message.
    pub msg_type: &'static str,
    /// `valid`, `invalid`, `dropped` or `skipped`.
    pub outcome: &'static str,
    /// The reason, in the policy's own vocabulary.
    pub reason: &'static str,
    /// The queue priority, for a message that was verified.
    pub priority: u32,
    /// The primitive, for a message that was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primitive: Option<&'static str>,
    /// The modelled cost, µs, for a message that was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_us: Option<u64>,
    /// How many tasks were waiting ahead of this one when it was queued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_depth: Option<u64>,
}

impl VerifyDecisionRecord {
    /// The record of a decision that settles the message's fate at once — a skip or a
    /// drop — or `None` for a decision to verify, whose record is written when the check
    /// runs.
    #[must_use]
    pub fn decided(
        node: NodeId,
        at: SimTime,
        policy: &'static str,
        msg_type: &'static str,
        decision: &VerifyDecision,
    ) -> Option<Self> {
        let (outcome, reason) = match decision {
            VerifyDecision::Verify { .. } => return None,
            VerifyDecision::DeliverUnverified { reason } => (
                "skipped",
                match reason {
                    SkipReason::NotRelevant => "not-relevant",
                    SkipReason::KnownVerifiedSigner => "known-verified-signer",
                },
            ),
            VerifyDecision::Drop { cause } => ("dropped", cause.as_str()),
        };
        Some(Self {
            node,
            t_enqueue: at,
            t_start: None,
            t_done: None,
            policy,
            msg_type,
            outcome,
            reason,
            priority: 0,
            primitive: None,
            cost_us: None,
            queue_depth: None,
        })
    }

    /// The record of a task the queue refused or evicted.
    #[must_use]
    pub fn overflowed(
        node: NodeId,
        at: SimTime,
        policy: &'static str,
        msg_type: &'static str,
    ) -> Self {
        Self {
            node,
            t_enqueue: at,
            t_start: None,
            t_done: None,
            policy,
            msg_type,
            outcome: "dropped",
            reason: DropCause::VerifyOverflow.as_str(),
            priority: 0,
            primitive: None,
            cost_us: None,
            queue_depth: None,
        }
    }

    /// The record of a signature check that ran.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn verified(
        node: NodeId,
        enqueued: SimTime,
        start: SimTime,
        done: SimTime,
        policy: &'static str,
        msg_type: &'static str,
        primitive: &'static str,
        outcome: &'static str,
        queue_depth: u64,
    ) -> Self {
        Self {
            node,
            t_enqueue: enqueued,
            t_start: Some(start),
            t_done: Some(done),
            policy,
            msg_type,
            outcome,
            reason: "verified",
            priority: 0,
            primitive: Some(primitive),
            cost_us: Some(done.saturating_sub(start) / 1_000),
            queue_depth: Some(queue_depth),
        }
    }
}

impl v2xw_core::ctx::Record for VerifyDecisionRecord {
    const CHANNEL: &'static str = "node.verify";
    const VISIBILITY: v2xw_core::ctx::Visibility = v2xw_core::ctx::Visibility::Node;
}

/// Decides what a node does with each arriving message.
pub trait VerificationPolicy: Model {
    /// The decision for one message.
    fn decide(&mut self, m: &RxSummary, view: &PolicyView<'_>) -> VerifyDecision;

    /// The code this policy has in the telemetry record (§3.5.2 offset 194):
    /// `0` verify-all, `1` on-demand, `2` prioritized.
    fn code(&self) -> u8;

    /// Whether an overflowing queue sheds its oldest entry rather than refusing the
    /// newcomer.
    fn oldest_drop(&self) -> bool {
        false
    }
}

/// Model id of the verify-everything policy.
pub const VERIFY_ALL_ID: &str = "verification-policy/verify-all";
/// Model id of the verify-on-demand policy.
pub const ON_DEMAND_ID: &str = "verification-policy/on-demand";
/// Model id of the prioritised policy.
pub const PRIORITIZED_ID: &str = "verification-policy/prioritized";

/// Verify everything, in arrival order.
#[derive(Debug, Clone)]
pub struct VerifyAll {
    card: ModelCard,
}

impl Default for VerifyAll {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyAll {
    /// The policy.
    pub fn new() -> Self {
        let mut card = ModelCard::new(
            VERIFY_ALL_ID,
            Family::VerificationPolicy,
            "0.1.0",
            "Verify every received SPDU in arrival order; drop on queue overflow. \
             Nothing unverified reaches an application.",
        );
        card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
        card.sources.push(Source::new(
            SourceKind::Standard,
            "IEEE 1609.2-2022 clause 5.2: the default receiver behaviour, against which \
             the two relaxations below are measured",
        ));
        card.assumptions.push(
            "An overflowing queue refuses the newcomer rather than shedding the oldest, so \
             the messages that survive are the ones that arrived while there was room."
                .to_string(),
        );
        card.validation = Validation::new(ValidationStatus::UnitTested);
        VerifyAll { card }
    }
}

impl Model for VerifyAll {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl VerificationPolicy for VerifyAll {
    fn decide(&mut self, _m: &RxSummary, view: &PolicyView<'_>) -> VerifyDecision {
        if view.queue_depth >= view.queue_capacity {
            return VerifyDecision::Drop {
                cause: DropCause::VerifyOverflow,
            };
        }
        VerifyDecision::Verify {
            priority: 0,
            reason: VerifyReason::PolicyVerifiesAll,
        }
    }

    fn code(&self) -> u8 {
        0
    }
}

/// Verify only what an application marked relevant; deliver the rest flagged.
#[derive(Debug, Clone)]
pub struct OnDemand {
    threshold: f64,
    card: ModelCard,
}

impl OnDemand {
    /// The policy, verifying anything whose relevance is at or above `threshold`.
    ///
    /// The threshold is a scenario parameter and the card says so: the Verify-on-Demand
    /// paper establishes the *mechanism*, not a number, and no source in the design set
    /// gives one.
    pub fn new(threshold: f64) -> Self {
        let mut card = ModelCard::new(
            ON_DEMAND_ID,
            Family::VerificationPolicy,
            "0.1.0",
            "Verify only the messages a safety application marks relevant; deliver the \
             rest to the applications flagged as unverified.",
        );
        card.tier = vec![Tier::Medium, Tier::High];
        let mut p = Parameter::new(
            "relevance_threshold",
            "ratio",
            serde_json::json!(threshold),
            Source::todo_calibrate("the relevance score above which a message is verified"),
        );
        p.range = Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]);
        p.calibration = Some(
            "Krishnan & Weimerskirch establish verify-on-demand as a mechanism and do not \
             publish an operating point [R7 §H1: SAE Int. J. Passenger Cars 4:536-546, \
             2011]. Sweep the threshold against the false-negative rate of the safety \
             applications a scenario runs and pick the knee; record the sweep in the \
             manifest."
                .to_string(),
        );
        card.parameters.push(p);
        card.sources.push(Source::new(
            SourceKind::Paper,
            "Krishnan & Weimerskirch, 'Verify-on-Demand', SAE Int. J. Passenger Cars - \
             Mech. Syst. 4:536-546, 2011 [R7 §H1, cited via ePrint 2022/133 ref. 7]",
        ));
        card.limitations.push(
            "A message delivered unverified is delivered: an application acting on it is \
             acting on an unauthenticated claim, and the `unverified_ratio` telemetry \
             field is what makes that visible."
                .to_string(),
        );
        card.validation = Validation::new(ValidationStatus::LiteratureChecked);
        OnDemand { threshold, card }
    }
}

impl Model for OnDemand {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl VerificationPolicy for OnDemand {
    fn decide(&mut self, m: &RxSummary, view: &PolicyView<'_>) -> VerifyDecision {
        // An unknown signer is verified whatever the applications think: the node has no
        // basis at all for the claim, and a policy that skipped it would let an attacker
        // stay unverified simply by being uninteresting.
        let unknown = match &m.signer {
            None => true,
            Some(s) => view.neighbors.get(s).is_none(),
        };
        let relevant = m.relevance.is_some_and(|r| r >= self.threshold);
        if !unknown && !relevant {
            return VerifyDecision::DeliverUnverified {
                reason: SkipReason::NotRelevant,
            };
        }
        if view.queue_depth >= view.queue_capacity {
            return VerifyDecision::Drop {
                cause: DropCause::VerifyOverflow,
            };
        }
        VerifyDecision::Verify {
            priority: 0,
            reason: if unknown {
                VerifyReason::UnknownSigner
            } else {
                VerifyReason::ApplicationRelevant
            },
        }
    }

    fn code(&self) -> u8 {
        1
    }
}

/// Order the queue by relevance and proximity; shed the oldest on overflow.
#[derive(Debug, Clone)]
pub struct Prioritized {
    horizon_m: f64,
    card: ModelCard,
}

impl Prioritized {
    /// The policy, with the distance beyond which proximity contributes nothing.
    pub fn new(horizon_m: f64) -> Self {
        let mut card = ModelCard::new(
            PRIORITIZED_ID,
            Family::VerificationPolicy,
            "0.1.0",
            "Order the verification queue by application relevance and by how close the \
             sender claims to be; drop the oldest entry when the queue overflows.",
        );
        card.tier = vec![Tier::Medium, Tier::High];
        let mut p = Parameter::new(
            "horizon_m",
            "m",
            serde_json::json!(horizon_m),
            Source::todo_calibrate("distance beyond which proximity adds no priority"),
        );
        p.range = Some(vec![serde_json::json!(1.0), serde_json::json!(10000.0)]);
        p.calibration = Some(
            "06-node-models §2.1 specifies 'priority by relevance score and distance' \
             without a horizon, and no source in the design set gives one. Sweep it \
             against the time-to-collision distribution of the scenario's safety \
             applications; a first bracket is the communication range the PHY model \
             reports at the scenario's transmit power."
                .to_string(),
        );
        card.parameters.push(p);
        card.equations.push(v2xw_core::card::Equation::new(
            "priority",
            "priority = round(1000 * relevance) + round(1000 * max(0, 1 - d/horizon)), \
             where d is the distance from the node's believed position to the claimed one",
        ));
        card.sources.push(Source::new(
            SourceKind::TodoCalibrate,
            "06-node-models.md §2.1 (prioritized policy: 'priority by relevance score and \
             distance; oldest-drop when the queue exceeds queue_depth')",
        ));
        card.assumptions.push(
            "Distance is computed from the *claimed* position, because that is all a \
             receiver has before verification — which means an attacker can raise its own \
             priority by claiming to be close, and that is a real property of the policy \
             rather than a modelling error."
                .to_string(),
        );
        card.validation = Validation::new(ValidationStatus::UnitTested);
        Prioritized { horizon_m, card }
    }

    /// The priority this policy assigns.
    ///
    /// Both terms are quantised before they are combined and compared, per build decision
    /// D10: a priority decides the service order, the service order decides what gets
    /// dropped, and a transcendental one unit in the last place away would otherwise be
    /// able to change which message a run verified.
    pub fn priority(&self, m: &RxSummary, own: &PositionEstimate) -> u32 {
        let relevance = m.relevance.unwrap_or(0.0).clamp(0.0, 1.0);
        let proximity = match m.claimed_pos {
            None => 0.0,
            Some(p) => {
                let d = p.distance_2d(own.pos);
                if !d.is_finite() || self.horizon_m <= 0.0 {
                    0.0
                } else {
                    (1.0 - d / self.horizon_m).clamp(0.0, 1.0)
                }
            }
        };
        let r = v2xw_core::math::quantize_to(relevance * 1000.0, 1.0) as u32;
        let q = v2xw_core::math::quantize_to(proximity * 1000.0, 1.0) as u32;
        r.saturating_add(q)
    }
}

impl Model for Prioritized {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl VerificationPolicy for Prioritized {
    fn decide(&mut self, m: &RxSummary, view: &PolicyView<'_>) -> VerifyDecision {
        VerifyDecision::Verify {
            priority: self.priority(m, view.position),
            reason: if m.relevance.is_some_and(|r| r > 0.0) {
                VerifyReason::ApplicationRelevant
            } else {
                VerifyReason::Proximate
            },
        }
    }

    fn code(&self) -> u8 {
        2
    }

    fn oldest_drop(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stores::{VerificationState, pseudo_signer};
    use v2xw_core::geom::Vec3;

    fn own() -> PositionEstimate {
        PositionEstimate::no_fix(0)
    }

    fn summary(relevance: Option<f64>, at: Option<Vec3>, known: bool) -> RxSummary {
        RxSummary {
            signer: known.then(|| pseudo_signer(NodeId::new(7), 0)),
            msg_type: MsgType::Cam,
            bytes: 300,
            received_at: 0,
            claimed_pos: at,
            relevance,
        }
    }

    fn table(with_peer: bool) -> NeighborTable {
        let mut t = NeighborTable::new(8);
        if with_peer {
            t.observe(crate::stores::Neighbor {
                signer: pseudo_signer(NodeId::new(7), 0),
                claimed_pos: Vec3::ZERO,
                claimed_speed_mps: 0.0,
                claimed_heading_rad: 0.0,
                claimed_generation_time: 0,
                last_heard: 0,
                messages: 1,
                state: VerificationState::Verified,
            });
        }
        t
    }

    fn view<'a>(
        pos: &'a PositionEstimate,
        nbrs: &'a NeighborTable,
        depth: usize,
        cap: usize,
    ) -> PolicyView<'a> {
        PolicyView {
            position: pos,
            neighbors: nbrs,
            queue_depth: depth,
            queue_capacity: cap,
        }
    }

    /// verify-all verifies, and drops only when there is no room.
    #[test]
    fn verify_all_verifies_until_the_queue_is_full() {
        let mut p = VerifyAll::new();
        let (o, t) = (own(), table(true));
        assert!(matches!(
            p.decide(&summary(None, None, true), &view(&o, &t, 0, 4)),
            VerifyDecision::Verify {
                reason: VerifyReason::PolicyVerifiesAll,
                ..
            }
        ));
        assert_eq!(
            p.decide(&summary(None, None, true), &view(&o, &t, 4, 4)),
            VerifyDecision::Drop {
                cause: DropCause::VerifyOverflow
            }
        );
        assert_eq!(p.code(), 0);
        assert!(!p.oldest_drop());
    }

    /// on-demand skips the routine update from a known peer, verifies the relevant one,
    /// and always verifies an unknown signer — which is the clause that stops an attacker
    /// avoiding scrutiny by being uninteresting.
    #[test]
    fn on_demand_skips_the_routine_and_never_skips_a_stranger() {
        let mut p = OnDemand::new(0.5);
        let o = own();
        let known = table(true);
        let empty = table(false);

        assert_eq!(
            p.decide(&summary(Some(0.1), None, true), &view(&o, &known, 0, 8)),
            VerifyDecision::DeliverUnverified {
                reason: SkipReason::NotRelevant
            }
        );
        assert!(matches!(
            p.decide(&summary(Some(0.9), None, true), &view(&o, &known, 0, 8)),
            VerifyDecision::Verify {
                reason: VerifyReason::ApplicationRelevant,
                ..
            }
        ));
        assert!(matches!(
            p.decide(&summary(Some(0.0), None, true), &view(&o, &empty, 0, 8)),
            VerifyDecision::Verify {
                reason: VerifyReason::UnknownSigner,
                ..
            }
        ));
        assert!(matches!(
            p.decide(&summary(None, None, false), &view(&o, &known, 0, 8)),
            VerifyDecision::Verify {
                reason: VerifyReason::UnknownSigner,
                ..
            }
        ));
        assert_eq!(p.code(), 1);
    }

    /// The prioritised order is relevance first, proximity second, and a claim from
    /// beyond the horizon contributes nothing.
    #[test]
    fn prioritized_orders_by_relevance_then_proximity() {
        let p = Prioritized::new(300.0);
        let o = own();
        let near = summary(None, Some(Vec3::new(30.0, 0.0, 0.0)), true);
        let far = summary(None, Some(Vec3::new(280.0, 0.0, 0.0)), true);
        let beyond = summary(None, Some(Vec3::new(1000.0, 0.0, 0.0)), true);
        let relevant_far = summary(Some(1.0), Some(Vec3::new(280.0, 0.0, 0.0)), true);

        assert_eq!(p.priority(&near, &o), 900);
        assert_eq!(p.priority(&far, &o), 67);
        assert_eq!(p.priority(&beyond, &o), 0);
        assert_eq!(p.priority(&relevant_far, &o), 1067);
        assert!(p.priority(&relevant_far, &o) > p.priority(&near, &o));
    }

    /// The prioritised policy sheds the oldest rather than refusing the newcomer, which
    /// is the one behavioural difference from verify-all under overload.
    #[test]
    fn prioritized_sheds_the_oldest() {
        let mut p = Prioritized::new(300.0);
        let (o, t) = (own(), table(true));
        // Even at capacity it admits: the queue evicts rather than the policy refusing.
        assert!(matches!(
            p.decide(&summary(None, None, true), &view(&o, &t, 8, 8)),
            VerifyDecision::Verify { .. }
        ));
        assert!(p.oldest_drop());
        assert_eq!(p.code(), 2);
    }

    /// Each policy's card validates and carries its calibration plan, so all three
    /// register (03-interfaces.md §12, rule R1).
    #[test]
    fn every_policy_card_validates() {
        VerifyAll::new().card().validate().expect("verify-all");
        OnDemand::new(0.5).card().validate().expect("on-demand");
        Prioritized::new(300.0)
            .card()
            .validate()
            .expect("prioritized");
        assert_eq!(OnDemand::new(0.5).card().todo_calibrate().count(), 1);
        assert_eq!(Prioritized::new(300.0).card().todo_calibrate().count(), 1);
        // The three codes are distinct and match §3.5.2's enumeration.
        assert_eq!(
            [
                VerifyAll::new().code(),
                OnDemand::new(0.5).code(),
                Prioritized::new(300.0).code()
            ],
            [0, 1, 2]
        );
    }
}
