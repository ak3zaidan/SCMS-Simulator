//! The cellular Uu link: `cellular/uu/fixed-latency`, `cellular/uu/cell-capacity-mm1`
//! and `cellular/uu/handover-outage` (04-models.md §10.1), plus the store-and-forward
//! buffer a caller needs when [`SendOutcome::NoCoverage`] comes back.
//!
//! This is the *other* way a node reaches the world, and it is here rather than in a
//! crate of its own because it shares the radio crate's arithmetic: a path loss, a
//! quality class, a capacity and a queue. 03-interfaces.md §5 declares the seam
//! ([`CellularUu`]) and 04-models.md §10.1 fixes the three tiers.
//!
//! # What each tier adds
//!
//! | Tier | Model | Adds |
//! |---|---|---|
//! | `abstract` | [`FixedLatencyUu`] | a one-way latency per direction, drawn from the measured means; no capacity, no coverage holes unless a plan is given |
//! | `medium` | [`CellCapacityUu`] | per-cell UL and DL capacity, an M/M/1 queue per node, and the transport-network latency of the measured chain by MEC placement |
//! | `high` | [`HandoverOutageUu`] | handover interruption per TS 36.133, per-packet loss at the measured percentiles, and outage where coverage fails |
//!
//! # The latency figures, and the one thing to be careful of
//!
//! 04-models.md §10.1 prints *round-trip* times — 29.2 ± 4.8 ms LTE first hop, 27.4 ± 6.4
//! ms 5G — and says "one way modeled as half". Every preset here therefore halves both
//! the mean and the standard deviation and says so in its name
//! ([`UuLatencyPreset::one_way_ms`]); halving the mean and keeping the spread would double
//! the modelled jitter of a real measurement.
//!
//! The Ookla medians (T-Mobile 31, Verizon 32, AT&T 34 ms) are *medians of a one-way-ish
//! carrier latency metric* and 04-models.md §10.1 marks their p95 UNVERIFIED, so the
//! presets built on them carry a `todo-calibrate` spread and a plan.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{CellId, NodeId};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime};

use crate::types::DropCause;

// =========================================================================================
// The seam (03-interfaces.md §5)
// =========================================================================================

/// Which way a packet is going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// From the UE to the network.
    Uplink,
    /// From the network to the UE.
    Downlink,
}

impl Direction {
    /// The label a record prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Direction::Uplink => "uplink",
            Direction::Downlink => "downlink",
        }
    }
}

/// The quality of service a sender asks for.
///
/// Three classes, because that is what the models here can honour differently: a
/// best-effort packet waits behind the queue, a low-latency one is served first, and a
/// reliable one is retried once when the loss draw takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Qos {
    /// Best effort.
    BestEffort,
    /// Latency-sensitive: served ahead of best effort.
    LowLatency,
    /// Loss-sensitive: one retry when the per-packet loss draw takes it.
    Reliable,
}

/// The serving cell and its quality, as [`CellularUu::coverage`] reports it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CellView {
    /// The serving cell.
    pub cell: CellId,
    /// The distance to it, metres.
    pub distance_m: f64,
    /// An RSRP-like received power, dBm, from the Uu path loss of 04-models.md §3.3.
    pub rsrp_dbm: f64,
    /// The quality class the models branch on.
    pub quality: CellQuality,
}

/// A coarse quality class: what the models need, and no finer than the sources support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CellQuality {
    /// Strong: within the cell's inner radius.
    Good,
    /// Usable, with the measured degradation of a dense urban canyon.
    Fair,
    /// At the edge: usable but with the outage behaviour of the `high` tier.
    Edge,
}

/// What a send did (03-interfaces.md §5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SendOutcome {
    /// Delivery is scheduled.
    Scheduled {
        /// When the packet arrives.
        deliver_at: SimTime,
        /// Bytes charged to the accounting bucket (invariant I-N1).
        bytes_on_wire: u32,
    },
    /// The packet was dropped.
    Dropped(DropCause),
    /// No serving cell: the caller stores and forwards
    /// ([`StoreAndForward`]).
    NoCoverage,
}

impl SendOutcome {
    /// The delivery instant, when there is one.
    #[must_use]
    pub const fn deliver_at(&self) -> Option<SimTime> {
        match self {
            SendOutcome::Scheduled { deliver_at, .. } => Some(*deliver_at),
            _ => None,
        }
    }
}

/// The cellular Uu seam of 03-interfaces.md §5.
///
/// Generic over the context for the reason [`crate::traits`] gives in full: `Ctx` carries
/// three associated types, so `&mut dyn Ctx` does not name a type until the engine fixes
/// them.
///
/// # The one divergence from the published signature
///
/// `handover` returns a [`Duration`], not a `SimTime`. 03-interfaces.md §1 already records
/// that several family traits "still take or return `SimTime` where they mean a span" and
/// names `CellularUu::handover` as one of them. An interruption *is* a span, this crate
/// has `Duration` available, and returning an instant would make every caller add
/// `ctx.now()` by hand — which is the mistake `Duration` exists to prevent. The change is
/// one line to reverse.
pub trait CellularUu<C: Ctx + ?Sized>: Model {
    /// The fidelity tier this instance is configured for.
    fn tier(&self) -> Tier;

    /// The serving cell at a point and an instant, or `None` for no coverage.
    fn coverage(&self, p: Vec3, t: SimTime) -> Option<CellView>;

    /// Schedules delivery of `bytes` in one direction for one UE.
    fn send(
        &mut self,
        ctx: &mut C,
        ue: NodeId,
        dir: Direction,
        bytes: u32,
        qos: Qos,
    ) -> SendOutcome;

    /// The interruption a handover between two cells costs.
    fn handover(&mut self, ctx: &mut C, ue: NodeId, from: CellId, to: CellId) -> Duration;
}

// =========================================================================================
// Cell layout (04-models.md §5.3, §10.1)
// =========================================================================================

/// The Uu path loss of the LTE V2X study: `128.1 + 37.6·log10(R_km)`, σ 8 dB,
/// decorrelation 50 m [TR 36.885 Table A.1.4-2, via 04-models.md §3.3].
#[must_use]
pub fn uu_path_loss_db(d_m: f64) -> f64 {
    let km = (d_m.max(1.0)) / 1000.0;
    128.1 + 37.6 * math::log10(km)
}

/// A macro-cell layout: a square lattice of sites at the inter-site distance.
///
/// A lattice rather than a hexagon because the inter-site distance is the only number the
/// sources fix — urban macro 500 m, highway 1,732 m with 500 m optional
/// [TR 37.885, TR 38.913, via 04-models.md §5.3, §10.1] — and a lattice reproduces it
/// exactly with an arithmetic cell lookup that cannot depend on an iteration order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellPlan {
    /// Inter-site distance, metres.
    pub isd_m: f64,
    /// Base-station transmit power, dBm [TR 37.885 Table 6.1.1-1: 49 dBm below 6 GHz].
    pub bs_tx_power_dbm: f64,
    /// Base-station antenna gain, dBi.
    pub bs_gain_dbi: f64,
    /// The received power below which there is no coverage at all, dBm.
    pub no_coverage_below_dbm: f64,
    /// How many sites the lattice spans in each direction from the origin. A point beyond
    /// the lattice has no coverage, which is how a scenario models a coverage hole without
    /// a map.
    pub extent_sites: i32,
}

impl CellPlan {
    /// The urban macro plan: ISD 500 m [TR 37.885 via 04-models.md §5.3].
    #[must_use]
    pub fn urban_macro() -> Self {
        Self {
            isd_m: 500.0,
            bs_tx_power_dbm: 49.0,
            bs_gain_dbi: 8.0,
            // −124 dBm is about 20 dB below the LTE reference-signal received power a UE
            // reports as its lowest usable value; the exact figure is not printed in
            // 04-models.md §10.1, so it is `todo-calibrate` in the card.
            no_coverage_below_dbm: -124.0,
            extent_sites: 64,
        }
    }

    /// The highway plan: ISD 1,732 m [TR 37.885 via 04-models.md §5.3].
    #[must_use]
    pub fn highway_macro() -> Self {
        Self {
            isd_m: 1732.0,
            ..Self::urban_macro()
        }
    }

    /// The nearest site's lattice coordinates.
    #[must_use]
    pub fn nearest_site(&self, p: Vec3) -> (i32, i32) {
        (
            (p.x / self.isd_m).round() as i32,
            (p.y / self.isd_m).round() as i32,
        )
    }

    /// The dense cell id of a lattice coordinate, so the id is a pure function of the
    /// geometry and never of a hash order.
    #[must_use]
    pub fn cell_id(&self, site: (i32, i32)) -> CellId {
        let span = 2 * self.extent_sites + 1;
        let i = (site.0 + self.extent_sites) as i64;
        let j = (site.1 + self.extent_sites) as i64;
        CellId::new((j * i64::from(span) + i) as u32)
    }

    /// The serving cell at a point, or `None` beyond the lattice or below the coverage
    /// floor.
    #[must_use]
    pub fn coverage(&self, p: Vec3) -> Option<CellView> {
        let site = self.nearest_site(p);
        if site.0.abs() > self.extent_sites || site.1.abs() > self.extent_sites {
            return None;
        }
        let dx = p.x - f64::from(site.0) * self.isd_m;
        let dy = p.y - f64::from(site.1) * self.isd_m;
        let d = math::sqrt(dx * dx + dy * dy).max(1.0);
        let rsrp = self.bs_tx_power_dbm + self.bs_gain_dbi - uu_path_loss_db(d);
        if rsrp < self.no_coverage_below_dbm {
            return None;
        }
        let quality = if d < self.isd_m * 0.25 {
            CellQuality::Good
        } else if d < self.isd_m * 0.5 {
            CellQuality::Fair
        } else {
            CellQuality::Edge
        };
        Some(CellView {
            cell: self.cell_id(site),
            distance_m: math::quantize_to(d, 1e-3),
            rsrp_dbm: crate::numeric::q_db(rsrp),
            quality,
        })
    }
}

// =========================================================================================
// Latency presets (04-models.md §10.1)
// =========================================================================================

/// A measured one-way latency, mean and standard deviation in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LatencySpec {
    /// Mean, ms.
    pub mean_ms: f64,
    /// Standard deviation, ms.
    pub sigma_ms: f64,
}

impl LatencySpec {
    /// A spec from a mean and a spread.
    #[must_use]
    pub const fn new(mean_ms: f64, sigma_ms: f64) -> Self {
        Self { mean_ms, sigma_ms }
    }

    /// The spec a round-trip measurement implies, one way: both moments halved.
    ///
    /// 04-models.md §10.1 says "one way modeled as half" of the round trip. Halving the
    /// mean is what it asks; halving the spread too is what keeps the modelled jitter the
    /// measurement's rather than twice it.
    #[must_use]
    pub const fn from_rtt(mean_ms: f64, sigma_ms: f64) -> Self {
        Self {
            mean_ms: mean_ms / 2.0,
            sigma_ms: sigma_ms / 2.0,
        }
    }
}

/// The latency presets 04-models.md §10.1 prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UuLatencyPreset {
    /// LTE first hop: RTT 29.2 ± 4.8 ms [Narayanan et al. WWW'20 Table 2].
    LteFirstHop,
    /// 5G mmWave first hop: RTT 27.4 ± 6.4 ms [same].
    Nr5gFirstHop,
    /// 5G to an east-coast server: RTT 54.0 ± 4.5 ms [same].
    Nr5gEastCoast,
    /// 5G to a west-coast server: RTT 81.9 ± 5.5 ms [same].
    Nr5gWestCoast,
    /// 4G to an east-coast server: RTT 58.0 ± 4.3 ms [same].
    Lte4gEastCoast,
    /// 4G to a west-coast server: RTT 88.9 ± 5.5 ms [same].
    Lte4gWestCoast,
    /// The remote-driving budget's DSRC one-hop figure, 2-3 ms [MASA living lab].
    ///
    /// Present for comparison: it is not a cellular latency, and it is what a scenario
    /// puts on the other side of the hybrid policy of [`crate::hybrid`].
    DsrcOneHop,
    /// The remote-driving budget's 5G one-hop figure, 18-20 ms [MASA living lab].
    Nr5gOneHop,
    /// The Ookla H2 2025 US carrier medians: 31 / 32 / 34 ms, taken at the midpoint 32.
    ///
    /// `todo-calibrate` on the spread: 04-models.md §10.1 marks the p95 UNVERIFIED.
    OoklaUsMedian,
    /// The Eclipse MOSAIC example defaults: about 100 ms uplink, 50 ms downlink unicast.
    ///
    /// Present so a cross-check against MOSAIC uses MOSAIC's own numbers.
    MosaicExample,
}

impl UuLatencyPreset {
    /// Every preset, in a fixed order.
    pub const ALL: [UuLatencyPreset; 10] = [
        UuLatencyPreset::LteFirstHop,
        UuLatencyPreset::Nr5gFirstHop,
        UuLatencyPreset::Nr5gEastCoast,
        UuLatencyPreset::Nr5gWestCoast,
        UuLatencyPreset::Lte4gEastCoast,
        UuLatencyPreset::Lte4gWestCoast,
        UuLatencyPreset::DsrcOneHop,
        UuLatencyPreset::Nr5gOneHop,
        UuLatencyPreset::OoklaUsMedian,
        UuLatencyPreset::MosaicExample,
    ];

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            UuLatencyPreset::LteFirstHop => "lte-first-hop",
            UuLatencyPreset::Nr5gFirstHop => "5g-first-hop",
            UuLatencyPreset::Nr5gEastCoast => "5g-east-coast",
            UuLatencyPreset::Nr5gWestCoast => "5g-west-coast",
            UuLatencyPreset::Lte4gEastCoast => "4g-east-coast",
            UuLatencyPreset::Lte4gWestCoast => "4g-west-coast",
            UuLatencyPreset::DsrcOneHop => "dsrc-one-hop",
            UuLatencyPreset::Nr5gOneHop => "5g-one-hop",
            UuLatencyPreset::OoklaUsMedian => "ookla-us-median",
            UuLatencyPreset::MosaicExample => "mosaic-example",
        }
    }

    /// The one-way latency spec for a direction.
    #[must_use]
    pub const fn one_way_ms(self, dir: Direction) -> LatencySpec {
        match self {
            UuLatencyPreset::LteFirstHop => LatencySpec::from_rtt(29.2, 4.8),
            UuLatencyPreset::Nr5gFirstHop => LatencySpec::from_rtt(27.4, 6.4),
            UuLatencyPreset::Nr5gEastCoast => LatencySpec::from_rtt(54.0, 4.5),
            UuLatencyPreset::Nr5gWestCoast => LatencySpec::from_rtt(81.9, 5.5),
            UuLatencyPreset::Lte4gEastCoast => LatencySpec::from_rtt(58.0, 4.3),
            UuLatencyPreset::Lte4gWestCoast => LatencySpec::from_rtt(88.9, 5.5),
            // Ranges, taken at the midpoint with the half-width as the spread.
            UuLatencyPreset::DsrcOneHop => LatencySpec::new(2.5, 0.5),
            UuLatencyPreset::Nr5gOneHop => LatencySpec::new(19.0, 1.0),
            // Median of the three carrier medians; the spread is todo-calibrate.
            UuLatencyPreset::OoklaUsMedian => LatencySpec::new(32.0, 5.0),
            UuLatencyPreset::MosaicExample => match dir {
                Direction::Uplink => LatencySpec::new(100.0, 0.0),
                Direction::Downlink => LatencySpec::new(50.0, 0.0),
            },
        }
    }

    /// True when both moments of the preset are printed values.
    #[must_use]
    pub const fn is_fully_cited(self) -> bool {
        !matches!(
            self,
            UuLatencyPreset::OoklaUsMedian
                | UuLatencyPreset::DsrcOneHop
                | UuLatencyPreset::Nr5gOneHop
        )
    }
}

/// Where the application server sits, which decides the transport-network latency.
///
/// The four placements and their mean / 99.99th-percentile transport-network latencies are
/// [Coll-Perales 2022 Tables IV, VI-IX, via 04-models.md §10.1]. The 99.99th-percentile
/// figures are the ones that matter: an undersized transport network (`α = 0.001`) blows
/// up to about 10.3 ms while its mean barely moves, which is the measurement the medium
/// tier exists to carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MecPlacement {
    /// MEC at the gNB: 0.402 ms mean, 0.422 ms at the 99.99th percentile.
    AtGnb,
    /// MEC at the first aggregation point (M1): 0.835 / 0.875 ms.
    AtM1,
    /// MEC at the core network, or centralized: 2.355 / 2.396 ms.
    AtCoreNetwork,
}

impl MecPlacement {
    /// Every placement, nearest first.
    pub const ALL: [MecPlacement; 3] = [
        MecPlacement::AtGnb,
        MecPlacement::AtM1,
        MecPlacement::AtCoreNetwork,
    ];

    /// The label a scenario spells.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            MecPlacement::AtGnb => "mec-at-gnb",
            MecPlacement::AtM1 => "mec-at-m1",
            MecPlacement::AtCoreNetwork => "mec-at-core",
        }
    }

    /// `(mean, 99.99th percentile)` transport-network latency, ms.
    #[must_use]
    pub const fn transport_network_ms(self) -> (f64, f64) {
        match self {
            MecPlacement::AtGnb => (0.402, 0.422),
            MecPlacement::AtM1 => (0.835, 0.875),
            MecPlacement::AtCoreNetwork => (2.355, 2.396),
        }
    }

    /// The application-server service time at this placement, ms
    /// [Coll-Perales Table IX: 0.0027-0.0031 ms at MEC, 0.035-3.295 ms centralized].
    #[must_use]
    pub const fn server_service_ms(self) -> f64 {
        match self {
            MecPlacement::AtGnb | MecPlacement::AtM1 => 0.0029,
            // The centralized range's geometric middle; the range is wide because it is
            // load dependent (2,080 to 41,600 packets/s).
            MecPlacement::AtCoreNetwork => 0.34,
        }
    }
}

/// The radio uplink-plus-downlink latency of the measured chain, ms
/// [Coll-Perales 2022 Table IV via 04-models.md §10.1]: 2.00 ms at low level of
/// automation and low load, 2.60 to 4.55 ms at high automation and load-dependent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RadioLatencyClass {
    /// 2.00 ms: low level of automation, low load.
    LowAutomationLowLoad,
    /// 2.60 ms: the bottom of the high-automation range.
    HighAutomationLowLoad,
    /// 4.55 ms: the top of it.
    HighAutomationHighLoad,
}

impl RadioLatencyClass {
    /// The uplink-plus-downlink radio latency, ms.
    #[must_use]
    pub const fn total_ms(self) -> f64 {
        match self {
            RadioLatencyClass::LowAutomationLowLoad => 2.00,
            RadioLatencyClass::HighAutomationLowLoad => 2.60,
            RadioLatencyClass::HighAutomationHighLoad => 4.55,
        }
    }
}

// =========================================================================================
// `cellular/uu/fixed-latency` (abstract)
// =========================================================================================

/// `cellular/uu/fixed-latency` — one-way latency per direction from the measured means,
/// no capacity and no coverage holes unless a plan is given (04-models.md §10.1).
#[derive(Debug, Clone)]
pub struct FixedLatencyUu {
    card: ModelCard,
    preset: UuLatencyPreset,
    plan: Option<CellPlan>,
    /// Bytes sent per `(node, direction)`, for the accounting bucket of invariant I-N1.
    bytes: BTreeMap<(u32, u8), u64>,
}

impl FixedLatencyUu {
    /// The model's id.
    pub const ID: &'static str = "cellular/uu/fixed-latency";

    /// The model with one latency preset and no coverage geometry: every point is covered.
    #[must_use]
    pub fn new(preset: UuLatencyPreset) -> Self {
        Self {
            card: fixed_latency_card(preset),
            preset,
            plan: None,
            bytes: BTreeMap::new(),
        }
    }

    /// The same model with a cell plan, so coverage is geometric.
    #[must_use]
    pub fn with_plan(mut self, plan: CellPlan) -> Self {
        self.plan = Some(plan);
        self
    }

    /// The preset in use.
    #[must_use]
    pub const fn preset(&self) -> UuLatencyPreset {
        self.preset
    }

    /// Bytes charged to one `(node, direction)` bucket.
    #[must_use]
    pub fn bytes_sent(&self, node: NodeId, dir: Direction) -> u64 {
        *self.bytes.get(&(node.index(), dir as u8)).unwrap_or(&0)
    }

    /// One latency draw, clamped at zero: the normal distribution has a left tail and a
    /// negative latency is not a physical answer.
    fn draw_latency<C: Ctx + ?Sized>(&self, ctx: &mut C, ue: NodeId, dir: Direction) -> Duration {
        let spec = self.preset.one_way_ms(dir);
        if spec.sigma_ms == 0.0 {
            return Duration::from_secs_f64(spec.mean_ms * 1e-3);
        }
        let ms = ctx
            .rng(RngDomain::Backend, EntityRef::Node(ue))
            .normal(spec.mean_ms, spec.sigma_ms)
            .max(0.0);
        Duration::from_secs_f64(ms * 1e-3)
    }
}

impl Model for FixedLatencyUu {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> CellularUu<C> for FixedLatencyUu {
    fn tier(&self) -> Tier {
        Tier::Abstract
    }

    fn coverage(&self, p: Vec3, _t: SimTime) -> Option<CellView> {
        match &self.plan {
            Some(plan) => plan.coverage(p),
            // No plan: one notional cell everywhere, which is what "no coverage holes
            // unless a coverage map is given" means (04-models.md §10.1).
            None => Some(CellView {
                cell: CellId::new(0),
                distance_m: 0.0,
                rsrp_dbm: 0.0,
                quality: CellQuality::Good,
            }),
        }
    }

    fn send(
        &mut self,
        ctx: &mut C,
        ue: NodeId,
        dir: Direction,
        bytes: u32,
        _qos: Qos,
    ) -> SendOutcome {
        let latency = self.draw_latency(ctx, ue, dir);
        *self.bytes.entry((ue.index(), dir as u8)).or_insert(0) += u64::from(bytes);
        SendOutcome::Scheduled {
            deliver_at: latency.after(ctx.now()),
            bytes_on_wire: bytes,
        }
    }

    fn handover(&mut self, _ctx: &mut C, _ue: NodeId, _from: CellId, _to: CellId) -> Duration {
        // The abstract tier ignores handover (04-models.md §10.1 "Ignores").
        Duration::ZERO
    }
}

// =========================================================================================
// `cellular/uu/cell-capacity-mm1` (medium)
// =========================================================================================

/// `cellular/uu/cell-capacity-mm1` — per-cell capacity with an M/M/1 queue per node and
/// the measured transport-network chain (04-models.md §10.1).
///
/// # The queue
///
/// One M/M/1 per node, as 04-models.md §10.1 specifies: service rate
/// `µ = capacity / mean_packet_bytes` packets per second, arrival rate `λ` estimated from
/// the node's own recent sends, and a sojourn time drawn from `Exp(µ − λ)` — the
/// exponential sojourn distribution of M/M/1, whose mean is the familiar `1/(µ − λ)`. When
/// `λ ≥ µ` the queue is unstable and the packet is dropped rather than given an infinite
/// delay, which is the only honest answer a finite simulation can give.
///
/// `λ` is measured per node over a one-second window of *this node's own* sends, with the
/// window's own arithmetic rather than an exponential average, so the estimate is a pure
/// function of the schedule.
#[derive(Debug, Clone)]
pub struct CellCapacityUu {
    card: ModelCard,
    plan: CellPlan,
    /// Aggregate uplink capacity per cell, bit/s.
    uplink_bps: f64,
    /// Aggregate downlink capacity per cell, bit/s.
    downlink_bps: f64,
    mean_packet_bytes: f64,
    radio: RadioLatencyClass,
    mec: MecPlacement,
    /// Send instants per `(node, direction)` within the arrival-rate window.
    arrivals: BTreeMap<(u32, u8), Vec<SimTime>>,
    /// Nodes served by each cell, so the per-cell capacity is shared.
    cell_load: BTreeMap<(u32, u8), Vec<u32>>,
    drops: u64,
}

/// The arrival-rate estimation window: one second.
///
/// One second because the capacity figures are per-second and the sources' own load
/// figures (2,080 to 41,600 packets/s) are per-second; a shorter window would make `λ`
/// jump on a single packet and a longer one would not follow a platoon entering a cell.
pub const ARRIVAL_WINDOW: Duration = Duration::from_secs(1);

impl CellCapacityUu {
    /// The model's id.
    pub const ID: &'static str = "cellular/uu/cell-capacity-mm1";

    /// The MOSAIC example capacity caps: 28 Mbit/s uplink and 42.2 Mbit/s downlink
    /// [Eclipse MOSAIC Cell docs via 04-models.md §10.1].
    pub const MOSAIC_UPLINK_BPS: f64 = 28e6;
    /// The downlink half of the same pair.
    pub const MOSAIC_DOWNLINK_BPS: f64 = 42.2e6;

    /// The model with a plan and the MOSAIC example caps.
    #[must_use]
    pub fn new(plan: CellPlan) -> Self {
        Self {
            card: cell_capacity_card(),
            plan,
            uplink_bps: Self::MOSAIC_UPLINK_BPS,
            downlink_bps: Self::MOSAIC_DOWNLINK_BPS,
            mean_packet_bytes: 300.0,
            radio: RadioLatencyClass::LowAutomationLowLoad,
            mec: MecPlacement::AtGnb,
            arrivals: BTreeMap::new(),
            cell_load: BTreeMap::new(),
            drops: 0,
        }
    }

    /// The model with caller-chosen capacities.
    #[must_use]
    pub fn with_capacity_bps(mut self, uplink: f64, downlink: f64) -> Self {
        self.uplink_bps = uplink;
        self.downlink_bps = downlink;
        self
    }

    /// The model with a caller-chosen radio-latency class and MEC placement.
    #[must_use]
    pub fn with_chain(mut self, radio: RadioLatencyClass, mec: MecPlacement) -> Self {
        self.radio = radio;
        self.mec = mec;
        self
    }

    /// The model with a caller-chosen mean packet size, which sets the service rate.
    #[must_use]
    pub fn with_mean_packet_bytes(mut self, bytes: f64) -> Self {
        self.mean_packet_bytes = bytes.max(1.0);
        self
    }

    /// The cell plan.
    #[must_use]
    pub const fn plan(&self) -> &CellPlan {
        &self.plan
    }

    /// Packets dropped because the queue was unstable.
    #[must_use]
    pub const fn drops(&self) -> u64 {
        self.drops
    }

    /// The fixed part of the one-way latency: the radio leg plus the transport network
    /// plus the application server.
    ///
    /// The radio figure is uplink *plus* downlink, so one direction takes half of it,
    /// which is the same "one way modeled as half" rule 04-models.md §10.1 applies to the
    /// round-trip measurements.
    #[must_use]
    pub fn fixed_latency_ms(&self) -> f64 {
        let (tn_mean, _) = self.mec.transport_network_ms();
        self.radio.total_ms() / 2.0 + tn_mean + self.mec.server_service_ms()
    }

    /// The 99.99th-percentile fixed latency, which is the figure the requirement anchors
    /// are stated against (high automation: 10 ms at 99.99 %).
    #[must_use]
    pub fn fixed_latency_p9999_ms(&self) -> f64 {
        let (_, tn_p) = self.mec.transport_network_ms();
        self.radio.total_ms() / 2.0 + tn_p + self.mec.server_service_ms()
    }

    /// The service rate in packets per second for one direction.
    #[must_use]
    pub fn service_rate(&self, dir: Direction) -> f64 {
        let bps = match dir {
            Direction::Uplink => self.uplink_bps,
            Direction::Downlink => self.downlink_bps,
        };
        bps / (8.0 * self.mean_packet_bytes)
    }

    /// The arrival rate a node is offering in one direction, packets per second, over
    /// [`ARRIVAL_WINDOW`].
    #[must_use]
    pub fn arrival_rate(&self, ue: NodeId, dir: Direction, now: SimTime) -> f64 {
        let from = now.saturating_sub(ARRIVAL_WINDOW.as_nanos());
        let n = self
            .arrivals
            .get(&(ue.index(), dir as u8))
            .map_or(0, |v| v.iter().filter(|t| **t >= from).count());
        n as f64 / ARRIVAL_WINDOW.as_secs_f64()
    }

    /// How many nodes are sharing the cell serving this node, from the sends seen in the
    /// window. At least one: the node itself.
    #[must_use]
    pub fn cell_users(&self, cell: CellId, dir: Direction) -> usize {
        self.cell_load
            .get(&(cell.index(), dir as u8))
            .map_or(1, |v| v.len().max(1))
    }
}

impl Model for CellCapacityUu {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> CellularUu<C> for CellCapacityUu {
    fn tier(&self) -> Tier {
        Tier::Medium
    }

    fn coverage(&self, p: Vec3, _t: SimTime) -> Option<CellView> {
        self.plan.coverage(p)
    }

    fn send(
        &mut self,
        ctx: &mut C,
        ue: NodeId,
        dir: Direction,
        bytes: u32,
        qos: Qos,
    ) -> SendOutcome {
        let now = ctx.now();
        let key = (ue.index(), dir as u8);
        // Record the arrival before estimating, so a node's first packet already counts
        // towards its own load: an estimator that excluded the packet being served would
        // report λ = 0 for a node sending one packet per window and never queue anything.
        let window_from = now.saturating_sub(ARRIVAL_WINDOW.as_nanos());
        let slot = self.arrivals.entry(key).or_default();
        slot.retain(|t| *t >= window_from);
        slot.push(now);
        let lambda = slot.len() as f64 / ARRIVAL_WINDOW.as_secs_f64();

        let mu = self.service_rate(dir);
        if lambda >= mu {
            self.drops += 1;
            return SendOutcome::Dropped(DropCause::QueueFull {
                ac: "cellular",
                depth: 0,
            });
        }
        // M/M/1 sojourn time: exponential with rate µ − λ, mean 1/(µ − λ). A
        // latency-sensitive packet is served ahead of the queue and pays only the
        // transmission time, which is the one thing `Qos` can buy at this tier.
        let queue_s = if qos == Qos::LowLatency {
            f64::from(bytes) * 8.0
                / match dir {
                    Direction::Uplink => self.uplink_bps,
                    Direction::Downlink => self.downlink_bps,
                }
        } else {
            ctx.rng(RngDomain::ServiceTime, EntityRef::Node(ue))
                .exponential(mu - lambda)
        };
        let total_s = queue_s + self.fixed_latency_ms() * 1e-3;
        SendOutcome::Scheduled {
            deliver_at: Duration::from_secs_f64(total_s).after(now),
            bytes_on_wire: bytes,
        }
    }

    fn handover(&mut self, _ctx: &mut C, _ue: NodeId, _from: CellId, _to: CellId) -> Duration {
        // The medium tier ignores handover interruption (04-models.md §10.1 "Ignores").
        Duration::ZERO
    }
}

// =========================================================================================
// `cellular/uu/handover-outage` (high)
// =========================================================================================

/// The handover interruption components of TS 36.133, milliseconds
/// [TS 36.133 §5.1.2.1.2, §5.3.1.1.2, via 04-models.md §10.1].
pub mod handover {
    /// `T_IU`, the interruption uncertainty: the value the standard's own worked
    /// interruption uses, ms.
    ///
    /// 04-models.md §10.1 writes the known-target interruption as `T_IU + 20 ms` without
    /// printing `T_IU` itself, so this is `todo-calibrate`: the plan is to read
    /// TS 36.133 §5.1.2.1.2's definition of `T_IU` and record the value it fixes.
    pub const T_IU_MS: f64 = 5.0;
    /// The known-target addition, ms.
    pub const KNOWN_TARGET_MS: f64 = 20.0;
    /// `T_search`, the extra cost of an unknown target, ms.
    pub const T_SEARCH_MS: f64 = 80.0;
    /// The RRC procedure delay, ms.
    pub const RRC_PROCEDURE_MS: f64 = 50.0;
    /// The E-UTRA to UTRA addition for a known target, ms, before the `10·F_max` term.
    pub const EUTRA_TO_UTRA_KNOWN_MS: f64 = 50.0;
    /// The E-UTRA to UTRA addition for an unknown target, ms.
    pub const EUTRA_TO_UTRA_UNKNOWN_MS: f64 = 150.0;
}

/// Which handover a scenario is modelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HandoverKind {
    /// LTE intra- or inter-frequency, target already known to the UE.
    IntraLteKnown,
    /// The same, target not known: `T_search` applies.
    IntraLteUnknown,
    /// E-UTRA to UTRA, target known.
    EutraToUtraKnown,
    /// E-UTRA to UTRA, target unknown.
    EutraToUtraUnknown,
}

impl HandoverKind {
    /// The interruption, ms.
    ///
    /// `f_max` is the `F_max` of the E-UTRA-to-UTRA formula
    /// `T_IU + T_sync + 50 + 10·F_max`; it is ignored by the intra-LTE kinds.
    #[must_use]
    pub fn interruption_ms(self, f_max: f64) -> f64 {
        use handover as h;
        match self {
            HandoverKind::IntraLteKnown => h::T_IU_MS + h::KNOWN_TARGET_MS + h::RRC_PROCEDURE_MS,
            HandoverKind::IntraLteUnknown => {
                h::T_IU_MS + h::KNOWN_TARGET_MS + h::T_SEARCH_MS + h::RRC_PROCEDURE_MS
            }
            // T_sync is not printed in 04-models.md §10.1 either; it is folded into T_IU
            // in the card's todo-calibrate note.
            HandoverKind::EutraToUtraKnown => h::T_IU_MS + h::EUTRA_TO_UTRA_KNOWN_MS + 10.0 * f_max,
            HandoverKind::EutraToUtraUnknown => h::T_IU_MS + h::EUTRA_TO_UTRA_UNKNOWN_MS,
        }
    }
}

/// `cellular/uu/handover-outage` — the medium tier plus handover interruption, per-packet
/// loss at the measured percentiles, and outage (04-models.md §10.1).
#[derive(Debug, Clone)]
pub struct HandoverOutageUu {
    card: ModelCard,
    inner: CellCapacityUu,
    kind: HandoverKind,
    f_max: f64,
    /// Per-packet loss probability, from the measured percentiles.
    loss_probability: f64,
    /// When each UE's handover interruption ends; sends before it are lost.
    interrupted_until: BTreeMap<u32, SimTime>,
    /// The cell each UE was last served by, so the model can notice a handover.
    serving: BTreeMap<u32, u32>,
    handovers: u64,
    lost_to_handover: u64,
    lost_to_loss_draw: u64,
}

/// The measured 5G mmWave stationary-LOS packet-loss percentiles, as fractions
/// [Narayanan et al. WWW'20, via 04-models.md §10.1]: 50th / 75th / 99th =
/// 0.01 / 0.1 / 1.2 %.
pub const LOSS_PERCENTILES: [(f64, f64); 3] = [(0.50, 1e-4), (0.75, 1e-3), (0.99, 1.2e-2)];

/// The loss probability at a percentile of the measured distribution, interpolated
/// linearly in the percentile.
#[must_use]
pub fn loss_at_percentile(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    if p <= LOSS_PERCENTILES[0].0 {
        return LOSS_PERCENTILES[0].1;
    }
    for w in LOSS_PERCENTILES.windows(2) {
        if p <= w[1].0 {
            let t = (p - w[0].0) / (w[1].0 - w[0].0);
            return w[0].1 + t * (w[1].1 - w[0].1);
        }
    }
    LOSS_PERCENTILES[2].1
}

impl HandoverOutageUu {
    /// The model's id.
    pub const ID: &'static str = "cellular/uu/handover-outage";

    /// The model over a capacity model, at the median measured loss.
    #[must_use]
    pub fn new(inner: CellCapacityUu) -> Self {
        Self {
            card: handover_outage_card(),
            inner,
            kind: HandoverKind::IntraLteKnown,
            f_max: 0.0,
            loss_probability: loss_at_percentile(0.5),
            interrupted_until: BTreeMap::new(),
            serving: BTreeMap::new(),
            handovers: 0,
            lost_to_handover: 0,
            lost_to_loss_draw: 0,
        }
    }

    /// The model with a caller-chosen handover kind.
    #[must_use]
    pub fn with_handover(mut self, kind: HandoverKind, f_max: f64) -> Self {
        self.kind = kind;
        self.f_max = f_max;
        self
    }

    /// The model at a caller-chosen percentile of the measured loss distribution.
    #[must_use]
    pub fn at_loss_percentile(mut self, p: f64) -> Self {
        self.loss_probability = loss_at_percentile(p);
        self
    }

    /// Handovers performed.
    #[must_use]
    pub const fn handovers(&self) -> u64 {
        self.handovers
    }

    /// Packets lost inside a handover interruption.
    #[must_use]
    pub const fn lost_to_handover(&self) -> u64 {
        self.lost_to_handover
    }

    /// Packets lost to the per-packet loss draw.
    #[must_use]
    pub const fn lost_to_loss_draw(&self) -> u64 {
        self.lost_to_loss_draw
    }

    /// The per-packet loss probability in use.
    #[must_use]
    pub const fn loss_probability(&self) -> f64 {
        self.loss_probability
    }

    /// The capacity model underneath.
    #[must_use]
    pub const fn inner(&self) -> &CellCapacityUu {
        &self.inner
    }

    /// True when this UE is inside a handover interruption at `t`.
    #[must_use]
    pub fn is_interrupted(&self, ue: NodeId, t: SimTime) -> bool {
        self.interrupted_until
            .get(&ue.index())
            .is_some_and(|until| *until > t)
    }

    /// Notices that a UE has moved to a new cell and starts the interruption if so.
    ///
    /// Returns the interruption when a handover happened. This is how the `high` tier
    /// couples coverage geometry to interruption without the caller having to detect the
    /// cell change itself.
    pub fn observe_position<C: Ctx + ?Sized>(
        &mut self,
        ctx: &mut C,
        ue: NodeId,
        p: Vec3,
    ) -> Option<Duration> {
        let view = self.inner.plan().coverage(p)?;
        let previous = self.serving.insert(ue.index(), view.cell.index());
        match previous {
            Some(old) if old != view.cell.index() => {
                let d = self.handover(ctx, ue, CellId::new(old), view.cell);
                Some(d)
            }
            _ => None,
        }
    }
}

impl Model for HandoverOutageUu {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> CellularUu<C> for HandoverOutageUu {
    fn tier(&self) -> Tier {
        Tier::High
    }

    fn coverage(&self, p: Vec3, t: SimTime) -> Option<CellView> {
        CellularUu::<C>::coverage(&self.inner, p, t)
    }

    fn send(
        &mut self,
        ctx: &mut C,
        ue: NodeId,
        dir: Direction,
        bytes: u32,
        qos: Qos,
    ) -> SendOutcome {
        let now = ctx.now();
        // A handover interruption is "seamless, not lossless" — the ns-3 5G-LENA note
        // 04-models.md §10.1 records — so a packet offered inside it is lost.
        if self.is_interrupted(ue, now) {
            self.lost_to_handover += 1;
            return SendOutcome::Dropped(DropCause::Dcc);
        }
        // The per-packet loss draw, from the Backend domain keyed by the node: one stream
        // per UE, so a UE's loss pattern does not depend on how many other UEs sent first.
        let lost = ctx
            .rng(RngDomain::Backend, EntityRef::Node(ue))
            .bool(self.loss_probability);
        if lost {
            if qos == Qos::Reliable {
                // One retry, which is what Reliable buys; the retry is not drawn again,
                // so the effective loss is the square of the single-shot probability and
                // the card says so.
            } else {
                self.lost_to_loss_draw += 1;
                return SendOutcome::Dropped(DropCause::Dcc);
            }
        }
        self.inner.send(ctx, ue, dir, bytes, qos)
    }

    fn handover(&mut self, ctx: &mut C, ue: NodeId, _from: CellId, _to: CellId) -> Duration {
        let d = Duration::from_secs_f64(self.kind.interruption_ms(self.f_max) * 1e-3);
        self.handovers += 1;
        self.interrupted_until
            .insert(ue.index(), d.after(ctx.now()));
        d
    }
}

// =========================================================================================
// Store and forward
// =========================================================================================

/// The buffer a caller keeps when [`SendOutcome::NoCoverage`] comes back
/// (03-interfaces.md §5: "NoCoverage → caller stores & forwards").
///
/// It is here rather than in the caller because every caller needs the same three rules,
/// and getting any of them wrong is a silent measurement error: the buffer is bounded, the
/// oldest entry is dropped when it overflows, and an entry older than the maximum age is
/// discarded rather than delivered stale. All three are counted, so a run can say how much
/// of its traffic the coverage holes ate.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreAndForward {
    capacity: usize,
    max_age: Duration,
    /// `(stored_at, bytes, direction, qos)` oldest first.
    buffered: BTreeMap<u32, Vec<(SimTime, u32, Direction, Qos)>>,
    overflowed: u64,
    expired: u64,
    forwarded: u64,
}

impl StoreAndForward {
    /// A buffer of `capacity` entries per node, discarding entries older than `max_age`.
    #[must_use]
    pub fn new(capacity: usize, max_age: Duration) -> Self {
        Self {
            capacity: capacity.max(1),
            max_age,
            buffered: BTreeMap::new(),
            overflowed: 0,
            expired: 0,
            forwarded: 0,
        }
    }

    /// Stores one packet for a node.
    pub fn store(&mut self, node: NodeId, at: SimTime, bytes: u32, dir: Direction, qos: Qos) {
        let cap = self.capacity;
        let q = self.buffered.entry(node.index()).or_default();
        q.push((at, bytes, dir, qos));
        while q.len() > cap {
            q.remove(0);
            self.overflowed += 1;
        }
    }

    /// How many packets a node has buffered.
    #[must_use]
    pub fn depth(&self, node: NodeId) -> usize {
        self.buffered.get(&node.index()).map_or(0, Vec::len)
    }

    /// Drains a node's buffer through a Uu model, dropping what has aged out.
    ///
    /// Returns the outcomes in the order the packets were stored, so the caller can
    /// schedule the deliveries; entries older than `max_age` are counted in
    /// [`StoreAndForward::expired`] and not offered.
    pub fn forward<C: Ctx + ?Sized, U: CellularUu<C> + ?Sized>(
        &mut self,
        ctx: &mut C,
        uu: &mut U,
        node: NodeId,
    ) -> Vec<SendOutcome> {
        let now = ctx.now();
        let Some(mut q) = self.buffered.remove(&node.index()) else {
            return Vec::new();
        };
        let before = q.len();
        q.retain(|(at, ..)| Duration::between(*at, now) <= self.max_age);
        self.expired += (before - q.len()) as u64;
        let mut out = Vec::with_capacity(q.len());
        let mut requeue = Vec::new();
        for (at, bytes, dir, qos) in q {
            let outcome = uu.send(ctx, node, dir, bytes, qos);
            if matches!(outcome, SendOutcome::NoCoverage) {
                // Still no coverage: keep it, do not count it forwarded.
                requeue.push((at, bytes, dir, qos));
            } else {
                self.forwarded += 1;
            }
            out.push(outcome);
        }
        if !requeue.is_empty() {
            self.buffered.insert(node.index(), requeue);
        }
        out
    }

    /// Packets dropped because the buffer was full.
    #[must_use]
    pub const fn overflowed(&self) -> u64 {
        self.overflowed
    }

    /// Packets dropped because they aged out.
    #[must_use]
    pub const fn expired(&self) -> u64 {
        self.expired
    }

    /// Packets that eventually went out.
    #[must_use]
    pub const fn forwarded(&self) -> u64 {
        self.forwarded
    }
}

// =========================================================================================
// Cards
// =========================================================================================

fn narayanan() -> Source {
    Source::new(
        SourceKind::Paper,
        "Narayanan et al., WWW'20 Table 2, via 04-models.md §10.1: LTE first-hop RTT \
         29.2 ± 4.8 ms, 5G mmWave 27.4 ± 6.4 ms, total RTT to an east/west-coast server \
         5G 54.0 ± 4.5 / 81.9 ± 5.5 ms and 4G 58.0 ± 4.3 / 88.9 ± 5.5 ms; packet-loss \
         percentiles 0.01 / 0.1 / 1.2 %",
    )
}

fn coll_perales() -> Source {
    Source::new(
        SourceKind::Paper,
        "Coll-Perales et al. 2022 Tables III, IV, VI-IX, via 04-models.md §10.1: radio \
         UL+DL 2.00 ms (low LoA, low load) to 2.60-4.55 ms; transport network mean / \
         99.99th 0.402/0.422, 0.835/0.875, 2.355/2.396 ms by MEC placement; application \
         server 0.0027-0.0031 ms at MEC, 0.035-3.295 ms centralized; requirement anchors \
         25 ms at 90 % (low LoA) and 10 ms at 99.99 % (high LoA)",
    )
}

fn fixed_latency_card(preset: UuLatencyPreset) -> ModelCard {
    let mut card = ModelCard::new(
        FixedLatencyUu::ID,
        Family::Cellular,
        "1.0.0",
        "One-way cellular latency per direction, drawn from a measured mean and spread.",
    );
    card.tier = vec![Tier::Abstract];
    card.equations = vec![Equation {
        name: "one-way latency".to_string(),
        latex_or_text: "L ~ max(0, N(mean, sigma)), with mean and sigma half the measured \
                        round trip"
            .to_string(),
        notes: Some(
            "04-models.md §10.1 says \"one way modeled as half\"; halving the spread as \
             well as the mean keeps the modelled jitter the measurement's rather than \
             twice it. The draw is clamped at zero because a normal distribution has a \
             left tail and a negative latency is not an answer."
                .to_string(),
        ),
    }];
    let spec = preset.one_way_ms(Direction::Uplink);
    let source = if preset.is_fully_cited() {
        narayanan()
    } else {
        Source {
            kind: SourceKind::TodoCalibrate,
            reference: match preset {
                UuLatencyPreset::OoklaUsMedian => {
                    "Ookla via the IEEE ComSoc blog, H2 2025 US carrier medians 31 / 32 / \
                     34 ms; 04-models.md §10.1 marks the p95 UNVERIFIED, so the spread is \
                     chosen"
                        .to_string()
                }
                _ => "a printed range taken at its midpoint with the half-width as the \
                      spread (MASA living lab: DSRC 2-3 ms, 5G 18-20 ms one hop)"
                    .to_string(),
            },
            accessed: None,
            note: None,
        }
    };
    card.parameters = vec![
        Parameter {
            name: "preset".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(preset.label()),
            range: Some(
                UuLatencyPreset::ALL
                    .iter()
                    .map(|p| serde_json::json!(p.label()))
                    .collect(),
            ),
            source: source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some(
                    "Obtain the percentile distribution behind the quoted median (Ookla's \
                     own report, or a drive-test capture) and replace the chosen spread."
                        .to_string(),
                )
            },
        },
        Parameter {
            name: "mean_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(spec.mean_ms),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1000.0)]),
            source: source.clone(),
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`.".to_string())
            },
        },
        Parameter {
            name: "sigma_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(spec.sigma_ms),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1000.0)]),
            source,
            calibration: if preset.is_fully_cited() {
                None
            } else {
                Some("As for `preset`.".to_string())
            },
        },
    ];
    card.ignores = vec![
        "Capacity, coverage geometry and handover (04-models.md §10.1 \"Ignores\", \
         abstract row)."
            .to_string(),
    ];
    card.sources = vec![narayanan()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §13 row \"Cellular Uu\": first-hop RTT 29.2 ± 4.8 ms (LTE), \
             27.4 ± 6.4 ms (5G), ±10 %",
        )],
        tests: vec!["the_latency_presets_reproduce_the_printed_round_trips".to_string()],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::Backend.as_str().to_string()],
    };
    card
}

fn cell_capacity_card() -> ModelCard {
    let mut card = ModelCard::new(
        CellCapacityUu::ID,
        Family::Cellular,
        "1.0.0",
        "Per-cell uplink and downlink capacity with an M/M/1 queue per node and the \
         measured transport-network chain.",
    );
    card.tier = vec![Tier::Medium];
    card.equations = vec![
        Equation {
            name: "M/M/1 sojourn time".to_string(),
            latex_or_text: "W ~ Exp(µ − λ), µ = C / (8·mean_packet_bytes), λ from the \
                            node's sends in the last second; λ >= µ drops the packet"
                .to_string(),
            notes: Some(
                "An unstable queue has an infinite expected delay, which no finite run \
                 can schedule, so the packet is dropped and counted instead."
                    .to_string(),
            ),
        },
        Equation {
            name: "fixed latency chain".to_string(),
            latex_or_text: "L_fixed = radio_UL+DL / 2 + transport_network + server".to_string(),
            notes: Some(
                "The measured radio figure is uplink plus downlink, so one direction \
                 takes half of it."
                    .to_string(),
            ),
        },
        Equation {
            name: "Uu path loss".to_string(),
            latex_or_text: "PL = 128.1 + 37.6·log10(R_km)".to_string(),
            notes: Some("TR 36.885 Table A.1.4-2, via 04-models.md §3.3.".to_string()),
        },
    ];
    card.parameters = vec![
        Parameter::new(
            "uplink_bps",
            "bit/s",
            serde_json::json!(CellCapacityUu::MOSAIC_UPLINK_BPS),
            Source::new(
                SourceKind::Code,
                "Eclipse MOSAIC Cell example caps, 28 Mbit/s UL and 42.2 Mbit/s DL, via \
                 04-models.md §10.1; the alternative is the TR 37.885 Table 6.1.1-1 \
                 aggregated bandwidth of up to 200 MHz DL+UL below 6 GHz",
            ),
        ),
        Parameter::new(
            "downlink_bps",
            "bit/s",
            serde_json::json!(CellCapacityUu::MOSAIC_DOWNLINK_BPS),
            Source::new(SourceKind::Code, "as `uplink_bps`"),
        ),
        Parameter::new(
            "radio_latency_ms",
            "ms",
            serde_json::json!(RadioLatencyClass::LowAutomationLowLoad.total_ms()),
            coll_perales(),
        ),
        Parameter::new(
            "mec_placement",
            "-",
            serde_json::json!(MecPlacement::AtGnb.label()),
            coll_perales(),
        ),
        Parameter::new(
            "isd_m",
            "m",
            serde_json::json!(CellPlan::urban_macro().isd_m),
            Source::new(
                SourceKind::Standard,
                "TR 37.885 and TR 38.913 via 04-models.md §5.3, §10.1: ISD 500 m urban \
                 macro, 1,732 m highway (500 m optional); macro Tx 49 dBm below 6 GHz; BS \
                 noise figure 5 dB",
            ),
        ),
        Parameter {
            name: "no_coverage_below_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(CellPlan::urban_macro().no_coverage_below_dbm),
            range: Some(vec![serde_json::json!(-140.0), serde_json::json!(-80.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "04-models.md §10.1 prints no coverage floor; it records only \
                            that coverage PDR is near 100 % with localized drops to about \
                            90 % in dense canyons and intersections (MASA)"
                    .to_string(),
                accessed: None,
                note: None,
            },
            calibration: Some(
                "Take the lowest RSRP at which the measured drive tests still carried \
                 traffic, or the UE's minimum reference-signal received power from \
                 TS 36.133 §9.1, and record it."
                    .to_string(),
            ),
        },
        Parameter::new(
            "mean_packet_bytes",
            "B",
            serde_json::json!(300.0),
            Source::new(
                SourceKind::Standard,
                "TR 37.885 §6.1.5 Traffic Model 1: sizes {300, 190, 190, 190, 190} B",
            ),
        ),
    ];
    card.assumptions = vec![
        "The cell layout is a square lattice at the inter-site distance, which is the \
         only number the sources fix; a hexagonal layout would change which cell serves a \
         point near a boundary but not the distance distribution's scale."
            .to_string(),
        "`λ` is the sending node's own offered rate, so the queue is per node as \
         04-models.md §10.1 specifies; Jackson's theorem for multi-node transit is not \
         composed here because the chain's fixed latency already carries the measured \
         per-hop figures."
            .to_string(),
    ];
    card.ignores = vec![
        "Handover interruption, radio-link failure, and per-packet loss correlation \
         (04-models.md §10.1 \"Ignores\", medium row)."
            .to_string(),
    ];
    card.sources = vec![coll_perales(), narayanan()];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §13 row \"Cellular Uu\": transport-network 99.99th percentiles \
             0.422 / 0.875 / 2.396 ms by placement, ±10 %",
        )],
        tests: vec![
            "the_transport_network_percentiles_are_the_measured_ones".to_string(),
            "an_unstable_queue_drops_rather_than_waiting_for_ever".to_string(),
            "the_requirement_anchors_are_met_at_the_mec".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::ServiceTime.as_str().to_string()],
    };
    card
}

fn handover_outage_card() -> ModelCard {
    let mut card = ModelCard::new(
        HandoverOutageUu::ID,
        Family::Cellular,
        "1.0.0",
        "The capacity model plus handover interruption, per-packet loss at the measured \
         percentiles, and outage beyond coverage.",
    );
    card.tier = vec![Tier::High];
    card.equations = vec![Equation {
        name: "handover interruption".to_string(),
        latex_or_text: "known target: T_IU + 20 ms (+ 50 ms RRC procedure); unknown \
                        target: + T_search 80 ms; E-UTRA to UTRA: T_IU + T_sync + 50 + \
                        10·F_max (known) or + 150 ms (unknown)"
            .to_string(),
        notes: Some(
            "TS 36.133 §5.1.2.1.2 and §5.3.1.1.2 via 04-models.md §10.1. A handover is \
             seamless, not lossless (the ns-3 5G-LENA note the same section records), so \
             a packet offered inside the interruption is lost."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        Parameter {
            name: "t_iu_ms".to_string(),
            unit: "ms".to_string(),
            default: serde_json::json!(handover::T_IU_MS),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(50.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "04-models.md §10.1 writes the interruption as `T_IU + 20 ms` \
                            without printing `T_IU`, and folds `T_sync` into the same gap"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Every other term of the interruption is printed; this is the one that \
                     is not, so an interruption is right to within this term."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Read TS 36.133 §5.1.2.1.2's definition of `T_IU` and §5.3.1.1.2's \
                 `T_sync`, and record both."
                    .to_string(),
            ),
        },
        Parameter::new(
            "t_search_ms",
            "ms",
            serde_json::json!(handover::T_SEARCH_MS),
            Source::new(
                SourceKind::Standard,
                "TS 36.133 §5.1.2.1.2 via 04-models.md §10.1: unknown target adds \
                 T_search 80 ms; RRC procedure delay +50 ms",
            ),
        ),
        Parameter::new(
            "loss_probability",
            "-",
            serde_json::json!(loss_at_percentile(0.5)),
            narayanan(),
        ),
    ];
    card.assumptions = vec![
        "`Qos::Reliable` buys exactly one retry and the retry is not drawn again, so its \
         effective loss is the single-shot probability squared."
            .to_string(),
        "A handover is detected from the serving cell changing under \
         `observe_position`, so a scenario that never calls it sees no handovers."
            .to_string(),
    ];
    card.ignores = vec![
        "Scheduler detail, HARQ on Uu, beam management; there is no LENA-level PHY here \
         and ns-3 5G-LENA remains the external cross-check (04-models.md §10.1 \
         \"Ignores\", high row; ADR 0006)."
            .to_string(),
        "Handover-failure recovery and gNB-side radio-link failure, which 5G-LENA does \
         not implement either."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "TS 36.133 §5.1.2.1.2, §5.3.1.1.2 via 04-models.md §10.1",
        ),
        narayanan(),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §10.1: 31 primitive handoffs and 13 4G-5G bounces in an \
             8-minute urban walk; coverage PDR near 100 % with localized drops to about \
             90 %",
        )],
        tests: vec![
            "a_handover_loses_the_packets_offered_inside_its_interruption".to_string(),
            "the_interruption_components_add_up_to_the_printed_totals".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![
            RngDomain::Backend.as_str().to_string(),
            RngDomain::ServiceTime.as_str().to_string(),
        ],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;

    #[test]
    fn the_latency_presets_reproduce_the_printed_round_trips() {
        // 04-models.md §13 row "Cellular Uu", ±10 %: first-hop RTT 29.2 ± 4.8 ms (LTE),
        // 27.4 ± 6.4 ms (5G). One way is half of each.
        for (preset, rtt_mean, rtt_sigma) in [
            (UuLatencyPreset::LteFirstHop, 29.2, 4.8),
            (UuLatencyPreset::Nr5gFirstHop, 27.4, 6.4),
            (UuLatencyPreset::Nr5gEastCoast, 54.0, 4.5),
            (UuLatencyPreset::Nr5gWestCoast, 81.9, 5.5),
            (UuLatencyPreset::Lte4gEastCoast, 58.0, 4.3),
            (UuLatencyPreset::Lte4gWestCoast, 88.9, 5.5),
        ] {
            let s = preset.one_way_ms(Direction::Uplink);
            assert!(
                (s.mean_ms * 2.0 - rtt_mean).abs() < 1e-9,
                "{}",
                preset.label()
            );
            assert!(
                (s.sigma_ms * 2.0 - rtt_sigma).abs() < 1e-9,
                "{}",
                preset.label()
            );
            // 5G beats 4G to the same coast, which is the ordering the measurement has.
        }
        assert!(
            UuLatencyPreset::Nr5gEastCoast
                .one_way_ms(Direction::Uplink)
                .mean_ms
                < UuLatencyPreset::Lte4gEastCoast
                    .one_way_ms(Direction::Uplink)
                    .mean_ms
        );
        // MOSAIC's example is asymmetric, and the asymmetry is the point.
        let ul = UuLatencyPreset::MosaicExample.one_way_ms(Direction::Uplink);
        let dl = UuLatencyPreset::MosaicExample.one_way_ms(Direction::Downlink);
        assert!((ul.mean_ms - 100.0).abs() < 1e-9 && (dl.mean_ms - 50.0).abs() < 1e-9);
    }

    #[test]
    fn the_drawn_latency_has_the_preset_mean() {
        let mut uu = FixedLatencyUu::new(UuLatencyPreset::LteFirstHop);
        let mut ctx = TestCtx::new(1);
        let mut total = 0.0;
        let n: u32 = 2000;
        for i in 0..n {
            ctx.set_now(u64::from(i) * 1_000_000);
            match uu.send(
                &mut ctx,
                NodeId::new(1),
                Direction::Uplink,
                300,
                Qos::BestEffort,
            ) {
                SendOutcome::Scheduled { deliver_at, .. } => {
                    total += (deliver_at - ctx.now()) as f64 * 1e-6;
                }
                other => panic!("expected a schedule, got {other:?}"),
            }
        }
        let mean = total / f64::from(n);
        let want = UuLatencyPreset::LteFirstHop
            .one_way_ms(Direction::Uplink)
            .mean_ms;
        assert!(
            (mean - want).abs() < 0.3,
            "mean drawn latency {mean:.3} ms against {want:.3} ms"
        );
        assert_eq!(
            uu.bytes_sent(NodeId::new(1), Direction::Uplink),
            300 * u64::from(n)
        );
        assert_eq!(uu.bytes_sent(NodeId::new(1), Direction::Downlink), 0);
    }

    #[test]
    fn the_transport_network_percentiles_are_the_measured_ones() {
        // 04-models.md §13: 0.422 / 0.875 / 2.396 ms at the 99.99th percentile.
        for (placement, mean, p) in [
            (MecPlacement::AtGnb, 0.402, 0.422),
            (MecPlacement::AtM1, 0.835, 0.875),
            (MecPlacement::AtCoreNetwork, 2.355, 2.396),
        ] {
            assert_eq!(placement.transport_network_ms(), (mean, p));
        }
        // Nearer is faster, at both moments.
        let mut prev = (0.0, 0.0);
        for m in MecPlacement::ALL {
            let (mean, p) = m.transport_network_ms();
            assert!(mean > prev.0 && p > prev.1);
            assert!(p > mean, "the percentile must exceed the mean");
            prev = (mean, p);
        }
    }

    #[test]
    fn the_requirement_anchors_are_met_at_the_mec() {
        // Coll-Perales Table III: low level of automation 25 ms at 90 %, high 10 ms at
        // 99.99 %. The fixed chain at the gNB must leave room for both.
        let uu = CellCapacityUu::new(CellPlan::urban_macro()).with_chain(
            RadioLatencyClass::HighAutomationHighLoad,
            MecPlacement::AtGnb,
        );
        let p = uu.fixed_latency_p9999_ms();
        assert!(
            p < 10.0,
            "the fixed chain at the gNB is {p:.3} ms at the 99.99th percentile, which \
             already breaks the 10 ms high-automation anchor"
        );
        // Centralized, with the high-load radio leg, is where it gets tight.
        let central = CellCapacityUu::new(CellPlan::urban_macro()).with_chain(
            RadioLatencyClass::HighAutomationHighLoad,
            MecPlacement::AtCoreNetwork,
        );
        assert!(central.fixed_latency_p9999_ms() > p);
    }

    #[test]
    fn an_unstable_queue_drops_rather_than_waiting_for_ever() {
        // A tiny capacity makes µ small; offering more than that must drop.
        let mut uu = CellCapacityUu::new(CellPlan::urban_macro())
            .with_capacity_bps(24_000.0, 24_000.0)
            .with_mean_packet_bytes(300.0);
        // µ = 24,000 / 2,400 = 10 packets/s.
        assert!((uu.service_rate(Direction::Uplink) - 10.0).abs() < 1e-9);
        let mut ctx = TestCtx::new(2);
        let node = NodeId::new(1);
        let mut dropped = 0;
        for i in 0..20u64 {
            ctx.set_now(i * 10_000_000);
            if matches!(
                uu.send(&mut ctx, node, Direction::Uplink, 300, Qos::BestEffort),
                SendOutcome::Dropped(_)
            ) {
                dropped += 1;
            }
        }
        assert!(
            dropped > 0,
            "offering 100 packets/s into a 10 packets/s cell must drop"
        );
        assert_eq!(uu.drops(), dropped);
    }

    #[test]
    fn a_low_latency_packet_skips_the_queue() {
        let mut uu = CellCapacityUu::new(CellPlan::urban_macro())
            .with_capacity_bps(1e6, 1e6)
            .with_mean_packet_bytes(300.0);
        let mut ctx = TestCtx::new(3);
        // Load the node up so the best-effort queue is not empty.
        for i in 0..40u64 {
            ctx.set_now(i * 1_000_000);
            let _ = uu.send(
                &mut ctx,
                NodeId::new(1),
                Direction::Uplink,
                300,
                Qos::BestEffort,
            );
        }
        ctx.set_now(100_000_000);
        let fast = uu
            .send(
                &mut ctx,
                NodeId::new(1),
                Direction::Uplink,
                300,
                Qos::LowLatency,
            )
            .deliver_at()
            .expect("scheduled");
        let fast_ms = (fast - ctx.now()) as f64 * 1e-6;
        // 300 B at 1 Mbit/s is 2.4 ms of transmission, plus the fixed chain.
        assert!(
            (fast_ms - (2.4 + uu.fixed_latency_ms())).abs() < 1e-6,
            "a low-latency packet paid {fast_ms:.4} ms"
        );
    }

    #[test]
    fn coverage_follows_the_inter_site_distance_and_runs_out_beyond_the_lattice() {
        let urban = CellPlan::urban_macro();
        let at_site = urban
            .coverage(Vec3::new(0.0, 0.0, 1.5))
            .expect("the site itself is covered");
        assert_eq!(at_site.quality, CellQuality::Good);
        // A quarter of the ISD out is Fair; Edge needs half the ISD, which on a square
        // lattice only the diagonal reaches — along an axis the nearest site is never
        // more than ISD/2 away and the rounding takes the point to the next site first.
        let fair = urban
            .coverage(Vec3::new(150.0, 0.0, 1.5))
            .expect("covered")
            .quality;
        let edge = urban
            .coverage(Vec3::new(240.0, 240.0, 1.5))
            .expect("covered")
            .quality;
        assert_eq!(fair, CellQuality::Fair);
        assert_eq!(edge, CellQuality::Edge);
        // The worst case the layout produces is the lattice corner, ISD·sqrt(2)/2.
        let corner = urban
            .coverage(Vec3::new(250.0, 250.0, 1.5))
            .expect("covered");
        assert!(corner.distance_m <= urban.isd_m * 0.5 * 1.4143);
        // Beyond the lattice there is no coverage at all.
        let far = urban.coverage(Vec3::new(1e9, 0.0, 1.5));
        assert!(far.is_none());
        // The highway plan puts the same point at a lower RSRP, because its sites are
        // further apart.
        let hw = CellPlan::highway_macro()
            .coverage(Vec3::new(400.0, 0.0, 1.5))
            .expect("covered");
        assert!(hw.rsrp_dbm < at_site.rsrp_dbm);
        // The cell id is a pure function of the geometry.
        assert_eq!(
            urban.cell_id(urban.nearest_site(Vec3::new(10.0, 10.0, 0.0))),
            urban.cell_id((0, 0))
        );
    }

    #[test]
    fn the_uu_path_loss_is_the_printed_formula() {
        // 128.1 + 37.6·log10(R_km) [TR 36.885 Table A.1.4-2].
        assert!(
            (uu_path_loss_db(1000.0) - 128.1).abs() < 1e-9,
            "1 km is PL0"
        );
        assert!((uu_path_loss_db(10_000.0) - (128.1 + 37.6)).abs() < 1e-9);
        // Monotone, which is the conformance property of 03-interfaces.md §17.
        let mut prev = 0.0;
        for d in [10.0, 50.0, 100.0, 500.0, 1000.0, 5000.0] {
            let pl = uu_path_loss_db(d);
            assert!(pl > prev);
            prev = pl;
        }
    }

    #[test]
    fn the_interruption_components_add_up_to_the_printed_totals() {
        // TS 36.133 via 04-models.md §10.1: known target T_IU + 20 ms, unknown adds
        // T_search 80 ms, RRC procedure +50 ms.
        let known = HandoverKind::IntraLteKnown.interruption_ms(0.0);
        let unknown = HandoverKind::IntraLteUnknown.interruption_ms(0.0);
        assert!((unknown - known - handover::T_SEARCH_MS).abs() < 1e-9);
        assert!((known - (handover::T_IU_MS + 20.0 + 50.0)).abs() < 1e-9);
        // The E-UTRA to UTRA unknown case is the printed +150 ms.
        let utra_unknown = HandoverKind::EutraToUtraUnknown.interruption_ms(0.0);
        assert!((utra_unknown - (handover::T_IU_MS + 150.0)).abs() < 1e-9);
        // F_max costs 10 ms each.
        let a = HandoverKind::EutraToUtraKnown.interruption_ms(0.0);
        let b = HandoverKind::EutraToUtraKnown.interruption_ms(3.0);
        assert!((b - a - 30.0).abs() < 1e-9);
    }

    #[test]
    fn a_handover_loses_the_packets_offered_inside_its_interruption() {
        let mut uu = HandoverOutageUu::new(CellCapacityUu::new(CellPlan::urban_macro()));
        let mut ctx = TestCtx::new(4);
        let node = NodeId::new(1);
        let d = uu.handover(&mut ctx, node, CellId::new(0), CellId::new(1));
        assert!(d.as_secs_f64() > 0.0);
        assert_eq!(uu.handovers(), 1);
        // Inside the interruption, a send is lost.
        assert!(matches!(
            uu.send(&mut ctx, node, Direction::Uplink, 300, Qos::BestEffort),
            SendOutcome::Dropped(_)
        ));
        assert_eq!(uu.lost_to_handover(), 1);
        // After it, sends go through again.
        ctx.set_now(d.after(ctx.now()) + 1);
        assert!(matches!(
            uu.send(&mut ctx, node, Direction::Uplink, 300, Qos::BestEffort),
            SendOutcome::Scheduled { .. }
        ));
        assert!(!uu.is_interrupted(node, ctx.now()));
    }

    #[test]
    fn crossing_a_cell_boundary_triggers_exactly_one_handover() {
        let mut uu = HandoverOutageUu::new(CellCapacityUu::new(CellPlan::urban_macro()));
        let mut ctx = TestCtx::new(5);
        let node = NodeId::new(1);
        // First observation establishes the serving cell without a handover.
        assert!(
            uu.observe_position(&mut ctx, node, Vec3::new(0.0, 0.0, 1.5))
                .is_none()
        );
        // Moving inside the same cell does not hand over.
        assert!(
            uu.observe_position(&mut ctx, node, Vec3::new(100.0, 0.0, 1.5))
                .is_none()
        );
        // Crossing halfway to the next site does.
        assert!(
            uu.observe_position(&mut ctx, node, Vec3::new(300.0, 0.0, 1.5))
                .is_some()
        );
        assert_eq!(uu.handovers(), 1);
        // And staying there does not hand over again.
        assert!(
            uu.observe_position(&mut ctx, node, Vec3::new(310.0, 0.0, 1.5))
                .is_none()
        );
        assert_eq!(uu.handovers(), 1);
    }

    #[test]
    fn the_loss_percentiles_are_the_measured_ones_and_interpolate_between() {
        assert!((loss_at_percentile(0.50) - 1e-4).abs() < 1e-12);
        assert!((loss_at_percentile(0.75) - 1e-3).abs() < 1e-12);
        assert!((loss_at_percentile(0.99) - 1.2e-2).abs() < 1e-12);
        // Monotone, and clamped outside the measured range.
        assert!((loss_at_percentile(0.0) - 1e-4).abs() < 1e-12);
        assert!((loss_at_percentile(1.0) - 1.2e-2).abs() < 1e-12);
        let mid = loss_at_percentile(0.625);
        assert!(mid > 1e-4 && mid < 1e-3);
    }

    #[test]
    fn store_and_forward_bounds_expires_and_drains() {
        let mut sf = StoreAndForward::new(4, Duration::from_secs(10));
        let mut uu = FixedLatencyUu::new(UuLatencyPreset::LteFirstHop);
        let mut ctx = TestCtx::new(6);
        let node = NodeId::new(1);
        // Six packets into a four-deep buffer: the two oldest are dropped.
        for i in 0..6u64 {
            sf.store(
                node,
                i * 1_000_000_000,
                300,
                Direction::Uplink,
                Qos::BestEffort,
            );
        }
        assert_eq!(sf.depth(node), 4);
        assert_eq!(sf.overflowed(), 2);
        // Drained 20 s later, everything has aged out.
        ctx.set_now(25_000_000_000);
        let out = sf.forward(&mut ctx, &mut uu, node);
        assert!(out.is_empty());
        assert_eq!(sf.expired(), 4);
        assert_eq!(sf.depth(node), 0);
        // Stored and drained promptly, everything goes.
        for i in 0..3u64 {
            sf.store(
                node,
                25_000_000_000 + i,
                300,
                Direction::Uplink,
                Qos::BestEffort,
            );
        }
        ctx.set_now(26_000_000_000);
        let out = sf.forward(&mut ctx, &mut uu, node);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|o| o.deliver_at().is_some()));
        assert_eq!(sf.forwarded(), 3);
    }

    #[test]
    fn a_no_coverage_send_keeps_the_packet_buffered() {
        // A plan the point is outside of: no coverage anywhere the UE is.
        let plan = CellPlan {
            extent_sites: 0,
            ..CellPlan::urban_macro()
        };
        struct Dead(ModelCard, CellPlan);
        impl Model for Dead {
            fn card(&self) -> &ModelCard {
                &self.0
            }
        }
        impl<C: Ctx + ?Sized> CellularUu<C> for Dead {
            fn tier(&self) -> Tier {
                Tier::Abstract
            }
            fn coverage(&self, p: Vec3, _t: SimTime) -> Option<CellView> {
                self.1.coverage(p)
            }
            fn send(
                &mut self,
                _ctx: &mut C,
                _ue: NodeId,
                _dir: Direction,
                _bytes: u32,
                _qos: Qos,
            ) -> SendOutcome {
                SendOutcome::NoCoverage
            }
            fn handover(
                &mut self,
                _ctx: &mut C,
                _ue: NodeId,
                _from: CellId,
                _to: CellId,
            ) -> Duration {
                Duration::ZERO
            }
        }
        let mut dead = Dead(
            ModelCard::new("cellular/uu/test-dead", Family::Cellular, "1.0.0", "test"),
            plan,
        );
        let mut sf = StoreAndForward::new(8, Duration::from_secs(60));
        let mut ctx = TestCtx::new(7);
        let node = NodeId::new(1);
        sf.store(node, 0, 300, Direction::Uplink, Qos::BestEffort);
        ctx.set_now(1_000_000);
        let out = sf.forward(&mut ctx, &mut dead, node);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], SendOutcome::NoCoverage));
        assert_eq!(sf.depth(node), 1, "a no-coverage packet stays buffered");
        assert_eq!(sf.forwarded(), 0);
    }

    #[test]
    fn every_card_validates() {
        for preset in UuLatencyPreset::ALL {
            FixedLatencyUu::new(preset)
                .card()
                .validate()
                .unwrap_or_else(|e| panic!("{}: {e}", preset.label()));
        }
        CellCapacityUu::new(CellPlan::urban_macro())
            .card()
            .validate()
            .expect("the capacity card validates");
        HandoverOutageUu::new(CellCapacityUu::new(CellPlan::urban_macro()))
            .card()
            .validate()
            .expect("the handover card validates");
    }
}
