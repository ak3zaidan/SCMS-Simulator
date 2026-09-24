//! `v2xw-metrics` — the metric providers and the invariant checks.
//!
//! The measurement layer of the V2X World Simulator, specified by
//! `docs/design/08-measurement-and-data.md` (the metric catalog, with a formula and a source
//! per metric) and `docs/design/03-interfaces.md` §10 (the [`MetricProvider`] seam) and §14
//! (the event channels a provider reads).
//!
//! Never touches engine state: a provider reads recorded events and reduces them
//! (02-architecture.md §2, ADR 0010).
//!
//! # What is here
//!
//! | Module | What it holds |
//! |---|---|
//! | [`provider`] | the [`MetricProvider`] trait as 03-interfaces.md §10 publishes it, and [`ProviderSet`], which registers providers through `v2xw_core::Registry` and dispatches events to them |
//! | [`def`] | [`MetricDef`] — name, unit, dimensions, aggregation, visibility, definition, source, **grid**, **insufficiency threshold**, **what it does not account for** — and [`MetricSample`], the `metric.sample` record |
//! | [`stats`] | the honest statistics: sample counts, [`Proportion`] with the Wilson score interval, [`Distribution`] with one stated percentile rule, and insufficiency instead of a point estimate over too few samples |
//! | [`bins`] | binning on the integer grid, so a boundary case cannot flip between platforms |
//! | [`channels`] | reader-side views of the event channels of 03-interfaces.md §14 |
//! | [`comms`], [`latency`], [`awareness`], [`load`], [`overhead`], [`security`], [`detection`], [`safety`], [`runtime`] | the metric families |
//! | [`invariants`] | each stated invariant as a runnable check that returns which invariant failed and with what numbers |
//! | [`gate`] | the model-card completeness gate of 10-roadmap.md Phase 6, as a function over the registry: no `todo-calibrate` on a `high`-tier default without a tracked calibration issue |
//! | [`ledger`] | the decoded record history the invariant checks read |
//! | [`arrow_out`] | Arrow record batches for tabular output |
//! | [`summary`] | the run-manifest summary, and the digest runtime diagnostics cannot enter |
//!
//! # The five families, and what each is measured with
//!
//! Every metric states its formula, its unit and — this is the part a measurement layer
//! usually omits — **what it does not account for** ([`MetricDef::not_accounted`], which
//! [`MetricDef::validate`] refuses to leave empty, because no metric accounts for
//! everything). The per-module tables hold the details:
//!
//! * **Communication** ([`comms`]): `pdr` by distance bin, `per`, `pdr_by_cause`, `cbr`,
//!   `pir`, `goodput`, `bytes_air`/`bytes_uu_ul`/`bytes_uu_dl`/`bytes_backhaul`/
//!   `bytes_backend` and their sum `bytes_total`, `airtime_per_node`.
//! * **Latency** ([`latency`]): `e2e_latency` (p50, p95, p99) per flow and message type,
//!   decomposed into contiguous stages — `latency_stage`, `latency_stage_share` — with the
//!   general [`latency::LatencyTrace`] every backend or multi-hop flow plugs into.
//! * **Awareness** ([`awareness`]): `aoi`, `aoi_peak`, `nar`, `delivery_ratio` by distance.
//! * **Load** ([`load`]): `channel_occupancy`, `channel_load`, `offered_load`,
//!   `carried_load`, `loss_rate` by cause, `collision_rate`, `half_duplex_rate`,
//!   `mac_queue_depth`, `mac_drops`, `mac_access_delay`.
//! * **Overhead** ([`overhead`]): `security_overhead`, `net_header_overhead`,
//!   `link_overhead`, `cert_bytes_share`, `air_bytes_per_payload_byte`,
//!   `bytes_per_vehicle_hour` per bucket.
//! * **Security** ([`security`]): `verify_rate`, `verify_cost`, `verify_wait`,
//!   `verify_queue_depth`, `unverified_ratio`, `full_cert_share`, `envelope_overhead`,
//!   `revocation_latency_stage`, `crl_entries`, `crl_bytes`.
//! * **Detection** ([`detection`]): the [`ConfusionMatrix`] itself — four counts, reported
//!   in full — and `det_recall`, `det_fpr`, `det_precision`, `det_f1`, `det_accuracy`,
//!   `time_to_detect`, `time_to_decision`, `false_accusations` derived from it.
//! * **Mobility and safety** ([`safety`]): `ttc_min`, `ttc_conflicts`, `pet`, `drac`,
//!   `headway_time`, `headway_distance`, Edie's `flow`/`density`/`mean_speed`, `speed`,
//!   `acceleration`.
//! * **Runtime** ([`runtime`]): `events_per_second`, `wall_clock_per_sim_second`,
//!   `memory_high_water_mark` — diagnostics, excluded from every digest by construction.
//!
//! # Statistics done honestly
//!
//! Five rules, enforced by the types rather than by review ([`stats`] has the reasoning):
//!
//! 1. every aggregate carries its sample count;
//! 2. a proportion carries a **Wilson score interval** ([`stats::wilson_interval`]), and a
//!    ratio that is *not* a proportion — bytes over bytes — carries none and says so
//!    ([`RatioEstimate::RatioOfSums`]);
//! 3. too few samples report [`RatioEstimate::Insufficient`], never a point estimate;
//! 4. percentiles name their interpolation rule ([`Interpolation::Type7Linear`]);
//! 5. nothing divides by zero — a zero denominator is insufficiency, not a `NaN`.
//!
//! # Determinism
//!
//! * Reductions are ordered. Counts, bytes and airtime accumulate as **integers**, which are
//!   exactly associative. Float reductions go through [`Distribution`], which sorts into
//!   IEEE-754 total order before summing with `v2xw_core::math::sum_ordered`; per-entity
//!   reductions iterate `BTreeMap`s in id order and use
//!   `v2xw_core::math::sum_sorted_by_key`. Every aggregate is therefore a function of the
//!   data and not of the arrival order, the thread count or the flush order.
//! * Binning is on integer grid indices ([`bins`]), so a value one bit either side of a
//!   25 m edge lands in the same bin everywhere — build decision D10.
//! * No `std` transcendental is called: the one non-`+ - * /` operation in the crate is
//!   `v2xw_core::math::sqrt`, inside the Wilson interval.
//! * No `HashMap` iteration reaches an output ordering or a hash. The single
//!   `std::collections::HashMap` in the crate holds one entry, the Arrow schema id, because
//!   Arrow's API takes that type; [`arrow_out`] says so where it is.
//! * No wall clock is read. The runtime diagnostics take their measurements as arguments
//!   ([`runtime::RuntimeProvider::observe_elapsed`]); the crate has no clock in it, which
//!   `tests/discipline.rs` checks by scanning its own sources.
//! * Every float that reaches an output is quantised at the writer on its field's declared
//!   grid, and a digest hashes the **integer multiple** of that grid rather than the rounded
//!   float — build decision D9, implemented in [`quant`] and checked by
//!   [`invariants::check_d9_quantisation`], which holds each float to the grid the writer
//!   put it on (a proportion's bounds are on the finer probability grid, a ratio of sums'
//!   two sums on the sum grid) and reports a non-finite value, which no grid test can see.
//! * A machine-dependent runtime diagnostic reaches neither a digest nor a **digested
//!   document**: [`RunSummary`] does not serialise its diagnostics, so the file digest a run
//!   manifest records for the summary is reproducible across machines, and the numbers leave
//!   through [`RunSummary::diagnostics_json`] as their own sidecar.
//!
//! # A run, end to end
//!
//! ```
//! use v2xw_core::ctx::{OwnedRecord, Visibility};
//! use v2xw_core::registry::Registry;
//! use v2xw_metrics::{EventLedger, ProviderSet, RunSummary};
//! use v2xw_metrics::comms::CommsProvider;
//!
//! let mut registry = Registry::new();
//! let mut providers = ProviderSet::new();
//! providers.register(&mut registry, Box::new(CommsProvider::new(0)))?;
//!
//! // The run's records reach the providers and, for an audited run, a ledger.
//! let record = OwnedRecord {
//!     channel: "node.tx",
//!     visibility: Visibility::Node,
//!     json: br#"{"t":0,"node":1,"msg":1,"bytes_on_wire":400,"airtime_us":600}"#.to_vec(),
//! };
//! let mut ledger = EventLedger::new();
//! providers.on_event(&record);
//! ledger.ingest(&record);
//!
//! // At the end of the window: samples, invariants, a summary for the manifest.
//! let samples = providers.flush(1_000_000_000);
//! let report = v2xw_metrics::check_all(&ledger, &samples);
//! report.assert_all()?;
//! let summary = RunSummary::new(samples)?;
//! assert_eq!(summary.digest.len(), 64);
//! # Ok::<(), v2xw_metrics::MetricError>(())
//! ```
#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod arrow_out;
pub mod awareness;
pub mod bins;
pub mod cards;
pub mod channels;
pub mod comms;
pub mod def;
pub mod detection;
pub mod error;
pub mod frag;
pub mod gate;
pub mod invariants;
pub mod latency;
pub mod ledger;
pub mod load;
pub mod overhead;
pub mod provider;
pub mod quant;
pub mod runtime;
pub mod safety;
pub mod security;
pub mod stats;
pub mod summary;
pub mod vis;

pub use def::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
pub use detection::{Cell, ConfusionMatrix, DetectionLevel};
pub use error::{MetricError, Result};
pub use gate::{CalibrationIssue, GateFailure, GateReport, IssueRegister, IssueState};
pub use invariants::{InvariantOutcome, InvariantReport, InvariantViolation, check_all};
pub use ledger::EventLedger;
pub use provider::{Decoded, MetricProvider, ProviderSet};
pub use quant::Quantum;
pub use stats::{
    ConfidenceLevel, Distribution, DistributionSummary, Estimate, Interpolation, Percentile,
    Proportion, RatioEstimate,
};
pub use summary::{DigestSet, RunDiagnostics, RunSummary, metric_digest};

/// The providers 08-measurement-and-data.md's catalog is covered by, registered in one call.
///
/// In a fixed order: communication, latency, awareness, load, overhead, fragmentation, security,
/// detection, mobility and safety, runtime diagnostics. `t0` is the start of the first
/// window.
///
/// A run that wants a subset registers the providers itself; this is the "metrics: all"
/// of the scenario schema (03-interfaces.md §13).
///
/// # Errors
/// Whatever [`ProviderSet::register`] returns: an invalid definition, or a registry
/// refusal (duplicate id, API-version mismatch, licence gate).
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
    set: &mut ProviderSet,
    t0: v2xw_core::time::SimTime,
) -> Result<()> {
    set.register(registry, Box::new(comms::CommsProvider::new(t0)))?;
    set.register(registry, Box::new(latency::LatencyProvider::new()))?;
    set.register(registry, Box::new(awareness::AwarenessProvider::new(t0)))?;
    set.register(registry, Box::new(load::LoadProvider::new(t0)))?;
    set.register(registry, Box::new(overhead::OverheadProvider::new(t0)))?;
    set.register(registry, Box::new(frag::FragProvider::new()))?;
    set.register(registry, Box::new(security::SecurityProvider::new(t0)))?;
    set.register(registry, Box::new(detection::DetectionProvider::new()))?;
    set.register(registry, Box::new(safety::SafetyProvider::new(t0)))?;
    set.register(registry, Box::new(runtime::RuntimeProvider::new()))?;
    Ok(())
}
