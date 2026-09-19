//! Runtime diagnostics — and the reason they are kept out of every digested artefact.
//!
//! 08-measurement-and-data.md's metric catalog is about the *simulation*: a PDR, a
//! verification queue depth and a time to collision are properties of the scenario, the
//! seed and the engine build, and two machines running the same run must agree on them to
//! the bit. That is the determinism contract, and the golden digests are how it is checked.
//!
//! The three numbers in this module are not like that:
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `events_per_second` | events processed / **wall-clock** seconds | 1/s | the machine: a laptop on battery and a CI runner disagree by a factor of several, and neither is wrong |
//! | `wall_clock_per_sim_second` | wall-clock seconds / simulated seconds | s/s | the same, plus whatever else the machine was doing |
//! | `memory_high_water_mark` | the peak resident size the host reported | B | the allocator, the page size and the platform's accounting; it is not the engine's own byte count |
//!
//! So they are marked [`crate::MetricDef::diagnostic`], and [`crate::DigestSet`] — the only
//! way to build a digest in this crate — **cannot be constructed with one**. The exclusion
//! is structural, not a rule someone has to remember: see [`crate::summary`].
//!
//! # `events_processed` is not a diagnostic, and that is the point
//!
//! The count of events a run produced *is* a property of the run. It is reported here
//! alongside the three diagnostics, without the flag, and it goes into the digest — because
//! two machines that processed different numbers of events did not run the same simulation,
//! and that is exactly what a determinism gate should catch. Putting the deterministic count
//! and the machine-dependent rates side by side is what makes the distinction legible; the
//! flag, not the neighbourhood, is what decides.
//!
//! # This crate reads no clock
//!
//! The non-negotiable rule is that no wall-clock read appears in engine-facing code. This
//! module therefore has no clock in it: `std::time::Instant` and `SystemTime` appear nowhere
//! in the crate, and the harness that *does* own a clock passes its measurements in through
//! [`RuntimeProvider::observe_elapsed`] and [`RuntimeProvider::observe_memory`]. The
//! integration test `tests/discipline.rs` scans the crate's own sources for a clock read and
//! fails if one appears, so the rule is checked rather than asserted.

use serde_json::json;
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cards;
use crate::channels::{
    ChannelView, DetObservationView, GtAttackActionView, GtKinematicsView, MaDecisionView,
    MaReportView, MacCbrView, NetBytesView, NetFragView, NodeTelemetryView, NodeTxView,
    NodeVerifyView, PhyRxView, ProtoMsgView, ProtoRevocationView, SecCertView,
};
use crate::def::{Agg, Dim, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::MetricProvider;
use crate::quant::Quantum;
use crate::stats::Estimate;

/// The channels this crate knows how to read, which is what [`RuntimeProvider`] counts by
/// default.
///
/// Every one of them is a channel some provider in this crate subscribes to, so the default
/// event count is "events the measurement layer saw" rather than "events the engine
/// produced" — a distinction the metric's definition states.
#[must_use]
pub fn known_channels() -> Vec<ChannelName> {
    vec![
        DetObservationView::channel_name(),
        GtAttackActionView::channel_name(),
        GtKinematicsView::channel_name(),
        MaDecisionView::channel_name(),
        MaReportView::channel_name(),
        MacCbrView::channel_name(),
        NetBytesView::channel_name(),
        NetFragView::channel_name(),
        NodeTelemetryView::channel_name(),
        NodeTxView::channel_name(),
        NodeVerifyView::channel_name(),
        PhyRxView::channel_name(),
        ProtoMsgView::channel_name(),
        ProtoRevocationView::channel_name(),
        SecCertView::channel_name(),
    ]
}

/// The runtime-diagnostics provider.
///
/// Cumulative over the run: the event count, the elapsed wall clock and simulated time, and
/// the memory high-water mark all accumulate and are reported at every flush.
pub struct RuntimeProvider {
    card: ModelCard,
    channels: Vec<ChannelName>,
    events: u64,
    wall: Duration,
    sim: Duration,
    peak_bytes: u64,
    measurements: u64,
}

impl RuntimeProvider {
    /// A provider counting every channel this crate knows.
    #[must_use]
    pub fn new() -> Self {
        Self::counting(known_channels())
    }

    /// A provider counting the given channels.
    #[must_use]
    pub fn counting(channels: Vec<ChannelName>) -> Self {
        let mut channels = channels;
        channels.sort();
        channels.dedup();
        Self {
            card: Self::build_card(),
            channels,
            events: 0,
            wall: Duration::ZERO,
            sim: Duration::ZERO,
            peak_bytes: 0,
            measurements: 0,
        }
    }

    /// Records a wall-clock and simulated-time interval the **caller** measured.
    ///
    /// This crate owns no clock. The harness that drives the run measures the interval and
    /// passes it here, which is why there is no `start()`/`stop()` pair: a pair would need a
    /// stored instant, and a stored instant is a clock read.
    pub fn observe_elapsed(&mut self, wall: Duration, sim: Duration) {
        self.wall = Duration::from_nanos(self.wall.as_nanos().saturating_add(wall.as_nanos()));
        self.sim = Duration::from_nanos(self.sim.as_nanos().saturating_add(sim.as_nanos()));
        self.measurements += 1;
    }

    /// Records a resident-size reading the **caller** took, keeping the maximum.
    ///
    /// The maximum rather than the last, because the metric is a high-water mark: a run that
    /// peaked at 8 GB and ended at 1 GB needs 8 GB of machine.
    pub fn observe_memory(&mut self, bytes: u64) {
        self.peak_bytes = self.peak_bytes.max(bytes);
    }

    /// The events counted so far.
    #[must_use]
    pub const fn events(&self) -> u64 {
        self.events
    }

    /// The memory high-water mark so far.
    #[must_use]
    pub const fn peak_bytes(&self) -> u64 {
        self.peak_bytes
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/runtime/diagnostics",
            "1.0.0",
            "Machine-dependent runtime diagnostics — event throughput, wall clock per \
             simulated second and the memory high-water mark — plus the deterministic event \
             count they are measured against.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "events_per_second",
                "events_per_second = events processed / wall-clock seconds elapsed",
            ),
            v2xw_core::card::Equation::new(
                "wall_clock_per_sim_second",
                "wall_clock_per_sim_second = wall-clock seconds / simulated seconds",
            ),
        ];
        card.parameters = Vec::new();
        card.parameters.push(cards::param(
            "count_channels",
            "count",
            json!(15),
            json!(0),
            json!(64),
            cards::design(
                "03-interfaces.md §14: the number of recording channels the event count is \
                 taken over. Declared because the count's meaning depends on it.",
            ),
        ));
        card.sources = vec![
            cards::design(
                "08-measurement-and-data.md §2 (runtime numbers are diagnostics, not results)",
            ),
            cards::design(
                "02-architecture.md §6.1 and ADR 0004: the determinism contract these three \
                 numbers cannot satisfy, which is why they are excluded from every digest",
            ),
        ];
        card.assumptions.push(
            "The wall-clock and memory readings are taken by the caller: this crate reads no \
             clock and queries no allocator."
                .to_string(),
        );
        card.limitations = vec![
            "All three diagnostics are machine-dependent and are excluded from every digested \
             artefact by construction (crate::DigestSet)."
                .to_string(),
            "The event count is over the channels this provider was told to count, which is \
             not necessarily every channel the run produced."
                .to_string(),
            "The memory high-water mark is whatever the host reported; it is not the engine's \
             own byte accounting and is not comparable across platforms."
                .to_string(),
        ];
        card.ignores = vec![
            "Per-phase timing and profiling, which belong to a benchmark harness rather than \
             to a metric provider."
                .to_string(),
        ];
        card.validation.tests = vec![
            "runtime::tests::the_three_diagnostics_are_flagged_and_the_event_count_is_not"
                .to_string(),
            "summary::tests::a_runtime_diagnostic_cannot_enter_a_digest".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let src = cards::design("08-measurement-and-data.md §2");
        vec![
            MetricDef::new(
                "events_processed",
                "count",
                Agg::Count,
                Visibility::Meta,
                Quantum::COUNT,
                "Events delivered to this provider. A property of the run, **not** of the \
                 machine, so it is digested: two hosts that processed different numbers of \
                 events did not run the same simulation.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("channels this provider was not told to count")
            .not_accounting_for("events the engine produced and the recorder dropped"),
            MetricDef::new(
                "events_per_second",
                "1/s",
                Agg::Rate,
                Visibility::Meta,
                Quantum::COUNT,
                "Events processed per wall-clock second. **A diagnostic**: machine-dependent, \
                 and excluded from every digested artefact.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("the machine it ran on, which dominates it")
            .not_accounting_for("what else the machine was doing")
            .as_diagnostic(),
            MetricDef::new(
                "wall_clock_per_sim_second",
                "s/s",
                Agg::Rate,
                Visibility::Meta,
                Quantum::TIME_S,
                "Wall-clock seconds per simulated second — the run's slowdown factor. **A \
                 diagnostic**: machine-dependent, and excluded from every digested artefact.",
            )
            .with_dims([Dim::T])
            .with_source(src.clone())
            .with_min_samples(1)
            .not_accounting_for("the machine, the thread count and the tier planner's choices")
            .not_accounting_for("time-dilation windows, which change what a simulated second costs")
            .as_diagnostic(),
            MetricDef::new(
                "memory_high_water_mark",
                "B",
                Agg::Max,
                Visibility::Meta,
                Quantum::BYTES,
                "The peak resident size the host reported during the run. **A diagnostic**: \
                 machine-dependent, and excluded from every digested artefact.",
            )
            .with_dims([Dim::T])
            .with_source(src)
            .with_min_samples(1)
            .not_accounting_for("the allocator, the page size and the platform's accounting")
            .not_accounting_for("the engine's own byte counts, which are deterministic")
            .as_diagnostic(),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }
}

impl Default for RuntimeProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for RuntimeProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for RuntimeProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        self.channels.clone()
    }

    fn on_event(&mut self, _ev: &EventRecord) {
        self.events += 1;
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        let mut out = vec![MetricSample::new(
            &self.def("events_processed"),
            at,
            Dims::new(),
            SampleValue::count(self.events),
        )];
        let wall_s = self.wall.as_secs_f64();
        let sim_s = self.sim.as_secs_f64();
        out.push(MetricSample::new(
            &self.def("events_per_second"),
            at,
            Dims::new(),
            SampleValue::Scalar(if wall_s > 0.0 {
                Estimate::Value {
                    point: (self.events as f64) / wall_s,
                    n: self.events,
                }
            } else {
                Estimate::Insufficient {
                    n: self.measurements,
                    required: 1,
                }
            }),
        ));
        out.push(MetricSample::new(
            &self.def("wall_clock_per_sim_second"),
            at,
            Dims::new(),
            SampleValue::Scalar(if sim_s > 0.0 {
                Estimate::Value {
                    point: wall_s / sim_s,
                    n: self.measurements,
                }
            } else {
                Estimate::Insufficient {
                    n: self.measurements,
                    required: 1,
                }
            }),
        ));
        out.push(MetricSample::new(
            &self.def("memory_high_water_mark"),
            at,
            Dims::new(),
            SampleValue::count(self.peak_bytes),
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::OwnedRecord;

    fn rec() -> OwnedRecord {
        OwnedRecord {
            channel: "node.tx",
            visibility: Visibility::Node,
            json: b"{}".to_vec(),
        }
    }

    #[test]
    fn every_definition_validates_and_the_card_is_accepted() {
        let p = RuntimeProvider::new();
        p.validate_defs().unwrap();
        p.card().validate().unwrap();
        p.card().check_api_version().unwrap();
    }

    #[test]
    fn the_three_diagnostics_are_flagged_and_the_event_count_is_not() {
        let defs = RuntimeProvider::new().defs();
        let flagged: Vec<&str> = defs
            .iter()
            .filter(|d| d.diagnostic)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(
            flagged,
            vec![
                "events_per_second",
                "wall_clock_per_sim_second",
                "memory_high_water_mark"
            ]
        );
        let plain: Vec<&str> = defs
            .iter()
            .filter(|d| !d.diagnostic)
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(plain, vec!["events_processed"]);
    }

    #[test]
    fn the_flag_travels_from_the_definition_to_the_sample() {
        let mut p = RuntimeProvider::new();
        p.observe_elapsed(Duration::from_millis(500), Duration::from_secs(10));
        p.observe_memory(1_024);
        p.on_event(&rec());
        let s = p.flush(1_000_000_000);
        for x in &s {
            let expected = x.metric != "events_processed";
            assert_eq!(x.diagnostic, expected, "{}", x.metric);
        }
    }

    /// The hand-computed case: 200 events in 0.5 s of wall clock is 400 events/s, and 0.5 s
    /// of wall clock for 10 s of simulation is 0.05 s/s.
    #[test]
    fn the_rates_reproduce_a_hand_computed_fixture() {
        let mut p = RuntimeProvider::new();
        for _ in 0..200 {
            p.on_event(&rec());
        }
        p.observe_elapsed(Duration::from_millis(500), Duration::from_secs(10));
        p.observe_memory(4_096);
        p.observe_memory(1_024);
        let s = p.flush(10_000_000_000);
        let get = |name: &str| {
            s.iter()
                .find(|x| x.metric == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        assert_eq!(get("events_processed").value, SampleValue::count(200));
        assert_eq!(get("events_per_second").value.point(), Some(400.0));
        assert_eq!(get("wall_clock_per_sim_second").value.point(), Some(0.05));
        assert_eq!(
            get("memory_high_water_mark").value,
            SampleValue::count(4_096),
            "the maximum, not the last reading"
        );
    }

    #[test]
    fn with_no_measurement_the_rates_are_insufficient_rather_than_nan() {
        let mut p = RuntimeProvider::new();
        p.on_event(&rec());
        let s = p.flush(1_000);
        for x in &s {
            if x.metric.ends_with("_per_second") || x.metric.contains("per_sim_second") {
                assert!(x.value.is_insufficient(), "{}", x.metric);
            }
            for f in x.floats() {
                assert!(f.is_finite(), "{f}");
            }
        }
    }

    #[test]
    fn the_counted_channels_are_sorted_and_deduplicated() {
        let p = RuntimeProvider::counting(vec![
            NodeTxView::channel_name(),
            NodeTxView::channel_name(),
            MacCbrView::channel_name(),
        ]);
        let subs: Vec<&str> = p.subscribe().iter().map(|c| c.as_str()).collect();
        assert_eq!(subs, vec!["mac.cbr", "node.tx"]);
    }

    #[test]
    fn the_default_channel_list_matches_the_declared_count() {
        let p = RuntimeProvider::new();
        let declared = p
            .card()
            .parameters
            .iter()
            .find(|x| x.name == "count_channels")
            .and_then(|x| x.default.as_u64())
            .unwrap();
        assert_eq!(p.subscribe().len() as u64, declared);
    }
}
