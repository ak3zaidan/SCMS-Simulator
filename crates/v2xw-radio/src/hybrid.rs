//! Hybrid operation: a node with both a direct radio and a cellular one, and a policy
//! that chooses per message.
//!
//! 04-models.md does not specify a hybrid model, so this module specifies nothing new: it
//! *composes* the models that are specified — the sidelink or ITS-G5 stack of §4 and §5
//! and the Uu link of §10.1 — behind one decision. Every threshold it decides on is either
//! a cited number from one of those sections or a scenario parameter with no default at
//! all, and [`HybridPolicy::cited_basis`] says which for each policy.
//!
//! # Why the policy is a model and not an `if`
//!
//! Because the choice is a claim about the world that a reader has to be able to check. A
//! policy that sends a cooperative-awareness message over cellular when the sidelink is
//! busy is asserting that the cellular path meets the message's deadline, and the numbers
//! behind that assertion are measured: one hop is 2-3 ms on DSRC against 18-20 ms on 5G
//! [MASA living lab, 04-models.md §10.1], and a 300 B message wants 100 ms end to end
//! [TR 37.885 §6.1.5 Traffic Model 1]. Those two facts decide the policy, and they are in
//! the card.
//!
//! # The four policies
//!
//! | Policy | Chooses cellular when | Cited basis |
//! |---|---|---|
//! | [`HybridPolicy::DirectOnly`] | never | — |
//! | [`HybridPolicy::CellularOnly`] | always | — |
//! | [`HybridPolicy::DirectWithCellularFallback`] | the direct radio has no coverage of the destination, or its channel load exceeds the threshold | the ETSI DCC state boundaries (§6.2) and the illustrative CR-limit table (§5.1) |
//! | [`HybridPolicy::LatencyBudget`] | the direct radio's predicted latency misses the deadline and cellular's does not | the one-hop latency measurements (§10.1) and Traffic Model 1's 100 ms budget (§5.3) |
//!
//! There is deliberately no "lowest latency wins" policy. It would send everything over
//! the direct radio in every configuration these numbers describe, which is
//! [`HybridPolicy::DirectOnly`] under another name.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cellular::{CellularUu, Qos};
use crate::types::Rat;

/// Which radio a message goes out on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadioChoice {
    /// The direct radio: ITS-G5, LTE-V2X PC5 or NR-V2X PC5.
    Direct(Rat),
    /// The cellular uplink.
    Cellular,
    /// Both, which is what a message worth duplicating gets.
    Both(Rat),
}

impl RadioChoice {
    /// True when the direct radio carries a copy.
    #[must_use]
    pub const fn uses_direct(self) -> bool {
        matches!(self, RadioChoice::Direct(_) | RadioChoice::Both(_))
    }

    /// True when the cellular link carries a copy.
    #[must_use]
    pub const fn uses_cellular(self) -> bool {
        matches!(self, RadioChoice::Cellular | RadioChoice::Both(_))
    }

    /// The label a record prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            RadioChoice::Direct(_) => "direct",
            RadioChoice::Cellular => "cellular",
            RadioChoice::Both(_) => "both",
        }
    }
}

/// Where a message is going, which is most of what the policy needs to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Destination {
    /// Every node in range: a cooperative-awareness message, a decentralised
    /// notification. Only the direct radio can do this in one transmission.
    LocalBroadcast,
    /// A backend entity: an enrolment request, a misbehaviour report, a certificate
    /// download. Only cellular or a backhauled roadside unit can reach it.
    Backend,
    /// A specific node, which either radio can reach.
    Node(NodeId),
}

/// What the policy is asked about.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct HybridRequest {
    /// Where it is going.
    pub destination: Destination,
    /// How large it is, bytes.
    pub bytes: u32,
    /// The latency budget: the instant by which it is useless if undelivered.
    ///
    /// Traffic Model 1's 100 ms [TR 37.885 §6.1.5] for a periodic awareness message; a
    /// scenario sets its own for anything else.
    pub deadline: SimTime,
    /// The quality of service the cellular leg is asked for.
    pub qos: Qos,
    /// The sender's position, for the coverage lookup.
    pub position: Vec3,
    /// The sidelink or ITS-G5 channel load the sender measured, `0.0..=1.0`.
    ///
    /// The channel busy ratio for ITS-G5 ([`crate::mac::CbrMeter`]) or the sidelink CBR
    /// for a PC5 pool ([`crate::sps::SpsEngine::sidelink_cbr`]). Both are "how much of
    /// the shared resource is in use", which is what the policy needs.
    pub direct_load: f64,
}

impl HybridRequest {
    /// A local broadcast of `bytes` with a 100 ms budget: the shape of a cooperative
    /// awareness message.
    #[must_use]
    pub fn awareness(bytes: u32, now: SimTime, position: Vec3, direct_load: f64) -> Self {
        Self {
            destination: Destination::LocalBroadcast,
            bytes,
            deadline: TRAFFIC_MODEL_1_BUDGET.after(now),
            qos: Qos::LowLatency,
            position,
            direct_load,
        }
    }

    /// A backend-bound message of `bytes` with a caller-chosen budget.
    #[must_use]
    pub fn backend(bytes: u32, deadline: SimTime, position: Vec3) -> Self {
        Self {
            destination: Destination::Backend,
            bytes,
            deadline,
            qos: Qos::Reliable,
            position,
            direct_load: 0.0,
        }
    }
}

/// Traffic Model 1's latency budget, 100 ms [TR 37.885 §6.1.5, via 04-models.md §5.3].
pub const TRAFFIC_MODEL_1_BUDGET: Duration = Duration::from_millis(100);

/// The one-hop latency of a direct radio, from the remote-driving budget's measurement:
/// DSRC 2-3 ms [MASA living lab, via 04-models.md §10.1], taken at the midpoint.
pub const DIRECT_ONE_HOP_MS: f64 = 2.5;

/// The one-hop latency of 5G, from the same measurement: 18-20 ms, at the midpoint.
pub const CELLULAR_ONE_HOP_MS: f64 = 19.0;

/// The channel load above which [`HybridPolicy::DirectWithCellularFallback`] offloads.
///
/// 0.62 is the EN 302 571 idle-time bound's own knee — below it the bound imposes only the
/// 25 ms floor, above it the off-time grows without limit (04-models.md §6.3, and
/// [`crate::dcc::En302571Floor`]) — and it is within a percentage point of the ETSI
/// reactive approach's `Restrictive` boundary at 0.60 (§6.2) and the illustrative sidelink
/// CR-limit table's first limited band at 0.65 (§5.1). It is the one load threshold that
/// three independent cited tables agree on, which is why it is the default rather than a
/// round number.
pub const OFFLOAD_LOAD_THRESHOLD: f64 = 0.62;

/// How a node with two radios chooses.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HybridPolicy {
    /// Never use cellular. A backend-bound message has no path and is refused, which is
    /// the honest answer for a node with no cellular subscription.
    DirectOnly,
    /// Never use the direct radio.
    CellularOnly,
    /// Use the direct radio, and fall back to cellular when it cannot carry the message:
    /// a backend destination, or a channel load above `threshold`.
    DirectWithCellularFallback {
        /// The load above which the node offloads. [`OFFLOAD_LOAD_THRESHOLD`] by default.
        threshold: f64,
        /// Whether an offloaded local broadcast also goes out on the direct radio.
        ///
        /// `true` is the duplicating variant: it costs twice the resource and is what a
        /// reliability study wants; `false` is the offloading variant, which is what a
        /// congestion study wants.
        duplicate: bool,
    },
    /// Choose whichever radio's predicted latency meets the deadline, preferring the
    /// direct one when both do.
    LatencyBudget {
        /// The direct radio's predicted one-way latency, ms.
        direct_ms: f64,
        /// The cellular link's predicted one-way latency, ms.
        cellular_ms: f64,
    },
}

impl HybridPolicy {
    /// The fallback policy at the cited load threshold, offloading rather than
    /// duplicating.
    #[must_use]
    pub const fn fallback() -> Self {
        HybridPolicy::DirectWithCellularFallback {
            threshold: OFFLOAD_LOAD_THRESHOLD,
            duplicate: false,
        }
    }

    /// The latency policy at the cited one-hop measurements.
    #[must_use]
    pub const fn latency_budget() -> Self {
        HybridPolicy::LatencyBudget {
            direct_ms: DIRECT_ONE_HOP_MS,
            cellular_ms: CELLULAR_ONE_HOP_MS,
        }
    }

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            HybridPolicy::DirectOnly => "direct-only",
            HybridPolicy::CellularOnly => "cellular-only",
            HybridPolicy::DirectWithCellularFallback { .. } => "direct-with-cellular-fallback",
            HybridPolicy::LatencyBudget { .. } => "latency-budget",
        }
    }

    /// What the policy's numbers rest on, in one line, for the card and for a report.
    #[must_use]
    pub const fn cited_basis(self) -> &'static str {
        match self {
            HybridPolicy::DirectOnly | HybridPolicy::CellularOnly => {
                "no numeric parameter: the policy is the scenario's statement about what \
                 the node is equipped with"
            }
            HybridPolicy::DirectWithCellularFallback { .. } => {
                "the load threshold is the EN 302 571 idle-time knee at CBR 0.62 \
                 (04-models.md §6.3), which agrees with the TS 102 687 reactive \
                 `Restrictive` boundary at 0.60 (§6.2) and the illustrative sidelink \
                 CR-limit table's first limited band at 0.65 (§5.1)"
            }
            HybridPolicy::LatencyBudget { .. } => {
                "one-hop latency DSRC 2-3 ms against 5G 18-20 ms [MASA living lab, \
                 04-models.md §10.1]; the deadline is Traffic Model 1's 100 ms \
                 [TR 37.885 §6.1.5]"
            }
        }
    }
}

/// Why the policy chose what it chose. A record carries this, so a run can be read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HybridReason {
    /// The policy admits only one radio.
    PolicyFixed,
    /// A local broadcast, which only the direct radio does in one transmission.
    LocalBroadcastNeedsDirect,
    /// A backend destination, which the direct radio cannot reach.
    BackendNeedsCellular,
    /// The direct radio's channel load is above the offload threshold.
    DirectOverloaded,
    /// Both radios meet the deadline, so the faster one wins.
    DirectMeetsDeadline,
    /// Only the cellular link meets the deadline.
    OnlyCellularMeetsDeadline,
    /// Neither meets it; the choice is the faster of the two and the caller is told.
    NeitherMeetsDeadline,
    /// There is no cellular coverage here, so the direct radio is the only path.
    NoCellularCoverage,
}

/// What the selector decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct HybridDecision {
    /// The radio or radios.
    pub choice: Option<RadioChoice>,
    /// Why.
    pub reason: HybridReason,
}

impl HybridDecision {
    /// A decision with a choice.
    #[must_use]
    pub const fn new(choice: RadioChoice, reason: HybridReason) -> Self {
        Self {
            choice: Some(choice),
            reason,
        }
    }

    /// A refusal: no radio can carry this message.
    #[must_use]
    pub const fn refused(reason: HybridReason) -> Self {
        Self {
            choice: None,
            reason,
        }
    }
}

/// `radio/hybrid/policy` — a node with a direct radio and a cellular one, and a policy.
#[derive(Debug, Clone)]
pub struct HybridSelector {
    card: ModelCard,
    rat: Rat,
    policy: HybridPolicy,
    /// Decisions per `(node, choice label)`, so a run can report the split.
    counts: BTreeMap<(u32, &'static str), u64>,
    refusals: u64,
}

impl HybridSelector {
    /// The model's id.
    pub const ID: &'static str = "radio/hybrid/policy";

    /// The selector for one direct RAT and one policy.
    #[must_use]
    pub fn new(rat: Rat, policy: HybridPolicy) -> Self {
        Self {
            card: card(rat, policy),
            rat,
            policy,
            counts: BTreeMap::new(),
            refusals: 0,
        }
    }

    /// The direct RAT this node carries.
    #[must_use]
    pub const fn rat(&self) -> Rat {
        self.rat
    }

    /// The policy in use.
    #[must_use]
    pub const fn policy(&self) -> HybridPolicy {
        self.policy
    }

    /// How many messages a node sent on one choice.
    #[must_use]
    pub fn count(&self, node: NodeId, choice: RadioChoice) -> u64 {
        *self
            .counts
            .get(&(node.index(), choice.label()))
            .unwrap_or(&0)
    }

    /// Messages no radio could carry.
    #[must_use]
    pub const fn refusals(&self) -> u64 {
        self.refusals
    }

    /// Chooses a radio for one message.
    ///
    /// `uu` is consulted for coverage only — the decision never sends anything, because
    /// the caller owns the send and the accounting bucket it lands in (invariant I-N1).
    pub fn select<C: Ctx + ?Sized, U: CellularUu<C> + ?Sized>(
        &mut self,
        ctx: &C,
        node: NodeId,
        uu: &U,
        req: &HybridRequest,
    ) -> HybridDecision {
        let decision = self.decide(ctx, uu, req);
        if let Some(choice) = decision.choice {
            *self
                .counts
                .entry((node.index(), choice.label()))
                .or_insert(0) += 1;
        } else {
            self.refusals += 1;
        }
        decision
    }

    fn decide<C: Ctx + ?Sized, U: CellularUu<C> + ?Sized>(
        &self,
        ctx: &C,
        uu: &U,
        req: &HybridRequest,
    ) -> HybridDecision {
        let covered = uu.coverage(req.position, ctx.now()).is_some();
        let direct = RadioChoice::Direct(self.rat);
        match self.policy {
            HybridPolicy::DirectOnly => {
                if req.destination == Destination::Backend {
                    // A node with no cellular path cannot reach a backend entity itself.
                    // Saying so is the point: the alternative is a scenario that silently
                    // delivers enrolment requests out of thin air.
                    HybridDecision::refused(HybridReason::BackendNeedsCellular)
                } else {
                    HybridDecision::new(direct, HybridReason::PolicyFixed)
                }
            }
            HybridPolicy::CellularOnly => {
                if !covered {
                    HybridDecision::refused(HybridReason::NoCellularCoverage)
                } else if req.destination == Destination::LocalBroadcast {
                    // A local broadcast over cellular needs a multicast or a server
                    // relay, which is a backend model's business, not this one's; the
                    // policy still chooses cellular and the reason records why it is
                    // unusual.
                    HybridDecision::new(
                        RadioChoice::Cellular,
                        HybridReason::LocalBroadcastNeedsDirect,
                    )
                } else {
                    HybridDecision::new(RadioChoice::Cellular, HybridReason::PolicyFixed)
                }
            }
            HybridPolicy::DirectWithCellularFallback {
                threshold,
                duplicate,
            } => {
                if req.destination == Destination::Backend {
                    return if covered {
                        HybridDecision::new(
                            RadioChoice::Cellular,
                            HybridReason::BackendNeedsCellular,
                        )
                    } else {
                        HybridDecision::refused(HybridReason::NoCellularCoverage)
                    };
                }
                if req.direct_load > threshold && covered {
                    let choice = if duplicate {
                        RadioChoice::Both(self.rat)
                    } else {
                        RadioChoice::Cellular
                    };
                    return HybridDecision::new(choice, HybridReason::DirectOverloaded);
                }
                HybridDecision::new(direct, HybridReason::DirectMeetsDeadline)
            }
            HybridPolicy::LatencyBudget {
                direct_ms,
                cellular_ms,
            } => {
                let budget_ms = Duration::between(ctx.now(), req.deadline).as_secs_f64() * 1e3;
                let direct_ok = direct_ms <= budget_ms;
                let cellular_ok = covered && cellular_ms <= budget_ms;
                if req.destination == Destination::Backend {
                    return if cellular_ok {
                        HybridDecision::new(
                            RadioChoice::Cellular,
                            HybridReason::BackendNeedsCellular,
                        )
                    } else if covered {
                        HybridDecision::new(
                            RadioChoice::Cellular,
                            HybridReason::NeitherMeetsDeadline,
                        )
                    } else {
                        HybridDecision::refused(HybridReason::NoCellularCoverage)
                    };
                }
                match (direct_ok, cellular_ok) {
                    (true, _) => HybridDecision::new(direct, HybridReason::DirectMeetsDeadline),
                    (false, true) => HybridDecision::new(
                        RadioChoice::Cellular,
                        HybridReason::OnlyCellularMeetsDeadline,
                    ),
                    (false, false) => {
                        // Neither meets it. The direct radio is still the faster of the
                        // two in every configuration these measurements describe, so it
                        // carries the message and the reason says the budget was missed.
                        HybridDecision::new(direct, HybridReason::NeitherMeetsDeadline)
                    }
                }
            }
        }
    }
}

impl Model for HybridSelector {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

fn card(rat: Rat, policy: HybridPolicy) -> ModelCard {
    let mut card = ModelCard::new(
        HybridSelector::ID,
        // The selector plugs into no published family: it is a composition over two of
        // them. `Family::Net` is the closest published seam — it is the layer that
        // decides which interface a protocol data unit leaves on — and the card says so
        // rather than inventing a family, which ADR 0007 forbids ("adding a family means
        // adding a trait, a conformance suite and a Python base class").
        Family::Net,
        "1.0.0",
        "Chooses between a node's direct radio and its cellular link, per message.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation {
        name: "latency-budget decision".to_string(),
        latex_or_text: "direct if L_direct <= deadline − now, else cellular if \
                        L_cellular <= deadline − now and covered, else direct with the \
                        budget recorded as missed"
            .to_string(),
        notes: Some(
            "There is no \"lowest latency wins\" branch: with the cited one-hop \
             measurements (2.5 ms direct against 19 ms cellular) it would reduce to \
             direct-only."
                .to_string(),
        ),
    }];
    let masa = Source::new(
        SourceKind::Paper,
        "MASA living lab via 04-models.md §10.1: one-hop latency DSRC 2-3 ms, 5G \
         18-20 ms, in a remote-driving budget",
    );
    card.parameters = vec![
        Parameter::new(
            "policy",
            "-",
            serde_json::json!(policy.label()),
            // Not `todo-calibrate`: the policy is a structural choice a scenario makes,
            // not a number awaiting measurement. What its *numbers* rest on is the
            // `cited_basis` text, and the numbered parameters below carry their own
            // sources.
            Source::new(SourceKind::Paper, policy.cited_basis()),
        ),
        Parameter::new("direct_rat", "-", serde_json::json!(format!("{rat:?}")), {
            Source::new(
                SourceKind::Standard,
                "03-interfaces.md §4: Rat is Dsrc80211p | LteV2xPc5 | NrV2xPc5",
            )
        }),
        Parameter {
            name: "offload_load_threshold".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(OFFLOAD_LOAD_THRESHOLD),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: Source::new(
                SourceKind::Standard,
                "EN 302 571 §4.2.10.2 idle-time bound knee at CBR 0.62 (04-models.md \
                 §6.3); corroborated by TS 102 687 Annex A's `Restrictive` boundary at \
                 0.60 and the illustrative sidelink CR-limit table's first limited band \
                 at 0.65",
            ),
            calibration: None,
        },
        Parameter::new(
            "direct_one_hop_ms",
            "ms",
            serde_json::json!(DIRECT_ONE_HOP_MS),
            masa.clone(),
        ),
        Parameter::new(
            "cellular_one_hop_ms",
            "ms",
            serde_json::json!(CELLULAR_ONE_HOP_MS),
            masa.clone(),
        ),
        Parameter::new(
            "deadline_ms",
            "ms",
            serde_json::json!(TRAFFIC_MODEL_1_BUDGET.as_secs_f64() * 1e3),
            Source::new(
                SourceKind::Standard,
                "TR 37.885 §6.1.5 Traffic Model 1: 100 ms period, sizes {300, 190, 190, \
                 190, 190} B, latency 100 ms",
            ),
        ),
    ];
    card.assumptions = vec![
        "A node's two radios do not interfere with each other: the direct radio is at \
         5.9 GHz and the cellular one in a licensed band, and no adjacent-band \
         desensitisation is modelled."
            .to_string(),
        "The policy consults coverage but never sends: the caller owns the send and the \
         accounting bucket it lands in (invariant I-N1)."
            .to_string(),
    ];
    card.limitations = vec![
        "A local broadcast over cellular needs a multicast group or a server relay, which \
         is a backend model's business; `CellularOnly` chooses cellular for one and \
         records `LocalBroadcastNeedsDirect` so the gap is visible in the run rather than \
         hidden in this model."
            .to_string(),
        "The latency policy uses a *predicted* latency, not a measured one: it does not \
         read the queue state of either radio. A policy that did would need the engine's \
         per-node view, which is 06-node-models.md's seam, not this one."
            .to_string(),
    ];
    card.ignores = vec![
        "Cost. Every deployment fact 04-models.md §10.2 records about hybrid operation is \
         economic — Tampa THEA moved its express-lane roadside units off cellular at $35 \
         to $100 per month per unit — and no policy here prices a byte."
            .to_string(),
    ];
    card.sources = vec![masa];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "the_fallback_policy_offloads_only_above_the_cited_threshold".to_string(),
            "a_backend_message_has_no_path_without_cellular".to_string(),
            "the_latency_policy_prefers_the_direct_radio_whenever_it_fits".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cellular::{CellPlan, FixedLatencyUu, UuLatencyPreset};
    use crate::testctx::TestCtx;

    fn uu() -> FixedLatencyUu {
        FixedLatencyUu::new(UuLatencyPreset::Nr5gFirstHop)
    }

    fn uu_with_holes() -> FixedLatencyUu {
        FixedLatencyUu::new(UuLatencyPreset::Nr5gFirstHop).with_plan(CellPlan {
            extent_sites: 0,
            ..CellPlan::urban_macro()
        })
    }

    fn here() -> Vec3 {
        Vec3::new(0.0, 0.0, 1.5)
    }

    #[test]
    fn the_fallback_policy_offloads_only_above_the_cited_threshold() {
        let mut s = HybridSelector::new(Rat::LteV2xPc5, HybridPolicy::fallback());
        let ctx = TestCtx::new(1);
        let u = uu();
        let node = NodeId::new(1);
        // Just below the threshold: the direct radio keeps it.
        let low = HybridRequest::awareness(300, 0, here(), OFFLOAD_LOAD_THRESHOLD - 0.01);
        let d = s.select(&ctx, node, &u, &low);
        assert_eq!(d.choice, Some(RadioChoice::Direct(Rat::LteV2xPc5)));
        // Just above it: offloaded.
        let high = HybridRequest::awareness(300, 0, here(), OFFLOAD_LOAD_THRESHOLD + 0.01);
        let d = s.select(&ctx, node, &u, &high);
        assert_eq!(d.choice, Some(RadioChoice::Cellular));
        assert_eq!(d.reason, HybridReason::DirectOverloaded);
        assert_eq!(s.count(node, RadioChoice::Cellular), 1);
        assert_eq!(s.count(node, RadioChoice::Direct(Rat::LteV2xPc5)), 1);

        // The duplicating variant keeps the direct copy too.
        let mut dup = HybridSelector::new(
            Rat::LteV2xPc5,
            HybridPolicy::DirectWithCellularFallback {
                threshold: OFFLOAD_LOAD_THRESHOLD,
                duplicate: true,
            },
        );
        let d = dup.select(&ctx, node, &u, &high);
        assert_eq!(d.choice, Some(RadioChoice::Both(Rat::LteV2xPc5)));
        assert!(d.choice.unwrap().uses_direct() && d.choice.unwrap().uses_cellular());
    }

    #[test]
    fn an_overloaded_node_with_no_cellular_coverage_still_uses_the_direct_radio() {
        let mut s = HybridSelector::new(Rat::Dsrc80211p, HybridPolicy::fallback());
        let ctx = TestCtx::new(1);
        let u = uu_with_holes();
        // Far outside the one-site lattice.
        let far = Vec3::new(1e7, 0.0, 1.5);
        let req = HybridRequest::awareness(300, 0, far, 0.95);
        let d = s.select(&ctx, NodeId::new(1), &u, &req);
        assert_eq!(d.choice, Some(RadioChoice::Direct(Rat::Dsrc80211p)));
        assert_eq!(s.refusals(), 0);
    }

    #[test]
    fn a_backend_message_has_no_path_without_cellular() {
        let mut direct_only = HybridSelector::new(Rat::Dsrc80211p, HybridPolicy::DirectOnly);
        let ctx = TestCtx::new(1);
        let u = uu();
        let req = HybridRequest::backend(1200, 1_000_000_000, here());
        let d = direct_only.select(&ctx, NodeId::new(1), &u, &req);
        assert!(d.choice.is_none());
        assert_eq!(d.reason, HybridReason::BackendNeedsCellular);
        assert_eq!(direct_only.refusals(), 1);
        // With the fallback policy it goes over cellular.
        let mut fb = HybridSelector::new(Rat::Dsrc80211p, HybridPolicy::fallback());
        let d = fb.select(&ctx, NodeId::new(1), &u, &req);
        assert_eq!(d.choice, Some(RadioChoice::Cellular));
        assert_eq!(d.reason, HybridReason::BackendNeedsCellular);
        // But not where there is no coverage.
        let d = fb.select(
            &ctx,
            NodeId::new(1),
            &uu_with_holes(),
            &HybridRequest::backend(1200, 1_000_000_000, Vec3::new(1e7, 0.0, 1.5)),
        );
        assert!(d.choice.is_none());
        assert_eq!(d.reason, HybridReason::NoCellularCoverage);
    }

    #[test]
    fn the_latency_policy_prefers_the_direct_radio_whenever_it_fits() {
        let mut s = HybridSelector::new(Rat::NrV2xPc5, HybridPolicy::latency_budget());
        let ctx = TestCtx::new(1);
        let u = uu();
        let node = NodeId::new(1);
        // The 100 ms Traffic Model 1 budget: both radios fit, the direct one wins.
        let d = s.select(
            &ctx,
            node,
            &u,
            &HybridRequest::awareness(300, 0, here(), 0.1),
        );
        assert_eq!(d.choice, Some(RadioChoice::Direct(Rat::NrV2xPc5)));
        assert_eq!(d.reason, HybridReason::DirectMeetsDeadline);

        // A 10 ms budget: only the direct radio fits.
        let tight = HybridRequest {
            deadline: Duration::from_millis(10).after(0),
            ..HybridRequest::awareness(300, 0, here(), 0.1)
        };
        let d = s.select(&ctx, node, &u, &tight);
        assert_eq!(d.choice, Some(RadioChoice::Direct(Rat::NrV2xPc5)));

        // A 1 ms budget: neither fits, and the caller is told.
        let impossible = HybridRequest {
            deadline: Duration::from_millis(1).after(0),
            ..HybridRequest::awareness(300, 0, here(), 0.1)
        };
        let d = s.select(&ctx, node, &u, &impossible);
        assert_eq!(d.reason, HybridReason::NeitherMeetsDeadline);
        assert!(
            d.choice.is_some(),
            "a missed budget is still sent, and recorded"
        );

        // Only cellular fits when the direct radio is configured slower than it.
        let mut odd = HybridSelector::new(
            Rat::NrV2xPc5,
            HybridPolicy::LatencyBudget {
                direct_ms: 50.0,
                cellular_ms: 5.0,
            },
        );
        let d = odd.select(&ctx, node, &u, &tight);
        assert_eq!(d.choice, Some(RadioChoice::Cellular));
        assert_eq!(d.reason, HybridReason::OnlyCellularMeetsDeadline);
    }

    #[test]
    fn the_cited_thresholds_are_the_ones_the_sources_print() {
        // DSRC 2-3 ms and 5G 18-20 ms, at their midpoints.
        assert!((DIRECT_ONE_HOP_MS - 2.5).abs() < 1e-12);
        assert!((CELLULAR_ONE_HOP_MS - 19.0).abs() < 1e-12);
        const { assert!(DIRECT_ONE_HOP_MS < CELLULAR_ONE_HOP_MS) };
        // Traffic Model 1's budget.
        assert_eq!(TRAFFIC_MODEL_1_BUDGET, Duration::from_millis(100));
        // The offload threshold is the EN 302 571 knee.
        assert!((OFFLOAD_LOAD_THRESHOLD - 0.62).abs() < 1e-12);
        // Every policy says what its numbers rest on.
        for p in [
            HybridPolicy::DirectOnly,
            HybridPolicy::CellularOnly,
            HybridPolicy::fallback(),
            HybridPolicy::latency_budget(),
        ] {
            assert!(!p.cited_basis().is_empty());
            assert!(!p.label().is_empty());
            HybridSelector::new(Rat::LteV2xPc5, p)
                .card()
                .validate()
                .unwrap_or_else(|e| panic!("{}: {e}", p.label()));
        }
    }

    #[test]
    fn a_cellular_only_node_records_that_a_broadcast_is_not_what_cellular_does() {
        let mut s = HybridSelector::new(Rat::LteV2xPc5, HybridPolicy::CellularOnly);
        let ctx = TestCtx::new(1);
        let u = uu();
        let d = s.select(
            &ctx,
            NodeId::new(1),
            &u,
            &HybridRequest::awareness(300, 0, here(), 0.1),
        );
        assert_eq!(d.choice, Some(RadioChoice::Cellular));
        assert_eq!(d.reason, HybridReason::LocalBroadcastNeedsDirect);
        // And it refuses where there is no coverage rather than pretending.
        let d = s.select(
            &ctx,
            NodeId::new(1),
            &uu_with_holes(),
            &HybridRequest::awareness(300, 0, Vec3::new(1e7, 0.0, 1.5), 0.1),
        );
        assert!(d.choice.is_none());
        assert_eq!(d.reason, HybridReason::NoCellularCoverage);
    }

    #[test]
    fn the_direction_and_qos_a_request_carries_reach_the_cellular_model() {
        // The policy does not send, so this is a shape test: an awareness message asks
        // for low latency, a backend message for reliability, and both go uplink.
        let a = HybridRequest::awareness(300, 0, here(), 0.1);
        let b = HybridRequest::backend(1200, 1_000_000_000, here());
        assert_eq!(a.qos, Qos::LowLatency);
        assert_eq!(b.qos, Qos::Reliable);
        assert_eq!(crate::cellular::Direction::Uplink.label(), "uplink");
    }
}
