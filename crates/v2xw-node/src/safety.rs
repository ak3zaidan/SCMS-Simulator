//! Safety applications over the neighbour table, and the surrogate measures they emit.
//!
//! This is the module that turns a communication result into a safety result, which
//! 00-design-brief.md §1 names as the point of the whole simulator. A node that receives
//! messages runs an application over them; the application decides whether to warn its
//! driver; and each decision carries the surrogate safety measures 04-models.md §11
//! defines, so the chain from "the frame was lost" or "the frame was a lie" to "the
//! warning did not fire" or "the warning fired for nothing" is measurable.
//!
//! # What an application may read
//!
//! Exactly two things: this node's own [`PositionEstimate`] and this node's own
//! [`NeighborTable`]. Both are beliefs. The neighbour table holds the position a peer
//! **claimed** and the instant this node believes it heard the claim, and nothing else —
//! see [`crate::stores::Neighbor`], and [`crate::firewall`] for the sentinel that keeps
//! it that way.
//!
//! That is what makes a ghost vehicle produce a false warning here. An application
//! evaluating its thresholds against the truth would never warn about a vehicle that does
//! not exist, and the false-warning rate — the quantity 07-threats-and-detection.md §5
//! says the engine must *measure* because no published experiment gives it — would come
//! out as identically zero.
//!
//! # The three applications
//!
//! | Model id | What it warns about | 04-models.md §11 status |
//! |---|---|---|
//! | [`Fcw`] (`safety-app/fcw-vsca`) | a rear-end collision with a vehicle ahead in the same direction | threshold UNVERIFIED |
//! | [`Ima`] (`safety-app/ima-vsca`) | a crossing conflict at an intersection | threshold UNVERIFIED |
//! | [`Eebl`] (`safety-app/eebl-vsca`) | a vehicle ahead braking hard | the 0.4 g flag is VERIFIED |
//!
//! Every numeric trigger 04-models.md §11 marks `TODO: calibrate` is carried here as a
//! `todo-calibrate` card parameter with a plan, never as a plausible default in silence.
//! The one number that is cited is the deceleration at which J2945/1 sets the BSM Part II
//! event flag, [`EEBL_DECEL_THRESHOLD_MPS2`].
//!
//! # The surrogate measures, and how they differ from the metric crate's
//!
//! `v2xw-metrics`'s `metric/safety/surrogates-and-flow` computes `ttc_min`, `pet` and
//! `drac` over **ground truth**, which is the right thing for "how dangerous was this
//! run". [`Surrogates`] is the same family of quantities computed over **what the node
//! believed**, which is the right thing for "what did the application have to go on".
//! Both are needed and they are not interchangeable: their difference under attack *is*
//! the attack's effect on safety, and a single ground-truth number cannot express it.
//! The two are therefore kept on different channels — the metric crate's on
//! `metric.sample`, these on `app.warning` — and the field names are the ones vwp-v1
//! §3.6.9 gives.
//!
//! # Quantisation
//!
//! Every float that reaches a [`WarningRecord`] is quantised at construction to
//! [`SURROGATE_Q`] (build decision D9). `NaN` passes through unchanged, because it is the
//! wire's "not applicable" sentinel and vwp-v1 §3.6.9 says so for `ttc_s`.

use std::collections::BTreeMap;

use v2xw_core::belief::PositionEstimate;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::{Record, Visibility};
use v2xw_core::geom::Vec3;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::time::{Duration, SimTime};
use v2xw_msg::sec_types::HashedId8;

use crate::ctx::{NodeCtx, NodeCtxExt};
use crate::server::{OpClass, OpDescriptor};
use crate::stores::{NeighborTable, VerificationState};

/// The quantum every float on a [`WarningRecord`] is rounded to: 1e-3.
///
/// The same grid `v2xw-threat` rounds detector scores to and the same grid ADR 0004
/// decision 7 names the legacy quantum, so a surrogate measure and a detector score from
/// one run serialise to the same number of digits.
pub const SURROGATE_Q: f64 = v2xw_core::math::LEGACY_QUANTUM;

/// The one **verified** surrogate-safety threshold in 04-models.md §11: 1.5 s.
///
/// FHWA-HRT-08-051 (the Surrogate Safety Assessment Model) gives it as the time-to-collision
/// below which an interaction counts as a conflict, "as suggested in previous research".
/// `v2xw-metrics` carries the same constant for the ground-truth metric; it is repeated
/// here rather than imported because `v2xw-node` does not depend on `v2xw-metrics` and
/// must not, and the two are pinned together by
/// `tests/safety_apps.rs::the_conflict_threshold_is_the_one_the_metrics_crate_uses`.
pub const TTC_CONFLICT_THRESHOLD_S: f64 = 1.5;

/// The deceleration at which J2945/1 sets the BSM Part II emergency-brake event flag:
/// 0.4 g = 3.92 m/s².
///
/// 04-models.md §8.1 and §11 both give it, and §11 marks the EEBL row VERIFIED. It is the
/// only numeric trigger in §11's table that is not `TODO: calibrate`.
pub const EEBL_DECEL_THRESHOLD_MPS2: f64 = 3.92;

/// Model id of the forward-collision-warning application.
pub const FCW_ID: &str = "safety-app/fcw-vsca";
/// Model id of the intersection-movement-assist application.
pub const IMA_ID: &str = "safety-app/ima-vsca";
/// Model id of the emergency-electronic-brake-light application.
pub const EEBL_ID: &str = "safety-app/eebl-vsca";

/// Rounds a surrogate measure to [`SURROGATE_Q`], leaving `NaN` alone.
#[must_use]
pub fn q(x: f64) -> f64 {
    math::quantize_to(x, SURROGATE_Q)
}

/// The eight bytes a [`HashedId8`] is, for a map key.
///
/// The applications key their per-subject state by this rather than by [`HashedId8`],
/// for the same reason [`NeighborTable`] does: the generated ASN.1 type is not `Ord`, and
/// an ordering is needed so that two runs that heard the same peers in a different order
/// produce the same warnings in the same order.
#[must_use]
pub fn digest_key(d: &HashedId8) -> [u8; 8] {
    let mut k = [0u8; 8];
    k.copy_from_slice(&d.0[..]);
    k
}

/// A [`HashedId8`] as lowercase hex — the subject id a record names.
///
/// The same spelling `v2xw-threat`'s `det.observation` uses, so a warning and a detector
/// observation about one pseudonym join on equality of this string.
#[must_use]
pub fn digest_hex(d: &HashedId8) -> String {
    let mut s = String::with_capacity(16);
    for b in digest_key(d) {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

// =========================================================================================
// Surrogate measures
// =========================================================================================

/// The surrogate safety measures one interaction produced, as the node believed it.
///
/// `NaN` means "not applicable to this interaction", which is vwp-v1 §3.6.9's own
/// convention for `ttc_s`. A rear-end interaction has no post-encroachment time and a
/// crossing interaction has no headway, so a struct where every field always carried a
/// number would be carrying invented ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Surrogates {
    /// Time to collision, seconds: the gap divided by the closing speed, or for a
    /// crossing conflict the earlier of the two arrival times at the conflict point.
    pub ttc_s: f64,
    /// Post-encroachment time, seconds: the interval between one road user leaving the
    /// conflict point and the other arriving at it. Zero means they are there together.
    pub pet_s: f64,
    /// The deceleration the ego would need to avoid the collision, m/s²:
    /// `closing² / (2·gap)`.
    ///
    /// **Not** FHWA-HRT-08-051's DRAC, which is the deceleration the second vehicle was
    /// *observed* to apply. This is the required rate, computed from the node's belief
    /// before any braking happens, which is what an application can actually know. The
    /// distinction is on every card's `limitations`.
    pub required_decel_mps2: f64,
    /// The distance between the two reference points, metres, as the node believed it.
    pub distance_m: f64,
    /// The rate at which the gap is closing, m/s. Negative means opening.
    pub closing_mps: f64,
}

impl Surrogates {
    /// Every measure absent.
    pub const NONE: Surrogates = Surrogates {
        ttc_s: f64::NAN,
        pet_s: f64::NAN,
        required_decel_mps2: f64::NAN,
        distance_m: f64::NAN,
        closing_mps: f64::NAN,
    };

    /// The same measures with every float on the [`SURROGATE_Q`] grid.
    #[must_use]
    pub fn quantised(self) -> Surrogates {
        Surrogates {
            ttc_s: q(self.ttc_s),
            pet_s: q(self.pet_s),
            required_decel_mps2: q(self.required_decel_mps2),
            distance_m: q(self.distance_m),
            closing_mps: q(self.closing_mps),
        }
    }

    /// Whether the time to collision is below the SSAM conflict threshold.
    #[must_use]
    pub fn is_conflict(&self) -> bool {
        self.ttc_s.is_finite() && self.ttc_s >= 0.0 && self.ttc_s <= TTC_CONFLICT_THRESHOLD_S
    }
}

/// A rear-end interaction resolved in the ego's own heading frame.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Longitudinal {
    /// Distance ahead along the ego's heading, metres. Negative is behind.
    ahead_m: f64,
    /// Offset across the ego's heading, metres.
    lateral_m: f64,
    /// The peer's speed projected onto the ego's heading, m/s.
    peer_along_mps: f64,
    /// The signed heading difference, radians.
    heading_delta_rad: f64,
}

/// Resolves a peer's claimed state into the ego's heading frame.
///
/// Pure arithmetic apart from the two trigonometric calls, which go through
/// [`v2xw_core::math`] so the result is identical on every platform (ADR 0003).
fn resolve(ego: &PositionEstimate, peer_pos: Vec3, peer_speed: f64, peer_heading: f64) -> Longitudinal {
    let (sin_h, cos_h) = math::sin_cos(ego.heading_rad);
    let rel_x = peer_pos.x - ego.pos.x;
    let rel_y = peer_pos.y - ego.pos.y;
    let delta = v2xw_msg::generator::angular_difference(peer_heading, ego.heading_rad);
    Longitudinal {
        ahead_m: rel_x * cos_h + rel_y * sin_h,
        lateral_m: -rel_x * sin_h + rel_y * cos_h,
        peer_along_mps: peer_speed * math::cos(delta),
        heading_delta_rad: delta,
    }
}

/// The time-to-collision of a closing pair, or `NaN` when the pair is not closing.
///
/// `ttc = gap / closing`, the SSAM definition restricted to the one-dimensional case
/// [FHWA-HRT-08-051]. A non-positive closing speed has no time to collision — the pair is
/// separating — and the answer is the absent sentinel rather than a negative number a
/// threshold comparison would treat as imminent.
#[must_use]
pub fn time_to_collision_s(gap_m: f64, closing_mps: f64) -> f64 {
    if !gap_m.is_finite() || !closing_mps.is_finite() || closing_mps <= 0.0 || gap_m < 0.0 {
        return f64::NAN;
    }
    gap_m / closing_mps
}

/// The deceleration needed to stop short of the vehicle ahead: `closing² / (2·gap)`.
///
/// `NaN` for a pair that is not closing, and `f64::INFINITY` is never returned: a zero gap
/// means the collision has already happened and the required rate is unbounded, which is
/// reported as the absent sentinel rather than as an infinity that would quantise to one.
#[must_use]
pub fn required_deceleration_mps2(gap_m: f64, closing_mps: f64) -> f64 {
    if !gap_m.is_finite() || !closing_mps.is_finite() || closing_mps <= 0.0 || gap_m <= 0.0 {
        return f64::NAN;
    }
    (closing_mps * closing_mps) / (2.0 * gap_m)
}

/// Where and when two constant-velocity paths cross, in the ego's own frame of reference.
///
/// Returns `(t_ego, t_peer)`, the times at which each reaches the crossing point, or
/// `None` when the paths are parallel, when either is stationary, or when the crossing is
/// behind one of them. Solved by Cramer's rule on the 2×2 system `p_e + t·v_e = p_o + s·v_o`,
/// so no transcendental is involved and the result is exact to the arithmetic.
#[must_use]
pub fn crossing_times_s(
    ego_pos: Vec3,
    ego_vel: Vec3,
    peer_pos: Vec3,
    peer_vel: Vec3,
) -> Option<(f64, f64)> {
    let det = ego_vel.x * peer_vel.y - ego_vel.y * peer_vel.x;
    // A determinant this small is a pair of paths that are parallel to within the
    // quantisation of a reported heading; calling them crossing would put the conflict
    // point at an arbitrary distance.
    if !det.is_finite() || det.abs() < 1e-9 {
        return None;
    }
    let wx = peer_pos.x - ego_pos.x;
    let wy = peer_pos.y - ego_pos.y;
    let t_ego = (wx * peer_vel.y - wy * peer_vel.x) / det;
    let t_peer = (wx * ego_vel.y - wy * ego_vel.x) / det;
    if !t_ego.is_finite() || !t_peer.is_finite() || t_ego < 0.0 || t_peer < 0.0 {
        return None;
    }
    Some((t_ego, t_peer))
}

/// The velocity a peer's claim implies, from its claimed speed and heading.
fn claimed_velocity(speed_mps: f64, heading_rad: f64) -> Vec3 {
    let (sin_h, cos_h) = math::sin_cos(heading_rad);
    Vec3::new(speed_mps * cos_h, speed_mps * sin_h, 0.0)
}

// =========================================================================================
// Warnings
// =========================================================================================

/// Whether a warning is new, continuing or over — vwp-v1 §3.6.9's `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WarningKind {
    /// First time this subject triggered this application.
    Issue,
    /// The warning is still standing, with new numbers.
    Update,
    /// The condition has gone away.
    Clear,
}

impl WarningKind {
    /// The code vwp-v1 §3.6.9 gives: `0` issue, `1` update, `2` clear.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            WarningKind::Issue => 0,
            WarningKind::Update => 1,
            WarningKind::Clear => 2,
        }
    }

    /// The stable name a record carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningKind::Issue => "issue",
            WarningKind::Update => "update",
            WarningKind::Clear => "clear",
        }
    }
}

/// How urgent a warning is — vwp-v1 §3.6.9's `severity`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Severity {
    /// Informational only.
    Info,
    /// Advisory: the condition exists but there is time.
    Caution,
    /// A warning the driver is expected to act on.
    Warning,
    /// Collision imminent.
    Imminent,
}

impl Severity {
    /// The code vwp-v1 §3.6.9 gives: `0` info, `1` caution, `2` warning, `3` imminent.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Severity::Info => 0,
            Severity::Caution => 1,
            Severity::Warning => 2,
            Severity::Imminent => 3,
        }
    }

    /// The stable name a record carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Severity::Info => "info",
            Severity::Caution => "caution",
            Severity::Warning => "warning",
            Severity::Imminent => "imminent",
        }
    }
}

/// One application's decision about one subject.
///
/// There is no true position and no actor id on it, because the application had neither:
/// [`Warning::subject`] is the pseudonym digest the message was signed under, which is the
/// only identity a receiver has.
#[derive(Debug, Clone, PartialEq)]
pub struct Warning {
    /// Which application, by the short name vwp-v1 §3.6.9 puts in `str_app`.
    pub app: &'static str,
    /// The pseudonym the warning is about.
    pub subject: HashedId8,
    /// Whether it is new, continuing or over.
    pub kind: WarningKind,
    /// How urgent.
    pub severity: Severity,
    /// The surrogate measures behind it, quantised.
    pub surrogates: Surrogates,
    /// The node's own believed instant of the decision.
    pub at: SimTime,
}

impl Warning {
    /// Whether this warning asserts a condition (as against clearing one).
    #[must_use]
    pub const fn fired(&self) -> bool {
        !matches!(self.kind, WarningKind::Clear)
    }
}

/// `app.warning` — one safety application's outcome at one node (NODE).
///
/// The field names are vwp-v1 §3.6.9's, plus `pet_s`, `required_decel_mps2` and
/// `closing_mps`, which §3.6.9's 32-byte layout has no room for and which the dataset
/// views read by name. The two fields §3.6.9 marks **GT** — `truth` and `subject_actor_id`
/// — are deliberately **absent**: a node cannot compute either, so writing them here would
/// be the leak `v2xw-record`'s NODE profile exists to strip. The offline join against
/// ground truth that labels a warning true, false or missed is
/// 07-threats-and-detection.md §5's, and it runs over the recording.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct WarningRecord {
    /// The node's own believed instant.
    pub t: SimTime,
    /// The warning node.
    pub node: NodeId,
    /// The application's short name: `fcw`, `ima`, `eebl`.
    pub app: &'static str,
    /// The subject's pseudonym digest, lowercase hex.
    pub subject: String,
    /// Whether the warning asserts a condition. `false` on a clear.
    pub fired: bool,
    /// `issue`, `update` or `clear`.
    pub kind: &'static str,
    /// `info`, `caution`, `warning` or `imminent`.
    pub severity: &'static str,
    /// Time to collision, seconds; `NaN` if not applicable.
    pub ttc_s: f64,
    /// Post-encroachment time, seconds; `NaN` if not applicable.
    pub pet_s: f64,
    /// The deceleration the ego would need, m/s²; `NaN` if not applicable.
    pub required_decel_mps2: f64,
    /// The believed distance to the subject, metres.
    pub distance_m: f64,
    /// The believed closing rate, m/s.
    pub closing_mps: f64,
}

impl Record for WarningRecord {
    const CHANNEL: &'static str = "app.warning";
    const VISIBILITY: Visibility = Visibility::Node;
}

impl WarningRecord {
    /// The record for one warning, with every float already on the [`SURROGATE_Q`] grid.
    #[must_use]
    pub fn of(node: NodeId, w: &Warning) -> WarningRecord {
        let s = w.surrogates.quantised();
        WarningRecord {
            t: w.at,
            node,
            app: w.app,
            subject: digest_hex(&w.subject),
            fired: w.fired(),
            kind: w.kind.as_str(),
            severity: w.severity.as_str(),
            ttc_s: s.ttc_s,
            pet_s: s.pet_s,
            required_decel_mps2: s.required_decel_mps2,
            distance_m: s.distance_m,
            closing_mps: s.closing_mps,
        }
    }
}

// =========================================================================================
// The family trait
// =========================================================================================

/// An application that runs over the neighbour table (03-interfaces.md §8).
///
/// Narrowed to [`NodeCtx`] rather than `Ctx` per build decision D12.2, exactly as
/// [`crate::server::ServiceModel`] is: a crate below the engine cannot name the engine's
/// event payload, and an application has no business scheduling anything anyway.
///
/// The signature is the published one with one addition: `believed` is passed explicitly
/// rather than read from the context, because [`NodeCtx::now`] is the *simulator's* clock
/// and every interval an application measures must be on the node's own. An application
/// that read `ctx.now()` would be immune to a clock attack, which is the opposite of what
/// the simulator is for.
pub trait SafetyApp: Model {
    /// The warnings this application raises, in subject-digest order.
    fn on_neighbors(
        &mut self,
        ctx: &mut dyn NodeCtx,
        node: NodeId,
        believed: SimTime,
        neighbors: &NeighborTable,
        ego: &PositionEstimate,
    ) -> Vec<Warning>;

    /// The work one pass costs, charged to the node's CPU by the runtime.
    ///
    /// Defaulted to an application-class task so that a new application is charged rather
    /// than free; the profile's own `app_task` cost decides what that comes to, and none
    /// of the ten shipped profiles publishes one, so it is zero until a scenario supplies
    /// it (see [`crate::server::ProfileServiceModel::with_app_task_cost`]).
    fn cost(&self) -> OpDescriptor {
        OpDescriptor::task("safety-app", OpClass::Application)
    }

    /// The short name vwp-v1 §3.6.9 puts in `str_app`.
    fn app_name(&self) -> &'static str;
}

/// Whether a neighbour's claim is one an application should act on.
///
/// [`VerificationState::Invalid`] never reaches the neighbour table, and a
/// [`VerificationState::Revoked`] peer is one the node has decided not to believe, so
/// what remains is `Verified` and `Unverified`. Both are acted on, and which one it was is
/// recoverable from the `node.verify` record for the same message: a study of what
/// `on-demand` verification costs in safety terms needs the warnings an unverified message
/// caused, not their absence.
fn actionable(state: VerificationState) -> bool {
    matches!(
        state,
        VerificationState::Verified | VerificationState::Unverified
    )
}

/// Per-subject warning state, so that `issue`, `update` and `clear` mean what they say.
///
/// A [`BTreeMap`] keyed by the digest bytes: ordered iteration, and a `clear` is emitted in
/// the same order on every run.
#[derive(Debug, Clone, Default)]
struct Standing {
    /// Subjects currently warned about, with the instant the warning was last refreshed.
    active: BTreeMap<[u8; 8], (HashedId8, SimTime)>,
}

impl Standing {
    /// Records that `subject` is (still) warned about, returning the right
    /// [`WarningKind`].
    fn assert(&mut self, subject: &HashedId8, at: SimTime) -> WarningKind {
        let key = digest_key(subject);
        let kind = if self.active.contains_key(&key) {
            WarningKind::Update
        } else {
            WarningKind::Issue
        };
        self.active.insert(key, (subject.clone(), at));
        kind
    }

    /// Every subject that was warned about and is no longer in `still`, in digest order.
    fn retire(&mut self, still: &BTreeMap<[u8; 8], ()>) -> Vec<HashedId8> {
        let gone: Vec<[u8; 8]> = self
            .active
            .keys()
            .filter(|k| !still.contains_key(*k))
            .copied()
            .collect();
        let mut out = Vec::with_capacity(gone.len());
        for k in gone {
            if let Some((digest, _)) = self.active.remove(&k) {
                out.push(digest);
            }
        }
        out
    }
}

// =========================================================================================
// `safety-app/fcw-vsca`
// =========================================================================================

/// The parameters of the forward-collision warning.
///
/// 04-models.md §11 gives the application's definition from the CAMP VSC-A final report
/// [DOT HS 811 492A] and marks **every number** in its row `TODO: calibrate`: "time-to-
/// collision threshold `TODO: calibrate` (plan: SAE J2945/1 or the VSC-A companion volume;
/// not in the accessible report)". So every field here is a `todo-calibrate` parameter
/// with that plan, and the starting values are stated for what they are.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FcwParams {
    /// The time to collision at which the application warns, seconds.
    ///
    /// The starting value is [`TTC_CONFLICT_THRESHOLD_S`] — the SSAM *conflict* threshold,
    /// which is a different quantity from a warning threshold and is used here only
    /// because it is the one surrogate-safety number 04-models.md §11 records as VERIFIED.
    /// The card says so.
    pub ttc_warn_s: f64,
    /// The time to collision below which the warning is [`Severity::Imminent`], seconds.
    pub ttc_imminent_s: f64,
    /// Half-width of the corridor a peer must be inside to count as "in the same lane",
    /// metres.
    pub corridor_half_width_m: f64,
    /// The largest heading difference at which a peer still counts as travelling in the
    /// same direction, radians.
    pub heading_tolerance_rad: f64,
    /// The furthest ahead a peer is considered at all, metres. Beyond it the interaction
    /// is not a rear-end candidate and the pair is not evaluated.
    pub range_m: f64,
}

impl FcwParams {
    /// The starting values, every one of them uncalibrated. See the type documentation.
    #[must_use]
    pub fn vsca() -> FcwParams {
        FcwParams {
            ttc_warn_s: TTC_CONFLICT_THRESHOLD_S,
            ttc_imminent_s: 0.5 * TTC_CONFLICT_THRESHOLD_S,
            // One passenger-car width, `v2xw_core::geom::Dims::CAR.width_m`. A lane is
            // wider than that; a corridor as wide as a lane would include a vehicle
            // straddling the next one.
            corridor_half_width_m: 0.5 * v2xw_core::geom::Dims::CAR.width_m,
            heading_tolerance_rad: 45.0 * (core::f64::consts::PI / 180.0),
            // The VSC-A reference forward-looking radar reaches 150 m for a 10 m² target
            // [04-models.md §11, VSC-A Table 3]. A V2V application is not range-limited
            // the same way, but a 150 m horizon is the one distance in §11's FCW material
            // that is published, and it is used as the evaluation horizon rather than as a
            // sensor range.
            range_m: 150.0,
        }
    }
}

impl Default for FcwParams {
    fn default() -> FcwParams {
        FcwParams::vsca()
    }
}

/// `safety-app/fcw-vsca` — the forward-collision warning.
///
/// Warns of "an impending rear-end collision with a remote vehicle ahead in the same lane
/// and direction" (04-models.md §11). "Same lane" is approximated by a corridor in the
/// ego's own heading frame, because a node knows its own lane but not a peer's: a peer
/// reports a position, not a lane id, and inferring one would need the map that
/// 06-node-models.md §3 gives an RSU and not a vehicle. The approximation is on the card.
#[derive(Debug, Clone)]
pub struct Fcw {
    card: ModelCard,
    params: FcwParams,
    standing: Standing,
}

impl Default for Fcw {
    fn default() -> Fcw {
        Fcw::new(FcwParams::vsca())
    }
}

impl Fcw {
    /// The application with `params`.
    #[must_use]
    pub fn new(params: FcwParams) -> Fcw {
        Fcw {
            card: fcw_card(&params),
            params,
            standing: Standing::default(),
        }
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> &FcwParams {
        &self.params
    }

    /// The surrogate measures for one peer, or `None` when the peer is not a rear-end
    /// candidate at all.
    ///
    /// Public because this is the predicate a study of "what did the application see"
    /// wants, and because reimplementing it is how two answers to one question appear.
    #[must_use]
    pub fn evaluate(
        &self,
        ego: &PositionEstimate,
        peer_pos: Vec3,
        peer_speed_mps: f64,
        peer_heading_rad: f64,
    ) -> Option<Surrogates> {
        rear_end_surrogates(&self.params, ego, peer_pos, peer_speed_mps, peer_heading_rad)
    }
}

/// The rear-end geometry and its surrogate measures, as a free function.
///
/// [`Fcw::evaluate`] is this, and [`Eebl`] calls it too: 04-models.md §11 has the remote
/// vehicles "judge relevance" for a brake event, and the judgement is exactly the FCW's
/// corridor. A free function rather than an `Fcw` held inside the `Eebl` because building
/// an `Fcw` builds a [`ModelCard`], and a card allocated once per application pass per
/// neighbour would be the dominant cost of the whole safety layer.
#[must_use]
pub fn rear_end_surrogates(
    params: &FcwParams,
    ego: &PositionEstimate,
    peer_pos: Vec3,
    peer_speed_mps: f64,
    peer_heading_rad: f64,
) -> Option<Surrogates> {
    let r = resolve(ego, peer_pos, peer_speed_mps, peer_heading_rad);
    if r.ahead_m <= 0.0 || r.ahead_m > params.range_m {
        return None;
    }
    if r.lateral_m.abs() > params.corridor_half_width_m {
        return None;
    }
    if r.heading_delta_rad.abs() > params.heading_tolerance_rad {
        return None;
    }
    let closing = ego.ground_speed_mps() - r.peer_along_mps;
    Some(Surrogates {
        ttc_s: time_to_collision_s(r.ahead_m, closing),
        // A rear-end interaction has no post-encroachment time: the two never occupy the
        // conflict point at different times, they converge on the same line.
        pet_s: f64::NAN,
        required_decel_mps2: required_deceleration_mps2(r.ahead_m, closing),
        distance_m: ego.pos.distance_2d(peer_pos),
        closing_mps: closing,
    })
}

impl Model for Fcw {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl SafetyApp for Fcw {
    fn on_neighbors(
        &mut self,
        _ctx: &mut dyn NodeCtx,
        _node: NodeId,
        believed: SimTime,
        neighbors: &NeighborTable,
        ego: &PositionEstimate,
    ) -> Vec<Warning> {
        let mut out = Vec::new();
        let mut still: BTreeMap<[u8; 8], ()> = BTreeMap::new();
        // `NeighborTable::iter` is in digest order, so the warnings come out in digest
        // order and two runs that heard the same peers agree.
        for n in neighbors.iter() {
            if !actionable(n.state) {
                continue;
            }
            let Some(s) = self.evaluate(
                ego,
                n.claimed_pos,
                n.claimed_speed_mps,
                n.claimed_heading_rad,
            ) else {
                continue;
            };
            if !(s.ttc_s.is_finite() && s.ttc_s <= self.params.ttc_warn_s) {
                continue;
            }
            still.insert(digest_key(&n.signer), ());
            let kind = self.standing.assert(&n.signer, believed);
            out.push(Warning {
                app: self.app_name(),
                subject: n.signer.clone(),
                kind,
                severity: if s.ttc_s <= self.params.ttc_imminent_s {
                    Severity::Imminent
                } else {
                    Severity::Warning
                },
                surrogates: s.quantised(),
                at: believed,
            });
        }
        for subject in self.standing.retire(&still) {
            out.push(Warning {
                app: self.app_name(),
                subject,
                kind: WarningKind::Clear,
                severity: Severity::Info,
                surrogates: Surrogates::NONE,
                at: believed,
            });
        }
        out
    }

    fn cost(&self) -> OpDescriptor {
        OpDescriptor::task(FCW_ID, OpClass::Application)
    }

    fn app_name(&self) -> &'static str {
        "fcw"
    }
}

fn todo(name: &str, unit: &str, default: serde_json::Value, why: &str, plan: &str) -> Parameter {
    let mut p = Parameter::new(name, unit, default, Source::todo_calibrate(why));
    p.calibration = Some(plan.to_string());
    p
}

/// The plan 04-models.md §11 gives for every uncalibrated VSC-A trigger.
const VSCA_PLAN: &str = "04-models.md §11 records this trigger as TODO: calibrate with the \
     plan 'SAE J2945/1 or the VSC-A companion volume; not in the accessible report'. Read \
     the companion volume to DOT HS 811 492A, or J2945/1 §6, and replace the value; until \
     then a study that depends on it must vary it and report the sensitivity.";

fn fcw_card(p: &FcwParams) -> ModelCard {
    let mut card = ModelCard::new(
        FCW_ID,
        Family::SafetyApp,
        "0.1.0",
        "Forward-collision warning over the neighbour table: warns of an impending \
         rear-end collision with a peer ahead in the same direction, and emits the \
         belief-side time to collision and required deceleration.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "time to collision",
            "ttc = gap / (v_ego − v_peer·cos(Δheading)), evaluated only while closing",
        ),
        Equation::new(
            "required deceleration",
            "a_req = closing² / (2·gap) — the rate the ego would need, not the rate the \
             peer was observed to apply",
        ),
        Equation::new(
            "same-direction corridor",
            "|(p_peer − p_ego)·l̂| ≤ corridor_half_width_m and |Δheading| ≤ \
             heading_tolerance_rad, with l̂ the ego's lateral unit vector",
        ),
    ];
    card.parameters = vec![
        todo(
            "ttc_warn_s",
            "s",
            serde_json::json!(p.ttc_warn_s),
            "04-models.md §11 marks the FCW time-to-collision threshold UNVERIFIED",
            "The starting value is FHWA-HRT-08-051's 1.5 s SSAM *conflict* threshold, \
             which is a different quantity from a warning threshold and is used only \
             because it is the one surrogate-safety number 04-models.md §11 records as \
             VERIFIED. Replace it from SAE J2945/1 or the VSC-A companion volume.",
        ),
        todo(
            "ttc_imminent_s",
            "s",
            serde_json::json!(p.ttc_imminent_s),
            "no source gives the boundary between a warning and an imminent-collision \
             alert",
            "Half of ttc_warn_s, chosen so the severity split is a stated fraction rather \
             than a second invented threshold. The VSC-A companion volume defines the \
             alert levels; read it and replace both numbers together.",
        ),
        todo(
            "corridor_half_width_m",
            "m",
            serde_json::json!(p.corridor_half_width_m),
            "04-models.md §11 marks the VSC-A zone geometry TODO: calibrate",
            "Half a passenger car's width (v2xw_core::geom::Dims::CAR), so a peer must be \
             within one vehicle width of the ego's own path. The published alternative is \
             a lane-based test, which needs the map a vehicle does not have. Calibrate \
             against the VSC-A companion volume's FCW target-classification zone, or \
             against a lane-resolved run with the world's lane widths.",
        ),
        todo(
            "heading_tolerance_rad",
            "rad",
            serde_json::json!(p.heading_tolerance_rad),
            "no source gives the heading agreement that makes a peer 'same direction'",
            "45° admits a peer on a curve and excludes oncoming and crossing traffic. \
             Calibrate by sweeping it on a curved-road scenario and taking the value at \
             which same-direction recall stops improving.",
        ),
        todo(
            "range_m",
            "m",
            serde_json::json!(p.range_m),
            "the evaluation horizon is an implementation choice, not a published one",
            "150 m is the VSC-A reference radar's range for a 10 m² target [04-models.md \
             §11, VSC-A Table 3] and is used here as an evaluation horizon, not a sensor \
             range. Raise it to the communication range and confirm no warning is lost.",
        ),
    ];
    card.assumptions = vec![
        "A peer's lane is unknown to a receiver, so 'same lane' is a corridor in the ego's \
         own heading frame."
            .into(),
        "Closing speed uses the peer's claimed speed projected onto the ego's heading; the \
         peer's own acceleration is not in a CAM's or a BSM's mandatory content at this \
         tier."
            .into(),
    ];
    card.limitations = vec![
        "Gaps are between reported reference points, so vehicle length is not accounted \
         for and every time to collision is correspondingly optimistic — the same \
         limitation v2xw-metrics records for the ground-truth ttc_min."
            .into(),
        "`required_decel_mps2` is the deceleration the ego would need, not \
         FHWA-HRT-08-051's DRAC, which is the deceleration the second vehicle was observed \
         to apply. The two are not interchangeable and the field name says which this is."
            .into(),
        "Every numeric trigger is uncalibrated: 04-models.md §11's FCW row has no verified \
         number in it."
            .into(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Paper,
            "CAMP VSC-A final report, DOT HS 811 492A (application definition, 04-models.md \
             §11)",
        ),
        Source::new(
            SourceKind::Paper,
            "FHWA-HRT-08-051, Surrogate Safety Assessment Model (time to collision, 1.5 s \
             conflict threshold)",
        ),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.validation.tests = vec![
        "safety_apps::a_closing_vehicle_ahead_warns_at_the_computed_ttc".to_string(),
        "safety_apps::an_opening_pair_never_warns".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}

// =========================================================================================
// `safety-app/ima-vsca`
// =========================================================================================

/// The parameters of intersection movement assist.
///
/// 04-models.md §11: "warns when entering an intersection is unsafe; initial scope
/// stop-sign-controlled and uncontrolled intersections", with the time-to-intersection
/// threshold `TODO: calibrate` and marked UNVERIFIED.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImaParams {
    /// The ego's time to the conflict point at which the application warns, seconds.
    pub tti_warn_s: f64,
    /// The largest post-encroachment time the application will **warn** about, seconds.
    ///
    /// Distinct from [`ImaParams::pet_conflict_s`], and the distinction matters: SSAM's 5 s
    /// is the threshold below which an interaction is *counted* as a conflict over a whole
    /// run, and a vehicle that clears the junction three seconds before the ego arrives is
    /// inside it while being no reason to warn a driver. So the measure is reported at the
    /// SSAM threshold and the warning fires at this one.
    pub pet_warn_s: f64,
    /// The largest post-encroachment time for which a time to collision is reported at
    /// all, seconds — SSAM's own conflict threshold.
    pub pet_conflict_s: f64,
    /// The smallest speed at which a road user is treated as moving, m/s. Below it the
    /// crossing geometry is undefined — a stationary peer has no path.
    pub moving_threshold_mps: f64,
    /// The furthest the conflict point may be from the ego, metres.
    pub range_m: f64,
}

impl ImaParams {
    /// The starting values, every one of them uncalibrated. See the type documentation.
    #[must_use]
    pub fn vsca() -> ImaParams {
        ImaParams {
            tti_warn_s: 4.0,
            pet_warn_s: TTC_CONFLICT_THRESHOLD_S,
            pet_conflict_s: 5.0,
            moving_threshold_mps: 0.5,
            range_m: 150.0,
        }
    }
}

impl Default for ImaParams {
    fn default() -> ImaParams {
        ImaParams::vsca()
    }
}

/// `safety-app/ima-vsca` — intersection movement assist.
///
/// A crossing conflict, not a rear-end one: the two paths are extrapolated at constant
/// velocity, and the application asks when each road user reaches the point where the
/// paths cross. The difference between the two arrival times is the post-encroachment
/// time, which is 04-models.md §11's own definition of PET — "time between the first
/// vehicle leaving a position and the second arriving at it" — evaluated on the node's
/// belief rather than on the truth.
///
/// This is also the reason the metrics crate's cell-based PET and this one are both worth
/// having: the metric answers "did two vehicles share a position", this answers "did the
/// node have reason to think they would".
#[derive(Debug, Clone)]
pub struct Ima {
    card: ModelCard,
    params: ImaParams,
    standing: Standing,
}

impl Default for Ima {
    fn default() -> Ima {
        Ima::new(ImaParams::vsca())
    }
}

impl Ima {
    /// The application with `params`.
    #[must_use]
    pub fn new(params: ImaParams) -> Ima {
        Ima {
            card: ima_card(&params),
            params,
            standing: Standing::default(),
        }
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> &ImaParams {
        &self.params
    }

    /// The surrogate measures for one crossing candidate, or `None` when the pair has no
    /// crossing conflict.
    #[must_use]
    pub fn evaluate(
        &self,
        ego: &PositionEstimate,
        peer_pos: Vec3,
        peer_speed_mps: f64,
        peer_heading_rad: f64,
    ) -> Option<Surrogates> {
        let ego_speed = ego.ground_speed_mps();
        if ego_speed < self.params.moving_threshold_mps
            || peer_speed_mps < self.params.moving_threshold_mps
        {
            return None;
        }
        let ego_vel = claimed_velocity(ego_speed, ego.heading_rad);
        let peer_vel = claimed_velocity(peer_speed_mps, peer_heading_rad);
        let (t_ego, t_peer) = crossing_times_s(ego.pos, ego_vel, peer_pos, peer_vel)?;
        // `t_ego` is the ego's own time to the conflict point, so the distance to it is
        // that time times its speed — no square root and no second geometry.
        if t_ego * ego_speed > self.params.range_m {
            return None;
        }
        let pet = (t_ego - t_peer).abs();
        Some(Surrogates {
            // The pair collides only if both are at the point together; when they are not,
            // there is no time to collision and the absent sentinel says so while `pet_s`
            // still reports how close they came.
            ttc_s: if pet <= self.params.pet_conflict_s {
                t_ego.min(t_peer)
            } else {
                f64::NAN
            },
            pet_s: pet,
            // A crossing conflict is not avoided by decelerating along the ego's own path
            // in the way a rear-end one is: stopping short of the conflict point is a
            // different manoeuvre from matching a leader's speed. No required rate is
            // claimed.
            required_decel_mps2: f64::NAN,
            distance_m: ego.pos.distance_2d(peer_pos),
            closing_mps: f64::NAN,
        })
    }
}

impl Model for Ima {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl SafetyApp for Ima {
    fn on_neighbors(
        &mut self,
        _ctx: &mut dyn NodeCtx,
        _node: NodeId,
        believed: SimTime,
        neighbors: &NeighborTable,
        ego: &PositionEstimate,
    ) -> Vec<Warning> {
        let mut out = Vec::new();
        let mut still: BTreeMap<[u8; 8], ()> = BTreeMap::new();
        for n in neighbors.iter() {
            if !actionable(n.state) {
                continue;
            }
            let Some(s) = self.evaluate(
                ego,
                n.claimed_pos,
                n.claimed_speed_mps,
                n.claimed_heading_rad,
            ) else {
                continue;
            };
            // Three conditions, and the second is the one that keeps this application
            // from warning about every vehicle that will ever cross the ego's path: the
            // two must be at the conflict point close together in time, not merely inside
            // SSAM's reporting window.
            if !(s.ttc_s.is_finite()
                && s.pet_s <= self.params.pet_warn_s
                && s.ttc_s <= self.params.tti_warn_s)
            {
                continue;
            }
            still.insert(digest_key(&n.signer), ());
            let kind = self.standing.assert(&n.signer, believed);
            out.push(Warning {
                app: self.app_name(),
                subject: n.signer.clone(),
                kind,
                // Imminent when the ego itself reaches the conflict point inside the SSAM
                // conflict threshold. A cited boundary reused rather than a fourth
                // threshold invented; the card says so.
                severity: if s.ttc_s <= TTC_CONFLICT_THRESHOLD_S {
                    Severity::Imminent
                } else {
                    Severity::Warning
                },
                surrogates: s.quantised(),
                at: believed,
            });
        }
        for subject in self.standing.retire(&still) {
            out.push(Warning {
                app: self.app_name(),
                subject,
                kind: WarningKind::Clear,
                severity: Severity::Info,
                surrogates: Surrogates::NONE,
                at: believed,
            });
        }
        out
    }

    fn cost(&self) -> OpDescriptor {
        OpDescriptor::task(IMA_ID, OpClass::Application)
    }

    fn app_name(&self) -> &'static str {
        "ima"
    }
}

fn ima_card(p: &ImaParams) -> ModelCard {
    let mut card = ModelCard::new(
        IMA_ID,
        Family::SafetyApp,
        "0.1.0",
        "Intersection movement assist over the neighbour table: extrapolates both paths at \
         constant velocity, warns when the ego and a crossing peer reach the conflict \
         point together, and emits the belief-side post-encroachment time.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "conflict point",
            "solve p_ego + t·v_ego = p_peer + s·v_peer by Cramer's rule; a determinant \
             below 1e-9 is treated as parallel",
        ),
        Equation::new(
            "post-encroachment time",
            "pet = |t_ego − t_peer|, the interval between one road user leaving the \
             conflict point and the other arriving (04-models.md §11)",
        ),
    ];
    card.parameters = vec![
        todo(
            "tti_warn_s",
            "s",
            serde_json::json!(p.tti_warn_s),
            "04-models.md §11 marks the IMA time-to-intersection threshold UNVERIFIED and \
             notes it is 'not in the accessible report'",
            VSCA_PLAN,
        ),
        todo(
            "pet_warn_s",
            "s",
            serde_json::json!(p.pet_warn_s),
            "no source gives the post-encroachment time at which an intersection \
             application should warn; 04-models.md §11 records the PET threshold itself as \
             UNVERIFIED",
            "The SSAM 1.5 s time-to-collision threshold, reused because it is the one \
             VERIFIED surrogate-safety number in 04-models.md §11 and because SSAM's own \
             5 s PET threshold is a *counting* threshold: a vehicle that clears the \
             junction three seconds ahead of the ego is inside it and is no reason to warn \
             a driver. Replace it from the VSC-A companion volume's IMA alert definition.",
        ),
        todo(
            "pet_conflict_s",
            "s",
            serde_json::json!(p.pet_conflict_s),
            "04-models.md §11 records the PET threshold as UNVERIFIED",
            "5 s is the value 04-models.md §11 reports as 'commonly < 5 s, not confirmed', \
             and it gates whether a time to collision is reported rather than whether a \
             warning fires. The plan the design set gives is to read FHWA-HRT-08-050 §3 \
             and record the measured threshold; the same plan v2xw-metrics carries for its \
             own pet.",
        ),
        todo(
            "moving_threshold_mps",
            "m/s",
            serde_json::json!(p.moving_threshold_mps),
            "no source gives the speed below which a crossing path is undefined",
            "0.5 m/s is EN 302 637-2's own speed-change trigger, reused because a road user \
             whose reported speed is under the threshold at which the standard considers \
             the speed to have changed has no usable heading either. Calibrate by sweeping \
             it and taking the value at which stationary peers stop producing conflicts.",
        ),
        todo(
            "range_m",
            "m",
            serde_json::json!(p.range_m),
            "the evaluation horizon is an implementation choice",
            "As the FCW's range_m, and the same plan: raise it to the communication range \
             and confirm no warning is lost.",
        ),
    ];
    card.assumptions = vec![
        "Both paths are straight and both speeds constant over the prediction horizon, \
         which is the constant-velocity extrapolation VSC-A's own IMA description assumes \
         for a stop-sign-controlled or uncontrolled intersection."
            .into(),
        "The intersection's geometry is not read: the conflict point is where the two \
         reported paths cross, which needs no map and therefore works on a node that has \
         none."
            .into(),
    ];
    card.limitations = vec![
        "Road users are points: neither vehicle's extent beyond the conflict point is \
         accounted for, so the post-encroachment time is an over-estimate of the safety \
         margin in exactly the way v2xw-metrics records for its cell-based pet."
            .into(),
        "A curving approach is a straight line here, so a vehicle turning into the ego's \
         path is seen late."
            .into(),
        "Every numeric trigger is uncalibrated: 04-models.md §11's IMA row has no verified \
         number in it."
            .into(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Paper,
            "CAMP VSC-A final report, DOT HS 811 492A (application definition, 04-models.md \
             §11)",
        ),
        Source::new(
            SourceKind::Paper,
            "FHWA-HRT-08-051, Surrogate Safety Assessment Model (post-encroachment time)",
        ),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.validation.tests = vec![
        "safety_apps::two_paths_that_cross_together_produce_a_zero_pet".to_string(),
        "safety_apps::two_paths_that_cross_at_different_times_are_not_a_conflict".to_string(),
        "safety_apps::parallel_paths_never_produce_a_crossing_conflict".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}

// =========================================================================================
// `safety-app/eebl-vsca`
// =========================================================================================

/// The parameters of the emergency-electronic-brake-light application.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EeblParams {
    /// The deceleration at which a peer counts as braking hard, m/s².
    ///
    /// The one cited trigger: [`EEBL_DECEL_THRESHOLD_MPS2`].
    pub decel_threshold_mps2: f64,
    /// The longest interval between two claims that a deceleration may be estimated over.
    ///
    /// TS 103 759 V2.2.1's plausibility gate for an EEBL report is that "the preceding
    /// vehicle's deceleration must be positive within 500 ms before the claimed
    /// `detectionTime`" (04-models.md §11). A node estimating the deceleration itself can
    /// apply the same window: an estimate spanning longer than this is not evidence about
    /// the instant claimed.
    pub estimate_window: Duration,
    /// How long a brake event stays relevant after the last claim that supported it.
    pub hold: Duration,
    /// The corridor and heading rules are the FCW's, so a braking peer that is not ahead
    /// in the same direction is not warned about.
    pub geometry: FcwParams,
}

impl EeblParams {
    /// The starting values.
    #[must_use]
    pub fn vsca() -> EeblParams {
        EeblParams {
            decel_threshold_mps2: EEBL_DECEL_THRESHOLD_MPS2,
            estimate_window: Duration::from_millis(500),
            hold: Duration::from_millis(500),
            geometry: FcwParams::vsca(),
        }
    }
}

impl Default for EeblParams {
    fn default() -> EeblParams {
        EeblParams::vsca()
    }
}

/// What the application remembers about one peer between passes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BrakeTrack {
    /// The peer's last claimed speed.
    speed_mps: f64,
    /// The instant it claimed it, on the peer's clock.
    claimed_at: SimTime,
    /// The peer's estimated deceleration, m/s², positive when slowing.
    decel_mps2: f64,
    /// When this node last saw the deceleration exceed the threshold, on its own clock.
    braking_since: Option<SimTime>,
}

/// `safety-app/eebl-vsca` — the emergency electronic brake light.
///
/// 04-models.md §11 describes EEBL as a *pair*: the host broadcasts a self-generated
/// brake event and "remote vehicles judge relevance and warn". This is the receiving half.
/// The transmitting half is the generator's: `v2xw_msg::generator::AppEventKind::HardBraking`
/// carries the event at the same 0.4 g flag, and it is that crate's business because it
/// changes what goes on the air.
///
/// # Why the deceleration is estimated and not read
///
/// A BSM's Part II `VehicleSafetyExtensions` carries the event flags directly, and
/// 04-models.md §8.1 records the Part II cadence as UNVERIFIED (patent text only); the
/// mandatory content this crate encodes is Part I. A CAM has no brake-event field at all
/// in its basic vehicle container. So a receiver at this fidelity has the peer's claimed
/// *speeds* and nothing else, and it differentiates them — which is what a real
/// implementation falls back to when the flag is absent, and which makes the warning
/// sensitive to the message rate the rest of the simulator is measuring. When a Part II
/// encoder lands, the flag replaces the estimate and this application reads it instead.
#[derive(Debug, Clone)]
pub struct Eebl {
    card: ModelCard,
    params: EeblParams,
    tracks: BTreeMap<[u8; 8], BrakeTrack>,
    standing: Standing,
}

impl Default for Eebl {
    fn default() -> Eebl {
        Eebl::new(EeblParams::vsca())
    }
}

impl Eebl {
    /// The application with `params`.
    #[must_use]
    pub fn new(params: EeblParams) -> Eebl {
        Eebl {
            card: eebl_card(&params),
            params,
            tracks: BTreeMap::new(),
            standing: Standing::default(),
        }
    }

    /// The parameters in force.
    #[must_use]
    pub const fn params(&self) -> &EeblParams {
        &self.params
    }

    /// The deceleration this node has estimated for a peer, m/s², if it has one.
    #[must_use]
    pub fn estimated_deceleration(&self, subject: &HashedId8) -> Option<f64> {
        self.tracks.get(&digest_key(subject)).map(|t| t.decel_mps2)
    }

    /// Updates the track for one peer and returns its estimated deceleration.
    ///
    /// `None` until two claims separated by a positive interval no longer than
    /// [`EeblParams::estimate_window`] have arrived, which is the plausibility gate of
    /// TS 103 759 applied to the node's own estimate.
    fn track(&mut self, subject: &HashedId8, claimed_at: SimTime, speed_mps: f64) -> Option<f64> {
        let key = digest_key(subject);
        let previous = self.tracks.get(&key).copied();
        let decel = match previous {
            Some(p) if claimed_at > p.claimed_at => {
                let span = Duration::between(p.claimed_at, claimed_at);
                if span > self.params.estimate_window || span.is_zero() {
                    None
                } else {
                    Some((p.speed_mps - speed_mps) / span.as_secs_f64())
                }
            }
            // A repeated or out-of-order generation time is no new evidence. Treating it
            // as a zero-length interval would divide by zero; treating it as a long one
            // would invent a deceleration out of clock skew.
            _ => None,
        };
        self.tracks.insert(
            key,
            BrakeTrack {
                speed_mps,
                claimed_at,
                decel_mps2: decel.unwrap_or(0.0),
                braking_since: previous.and_then(|p| p.braking_since),
            },
        );
        decel
    }

    /// Forgets peers no longer in the table, so the track map stays the size of the
    /// neighbourhood rather than of the run.
    fn prune(&mut self, neighbors: &NeighborTable) {
        let live: BTreeMap<[u8; 8], ()> = neighbors
            .iter()
            .map(|n| (digest_key(&n.signer), ()))
            .collect();
        self.tracks.retain(|k, _| live.contains_key(k));
    }
}

impl Model for Eebl {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl SafetyApp for Eebl {
    fn on_neighbors(
        &mut self,
        _ctx: &mut dyn NodeCtx,
        _node: NodeId,
        believed: SimTime,
        neighbors: &NeighborTable,
        ego: &PositionEstimate,
    ) -> Vec<Warning> {
        self.prune(neighbors);
        let geometry = self.params.geometry;
        let mut out = Vec::new();
        let mut still: BTreeMap<[u8; 8], ()> = BTreeMap::new();
        for n in neighbors.iter() {
            if !actionable(n.state) {
                continue;
            }
            let estimate = self.track(&n.signer, n.claimed_generation_time, n.claimed_speed_mps);
            let key = digest_key(&n.signer);
            if let Some(decel) = estimate
                && decel >= self.params.decel_threshold_mps2
                && let Some(track) = self.tracks.get_mut(&key)
            {
                track.braking_since = Some(believed);
            }
            let braking = self
                .tracks
                .get(&key)
                .and_then(|t| t.braking_since)
                .is_some_and(|since| Duration::between(since, believed) <= self.params.hold);
            if !braking {
                continue;
            }
            // Relevance: 04-models.md §11 has the remote vehicles "judge relevance", and
            // the judgement is the FCW's geometry — a brake event behind the ego, or on a
            // crossing road, is not this driver's business.
            let Some(mut s) = rear_end_surrogates(
                &geometry,
                ego,
                n.claimed_pos,
                n.claimed_speed_mps,
                n.claimed_heading_rad,
            ) else {
                continue;
            };
            s.required_decel_mps2 = self
                .tracks
                .get(&key)
                .map_or(f64::NAN, |t| t.decel_mps2)
                .max(0.0);
            still.insert(key, ());
            let kind = self.standing.assert(&n.signer, believed);
            out.push(Warning {
                app: self.app_name(),
                subject: n.signer.clone(),
                kind,
                severity: if s.ttc_s.is_finite() && s.ttc_s <= self.params.geometry.ttc_warn_s {
                    Severity::Imminent
                } else {
                    Severity::Caution
                },
                surrogates: s.quantised(),
                at: believed,
            });
        }
        for subject in self.standing.retire(&still) {
            out.push(Warning {
                app: self.app_name(),
                subject,
                kind: WarningKind::Clear,
                severity: Severity::Info,
                surrogates: Surrogates::NONE,
                at: believed,
            });
        }
        out
    }

    fn cost(&self) -> OpDescriptor {
        OpDescriptor::task(EEBL_ID, OpClass::Application)
    }

    fn app_name(&self) -> &'static str {
        "eebl"
    }
}

fn eebl_card(p: &EeblParams) -> ModelCard {
    let mut card = ModelCard::new(
        EEBL_ID,
        Family::SafetyApp,
        "0.1.0",
        "Emergency electronic brake light, receiving half: estimates a peer's \
         deceleration from its claimed speeds, applies the TS 103 759 plausibility window \
         and the forward-collision geometry, and warns when a relevant peer ahead is \
         braking at or above 0.4 g.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![Equation::new(
        "deceleration estimate",
        "a = (v(t₁) − v(t₂)) / (t₂ − t₁) over two consecutive claims, with \
         0 < t₂ − t₁ ≤ estimate_window_ms and both times the sender's own",
    )];
    card.parameters = vec![
        Parameter::new(
            "decel_threshold_mps2",
            "m/s2",
            serde_json::json!(p.decel_threshold_mps2),
            Source::new(
                SourceKind::Standard,
                "SAE J2945/1 BSM Part II emergency-brake event flag at 0.4 g = 3.92 m/s² \
                 [04-models.md §8.1 and §11, both VERIFIED]",
            ),
        ),
        Parameter::new(
            "estimate_window_ms",
            "ms",
            serde_json::json!(p.estimate_window.as_nanos() / 1_000_000),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 759 V2.2.1: the EEBL plausibility gate requires the preceding \
                 vehicle's deceleration to be positive within 500 ms before the claimed \
                 detectionTime [04-models.md §11, R11 §E2]",
            ),
        ),
        todo(
            "hold_ms",
            "ms",
            serde_json::json!(p.hold.as_nanos() / 1_000_000),
            "no source gives how long a brake-event warning stands after the last \
             supporting claim",
            "500 ms matches the plausibility window, so the warning stands exactly as long \
             as the evidence would still be admissible. Calibrate against the VSC-A \
             companion volume's EEBL alert duration, or against J2735's own event-flag \
             persistence once a Part II encoder exists to read it from.",
        ),
    ];
    card.assumptions = vec![
        "A receiver at this fidelity has no brake-event flag: BSM Part II is not encoded \
         (04-models.md §8.1 marks its cadence UNVERIFIED) and a CAM's basic vehicle \
         container carries none, so the deceleration is differentiated from claimed \
         speeds."
            .into(),
        "The interval is measured between the two *claimed* generation times, so a peer \
         with a drifting clock produces a wrong deceleration — which is the real \
         consequence and the one a clock attack should have."
            .into(),
    ];
    card.limitations = vec![
        "A two-point difference of a quantised speed is noisy: a CAM's speed is reported to \
         0.01 m/s and a 100 ms interval turns one quantum into 0.1 m/s², so the estimate \
         is usable at the 0.4 g threshold and not below it."
            .into(),
        "The host-side half — broadcasting a self-generated brake event — is the \
         generator's, not this application's."
            .into(),
    ];
    card.sources = vec![
        Source::new(
            SourceKind::Paper,
            "CAMP VSC-A final report, DOT HS 811 492A (application definition, 04-models.md \
             §11)",
        ),
        Source::new(SourceKind::Standard, "ETSI TS 103 759 V2.2.1 §4-7"),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.validation.tests = vec![
        "safety_apps::a_peer_braking_at_point_four_g_raises_an_eebl_warning".to_string(),
        "safety_apps::a_deceleration_estimated_over_too_long_a_window_is_refused".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}

// =========================================================================================
// The set a node runs
// =========================================================================================

/// The applications one node runs, in a fixed order, with the relevance scores they imply.
///
/// # Why the set and not a bare `Vec`
///
/// Two jobs that would otherwise be duplicated at every call site. It **emits** the
/// `app.warning` records, once per warning, so no application has to know the channel;
/// and it **publishes the relevance scores** the `on-demand` verification policy consumes.
/// 06-node-models.md §2.1 specifies that policy as "verify only messages that a safety
/// application marks relevant", and until now nothing in this crate produced a mark — the
/// module documentation of `crate::lib` listed it as a known gap and
/// [`crate::policy::OnDemand`] saw `None` for every message.
///
/// The score is `1 / (1 + ttc)`, bounded to `[0, 1]`, taken over the smallest time to
/// collision any application computed for that subject: 1 at a collision, 0.4 at the SSAM
/// conflict threshold, 0.1 at 9 s. That shape is a modelling choice with no published
/// source, so it is a card parameter with a plan like any other.
pub struct SafetyAppSet {
    apps: Vec<Box<dyn SafetyApp>>,
    relevance: BTreeMap<[u8; 8], f64>,
    warnings: u64,
    cleared: u64,
}

impl core::fmt::Debug for SafetyAppSet {
    /// `dyn SafetyApp` is not `Debug` — a family trait that required it would force every
    /// out-of-process plug-in to implement it — so the set prints its members' ids, which
    /// is what a reader of a panic message wants anyway.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SafetyAppSet")
            .field("apps", &self.ids())
            .field("warnings", &self.warnings)
            .field("cleared", &self.cleared)
            .finish()
    }
}

impl Default for SafetyAppSet {
    /// The three applications of 04-models.md §11 this crate implements, in card order.
    fn default() -> SafetyAppSet {
        SafetyAppSet::new(vec![
            Box::new(Fcw::default()),
            Box::new(Ima::default()),
            Box::new(Eebl::default()),
        ])
    }
}

impl SafetyAppSet {
    /// A set over `apps`, run in the order given.
    #[must_use]
    pub fn new(apps: Vec<Box<dyn SafetyApp>>) -> SafetyAppSet {
        SafetyAppSet {
            apps,
            relevance: BTreeMap::new(),
            warnings: 0,
            cleared: 0,
        }
    }

    /// An empty set: a node that receives and does not act.
    #[must_use]
    pub fn none() -> SafetyAppSet {
        SafetyAppSet::new(Vec::new())
    }

    /// How many applications are installed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.apps.len()
    }

    /// Whether no application is installed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    /// The model ids, in run order.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.apps.iter().map(|a| a.id().to_string()).collect()
    }

    /// The work one pass costs, one descriptor per application, in run order.
    #[must_use]
    pub fn costs(&self) -> Vec<OpDescriptor> {
        self.apps.iter().map(|a| a.cost()).collect()
    }

    /// How many warnings have been raised or refreshed.
    #[must_use]
    pub const fn warnings(&self) -> u64 {
        self.warnings
    }

    /// How many warnings have been cleared.
    #[must_use]
    pub const fn cleared(&self) -> u64 {
        self.cleared
    }

    /// The relevance score for one signer, for [`crate::policy::OnDemand`].
    #[must_use]
    pub fn relevance_of(&self, signer: &HashedId8) -> Option<f64> {
        self.relevance.get(&digest_key(signer)).copied()
    }

    /// Every relevance score, by digest — the map a runtime installs.
    #[must_use]
    pub fn relevance(&self) -> &BTreeMap<[u8; 8], f64> {
        &self.relevance
    }

    /// Runs every application over the table, emits the records, and refreshes the
    /// relevance scores.
    ///
    /// The warnings come back in application order then digest order, which is a total
    /// order and therefore the same on every run.
    pub fn run(
        &mut self,
        ctx: &mut dyn NodeCtx,
        node: NodeId,
        believed: SimTime,
        neighbors: &NeighborTable,
        ego: &PositionEstimate,
    ) -> Vec<Warning> {
        let mut out = Vec::new();
        let mut scores: BTreeMap<[u8; 8], f64> = BTreeMap::new();
        for app in &mut self.apps {
            for w in app.on_neighbors(ctx, node, believed, neighbors, ego) {
                if w.fired() {
                    self.warnings = self.warnings.saturating_add(1);
                    if w.surrogates.ttc_s.is_finite() {
                        let score = relevance_from_ttc(w.surrogates.ttc_s);
                        let key = digest_key(&w.subject);
                        let best = scores.get(&key).copied().unwrap_or(0.0);
                        scores.insert(key, score.max(best));
                    }
                } else {
                    self.cleared = self.cleared.saturating_add(1);
                }
                ctx.emit(WarningRecord::of(node, &w));
                out.push(w);
            }
        }
        self.relevance = scores;
        out
    }
}

/// The relevance score a time to collision implies: `1 / (1 + ttc)`, clamped to `[0, 1]`.
///
/// A modelling choice, not a published one. It is monotone decreasing in the time to
/// collision, it needs no threshold of its own, and it puts the SSAM conflict threshold at
/// 0.4 — which is why [`crate::policy::OnDemand`]'s shipped threshold of 0.5 verifies a
/// message from a peer that is closer than the conflict threshold and skips one that is
/// further. A negative or absent time to collision scores zero.
#[must_use]
pub fn relevance_from_ttc(ttc_s: f64) -> f64 {
    if !ttc_s.is_finite() || ttc_s < 0.0 {
        return 0.0;
    }
    (1.0 / (1.0 + ttc_s)).clamp(0.0, 1.0)
}

/// Registers the three applications of 04-models.md §11 that this crate implements.
///
/// # Errors
/// [`v2xw_core::registry::RegistryError`] if a card fails validation or an id is taken.
pub fn register_all(
    registry: &mut v2xw_core::registry::Registry,
) -> core::result::Result<Vec<v2xw_core::registry::ModelRef>, v2xw_core::registry::RegistryError> {
    use std::sync::Arc;
    use v2xw_core::model::ModelHandle;

    let apps: Vec<ModelHandle> = vec![
        Arc::new(Fcw::default()),
        Arc::new(Ima::default()),
        Arc::new(Eebl::default()),
    ];
    let mut refs = Vec::with_capacity(apps.len());
    for a in apps {
        refs.push(registry.register_model(a)?);
    }
    Ok(refs)
}

/// How many models [`register_all`] registers.
pub const MODEL_COUNT: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::belief::FixQuality;
    use v2xw_core::rng::RngRegistry;
    use v2xw_core::time::NS_PER_MS;

    use crate::ctx::NodeRuntimeCtx;
    use crate::stores::{Neighbor, pseudo_signer};

    fn ego(pos: Vec3, speed: f64, heading: f64) -> PositionEstimate {
        let mut p = PositionEstimate::no_fix(0);
        p.pos = pos;
        p.heading_rad = heading;
        p.vel = claimed_velocity(speed, heading);
        p.fix = FixQuality::ThreeD;
        p
    }

    fn neighbour(j: u32, pos: Vec3, speed: f64, heading: f64, claimed_at: SimTime) -> Neighbor {
        Neighbor {
            signer: pseudo_signer(NodeId::new(9), j),
            claimed_pos: pos,
            claimed_speed_mps: speed,
            claimed_heading_rad: heading,
            claimed_generation_time: claimed_at,
            last_heard: claimed_at,
            messages: 1,
            state: VerificationState::Verified,
        }
    }

    fn table(entries: Vec<Neighbor>) -> NeighborTable {
        let mut t = NeighborTable::new(16);
        for e in entries {
            t.observe(e);
        }
        t
    }

    /// `gap / closing`, on a hand-computed case: 30 m at 15 m/s of closing is 2 s.
    #[test]
    fn time_to_collision_is_the_gap_over_the_closing_speed() {
        assert_eq!(time_to_collision_s(30.0, 15.0), 2.0);
        assert!(time_to_collision_s(30.0, 0.0).is_nan());
        assert!(time_to_collision_s(30.0, -5.0).is_nan());
    }

    /// The required deceleration of a 20 m/s closing speed over 40 m is 5 m/s².
    #[test]
    fn the_required_deceleration_is_the_kinematic_one() {
        assert_eq!(required_deceleration_mps2(40.0, 20.0), 5.0);
        assert!(required_deceleration_mps2(0.0, 20.0).is_nan());
    }

    /// Two paths crossing at the origin, one arriving from the west at 10 m/s from 100 m
    /// and one from the south at 20 m/s from 200 m: both take 10 s, so the PET is zero.
    #[test]
    fn a_symmetric_crossing_has_a_zero_post_encroachment_time() {
        let (t_a, t_b) = crossing_times_s(
            Vec3::new(-100.0, 0.0, 0.0),
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::new(0.0, -200.0, 0.0),
            Vec3::new(0.0, 20.0, 0.0),
        )
        .expect("the paths cross");
        assert!((t_a - 10.0).abs() < 1e-9, "{t_a}");
        assert!((t_b - 10.0).abs() < 1e-9, "{t_b}");
    }

    /// Parallel paths have no crossing point, and a determinant that is merely small is
    /// treated as parallel rather than producing a conflict point kilometres away.
    #[test]
    fn parallel_paths_do_not_cross() {
        assert!(
            crossing_times_s(
                Vec3::ZERO,
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
            )
            .is_none()
        );
    }

    /// A crossing already behind both road users is not a conflict.
    #[test]
    fn a_crossing_behind_both_is_not_a_conflict() {
        assert!(
            crossing_times_s(
                Vec3::new(100.0, 0.0, 0.0),
                Vec3::new(10.0, 0.0, 0.0),
                Vec3::new(0.0, 100.0, 0.0),
                Vec3::new(0.0, 10.0, 0.0),
            )
            .is_none()
        );
    }

    /// The FCW fires on a stationary vehicle 20 m ahead while the ego does 20 m/s: a
    /// one-second time to collision, which is inside the 1.5 s threshold.
    #[test]
    fn a_stationary_vehicle_ahead_produces_a_one_second_ttc() {
        let app = Fcw::default();
        let s = app
            .evaluate(
                &ego(Vec3::ZERO, 20.0, 0.0),
                Vec3::new(20.0, 0.0, 0.0),
                0.0,
                0.0,
            )
            .expect("a candidate");
        assert!((s.ttc_s - 1.0).abs() < 1e-9, "{}", s.ttc_s);
        assert!(s.is_conflict());
        assert!(s.pet_s.is_nan(), "a rear-end pair has no PET");
    }

    /// A peer outside the corridor is not a candidate, however close it is.
    #[test]
    fn a_peer_beside_the_ego_is_not_a_rear_end_candidate() {
        let app = Fcw::default();
        assert!(
            app.evaluate(
                &ego(Vec3::ZERO, 20.0, 0.0),
                Vec3::new(20.0, 5.0, 0.0),
                0.0,
                0.0,
            )
            .is_none()
        );
    }

    /// An oncoming peer is not a rear-end candidate either: the heading tolerance excludes
    /// it, which is what stops the FCW warning about every vehicle on the other side of
    /// the road.
    #[test]
    fn an_oncoming_peer_is_not_a_rear_end_candidate() {
        let app = Fcw::default();
        assert!(
            app.evaluate(
                &ego(Vec3::ZERO, 20.0, 0.0),
                Vec3::new(20.0, 0.0, 0.0),
                20.0,
                core::f64::consts::PI,
            )
            .is_none()
        );
    }

    /// The set emits one record per warning on `app.warning`, and clears it when the
    /// condition goes away.
    #[test]
    fn the_set_emits_a_record_and_then_a_clear() {
        let rng = RngRegistry::new(7);
        let mut ctx = NodeRuntimeCtx::new(0, &rng);
        let mut set = SafetyAppSet::new(vec![Box::new(Fcw::default())]);
        let node = NodeId::new(1);

        let close = table(vec![neighbour(0, Vec3::new(20.0, 0.0, 0.0), 0.0, 0.0, 0)]);
        let first = set.run(&mut ctx, node, 0, &close, &ego(Vec3::ZERO, 20.0, 0.0));
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].kind, WarningKind::Issue);
        assert!(first[0].fired());
        assert_eq!(set.warnings(), 1);

        // Still there one tick later: an update, not a second issue.
        let second = set.run(
            &mut ctx,
            node,
            100 * NS_PER_MS,
            &close,
            &ego(Vec3::ZERO, 20.0, 0.0),
        );
        assert_eq!(second[0].kind, WarningKind::Update);

        // Gone: a clear, with no surrogate measures attached.
        let empty = table(Vec::new());
        let third = set.run(
            &mut ctx,
            node,
            200 * NS_PER_MS,
            &empty,
            &ego(Vec3::ZERO, 20.0, 0.0),
        );
        assert_eq!(third.len(), 1);
        assert_eq!(third[0].kind, WarningKind::Clear);
        assert!(!third[0].fired());
        assert_eq!(set.cleared(), 1);

        let channels: Vec<&str> = ctx.emitted().iter().map(|r| r.channel).collect();
        assert_eq!(channels.len(), 3);
        assert!(channels.iter().all(|c| *c == "app.warning"));
    }

    /// The relevance score is monotone in the time to collision, and the shipped
    /// `on-demand` threshold of 0.5 falls between a collision and the conflict threshold.
    #[test]
    fn relevance_is_monotone_and_straddles_the_on_demand_threshold() {
        assert_eq!(relevance_from_ttc(0.0), 1.0);
        assert!(relevance_from_ttc(1.0) > relevance_from_ttc(2.0));
        assert!(relevance_from_ttc(f64::NAN) == 0.0);
        let at_conflict = relevance_from_ttc(TTC_CONFLICT_THRESHOLD_S);
        assert!((at_conflict - 0.4).abs() < 1e-9, "{at_conflict}");
        assert!(relevance_from_ttc(0.9) > 0.5);
    }

    /// Every float on the record is on the quantisation grid, and `NaN` survives it.
    #[test]
    fn every_recorded_float_is_quantised_and_nan_survives() {
        let w = Warning {
            app: "fcw",
            subject: pseudo_signer(NodeId::new(1), 0),
            kind: WarningKind::Issue,
            severity: Severity::Warning,
            surrogates: Surrogates {
                ttc_s: 1.0 / 3.0,
                pet_s: f64::NAN,
                required_decel_mps2: 2.0 / 3.0,
                distance_m: 1.2345678,
                closing_mps: -0.00049,
            },
            at: 0,
        };
        let r = WarningRecord::of(NodeId::new(1), &w);
        for v in [r.ttc_s, r.required_decel_mps2, r.distance_m, r.closing_mps] {
            assert!(math::is_on_grid(v, SURROGATE_Q), "{v} is off the grid");
        }
        assert!(r.pet_s.is_nan());
        assert_eq!(r.distance_m, 1.235);
    }

    /// The hex spelling is sixteen lowercase hex digits, which is what a join against
    /// `det.observation` needs.
    #[test]
    fn the_subject_is_lowercase_hex() {
        let d = pseudo_signer(NodeId::new(3), 1);
        let hex = digest_hex(&d);
        assert_eq!(hex.len(), 16);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hex, hex.to_lowercase());
    }
}
