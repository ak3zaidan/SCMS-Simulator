//! **Worked example — the `MetricProvider` seam.** How much of a detector's output is the
//! same subject over and over.
//!
//! A detector that fires a hundred times about one vehicle and a detector that fires a
//! hundred times about a hundred vehicles produce the same count and mean two entirely
//! different things. The first is one accusation with a long tail; the second is a
//! neighbourhood-wide event, or a detector with a false-positive problem. Nothing in the
//! shipped catalog separates them, so this example does.
//!
//! # The two metrics
//!
//! | Metric | Shape | What it says |
//! |---|---|---|
//! | `example_det_firings` | count, per detector | how many observations that detector produced in the window |
//! | `example_det_repeat_share` | proportion with a Wilson interval, per detector | the share of those observations that were about a subject the same node had already flagged in this window |
//!
//! A repeat share near 1 means the detector is re-reporting a handful of subjects; near 0
//! it means every firing is about somebody new.
//!
//! # The four rules this crate demonstrates that no other example does
//!
//! 1. **A metric provider reads channels, never engine state.** Look at the trait: it gets
//!    `subscribe()`, `on_event(&EventRecord)` and `flush(at)`. There is no world, no actor
//!    index and no clock of its own. A provider written in Python sees exactly what this
//!    one sees, which is what makes the two comparable (08-measurement-and-data.md §1).
//! 2. **A record it cannot read is counted, not swallowed.** `on_event` returns `()`, so
//!    there is nowhere to put a decode failure. A metric computed over half its input with
//!    no indication is worse than no metric, so every failure increments
//!    [`MetricProvider::rejected`] and the run manifest reports the total.
//! 3. **Every definition declares what it does not account for.** `MetricDef::validate`
//!    *refuses* an empty `not_accounted` list, on the grounds that no metric accounts for
//!    everything, so an empty list means the author did not think about it.
//! 4. **A proportion gets a Wilson interval, or a refusal — never a bare ratio.** Below
//!    the definition's `min_samples` the value is
//!    `RatioEstimate::Insufficient`, which a reader can tell apart from a measured zero.
//!    Three firings out of four is not 75 %.
//!
//! # Visibility
//!
//! Both metrics are [`Visibility::Node`], because `det.observation` is node-visible: it
//! records what a node concluded from what it heard. A metric derived from a ground-truth
//! channel would be `Gt` and would propagate that tag into every exported file
//! (08-measurement-and-data.md §1: "visibility tags propagate"). Getting this field wrong
//! is how a misbehaviour-detection dataset acquires a label column.
//!
//! # What it is not
//!
//! Not `detector_reliability` (08-measurement-and-data.md §2.4), which is the *precision*
//! of the reports citing a detector and needs the ground truth about each subject. This
//! provider never learns whether a subject was really misbehaving, and says so.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::{
    Determinism, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::NodeId;
use v2xw_core::model::Model;
use v2xw_core::time::SimTime;
use v2xw_metrics::channels::{ChannelView, DetObservationView, decode};
use v2xw_metrics::def::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use v2xw_metrics::provider::MetricProvider;
use v2xw_metrics::quant::Quantum;
use v2xw_metrics::stats::{ConfidenceLevel, Proportion};

/// The model's stable id.
pub const MODEL_ID: &str = "example/metric/detector-load";

/// The model's own version.
pub const MODEL_VERSION: &str = "1.0.0";

/// The count of observations per detector in the window.
pub const FIRINGS: &str = "example_det_firings";

/// The share of those observations that were repeats about one subject at one node.
pub const REPEAT_SHARE: &str = "example_det_repeat_share";

/// What the provider accumulates for one detector in one window.
#[derive(Debug, Default, Clone)]
struct Window {
    /// Observations seen, whatever their subject.
    firings: u64,
    /// Of those, how many were about a `(node, subject)` pair already seen in this
    /// window — the proportion's numerator, with `firings` minus the first sighting of
    /// each pair as its natural denominator.
    ///
    /// Kept as a [`Proportion`] rather than as two loose counters because a proportion is
    /// two integers and therefore order-independent by construction: there is no
    /// floating-point reduction here to get wrong.
    repeats: Proportion,
    /// The `(node, subject)` pairs seen in this window.
    ///
    /// A [`BTreeSet`], not a `HashSet`: the set's *size* is all that is read today, but a
    /// hash-ordered container is one refactor away from putting a hash seed into an output
    /// ordering, and the rule is cheaper to keep than to audit.
    seen: BTreeSet<(NodeId, String)>,
}

/// The provider.
#[derive(Debug)]
pub struct DetectorLoad {
    card: ModelCard,
    /// One accumulator per detector id, so the output order is the detector-id order
    /// whatever order the records arrived in.
    windows: BTreeMap<String, Window>,
    /// Records on a subscribed channel that would not decode.
    rejected: u64,
    /// The trial count a proportion needs before it is reported as a point estimate.
    min_samples: u64,
    /// The interval's nominal level.
    level: ConfidenceLevel,
}

impl Default for DetectorLoad {
    fn default() -> Self {
        Self::new()
    }
}

impl DetectorLoad {
    /// A provider with the crate-wide defaults: thirty trials and a 95 % interval.
    #[must_use]
    pub fn new() -> Self {
        Self::with_threshold(v2xw_metrics::stats::DEFAULT_MIN_SAMPLES, ConfidenceLevel::P95)
    }

    /// A provider with an explicit reporting threshold and interval level.
    ///
    /// **The card is rebuilt here, not carried over.** `min_samples` is a number the
    /// provider reads at run time, so it appears on the card (invariant I-C3), and a
    /// constructor that changed the number without rebuilding the card would make the
    /// manifest's content hash describe a provider that is not the one running. That is
    /// the whole reason the card is built in the constructor and never mutated: there is
    /// no path by which the two can disagree.
    #[must_use]
    pub fn with_threshold(min_samples: u64, level: ConfidenceLevel) -> Self {
        // Clamped to one, which is what `Proportion::estimate` does with it anyway. A
        // threshold of zero would also put the card's declared default outside the range
        // the card itself declares, which `ModelCard::validate` refuses — a correct
        // refusal arriving at a confusing moment (registration, not construction), so it
        // is prevented here.
        let min_samples = min_samples.max(1);
        Self {
            card: card(min_samples),
            windows: BTreeMap::new(),
            rejected: 0,
            min_samples,
            level,
        }
    }

    /// A provider that reports a point estimate from `min_samples` trials up.
    ///
    /// Lowering it below the default is legitimate — a short scenario has few firings —
    /// and it is a *declared* choice: the number reaches the metric definition, which is
    /// exported beside every sample, so a reader of the table can see what threshold the
    /// number was reported under.
    #[must_use]
    pub fn with_min_samples(self, min_samples: u64) -> Self {
        Self::with_threshold(min_samples, self.level)
    }

    /// A provider reporting intervals at `level`.
    #[must_use]
    pub fn with_level(self, level: ConfidenceLevel) -> Self {
        Self::with_threshold(self.min_samples, level)
    }

    /// The definition of one of this provider's metrics.
    ///
    /// Built on demand from one function, so the definition a sample carries and the
    /// definition the generated catalog page prints are the same value. A provider that
    /// kept two copies would eventually disagree with itself.
    #[must_use]
    pub fn def(&self, name: &str) -> MetricDef {
        let source = Source {
            kind: SourceKind::Paper,
            reference: "08-measurement-and-data.md §2.4 (the detection metric family) and \
                        §1 (a metric is a MetricDef computed from the typed channels)"
                .to_string(),
            accessed: Some("2026-09-22".to_string()),
            note: Some(
                "Neither metric is one §2.4 lists: `detector_reliability` is the precision \
                 of the reports citing a detector, which needs the ground truth about each \
                 subject. These two are computed from node-visible records alone."
                    .to_string(),
            ),
        };
        match name {
            REPEAT_SHARE => MetricDef::new(
                REPEAT_SHARE,
                "ratio",
                Agg::ratio(
                    "observations about a (node, subject) pair already seen in this window",
                    "observations after the first sighting of each pair",
                ),
                Visibility::Node,
                Quantum::RATIO,
                "Of the observations one detector produced in the window, the share that \
                 were about a subject the *same node* had already flagged in that window. \
                 A share near 1 means the detector is re-reporting a few subjects; near 0, \
                 that every firing names somebody new.",
            )
            .with_dims([Dim::T, Dim::Detector])
            .with_source(source)
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "whether any subject was really misbehaving: that is ground truth, and \
                 this provider reads node-visible records only",
            )
            .not_accounting_for(
                "repeats across windows — a subject flagged in two consecutive windows \
                 counts as new in the second",
            )
            .not_accounting_for(
                "pseudonym changes, which make one device several subjects and therefore \
                 lower the share",
            ),
            // Anything else is the count. A `match` with a `_` arm rather than one arm per
            // name, because `defs()` below is the list of names that exist and a second
            // list here could drift from it.
            _ => MetricDef::new(
                FIRINGS,
                "count",
                Agg::Count,
                Visibility::Node,
                Quantum::COUNT,
                "How many `det.observation` records one detector produced in the window, \
                 over every node and every subject.",
            )
            .with_dims([Dim::T, Dim::Detector])
            .with_source(source)
            .not_accounting_for(
                "observations a node computed and did not record, which a verification \
                 policy that never delivered the message would cause",
            )
            .not_accounting_for(
                "the severity of a firing: a score of 1.001 and a score of 12 count the \
                 same",
            ),
        }
    }

    /// Consumes one decoded observation.
    ///
    /// Separated from [`MetricProvider::on_event`] so a test can feed the provider typed
    /// values without building a JSON record — and so a reader can see the accounting on
    /// its own.
    pub fn observe(&mut self, node: NodeId, detector: &str, subject: &str) {
        let window = self.windows.entry(detector.to_string()).or_default();
        window.firings += 1;
        let key = (node, subject.to_string());
        // The first sighting of a pair is not a trial: there was nothing it could have
        // repeated. Every sighting after it is one trial, and it is a success exactly
        // when the pair was already there. That makes the denominator "observations after
        // the first sighting of each pair", which is what the definition says.
        if !window.seen.insert(key) {
            window.repeats.observe(true);
        } else if window.firings > 1 {
            // A new pair, in a window that already had at least one observation from this
            // detector: a trial that could have been a repeat and was not.
            window.repeats.observe(false);
        }
    }
}

impl Model for DetectorLoad {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for DetectorLoad {
    fn defs(&self) -> Vec<MetricDef> {
        vec![self.def(FIRINGS), self.def(REPEAT_SHARE)]
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        // The name comes from the record type, so it cannot be misspelled in a string
        // literal. `DetObservationView::channel_name()` is `ChannelName("det.observation")`
        // and the compiler is what keeps the two in step.
        vec![DetObservationView::channel_name()]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        // A record on a channel this provider did not subscribe to never arrives — the
        // set dispatches by channel — but checking is one comparison and it makes the
        // decode failure below unambiguous: a rejection is a schema mismatch, not a
        // misrouted record.
        if ev.channel != DetObservationView::CHANNEL {
            return;
        }
        match decode::<DetObservationView>(ev) {
            Ok(view) => self.observe(view.node, &view.detector, &view.subject),
            // Counted, never swallowed. See rule 2 in the crate documentation.
            Err(_) => self.rejected += 1,
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = Vec::new();
        // `take` drains the accumulators, which is the windowing half of the contract:
        // flushing twice at one instant produces the samples once and then an empty set,
        // never the same numbers twice. Both of this provider's metrics are windowed;
        // neither is cumulative.
        for (detector, window) in core::mem::take(&mut self.windows) {
            let mut dims = Dims::new();
            dims.insert(Dim::Detector, DimValue::label(detector));
            out.push(MetricSample::new(
                &self.def(FIRINGS),
                at,
                dims.clone(),
                SampleValue::count(window.firings),
            ));
            out.push(MetricSample::new(
                &self.def(REPEAT_SHARE),
                at,
                dims,
                // The Wilson interval, computed by `v2xw-metrics` and not by this crate.
                // A provider that wrote its own interval for a proportion would be
                // re-deriving something this repository has already reasoned about, and
                // getting it wrong in a way no test would notice.
                SampleValue::Ratio(window.repeats.estimate(self.min_samples, self.level)),
            ));
        }
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

/// Builds the card.
#[must_use]
pub fn card(min_samples: u64) -> ModelCard {
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Metric,
        MODEL_VERSION,
        "Worked example. Two metrics over the local-detection channel: how many \
         observations each detector produced in the window, and what share of them were \
         about a subject the same node had already flagged. Separates a detector that \
         found one problem from a detector that found many.",
    );
    // A metric provider is tier-independent: it reads records, and a record from an
    // abstract-tier run has the same shape as one from a high-tier run. Declaring all
    // three is the honest answer; declaring one would make the provider unselectable in
    // the other two.
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];

    card.parameters = vec![Parameter {
        range: Some(vec![serde_json::json!(1), serde_json::json!(10_000)]),
        ..Parameter::new(
            "min_samples",
            "-",
            serde_json::json!(min_samples),
            Source {
                kind: SourceKind::Code,
                reference: "v2xw_metrics::stats::DEFAULT_MIN_SAMPLES".to_string(),
                accessed: Some("2026-09-22".to_string()),
                note: Some(
                    "Thirty, the conventional threshold below which a binomial point \
                     estimate misleads more than it informs. It is a declared default of \
                     the metrics crate rather than a discovered one, and its own \
                     documentation says so; this provider adopts it rather than choosing \
                     a second number."
                        .to_string(),
                ),
            },
        )
    }];

    card.assumptions = vec![
        "A subject id identifies a sender for as long as it holds that pseudonym. A \
         pseudonym change makes one device two subjects, which lowers the repeat share."
            .to_string(),
        "Every `det.observation` record reaches this provider. A provider registered after \
         the run started would see a truncated window and report it as a short one."
            .to_string(),
    ];

    card.limitations = vec![
        "The window boundary is where the run flushes, so a detector that fires either \
         side of one boundary is two windows' worth of new subjects rather than one \
         window's worth of repeats."
            .to_string(),
        "The repeat share has no meaning for a detector that fired once: with one \
         observation there are no trials, and the value is `Insufficient` rather than 0."
            .to_string(),
    ];

    card.ignores = vec![
        "The score on each observation, so a barely-over-threshold firing and an \
         egregious one weigh the same."
            .to_string(),
        "Which node did the flagging, beyond keeping the pairs apart: the metric is per \
         detector, not per node. Adding `Dim::Node` to both definitions is the change, and \
         it multiplies the row count by the fleet size."
            .to_string(),
        "Whether a report was ever filed about the subject, which lives on `ma.report`."
            .to_string(),
    ];

    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "a_single_subject_flagged_repeatedly_is_all_repeats".to_string(),
            "distinct_subjects_are_no_repeats".to_string(),
            "two_nodes_flagging_one_subject_are_not_repeats".to_string(),
            "an_undecodable_record_is_counted_not_swallowed".to_string(),
            "flushing_twice_does_not_report_the_window_twice".to_string(),
        ],
    };

    // A metric provider is a pure reduction of records. It draws nothing, and a provider
    // that did would be reporting a random number as a measurement.
    card.determinism = Determinism::default();

    card
}

/// Registers the provider's card and hands the live object to `set`.
///
/// What goes into the registry is the **card**, not the live object: the registry's handle
/// type is an immutable `Arc<dyn Model + Send + Sync>` and a metric provider is mutated on
/// every event, so the two could never be the same value. The card pins the content hash
/// the manifest needs and the [`v2xw_metrics::ProviderSet`] owns the object.
///
/// # Errors
/// Whatever the registry or the definition validator refused, by name.
pub fn register(
    registry: &mut v2xw_core::registry::Registry,
    set: &mut v2xw_metrics::ProviderSet,
) -> v2xw_metrics::Result<v2xw_core::registry::ModelRef> {
    set.register(registry, Box::new(DetectorLoad::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::OwnedRecord;
    use v2xw_core::registry::Registry;
    use v2xw_metrics::ProviderSet;

    const DETECTOR: &str = "example/detector/heading-rate";

    fn record(node: u32, detector: &str, subject: &str, score: f64) -> OwnedRecord {
        let json = serde_json::json!({
            "t": 1_000_000_000u64,
            "node": node,
            "detector": detector,
            "subject": subject,
            "score": score,
        });
        OwnedRecord {
            channel: DetObservationView::CHANNEL,
            visibility: Visibility::Node,
            json: serde_json::to_vec(&json).expect("the fixture serialises"),
        }
    }

    fn sample<'a>(samples: &'a [MetricSample], key: &str) -> &'a MetricSample {
        samples
            .iter()
            .find(|s| s.key() == key)
            .unwrap_or_else(|| panic!("no sample keyed {key} in {:?}", keys(samples)))
    }

    fn keys(samples: &[MetricSample]) -> Vec<String> {
        samples.iter().map(MetricSample::key).collect()
    }

    #[test]
    fn the_card_validates_and_so_does_every_definition() {
        card(v2xw_metrics::stats::DEFAULT_MIN_SAMPLES)
            .validate()
            .expect("the card must validate");
        // `validate_defs` is defaulted on the trait in terms of `defs()`, so a provider
        // gets this check for free — including the rule that every definition names at
        // least one thing it does not account for.
        DetectorLoad::new()
            .validate_defs()
            .expect("every definition must validate");
    }

    #[test]
    fn registration_goes_through_the_core_registry() {
        let mut registry = Registry::new();
        let mut set = ProviderSet::new();
        let reference = register(&mut registry, &mut set).expect("registration must succeed");
        assert_eq!(registry.get_ref(reference).unwrap().card.id, MODEL_ID);
        assert_eq!(set.len(), 1);
        // The live object is the set's, not the registry's, which is the honest answer
        // and not an omission.
        assert!(registry.get_model(reference).is_none());
        assert_eq!(
            set.channels()
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>(),
            vec!["det.observation"]
        );
    }

    #[test]
    fn a_single_subject_flagged_repeatedly_is_all_repeats() {
        // Four firings about one subject at one node: the first is the sighting, the other
        // three are trials and all three are repeats.
        let mut p = DetectorLoad::new().with_min_samples(1);
        for _ in 0..4 {
            p.observe(NodeId::new(0), DETECTOR, "aabb");
        }
        let samples = p.flush(1_000_000_000);
        let firings = sample(&samples, &format!("{FIRINGS}|detector={DETECTOR}"));
        assert_eq!(firings.value, SampleValue::count(4));
        let share = sample(&samples, &format!("{REPEAT_SHARE}|detector={DETECTOR}"));
        assert_eq!(share.value.point(), Some(1.0));
        assert_eq!(share.value.n(), 3, "three trials, not four");
    }

    /// The assertion that makes the metric a measurement rather than a restatement of the
    /// count.
    ///
    /// **Shown to fail:** dropping the `else if` arm in [`DetectorLoad::observe`] — so a
    /// new pair is never counted as a non-repeat — makes this share `Insufficient` with
    /// zero trials and the test goes red. That is how it was checked.
    #[test]
    fn distinct_subjects_are_no_repeats() {
        let mut p = DetectorLoad::new().with_min_samples(1);
        for i in 0..4u32 {
            p.observe(NodeId::new(0), DETECTOR, &format!("subject-{i}"));
        }
        let samples = p.flush(1_000_000_000);
        let share = sample(&samples, &format!("{REPEAT_SHARE}|detector={DETECTOR}"));
        assert_eq!(share.value.point(), Some(0.0));
        assert_eq!(share.value.n(), 3);
    }

    #[test]
    fn two_nodes_flagging_one_subject_are_not_repeats() {
        // The pair is `(node, subject)`: two receivers independently flagging one sender
        // is breadth of evidence, which is the opposite of a repeat, and conflating the
        // two would make a genuinely corroborated accusation look like noise.
        let mut p = DetectorLoad::new().with_min_samples(1);
        p.observe(NodeId::new(0), DETECTOR, "aabb");
        p.observe(NodeId::new(1), DETECTOR, "aabb");
        let samples = p.flush(1_000_000_000);
        let share = sample(&samples, &format!("{REPEAT_SHARE}|detector={DETECTOR}"));
        assert_eq!(share.value.point(), Some(0.0));
    }

    #[test]
    fn a_thin_window_refuses_a_point_estimate() {
        // Two firings with the crate default of thirty trials: the count is a
        // measurement, the share is a refusal, and a reader can tell them apart.
        let mut p = DetectorLoad::new();
        p.observe(NodeId::new(0), DETECTOR, "aabb");
        p.observe(NodeId::new(0), DETECTOR, "aabb");
        let samples = p.flush(1_000_000_000);
        let firings = sample(&samples, &format!("{FIRINGS}|detector={DETECTOR}"));
        assert_eq!(firings.value, SampleValue::count(2));
        let share = sample(&samples, &format!("{REPEAT_SHARE}|detector={DETECTOR}"));
        assert!(
            share.value.is_insufficient(),
            "one trial must not be reported as 100 %"
        );
        assert_eq!(share.value.point(), None);
    }

    #[test]
    fn detectors_are_kept_apart_and_come_out_in_id_order() {
        let mut p = DetectorLoad::new().with_min_samples(1);
        p.observe(NodeId::new(0), "zebra", "aabb");
        p.observe(NodeId::new(0), "alpha", "aabb");
        p.observe(NodeId::new(0), "alpha", "aabb");
        let samples = p.flush(1_000_000_000);
        // Four samples, and the `alpha` pair before the `zebra` pair whatever order the
        // records arrived in, because the accumulator map is a BTreeMap.
        assert_eq!(
            keys(&samples),
            vec![
                format!("{FIRINGS}|detector=alpha"),
                format!("{REPEAT_SHARE}|detector=alpha"),
                format!("{FIRINGS}|detector=zebra"),
                format!("{REPEAT_SHARE}|detector=zebra"),
            ]
        );
        assert_eq!(
            sample(&samples, &format!("{FIRINGS}|detector=alpha")).value,
            SampleValue::count(2)
        );
    }

    #[test]
    fn the_record_path_and_the_typed_path_agree() {
        let mut typed = DetectorLoad::new().with_min_samples(1);
        typed.observe(NodeId::new(3), DETECTOR, "aabb");
        typed.observe(NodeId::new(3), DETECTOR, "aabb");

        let mut recorded = DetectorLoad::new().with_min_samples(1);
        recorded.on_event(&record(3, DETECTOR, "aabb", 1.25));
        recorded.on_event(&record(3, DETECTOR, "aabb", 1.25));

        assert_eq!(
            keys(&typed.flush(1_000)),
            keys(&recorded.flush(1_000)),
            "a decoded record must be indistinguishable from the typed call"
        );
        assert_eq!(recorded.rejected(), 0);
    }

    #[test]
    fn an_undecodable_record_is_counted_not_swallowed() {
        let mut p = DetectorLoad::new();
        // The right channel, the wrong shape: `node` missing. A provider that ignored it
        // would report a window computed over one record while claiming two.
        p.on_event(&OwnedRecord {
            channel: DetObservationView::CHANNEL,
            visibility: Visibility::Node,
            json: br#"{"t":1,"detector":"d","subject":"s"}"#.to_vec(),
        });
        assert_eq!(p.rejected(), 1);
        assert!(p.flush(1_000).is_empty(), "nothing was counted");
    }

    #[test]
    fn a_record_on_another_channel_is_ignored_and_not_rejected() {
        let mut p = DetectorLoad::new();
        p.on_event(&OwnedRecord {
            channel: "node.tx",
            visibility: Visibility::Node,
            json: b"{}".to_vec(),
        });
        assert_eq!(p.rejected(), 0, "a misrouted record is not a schema mismatch");
        assert!(p.flush(1_000).is_empty());
    }

    #[test]
    fn flushing_twice_does_not_report_the_window_twice() {
        let mut p = DetectorLoad::new().with_min_samples(1);
        p.observe(NodeId::new(0), DETECTOR, "aabb");
        p.observe(NodeId::new(0), DETECTOR, "aabb");
        assert_eq!(p.flush(1_000).len(), 2);
        assert!(
            p.flush(2_000).is_empty(),
            "the second flush must not repeat the first window"
        );
    }

    #[test]
    fn every_exported_float_sits_on_its_grid() {
        // Build decision D9, on the samples that actually leave the provider.
        // `MetricSample::new` is the only constructor and it quantises, so this test is
        // really checking that no other path exists — which is why it is worth having.
        let mut p = DetectorLoad::new().with_min_samples(1);
        for i in 0..7u32 {
            p.observe(NodeId::new(i % 3), DETECTOR, &format!("s-{}", i % 4));
        }
        for s in p.flush(1_000) {
            for (value, quantum) in s.graded_floats() {
                assert!(
                    quantum.holds(value),
                    "{value} is off the {} grid",
                    quantum.get()
                );
            }
        }
    }
}
