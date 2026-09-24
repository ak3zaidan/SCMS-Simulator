//! Mobility and safety metrics: surrogate safety measures, headway, the fundamental
//! diagram, speed and acceleration (08-measurement-and-data.md §2.5, 04-models.md §11 and
//! §2.9).
//!
//! | Metric | Formula | Unit | What it does **not** account for |
//! |---|---|---|---|
//! | `ttc_min` | `gap / (v_follower − v_leader)` while closing, per same-lane pair per step | s | **vehicle length**: the gap is between reported lane positions, so a bumper-to-bumper TTC is shorter than this by `length / Δv`; and crossing conflicts, which need trajectory intersection |
//! | `ttc_conflicts` | pairs whose TTC is below the threshold | count | the same, plus the threshold itself (1.5 s is the only *verified* number in 04-models.md §11) |
//! | `pet` | time between one actor leaving a cell and a different actor entering it | s | the conflict angle, the vehicles' extent beyond one cell, and a cell revisited by the *same* actor, which is not a conflict |
//! | `drac` | `(v_follower − v_leader)² / (2·gap)` while closing | m/s² | the follower's actual braking capability, and the road surface: it is the deceleration *required*, not one that was achieved |
//! | `headway_time` | `gap / v_follower` | s | a stopped follower, which has no time headway and contributes no sample rather than an infinite one |
//! | `headway_distance` | `pos_leader − pos_follower` along the lane | m | vehicle length, as with TTC |
//! | `flow`, `density`, `mean_speed` | Edie's generalised definitions over the window and the lane | veh/h, veh/km, m/s | the lane's own capacity and geometry; and every lane whose length the run did not declare, which is left out and counted |
//! | `speed`, `acceleration` | the sampled distributions | m/s, m/s² | the sampling interval's bias: these are time-sampled, so a vehicle stopped for a minute contributes six hundred samples at 10 Hz |
//!
//! # Edie's definitions, and why they need the lane's length
//!
//! Over a window of length `T` on a lane of length `L`, with `Δt` between samples:
//!
//! ```text
//! total time spent      T_s = Σ Δt                       (one term per sample on the lane)
//! total distance        D   = Σ v·Δt
//! density   k = T_s / (L·T)         [veh/m]  → ×1000 → veh/km
//! flow      q = D   / (L·T)         [veh/s]  → ×3600 → veh/h
//! mean speed v̄ = D / T_s = q / k    [m/s]    (the *space*-mean speed)
//! ```
//!
//! Edie (1963), "Discussion of traffic stream measurements and definitions"; the same
//! definitions in FHWA's *Traffic Flow Theory* chapter 2, which 04-models.md §2.9 cites for
//! the fundamental-diagram targets. `L` is world geometry, which this crate has no business
//! knowing, so the run declares it with [`SafetyProvider::declare_lane_length`]; a lane
//! whose length was never declared produces no aggregate and is counted into
//! [`SafetyProvider::undeclared_lanes`].
//!
//! The mean is the **space**-mean speed, which is what a fundamental diagram is drawn with.
//! The time-mean speed is 6–12 % higher on a mixed-speed signalised dataset (Wardrop 1952,
//! via 04-models.md §2.9), so the two are not interchangeable and this one says which it is.
//!
//! # Determinism
//!
//! Same-lane pairing sorts by the **integer millimetre grid index** of the lane position,
//! tie-broken by `ActorId`, so two platforms order a lane's vehicles identically even when
//! two positions differ in the last bit. The PET cells are integer grid coordinates for the
//! same reason. Every threshold comparison is made on quantised values, which is build
//! decision D10.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use v2xw_core::card::ModelCard;
use v2xw_core::ctx::{ChannelName, EventRecord, Visibility};
use v2xw_core::ids::ActorId;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};

use crate::cards;
use crate::channels::{ChannelView, GtKinematicsView};
use crate::def::{Agg, Dim, DimValue, Dims, MetricDef, MetricSample, SampleValue};
use crate::provider::{Decoded, MetricProvider};
use crate::quant::Quantum;
use crate::stats::{Distribution, Estimate, ratio_of_sums};

/// The time-to-collision threshold below which a same-lane pair is counted as a conflict:
/// **1.5 s**.
///
/// The one *verified* surrogate-safety threshold in 04-models.md §11 (FHWA-HRT-08-051, "as
/// suggested in previous research"). The PET and DRAC thresholds in the same table are
/// marked UNVERIFIED, which is why this crate thresholds TTC and reports the other two as
/// distributions.
pub const TTC_THRESHOLD_S: f64 = 1.5;

/// One vehicle's state on a lane, as this provider needs it.
#[derive(Debug, Clone, Copy)]
struct OnLane {
    actor: ActorId,
    /// The lane position's integer millimetre grid index — the sort key.
    pos_grid: i64,
    pos_m: f64,
    speed_mps: f64,
}

/// Edie's accumulators for one lane over one window.
#[derive(Debug, Default)]
struct LaneAccumulator {
    /// One sample per (actor, step) on this lane.
    samples: u64,
    /// `v·Δt` per sample, reduced by sorting so the sum is order-independent.
    distance: Distribution,
}

/// The mobility and safety metric provider.
///
/// Every metric here is **ground truth**: surrogate safety measures are computed from the
/// simulator's own state, which no node could know, so every definition is tagged
/// [`Visibility::Gt`] and an exporter must keep them out of a node-visible file
/// (08-measurement-and-data.md §1).
///
/// Windowed. A frame — all the kinematics samples sharing one `SimTime` — is processed when
/// the first record of the *next* frame arrives, or at flush, so pairing sees the whole
/// frame. That makes one documented assumption on the input: records arrive in
/// non-decreasing time order, which is the order a run produces them in.
pub struct SafetyProvider {
    card: ModelCard,
    min_samples: u64,
    /// The mobility sampling interval, the scenario's `mobility_step_ms`.
    dt: Duration,
    /// The PET cell's side length in metres.
    cell_m: f64,
    /// The longest gap counted as a post-encroachment time.
    pet_horizon: Duration,
    /// The TTC threshold, quantised once so the comparison is on the grid (D10).
    ttc_threshold_grid: i64,

    /// The declared lane lengths: world geometry the run supplies.
    lane_length_m: BTreeMap<u32, f64>,
    /// Lanes seen in the data whose length was never declared.
    undeclared_lanes: BTreeSet<u32>,

    /// The frame being assembled, keyed by actor so a duplicate sample cannot double-count.
    frame_t: Option<SimTime>,
    frame: BTreeMap<ActorId, GtKinematicsView>,

    ttc: Distribution,
    ttc_conflicts: u64,
    drac: Distribution,
    headway_time: Distribution,
    headway_distance: Distribution,
    pet: Distribution,
    speed: Distribution,
    acceleration: Distribution,
    lanes: BTreeMap<u32, LaneAccumulator>,
    /// The last actor to occupy each PET cell, and when.
    cell_last: BTreeMap<(i64, i64), (ActorId, SimTime)>,

    window_start: SimTime,
    rejected: u64,
}

impl SafetyProvider {
    /// A provider whose first window starts at `t0`, sampling at 100 ms (the scenario
    /// default `mobility_step_ms`), with 1 m PET cells and a 5 s PET horizon.
    #[must_use]
    pub fn new(t0: SimTime) -> Self {
        Self {
            card: Self::build_card(),
            min_samples: crate::stats::DEFAULT_MIN_SAMPLES,
            dt: Duration::from_millis(100),
            cell_m: 1.0,
            pet_horizon: Duration::from_secs(5),
            ttc_threshold_grid: Quantum::TIME_S.grid(TTC_THRESHOLD_S),
            lane_length_m: BTreeMap::new(),
            undeclared_lanes: BTreeSet::new(),
            frame_t: None,
            frame: BTreeMap::new(),
            ttc: Distribution::new(),
            ttc_conflicts: 0,
            drac: Distribution::new(),
            headway_time: Distribution::new(),
            headway_distance: Distribution::new(),
            pet: Distribution::new(),
            speed: Distribution::new(),
            acceleration: Distribution::new(),
            lanes: BTreeMap::new(),
            cell_last: BTreeMap::new(),
            window_start: t0,
            rejected: 0,
        }
    }

    /// Sets the insufficiency threshold.
    #[must_use]
    pub const fn with_min_samples(mut self, n: u64) -> Self {
        self.min_samples = n;
        self
    }

    /// Sets the mobility sampling interval (the card's `sample_interval_s`).
    ///
    /// Edie's definitions weight every sample by this, so a scenario running at 10 ms steps
    /// must say so or its flow and density will be ten times too large.
    #[must_use]
    pub const fn with_sample_interval(mut self, dt: Duration) -> Self {
        self.dt = dt;
        self
    }

    /// Sets the PET cell's side length (the card's `pet_cell_m`).
    #[must_use]
    pub const fn with_pet_cell(mut self, metres: f64) -> Self {
        self.cell_m = metres;
        self
    }

    /// Declares a lane's length, so Edie's aggregates can be computed on it.
    pub fn declare_lane_length(&mut self, lane: u32, metres: f64) {
        self.lane_length_m.insert(lane, metres);
    }

    /// Lanes that appeared in the data and whose length was never declared, so they have no
    /// fundamental-diagram aggregate.
    #[must_use]
    pub const fn undeclared_lanes(&self) -> &BTreeSet<u32> {
        &self.undeclared_lanes
    }

    fn build_card() -> ModelCard {
        let mut card = cards::provider_card(
            "metric/safety/surrogates-and-flow",
            "1.0.0",
            "Surrogate safety measures (time to collision, post-encroachment time, \
             deceleration rate to avoid the crash), headway distributions, Edie's \
             fundamental-diagram aggregates, and the speed and acceleration distributions.",
        );
        card.equations = vec![
            v2xw_core::card::Equation::new(
                "ttc",
                "ttc = gap / (v_follower − v_leader) for a closing same-lane pair; gap is \
                 the difference of reported lane positions, so vehicle length is not \
                 subtracted",
            ),
            v2xw_core::card::Equation::new(
                "pet",
                "pet = t(actor B enters cell) − t(actor A left cell) for A ≠ B, over cells of \
                 pet_cell_m metres",
            ),
            v2xw_core::card::Equation::new(
                "drac",
                "drac = (v_follower − v_leader)² / (2·gap) for a closing same-lane pair",
            ),
            v2xw_core::card::Equation::new("headway_time", "headway_time = gap / v_follower"),
            v2xw_core::card::Equation::new(
                "edie",
                "k = Σ Δt / (L·T); q = Σ v·Δt / (L·T); v̄ = q / k (the space-mean speed)",
            ),
        ];
        card.parameters = cards::statistics_params();
        card.parameters.push(cards::param(
            "ttc_threshold_s",
            "s",
            json!(TTC_THRESHOLD_S),
            json!(0.1),
            json!(10.0),
            cards::paper(
                "FHWA-HRT-08-051 (SSAM): TTC threshold 1.5 s, the one verified surrogate \
                 threshold in 04-models.md §11",
            ),
        ));
        card.parameters.push(cards::param(
            "sample_interval_s",
            "s",
            json!(0.1),
            json!(0.001),
            json!(1.0),
            cards::design(
                "03-interfaces.md §13 (time.mobility_step_ms, default 100 ms): Edie's \
                 definitions weight every sample by this interval",
            ),
        ));
        card.parameters.push(cards::param_todo(
            "pet_cell_m",
            "m",
            json!(1.0),
            json!(0.1),
            json!(10.0),
            "the cell size at which a shared position counts as the same position for PET",
            "Compare the cell-based PET distribution against a trajectory-intersection PET \
             on the same recording for cell sizes 0.5, 1 and 2 m, and pick the largest size \
             whose p50 and p95 agree with the intersection method within 5 %.",
        ));
        card.parameters.push(cards::param_todo(
            "pet_horizon_s",
            "s",
            json!(5.0),
            json!(0.1),
            json!(60.0),
            "the longest gap that still counts as a post-encroachment conflict",
            "04-models.md §11 records the PET threshold as UNVERIFIED (commonly < 5 s, not \
             confirmed). Plan: read FHWA-HRT-08-050 §3 and replace this default with the \
             sourced number.",
        ));
        card.sources = vec![
            cards::design("08-measurement-and-data.md §2.5 (traffic and safety)"),
            cards::design("04-models.md §11 (surrogate safety measures and their thresholds)"),
            cards::design("04-models.md §2.9 (fundamental-diagram validation targets)"),
            cards::paper(
                "L. C. Edie, Discussion of traffic stream measurements and definitions, \
                 Proc. 2nd Int. Symp. on the Theory of Traffic Flow, 1963",
            ),
            cards::paper(
                "FHWA-HRT-08-051, Surrogate Safety Assessment Model (SSAM): TTC, PET and DRAC",
            ),
            cards::paper(
                "J. G. Wardrop, 1952, via FHWA Traffic Flow Theory ch. 2: the time-mean \
                 versus space-mean speed difference this provider avoids by reporting the \
                 space-mean",
            ),
        ];
        card.limitations = vec![
            "TTC, DRAC and the headways are computed between reported lane positions, so \
             vehicle length is not subtracted: every gap is longer than the bumper-to-bumper \
             gap and every TTC is correspondingly optimistic."
                .to_string(),
            "Only same-lane, same-direction conflicts are found by the pairing. Crossing \
             conflicts are covered, approximately, by the cell-based PET and not by TTC."
                .to_string(),
            "PET is cell-based: it finds a shared position to within pet_cell_m and says \
             nothing about the conflict angle. The cell size is a todo-calibrate parameter."
                .to_string(),
            "The speed and acceleration distributions are time-sampled, so they are weighted \
             by how long each vehicle spent at each speed. A stopped vehicle dominates them."
                .to_string(),
            "Edie's aggregates need the lane's length, which the run declares. Undeclared \
             lanes are excluded and counted."
                .to_string(),
        ];
        card.ignores = vec![
            "Lane changes as conflicts: a vehicle moving between lanes leaves one pairing and \
             joins another, and the transition itself is not measured."
                .to_string(),
            "Vehicle dynamics: DRAC is a required deceleration, not an achievable one.".to_string(),
        ];
        card.validation.tests = vec![
            "safety::tests::ttc_and_headway_reproduce_a_hand_computed_fixture".to_string(),
            "safety::tests::edies_aggregates_reproduce_a_hand_computed_fixture".to_string(),
            "safety::tests::pet_needs_two_different_actors_in_one_cell".to_string(),
        ];
        card
    }

    fn definitions(&self) -> Vec<MetricDef> {
        let s25 = cards::design("08-measurement-and-data.md §2.5");
        let ssam = cards::paper("FHWA-HRT-08-051 (SSAM); 04-models.md §11");
        let edie =
            cards::paper("L. C. Edie 1963; FHWA Traffic Flow Theory ch. 2; 04-models.md §2.9");
        vec![
            MetricDef::new(
                "ttc_min",
                "s",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::TIME_S,
                "Time to collision for a closing same-lane pair: `gap / (v_follower − \
                 v_leader)`, sampled at every mobility step. Reported as a distribution; the \
                 minimum of the run is its `min`.",
            )
            .with_dims([Dim::T])
            .with_source(ssam.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("vehicle length: the gap is between reported lane positions")
            .not_accounting_for("crossing conflicts, which need trajectory intersection")
            .not_accounting_for("a pair that is not closing, which has no time to collision"),
            MetricDef::new(
                "ttc_conflicts",
                "count",
                Agg::Count,
                Visibility::Gt,
                Quantum::COUNT,
                "Same-lane pair-steps whose time to collision is below the 1.5 s threshold \
                 (the one verified threshold in 04-models.md §11). The comparison is made on \
                 the quantised value, so a boundary case cannot flip between platforms.",
            )
            .with_dims([Dim::T])
            .with_source(ssam.clone())
            .with_min_samples(1)
            .not_accounting_for("distinct conflicts: a pair closing for ten steps counts ten times")
            .not_accounting_for("the threshold's own uncertainty"),
            MetricDef::new(
                "pet",
                "s",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::TIME_S,
                "Post-encroachment time: the interval between one actor leaving a position \
                 and a different actor arriving at it, resolved to cells of `pet_cell_m`.",
            )
            .with_dims([Dim::T])
            .with_source(ssam.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("the conflict angle")
            .not_accounting_for("vehicle extent beyond one cell")
            .not_accounting_for("a cell revisited by the same actor, which is not a conflict"),
            MetricDef::new(
                "drac",
                "m/s²",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::SPEED,
                "The deceleration rate required to avoid the crash: `(v_follower − \
                 v_leader)² / (2·gap)` for a closing same-lane pair.",
            )
            .with_dims([Dim::T])
            .with_source(ssam)
            .with_min_samples(self.min_samples)
            .not_accounting_for("the follower's braking capability or the road surface")
            .not_accounting_for("vehicle length, as with ttc_min"),
            MetricDef::new(
                "headway_time",
                "s",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::TIME_S,
                "Time headway: the gap to the leader divided by the follower's speed.",
            )
            .with_dims([Dim::T])
            .with_source(s25.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("a stopped follower, which contributes no sample")
            .not_accounting_for("vehicle length"),
            MetricDef::new(
                "headway_distance",
                "m",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::LENGTH_M,
                "Distance headway: the difference of lane positions between a follower and \
                 its leader.",
            )
            .with_dims([Dim::T])
            .with_source(s25.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for("vehicle length, so this is centre-to-centre, not gap")
            .not_accounting_for("vehicles on other lanes"),
            MetricDef::new(
                "flow",
                "veh/h",
                Agg::Rate,
                Visibility::Gt,
                Quantum::TRAFFIC,
                "Edie's flow over the window and the lane: `Σ v·Δt / (L·T)`, in veh/h.",
            )
            .with_dims([Dim::T, Dim::Region])
            .with_source(edie.clone())
            .with_min_samples(1)
            .not_accounting_for("lanes whose length the run did not declare")
            .not_accounting_for("the lane's capacity, which is a property of the road"),
            MetricDef::new(
                "density",
                "veh/km",
                Agg::Rate,
                Visibility::Gt,
                Quantum::TRAFFIC,
                "Edie's density over the window and the lane: `Σ Δt / (L·T)`, in veh/km.",
            )
            .with_dims([Dim::T, Dim::Region])
            .with_source(edie.clone())
            .with_min_samples(1)
            .not_accounting_for("lanes whose length the run did not declare")
            .not_accounting_for("vehicle length, so this is a count density, not an occupancy"),
            MetricDef::new(
                "mean_speed",
                "m/s",
                Agg::Mean,
                Visibility::Gt,
                Quantum::SPEED,
                "The **space**-mean speed, `q / k` — not the time-mean speed, which is 6–12 % \
                 higher on a mixed-speed signalised dataset (Wardrop 1952).",
            )
            .with_dims([Dim::T, Dim::Region])
            .with_source(edie)
            .with_min_samples(1)
            .not_accounting_for("lanes whose length the run did not declare")
            .not_accounting_for("the time-mean speed, which is a different quantity"),
            MetricDef::new(
                "speed",
                "m/s",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::SPEED,
                "The distribution of sampled actor speeds over the window.",
            )
            .with_dims([Dim::T])
            .with_source(s25.clone())
            .with_min_samples(self.min_samples)
            .not_accounting_for(
                "the sampling bias: this is time-weighted, so a stopped vehicle dominates it",
            ),
            MetricDef::new(
                "acceleration",
                "m/s²",
                Agg::Distribution,
                Visibility::Gt,
                Quantum::SPEED,
                "The distribution of sampled actor accelerations over the window.",
            )
            .with_dims([Dim::T])
            .with_source(s25)
            .with_min_samples(self.min_samples)
            .not_accounting_for("the same time-weighting bias as `speed`")
            .not_accounting_for("jerk, which the kinematics channel does not carry"),
        ]
    }

    fn def(&self, name: &str) -> MetricDef {
        self.definitions()
            .into_iter()
            .find(|d| d.name == name)
            .unwrap_or_else(|| panic!("metric {name} is not one of this provider's definitions"))
    }

    fn on_kinematics(&mut self, v: GtKinematicsView) {
        if self.frame_t != Some(v.t) {
            self.close_frame();
            self.frame_t = Some(v.t);
        }
        self.frame.insert(v.actor, v);
    }

    /// Processes the assembled frame: pairing, PET cells, distributions and Edie's terms.
    fn close_frame(&mut self) {
        let Some(t) = self.frame_t.take() else {
            self.frame.clear();
            return;
        };
        let frame = core::mem::take(&mut self.frame);
        let dt_s = self.dt.as_secs_f64();

        // --- per-actor quantities -------------------------------------------------------
        let mut by_lane: BTreeMap<u32, Vec<OnLane>> = BTreeMap::new();
        for (actor, v) in &frame {
            self.speed.observe(v.speed_mps);
            if let Some(a) = v.acc_mps2 {
                self.acceleration.observe(a);
            }
            // The PET cell. Integer coordinates, so the cell a position falls in is a
            // property of the declared grid rather than of the last bit of a float.
            let cx = Quantum::new(self.cell_m).grid(v.x_m);
            let cy = Quantum::new(self.cell_m).grid(v.y_m);
            match self.cell_last.insert((cx, cy), (*actor, t)) {
                Some((prev_actor, prev_t))
                    if prev_actor != *actor
                        && t >= prev_t
                        && (t - prev_t) <= self.pet_horizon.as_nanos() =>
                {
                    self.pet.observe(Duration::between(prev_t, t).as_secs_f64());
                }
                _ => {}
            }
            if let (Some(lane), Some(pos)) = (v.lane, v.lane_pos_m) {
                by_lane.entry(lane).or_default().push(OnLane {
                    actor: *actor,
                    pos_grid: Quantum::LENGTH_M.grid(pos),
                    pos_m: Quantum::LENGTH_M.quantise(pos),
                    speed_mps: v.speed_mps,
                });
            }
            if let Some(lane) = v.lane {
                if self.lane_length_m.contains_key(&lane) {
                    let acc = self.lanes.entry(lane).or_default();
                    acc.samples += 1;
                    acc.distance.observe(v.speed_mps * dt_s);
                } else {
                    self.undeclared_lanes.insert(lane);
                }
            }
        }

        // --- same-lane pairing ----------------------------------------------------------
        for vehicles in by_lane.values_mut() {
            // Sorted by the integer grid index, tie-broken by actor id: a total order that
            // two platforms cannot disagree about.
            vehicles.sort_by(|a, b| a.pos_grid.cmp(&b.pos_grid).then(a.actor.cmp(&b.actor)));
            for pair in vehicles.windows(2) {
                let follower = pair[0];
                let leader = pair[1];
                let gap = leader.pos_m - follower.pos_m;
                if gap <= 0.0 {
                    // Two vehicles on the same grid point: no gap, so no surrogate measure.
                    continue;
                }
                self.headway_distance.observe(gap);
                if follower.speed_mps > 0.0 {
                    self.headway_time.observe(gap / follower.speed_mps);
                }
                let closing = follower.speed_mps - leader.speed_mps;
                if closing > 0.0 {
                    let ttc = gap / closing;
                    self.ttc.observe(ttc);
                    // D10: the threshold comparison is on the quantised value, never on the
                    // raw quotient.
                    if Quantum::TIME_S.grid(ttc) < self.ttc_threshold_grid {
                        self.ttc_conflicts += 1;
                    }
                    self.drac.observe(closing * closing / (2.0 * gap));
                }
            }
        }
    }

    fn window_secs(&self, at: SimTime) -> Option<f64> {
        if at <= self.window_start {
            return None;
        }
        Some(Duration::between(self.window_start, at).as_secs_f64())
    }
}

impl Model for SafetyProvider {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl MetricProvider for SafetyProvider {
    fn defs(&self) -> Vec<MetricDef> {
        self.definitions()
    }

    fn subscribe(&self) -> Vec<ChannelName> {
        vec![GtKinematicsView::channel_name()]
    }

    fn on_event(&mut self, ev: &EventRecord) {
        self.on_decoded(&Decoded::new(ev));
    }

    fn on_decoded(&mut self, ev: &Decoded<'_>) {
        if ev.channel() == GtKinematicsView::CHANNEL {
            ev.with(|v: Option<&GtKinematicsView>| match v {
                Some(v) => self.on_kinematics(v.clone()),
                None => self.rejected += 1,
            });
        }
    }

    fn flush(&mut self, at: SimTime) -> Vec<MetricSample> {
        // The frame in progress belongs to this window.
        self.close_frame();
        let secs = self.window_secs(at);
        let mut out = Vec::new();

        let push_distribution = |out: &mut Vec<MetricSample>, def: MetricDef, d: Distribution| {
            let min = def.min_samples;
            out.push(MetricSample::new(
                &def,
                at,
                Dims::new(),
                SampleValue::Distribution(d.summary(min)),
            ));
        };
        push_distribution(
            &mut out,
            self.def("ttc_min"),
            core::mem::replace(&mut self.ttc, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("pet"),
            core::mem::replace(&mut self.pet, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("drac"),
            core::mem::replace(&mut self.drac, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("headway_time"),
            core::mem::replace(&mut self.headway_time, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("headway_distance"),
            core::mem::replace(&mut self.headway_distance, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("speed"),
            core::mem::replace(&mut self.speed, Distribution::new()),
        );
        push_distribution(
            &mut out,
            self.def("acceleration"),
            core::mem::replace(&mut self.acceleration, Distribution::new()),
        );
        out.push(MetricSample::new(
            &self.def("ttc_conflicts"),
            at,
            Dims::new(),
            SampleValue::count(core::mem::take(&mut self.ttc_conflicts)),
        ));

        // --- Edie's aggregates, per lane -------------------------------------------------
        let flow_def = self.def("flow");
        let density_def = self.def("density");
        let speed_def = self.def("mean_speed");
        for (lane, acc) in core::mem::take(&mut self.lanes) {
            let mut dims = Dims::new();
            dims.insert(Dim::Region, DimValue::index(u64::from(lane)));
            let length = self.lane_length_m.get(&lane).copied().unwrap_or(0.0);
            let time_spent = (acc.samples as f64) * self.dt.as_secs_f64();
            let distance = acc.distance.sum(1).point().unwrap_or(0.0);
            let (flow, density, mean) = match secs {
                Some(t) if length > 0.0 && t > 0.0 => {
                    let area = length * t;
                    (
                        Estimate::Value {
                            point: distance / area * 3600.0,
                            n: acc.samples,
                        },
                        Estimate::Value {
                            point: time_spent / area * 1000.0,
                            n: acc.samples,
                        },
                        // The space-mean speed is q/k = distance/time_spent. A lane with no
                        // time spent on it has no mean speed, which is what ratio_of_sums
                        // answers.
                        ratio_of_sums(distance, time_spent, acc.samples, 1),
                    )
                }
                _ => (
                    Estimate::Insufficient {
                        n: acc.samples,
                        required: 1,
                    },
                    Estimate::Insufficient {
                        n: acc.samples,
                        required: 1,
                    },
                    crate::stats::RatioEstimate::Insufficient {
                        trials: acc.samples,
                        required: 1,
                    },
                ),
            };
            out.push(MetricSample::new(
                &flow_def,
                at,
                dims.clone(),
                SampleValue::Scalar(flow),
            ));
            out.push(MetricSample::new(
                &density_def,
                at,
                dims.clone(),
                SampleValue::Scalar(density),
            ));
            out.push(MetricSample::new(
                &speed_def,
                at,
                dims,
                SampleValue::Ratio(mean),
            ));
        }

        // PET cells are kept across windows: a conflict spans the boundary as easily as not.
        // They are pruned to the horizon so the map does not grow with the run.
        let cutoff = at.saturating_sub(self.pet_horizon.as_nanos());
        self.cell_last.retain(|_, (_, t)| *t >= cutoff);
        self.window_start = at;
        out
    }

    fn rejected(&self) -> u64 {
        self.rejected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::ctx::OwnedRecord;

    fn kin(
        t: SimTime,
        actor: u32,
        lane: Option<u32>,
        pos: f64,
        speed: f64,
        acc: Option<f64>,
    ) -> OwnedRecord {
        let mut j = json!({
            "t": t, "actor": actor, "x_m": pos, "y_m": 0.0, "speed_mps": speed,
        });
        if let Some(l) = lane {
            j["lane"] = json!(l);
            j["lane_pos_m"] = json!(pos);
        }
        if let Some(a) = acc {
            j["acc_mps2"] = json!(a);
        }
        OwnedRecord {
            channel: "gt.kinematics",
            visibility: Visibility::Gt,
            json: serde_json::to_vec(&j).unwrap(),
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
        let p = SafetyProvider::new(0);
        p.validate_defs().unwrap();
        p.card().validate().unwrap();
        p.card().check_api_version().unwrap();
        // The two todo-calibrate parameters carry their plans (registry rule R1).
        let todo: Vec<&str> = p.card().todo_calibrate().map(|x| x.name.as_str()).collect();
        assert_eq!(todo, vec!["pet_cell_m", "pet_horizon_s"]);
    }

    /// The hand-computed fixture: a follower at 0 m doing 20 m/s behind a leader at 30 m
    /// doing 10 m/s. Gap 30 m, closing speed 10 m/s, so TTC = 3 s, time headway =
    /// 30/20 = 1.5 s, distance headway 30 m, DRAC = 10²/(2·30) = 1.666… m/s².
    #[test]
    fn ttc_and_headway_reproduce_a_hand_computed_fixture() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, Some(7), 0.0, 20.0, Some(0.5)));
        p.on_event(&kin(0, 2, Some(7), 30.0, 10.0, Some(-1.0)));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "ttc_min").value.point(), Some(3.0));
        assert_eq!(sample(&s, "headway_time").value.point(), Some(1.5));
        assert_eq!(sample(&s, "headway_distance").value.point(), Some(30.0));
        assert_eq!(sample(&s, "drac").value.point(), Some(1.667));
        assert_eq!(
            sample(&s, "ttc_conflicts").value,
            SampleValue::count(0),
            "3 s is above the 1.5 s threshold"
        );
        // Speeds 10 and 20: mean 15. Accelerations 0.5 and −1.0: mean −0.25.
        assert_eq!(sample(&s, "speed").value.point(), Some(15.0));
        assert_eq!(sample(&s, "acceleration").value.point(), Some(-0.25));
    }

    #[test]
    fn a_pair_that_is_not_closing_has_no_ttc() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        // The leader is faster, so the gap is opening.
        p.on_event(&kin(0, 1, Some(7), 0.0, 10.0, None));
        p.on_event(&kin(0, 2, Some(7), 30.0, 20.0, None));
        let s = p.flush(1_000_000_000);
        assert!(sample(&s, "ttc_min").value.is_insufficient());
        assert!(sample(&s, "drac").value.is_insufficient());
        // …but the headways are still defined.
        assert_eq!(sample(&s, "headway_distance").value.point(), Some(30.0));
    }

    #[test]
    fn a_ttc_below_the_threshold_is_counted_and_the_comparison_is_quantised() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        // Gap 10 m, closing 10 m/s → TTC 1.0 s, below 1.5 s.
        p.on_event(&kin(0, 1, Some(1), 0.0, 20.0, None));
        p.on_event(&kin(0, 2, Some(1), 10.0, 10.0, None));
        // Gap 15 m, closing 10 m/s → TTC exactly 1.5 s, which is *not* below the threshold.
        p.on_event(&kin(0, 3, Some(2), 0.0, 20.0, None));
        p.on_event(&kin(0, 4, Some(2), 15.0, 10.0, None));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "ttc_conflicts").value, SampleValue::count(1));
    }

    #[test]
    fn a_stopped_follower_has_no_time_headway_and_no_infinity() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, Some(7), 0.0, 0.0, None));
        p.on_event(&kin(0, 2, Some(7), 30.0, 0.0, None));
        let s = p.flush(1_000_000_000);
        assert!(sample(&s, "headway_time").value.is_insufficient());
        assert_eq!(sample(&s, "headway_distance").value.point(), Some(30.0));
        for f in s.iter().flat_map(MetricSample::floats) {
            assert!(f.is_finite(), "{f}");
        }
    }

    /// Edie on a 100 m lane over a 1 s window with 100 ms samples: one vehicle present for
    /// all ten samples at 20 m/s.
    ///
    /// `T_s = 10·0.1 = 1 s`, `D = 10·20·0.1 = 20 m`, `L·T = 100 m·1 s = 100 m·s`.
    /// `k = 1/100 = 0.01 veh/m = 10 veh/km`; `q = 20/100 = 0.2 veh/s = 720 veh/h`;
    /// `v̄ = 20/1 = 20 m/s`.
    #[test]
    fn edies_aggregates_reproduce_a_hand_computed_fixture() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.declare_lane_length(7, 100.0);
        for i in 0..10u64 {
            p.on_event(&kin(
                i * 100_000_000,
                1,
                Some(7),
                (i as f64) * 2.0,
                20.0,
                None,
            ));
        }
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "density|region=7").value.point(), Some(10.0));
        assert_eq!(sample(&s, "flow|region=7").value.point(), Some(720.0));
        assert_eq!(sample(&s, "mean_speed|region=7").value.point(), Some(20.0));
        // And the identity q = k·v̄ holds on the reported numbers: 10 veh/km × 20 m/s =
        // 10 veh/km × 72 km/h = 720 veh/h.
        assert_eq!(10.0 * 72.0, 720.0);
    }

    #[test]
    fn an_undeclared_lane_has_no_aggregate_and_is_counted() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, Some(42), 0.0, 20.0, None));
        let s = p.flush(1_000_000_000);
        assert!(!s.iter().any(|x| x.metric == "flow"));
        assert!(p.undeclared_lanes().contains(&42));
    }

    #[test]
    fn pet_needs_two_different_actors_in_one_cell() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        // Actor 1 occupies the cell at t = 0; actor 2 arrives 1.5 s later.
        p.on_event(&kin(0, 1, None, 0.25, 5.0, None));
        p.on_event(&kin(1_500_000_000, 2, None, 0.25, 5.0, None));
        let s = p.flush(3_000_000_000);
        assert_eq!(sample(&s, "pet").value.point(), Some(1.5));

        // The same actor revisiting its own cell is not a conflict.
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, None, 0.25, 5.0, None));
        p.on_event(&kin(1_500_000_000, 1, None, 0.25, 5.0, None));
        let s = p.flush(3_000_000_000);
        assert!(sample(&s, "pet").value.is_insufficient());

        // And a gap beyond the horizon is not a conflict either.
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, None, 0.25, 5.0, None));
        p.on_event(&kin(9_000_000_000, 2, None, 0.25, 5.0, None));
        let s = p.flush(10_000_000_000);
        assert!(sample(&s, "pet").value.is_insufficient());
    }

    #[test]
    fn an_empty_window_is_insufficient_and_never_nan() {
        let mut p = SafetyProvider::new(0);
        let s = p.flush(1_000_000_000);
        for key in [
            "ttc_min",
            "pet",
            "drac",
            "headway_time",
            "headway_distance",
            "speed",
            "acceleration",
        ] {
            assert!(sample(&s, key).value.is_insufficient(), "{key}");
        }
        assert_eq!(sample(&s, "ttc_conflicts").value, SampleValue::count(0));
        for f in s.iter().flat_map(MetricSample::floats) {
            assert!(f.is_finite(), "{f}");
        }
    }

    /// Permuting the actors within a frame must not move a bit: the pairing sorts by the
    /// integer lane-position grid index, and the distributions sort before they reduce.
    #[test]
    fn a_frames_results_do_not_depend_on_the_order_of_its_actors() {
        let positions = [(1u32, 0.0, 22.5), (2, 13.75, 18.0), (3, 41.125, 9.5)];
        let run = |order: Vec<usize>| {
            let mut p = SafetyProvider::new(0).with_min_samples(1);
            p.declare_lane_length(3, 200.0);
            for i in order {
                let (a, pos, v) = positions[i];
                p.on_event(&kin(0, a, Some(3), pos, v, Some(0.125)));
            }
            p.flush(1_000_000_000)
                .into_iter()
                .map(|s| {
                    (
                        s.key(),
                        s.floats().iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        };
        let forward = run(vec![0, 1, 2]);
        assert_eq!(forward, run(vec![2, 1, 0]));
        assert_eq!(forward, run(vec![1, 2, 0]));
    }

    #[test]
    fn every_safety_metric_is_tagged_ground_truth() {
        for d in SafetyProvider::new(0).defs() {
            assert_eq!(d.visibility, Visibility::Gt, "{}", d.name);
            assert!(
                !d.visibility.allowed_on_node_channel(),
                "{} must not reach a node channel",
                d.name
            );
        }
    }

    #[test]
    fn a_duplicate_sample_for_one_actor_in_one_frame_does_not_double_count() {
        let mut p = SafetyProvider::new(0).with_min_samples(1);
        p.on_event(&kin(0, 1, Some(7), 0.0, 20.0, None));
        p.on_event(&kin(0, 1, Some(7), 0.0, 20.0, None));
        let s = p.flush(1_000_000_000);
        assert_eq!(sample(&s, "speed").value.n(), 1);
    }
}
