//! Detection metrics: the confusion matrix, and the summaries derived from it
//! (08-measurement-and-data.md §2.4).
//!
//! # The matrix is the result; the summaries are conveniences
//!
//! A precision of 0.9 can mean 9 true positives out of 10 reports or 9,000 out of 10,000,
//! and it can sit beside 3 false negatives or 3,000. Reporting only the summaries throws
//! away the information a reader needs to judge them, which is why
//! [`ConfusionMatrix`] is reported in full — as four counts, as its own Arrow batch and in
//! the run summary — and the summaries are computed from it rather than accumulated
//! separately. They therefore cannot disagree with it.
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `det_tp`, `det_fp`, `det_fn`, `det_tn` | the four cells | count | nothing: they are exact counts |
//! | `det_recall` (= TPR) | tp / (tp + fn) | ratio | attackers that never acted, which are not in the population |
//! | `det_fpr` | fp / (fp + tn) | ratio | the base rate: with 2 % attackers a 1 % FPR still produces more false than true positives, which is what the matrix shows and the ratio hides |
//! | `det_precision` | tp / (tp + fp) | ratio | the same base-rate caveat, in the other direction |
//! | `det_f1` | 2·tp / (2·tp + fp + fn) | ratio | **a confidence interval**: F1 is a function of two dependent proportions and has no exact binomial interval, so none is fabricated for it |
//! | `det_accuracy` | (tp + tn) / total | ratio | class imbalance: with 2 % attackers, deciding "benign" for everyone scores 0.98 |
//! | `time_to_detect` | first correct report at the authority − attack onset | s | an attacker never reported, which contributes no sample: read it beside `det_recall` |
//! | `time_to_decision` | authority decision − attack onset | s | the same, for attackers never decided about |
//! | `false_accusations` | benign subjects with at least one report, and with a revocation | count | a benign subject reported and then cleared, which is still an accusation and is still counted |
//!
//! # The two levels
//!
//! 08-measurement-and-data.md §2.4 asks for precision and recall "over reports (subject
//! truly misbehaving) and over vehicles (revoked ∩ attacker), as in `validate.py`", so
//! there are two matrices and every summary carries a `level` dimension:
//!
//! * [`DetectionLevel::Report`] — the predictor is "at least one report reached the
//!   authority about this subject". The population is the declared subjects.
//! * [`DetectionLevel::Vehicle`] — the predictor is "the authority decided to revoke this
//!   subject". Same population.
//!
//! # Where the truth comes from
//!
//! A report names its subject the way a node can: by pseudonym digest. Whether that subject
//! is *really* misbehaving is ground truth, and this crate cannot derive the link — it is
//! exactly the ground-truth firewall of invariants I-C2 and I-T2. The run therefore
//! **declares** it: [`DetectionProvider::declare_subject`] states that a subject id belongs
//! to a true actor, and the attacker set comes from the `gt.attack.action` channel. A
//! subject that was never declared is counted into
//! [`DetectionProvider::undeclared_subjects`] and left out of the matrix, because guessing
//! would be worse than reporting the gap.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::ActorId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cards;
use crate::channels::{
    ChannelView, DetObservationView, GtAttackActionView, MaDecisionView, MaReportView, decode,
};
use crate::def::{Agg, DEFAULT_LEVEL, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::{ConfidenceLevel, Distribution, Estimate, Proportion, RatioEstimate};

/// What a detection metric is measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DetectionLevel {
    /// The predictor is "at least one report about this subject reached the authority".
    Report,
    /// The predictor is "the authority decided to revoke this subject".
    Vehicle,
}

impl DetectionLevel {
    /// Both levels, in a fixed order.
    pub const ALL: [DetectionLevel; 2] = [DetectionLevel::Report, DetectionLevel::Vehicle];

    /// The dimension value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DetectionLevel::Report => "report",
            DetectionLevel::Vehicle => "vehicle",
        }
    }
}

impl core::fmt::Display for DetectionLevel {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A 2×2 confusion matrix: the result a detection evaluation reports.
///
/// Four exact integer counts. Every summary is computed from them on demand, so a summary
/// can never contradict the matrix it came from, and the matrix is order-independent because
/// integer addition is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfusionMatrix {
    /// Predicted misbehaving, truly misbehaving.
    pub tp: u64,
    /// Predicted misbehaving, truly benign.
    pub fp: u64,
    /// Predicted benign, truly misbehaving.
    #[serde(rename = "fn")]
    pub fn_: u64,
    /// Predicted benign, truly benign.
    pub tn: u64,
}

impl ConfusionMatrix {
    /// An empty matrix.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tp: 0,
            fp: 0,
            fn_: 0,
            tn: 0,
        }
    }

    /// Records one subject's prediction against its truth.
    pub const fn observe(&mut self, predicted_misbehaving: bool, truly_misbehaving: bool) {
        match (predicted_misbehaving, truly_misbehaving) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, true) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
    }

    /// The total number of subjects in the matrix.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.tp + self.fp + self.fn_ + self.tn
    }

    /// The count in one cell.
    #[must_use]
    pub const fn cell(&self, cell: Cell) -> u64 {
        match cell {
            Cell::Tp => self.tp,
            Cell::Fp => self.fp,
            Cell::Fn => self.fn_,
            Cell::Tn => self.tn,
        }
    }

    /// Recall, also the true-positive rate: `tp / (tp + fn)`. A proportion, so it carries a
    /// Wilson interval.
    #[must_use]
    pub fn recall(&self, min_samples: u64, level: ConfidenceLevel) -> RatioEstimate {
        Proportion::from_counts(self.tp, self.tp + self.fn_).estimate(min_samples, level)
    }

    /// The false-positive rate: `fp / (fp + tn)`. A proportion, so it carries a Wilson
    /// interval.
    #[must_use]
    pub fn fpr(&self, min_samples: u64, level: ConfidenceLevel) -> RatioEstimate {
        Proportion::from_counts(self.fp, self.fp + self.tn).estimate(min_samples, level)
    }

    /// Precision: `tp / (tp + fp)`. A proportion, so it carries a Wilson interval.
    #[must_use]
    pub fn precision(&self, min_samples: u64, level: ConfidenceLevel) -> RatioEstimate {
        Proportion::from_counts(self.tp, self.tp + self.fp).estimate(min_samples, level)
    }

    /// Accuracy: `(tp + tn) / total`. A proportion, so it carries a Wilson interval — and a
    /// caveat about class imbalance that belongs on the definition, not in the arithmetic.
    #[must_use]
    pub fn accuracy(&self, min_samples: u64, level: ConfidenceLevel) -> RatioEstimate {
        Proportion::from_counts(self.tp + self.tn, self.total()).estimate(min_samples, level)
    }

    /// The F1 score: `2·tp / (2·tp + fp + fn)`, the harmonic mean of precision and recall.
    ///
    /// Reported as an [`Estimate`] and **not** as a proportion, so no confidence interval is
    /// attached to it. F1 is a function of two proportions computed over overlapping
    /// samples; there is no exact binomial interval for it, and the intervals people do put
    /// on it come from a bootstrap over replications, which belongs to the experiment layer
    /// (08-measurement-and-data.md §4) and not here. Fabricating one would be worse than
    /// omitting it, so the matrix is reported instead.
    ///
    /// `n` is the matrix's total, so a reader always knows how thin the score is.
    /// A matrix with no positives of either kind has no F1, and reports
    /// [`Estimate::Insufficient`] rather than `0/0`.
    #[must_use]
    pub fn f1(&self, min_samples: u64) -> Estimate {
        let denom = 2 * self.tp + self.fp + self.fn_;
        let required = min_samples.max(1);
        if denom == 0 || self.total() < required {
            return Estimate::Insufficient {
                n: self.total(),
                required,
            };
        }
        Estimate::Value {
            point: (2 * self.tp) as f64 / (denom as f64),
            n: self.total(),
        }
    }
}

/// One cell of a [`ConfusionMatrix`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Cell {
    /// True positive.
    Tp,
    /// False positive.
    Fp,
    /// False negative.
    Fn,
    /// True negative.
    Tn,
}

impl Cell {
    /// Every cell, in a fixed order.
    pub const ALL: [Cell; 4] = [Cell::Tp, Cell::Fp, Cell::Fn, Cell::Tn];

    /// The cell's name, as it appears in a `cell` dimension value and a metric name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Cell::Tp => "tp",
            Cell::Fp => "fp",
            Cell::Fn => "fn",
            Cell::Tn => "tn",
        }
    }
}

impl core::fmt::Display for Cell {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The detection metric provider.
///
/// **Cumulative, not windowed.** Detection is a statement about a whole run: an attacker
/// detected in the third window is not a false negative in the first. Every flush therefore
/// reports the run-to-date matrix and its summaries, each sample carrying its counts, so a
/// reader who wants a per-window view can difference two flushes.
pub struct DetectionProvider {
    card: ModelCard,
    level: ConfidenceLevel,
    min_samples: u64,

    /// The declared subject-to-actor links: the ground-truth join the metric cannot derive.
    subject_actor: BTreeMap<String, ActorId>,
    /// Actors that took at least one attack action that changed bytes on the air.
    attackers: BTreeSet<ActorId>,
    /// The first such action per actor: the attack onset.
    onset: BTreeMap<ActorId, SimTime>,
    /// The first report about each subject that reached the authority.
    first_report: BTreeMap<String, SimTime>,
    /// Reports per subject, for the per-report precision.
    reports: BTreeMap<String, u64>,
    /// The authority's decision per subject, and when.
    decision: BTreeMap<String, (SimTime, String)>,
    /// Subjects named by a report or a decision that were never declared.
    undeclared: BTreeSet<String>,
    /// Local detector firings per detector, for `detector_reliability`'s denominator.
    detector_firings: BTreeMap<String, u64>,

    rejected: u64,
}

impl DetectionProvider {
    /// A provider with no declared subjects.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: Self::build_card(),
            level: DEFAULT_LEVEL,
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            subject_actor: BTreeMap::new(),
            attackers: BTreeSet::new(),
            onset: BTreeMap::new(),
            first_report: BTreeMap::new(),
            reports: BTreeMap::new(),
            decision: BTreeMap::new(),
            undeclared: BTreeSet::new(),
            detector_firings: BTreeMap::new(),
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    /// Sets the confidence level.
    #[must_use]
    pub const fn with_level(mut self, level: ConfidenceLevel) -> Self {
        self.level = level;
        self
    }

    /// Declares that `subject` — the identifier a report or a decision uses — belongs to
    /// the true actor `actor`.
    ///
    /// This is the ground-truth join, and it is a **declaration** rather than an inference
    /// on purpose: a report names its subject by pseudonym digest, and resolving that to an
    /// actor is exactly what a node may not do (invariant I-C2). The run, which is on the
    /// ground-truth side of the firewall, supplies it. Declaring a subject also puts it in
    /// the population, so a benign vehicle nobody reported becomes a true negative rather
    /// than a vehicle nobody counted.
    pub fn declare_subject(&mut self, subject: impl Into<String>, actor: ActorId) {
        self.subject_actor.insert(subject.into(), actor);
    }

    /// Subjects a report or a decision named that were never declared, so they are outside
    /// the matrix.
    #[must_use]
    pub const fn undeclared_subjects(&self) -> &BTreeSet<String> {
        &self.undeclared
    }

    /// The true attacker set, as observed on the ground-truth attack channel.
    #[must_use]
    pub const fn attackers(&self) -> &BTreeSet<ActorId> {
        &self.attackers
    }

    /// True if `subject`'s declared actor took an attack action.
    fn truly_misbehaving(&self, subject: &str) -> Option<bool> {
        self.subject_actor
            .get(subject)
            .map(|a| self.attackers.contains(a))
    }

    /// The matrix at one level, over the declared population.
    #[must_use]
    pub fn matrix(&self, level: DetectionLevel) -> ConfusionMatrix {
        let mut m = ConfusionMatrix::new();
        // Iteration over a `BTreeMap`, so the matrix is built in subject order. Integer
        // counts make it order-independent anyway; the order is fixed so that a future
        // per-subject dump is too.
        for (subject, actor) in &self.subject_actor {
            let truth = self.attackers.contains(actor);
            let predicted = match level {
                DetectionLevel::Report => self.first_report.contains_key(subject),
                DetectionLevel::Vehicle => self
                    .decision
                    .get(subject)
                    .is_some_and(|(_, d)| d == "revoke"),
            };
            m.observe(predicted, truth);
        }
        m
    }

    /// Precision over **reports** rather than over vehicles: reports whose subject is truly
    /// misbehaving, divided by all reports about declared subjects.
    ///
    /// This is the legacy `validate.py` definition, and it differs from the vehicle-level
    /// precision whenever reports are unevenly distributed: one attacker reported a thousand
    /// times and one benign vehicle reported once give a per-report precision of 0.999 and a
    /// per-vehicle precision of 0.5. Both are reported, with the `level` dimension saying
    /// which is which.
    #[must_use]
    pub fn report_precision(&self) -> Proportion {
        let mut p = Proportion::new();
        for (subject, count) in &self.reports {
            if let Some(truth) = self.truly_misbehaving(subject) {
                p.observe_many(if truth { *count } else { 0 }, *count);
            }
        }
        p
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/detection/confusion",
            "1.0.0",
            "The detection confusion matrix and the summaries derived from it — true and \
             false positive rates, precision, recall, F1, accuracy — plus detection and \
             decision latency and the false-accusation counts.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new("recall", "recall = tpr = tp / (tp + fn)"),
            v2xw_core::card::Equation::new("fpr", "fpr = fp / (fp + tn)"),
            v2xw_core::card::Equation::new("precision", "precision = tp / (tp + fp)"),
            v2xw_core::card::Equation::new(
                "f1",
                "f1 = 2·precision·recall / (precision + recall) = 2·tp / (2·tp + fp + fn); \
                 reported without a confidence interval, because no exact binomial interval \
                 exists for a function of two dependent proportions",
            ),
            v2xw_core::card::Equation::new(
                "accuracy",
                "accuracy = (tp + tn) / (tp + fp + fn + tn)",
            ),
            v2xw_core::card::Equation::new(
                "time_to_detect",
                "time_to_detect = t(first report about the subject at the authority) − \
                 t(attack onset)",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.sources = vec![
            cards::design("08-measurement-and-data.md §2.4 (detection)"),
            cards::design(
                "01-inventory.md §5 / legacy validate.py: the two levels, over reports and \
                 over vehicles",
            ),
            cards::standard(
                "ETSI TS 103 759 (misbehaviour reporting): the report the authority receives",
            ),
        ];
        card.limitations = vec![
            "The subject-to-actor link is declared by the run, not derived: a report names \
             its subject by pseudonym digest and resolving that is ground truth (I-C2). A \
             subject nobody declared is outside the matrix and is counted separately."
                .to_string(),
            "F1 carries no confidence interval. An interval across seeds is the experiment \
             layer's bootstrap (08 §4), not a binomial interval on one run."
                .to_string(),
            "Accuracy is reported because it is asked for, and is close to useless under the \
             class imbalance a 2 % attacker fraction produces; the matrix is the answer."
                .to_string(),
            "A pseudonym change makes one vehicle several subjects unless the run declares \
             every pseudonym to the same actor. The count of undeclared subjects is the \
             signal that it did not."
                .to_string(),
        ];
        card.ignores = vec![
            "Detector internals: this provider reads reports and decisions, not scores."
                .to_string(),
            "Residual harm after a decision, which is 08 §2.3.".to_string(),
        ];
        card.validation.tests = vec![
            "detection::tests::the_matrix_and_its_summaries_reproduce_a_hand_computed_fixture"
                .to_string(),
            "detection::tests::per_report_and_per_vehicle_precision_differ_and_both_are_reported"
                .to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2.4");
        let mut defs: Vec<MetricDef> = Cell::ALL
            .into_iter()
            .map(|c| {
                MetricDef::new(
                    format!("det_{c}"),
                    "count",
                    Agg::Count,
                    // The matrix joins node-visible reports to ground-truth attacker
                    // identity, so it is GT-tainted and an exporter must tag it.
                    Visibility::NodeAndGt,
                    Quantum::COUNT,
                    format!(
                        "The `{c}` cell of the detection confusion matrix over the declared \
                         subject population."
                    ),
                )
                .with_dims([Dim::T, Dim::Level, Dim::Cell])
                .with_source(src.clone())
                .with_min_samples(1)
                .not_accounting_for("subjects the run never declared")
                .not_accounting_for("attackers that never acted, which are not in the population")
            })
            .collect();
        defs.push(
            MetricDef::new(
                "det_recall",
                "ratio",
                Agg::ratio("true positives", "truly misbehaving subjects"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Recall, also the true-positive rate: `tp / (tp + fn)`.",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("how many reports each detection took")
            .not_accounting_for("attackers that never acted"),
        );
        defs.push(
            MetricDef::new(
                "det_fpr",
                "ratio",
                Agg::ratio("false positives", "truly benign subjects"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "The false-positive rate: `fp / (fp + tn)`.",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "the base rate: at a 2 % attacker fraction a 1 % false-positive rate still \
                 produces more false than true positives",
            ),
        );
        defs.push(
            MetricDef::new(
                "det_precision",
                "ratio",
                Agg::ratio("true positives", "positives"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Precision: `tp / (tp + fp)`. At the `report` level this is the per-report \
                 precision of the legacy `validate.py`; at the `vehicle` level it is \
                 `revoked ∩ attacker / revoked`.",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("the base rate")
            .not_accounting_for("reports about subjects the run never declared"),
        );
        defs.push(
            MetricDef::new(
                "det_f1",
                "ratio",
                Agg::Mean,
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "The F1 score, `2·tp / (2·tp + fp + fn)`. Reported **without** a confidence \
                 interval: see the card's limitations.",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("a confidence interval, which has no exact binomial form here")
            .not_accounting_for("true negatives, which F1 ignores by construction"),
        );
        defs.push(
            MetricDef::new(
                "det_accuracy",
                "ratio",
                Agg::ratio("correct decisions", "subjects"),
                Visibility::NodeAndGt,
                Quantum::RATIO,
                "Accuracy: `(tp + tn) / total`.",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "class imbalance: with 2 % attackers, calling everyone benign scores 0.98",
            ),
        );
        defs.push(
            MetricDef::new(
                "time_to_detect",
                "s",
                Agg::Distribution,
                Visibility::NodeAndGt,
                Quantum::TIME_S,
                "Attack onset to the first correct report about the attacker at the \
                 authority.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for(
                "attackers never reported, which contribute no sample: read it beside \
                 det_recall",
            )
            .not_accounting_for("reports about benign subjects, which are false_accusations"),
        );
        defs.push(
            MetricDef::new(
                "time_to_decision",
                "s",
                Agg::Distribution,
                Visibility::NodeAndGt,
                Quantum::TIME_S,
                "Attack onset to the authority's decision about the attacker.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("attackers never decided about")
            .not_accounting_for("the decision's propagation, which is revocation_latency_stage"),
        );
        defs.push(
            MetricDef::new(
                "false_accusations",
                "count",
                Agg::Count,
                Visibility::NodeAndGt,
                Quantum::COUNT,
                "Benign subjects with at least one report (`level = report`) and benign \
                 subjects that were revoked (`level = vehicle`).",
            )
            .with_dims([Dim::T, Dim::Level])
            .with_source(src)
            .with_min_samples(1)
            .not_accounting_for("how many times each benign subject was reported")
            .not_accounting_for("a benign subject reported and then cleared, which still counts"),
        );
        defs
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_attack(&mut self, v: &GtAttackActionView) {
        if !v.changed_bytes_on_air {
            return;
        }
        self.attackers.insert(v.actor);
        self.onset.entry(v.actor).or_insert(v.t);
    }

    fn on_report(&mut self, v: &MaReportView) {
        self.first_report.entry(v.subject.clone()).or_insert(v.t);
        *self.reports.entry(v.subject.clone()).or_insert(0) += 1;
        if !self.subject_actor.contains_key(&v.subject) {
            self.undeclared.insert(v.subject.clone());
        }
    }

    fn on_decision(&mut self, v: &MaDecisionView) {
        self.decision
            .entry(v.subject.clone())
            .or_insert((v.t, v.decision.clone()));
        if !self.subject_actor.contains_key(&v.subject) {
            self.undeclared.insert(v.subject.clone());
        }
    }

    fn on_observation(&mut self, v: &DetObservationView) {
        *self.detector_firings.entry(v.detector.clone()).or_insert(0) += 1;
    }

    /// Local detector firings per detector id, in detector order.
    #[must_use]
    pub const fn detector_firings(&self) -> &BTreeMap<String, u64> {
        &self.detector_firings
    }
}

impl Default for DetectionProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for DetectionProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for DetectionProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![
            GtAttackActionView::channel_name(),
            MaReportView::channel_name(),
            MaDecisionView::channel_name(),
            DetObservationView::channel_name(),
        ]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        match ev.channel {
            GtAttackActionView::CHANNEL => match decode::<GtAttackActionView>(ev) {
                Ok(v) => self.on_attack(&v),
                Err(_) => self.rejected += 1,
            },
            MaReportView::CHANNEL => match decode::<MaReportView>(ev) {
                Ok(v) => self.on_report(&v),
                Err(_) => self.rejected += 1,
            },
            MaDecisionView::CHANNEL => match decode::<MaDecisionView>(ev) {
                Ok(v) => self.on_decision(&v),
                Err(_) => self.rejected += 1,
            },
            DetObservationView::CHANNEL => match decode::<DetObservationView>(ev) {
                Ok(v) => self.on_observation(&v),
                Err(_) => self.rejected += 1,
            },
            _ => {}
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();

        for level in DetectionLevel::ALL {
            let m = self.matrix(level);
            let dims_for = |extra: Option<Cell>| {
                let mut d = Dims::new();
                d.insert(Dim::Level, DimValue::label(level.as_str()));
                if let Some(c) = extra {
                    d.insert(Dim::Cell, DimValue::label(c.as_str()));
                }
                d
            };
            for c in Cell::ALL {
                out.push(MetricSample::new(
                    &self.def(&format!("det_{c}")),
                    at,
                    dims_for(Some(c)),
                    SampleValue::count(m.cell(c)),
                ));
            }
            out.push(MetricSample::new(
                &self.def("det_recall"),
                at,
                dims_for(None),
                SampleValue::Ratio(m.recall(self.min_samples, self.level)),
            ));
            out.push(MetricSample::new(
                &self.def("det_fpr"),
                at,
                dims_for(None),
                SampleValue::Ratio(m.fpr(self.min_samples, self.level)),
            ));
            // At the report level, precision is the per-report one of `validate.py`; at the
            // vehicle level it is the matrix's.
            let precision = match level {
                DetectionLevel::Report => self
                    .report_precision()
                    .estimate(self.min_samples, self.level),
                DetectionLevel::Vehicle => m.precision(self.min_samples, self.level),
            };
            out.push(MetricSample::new(
                &self.def("det_precision"),
                at,
                dims_for(None),
                SampleValue::Ratio(precision),
            ));
            out.push(MetricSample::new(
                &self.def("det_f1"),
                at,
                dims_for(None),
                SampleValue::Scalar(m.f1(self.min_samples)),
            ));
            out.push(MetricSample::new(
                &self.def("det_accuracy"),
                at,
                dims_for(None),
                SampleValue::Ratio(m.accuracy(self.min_samples, self.level)),
            ));
            out.push(MetricSample::new(
                &self.def("false_accusations"),
                at,
                dims_for(None),
                SampleValue::count(m.fp),
            ));
        }

        // Latencies: over attackers whose onset and whose first correct report/decision are
        // both known. Built over `BTreeMap`s and reduced by `Distribution`, which sorts, so
        // the summary is a function of the data alone.
        let mut detect = Distribution::new();
        let mut decide = Distribution::new();
        for (subject, actor) in &self.subject_actor {
            if !self.attackers.contains(actor) {
                continue;
            }
            let Some(&onset) = self.onset.get(actor) else {
                continue;
            };
            if let Some(&t) = self.first_report.get(subject)
                && t >= onset
            {
                detect.observe(Duration::between(onset, t).as_secs_f64());
            }
            if let Some((t, decision)) = self.decision.get(subject)
                && decision == "revoke"
                && *t >= onset
            {
                decide.observe(Duration::between(onset, *t).as_secs_f64());
            }
        }
        out.push(MetricSample::new(
            &self.def("time_to_detect"),
            at,
            Dims::new(),
            SampleValue::Distribution(detect.summary(1)),
        ));
        out.push(MetricSample::new(
            &self.def("time_to_decision"),
            at,
            Dims::new(),
            SampleValue::Distribution(decide.summary(1)),
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
    use serde_json::json;
    use v2xw_core::ctx::OwnedRecord;

    fn rec(channel: &'static str, json: serde_json::Value) -> OwnedRecord {
        OwnedRecord {
            channel,
            visibility: Visibility::Node,
            json: serde_json::to_vec(&json).unwrap(),
        }
    }

    fn sample<'a>(samples: &'a [MetricSample], key: &str) -> &'a MetricSample {
        samples.iter().find(|s| s.key() == key).unwrap_or_else(|| {
            panic!(
                "no sample with key {key}; have {:?}",
                samples.iter().map(MetricSample::key).collect::<Vec<_>>()
            )
        })
    }

    #[test]
    fn every_definition_validates_and_the_card_is_accepted() {
        let p = DetectionProvider::new();
        p.validate_defs().unwrap();
        p.card().validate().unwrap();
        p.card().check_api_version().unwrap();
    }

    /// Ten vehicles: four attackers, six benign. The authority revokes three attackers and
    /// one benign vehicle. By hand: tp = 3, fp = 1, fn = 1, tn = 5;
    /// recall = 3/4 = 0.75, fpr = 1/6, precision = 3/4 = 0.75,
    /// f1 = 2·3/(2·3 + 1 + 1) = 0.75, accuracy = 8/10 = 0.8.
    #[test]
    fn the_matrix_and_its_summaries_reproduce_a_hand_computed_fixture() {
        let mut p = DetectionProvider::new().with_min_samples(1);
        for i in 0..10u32 {
            p.declare_subject(format!("s{i}"), ActorId::new(i));
        }
        for i in 0..4u32 {
            p.on_event(&rec(
                "gt.attack.action",
                json!({"t": 1_000_000_000u64, "actor": i, "attacker": "ghost",
                       "action": "false-position"}),
            ));
        }
        // Revoked: three attackers (0, 1, 2) and one benign (7).
        for i in [0u32, 1, 2, 7] {
            p.on_event(&rec(
                "ma.decision",
                json!({"t": 5_000_000_000u64, "subject": format!("s{i}"), "decision": "revoke"}),
            ));
        }
        let m = p.matrix(DetectionLevel::Vehicle);
        assert_eq!(
            m,
            ConfusionMatrix {
                tp: 3,
                fp: 1,
                fn_: 1,
                tn: 5
            }
        );
        assert_eq!(m.total(), 10);
        let s = p.flush(10_000_000_000);
        assert_eq!(
            sample(&s, "det_tp|level=vehicle|cell=tp").value,
            SampleValue::count(3)
        );
        assert_eq!(
            sample(&s, "det_fn|level=vehicle|cell=fn").value,
            SampleValue::count(1)
        );
        assert_eq!(
            sample(&s, "det_recall|level=vehicle").value.point(),
            Some(0.75)
        );
        assert_eq!(
            sample(&s, "det_fpr|level=vehicle").value.point(),
            Some(0.1667),
            "1/6 quantised onto the 1e-4 ratio grid"
        );
        assert_eq!(
            sample(&s, "det_precision|level=vehicle").value.point(),
            Some(0.75)
        );
        assert_eq!(sample(&s, "det_f1|level=vehicle").value.point(), Some(0.75));
        assert_eq!(
            sample(&s, "det_accuracy|level=vehicle").value.point(),
            Some(0.8)
        );
        assert_eq!(
            sample(&s, "false_accusations|level=vehicle").value,
            SampleValue::count(1)
        );
        // The summaries carry the matrix total as their sample count.
        assert_eq!(sample(&s, "det_accuracy|level=vehicle").value.n(), 10);
        // F1 carries no interval.
        match &sample(&s, "det_f1|level=vehicle").value {
            SampleValue::Scalar(_) => {}
            other => panic!("F1 must not be a proportion with an interval: {other:?}"),
        }
    }

    #[test]
    fn per_report_and_per_vehicle_precision_differ_and_both_are_reported() {
        let mut p = DetectionProvider::new().with_min_samples(1);
        p.declare_subject("attacker", ActorId::new(1));
        p.declare_subject("benign", ActorId::new(2));
        p.on_event(&rec(
            "gt.attack.action",
            json!({"t":0,"actor":1,"attacker":"ghost","action":"x"}),
        ));
        // The attacker is reported a thousand times, the benign vehicle once.
        for i in 0..1000u64 {
            p.on_event(&rec(
                "ma.report",
                json!({"t": i, "subject": "attacker", "detector": "d1"}),
            ));
        }
        p.on_event(&rec(
            "ma.report",
            json!({"t": 1, "subject": "benign", "detector": "d1"}),
        ));
        let s = p.flush(1_000_000_000);
        // Per report: 1000 of 1001.
        let per_report = sample(&s, "det_precision|level=report")
            .value
            .point()
            .unwrap();
        assert!((per_report - 0.999).abs() < 1e-9, "{per_report}");
        // Per vehicle at the report level the matrix is tp=1, fp=1 → 0.5, which is what the
        // recall and the matrix cells show.
        assert_eq!(
            sample(&s, "det_tp|level=report|cell=tp").value,
            SampleValue::count(1)
        );
        assert_eq!(
            sample(&s, "det_fp|level=report|cell=fp").value,
            SampleValue::count(1)
        );
        assert_eq!(
            sample(&s, "det_recall|level=report").value.point(),
            Some(1.0)
        );
    }

    #[test]
    fn an_undeclared_subject_is_reported_not_guessed() {
        let mut p = DetectionProvider::new().with_min_samples(1);
        p.on_event(&rec(
            "ma.report",
            json!({"t":0,"subject":"who-is-this","detector":"d1"}),
        ));
        assert_eq!(p.undeclared_subjects().len(), 1);
        assert!(p.undeclared_subjects().contains("who-is-this"));
        // …and it is outside the matrix.
        assert_eq!(p.matrix(DetectionLevel::Report).total(), 0);
    }

    #[test]
    fn an_empty_matrix_is_insufficient_rather_than_nan() {
        let p = DetectionProvider::new();
        let m = ConfusionMatrix::new();
        assert!(m.recall(1, p.level).is_insufficient());
        assert!(m.fpr(1, p.level).is_insufficient());
        assert!(m.precision(1, p.level).is_insufficient());
        assert!(m.accuracy(1, p.level).is_insufficient());
        assert!(m.f1(1).is_insufficient());
        let mut p = DetectionProvider::new();
        let s = p.flush(1_000);
        for x in &s {
            assert!(
                matches!(x.value, SampleValue::Count { count: 0 }) || x.value.is_insufficient(),
                "{}: {:?}",
                x.key(),
                x.value
            );
            for f in x.floats() {
                assert!(f.is_finite(), "{f}");
            }
        }
    }

    /// A matrix with no positives at all — every subject correctly called benign — has no
    /// F1 and no precision, and says so rather than dividing.
    #[test]
    fn a_matrix_with_no_positives_has_no_f1_and_no_precision() {
        let m = ConfusionMatrix {
            tp: 0,
            fp: 0,
            fn_: 0,
            tn: 50,
        };
        assert!(m.f1(1).is_insufficient());
        assert!(m.precision(1, ConfidenceLevel::P95).is_insufficient());
        assert!(m.recall(1, ConfidenceLevel::P95).is_insufficient());
        assert_eq!(m.accuracy(1, ConfidenceLevel::P95).point(), Some(1.0));
        assert_eq!(m.fpr(1, ConfidenceLevel::P95).point(), Some(0.0));
    }

    #[test]
    fn detection_latency_measures_from_onset_to_first_correct_report() {
        let mut p = DetectionProvider::new().with_min_samples(1);
        p.declare_subject("a", ActorId::new(1));
        p.declare_subject("b", ActorId::new(2));
        p.on_event(&rec(
            "gt.attack.action",
            json!({"t":1_000_000_000u64,"actor":1,"attacker":"ghost","action":"x"}),
        ));
        // A later action must not move the onset.
        p.on_event(&rec(
            "gt.attack.action",
            json!({"t":9_000_000_000u64,"actor":1,"attacker":"ghost","action":"x"}),
        ));
        p.on_event(&rec(
            "ma.report",
            json!({"t":3_500_000_000u64,"subject":"a","detector":"d1"}),
        ));
        // A report about a benign subject contributes no detection latency.
        p.on_event(&rec(
            "ma.report",
            json!({"t":2_000_000_000u64,"subject":"b","detector":"d1"}),
        ));
        p.on_event(&rec(
            "ma.decision",
            json!({"t":6_000_000_000u64,"subject":"a","decision":"revoke"}),
        ));
        let s = p.flush(10_000_000_000);
        assert_eq!(sample(&s, "time_to_detect").value.point(), Some(2.5));
        assert_eq!(sample(&s, "time_to_detect").value.n(), 1);
        assert_eq!(sample(&s, "time_to_decision").value.point(), Some(5.0));
    }

    #[test]
    fn a_dismissed_decision_is_not_a_revocation() {
        let mut p = DetectionProvider::new().with_min_samples(1);
        p.declare_subject("a", ActorId::new(1));
        p.on_event(&rec(
            "gt.attack.action",
            json!({"t":0,"actor":1,"attacker":"ghost","action":"x"}),
        ));
        p.on_event(&rec(
            "ma.decision",
            json!({"t":1_000_000_000u64,"subject":"a","decision":"dismiss"}),
        ));
        assert_eq!(
            p.matrix(DetectionLevel::Vehicle),
            ConfusionMatrix {
                tp: 0,
                fp: 0,
                fn_: 1,
                tn: 0
            }
        );
    }

    #[test]
    fn an_attack_that_changed_no_bytes_on_the_air_is_not_an_onset() {
        let mut p = DetectionProvider::new();
        p.on_event(&rec(
            "gt.attack.action",
            json!({"t":0,"actor":1,"attacker":"passive","action":"eavesdrop",
                   "changed_bytes_on_air":false}),
        ));
        assert!(p.attackers().is_empty());
    }

    #[test]
    fn the_matrix_is_order_independent() {
        let events: Vec<OwnedRecord> = (0..6u32)
            .map(|i| {
                rec(
                    "ma.decision",
                    json!({"t": i as u64, "subject": format!("s{i}"), "decision": "revoke"}),
                )
            })
            .collect();
        let run = |order: Vec<usize>| {
            let mut p = DetectionProvider::new().with_min_samples(1);
            for i in 0..6u32 {
                p.declare_subject(format!("s{i}"), ActorId::new(i));
            }
            p.on_event(&rec(
                "gt.attack.action",
                json!({"t":0,"actor":0,"attacker":"g","action":"x"}),
            ));
            for i in order {
                p.on_event(&events[i]);
            }
            p.matrix(DetectionLevel::Vehicle)
        };
        assert_eq!(run((0..6).collect()), run((0..6).rev().collect()));
        assert_eq!(run((0..6).collect()), run(vec![3, 0, 5, 1, 4, 2]));
    }

    #[test]
    fn local_detector_firings_are_counted_per_detector() {
        let mut p = DetectionProvider::new();
        for d in [" art", "art", "sybil"] {
            p.on_event(&rec(
                "det.observation",
                json!({"t":0,"node":1,"detector":d.trim(),"subject":"x","score":1.0}),
            ));
        }
        assert_eq!(p.detector_firings().get("art"), Some(&2));
        assert_eq!(p.detector_firings().get("sybil"), Some(&1));
    }
}
