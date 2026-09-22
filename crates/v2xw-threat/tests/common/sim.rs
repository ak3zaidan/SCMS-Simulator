//! An **attack-in-the-loop** simulation: the attacks and the detectors of this crate
//! wired into a running world, a real radio and real node runtimes.
//!
//! # Why this lives in `tests/`
//!
//! `v2xw-threat` has exactly one internal dependency — `v2xw-core` — because the crate
//! documentation's "the seam" says so: naming a node-runtime or engine type in the library
//! would couple the threat model to their churn. The wiring nevertheless has to exist and
//! has to be *run*, so it lives here on dev-dependencies.
//!
//! The models are the ones `v2xw_engine::wiring` selects for a `medium`-tier scenario, and
//! they are constructed the same way, which is why the comments below name the wiring
//! function each block corresponds to. The harness deliberately does **not** depend on
//! `v2xw-engine`: the engine's `EngineCtx::new` is `pub(crate)` so its loop cannot be
//! driven from outside anyway, and the dependency would make this crate's tests fail
//! whenever the engine is mid-edit.
//!
//! What this harness adds, and what `v2xw_engine::run` has no seam for yet, is two calls:
//!
//! 1. [`Attacker::act`] between `ObuRuntime::step` and the frame going on the air, so an
//!    attacker's edit reaches the channel rather than a post-processing pass;
//! 2. [`Detector::on_message`] on each node's **delivered** messages, so a detector sees
//!    what that node received and verified and nothing else.
//!
//! Everything between the two — signing latency, the AIFS, log-distance shadowing,
//! Nakagami fading, the PER draw, the node's queues and its verification policy — is the
//! ordinary path, so a detection rate measured here is a rate measured against modelled
//! propagation and a modelled receiver.
//!
//! # The firewall
//!
//! The harness is the **host**: it is the only thing here that knows an
//! [`v2xw_core::ids::ActorId`]. It calls [`log_actions`] (I-T3) and it declares the
//! subject→actor join to the metrics provider. The attacker gets an [`AttackerView`] and
//! the detector gets an [`ObservedMessage`]; neither can be built from ground truth, which
//! is the point.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use v2xw_core::card::Tier;
use v2xw_core::ctx::{Ctx, ErasedRecord, OwnedRecord};
use v2xw_core::event::{EventClass, EventHandle, Scheduler};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::{ActorId, LinkKey, NodeId};
use v2xw_core::kinematics::Kinematics;
use v2xw_core::provenance::{ProvSubject, ProvenanceLog};
use v2xw_core::registry::{ModelRef, ParamSet, ParamSetId};
use v2xw_core::rng::{EntityRef, RngDomain, RngGuard, RngRegistry};
use v2xw_core::time::{Duration, SimTime, secs_to_ns};
use v2xw_core::weather::WeatherState;
use v2xw_mobility::{
    ActorSnapshot, DriverProfile, GnssEnv, GnssModel, Mobility, MobilityUpdate, VehicleClass,
    VehicleView,
};
use v2xw_node::{NodeConfig, ObuRuntime, RxFrame, Transmission, VerificationPolicy};
use v2xw_radio::{Fading, LosResult, PerModel, Propagation, RadioEndpoint};
use v2xw_sec::hashedid::{id8_bytes, id8_from_bytes};
use v2xw_world::World;

use v2xw_threat::attack::{
    AttackAction, AttackKind, Attacker, AttackerView, Emission, HonestClaim, is_falsified,
    log_actions,
};
use v2xw_threat::attack_legacy::{LegacyAttacker, LegacyAttackerParams};
use v2xw_threat::capability::{AttackSchedule, Capabilities};
use v2xw_threat::detect::{Detector, DetectorId, DetectorParams, Legacy12, Verdict};
use v2xw_threat::ma::{MaParams, MaPipeline};
use v2xw_threat::obs::{
    LocalEnvironment, ObservedKind, ObservedMessage, SelfBelief, StationType, VerificationState,
};
use v2xw_threat::report::{CertValidity, Evidence, MisbehaviourReport};

/// The safety channel's centre frequency, as `v2xw_engine::run` fixes it.
const SAFETY_FREQ_HZ: f64 = 5.860e9;
/// The EDCA AC_VI AIFS on a 10 MHz OCB channel, as `v2xw_engine::run` fixes it.
const AIFS: Duration = Duration::from_micros(58);
/// The receiver's thermal noise floor on a 10 MHz channel, dBm.
const NOISE_FLOOR_DBM: f64 = -174.0 + 70.0 + 9.0;
/// The candidate range, equal to the snapshot's grid cell size.
const MAX_RANGE_M: f64 = 1000.0;
/// The transmit power every node runs at (`v2xw_engine::wiring::TX_POWER_DBM`).
const TX_POWER_DBM: f64 = 20.0;
/// How many pseudonyms a Sybil attacker's ghost set holds (`sybil_ghosts`, 6).
const GHOSTS: u32 = 6;

// --------------------------------------------------------------------------------------
// The context
// --------------------------------------------------------------------------------------

/// The harness's [`Ctx`]: what the engine's `EngineCtx` is, with a constructor this side
/// of the crate boundary can call.
///
/// The `Payload` type is `()` because the harness drives a fixed-step loop rather than a
/// heap: nothing a model here can reach schedules an event. A scheduler is nevertheless
/// held and forwarded to, so a model that *did* schedule would be served rather than
/// silently ignored.
pub struct HarnessCtx<'a> {
    now: SimTime,
    rng: &'a RngRegistry,
    world: &'a World,
    actors: &'a ActorSnapshot,
    scheduler: &'a mut Scheduler<()>,
    provenance: &'a mut ProvenanceLog,
    params: &'a ParamSet,
    out: &'a mut Vec<(SimTime, OwnedRecord)>,
    refused: u64,
}

impl Ctx for HarnessCtx<'_> {
    type World = World;
    type Actors = ActorSnapshot;
    type Payload = ();

    fn now(&self) -> SimTime {
        self.now
    }

    fn rng(&self, domain: RngDomain, entity: EntityRef) -> RngGuard<'_> {
        self.rng.checkout(domain, entity)
    }

    fn schedule(&mut self, at: SimTime, class: EventClass, payload: ()) -> EventHandle {
        self.scheduler.schedule(at, class, payload)
    }

    fn cancel(&mut self, handle: EventHandle) -> bool {
        self.scheduler.cancel(handle)
    }

    fn world(&self) -> &World {
        self.world
    }

    fn actors(&self) -> &ActorSnapshot {
        self.actors
    }

    fn emit_erased(&mut self, record: &dyn ErasedRecord) {
        let Ok(owned) = record.to_owned_record() else {
            self.refused += 1;
            return;
        };
        // The recorder's rule (03-interfaces.md §14), verbatim from `EngineCtx`: a
        // ground-truth record may not be written to a NODE channel.
        if owned.channel.starts_with("node.") && !owned.visibility.allowed_on_node_channel() {
            self.refused += 1;
            return;
        }
        let at = self.now;
        self.out.push((at, owned));
    }

    fn why(&mut self, subject: ProvSubject, model: ModelRef, params: ParamSetId) {
        self.provenance.record(subject, model, params);
    }

    fn params(&self) -> &ParamSet {
        self.params
    }
}

/// Builds a context over the harness's borrows. A macro rather than a function because
/// every call site holds a different subset of the state mutably.
macro_rules! hctx {
    ($now:expr, $rng:expr, $world:expr, $snap:expr, $sched:expr, $prov:expr, $params:expr, $out:expr) => {
        HarnessCtx {
            now: $now,
            rng: $rng,
            world: $world,
            actors: $snap,
            scheduler: $sched,
            provenance: $prov,
            params: $params,
            out: $out,
            refused: 0,
        }
    };
}

// --------------------------------------------------------------------------------------
// The node's map store
// --------------------------------------------------------------------------------------

/// The node's own map store, backed by the world's lane geometry.
///
/// A declared capability (07-threats §1, "Knowledge: map"), not a window on the world:
/// what it answers is the distance from a **claimed** coordinate to the nearest lane
/// centre, which is what a fielded receiver's HD map answers. It holds lane centre-line
/// points only — no actor, no truth.
pub struct LaneMap {
    /// Centre-line **segments** bucketed into [`LaneMap::CELL_M`] cells, so a lookup
    /// touches a bounded neighbourhood rather than every lane in the city.
    ///
    /// Segments and not vertices: a lane on a 120 m block has its two centre-line points
    /// 120 m apart, so a vertex-only map puts an honest vehicle mid-block 60 m from the
    /// nearest "road" and `mapOffRoad` scores 60 / 15 = 4 on every vehicle in the fleet.
    /// That is a map that is wrong, not a detector that is right, and it was found by
    /// looking at which check was accusing the honest vehicles.
    cells: BTreeMap<(i64, i64), Vec<[f64; 4]>>,
}

impl LaneMap {
    /// The bucket size, metres.
    pub const CELL_M: f64 = 25.0;
    /// The distance reported for a claim with no lane point in the searched
    /// neighbourhood.
    ///
    /// A *floor*, not a measurement: the search stops at three cells, so the honest answer
    /// is "further than this", and `offroad_tol_m` (15 m) is well inside it.
    pub const FAR_M: f64 = 100.0;

    /// Buckets every lane centre-line segment into the cells it passes through.
    pub fn of(world: &World) -> Self {
        let mut cells: BTreeMap<(i64, i64), Vec<[f64; 4]>> = BTreeMap::new();
        for lane in world.roads.lanes() {
            for w in lane.centreline.windows(2) {
                let seg = [w[0].x, w[0].y, w[1].x, w[1].y];
                let (x0, x1) = (w[0].x.min(w[1].x), w[0].x.max(w[1].x));
                let (y0, y1) = (w[0].y.min(w[1].y), w[0].y.max(w[1].y));
                let (cx0, cx1) = (
                    (x0 / Self::CELL_M).floor() as i64,
                    (x1 / Self::CELL_M).floor() as i64,
                );
                let (cy0, cy1) = (
                    (y0 / Self::CELL_M).floor() as i64,
                    (y1 / Self::CELL_M).floor() as i64,
                );
                for cx in cx0..=cx1 {
                    for cy in cy0..=cy1 {
                        cells.entry((cx, cy)).or_default().push(seg);
                    }
                }
            }
        }
        Self { cells }
    }

    /// The distance from a point to a segment.
    fn to_segment(px: f64, py: f64, s: &[f64; 4]) -> f64 {
        let (ax, ay, bx, by) = (s[0], s[1], s[2], s[3]);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 <= f64::EPSILON {
            0.0
        } else {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        };
        v2xw_core::math::hypot(px - (ax + t * dx), py - (ay + t * dy))
    }
}

impl LocalEnvironment for LaneMap {
    fn distance_to_road_m(&self, x_m: f64, y_m: f64) -> f64 {
        let cx = (x_m / Self::CELL_M).floor() as i64;
        let cy = (y_m / Self::CELL_M).floor() as i64;
        let mut best = f64::INFINITY;
        for dx in -3..=3i64 {
            for dy in -3..=3i64 {
                let Some(list) = self.cells.get(&(cx + dx, cy + dy)) else {
                    continue;
                };
                for seg in list {
                    let d = Self::to_segment(x_m, y_m, seg);
                    if d < best {
                        best = d;
                    }
                }
            }
        }
        if best.is_finite() { best } else { Self::FAR_M }
    }
}

// --------------------------------------------------------------------------------------
// Options and results
// --------------------------------------------------------------------------------------

/// How one harness run is configured.
#[derive(Debug, Clone)]
pub struct SimOptions {
    /// The world.
    pub grid: v2xw_world::procedural::GridParams,
    /// Simulated seconds.
    pub duration_s: f64,
    /// The mobility step, which is also the beacon interval.
    pub step_ms: u64,
    /// Vehicle arrival rate, veh/h.
    pub rate_veh_per_h: f64,
    /// A cap on total spawns, or `0` for none — the legacy `max_total_vehicles`.
    pub max_vehicles: u64,
    /// What share of vehicles are attackers.
    pub attacker_fraction: f64,
    /// The attack types the attacker population is drawn from, round-robin in this order
    /// — the legacy `attack_types` catalog rule.
    pub attacks: Vec<AttackKind>,
    /// The master seed.
    pub seed: u64,
    /// The probability a fired verdict becomes a report (`PipelineConfig.report_prob`).
    pub report_prob: f64,
    /// When the attackers start, seconds.
    pub attack_from_s: f64,
    /// When they stop, seconds.
    pub attack_to_s: f64,
    /// The share of benign vehicles whose GNSS is faulty — a sustained bias several times
    /// nominal (`PipelineConfig.faulty_pct`, 0.05). These are the honest vehicles a
    /// plausibility detector can legitimately false-positive on.
    pub faulty_fraction: f64,
    /// The receiver's configured range, metres, which the `acceptanceRangeThreshold` check
    /// compares against and which the lossless regime uses as its reception disc.
    pub radio_range_m: f64,
    /// **The ideal regime.** A node's belief is its own truth and the broadcast position
    /// confidence is zero, so every plausibility residual is the attacker's lie and
    /// nothing else. Matches the legacy engine run with `gps_sigma_m = 0`.
    pub perfect_belief: bool,
    /// The node's per-queue capacity. The engine default is 64, which is one second of
    /// arrivals from six neighbours at 10 Hz.
    pub queue_capacity: usize,
    /// **The ideal regime.** The detector is fed straight from the radio's inbox rather
    /// than from `StepOutcome::delivered`, so the receiver has unlimited verification
    /// capacity.
    ///
    /// This exists for one reason and it is a comparison reason: the legacy engine has no
    /// verification budget at all — every in-range beacon reaches its detection pass — so
    /// a like-for-like comparison has to give the ported receiver the same. With the
    /// budget on, the reference OBU's secure element publishes >110 signatures/s and a
    /// node in a sixty-vehicle fleet at 10 Hz is offered ~590/s, so most of what the
    /// channel delivers never reaches a detector. That gap is a *result* (the realistic
    /// regime measures it), not something to configure away, which is why this flag is
    /// off by default and the run report prints both numbers.
    pub bypass_node_compute: bool,
    /// **The ideal regime.** Reception is the legacy `radio_model = "disc"`: every node
    /// within [`Self::radio_range_m`] hears the frame, with no link budget and no
    /// packet-error draw.
    pub lossless: bool,
    /// Where to write an MCAP recording, or `None`.
    pub record: Option<PathBuf>,
    /// The detector thresholds.
    pub detector: DetectorParams,
    /// The authority's operating point.
    pub ma: MaParams,
}

impl Default for SimOptions {
    fn default() -> Self {
        Self {
            // A 13 x 13 lattice of 120 m blocks: 1440 m x 1440 m. This is the legacy
            // engine's own `road_network="grid", grid_w=13, grid_h=13, grid_block_m=120`,
            // so the cross-engine comparison in `tests/legacy_engine_compare.rs` puts the
            // two fleets on the same geometry rather than on two different cities. Two
            // lanes per direction and the 25 mph NYC citywide limit, as
            // `scenarios/phase1-grid.yaml` sets them.
            grid: v2xw_world::procedural::GridParams {
                cols: 13,
                rows: 13,
                block_x_m: 120.0,
                block_y_m: 120.0,
                lanes_per_direction: 2,
                lane_width_m: 3.5,
                sidewalk_m: 2.0,
                speed_limit_mps: 11.176,
                signalised: true,
                cycle_s: 60.0,
                amber_s: 3.0,
                crossings: false,
                block_buildings: false,
                building_height_m: 30.0,
                rsu_at_junctions: false,
                ..v2xw_world::procedural::GridParams::legacy()
            },
            duration_s: 60.0,
            step_ms: 100,
            rate_veh_per_h: 14_400.0,
            max_vehicles: 60,
            attacker_fraction: 0.25,
            attacks: AttackKind::LEGACY_CATALOG.to_vec(),
            seed: 0xC0FFEE_5EED,
            report_prob: 0.9,
            attack_from_s: 5.0,
            attack_to_s: 1.0e9,
            faulty_fraction: 0.05,
            radio_range_m: 500.0,
            queue_capacity: 64,
            bypass_node_compute: false,
            perfect_belief: false,
            lossless: false,
            record: None,
            detector: DetectorParams::default(),
            ma: MaParams::default(),
        }
    }
}

impl SimOptions {
    /// A run at the given seed offset, fleet rate and attacker fraction.
    pub fn new(seed: u64, rate_veh_per_h: f64, attacker_fraction: f64) -> Self {
        Self {
            seed: 0xC0FFEE_5EED ^ seed,
            rate_veh_per_h,
            attacker_fraction,
            ..Self::default()
        }
    }

    /// The same, narrowed to one attack type.
    pub fn with_attack(mut self, k: AttackKind) -> Self {
        self.attacks = vec![k];
        self
    }

    /// The ideal regime: noise-free belief, a hard reception disc, no faulty sensors and
    /// every fired verdict reported. What is left is the detector suite against the
    /// attacker's lie.
    pub fn ideal(mut self) -> Self {
        self.perfect_belief = true;
        self.lossless = true;
        self.faulty_fraction = 0.0;
        self.report_prob = 1.0;
        self.bypass_node_compute = true;
        self
    }
}

/// The repository root, from this crate's manifest directory.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// What one harness run produced.
#[derive(Debug, Clone, Default)]
pub struct SimOut {
    /// Every record, in emission order.
    pub records: Vec<(SimTime, OwnedRecord)>,
    /// Nodes created.
    pub nodes: u64,
    /// Attacker nodes.
    pub attackers: u64,
    /// The attack type each attacker node ran.
    pub attack_of: BTreeMap<NodeId, AttackKind>,
    /// Frames put on the air, ghosts included.
    pub frames: u64,
    /// Frames an attacker altered before they went on the air.
    pub falsified_frames: u64,
    /// Reception attempts evaluated.
    pub reception_attempts: u64,
    /// Receptions that closed.
    pub receptions: u64,
    /// Messages a node's runtime delivered to its applications — what the detectors saw.
    pub delivered: u64,
    /// Detector firings by check id.
    pub firings: BTreeMap<String, u64>,
    /// The highest score each check ever reached.
    pub peak: BTreeMap<String, f64>,
    /// Reports filed.
    pub reports: u64,
    /// The authority's revocations.
    pub revocations: u64,
    /// The subject→actor join the run declared, for every identity that actually
    /// transmitted. A pseudonym a node holds but never signs with is left out: declaring
    /// it would put a subject in the confusion matrix that no receiver could ever have
    /// heard of, and it would count as a false negative for ever.
    pub subject_actor: BTreeMap<String, ActorId>,
    /// The greatest distance at which a frame was received, metres. The
    /// `acceptanceRangeThreshold` check compares a claim against
    /// [`SimOptions::radio_range_m`]; when the propagation model reaches further than
    /// that, honest distant senders score above 1 and the run says so.
    pub max_rx_distance_m: f64,
    /// Receptions from beyond [`SimOptions::radio_range_m`].
    pub rx_beyond_nominal_range: u64,
    /// The nominal range the run configured, metres, so a report can print both.
    pub nominal_range_m: f64,
    /// How many times a receiver was handed two messages from one signer bearing the
    /// **same** believed arrival instant.
    ///
    /// This is the input the ported alpha-beta tracker has no guard for: its `dtk` hits
    /// its 1 ms floor and the velocity update `β·r/dtk` multiplies the residual by 300.
    /// The legacy engine cannot produce it — it delivers at most one beacon per sender per
    /// step — so the port inherits a division that was safe there and is not here.
    pub same_instant_repeats: u64,
    /// The largest `kalmanConsistency` score any message produced, unrounded.
    pub kalman_peak: f64,
    /// The actors that took an attack action that changed bytes on the air.
    pub true_attacker_actors: BTreeSet<ActorId>,
    /// Firings whose subject was a true attacker, by check id.
    pub firings_on_attacker: BTreeMap<String, u64>,
    /// Firings whose subject was an honest vehicle, by check id — the false accusations,
    /// attributed to the check that made them.
    pub firings_on_benign: BTreeMap<String, u64>,
    /// The record digest, for the determinism check.
    pub digest: String,
    /// Where the recording was written.
    pub recording: Option<PathBuf>,
}

impl SimOut {
    /// How many records landed on `channel`.
    pub fn count_on(&self, channel: &str) -> usize {
        self.records
            .iter()
            .filter(|(_, r)| r.channel == channel)
            .count()
    }

    /// The records on `channel`, decoded as `T`.
    pub fn decode<T: serde::de::DeserializeOwned>(&self, channel: &str) -> Vec<T> {
        self.records
            .iter()
            .filter(|(_, r)| r.channel == channel)
            .filter_map(|(_, r)| serde_json::from_slice::<T>(&r.json).ok())
            .collect()
    }
}

// --------------------------------------------------------------------------------------
// Internal state
// --------------------------------------------------------------------------------------

struct ActorRec {
    class: VehicleClass,
    driver: DriverProfile,
    node: Option<NodeId>,
    last: Kinematics,
    faulty: bool,
}

/// One frame on the air, with the claim it carries.
struct FrameState {
    tx: NodeId,
    tx_pos: Vec3,
    bytes: u32,
    signer: [u8; 8],
    /// The claim, after the attacker had it.
    claim: OnAirClaim,
    start: SimTime,
    air: Duration,
}

/// Everything a receiver can read off one frame's payload and envelope.
#[derive(Debug, Clone)]
struct OnAirClaim {
    x_m: f64,
    y_m: f64,
    speed_mps: f64,
    heading_rad: f64,
    generation_time: SimTime,
    station_type: StationType,
    repetitions: u32,
    signature_valid: bool,
    cert_valid_from: SimTime,
    cert_valid_to: SimTime,
    pos_confidence_m: f64,
    kind: ObservedKind,
    /// The instant the frame finished arriving, on the simulator's clock. The receiver
    /// converts it to its **own** clock before any detector sees it.
    ///
    /// Carried per frame rather than taken as "the step": two beacons from one sender
    /// inside one mobility step would otherwise share an arrival instant, the alpha-beta
    /// tracker's `dtk` would hit its 1 ms floor, and its velocity term `β·r/dtk` would
    /// diverge. That is what the fingerprint's `kalmanConsistency` peak of 1e122 in an
    /// early harness run was, and it is a harness fault and not a ported one.
    arrival: SimTime,
}

// --------------------------------------------------------------------------------------
// The run
// --------------------------------------------------------------------------------------

/// Runs one attack-in-the-loop simulation.
///
/// # Panics
/// If the world or the demand model cannot be built — a harness failure, not a model
/// outcome.
pub fn run(opts: &SimOptions) -> SimOut {
    // `v2xw_engine::wiring::build_world`, procedural branch.
    let import = v2xw_world::ImportOptions {
        imported_at: "2026-09-18T00:00:00Z".to_string(),
        ..v2xw_world::ImportOptions::default()
    };
    let world = v2xw_world::procedural::grid(&opts.grid, &import).expect("the world must build");
    let map = LaneMap::of(&world);
    let rng = RngRegistry::new(opts.seed);
    let step = Duration::from_millis(opts.step_ms);

    // `v2xw_engine::wiring::build_mobility`.
    let mut mobility = v2xw_mobility::NativeMobility::new(v2xw_mobility::EngineParams {
        step,
        ..v2xw_mobility::EngineParams::default()
    });
    // `v2xw_engine::wiring::build_gnss`: the Gauss–Markov receiver, not a perfect one.
    let mut gnss =
        v2xw_mobility::GaussMarkovGnss::new(v2xw_mobility::gnss::GaussMarkovParams::default());
    // `v2xw_engine::wiring::build_radio`, medium tier.
    let env_class = world.env_class_at(Vec3::new(
        (world.bbox.min.x + world.bbox.max.x) * 0.5,
        (world.bbox.min.y + world.bbox.max.y) * 0.5,
        0.0,
    ));
    let mut propagation = v2xw_radio::LogDistanceShadowing::auto(Tier::Medium, env_class);
    let mut fading = v2xw_radio::NakagamiFading::new(v2xw_radio::NakagamiPreset::FixedMedium);
    let per = PerModel::new(v2xw_radio::PerPreset::Ideal);
    let weather = WeatherState::CLEAR;

    let mut scheduler: Scheduler<()> = Scheduler::new();
    let mut provenance = ProvenanceLog::new();
    let params = ParamSet::new();
    let mut records: Vec<(SimTime, OwnedRecord)> = Vec::new();
    let mut snapshot = ActorSnapshot::new(0, MAX_RANGE_M);

    {
        // `v2xw_engine::wiring::build_demand`, Poisson branch.
        let demand = v2xw_mobility::PoissonDemand::new(
            &world,
            v2xw_mobility::demand::PoissonParams {
                arrival_rate_per_s: opts.rate_veh_per_h / 3600.0,
                duration: Duration::from_secs_f64(opts.duration_s),
                max_total_vehicles: opts.max_vehicles,
                ..v2xw_mobility::demand::PoissonParams::default()
            },
            v2xw_mobility::demand::OdParams::default(),
        )
        .expect("the demand model must accept the world");
        let mut ctx = hctx!(
            0,
            &rng,
            &world,
            &snapshot,
            &mut scheduler,
            &mut provenance,
            &params,
            &mut records
        );
        Mobility::init(
            &mut mobility,
            &mut v2xw_mobility::CoreCtx(&mut ctx),
            Box::new(demand),
        )
        .expect("the mobility provider must accept the world");
    }
    records.clear();

    let mut out = SimOut {
        nominal_range_m: opts.radio_range_m,
        ..SimOut::default()
    };
    let mut actors: BTreeMap<ActorId, ActorRec> = BTreeMap::new();
    let mut nodes: BTreeMap<NodeId, ObuRuntime> = BTreeMap::new();
    let mut attackers: BTreeMap<NodeId, LegacyAttacker> = BTreeMap::new();
    let mut detectors: BTreeMap<NodeId, Legacy12> = BTreeMap::new();
    let mut inboxes: BTreeMap<NodeId, Vec<(RxFrame, OnAirClaim)>> = BTreeMap::new();
    let mut last_rx: BTreeMap<NodeId, Vec<ObservedMessage>> = BTreeMap::new();
    let mut ma = v2xw_threat::ma::LegacyWindow::new(opts.ma.clone());
    let mut next_node = 0u32;
    let mut next_report = 0u64;
    let mut next_msg = 0u64;
    let mut attacker_index = 0usize;

    let horizon = secs_to_ns(opts.duration_s);
    let schedule = AttackSchedule {
        from: secs_to_ns(opts.attack_from_s),
        to: secs_to_ns(opts.attack_to_s),
        ..AttackSchedule::default()
    };

    let mut now: SimTime = 0;
    while now <= horizon {
        // ---- mobility phase -----------------------------------------------------------
        let update: MobilityUpdate = {
            let mut ctx = hctx!(
                now,
                &rng,
                &world,
                &snapshot,
                &mut scheduler,
                &mut provenance,
                &params,
                &mut records
            );
            Mobility::step(&mut mobility, &mut v2xw_mobility::CoreCtx(&mut ctx), step).quantized()
        };

        // The attacker draw and the faulty-sensor draw are keyed by the actor, so neither
        // depends on how many vehicles spawned before it.
        for spawn in &update.spawned {
            let is_attacker = rng
                .checkout(RngDomain::Attack, EntityRef::Actor(spawn.actor))
                .bool(opts.attacker_fraction);
            let faulty = !is_attacker
                && rng
                    .checkout(RngDomain::Gnss, EntityRef::Actor(spawn.actor))
                    .bool(opts.faulty_fraction);
            let id = NodeId::new(next_node);
            next_node += 1;
            nodes.insert(id, build_node(id, update.t, opts.queue_capacity));
            detectors.insert(id, Legacy12::new(opts.detector.clone()));
            inboxes.insert(id, Vec::new());
            last_rx.insert(id, Vec::new());
            out.nodes += 1;
            if is_attacker && !opts.attacks.is_empty() {
                let kind = opts.attacks[attacker_index % opts.attacks.len()];
                attacker_index += 1;
                let ghosts: Vec<[u8; 8]> = (1..=GHOSTS)
                    .map(|i| id8_bytes(&v2xw_node::stores::pseudo_signer(id, i)))
                    .collect();
                let mut p = LegacyAttackerParams::new(kind);
                p.dt_s = step.as_secs_f64();
                p.sybil_ghosts = GHOSTS;
                attackers.insert(
                    id,
                    LegacyAttacker::new(id, p, Capabilities::insider(20), schedule.clone(), ghosts),
                );
                out.attackers += 1;
                out.attack_of.insert(id, kind);
            }
            actors.insert(
                spawn.actor,
                ActorRec {
                    class: spawn.class,
                    driver: spawn.driver,
                    node: Some(id),
                    last: spawn.kinematics,
                    faulty,
                },
            );
        }
        for (actor, _) in &update.despawned {
            if let Some(rec) = actors.remove(actor)
                && let Some(node) = rec.node
            {
                nodes.remove(&node);
                detectors.remove(&node);
                inboxes.remove(&node);
                last_rx.remove(&node);
                attackers.remove(&node);
            }
        }
        for (actor, k) in &update.states {
            if let Some(rec) = actors.get_mut(actor) {
                rec.last = *k;
            }
        }

        snapshot = build_snapshot(&world, &actors, &update);

        // ---- belief phase -------------------------------------------------------------
        let pairs: Vec<(NodeId, Kinematics, bool)> = actors
            .values()
            .filter_map(|a| a.node.map(|n| (n, a.last, a.faulty)))
            .collect();
        let mut confidence: BTreeMap<NodeId, f64> = BTreeMap::new();
        for (node, truth, faulty) in pairs {
            let belief = if opts.perfect_belief {
                // The ideal regime: the node's belief is its own truth, so the broadcast
                // confidence is zero and the detector's tolerance is its own floor —
                // which is what the legacy engine does at `gps_sigma_m = 0`.
                confidence.insert(node, 0.0);
                v2xw_core::belief::PositionEstimate {
                    pos: truth.pos,
                    vel: truth.vel,
                    heading_rad: truth.heading_rad,
                    semi_major_m: 0.0,
                    semi_minor_m: 0.0,
                    orientation_rad: 0.0,
                    time_ns: now,
                    fix: v2xw_core::belief::FixQuality::Rtk,
                }
                .quantized()
            } else {
                let genv = GnssEnv {
                    weather,
                    faulty_sensor: faulty,
                    ..GnssEnv::OPEN_SKY
                };
                let mut ctx = hctx!(
                    now,
                    &rng,
                    &world,
                    &snapshot,
                    &mut scheduler,
                    &mut provenance,
                    &params,
                    &mut records
                );
                let b = gnss
                    .estimate(&mut v2xw_mobility::CoreCtx(&mut ctx), node, &truth, &genv)
                    .quantized();
                // The broadcast position confidence: the legacy engine broadcasts
                // `2.448·σ` (`run.py:1896`, the 95 % two-dimensional circle), and this
                // model's σ is the semi-major axis of its error ellipse.
                confidence.insert(node, 2.448 * b.semi_major_m.max(0.0));
                b
            };
            let error =
                v2xw_core::math::hypot(belief.pos.x - truth.pos.x, belief.pos.y - truth.pos.y);
            if let Some(runtime) = nodes.get_mut(&node) {
                runtime.set_belief(belief);
                runtime.observe_truth(error as f32);
                runtime.set_dcc(v2xw_msg::generator::DccState::UNRESTRICTED, 0);
            }
        }

        // ---- node phase ---------------------------------------------------------------
        let node_ids: Vec<NodeId> = nodes.keys().copied().collect();
        let mut launches: Vec<(NodeId, Transmission, OnAirClaim)> = Vec::new();
        let mut verdicts: Vec<(NodeId, SimTime, Verdict, ObservedMessage)> = Vec::new();

        for id in node_ids {
            let inbox: Vec<(RxFrame, OnAirClaim)> = inboxes
                .get_mut(&id)
                .map(core::mem::take)
                .unwrap_or_default();
            // The fields the node runtime's `VerifiedMessage` does not carry — the
            // repetition count from the MAC, the certificate window from the envelope, the
            // declared station type and the broadcast confidence — joined back by
            // (signer, generation time), which is what identifies a beacon on the air.
            let mut claims: BTreeMap<([u8; 8], SimTime), OnAirClaim> = BTreeMap::new();
            for (f, c) in &inbox {
                let key = (
                    f.signer.as_ref().map_or([0u8; 8], id8_bytes),
                    f.claimed_generation_time,
                );
                claims.entry(key).or_insert_with(|| c.clone());
            }
            let frames: Vec<RxFrame> = inbox.into_iter().map(|(f, _)| f).collect();
            // In the ideal regime the node's compute budget is out of the way: the frames
            // are handed to the detector directly and the runtime is stepped with an empty
            // inbox, so it still generates and signs its own beacons.
            let (to_runtime, direct) = if opts.bypass_node_compute {
                (Vec::new(), frames.clone())
            } else {
                (frames, Vec::new())
            };

            let Some(runtime) = nodes.get_mut(&id) else {
                continue;
            };
            let travelled = runtime
                .state()
                .transmits()
                .then(|| {
                    v2xw_core::NodeView::position(runtime).ground_speed_mps() * step.as_secs_f64()
                })
                .unwrap_or(0.0);
            let outcome = {
                let mut ctx = hctx!(
                    now,
                    &rng,
                    &world,
                    &snapshot,
                    &mut scheduler,
                    &mut provenance,
                    &params,
                    &mut records
                );
                runtime.step(&mut v2xw_node::CoreCtx(&mut ctx), to_runtime, travelled)
            };
            out.delivered += outcome.delivered.len() as u64;

            // ---- the detector runs on what this node received and verified ------------
            let belief = v2xw_core::NodeView::position(runtime);
            let me = SelfBelief {
                node: id,
                believed_time: runtime.clock().believed_time(now),
                x_m: belief.pos.x,
                y_m: belief.pos.y,
                radio_range_m: opts.radio_range_m,
            };
            let mut last_seen: BTreeMap<[u8; 8], SimTime> = BTreeMap::new();
            let mut heard: Vec<ObservedMessage> = Vec::new();
            for vm in &outcome.delivered {
                let signer = vm.signer.as_ref().map_or([0u8; 8], id8_bytes);
                let Some(claim) = claims.get(&(signer, vm.claimed_generation_time)) else {
                    continue;
                };
                heard.push(ObservedMessage {
                    signer,
                    kind: claim.kind.clone(),
                    // The frame's own arrival, on this node's clock — see
                    // `OnAirClaim::arrival`.
                    received_at: runtime.clock().believed_time(claim.arrival),
                    claimed_generation_time: vm.claimed_generation_time,
                    claimed_x_m: vm.claimed_pos.map_or(0.0, |p| p.x),
                    claimed_y_m: vm.claimed_pos.map_or(0.0, |p| p.y),
                    claimed_speed_mps: vm.claimed_speed_mps,
                    claimed_heading_rad: vm.claimed_heading_rad,
                    claimed_pos_confidence_m: claim.pos_confidence_m,
                    repetitions: claim.repetitions,
                    cert_valid_from: claim.cert_valid_from,
                    cert_valid_to: claim.cert_valid_to,
                    station_type: claim.station_type,
                    verification: match vm.verification {
                        v2xw_node::stores::VerificationState::Verified => VerificationState::Valid,
                        v2xw_node::stores::VerificationState::Invalid => {
                            VerificationState::BadSignature
                        }
                        v2xw_node::stores::VerificationState::Revoked => {
                            VerificationState::UnknownCertificate
                        }
                        v2xw_node::stores::VerificationState::Unverified => {
                            VerificationState::Unverified
                        }
                    },
                });
            }
            for f in &direct {
                let signer = f.signer.as_ref().map_or([0u8; 8], id8_bytes);
                let Some(claim) = claims.get(&(signer, f.claimed_generation_time)) else {
                    continue;
                };
                heard.push(ObservedMessage {
                    signer,
                    kind: claim.kind.clone(),
                    received_at: runtime.clock().believed_time(claim.arrival),
                    claimed_generation_time: f.claimed_generation_time,
                    claimed_x_m: f.claimed_pos.map_or(0.0, |p| p.x),
                    claimed_y_m: f.claimed_pos.map_or(0.0, |p| p.y),
                    claimed_speed_mps: f.claimed_speed_mps,
                    claimed_heading_rad: f.claimed_heading_rad,
                    claimed_pos_confidence_m: claim.pos_confidence_m,
                    repetitions: claim.repetitions,
                    cert_valid_from: claim.cert_valid_from,
                    cert_valid_to: claim.cert_valid_to,
                    station_type: claim.station_type,
                    verification: if f.signature_valid {
                        VerificationState::Valid
                    } else {
                        VerificationState::BadSignature
                    },
                });
            }
            out.delivered += direct.len() as u64;
            for m in &heard {
                if let Some(prev) = last_seen.insert(m.signer, m.received_at)
                    && prev == m.received_at
                {
                    out.same_instant_repeats += 1;
                }
            }
            if let Some(det) = detectors.get_mut(&id) {
                for m in &heard {
                    let mut ctx = hctx!(
                        now,
                        &rng,
                        &world,
                        &snapshot,
                        &mut scheduler,
                        &mut provenance,
                        &params,
                        &mut records
                    );
                    let v = det.on_message(&mut ctx, &me, m, &map);
                    let k = v.fingerprint.get(DetectorId::KalmanConsistency);
                    if k > out.kalman_peak {
                        out.kalman_peak = k;
                    }
                    for d in DetectorId::ALL {
                        let s = v.fingerprint.get(d);
                        let slot = out.peak.entry(d.as_str().to_string()).or_insert(0.0);
                        if s > *slot {
                            *slot = s;
                        }
                    }
                    if v.fired() {
                        verdicts.push((id, me.believed_time, v, m.clone()));
                    }
                }
            }
            last_rx.insert(id, heard);

            // ---- the attacker edits the outgoing claim, before it goes on the air -----
            for tx in &outcome.transmissions {
                let belief = v2xw_core::NodeView::position(runtime);
                let honest = HonestClaim {
                    x_m: belief.pos.x,
                    y_m: belief.pos.y,
                    speed_mps: belief.ground_speed_mps(),
                    heading_rad: belief.heading_rad,
                };
                let cred = runtime.stores().certs.active();
                let signer = id8_bytes(&tx.signer);
                let (cvf, cvt) = cred.map_or((0, SimTime::MAX), |c| (c.valid_from, c.valid_until));
                let mut emission = Emission::honest(signer, honest, tx.generation_time, cvf, cvt);
                let mut actions: Vec<AttackAction> = Vec::new();
                let believed = runtime.clock().believed_time(now);
                if let Some(att) = attackers.get_mut(&id) {
                    let own_creds: Vec<[u8; 8]> = runtime
                        .stores()
                        .certs
                        .credentials()
                        .iter()
                        .map(|c| id8_bytes(&c.digest))
                        .collect();
                    let view = AttackerView {
                        own_rx: last_rx.get(&id).map(Vec::as_slice).unwrap_or(&[]),
                        own_credentials: &own_creds,
                        crl_revocations_seen: None,
                        own_belief: SelfBelief {
                            node: id,
                            believed_time: believed,
                            x_m: honest.x_m,
                            y_m: honest.y_m,
                            radio_range_m: opts.radio_range_m,
                        },
                        honest,
                        believed_time: believed,
                    };
                    let mut ctx = hctx!(
                        now,
                        &rng,
                        &world,
                        &snapshot,
                        &mut scheduler,
                        &mut provenance,
                        &params,
                        &mut records
                    );
                    att.observe(&mut ctx, &view);
                    actions = att.act(&mut ctx, &view, &mut emission);
                }
                let msg = if emission.suppressed {
                    None
                } else {
                    let m = next_msg;
                    next_msg += 1;
                    Some(m)
                };
                if !actions.is_empty() {
                    let actor = actors
                        .iter()
                        .find(|(_, r)| r.node == Some(id))
                        .map(|(a, _)| *a);
                    if let Some(actor) = actor {
                        let mut ctx = hctx!(
                            now,
                            &rng,
                            &world,
                            &snapshot,
                            &mut scheduler,
                            &mut provenance,
                            &params,
                            &mut records
                        );
                        // I-T3: the host names the actor, because the attacker cannot.
                        log_actions(
                            &mut ctx,
                            now,
                            actor,
                            v2xw_threat::attack_legacy::MODEL_ID,
                            &actions,
                            msg,
                        );
                    }
                }
                if emission.suppressed {
                    continue;
                }
                if is_falsified(&honest, &emission, believed, StationType::Vehicle) {
                    out.falsified_frames += 1;
                }
                let conf = confidence.get(&id).copied().unwrap_or(0.0);
                let claim = |e: &Emission| OnAirClaim {
                    x_m: e.x_m,
                    y_m: e.y_m,
                    speed_mps: e.speed_mps,
                    heading_rad: e.heading_rad,
                    generation_time: e.generation_time,
                    station_type: e.station_type,
                    repetitions: e.repetitions,
                    signature_valid: e.signature_valid,
                    cert_valid_from: e.cert_valid_from,
                    cert_valid_to: e.cert_valid_to,
                    pos_confidence_m: conf,
                    kind: ObservedKind::Beacon,
                    arrival: 0,
                };
                launches.push((id, tx.clone(), claim(&emission)));
                // A Sybil attacker's ghosts go on the air as ordinary frames signed with
                // the attacker's other pseudonyms: the whole point is that they are
                // indistinguishable from other senders to a receiver.
                for g in &emission.ghosts {
                    let mut t2 = tx.clone();
                    t2.signer = id8_from_bytes(g.signer);
                    launches.push((id, t2, claim(g)));
                }
            }
        }

        // ---- the air ------------------------------------------------------------------
        let mut frames: Vec<FrameState> = Vec::new();
        for (id, tx, claim) in launches {
            let believed = nodes.get(&id).map_or(now, |n| n.clock().believed_time(now));
            let signing = Duration::between(believed, tx.ready_at);
            let at = Duration::from_nanos(signing.as_nanos() + AIFS.as_nanos()).after(now);
            if at > horizon {
                continue;
            }
            let Some(pos) = actors
                .values()
                .find(|a| a.node == Some(id))
                .map(|a| a.last.extrapolate(at).pos)
            else {
                continue;
            };
            frames.push(FrameState {
                tx: id,
                tx_pos: pos,
                bytes: tx.bytes,
                signer: id8_bytes(&tx.signer),
                claim,
                start: at,
                air: v2xw_radio::air_time(tx.bytes, v2xw_radio::Mcs::R6Qpsk12),
            });
        }
        for f in &frames {
            let hex = v2xw_core::hash::hex_encode(&f.signer);
            let actor = actors
                .iter()
                .find(|(_, r)| r.node == Some(f.tx))
                .map(|(a, _)| *a);
            if let Some(a) = actor {
                out.subject_actor.insert(hex, a);
            }
        }
        // Frames go on the air in (start, tx, signer) order, so the shadowing and fading
        // processes are advanced in a fixed order whatever order the nodes were walked in.
        frames.sort_by(|a, b| (a.start, a.tx, a.signer).cmp(&(b.start, b.tx, b.signer)));
        out.frames += frames.len() as u64;

        for f in &frames {
            let end = f.air.after(f.start);
            let mut candidates: Vec<(NodeId, Vec3)> = Vec::new();
            for actor in snapshot.actors_within(f.tx_pos, MAX_RANGE_M) {
                let Some(rec) = actors.get(&actor) else {
                    continue;
                };
                let Some(node) = rec.node else { continue };
                if node == f.tx {
                    continue;
                }
                candidates.push((node, rec.last.extrapolate(end).pos));
            }
            candidates.sort_by_key(|(n, _)| *n);

            for (rx, rx_pos) in candidates {
                let link = LinkKey::new(f.tx, rx);
                let distance_m = f.tx_pos.distance(rx_pos);
                out.reception_attempts += 1;
                let received = if opts.lossless {
                    distance_m <= opts.radio_range_m
                } else {
                    let (loss, fade) = {
                        let mut ctx = hctx!(
                            now,
                            &rng,
                            &world,
                            &snapshot,
                            &mut scheduler,
                            &mut provenance,
                            &params,
                            &mut records
                        );
                        let tx_end = RadioEndpoint::isotropic(
                            f.tx,
                            f.tx_pos,
                            v2xw_radio::ActorClass::Car,
                            f.start,
                        );
                        let rx_end = RadioEndpoint::isotropic(
                            rx,
                            rx_pos,
                            v2xw_radio::ActorClass::Car,
                            f.start,
                        );
                        let los = LosResult::clear();
                        let loss = propagation.loss_db(
                            &mut ctx,
                            &tx_end,
                            &rx_end,
                            SAFETY_FREQ_HZ,
                            &los,
                            &weather,
                        );
                        let fade = fading.sample_db(&mut ctx, link, distance_m, f.start);
                        (loss, fade)
                    };
                    let rssi_dbm =
                        v2xw_core::math::sum_ordered([TX_POWER_DBM, -loss.total_db, fade]);
                    let sinr_db = rssi_dbm - NOISE_FLOOR_DBM;
                    let p = per.per(f.bytes, v2xw_radio::Mcs::R6Qpsk12, sinr_db);
                    // D10: the comparison is made against a quantised value, so a
                    // cross-engine comparison cannot flip on a last-bit difference.
                    let per_q = v2xw_core::math::quantize_to(p, 1e-6);
                    let draw = rng
                        .checkout(
                            RngDomain::AbstractRx,
                            EntityRef::LinkFrame {
                                link,
                                frame: f.start,
                            },
                        )
                        .f64();
                    draw >= per_q
                };
                if !received {
                    continue;
                }
                out.receptions += 1;
                if distance_m > out.max_rx_distance_m {
                    out.max_rx_distance_m = distance_m;
                }
                if distance_m > opts.radio_range_m {
                    out.rx_beyond_nominal_range += 1;
                }
                if let Some(inbox) = inboxes.get_mut(&rx) {
                    inbox.push((
                        RxFrame {
                            signer: Some(id8_from_bytes(f.signer)),
                            msg_type: v2xw_msg::MsgType::Bsm,
                            bytes: f.bytes,
                            claimed_pos: Some(Vec3::new(f.claim.x_m, f.claim.y_m, 0.0)),
                            claimed_speed_mps: f.claim.speed_mps,
                            claimed_heading_rad: f.claim.heading_rad,
                            claimed_generation_time: f.claim.generation_time,
                            full_certificate: true,
                            signature_valid: f.claim.signature_valid,
                            claimed_cert_period: 0,
                            claimed_linkage: None,
                            // The legacy path: the harness decides validity on the node's
                            // behalf and the node only pays for the check. Carrying real
                            // SPDU bytes would mean signing every ghost with a key the
                            // attacker does not have, which is a `crypto_mode: real`
                            // question and not this comparison's.
                            spdu: None,
                        },
                        OnAirClaim {
                            arrival: end,
                            ..f.claim.clone()
                        },
                    ));
                }
            }
        }

        // ---- reports and the authority ------------------------------------------------
        for (reporter, at, v, m) in verdicts {
            for o in &v.fired {
                *out.firings
                    .entry(o.detector.as_str().to_string())
                    .or_insert(0) += 1;
            }
            let coin = rng
                .checkout(RngDomain::Report, EntityRef::Node(reporter))
                .f64();
            if coin > opts.report_prob {
                continue;
            }
            let reporter_digest = nodes
                .get(&reporter)
                .and_then(|n| n.stores().certs.active().map(|c| id8_bytes(&c.digest)))
                .map(|b| v2xw_core::hash::hex_encode(&b))
                .unwrap_or_default();
            next_report += 1;
            let ev = Evidence {
                subject_pos_confidence_m: m.claimed_pos_confidence_m,
                station_type: m.station_type.as_str().to_string(),
                cert_validity: CertValidity {
                    sig_valid: m.verification.is_valid(),
                    not_expired: m.cert_valid_to >= at,
                    not_revoked: true,
                    chain_ok: true,
                },
                bbox: [m.claimed_x_m, m.claimed_y_m, m.claimed_x_m, m.claimed_y_m],
                detection_time: at,
                ingest_time: at,
            };
            let Some(report) = MisbehaviourReport::from_verdict(
                format!("rpt_{next_report:05}"),
                reporter,
                reporter_digest,
                &v,
                &ev,
            ) else {
                continue;
            };
            out.reports += 1;
            let mut ctx = hctx!(
                now,
                &rng,
                &world,
                &snapshot,
                &mut scheduler,
                &mut provenance,
                &params,
                &mut records
            );
            for a in ma.on_report(&mut ctx, &report) {
                if matches!(a, v2xw_threat::ma::MaAction::Revoke { .. }) {
                    out.revocations += 1;
                }
            }
        }

        now = step.after(now);
    }

    // The true attacker set, read off the ground-truth channel — the same way the metrics
    // provider reads it.
    for (_, r) in &records {
        if r.channel == "gt.attack.action"
            && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&r.json)
            && v.get("changed_bytes_on_air")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true)
            && let Some(a) = v.get("actor").and_then(serde_json::Value::as_u64)
        {
            out.true_attacker_actors.insert(ActorId::new(a as u32));
        }
    }

    // Which check accused whom: the host side of the score, from the same declared join
    // and the same ground-truth channel the metrics provider reads. A detector cannot see
    // this and never asks for it.
    for (_, r) in &records {
        if r.channel != "det.observation" {
            continue;
        }
        let Ok(o) = serde_json::from_slice::<v2xw_threat::records::DetObservation>(&r.json) else {
            continue;
        };
        let truth = out
            .subject_actor
            .get(&o.subject)
            .map(|a| out.true_attacker_actors.contains(a));
        match truth {
            Some(true) => *out.firings_on_attacker.entry(o.detector).or_insert(0) += 1,
            Some(false) => *out.firings_on_benign.entry(o.detector).or_insert(0) += 1,
            None => {}
        }
    }

    out.digest = digest_of(&records);
    if let Some(path) = &opts.record {
        write_recording(path, &records);
        out.recording = Some(path.clone());
    }
    out.records = records;
    out
}

// --------------------------------------------------------------------------------------
// Helpers
// --------------------------------------------------------------------------------------

/// One node on the reference profile with a bootstrap credential — what
/// `v2xw_engine::wiring::build_node` and `bootstrap_credentials` do, without needing a
/// `Scenario`.
///
/// The verification policy is `verify-all` rather than `prioritized`: a detector must see
/// every message the node received, and `prioritized` would leave a distant sender's
/// signature unchecked, which reads as an unverified message rather than as an attack.
fn build_node(node: NodeId, at: SimTime, queue_capacity: usize) -> ObuRuntime {
    let profile = v2xw_node::profiles::get(v2xw_node::profiles::REFERENCE_OBU)
        .expect("the reference profile ships with v2xw-node")
        .clone();
    let policy: Box<dyn VerificationPolicy> = Box::new(v2xw_node::VerifyAll::new());
    let config = NodeConfig {
        tx_power_dbm: TX_POWER_DBM,
        queue_capacity: [queue_capacity; 5],
        // BSM only, which is what `scenarios/phase1-*.yaml` ask for (`messages.sets:
        // [bsm]`). `NodeConfig::default()` is `ServiceSet::BOTH`, and a dual-stack node
        // puts a CAM and a BSM on the air in the same generation interval carrying the
        // same signer and the same claimed generation time — two messages from one sender
        // at one instant, which is the input the ported alpha-beta tracker's `dtk` floor
        // has no guard for. Measured with BOTH: 135 809 such pairs in a 345 088-reception
        // run, and `kalmanConsistency` diverged to `inf`.
        services: v2xw_node::ServiceSet::SAE,
        ..NodeConfig::default()
    };
    let mut runtime = ObuRuntime::new(node, profile, policy, config, at);
    runtime
        .stores_mut()
        .certs
        .insert(v2xw_node::stores::CredentialHandle {
            digest: v2xw_node::stores::pseudo_signer(node, 0),
            cert_coer: vec![0u8; 117],
            key: v2xw_sec::KeyId(u64::from(node.index())),
            i_period: 0,
            j_index: 0,
            valid_from: at,
            valid_until: SimTime::MAX,
            state: v2xw_node::stores::CredState::Active,
        });
    runtime
}

/// The instant a node believes it is, on its own clock.
fn me_believed(runtime: &ObuRuntime, now: SimTime) -> SimTime {
    runtime.clock().believed_time(now)
}

fn build_snapshot(
    world: &World,
    actors: &BTreeMap<ActorId, ActorRec>,
    update: &MobilityUpdate,
) -> ActorSnapshot {
    let mut entries: Vec<(VehicleView, Kinematics)> = Vec::with_capacity(update.states.len());
    for (actor, k) in &update.states {
        let Some(rec) = actors.get(actor) else {
            continue;
        };
        let lane = k.lane.unwrap_or(v2xw_core::geom::LanePos::new(
            v2xw_core::ids::LaneId::new(0),
            0.0,
            0.0,
        ));
        entries.push((
            VehicleView {
                actor: *actor,
                class: rec.class,
                lane: lane.lane,
                lane_index: world
                    .roads
                    .lanes()
                    .get(lane.lane.0 as usize)
                    .map_or(0, |l| l.index),
                s_m: lane.s_m + k.dims.length_m,
                lateral_m: lane.d_m,
                speed_mps: k.ground_speed_mps(),
                accel_mps2: k.acc.x,
                heading_rad: k.heading_rad,
                dims: k.dims,
                driver: rec.driver,
            },
            *k,
        ));
    }
    ActorSnapshot::build(update.t, MAX_RANGE_M, entries)
}

/// `SHA-256(t ‖ channel ‖ visibility ‖ json)*` over every record, in order — the same
/// digest `v2xw_engine::MemoryRecorder::digest_hex` computes.
pub fn digest_of(records: &[(SimTime, OwnedRecord)]) -> String {
    let mut w = v2xw_core::hash::Sha256Writer::new();
    for (t, r) in records {
        w.update(&t.to_le_bytes());
        w.update(r.channel.as_bytes());
        w.update(r.visibility.to_string().as_bytes());
        w.update(&r.json);
    }
    w.finish_hex()
}

fn write_recording(path: &Path, records: &[(SimTime, OwnedRecord)]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut writer = v2xw_record::RecordingWriter::create(
        path,
        v2xw_record::RecordingOptions {
            profile: v2xw_record::Profile::Full,
            ..v2xw_record::RecordingOptions::default()
        },
    )
    .expect("the recording must open");
    for (t, r) in records {
        writer
            .write_record(*t, r)
            .expect("every channel the harness writes is a declared one");
    }
    writer.finish().expect("the recording must close");
}

/// The confusion matrices the metrics crate builds from one run's record stream.
///
/// There is no second scorer: the records go into `v2xw_metrics::DetectionProvider`
/// exactly as a run would feed them, and the matrices come out of it.
pub fn score(out: &SimOut) -> (v2xw_metrics::ConfusionMatrix, v2xw_metrics::ConfusionMatrix) {
    let mut p = v2xw_metrics::detection::DetectionProvider::new();
    for (subject, actor) in &out.subject_actor {
        p.declare_subject(subject.clone(), *actor);
    }
    for (_, r) in &out.records {
        v2xw_metrics::MetricProvider::on_event(&mut p, r);
    }
    (
        p.matrix(v2xw_metrics::DetectionLevel::Report),
        p.matrix(v2xw_metrics::DetectionLevel::Vehicle),
    )
}

/// A short human-readable summary, for a test that prints real numbers.
pub fn summarize(out: &SimOut) -> String {
    let (report, vehicle) = score(out);
    let mut s = String::new();
    s.push_str(&format!(
        "nodes {} ({} attackers) | frames {} ({} falsified) | rx {}/{} | delivered {} | \
         reports {} | revocations {}\n",
        out.nodes,
        out.attackers,
        out.frames,
        out.falsified_frames,
        out.receptions,
        out.reception_attempts,
        out.delivered,
        out.reports,
        out.revocations,
    ));
    s.push_str(&format!(
        "report-level  tp {} fp {} fn {} tn {}\n",
        report.tp, report.fp, report.fn_, report.tn
    ));
    s.push_str(&format!(
        "vehicle-level tp {} fp {} fn {} tn {}\n",
        vehicle.tp, vehicle.fp, vehicle.fn_, vehicle.tn
    ));
    s.push_str(&format!(
        "tracker: {} same-instant repeats, kalmanConsistency peak {:.3e}\n",
        out.same_instant_repeats, out.kalman_peak
    ));
    s.push_str(&format!(
        "reception: max {:.0} m, {} of {} receptions beyond the nominal {:.0} m range\n",
        out.max_rx_distance_m,
        out.rx_beyond_nominal_range,
        out.receptions,
        out.nominal_range_m
    ));
    for d in DetectorId::ALL {
        let name = d.as_str();
        let fired = out.firings.get(name).copied().unwrap_or(0);
        let peak = out.peak.get(name).copied().unwrap_or(0.0);
        let att = out.firings_on_attacker.get(name).copied().unwrap_or(0);
        let ben = out.firings_on_benign.get(name).copied().unwrap_or(0);
        s.push_str(&format!(
            "  {name:28} fired {fired:6}  peak {peak:8.3}  on attackers {att:6}  on honest {ben:6}\n"
        ));
    }
    s
}
