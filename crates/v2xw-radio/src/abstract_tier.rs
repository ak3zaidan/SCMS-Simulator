//! The abstract tier: `phy/abstract/distance-load-table`, the two legacy models kept for
//! parity, and the calibration procedure of 04-models.md §4.9.
//!
//! # Why a table
//!
//! The abstract tier exists for 10,000-node runs, where there is no budget for a link
//! budget per pair per frame. It answers one question — was this frame received? — from a
//! two-dimensional table indexed by distance and local load, calibrated against the high
//! tier of the same RAT. Invariant I-R4 is that the calibration has actually been done:
//! a table that has not been accepted registers `uncalibrated` and the scenario validator
//! warns.
//!
//! # The calibration procedure, runnable today
//!
//! 04-models.md §4.9 states the procedure in six steps: worlds and densities, a
//! high-tier run, binning, registration, acceptance within 5 percentage points per bin,
//! and a scope check. Steps 1, 3, 4, 5 and 6 are implemented here as
//! [`CalibrationPlan`], [`CalibrationRun`], [`DistanceLoadTable`], [`AcceptanceReport`]
//! and [`TableEnvelope`], and they run today. Step 2 — "run the homogeneous high tier" —
//! needs the engine's event loop for the MAC contention part; what exists in this crate
//! is [`link_budget_samples`], which runs the *physical* half of that step end to end (a
//! synthetic drop, the propagation stack, the NIST error model, per-frame draws) so the
//! procedure is callable and testable now. When the engine lands, its high-tier run
//! replaces that one sample source and nothing else changes: the routine consumes
//! [`ReceptionSample`]s and does not care who produced them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::{LinkKey, NodeId};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain, RngRegistry};
use v2xw_core::time::{Duration, SimTime};
use v2xw_core::weather::WeatherState;
use v2xw_world::model::EnvClass;

use crate::error::{RadioError, Result};
use crate::fading::{NakagamiFading, NakagamiPreset};
use crate::numeric;
use crate::per::PerModel;
use crate::phy::{OfdmPhy, air_time};
use crate::prop::{LogDistancePreset, LogDistanceShadowing};
use crate::sweep::{ChannelModel, DsrcConfig, HighwaySweep, SweepReport, sweep_dsrc_with_samples};
use crate::traits::{Fading, Phy, Propagation};
use crate::types::{
    ActorClass, CcaState, ChannelId, FrameDescriptor, LosResult, LossCause, Mcs, RadioEndpoint,
    Rat, RxHandle, RxOutcome, TxHandle,
};

/// The distance bin width of the calibrated table, 25 m (04-models.md §4.9 step 3).
pub const DISTANCE_BIN_M: f64 = 25.0;

/// The default upper distance the table covers, 1,000 m (step 3).
pub const RANGE_MAX_M: f64 = 1_000.0;

/// The load bin width when the load axis is CBR, 0.1 (step 3).
pub const CBR_BIN: f64 = 0.1;

/// The load bin width when the load axis is heard transmitters, 10 (step 3).
pub const HEARD_BIN: f64 = 10.0;

/// The default number of seeds per calibration point, 10 (step 2, "registry default").
pub const DEFAULT_SEEDS: u32 = 10;

/// The per-bin PDR tolerance of the acceptance test, 5 percentage points
/// (step 5, and invariant I-R4).
pub const PDR_TOLERANCE: f64 = 0.05;

/// The mean-CBR tolerance of the acceptance test, 0.05 (step 5).
pub const CBR_TOLERANCE: f64 = 0.05;

/// Which quantity the table's load axis is binned on (04-models.md §4.9 step 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadAxis {
    /// The measured channel busy ratio, in steps of 0.1. The default.
    #[default]
    Cbr,
    /// The number of distinct transmitters heard in the last `T_CBR`, in steps of 10.
    HeardTransmitters,
}

impl LoadAxis {
    /// The bin width of this axis.
    #[must_use]
    pub const fn bin_width(self) -> f64 {
        match self {
            LoadAxis::Cbr => CBR_BIN,
            LoadAxis::HeardTransmitters => HEARD_BIN,
        }
    }

    /// How many bins the axis has.
    #[must_use]
    pub const fn bins(self) -> usize {
        match self {
            // CBR in [0, 1] in steps of 0.1.
            LoadAxis::Cbr => 10,
            // Up to 200 heard transmitters, which is past any density in the plan.
            LoadAxis::HeardTransmitters => 20,
        }
    }

    /// The bin a load value falls in.
    #[must_use]
    pub fn bin_of(self, load: f64) -> usize {
        let raw = (load.max(0.0) / self.bin_width()) as usize;
        raw.min(self.bins() - 1)
    }
}

/// One observation the calibration consumes: was this frame received, at what distance and
/// under what local load?
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReceptionSample {
    /// Transmitter-receiver distance, metres.
    pub distance_m: f64,
    /// The receiver's local load on the axis the table is binned on.
    pub load: f64,
    /// Whether the frame was decoded.
    pub received: bool,
}

/// The scope a table is valid for (04-models.md §4.9 step 6).
///
/// The validator rejects use outside it — "for example a 1,000 B CPM load on a 300 B
/// table" — which is why the envelope is part of the table rather than a note beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableEnvelope {
    /// The RAT the table was built for.
    pub rat: Rat,
    /// The MCS.
    pub mcs: Mcs,
    /// The packet-size pattern, bytes, in the order the generator emits it.
    pub packet_bytes: Vec<u32>,
    /// The generation rate, Hz.
    pub rate_hz: f64,
    /// The transmit power, dBm.
    pub tx_power_dbm: f64,
    /// The id of the propagation stack the high-tier run used.
    pub propagation_stack: String,
    /// The world content hashes the run used, hex, in order.
    pub world_hashes: Vec<String>,
    /// The vehicle densities, vehicles per km.
    pub densities_veh_km: Vec<f64>,
    /// The seeds.
    pub seeds: Vec<u64>,
    /// The engine build the run was made with, as the caller supplies it.
    pub engine_build: String,
    /// The load axis.
    pub load_axis: LoadAxis,
}

impl TableEnvelope {
    /// Whether a frame of `bytes` at `mcs` is inside the envelope.
    #[must_use]
    pub fn admits(&self, bytes: u32, mcs: Mcs) -> bool {
        mcs == self.mcs && self.packet_bytes.contains(&bytes)
    }
}

/// One cell of the calibrated table: the pooled reception ratio and its Wilson interval.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Cell {
    /// Frames observed.
    pub trials: u64,
    /// Frames received.
    pub successes: u64,
    /// The pooled reception ratio.
    pub p: f64,
    /// Lower end of the 95 % Wilson interval.
    pub lower: f64,
    /// Upper end of the 95 % Wilson interval.
    pub upper: f64,
}

impl Cell {
    /// The 95 % Wilson score interval for `successes` out of `trials`.
    ///
    /// Wilson rather than the normal approximation because the cells at the edge of the
    /// range have `p` near zero or one, where the normal interval leaves the unit
    /// interval and stops meaning anything.
    #[must_use]
    pub fn wilson(successes: u64, trials: u64) -> Self {
        if trials == 0 {
            return Self::default();
        }
        let z = 1.959_963_984_540_054_f64; // the 97.5th percentile of the standard normal
        let n = trials as f64;
        let p = successes as f64 / n;
        let z2 = z * z;
        let denominator = 1.0 + z2 / n;
        let centre = (p + z2 / (2.0 * n)) / denominator;
        let spread = (z / denominator) * math::sqrt(p * (1.0 - p) / n + z2 / (4.0 * n * n));
        Self {
            trials,
            successes,
            p,
            lower: (centre - spread).clamp(0.0, 1.0),
            upper: (centre + spread).clamp(0.0, 1.0),
        }
    }
}

/// The calibrated reception table (04-models.md §4.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistanceLoadTable {
    /// The scope this table is valid for.
    pub envelope: TableEnvelope,
    /// Distance bin width, metres.
    pub distance_bin_m: f64,
    /// The largest distance the table covers, metres.
    pub range_max_m: f64,
    /// `cells[distance_bin][load_bin]`.
    pub cells: Vec<Vec<Cell>>,
    /// Whether the acceptance test of step 5 passed. A table that has not passed
    /// registers `uncalibrated` (invariant I-R4).
    pub accepted: bool,
}

impl DistanceLoadTable {
    /// How many distance bins a range needs.
    #[must_use]
    pub fn distance_bins(range_max_m: f64, bin_m: f64) -> usize {
        ((range_max_m / bin_m).ceil() as usize).max(1)
    }

    /// An empty table for an envelope.
    #[must_use]
    pub fn empty(envelope: TableEnvelope) -> Self {
        let bins = Self::distance_bins(RANGE_MAX_M, DISTANCE_BIN_M);
        let load_bins = envelope.load_axis.bins();
        Self {
            envelope,
            distance_bin_m: DISTANCE_BIN_M,
            range_max_m: RANGE_MAX_M,
            cells: vec![vec![Cell::default(); load_bins]; bins],
            accepted: false,
        }
    }

    /// The distance bin a distance falls in, or `None` beyond the table's range.
    #[must_use]
    pub fn distance_bin_of(&self, d_m: f64) -> Option<usize> {
        if d_m < 0.0 || d_m >= self.range_max_m {
            return None;
        }
        Some(((d_m / self.distance_bin_m) as usize).min(self.cells.len() - 1))
    }

    /// The reception probability at a distance and load, interpolated linearly in both
    /// (04-models.md §4.9 step 4).
    ///
    /// Beyond the table's range the probability is zero: the table's range *is* the
    /// model's range, which is what makes [`LossCause::OutOfRange`] meaningful at this
    /// tier.
    #[must_use]
    pub fn probability(&self, d_m: f64, load: f64) -> f64 {
        if d_m >= self.range_max_m {
            return 0.0;
        }
        let axis = self.envelope.load_axis;
        let bins = self.cells.len();
        let load_bins = axis.bins();
        // Bin centres, so interpolation is symmetric about the value a bin represents.
        let x = (d_m.max(0.0) / self.distance_bin_m - 0.5).clamp(0.0, (bins - 1) as f64);
        let y = (load.max(0.0) / axis.bin_width() - 0.5).clamp(0.0, (load_bins - 1) as f64);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(bins - 1), (y0 + 1).min(load_bins - 1));
        let (fx, fy) = (x - x0 as f64, y - y0 as f64);
        let at = |i: usize, j: usize| self.cells[i][j].p;
        let top = at(x0, y0) * (1.0 - fx) + at(x1, y0) * fx;
        let bottom = at(x0, y1) * (1.0 - fx) + at(x1, y1) * fx;
        (top * (1.0 - fy) + bottom * fy).clamp(0.0, 1.0)
    }

    /// The table's content hash, for the `@<hash>` suffix step 4 registers it under.
    #[must_use]
    pub fn content_hash_hex(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        v2xw_core::hash::sha256_hex(&json)
    }

    /// The quantised form a recorder or exporter writes: every probability on the
    /// `1e-6` grid (build decision D9).
    #[must_use]
    pub fn quantized(&self) -> Self {
        let mut out = self.clone();
        for row in &mut out.cells {
            for cell in row {
                cell.p = numeric::q_ratio(cell.p);
                cell.lower = numeric::q_ratio(cell.lower);
                cell.upper = numeric::q_ratio(cell.upper);
            }
        }
        out
    }
}

/// The calibration plan of 04-models.md §4.9 step 1: which worlds, densities and loads
/// the high-tier run covers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationPlan {
    /// Speeds to drop vehicles at, km/h: the TR 36.885 urban-grid and freeway points.
    pub speeds_kmh: Vec<f64>,
    /// The Todisco highway densities, vehicles per km.
    pub densities_veh_km: Vec<f64>,
    /// The Bazzi urban neighbour densities, vehicles per 100 m, with their spreads.
    pub urban_neighbours_per_100m: Vec<(f64, f64)>,
    /// Message generation rate, Hz.
    pub rate_hz: f64,
    /// The packet-size pattern, bytes.
    pub packet_bytes: Vec<u32>,
    /// The MCS.
    pub mcs: Mcs,
    /// Transmit power, dBm.
    pub tx_power_dbm: f64,
    /// Seeds per point.
    pub seeds: Vec<u64>,
    /// The load axis to bin on.
    pub load_axis: LoadAxis,
    /// The largest distance to cover, metres.
    pub range_max_m: f64,
    /// The headway used by the TR 36.885 drops, seconds.
    pub headway_s: f64,
}

impl CalibrationPlan {
    /// The plan exactly as 04-models.md §4.9 step 1 states it.
    ///
    /// Speeds 15 and 60 km/h (urban grid) and 70 and 140 km/h (freeway) with a 2.5 s
    /// headway [TR 36.885]; the Todisco densities 50, 100 and 200 veh/km on a 2 km
    /// highway; the Bazzi Cologne and Bologna neighbour densities 14.8 ± 8.8 and
    /// 25.4 ± 25.4 per 100 m as the urban points; 10 Hz, 300 B (or the TR 37.885 pattern
    /// {300, 190, 190, 190, 190} B); MCS 6 Mbit/s; power 23 dBm; ten seeds.
    #[must_use]
    pub fn document_default() -> Self {
        Self {
            speeds_kmh: vec![15.0, 60.0, 70.0, 140.0],
            densities_veh_km: vec![50.0, 100.0, 200.0],
            urban_neighbours_per_100m: vec![(14.8, 8.8), (25.4, 25.4)],
            rate_hz: 10.0,
            packet_bytes: vec![300, 190, 190, 190, 190],
            mcs: Mcs::R6Qpsk12,
            tx_power_dbm: 23.0,
            seeds: (0..u64::from(DEFAULT_SEEDS)).collect(),
            load_axis: LoadAxis::Cbr,
            range_max_m: RANGE_MAX_M,
            headway_s: 2.5,
        }
    }

    /// The envelope a table built to this plan carries.
    #[must_use]
    pub fn envelope(
        &self,
        propagation_stack: impl Into<String>,
        world_hashes: Vec<String>,
        engine_build: impl Into<String>,
    ) -> TableEnvelope {
        TableEnvelope {
            rat: Rat::Dsrc80211p,
            mcs: self.mcs,
            packet_bytes: self.packet_bytes.clone(),
            rate_hz: self.rate_hz,
            tx_power_dbm: self.tx_power_dbm,
            propagation_stack: propagation_stack.into(),
            world_hashes,
            densities_veh_km: self.densities_veh_km.clone(),
            seeds: self.seeds.clone(),
            engine_build: engine_build.into(),
            load_axis: self.load_axis,
        }
    }
}

impl Default for CalibrationPlan {
    fn default() -> Self {
        Self::document_default()
    }
}

/// The accumulator of 04-models.md §4.9 steps 2 and 3: it eats samples and produces the
/// table.
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationRun {
    plan: CalibrationPlan,
    envelope: TableEnvelope,
    /// `(distance bin, load bin) -> (trials, successes)`, a `BTreeMap` so that a dump is
    /// ordered.
    counts: BTreeMap<(usize, usize), (u64, u64)>,
    cbr_samples: Vec<f64>,
}

impl CalibrationRun {
    /// A run for a plan and an envelope.
    #[must_use]
    pub fn new(plan: CalibrationPlan, envelope: TableEnvelope) -> Self {
        Self {
            plan,
            envelope,
            counts: BTreeMap::new(),
            cbr_samples: Vec::new(),
        }
    }

    /// Records one observation (step 2).
    pub fn record(&mut self, sample: ReceptionSample) {
        let bins = DistanceLoadTable::distance_bins(self.plan.range_max_m, DISTANCE_BIN_M);
        if sample.distance_m < 0.0 || sample.distance_m >= self.plan.range_max_m {
            return;
        }
        let d_bin = ((sample.distance_m / DISTANCE_BIN_M) as usize).min(bins - 1);
        let l_bin = self.envelope.load_axis.bin_of(sample.load);
        let entry = self.counts.entry((d_bin, l_bin)).or_insert((0, 0));
        entry.0 += 1;
        if sample.received {
            entry.1 += 1;
        }
        if self.envelope.load_axis == LoadAxis::Cbr {
            self.cbr_samples.push(sample.load);
        }
    }

    /// Records every observation of an iterator.
    pub fn record_all(&mut self, samples: impl IntoIterator<Item = ReceptionSample>) {
        for s in samples {
            self.record(s);
        }
    }

    /// The mean CBR of the recorded samples, for the acceptance test's second half.
    #[must_use]
    pub fn mean_cbr(&self) -> f64 {
        if self.cbr_samples.is_empty() {
            return 0.0;
        }
        math::sum_ordered(self.cbr_samples.iter().copied()) / self.cbr_samples.len() as f64
    }

    /// Builds the table (step 3): the pooled reception ratio per cell with its Wilson
    /// interval.
    #[must_use]
    pub fn finish(&self) -> DistanceLoadTable {
        let mut table = DistanceLoadTable::empty(self.envelope.clone());
        table.range_max_m = self.plan.range_max_m;
        let bins = DistanceLoadTable::distance_bins(self.plan.range_max_m, DISTANCE_BIN_M);
        table.cells = vec![vec![Cell::default(); self.envelope.load_axis.bins()]; bins];
        for (&(d_bin, l_bin), &(trials, successes)) in &self.counts {
            table.cells[d_bin][l_bin] = Cell::wilson(successes, trials);
        }
        table
    }
}

/// One bin's comparison in the acceptance test (04-models.md §4.9 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BinComparison {
    /// The distance bin.
    pub distance_bin: usize,
    /// The load bin.
    pub load_bin: usize,
    /// The abstract tier's PDR in that bin.
    pub abstract_pdr: f64,
    /// The high tier's PDR in that bin.
    pub high_pdr: f64,
    /// Whether the difference is inside [`PDR_TOLERANCE`].
    pub within_tolerance: bool,
}

/// The acceptance report of step 5, and invariant I-R4's evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceReport {
    /// Every bin that was compared.
    pub bins: Vec<BinComparison>,
    /// The abstract tier's mean CBR estimate.
    pub abstract_mean_cbr: f64,
    /// The high tier's mean CBR.
    pub high_mean_cbr: f64,
    /// Whether every bin and the mean CBR are inside tolerance.
    pub accepted: bool,
    /// The worst absolute PDR difference seen, in percentage points.
    pub worst_pdr_gap: f64,
}

impl AcceptanceReport {
    /// Compares an abstract-tier table against the high-tier table it was built from.
    ///
    /// Only cells the high-tier run actually observed are compared: a bin with no trials
    /// says nothing about either tier, and counting it as a failure would make the test
    /// depend on how far the drop happened to reach.
    #[must_use]
    pub fn compare(
        abstract_table: &DistanceLoadTable,
        high_table: &DistanceLoadTable,
        abstract_mean_cbr: f64,
        high_mean_cbr: f64,
    ) -> Self {
        let mut bins = Vec::new();
        let mut worst = 0.0_f64;
        for (d, row) in high_table.cells.iter().enumerate() {
            for (l, high_cell) in row.iter().enumerate() {
                if high_cell.trials == 0 {
                    continue;
                }
                let abstract_cell = abstract_table
                    .cells
                    .get(d)
                    .and_then(|r| r.get(l))
                    .copied()
                    .unwrap_or_default();
                let gap = (abstract_cell.p - high_cell.p).abs();
                worst = worst.max(gap);
                bins.push(BinComparison {
                    distance_bin: d,
                    load_bin: l,
                    abstract_pdr: abstract_cell.p,
                    high_pdr: high_cell.p,
                    within_tolerance: gap <= PDR_TOLERANCE,
                });
            }
        }
        let cbr_ok = (abstract_mean_cbr - high_mean_cbr).abs() <= CBR_TOLERANCE;
        let accepted = cbr_ok && bins.iter().all(|b| b.within_tolerance);
        Self {
            bins,
            abstract_mean_cbr,
            high_mean_cbr,
            accepted,
            worst_pdr_gap: worst,
        }
    }
}

/// The calibration routine of 04-models.md §4.9, end to end over a supplied sample
/// stream.
///
/// Steps 3 to 5: bin the samples, build the table with its Wilson intervals, re-run the
/// abstract tier against the same sample distribution, and compare. The returned table
/// carries `accepted`, which is what decides between registering it calibrated and
/// registering it `uncalibrated` (invariant I-R4).
#[must_use]
pub fn calibrate(
    plan: &CalibrationPlan,
    envelope: TableEnvelope,
    samples: impl IntoIterator<Item = ReceptionSample>,
    seed: u64,
) -> (DistanceLoadTable, AcceptanceReport) {
    let mut high_run = CalibrationRun::new(plan.clone(), envelope.clone());
    let collected: Vec<ReceptionSample> = samples.into_iter().collect();
    high_run.record_all(collected.iter().copied());
    let high_table = high_run.finish();

    // Step 5: re-run the abstract tier on the same scenario. Without an engine that means
    // replaying the same (distance, load) points through the table's own draw, which is
    // exactly what the abstract tier would do at those points.
    let rng = RngRegistry::new(seed);
    let mut abstract_run = CalibrationRun::new(plan.clone(), envelope.clone());
    for (i, s) in collected.iter().enumerate() {
        let p = high_table.probability(s.distance_m, s.load);
        let received = rng
            .checkout(
                RngDomain::AbstractRx,
                EntityRef::LinkFrame {
                    link: LinkKey(NodeId::new(0), NodeId::new(1)),
                    frame: i as u64,
                },
            )
            .bool(p);
        abstract_run.record(ReceptionSample { received, ..*s });
    }
    let abstract_table = abstract_run.finish();
    let report = AcceptanceReport::compare(
        &abstract_table,
        &high_table,
        abstract_run.mean_cbr(),
        high_run.mean_cbr(),
    );
    let mut table = high_table;
    table.accepted = report.accepted;
    (table, report)
}

// =========================================================================================
// Wiring: a scenario asks for a calibrated abstract tier (04-models.md §4.9)
// =========================================================================================

/// The fit a calibrated abstract tier carries: what the acceptance test of 04-models.md
/// §4.9 step 5 found, in the form a model card records.
///
/// This is the difference between the abstract tier being *calibrated* and being *guessed*.
/// A table built from a guess has no fit; a table built by [`CalibrationRequest::run`] has
/// one, it names the high-tier runs it was fitted to, and it is what moves the card's
/// [`Validation::status`] off [`ValidationStatus::Unvalidated`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CalibrationFit {
    /// Whether every compared bin and the mean CBR are inside tolerance (step 5).
    pub accepted: bool,
    /// The worst per-bin delivery gap, in percentage points.
    pub worst_pdr_gap_pp: f64,
    /// The abstract tier's mean CBR estimate.
    pub abstract_mean_cbr: f64,
    /// The high tier's mean CBR.
    pub high_mean_cbr: f64,
    /// `|abstract − high|` for the mean CBR, against the 0.05 of step 5.
    pub mean_cbr_gap: f64,
    /// How many `(distance, load)` cells the high-tier run actually observed, and so how
    /// many were compared.
    pub bins_compared: usize,
    /// How many of them missed the tolerance.
    pub bins_failed: usize,
    /// How many reception samples the fit consumed.
    pub samples: u64,
    /// The table's content hash — the `@<hash>` suffix step 4 registers it under.
    pub table_hash: String,
    /// The high-tier runs it was fitted to, by their [`crate::sweep::SweepReport::label`].
    pub high_tier_runs: Vec<String>,
    /// The propagation stack those runs used, which the envelope also pins.
    pub propagation_stack: String,
    /// The engine build, as the caller supplied it.
    pub engine_build: String,
}

impl CalibrationFit {
    /// The fit an acceptance report and the run that produced it imply.
    #[must_use]
    pub fn from_report(
        report: &AcceptanceReport,
        table: &DistanceLoadTable,
        samples: u64,
        high_tier_runs: Vec<String>,
        engine_build: impl Into<String>,
    ) -> Self {
        Self {
            accepted: report.accepted,
            worst_pdr_gap_pp: numeric::q_ratio(report.worst_pdr_gap * 100.0),
            abstract_mean_cbr: numeric::q_ratio(report.abstract_mean_cbr),
            high_mean_cbr: numeric::q_ratio(report.high_mean_cbr),
            mean_cbr_gap: numeric::q_ratio(
                (report.abstract_mean_cbr - report.high_mean_cbr).abs(),
            ),
            bins_compared: report.bins.len(),
            bins_failed: report.bins.iter().filter(|b| !b.within_tolerance).count(),
            samples,
            table_hash: table.content_hash_hex(),
            high_tier_runs,
            propagation_stack: table.envelope.propagation_stack.clone(),
            engine_build: engine_build.into(),
        }
    }

    /// The one-line summary the card and the manifest carry.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "fitted to {} high-tier run(s) over {} samples with {}: worst per-bin \
             delivery gap {:.3} pp over {} compared cell(s) ({} outside tolerance), mean \
             CBR gap {:.4} — {} the 04-models.md §4.9 step 5 tolerance of {:.0} pp and \
             {:.2} CBR",
            self.high_tier_runs.len(),
            self.samples,
            self.propagation_stack,
            self.worst_pdr_gap_pp,
            self.bins_compared,
            self.bins_failed,
            self.mean_cbr_gap,
            if self.accepted { "within" } else { "OUTSIDE" },
            PDR_TOLERANCE * 100.0,
            CBR_TOLERANCE,
        )
    }
}

/// A calibrated abstract tier: the table, the acceptance report, the fit, and the
/// high-tier runs it came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibratedAbstractTier {
    /// The table. `accepted` on it is step 5's verdict.
    pub table: DistanceLoadTable,
    /// The bin-by-bin comparison.
    pub report: AcceptanceReport,
    /// The fit, for the card and the manifest.
    pub fit: CalibrationFit,
    /// The high-tier runs, so a reviewer can see the curves the fit came from.
    pub high_tier_reports: Vec<SweepReport>,
}

impl CalibratedAbstractTier {
    /// The PHY, with the fit recorded on its card.
    #[must_use]
    pub fn into_phy(self) -> AbstractPhy {
        AbstractPhy::calibrated(self.table, self.fit)
    }

    /// Whether the fit passed, which is what decides between a calibrated registration and
    /// an `uncalibrated` one (invariant I-R4).
    #[must_use]
    pub const fn accepted(&self) -> bool {
        self.fit.accepted
    }
}

/// What a scenario asks for when it asks for a **calibrated** abstract tier rather than a
/// guessed one (04-models.md §4.9).
///
/// This is the wiring the scope asked for. A scenario names the abstract tier and this
/// request; the engine runs the homogeneous high tier at each `(density, seed)` point,
/// records every `(distance, load, decoded)` triple, bins them, fits the table, runs the
/// acceptance test, and registers the result — calibrated if it passed, `uncalibrated`
/// with a validator warning if it did not. Nothing downstream of
/// [`CalibratedAbstractTier::into_phy`] can tell the difference between a fitted table and
/// a hand-written one except by reading the card, which is the point: the tier is swapped
/// by changing one field.
///
/// # What the high-tier run is, here
///
/// [`crate::sweep::sweep_dsrc_with_samples`] — the same harness the §13 validation curves
/// are measured with, driving the real [`crate::phy::OfdmPhy`] at [`Tier::High`] with the
/// real error model, real shadowing and sequential CSMA contention. Its approximations are
/// its own card's business and are stated on it (see [`crate::sweep`]); what matters here
/// is that the numbers come out of a run rather than out of a guess, and that the run is
/// named in the fit so a reader can re-do it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CalibrationRequest {
    /// The plan of step 1: loads, sizes, MCS, power, seeds.
    pub plan: CalibrationPlan,
    /// The channel the high-tier runs use. Pinned into the table's envelope, because step
    /// 6 makes a table valid only for the propagation stack it was built with.
    pub channel: ChannelModel,
    /// The ring circumference, metres. At least twice [`CalibrationPlan::range_max_m`], or
    /// a distance bin wraps onto itself.
    pub ring_m: f64,
    /// How long each high-tier run counts for.
    pub duration: Duration,
    /// How long each run warms up before counting.
    pub warmup: Duration,
    /// The engine build, recorded in the envelope and the fit.
    pub engine_build: String,
    /// The world content hashes, recorded in the envelope.
    pub world_hashes: Vec<String>,
}

impl CalibrationRequest {
    /// The request 04-models.md §4.9 step 1 states, on the `abbas-los-highway` channel —
    /// the only measurement-fitted, fully verified highway LOS model the workspace ships
    /// (04-models.md §3.2).
    ///
    /// Short runs: a second of warm-up and two seconds of counting per `(density, seed)`
    /// point. At 10 Hz that is twenty frames per vehicle per run, which at the plan's
    /// densities is millions of evaluated arrivals across the grid — enough for the Wilson
    /// intervals of step 3 to be narrow in the bins that matter, and short enough that the
    /// whole fit is a nightly job rather than a weekend one. The span is a parameter, so a
    /// campaign that wants the published spans sets them.
    #[must_use]
    pub fn document_default(engine_build: impl Into<String>) -> Self {
        Self {
            plan: CalibrationPlan::document_default(),
            channel: ChannelModel::AbbasLosHighway,
            ring_m: 2_000.0,
            duration: Duration::from_secs(2),
            warmup: Duration::from_secs(1),
            engine_build: engine_build.into(),
            world_hashes: Vec::new(),
        }
    }

    /// The `DsrcConfig` the high-tier runs use: the plan's MCS with the harness defaults.
    #[must_use]
    pub fn dsrc_config(&self) -> DsrcConfig {
        DsrcConfig {
            mcs: self.plan.mcs,
            ..DsrcConfig::default()
        }
    }

    /// The envelope a table fitted to this request carries (step 6).
    #[must_use]
    pub fn envelope(&self) -> TableEnvelope {
        self.plan.envelope(
            self.channel.label(),
            self.world_hashes.clone(),
            self.engine_build.clone(),
        )
    }

    /// Every high-tier run this request implies: one per `(density, seed)` point of step 1,
    /// in a fixed order.
    ///
    /// The seed of a point is `plan.seeds[i]` mixed with the density's index, so two
    /// densities do not replay the same vehicle placement, and the order is
    /// density-major so the list is a pure function of the request.
    #[must_use]
    pub fn sweeps(&self) -> Vec<HighwaySweep> {
        let mut out = Vec::with_capacity(self.plan.densities_veh_km.len() * self.plan.seeds.len());
        for (d_index, density) in self.plan.densities_veh_km.iter().enumerate() {
            for seed in &self.plan.seeds {
                let mut sweep = HighwaySweep::highway_slow(self.plan.rate_hz.max(1.0) as u32)
                    .with_channel(self.channel)
                    .with_density(*density)
                    .with_duration(self.warmup, self.duration)
                    // A seed per (density, seed) point. The mix is a multiply-add on the
                    // density index, not a hash, so the list is reproducible by hand.
                    .with_seed(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(d_index as u64));
                sweep.ring_m = self.ring_m;
                sweep.payload_cycle = self.plan.packet_bytes.clone();
                sweep.tx_power_dbm = self.plan.tx_power_dbm;
                sweep.bin_m = DISTANCE_BIN_M;
                sweep.max_distance_m = self.plan.range_max_m;
                sweep.sensing_range_m = self.plan.range_max_m;
                out.push(sweep);
            }
        }
        out
    }

    /// Runs the procedure end to end and returns the calibrated tier.
    ///
    /// Steps 2 to 5 of 04-models.md §4.9: run the homogeneous high tier at every point,
    /// pool every `(distance, load, decoded)` triple, bin them, fit, and compare the
    /// abstract tier's re-run against the high tier per bin.
    ///
    /// The comparison's own draw is seeded from the plan's first seed, so a fit is a pure
    /// function of the request.
    ///
    /// # Panics
    ///
    /// As [`crate::sweep::sweep_dsrc_with_samples`]: if a frame exceeds the MSDU cap,
    /// which is a configuration error in the plan's packet sizes.
    #[must_use]
    pub fn run(&self) -> CalibratedAbstractTier {
        let cfg = self.dsrc_config();
        let mut samples: Vec<ReceptionSample> = Vec::new();
        let mut reports: Vec<SweepReport> = Vec::new();
        for sweep in self.sweeps() {
            let (report, mut got) = sweep_dsrc_with_samples(&sweep, cfg);
            samples.append(&mut got);
            reports.push(report);
        }
        let n = samples.len() as u64;
        let seed = self.plan.seeds.first().copied().unwrap_or(0);
        let (table, report) = calibrate(&self.plan, self.envelope(), samples, seed);
        let labels = reports.iter().map(|r| r.label.clone()).collect();
        let fit = CalibrationFit::from_report(&report, &table, n, labels, self.engine_build.clone());
        CalibratedAbstractTier {
            table,
            report,
            fit,
            high_tier_reports: reports,
        }
    }
}

/// The physical half of step 2, runnable today: a synthetic homogeneous drop evaluated
/// through the real propagation, fading and error models.
///
/// `n_vehicles` are placed on a straight road at the spacing `density_veh_km` implies,
/// each sends `frames_per_vehicle` frames of the plan's size pattern, and every
/// transmitter-receiver pair inside `range_max_m` is evaluated with the medium or high
/// propagation stack and the NIST error model. What it does **not** model is MAC
/// contention: there is no channel access here, so the load axis is supplied by the
/// caller rather than measured, and the samples describe the link, not the network. That
/// is why this is the sample source for the procedure's own tests and not the calibration
/// itself.
pub fn link_budget_samples<C: Ctx + ?Sized>(
    ctx: &mut C,
    plan: &CalibrationPlan,
    density_veh_km: f64,
    n_vehicles: u32,
    frames_per_vehicle: u32,
    load: f64,
    env: EnvClass,
) -> Vec<ReceptionSample> {
    let spacing_m = if density_veh_km > 0.0 {
        1_000.0 / density_veh_km
    } else {
        25.0
    };
    let mut propagation = LogDistanceShadowing::new(
        Tier::High,
        LogDistancePreset::for_environment(env, false),
        env,
    );
    let mut fading = NakagamiFading::new(NakagamiPreset::for_environment(env));
    let per = PerModel::default();
    let noise = crate::phy::THERMAL_NOISE_10MHZ_DBM + crate::phy::NOISE_FIGURE_HARDWARE_DB;
    let mut samples = Vec::new();
    for tx_index in 0..n_vehicles {
        let tx = RadioEndpoint::isotropic(
            NodeId::new(tx_index),
            v2xw_core::geom::Vec3::new(f64::from(tx_index) * spacing_m, 0.0, 1.5),
            ActorClass::Car,
            ctx.now(),
        );
        for rx_index in 0..n_vehicles {
            if rx_index == tx_index {
                continue;
            }
            let rx = RadioEndpoint::isotropic(
                NodeId::new(rx_index),
                v2xw_core::geom::Vec3::new(f64::from(rx_index) * spacing_m, 0.0, 1.5),
                ActorClass::Car,
                ctx.now(),
            );
            let d = tx.pos.distance(rx.pos);
            if d >= plan.range_max_m {
                continue;
            }
            for frame in 0..frames_per_vehicle {
                let bytes = plan.packet_bytes[(frame as usize) % plan.packet_bytes.len().max(1)];
                let breakdown = propagation.loss_db(
                    ctx,
                    &tx,
                    &rx,
                    ChannelId::CCH.centre_hz(),
                    &LosResult::clear(),
                    &WeatherState::CLEAR,
                );
                let link = LinkKey(tx.node, rx.node);
                let t = ctx.now() + u64::from(frame) * 100_000_000;
                let fade = fading.sample_db(ctx, link, d, t);
                let rx_dbm = breakdown.rx_power_dbm(plan.tx_power_dbm) + fade;
                let sensitivity = plan.mcs.sensitivity_static_dbm();
                let received = if rx_dbm < sensitivity {
                    false
                } else {
                    let sinr = rx_dbm - noise;
                    let p_err = per.per(bytes, plan.mcs, sinr);
                    !ctx.rng(
                        RngDomain::plugin(OfdmPhy::ID),
                        EntityRef::LinkFrame { link, frame: t },
                    )
                    .bool(p_err)
                };
                samples.push(ReceptionSample {
                    distance_m: d,
                    load,
                    received,
                });
            }
        }
    }
    samples
}

// =========================================================================================
// `phy/abstract/distance-load-table`
// =========================================================================================

/// `phy/abstract/distance-load-table` — the abstract tier's PHY.
#[derive(Debug)]
pub struct AbstractPhy {
    card: ModelCard,
    table: DistanceLoadTable,
    /// The fit, when this table came from a high-tier run rather than from a guess
    /// (04-models.md §4.9). `None` for a hand-written table, which is what
    /// [`AbstractPhy::new`] builds.
    fit: Option<CalibrationFit>,
    /// The load at each receiver, as the engine's own estimate supplies it.
    loads: BTreeMap<u32, f64>,
    /// Registered arrivals: distance and frame, by `(receiver, transmission id)`.
    arrivals: BTreeMap<(u32, u64), AbstractArrival>,
    next_tx_id: u64,
}

/// One arrival at the abstract tier: no power, only a distance.
#[derive(Debug, Clone, Copy, PartialEq)]
struct AbstractArrival {
    tx: NodeId,
    rx: NodeId,
    distance_m: f64,
    frame: FrameDescriptor,
    start: SimTime,
    end: SimTime,
}

impl AbstractPhy {
    /// The model's id, without the table hash step 4 appends.
    pub const ID: &'static str = "phy/abstract/distance-load-table";

    /// The PHY over a table, with no fit recorded.
    ///
    /// The table's `accepted` flag still decides between a unit-tested registration and an
    /// `uncalibrated` one (invariant I-R4), but nothing on the card says *what* the table
    /// was fitted to, because nothing told it. A scenario that asks for a calibrated
    /// abstract tier goes through [`CalibrationRequest::run`] and
    /// [`CalibratedAbstractTier::into_phy`], which reaches
    /// [`AbstractPhy::calibrated`] instead.
    #[must_use]
    pub fn new(table: DistanceLoadTable) -> Self {
        Self {
            card: abstract_card(&table),
            table,
            fit: None,
            loads: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            next_tx_id: 1,
        }
    }

    /// The PHY over a table fitted to a high-tier run, with the fit recorded in the
    /// card's validation section (04-models.md §4.9 step 4).
    ///
    /// This is the constructor that makes the difference between a calibrated tier and a
    /// guessed one visible: the card's `validation.status` becomes
    /// [`ValidationStatus::LiteratureChecked`] when the fit passed — 04-models.md §13's
    /// "Abstract radio (`abstract` versus `high`)" row *is* the acceptance test of §4.9
    /// step 5, so a table that passed it has been compared against the reference the row
    /// names — and stays [`ValidationStatus::Unvalidated`] when it did not, with the fit's
    /// own summary saying which bins missed.
    #[must_use]
    pub fn calibrated(table: DistanceLoadTable, fit: CalibrationFit) -> Self {
        Self {
            card: abstract_card_with_fit(&table, Some(&fit)),
            table,
            fit: Some(fit),
            loads: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            next_tx_id: 1,
        }
    }

    /// The fit, when there is one.
    #[must_use]
    pub fn fit(&self) -> Option<&CalibrationFit> {
        self.fit.as_ref()
    }

    /// The id the registry stores this instance under: `id@<table hash>` (step 4).
    #[must_use]
    pub fn id_with_hash(&self) -> String {
        format!("{}@{}", Self::ID, &self.table.content_hash_hex()[..16])
    }

    /// The table.
    #[must_use]
    pub const fn table(&self) -> &DistanceLoadTable {
        &self.table
    }

    /// Sets the local load estimate at a receiver, which the engine measures or estimates
    /// per 04-models.md §4.5.
    pub fn set_load(&mut self, node: NodeId, load: f64) {
        self.loads.insert(node.index(), load.max(0.0));
    }

    /// The load in force at a receiver.
    #[must_use]
    pub fn load(&self, node: NodeId) -> f64 {
        self.loads.get(&node.index()).copied().unwrap_or(0.0)
    }

    /// Registers an arrival: at this tier, a distance and a frame.
    ///
    /// # Errors
    ///
    /// [`RadioError::OutsideEnvelope`] when the frame is outside the table's calibrated
    /// scope (step 6).
    pub fn register_arrival(
        &mut self,
        tx: NodeId,
        rx: NodeId,
        distance_m: f64,
        frame: &FrameDescriptor,
        start: SimTime,
        tx_id: u64,
    ) -> Result<RxHandle> {
        if !self.table.envelope.admits(frame.bytes, frame.mcs) {
            return Err(RadioError::OutsideEnvelope {
                what: "frame",
                detail: format!(
                    "{} B at {} is outside the table's envelope ({:?} B at {})",
                    frame.bytes,
                    frame.mcs,
                    self.table.envelope.packet_bytes,
                    self.table.envelope.mcs
                ),
            });
        }
        let end = air_time(frame.bytes, frame.mcs).after(start);
        self.arrivals.insert(
            (rx.index(), tx_id),
            AbstractArrival {
                tx,
                rx,
                distance_m,
                frame: *frame,
                start,
                end,
            },
        );
        Ok(RxHandle { tx: tx_id, rx })
    }
}

impl Model for AbstractPhy {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Phy<C> for AbstractPhy {
    fn tier(&self) -> Tier {
        Tier::Abstract
    }

    fn rat(&self) -> Rat {
        self.table.envelope.rat
    }

    fn begin_tx(&mut self, ctx: &mut C, tx: NodeId, f: &FrameDescriptor) -> Result<TxHandle> {
        if f.bytes > crate::types::timing::MAX_MSDU_BYTES {
            return Err(RadioError::FrameTooLarge {
                bytes: f.bytes,
                cap: crate::types::timing::MAX_MSDU_BYTES,
            });
        }
        let air = air_time(f.bytes, f.mcs);
        let start = ctx.now();
        let handle = TxHandle {
            id: self.next_tx_id,
            tx,
            channel: f.channel,
            start,
            end: air.after(start),
            air_time: air,
        };
        self.next_tx_id += 1;
        Ok(handle)
    }

    fn air_time(&self, bytes: u32, mcs: Mcs) -> Duration {
        // Air time is exact even at this tier: it is arithmetic on the OFDM parameters,
        // not a model of anything, and the DCC gatekeeper needs it.
        air_time(bytes, mcs)
    }

    fn cca(&self, _ctx: &C, node: NodeId, _ch: ChannelId) -> CcaState {
        // The abstract tier has no arrival powers. The load estimate is the only thing it
        // knows about the channel, so CCA is busy when the estimated load is above the
        // top load bin's floor — enough for a MAC that only needs "is it worth trying".
        if self.load(node) >= 1.0 - self.table.envelope.load_axis.bin_width() {
            CcaState::Busy {
                energy_dbm: crate::phy::CBR_BUSY_THRESHOLD_DBM,
            }
        } else {
            CcaState::Idle
        }
    }

    fn finish_rx(&mut self, ctx: &mut C, rx: NodeId, h: RxHandle) -> RxOutcome {
        let Some(arrival) = self.arrivals.remove(&(rx.index(), h.tx)) else {
            return RxOutcome::Lost(LossCause::OutOfRange);
        };
        if arrival.distance_m >= self.table.range_max_m {
            return RxOutcome::Lost(LossCause::OutOfRange);
        }
        let p = self
            .table
            .probability(arrival.distance_m, self.load(arrival.rx));
        let received = ctx
            .rng(
                RngDomain::AbstractRx,
                EntityRef::LinkFrame {
                    link: LinkKey(arrival.tx, arrival.rx),
                    frame: arrival.start,
                },
            )
            .bool(p);
        if received {
            RxOutcome::Received {
                // The tier models no SINR and no RSSI. Reporting the sensitivity limit
                // would be a fabricated measurement, so the fields carry the one honest
                // value available: the table's own probability expressed in dB is not a
                // power, so both are NaN-free sentinels at the sensitivity floor and the
                // card says so.
                sinr_db: 0.0,
                rssi_dbm: arrival.frame.mcs.sensitivity_static_dbm(),
            }
        } else {
            RxOutcome::Lost(LossCause::Abstract)
        }
    }

    fn noise_floor_dbm(&self, _node: NodeId, _ch: ChannelId) -> f64 {
        crate::phy::THERMAL_NOISE_10MHZ_DBM + crate::phy::NOISE_FIGURE_HARDWARE_DB
    }
}

fn abstract_card(table: &DistanceLoadTable) -> ModelCard {
    abstract_card_with_fit(table, None)
}

fn abstract_card_with_fit(table: &DistanceLoadTable, fit: Option<&CalibrationFit>) -> ModelCard {
    let doc = Source::new(
        SourceKind::Standard,
        "04-models.md §4.9 (the calibration procedure) and 02-architecture.md §7.3 (the \
         5-percentage-point tolerance), invariant I-R4",
    );
    let mut card = ModelCard::new(
        AbstractPhy::ID,
        Family::Phy,
        "1.0.0",
        "The abstract tier's PHY: reception is a Bernoulli draw against a table of \
         (distance bin, load bin) calibrated to the high tier of the same RAT.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![
        Equation::new(
            "reception",
            "P_rx[d_bin][load_bin], bilinearly interpolated in distance and load; one \
             Bernoulli draw per (link, frame) from the AbstractRx stream",
        ),
        Equation::new(
            "Wilson interval",
            "p ± z·sqrt(p(1−p)/n + z²/4n²) / (1 + z²/n) about (p + z²/2n)/(1 + z²/n), \
             z = 1.96",
        ),
    ];
    card.parameters = vec![
        Parameter {
            name: "distance_bin_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(DISTANCE_BIN_M),
            range: None,
            source: doc.clone(),
            calibration: None,
        },
        Parameter {
            name: "range_max_m".to_string(),
            unit: "m".to_string(),
            default: serde_json::json!(RANGE_MAX_M),
            range: None,
            source: doc.clone(),
            calibration: None,
        },
        Parameter {
            name: "load_axis".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(match table.envelope.load_axis {
                LoadAxis::Cbr => "cbr",
                LoadAxis::HeardTransmitters => "heard-transmitters",
            }),
            range: Some(vec![
                serde_json::json!("cbr"),
                serde_json::json!("heard-transmitters"),
            ]),
            source: doc.clone(),
            calibration: None,
        },
        Parameter {
            name: "table_hash".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(table.content_hash_hex()),
            range: None,
            source: doc.clone(),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The table's envelope holds: same RAT, MCS, packet-size pattern and propagation \
         stack it was built with (04-models.md §4.9 step 6)."
            .to_string(),
        "The engine supplies the receiver's local load; this tier does not measure it.".to_string(),
        "Reported sinr_db and rssi_dbm are not measurements: this tier has no link \
         budget, and the fields carry the sensitivity floor so that a consumer cannot \
         mistake them for one."
            .to_string(),
    ];
    card.limitations = vec![
        "No SINR, no interference geometry, no capture, no timing, no hidden terminals \
         (04-models.md §4.9 tier table)."
            .to_string(),
        if table.accepted {
            "Accepted against the high tier within 5 percentage points per bin.".to_string()
        } else {
            "This table has NOT passed the acceptance test of 04-models.md §4.9 step 5, \
             so it is registered uncalibrated and the scenario validator warns \
             (invariant I-R4)."
                .to_string()
        },
        match fit {
            Some(f) => f.summary(),
            None => "No fit is recorded: this table was supplied rather than produced by \
                     the calibration procedure, so its parameters rest on whoever wrote \
                     them. CalibrationRequest::run is what produces a fitted one \
                     (04-models.md §4.9)."
                .to_string(),
        },
    ];
    card.ignores = vec![
        "Everything the medium tier models: SINR, interference, capture, frame timing.".to_string(),
    ];
    card.sources = vec![doc];
    // The fit *is* the validation status. 04-models.md §13's "Abstract radio (abstract
    // versus high)" row is the acceptance test of §4.9 step 5, so a table that passed it
    // with a recorded fit has been compared against the reference the row names and is
    // `literature-checked`; a table that merely exists and happens to be flagged accepted
    // — a hand-written one, or one from this crate's own tests — is only `unit-tested`;
    // and a table that failed, or was never fitted at all, is `unvalidated` and the
    // scenario validator warns (invariant I-R4, registry rule R2).
    let status = match fit {
        Some(f) if f.accepted && table.accepted => ValidationStatus::LiteratureChecked,
        Some(_) => ValidationStatus::Unvalidated,
        None if table.accepted => ValidationStatus::UnitTested,
        None => ValidationStatus::Unvalidated,
    };
    let mut references = Vec::new();
    let mut tests = vec![
        "the_calibration_routine_runs_end_to_end".to_string(),
        "the_table_interpolates_and_stops_at_its_range".to_string(),
        "a_frame_outside_the_envelope_is_rejected".to_string(),
    ];
    if let Some(f) = fit {
        references.push(Source {
            kind: SourceKind::Standard,
            reference: "04-models.md §13, abstract-radio row: the §4.9 procedure per RAT, \
                        ≤ 5 pp delivery per 25 m bin at each calibration density and the \
                        mean CBR within 0.05 (02-architecture.md §7.3, invariant I-R4)"
                .to_string(),
            accessed: None,
            note: Some(f.summary()),
        });
        for run in &f.high_tier_runs {
            references.push(Source {
                kind: SourceKind::Dataset,
                reference: format!("high-tier run: {run}"),
                accessed: None,
                note: Some(format!(
                    "engine build {}, propagation stack {}",
                    f.engine_build, f.propagation_stack
                )),
            });
        }
        tests.push("a_fitted_table_records_its_fit_on_its_card".to_string());
    }
    card.validation = Validation {
        status,
        references,
        tests,
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["abstract-rx".to_string()],
    };
    card
}

// =========================================================================================
// The legacy abstract models
// =========================================================================================

/// Which legacy abstract model an instance implements (04-models.md §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LegacyKind {
    /// `phy/abstract/disc-legacy`: heard iff `d <= radio_range_m`.
    Disc,
    /// `phy/abstract/logdistance-legacy`: heard iff
    /// `10·n·log10(rr/d) − margin + N(0, σ) >= 0`.
    LogDistance,
}

impl LegacyKind {
    /// The model id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            LegacyKind::Disc => "phy/abstract/disc-legacy",
            LegacyKind::LogDistance => "phy/abstract/logdistance-legacy",
        }
    }
}

/// The legacy engine's abstract radio constants (04-models.md §3.2, §4.9), kept for
/// parity with the recorded legacy corpus.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LegacyParams {
    /// `radio_range_m`, 500.
    pub radio_range_m: f64,
    /// `pathloss_exponent`, 2.7.
    pub pathloss_exponent: f64,
    /// `shadowing_sigma_db`, 4.0.
    pub shadowing_sigma_db: f64,
    /// `rx_sensitivity_margin_db`, 0.
    pub rx_sensitivity_margin_db: f64,
    /// `packet_loss_base`, 0.
    pub packet_loss_base: f64,
    /// `nlos_loss`, 0.
    pub nlos_loss: f64,
    /// `chan_capacity`, 40 in-range messages per step.
    pub chan_capacity: f64,
    /// The weather packet-loss add-on, per §2.6's legacy multipliers.
    pub weather_loss: f64,
}

impl LegacyParams {
    /// The constants as `run.py` L340-355 and L2684-2752 carry them.
    pub const LEGACY: LegacyParams = LegacyParams {
        radio_range_m: 500.0,
        pathloss_exponent: 2.7,
        shadowing_sigma_db: 4.0,
        rx_sensitivity_margin_db: 0.0,
        packet_loss_base: 0.0,
        nlos_loss: 0.0,
        chan_capacity: 40.0,
        weather_loss: 0.0,
    };

    /// The legacy congestion term: `min(0.8, max(0, (load − capacity)/capacity)·0.5)`.
    #[must_use]
    pub fn congestion(&self, load: f64) -> f64 {
        let over = ((load - self.chan_capacity) / self.chan_capacity).max(0.0);
        (over * 0.5).min(0.8)
    }

    /// The legacy drop probability at a distance under a load.
    #[must_use]
    pub fn drop_probability(&self, d_m: f64, load: f64) -> f64 {
        let nlos = self.nlos_loss * (d_m / self.radio_range_m);
        (self.packet_loss_base + nlos + self.congestion(load) + self.weather_loss).clamp(0.0, 1.0)
    }
}

impl Default for LegacyParams {
    fn default() -> Self {
        Self::LEGACY
    }
}

/// The two legacy abstract models of 04-models.md §4.9, kept for parity and registered
/// `uncalibrated`.
#[derive(Debug)]
pub struct LegacyAbstractPhy {
    card: ModelCard,
    kind: LegacyKind,
    params: LegacyParams,
    loads: BTreeMap<u32, f64>,
    arrivals: BTreeMap<(u32, u64), AbstractArrival>,
    next_tx_id: u64,
}

impl LegacyAbstractPhy {
    /// The disc model.
    #[must_use]
    pub fn disc() -> Self {
        Self::new(LegacyKind::Disc)
    }

    /// The log-distance model.
    #[must_use]
    pub fn log_distance() -> Self {
        Self::new(LegacyKind::LogDistance)
    }

    /// One of the two, by kind.
    #[must_use]
    pub fn new(kind: LegacyKind) -> Self {
        Self {
            card: legacy_card(kind),
            kind,
            params: LegacyParams::LEGACY,
            loads: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            next_tx_id: 1,
        }
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> LegacyParams {
        self.params
    }

    /// Sets the in-range message load at a receiver, the legacy congestion input.
    pub fn set_load(&mut self, node: NodeId, load: f64) {
        self.loads.insert(node.index(), load.max(0.0));
    }

    /// Registers an arrival at a distance.
    pub fn register_arrival(
        &mut self,
        tx: NodeId,
        rx: NodeId,
        distance_m: f64,
        frame: &FrameDescriptor,
        start: SimTime,
        tx_id: u64,
    ) -> RxHandle {
        let end = air_time(frame.bytes, frame.mcs).after(start);
        self.arrivals.insert(
            (rx.index(), tx_id),
            AbstractArrival {
                tx,
                rx,
                distance_m,
                frame: *frame,
                start,
                end,
            },
        );
        RxHandle { tx: tx_id, rx }
    }

    /// The legacy candidate-window cap of `logdistance-legacy`:
    /// `rr · 10^((cap_sigma·σ − min(0, margin))/(10·n))`.
    #[must_use]
    pub fn candidate_window_m(&self, cap_sigma: f64, radio_cap_max_mult: f64) -> f64 {
        let p = self.params;
        let exponent = (cap_sigma * p.shadowing_sigma_db - p.rx_sensitivity_margin_db.min(0.0))
            / (10.0 * p.pathloss_exponent);
        (p.radio_range_m * math::pow(10.0, exponent)).min(p.radio_range_m * radio_cap_max_mult)
    }
}

impl Model for LegacyAbstractPhy {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Phy<C> for LegacyAbstractPhy {
    fn tier(&self) -> Tier {
        Tier::Abstract
    }

    fn rat(&self) -> Rat {
        Rat::Dsrc80211p
    }

    fn begin_tx(&mut self, ctx: &mut C, tx: NodeId, f: &FrameDescriptor) -> Result<TxHandle> {
        let air = air_time(f.bytes, f.mcs);
        let start = ctx.now();
        let handle = TxHandle {
            id: self.next_tx_id,
            tx,
            channel: f.channel,
            start,
            end: air.after(start),
            air_time: air,
        };
        self.next_tx_id += 1;
        Ok(handle)
    }

    fn air_time(&self, bytes: u32, mcs: Mcs) -> Duration {
        air_time(bytes, mcs)
    }

    fn cca(&self, _ctx: &C, node: NodeId, _ch: ChannelId) -> CcaState {
        if self.loads.get(&node.index()).copied().unwrap_or(0.0) >= self.params.chan_capacity {
            CcaState::Busy {
                energy_dbm: crate::phy::CBR_BUSY_THRESHOLD_DBM,
            }
        } else {
            CcaState::Idle
        }
    }

    fn finish_rx(&mut self, ctx: &mut C, rx: NodeId, h: RxHandle) -> RxOutcome {
        let Some(arrival) = self.arrivals.remove(&(rx.index(), h.tx)) else {
            return RxOutcome::Lost(LossCause::OutOfRange);
        };
        let p = self.params;
        let link = LinkKey(arrival.tx, arrival.rx);
        let d = arrival.distance_m.max(1e-6);
        let heard = match self.kind {
            LegacyKind::Disc => d <= p.radio_range_m,
            LegacyKind::LogDistance => {
                let draw = ctx
                    .rng(RngDomain::AbstractRx, EntityRef::Link(link))
                    .normal(0.0, p.shadowing_sigma_db);
                10.0 * p.pathloss_exponent * math::log10(p.radio_range_m / d)
                    - p.rx_sensitivity_margin_db
                    + draw
                    >= 0.0
            }
        };
        if !heard {
            return RxOutcome::Lost(LossCause::OutOfRange);
        }
        let load = self.loads.get(&arrival.rx.index()).copied().unwrap_or(0.0);
        let dropped = ctx
            .rng(
                RngDomain::AbstractRx,
                EntityRef::LinkFrame {
                    link,
                    frame: arrival.start,
                },
            )
            .bool(p.drop_probability(d, load));
        if dropped {
            RxOutcome::Lost(LossCause::Abstract)
        } else {
            RxOutcome::Received {
                sinr_db: 0.0,
                rssi_dbm: arrival.frame.mcs.sensitivity_static_dbm(),
            }
        }
    }

    fn noise_floor_dbm(&self, _node: NodeId, _ch: ChannelId) -> f64 {
        crate::phy::THERMAL_NOISE_10MHZ_DBM + crate::phy::NOISE_FIGURE_HARDWARE_DB
    }
}

fn legacy_card(kind: LegacyKind) -> ModelCard {
    let legacy = Source {
        kind: SourceKind::Code,
        reference: match kind {
            LegacyKind::Disc => "legacy run.py L2684-2752 and L340-355, via 04-models.md §4.9",
            LegacyKind::LogDistance => {
                "legacy run.py L2696-2733 and L340-355, via 04-models.md §3.2 and §4.9"
            }
        }
        .to_string(),
        accessed: None,
        note: Some(
            "Kept for parity with the recorded legacy corpus. The constants have no \
             physical derivation: radio_range_m 500, pathloss_exponent 2.7, \
             shadowing_sigma_db 4.0, chan_capacity 40 in-range messages per step, and a \
             weather packet-loss add-on with no physical basis."
                .to_string(),
        ),
    };
    let mut card = ModelCard::new(
        kind.id(),
        Family::Phy,
        "1.0.0",
        match kind {
            LegacyKind::Disc => {
                "The legacy disc model: heard inside a fixed range, then dropped with a \
                 probability built from a base rate, an NLOS term, a congestion term and \
                 a weather term."
            }
            LegacyKind::LogDistance => {
                "The legacy log-distance model: heard when the median-range budget plus a \
                 log-normal draw clears zero, then dropped with the same legacy \
                 probability."
            }
        },
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![
        match kind {
            LegacyKind::Disc => Equation::new("reception", "heard iff d <= radio_range_m"),
            LegacyKind::LogDistance => Equation::new(
                "reception",
                "heard iff 10·n·log10(rr/d) − margin + N(0, σ) >= 0",
            ),
        },
        Equation::new(
            "drop",
            "p = packet_loss_base + nlos_loss·(d/rr) + min(0.8, max(0, (load − \
             chan_capacity)/chan_capacity)·0.5) + weather_loss",
        ),
    ];
    let p = LegacyParams::LEGACY;
    card.parameters = vec![
        Parameter::new(
            "radio_range_m",
            "m",
            serde_json::json!(p.radio_range_m),
            legacy.clone(),
        ),
        Parameter::new(
            "pathloss_exponent",
            "-",
            serde_json::json!(p.pathloss_exponent),
            legacy.clone(),
        ),
        Parameter::new(
            "shadowing_sigma_db",
            "dB",
            serde_json::json!(p.shadowing_sigma_db),
            legacy.clone(),
        ),
        Parameter::new(
            "rx_sensitivity_margin_db",
            "dB",
            serde_json::json!(p.rx_sensitivity_margin_db),
            legacy.clone(),
        ),
        Parameter::new(
            "packet_loss_base",
            "-",
            serde_json::json!(p.packet_loss_base),
            legacy.clone(),
        ),
        Parameter::new(
            "nlos_loss",
            "-",
            serde_json::json!(p.nlos_loss),
            legacy.clone(),
        ),
        Parameter::new(
            "chan_capacity",
            "messages/step",
            serde_json::json!(p.chan_capacity),
            legacy.clone(),
        ),
    ];
    card.assumptions = vec![
        "Reception depends on distance and an aggregate load only.".to_string(),
        "Draws come from per-link streams in this engine, not from the legacy global \
         generator."
            .to_string(),
    ];
    card.limitations = vec![
        "Uncalibrated by construction: these are the legacy engine's numbers, kept for \
         parity, not a model of a radio."
            .to_string(),
        "The weather term is a packet-loss add-on with no physical basis and survives \
         only inside these models (04-models.md §3.6)."
            .to_string(),
    ];
    card.ignores = vec!["Everything above the abstract tier.".to_string()];
    card.sources = vec![legacy];
    card.validation = Validation {
        // Registered uncalibrated, exactly as 04-models.md §4.9 requires.
        status: ValidationStatus::Unvalidated,
        references: Vec::new(),
        tests: vec!["the_legacy_models_reproduce_their_own_constants".to_string()],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["abstract-rx".to_string()],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;
    use crate::types::SduRef;
    use v2xw_core::ids::{FrameSeq, SduId};

    fn envelope() -> TableEnvelope {
        CalibrationPlan::document_default().envelope(
            "propagation/log-distance-shadowing@abbas-los-highway + fading/nakagami-m",
            vec!["0".repeat(64)],
            "v2xw-radio test",
        )
    }

    fn frame(bytes: u32) -> FrameDescriptor {
        FrameDescriptor::broadcast(
            bytes,
            Mcs::R6Qpsk12,
            SduRef::new(SduId::new(1), FrameSeq::new(1)),
        )
    }

    /// A small calibration request: one density, one seed, a short run — enough to
    /// exercise steps 2 to 5 end to end without spending a nightly job on it.
    fn tiny_request() -> CalibrationRequest {
        let mut plan = CalibrationPlan::document_default();
        plan.densities_veh_km = vec![20.0];
        plan.seeds = vec![0xC0FF_EE01];
        plan.packet_bytes = vec![300];
        plan.range_max_m = 200.0;
        CalibrationRequest {
            plan,
            channel: ChannelModel::AbbasLosHighway,
            ring_m: 400.0,
            duration: Duration::from_millis(200),
            warmup: Duration::from_millis(100),
            engine_build: "v2xw-radio test".to_string(),
            world_hashes: vec!["0".repeat(64)],
        }
    }

    /// The request's sweeps are one per `(density, seed)` point, carry the plan's own
    /// load and distance axes, and are a pure function of the request.
    #[test]
    fn a_calibration_request_expands_to_one_high_tier_run_per_point() {
        let mut req = tiny_request();
        req.plan.densities_veh_km = vec![50.0, 100.0];
        req.plan.seeds = vec![1, 2, 3];
        let sweeps = req.sweeps();
        assert_eq!(sweeps.len(), 6, "two densities times three seeds");
        // Density-major order.
        assert_eq!(sweeps[0].density_veh_per_km, 50.0);
        assert_eq!(sweeps[2].density_veh_per_km, 50.0);
        assert_eq!(sweeps[3].density_veh_per_km, 100.0);
        // Every point has its own seed: no two runs replay the same placement.
        let mut seeds: Vec<u64> = sweeps.iter().map(|s| s.seed).collect();
        seeds.sort_unstable();
        seeds.dedup();
        assert_eq!(seeds.len(), 6);
        // The axes come from the plan, not from the harness's defaults.
        for sweep in &sweeps {
            assert_eq!(sweep.bin_m, DISTANCE_BIN_M);
            assert_eq!(sweep.max_distance_m, req.plan.range_max_m);
            assert_eq!(sweep.tx_power_dbm, req.plan.tx_power_dbm);
            assert_eq!(sweep.payload_cycle, req.plan.packet_bytes);
            assert_eq!(sweep.ring_m, req.ring_m);
            assert!(
                sweep.ring_m >= 2.0 * sweep.max_distance_m,
                "a distance bin must not wrap onto itself"
            );
        }
        assert_eq!(req.sweeps(), sweeps, "the expansion is deterministic");
        assert_eq!(req.dsrc_config().mcs, req.plan.mcs);
        assert_eq!(
            req.envelope().propagation_stack,
            ChannelModel::AbbasLosHighway.label()
        );
    }

    /// The whole point of the wiring: the table's parameters come out of a high-tier run,
    /// and the fit that produced them is on the card.
    #[test]
    fn a_fitted_table_records_its_fit_on_its_card() {
        let req = tiny_request();
        let calibrated = req.run();
        assert!(!calibrated.high_tier_reports.is_empty());
        assert_eq!(calibrated.high_tier_reports.len(), 1);
        let fit = calibrated.fit.clone();
        assert!(fit.samples > 0, "the high-tier run produced samples");
        assert_eq!(fit.bins_compared, calibrated.report.bins.len());
        assert_eq!(fit.table_hash, calibrated.table.content_hash_hex());
        assert_eq!(fit.propagation_stack, ChannelModel::AbbasLosHighway.label());
        assert_eq!(fit.engine_build, "v2xw-radio test");
        assert_eq!(fit.accepted, calibrated.accepted());
        assert_eq!(fit.accepted, calibrated.table.accepted);
        assert!(fit.summary().contains("high-tier run"), "{}", fit.summary());

        // The card carries it, and the status follows the fit rather than the flag.
        let phy = calibrated.into_phy();
        let card = phy.card();
        card.validate().expect("card validates");
        assert!(phy.fit().is_some());
        let expected = if fit.accepted {
            ValidationStatus::LiteratureChecked
        } else {
            ValidationStatus::Unvalidated
        };
        assert_eq!(card.validation.status, expected, "{}", fit.summary());
        assert!(
            card.validation
                .references
                .iter()
                .any(|r| r.reference.contains("§13")),
            "the §13 abstract-radio row is the reference the fit was judged against"
        );
        assert!(
            card.validation
                .references
                .iter()
                .any(|r| r.reference.starts_with("high-tier run:")),
            "the runs the fit came from are named"
        );
        assert!(
            card.limitations.iter().any(|l| l.contains("high-tier run")),
            "{:?}",
            card.limitations
        );
        // …and the fit is reproducible.
        let again = tiny_request().run();
        assert_eq!(again.fit, fit);
    }

    /// A table with no fit says so, and is never dressed up as literature-checked.
    #[test]
    fn an_unfitted_table_is_never_literature_checked() {
        let mut table = DistanceLoadTable::empty(envelope());
        table.accepted = true;
        let phy = AbstractPhy::new(table);
        assert!(phy.fit().is_none());
        assert_eq!(phy.card().validation.status, ValidationStatus::UnitTested);
        assert!(
            phy.card()
                .limitations
                .iter()
                .any(|l| l.contains("No fit is recorded")),
            "{:?}",
            phy.card().limitations
        );
        // A failed fit takes the card back to unvalidated whatever the flag says.
        let mut table = DistanceLoadTable::empty(envelope());
        table.accepted = true;
        let bad = CalibrationFit {
            accepted: false,
            worst_pdr_gap_pp: 42.0,
            abstract_mean_cbr: 0.1,
            high_mean_cbr: 0.4,
            mean_cbr_gap: 0.3,
            bins_compared: 12,
            bins_failed: 7,
            samples: 1_000,
            table_hash: table.content_hash_hex(),
            high_tier_runs: vec!["a run".to_string()],
            propagation_stack: "abbas-los-highway".to_string(),
            engine_build: "test".to_string(),
        };
        assert!(bad.summary().contains("OUTSIDE"), "{}", bad.summary());
        let phy = AbstractPhy::calibrated(table, bad);
        assert_eq!(phy.card().validation.status, ValidationStatus::Unvalidated);
        phy.card().validate().expect("card validates");
    }

    #[test]
    fn the_plan_is_the_documents_own() {
        let p = CalibrationPlan::document_default();
        assert_eq!(p.speeds_kmh, vec![15.0, 60.0, 70.0, 140.0]);
        assert_eq!(p.densities_veh_km, vec![50.0, 100.0, 200.0]);
        assert_eq!(p.urban_neighbours_per_100m, vec![(14.8, 8.8), (25.4, 25.4)]);
        assert_eq!(p.rate_hz, 10.0);
        assert_eq!(p.packet_bytes, vec![300, 190, 190, 190, 190]);
        assert_eq!(p.mcs, Mcs::R6Qpsk12);
        assert_eq!(p.tx_power_dbm, 23.0);
        assert_eq!(p.seeds.len(), DEFAULT_SEEDS as usize);
        assert_eq!(p.headway_s, 2.5);
        assert_eq!(p.range_max_m, 1_000.0);
        assert_eq!(DISTANCE_BIN_M, 25.0);
        assert_eq!(CBR_BIN, 0.1);
        assert_eq!(PDR_TOLERANCE, 0.05);
        assert_eq!(CBR_TOLERANCE, 0.05);
    }

    #[test]
    fn the_wilson_interval_brackets_the_ratio() {
        let c = Cell::wilson(90, 100);
        assert_eq!(c.p, 0.9);
        assert!(c.lower < 0.9 && c.upper > 0.9, "{c:?}");
        // At the edges the interval stays inside [0, 1], which is the reason Wilson is
        // used instead of the normal approximation.
        let none = Cell::wilson(0, 50);
        assert_eq!(none.p, 0.0);
        assert!(none.lower >= 0.0 && none.upper > 0.0, "{none:?}");
        let all = Cell::wilson(50, 50);
        assert_eq!(all.p, 1.0);
        assert!(all.upper <= 1.0 && all.lower < 1.0, "{all:?}");
        // An unobserved cell says nothing.
        assert_eq!(Cell::wilson(0, 0), Cell::default());
        // More trials tighten the interval.
        let wide = Cell::wilson(9, 10);
        let narrow = Cell::wilson(900, 1_000);
        assert!(narrow.upper - narrow.lower < wide.upper - wide.lower);
    }

    #[test]
    fn the_table_interpolates_and_stops_at_its_range() {
        let mut table = DistanceLoadTable::empty(envelope());
        // A clean gradient: fully received up to 100 m, nothing beyond 500 m.
        for (d, row) in table.cells.iter_mut().enumerate() {
            let p = (1.0 - (d as f64 * DISTANCE_BIN_M) / 500.0).clamp(0.0, 1.0);
            for cell in row.iter_mut() {
                *cell = Cell::wilson((p * 1_000.0) as u64, 1_000);
            }
        }
        // Inside the range the probability follows the gradient.
        assert!(table.probability(10.0, 0.2) > 0.9);
        let mid = table.probability(250.0, 0.2);
        assert!((mid - 0.5).abs() < 0.06, "{mid}");
        assert_eq!(
            table.probability(1_000.0, 0.2),
            0.0,
            "the range is the range"
        );
        assert_eq!(table.probability(5_000.0, 0.2), 0.0);
        // Monotone in distance, because the table is.
        let mut previous = 1.1;
        let mut d = 0.0;
        while d < 1_000.0 {
            let p = table.probability(d, 0.3);
            assert!(p <= previous + 1e-9, "rose at {d}");
            previous = p;
            d += 5.0;
        }
        // The bins themselves.
        assert_eq!(table.distance_bin_of(0.0), Some(0));
        assert_eq!(table.distance_bin_of(24.9), Some(0));
        assert_eq!(table.distance_bin_of(25.0), Some(1));
        assert_eq!(table.distance_bin_of(1_000.0), None);
        assert_eq!(LoadAxis::Cbr.bin_of(0.05), 0);
        assert_eq!(LoadAxis::Cbr.bin_of(0.15), 1);
        assert_eq!(LoadAxis::Cbr.bin_of(2.0), LoadAxis::Cbr.bins() - 1);
        assert_eq!(LoadAxis::HeardTransmitters.bin_of(15.0), 1);
    }

    #[test]
    fn the_calibration_routine_runs_end_to_end() {
        // Step 2's physical half: a synthetic 100 veh/km drop through the real
        // propagation, fading and error models.
        let mut ctx = TestCtx::new(4_242);
        let plan = CalibrationPlan::document_default();
        let samples = link_budget_samples(&mut ctx, &plan, 100.0, 40, 5, 0.3, EnvClass::Highway);
        assert!(!samples.is_empty());
        // Steps 3 to 5.
        let (table, report) = calibrate(&plan, envelope(), samples.clone(), 7);
        // Every observed cell has an interval, and the near cells are mostly received.
        let near = &table.cells[0];
        assert!(
            near.iter().any(|c| c.trials > 0),
            "the near bin was observed"
        );
        let observed: Vec<&Cell> = table
            .cells
            .iter()
            .flatten()
            .filter(|c| c.trials > 0)
            .collect();
        assert!(observed.len() > 5, "{} observed cells", observed.len());
        for cell in &observed {
            // The interval brackets the ratio, to within the last bit of the arithmetic:
            // a cell with every trial received has p = 1 and an upper end that misses it
            // by one ulp.
            assert!(
                cell.lower <= cell.p + 1e-12 && cell.p <= cell.upper + 1e-12,
                "{cell:?}"
            );
        }
        // The acceptance test ran and says something about every observed cell.
        assert_eq!(report.bins.len(), observed.len());
        assert!(report.worst_pdr_gap >= 0.0);
        // Reception falls off with distance, which is the whole point of the table.
        let first_p = table.cells[0]
            .iter()
            .find(|c| c.trials > 0)
            .map(|c| c.p)
            .expect("observed");
        let far = table
            .cells
            .iter()
            .enumerate()
            .rev()
            .find_map(|(d, row)| row.iter().find(|c| c.trials > 0).map(|c| (d, c.p)))
            .expect("some far cell was observed");
        assert!(first_p >= far.1, "{first_p} at 0-25 m against {:?}", far);
        // The routine is deterministic: same samples, same seed, same table.
        let (again, report2) = calibrate(&plan, envelope(), samples, 7);
        assert_eq!(table, again);
        assert_eq!(report.accepted, report2.accepted);
        assert_eq!(table.content_hash_hex(), again.content_hash_hex());
        // And the quantised form is on the declared grid.
        let q = table.quantized();
        for cell in q.cells.iter().flatten() {
            assert!(v2xw_core::math::is_on_grid(cell.p, numeric::Q_RATIO));
        }
    }

    #[test]
    fn the_acceptance_test_rejects_a_table_that_does_not_match() {
        let e = envelope();
        let mut high = DistanceLoadTable::empty(e.clone());
        let mut abstract_table = DistanceLoadTable::empty(e);
        // The high tier received everything in one cell; the abstract table says nothing
        // gets through. That is a 100-percentage-point gap.
        high.cells[2][3] = Cell::wilson(1_000, 1_000);
        abstract_table.cells[2][3] = Cell::wilson(0, 1_000);
        let report = AcceptanceReport::compare(&abstract_table, &high, 0.3, 0.3);
        assert!(!report.accepted);
        assert_eq!(report.bins.len(), 1);
        assert!(!report.bins[0].within_tolerance);
        assert!((report.worst_pdr_gap - 1.0).abs() < 1e-9);
        // A small gap passes.
        abstract_table.cells[2][3] = Cell::wilson(970, 1_000);
        let ok = AcceptanceReport::compare(&abstract_table, &high, 0.3, 0.3);
        assert!(ok.accepted, "{ok:?}");
        // A CBR estimate off by more than 0.05 fails even when every bin passes.
        let cbr_off = AcceptanceReport::compare(&abstract_table, &high, 0.3, 0.4);
        assert!(!cbr_off.accepted);
    }

    #[test]
    fn a_frame_outside_the_envelope_is_rejected() {
        let mut phy = AbstractPhy::new(DistanceLoadTable::empty(envelope()));
        // The plan's pattern is {300, 190, …} bytes at 6 Mbit/s.
        assert!(
            phy.register_arrival(NodeId::new(0), NodeId::new(1), 100.0, &frame(300), 0, 1)
                .is_ok()
        );
        let err = phy
            .register_arrival(NodeId::new(0), NodeId::new(1), 100.0, &frame(1_000), 0, 2)
            .expect_err("a 1,000 B CPM load on a 300 B table");
        assert!(matches!(err, RadioError::OutsideEnvelope { .. }));
        // And the envelope itself answers the question.
        let e = envelope();
        assert!(e.admits(300, Mcs::R6Qpsk12));
        assert!(e.admits(190, Mcs::R6Qpsk12));
        assert!(!e.admits(300, Mcs::R3Bpsk12));
        assert!(!e.admits(1_000, Mcs::R6Qpsk12));
    }

    #[test]
    fn an_uncalibrated_table_registers_unvalidated() {
        // Invariant I-R4: a table that has not passed acceptance is registered
        // uncalibrated and the validator warns.
        let not_accepted = DistanceLoadTable::empty(envelope());
        assert!(!not_accepted.accepted);
        let phy = AbstractPhy::new(not_accepted);
        assert_eq!(phy.card().validation.status, ValidationStatus::Unvalidated);
        assert!(
            phy.card()
                .limitations
                .iter()
                .any(|l| l.contains("NOT passed"))
        );
        let mut accepted = DistanceLoadTable::empty(envelope());
        accepted.accepted = true;
        let phy = AbstractPhy::new(accepted);
        assert_eq!(phy.card().validation.status, ValidationStatus::UnitTested);
        // The registered id carries the table hash (step 4).
        assert!(phy.id_with_hash().starts_with(AbstractPhy::ID));
        assert!(phy.id_with_hash().contains('@'));
    }

    #[test]
    fn the_abstract_phy_draws_against_the_table() {
        let mut ctx = TestCtx::new(11);
        let mut table = DistanceLoadTable::empty(envelope());
        // Everything is received inside 100 m, nothing beyond.
        for (d, row) in table.cells.iter_mut().enumerate() {
            let p = if d < 4 { 1.0 } else { 0.0 };
            for cell in row.iter_mut() {
                *cell = Cell::wilson((p * 100.0) as u64, 100);
            }
        }
        let mut phy = AbstractPhy::new(table);
        phy.set_load(NodeId::new(1), 0.35);
        assert_eq!(phy.load(NodeId::new(1)), 0.35);
        // A near link is received.
        let h = phy
            .register_arrival(NodeId::new(0), NodeId::new(1), 20.0, &frame(300), 0, 1)
            .expect("inside the envelope");
        assert!(matches!(
            Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h),
            RxOutcome::Received { .. }
        ));
        // A far one inside the table's range is lost as an abstract draw.
        let h = phy
            .register_arrival(NodeId::new(0), NodeId::new(1), 600.0, &frame(300), 0, 2)
            .expect("inside the envelope");
        assert_eq!(
            Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h),
            RxOutcome::Lost(LossCause::Abstract)
        );
        // Beyond the range it is out of range, which is a different statement.
        let h = phy
            .register_arrival(NodeId::new(0), NodeId::new(1), 1_500.0, &frame(300), 0, 3)
            .expect("registered");
        assert_eq!(
            Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h),
            RxOutcome::Lost(LossCause::OutOfRange)
        );
        // Air time is exact even here: the DCC gatekeeper needs it.
        assert_eq!(
            Phy::<TestCtx>::air_time(&phy, 400, Mcs::R6Qpsk12),
            crate::phy::air_time(400, Mcs::R6Qpsk12)
        );
        assert_eq!(Phy::<TestCtx>::tier(&phy), Tier::Abstract);
        assert_eq!(Phy::<TestCtx>::rat(&phy), Rat::Dsrc80211p);
    }

    #[test]
    fn the_legacy_models_reproduce_their_own_constants() {
        let p = LegacyParams::LEGACY;
        assert_eq!(p.radio_range_m, 500.0);
        assert_eq!(p.pathloss_exponent, 2.7);
        assert_eq!(p.shadowing_sigma_db, 4.0);
        assert_eq!(p.rx_sensitivity_margin_db, 0.0);
        assert_eq!(p.chan_capacity, 40.0);
        // The congestion term: zero at capacity, 0.5·overload, capped at 0.8.
        assert_eq!(p.congestion(0.0), 0.0);
        assert_eq!(p.congestion(40.0), 0.0);
        assert!((p.congestion(80.0) - 0.5).abs() < 1e-12);
        assert_eq!(p.congestion(4_000.0), 0.8);
        // With the legacy defaults, an in-range frame under capacity is never dropped.
        assert_eq!(p.drop_probability(250.0, 10.0), 0.0);

        let mut ctx = TestCtx::new(12);
        let mut disc = LegacyAbstractPhy::disc();
        // Inside the disc: received. Outside: out of range, deterministically.
        let h = disc.register_arrival(NodeId::new(0), NodeId::new(1), 499.0, &frame(300), 0, 1);
        assert!(matches!(
            Phy::finish_rx(&mut disc, &mut ctx, NodeId::new(1), h),
            RxOutcome::Received { .. }
        ));
        let h = disc.register_arrival(NodeId::new(0), NodeId::new(1), 501.0, &frame(300), 0, 2);
        assert_eq!(
            Phy::finish_rx(&mut disc, &mut ctx, NodeId::new(1), h),
            RxOutcome::Lost(LossCause::OutOfRange)
        );

        // The log-distance model's median range is the legacy radio_range_m: at exactly
        // that distance the budget is zero, so it is a coin flip on the shadowing draw.
        let mut logd = LegacyAbstractPhy::log_distance();
        let mut heard = 0;
        for i in 0..2_000u64 {
            let h = logd.register_arrival(
                NodeId::new(0),
                NodeId::new(u32::try_from(i % 500).unwrap_or(1) + 1),
                500.0,
                &frame(300),
                i,
                i,
            );
            if matches!(
                Phy::finish_rx(&mut logd, &mut ctx, h.rx, h),
                RxOutcome::Received { .. }
            ) {
                heard += 1;
            }
        }
        let fraction = f64::from(heard) / 2_000.0;
        assert!((fraction - 0.5).abs() < 0.05, "{fraction}");
        // The candidate window cap of the legacy engine.
        let window = logd.candidate_window_m(3.0, 2.0);
        assert!(window > 500.0 && window <= 1_000.0, "{window}");
        // Both are registered uncalibrated.
        assert_eq!(
            LegacyAbstractPhy::disc().card().validation.status,
            ValidationStatus::Unvalidated
        );
        assert_eq!(
            LegacyAbstractPhy::log_distance().card().validation.status,
            ValidationStatus::Unvalidated
        );
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        let mut accepted = DistanceLoadTable::empty(envelope());
        accepted.accepted = true;
        for card in [
            abstract_card(&accepted),
            legacy_card(LegacyKind::Disc),
            legacy_card(LegacyKind::LogDistance),
        ] {
            card.validate().expect("card validates");
            card.check_api_version().expect("api version");
            registry.register(card).expect("registers");
        }
        assert!(registry.contains(AbstractPhy::ID));
        assert!(registry.contains(LegacyKind::Disc.id()));
        assert!(registry.contains(LegacyKind::LogDistance.id()));
    }
}
