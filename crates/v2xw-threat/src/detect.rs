//! Local, on-node misbehaviour detection: the legacy twelve, the two gated checks and the
//! soft tracker feature, with every threshold a declared card parameter.
//!
//! # Normalisation
//!
//! Every detector returns a score normalised so that **≈ 1 is its firing threshold**
//! (`detnorm ≈ 1 at threshold`, 01-inventory §3.3). That is what makes a score comparable
//! across checks whose raw units are metres, degrees, seconds and counts, and it is the
//! contract the machine-learning dataset's `detnorm_*` columns depend on. A check fires
//! when its score has been ≥ 1 for [`DetectorParams::min_consecutive`] messages in a row
//! from the same signer, which is the legacy streak gate: a single GNSS outlier is one
//! sample, a real kinematic attack persists.
//!
//! # Belief only
//!
//! Every input is an [`crate::obs::ObservedMessage`], the receiver's own
//! [`crate::obs::SelfBelief`] and, for the map check, the node's own map store. There is
//! no path from here to the world: see [`crate::obs`]. Two consequences are worth naming
//! because they are the honest cost of the firewall:
//!
//! * **`acceptanceRangeThreshold` compares against the receiver's own configured range**,
//!   not against the true propagation distance — which is why the legacy engine's own note
//!   says an RSU with a longer range must use *its* range or it flags honest distant
//!   senders.
//! * **`sybilCoLocation` counts only what this receiver heard.** The legacy engine
//!   computed the co-location census over *every broadcast in the step*
//!   (`run.py`, `cells = Counter(… for b in broadcasts)`), which is a global view no
//!   single receiver has; the JVM reference does it per receiver over a 1.5 s window. This
//!   port is per receiver. A Sybil whose ghosts are heard by different receivers is
//!   therefore harder to catch here than in the legacy engine, which is a real property of
//!   the attack and not a regression. See the crate report.
//!
//! # What is deliberately soft
//!
//! `kalmanConsistency` is carried in the fingerprint and never fires on its own: a
//! constant-velocity tracker false-positives on a sustained curve, so promoting it to a
//! trigger would manufacture false accusations against honest vehicles going round
//! corners. The legacy engine says exactly this and so does the JVM reference.

use std::collections::BTreeMap;

use crate::capability::{angle_diff_rad, bearing_rad};
use crate::cards::{LEGACY_JVM, LEGACY_PY, design, legacy, legacy_param, legacy_uncited, standard};
use crate::ctx::{ThreatCtx, ThreatCtxExt};
use crate::obs::{LocalEnvironment, ObservedKind, ObservedMessage, SelfBelief, StationType};
use crate::records::DetObservation;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Tier, Validation, ValidationStatus,
};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::time::{SimTime, ns_to_secs};

/// The model id the detector suite's card and every `det.observation` it writes carry.
pub const MODEL_ID: &str = "threat/detector/legacy-12";

/// One check in the suite.
///
/// The discriminant order is the legacy `DET_KEYS` order (`run.py :: DET_KEYS`), which is
/// the on-disk `detnorm_*` column order of the legacy corpus. Two gated checks follow the
/// twelve — the legacy engine appends them only when station types or the event-message
/// layer are in play — and the soft tracker feature is last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DetectorId {
    /// Claimed displacement does not match claimed speed over the interval.
    PositionSpeedInconsistency,
    /// Claimed position moved further than any plausible motion allows.
    PositionJump,
    /// Claimed heading disagrees with the bearing of the claimed motion.
    HeadingInconsistency,
    /// The generation time is old, or the claim repeats a stored one.
    StaleOrReplay,
    /// The claimed position is frozen while the claimed speed is not zero.
    ConstantPositionFrozen,
    /// The implied acceleration exceeds what a vehicle can do.
    ImplausibleAcceleration,
    /// Several distinct certificates claim nearly one point and one heading.
    SybilCoLocation,
    /// The claimed position is further away than this receiver can hear.
    AcceptanceRangeThreshold,
    /// The sender is transmitting faster than the message rate allows.
    BeaconFrequency,
    /// The signature did not verify.
    SignatureVerification,
    /// The certificate is expired or not yet valid.
    CertValidity,
    /// The claimed position is not on any road the node's map knows.
    MapOffRoad,
    /// A beacon declaring the vulnerable-road-user type is not moving like one.
    VruImpersonation,
    /// An event message announces a hazard the sender's own claimed speed contradicts.
    DenmPlausibility,
    /// Residual against a constant-velocity tracker. **Soft**: never fires on its own.
    KalmanConsistency,
}

impl DetectorId {
    /// The legacy twelve, in the legacy `DET_KEYS` order.
    pub const LEGACY_12: [DetectorId; 12] = [
        DetectorId::PositionSpeedInconsistency,
        DetectorId::PositionJump,
        DetectorId::HeadingInconsistency,
        DetectorId::StaleOrReplay,
        DetectorId::ConstantPositionFrozen,
        DetectorId::ImplausibleAcceleration,
        DetectorId::SybilCoLocation,
        DetectorId::AcceptanceRangeThreshold,
        DetectorId::BeaconFrequency,
        DetectorId::SignatureVerification,
        DetectorId::CertValidity,
        DetectorId::MapOffRoad,
    ];

    /// The motion checks a vulnerable-road-user declaration suppresses
    /// (`run.py :: MOTION_KEYS`).
    pub const MOTION: [DetectorId; 5] = [
        DetectorId::PositionSpeedInconsistency,
        DetectorId::PositionJump,
        DetectorId::HeadingInconsistency,
        DetectorId::ConstantPositionFrozen,
        DetectorId::ImplausibleAcceleration,
    ];

    /// Every check, including the two gated ones and the soft feature.
    pub const ALL: [DetectorId; 15] = [
        DetectorId::PositionSpeedInconsistency,
        DetectorId::PositionJump,
        DetectorId::HeadingInconsistency,
        DetectorId::StaleOrReplay,
        DetectorId::ConstantPositionFrozen,
        DetectorId::ImplausibleAcceleration,
        DetectorId::SybilCoLocation,
        DetectorId::AcceptanceRangeThreshold,
        DetectorId::BeaconFrequency,
        DetectorId::SignatureVerification,
        DetectorId::CertValidity,
        DetectorId::MapOffRoad,
        DetectorId::VruImpersonation,
        DetectorId::DenmPlausibility,
        DetectorId::KalmanConsistency,
    ];

    /// The check's id, as the legacy `reason_codes` and `detnorm_*` columns spell it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            DetectorId::PositionSpeedInconsistency => "positionSpeedInconsistency",
            DetectorId::PositionJump => "positionJump",
            DetectorId::HeadingInconsistency => "headingInconsistency",
            DetectorId::StaleOrReplay => "staleOrReplay",
            DetectorId::ConstantPositionFrozen => "constantPositionFrozen",
            DetectorId::ImplausibleAcceleration => "implausibleAcceleration",
            DetectorId::SybilCoLocation => "sybilCoLocation",
            DetectorId::AcceptanceRangeThreshold => "acceptanceRangeThreshold",
            DetectorId::BeaconFrequency => "beaconFrequency",
            DetectorId::SignatureVerification => "signatureVerification",
            DetectorId::CertValidity => "certValidity",
            DetectorId::MapOffRoad => "mapOffRoad",
            DetectorId::VruImpersonation => "vruImpersonation",
            DetectorId::DenmPlausibility => "denmPlausibility",
            DetectorId::KalmanConsistency => "kalmanConsistency",
        }
    }

    /// Whether the check may raise a report on its own.
    ///
    /// False for [`DetectorId::KalmanConsistency`]: it is a fusion feature, not a
    /// trigger, because a constant-velocity tracker false-positives on a sustained curve.
    #[must_use]
    pub const fn is_hard(self) -> bool {
        !matches!(self, DetectorId::KalmanConsistency)
    }

    /// Index into a [`Fingerprint`].
    #[must_use]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// Which TS 103 759 observation class this check belongs to
    /// (07-threats-and-detection.md §3.1).
    ///
    /// 1 implausible values; 2 inconsistency with previous messages from the same
    /// station; 3 inconsistency with the local environment or LDM; 4 inconsistency with
    /// on-board sensors; 5 inconsistency with other stations' messages. `0` for the two
    /// checks that are envelope-level rather than observational.
    #[must_use]
    pub const fn ts103759_class(self) -> u8 {
        match self {
            DetectorId::AcceptanceRangeThreshold
            | DetectorId::ImplausibleAcceleration
            | DetectorId::BeaconFrequency
            | DetectorId::DenmPlausibility
            | DetectorId::VruImpersonation => 1,
            DetectorId::PositionSpeedInconsistency
            | DetectorId::PositionJump
            | DetectorId::HeadingInconsistency
            | DetectorId::StaleOrReplay
            | DetectorId::ConstantPositionFrozen
            | DetectorId::KalmanConsistency => 2,
            DetectorId::MapOffRoad => 3,
            DetectorId::SybilCoLocation => 5,
            DetectorId::SignatureVerification | DetectorId::CertValidity => 0,
        }
    }
}

impl core::fmt::Display for DetectorId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Every check's normalised score for one message: the fusion fingerprint the
/// machine-learning contract consumes as `detnorm_*`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fingerprint([f64; DetectorId::ALL.len()]);

impl Default for Fingerprint {
    fn default() -> Self {
        Self([0.0; DetectorId::ALL.len()])
    }
}

impl Fingerprint {
    /// One check's score.
    #[must_use]
    pub fn get(&self, id: DetectorId) -> f64 {
        self.0[id.index()]
    }

    /// Sets one check's score.
    pub fn set(&mut self, id: DetectorId, v: f64) {
        self.0[id.index()] = v;
    }

    /// Raises one check's score to `v` if `v` is larger.
    pub fn raise(&mut self, id: DetectorId, v: f64) {
        let slot = &mut self.0[id.index()];
        if v > *slot {
            *slot = v;
        }
    }

    /// Every `(check, score)` pair in [`DetectorId::ALL`] order.
    pub fn iter(&self) -> impl Iterator<Item = (DetectorId, f64)> + '_ {
        DetectorId::ALL.into_iter().map(|d| (d, self.get(d)))
    }

    /// The largest score in the fingerprint.
    #[must_use]
    pub fn max(&self) -> f64 {
        self.0.iter().copied().fold(0.0, f64::max)
    }
}

/// One check firing on one message.
#[derive(Debug, Clone, PartialEq)]
pub struct Observation {
    /// Which check.
    pub detector: DetectorId,
    /// Its normalised score, ≈ 1 at the firing threshold.
    pub score: f64,
    /// The subject: the signer's certificate digest in hex.
    pub subject: String,
    /// When the observation was made, on the observing node's clock.
    pub at: SimTime,
}

/// What one message produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// The subject: the signer's certificate digest in hex.
    pub subject: String,
    /// Every check's score.
    pub fingerprint: Fingerprint,
    /// The checks that fired, highest score first — the legacy `reason_codes` order.
    pub fired: Vec<Observation>,
}

impl Verdict {
    /// True when at least one check fired.
    #[must_use]
    pub fn fired(&self) -> bool {
        !self.fired.is_empty()
    }

    /// The highest-scoring fired check: the reason a report leads with.
    #[must_use]
    pub fn leading(&self) -> Option<&Observation> {
        self.fired.first()
    }
}

/// What a detector costs the node's CPU per message (03-interfaces.md §9, `Detector::cost`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectorCost {
    /// Microseconds of node CPU per message checked.
    pub per_message_us: f64,
}

/// A local, on-node misbehaviour detector.
pub trait Detector: Model {
    /// Checks one received message against what this node believes.
    ///
    /// `me` is the node's own belief, `env` its own map store. There is no world
    /// argument, which is invariant I-T2 expressed as a signature.
    fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        me: &SelfBelief,
        m: &ObservedMessage,
        env: &dyn LocalEnvironment,
    ) -> Verdict;

    /// What one call costs the node's CPU.
    fn cost(&self) -> DetectorCost;
}

/// Every threshold the suite reads. Defaults are the legacy values.
#[derive(Debug, Clone, PartialEq)]
pub struct DetectorParams {
    /// Residual scale, metres (`PipelineConfig.consistency_threshold_m`, 5.0).
    pub consistency_threshold_m: f64,
    /// Heading disagreement that scores 1.0, degrees
    /// (`PipelineConfig.heading_threshold_deg`, 35.0).
    pub heading_threshold_deg: f64,
    /// How old the motion reference is kept, seconds (`PipelineConfig.detector_lag_s`,
    /// 1.5). A short lag keeps the path straight over the interval, so a turn does not
    /// read as an inconsistency.
    pub detector_lag_s: f64,
    /// How many broadcast-uncertainty sigmas a residual must exceed
    /// (`PipelineConfig.detector_z_threshold`, 3.0).
    pub z_threshold: f64,
    /// Consecutive violations before a check fires (`PipelineConfig.detector_min_consec`,
    /// 2).
    pub min_consecutive: u32,
    /// Distinct co-located certificates that score 1.0 (`PipelineConfig.sybil_min_certs`,
    /// `_SYBIL_MIN` = 4).
    pub sybil_min_certs: u32,
    /// Co-location cell size, metres (`PipelineConfig.sybil_cell_m`, `_CELL_M` = 3.0).
    pub sybil_cell_m: f64,
    /// How long a co-location observation counts for, seconds.
    pub sybil_window_s: f64,
    /// Tolerance for a claim beyond this receiver's range, metres
    /// (`PipelineConfig.art_max_m`, 150.0).
    pub art_max_m: f64,
    /// Tolerated distance from the nearest road, metres (`PipelineConfig.offroad_tol_m`,
    /// 15.0).
    pub offroad_tol_m: f64,
    /// Implausible-acceleration threshold, m/s² (`PipelineConfig.max_accel_mps2`, 12.0).
    pub max_accel_mps2: f64,
    /// Beacon-rate normaliser, messages per interval (`PipelineConfig.freq_max`, 6.0).
    pub freq_max: f64,
    /// Staleness that scores 1.0, seconds (`PipelineConfig.stale_max_s`, 5.0).
    pub stale_max_s: f64,
    /// The generation interval the rate checks assume, seconds (`PipelineConfig.dt`, 1.0).
    pub generation_interval_s: f64,
    /// Slack on the certificate validity window, seconds (`run.py`, `cvt + 1.0`).
    pub cert_slack_s: f64,
    /// The score a failed signature or certificate check reports (`run.py`, `1.5`).
    pub hard_fail_score: f64,
    /// The score a frozen claimed position reports (`run.py`, `1.5`).
    pub frozen_score: f64,
    /// The staleness score a frozen claimed position also reports (`run.py`, `1.2`).
    pub frozen_stale_score: f64,
    /// Claimed speed above which a frozen position is a contradiction, m/s
    /// (`run.py`, `cs > 0.5`).
    pub frozen_min_speed_mps: f64,
    /// Jerk tolerance factor in the position/speed residual (`run.py`, `0.3`).
    pub jerk_slack_factor: f64,
    /// Floor on the uncertainty scale, as a fraction of
    /// [`Self::consistency_threshold_m`] (`run.py`, `0.5 *`).
    pub tolerance_floor_factor: f64,
    /// Claimed speed below which the heading check is not attempted, m/s
    /// (`run.py`, `cs > 3.0`).
    pub heading_min_speed_mps: f64,
    /// Minimum claimed displacement for the heading check, metres (`run.py`, `5.0`).
    pub heading_min_disp_m: f64,
    /// The same, as a multiple of the broadcast confidence (`run.py`, `2.5 * conf`).
    pub heading_conf_factor: f64,
    /// The alpha-beta tracker's position gain (`ScmsBeaconApp.java :: KF_ALPHA`, 0.5).
    pub kalman_alpha: f64,
    /// Its velocity gain (`ScmsBeaconApp.java :: KF_BETA`, 0.3).
    pub kalman_beta: f64,
    /// Speed above which a self-declared vulnerable road user is a vehicle, m/s
    /// (`VRU_MAX_PLAUSIBLE_SPEED_MPS`, 10.0).
    pub vru_max_plausible_speed_mps: f64,
    /// The multipath-outlier magnitude allowed for in the vulnerable-road-user position
    /// arm, metres (`PipelineConfig.gps_outlier_mag_m`, 12.0).
    pub gps_outlier_mag_m: f64,
    /// Claimed speed above which a generic hazard announcement is implausible, m/s
    /// (`DENM_IMPLAUSIBLE_SPEED_MPS`, 6.0).
    pub denm_implausible_speed_mps: f64,
    /// The benign brake/stationary trigger bound, m/s (`DENM_BENIGN_MAX_SPEED_MPS`, 4.0).
    /// The brake-specific implausibility bound is derived as this plus
    /// [`Self::denm_brake_margin_mps`].
    pub denm_benign_max_speed_mps: f64,
    /// The margin above the benign bound at which a brake announcement is implausible,
    /// m/s (`DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS = DENM_BENIGN_MAX_SPEED_MPS + 0.5`).
    pub denm_brake_margin_mps: f64,
    /// Whether the suite runs the vulnerable-road-user impersonation check.
    ///
    /// The legacy engine appends it only when station types are in play, so that the
    /// default corpus stays byte-identical; the same gate is kept here.
    pub station_types_in_play: bool,
    /// Whether the suite runs the event-message plausibility check.
    pub event_messages_in_play: bool,
}

impl Default for DetectorParams {
    fn default() -> Self {
        Self {
            consistency_threshold_m: 5.0,
            heading_threshold_deg: 35.0,
            detector_lag_s: 1.5,
            z_threshold: 3.0,
            min_consecutive: 2,
            sybil_min_certs: 4,
            sybil_cell_m: 3.0,
            sybil_window_s: 1.0,
            art_max_m: 150.0,
            offroad_tol_m: 15.0,
            max_accel_mps2: 12.0,
            freq_max: 6.0,
            stale_max_s: 5.0,
            generation_interval_s: 1.0,
            cert_slack_s: 1.0,
            hard_fail_score: 1.5,
            frozen_score: 1.5,
            frozen_stale_score: 1.2,
            frozen_min_speed_mps: 0.5,
            jerk_slack_factor: 0.3,
            tolerance_floor_factor: 0.5,
            heading_min_speed_mps: 3.0,
            heading_min_disp_m: 5.0,
            heading_conf_factor: 2.5,
            kalman_alpha: 0.5,
            kalman_beta: 0.3,
            vru_max_plausible_speed_mps: 10.0,
            gps_outlier_mag_m: 12.0,
            denm_implausible_speed_mps: 6.0,
            denm_benign_max_speed_mps: 4.0,
            denm_brake_margin_mps: 0.5,
            station_types_in_play: false,
            event_messages_in_play: false,
        }
    }
}

impl DetectorParams {
    /// The brake-specific implausibility bound, derived rather than configured so it can
    /// never sit below the benign trigger and flag a genuine emergency brake.
    #[must_use]
    pub fn denm_brake_implausible_speed_mps(&self) -> f64 {
        self.denm_benign_max_speed_mps + self.denm_brake_margin_mps
    }
}

/// One claimed fix this receiver kept, for the motion checks.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Fix {
    x: f64,
    y: f64,
    speed: f64,
    heading: f64,
    t: SimTime,
}

/// Per-subject tracker state.
///
/// Per *receiver* per subject: the legacy engine kept one global dictionary keyed by
/// `(rx.vid, digest)`, and 07-threats §3.1 requires per-receiver state, which is what this
/// is — the suite belongs to one node.
#[derive(Debug, Clone, Default, PartialEq)]
struct SubjectState {
    history: Vec<Fix>,
    streak: BTreeMap<DetectorId, u32>,
    kf: Option<(f64, f64, f64, f64, SimTime)>,
}

/// The legacy twelve-detector suite plus the two gated checks and the soft tracker.
#[derive(Debug, Clone)]
pub struct Legacy12 {
    card: ModelCard,
    params: DetectorParams,
    subjects: BTreeMap<[u8; 8], SubjectState>,
    /// Co-location census: `(cell_x, cell_y, heading_octant) -> digest -> last heard`.
    cells: BTreeMap<(i64, i64, u8), BTreeMap<[u8; 8], SimTime>>,
}

impl Legacy12 {
    /// The suite with the given thresholds.
    #[must_use]
    pub fn new(params: DetectorParams) -> Self {
        let card = card(&params);
        Self {
            card,
            params,
            subjects: BTreeMap::new(),
            cells: BTreeMap::new(),
        }
    }

    /// The suite with the legacy thresholds.
    #[must_use]
    pub fn legacy_defaults() -> Self {
        Self::new(DetectorParams::default())
    }

    /// The thresholds it reads.
    #[must_use]
    pub fn params(&self) -> &DetectorParams {
        &self.params
    }

    /// How many distinct subjects it is tracking.
    #[must_use]
    pub fn tracked_subjects(&self) -> usize {
        self.subjects.len()
    }

    /// Records a claimed position in the co-location census and returns how many distinct
    /// certificates this receiver has heard in that cell within the window.
    ///
    /// The cell is `(⌊x/cell⌋, ⌊y/cell⌋, heading octant)`. Keying on heading too means
    /// crossing traffic converging at a junction is not mistaken for a Sybil, whose ghosts
    /// copy one position *and* one heading.
    fn census(&mut self, m: &ObservedMessage, t: SimTime) -> u32 {
        let cell = self.cell_of(m.claimed_x_m, m.claimed_y_m, m.claimed_heading_rad);
        let window = v2xw_core::time::secs_to_ns(self.params.sybil_window_s);
        let cutoff = t.saturating_sub(window);
        let entry = self.cells.entry(cell).or_default();
        entry.insert(m.signer, t);
        entry.retain(|_, last| *last >= cutoff);
        let n = entry.len();
        // Keep the census bounded: a cell nobody has claimed inside the window is dead.
        self.cells.retain(|_, v| !v.is_empty());
        u32::try_from(n).unwrap_or(u32::MAX)
    }

    fn cell_of(&self, x: f64, y: f64, heading_rad: f64) -> (i64, i64, u8) {
        let c = self.params.sybil_cell_m;
        let deg = crate::capability::wrap_heading(heading_rad).to_degrees();
        let octant = ((deg / 45.0).floor() as i64).rem_euclid(8) as u8;
        (math::grid_index(x, c), math::grid_index(y, c), octant)
    }

    /// The motion checks against a lagged reference — the legacy `detectors()`.
    fn motion(&self, r: Fix, m: &ObservedMessage, t: SimTime, f: &mut Fingerprint) {
        let p = &self.params;
        let dtt = ns_to_secs(t.saturating_sub(r.t)).max(1e-6);
        let disp = math::hypot(m.claimed_x_m - r.x, m.claimed_y_m - r.y);
        let tol = m
            .claimed_pos_confidence_m
            .max(p.tolerance_floor_factor * p.consistency_threshold_m);
        let avg_v = 0.5 * (m.claimed_speed_mps + r.speed);
        let jerk_slack = p.jerk_slack_factor * (m.claimed_speed_mps - r.speed).abs() * dtt;
        let zt = p.z_threshold * tol;
        f.set(
            DetectorId::PositionSpeedInconsistency,
            ((disp - avg_v * dtt).abs() - jerk_slack).max(0.0) / zt,
        );
        f.set(
            DetectorId::PositionJump,
            disp / (avg_v * dtt + zt + p.consistency_threshold_m),
        );
        f.set(
            DetectorId::ImplausibleAcceleration,
            ((m.claimed_speed_mps - r.speed).abs() / dtt) / p.max_accel_mps2,
        );
        if m.claimed_x_m == r.x
            && m.claimed_y_m == r.y
            && m.claimed_speed_mps > p.frozen_min_speed_mps
        {
            f.set(DetectorId::ConstantPositionFrozen, p.frozen_score);
            f.set(DetectorId::StaleOrReplay, p.frozen_stale_score);
        }
    }

    /// The lagged reference: the newest kept fix at least `detector_lag_s` old, else the
    /// oldest.
    fn reference(&self, history: &[Fix], t: SimTime) -> Fix {
        let lag = self.params.detector_lag_s;
        let mut r = history[0];
        for f in history {
            if ns_to_secs(t.saturating_sub(f.t)) >= lag {
                r = *f;
            } else {
                break;
            }
        }
        r
    }

    /// The event-message plausibility check, which is the whole of the DENM path.
    fn denm_verdict(&self, m: &ObservedMessage, event_type: &str, t: SimTime) -> Verdict {
        let mut f = Fingerprint::default();
        let mut fired = Vec::new();
        if m.verification.is_valid() && self.params.event_messages_in_play {
            let thresh = if event_type == "emergencyElectronicBrakeLight" {
                self.params.denm_brake_implausible_speed_mps()
            } else {
                self.params.denm_implausible_speed_mps
            };
            let score = m.claimed_speed_mps.max(0.0) / thresh;
            f.set(DetectorId::DenmPlausibility, score);
            if score >= 1.0 {
                fired.push(Observation {
                    detector: DetectorId::DenmPlausibility,
                    score,
                    subject: m.signer_hex(),
                    at: t,
                });
            }
        }
        Verdict {
            subject: m.signer_hex(),
            fingerprint: f,
            fired,
        }
    }
}

impl Model for Legacy12 {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl Detector for Legacy12 {
    fn on_message(
        &mut self,
        ctx: &mut dyn ThreatCtx,
        me: &SelfBelief,
        m: &ObservedMessage,
        env: &dyn LocalEnvironment,
    ) -> Verdict {
        // The receiver's own clock at reception, never the simulator's.
        let t = m.received_at;
        let subject = m.signer_hex();

        if let ObservedKind::Denm(event) = &m.kind {
            let v = self.denm_verdict(m, event, t);
            emit(ctx, me.node, &v);
            return v;
        }

        let p = self.params.clone();
        let mut f = Fingerprint::default();
        let fix = Fix {
            x: m.claimed_x_m,
            y: m.claimed_y_m,
            speed: m.claimed_speed_mps,
            heading: m.claimed_heading_rad,
            t,
        };
        let cell_count = self.census(m, t);

        let first = !self.subjects.contains_key(&m.signer);
        let reference = if first {
            fix
        } else {
            let history = &self.subjects[&m.signer].history;
            let r = self.reference(history, t);
            self.motion(r, m, t, &mut f);
            // The heading check runs over a ONE-STEP baseline, not the lagged reference:
            // a 1.5 s baseline spans a road turn and reads it as a heading lie.
            let prev = *history.last().unwrap_or(&r);
            let dprev = math::hypot(m.claimed_x_m - prev.x, m.claimed_y_m - prev.y);
            let gap_ok = ns_to_secs(t.saturating_sub(prev.t)) <= 2.0 * p.generation_interval_s;
            let disp_ok = dprev
                > p.heading_min_disp_m
                    .max(p.heading_conf_factor * m.claimed_pos_confidence_m);
            if m.claimed_speed_mps > p.heading_min_speed_mps && gap_ok && disp_ok {
                let bearing = bearing_rad(prev.x, prev.y, m.claimed_x_m, m.claimed_y_m);
                let d = angle_diff_rad(m.claimed_heading_rad, bearing).to_degrees();
                f.set(
                    DetectorId::HeadingInconsistency,
                    d / p.heading_threshold_deg,
                );
            }
            r
        };

        // Radio- and envelope-level checks: these need only what the receiver observed.
        f.set(
            DetectorId::SybilCoLocation,
            f64::from(cell_count) / f64::from(p.sybil_min_certs),
        );
        f.set(
            DetectorId::AcceptanceRangeThreshold,
            (math::hypot(m.claimed_x_m - me.x_m, m.claimed_y_m - me.y_m) - me.radio_range_m)
                .max(0.0)
                / p.art_max_m,
        );
        f.set(
            DetectorId::BeaconFrequency,
            f64::from(m.repetitions) / p.freq_max,
        );
        f.raise(
            DetectorId::StaleOrReplay,
            ns_to_secs(t.saturating_sub(m.claimed_generation_time)) / p.stale_max_s,
        );
        f.set(
            DetectorId::MapOffRoad,
            env.distance_to_road_m(m.claimed_x_m, m.claimed_y_m) / p.offroad_tol_m,
        );
        let slack = v2xw_core::time::secs_to_ns(p.cert_slack_s);
        if t > m.cert_valid_to.saturating_add(slack) || t.saturating_add(slack) < m.cert_valid_from
        {
            f.set(DetectorId::CertValidity, p.hard_fail_score);
        }

        // The soft constant-velocity (alpha-beta) tracker.
        {
            let st = self.subjects.entry(m.signer).or_default();
            match st.kf {
                None => st.kf = Some((fix.x, fix.y, 0.0, 0.0, t)),
                Some((ex, ey, evx, evy, et)) => {
                    let dtk = ns_to_secs(t.saturating_sub(et)).max(1e-3);
                    let (px, py) = (ex + evx * dtk, ey + evy * dtk);
                    let (rx, ry) = (fix.x - px, fix.y - py);
                    let denom = 2.0 * p.consistency_threshold_m + m.claimed_pos_confidence_m;
                    f.set(DetectorId::KalmanConsistency, math::hypot(rx, ry) / denom);
                    st.kf = Some((
                        px + p.kalman_alpha * rx,
                        py + p.kalman_alpha * ry,
                        evx + p.kalman_beta * rx / dtk,
                        evy + p.kalman_beta * ry / dtk,
                        t,
                    ));
                }
            }
        }

        if !m.verification.is_valid() {
            // The content of an unverifiable message is worthless, so the plausibility
            // checks are moot; the receiver reports the cryptographic failure itself.
            f = Fingerprint::default();
            f.set(DetectorId::SignatureVerification, p.hard_fail_score);
        } else if m.station_type == StationType::Vru {
            // A self-declared vulnerable road user legitimately travels off the road
            // centreline and moves slowly and erratically, so the map check and the
            // vehicle-kinematic checks would raise benign false positives. Suppress
            // exactly those — and close the hole the self-declaration opens with the two
            // impersonation arms.
            for k in DetectorId::MOTION {
                f.set(k, 0.0);
            }
            f.set(DetectorId::MapOffRoad, 0.0);
            if p.station_types_in_play {
                let speed_arm = m.claimed_speed_mps.max(0.0) / p.vru_max_plausible_speed_mps;
                let dtt = ns_to_secs(t.saturating_sub(reference.t)).max(p.generation_interval_s);
                let allow = p.vru_max_plausible_speed_mps * dtt
                    + p.z_threshold
                        * m.claimed_pos_confidence_m
                            .max(p.tolerance_floor_factor * p.consistency_threshold_m)
                    + p.gps_outlier_mag_m;
                let jump_arm =
                    math::hypot(m.claimed_x_m - reference.x, m.claimed_y_m - reference.y)
                        / allow.max(1e-6);
                f.set(DetectorId::VruImpersonation, speed_arm.max(jump_arm));
            }
        }

        // Streak gate, then the fired set in descending score — the legacy reason order.
        let st = self.subjects.entry(m.signer).or_default();
        let mut fired: Vec<Observation> = Vec::new();
        for d in DetectorId::ALL {
            if !d.is_hard() {
                continue;
            }
            if d == DetectorId::VruImpersonation && !p.station_types_in_play {
                continue;
            }
            if d == DetectorId::DenmPlausibility {
                continue;
            }
            let score = f.get(d);
            let run = st.streak.entry(d).or_insert(0);
            *run = if score >= 1.0 { *run + 1 } else { 0 };
            if *run >= p.min_consecutive {
                fired.push(Observation {
                    detector: d,
                    score,
                    subject: subject.clone(),
                    at: t,
                });
            }
        }
        fired.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then(a.detector.cmp(&b.detector))
        });

        st.history.push(fix);
        let keep_for =
            v2xw_core::time::secs_to_ns(p.detector_lag_s + 2.0 * p.generation_interval_s);
        while st.history.len() > 1 && t.saturating_sub(st.history[0].t) > keep_for {
            st.history.remove(0);
        }

        let v = Verdict {
            subject,
            fingerprint: f,
            fired,
        };
        emit(ctx, me.node, &v);
        v
    }

    fn cost(&self) -> DetectorCost {
        // Fifteen scalar checks and one tracker update per message. The number is a
        // placeholder until the node CPU model is calibrated; it is declared on the card
        // as todo-calibrate rather than presented as measured.
        DetectorCost {
            per_message_us: 20.0,
        }
    }
}

/// Writes a `det.observation` for each fired check.
fn emit(ctx: &mut dyn ThreatCtx, node: v2xw_core::ids::NodeId, v: &Verdict) {
    for o in &v.fired {
        ctx.emit(DetObservation::new(
            o.at,
            node,
            o.detector.as_str(),
            &o.subject,
            o.score,
        ));
    }
}

/// The model card for the suite.
#[must_use]
pub fn card(p: &DetectorParams) -> ModelCard {
    use serde_json::json;
    let mut card = ModelCard::new(
        MODEL_ID,
        Family::Detector,
        "1.0.0",
        "The legacy twelve local misbehaviour detectors, the two gated checks and the \
         soft constant-velocity tracker feature, with the legacy normalisation \
         (score ≈ 1 at the firing threshold) and the legacy streak gate.",
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "position/speed inconsistency",
            "max(0, |Δs − v̄·Δt| − 0.3·|Δv|·Δt) / (Z · max(conf, 0.5·c))",
        ),
        Equation::new("position jump", "Δs / (v̄·Δt + Z·max(conf, 0.5·c) + c)"),
        Equation::new("implausible acceleration", "(|Δv| / Δt) / a_max"),
        Equation::new(
            "heading inconsistency",
            "angle(heading_claimed, bearing(prev → now)) / heading_threshold",
        ),
        Equation::new("stale or replay", "(t_rx − t_generation) / stale_max"),
        Equation::new(
            "Sybil co-location",
            "distinct certs in cell / sybil_min_certs",
        ),
        Equation::new(
            "acceptance range",
            "max(0, |claimed − self| − range_rx) / art_max",
        ),
        Equation::new("beacon frequency", "messages per interval / freq_max"),
        Equation::new("map off-road", "distance to nearest lane / offroad_tol"),
        Equation::new(
            "alpha-beta tracker (soft)",
            "‖p_claimed − (p̂ + v̂·Δt)‖ / (2c + conf); p̂ ← p̂ + α·r, v̂ ← v̂ + β·r/Δt",
        ),
        Equation::new(
            "vulnerable-road-user impersonation",
            "max(v_claimed / v_vru_max, Δs / (v_vru_max·Δt + Z·max(conf, 0.5·c) + outlier))",
        ),
        Equation::new(
            "event plausibility",
            "v_claimed / (benign_max + 0.5 for a brake announcement, else implausible_max)",
        ),
    ];
    card.parameters = vec![
        legacy_param(
            "consistency_threshold_m",
            "m",
            json!(p.consistency_threshold_m),
            LEGACY_PY,
            "PipelineConfig.consistency_threshold_m",
        ),
        legacy_param(
            "heading_threshold_deg",
            "deg",
            json!(p.heading_threshold_deg),
            LEGACY_PY,
            "PipelineConfig.heading_threshold_deg",
        ),
        legacy_param(
            "detector_lag_s",
            "s",
            json!(p.detector_lag_s),
            LEGACY_PY,
            "PipelineConfig.detector_lag_s",
        ),
        legacy_param(
            "z_threshold",
            "-",
            json!(p.z_threshold),
            LEGACY_PY,
            "PipelineConfig.detector_z_threshold",
        ),
        legacy_param(
            "min_consecutive",
            "-",
            json!(p.min_consecutive),
            LEGACY_PY,
            "PipelineConfig.detector_min_consec",
        ),
        legacy_param(
            "sybil_min_certs",
            "-",
            json!(p.sybil_min_certs),
            LEGACY_PY,
            "_SYBIL_MIN",
        ),
        legacy_param(
            "sybil_cell_m",
            "m",
            json!(p.sybil_cell_m),
            LEGACY_PY,
            "_CELL_M",
        ),
        legacy_uncited(
            "sybil_window_s",
            "s",
            json!(p.sybil_window_s),
            LEGACY_PY,
            "the per-step co-location census (dt)",
            "the legacy engine counted co-location over one simulation step of a GLOBAL \
             broadcast census; the JVM reference uses a 1.5 s per-receiver window \
             (ScmsBeaconApp.java, `cd.values().removeIf(v -> t - v > 1.5)`). Measure the \
             window against the pseudonym-change interval, since a window longer than it \
             counts one honest vehicle's two pseudonyms as two identities.",
        ),
        legacy_uncited(
            "art_max_m",
            "m",
            json!(p.art_max_m),
            LEGACY_PY,
            "PipelineConfig.art_max_m",
            "derive from the receiver's own sensitivity and the propagation model rather \
             than from a round number: the tolerance should be the distance at which a \
             link plausibly still closes under favourable shadowing.",
        ),
        legacy_uncited(
            "offroad_tol_m",
            "m",
            json!(p.offroad_tol_m),
            LEGACY_PY,
            "PipelineConfig.offroad_tol_m",
            "measure against the lane-geometry error of the imported map plus the GNSS \
             error budget, so the tolerance is the map's accuracy and not a guess.",
        ),
        Parameter::new(
            "max_accel_mps2",
            "m/s^2",
            json!(p.max_accel_mps2),
            standard("ETSI TR 103 460 (F2MD acceleration-plausibility check)"),
        ),
        legacy_uncited(
            "freq_max",
            "msg/interval",
            json!(p.freq_max),
            LEGACY_PY,
            "PipelineConfig.freq_max",
            "derive from the CAM triggering rules (EN 302 637-2 §6.1.3: T_GenCamMin \
             0.1 s, T_GenCamMax 1 s) plus the DCC limit, which bound what a conforming \
             sender can emit; the legacy 6 predates that derivation.",
        ),
        legacy_uncited(
            "stale_max_s",
            "s",
            json!(p.stale_max_s),
            LEGACY_PY,
            "PipelineConfig.stale_max_s",
            "align with the IEEE 1609.2 generation-time acceptance window the node's own \
             envelope check uses, so the two cannot disagree.",
        ),
        legacy_param(
            "generation_interval_s",
            "s",
            json!(p.generation_interval_s),
            LEGACY_PY,
            "PipelineConfig.dt",
        ),
        legacy_param(
            "cert_slack_s",
            "s",
            json!(p.cert_slack_s),
            LEGACY_PY,
            "the detection pass (t > b['cvt'] + 1.0)",
        ),
        legacy_param(
            "hard_fail_score",
            "-",
            json!(p.hard_fail_score),
            LEGACY_PY,
            "the detection pass (1.5)",
        ),
        legacy_param(
            "frozen_score",
            "-",
            json!(p.frozen_score),
            LEGACY_PY,
            "detectors() (1.5)",
        ),
        legacy_param(
            "frozen_stale_score",
            "-",
            json!(p.frozen_stale_score),
            LEGACY_PY,
            "detectors() (1.2)",
        ),
        legacy_param(
            "frozen_min_speed_mps",
            "m/s",
            json!(p.frozen_min_speed_mps),
            LEGACY_PY,
            "detectors() (cs > 0.5)",
        ),
        legacy_param(
            "jerk_slack_factor",
            "-",
            json!(p.jerk_slack_factor),
            LEGACY_PY,
            "detectors() (0.3)",
        ),
        legacy_param(
            "tolerance_floor_factor",
            "-",
            json!(p.tolerance_floor_factor),
            LEGACY_PY,
            "detectors() (0.5 * consistency_threshold_m)",
        ),
        legacy_param(
            "heading_min_speed_mps",
            "m/s",
            json!(p.heading_min_speed_mps),
            LEGACY_PY,
            "the detection pass (cs > 3.0)",
        ),
        legacy_param(
            "heading_min_disp_m",
            "m",
            json!(p.heading_min_disp_m),
            LEGACY_PY,
            "the detection pass (max(5.0, 2.5 * conf))",
        ),
        legacy_param(
            "heading_conf_factor",
            "-",
            json!(p.heading_conf_factor),
            LEGACY_PY,
            "the detection pass (2.5 * conf)",
        ),
        legacy_param(
            "kalman_alpha",
            "-",
            json!(p.kalman_alpha),
            LEGACY_JVM,
            "KF_ALPHA",
        ),
        legacy_param(
            "kalman_beta",
            "-",
            json!(p.kalman_beta),
            LEGACY_JVM,
            "KF_BETA",
        ),
        legacy_param(
            "vru_max_plausible_speed_mps",
            "m/s",
            json!(p.vru_max_plausible_speed_mps),
            LEGACY_PY,
            "VRU_MAX_PLAUSIBLE_SPEED_MPS",
        ),
        legacy_param(
            "gps_outlier_mag_m",
            "m",
            json!(p.gps_outlier_mag_m),
            LEGACY_PY,
            "PipelineConfig.gps_outlier_mag_m",
        ),
        legacy_param(
            "denm_implausible_speed_mps",
            "m/s",
            json!(p.denm_implausible_speed_mps),
            LEGACY_PY,
            "DENM_IMPLAUSIBLE_SPEED_MPS",
        ),
        legacy_param(
            "denm_benign_max_speed_mps",
            "m/s",
            json!(p.denm_benign_max_speed_mps),
            LEGACY_PY,
            "DENM_BENIGN_MAX_SPEED_MPS",
        ),
        legacy_param(
            "denm_brake_margin_mps",
            "m/s",
            json!(p.denm_brake_margin_mps),
            LEGACY_PY,
            "DENM_BRAKE_IMPLAUSIBLE_SPEED_MPS",
        ),
    ];
    card.sources = vec![
        legacy(LEGACY_PY, "detectors() + the detection pass"),
        legacy(LEGACY_JVM, "onCam (the alpha-beta tracker and its gate)"),
        design("07-threats-and-detection.md §3.1"),
        standard("ETSI TS 103 759 (misbehaviour observation classes 1–5)"),
    ];
    card.assumptions = vec![
        "Every input is belief: the node's own position estimate, its own clock, the \
         messages it verified and its own map store (invariant I-T2)."
            .to_string(),
        "The station type on a received beacon is self-declared, so the suppressions it \
         buys are paired with the two impersonation arms that close the hole."
            .to_string(),
        "A score of ≈ 1 is the firing threshold for every check, which is what makes the \
         fingerprint comparable across checks with different units."
            .to_string(),
    ];
    card.limitations = vec![
        "The Sybil co-location census is per receiver. The legacy engine counted a global \
         per-step census over every broadcast, which no single receiver has; recall on \
         geographically spread ghosts is therefore lower here than in the legacy corpus."
            .to_string(),
        "kalmanConsistency never fires on its own: a constant-velocity tracker \
         false-positives on a sustained curve."
            .to_string(),
        "Perception cross-check and CPM consistency (07-threats-and-detection.md §3.1) \
         are not implemented: they need the perception model's Detection type, which does \
         not exist yet. Left as a documented hook."
            .to_string(),
        "The per-message CPU cost is a placeholder, not a measurement.".to_string(),
    ];
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![legacy(LEGACY_PY, "detectors()")],
        tests: vec![
            "legacy_conformance::thresholds_match_the_legacy_source".to_string(),
            "detectors::each_detector_fires_on_its_own_attack".to_string(),
            "detectors::benign_traffic_stays_quiet".to_string(),
        ],
    };
    card.cost = Some(v2xw_core::card::CostClass {
        per_call_us: Some(20.0),
        notes: Some(
            "Placeholder: fifteen scalar checks and one tracker update. Not measured.".to_string(),
        ),
    });
    card
}
