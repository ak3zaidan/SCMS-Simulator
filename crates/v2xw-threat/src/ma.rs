//! The misbehaviour-authority pipeline: ingestion, correlation, decision.
//!
//! [`LegacyWindow`] is the `legacy-window` plug-in of 07-threats-and-detection.md §3.2,
//! at the legacy operating point: **k = 3 trusted reporters, 4 distinct seconds, a 3 s
//! span, a 15 s window, a reporter budget of 30 and a reputation cap of 40**.
//!
//! # Why a sliding window
//!
//! Evidence accumulated over a whole trip lets a benign vehicle's bursty faults — an urban
//! canyon, a tunnel mouth, a multipath spike — add up to a revocation. The legacy engine's
//! own comment says so: the window "requires genuinely sustained misbehaviour". The three
//! conditions are conjunctive on purpose, and each blunts a different false positive:
//! distinct reporters blunt one broken receiver, distinct seconds blunt one bad instant,
//! and the span blunts a burst.
//!
//! # Collusion defence
//!
//! A reporter is *trusted* only if it is not itself revoked, has not been reported more
//! than the reputation cap, and has not filed more than the budget. Without that gate a
//! coalition of colluders can revoke any honest vehicle it likes by filing k fabricated
//! reports, which is the [`crate::report::forge`] attack. The gate is a parameter so the
//! scenario can turn it off and measure what it is worth.
//!
//! # What is not here
//!
//! Investigation — the protocol's identity resolution, the SCMS PCA and LA queries or the
//! ETSI EA lookup — is a flow with round trips through `v2xw-proto`, and the responder
//! that issues the revocation belongs there too (07-threats §3.3). This pipeline emits
//! [`MaAction::Revoke`] and stops; the hand-off is the engine's.

use std::collections::{BTreeMap, BTreeSet};

use crate::cards::{LEGACY_PY, design, legacy_param, legacy_uncited};
use crate::ctx::{ThreatCtx, ThreatCtxExt};
use crate::records::{MaDecisionRecord, MaReportRecord};
use crate::report::MisbehaviourReport;
use v2xw_core::card::{Determinism, Family, ModelCard, Tier, Validation, ValidationStatus};
use v2xw_core::model::Model;
use v2xw_core::time::{NS_PER_S, SimTime, secs_to_ns};

/// The model id the pipeline's card and its decisions carry.
pub const MODEL_ID: &str = "threat/ma/legacy-window";

/// What the authority decided to do about a subject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaAction {
    /// Revoke the subject's credentials.
    Revoke {
        /// The subject's certificate digest.
        subject: String,
    },
    /// Open an investigation: enough evidence to look, not enough to act.
    Investigate {
        /// The subject's certificate digest.
        subject: String,
    },
    /// Dismiss the accumulated evidence about a subject.
    Dismiss {
        /// The subject's certificate digest.
        subject: String,
    },
}

impl MaAction {
    /// The subject the action is about.
    #[must_use]
    pub fn subject(&self) -> &str {
        match self {
            MaAction::Revoke { subject }
            | MaAction::Investigate { subject }
            | MaAction::Dismiss { subject } => subject,
        }
    }

    /// The decision's wire spelling, as the `ma.decision` channel carries it.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            MaAction::Revoke { .. } => "revoke",
            MaAction::Investigate { .. } => "investigate",
            MaAction::Dismiss { .. } => "dismiss",
        }
    }
}

/// A misbehaviour-authority pipeline (03-interfaces.md §9).
pub trait MaPipeline: Model {
    /// Ingests one report and returns whatever it decided as a result.
    fn on_report(&mut self, ctx: &mut dyn ThreatCtx, r: &MisbehaviourReport) -> Vec<MaAction>;

    /// Runs the windowed correlation at `t`, for subjects whose evidence aged out of the
    /// window since the last call.
    fn on_tick(&mut self, ctx: &mut dyn ThreatCtx, t: SimTime) -> Vec<MaAction>;

    /// Every decision taken so far, in decision order.
    fn decisions(&self) -> &[MaAction];
}

/// The correlation operating point.
#[derive(Debug, Clone, PartialEq)]
pub struct MaParams {
    /// Distinct trusted reporters required (`PipelineConfig.report_threshold_k`, 3).
    pub report_threshold_k: usize,
    /// Distinct whole seconds the evidence must span
    /// (`PipelineConfig.revoke_min_seconds`, 4).
    pub revoke_min_seconds: usize,
    /// How long the evidence must span (`PipelineConfig.revoke_persist_s`, 3.0 s).
    pub revoke_persist_s: f64,
    /// The sliding window evidence must be sustained within
    /// (`PipelineConfig.revoke_window_s`, 15.0 s).
    pub revoke_window_s: f64,
    /// Whether the trusted-reporter gate is on (`PipelineConfig.ma_defense`, true).
    pub defence: bool,
    /// A reporter itself reported more than this many times is distrusted
    /// (`PipelineConfig.reputation_max`, 40).
    pub reputation_max: u32,
    /// A reporter filing more than this many reports is rate-limited
    /// (`PipelineConfig.report_budget`, 30).
    pub report_budget: u32,
}

impl Default for MaParams {
    fn default() -> Self {
        Self {
            report_threshold_k: 3,
            revoke_min_seconds: 4,
            revoke_persist_s: 3.0,
            revoke_window_s: 15.0,
            defence: true,
            reputation_max: 40,
            report_budget: 30,
        }
    }
}

/// The legacy windowed correlator.
#[derive(Debug, Clone)]
pub struct LegacyWindow {
    card: ModelCard,
    params: MaParams,
    /// Per subject: `(instant, reporter digest)`, oldest first.
    evidence: BTreeMap<String, Vec<(SimTime, String)>>,
    filed_by: BTreeMap<String, u32>,
    reported: BTreeMap<String, u32>,
    revoked: BTreeSet<String>,
    /// Certificates whose holder is trusted infrastructure: never rate-limited or
    /// distrusted, because an RSU is a fixed, operator-run receiver.
    infrastructure: BTreeSet<String>,
    decisions: Vec<MaAction>,
}

impl LegacyWindow {
    /// The pipeline at the given operating point.
    #[must_use]
    pub fn new(params: MaParams) -> Self {
        Self {
            card: card(&params),
            params,
            evidence: BTreeMap::new(),
            filed_by: BTreeMap::new(),
            reported: BTreeMap::new(),
            revoked: BTreeSet::new(),
            infrastructure: BTreeSet::new(),
            decisions: Vec::new(),
        }
    }

    /// The pipeline at the legacy operating point.
    #[must_use]
    pub fn legacy_defaults() -> Self {
        Self::new(MaParams::default())
    }

    /// The operating point it runs at.
    #[must_use]
    pub fn params(&self) -> &MaParams {
        &self.params
    }

    /// Declares a certificate as trusted infrastructure.
    pub fn trust_infrastructure(&mut self, digest: impl Into<String>) {
        self.infrastructure.insert(digest.into());
    }

    /// Whether the subject has been revoked.
    #[must_use]
    pub fn is_revoked(&self, subject: &str) -> bool {
        self.revoked.contains(subject)
    }

    /// How many subjects have been revoked.
    #[must_use]
    pub fn revoked_count(&self) -> usize {
        self.revoked.len()
    }

    /// Whether a reporter's evidence counts.
    #[must_use]
    pub fn trusted(&self, reporter: &str) -> bool {
        if !self.params.defence {
            return true;
        }
        if self.infrastructure.contains(reporter) {
            return true;
        }
        !self.revoked.contains(reporter)
            && self.filed_by.get(reporter).copied().unwrap_or(0) <= self.params.report_budget
            && self.reported.get(reporter).copied().unwrap_or(0) < self.params.reputation_max
    }

    /// Correlates the evidence about one subject at `t` and decides.
    fn correlate(&mut self, subject: &str, t: SimTime) -> Option<MaAction> {
        if self.revoked.contains(subject) {
            return None;
        }
        let window = secs_to_ns(self.params.revoke_window_s);
        let cutoff = t.saturating_sub(window);
        {
            let ev = self.evidence.get_mut(subject)?;
            ev.retain(|(at, _)| *at >= cutoff);
        }
        let ev = self.evidence.get(subject)?;
        if ev.is_empty() {
            return None;
        }
        let reporters: BTreeSet<&str> = ev
            .iter()
            .filter(|(_, r)| self.trusted(r))
            .map(|(_, r)| r.as_str())
            .collect();
        let seconds: BTreeSet<u64> = ev.iter().map(|(at, _)| at / NS_PER_S).collect();
        let span = ev.last().map_or(0, |e| e.0) - ev.first().map_or(0, |e| e.0);
        let reporters = reporters.len();
        let persist = secs_to_ns(self.params.revoke_persist_s);
        if reporters >= self.params.report_threshold_k
            && seconds.len() >= self.params.revoke_min_seconds
            && span >= persist
        {
            self.revoked.insert(subject.to_string());
            return Some(MaAction::Revoke {
                subject: subject.to_string(),
            });
        }
        None
    }
}

impl Model for LegacyWindow {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MaPipeline for LegacyWindow {
    fn on_report(&mut self, ctx: &mut dyn ThreatCtx, r: &MisbehaviourReport) -> Vec<MaAction> {
        ctx.emit(MaReportRecord {
            t: r.ingest_time,
            reporter: r.reporter,
            subject: r.subject_cert_digest.clone(),
            detector: r.leading_reason().map(str::to_string),
        });
        *self
            .filed_by
            .entry(r.reporter_cert_digest.clone())
            .or_insert(0) += 1;
        *self
            .reported
            .entry(r.subject_cert_digest.clone())
            .or_insert(0) += 1;
        self.evidence
            .entry(r.subject_cert_digest.clone())
            .or_default()
            .push((r.ingest_time, r.reporter_cert_digest.clone()));

        let mut out = Vec::new();
        if let Some(a) = self.correlate(&r.subject_cert_digest, r.ingest_time) {
            ctx.emit(MaDecisionRecord {
                t: r.ingest_time,
                subject: a.subject().to_string(),
                decision: a.as_str().to_string(),
            });
            self.decisions.push(a.clone());
            out.push(a);
        }
        out
    }

    fn on_tick(&mut self, ctx: &mut dyn ThreatCtx, t: SimTime) -> Vec<MaAction> {
        // Subjects are visited in digest order, so the decision order is the same on
        // every run and on every thread count.
        let subjects: Vec<String> = self.evidence.keys().cloned().collect();
        let mut out = Vec::new();
        for s in subjects {
            if let Some(a) = self.correlate(&s, t) {
                ctx.emit(MaDecisionRecord {
                    t,
                    subject: a.subject().to_string(),
                    decision: a.as_str().to_string(),
                });
                self.decisions.push(a.clone());
                out.push(a);
            }
        }
        out
    }

    fn decisions(&self) -> &[MaAction] {
        &self.decisions
    }
}

/// The model card for the pipeline.
#[must_use]
pub fn card(p: &MaParams) -> ModelCard {
    use serde_json::json;
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::MaPipeline,
        "1.0.0",
        "Windowed correlation of misbehaviour reports at the legacy operating point: \
         k distinct trusted reporters, over distinct seconds, spanning a minimum time, \
         inside a sliding window, with a reporter budget and a reputation cap.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![v2xw_core::card::Equation::new(
        "revocation condition",
        "|{trusted reporters in window}| ≥ k ∧ |{distinct seconds}| ≥ n ∧ \
         (t_last − t_first) ≥ persist",
    )];
    card.parameters = vec![
        legacy_uncited(
            "report_threshold_k",
            "-",
            json!(p.report_threshold_k),
            LEGACY_PY,
            "PipelineConfig.report_threshold_k",
            "sweep k against the false-accusation rate at a stated colluder fraction; \
             the right k is the smallest one whose false-accusation rate is acceptable \
             at the coalition size the scenario declares, not a fixed 3.",
        ),
        legacy_uncited(
            "revoke_min_seconds",
            "-",
            json!(p.revoke_min_seconds),
            LEGACY_PY,
            "PipelineConfig.revoke_min_seconds",
            "derive from the duration of the benign GNSS degradation bursts the GNSS \
             model produces, so the gate outlasts them by construction.",
        ),
        legacy_uncited(
            "revoke_persist_s",
            "s",
            json!(p.revoke_persist_s),
            LEGACY_PY,
            "PipelineConfig.revoke_persist_s",
            "as for revoke_min_seconds: it must exceed the benign burst length.",
        ),
        legacy_uncited(
            "revoke_window_s",
            "s",
            json!(p.revoke_window_s),
            LEGACY_PY,
            "PipelineConfig.revoke_window_s",
            "set against the pseudonym change interval: a window longer than it splits \
             one attacker's evidence across two subjects.",
        ),
        legacy_param(
            "ma_defence",
            "-",
            json!(p.defence),
            LEGACY_PY,
            "PipelineConfig.ma_defense",
        ),
        legacy_uncited(
            "reputation_max",
            "-",
            json!(p.reputation_max),
            LEGACY_PY,
            "PipelineConfig.reputation_max",
            "measure the report-received distribution of honest vehicles in a dense \
             scenario and set the cap above its tail, rather than at a round number.",
        ),
        legacy_uncited(
            "report_budget",
            "-",
            json!(p.report_budget),
            LEGACY_PY,
            "PipelineConfig.report_budget",
            "as for reputation_max, from the report-filed distribution of honest \
             vehicles.",
        ),
    ];
    card.sources = vec![
        crate::cards::legacy(LEGACY_PY, "the online MA decision pass + trusted()"),
        design("07-threats-and-detection.md §3.2"),
    ];
    card.assumptions = vec![
        "A report names its subject by pseudonym digest only; the pipeline never resolves \
         it to an actor, which is the protocol's identity-resolution flow."
            .to_string(),
        "Reports arrive with their own transport delay already applied, so ingest_time is \
         authority time and the correlation window is measured in it."
            .to_string(),
    ];
    card.limitations = vec![
        "No investigation stage: identity resolution and the revocation flow belong to \
         v2xw-proto (07-threats-and-detection.md §3.2, §3.3)."
            .to_string(),
        "No service model: the authority's own processing cost is not charged here.".to_string(),
        "Dismiss and Investigate are modelled as decisions the interface can express but \
         the legacy operating point never produces; a replacement stage can."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![crate::cards::legacy(
            LEGACY_PY,
            "the online MA decision pass",
        )],
        tests: vec!["ma::the_legacy_operating_point_holds".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::CollectingCtx;
    use crate::detect::{DetectorId, Fingerprint, Observation, Verdict};
    use v2xw_core::ids::NodeId;

    fn report(id: &str, reporter: u32, subject: &str, t: SimTime) -> MisbehaviourReport {
        let mut f = Fingerprint::default();
        f.set(DetectorId::PositionJump, 2.0);
        let v = Verdict {
            subject: subject.to_string(),
            fingerprint: f,
            fired: vec![Observation {
                detector: DetectorId::PositionJump,
                score: 2.0,
                subject: subject.to_string(),
                at: t,
            }],
        };
        MisbehaviourReport::from_verdict(
            id,
            NodeId::new(reporter),
            format!("rep{reporter:04}"),
            &v,
            &crate::report::Evidence::at(t, t, 2.0),
        )
        .unwrap()
    }

    #[test]
    fn the_legacy_operating_point_holds() {
        let p = MaParams::default();
        assert_eq!(p.report_threshold_k, 3);
        assert_eq!(p.revoke_min_seconds, 4);
        assert_eq!(p.revoke_persist_s, 3.0);
        assert_eq!(p.revoke_window_s, 15.0);
        assert_eq!(p.report_budget, 30);
        assert_eq!(p.reputation_max, 40);
    }

    #[test]
    fn three_reporters_over_four_seconds_revoke_and_two_do_not() {
        let mut ctx = CollectingCtx::new(1);
        let mut ma = LegacyWindow::legacy_defaults();
        // Two reporters, four seconds: not enough reporters.
        for (i, s) in [0u64, 1, 2, 3].into_iter().enumerate() {
            let r = report(&format!("a{i}"), (i % 2) as u32 + 1, "subj", s * NS_PER_S);
            assert!(ma.on_report(&mut ctx, &r).is_empty());
        }
        assert!(!ma.is_revoked("subj"));
        // A third reporter closes it.
        let r = report("a9", 3, "subj", 4 * NS_PER_S);
        let acts = ma.on_report(&mut ctx, &r);
        assert_eq!(
            acts,
            vec![MaAction::Revoke {
                subject: "subj".into()
            }]
        );
        assert!(ma.is_revoked("subj"));
        assert_eq!(ctx.on_channel("ma.decision").len(), 1);
        assert_eq!(ctx.on_channel("ma.report").len(), 5);
    }

    #[test]
    fn evidence_outside_the_window_does_not_accumulate() {
        let mut ctx = CollectingCtx::new(1);
        let mut ma = LegacyWindow::legacy_defaults();
        for (i, r) in [1u32, 2, 3].into_iter().enumerate() {
            let rep = report(&format!("b{i}"), r, "slow", (i as u64) * NS_PER_S);
            ma.on_report(&mut ctx, &rep);
        }
        // Three reporters, but only three distinct seconds and a 2 s span.
        assert!(!ma.is_revoked("slow"));
        // Twenty seconds later the old evidence has aged out of the 15 s window.
        let rep = report("b9", 4, "slow", 25 * NS_PER_S);
        ma.on_report(&mut ctx, &rep);
        assert!(!ma.is_revoked("slow"));
    }

    #[test]
    fn a_rate_limited_reporter_stops_counting_when_the_defence_is_on() {
        let mut ma = LegacyWindow::legacy_defaults();
        ma.filed_by.insert("spray".to_string(), 31);
        assert!(!ma.trusted("spray"));
        ma.reported.insert("bad".to_string(), 40);
        assert!(!ma.trusted("bad"));
        ma.trust_infrastructure("rsu0");
        ma.filed_by.insert("rsu0".to_string(), 10_000);
        assert!(ma.trusted("rsu0"), "infrastructure is never rate-limited");

        let mut off = LegacyWindow::new(MaParams {
            defence: false,
            ..MaParams::default()
        });
        off.filed_by.insert("spray".to_string(), 10_000);
        assert!(off.trusted("spray"));
    }
}
