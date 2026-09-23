//! The compromised road-side unit: an attack on the infrastructure rather than on the air
//! (07-threats-and-detection.md §2.2, "Compromised RSU").
//!
//! # It is a node whose behaviour differs
//!
//! There is no special case for it anywhere in the engine. A compromised road-side unit is
//! an [`Attacker`] like any other: it edits what it is about to transmit before signing,
//! and everything downstream — signing, the MAC, DCC, the backhaul — is the ordinary path.
//! What makes it different is *what it holds*: [`crate::capability::CredentialAccess::CompromisedRsu`]
//! means its signatures verify against a certificate every receiver already trusts, and it
//! sits on the path reports take to the authority.
//!
//! # Two surfaces
//!
//! 1. **The air.** False SPaT and MAP, false CRL or CTL. The encoders and the broadcaster
//!    exist elsewhere — `v2xw_msg::j2735::spat` and `map`, and
//!    `v2xw_node::rsu::RsuRuntime` in its `SpatMapBroadcast` role, which takes real octets
//!    through `set_payload` — so what this model does is declare the falsification on
//!    [`crate::attack::Emission::infra`]: which message, which field, by how much. Two
//!    things are still missing and the card says so: the engine has to *apply* the declared
//!    claim when it builds that payload, and nothing consumes signal state yet, so the harm
//!    is recorded and not yet felt. A receiver quietly harmed by a message nobody sent
//!    would be worse than an attack that is honest about its reach.
//! 2. **The reporting path.** [`CompromisedRsu::on_forward`] is the attack that needs no
//!    unbuilt model at all: a report handed to this unit for forwarding is dropped, passed
//!    on, or **re-targeted at an innocent subject**. This is report poisoning mounted from
//!    infrastructure, and it is the one an operator cannot dismiss as theoretical, because
//!    an authority that exempts infrastructure from its reporter-reputation gate
//!    ([`crate::ma::LegacyWindow::trust_infrastructure`]) has no defence against it at
//!    all. Toggling that exemption is how a run measures what the gate is worth.
//!
//! # What it still cannot do
//!
//! It cannot see the world, it cannot name an actor, and it picks its victims from the
//! digests it has *heard*. A compromised unit with a wide receiver hears more vehicles
//! than a vehicle does, which is a property of where it is mounted and not a licence.

use std::collections::BTreeSet;

use crate::attack::{AttackAction, AttackFamily, Attacker, AttackerView, Emission, InfraClaim};
use crate::capability::{AttackSchedule, Capabilities, CredentialAccess};
use crate::cards::design;
use crate::ctx::ThreatCtx;
use crate::obs::StationType;
use crate::report::{ForgeryProfile, MisbehaviourReport, forge};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::SimTime;

/// The model id this module's card and its `gt.attack.action` records carry.
pub const MODEL_ID: &str = "threat/attacker/compromised-rsu";

/// What a compromised road-side unit does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RsuAttackKind {
    /// Broadcast a signal phase and timing message that does not match the signal.
    FalseSpat,
    /// Broadcast a topology message whose lane connections are wrong.
    FalseMap,
    /// Broadcast a revocation list with fabricated entries.
    FalseCrl,
    /// Broadcast a trust list with fabricated entries.
    FalseCtl,
    /// Drop the misbehaviour reports it is asked to forward to the authority.
    SuppressForwardedReports,
    /// Re-target the reports it forwards at innocent subjects.
    PoisonForwardedReports,
}

impl RsuAttackKind {
    /// Every kind, in declaration order.
    pub const ALL: [RsuAttackKind; 6] = [
        RsuAttackKind::FalseSpat,
        RsuAttackKind::FalseMap,
        RsuAttackKind::FalseCrl,
        RsuAttackKind::FalseCtl,
        RsuAttackKind::SuppressForwardedReports,
        RsuAttackKind::PoisonForwardedReports,
    ];

    /// The name a scenario file carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RsuAttackKind::FalseSpat => "FalseSpat",
            RsuAttackKind::FalseMap => "FalseMap",
            RsuAttackKind::FalseCrl => "FalseCrl",
            RsuAttackKind::FalseCtl => "FalseCtl",
            RsuAttackKind::SuppressForwardedReports => "SuppressForwardedReports",
            RsuAttackKind::PoisonForwardedReports => "PoisonForwardedReports",
        }
    }

    /// Parses a name. `None` for anything not in [`RsuAttackKind::ALL`].
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        RsuAttackKind::ALL.into_iter().find(|k| k.as_str() == name)
    }

    /// Every kind is in the infrastructure family: what distinguishes them is which
    /// infrastructure function is abused, not which field of a beacon is edited.
    #[must_use]
    pub const fn family(self) -> AttackFamily {
        AttackFamily::Infrastructure
    }

    /// Whether this kind acts on the air (`true`) or on the reporting path (`false`).
    #[must_use]
    pub const fn is_over_the_air(self) -> bool {
        matches!(
            self,
            RsuAttackKind::FalseSpat
                | RsuAttackKind::FalseMap
                | RsuAttackKind::FalseCrl
                | RsuAttackKind::FalseCtl
        )
    }

    /// The infrastructure message it falsifies, and the field, for the air-facing kinds.
    #[must_use]
    pub const fn message_and_field(self) -> Option<(&'static str, &'static str)> {
        match self {
            RsuAttackKind::FalseSpat => Some(("spat", "phase")),
            RsuAttackKind::FalseMap => Some(("map", "lane-connection")),
            RsuAttackKind::FalseCrl => Some(("crl", "entries")),
            RsuAttackKind::FalseCtl => Some(("ctl", "entries")),
            _ => None,
        }
    }
}

impl core::fmt::Display for RsuAttackKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the unit did with a report it was asked to forward.
#[derive(Debug, Clone, PartialEq)]
pub enum ForwardDecision {
    /// Forward it unchanged, which is what an honest unit always does.
    Forward,
    /// Drop it: the evidence never reaches the authority.
    Drop,
    /// Forward this report instead — same evidence, innocent subject.
    Replace(Box<MisbehaviourReport>),
}

/// Everything a compromised unit reads.
#[derive(Debug, Clone, PartialEq)]
pub struct RsuAttackParams {
    /// Which abuse this unit mounts.
    pub kind: RsuAttackKind,
    /// The unit's own pseudonym digest, hex: what it signs a forwarded report as.
    pub own_cert_digest: String,
    /// Seconds of fabricated green a false SPaT claims.
    pub false_green_s: f64,
    /// Fabricated entries a false CRL or CTL claims.
    pub fabricated_entries: f64,
    /// The probability a report handed to it for forwarding is dropped.
    ///
    /// The default is 1.0 — every report — because 07-threats-and-detection.md §2.2 says a
    /// compromised unit suppresses at the forwarding stage and gives no rate. One is the
    /// unambiguous rendering of "it suppresses"; a scenario that wants a partial adversary
    /// states the fraction it means rather than inheriting a number nobody chose.
    pub suppress_prob: f64,
    /// The probability a forwarded report is re-targeted at an innocent subject, on the
    /// same reasoning as [`Self::suppress_prob`].
    pub poison_prob: f64,
    /// The distributions the re-targeted report's fabricated evidence is drawn from.
    pub forgery: ForgeryProfile,
}

impl Default for RsuAttackParams {
    fn default() -> Self {
        Self {
            kind: RsuAttackKind::PoisonForwardedReports,
            own_cert_digest: String::new(),
            false_green_s: 1.0,
            fabricated_entries: 1.0,
            suppress_prob: 1.0,
            poison_prob: 1.0,
            forgery: ForgeryProfile::default(),
        }
    }
}

impl RsuAttackParams {
    /// The parameters for one kind, with the unit's own certificate digest.
    #[must_use]
    pub fn new(kind: RsuAttackKind, own_cert_digest: impl Into<String>) -> Self {
        Self {
            kind,
            own_cert_digest: own_cert_digest.into(),
            ..Self::default()
        }
    }
}

/// A compromised road-side unit.
#[derive(Debug, Clone)]
pub struct CompromisedRsu {
    card: ModelCard,
    params: RsuAttackParams,
    capabilities: Capabilities,
    schedule: AttackSchedule,
    node: NodeId,
    /// Digests this unit has heard: its victim pool, and nothing it was told.
    heard: BTreeSet<[u8; 8]>,
    forwarded: u64,
    dropped: u64,
    retargeted: u64,
    seq: u64,
}

impl CompromisedRsu {
    /// A compromised unit at `node`.
    ///
    /// The capabilities declare [`CredentialAccess::CompromisedRsu`] for `node`: its
    /// signatures verify, which is the whole point of compromising infrastructure rather
    /// than impersonating it.
    #[must_use]
    pub fn new(node: NodeId, params: RsuAttackParams, schedule: AttackSchedule) -> Self {
        let card = card(&params);
        let capabilities = Capabilities {
            credentials: CredentialAccess::CompromisedRsu(node),
            radio: crate::capability::RadioCaps::default(),
            knowledge: crate::capability::Knowledge {
                crl: true,
                map: true,
                neighbors_via_rx: true,
                sensing: false,
            },
            coordination: None,
            compute: None,
        };
        Self {
            card,
            params,
            capabilities,
            schedule,
            node,
            heard: BTreeSet::new(),
            forwarded: 0,
            dropped: 0,
            retargeted: 0,
            seq: 0,
        }
    }

    /// Which abuse it mounts.
    #[must_use]
    pub fn kind(&self) -> RsuAttackKind {
        self.params.kind
    }

    /// The parameters it reads.
    #[must_use]
    pub fn params(&self) -> &RsuAttackParams {
        &self.params
    }

    /// How many reports it passed on unchanged.
    #[must_use]
    pub fn forwarded(&self) -> u64 {
        self.forwarded
    }

    /// How many reports it dropped: evidence the authority never saw.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// How many reports it re-targeted at an innocent subject.
    #[must_use]
    pub fn retargeted(&self) -> u64 {
        self.retargeted
    }

    /// How many distinct senders it has heard: its victim pool's size.
    #[must_use]
    pub fn heard(&self) -> usize {
        self.heard.len()
    }

    /// The victim it frames next: the lowest digest it has heard that is not the report's
    /// own subject and not its own certificate.
    ///
    /// Deterministic by construction — a `BTreeSet` in digest order — because a victim
    /// chosen by hash-map iteration order would make the run unreproducible, and because
    /// which innocent vehicle is framed is exactly what a false-accusation metric counts.
    fn victim(&self, avoid: &str) -> Option<String> {
        self.heard
            .iter()
            .map(|d| v2xw_core::hash::hex_encode(d))
            .find(|hex| hex != avoid && *hex != self.params.own_cert_digest)
    }
}

impl Model for CompromisedRsu {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Attacker for CompromisedRsu {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn schedule(&self) -> &AttackSchedule {
        &self.schedule
    }

    fn observe(&mut self, _ctx: &mut dyn ThreatCtx, view: &AttackerView<'_>) {
        for m in view.own_rx {
            self.heard.insert(m.signer);
        }
    }

    fn act(
        &mut self,
        _ctx: &mut dyn ThreatCtx,
        view: &AttackerView<'_>,
        out: &mut Emission,
    ) -> Vec<AttackAction> {
        // A road-side unit declares what it is, compromised or not: the station type is
        // not the lie here.
        out.station_type = StationType::Rsu;
        if !self
            .schedule
            .active_at(view.believed_time, view.own_belief.x_m, view.own_belief.y_m)
        {
            return Vec::new();
        }
        let Some((message, field)) = self.params.kind.message_and_field() else {
            // The reporting-path kinds do nothing on the air; `on_forward` is their seam.
            return Vec::new();
        };
        let magnitude = match self.params.kind {
            RsuAttackKind::FalseSpat => self.params.false_green_s,
            RsuAttackKind::FalseMap => 1.0,
            RsuAttackKind::FalseCrl | RsuAttackKind::FalseCtl => self.params.fabricated_entries,
            _ => 0.0,
        };
        out.infra.push(InfraClaim {
            message: message.to_string(),
            field: field.to_string(),
            magnitude,
        });
        vec![AttackAction::FalsifyInfrastructure {
            message: message.to_string(),
            field: field.to_string(),
            magnitude,
        }]
    }
}

impl CompromisedRsu {
    /// What this unit does with a report it was asked to forward to the authority.
    ///
    /// The host calls it on the forwarding path — an RSU with a backhaul is where a
    /// vehicle's report goes (10-roadmap Phase 2 scope) — and applies the decision. The
    /// returned actions go to [`crate::attack::log_actions`] with the actor id the host
    /// knows, exactly as for an air-facing action.
    ///
    /// A re-targeted report keeps the *shape* of a genuine one: the fabricated evidence
    /// comes from [`forge`], whose distributions are the legacy collusion pass's, so the
    /// poisoned report is not separable from a genuine one by structural zeros. A poisoned
    /// report that were separable would make every measured robustness number meaningless.
    pub fn on_forward(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        report: &MisbehaviourReport,
        now: SimTime,
    ) -> (ForwardDecision, Vec<AttackAction>) {
        match self.params.kind {
            RsuAttackKind::SuppressForwardedReports => {
                let mut rng = ctx.rng(RngDomain::Report, EntityRef::Node(self.node));
                let drop_it = rng.bool(self.params.suppress_prob);
                drop(rng);
                if drop_it {
                    self.dropped += 1;
                    (
                        ForwardDecision::Drop,
                        vec![AttackAction::SuppressReport {
                            subject: report.subject_cert_digest.clone(),
                        }],
                    )
                } else {
                    self.forwarded += 1;
                    (ForwardDecision::Forward, Vec::new())
                }
            }
            RsuAttackKind::PoisonForwardedReports => {
                let mut rng = ctx.rng(RngDomain::Collusion, EntityRef::Node(self.node));
                let poison = rng.bool(self.params.poison_prob);
                drop(rng);
                let victim = self.victim(&report.subject_cert_digest);
                match (poison, victim) {
                    (true, Some(victim)) => {
                        self.seq += 1;
                        let id = format!("rpt_rsu_{}_{:05}", self.node.index(), self.seq);
                        let mut rng = ctx.rng(RngDomain::Collusion, EntityRef::Node(self.node));
                        let forged = forge(
                            id,
                            self.node,
                            self.params.own_cert_digest.clone(),
                            victim.clone(),
                            &self.params.forgery,
                            now,
                            &mut rng,
                        );
                        drop(rng);
                        self.retargeted += 1;
                        (
                            ForwardDecision::Replace(Box::new(forged)),
                            vec![AttackAction::ForgeReport { subject: victim }],
                        )
                    }
                    // Nothing heard yet, so nobody to frame: it forwards, and the counters
                    // say the attack did not fire rather than the run reporting a
                    // poisoning that never happened.
                    _ => {
                        self.forwarded += 1;
                        (ForwardDecision::Forward, Vec::new())
                    }
                }
            }
            _ => {
                self.forwarded += 1;
                (ForwardDecision::Forward, Vec::new())
            }
        }
    }
}

/// The model card for a [`CompromisedRsu`].
#[must_use]
pub fn card(params: &RsuAttackParams) -> ModelCard {
    use serde_json::json;
    let structural = |what: &str| Source {
        kind: SourceKind::Code,
        reference: "docs/design/07-threats-and-detection.md §2.2 (Compromised RSU)".to_string(),
        accessed: Some(crate::cards::LEGACY_ACCESSED.to_string()),
        note: Some(what.to_string()),
    };
    let ranged01 = |name: &str, v: f64, note: &str| {
        let mut p = Parameter::new(name, "-", json!(v), structural(note));
        p.range = Some(vec![json!(0.0), json!(1.0)]);
        p
    };
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Attacker,
        "1.0.0",
        format!(
            "A compromised road-side unit running {}: infrastructure credentials every \
             receiver trusts, abused on the air or on the path reports take to the \
             authority.",
            params.kind
        ),
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "false infrastructure claim",
            "the emission carries (message, field, magnitude); the host renders the \
             falsified SPaT/MAP/CRL/CTL and the ground-truth channel carries the same \
             triple",
        ),
        Equation::new(
            "report poisoning at the forwarding stage",
            "forward(report) → Replace(forge(own digest, victim)) with probability \
             poison_prob, where victim is the lowest digest this unit has heard that is \
             neither the subject nor itself",
        ),
    ];
    card.parameters = vec![
        {
            let mut p = Parameter::new(
                "false_green_s",
                "s",
                json!(params.false_green_s),
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "no anchor for a fabricated signal phase".to_string(),
                    accessed: None,
                    note: Some(
                        "07-threats-and-detection.md §2.2 requires false SPaT; it gives no \
                         magnitude, and neither does 04-models.md §2.3"
                            .to_string(),
                    ),
                },
            );
            p.calibration = Some(
                "set against the intersection-control model's own phase and clearance \
                 intervals (04-models.md §2.3): the interesting magnitude is the one that \
                 removes the all-red clearance a driver relies on, which is that model's \
                 number and not a free parameter."
                    .to_string(),
            );
            p
        },
        {
            let mut p = Parameter::new(
                "fabricated_entries",
                "-",
                json!(params.fabricated_entries),
                Source {
                    kind: SourceKind::TodoCalibrate,
                    reference: "no anchor for a fabricated list length".to_string(),
                    accessed: None,
                    note: Some(
                        "a false CRL's harm scales with the entries a receiver accepts, \
                         which depends on the list's own size model (04-models.md §9)"
                            .to_string(),
                    ),
                },
            );
            p.calibration = Some(
                "sweep against the CRL size model and the node's CRL processing cost \
                 (08-measurement-and-data.md §2.3 crl_processing_time), and report the \
                 entry count at which processing cost becomes the harm rather than the \
                 false revocation."
                    .to_string(),
            );
            p
        },
        ranged01(
            "suppress_prob",
            params.suppress_prob,
            "1.0 is the unambiguous rendering of 'it suppresses'; the design states the \
             behaviour and no rate",
        ),
        ranged01(
            "poison_prob",
            params.poison_prob,
            "as for suppress_prob: the design states the behaviour and no rate",
        ),
    ];
    card.sources = vec![
        design("07-threats-and-detection.md §2.2 (Compromised RSU, report poisoning)"),
        design("07-threats-and-detection.md §3.2 (the authority pipeline it attacks)"),
        design("10-roadmap.md Phase 2 (the RSU-with-backhaul forwarding path)"),
        crate::cards::legacy(
            crate::cards::LEGACY_PY,
            "the collusion pass (the forged-evidence distributions reused here)",
        ),
    ];
    card.assumptions = vec![
        "Its signatures verify: a compromised unit holds real infrastructure credentials, \
         which is what distinguishes it from an impersonator."
            .to_string(),
        "It picks victims from the digests it has heard, so its reach is its receiver's \
         reach and nothing more (invariant I-T1)."
            .to_string(),
        "A re-targeted report carries plausible fabricated evidence, so it is not \
         separable from a genuine report by structural zeros."
            .to_string(),
    ];
    card.limitations = vec![
        "False SPaT, MAP, CRL and CTL are declared rather than rendered. The encoder \
         (v2xw_msg::j2735::spat, ::map) and the broadcaster \
         (v2xw_node::rsu::RsuRuntime, RsuRole::SpatMapBroadcast, set_payload) exist, so \
         the missing pieces are the engine applying the declared claim to the payload it \
         builds, and any consumer of signal state: no safety application and no detector \
         reads SPaT today, so TS 103 759 class 3 cannot cross-check a false one."
            .to_string(),
        "The unit's own beacons are otherwise honest. A compromised unit that also \
         falsified its own position is that attacker composed with a legacy rendering, \
         which is a scenario with two attacker instances."
            .to_string(),
        "An authority that exempts infrastructure from its reporter gate has no defence \
         against the forwarding-stage attack at all. That is the finding, not a bug: \
         v2xw_threat::ma::LegacyWindow::trust_infrastructure is the toggle a run flips to \
         measure it."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![
            RngDomain::Report.as_str().to_string(),
            RngDomain::Collusion.as_str().to_string(),
        ],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![design("07-threats-and-detection.md §2.2")],
        tests: vec![
            "poisoning::a_compromised_unit_retargets_a_report_at_an_innocent_subject".to_string(),
            "poisoning::a_compromised_unit_with_nobody_to_frame_forwards".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_parses_and_declares_its_surface() {
        assert_eq!(RsuAttackKind::ALL.len(), 6);
        for k in RsuAttackKind::ALL {
            assert_eq!(RsuAttackKind::parse(k.as_str()), Some(k));
            assert_eq!(k.family(), AttackFamily::Infrastructure);
            assert_eq!(k.is_over_the_air(), k.message_and_field().is_some());
        }
        assert_eq!(
            RsuAttackKind::FalseSpat.message_and_field(),
            Some(("spat", "phase"))
        );
        assert_eq!(
            RsuAttackKind::PoisonForwardedReports.message_and_field(),
            None
        );
    }

    #[test]
    fn the_card_validates_for_every_kind() {
        for k in RsuAttackKind::ALL {
            let c = card(&RsuAttackParams::new(k, "abcd"));
            c.validate().unwrap();
            c.check_api_version().unwrap();
            for p in &c.parameters {
                let cited = p.source.kind != SourceKind::TodoCalibrate;
                let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
                assert!(cited || planned, "{}", p.name);
            }
        }
    }
}
