//! Message generation rules — the `MessageGenerator` seam of 03-interfaces.md §6.
//!
//! A generator decides **when** a node sends and **what shape** the message has. It does
//! not build the message and it does not encode it: that is [`crate::cam`] and
//! [`crate::codec`]. Keeping the two apart is what lets the CAM triggering rules be tested
//! against the standard's own numbers without an ASN.1 encoder anywhere in the test.
//!
//! Two generators live here, with the parameters 04-models.md §8.1 cites:
//!
//! * [`CamGenerator`] — `generator/cam-en302637-2`, the real EN 302 637-2 V1.4.1 §6.1.3
//!   state machine: `T_CheckCamGen`, the three dynamics triggers, the adaptive `T_GenCam`,
//!   the `N_GenCam` reset and the 500 ms low-frequency container.
//! * [`BsmGenerator`] — `generator/bsm-j2945-1`, 10 Hz nominal with Part I in every
//!   message.
//!
//! # Why the trait is generic and the state machine is not
//!
//! [`v2xw_core::ctx::Ctx`] and [`v2xw_core::nodeview::NodeView`] both carry associated
//! types, so `&mut dyn Ctx` on its own does not name a type. [`MessageGenerator`] therefore
//! takes the context and the view as type parameters; once the engine fixes them,
//! `dyn MessageGenerator<EngineCtx, EngineNodeView>` is an ordinary trait object, which is
//! how in-process plug-ins are called (ADR 0007 §8).
//!
//! The decision logic itself is a plain struct — [`CamTriggerState`] — that takes a time,
//! a [`CamDynamics`] and a [`DccState`] and returns a [`CamDecision`]. Every trigger path
//! is unit-tested through that struct directly, with no engine, no node and no ASN.1.
//!
//! # Belief, not truth
//!
//! Both generators read the node's position from [`v2xw_core::nodeview::NodeView::position`],
//! which is the GNSS model's *estimate*. A generator that used ground truth would make a
//! spoofed vehicle send CAMs on its real trajectory while claiming a false one — the exact
//! opposite of what a spoofing attack does (invariant I-C2).

use v2xw_core::card::{
    Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation, ValidationStatus,
};
use v2xw_core::ctx::{Ctx, CtxExt, Record, Visibility};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::nodeview::NodeView;
use v2xw_core::time::{Duration, SimTime};

use crate::codec::MsgType;
use crate::units;

// =========================================================================================
// The seam
// =========================================================================================

/// The part of the decentralised-congestion-control state a message generator needs.
///
/// The DCC family (04-models.md §6) owns the full state — CBR measurements, the
/// TS 102 687 state machine, the transmit-power and datarate decisions. A generator needs
/// exactly two things from it: the minimum time DCC allows between transmissions, and the
/// channel load, which it records but does not act on. This struct is that subset, declared
/// here so the message layer can be built and tested before the DCC crate exists; when it
/// lands it either re-exports this type or converts into it.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DccState {
    /// `T_GenCam_Dcc` in EN 302 637-2 terms, `T_off` in C2C-CC RS 2037 (RS_BSP_293): the
    /// minimum time that must elapse between two transmissions on this channel.
    pub t_off: Duration,
    /// Measured channel busy ratio, `0.0..=1.0`, if the node measures one.
    pub cbr: Option<f64>,
}

impl DccState {
    /// DCC imposing nothing: the state an unloaded channel is in, and the right default
    /// for a scenario that does not model congestion control.
    pub const UNRESTRICTED: DccState = DccState {
        t_off: Duration::ZERO,
        cbr: None,
    };
}

impl Default for DccState {
    fn default() -> Self {
        Self::UNRESTRICTED
    }
}

/// Something that happened at the application layer and that a generator may react to.
///
/// Deliberately small. The full hazard taxonomy is DENM's (`CauseCodeV2` has some 40
/// causes), and it belongs in [`crate::denm`]; what a *generator* needs is the handful of
/// events that change its own cadence.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub enum AppEventKind {
    /// The vehicle is braking hard. 04-models.md §8.1 puts the J2945/1 EEBL event flag at
    /// a deceleration of 0.4 g, which is 3.92 m/s².
    HardBraking {
        /// Deceleration, m/s², positive.
        decel_mps2: f64,
    },
    /// A hazard the application wants announced. The cause is DENM's business; this only
    /// says that one exists.
    HazardDetected,
    /// A previously announced hazard is over.
    HazardCleared,
}

/// An application event with the instant it happened at.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppEvent {
    /// What happened.
    pub kind: AppEventKind,
    /// When, on the node's own clock.
    pub at: SimTime,
}

/// Why a generator asked for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum GenReason {
    /// The first message after the service was activated.
    First,
    /// One or more of the dynamics thresholds was crossed (EN 302 637-2 trigger 1).
    Dynamics(DynamicsTriggers),
    /// The periodic deadline elapsed (EN 302 637-2 trigger 2; J2945/1's 10 Hz).
    Periodic,
    /// An application event asked for it.
    Event,
}

/// Which of the three EN 302 637-2 dynamics thresholds were crossed.
///
/// All three are reported, not just the first: a CAM sent because the vehicle both turned
/// and accelerated is a different observation from one sent because it only turned, and a
/// generation-rate analysis that collapsed them would not be able to tell.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, Hash,
)]
pub struct DynamicsTriggers {
    /// Heading changed by more than the threshold.
    pub heading: bool,
    /// Position moved by more than the threshold.
    pub position: bool,
    /// Speed changed by more than the threshold.
    pub speed: bool,
}

impl DynamicsTriggers {
    /// True if any threshold was crossed.
    pub const fn any(self) -> bool {
        self.heading || self.position || self.speed
    }
}

/// One message a generator wants sent.
///
/// It says *what* and *why*, not *how*: the node runtime turns it into a real message with
/// [`crate::cam::build_cam`] and hands that to a [`crate::codec::MessageCodec`]. The split
/// is what keeps the triggering rules testable without an encoder.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GenRequest {
    /// Which message to build.
    pub msg_type: MsgType,
    /// Why it was asked for.
    pub reason: GenReason,
    /// Whether this one carries the low-frequency container (CAM) or the equivalent
    /// low-cadence content of its message type.
    pub include_low_frequency: bool,
    /// The instant the generator decided at, on the node's own clock.
    pub at: SimTime,
}

/// Decides when a node sends, and with what content (03-interfaces.md §6).
///
/// Generic over the engine's context and node-view types; see the module documentation for
/// why that is not the same as being un-object-safe.
pub trait MessageGenerator<C, V>: Model
where
    C: Ctx + ?Sized,
    V: NodeView + ?Sized,
{
    /// How often the engine must call [`MessageGenerator::on_tick`].
    ///
    /// EN 302 637-2 calls this `T_CheckCamGen` and requires it to be no greater than
    /// `T_GenCamMin`; a generator whose check interval were coarser could not honour its
    /// own minimum period. The engine schedules a [`v2xw_core::event::EventClass::NodeTask`]
    /// at this cadence.
    fn check_interval(&self) -> Duration;

    /// Called every [`MessageGenerator::check_interval`].
    fn on_tick(&mut self, ctx: &mut C, node: &V, dcc: &DccState) -> Vec<GenRequest>;

    /// Called when the application reports an event.
    ///
    /// Defaulted to "no extra message": most generators are purely periodic, and a
    /// generator that reacts to events says so by overriding this.
    fn on_event(&mut self, _ctx: &mut C, _node: &V, _ev: &AppEvent) -> Vec<GenRequest> {
        Vec::new()
    }
}

/// The record a generator emits when it decides to send.
///
/// [`Visibility::Node`]: everything in it is the node's own decision made from its own
/// belief, so a detector or a dataset may read it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GenerationRecord {
    /// Which node decided.
    pub node: NodeId,
    /// The node's own clock at the decision.
    pub t_ns: SimTime,
    /// What it decided to send.
    pub msg_type: MsgType,
    /// Why.
    pub reason: GenReason,
    /// Whether the low-frequency content is included.
    pub low_frequency: bool,
    /// The period the generator will next aim for, nanoseconds. For a CAM this is
    /// `T_GenCam`, which the trigger-1 rule rewrites.
    pub next_period_ns: u64,
}

impl Record for GenerationRecord {
    const CHANNEL: &'static str = "msg.generation";
    const VISIBILITY: Visibility = Visibility::Node;
}

// =========================================================================================
// CAM — EN 302 637-2 V1.4.1 §6.1.3
// =========================================================================================

/// Model id of the CAM generator.
pub const CAM_GENERATOR_ID: &str = "generator/cam-en302637-2";

/// The EN 302 637-2 parameters, with the standard's defaults.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CamGenParams {
    /// `T_GenCamMin`, the shortest permitted interval between two CAMs. 100 ms
    /// [EN 302 637-2 V1.4.1 §6.1.3].
    pub t_gen_cam_min: Duration,
    /// `T_GenCamMax`, the longest. 1,000 ms [ibid.].
    pub t_gen_cam_max: Duration,
    /// `T_CheckCamGen`, how often the rules are evaluated. Must be ≤ `t_gen_cam_min`
    /// [ibid.]; defaults to it.
    pub t_check_cam_gen: Duration,
    /// `N_GenCam`, the number of consecutive periodic CAMs after a dynamics-triggered one
    /// before `T_GenCam` returns to `T_GenCamMax`. 3 [ibid.].
    pub n_gen_cam: u8,
    /// Heading-change threshold, radians. 4° [ibid. §6.1.3].
    pub heading_threshold_rad: f64,
    /// Position-change threshold, metres. 4 m [ibid.].
    pub position_threshold_m: f64,
    /// Speed-change threshold, m/s. 0.5 m/s [ibid.].
    pub speed_threshold_mps: f64,
    /// Minimum interval between two CAMs that carry the low-frequency container. 500 ms
    /// [ibid. §6.1.4].
    pub low_frequency_interval: Duration,
}

impl CamGenParams {
    /// Four degrees in radians — the heading threshold, written once.
    pub const FOUR_DEGREES_RAD: f64 = 4.0 * (core::f64::consts::PI / 180.0);

    /// The standard's defaults.
    pub const fn en302637_2() -> Self {
        Self {
            t_gen_cam_min: Duration::from_millis(100),
            t_gen_cam_max: Duration::from_millis(1_000),
            t_check_cam_gen: Duration::from_millis(100),
            n_gen_cam: 3,
            heading_threshold_rad: Self::FOUR_DEGREES_RAD,
            position_threshold_m: 4.0,
            speed_threshold_mps: 0.5,
            low_frequency_interval: Duration::from_millis(500),
        }
    }
}

impl Default for CamGenParams {
    fn default() -> Self {
        Self::en302637_2()
    }
}

/// The node dynamics the CAM rules are evaluated against, all from the node's own belief.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CamDynamics {
    /// Believed position, world ENU metres.
    pub pos: Vec3,
    /// Believed heading, ENU radians (D6).
    pub heading_rad: f64,
    /// Believed ground speed, m/s.
    pub speed_mps: f64,
}

impl CamDynamics {
    /// Reads the dynamics a CAM is triggered on out of a position belief.
    pub fn from_estimate(p: &v2xw_core::belief::PositionEstimate) -> Self {
        Self {
            pos: p.pos,
            heading_rad: p.heading_rad,
            speed_mps: p.ground_speed_mps(),
        }
    }
}

/// What [`CamTriggerState::check`] decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CamDecision {
    /// Why this CAM is being sent.
    pub reason: GenReason,
    /// Whether it carries the low-frequency container.
    pub include_low_frequency: bool,
    /// `T_GenCam` after this decision — what the next periodic deadline will be measured
    /// against.
    pub t_gen_cam: Duration,
}

/// The state EN 302 637-2 §6.1.3 keeps between checks.
#[derive(Debug, Clone, PartialEq)]
struct LastCam {
    t: SimTime,
    dynamics: CamDynamics,
}

/// The CAM triggering state machine, with no engine attached.
///
/// # The rules, as implemented
///
/// On every check (every `T_CheckCamGen`):
///
/// 1. If no CAM has been sent yet, send one. It is the first CAM, so it carries the
///    low-frequency container (§6.1.4) and `T_GenCam` stays at `T_GenCamMax`.
/// 2. Otherwise, let `elapsed` be the time since the last CAM. If
///    `elapsed < max(T_GenCamMin, T_GenCam_Dcc)`, send nothing — this is the floor that
///    both the standard's own minimum and DCC impose.
/// 3. Otherwise, if the heading has changed by more than 4°, **or** the position by more
///    than 4 m, **or** the speed by more than 0.5 m/s since the last CAM, send one
///    (trigger 1). `T_GenCam` becomes `elapsed`, and the consecutive-periodic counter
///    resets to zero.
/// 4. Otherwise, if `elapsed >= T_GenCam`, send one (trigger 2) and increment the
///    consecutive-periodic counter. When it reaches `N_GenCam`, `T_GenCam` returns to
///    `T_GenCamMax` and the counter resets.
///
/// Step 3 before step 4 matters: a CAM that satisfies both is a *dynamics* CAM, and
/// counting it as periodic would let `T_GenCam` drift back to the maximum while the
/// vehicle is still manoeuvring.
///
/// # Determinism
///
/// Every threshold comparison is made on **quantised** values (build decision D10): the
/// heading on a 1 µrad grid, the distance on a 1 mm grid, the speed on a 1 mm/s grid. A
/// vehicle sitting exactly on a threshold therefore triggers or does not trigger
/// identically on every platform, instead of depending on the last bit of an f64 that came
/// out of a mobility model.
#[derive(Debug, Clone, PartialEq)]
pub struct CamTriggerState {
    params: CamGenParams,
    last: Option<LastCam>,
    t_gen_cam: Duration,
    consecutive_periodic: u8,
    last_low_frequency: Option<SimTime>,
}

impl CamTriggerState {
    /// A fresh state machine with `params`.
    pub fn new(params: CamGenParams) -> Self {
        Self {
            t_gen_cam: params.t_gen_cam_max,
            params,
            last: None,
            consecutive_periodic: 0,
            last_low_frequency: None,
        }
    }

    /// The parameters in force.
    pub fn params(&self) -> &CamGenParams {
        &self.params
    }

    /// The current `T_GenCam`.
    pub fn t_gen_cam(&self) -> Duration {
        self.t_gen_cam
    }

    /// How many consecutive periodic CAMs have been sent since the last dynamics one.
    pub fn consecutive_periodic(&self) -> u8 {
        self.consecutive_periodic
    }

    /// When the last CAM was sent, if any.
    pub fn last_cam_at(&self) -> Option<SimTime> {
        self.last.as_ref().map(|l| l.t)
    }

    /// Evaluates the rules at `now`. Returns the decision, or `None` for "not yet".
    ///
    /// `now` is the node's **own** believed time, because every interval in EN 302 637-2 is
    /// measured on the sender's clock — which is what makes a clock attack visible as a
    /// changed CAM rate rather than as nothing at all.
    pub fn check(
        &mut self,
        now: SimTime,
        dynamics: &CamDynamics,
        dcc: &DccState,
    ) -> Option<CamDecision> {
        let Some(last) = self.last.clone() else {
            return Some(self.emit(now, dynamics, GenReason::First));
        };

        // A clock that went backwards (a step correction, or an attacker) cannot produce a
        // negative interval; treat it as "no time has passed", which delays the next CAM
        // rather than emitting a burst.
        let elapsed = Duration::between(last.t, now);
        let floor = self.params.t_gen_cam_min.max(dcc.t_off);
        if elapsed < floor {
            return None;
        }

        let triggers = self.dynamics_triggers(&last.dynamics, dynamics);
        if triggers.any() {
            // Trigger 1. T_GenCam becomes the interval that was actually achieved, bounded
            // by the standard's own limits, and the periodic run starts again.
            self.t_gen_cam = elapsed.clamp(self.params.t_gen_cam_min, self.params.t_gen_cam_max);
            self.consecutive_periodic = 0;
            return Some(self.emit(now, dynamics, GenReason::Dynamics(triggers)));
        }

        if elapsed >= self.t_gen_cam {
            // Trigger 2.
            self.consecutive_periodic = self.consecutive_periodic.saturating_add(1);
            if self.consecutive_periodic >= self.params.n_gen_cam {
                self.t_gen_cam = self.params.t_gen_cam_max;
                self.consecutive_periodic = 0;
            }
            return Some(self.emit(now, dynamics, GenReason::Periodic));
        }

        None
    }

    /// Whether the three dynamics thresholds are crossed between two states.
    ///
    /// Public because a detector that wants to ask "would this peer's CAM have been
    /// triggered?" needs exactly this predicate, and reimplementing it is how two answers
    /// to one question appear.
    pub fn dynamics_triggers(&self, from: &CamDynamics, to: &CamDynamics) -> DynamicsTriggers {
        let heading_delta = math::quantize_to(
            angular_difference(to.heading_rad, from.heading_rad).abs(),
            units::Q_RAD,
        );
        let position_delta = math::quantize_to(to.pos.distance_2d(from.pos), units::Q_M);
        let speed_delta = math::quantize_to((to.speed_mps - from.speed_mps).abs(), units::Q_MPS);
        DynamicsTriggers {
            heading: heading_delta
                > math::quantize_to(self.params.heading_threshold_rad, units::Q_RAD),
            position: position_delta
                > math::quantize_to(self.params.position_threshold_m, units::Q_M),
            speed: speed_delta > math::quantize_to(self.params.speed_threshold_mps, units::Q_MPS),
        }
    }

    fn emit(&mut self, now: SimTime, dynamics: &CamDynamics, reason: GenReason) -> CamDecision {
        // §6.1.4: the low-frequency container goes in the first CAM, then in a CAM only if
        // at least 500 ms have passed since the last one that carried it.
        let include_low_frequency = match self.last_low_frequency {
            None => true,
            Some(t) => Duration::between(t, now) >= self.params.low_frequency_interval,
        };
        if include_low_frequency {
            self.last_low_frequency = Some(now);
        }
        self.last = Some(LastCam {
            t: now,
            dynamics: *dynamics,
        });
        CamDecision {
            reason,
            include_low_frequency,
            t_gen_cam: self.t_gen_cam,
        }
    }
}

/// The signed difference between two ENU headings, wrapped into `(-π, π]`.
///
/// Arithmetic only — `rem_euclid` is exact for finite inputs — so no transcendental is
/// involved and the result is identical on every platform (ADR 0003).
pub fn angular_difference(a: f64, b: f64) -> f64 {
    const TWO_PI: f64 = core::f64::consts::TAU;
    if !a.is_finite() || !b.is_finite() {
        return 0.0;
    }
    let d = (a - b).rem_euclid(TWO_PI);
    if d > core::f64::consts::PI {
        d - TWO_PI
    } else {
        d
    }
}

/// `generator/cam-en302637-2`: the EN 302 637-2 CAM generation service.
#[derive(Debug, Clone)]
pub struct CamGenerator {
    card: ModelCard,
    state: CamTriggerState,
}

impl Default for CamGenerator {
    fn default() -> Self {
        Self::new(CamGenParams::en302637_2())
    }
}

impl CamGenerator {
    /// A generator with the given parameters and its card.
    pub fn new(params: CamGenParams) -> Self {
        Self {
            card: cam_card(&params),
            state: CamTriggerState::new(params),
        }
    }

    /// The state machine, for inspection and for tests.
    pub fn state(&self) -> &CamTriggerState {
        &self.state
    }
}

fn cam_card(params: &CamGenParams) -> ModelCard {
    let en = |clause: &str| {
        Source::new(
            SourceKind::Standard,
            format!("ETSI EN 302 637-2 V1.4.1 {clause}"),
        )
    };
    let mut card = ModelCard::new(
        CAM_GENERATOR_ID,
        Family::Generator,
        "1.0.0",
        "CAM generation rules of EN 302 637-2: the three dynamics triggers, the adaptive \
         periodic trigger with its N_GenCam reset, and the 500 ms low-frequency container.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        v2xw_core::card::Equation::new(
            "trigger 1 (dynamics)",
            "|heading - heading_last| > 4 deg OR |pos - pos_last| > 4 m OR |v - v_last| > 0,5 m/s",
        ),
        v2xw_core::card::Equation::new(
            "T_GenCam update",
            "trigger 1: T_GenCam := clamp(elapsed, T_GenCamMin, T_GenCamMax); after N_GenCam \
             consecutive trigger-2 CAMs: T_GenCam := T_GenCamMax",
        ),
    ];
    card.parameters = vec![
        Parameter::new(
            "t_gen_cam_min_ms",
            "ms",
            serde_json::json!(params.t_gen_cam_min.as_nanos() / 1_000_000),
            en("§6.1.3"),
        ),
        Parameter::new(
            "t_gen_cam_max_ms",
            "ms",
            serde_json::json!(params.t_gen_cam_max.as_nanos() / 1_000_000),
            en("§6.1.3"),
        ),
        Parameter::new(
            "t_check_cam_gen_ms",
            "ms",
            serde_json::json!(params.t_check_cam_gen.as_nanos() / 1_000_000),
            en("§6.1.3"),
        ),
        Parameter::new(
            "n_gen_cam",
            "count",
            serde_json::json!(params.n_gen_cam),
            en("§6.1.3"),
        ),
        Parameter::new(
            "heading_threshold_deg",
            "degree",
            serde_json::json!(4.0),
            en("§6.1.3"),
        ),
        Parameter::new(
            "position_threshold_m",
            "m",
            serde_json::json!(params.position_threshold_m),
            en("§6.1.3"),
        ),
        Parameter::new(
            "speed_threshold_mps",
            "m/s",
            serde_json::json!(params.speed_threshold_mps),
            en("§6.1.3"),
        ),
        Parameter::new(
            "low_frequency_interval_ms",
            "ms",
            serde_json::json!(params.low_frequency_interval.as_nanos() / 1_000_000),
            en("§6.1.4"),
        ),
    ];
    card.assumptions = vec![
        "Every interval is measured on the sending node's own believed clock, not on \
         simulated time, so a drifting or attacked clock changes the observed CAM rate."
            .to_string(),
        "T_GenCam_Dcc arrives through DccState::t_off and acts only as a floor on the \
         inter-CAM interval; the rest of the DCC state machine is the DCC family's."
            .to_string(),
    ];
    card.limitations = vec![
        "The special-vehicle container's own >= 500 ms cadence and the RSU CAM's >= 1,000 ms \
         cadence (04-models.md §8.1) are not implemented: this crate's builder fills neither \
         container."
            .to_string(),
        "The <= 50 ms generation budget of §6.1.3 is not modelled as a delay; a CAM is \
         assembled instantaneously."
            .to_string(),
    ];
    card.sources = vec![
        en("§6.1.3, §6.1.4, §6.1.5"),
        Source::new(
            SourceKind::Standard,
            "C2C-CC RS 2037 RS_BSP_293, RS_BSP_297 — T_GenCam_Dcc = T_off, N_GenCam = pCamGenNumber",
        ),
        Source::new(
            SourceKind::Paper,
            "C2C-CC TR 2052 Observation 10 — field mean CAM interval 0,33-0,47 s, the \
             validation target",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(
            SourceKind::Paper,
            "C2C-CC TR 2052 Observation 10 (field mean CAM interval 0,33-0,47 s)",
        )],
        tests: vec![
            "generator::tests::heading_change_triggers_a_cam".to_string(),
            "generator::tests::position_change_triggers_a_cam".to_string(),
            "generator::tests::speed_change_triggers_a_cam".to_string(),
            "generator::tests::the_periodic_trigger_fires_at_t_gen_cam".to_string(),
            "generator::tests::n_gen_cam_consecutive_periodic_cams_restore_the_maximum".to_string(),
            "generator::tests::the_low_frequency_container_obeys_the_500_ms_rule".to_string(),
        ],
    };
    card.determinism = v2xw_core::card::Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

impl Model for CamGenerator {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C, V> MessageGenerator<C, V> for CamGenerator
where
    C: Ctx + ?Sized,
    V: NodeView + ?Sized,
{
    fn check_interval(&self) -> Duration {
        self.state.params.t_check_cam_gen
    }

    fn on_tick(&mut self, ctx: &mut C, node: &V, dcc: &DccState) -> Vec<GenRequest> {
        let now = node.believed_time();
        let dynamics = CamDynamics::from_estimate(node.position());
        let Some(decision) = self.state.check(now, &dynamics, dcc) else {
            return Vec::new();
        };
        ctx.emit(GenerationRecord {
            node: node.node(),
            t_ns: now,
            msg_type: MsgType::Cam,
            reason: decision.reason,
            low_frequency: decision.include_low_frequency,
            next_period_ns: decision.t_gen_cam.as_nanos(),
        });
        vec![GenRequest {
            msg_type: MsgType::Cam,
            reason: decision.reason,
            include_low_frequency: decision.include_low_frequency,
            at: now,
        }]
    }
}

// =========================================================================================
// BSM — SAE J2945/1
// =========================================================================================

/// Model id of the BSM generator.
pub const BSM_GENERATOR_ID: &str = "generator/bsm-j2945-1";

/// The J2945/1 parameters this generator uses.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BsmGenParams {
    /// Nominal inter-transmission time. 100 ms, i.e. 10 Hz — the one VERIFIED number in
    /// 04-models.md §8.1's BSM row.
    pub nominal_itt: Duration,
    /// The floor J2945/1 puts under a DCC-stretched interval, `vMinITT`. 100 ms
    /// (04-models.md §8.1, marked UNVERIFIED there).
    pub min_itt: Duration,
    /// The ceiling, `vMaxITT`. 600 ms (ibid., UNVERIFIED).
    pub max_itt: Duration,
}

impl BsmGenParams {
    /// The defaults of 04-models.md §8.1.
    pub const fn j2945_1() -> Self {
        Self {
            nominal_itt: Duration::from_millis(100),
            min_itt: Duration::from_millis(100),
            max_itt: Duration::from_millis(600),
        }
    }
}

impl Default for BsmGenParams {
    fn default() -> Self {
        Self::j2945_1()
    }
}

/// `generator/bsm-j2945-1`: 10 Hz nominal, Part I in every message.
///
/// # What is deliberately not here
///
/// J2945/1's inter-transmission-time control — the congestion-driven stretch of the
/// interval between `vMinITT` and `vMaxITT` — belongs to the DCC family
/// (04-models.md §6.4, `dcc/sae/j2945-1-rate-power`), not to the generator. This generator
/// therefore honours [`DccState::t_off`] as a floor and does nothing else with congestion;
/// when the DCC crate lands it drives the floor and this code does not change. Putting the
/// control loop here instead would mean two models computing a rate and the scenario
/// choosing which one wins by accident.
#[derive(Debug, Clone)]
pub struct BsmGenerator {
    card: ModelCard,
    params: BsmGenParams,
    last_tx: Option<SimTime>,
    msg_count: u8,
}

impl Default for BsmGenerator {
    fn default() -> Self {
        Self::new(BsmGenParams::j2945_1())
    }
}

impl BsmGenerator {
    /// A generator with the given parameters and its card.
    pub fn new(params: BsmGenParams) -> Self {
        Self {
            card: bsm_card(&params),
            params,
            last_tx: None,
            msg_count: 0,
        }
    }

    /// The `msgCnt` the next BSM will carry: J2735 `MsgCount ::= INTEGER (0..127)`,
    /// incremented per message and wrapping.
    pub fn msg_count(&self) -> u8 {
        self.msg_count
    }

    /// When the last BSM was asked for.
    pub fn last_tx(&self) -> Option<SimTime> {
        self.last_tx
    }

    /// The decision, with no engine attached — the testable core.
    pub fn check(&mut self, now: SimTime, dcc: &DccState) -> Option<GenReason> {
        let reason = match self.last_tx {
            None => GenReason::First,
            Some(last) => {
                let elapsed = Duration::between(last, now);
                let interval = self
                    .params
                    .nominal_itt
                    .max(dcc.t_off)
                    .clamp(self.params.min_itt, self.params.max_itt);
                if elapsed < interval {
                    return None;
                }
                GenReason::Periodic
            }
        };
        self.last_tx = Some(now);
        self.msg_count = (self.msg_count + 1) % 128;
        Some(reason)
    }
}

fn bsm_card(params: &BsmGenParams) -> ModelCard {
    let mut card = ModelCard::new(
        BSM_GENERATOR_ID,
        Family::Generator,
        "1.0.0",
        "BSM generation at the J2945/1 nominal 10 Hz, Part I in every message; the \
         inter-transmission-time control loop is the DCC family's.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.parameters = vec![
        Parameter::new(
            "nominal_itt_ms",
            "ms",
            serde_json::json!(params.nominal_itt.as_nanos() / 1_000_000),
            Source::new(
                SourceKind::Standard,
                "SAE J2945/1 — 10 Hz nominal; VERIFIED via secondary sources (NDSS 2024 §II, \
                 Bindel 2021, Rostami 2018), 04-models.md §8.1",
            ),
        ),
        {
            let mut p = Parameter::new(
                "min_itt_ms",
                "ms",
                serde_json::json!(params.min_itt.as_nanos() / 1_000_000),
                Source::todo_calibrate(
                    "vMinITT 100 ms is implied rather than sourced (04-models.md §8.1 marks it \
                     UNVERIFIED)",
                ),
            );
            p.calibration = Some(
                "Read SAE J2945/1 §6 on a machine that holds a licence and record vMinITT and \
                 vMaxITT with their clause numbers."
                    .to_string(),
            );
            p
        },
        {
            let mut p = Parameter::new(
                "max_itt_ms",
                "ms",
                serde_json::json!(params.max_itt.as_nanos() / 1_000_000),
                Source::todo_calibrate(
                    "vMaxITT 600 ms is implied rather than sourced (04-models.md §8.1 marks it \
                     UNVERIFIED)",
                ),
            );
            p.calibration = Some(
                "Read SAE J2945/1 §6 on a machine that holds a licence and record vMinITT and \
                 vMaxITT with their clause numbers."
                    .to_string(),
            );
            p
        },
    ];
    card.assumptions = vec![
        "Part I is present in every BSM, which is what J2735 requires of BSMcoreData.".to_string(),
    ];
    card.limitations = vec![
        "Part II is not generated: PathHistory (about 300 m, max 23 points) and PathPrediction \
         are 04-models.md §8.1's UNVERIFIED rows and arrive with the hand-written BSM codec."
            .to_string(),
        "The EEBL event flag at a deceleration of 0,4 g is not raised here; on_event carries \
         the HardBraking event that will raise it."
            .to_string(),
        "The inter-transmission-time control of J2945/1 is not implemented here by design \
         (see the type's documentation); DccState::t_off is honoured only as a floor."
            .to_string(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Standard,
            "SAE J2945/1 — BSM generation; 04-models.md §8.1 records which of its numbers are \
             VERIFIED and which are not",
        ),
        Source::new(
            SourceKind::Paper,
            "Rostami 2018; Bindel 2021; NDSS 2024 §II — 10 Hz confirmed by secondary sources",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "generator::tests::the_bsm_generator_runs_at_ten_hertz".to_string(),
            "generator::tests::dcc_can_only_slow_the_bsm_generator_down".to_string(),
        ],
    };
    card.determinism = v2xw_core::card::Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

impl Model for BsmGenerator {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C, V> MessageGenerator<C, V> for BsmGenerator
where
    C: Ctx + ?Sized,
    V: NodeView + ?Sized,
{
    fn check_interval(&self) -> Duration {
        // The check cadence is the nominal period: there is no sub-period trigger to catch.
        self.params.nominal_itt
    }

    fn on_tick(&mut self, ctx: &mut C, node: &V, dcc: &DccState) -> Vec<GenRequest> {
        let now = node.believed_time();
        let Some(reason) = self.check(now, dcc) else {
            return Vec::new();
        };
        ctx.emit(GenerationRecord {
            node: node.node(),
            t_ns: now,
            msg_type: MsgType::Bsm,
            reason,
            // Part I is in every BSM; there is no low-frequency container in a BSM, so the
            // flag is always false and the field means "no extra content" here.
            low_frequency: false,
            next_period_ns: self.params.nominal_itt.as_nanos(),
        });
        vec![GenRequest {
            msg_type: MsgType::Bsm,
            reason,
            include_low_frequency: false,
            at: now,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::time::NS_PER_MS;

    fn ms(n: u64) -> SimTime {
        n * NS_PER_MS
    }

    fn still() -> CamDynamics {
        CamDynamics {
            pos: Vec3::new(100.0, 200.0, 0.0),
            heading_rad: 0.0,
            speed_mps: 10.0,
        }
    }

    fn state() -> CamTriggerState {
        CamTriggerState::new(CamGenParams::en302637_2())
    }

    #[test]
    fn the_first_check_always_sends_and_carries_the_low_frequency_container() {
        let mut s = state();
        let d = s
            .check(0, &still(), &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(d.reason, GenReason::First);
        assert!(
            d.include_low_frequency,
            "EN 302 637-2 §6.1.4: the first CAM"
        );
        assert_eq!(d.t_gen_cam, Duration::from_millis(1_000));
    }

    #[test]
    fn nothing_is_sent_before_t_gen_cam_min() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        // A 90-degree turn at 99 ms is still inside the floor.
        let turned = CamDynamics {
            heading_rad: core::f64::consts::FRAC_PI_2,
            ..still()
        };
        assert!(s.check(ms(99), &turned, &DccState::UNRESTRICTED).is_none());
        // At 100 ms it goes.
        assert!(s.check(ms(100), &turned, &DccState::UNRESTRICTED).is_some());
    }

    #[test]
    fn heading_change_triggers_a_cam() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();

        // 3,9 degrees: under the threshold, and the periodic deadline is 1 s away.
        let under = CamDynamics {
            heading_rad: 3.9_f64.to_radians(),
            ..still()
        };
        assert!(s.check(ms(200), &under, &DccState::UNRESTRICTED).is_none());

        // 4,1 degrees: over it.
        let over = CamDynamics {
            heading_rad: 4.1_f64.to_radians(),
            ..still()
        };
        let d = s
            .check(ms(300), &over, &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(
            d.reason,
            GenReason::Dynamics(DynamicsTriggers {
                heading: true,
                position: false,
                speed: false
            })
        );
        // T_GenCam becomes the interval that was achieved: 300 ms.
        assert_eq!(d.t_gen_cam, Duration::from_millis(300));
    }

    /// The threshold is "> 4 degrees", so exactly 4 degrees must not trigger. This is the
    /// boundary that D10's quantise-before-comparing rule exists to keep stable.
    #[test]
    fn exactly_four_degrees_does_not_trigger() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        let exactly = CamDynamics {
            heading_rad: CamGenParams::FOUR_DEGREES_RAD,
            ..still()
        };
        assert!(
            s.check(ms(200), &exactly, &DccState::UNRESTRICTED)
                .is_none()
        );
    }

    /// The heading difference must wrap: 359 degrees to 1 degree is a 2-degree change, not
    /// a 358-degree one.
    #[test]
    fn heading_difference_wraps_around_the_circle() {
        let mut s = state();
        let near_north = CamDynamics {
            heading_rad: 359.0_f64.to_radians(),
            ..still()
        };
        s.check(0, &near_north, &DccState::UNRESTRICTED).unwrap();
        let just_past = CamDynamics {
            heading_rad: 1.0_f64.to_radians(),
            ..still()
        };
        assert!(
            s.check(ms(200), &just_past, &DccState::UNRESTRICTED)
                .is_none(),
            "a 2-degree change across the wrap must not trigger"
        );
        assert!((angular_difference(0.1, core::f64::consts::TAU + 0.1)).abs() < 1e-12);
    }

    #[test]
    fn position_change_triggers_a_cam() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();

        let near = CamDynamics {
            pos: Vec3::new(103.9, 200.0, 0.0),
            ..still()
        };
        assert!(s.check(ms(200), &near, &DccState::UNRESTRICTED).is_none());

        let far = CamDynamics {
            pos: Vec3::new(104.1, 200.0, 0.0),
            ..still()
        };
        let d = s
            .check(ms(300), &far, &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(
            d.reason,
            GenReason::Dynamics(DynamicsTriggers {
                heading: false,
                position: true,
                speed: false
            })
        );
    }

    #[test]
    fn speed_change_triggers_a_cam() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();

        let slower = CamDynamics {
            speed_mps: 9.6,
            ..still()
        };
        assert!(s.check(ms(200), &slower, &DccState::UNRESTRICTED).is_none());

        let much_slower = CamDynamics {
            speed_mps: 9.4,
            ..still()
        };
        let d = s
            .check(ms(300), &much_slower, &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(
            d.reason,
            GenReason::Dynamics(DynamicsTriggers {
                heading: false,
                position: false,
                speed: true
            })
        );
    }

    #[test]
    fn all_three_triggers_are_reported_when_all_three_fire() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        let everything = CamDynamics {
            pos: Vec3::new(120.0, 220.0, 0.0),
            heading_rad: 0.5,
            speed_mps: 20.0,
        };
        let d = s
            .check(ms(200), &everything, &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(
            d.reason,
            GenReason::Dynamics(DynamicsTriggers {
                heading: true,
                position: true,
                speed: true
            })
        );
    }

    #[test]
    fn the_periodic_trigger_fires_at_t_gen_cam() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        // Nothing changes; T_GenCam is T_GenCamMax = 1 s.
        for t in [100, 200, 500, 999] {
            assert!(
                s.check(ms(t), &still(), &DccState::UNRESTRICTED).is_none(),
                "no CAM at {t} ms"
            );
        }
        let d = s
            .check(ms(1_000), &still(), &DccState::UNRESTRICTED)
            .expect("sends");
        assert_eq!(d.reason, GenReason::Periodic);
    }

    /// EN 302 637-2 §6.1.3: after a dynamics CAM, `T_GenCam` is the achieved interval, and
    /// it returns to `T_GenCamMax` only after `N_GenCam` consecutive *periodic* CAMs.
    #[test]
    fn n_gen_cam_consecutive_periodic_cams_restore_the_maximum() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();

        // A dynamics CAM at 300 ms sets T_GenCam to 300 ms.
        let turned = CamDynamics {
            heading_rad: 0.5,
            ..still()
        };
        let d = s.check(ms(300), &turned, &DccState::UNRESTRICTED).unwrap();
        assert_eq!(d.t_gen_cam, Duration::from_millis(300));
        assert_eq!(s.consecutive_periodic(), 0);

        // Three periodic CAMs at 300 ms spacing. The third restores T_GenCamMax.
        let mut t = 300u64;
        for i in 1..=3 {
            t += 300;
            let d = s
                .check(ms(t), &turned, &DccState::UNRESTRICTED)
                .unwrap_or_else(|| panic!("periodic CAM {i} at {t} ms"));
            assert_eq!(d.reason, GenReason::Periodic, "CAM {i}");
            if i < 3 {
                assert_eq!(d.t_gen_cam, Duration::from_millis(300), "CAM {i}");
                assert_eq!(s.consecutive_periodic(), i as u8);
            } else {
                assert_eq!(
                    d.t_gen_cam,
                    Duration::from_millis(1_000),
                    "the third restores T_GenCamMax"
                );
                assert_eq!(s.consecutive_periodic(), 0, "and resets the counter");
            }
        }

        // And the next periodic CAM is now a second away.
        assert!(
            s.check(ms(t + 900), &turned, &DccState::UNRESTRICTED)
                .is_none()
        );
        assert!(
            s.check(ms(t + 1_000), &turned, &DccState::UNRESTRICTED)
                .is_some()
        );
    }

    /// A dynamics CAM in the middle of a periodic run restarts the count — otherwise
    /// `T_GenCam` would climb back to the maximum while the vehicle is manoeuvring.
    #[test]
    fn a_dynamics_cam_resets_the_periodic_run() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        let turned = CamDynamics {
            heading_rad: 0.5,
            ..still()
        };
        s.check(ms(300), &turned, &DccState::UNRESTRICTED).unwrap();
        s.check(ms(600), &turned, &DccState::UNRESTRICTED).unwrap(); // periodic 1
        assert_eq!(s.consecutive_periodic(), 1);
        let turned_more = CamDynamics {
            heading_rad: 1.5,
            ..still()
        };
        let d = s
            .check(ms(750), &turned_more, &DccState::UNRESTRICTED)
            .unwrap();
        assert!(matches!(d.reason, GenReason::Dynamics(_)));
        assert_eq!(s.consecutive_periodic(), 0, "the run starts over");
        assert_eq!(d.t_gen_cam, Duration::from_millis(150));
    }

    #[test]
    fn the_low_frequency_container_obeys_the_500_ms_rule() {
        let mut s = state();
        assert!(
            s.check(0, &still(), &DccState::UNRESTRICTED)
                .unwrap()
                .include_low_frequency,
            "the first CAM always carries it"
        );

        // Dynamics CAMs every 100 ms; only the ones at least 500 ms after the last carrier
        // may carry it again.
        let mut carried_at = vec![0u64];
        for step in 1..=12u64 {
            let t = step * 100;
            let moved = CamDynamics {
                pos: Vec3::new(100.0 + 5.0 * step as f64, 200.0, 0.0),
                ..still()
            };
            let d = s
                .check(ms(t), &moved, &DccState::UNRESTRICTED)
                .unwrap_or_else(|| panic!("a 5 m step must trigger at {t} ms"));
            if d.include_low_frequency {
                carried_at.push(t);
            }
        }
        assert_eq!(
            carried_at,
            vec![0, 500, 1_000],
            "the low-frequency container goes out at 0, 500 and 1 000 ms"
        );
        for pair in carried_at.windows(2) {
            assert!(pair[1] - pair[0] >= 500);
        }
    }

    #[test]
    fn dcc_raises_the_floor_but_never_lowers_it() {
        let mut s = state();
        s.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        let restrictive = DccState {
            t_off: Duration::from_millis(400),
            cbr: Some(0.62),
        };
        let moved = CamDynamics {
            pos: Vec3::new(200.0, 200.0, 0.0),
            ..still()
        };
        // A 100 m jump would normally trigger at 100 ms; DCC holds it to 400.
        for t in [100, 200, 399] {
            assert!(s.check(ms(t), &moved, &restrictive).is_none(), "at {t} ms");
        }
        assert!(s.check(ms(400), &moved, &restrictive).is_some());

        // A t_off below T_GenCamMin cannot speed the generator up.
        let mut s2 = state();
        s2.check(0, &still(), &DccState::UNRESTRICTED).unwrap();
        let generous = DccState {
            t_off: Duration::from_millis(10),
            cbr: Some(0.01),
        };
        assert!(s2.check(ms(50), &moved, &generous).is_none());
        assert!(s2.check(ms(100), &moved, &generous).is_some());
    }

    /// A clock that steps backwards must not produce a burst of CAMs.
    #[test]
    fn a_backwards_clock_does_not_produce_a_burst() {
        let mut s = state();
        s.check(ms(10_000), &still(), &DccState::UNRESTRICTED)
            .unwrap();
        let moved = CamDynamics {
            pos: Vec3::new(500.0, 500.0, 0.0),
            ..still()
        };
        assert!(
            s.check(ms(9_000), &moved, &DccState::UNRESTRICTED)
                .is_none(),
            "an interval into the past is not an elapsed interval"
        );
    }

    /// The state machine must be a pure function of its inputs: two identical sequences
    /// must produce identical decisions, which is what makes a run reproducible.
    #[test]
    fn the_state_machine_is_deterministic() {
        let run = || {
            let mut s = state();
            let mut out = Vec::new();
            for step in 0..200u64 {
                let t = step * 50;
                let d = CamDynamics {
                    pos: Vec3::new(100.0 + 0.3 * step as f64, 200.0, 0.0),
                    heading_rad: 0.001 * step as f64,
                    speed_mps: 10.0 + 0.004 * step as f64,
                };
                out.push(s.check(ms(t), &d, &DccState::UNRESTRICTED));
            }
            out
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn the_bsm_generator_runs_at_ten_hertz() {
        let mut g = BsmGenerator::default();
        assert_eq!(g.check(0, &DccState::UNRESTRICTED), Some(GenReason::First));
        assert_eq!(g.msg_count(), 1);
        assert_eq!(g.check(ms(99), &DccState::UNRESTRICTED), None);
        assert_eq!(
            g.check(ms(100), &DccState::UNRESTRICTED),
            Some(GenReason::Periodic)
        );
        // Ten messages in the first second, counting the one at t = 0.
        let mut g = BsmGenerator::default();
        let sent = (0..1_000u64)
            .filter(|ms_| g.check(ms(*ms_), &DccState::UNRESTRICTED).is_some())
            .count();
        assert_eq!(sent, 10);
    }

    #[test]
    fn dcc_can_only_slow_the_bsm_generator_down() {
        let mut g = BsmGenerator::default();
        g.check(0, &DccState::UNRESTRICTED).unwrap();
        let restrictive = DccState {
            t_off: Duration::from_millis(300),
            cbr: Some(0.6),
        };
        assert_eq!(g.check(ms(299), &restrictive), None);
        assert_eq!(g.check(ms(300), &restrictive), Some(GenReason::Periodic));

        // And it is clamped at vMaxITT even if DCC asks for more.
        let mut g = BsmGenerator::default();
        g.check(0, &DccState::UNRESTRICTED).unwrap();
        let extreme = DccState {
            t_off: Duration::from_millis(5_000),
            cbr: Some(0.95),
        };
        assert_eq!(g.check(ms(599), &extreme), None);
        assert_eq!(g.check(ms(600), &extreme), Some(GenReason::Periodic));
    }

    #[test]
    fn msg_count_wraps_at_the_j2735_range() {
        let mut g = BsmGenerator::default();
        for _ in 0..128 {
            let t = g.last_tx().map_or(0, |l| l + ms(100));
            g.check(t, &DccState::UNRESTRICTED).unwrap();
        }
        assert_eq!(g.msg_count(), 0, "MsgCount ::= INTEGER (0..127) wraps");
    }

    #[test]
    fn both_cards_validate_and_declare_no_rng() {
        for card in [
            CamGenerator::default().card().clone(),
            BsmGenerator::default().card().clone(),
        ] {
            card.validate()
                .unwrap_or_else(|e| panic!("{}: {e}", card.id));
            assert_eq!(card.family, Family::Generator);
            assert!(
                !card.determinism.uses_rng,
                "{} must not claim an RNG it does not use",
                card.id
            );
        }
    }

    #[test]
    fn the_check_interval_is_never_coarser_than_the_minimum_period() {
        let g = CamGenerator::default();
        let params = *g.state().params();
        assert!(params.t_check_cam_gen <= params.t_gen_cam_min);
    }
}
