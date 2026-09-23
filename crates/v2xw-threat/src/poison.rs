//! Misbehaviour-report poisoning: the attack on the authority rather than on the air
//! (07-threats-and-detection.md §2.1 "collusion", §2.2 "Misbehavior-report poisoning").
//!
//! # What it is
//!
//! An insider with entirely valid credentials, behaving perfectly on the air, files
//! reports against vehicles it has no evidence about. Two things make it worth modelling
//! separately from any falsification attack:
//!
//! 1. **It is the attack the authority's defences exist to resist.** The trusted-reporter
//!    gate of [`crate::ma::LegacyWindow`] (k distinct reporters, a reporter budget, a
//!    reputation cap) and the two-authority identity resolution of
//!    [`crate::resolve::TwoAuthorityResolution`] are both there for this and for nothing
//!    else. Without an attacker that mounts it, those defences are untested code and their
//!    measured value is zero by construction.
//! 2. **It costs the attacker nothing on the air.** A forged report is a few hundred bytes
//!    to the authority, so a coalition can frame a vehicle far more cheaply than it can
//!    fake one, and a report flood can exhaust the authority's budget rather than any
//!    receiver's.
//!
//! # How it is measurable
//!
//! Every forged report goes through the ordinary reporting transport, lands on `ma.report`
//! when it arrives, and is logged on the ground-truth channel as `ForgeReport` with the
//! true actor behind it. `v2xw_metrics::detection` then counts `false_accusations` — benign
//! subjects with at least one report, and with a revocation — from exactly those channels
//! plus the run's declared digest-to-actor map. Turning
//! [`crate::ma::MaParams::defence`] off and on, or moving
//! [`crate::resolve::ResolutionParams::authorities_required`] between 1 and 2, is how a run
//! measures what each defence is worth against a stated coalition.
//!
//! # Fabricated evidence
//!
//! The forged report's fingerprint comes from [`crate::report::forge`], whose
//! distributions are the legacy collusion pass's own. The legacy engine learned the lesson
//! the hard way: a fabricated report whose radio detectors are exactly zero is separable
//! from every genuine one by those structural zeros, which makes collusion trivially
//! detectable in the dataset and every measured robustness number meaningless.
//!
//! # Victim choice
//!
//! Victims come from the digests this attacker has *heard*, filtered by a stable
//! per-digest coin, which mirrors the legacy `_is_flow_victim`: a fixed fraction of the
//! vehicles are framing targets for the whole run rather than a fresh sample every step,
//! because a colluder whose target set changes every second never accumulates the
//! sustained evidence the authority's window asks for — and the legacy engine's own note
//! says stable targeting is the point.
//!
//! One divergence from the legacy engine is recorded here rather than hidden: the legacy
//! coin is keyed by `(seed, vid)` and therefore **shared across colluders**, so two
//! colluders frame the same victims. [`crate::ctx::ThreatCtx`] hands out cached streams
//! keyed by `(domain, entity)`, so a per-digest stream at word zero is not reachable from
//! inside a plug-in; the coin here is drawn from this attacker's own stream and cached per
//! digest, so it is stable for this attacker and independent between attackers. A
//! coalition that must agree on victims declares them through
//! [`ReportPoisoner::designate_victim`].

use std::collections::{BTreeMap, BTreeSet};

use crate::attack::{AttackAction, AttackFamily, Attacker, AttackerView, Emission};
use crate::capability::{AttackSchedule, Capabilities};
use crate::cards::{LEGACY_PY, design, legacy, legacy_param, legacy_uncited};
use crate::ctx::ThreatCtx;
use crate::report::{ForgeryProfile, MisbehaviourReport, forge};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Tier, Validation, ValidationStatus,
};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};

/// The model id this module's card and its `gt.attack.action` records carry.
pub const MODEL_ID: &str = "threat/attacker/report-poisoner";

/// Everything the poisoner reads.
#[derive(Debug, Clone, PartialEq)]
pub struct PoisonParams {
    /// The attacker's own pseudonym digest, hex: what it signs its reports as.
    ///
    /// A poisoner that rotates pseudonyms gets a new digest from the credential store, and
    /// the scenario updates it through [`ReportPoisoner::set_own_digest`] — which is also
    /// the evasion: a reporter budget counted per digest is a budget per pseudonym.
    pub own_cert_digest: String,
    /// The probability it files a report about a victim it has selected
    /// (`PipelineConfig.report_prob`, 0.9).
    pub report_prob: f64,
    /// The fraction of the vehicles it hears that it designates as framing victims
    /// (`PipelineConfig.victim_pct`, 0.10).
    pub victim_pct: f64,
    /// How many reports it files per victim per interval.
    ///
    /// `1` is the legacy collusion pass. Above one is the report flood of
    /// 07-threats-and-detection.md §2.2, whose point is the authority's budget
    /// ([`crate::ma::MaParams::report_budget`], 30) rather than any receiver's.
    pub reports_per_interval: u32,
    /// The distributions the fabricated evidence is drawn from.
    pub forgery: ForgeryProfile,
}

impl Default for PoisonParams {
    fn default() -> Self {
        Self {
            own_cert_digest: String::new(),
            report_prob: 0.9,
            victim_pct: 0.10,
            reports_per_interval: 1,
            forgery: ForgeryProfile::default(),
        }
    }
}

impl PoisonParams {
    /// The parameters with this attacker's own certificate digest.
    #[must_use]
    pub fn new(own_cert_digest: impl Into<String>) -> Self {
        Self {
            own_cert_digest: own_cert_digest.into(),
            ..Self::default()
        }
    }

    /// The same parameters, flooding at `n` reports per victim per interval.
    #[must_use]
    pub fn flooding(mut self, n: u32) -> Self {
        self.reports_per_interval = n;
        self
    }
}

/// An insider that files false misbehaviour reports.
#[derive(Debug, Clone)]
pub struct ReportPoisoner {
    card: ModelCard,
    params: PoisonParams,
    capabilities: Capabilities,
    schedule: AttackSchedule,
    node: NodeId,
    /// Digests heard, in digest order: the victim pool.
    heard: BTreeSet<[u8; 8]>,
    /// The stable per-digest victim coin, drawn once and remembered.
    designated: BTreeMap<[u8; 8], bool>,
    outbox: Vec<MisbehaviourReport>,
    filed: u64,
    seq: u64,
}

impl ReportPoisoner {
    /// A poisoner at `node`.
    ///
    /// `capabilities` must declare valid credentials: a report signed by a certificate the
    /// authority will not verify is thrown out at ingestion, so an outsider cannot mount
    /// this attack at all — which is itself a result worth stating.
    #[must_use]
    pub fn new(
        node: NodeId,
        params: PoisonParams,
        capabilities: Capabilities,
        schedule: AttackSchedule,
    ) -> Self {
        let card = card(&params);
        Self {
            card,
            params,
            capabilities,
            schedule,
            node,
            heard: BTreeSet::new(),
            designated: BTreeMap::new(),
            outbox: Vec::new(),
            filed: 0,
            seq: 0,
        }
    }

    /// The parameters it reads.
    #[must_use]
    pub fn params(&self) -> &PoisonParams {
        &self.params
    }

    /// Replaces the digest it signs reports as, after a pseudonym change.
    pub fn set_own_digest(&mut self, digest: impl Into<String>) {
        self.params.own_cert_digest = digest.into();
    }

    /// Designates `digest` a framing victim regardless of the coin.
    ///
    /// The coalition's shared victim list: 07-threats-and-detection.md §1 requires a
    /// coalition's coordination to be *modelled*, so the list arrives through the
    /// coordination channel as ordinary messages and the engine calls this, rather than
    /// two attackers silently agreeing through a shared random stream.
    pub fn designate_victim(&mut self, digest: [u8; 8]) {
        self.designated.insert(digest, true);
    }

    /// How many reports it has filed.
    #[must_use]
    pub fn filed(&self) -> u64 {
        self.filed
    }

    /// How many distinct senders it has heard.
    #[must_use]
    pub fn heard(&self) -> usize {
        self.heard.len()
    }

    /// The digests it has designated as victims, in digest order.
    #[must_use]
    pub fn victims(&self) -> Vec<[u8; 8]> {
        self.designated
            .iter()
            .filter(|(_, v)| **v)
            .map(|(k, _)| *k)
            .collect()
    }

    /// Takes the reports it has queued, for the host to submit through the reporting
    /// transport.
    ///
    /// The queue exists because a report is not an emission: it travels over the reporting
    /// path with that path's own delay and batching (07-threats-and-detection.md §3.2), so
    /// the attacker hands it over and the host pays for it.
    pub fn take_reports(&mut self) -> Vec<MisbehaviourReport> {
        core::mem::take(&mut self.outbox)
    }

    /// Whether `digest` is a framing victim, drawing the stable coin once.
    fn is_victim(&mut self, ctx: &mut dyn ThreatCtx, digest: [u8; 8]) -> bool {
        if let Some(v) = self.designated.get(&digest) {
            return *v;
        }
        let mut rng = ctx.rng(RngDomain::Collusion, EntityRef::Node(self.node));
        let coin = rng.f64() < self.params.victim_pct;
        drop(rng);
        self.designated.insert(digest, coin);
        coin
    }
}

impl Model for ReportPoisoner {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Attacker for ReportPoisoner {
    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn schedule(&self) -> &AttackSchedule {
        &self.schedule
    }

    fn observe(&mut self, _ctx: &mut dyn ThreatCtx, view: &AttackerView<'_>) {
        for m in view.own_rx {
            // Its own certificate is not a victim, and neither is a sender whose signature
            // did not verify: the authority would throw that report out at ingestion, so
            // framing such a subject wastes the attacker's budget. Both are belief-side
            // judgements — the attacker is reading its own receptions.
            if m.verification.is_valid() {
                self.heard.insert(m.signer);
            }
        }
    }

    /// Files reports; the beacon is left exactly as the honest stack built it.
    ///
    /// That is the attack: an insider whose air behaviour is perfect and whose reports are
    /// fiction. A poisoner that also falsified its own claims would be caught by the
    /// ordinary detector suite, and the run would credit the authority's defences with a
    /// robustness they never had to provide.
    fn act(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        view: &AttackerView<'_>,
        _out: &mut Emission,
    ) -> Vec<AttackAction> {
        let t = view.believed_time;
        if !self
            .schedule
            .active_at(t, view.own_belief.x_m, view.own_belief.y_m)
        {
            return Vec::new();
        }
        if !self.capabilities.credentials.is_valid() {
            // No valid credential, no report the authority will accept. Nothing happens,
            // and nothing is claimed to have happened.
            return Vec::new();
        }
        let own = self.params.own_cert_digest.clone();
        // Digest order, so the report sequence is the same on every run and on every
        // thread count (the view's slice order is the radio's, not ours).
        let candidates: Vec<[u8; 8]> = self.heard.iter().copied().collect();
        let mut actions = Vec::new();
        for digest in candidates {
            let hex = v2xw_core::hash::hex_encode(&digest);
            if hex == own {
                continue;
            }
            if !self.is_victim(ctx, digest) {
                continue;
            }
            for _ in 0..self.params.reports_per_interval {
                let file = {
                    let mut rng = ctx.rng(RngDomain::Report, EntityRef::Node(self.node));
                    rng.bool(self.params.report_prob)
                };
                if !file {
                    continue;
                }
                self.seq += 1;
                let id = format!("rpt_poison_{}_{:05}", self.node.index(), self.seq);
                let mut rng = ctx.rng(RngDomain::Collusion, EntityRef::Node(self.node));
                let report = forge(
                    id,
                    self.node,
                    own.clone(),
                    hex.clone(),
                    &self.params.forgery,
                    t,
                    &mut rng,
                );
                drop(rng);
                self.outbox.push(report);
                self.filed += 1;
                actions.push(AttackAction::ForgeReport {
                    subject: hex.clone(),
                });
            }
        }
        actions
    }
}

/// The family report poisoning belongs to.
#[must_use]
pub const fn family() -> AttackFamily {
    AttackFamily::Poisoning
}

/// The model card for a [`ReportPoisoner`].
#[must_use]
pub fn card(params: &PoisonParams) -> ModelCard {
    use serde_json::json;
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Attacker,
        "1.0.0",
        "An insider with valid credentials that behaves honestly on the air and files \
         false misbehaviour reports against vehicles it has no evidence about: the attack \
         the authority's trusted-reporter gate and two-authority identity resolution exist \
         to resist.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "victim designation",
            "victim(digest) = coin(digest) < victim_pct, drawn once per digest and kept — \
             the legacy stable-targeting rule",
        ),
        Equation::new(
            "reports per interval",
            "for each designated victim heard: reports_per_interval draws of \
             Bernoulli(report_prob)",
        ),
        Equation::new(
            "fabricated fingerprint",
            "positionSpeedInconsistency ~ U(1.05, 4.0); sybilCoLocation = \
             U{1,2}/sybil_min_certs; beaconFrequency = U{1,3}/freq_max; staleOrReplay ~ \
             U(0, 0.15) — the legacy collusion pass, so no structural zero separates a \
             forged report from a genuine one",
        ),
    ];
    card.parameters = vec![
        legacy_param(
            "report_prob",
            "-",
            json!(params.report_prob),
            LEGACY_PY,
            "PipelineConfig.report_prob",
        ),
        legacy_param(
            "victim_pct",
            "-",
            json!(params.victim_pct),
            LEGACY_PY,
            "PipelineConfig.victim_pct",
        ),
        legacy_uncited(
            "reports_per_interval",
            "-",
            json!(params.reports_per_interval),
            LEGACY_PY,
            "the collusion pass (one report per victim per step)",
            "the interesting rate is relative to the authority's own report budget \
             (report_budget = 30) and to the reporting transport's capacity: sweep it and \
             report the rate at which the budget, rather than the correlation gate, is \
             what stops the attack.",
        ),
        legacy_param(
            "forgery.leading_score.min",
            "-",
            json!(params.forgery.leading_score.0),
            LEGACY_PY,
            "the collusion pass (cfab.uniform(1.05, 4.0))",
        ),
        legacy_param(
            "forgery.leading_score.max",
            "-",
            json!(params.forgery.leading_score.1),
            LEGACY_PY,
            "the collusion pass (cfab.uniform(1.05, 4.0))",
        ),
        legacy_param(
            "forgery.pos_confidence_m.min",
            "m",
            json!(params.forgery.pos_confidence_m.0),
            LEGACY_PY,
            "the collusion pass (cfab.uniform(2.0, 9.0))",
        ),
        legacy_param(
            "forgery.pos_confidence_m.max",
            "m",
            json!(params.forgery.pos_confidence_m.1),
            LEGACY_PY,
            "the collusion pass (cfab.uniform(2.0, 9.0))",
        ),
    ];
    card.sources = vec![
        design("07-threats-and-detection.md §2.1 (collusion), §2.2 (report poisoning)"),
        design("07-threats-and-detection.md §3.2 (the pipeline it attacks)"),
        design("08-measurement-and-data.md §2.4 (false_accusations)"),
        legacy(LEGACY_PY, "the collusion pass + _is_flow_victim"),
    ];
    card.assumptions = vec![
        "The attacker holds valid credentials; without them the authority discards its \
         reports at ingestion and the attack does not exist."
            .to_string(),
        "Victims are drawn from the digests this attacker heard and verified, so its reach \
         is its receiver's reach (invariant I-T1)."
            .to_string(),
        "Its own air behaviour is honest, so the local detector suite has nothing to find \
         and the authority's defences are what is being measured."
            .to_string(),
    ];
    card.limitations = vec![
        "The legacy victim coin is shared across colluders (keyed by seed and victim id); \
         this port's coin is per attacker, because a plug-in cannot reach a per-digest \
         stream at word zero through ThreatCtx. A coalition that must agree on victims \
         declares them with designate_victim."
            .to_string(),
        "The report's transport delay, batching and byte cost are the host's: this model \
         queues reports and take_reports hands them over."
            .to_string(),
        "It frames whole digests, not vehicles. A victim that changes pseudonym becomes a \
         second subject, which is the same fragmentation an honest reporter's evidence \
         suffers and is visible as such in the metrics."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![
            RngDomain::Collusion.as_str().to_string(),
            RngDomain::Report.as_str().to_string(),
        ],
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![legacy(LEGACY_PY, "the collusion pass")],
        tests: vec![
            "poisoning::a_poisoner_files_only_against_victims_it_has_heard".to_string(),
            "poisoning::the_trusted_reporter_gate_is_what_stops_a_coalition".to_string(),
            "poisoning::a_poisoner_without_credentials_files_nothing".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_validates_and_every_default_is_cited_or_planned() {
        let c = card(&PoisonParams::new("cafe"));
        c.validate().unwrap();
        c.check_api_version().unwrap();
        for p in &c.parameters {
            let cited = p.source.kind != v2xw_core::card::SourceKind::TodoCalibrate;
            let planned = p.calibration.as_ref().is_some_and(|s| !s.trim().is_empty());
            assert!(cited || planned, "{}", p.name);
        }
    }

    #[test]
    fn the_legacy_collusion_operating_point_is_the_default() {
        let p = PoisonParams::new("cafe");
        assert_eq!(p.report_prob, 0.9);
        assert_eq!(p.victim_pct, 0.10);
        assert_eq!(p.reports_per_interval, 1);
        assert_eq!(p.flooding(40).reports_per_interval, 40);
        assert_eq!(family(), AttackFamily::Poisoning);
    }

    #[test]
    fn a_designated_victim_overrides_the_coin() {
        let mut a = ReportPoisoner::new(
            NodeId::new(4),
            PoisonParams::new("cafe"),
            Capabilities::insider(20),
            AttackSchedule::default(),
        );
        a.designate_victim([1; 8]);
        assert_eq!(a.victims(), vec![[1u8; 8]]);
    }
}
