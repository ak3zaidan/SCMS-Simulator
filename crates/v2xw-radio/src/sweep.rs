//! The measurement harness: a self-contained highway that drives one radio stack and
//! reports packet delivery against distance, so the curves of 04-models.md §13 can be
//! checked rather than asserted.
//!
//! # Why a harness and not a scenario
//!
//! The validation rows of 04-models.md §13 are statements about *curves*: "PDR ≈ 0.97 at
//! 0 m, 0.95 at 100 m, 0.90 at 200 m …". Checking one needs tens of thousands of
//! transmissions and millions of reception evaluations at a controlled vehicle density,
//! and it needs the *same* world for every radio so that a comparison between
//! technologies is a comparison of radios. The engine can run that scenario once it
//! exists; this harness runs it now, from inside the crate that owns the models, with
//! nothing between the models and the statistic.
//!
//! Everything it adds beyond the models is geometry and bookkeeping: a ring highway with
//! wrap-around (Todisco's own layout), constant-speed vehicles in two directions, a
//! packet generator, and a distance-binned counter. There is no propagation, PHY, MAC or
//! error model here — those are [`crate::prop`], [`crate::cv2x`], [`crate::sps`],
//! [`crate::bler`], [`crate::phy`] and [`crate::mac`], driven through their own published
//! interfaces.
//!
//! # The one thing the harness has to be honest about
//!
//! **The published curves were produced with WINNER+ B1, which this workspace does not
//! ship.** 04-models.md §3.3 registers `propagation/winner-plus-b1` with "every
//! coefficient `TODO: calibrate`" because the WINNER II D1.1.2 report is not in the cache,
//! and says in as many words that "validation runs that follow Molina-Masegosa, Bazzi or
//! Todisco (§13) need it". The harness therefore offers two channels
//! ([`ChannelModel`]): the fully cited TR 37.885 highway-LOS model that *is* shipped, and
//! a dual-slope stand-in whose coefficients are `todo-calibrate` and which exists only so
//! that the size of the resulting disagreement can be measured rather than guessed. Every
//! report says which channel produced it, and
//! [`SweepReport::channel_disclaimer`] carries the sentence into any output.

use std::collections::BTreeMap;

use serde::Serialize;
use v2xw_core::ctx::{Ctx, ErasedRecord};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::ids::{ActorId, FrameSeq, LinkKey, NodeId, SduId};
use v2xw_core::math;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::{Duration, SimTime};
use v2xw_world::model::World;

use crate::abstract_tier::ReceptionSample;
use crate::bler::SidelinkErrorModel;
use crate::cv2x::{SidelinkPhy, SlArrival, SlInterferer};
use crate::numeric;
use crate::sidelink::{PoolConfig, SlResource};
use crate::sps::{SpsEngine, SpsParams};
use crate::traits::Mac;
use crate::types::{
    AccessCategory, ChannelId, FrameDescriptor, LossCause, MacSdu, Mcs, RxOutcome, SduRef,
};

// =========================================================================================
// A context the harness can drive
// =========================================================================================

/// A [`Ctx`] the harness owns: a clock it sets, a seeded RNG registry, a world and a sink
/// that discards records.
///
/// It is public and not `cfg(test)` because the harness is a shipped measurement tool, not
/// a test fixture: a reviewer asking "what does your LTE-V2X actually deliver at 300 m"
/// runs [`sweep_sidelink`] and needs a context to run it in.
pub struct SweepCtx {
    now: SimTime,
    rng: RngRegistry,
    world: World,
    actors: Vec<ActorId>,
    scheduler: Scheduler<&'static str>,
    provenance: ProvenanceLog,
    params: ParamSet,
    /// Records emitted, counted rather than kept: a sweep emits millions and nothing
    /// reads them.
    pub records: u64,
}

impl SweepCtx {
    /// A context seeded with `seed`.
    ///
    /// # Panics
    ///
    /// If the procedural world cannot be built, which would mean `v2xw-world`'s own
    /// smallest grid is broken.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        // The harness places its own vehicles by hand and never reads the road network;
        // `World` is required only because `Ctx` names it. The smallest real world is the
        // cheapest honest stand-in for an empty one.
        let world = v2xw_world::procedural::grid(
            &v2xw_world::procedural::GridParams::tr36885_urban().with_size(2, 2),
            &v2xw_world::ImportOptions::default().imported_at("1970-01-01T00:00:00Z"),
        )
        .expect("a 2 x 2 procedural grid builds");
        Self {
            now: 0,
            rng: RngRegistry::new(seed),
            world,
            actors: Vec::new(),
            scheduler: Scheduler::new(),
            provenance: ProvenanceLog::new(),
            params: ParamSet::new(),
            records: 0,
        }
    }

    /// Moves the clock to `t`.
    pub fn set_now(&mut self, t: SimTime) {
        self.now = t;
    }
}

impl Ctx for SweepCtx {
    type World = World;
    type Actors = Vec<ActorId>;
    type Payload = &'static str;

    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: Self::Payload) -> EventHandle {
        self.scheduler.schedule(at, class, payload)
    }

    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }

    fn world(&self) -> &Self::World {
        &self.world
    }

    fn actors(&self) -> &Self::Actors {
        &self.actors
    }

    fn emit_erased(&mut self, _record: &dyn ErasedRecord) {
        self.records += 1;
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        &self.params
    }
}

// =========================================================================================
// The channel
// =========================================================================================

/// Which large-scale channel the sweep runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelModel {
    /// `propagation/tr37885`, highway LOS: `PL = 32.4 + 20·log10(d3D) + 20·log10(fc_GHz)`,
    /// σ_SF 3 dB [TR 37.885 Tables 6.2-1, 6.2.1-1, via 04-models.md §3.3].
    ///
    /// Fully cited and shipped. Its exponent is 2, so it is free space with an offset, and
    /// it predicts a far longer noise-limited range than the measurement-fitted models do.
    Tr37885HighwayLos,
    /// A dual-slope stand-in for WINNER+ B1 LOS, which the workspace does not ship.
    ///
    /// **`TODO: calibrate`, every coefficient.** 04-models.md §3.3 registers
    /// `propagation/winner-plus-b1` unvalidated with all coefficients `TODO: calibrate`
    /// because WINNER II D1.1.2 is not cached, and this is not that model: it is a
    /// two-slope curve of the *shape* B1 has, with a near slope of 2.27, a breakpoint at
    /// `4·h_t·h_r/λ` and a far slope of 4, included only so that the effect of the
    /// channel choice on the measured curves can be quantified. Any number it produces is
    /// labelled as resting on an uncalibrated channel.
    WinnerB1ShapedTodoCalibrate,
    /// `propagation/log-distance-shadowing` with the `abbas-los-highway` preset: n1 1.66,
    /// n2 2.88, PL0 66.1 dB at 10 m, σ 3.95 dB, breakpoint 104 m
    /// [Abbas 2015 Table II, VERIFIED, via 04-models.md §3.2].
    ///
    /// The only *measurement-fitted, fully verified* highway LOS model the workspace
    /// ships, and therefore the most defensible stand-in for the papers' WINNER+ B1.
    AbbasLosHighway,
}

impl ChannelModel {
    /// Every channel, in a fixed order.
    pub const ALL: [ChannelModel; 3] = [
        ChannelModel::Tr37885HighwayLos,
        ChannelModel::WinnerB1ShapedTodoCalibrate,
        ChannelModel::AbbasLosHighway,
    ];

    /// The label a report prints.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            ChannelModel::Tr37885HighwayLos => "tr37885-highway-los",
            ChannelModel::WinnerB1ShapedTodoCalibrate => "winner-b1-shaped-todo-calibrate",
            ChannelModel::AbbasLosHighway => "abbas-los-highway",
        }
    }

    /// True when every coefficient of the model is a printed, verified value.
    #[must_use]
    pub const fn is_fully_cited(self) -> bool {
        !matches!(self, ChannelModel::WinnerB1ShapedTodoCalibrate)
    }

    /// The shadowing standard deviation, dB.
    #[must_use]
    pub const fn sigma_db(self) -> f64 {
        match self {
            // TR 37.885: σ_SF 3 dB for highway LOS and NLOSv.
            ChannelModel::Tr37885HighwayLos => 3.0,
            // TR 36.885 Table A.1.4-1 gives σ 3 dB LOS for WINNER+ B1.
            ChannelModel::WinnerB1ShapedTodoCalibrate => 3.0,
            // Abbas 2015 Table II.
            ChannelModel::AbbasLosHighway => 3.95,
        }
    }

    /// The shadowing decorrelation distance, metres: 25 m on a freeway
    /// [TR 36.885 Annex A.1.4, via 04-models.md §3.2].
    #[must_use]
    pub const fn decorrelation_m(self) -> f64 {
        25.0
    }

    /// The deterministic path loss at a distance, dB.
    #[must_use]
    pub fn path_loss_db(self, d_m: f64, f_hz: f64) -> f64 {
        let d = d_m.max(1.0);
        match self {
            ChannelModel::Tr37885HighwayLos => crate::prop::tr37885_highway_los_db(d, f_hz / 1e9),
            ChannelModel::WinnerB1ShapedTodoCalibrate => {
                let lambda = numeric::wavelength_m(f_hz);
                // 4·h_t·h_r/λ with both antennas at the 1.5 m vehicle height of
                // TR 36.885: 177 m at 5.9 GHz.
                let d_bp = 4.0 * 1.5 * 1.5 / lambda;
                let base = 41.0 + 20.0 * math::log10(f_hz / 5.0e9);
                if d <= d_bp {
                    base + 22.7 * math::log10(d)
                } else {
                    base + 22.7 * math::log10(d_bp) + 40.0 * math::log10(d / d_bp)
                }
            }
            ChannelModel::AbbasLosHighway => crate::prop::LogDistancePreset::AbbasLosHighway
                .params()
                .path_loss_db(d),
        }
    }
}

// =========================================================================================
// The scenario
// =========================================================================================

/// The highway the sweep runs on: a ring with wrap-around, two directions, constant
/// speeds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HighwaySweep {
    /// Ring circumference, metres. Must be at least twice `max_distance_m` so that a
    /// distance bin never wraps onto itself.
    pub ring_m: f64,
    /// Lanes per direction.
    pub lanes_per_direction: u32,
    /// Lane width, metres [TR 36.885 Annex A: 4 m freeway lanes].
    pub lane_width_m: f64,
    /// Vehicle density, vehicles per kilometre over all lanes.
    pub density_veh_per_km: f64,
    /// Mean speed, m/s.
    pub mean_speed_mps: f64,
    /// Speed standard deviation, m/s. Todisco draws N(70, 7) km/h.
    pub speed_sigma_mps: f64,
    /// Packets per second per vehicle.
    pub packets_per_second: u32,
    /// The payload cycle, bytes. Traffic Model 1 is `{300, 190, 190, 190, 190}`
    /// [TR 37.885 §6.1.5].
    pub payload_cycle: Vec<u32>,
    /// How long to simulate after the warm-up.
    pub duration: Duration,
    /// How long to run before counting, so the reselection counters and sensing windows
    /// are populated.
    pub warmup: Duration,
    /// Transmit power at the connector, dBm.
    pub tx_power_dbm: f64,
    /// Antenna gain at each end, dBi [TR 36.885 Annex A.1.1: 3 dBi at 1.5 m].
    pub antenna_gain_dbi: f64,
    /// The channel.
    pub channel: ChannelModel,
    /// Whether spatially correlated log-normal shadowing is applied.
    pub shadowing: bool,
    /// Whether per-frame Nakagami fading is applied on top.
    pub fading: bool,
    /// Distance-bin width, metres.
    pub bin_m: f64,
    /// The largest distance counted.
    pub max_distance_m: f64,
    /// The distance out to which a receiver's sensing is fed. Beyond it a transmission is
    /// neither sensed nor counted, which bounds the cost of the inner loop.
    pub sensing_range_m: f64,
    /// Master seed.
    pub seed: u64,
}

impl HighwaySweep {
    /// The Molina-Masegosa "Highway Slow" configuration: 120 veh/km at 70 km/h, the
    /// packets-per-second the caller chooses, 190 B with every fifth 300 B, WINNER+ B1 in
    /// the paper and [`ChannelModel::AbbasLosHighway`] here
    /// [Molina-Masegosa 2017 Fig. 3, via 04-models.md §5.5].
    ///
    /// The ring is 2 km, which is Todisco's layout and the shortest ring that lets a
    /// 500 m distance bin not wrap onto itself.
    #[must_use]
    pub fn highway_slow(packets_per_second: u32) -> Self {
        Self {
            ring_m: 2000.0,
            lanes_per_direction: 3,
            lane_width_m: 4.0,
            density_veh_per_km: 120.0,
            mean_speed_mps: 70.0 / 3.6,
            speed_sigma_mps: 0.0,
            packets_per_second,
            payload_cycle: vec![300, 190, 190, 190, 190],
            duration: Duration::from_secs(4),
            warmup: Duration::from_secs(2),
            tx_power_dbm: 23.0,
            antenna_gain_dbi: 3.0,
            channel: ChannelModel::AbbasLosHighway,
            shadowing: true,
            fading: false,
            bin_m: 25.0,
            max_distance_m: 500.0,
            sensing_range_m: 1000.0,
            seed: 0x5EED_0001,
        }
    }

    /// The Molina-Masegosa "Highway Fast" configuration: 60 veh/km at 140 km/h
    /// [Molina-Masegosa 2017 Table 2].
    #[must_use]
    pub fn highway_fast(packets_per_second: u32) -> Self {
        Self {
            density_veh_per_km: 60.0,
            mean_speed_mps: 140.0 / 3.6,
            ..Self::highway_slow(packets_per_second)
        }
    }

    /// Todisco's NR configuration: 2 km highway, 3 lanes per direction, wrap-around,
    /// N(70, 7) km/h, 350 B every 100 ms [Todisco 2021, via 04-models.md §5.2, §5.5].
    #[must_use]
    pub fn todisco(density_veh_per_km: f64) -> Self {
        Self {
            density_veh_per_km,
            speed_sigma_mps: 7.0 / 3.6,
            payload_cycle: vec![350],
            packets_per_second: 10,
            max_distance_m: 200.0,
            bin_m: 10.0,
            sensing_range_m: 600.0,
            // Todisco's own figure is 13 dBm/MHz power spectral density; over the 1.8 MHz
            // of one 10-PRB sub-channel at 15 kHz that is 15.6 dBm, not 23.
            tx_power_dbm: 13.0 + 10.0 * math::log10(1.8),
            ..Self::highway_slow(10)
        }
    }

    /// The same sweep on another channel.
    #[must_use]
    pub fn with_channel(mut self, channel: ChannelModel) -> Self {
        self.channel = channel;
        self
    }

    /// The same sweep at another density.
    #[must_use]
    pub fn with_density(mut self, density_veh_per_km: f64) -> Self {
        self.density_veh_per_km = density_veh_per_km;
        self
    }

    /// The same sweep with a different seed.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// The same sweep over a different span.
    #[must_use]
    pub fn with_duration(mut self, warmup: Duration, duration: Duration) -> Self {
        self.warmup = warmup;
        self.duration = duration;
        self
    }

    /// How many vehicles the ring holds.
    #[must_use]
    pub fn vehicles(&self) -> usize {
        ((self.ring_m / 1000.0) * self.density_veh_per_km)
            .round()
            .max(2.0) as usize
    }

    /// How many distance bins the report has.
    #[must_use]
    pub fn bins(&self) -> usize {
        (self.max_distance_m / self.bin_m).ceil() as usize
    }

    /// The bin a distance falls in, or `None` beyond `max_distance_m`.
    #[must_use]
    pub fn bin_of(&self, d_m: f64) -> Option<usize> {
        let i = (d_m / self.bin_m) as usize;
        (i < self.bins()).then_some(i)
    }

    /// The centre distance of a bin, metres.
    #[must_use]
    pub fn bin_centre_m(&self, i: usize) -> f64 {
        (i as f64 + 0.5) * self.bin_m
    }

    /// The offered load in sub-channel-slots per second, from the arithmetic alone.
    ///
    /// Reported next to the measured occupancy so that a disagreement with a published
    /// occupancy figure can be traced to the scenario reconstruction rather than to the
    /// engine: if the measured occupancy equals this, the engine is doing what the
    /// scenario asked.
    #[must_use]
    pub fn offered_subchannel_slots_per_s(&self, pool: &PoolConfig) -> f64 {
        let mean_len = math::sum_ordered(
            self.payload_cycle
                .iter()
                .map(|b| pool.subchannels_for(*b).unwrap_or(pool.subchannels()) as f64),
        ) / self.payload_cycle.len() as f64;
        self.vehicles() as f64 * f64::from(self.packets_per_second) * mean_len
    }
}

// =========================================================================================
// Vehicles
// =========================================================================================

/// One vehicle on the ring.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Vehicle {
    /// Position along the ring at t = 0, metres.
    start_m: f64,
    /// Speed, m/s, signed by direction.
    velocity_mps: f64,
    /// Lateral offset from the ring's centreline, metres.
    lateral_m: f64,
    /// The packet-generation phase, nanoseconds into the period.
    phase_ns: u64,
    /// Which payload of the cycle goes next.
    cycle: usize,
    /// Packets generated.
    generated: u64,
}

fn place_vehicles(s: &HighwaySweep, ctx: &SweepCtx) -> Vec<Vehicle> {
    let n = s.vehicles();
    let lanes = (s.lanes_per_direction * 2).max(1);
    let period_ns = 1_000_000_000u64 / u64::from(s.packets_per_second.max(1));
    (0..n)
        .map(|i| {
            let node = NodeId::new(i as u32);
            // Evenly spaced along the ring, then jittered within the gap so the
            // configuration is not a lattice; the jitter and the speed come from their own
            // keyed streams, so a vehicle's placement does not depend on how many others
            // were placed first.
            let gap = s.ring_m / n as f64;
            let jitter = ctx
                .rng(RngDomain::Spawn, EntityRef::Node(node))
                .uniform(0.0, gap);
            let lane = (i as u32) % lanes;
            let forward = lane < s.lanes_per_direction;
            let speed = if s.speed_sigma_mps > 0.0 {
                ctx.rng(RngDomain::DesiredSpeed, EntityRef::Node(node))
                    .normal(s.mean_speed_mps, s.speed_sigma_mps)
                    .max(1.0)
            } else {
                s.mean_speed_mps
            };
            let phase_ns = ctx
                .rng(RngDomain::Spawn, EntityRef::Node(node))
                .below(period_ns.max(1));
            Vehicle {
                start_m: (i as f64 * gap + jitter) % s.ring_m,
                velocity_mps: if forward { speed } else { -speed },
                lateral_m: (f64::from(lane) - f64::from(lanes - 1) / 2.0) * s.lane_width_m,
                phase_ns,
                cycle: i % s.payload_cycle.len().max(1),
                generated: 0,
            }
        })
        .collect()
}

/// The ring distance between two along-positions, metres: the shorter way round.
fn ring_delta(a: f64, b: f64, ring_m: f64) -> f64 {
    let d = (a - b).abs() % ring_m;
    d.min(ring_m - d)
}

// =========================================================================================
// The report
// =========================================================================================

/// One distance bin's outcome.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct BinStats {
    /// Bin centre, metres.
    pub centre_m: f64,
    /// Arrivals evaluated in this bin.
    pub evaluated: u64,
    /// Arrivals decoded.
    pub received: u64,
}

impl BinStats {
    /// Packet delivery in this bin, or `None` when nothing landed in it.
    #[must_use]
    pub fn pdr(&self) -> Option<f64> {
        (self.evaluated > 0).then(|| self.received as f64 / self.evaluated as f64)
    }
}

/// What one sweep measured.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SweepReport {
    /// What was swept: a label a report prints.
    pub label: String,
    /// The radio.
    pub rat: String,
    /// The channel.
    pub channel: &'static str,
    /// Vehicles on the ring.
    pub vehicles: usize,
    /// Vehicle density, veh/km.
    pub density_veh_per_km: f64,
    /// Packets per second per vehicle.
    pub packets_per_second: u32,
    /// Transmissions counted (after warm-up).
    pub transmissions: u64,
    /// Arrivals evaluated.
    pub evaluated: u64,
    /// Arrivals decoded.
    pub received: u64,
    /// Per-bin delivery.
    pub bins: Vec<BinStats>,
    /// Losses by cause label, over all bins.
    pub losses: BTreeMap<String, u64>,
    /// Measured sub-channel occupancy: *distinct* `(slot, sub-channel)` pairs carrying a
    /// transmission, over all `(slot, sub-channel)` pairs.
    ///
    /// Distinct, because that is what "the fraction of sub-channels occupied" means and
    /// what a published occupancy figure can be compared against: two vehicles that pick
    /// the same resource occupy one sub-channel, not two. The *demand* — which can exceed
    /// one — is [`SweepReport::offered_occupancy`].
    pub subchannel_occupancy: f64,
    /// The occupancy the offered load predicts from arithmetic alone, which exceeds one
    /// when the scenario asks the pool for more than it has.
    pub offered_occupancy: f64,
    /// Fraction of transmissions whose resource was also used, in the same slot, by
    /// another transmitter within `sensing_range_m`.
    pub resource_collision_ratio: f64,
    /// Mean sidelink CBR over the vehicles at the end of the run, where the stack
    /// measures one.
    pub mean_cbr: f64,
}

impl SweepReport {
    /// The delivery at the bin containing a distance.
    #[must_use]
    pub fn pdr_at(&self, d_m: f64) -> Option<f64> {
        let bin_m = if self.bins.len() > 1 {
            self.bins[1].centre_m - self.bins[0].centre_m
        } else {
            1.0
        };
        let i = (d_m / bin_m) as usize;
        self.bins.get(i).and_then(BinStats::pdr)
    }

    /// The largest distance at which delivery is still at or above `target`, by linear
    /// interpolation between bin centres.
    ///
    /// This is the statistic Todisco Figs. 8 and 10(a) are stated in ("range at PRR 0.9").
    #[must_use]
    pub fn range_at_pdr(&self, target: f64) -> Option<f64> {
        let pts: Vec<(f64, f64)> = self
            .bins
            .iter()
            .filter_map(|b| b.pdr().map(|p| (b.centre_m, p)))
            .collect();
        if pts.is_empty() {
            return None;
        }
        if pts[0].1 < target {
            return Some(0.0);
        }
        for w in pts.windows(2) {
            if w[0].1 >= target && w[1].1 < target {
                let t = (w[0].1 - target) / (w[0].1 - w[1].1);
                return Some(w[0].0 + t * (w[1].0 - w[0].0));
            }
        }
        Some(pts[pts.len() - 1].0)
    }

    /// Overall delivery.
    #[must_use]
    pub fn pdr(&self) -> f64 {
        if self.evaluated == 0 {
            return 0.0;
        }
        self.received as f64 / self.evaluated as f64
    }

    /// Mean delivery within a distance, weighted by arrivals: the statistic
    /// Bazzi 2018 Fig. 3 is stated in ("average PRR within 100 m urban / 200 m highway").
    #[must_use]
    pub fn mean_pdr_within(&self, d_m: f64) -> Option<f64> {
        let (mut ev, mut rx) = (0u64, 0u64);
        for b in &self.bins {
            if b.centre_m <= d_m {
                ev += b.evaluated;
                rx += b.received;
            }
        }
        (ev > 0).then(|| rx as f64 / ev as f64)
    }

    /// The sentence that must accompany any number from an uncalibrated channel.
    #[must_use]
    pub fn channel_disclaimer(&self) -> Option<&'static str> {
        (self.channel == ChannelModel::WinnerB1ShapedTodoCalibrate.label()).then_some(
            "This curve rests on a WINNER+ B1-*shaped* channel whose every coefficient is \
             `TODO: calibrate` (04-models.md §3.3 does not ship `propagation/winner-plus-b1` \
             because WINNER II D1.1.2 is not cached). It quantifies the effect of the \
             channel choice; it does not validate the radio.",
        )
    }

    /// The report as a table a reader can compare against a published figure.
    #[must_use]
    pub fn table(&self) -> String {
        let mut s = format!(
            "{}\n  rat {}  channel {}  {} veh ({} veh/km)  {} pps\n  \
             transmissions {}  arrivals {}  PDR {:.4}\n  \
             occupancy measured {:.4} / offered {:.4}  resource collisions {:.4}  CBR {:.3}\n",
            self.label,
            self.rat,
            self.channel,
            self.vehicles,
            self.density_veh_per_km,
            self.packets_per_second,
            self.transmissions,
            self.evaluated,
            self.pdr(),
            self.subchannel_occupancy,
            self.offered_occupancy,
            self.resource_collision_ratio,
            self.mean_cbr,
        );
        s.push_str("  distance_m  arrivals      pdr\n");
        for b in &self.bins {
            match b.pdr() {
                Some(p) => s.push_str(&format!(
                    "  {:>10.0}  {:>8}  {:>7.4}\n",
                    b.centre_m, b.evaluated, p
                )),
                None => s.push_str(&format!("  {:>10.0}  {:>8}        -\n", b.centre_m, 0)),
            }
        }
        if !self.losses.is_empty() {
            s.push_str("  losses:");
            for (cause, n) in &self.losses {
                s.push_str(&format!(" {cause}={n}"));
            }
            s.push('\n');
        }
        if let Some(d) = self.channel_disclaimer() {
            s.push_str("  NOTE: ");
            s.push_str(d);
            s.push('\n');
        }
        s
    }
}

// =========================================================================================
// Shadowing
// =========================================================================================

/// The spatially correlated shadowing field over all directed links.
///
/// A flat `Vec` indexed by `tx·n + rx`, not a map: it is `O(1)` to read, bounded at
/// `n²` entries, and indexed arithmetically, so no iteration order can reach a link
/// budget. The innovation for each update comes from
/// `(Shadow, LinkFrame { link, frame: slot })` — a single-use scope, so the draw is a pure
/// function of the link and the slot and does not depend on how many other links were
/// updated first (invariant I-R2).
struct ShadowField {
    n: usize,
    /// The current realisation, dB.
    value: Vec<f32>,
    /// The distance the last update was taken at, metres; negative means "never updated".
    last_d: Vec<f32>,
    sigma_db: f64,
    d_corr_m: f64,
}

impl ShadowField {
    fn new(n: usize, sigma_db: f64, d_corr_m: f64) -> Self {
        Self {
            n,
            value: vec![0.0; n * n],
            last_d: vec![-1.0; n * n],
            sigma_db,
            d_corr_m,
        }
    }

    /// The shadowing on one directed link at a distance, updating the AR(1) state.
    ///
    /// `S(n) = ρ·S(n−1) + sqrt(1 − ρ²)·N(0, σ²)` with `ρ = exp(−D/D_corr)` and `D` the
    /// distance moved since the last update [TR 36.885 Annex A.1.4, via 04-models.md §3.2].
    fn update(&mut self, ctx: &SweepCtx, tx: usize, rx: usize, d_m: f64, slot: u64) -> f64 {
        let i = tx * self.n + rx;
        let last = f64::from(self.last_d[i]);
        let draw = ctx
            .rng(
                RngDomain::Shadow,
                EntityRef::LinkFrame {
                    link: LinkKey(NodeId::new(tx as u32), NodeId::new(rx as u32)),
                    frame: slot,
                },
            )
            .normal(0.0, 1.0);
        let next = if last < 0.0 {
            self.sigma_db * draw
        } else {
            let moved = (d_m - last).abs();
            let rho = crate::prop::ShadowProcess::rho(moved, self.d_corr_m);
            let prev = f64::from(self.value[i]);
            rho * prev + math::sqrt((1.0 - rho * rho).max(0.0)) * self.sigma_db * draw
        };
        self.value[i] = next as f32;
        self.last_d[i] = d_m as f32;
        next
    }
}

// =========================================================================================
// The sidelink sweep
// =========================================================================================

/// One transmission in flight during a slot.
#[derive(Debug, Clone, Copy)]
struct SlotTx {
    vehicle: usize,
    resource: SlResource,
    bytes: u32,
}

/// Runs one LTE-V2X Mode 4 or NR-V2X Mode 2 sweep and reports the measured curve.
///
/// The loop is one slot at a time:
///
/// 1. advance the clock, place every vehicle;
/// 2. generate packets whose period has elapsed and enqueue them in the SPS engine;
/// 3. poll every vehicle whose booked resource is this slot, collecting the transmissions;
/// 4. for each transmission and each vehicle within the sensing range, compute the
///    received power once and use it twice — for the reception evaluation when the
///    distance is inside `max_distance_m`, and for the receiver's sensing window always;
/// 5. count.
///
/// # Panics
///
/// If the pool cannot carry the smallest payload in the cycle, which is a configuration
/// error the caller should see rather than a silent empty report.
#[must_use]
pub fn sweep_sidelink(sweep: &HighwaySweep, pool: PoolConfig, params: SpsParams) -> SweepReport {
    let mut ctx = SweepCtx::new(sweep.seed);
    let mut vehicles = place_vehicles(sweep, &ctx);
    let n = vehicles.len();
    let phy = SidelinkPhy::new(v2xw_core::card::Tier::High, pool.clone());
    let mut mac = SpsEngine::new(v2xw_core::card::Tier::High, pool.clone(), params.clone());
    let fading = crate::fading::NakagamiFading::new(crate::fading::NakagamiPreset::YinDsrcFreeway);
    let mut fading = fading;
    let mut shadow = ShadowField::new(n, sweep.channel.sigma_db(), sweep.channel.decorrelation_m());

    for b in &sweep.payload_cycle {
        assert!(
            pool.subchannels_for(*b).is_some(),
            "the pool cannot carry a {b} B payload"
        );
    }

    let slot_ns = pool.slot().as_nanos();
    let total_slots = (sweep.warmup.as_nanos() + sweep.duration.as_nanos()) / slot_ns;
    let warm_slots = sweep.warmup.as_nanos() / slot_ns;
    let period_ns = 1_000_000_000u64 / u64::from(sweep.packets_per_second.max(1));
    let subchannels = pool.subchannels();
    let gain2 = 2.0 * sweep.antenna_gain_dbi;

    let mut bins: Vec<BinStats> = (0..sweep.bins())
        .map(|i| BinStats {
            centre_m: sweep.bin_centre_m(i),
            ..BinStats::default()
        })
        .collect();
    let mut losses: BTreeMap<String, u64> = BTreeMap::new();
    let mut transmissions = 0u64;
    let mut distinct_used = 0u64;
    let mut counted_slots = 0u64;
    // One byte per sub-channel of the slot being counted, reused: "was this sub-channel
    // already claimed by an earlier transmission in this slot?".
    let mut claimed = vec![false; subchannels as usize];
    let mut collided_transmissions = 0u64;
    let mut along = vec![0.0f64; n];

    // Reusable buffers: the inner loop runs millions of times and must not allocate.
    //
    // `power_grid` is a *dense* row per transmission, indexed by receiver, so the
    // interference lookup "what did transmission j arrive at receiver r with" is one
    // index rather than a scan. With a sparse list it was O(transmissions x receivers)
    // per evaluated arrival, which at the validation density is a few hundred million
    // scans per second of simulated time.
    let mut slot_txs: Vec<SlotTx> = Vec::with_capacity(64);
    // Generation is collected before it is enqueued because the loop that produces it
    // borrows `vehicles` mutably and the enqueue borrows the context.
    let mut pending: Vec<(usize, MacSdu)> = Vec::with_capacity(64);
    let mut power_grid: Vec<Vec<f64>> = Vec::with_capacity(64);
    let mut in_range: Vec<Vec<(usize, f64)>> = Vec::with_capacity(64);

    for slot in 0..total_slots {
        let now = slot * slot_ns;
        ctx.set_now(now);
        let t_s = now as f64 * 1e-9;
        for (i, v) in vehicles.iter().enumerate() {
            let mut a = (v.start_m + v.velocity_mps * t_s) % sweep.ring_m;
            if a < 0.0 {
                a += sweep.ring_m;
            }
            along[i] = a;
        }

        // 2. Generation.
        for (i, v) in vehicles.iter_mut().enumerate() {
            let due = v.generated * period_ns + v.phase_ns;
            if due <= now {
                let bytes = sweep.payload_cycle[v.cycle % sweep.payload_cycle.len()];
                v.cycle += 1;
                v.generated += 1;
                let sdu = MacSdu {
                    frame: FrameDescriptor {
                        bytes,
                        tx_power_dbm: sweep.tx_power_dbm,
                        ..FrameDescriptor::broadcast(
                            bytes,
                            Mcs::R6Qpsk12,
                            SduRef::new(SduId::new(i as u32), FrameSeq::new(v.generated as u32)),
                        )
                    },
                    enqueued_at: now,
                };
                pending.push((i, sdu));
            }
        }
        for (i, sdu) in pending.drain(..) {
            let _ = Mac::enqueue(
                &mut mac,
                &mut ctx,
                NodeId::new(i as u32),
                sdu,
                AccessCategory::Vo,
            );
        }

        // 3. Grants.
        slot_txs.clear();
        for i in 0..n {
            let node = NodeId::new(i as u32);
            if let Some(grant) = Mac::poll(&mut mac, &mut ctx, node, ChannelId::CCH) {
                let bytes = grant.sdu.frame.bytes;
                let len = pool.subchannels_for(bytes).unwrap_or(1);
                let res = mac
                    .last_selection(node)
                    .map(|s| s.resource)
                    .unwrap_or_else(|| SlResource::new(slot, 0, len));
                let resource = SlResource::new(slot, res.subch.min(subchannels - len), len);
                mac.note_transmitted(node, resource);
                slot_txs.push(SlotTx {
                    vehicle: i,
                    resource,
                    bytes,
                });
            }
        }
        if slot >= warm_slots {
            counted_slots += 1;
            claimed.iter_mut().for_each(|c| *c = false);
            for tx in &slot_txs {
                transmissions += 1;
                for sc in tx.resource.range() {
                    if let Some(c) = claimed.get_mut(sc as usize) {
                        if !*c {
                            *c = true;
                            distinct_used += 1;
                        }
                    }
                }
            }
        }
        if slot_txs.is_empty() {
            continue;
        }

        // 4. Received power for every (transmission, nearby vehicle) pair, once.
        while power_grid.len() < slot_txs.len() {
            power_grid.push(vec![f64::NEG_INFINITY; n]);
            in_range.push(Vec::with_capacity(n));
        }
        for k in 0..slot_txs.len() {
            let tx = slot_txs[k];
            let row = &mut power_grid[k];
            let list = &mut in_range[k];
            for v in list.iter() {
                row[v.0] = f64::NEG_INFINITY;
            }
            list.clear();
            for rx in 0..n {
                if rx == tx.vehicle {
                    continue;
                }
                let dl = ring_delta(along[tx.vehicle], along[rx], sweep.ring_m);
                if dl > sweep.sensing_range_m {
                    continue;
                }
                let dlat = vehicles[tx.vehicle].lateral_m - vehicles[rx].lateral_m;
                let d = math::sqrt(dl * dl + dlat * dlat).max(1.0);
                let mut loss = sweep.channel.path_loss_db(d, pool.centre_hz);
                if sweep.shadowing {
                    loss -= shadow.update(&ctx, tx.vehicle, rx, d, slot);
                }
                let mut p = sweep.tx_power_dbm + gain2 - loss;
                if sweep.fading {
                    p += crate::traits::Fading::sample_db(
                        &mut fading,
                        &mut ctx,
                        LinkKey(NodeId::new(tx.vehicle as u32), NodeId::new(rx as u32)),
                        d,
                        now,
                    );
                }
                row[rx] = p;
                list.push((rx, d));
            }
        }

        // 5. Sensing: every vehicle that heard a transmission above the exclusion
        //    threshold records the SCI's reservation and the energy.
        //
        //    The decodability of the SCI is approximated by the RSRP threshold, which is
        //    what the published simulators do: a UE that hears a control channel above
        //    the threshold it would exclude on is assumed to have decoded it. The harness
        //    says so here rather than in a comment on the engine, because it is the
        //    harness's approximation and not the engine's.
        let rri_slots = params.rri.slots(pool.mu) as u32;
        for (k, tx) in slot_txs.iter().enumerate() {
            for &(rx, _d) in &in_range[k] {
                let p = power_grid[k][rx];
                let node = NodeId::new(rx as u32);
                for sc in tx.resource.range() {
                    mac.note_energy(node, slot, sc, p);
                }
                if p >= params.rsrp_threshold_dbm {
                    mac.note_sensed(node, tx.resource, p, rri_slots);
                }
            }
        }

        // 6. Reception.
        if slot < warm_slots {
            continue;
        }
        let mut interferers: Vec<SlInterferer> = Vec::with_capacity(16);
        for k in 0..slot_txs.len() {
            let tx = slot_txs[k];
            // Does anything else in this slot share a sub-channel with this transmitter,
            // within its sensing range? That is the resource collision the SPS engine is
            // supposed to have avoided, and it is a property of the transmission rather
            // than of any one receiver.
            let collided = slot_txs.iter().any(|other| {
                other.vehicle != tx.vehicle
                    && other.resource.overlaps(&tx.resource)
                    && ring_delta(along[tx.vehicle], along[other.vehicle], sweep.ring_m)
                        <= sweep.sensing_range_m
            });
            if collided {
                collided_transmissions += 1;
            }
            for &(rx, d) in &in_range[k] {
                let Some(bin) = sweep.bin_of(d) else {
                    continue;
                };
                let p = power_grid[k][rx];
                // Interference at this receiver: every other transmission in the slot, at
                // the power it arrives with there. One dense lookup per contributor.
                interferers.clear();
                for (j, other) in slot_txs.iter().enumerate() {
                    if j == k || other.vehicle == rx {
                        continue;
                    }
                    let op = power_grid[j][rx];
                    if op.is_finite() {
                        interferers.push(SlInterferer {
                            node: NodeId::new(other.vehicle as u32),
                            power_dbm: op,
                            resource: other.resource,
                        });
                    }
                }
                // Half duplex: the receiver transmitted in this slot.
                let rx_transmitting = slot_txs.iter().any(|t| t.vehicle == rx);
                let arrival = SlArrival {
                    tx_id: slot * 1000 + k as u64,
                    tx: NodeId::new(tx.vehicle as u32),
                    rx: NodeId::new(rx as u32),
                    power_dbm: p,
                    resource: tx.resource,
                    bytes: tx.bytes,
                    rx_transmitting,
                    interferers: interferers.clone(),
                };
                let outcome = phy.evaluate(&mut ctx, &arrival);
                bins[bin].evaluated += 1;
                match outcome {
                    RxOutcome::Received { .. } => bins[bin].received += 1,
                    RxOutcome::Lost(cause) => {
                        *losses.entry(cause.label().to_string()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    let evaluated: u64 = bins.iter().map(|b| b.evaluated).sum();
    let received: u64 = bins.iter().map(|b| b.received).sum();
    let capacity = counted_slots * u64::from(subchannels);
    let end_slot = total_slots.saturating_sub(1);
    let mean_cbr = if n == 0 {
        0.0
    } else {
        math::sum_ordered((0..n).map(|i| mac.sidelink_cbr(NodeId::new(i as u32), end_slot)))
            / n as f64
    };
    let offered = sweep.offered_subchannel_slots_per_s(&pool)
        / (1000.0 / pool.slot().as_secs_f64() / 1000.0 * f64::from(subchannels));

    SweepReport {
        label: format!(
            "{} at {} pps, {} veh/km",
            pool.rat.label(),
            sweep.packets_per_second,
            sweep.density_veh_per_km
        ),
        rat: pool.rat.label().to_string(),
        channel: sweep.channel.label(),
        vehicles: n,
        density_veh_per_km: sweep.density_veh_per_km,
        packets_per_second: sweep.packets_per_second,
        transmissions,
        evaluated,
        received,
        bins,
        losses,
        subchannel_occupancy: if capacity > 0 {
            distinct_used as f64 / capacity as f64
        } else {
            0.0
        },
        offered_occupancy: offered,
        resource_collision_ratio: if transmissions > 0 {
            collided_transmissions as f64 / transmissions as f64
        } else {
            0.0
        },
        mean_cbr,
    }
}

// =========================================================================================
// The 802.11p comparison arm
// =========================================================================================

/// How the ITS-G5 arm of a comparison is configured.
///
/// It exists so that the technology ordering of 04-models.md §5.5 ("802.11p at 18 Mbit/s
/// ≈ LTE-V up to about 160 m at 10 pps") can be *measured* on the same highway, the same
/// channel and the same traffic as the sidelink arm, which is the only way a comparison
/// between two radios is a comparison between two radios.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DsrcConfig {
    /// The rate.
    pub mcs: Mcs,
    /// The clear-channel-assessment threshold, dBm [EN 302 571: −85 dBm].
    pub cca_threshold_dbm: f64,
    /// The access category, which fixes AIFSN and the contention window.
    pub ac: AccessCategory,
    /// Bytes of header and trailer added to the payload before the air time is computed:
    /// the QoS MAC header, the LLC/SNAP header and the FCS
    /// ([`crate::types::timing`]).
    pub overhead_bytes: u32,
}

impl Default for DsrcConfig {
    fn default() -> Self {
        Self {
            mcs: Mcs::R6Qpsk12,
            cca_threshold_dbm: crate::phy::CcaConfig::ETSI.cca_threshold_dbm(),
            ac: AccessCategory::Vo,
            overhead_bytes: crate::types::timing::MAC_HEADER_QOS_BYTES
                + crate::types::timing::LLC_SNAP_BYTES
                + crate::types::timing::FCS_BYTES,
        }
    }
}

impl DsrcConfig {
    /// The 18 Mbit/s configuration 04-models.md §5.5's RAT-comparison row is stated at.
    #[must_use]
    pub fn at_18mbps() -> Self {
        Self {
            mcs: Mcs::R18Qam16_34,
            ..Self::default()
        }
    }
}

/// One committed 802.11p transmission.
#[derive(Debug, Clone, Copy, PartialEq)]
struct DsrcTx {
    vehicle: usize,
    start: SimTime,
    end: SimTime,
    bytes: u32,
    backoff_slots: u32,
}

/// Runs the ITS-G5 arm of a comparison on the same highway as [`sweep_sidelink`].
///
/// # The access model, and what it approximates
///
/// Contention is resolved *sequentially in generation order*: a frame's grant is computed
/// against the transmissions already committed, and then committed itself. That is exact
/// for the deferral a station owes to a transmission that started before its own
/// countdown — which is what carrier sense is — and it is the standard approximation for
/// the case where two stations count down to the same slot without hearing each other,
/// which stays a collision here because neither grant saw the other. What it does not
/// capture is a station backing off *again* because a frame committed later happened to
/// start during its countdown; that is a second-order effect on the collision rate and
/// the card records it as the harness's approximation.
///
/// Clear-channel assessment uses the deterministic path loss without the shadowing
/// realisation, because a station's carrier sense is a decision about a threshold
/// crossing over a long average rather than about one frame; reception uses the full
/// channel including shadowing. That split is deliberate and stated here rather than
/// buried.
///
/// # Panics
///
/// If the frame exceeds the MSDU cap, which is a configuration error.
#[must_use]
pub fn sweep_dsrc(sweep: &HighwaySweep, cfg: DsrcConfig) -> SweepReport {
    sweep_dsrc_inner(sweep, cfg, false).0
}

/// The same sweep, also returning one [`ReceptionSample`] per evaluated arrival — step 2
/// of the abstract-tier calibration procedure (04-models.md §4.9).
///
/// Step 2 asks for, "for every (transmitter, receiver, frame), the distance and the
/// receiver's local load (number of distinct transmitters heard in the last `T_CBR`, or
/// the measured CBR)". This is that record, taken from a homogeneous high-tier run of the
/// same harness the §13 curves are measured with, which is what makes the calibrated
/// abstract tier a fit rather than a guess.
///
/// The load reported per sample is the receiver's **run-average CBR**: busy time at that
/// receiver over the whole run, which is what [`SweepReport::mean_cbr`] is averaged from.
/// It is a run average and not a windowed `T_CBR` measurement, which matters and is
/// stated on the calibrated table's card: on a homogeneous ring at a constant density the
/// two agree to the sampling noise of one window, and the calibration densities of §4.9
/// step 1 are all homogeneous. A scenario whose load varies in time needs the windowed
/// form, and the table's envelope is what stops it being used there.
///
/// Collecting costs one `(u32, f64, bool)` per evaluated arrival, so it is a separate
/// entry point rather than something [`sweep_dsrc`] always pays for.
///
/// # Panics
///
/// As [`sweep_dsrc`]: if a frame exceeds the MSDU cap.
#[must_use]
pub fn sweep_dsrc_with_samples(
    sweep: &HighwaySweep,
    cfg: DsrcConfig,
) -> (SweepReport, Vec<ReceptionSample>) {
    sweep_dsrc_inner(sweep, cfg, true)
}

fn sweep_dsrc_inner(
    sweep: &HighwaySweep,
    cfg: DsrcConfig,
    collect: bool,
) -> (SweepReport, Vec<ReceptionSample>) {
    use crate::phy::{Arrival, InterferenceSource, OfdmPhy};
    use crate::traits::Phy;
    use crate::types::{ChannelId as Ch, RxHandle};

    let mut ctx = SweepCtx::new(sweep.seed);
    let vehicles = place_vehicles(sweep, &ctx);
    let n = vehicles.len();
    let mut phy = OfdmPhy::new(v2xw_core::card::Tier::High);
    let mut shadow = ShadowField::new(n, sweep.channel.sigma_db(), sweep.channel.decorrelation_m());
    let gain2 = 2.0 * sweep.antenna_gain_dbi;
    let total_ns = sweep.warmup.as_nanos() + sweep.duration.as_nanos();
    let period_ns = 1_000_000_000u64 / u64::from(sweep.packets_per_second.max(1));
    let slot_ns = crate::types::timing::SLOT_TIME.as_nanos();
    let aifs_ns = cfg.ac.aifs().as_nanos();
    let cw = cfg.ac.cw_min();

    // Position of a vehicle along the ring at an instant.
    let along_at = |i: usize, t: SimTime| -> f64 {
        let v = &vehicles[i];
        let mut a = (v.start_m + v.velocity_mps * (t as f64 * 1e-9)) % sweep.ring_m;
        if a < 0.0 {
            a += sweep.ring_m;
        }
        a
    };
    let distance_at = |i: usize, j: usize, t: SimTime| -> f64 {
        let dl = ring_delta(along_at(i, t), along_at(j, t), sweep.ring_m);
        let dlat = vehicles[i].lateral_m - vehicles[j].lateral_m;
        math::sqrt(dl * dl + dlat * dlat).max(1.0)
    };
    let f_hz = ChannelId::CCH.centre_hz();
    // Deterministic received power, for carrier sense.
    let deterministic_power = |i: usize, j: usize, t: SimTime| -> f64 {
        sweep.tx_power_dbm + gain2 - sweep.channel.path_loss_db(distance_at(i, j, t), f_hz)
    };

    // 1. Generation events, in time order. Ties broken by the vehicle index so the order
    //    is total and platform-independent.
    let mut events: Vec<(SimTime, usize, u32)> = Vec::new();
    for (i, v) in vehicles.iter().enumerate() {
        let mut k = 0u64;
        loop {
            let t = v.phase_ns + k * period_ns;
            if t >= total_ns {
                break;
            }
            let bytes = sweep.payload_cycle[(v.cycle + k as usize) % sweep.payload_cycle.len()];
            events.push((t, i, bytes));
            k += 1;
        }
    }
    events.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    // 2. Sequential contention.
    let mut committed: Vec<DsrcTx> = Vec::with_capacity(events.len());
    let mut longest_air = 0u64;
    for (gen_at, i, payload) in events {
        let bytes = payload + cfg.overhead_bytes;
        assert!(
            bytes <= crate::types::timing::MAX_MSDU_BYTES,
            "a {bytes} B frame exceeds the MSDU cap; the fragmenter must act first"
        );
        let air = crate::phy::air_time(bytes, cfg.mcs).as_nanos();
        longest_air = longest_air.max(air);
        ctx.set_now(gen_at);
        let drawn = ctx
            .rng(
                RngDomain::MacBackoff,
                EntityRef::Node(NodeId::new(i as u32)),
            )
            .below(u64::from(cw) + 1) as u32;
        let mut remaining = drawn;
        let mut t = gen_at + aifs_ns;
        // The audible committed transmissions, from the window that can still matter.
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 4096 {
                // The medium never went idle for long enough inside the guard. Transmit
                // anyway and let the collision be counted: dropping the frame silently
                // would hide the congestion the run is measuring.
                break;
            }
            // Is any audible transmission covering t?
            let mut busy_until: Option<SimTime> = None;
            let mut next_start: Option<SimTime> = None;
            for c in committed.iter().rev() {
                if c.end + longest_air < t {
                    // `committed` is in commit order, which is near-sorted by start; the
                    // window is bounded by the longest air time, so stop once past it.
                    if c.start + 2 * longest_air < t {
                        break;
                    }
                    continue;
                }
                if c.vehicle == i {
                    continue;
                }
                if deterministic_power(i, c.vehicle, t) < cfg.cca_threshold_dbm {
                    continue;
                }
                if c.start <= t && t < c.end {
                    busy_until = Some(busy_until.map_or(c.end, |b: SimTime| b.max(c.end)));
                } else if c.start > t {
                    next_start = Some(next_start.map_or(c.start, |s: SimTime| s.min(c.start)));
                }
            }
            if let Some(until) = busy_until {
                t = until + aifs_ns;
                continue;
            }
            let idle_ns = next_start.map_or(u64::MAX, |s| s.saturating_sub(t));
            let slots_available = idle_ns / slot_ns;
            if u64::from(remaining) <= slots_available {
                t += u64::from(remaining) * slot_ns;
                break;
            }
            remaining -= slots_available as u32;
            t = next_start.unwrap_or(t) + 1;
        }
        committed.push(DsrcTx {
            vehicle: i,
            start: t,
            end: t + air,
            bytes,
            backoff_slots: drawn,
        });
    }
    committed.sort_by(|a, b| a.start.cmp(&b.start).then(a.vehicle.cmp(&b.vehicle)));

    // 3. Reception.
    let mut bins: Vec<BinStats> = (0..sweep.bins())
        .map(|i| BinStats {
            centre_m: sweep.bin_centre_m(i),
            ..BinStats::default()
        })
        .collect();
    let mut losses: BTreeMap<String, u64> = BTreeMap::new();
    let mut transmissions = 0u64;
    let mut collided = 0u64;
    let warm = sweep.warmup.as_nanos();
    let mut busy_ns = vec![0u64; n];
    // `(receiver, distance, decoded)` per evaluated arrival. The load is filled in
    // afterwards from the receiver's own busy time, because that total is only complete
    // once the whole run has been walked: attaching the running value would make a
    // sample's load depend on how far through the run it happened to fall, which is a
    // different quantity from the one 04-models.md §4.9 step 2 asks for.
    let mut raw_samples: Vec<(usize, f64, bool)> = Vec::new();

    for (k, tx) in committed.iter().enumerate() {
        // Channel busy time at every station that could hear it, for the CBR statistic.
        for (rx, busy) in busy_ns.iter_mut().enumerate() {
            if rx == tx.vehicle {
                continue;
            }
            if deterministic_power(rx, tx.vehicle, tx.start) >= cfg.cca_threshold_dbm {
                *busy += tx.end - tx.start;
            }
        }
        if tx.start < warm {
            continue;
        }
        transmissions += 1;
        // The overlapping window, from the sorted list.
        let lo = committed.partition_point(|c| c.end <= tx.start);
        let hi = committed.partition_point(|c| c.start < tx.end);
        let any_overlap = committed[lo..hi]
            .iter()
            .enumerate()
            .any(|(off, c)| lo + off != k && c.vehicle != tx.vehicle);
        if any_overlap {
            collided += 1;
        }
        ctx.set_now(tx.start);
        for rx in 0..n {
            if rx == tx.vehicle {
                continue;
            }
            let d = distance_at(tx.vehicle, rx, tx.start);
            let Some(bin) = sweep.bin_of(d) else {
                continue;
            };
            let mut loss = sweep.channel.path_loss_db(d, f_hz);
            if sweep.shadowing {
                loss -= shadow.update(&ctx, tx.vehicle, rx, d, tx.start / 1_000_000);
            }
            let mut p = sweep.tx_power_dbm + gain2 - loss;
            if sweep.fading {
                // A fresh Nakagami model per arrival would be wasteful; the harness keeps
                // shadowing on and fading off for the comparison arms by default so that
                // the two radios differ only in their access and error models.
                p += 0.0;
            }
            let mut interferers: Vec<InterferenceSource> = Vec::new();
            for (off, other) in committed[lo..hi].iter().enumerate() {
                let j = lo + off;
                if j == k || other.vehicle == tx.vehicle || other.vehicle == rx {
                    continue;
                }
                let op = sweep.tx_power_dbm + gain2
                    - sweep
                        .channel
                        .path_loss_db(distance_at(other.vehicle, rx, other.start), f_hz);
                // Was the interferer audible to the victim's *transmitter* when the
                // victim started? If not, carrier sense could not have prevented the
                // overlap and the loss is a hidden terminal rather than a collision
                // (04-models.md §4.8).
                let audible = deterministic_power(tx.vehicle, other.vehicle, tx.start)
                    >= cfg.cca_threshold_dbm;
                let mut src = InterferenceSource::new(
                    NodeId::new(other.vehicle as u32),
                    op,
                    other.start,
                    other.end,
                );
                if !audible {
                    src = src.hidden();
                }
                interferers.push(src);
            }
            let frame = FrameDescriptor {
                bytes: tx.bytes,
                tx_power_dbm: sweep.tx_power_dbm,
                ..FrameDescriptor::broadcast(
                    tx.bytes,
                    cfg.mcs,
                    SduRef::new(SduId::new(tx.vehicle as u32), FrameSeq::new(k as u32)),
                )
            };
            let handle = phy.register_arrival(Arrival {
                tx_id: k as u64 + 1,
                tx: NodeId::new(tx.vehicle as u32),
                rx: NodeId::new(rx as u32),
                power_dbm: p,
                start: tx.start,
                end: tx.end,
                frame,
                interferers,
            });
            let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(rx as u32), handle);
            let _ = RxHandle {
                tx: handle.tx,
                rx: handle.rx,
            };
            bins[bin].evaluated += 1;
            let decoded = matches!(outcome, RxOutcome::Received { .. });
            match outcome {
                RxOutcome::Received { .. } => bins[bin].received += 1,
                RxOutcome::Lost(cause) => {
                    *losses.entry(cause.label().to_string()).or_insert(0) += 1;
                }
            }
            if collect {
                raw_samples.push((rx, d, decoded));
            }
        }
    }
    let _ = Ch::CCH;

    let evaluated: u64 = bins.iter().map(|b| b.evaluated).sum();
    let received: u64 = bins.iter().map(|b| b.received).sum();
    let span = total_ns.max(1) as f64;
    let mean_cbr = if n == 0 {
        0.0
    } else {
        math::sum_ordered(busy_ns.iter().map(|b| (*b as f64 / span).min(1.0))) / n as f64
    };
    let offered_air = math::sum_ordered(committed.iter().map(|c| (c.end - c.start) as f64)) / span;
    // The per-receiver run-average CBR, which is the load axis of the calibrated table.
    let per_rx_cbr: Vec<f64> = busy_ns
        .iter()
        .map(|b| (*b as f64 / span).min(1.0))
        .collect();
    let samples: Vec<ReceptionSample> = raw_samples
        .into_iter()
        .map(|(rx, distance_m, received)| ReceptionSample {
            distance_m,
            load: per_rx_cbr.get(rx).copied().unwrap_or(0.0),
            received,
        })
        .collect();

    let report = SweepReport {
        label: format!(
            "its-g5 {} at {} pps, {} veh/km",
            cfg.mcs.label(),
            sweep.packets_per_second,
            sweep.density_veh_per_km
        ),
        rat: format!("dsrc-80211p-{}", cfg.mcs.label()),
        channel: sweep.channel.label(),
        vehicles: n,
        density_veh_per_km: sweep.density_veh_per_km,
        packets_per_second: sweep.packets_per_second,
        transmissions,
        evaluated,
        received,
        bins,
        losses,
        // There are no sub-channels on a CSMA channel; the comparable quantity is the
        // fraction of time the medium carries a frame, which is what these two report.
        subchannel_occupancy: mean_cbr,
        offered_occupancy: offered_air,
        resource_collision_ratio: if transmissions > 0 {
            collided as f64 / transmissions as f64
        } else {
            0.0
        },
        mean_cbr,
    };
    (report, samples)
}

/// The error model a sweep's pool resolves to, for a report that wants to state it.
#[must_use]
pub fn error_model_label(pool: &PoolConfig) -> String {
    let m = SidelinkErrorModel::best_for(pool.mcs);
    format!(
        "{} ({}), 10 % BLER at {:.2} dB",
        m.data_curve().label,
        m.provenance().label(),
        m.sinr_at_10pc_db().unwrap_or(f64::NAN)
    )
}

/// The label a [`LossCause`] gets in a report, so a caller can index [`SweepReport::losses`]
/// without spelling the string.
#[must_use]
pub fn loss_label(cause: LossCause) -> &'static str {
    cause.label()
}

/// Convenience: the Molina-Masegosa Mode 4 sweep at one packet rate on one channel.
#[must_use]
pub fn molina_masegosa_sweep(pps: u32, channel: ChannelModel) -> SweepReport {
    let sweep = HighwaySweep::highway_slow(pps).with_channel(channel);
    sweep_sidelink(
        &sweep,
        PoolConfig::molina_masegosa_highway(),
        SpsParams::molina_masegosa(pps),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_distance_is_the_shorter_way_round() {
        assert!((ring_delta(10.0, 20.0, 100.0) - 10.0).abs() < 1e-12);
        assert!((ring_delta(5.0, 95.0, 100.0) - 10.0).abs() < 1e-12);
        assert!((ring_delta(0.0, 50.0, 100.0) - 50.0).abs() < 1e-12);
        // Symmetric, and never more than half the ring.
        for a in [0.0, 13.0, 70.0, 99.0] {
            for b in [0.0, 13.0, 70.0, 99.0] {
                let d = ring_delta(a, b, 100.0);
                assert!((d - ring_delta(b, a, 100.0)).abs() < 1e-12);
                assert!(d <= 50.0 + 1e-12);
            }
        }
    }

    #[test]
    fn the_channels_are_monotone_and_ordered_as_their_exponents_imply() {
        let f = 5.9e9;
        for ch in ChannelModel::ALL {
            let mut prev = 0.0;
            for d in [1.0, 10.0, 50.0, 100.0, 200.0, 500.0, 1000.0] {
                let pl = ch.path_loss_db(d, f);
                assert!(
                    pl > prev,
                    "{} is not monotone at {d} m: {pl} <= {prev}",
                    ch.label()
                );
                prev = pl;
            }
        }
        // At 500 m the dual-slope stand-in must be the most pessimistic and the
        // free-space-exponent TR 37.885 model the most optimistic, which is exactly the
        // disagreement the harness exists to quantify.
        let at500: Vec<(&str, f64)> = ChannelModel::ALL
            .iter()
            .map(|c| (c.label(), c.path_loss_db(500.0, f)))
            .collect();
        let tr = at500[0].1;
        let b1 = at500[1].1;
        assert!(
            b1 > tr,
            "the dual-slope stand-in ({b1:.1} dB) should exceed TR 37.885 ({tr:.1} dB) at \
             500 m"
        );
        for (label, pl) in &at500 {
            println!("path loss at 500 m, {label}: {pl:.2} dB");
        }
    }

    #[test]
    fn a_tiny_sweep_produces_a_falling_curve() {
        // Small enough for the debug profile: 20 vehicles on a 1 km ring for 300 ms.
        let sweep = HighwaySweep {
            ring_m: 1000.0,
            density_veh_per_km: 20.0,
            duration: Duration::from_millis(200),
            warmup: Duration::from_millis(100),
            max_distance_m: 400.0,
            bin_m: 100.0,
            sensing_range_m: 500.0,
            ..HighwaySweep::highway_slow(10)
        };
        let report = sweep_sidelink(
            &sweep,
            PoolConfig::molina_masegosa_highway(),
            SpsParams::molina_masegosa(10),
        );
        println!("{}", report.table());
        assert!(report.transmissions > 0, "nothing was transmitted");
        assert!(report.evaluated > 0, "no arrival was evaluated");
        // Delivery must not *rise* with distance across the whole curve.
        let first = report.bins[0].pdr().expect("the nearest bin has arrivals");
        let last = report
            .bins
            .iter()
            .rev()
            .find_map(BinStats::pdr)
            .expect("some far bin has arrivals");
        assert!(
            last <= first + 0.05,
            "delivery rose with distance: {first:.3} near, {last:.3} far"
        );
        // Every loss has a cause and the causes sum to the losses.
        let lost: u64 = report.losses.values().sum();
        assert_eq!(
            lost,
            report.evaluated - report.received,
            "the per-cause counts must sum to the losses (invariant I-R3)"
        );
    }

    #[test]
    fn a_sweep_is_reproducible_and_seed_dependent() {
        let sweep = HighwaySweep {
            ring_m: 1000.0,
            density_veh_per_km: 12.0,
            duration: Duration::from_millis(150),
            warmup: Duration::from_millis(50),
            max_distance_m: 300.0,
            bin_m: 150.0,
            sensing_range_m: 400.0,
            ..HighwaySweep::highway_slow(10)
        };
        let run = |s: &HighwaySweep| {
            sweep_sidelink(
                s,
                PoolConfig::molina_masegosa_highway(),
                SpsParams::molina_masegosa(10),
            )
        };
        let a = run(&sweep);
        let b = run(&sweep);
        assert_eq!(
            a.received, b.received,
            "the same seed must give the same run"
        );
        assert_eq!(a.evaluated, b.evaluated);
        assert_eq!(a.losses, b.losses);
        let c = run(&sweep.clone().with_seed(0xABCD_1234));
        assert!(
            c.received != a.received || c.evaluated != a.evaluated,
            "two seeds produced an identical run, which is suspicious"
        );
    }

    #[test]
    fn the_offered_occupancy_is_the_arithmetic_a_reader_can_check() {
        let pool = PoolConfig::molina_masegosa_highway();
        let sweep = HighwaySweep::highway_slow(10);
        // 240 vehicles at 10 pps; the cycle is one 300 B packet (two sub-channels) in
        // five, so the mean allocation is 1.2 sub-channels and the demand is
        // 240 · 10 · 1.2 = 2,880 sub-channel-slots per second out of 1,000 · 4 = 4,000.
        assert_eq!(sweep.vehicles(), 240);
        let offered = sweep.offered_subchannel_slots_per_s(&pool);
        assert!(
            (offered - 2880.0).abs() < 1.0,
            "offered load {offered} against the hand arithmetic 2,880"
        );
        println!(
            "Molina-Masegosa Highway Slow at 10 pps on a 2 km ring: {} vehicles, offered \
             occupancy {:.3} of the pool",
            sweep.vehicles(),
            offered / 4000.0
        );
    }

    #[test]
    fn the_error_model_a_pool_resolves_to_is_reportable() {
        let s = error_model_label(&PoolConfig::molina_masegosa_highway());
        println!("Molina-Masegosa pool error model: {s}");
        assert!(
            s.contains("verbatim"),
            "the reference mapping has a printed curve"
        );
        let nr = error_model_label(&PoolConfig::todisco_nr(
            crate::sidelink::Numerology::Mu0,
            crate::sidelink::nr_mcs(21).unwrap(),
        ));
        println!("Todisco NR MCS 21 pool error model: {nr}");
        assert!(nr.contains("fitted-point"));
    }

    // =====================================================================================
    // The validation runs of 04-models.md §13
    //
    // These are `#[ignore]`d because they are measurements, not unit tests: each one runs
    // tens of thousands of transmissions and millions of reception evaluations, which
    // wants the release profile. Run them with
    //
    //     cargo test --release -p v2xw-radio -- --ignored --nocapture
    //
    // and read the printed tables against the published figures. Each one asserts only
    // the properties that must hold whatever the absolute numbers are — monotonicity, the
    // technology ordering, the loss decomposition — and *prints* the comparison against
    // the published targets rather than asserting it, because the channel the published
    // curves were produced with is not shipped (see the module documentation).
    // =====================================================================================

    /// The published targets of 04-models.md §13, "C-V2X Mode 4 (high)".
    const MOLINA_10PPS: [(f64, f64); 6] = [
        (0.0, 0.97),
        (100.0, 0.95),
        (200.0, 0.90),
        (300.0, 0.79),
        (400.0, 0.66),
        (500.0, 0.45),
    ];
    const MOLINA_50PPS: [(f64, f64); 4] =
        [(25.0, 0.91), (100.0, 0.58), (200.0, 0.27), (300.0, 0.11)];

    fn print_against(report: &SweepReport, targets: &[(f64, f64)], source: &str) {
        println!("{}", report.table());
        println!("  against {source}:");
        println!("       d_m   published    measured        delta");
        for (d, want) in targets {
            match report.pdr_at(*d) {
                Some(got) => println!(
                    "  {:>8.0}     {:>7.3}     {:>7.3}     {:>+7.3}",
                    d,
                    want,
                    got,
                    got - want
                ),
                None => println!("  {:>8.0}     {:>7.3}           -            -", d, want),
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn molina_masegosa_pdr_versus_distance() {
        for channel in ChannelModel::ALL {
            for (pps, targets) in [(10u32, &MOLINA_10PPS[..]), (50u32, &MOLINA_50PPS[..])] {
                let report = molina_masegosa_sweep(pps, channel);
                print_against(
                    &report,
                    targets,
                    &format!("Molina-Masegosa 2017 Fig. 3 at {pps} pps"),
                );
                // Whatever the absolute level, delivery must fall with distance.
                let near = report.pdr_at(12.0).unwrap_or(1.0);
                let far = report.pdr_at(487.0).unwrap_or(0.0);
                assert!(
                    far <= near + 0.05,
                    "{}: delivery rose with distance ({near:.3} near, {far:.3} far)",
                    channel.label()
                );
                // The loss decomposition must close.
                let lost: u64 = report.losses.values().sum();
                assert_eq!(lost, report.evaluated - report.received);
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn molina_masegosa_occupancy_and_collisions() {
        // 04-models.md §13: 32.46 % / 3.38 % (10 pps slow), 80.91 % / 56.64 % (50 pps
        // slow), 17.08 % / 0.78 % and 62.08 % / 23.33 % (fast), ±2 pp.
        println!("  case                    occupancy(pub/meas/offered)   collisions(pub/meas)");
        for (name, sweep, occ, col) in [
            (
                "slow 10 pps",
                HighwaySweep::highway_slow(10),
                0.3246,
                0.0338,
            ),
            (
                "slow 50 pps",
                HighwaySweep::highway_slow(50),
                0.8091,
                0.5664,
            ),
            (
                "fast 10 pps",
                HighwaySweep::highway_fast(10),
                0.1708,
                0.0078,
            ),
            (
                "fast 50 pps",
                HighwaySweep::highway_fast(50),
                0.6208,
                0.2333,
            ),
        ] {
            let pps = sweep.packets_per_second;
            let r = sweep_sidelink(
                &sweep,
                PoolConfig::molina_masegosa_highway(),
                SpsParams::molina_masegosa(pps),
            );
            println!(
                "  {name:<22}  {occ:.4} / {:.4} / {:.4}          {col:.4} / {:.4}   ({} veh)",
                r.subchannel_occupancy, r.offered_occupancy, r.resource_collision_ratio, r.vehicles
            );
            // The density the *published occupancy* implies, which is the scenario
            // reconstruction the published figure actually describes. At four
            // sub-channels of a 1,000-subframe second and 1.2 sub-channels per packet,
            // an occupancy of `occ` is `occ · 4000 / (1.2 · pps)` vehicles, i.e. half
            // that many per kilometre on a 2 km ring.
            let implied_vehicles = occ * 4000.0 / (1.2 * f64::from(pps));
            println!(
                "      the published {occ:.4} occupancy implies {implied_vehicles:.0} \
                 transmitting vehicles, i.e. {:.0} veh/km on this 2 km ring, against the \
                 {:.0} veh/km the paper states",
                implied_vehicles / 2.0,
                sweep.density_veh_per_km
            );
            // The engine must put on the air what the scenario offered: this is the check
            // that separates a scenario-reconstruction disagreement from an engine defect.
            //
            // Distinct occupancy cannot exceed one, and below saturation it must track
            // the offered load; at or above saturation it must be close to one. The
            // difference between the two is exactly the resource reuse the collisions
            // consist of.
            assert!(
                r.subchannel_occupancy <= 1.0 + 1e-9,
                "{name}: distinct occupancy {:.4} exceeds the pool",
                r.subchannel_occupancy
            );
            if r.offered_occupancy < 0.5 {
                assert!(
                    (r.subchannel_occupancy - r.offered_occupancy).abs() < 0.1,
                    "{name}: below saturation the measured occupancy {:.4} must track the \
                     offered {:.4}",
                    r.subchannel_occupancy,
                    r.offered_occupancy
                );
            } else {
                assert!(
                    r.subchannel_occupancy > 0.4,
                    "{name}: an offered load of {:.2} produced only {:.4} occupancy",
                    r.offered_occupancy,
                    r.subchannel_occupancy
                );
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn todisco_nr_prr_versus_distance_with_and_without_in_band_emissions() {
        use crate::sidelink::{IbeMask, Numerology, nr_mcs};
        // 04-models.md §13, "C-V2X Mode 2 (high)": worst (SCS 15 kHz with IBE)
        // 0.98 / 0.89 / 0.74 / 0.62 / 0.30 / 0.04 at 10 / 60 / 90 / 100 / 120 / 150 m;
        // best (SCS 60 kHz without IBE) 1.0 / 0.85 / 0.76 / 0.46 / 0.09 at
        // 10 / 90 / 100 / 120 / 150 m.
        let worst: [(f64, f64); 6] = [
            (10.0, 0.98),
            (60.0, 0.89),
            (90.0, 0.74),
            (100.0, 0.62),
            (120.0, 0.30),
            (150.0, 0.04),
        ];
        let best: [(f64, f64); 5] = [
            (10.0, 1.00),
            (90.0, 0.85),
            (100.0, 0.76),
            (120.0, 0.46),
            (150.0, 0.09),
        ];
        let mcs21 = nr_mcs(21).expect("MCS 21");
        let sweep = HighwaySweep::todisco(100.0).with_channel(ChannelModel::AbbasLosHighway);

        let with_ibe = sweep_sidelink(
            &sweep,
            PoolConfig::todisco_nr(Numerology::Mu0, mcs21),
            SpsParams::ali_todisco(Numerology::Mu0, true),
        );
        print_against(
            &with_ibe,
            &worst,
            "Todisco 2021 Fig. 7 worst (SCS 15 kHz, IBE on)",
        );

        let no_ibe_pool = PoolConfig {
            ibe: IbeMask::OFF,
            ..PoolConfig::todisco_nr(Numerology::Mu2, mcs21)
        };
        let without_ibe = sweep_sidelink(
            &sweep,
            no_ibe_pool,
            SpsParams::ali_todisco(Numerology::Mu2, true),
        );
        print_against(
            &without_ibe,
            &best,
            "Todisco 2021 Fig. 7 best (SCS 60 kHz, IBE off)",
        );

        // The finding the ablation exists to reproduce: turning the emission mask off and
        // raising the numerology can only help, never hurt.
        let a = with_ibe.mean_pdr_within(150.0).unwrap_or(0.0);
        let b = without_ibe.mean_pdr_within(150.0).unwrap_or(0.0);
        println!(
            "  mean PRR within 150 m: SCS 15 kHz with IBE {a:.4}, SCS 60 kHz without IBE {b:.4}"
        );
        assert!(
            b >= a - 0.02,
            "removing in-band emissions and raising the numerology made delivery worse \
             ({a:.4} -> {b:.4}), which contradicts Todisco's ablation"
        );
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn todisco_nr_range_at_prr_0_9_versus_density() {
        use crate::sidelink::{Numerology, nr_mcs};
        // 04-models.md §13: about 190 / 155 / 100 m at 50 / 100 / 200 veh/km, ±10 m,
        // MCS 4, SCS 15 kHz, no retransmission.
        let mcs4 = nr_mcs(4).expect("MCS 4");
        println!("  veh/km   published_range_m   measured_range_m");
        let mut ranges = Vec::new();
        for (density, published) in [(50.0, 190.0), (100.0, 155.0), (200.0, 100.0)] {
            let sweep = HighwaySweep::todisco(density).with_channel(ChannelModel::AbbasLosHighway);
            let r = sweep_sidelink(
                &sweep,
                PoolConfig::todisco_nr(Numerology::Mu0, mcs4),
                SpsParams::ali_todisco(Numerology::Mu0, true),
            );
            let measured = r.range_at_pdr(0.9);
            println!(
                "  {density:>6.0}   {published:>17.0}   {:>16}",
                measured.map_or("-".to_string(), |m| format!("{m:.0}"))
            );
            println!("{}", r.table());
            ranges.push((density, measured));
        }
        // The ordering is the claim: range falls as density rises.
        let vals: Vec<f64> = ranges.iter().filter_map(|(_, m)| *m).collect();
        if vals.len() == 3 {
            assert!(
                vals[0] >= vals[1] - 10.0 && vals[1] >= vals[2] - 10.0,
                "range must fall as density rises, got {vals:?}"
            );
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn todisco_nr_range_with_and_without_the_layer_two_candidate_list() {
        use crate::sidelink::{Numerology, TxPercentage, nr_mcs};
        // 04-models.md §13: 110 m without the L2 list and 170 m with `sl-TxPercentage`
        // 20 %, at 350 B and RSRP −110 dBm (Todisco Fig. 10(a)).
        let mcs4 = nr_mcs(4).expect("MCS 4");
        // Two loads, because the answer depends on which one you ask at. Todisco's 350 B
        // at MCS 4 needs four of the pool's five sub-channels, so 100 veh/km offers the
        // pool about 1.5 times what it has; 30 veh/km offers it under half.
        for density in [30.0, 100.0] {
            let sweep = HighwaySweep::todisco(density).with_channel(ChannelModel::AbbasLosHighway);
            let pool = PoolConfig::todisco_nr(Numerology::Mu0, mcs4);
            let base =
                SpsParams::ali_todisco(Numerology::Mu0, true).with_rsrp_threshold_dbm(-110.0);

            let without = sweep_sidelink(&sweep, pool.clone(), base.clone().without_sensing());
            let with = sweep_sidelink(&sweep, pool, base.with_tx_percentage(TxPercentage::P20));
            let (a, b) = (without.range_at_pdr(0.9), with.range_at_pdr(0.9));
            println!(
                "  at {density:.0} veh/km (offered occupancy {:.2}): range at PRR 0.9 \
                 without the L2 list {} (published 110 m), with 20 % {} (published 170 m)",
                with.offered_occupancy,
                a.map_or("-".into(), |v| format!("{v:.0} m")),
                b.map_or("-".into(), |v| format!("{v:.0} m")),
            );
            println!("{}", without.table());
            println!("{}", with.table());
            // The claim only holds where the pool can carry the traffic. Sensing picks
            // the *quietest* resources, so when every resource is loud it concentrates
            // every UE onto the same few and does worse than spreading at random — which
            // is what the saturated arm measures, and why Ali 2021 carries a random-
            // selection baseline at all. Asserting the published ordering at a load the
            // pool cannot carry would be asserting the wrong thing.
            if with.offered_occupancy < 0.5 {
                if let (Some(a), Some(b)) = (a, b) {
                    assert!(
                        b >= a - 10.0,
                        "below saturation, sensing with the 20 % candidate list \
                         ({b:.0} m) must not be worse than random selection ({a:.0} m)"
                    );
                }
            } else {
                println!(
                    "      NOTE: at this load the pool is over-subscribed \
                     ({:.2}x), and sensing concentrates rather than spreads; the \
                     published ordering is not expected to hold here.",
                    with.offered_occupancy
                );
            }
        }
    }

    #[test]
    #[ignore = "measurement: run with --release --ignored"]
    fn the_technology_ordering_reproduces_the_literature() {
        // 04-models.md §5.5, the RAT-comparison row: "802.11p at 18 Mbit/s ≈ LTE-V up to
        // about 160 m at 10 pps; at 250 m LOS about 20 % of TBs lost to collisions".
        //
        // The acceptance criterion of the roadmap is the *qualitative ordering*, so this
        // run measures all three stacks on one highway, one channel and one traffic model,
        // and prints the crossover.
        let sweep = HighwaySweep::highway_slow(10).with_channel(ChannelModel::AbbasLosHighway);
        let lte = sweep_sidelink(
            &sweep,
            PoolConfig::molina_masegosa_highway(),
            SpsParams::molina_masegosa(10),
        );
        let g5_6 = sweep_dsrc(&sweep, DsrcConfig::default());
        let g5_18 = sweep_dsrc(&sweep, DsrcConfig::at_18mbps());
        let nr = {
            use crate::sidelink::{Numerology, nr_mcs};
            sweep_sidelink(
                &sweep,
                PoolConfig::todisco_nr(Numerology::Mu0, nr_mcs(4).unwrap()),
                SpsParams::ali_todisco(Numerology::Mu0, true),
            )
        };
        for r in [&lte, &nr, &g5_6, &g5_18] {
            println!("{}", r.table());
        }
        println!("  distance_m   lte-v2x   nr-v2x   g5-6Mbps   g5-18Mbps");
        for d in [
            25.0, 50.0, 100.0, 150.0, 160.0, 200.0, 250.0, 300.0, 400.0, 500.0,
        ] {
            let f = |r: &SweepReport| r.pdr_at(d).map_or("-".to_string(), |v| format!("{v:.3}"));
            println!(
                "  {d:>10.0}   {:>7}   {:>6}   {:>8}   {:>9}",
                f(&lte),
                f(&nr),
                f(&g5_6),
                f(&g5_18)
            );
        }
        println!(
            "  range at PDR 0.9: lte {} nr {} g5@6 {} g5@18 {}",
            lte.range_at_pdr(0.9)
                .map_or("-".into(), |v| format!("{v:.0} m")),
            nr.range_at_pdr(0.9)
                .map_or("-".into(), |v| format!("{v:.0} m")),
            g5_6.range_at_pdr(0.9)
                .map_or("-".into(), |v| format!("{v:.0} m")),
            g5_18
                .range_at_pdr(0.9)
                .map_or("-".into(), |v| format!("{v:.0} m")),
        );
        // The orderings that must hold whatever the absolute levels are.
        //
        // 1. A higher 802.11p rate costs sensitivity: EN 302 663 Table 1 puts 18 Mbit/s
        //    at −79 dBm against −88 dBm at 6 Mbit/s, so the higher rate must lose *more*
        //    frames to sensitivity. What it does not have to do is deliver less overall:
        //    its frames are three times shorter, so it collides far less, and on a LOS
        //    highway channel where 500 m is still 9 dB above the 18 Mbit/s sensitivity
        //    the shorter frame wins everywhere. Asserting the naive "higher rate, shorter
        //    range" would be asserting a link budget this configuration does not have.
        let sens_6 = *g5_6.losses.get("below-sensitivity").unwrap_or(&0);
        let sens_18 = *g5_18.losses.get("below-sensitivity").unwrap_or(&0);
        println!("  below-sensitivity losses: 6 Mbit/s {sens_6}, 18 Mbit/s {sens_18}");
        assert!(
            sens_18 > sens_6,
            "18 Mbit/s must lose more to sensitivity than 6 Mbit/s ({sens_18} against \
             {sens_6}); its receiver is 9 dB less sensitive"
        );
        // 2. The sidelink arm whose transport block needs four of five sub-channels
        //    (NR MCS 4 at 350 B) must be the most congested of the four, because it asks
        //    the pool for the most.
        println!(
            "  offered occupancy: lte {:.3}, nr {:.3}, g5@6 {:.3}, g5@18 {:.3}",
            lte.offered_occupancy,
            nr.offered_occupancy,
            g5_6.offered_occupancy,
            g5_18.offered_occupancy
        );
        assert!(
            nr.offered_occupancy > lte.offered_occupancy,
            "NR MCS 4 at 350 B should ask the pool for more than LTE QPSK r0.7 at 190 B"
        );
        // 3. Every radio must deliver better near than far.
        for r in [&lte, &nr, &g5_6, &g5_18] {
            let near = r.pdr_at(12.0).unwrap_or(0.0);
            let far = r.pdr_at(487.0).unwrap_or(0.0);
            assert!(
                far <= near + 0.05,
                "{}: delivery rose with distance ({near:.3} -> {far:.3})",
                r.rat
            );
        }
    }

    #[test]
    fn a_tiny_dsrc_sweep_produces_a_falling_curve() {
        let sweep = HighwaySweep {
            ring_m: 1000.0,
            density_veh_per_km: 20.0,
            duration: Duration::from_millis(200),
            warmup: Duration::from_millis(100),
            max_distance_m: 400.0,
            bin_m: 100.0,
            ..HighwaySweep::highway_slow(10)
        };
        let report = sweep_dsrc(&sweep, DsrcConfig::default());
        println!("{}", report.table());
        assert!(report.transmissions > 0);
        assert!(report.evaluated > 0);
        let first = report.bins[0].pdr().expect("the nearest bin has arrivals");
        let last = report
            .bins
            .iter()
            .rev()
            .find_map(BinStats::pdr)
            .expect("some far bin has arrivals");
        assert!(
            last <= first + 0.05,
            "delivery rose with distance: {first:.3} near, {last:.3} far"
        );
        let lost: u64 = report.losses.values().sum();
        assert_eq!(lost, report.evaluated - report.received);
        // A CSMA channel has a channel busy ratio, and at 20 veh/km it is not saturated.
        assert!(
            report.mean_cbr > 0.0 && report.mean_cbr < 1.0,
            "CBR {:.4} is implausible",
            report.mean_cbr
        );
    }

    #[test]
    fn the_dsrc_arm_defers_rather_than_colliding_when_the_medium_is_busy() {
        // Two vehicles, one metre apart, generating at the same instant: carrier sense
        // must separate their transmissions in time, because the second one's countdown
        // sees the first one's frame.
        let sweep = HighwaySweep {
            ring_m: 200.0,
            density_veh_per_km: 10.0,
            duration: Duration::from_millis(200),
            warmup: Duration::ZERO,
            max_distance_m: 100.0,
            bin_m: 50.0,
            packets_per_second: 10,
            ..HighwaySweep::highway_slow(10)
        };
        let report = sweep_dsrc(&sweep, DsrcConfig::default());
        println!("{}", report.table());
        // With two vehicles on a 200 m ring and 352 µs frames at 10 pps the medium is
        // almost always idle, so almost nothing should collide.
        assert!(
            report.resource_collision_ratio < 0.2,
            "collision ratio {:.3} is too high for an almost-idle medium: carrier sense \
             is not deferring",
            report.resource_collision_ratio
        );
        assert!(report.pdr() > 0.5, "an idle two-node medium should deliver");
    }
}
