//! Two-authority identity resolution: the stage that stands between a correlated case and
//! a revocation, and the reason report poisoning is hard rather than free.
//!
//! # What the second authority is for
//!
//! In the CAMP SCMS a pseudonym's linkage value is split between two linkage authorities,
//! LA1 and LA2, and neither alone can tie a certificate to a device: the misbehaviour
//! authority has to ask the PCA for the pre-linkage values and then *both* linkage
//! authorities for their halves (05-protocols.md §2, §8). In the ETSI PKI the equivalent
//! is a single enrolment-authority lookup. That difference is a research question rather
//! than a detail — 07-threats-and-detection.md §3.2 lists identity resolution as a
//! protocol-specific flow with round trips — and it is measurable only if the pipeline can
//! be run with the second authority required and with it not required.
//!
//! [`ResolutionParams::authorities_required`] is that knob: 2 for the SCMS split, 1 for a
//! single-authority lookup.
//!
//! # What this stage refuses
//!
//! Three things, each recorded on `ma.case` so that a run can tell them apart:
//!
//! 1. **A report with no evidence.** No fired check, no reason code, or no attached
//!    evidence reference. The authority re-verifies what it was sent
//!    (07-threats-and-detection.md §3.2, the ingestion stage) and a report that attaches
//!    nothing is not a report.
//! 2. **A report below the firing threshold.** Every detector in this crate is normalised
//!    so that ≈ 1 is its threshold, so a leading score under 1 is a check that *did not
//!    fire*. Acting on it would be acting on an absence of evidence — the same mistake, in
//!    a different place, as treating an unverified signature as a bad one.
//! 3. **A case whose subject cannot be resolved.** The correlation gate passed, but fewer
//!    than the required number of authorities can name the device behind the pseudonym.
//!    The case stays open as [`MaAction::Investigate`] and is re-examined on every tick,
//!    because a resolution that arrives late is still a resolution.
//!
//! # Strict by default, and loudly so
//!
//! A subject no authority has been declared for cannot be resolved, so by default it is
//! **not** revoked and the case says `unresolved-undeclared`. The alternative — assume
//! resolvable when nothing was declared — would make this whole stage inert in any run
//! that forgot to declare, and an inert gate reports a perfect defence. If a scenario
//! genuinely has no linkage model, it sets
//! [`ResolutionParams::assume_resolvable_when_undeclared`] and
//! [`TwoAuthorityResolution::undeclared_subjects`] still counts what it assumed.

use std::collections::{BTreeMap, BTreeSet};

use crate::cards::design;
use crate::ctx::{ThreatCtx, ThreatCtxExt};
use crate::ma::{LegacyWindow, MaAction, MaParams, MaPipeline};
use crate::records::{MaCaseRecord, MaDecisionRecord, MaReportRecord};
use crate::report::MisbehaviourReport;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;

/// The model id the pipeline's card and its decisions carry.
pub const MODEL_ID: &str = "threat/ma/two-authority";

/// Which authority answered a resolution query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Authority {
    /// SCMS linkage authority 1.
    La1,
    /// SCMS linkage authority 2.
    La2,
    /// SCMS pseudonym certificate authority, which holds the pre-linkage values.
    Pca,
    /// ETSI enrolment authority, whose single lookup is the one-authority comparison.
    Ea,
}

impl Authority {
    /// Every authority this stage knows.
    pub const ALL: [Authority; 4] = [
        Authority::La1,
        Authority::La2,
        Authority::Pca,
        Authority::Ea,
    ];

    /// The name a case record and a scenario carry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Authority::La1 => "la1",
            Authority::La2 => "la2",
            Authority::Pca => "pca",
            Authority::Ea => "ea",
        }
    }
}

impl core::fmt::Display for Authority {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a case reached the outcome it did. The `ma.case` `outcome` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseOutcome {
    /// The report was accepted into a case.
    Ingested,
    /// Thrown out: it attached no fired check or no evidence.
    DismissedNoEvidence,
    /// Thrown out: its leading check scored below its own firing threshold.
    DismissedBelowThreshold,
    /// Correlated, but no authority has been declared for the subject.
    UnresolvedUndeclared,
    /// Correlated, but fewer authorities can resolve the subject than required.
    UnresolvedSingleAuthority,
    /// Correlated and resolved: the subject is revoked.
    Revoked,
}

impl CaseOutcome {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CaseOutcome::Ingested => "ingested",
            CaseOutcome::DismissedNoEvidence => "dismissed-no-evidence",
            CaseOutcome::DismissedBelowThreshold => "dismissed-below-threshold",
            CaseOutcome::UnresolvedUndeclared => "unresolved-undeclared",
            CaseOutcome::UnresolvedSingleAuthority => "unresolved-single-authority",
            CaseOutcome::Revoked => "revoked",
        }
    }
}

/// The resolution stage's operating point.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolutionParams {
    /// How many authorities must independently resolve the subject's pseudonym before a
    /// revocation may issue.
    ///
    /// 2 is the CAMP SCMS linkage split (LA1 and LA2); 1 is a single-authority lookup, as
    /// in the ETSI enrolment-authority flow. The comparison is the point.
    pub authorities_required: usize,
    /// Whether the authority re-verifies what a report attached before opening a case.
    pub reverify_evidence: bool,
    /// Whether a subject nothing was declared for is assumed resolvable.
    ///
    /// `false` by default; see the module documentation.
    pub assume_resolvable_when_undeclared: bool,
    /// The correlation operating point the first stage runs at.
    pub correlation: MaParams,
}

impl Default for ResolutionParams {
    fn default() -> Self {
        Self {
            authorities_required: 2,
            reverify_evidence: true,
            assume_resolvable_when_undeclared: false,
            correlation: MaParams::default(),
        }
    }
}

impl ResolutionParams {
    /// The single-authority comparison: one lookup resolves an identity.
    #[must_use]
    pub fn single_authority() -> Self {
        Self {
            authorities_required: 1,
            ..Self::default()
        }
    }
}

/// One case the authority holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    /// The case's id.
    pub case_id: String,
    /// The subject, by pseudonym digest.
    pub subject: String,
    /// When the case last changed.
    pub t: SimTime,
    /// Where it stands.
    pub outcome: CaseOutcome,
    /// Distinct reporter certificates in the evidence.
    pub reporters: u32,
    /// How many of those the authority trusts.
    pub trusted_reporters: u32,
    /// How many authorities could resolve the subject.
    pub authorities_resolved: u32,
}

/// The windowed correlator plus the identity-resolution stage.
#[derive(Debug, Clone)]
pub struct TwoAuthorityResolution {
    card: ModelCard,
    params: ResolutionParams,
    inner: LegacyWindow,
    /// Which authorities can resolve which subject, as the run declares it.
    linkage: BTreeMap<String, BTreeSet<Authority>>,
    /// Correlated but unresolved subjects, with the instant they correlated.
    pending: BTreeMap<String, SimTime>,
    revoked: BTreeSet<String>,
    undeclared: BTreeSet<String>,
    cases: Vec<Case>,
    decisions: Vec<MaAction>,
    ingested: u64,
    dismissed: u64,
    seq: u64,
}

impl TwoAuthorityResolution {
    /// The pipeline at the given operating point.
    #[must_use]
    pub fn new(params: ResolutionParams) -> Self {
        let inner = LegacyWindow::new(params.correlation.clone());
        Self {
            card: card(&params),
            params,
            inner,
            linkage: BTreeMap::new(),
            pending: BTreeMap::new(),
            revoked: BTreeSet::new(),
            undeclared: BTreeSet::new(),
            cases: Vec::new(),
            decisions: Vec::new(),
            ingested: 0,
            dismissed: 0,
            seq: 0,
        }
    }

    /// The pipeline with the SCMS two-linkage-authority split and the legacy correlation
    /// operating point.
    #[must_use]
    pub fn scms_defaults() -> Self {
        Self::new(ResolutionParams::default())
    }

    /// The operating point it runs at.
    #[must_use]
    pub fn params(&self) -> &ResolutionParams {
        &self.params
    }

    /// The correlator underneath, for a run that wants its reporter statistics.
    #[must_use]
    pub fn correlator(&self) -> &LegacyWindow {
        &self.inner
    }

    /// Declares that `authority` can resolve `subject`'s pseudonym.
    ///
    /// A **declaration**, exactly as `v2xw_metrics::detection::DetectionProvider::declare_subject`
    /// is one: which authority holds which half of a linkage value is protocol state on the
    /// ground-truth side of the firewall, and this pipeline may not infer it. The engine's
    /// protocol flow calls this when its query returns.
    pub fn declare_linkage(&mut self, subject: impl Into<String>, authority: Authority) {
        self.linkage
            .entry(subject.into())
            .or_default()
            .insert(authority);
    }

    /// Declares that both SCMS linkage authorities can resolve `subject`.
    pub fn declare_scms_linkage(&mut self, subject: impl Into<String>) {
        let s: String = subject.into();
        self.declare_linkage(s.clone(), Authority::La1);
        self.declare_linkage(s, Authority::La2);
    }

    /// How many authorities can resolve `subject`.
    #[must_use]
    pub fn authorities_resolving(&self, subject: &str) -> usize {
        self.linkage.get(subject).map_or(0, |a| a.len())
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

    /// Subjects whose correlation passed while no authority had been declared for them.
    ///
    /// Reported rather than guessed, for the same reason the metrics crate reports
    /// undeclared subjects: a run with an empty linkage model should see the gap, not a
    /// defence that appears to work.
    #[must_use]
    pub const fn undeclared_subjects(&self) -> &BTreeSet<String> {
        &self.undeclared
    }

    /// Cases still open: correlated, not yet resolvable.
    #[must_use]
    pub fn pending_cases(&self) -> Vec<&str> {
        self.pending.keys().map(String::as_str).collect()
    }

    /// Every case, in the order they changed.
    #[must_use]
    pub fn cases(&self) -> &[Case] {
        &self.cases
    }

    /// How many reports were accepted into a case.
    #[must_use]
    pub fn ingested(&self) -> u64 {
        self.ingested
    }

    /// How many reports were thrown out before correlation.
    ///
    /// The first measurable of poisoning resistance: a forged report that attaches nothing
    /// never reaches the correlator.
    #[must_use]
    pub fn dismissed(&self) -> u64 {
        self.dismissed
    }

    /// Why a report would be thrown out at ingestion, or `None` when it stands up.
    ///
    /// Structural only: no threshold here is a number anybody chose. "It attached no fired
    /// check" and "its leading check scored below the threshold every detector in this
    /// crate is normalised to" are the two things an authority can check without
    /// re-running the reporter's detector.
    #[must_use]
    pub fn ingestion_refusal(&self, r: &MisbehaviourReport) -> Option<CaseOutcome> {
        if !self.params.reverify_evidence {
            return None;
        }
        if r.reason_codes.is_empty()
            || r.detector_outputs.is_empty()
            || r.evidence_msg_refs.is_empty()
        {
            return Some(CaseOutcome::DismissedNoEvidence);
        }
        if r.detector_score < 1.0 {
            return Some(CaseOutcome::DismissedBelowThreshold);
        }
        None
    }

    fn next_case_id(&mut self) -> String {
        self.seq += 1;
        format!("case_{:05}", self.seq)
    }

    fn record_case(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        t: SimTime,
        subject: &str,
        outcome: CaseOutcome,
    ) -> Case {
        let (reporters, trusted) = self.inner.evidence_reporters(subject);
        let resolved = u32::try_from(self.authorities_resolving(subject)).unwrap_or(u32::MAX);
        let case_id = self.next_case_id();
        let case = Case {
            case_id: case_id.clone(),
            subject: subject.to_string(),
            t,
            outcome,
            reporters,
            trusted_reporters: trusted,
            authorities_resolved: resolved,
        };
        ctx.emit(MaCaseRecord {
            t,
            case_id,
            subject: subject.to_string(),
            outcome: outcome.as_str().to_string(),
            reporters,
            trusted_reporters: trusted,
            authorities_resolved: resolved,
            authorities_required: u32::try_from(self.params.authorities_required)
                .unwrap_or(u32::MAX),
        });
        self.cases.push(case.clone());
        case
    }

    /// Decides what a correlated subject becomes: a revocation, or an open investigation.
    fn resolve(&mut self, ctx: &mut dyn ThreatCtx, subject: &str, t: SimTime) -> MaAction {
        let declared = self.authorities_resolving(subject);
        let enough = if declared == 0 {
            self.undeclared.insert(subject.to_string());
            self.params.assume_resolvable_when_undeclared
        } else {
            declared >= self.params.authorities_required
        };
        if enough {
            self.revoked.insert(subject.to_string());
            self.pending.remove(subject);
            self.record_case(ctx, t, subject, CaseOutcome::Revoked);
            MaAction::Revoke {
                subject: subject.to_string(),
            }
        } else {
            self.pending.entry(subject.to_string()).or_insert(t);
            let outcome = if declared == 0 {
                CaseOutcome::UnresolvedUndeclared
            } else {
                CaseOutcome::UnresolvedSingleAuthority
            };
            self.record_case(ctx, t, subject, outcome);
            MaAction::Investigate {
                subject: subject.to_string(),
            }
        }
    }

    fn emit_decision(&mut self, ctx: &mut dyn ThreatCtx, t: SimTime, a: MaAction) -> MaAction {
        ctx.emit(MaDecisionRecord {
            t,
            subject: a.subject().to_string(),
            decision: a.as_str().to_string(),
        });
        self.decisions.push(a.clone());
        a
    }
}

impl Model for TwoAuthorityResolution {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MaPipeline for TwoAuthorityResolution {
    fn on_report(&mut self, ctx: &mut dyn ThreatCtx, r: &MisbehaviourReport) -> Vec<MaAction> {
        // The report arrived, whatever happens to it next: `ma.report` is the transport's
        // record, not the decision's.
        ctx.emit(MaReportRecord {
            t: r.ingest_time,
            reporter: r.reporter,
            subject: r.subject_cert_digest.clone(),
            detector: r.leading_reason().map(str::to_string),
        });
        if let Some(refusal) = self.ingestion_refusal(r) {
            self.dismissed += 1;
            self.record_case(ctx, r.ingest_time, &r.subject_cert_digest, refusal);
            let a = self.emit_decision(
                ctx,
                r.ingest_time,
                MaAction::Dismiss {
                    subject: r.subject_cert_digest.clone(),
                },
            );
            return vec![a];
        }
        self.ingested += 1;
        if self.revoked.contains(&r.subject_cert_digest) {
            // Already revoked: the evidence is still recorded by the correlator, but there
            // is nothing left to decide.
            let _ = self.inner.ingest(r);
            return Vec::new();
        }
        let correlated = self.inner.ingest(r).is_some();
        if !correlated {
            return Vec::new();
        }
        let action = self.resolve(ctx, &r.subject_cert_digest, r.ingest_time);
        vec![self.emit_decision(ctx, r.ingest_time, action)]
    }

    fn on_tick(&mut self, ctx: &mut dyn ThreatCtx, t: SimTime) -> Vec<MaAction> {
        let mut out = Vec::new();
        // New correlations first, in digest order.
        for a in self.inner.correlate_all(t) {
            let subject = a.subject().to_string();
            if self.revoked.contains(&subject) {
                continue;
            }
            let action = self.resolve(ctx, &subject, t);
            out.push(self.emit_decision(ctx, t, action));
        }
        // Then the cases that were waiting on an authority. A resolution that arrives late
        // is still a resolution, which is why an unresolved case is not a dismissal.
        let waiting: Vec<String> = self.pending.keys().cloned().collect();
        for subject in waiting {
            let declared = self.authorities_resolving(&subject);
            if declared >= self.params.authorities_required && declared > 0 {
                let action = self.resolve(ctx, &subject, t);
                out.push(self.emit_decision(ctx, t, action));
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
pub fn card(p: &ResolutionParams) -> ModelCard {
    use serde_json::json;
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::MaPipeline,
        "1.0.0",
        "The legacy windowed correlator followed by protocol identity resolution: a \
         revocation issues only when the required number of authorities can independently \
         resolve the subject's pseudonym, which is what a two-authority linkage split buys \
         against report poisoning.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "ingestion",
            "a report with no fired check, no reason code or no evidence reference is \
             dismissed; so is one whose leading score < 1, which is a check that did not \
             fire under this crate's normalisation",
        ),
        Equation::new(
            "resolution",
            "revoke ⟺ correlation gate passed ∧ |{authorities that resolve subject}| ≥ \
             authorities_required; otherwise investigate and re-examine each tick",
        ),
    ];
    card.parameters = vec![
        Parameter::new(
            "authorities_required",
            "-",
            json!(p.authorities_required),
            Source {
                kind: SourceKind::Standard,
                reference: "CAMP SCMS: a pseudonym's linkage value is split between LA1 and \
                            LA2, so identity resolution needs both (05-protocols.md §2, §8)"
                    .to_string(),
                accessed: Some(crate::cards::LEGACY_ACCESSED.to_string()),
                note: Some(
                    "1 is the single-authority comparison, as in the ETSI enrolment-authority \
                     lookup"
                        .to_string(),
                ),
            },
        ),
        Parameter::new(
            "reverify_evidence",
            "-",
            json!(p.reverify_evidence),
            design("07-threats-and-detection.md §3.2 (ingestion and validation stage)"),
        ),
        Parameter::new(
            "assume_resolvable_when_undeclared",
            "-",
            json!(p.assume_resolvable_when_undeclared),
            design(
                "07-threats-and-detection.md §3.2; false keeps the stage from being inert in \
                 a run with no linkage model",
            ),
        ),
    ];
    card.sources = vec![
        design("07-threats-and-detection.md §3.2 (investigation and decision stages)"),
        design("05-protocols.md §2 and §8 (SCMS linkage split, revocation stages)"),
        design("08-measurement-and-data.md §2.4 (false_accusations, time_to_decision)"),
        crate::cards::paper(
            "B. Brecht et al., A Security Credential Management System for V2X \
             Communications, IEEE T-ITS 2018, arXiv:1802.05323 (the linkage-authority split)",
        ),
    ];
    card.assumptions = vec![
        "Which authority can resolve which pseudonym is declared by the run's protocol \
         flow; this pipeline never infers it, because that inference is the firewall."
            .to_string(),
        "A report's leading score is on this crate's normalisation, where ≈ 1 is the \
         firing threshold."
            .to_string(),
        "The correlation stage is the legacy window, unchanged, so a run can be compared \
         with a `legacy-window` run stage for stage."
            .to_string(),
    ];
    card.limitations = vec![
        "The resolution query's round trips, latency and cost are the protocol crate's \
         (07-threats-and-detection.md §3.2); this stage models the *answer*, not the flow."
            .to_string(),
        "A subject whose correlation gate passed is treated as suspended for the purpose \
         of counting its own reports, because the correlator marks it. That is what \
         Investigate means here; an authority that kept accepting a suspect's reports \
         would need the correlator's trusted() to take case state as an argument."
            .to_string(),
        "No service model: the authority's own processing cost is not charged here."
            .to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![design("07-threats-and-detection.md §3.2")],
        tests: vec![
            "poisoning::the_second_authority_is_what_stops_a_revocation".to_string(),
            "poisoning::a_report_below_the_firing_threshold_is_dismissed".to_string(),
            "poisoning::a_late_resolution_still_revokes".to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_card_validates_and_the_scms_split_is_the_default() {
        let p = ResolutionParams::default();
        assert_eq!(p.authorities_required, 2);
        assert!(p.reverify_evidence);
        assert!(!p.assume_resolvable_when_undeclared);
        assert_eq!(ResolutionParams::single_authority().authorities_required, 1);
        let c = card(&p);
        c.validate().unwrap();
        c.check_api_version().unwrap();
    }

    #[test]
    fn every_outcome_has_a_distinct_wire_spelling() {
        let all = [
            CaseOutcome::Ingested,
            CaseOutcome::DismissedNoEvidence,
            CaseOutcome::DismissedBelowThreshold,
            CaseOutcome::UnresolvedUndeclared,
            CaseOutcome::UnresolvedSingleAuthority,
            CaseOutcome::Revoked,
        ];
        let mut seen = BTreeSet::new();
        for o in all {
            assert!(seen.insert(o.as_str()), "duplicate spelling {}", o.as_str());
        }
        assert_eq!(Authority::ALL.len(), 4);
        assert_eq!(Authority::La2.to_string(), "la2");
    }

    #[test]
    fn declaring_both_linkage_authorities_resolves_a_subject() {
        let mut ma = TwoAuthorityResolution::scms_defaults();
        assert_eq!(ma.authorities_resolving("subj"), 0);
        ma.declare_scms_linkage("subj");
        assert_eq!(ma.authorities_resolving("subj"), 2);
        // Declaring the same authority twice is not two authorities.
        ma.declare_linkage("other", Authority::La1);
        ma.declare_linkage("other", Authority::La1);
        assert_eq!(ma.authorities_resolving("other"), 1);
    }
}
