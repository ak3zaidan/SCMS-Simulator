//! The vulnerable-road-user device: what makes a pedestrian audible on the channel.
//!
//! `v2xw-mobility` already walks pedestrians and cyclists with a social-force model
//! (04-models.md §2.5). What that produces is a *body* moving through the world. This
//! module is the **device** the body is carrying: a handset or a pedestrian beacon that
//! transmits a personal safety message (SAE J2735 PSM, per J2945/9) or a VRU awareness
//! message (ETSI TS 103 300-3 VAM), under the two constraints a hand-held radio has and a
//! roof-mounted one does not.
//!
//! # The constraints are the point
//!
//! A VRU device is not a small OBU. Two limits decide whether a pedestrian is detectable
//! at all, and neither applies to a vehicle:
//!
//! **Duty cycle.** EN 302 571 V2.1.1 §4.2.10.2 caps any ITS-G5 transmitter at a 3 % duty
//! cycle with `T_on ≤ 4 ms` and `T_off ≥ 25 ms`, whatever the congestion-control algorithm
//! decides (04-models.md §6). [`DutyCycle`] enforces exactly that, over the same one-second
//! observation window `v2xw-radio`'s DCC models use, and it computes the air time from
//! [`v2xw_radio::air_time`] — the OFDM parameters, not an assumed rate. At the VAM's
//! fastest permitted cadence the cap is not binding; in a burst, or with a large payload,
//! it is, and [`VruDeviceRuntime::duty_cycle_refusals`] counts what it stopped.
//!
//! **Energy.** A vehicle's radio is powered by the vehicle. A handset's is powered by a
//! battery whose capacity no source in the design set gives, and 06-node-models.md §7
//! publishes no VRU-device profile at all: `power_w` on
//! `vru-device/handset-generic` is `NOT PUBLISHED` with the measurement that would publish
//! it. So [`PowerBudget`] is **declared and inert** by default: it accounts the energy
//! every transmission costs, from the transmit power and the real air time, and it refuses
//! nothing until a scenario supplies a capacity. That is the honest shape — a default
//! battery would silently set the transmit budget of every run that forgot to configure
//! one — and it is why [`PowerBudget::battery_j`] is an `Option`.
//!
//! # What is transmitted
//!
//! | Message | Cadence | Payload length | Status |
//! |---|---|---|---|
//! | PSM | [`PsmGenParams`], default 1 Hz | the validated J2735 size model of 04-models.md §8.4 | rate `TODO: calibrate`, size model `derived-no-anchor` |
//! | VAM | [`vam_gen_params`], the TS 103 300-3 §6.2 triggers | the validated ETSI size model of `codec/size-model/etsi` | triggers VERIFIED, size `literature-checked` against four published figures |
//!
//! Neither payload is real wire bytes, and [`PayloadProvenance`] says so on every frame —
//! including, through `superseded_by`, whether a real encoder has since appeared that this
//! runtime is not yet wired to. Both lengths come from a size model in `v2xw-msg` rather
//! than from a constant here, so a row that is recalibrated or retired moves this device
//! with it instead of leaving a stale number behind.
//!
//! The **security envelope around the payload is real**: the SPDU is built and signed by
//! [`crate::secure::NodeSecurity`] exactly as a vehicle's is, so the signing cost, the
//! certificate-attachment cadence and the envelope overhead are the real ones even where
//! the payload is not. That is also why the payload is the size model's *payload* figure
//! and not the 235–350 B secured-message figures its rows are anchored against: the
//! envelope those figures include is one this runtime really builds.
//!
//! # What it reads
//!
//! Its own belief and its own clock, like every other runtime here. The VAM triggers are
//! evaluated against a [`PositionEstimate`], so a pedestrian with a poor urban fix
//! transmits at a different rate from one with a clear sky — which is the phenomenon, not
//! an artefact.

use std::collections::BTreeMap;

use v2xw_core::belief::PositionEstimate;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::geo::GeoOrigin;
use v2xw_core::geom::Dims;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::nodeview::NodeView;
use v2xw_core::time::{Duration, SimTime, WallClock};
use v2xw_msg::MsgType;
use v2xw_msg::codec::PLACEHOLDER_FILL;
use v2xw_msg::generator::{
    CamDynamics, CamGenParams, CamTriggerState, DccState, GenReason, GenRequest,
};
// `SizeModelVersion` lives in `v2xw_msg::codec` and `ContentProfile` in
// `v2xw_msg::size_model`; neither is re-exported by the other, so both are named where
// they are defined.
use v2xw_msg::codec::SizeModelVersion;
use v2xw_msg::size_model::ContentProfile;
use v2xw_record::wire::telemetry::NodeTelemetry;

use crate::clock::ClockModel;
use crate::ctx::{NodeCtx, NodeCtxExt};
use crate::policy::{
    PolicyView, RxSummary, VerificationPolicy, VerifyAll, VerifyDecision, VerifyDecisionRecord,
};
use crate::profile::{HardwareProfile, RunsOn};
use crate::queue::{Admission, DropCause, DropLedger, NodeQueue, QueueKind, Queued};
use crate::runtime::{RxDisposition, RxFrame, RxReport, RxStamp, Transmission, VerifiedMessage};
use crate::secure::{CryptoMode, NodeSecurity, PSID_SAFETY, SpduVerdict};
use crate::server::{OpDescriptor, ProfileServiceModel, ServerBank, ServiceModel};
use crate::stores::{
    CredentialHandle, Neighbor, NeighborTable, PeerCertCache, Stores, VerificationState,
};
use crate::telemetry::{NodeState, TelemetryInputs, TelemetryWindow, gnss_fix_code};

/// Model id of the VRU device runtime.
///
/// Registered under [`v2xw_core::card::Family::Generator`], not `HardwareProfile`: what a
/// scenario selects this model for is *when the device transmits* — the TS 103 300-3
/// triggers, the PSM cadence, the EN 302 571 duty cycle and the energy budget — and the
/// `Family` enum is closed on purpose (ADR 0007), so the honest fit is the family whose
/// definition is "message generation rules". The hardware it reads its costs from is a
/// separate model with its own card, `vru-device/handset-generic`.
pub const VRU_DEVICE_ID: &str = "node/vru-device";

/// What kind of device a vulnerable road user is carrying.
///
/// The distinction is not cosmetic: a beacon exists only to be heard, so it has no
/// receiver to spend energy on and no application to warn anybody with, while a handset
/// has both. It changes the defaults of [`VruConfig`] and it is recorded on the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum VruDeviceKind {
    /// A smartphone running a V2X application: it transmits and it receives, and its
    /// battery is shared with everything else the phone is doing.
    Handset,
    /// A dedicated beacon or tag — a cyclist's transponder, a school-crossing marker, a
    /// road worker's tag. Transmit-only, so its whole energy budget is the radio's.
    Beacon,
}

impl VruDeviceKind {
    /// The stable name a card and a scenario spell it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VruDeviceKind::Handset => "handset",
            VruDeviceKind::Beacon => "beacon",
        }
    }

    /// Whether this kind of device has a receiver at all.
    #[must_use]
    pub const fn receives(self) -> bool {
        matches!(self, VruDeviceKind::Handset)
    }
}

/// Which awareness services a VRU device runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VruServices {
    /// Transmit personal safety messages (SAE J2945/9 over DSRC/1609).
    pub psm: bool,
    /// Transmit VRU awareness messages (ETSI TS 103 300-3).
    pub vam: bool,
}

impl VruServices {
    /// The SAE stack: PSM only.
    pub const SAE: VruServices = VruServices {
        psm: true,
        vam: false,
    };
    /// The ETSI stack: VAM only.
    pub const ETSI: VruServices = VruServices {
        psm: false,
        vam: true,
    };
    /// Both, for a device carrying two stacks.
    pub const BOTH: VruServices = VruServices {
        psm: true,
        vam: true,
    };
    /// Neither: a device that listens and says nothing.
    pub const NONE: VruServices = VruServices {
        psm: false,
        vam: false,
    };
}

// =========================================================================================
// Payload provenance
// =========================================================================================

/// Where a VRU payload's length came from — and therefore what its bytes mean.
///
/// The same discipline `v2xw-msg`'s [`v2xw_msg::codec::SizeSource`] applies to a message
/// and `v2xw-proto`'s `SizeProvenance` applies to a protocol message, applied here because
/// **neither** of this device's two messages has a real encoder and a consumer must not be
/// able to mistake a modelled length for a measured one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadProvenance {
    /// A validated size model produced the length: exact to the model, placeholder bytes.
    /// `model` is the model card's id, so a manifest can resolve the table and its anchors.
    SizeModel {
        /// The size model's registry id.
        model: &'static str,
        /// Its version, which is what makes a recorded length interpretable.
        version: SizeModelVersion,
        /// The model id of the real encoder that has retired this row, if one has —
        /// `v2xw_msg::size_model::SizeEntry::superseded_by`. `Some` means a better answer
        /// exists and this runtime is not yet wired to it.
        superseded_by: Option<&'static str>,
    },
    /// A real encoder produced the bytes: the PSM, from `v2xw_msg::j2735::psm`. The VAM
    /// moves here when its encoder lands.
    Encoded {
        /// The codec's registry id.
        codec: &'static str,
    },
}

impl PayloadProvenance {
    /// Whether the bytes are real wire bytes a decoder could read back.
    ///
    /// Stated rather than assumed, so that a metric which counted ones in a placeholder
    /// cannot claim it was measuring a message.
    #[must_use]
    pub const fn is_real(self) -> bool {
        matches!(self, PayloadProvenance::Encoded { .. })
    }
}

/// The id of the size model the PSM's length comes from.
pub const PSM_SIZE_MODEL: &str = v2xw_msg::size_model::J2735_SIZE_MODEL_ID;

/// The id of the size model the VAM's length comes from.
///
/// `codec/size-model/etsi` holds the VAM rows, and its own card records the plan for
/// retiring them: commit `VAM-PDU-Descriptions.asn` (TS 103 300-3) and generate the real
/// encoder. Its rows are checked against four published whole-message sizes — 235 B
/// (C2C-CC), 300 B (arXiv 2506.22052) and 350 B (TR 2050 Fig. 12, with 5GAA stating the
/// same) — all at secured-message scope, which is why the row's own number is a *payload*
/// and the envelope this runtime really builds is added on top of it rather than included
/// in it.
pub const VAM_SIZE_MODEL: &str = v2xw_msg::etsi_size::ETSI_SIZE_MODEL_ID;

/// The modelled payload length for one message, with the provenance of the number.
///
/// `None` when no table has a row for the type, which is a real condition and not a
/// defensive branch: a size model is retired when its encoder lands, and a runtime that
/// invented a length when the row went away would be the exact defect rule H1 exists to
/// stop. The caller suppresses the transmission and records `no-payload`.
///
/// `elements` is the message's variable-element count — path-history points for both of
/// these two — and `None` takes the row's own `nominal_elements`, so a scenario that says
/// nothing transmits the message the size model was validated at.
#[must_use]
pub fn modelled_payload(
    msg_type: MsgType,
    profile: ContentProfile,
    elements: Option<u32>,
) -> Option<(u32, PayloadProvenance)> {
    let (entry, model, version) = match msg_type {
        MsgType::Psm => (
            v2xw_msg::size_model::lookup(msg_type, profile)?,
            PSM_SIZE_MODEL,
            v2xw_msg::size_model::VERSION,
        ),
        MsgType::Vam => (
            v2xw_msg::etsi_size::lookup(msg_type, profile)?,
            VAM_SIZE_MODEL,
            v2xw_msg::etsi_size::ETSI_VERSION,
        ),
        _ => return None,
    };
    let n = elements.unwrap_or(entry.nominal_elements);
    Some((
        entry.bytes(n),
        PayloadProvenance::SizeModel {
            model,
            version,
            superseded_by: entry.superseded_by,
        },
    ))
}

// =========================================================================================
// Duty cycle
// =========================================================================================

/// Why a transmission was refused, or that it was allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DutyVerdict {
    /// Allowed; the frame occupies `airtime` on the channel.
    Transmit {
        /// The air time [`v2xw_radio::air_time`] computed for the frame.
        airtime: Duration,
    },
    /// Refused: `T_on` exceeds the 4 ms EN 302 571 admits for a single frame. A payload
    /// this long cannot be transmitted conformantly at this modulation at all, and
    /// fragmenting it is the network layer's decision, not the device's.
    FrameTooLong {
        /// The air time the frame would have taken.
        airtime: Duration,
        /// The largest conformant `T_on`.
        limit: Duration,
    },
    /// Refused: the 3 % duty cycle over the last second is already spent.
    BudgetSpent {
        /// Air time used in the window, nanoseconds.
        used_ns: u64,
        /// The cap, nanoseconds.
        cap_ns: u64,
    },
    /// Refused: `T_off` since the last transmission has not elapsed.
    TooSoon {
        /// The earliest instant a transmission is permitted.
        until: SimTime,
    },
}

impl DutyVerdict {
    /// Whether the frame may go out.
    #[must_use]
    pub const fn is_transmit(self) -> bool {
        matches!(self, DutyVerdict::Transmit { .. })
    }

    /// The air time, for the verdicts that computed one.
    #[must_use]
    pub const fn airtime(self) -> Option<Duration> {
        match self {
            DutyVerdict::Transmit { airtime } | DutyVerdict::FrameTooLong { airtime, .. } => {
                Some(airtime)
            }
            _ => None,
        }
    }
}

/// The EN 302 571 duty-cycle constraint, over a one-second observation window.
///
/// # Why the window lives here
///
/// `v2xw-radio`'s DCC models keep a window of exactly this shape, and it is private to
/// that crate. It is reimplemented here — thirty lines, and the same one-second window —
/// rather than exported, because a VRU device's duty cycle is a property of the *device*:
/// it applies whether or not the scenario runs a congestion-control model, and a device
/// whose duty-cycle gate only worked when `SaeJ2945Dcc` happened to be installed would be
/// silently unconstrained in every abstract-tier run. The *limits* are not reimplemented:
/// [`v2xw_radio::En302571Floor`] is the one definition of `T_on_max`, the 3 % cap,
/// `T_off_min` and the load-dependent `T_off`, and this type holds one.
#[derive(Debug, Clone)]
pub struct DutyCycle {
    floor: v2xw_radio::En302571Floor,
    mcs: v2xw_radio::Mcs,
    /// `(instant, air time ns)` inside the window, in time order.
    sends: Vec<(SimTime, u64)>,
    last_send: Option<SimTime>,
    airtime_ns: u64,
    refused_too_long: u64,
    refused_budget: u64,
    refused_too_soon: u64,
    allowed: u64,
}

impl Default for DutyCycle {
    fn default() -> DutyCycle {
        DutyCycle::conformant()
    }
}

impl DutyCycle {
    /// The observation window the 3 % cap is measured over: one second.
    ///
    /// EN 302 571 §4.2.10.2 defines the duty cycle as the ratio of transmitter-on time to
    /// a one-second period, which is also the window `v2xw-radio`'s DCC models use, so the
    /// two agree by construction rather than by coincidence.
    pub const WINDOW: Duration = Duration::from_secs(1);

    /// The constraint as the standard defines it.
    #[must_use]
    pub fn conformant() -> DutyCycle {
        DutyCycle::new(
            v2xw_radio::En302571Floor::CONFORMANT,
            // 6 Mbit/s QPSK 1/2, which `v2xw_radio::Mcs::R6Qpsk12` documents as "the
            // default safety-channel rate". A device transmitting at 3 Mbit/s spends twice
            // the air time on the same payload, which is why the modulation is a field and
            // not a constant.
            v2xw_radio::Mcs::R6Qpsk12,
        )
    }

    /// The constraint with a chosen floor and modulation.
    #[must_use]
    pub fn new(floor: v2xw_radio::En302571Floor, mcs: v2xw_radio::Mcs) -> DutyCycle {
        DutyCycle {
            floor,
            mcs,
            sends: Vec::new(),
            last_send: None,
            airtime_ns: 0,
            refused_too_long: 0,
            refused_budget: 0,
            refused_too_soon: 0,
            allowed: 0,
        }
    }

    /// The limits in force.
    #[must_use]
    pub fn floor(&self) -> &v2xw_radio::En302571Floor {
        &self.floor
    }

    /// The modulation air time is computed at.
    #[must_use]
    pub fn mcs(&self) -> v2xw_radio::Mcs {
        self.mcs
    }

    /// The air time a frame of `bytes` would occupy, from the OFDM parameters.
    #[must_use]
    pub fn airtime_of(&self, bytes: u32) -> Duration {
        v2xw_radio::air_time(bytes, self.mcs)
    }

    /// Whether a transmission at `then` is inside the one-second window ending at `now`.
    ///
    /// Written as an age test rather than as a comparison against `now − WINDOW`, because
    /// the subtraction saturates at zero: with a cutoff of `now.saturating_sub(WINDOW)` and
    /// a strict `>`, every send at `t = 0` fell out of the window for the whole first
    /// second of a run, so the very first frame of a burst was never counted against the
    /// 3 % cap and `ratio` under-reported by one frame.
    fn in_window(now: SimTime, then: SimTime) -> bool {
        now.saturating_sub(then) < DutyCycle::WINDOW.as_nanos()
    }

    /// The air time used inside the window ending at `now`, nanoseconds.
    #[must_use]
    pub fn used_ns(&self, now: SimTime) -> u64 {
        self.sends
            .iter()
            .filter(|(t, _)| DutyCycle::in_window(now, *t))
            .map(|(_, d)| *d)
            .sum()
    }

    /// The duty cycle over the window ending at `now`, as a fraction.
    #[must_use]
    pub fn ratio(&self, now: SimTime) -> f64 {
        self.used_ns(now) as f64 / DutyCycle::WINDOW.as_nanos() as f64
    }

    /// Total air time this device has put on the channel, nanoseconds.
    #[must_use]
    pub const fn airtime_ns(&self) -> u64 {
        self.airtime_ns
    }

    /// How many frames were refused for exceeding `T_on`.
    #[must_use]
    pub const fn refused_too_long(&self) -> u64 {
        self.refused_too_long
    }

    /// How many frames were refused because the 3 % window was spent.
    #[must_use]
    pub const fn refused_budget(&self) -> u64 {
        self.refused_budget
    }

    /// How many frames were refused because `T_off` had not elapsed.
    #[must_use]
    pub const fn refused_too_soon(&self) -> u64 {
        self.refused_too_soon
    }

    /// How many frames it allowed.
    #[must_use]
    pub const fn allowed(&self) -> u64 {
        self.allowed
    }

    /// Whether a frame of `bytes` may be transmitted at `now`, and at what cost in air
    /// time.
    ///
    /// The three tests are the three EN 302 571 §4.2.10.2 imposes, in the order the clause
    /// imposes them: the frame's own length, the enforced idle time since the last
    /// transmission, and the duty cycle over the window. `cbr` is the measured channel
    /// busy ratio when the device measures one; it only ever *lengthens* `T_off`
    /// ([`v2xw_radio::En302571Floor::t_off`]), so a device that measures nothing is
    /// constrained by the unconditional 25 ms and not by less.
    ///
    /// Refusals are counted but nothing is recorded here: the caller knows which message
    /// it was and emits the record.
    pub fn gate(&mut self, now: SimTime, bytes: u32, cbr: Option<f64>) -> DutyVerdict {
        let airtime = self.airtime_of(bytes);
        if !self.floor.admits(airtime) {
            self.refused_too_long = self.refused_too_long.saturating_add(1);
            return DutyVerdict::FrameTooLong {
                airtime,
                limit: self.floor.t_on_max,
            };
        }
        if let Some(last) = self.last_send {
            let t_off = self.floor.t_off(airtime, cbr.unwrap_or(0.0));
            let earliest = t_off.after(last);
            if now < earliest {
                self.refused_too_soon = self.refused_too_soon.saturating_add(1);
                return DutyVerdict::TooSoon { until: earliest };
            }
        }
        let cap_ns = (DutyCycle::WINDOW.as_nanos() as f64 * self.floor.duty_cycle_max) as u64;
        let used_ns = self.used_ns(now);
        if self.floor.enforced && used_ns.saturating_add(airtime.as_nanos()) > cap_ns {
            self.refused_budget = self.refused_budget.saturating_add(1);
            return DutyVerdict::BudgetSpent { used_ns, cap_ns };
        }
        self.note(now, airtime);
        DutyVerdict::Transmit { airtime }
    }

    /// Records a transmission the caller went ahead with.
    fn note(&mut self, at: SimTime, airtime: Duration) {
        self.sends.push((at, airtime.as_nanos()));
        self.sends.retain(|(t, _)| DutyCycle::in_window(at, *t));
        self.last_send = Some(at);
        self.airtime_ns = self.airtime_ns.saturating_add(airtime.as_nanos());
        self.allowed = self.allowed.saturating_add(1);
    }
}

// =========================================================================================
// Energy
// =========================================================================================

/// The energy one transmission costs and how much is left.
///
/// # Inert by default, and why
///
/// [`PowerBudget::battery_j`] is `None` unless a scenario sets it. 06-node-models.md §7
/// publishes no VRU-device profile and `vru-device/handset-generic`'s `power_w` is
/// `NOT PUBLISHED`, so there is no capacity to default to. The budget still *accounts*
/// every transmission — the radiated energy is the transmit power times the real air time,
/// both of which are known — so a run reports what a battery would have lost even when no
/// battery is configured. It just refuses nothing.
///
/// A default capacity would be worse than no capacity: it would set the transmit budget of
/// every run that forgot to configure one, and it would do it invisibly, because a handset
/// going quiet after twenty minutes looks like a modelling result rather than like a
/// made-up number.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerBudget {
    /// Usable energy, joules. `None` means the constraint is accounted and not enforced.
    pub battery_j: Option<f64>,
    /// Radiated transmit power, dBm.
    pub tx_power_dbm: f64,
    /// Power-amplifier efficiency: radiated power divided by power drawn from the battery,
    /// in `(0, 1]`.
    pub pa_efficiency: f64,
    /// Energy the device spends per transmission outside the amplifier — building,
    /// signing and framing the message — joules. `None` means it is not modelled and only
    /// the radiated part is accounted.
    pub per_message_j: Option<f64>,
    /// Energy spent so far, joules.
    spent_j: f64,
    /// Transmissions refused for want of energy.
    refused: u64,
}

impl PowerBudget {
    /// A budget that accounts and does not enforce, at `tx_power_dbm`.
    #[must_use]
    pub fn accounting_only(tx_power_dbm: f64) -> PowerBudget {
        PowerBudget {
            battery_j: None,
            tx_power_dbm,
            pa_efficiency: DEFAULT_PA_EFFICIENCY,
            per_message_j: None,
            spent_j: 0.0,
            refused: 0,
        }
    }

    /// The same budget with a capacity, which makes it enforcing.
    #[must_use]
    pub fn with_battery_j(mut self, joules: f64) -> PowerBudget {
        self.battery_j = Some(joules);
        self
    }

    /// The radiated power in watts: `10^((dBm − 30)/10)`.
    ///
    /// Through [`v2xw_core::math::pow`], never `f64::powf`, so the value is bit-identical
    /// on every platform (ADR 0003).
    #[must_use]
    pub fn radiated_w(&self) -> f64 {
        math::pow(10.0, (self.tx_power_dbm - 30.0) / 10.0)
    }

    /// The energy one frame of `airtime` costs the battery, joules.
    #[must_use]
    pub fn energy_of(&self, airtime: Duration) -> f64 {
        let eff = if self.pa_efficiency > 0.0 && self.pa_efficiency <= 1.0 {
            self.pa_efficiency
        } else {
            1.0
        };
        let radio = self.radiated_w() * airtime.as_secs_f64() / eff;
        radio + self.per_message_j.unwrap_or(0.0)
    }

    /// Energy spent so far, joules.
    #[must_use]
    pub fn spent_j(&self) -> f64 {
        self.spent_j
    }

    /// Energy left, joules, for an enforcing budget.
    #[must_use]
    pub fn remaining_j(&self) -> Option<f64> {
        self.battery_j.map(|b| (b - self.spent_j).max(0.0))
    }

    /// Whether the budget is exhausted. Always `false` when it is not enforcing.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.remaining_j().is_some_and(|r| r <= 0.0)
    }

    /// How many transmissions were refused for want of energy.
    #[must_use]
    pub const fn refusals(&self) -> u64 {
        self.refused
    }

    /// Spends the energy one frame costs. `false` when an enforcing budget cannot pay,
    /// in which case nothing is spent and the refusal is counted.
    pub fn spend(&mut self, airtime: Duration) -> bool {
        let cost = self.energy_of(airtime);
        if let Some(battery) = self.battery_j
            && self.spent_j + cost > battery
        {
            self.refused = self.refused.saturating_add(1);
            return false;
        }
        self.spent_j += cost;
        true
    }
}

/// The power-amplifier efficiency the budget assumes when a scenario sets none.
///
/// **Uncalibrated.** No efficiency figure for a handset ITS-G5 or C-V2X front end appears
/// in R7 or R5, and `vru-device/handset-generic`'s `power_w` is `NOT PUBLISHED`. 0.35 is
/// carried as a starting value with a plan on [`VRU_DEVICE_ID`]'s card; it scales the
/// energy per message linearly, so a study that depends on the battery must vary it and
/// report the sensitivity. It cannot make an unconfigured run wrong, because an
/// unconfigured budget enforces nothing.
pub const DEFAULT_PA_EFFICIENCY: f64 = 0.35;

// =========================================================================================
// Generation
// =========================================================================================

/// The TS 103 300-3 §6.2 parameters, expressed in the CAM trigger machinery.
///
/// # Why the CAM state machine and not a second one
///
/// TS 103 300-3's VRU Basic Service is the CA Basic Service's shape with different bounds:
/// the individual-VAM triggers 04-models.md §8.1 lists — "elapsed > `T_GenVamMax`,
/// position change > 4 m, speed change > 0.5 m/s, velocity-orientation change > 4°" — are
/// EN 302 637-2's three dynamics thresholds with `T_GenVamMin` 100 ms and `T_GenVamMax`
/// 5,000 ms in place of 100 ms and 1,000 ms, and a low-frequency container "first, then
/// every ≥ 2,000 ms" in place of every ≥ 500 ms. So
/// [`v2xw_msg::generator::CamTriggerState`] is the state machine, parameterised. Writing a
/// second copy of the same rules would be the way the two answers to "was a message due"
/// come to disagree.
///
/// # The two rules this does *not* reproduce
///
/// Both are on the card as limitations rather than approximated:
///
/// * the **trajectory-interception** trigger ("probability change > 10 %"), which needs the
///   trajectory-interception indication a VAM carries and this crate does not build;
/// * **redundancy mitigation** and **clustering** (§6.5.3), which skip a VAM when a peer is
///   already reporting the same VRU. Both *reduce* the rate, so the modelled device
///   transmits at least as often as a conformant one — the pessimistic direction for
///   channel load and for battery, and the optimistic one for detectability, which the
///   card states.
///
/// `N_GenCam` has no TS 103 300-3 counterpart: `n_gen_cam` is 1 here, which makes the
/// shortened interval after a dynamics-triggered VAM apply to exactly one message. That is
/// the minimal reading of a clause that does not exist, and it is a card parameter.
#[must_use]
pub fn vam_gen_params() -> CamGenParams {
    CamGenParams {
        t_gen_cam_min: Duration::from_millis(100),
        t_gen_cam_max: Duration::from_millis(5_000),
        t_check_cam_gen: Duration::from_millis(100),
        n_gen_cam: 1,
        heading_threshold_rad: CamGenParams::FOUR_DEGREES_RAD,
        position_threshold_m: 4.0,
        speed_threshold_mps: 0.5,
        low_frequency_interval: Duration::from_millis(2_000),
    }
}

/// `T_AssembleVAM`, the time TS 103 300-3 §6.2 allows for assembling a VAM: 50 ms.
///
/// Charged as the message's construction cost when the profile costs an application task,
/// and recorded on the card whether or not it is charged, because it bounds how fast a
/// device can react to a dynamics trigger.
pub const T_ASSEMBLE_VAM: Duration = Duration::from_millis(50);

/// The PSM cadence.
///
/// 04-models.md §8.1's `generator/psm-j2945-9` row: "scope only VERIFIED (secondary);
/// rate rules UNVERIFIED (a patent claims 2-5 per s by speed; PASS used 100 ms); default
/// 1 Hz `TODO: calibrate` (plan: SAE J2945/9 §6 primary text)". The default is the design
/// set's own default and the plan is the design set's own plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsmGenParams {
    /// The nominal interval between two personal safety messages.
    pub interval: Duration,
    /// The floor a DCC-stretched interval is clamped up from.
    pub min_interval: Duration,
}

impl PsmGenParams {
    /// The design set's default: 1 Hz, floored at the 100 ms the PASS field trial used.
    #[must_use]
    pub const fn j2945_9() -> PsmGenParams {
        PsmGenParams {
            interval: Duration::from_secs(1),
            min_interval: Duration::from_millis(100),
        }
    }
}

impl Default for PsmGenParams {
    fn default() -> PsmGenParams {
        PsmGenParams::j2945_9()
    }
}

/// The PSM timer: a periodic cadence with a DCC floor and J2735's own `msgCnt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsmTimer {
    params: PsmGenParams,
    last: Option<SimTime>,
    msg_count: u8,
}

impl PsmTimer {
    /// A timer with `params`.
    #[must_use]
    pub fn new(params: PsmGenParams) -> PsmTimer {
        PsmTimer {
            params,
            last: None,
            msg_count: 0,
        }
    }

    /// The parameters in force.
    #[must_use]
    pub fn params(&self) -> &PsmGenParams {
        &self.params
    }

    /// The `msgCnt` the next PSM carries: `MsgCount ::= INTEGER (0..127)`, incremented per
    /// message and wrapping, exactly as the BSM generator does it.
    #[must_use]
    pub const fn msg_count(&self) -> u8 {
        self.msg_count
    }

    /// When the last PSM was asked for, on the device's own clock.
    #[must_use]
    pub const fn last_tx(&self) -> Option<SimTime> {
        self.last
    }

    /// Whether a PSM is due at `now`.
    ///
    /// `now` is the device's **believed** time: every interval is measured on the sender's
    /// own clock, so a device whose clock has been stepped transmits at a visibly wrong
    /// rate rather than at a correct one.
    pub fn check(&mut self, now: SimTime, dcc: &DccState) -> Option<GenReason> {
        let reason = match self.last {
            None => GenReason::First,
            Some(last) => {
                let elapsed = Duration::between(last, now);
                let interval = self
                    .params
                    .interval
                    .max(dcc.t_off)
                    .max(self.params.min_interval);
                if elapsed < interval {
                    return None;
                }
                GenReason::Periodic
            }
        };
        self.last = Some(now);
        self.msg_count = (self.msg_count + 1) % 128;
        Some(reason)
    }
}

/// The device's two timers.
#[derive(Debug, Clone)]
pub struct VruSchedule {
    services: VruServices,
    vam: CamTriggerState,
    psm: PsmTimer,
    generated: u32,
}

impl VruSchedule {
    /// A schedule running `services` with the standards' own bounds.
    #[must_use]
    pub fn new(services: VruServices) -> VruSchedule {
        VruSchedule {
            services,
            vam: CamTriggerState::new(vam_gen_params()),
            psm: PsmTimer::new(PsmGenParams::j2945_9()),
            generated: 0,
        }
    }

    /// The same schedule with non-default parameters.
    #[must_use]
    pub fn with_params(mut self, vam: CamGenParams, psm: PsmGenParams) -> VruSchedule {
        self.vam = CamTriggerState::new(vam);
        self.psm = PsmTimer::new(psm);
        self
    }

    /// How often the engine must call [`VruSchedule::due`]: `T_GenVamMin`, which is also
    /// the PSM floor, so one timer serves both.
    #[must_use]
    pub fn check_interval(&self) -> Duration {
        self.vam.params().t_gen_cam_min
    }

    /// The VAM trigger state, for a detector asking whether a peer's VAM was due.
    #[must_use]
    pub fn vam(&self) -> &CamTriggerState {
        &self.vam
    }

    /// The PSM timer.
    #[must_use]
    pub fn psm(&self) -> &PsmTimer {
        &self.psm
    }

    /// How many messages this schedule has asked for in the window.
    #[must_use]
    pub const fn generated(&self) -> u32 {
        self.generated
    }

    /// Clears the window counter.
    pub fn reset_window(&mut self) {
        self.generated = 0;
    }

    /// What, if anything, to send now — from the device's own belief and its own clock.
    pub fn due(
        &mut self,
        believed_now: SimTime,
        belief: &PositionEstimate,
        dcc: &DccState,
    ) -> Vec<GenRequest> {
        let mut out = Vec::new();
        if self.services.vam {
            let dynamics = CamDynamics::from_estimate(belief);
            if let Some(d) = self.vam.check(believed_now, &dynamics, dcc) {
                out.push(GenRequest {
                    msg_type: MsgType::Vam,
                    reason: d.reason,
                    include_low_frequency: d.include_low_frequency,
                    at: believed_now,
                });
            }
        }
        if self.services.psm
            && let Some(reason) = self.psm.check(believed_now, dcc)
        {
            out.push(GenRequest {
                msg_type: MsgType::Psm,
                reason,
                // The PSM's path-history container is its low-cadence content; the size
                // model's nominal element count for a PSM is zero, so nothing is claimed.
                include_low_frequency: false,
                at: believed_now,
            });
        }
        self.generated = self.generated.saturating_add(out.len() as u32);
        out
    }
}

// =========================================================================================
// Records
// =========================================================================================

/// `node.tx` — a transmission a VRU device suppressed, with the constraint that stopped
/// it.
///
/// It rides `node.tx` and not a channel of its own because it *is* a transmission
/// decision: 03-interfaces.md §14 gives `node.tx` the per-message send record, and a
/// suppressed message is the same event with `sent: false` and a cause. A new channel would
/// need a wire id in vwp-v1 §3.6 and a reader in `v2xw-metrics`, and an airtime or
/// duty-cycle study that filtered `node.tx` would then silently miss every suppression.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VruSuppressionRecord {
    /// The device's own believed instant.
    pub t: SimTime,
    /// The device.
    pub node: NodeId,
    /// What it was going to send.
    pub msg_type: &'static str,
    /// `false`, always: this record exists because the message did not go out.
    pub sent: bool,
    /// Why: `duty-cycle-budget`, `duty-cycle-t-off`, `frame-too-long`, `energy`,
    /// `no-credential`, `no-payload`, `no-signing-cost` or `sign-failed`.
    pub cause: &'static str,
    /// The frame's length, bytes, where one was built.
    pub bytes: u32,
    /// The duty cycle over the last second at the moment of the decision, as a fraction
    /// quantised to 1e-3.
    pub duty_cycle: f64,
    /// Energy spent so far, joules, quantised to 1e-3.
    pub energy_j: f64,
}

impl v2xw_core::ctx::Record for VruSuppressionRecord {
    const CHANNEL: &'static str = "node.tx";
    const VISIBILITY: v2xw_core::ctx::Visibility = v2xw_core::ctx::Visibility::Node;
}

/// The causes [`VruSuppressionRecord::cause`] uses, for a reader that wants the closed set.
pub const SUPPRESSION_CAUSES: [&str; 8] = [
    "duty-cycle-budget",
    "duty-cycle-t-off",
    "frame-too-long",
    "energy",
    "no-credential",
    "no-payload",
    "no-signing-cost",
    "sign-failed",
];

// =========================================================================================
// The runtime
// =========================================================================================

/// How a VRU device is configured.
#[derive(Debug, Clone)]
pub struct VruConfig {
    /// Handset or beacon.
    pub kind: VruDeviceKind,
    /// Which awareness services it runs.
    pub services: VruServices,
    /// Queue capacities, in [`QueueKind::ALL`] order.
    pub queue_capacity: [usize; 5],
    /// How many peer certificates it caches. Smaller than a vehicle's by default: a
    /// handset's cache competes with the rest of the phone for memory, and
    /// `vru-device/handset-generic` publishes no RAM allowance at all.
    pub peer_cache_capacity: usize,
    /// How many neighbours it tracks.
    pub neighbor_capacity: usize,
    /// How long between telemetry frames.
    pub telemetry_period: Duration,
    /// Transmit power, dBm. Also what the energy model charges for.
    pub tx_power_dbm: f64,
    /// The primitive the signing cost table is keyed by.
    pub sign_op: &'static str,
    /// The verification primitive.
    pub verify_op: &'static str,
    /// The scenario's wall clock, for certificate validity and `generationTime`.
    pub wall: WallClock,
    /// The geodetic anchor of the world's ENU frame.
    pub origin: GeoOrigin,
    /// The body's dimensions, for a message that carries them.
    pub dims: Dims,
    /// Which crypto backend signs and verifies.
    pub crypto_mode: CryptoMode,
    /// The PSID a safety message is signed under.
    pub psid: u64,
    /// Which content profile the size models are asked for.
    pub content_profile: ContentProfile,
    /// How many path-history points a PSM carries. `None` takes the size model's own
    /// nominal count for the chosen profile.
    pub psm_path_points: Option<u32>,
    /// How many path-history points a VAM carries. `None` as [`VruConfig::psm_path_points`].
    pub vam_path_points: Option<u32>,
    /// Who carries the device, for the PSM's `basicType`: a pedestrian or a cyclist.
    pub psm_user_type: v2xw_msg::j2735::psm::PersonalDeviceUserType,
}

impl Default for VruConfig {
    fn default() -> VruConfig {
        VruConfig {
            kind: VruDeviceKind::Handset,
            services: VruServices::ETSI,
            // A quarter of the vehicle default in the receive path and a sixteenth in the
            // CRL path: these are engine defaults, not device figures, for the same reason
            // the vehicle's are (no profile publishes a queue depth). The device is sized
            // smaller because a pedestrian hears fewer neighbours than a vehicle in
            // traffic does, not because anything published says so.
            queue_capacity: [32, 32, 32, 16, 8],
            peer_cache_capacity: 32,
            neighbor_capacity: 64,
            telemetry_period: Duration::from_secs(1),
            // The pedestrian-UE transmit power of 3GPP TR 36.885 Annex A.1.1, which
            // `vru-device/handset-generic` carries as an evaluation assumption.
            tx_power_dbm: 23.0,
            sign_op: "ecdsa-p256-sign",
            verify_op: "ecdsa-p256-verify",
            wall: WallClock::default(),
            origin: GeoOrigin::new(0.0, 0.0, 0.0),
            dims: Dims::PEDESTRIAN,
            crypto_mode: CryptoMode::Modeled,
            psid: PSID_SAFETY,
            content_profile: ContentProfile::Typical,
            psm_path_points: None,
            vam_path_points: None,
            psm_user_type: v2xw_msg::j2735::psm::PersonalDeviceUserType::Pedestrian,
        }
    }
}

impl VruConfig {
    /// The defaults for a device of `kind`.
    ///
    /// A beacon is transmit-only, so it runs no receive path; nothing else differs, because
    /// nothing published says what else would.
    #[must_use]
    pub fn for_kind(kind: VruDeviceKind) -> VruConfig {
        VruConfig {
            kind,
            ..VruConfig::default()
        }
    }
}

/// What one VRU-device step produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VruStepOutcome {
    /// Frames for the network layer, in generation order.
    pub transmissions: Vec<Transmission>,
    /// Messages delivered to the device's applications, in arrival order.
    pub delivered: Vec<VerifiedMessage>,
    /// Messages the device wanted to send and did not, with the reason.
    pub suppressed: Vec<(MsgType, &'static str)>,
    /// Where each transmission's payload length came from, parallel to
    /// [`VruStepOutcome::transmissions`] and in the same order.
    ///
    /// Carried beside the transmissions rather than on
    /// [`crate::runtime::Transmission`] because that type is the engine's, shared with
    /// the vehicle path, and the vehicle's payloads are real encoder output with nothing
    /// to qualify. A consumer that wants to know whether a byte count is a measurement or
    /// a modelled length reads it here.
    pub payload_provenance: Vec<(MsgType, PayloadProvenance)>,
    /// The telemetry record, when this step closed a window.
    pub telemetry: Option<NodeTelemetry>,
    /// Every received frame whose fate was settled in this step, with the instants of its
    /// journey through the device — the same report an OBU gives, so the engine records
    /// a pedestrian's receptions on `node.rx` exactly as a vehicle's.
    pub rx_reports: Vec<RxReport>,
}

/// A frame in the device's receive path, with what its reception report needs.
#[derive(Debug, Clone)]
struct RxItem {
    frame: RxFrame,
    token: u64,
    arrived: SimTime,
}

/// A vulnerable-road-user device: a phone or a beacon, with the limits a hand-held radio
/// has.
///
/// Its own runtime and not a configuration of [`crate::runtime::ObuRuntime`], for the same
/// reason [`crate::rsu::RsuRuntime`] is its own: the thing that makes it interesting is
/// what is *between* the generator and the antenna — a duty-cycle gate and an energy
/// budget — and neither exists on a vehicle. A vehicle with two extra optional gates would
/// be a vehicle carrying a pedestrian's constraints for every run.
pub struct VruDeviceRuntime {
    node: NodeId,
    config: VruConfig,
    service: ProfileServiceModel,
    cpu: ServerBank,
    hsm: ServerBank,
    accel: ServerBank,
    queues: [NodeQueue<Queued<RxItem>>; 5],
    drops: DropLedger,
    clock: ClockModel,
    belief: PositionEstimate,
    stores: Stores,
    policy: Box<dyn VerificationPolicy>,
    schedule: VruSchedule,
    duty: DutyCycle,
    power: PowerBudget,
    state: NodeState,
    received: Vec<VerifiedMessage>,
    evidence_capacity: usize,
    window: TelemetryWindow,
    dcc: DccState,
    dcc_state_code: u16,
    security: NodeSecurity,
    card: ModelCard,
    /// Relevance scores a safety application published, by signer digest.
    relevance: BTreeMap<[u8; 8], f64>,
    /// The §3.5.2 field marked **GT**, handed in from outside the firewall by
    /// [`VruDeviceRuntime::observe_truth`] and read by nothing but the telemetry path.
    gt_pos_error_m: f32,
}

impl core::fmt::Debug for VruDeviceRuntime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("VruDeviceRuntime")
            .field("node", &self.node)
            .field("kind", &self.config.kind)
            .field("profile", &self.service.profile().id)
            .field("state", &self.state)
            .field("duty_cycle_refusals", &self.duty_cycle_refusals())
            .field("energy_j", &self.power.spent_j())
            .finish_non_exhaustive()
    }
}

impl Model for VruDeviceRuntime {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl VruDeviceRuntime {
    /// A device on `profile` with `config`, starting at `at`.
    #[must_use]
    pub fn new(
        node: NodeId,
        profile: HardwareProfile,
        config: VruConfig,
        at: SimTime,
    ) -> VruDeviceRuntime {
        let cpu_servers = profile.cpu.cores.or(1).max(1);
        let hsm_servers = profile.hsm.servers.or(1).max(1);
        let service = ProfileServiceModel::new(profile.clone());
        let stores = Stores {
            peers: PeerCertCache::new(config.peer_cache_capacity),
            neighbors: NeighborTable::new(config.neighbor_capacity),
            ..Default::default()
        };
        let card = card(&profile, &config);
        VruDeviceRuntime {
            node,
            cpu: ServerBank::new("cpu", cpu_servers, at),
            hsm: ServerBank::new("hsm", hsm_servers, at),
            accel: ServerBank::new("accel", 1, at),
            queues: [
                NodeQueue::new(QueueKind::Rx, config.queue_capacity[0]),
                NodeQueue::new(QueueKind::Verify, config.queue_capacity[1]),
                NodeQueue::new(QueueKind::App, config.queue_capacity[2]),
                NodeQueue::new(QueueKind::Tx, config.queue_capacity[3]),
                NodeQueue::new(QueueKind::Crl, config.queue_capacity[4]),
            ],
            drops: DropLedger::new(),
            clock: ClockModel::new(0.0),
            belief: PositionEstimate::no_fix(at),
            stores,
            // `verify-all` on a device that verifies at all: a pedestrian has no lane and
            // no direction of travel to prioritise by, so `prioritized`'s distance score
            // would order its inbox by a quantity that means less here than it does in
            // traffic. A scenario that wants the on-demand policy sets it.
            policy: Box::new(VerifyAll::new()),
            schedule: VruSchedule::new(config.services),
            duty: DutyCycle::conformant(),
            power: PowerBudget::accounting_only(config.tx_power_dbm),
            state: NodeState::Active,
            received: Vec::new(),
            evidence_capacity: 64,
            window: TelemetryWindow::new(at),
            dcc: DccState::UNRESTRICTED,
            dcc_state_code: v2xw_record::wire::U16_NONE,
            security: NodeSecurity::new(config.wall, config.crypto_mode, config.psid),
            card,
            relevance: BTreeMap::new(),
            gt_pos_error_m: f32::NAN,
            service,
            config,
        }
    }

    /// The default device: the shipped `vru-device/handset-generic` profile, a handset,
    /// the ETSI stack.
    ///
    /// # Panics
    /// Never in a built crate: the profile is compiled in by
    /// [`crate::profiles::PROFILE_SOURCES`] and `profiles::all` has already validated it.
    #[must_use]
    pub fn handset(node: NodeId, at: SimTime) -> VruDeviceRuntime {
        let profile = crate::profiles::get(crate::profiles::GENERIC_VRU_DEVICE)
            .expect("the generic VRU-device profile ships with the crate")
            .clone();
        VruDeviceRuntime::new(
            node,
            profile,
            VruConfig::for_kind(VruDeviceKind::Handset),
            at,
        )
    }

    /// The same, as a transmit-only beacon on the SAE stack.
    ///
    /// # Panics
    /// As [`VruDeviceRuntime::handset`].
    #[must_use]
    pub fn beacon(node: NodeId, at: SimTime) -> VruDeviceRuntime {
        let profile = crate::profiles::get(crate::profiles::GENERIC_VRU_DEVICE)
            .expect("the generic VRU-device profile ships with the crate")
            .clone();
        let config = VruConfig {
            services: VruServices::SAE,
            ..VruConfig::for_kind(VruDeviceKind::Beacon)
        };
        VruDeviceRuntime::new(node, profile, config, at)
    }

    /// The hardware profile.
    #[must_use]
    pub fn profile(&self) -> &HardwareProfile {
        self.service.profile()
    }

    /// The configuration.
    #[must_use]
    pub fn config(&self) -> &VruConfig {
        &self.config
    }

    /// The device's stores.
    #[must_use]
    pub fn stores(&self) -> &Stores {
        &self.stores
    }

    /// The stores, mutably, for a credential protocol installing real credentials.
    pub fn stores_mut(&mut self) -> &mut Stores {
        &mut self.stores
    }

    /// The security stack.
    #[must_use]
    pub fn security(&self) -> &NodeSecurity {
        &self.security
    }

    /// The security stack, mutably.
    pub fn security_mut(&mut self) -> &mut NodeSecurity {
        &mut self.security
    }

    /// The duty-cycle constraint.
    #[must_use]
    pub fn duty(&self) -> &DutyCycle {
        &self.duty
    }

    /// The energy budget.
    #[must_use]
    pub fn power(&self) -> &PowerBudget {
        &self.power
    }

    /// Replaces the energy budget — how a scenario gives the device a battery.
    pub fn set_power(&mut self, power: PowerBudget) {
        self.power = power;
    }

    /// Replaces the duty-cycle constraint, for a study of a non-conformant device.
    pub fn set_duty(&mut self, duty: DutyCycle) {
        self.duty = duty;
    }

    /// Replaces the verification policy.
    pub fn set_policy(&mut self, policy: Box<dyn VerificationPolicy>) {
        self.policy = policy;
    }

    /// The message schedule.
    #[must_use]
    pub fn schedule(&self) -> &VruSchedule {
        &self.schedule
    }

    /// What state the device is in.
    #[must_use]
    pub fn state(&self) -> NodeState {
        self.state
    }

    /// Moves the device to another state.
    pub fn set_state(&mut self, state: NodeState) {
        self.state = state;
    }

    /// The device's clock model.
    #[must_use]
    pub fn clock(&self) -> &ClockModel {
        &self.clock
    }

    /// The clock model, mutably, for a scenario event or an attacker's step.
    pub fn clock_mut(&mut self) -> &mut ClockModel {
        &mut self.clock
    }

    /// Hands the device this step's position belief, from the GNSS model.
    pub fn set_belief(&mut self, belief: PositionEstimate) {
        self.belief = belief;
    }

    /// Sets the DCC state the timers honour.
    pub fn set_dcc(&mut self, dcc: DccState, state_code: u16) {
        self.dcc = dcc;
        self.dcc_state_code = state_code;
    }

    /// Installs the relevance scores a safety application published
    /// ([`crate::safety::SafetyAppSet::relevance`]).
    pub fn set_relevance(&mut self, relevance: BTreeMap<[u8; 8], f64>) {
        self.relevance = relevance;
    }

    /// Hands in the ground-truth position error §3.5.2 asks for, from outside the
    /// firewall. Written into the telemetry path and read by nothing else.
    pub fn observe_truth(&mut self, pos_error_m: f32) {
        self.gt_pos_error_m = pos_error_m;
    }

    /// How many transmissions the duty-cycle constraint stopped, all three causes
    /// together.
    #[must_use]
    pub fn duty_cycle_refusals(&self) -> u64 {
        self.duty
            .refused_budget()
            .saturating_add(self.duty.refused_too_soon())
            .saturating_add(self.duty.refused_too_long())
    }

    /// How many transmissions the energy budget stopped.
    #[must_use]
    pub fn energy_refusals(&self) -> u64 {
        self.power.refusals()
    }

    /// One engine tick.
    ///
    /// The order is the OBU's, with the two gates inserted where a hand-held radio has
    /// them — after the message has been built and signed, because a device that has
    /// already spent the signing energy and then finds it may not transmit is the real
    /// behaviour and the expensive one.
    pub fn step(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<RxFrame>,
        distance_travelled_m: f64,
    ) -> VruStepOutcome {
        let stamped = inbox.into_iter().map(|f| (f, RxStamp::default())).collect();
        self.step_timed(ctx, stamped, distance_travelled_m)
    }

    /// [`VruDeviceRuntime::step`] with each frame's reception token and arrival instant,
    /// so every frame's fate comes back in [`VruStepOutcome::rx_reports`] — which is how
    /// the engine hosts the device beside the vehicles' OBUs.
    pub fn step_timed(
        &mut self,
        ctx: &mut dyn NodeCtx,
        inbox: Vec<(RxFrame, RxStamp)>,
        distance_travelled_m: f64,
    ) -> VruStepOutcome {
        let now = ctx.now();
        self.clock.advance(now, self.belief.fix.has_position());
        let believed = self.clock.believed_time(now);

        let mut out = VruStepOutcome::default();
        let arrival = |clock: &ClockModel, stamp: &RxStamp| {
            stamp
                .arrived_at
                .map_or(believed, |t| clock.believed_time(t))
                .min(believed)
        };
        if self.state == NodeState::Off {
            for (_, stamp) in &inbox {
                let at = arrival(&self.clock, stamp);
                out.rx_reports.push(RxReport {
                    token: stamp.token,
                    disposition: RxDisposition::NodeOff,
                    arrived: at,
                    parsed: at,
                    verify_start: None,
                    verify_done: None,
                });
            }
            return out;
        }

        if self.config.kind.receives() {
            let items = inbox
                .into_iter()
                .map(|(frame, stamp)| RxItem {
                    arrived: arrival(&self.clock, &stamp),
                    token: stamp.token,
                    frame,
                })
                .collect();
            self.receive(ctx, believed, items, &mut out);
        } else if !inbox.is_empty() {
            // A beacon has no receiver. The frames are not silently discarded: they are
            // counted as arrivals and dropped with a cause, so a run where a beacon was
            // expected to hear something shows why it did not.
            self.drops
                .record_n(DropCause::VerifyPolicySkip, inbox.len() as u32);
            for (_, stamp) in &inbox {
                let at = arrival(&self.clock, stamp);
                out.rx_reports.push(RxReport {
                    token: stamp.token,
                    disposition: RxDisposition::Dropped(DropCause::VerifyPolicySkip),
                    arrived: at,
                    parsed: at,
                    verify_start: None,
                    verify_done: None,
                });
            }
        }
        self.stores.neighbors.age(believed);
        self.stores.certs.travelled(distance_travelled_m);
        self.stores.certs.sweep(believed, &self.stores.crl);
        let _ = self.stores.certs.rotate(believed);

        if self.state.transmits() && !self.power.is_exhausted() {
            self.generate(ctx, believed, &mut out);
        }

        if self.window.length(now) >= self.config.telemetry_period {
            out.telemetry = Some(self.close_window(now));
        }
        out
    }

    fn receive(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        inbox: Vec<RxItem>,
        out: &mut VruStepOutcome,
    ) {
        let mut inbox = inbox;
        inbox.sort_by_key(|i| i.arrived);
        for item in inbox {
            self.window.message_in();
            if let Admission::Refused(refused) = self.queues[0].push(Queued {
                enqueued_at: item.arrived,
                item,
            }) {
                self.drops.record(DropCause::RxOverflow);
                out.rx_reports.push(Self::dropped(&refused.item, DropCause::RxOverflow));
            }
        }

        for q in self.queues[0].drain() {
            let item = q.item;
            let frame = item.frame.clone();
            self.learn_or_request(&frame);
            let relevance = frame
                .signer
                .as_ref()
                .and_then(|s| self.relevance.get(&crate::safety::digest_key(s)).copied());
            let summary = RxSummary {
                signer: frame.signer.clone(),
                msg_type: frame.msg_type,
                bytes: frame.bytes,
                received_at: believed,
                claimed_pos: frame.claimed_pos,
                relevance,
            };
            let decision = {
                let view = PolicyView {
                    position: &self.belief,
                    neighbors: &self.stores.neighbors,
                    queue_depth: self.queues[1].len(),
                    queue_capacity: self.queues[1].capacity(),
                };
                self.policy.decide(&summary, &view)
            };
            self.log_decision(ctx, believed, frame.msg_type, &decision);
            match decision {
                VerifyDecision::Drop { cause } => {
                    self.drops.record(cause);
                    out.rx_reports.push(Self::dropped(&item, cause));
                }
                VerifyDecision::DeliverUnverified { reason } => {
                    let _ = reason;
                    self.drops.record(DropCause::VerifyPolicySkip);
                    let m = self.to_message(&frame, believed, VerificationState::Unverified);
                    self.deliver(m, out);
                    out.rx_reports.push(RxReport {
                        token: item.token,
                        disposition: RxDisposition::Delivered(VerificationState::Unverified),
                        arrived: item.arrived,
                        parsed: item.arrived,
                        verify_start: None,
                        verify_done: None,
                    });
                }
                VerifyDecision::Verify { .. } => {
                    let queued = Queued {
                        enqueued_at: item.arrived,
                        item,
                    };
                    let admitted = if self.policy.oldest_drop() {
                        self.queues[1].push_evicting(queued)
                    } else {
                        self.queues[1].push(queued)
                    };
                    match admitted {
                        Admission::Queued => {}
                        Admission::Refused(q) | Admission::Evicted(q) => {
                            self.drops.record(DropCause::VerifyOverflow);
                            out.rx_reports
                                .push(Self::dropped(&q.item, DropCause::VerifyOverflow));
                        }
                    }
                }
            }
        }

        self.run_verifications(ctx, believed, out);
    }

    fn learn_or_request(&mut self, frame: &RxFrame) {
        let Some(signer) = frame.signer.clone() else {
            return;
        };
        if frame.full_certificate {
            self.stores.peers.learn(&signer);
        } else if !self.stores.peers.touch(&signer) {
            self.stores.peers.record_p2pcd_request();
        }
    }

    fn run_verifications(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        out: &mut VruStepOutcome,
    ) {
        let probe = OpDescriptor::verify(self.config.verify_op, 0);
        if self.service.service_time(ctx, &probe).is_none() {
            // The profile costs no verification: the queue does not drain, which shows up
            // as a growing `q_verify` and a `verifications_per_s` of zero rather than as
            // free cryptography.
            return;
        }
        let where_ = self.service.runs_on(&probe);
        for q in self.queues[1].drain() {
            let item = q.item;
            let frame = item.frame.clone();
            let op = OpDescriptor::verify(self.config.verify_op, frame.bytes);
            let Some(cost) = self.service.service_time(ctx, &op) else {
                continue;
            };
            let sched = match where_ {
                RunsOn::Hsm => self.hsm.submit(q.enqueued_at, cost),
                RunsOn::Accelerator => self.accel.submit(q.enqueued_at, cost),
                RunsOn::Cpu => self.cpu.submit(q.enqueued_at, cost),
            };
            self.window.verification(sched.wait);
            let verdict = self.classify(ctx, &frame);
            ctx.emit(VerifyDecisionRecord::verified(
                self.node,
                q.enqueued_at,
                sched.start,
                sched.finish,
                policy_id(self.policy.code()),
                frame.msg_type.as_str(),
                self.config.verify_op,
                match verdict {
                    VerificationState::Verified | VerificationState::Revoked => "valid",
                    VerificationState::Invalid => "invalid",
                    _ => "skipped",
                },
                0,
            ));
            let m = self.to_message(&frame, believed, verdict);
            self.deliver(m, out);
            out.rx_reports.push(RxReport {
                token: item.token,
                disposition: RxDisposition::Delivered(verdict),
                arrived: item.arrived,
                parsed: item.arrived,
                verify_start: Some(sched.start),
                verify_done: Some(sched.finish),
            });
        }
    }

    /// The report for a frame dropped before it reached the applications.
    fn dropped(item: &RxItem, cause: DropCause) -> RxReport {
        RxReport {
            token: item.token,
            disposition: RxDisposition::Dropped(cause),
            arrived: item.arrived,
            parsed: item.arrived,
            verify_start: None,
            verify_done: None,
        }
    }

    /// What the device concludes about one frame, having spent the verification time.
    ///
    /// The same two paths [`crate::runtime::ObuRuntime`] has: the bytes when the engine
    /// carries them, and the engine's own verdict when it does not.
    fn classify(&mut self, ctx: &mut dyn NodeCtx, frame: &RxFrame) -> VerificationState {
        let authentic = match &frame.spdu {
            Some(bytes) => match self.verify_on_the_wire(ctx, bytes) {
                SpduVerdict::Valid => true,
                SpduVerdict::Invalid => false,
                SpduVerdict::Unverifiable => return VerificationState::Unverified,
            },
            None => frame.signature_valid,
        };
        if !authentic {
            return VerificationState::Invalid;
        }
        let Some(lv) = frame.claimed_linkage else {
            return VerificationState::Verified;
        };
        if let Some(own) = self.stores.certs.active() {
            let mine = own.i_period;
            if self.stores.crl.current_period() != mine {
                self.stores.crl.set_period(mine);
            }
        }
        match self
            .stores
            .crl
            .check(frame.claimed_cert_period, lv, authentic)
        {
            crate::stores::CrlVerdict::Revoked => VerificationState::Revoked,
            crate::stores::CrlVerdict::NotRevoked => VerificationState::Verified,
            crate::stores::CrlVerdict::RefusedImplausiblePeriod { .. } => {
                VerificationState::Invalid
            }
        }
    }

    fn verify_on_the_wire(&mut self, ctx: &mut dyn NodeCtx, bytes: &[u8]) -> SpduVerdict {
        let Some(parsed) = self.security.parse(bytes) else {
            return SpduVerdict::Invalid;
        };
        let certificate = match NodeSecurity::attached_certificate(&parsed) {
            Some(c) => {
                self.stores.peers.learn_certificate(c.clone());
                Some(c)
            }
            None => NodeSecurity::parsed_signer_digest(&parsed)
                .and_then(|d| self.stores.peers.certificate(&d)),
        };
        let Some(certificate) = certificate else {
            return SpduVerdict::Unverifiable;
        };
        self.security
            .verify_parsed(ctx, &parsed, &certificate, self.node)
    }

    fn to_message(
        &self,
        frame: &RxFrame,
        believed: SimTime,
        verification: VerificationState,
    ) -> VerifiedMessage {
        VerifiedMessage {
            signer: frame.signer.clone(),
            msg_type: frame.msg_type,
            bytes: frame.bytes,
            received_at: believed,
            claimed_generation_time: frame.claimed_generation_time,
            claimed_pos: frame.claimed_pos,
            claimed_speed_mps: frame.claimed_speed_mps,
            claimed_heading_rad: frame.claimed_heading_rad,
            verification,
        }
    }

    fn deliver(&mut self, m: VerifiedMessage, out: &mut VruStepOutcome) {
        self.window
            .delivered(m.verification == VerificationState::Verified);
        if m.verification != VerificationState::Invalid
            && let Some(signer) = m.signer.clone()
        {
            self.stores.neighbors.observe(Neighbor {
                signer,
                claimed_pos: m.claimed_pos.unwrap_or(v2xw_core::geom::Vec3::ZERO),
                claimed_speed_mps: m.claimed_speed_mps,
                claimed_heading_rad: m.claimed_heading_rad,
                claimed_generation_time: m.claimed_generation_time,
                last_heard: m.received_at,
                messages: 1,
                state: m.verification,
            });
        }
        if self.received.len() >= self.evidence_capacity {
            self.received.remove(0);
        }
        self.received.push(m.clone());
        out.delivered.push(m);
    }

    /// Builds, signs and gates whatever the schedule says is due.
    fn generate(&mut self, ctx: &mut dyn NodeCtx, believed: SimTime, out: &mut VruStepOutcome) {
        let requests = self.schedule.due(believed, &self.belief, &self.dcc);
        if requests.is_empty() {
            return;
        }
        let Some(cred) = self.stores.certs.active().cloned() else {
            for r in &requests {
                self.suppress(ctx, believed, r.msg_type, 0, "no-credential", out);
            }
            return;
        };
        if !self.provision_all(ctx, believed)
            || !self.security.set_active(cred.i_period, cred.j_index)
        {
            for r in &requests {
                self.suppress(ctx, believed, r.msg_type, 0, "no-credential", out);
            }
            return;
        }
        let Some(cred) = self.stores.certs.active().cloned() else {
            for r in &requests {
                self.suppress(ctx, believed, r.msg_type, 0, "no-credential", out);
            }
            return;
        };

        for r in requests {
            let Some((payload, provenance)) = self.encode_payload(r.msg_type, &cred, believed)
            else {
                self.suppress(ctx, believed, r.msg_type, 0, "no-payload", out);
                continue;
            };
            let sid = self.security.signer_id_for(r.msg_type, believed);
            let Ok((frame, pdu)) = self.security.sign(ctx, r.msg_type, &payload, sid, None) else {
                self.suppress(ctx, believed, r.msg_type, 0, "sign-failed", out);
                continue;
            };
            let bytes = frame.bytes_on_wire();
            let op = OpDescriptor::sign(self.config.sign_op, frame.payload_bytes());
            let Some(cost) = self.service.service_time(ctx, &op) else {
                // Nothing is signed for free, on a handset as on a vehicle.
                self.suppress(ctx, believed, r.msg_type, bytes, "no-signing-cost", out);
                continue;
            };
            let sched = match self.service.runs_on(&op) {
                RunsOn::Hsm => self.hsm.submit(believed, cost),
                RunsOn::Accelerator => self.accel.submit(believed, cost),
                RunsOn::Cpu => self.cpu.submit(believed, cost),
            };

            // The two gates a hand-held radio has, in the order they bite. `cbr` is read
            // out first so the gate's `&mut self.duty` and the read of `self.dcc` are two
            // statements rather than one expression borrowing two fields.
            let cbr = self.dcc.cbr;
            let verdict = self.duty.gate(sched.finish, bytes, cbr);
            let airtime = match verdict {
                DutyVerdict::Transmit { airtime } => airtime,
                DutyVerdict::FrameTooLong { .. } => {
                    self.suppress(ctx, believed, r.msg_type, bytes, "frame-too-long", out);
                    continue;
                }
                DutyVerdict::BudgetSpent { .. } => {
                    self.suppress(ctx, believed, r.msg_type, bytes, "duty-cycle-budget", out);
                    continue;
                }
                DutyVerdict::TooSoon { .. } => {
                    self.suppress(ctx, believed, r.msg_type, bytes, "duty-cycle-t-off", out);
                    continue;
                }
            };
            if !self.power.spend(airtime) {
                self.suppress(ctx, believed, r.msg_type, bytes, "energy", out);
                continue;
            }

            let full_certificate = pdu.signer_id == v2xw_sec::SignerIdChoice::Certificate;
            if full_certificate {
                self.security
                    .note_certificate_attached(r.msg_type, believed);
            }
            self.window.message_out(airtime, full_certificate);
            out.payload_provenance.push((r.msg_type, provenance));
            out.transmissions.push(Transmission {
                msg_type: r.msg_type,
                bytes,
                signer: cred.digest.clone(),
                full_certificate,
                ready_at: sched.finish,
                generation_time: r.at,
                sign_start: sched.start,
                signed: Some(frame),
            });
        }
    }

    /// Records a transmission that did not happen, and counts it as a transmit drop.
    fn suppress(
        &mut self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        msg_type: MsgType,
        bytes: u32,
        cause: &'static str,
        out: &mut VruStepOutcome,
    ) {
        self.drops.record(DropCause::TxOverflow);
        let duty = self.duty.ratio(believed);
        let energy = self.power.spent_j();
        ctx.emit(VruSuppressionRecord {
            t: believed,
            node: self.node,
            msg_type: msg_type_name(msg_type),
            sent: false,
            cause,
            bytes,
            duty_cycle: math::quantize_to(duty, crate::safety::SURROGATE_Q),
            energy_j: math::quantize_to(energy, crate::safety::SURROGATE_Q),
        });
        out.suppressed.push((msg_type, cause));
    }

    /// Issues a certificate for every credential the store holds. `false` if any failed.
    fn provision_all(&mut self, ctx: &mut dyn NodeCtx, believed: SimTime) -> bool {
        let pseudonyms: Vec<(u32, u32)> = self
            .stores
            .certs
            .credentials()
            .iter()
            .map(|c| (c.i_period, c.j_index))
            .collect();
        for (i, j) in pseudonyms {
            if self
                .security
                .provision(ctx, self.node, i, j, believed)
                .is_err()
            {
                return false;
            }
        }
        for cred in self.stores.certs.credentials_mut() {
            let Some(signer) = self
                .security
                .signer_for_pseudonym(cred.i_period, cred.j_index)
            else {
                return false;
            };
            if cred.digest != *signer.digest() {
                cred.digest = signer.digest().clone();
                cred.cert_coer = signer.cert_coer().to_vec();
            }
        }
        true
    }

    /// The payload for one message, with the provenance of its length.
    ///
    /// `None` for a message type this device does not build. Neither payload is real wire
    /// bytes and [`PayloadProvenance`] says which kind of not-real it is; the fill is
    /// [`v2xw_msg::codec::PLACEHOLDER_FILL`], so a consumer that decoded one fails loudly
    /// rather than reading a plausible all-zero message.
    /// The payload for one message, with the provenance of its length.
    ///
    /// The PSM is **really encoded**: SAE J2735 UPER from the device's own belief, by
    /// [`v2xw_msg::j2735::psm`], inside its `MessageFrame`, with the first four octets of
    /// the active pseudonym's digest as its temporary id — so the id on the air changes
    /// exactly when the pseudonym does, as a vehicle's does. The VAM still comes from the
    /// validated ETSI size model: its encoder needs the TS 103 300-3 module in the ETSI
    /// build unit, which is not there yet. [`PayloadProvenance`] says which is which on
    /// every frame; a placeholder is [`v2xw_msg::codec::PLACEHOLDER_FILL`] so a consumer
    /// that decodes one fails loudly.
    fn encode_payload(
        &self,
        msg_type: MsgType,
        cred: &CredentialHandle,
        believed: SimTime,
    ) -> Option<(Vec<u8>, PayloadProvenance)> {
        let station_id = {
            let mut id = [0u8; 4];
            id.copy_from_slice(&cred.digest.0[..4]);
            id
        };
        if msg_type == MsgType::Psm {
            use v2xw_msg::j2735::psm;
            // `msgCnt` is the value the PSM timer assigned when it said this PSM was due.
            let msg_cnt = self.schedule.psm().msg_count();
            let input = psm::PsmInput {
                basic_type: self.config.psm_user_type,
                msg_cnt,
                id: station_id,
                position: self.belief,
                origin: self.config.origin,
                sec_mark: v2xw_msg::j2735::bsm::sec_mark(self.config.wall, believed),
            };
            let message = psm::build_psm(&input).ok()?;
            let encoded = psm::encode_message_frame(&message).ok()?;
            return Some((
                encoded.bytes,
                PayloadProvenance::Encoded {
                    codec: psm::PSM_CODEC_ID,
                },
            ));
        }
        let elements = match msg_type {
            MsgType::Vam => self.config.vam_path_points,
            _ => return None,
        };
        let (bytes, provenance) =
            modelled_payload(msg_type, self.config.content_profile, elements)?;
        // `PLACEHOLDER_FILL` is 0xA5 and not zero, for the reason `v2xw-msg` gives: a
        // zero-filled buffer is indistinguishable from an uninitialised one, and a
        // consumer that decoded a placeholder should fail loudly.
        Some((vec![PLACEHOLDER_FILL; bytes as usize], provenance))
    }

    fn log_decision(
        &self,
        ctx: &mut dyn NodeCtx,
        believed: SimTime,
        msg_type: MsgType,
        d: &VerifyDecision,
    ) {
        // A skip or a drop settles the message's fate here; a decision to verify is
        // recorded when the check runs (`run_verifications`), with its instants.
        if let Some(rec) = VerifyDecisionRecord::decided(
            self.node,
            believed,
            policy_id(self.policy.code()),
            msg_type.as_str(),
            d,
        ) {
            ctx.emit(rec);
        }
    }

    fn close_window(&mut self, now: SimTime) -> NodeTelemetry {
        let storage = self.service.profile().storage_model;
        let profile = self.service.profile();
        let (total, verified, unverified, revoked) = self.stores.neighbors.counts();
        let queue_depths = [
            self.queues[0].depth_percentiles(),
            self.queues[1].depth_percentiles(),
            self.queues[2].depth_percentiles(),
            self.queues[3].depth_percentiles(),
            self.queues[4].depth_percentiles(),
        ];
        let stores_bytes = self.stores.bytes(&storage);
        let inputs = TelemetryInputs {
            node: self.node,
            storage_used_b: stores_bytes,
            storage_total_b: profile
                .flash_bytes
                .get()
                .copied()
                .unwrap_or(v2xw_record::wire::U64_NONE),
            next_topup_ns: v2xw_record::wire::U64_NONE,
            crl_bytes: self.stores.crl.bytes(&storage),
            outbox_bytes: self.stores.outbox.bytes(),
            clock_offset_ns: self.clock.offset_ns(),
            ram_used_kib: u32::try_from(
                stores_bytes.saturating_add(storage.baseline_ram_bytes) / 1024,
            )
            .unwrap_or(u32::MAX),
            ram_total_kib: profile
                .ram_bytes
                .get()
                .map(|b| u32::try_from(b / 1024).unwrap_or(u32::MAX))
                .unwrap_or(v2xw_record::wire::U32_NONE),
            drops: self.drops.counts(),
            cert_stored: self.stores.certs.stored_count() as u32,
            crl_entries: self.stores.crl.entries() as u32,
            outbox_msgs: self.stores.outbox.len() as u32,
            peer_cache_entries: self.stores.peers.len() as u32,
            p2pcd_requests: self.stores.peers.p2pcd_requests(),
            gnss_sigma_m: self.belief.semi_major_m as f32,
            gnss_hdop: f32::NAN,
            clock_drift_ppm: self.clock.drift_ppm() as f32,
            pos_error_m: self.gt_pos_error_m,
            cpu_util_pm: self.cpu.utilisation_pm(now),
            hsm_util_pm: self
                .hsm
                .utilisation_pm(now)
                .max(self.accel.utilisation_pm(now)),
            queue_depths,
            dcc_state: self.dcc_state_code,
            cbr_pm: self.dcc.cbr.map_or(v2xw_record::wire::U16_NONE, |c| {
                (math::quantize_to(c * 1000.0, 1.0) as u16).min(1000)
            }),
            tx_power_cdbm: i16::try_from(
                math::quantize_to(self.config.tx_power_dbm * 100.0, 1.0) as i64
            )
            .unwrap_or(i16::MAX),
            neighbors: (total, verified, unverified, revoked),
            cert_active: u16::try_from(self.stores.certs.active_count(now)).unwrap_or(u16::MAX),
            crl_expansion_pm: self.stores.crl.expansion_pm(),
            gnss_fix: gnss_fix_code(self.belief.fix),
            state: self.state,
            verify_policy: self.policy.code(),
        };
        let record = self.window.record(now, &inputs);

        self.window.reset(now);
        self.drops.reset();
        self.cpu.reset_window(now);
        self.hsm.reset_window(now);
        self.accel.reset_window(now);
        for q in &mut self.queues {
            q.reset_window();
        }
        self.stores.peers.reset_window();
        self.stores.crl.reset_window();
        self.schedule.reset_window();
        record
    }
}

impl NodeView for VruDeviceRuntime {
    type Neighbors = NeighborTable;
    type Credential = CredentialHandle;
    type Message = VerifiedMessage;

    fn node(&self) -> NodeId {
        self.node
    }

    fn believed_time(&self) -> SimTime {
        self.clock.believed_time(self.belief.time_ns)
    }

    fn position(&self) -> &PositionEstimate {
        &self.belief
    }

    fn neighbors(&self) -> &NeighborTable {
        &self.stores.neighbors
    }

    fn credentials(&self) -> &[CredentialHandle] {
        self.stores.certs.credentials()
    }

    fn received(&self) -> &[VerifiedMessage] {
        &self.received
    }
}

/// The policy id for a policy code, the same mapping [`crate::runtime`] uses.
fn policy_id(code: u8) -> &'static str {
    match code {
        0 => crate::policy::VERIFY_ALL_ID,
        1 => crate::policy::ON_DEMAND_ID,
        _ => crate::policy::PRIORITIZED_ID,
    }
}

fn msg_type_name(t: MsgType) -> &'static str {
    match t {
        MsgType::Psm => "psm",
        MsgType::Vam => "vam",
        MsgType::Cam => "cam",
        MsgType::Bsm => "bsm",
        MsgType::Denm => "denm",
        _ => "other",
    }
}

fn plan(name: &str, unit: &str, default: serde_json::Value, why: &str, how: &str) -> Parameter {
    let mut p = Parameter::new(name, unit, default, Source::todo_calibrate(why));
    p.calibration = Some(how.to_string());
    p
}

fn card(profile: &HardwareProfile, config: &VruConfig) -> ModelCard {
    let mut card = ModelCard::new(
        VRU_DEVICE_ID,
        // See VRU_DEVICE_ID: what this model decides is when the device transmits, so the
        // family is the one whose definition is "message generation rules". The `Family`
        // enum is closed (ADR 0007 Consequences), so a `VruDevice` variant is not this
        // build's to add.
        Family::Generator,
        "0.1.0",
        format!(
            "Vulnerable-road-user device runtime ({} on hardware profile `{}`): PSM and \
             VAM generation under the EN 302 571 duty cycle and an energy budget, with a \
             real IEEE 1609.2 envelope around a payload neither encoder produces.",
            config.kind.as_str(),
            profile.id
        ),
    );
    card.tier = vec![Tier::Abstract, Tier::Medium, Tier::High];
    card.equations = vec![
        Equation::new(
            "air time",
            "T = 32 us + 8 us + 8 us*ceil((N_SERVICE + 8*bytes + N_TAIL)/N_DBPS), through \
             v2xw_radio::air_time — the OFDM parameters of 04-models.md §4.2, not an \
             assumed rate",
        ),
        Equation::new(
            "duty cycle",
            "sum of T_on inside a one-second window divided by one second, capped at 3 % \
             [EN 302 571 V2.1.1 §4.2.10.2]",
        ),
        Equation::new(
            "energy per transmission",
            "E = 10^((P_dBm - 30)/10) * T_on / eta_PA + E_per_message",
        ),
    ];
    card.parameters = vec![
        Parameter::new(
            "duty_cycle_max",
            "-",
            serde_json::json!(0.03),
            Source::new(
                SourceKind::Standard,
                "ETSI EN 302 571 V2.1.1 §4.2.10.2 Eq. 2-5: duty cycle <= 3 %, T_on <= 4 ms, \
                 T_off >= 25 ms, applying under any DCC algorithm [04-models.md §6, R1 §D.3]",
            ),
        ),
        Parameter::new(
            "t_on_max_ms",
            "ms",
            serde_json::json!(4),
            Source::new(
                SourceKind::Standard,
                "ETSI EN 302 571 V2.1.1 §4.2.10.2 [04-models.md §6, R1 §D.3]",
            ),
        ),
        Parameter::new(
            "t_off_min_ms",
            "ms",
            serde_json::json!(25),
            Source::new(
                SourceKind::Standard,
                "ETSI EN 302 571 V2.1.1 §4.2.10.2 [04-models.md §6, R1 §D.3]",
            ),
        ),
        Parameter::new(
            "t_gen_vam_min_ms",
            "ms",
            serde_json::json!(100),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 300-3 V2.2.1 §6.2, all VERIFIED [04-models.md §8.1, R4 §A.6]",
            ),
        ),
        Parameter::new(
            "t_gen_vam_max_ms",
            "ms",
            serde_json::json!(5_000),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 300-3 V2.2.1 §6.2, all VERIFIED [04-models.md §8.1, R4 §A.6]",
            ),
        ),
        Parameter::new(
            "t_assemble_vam_ms",
            "ms",
            serde_json::json!(T_ASSEMBLE_VAM.as_nanos() / 1_000_000),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 300-3 V2.2.1 §6.2 [04-models.md §8.1]",
            ),
        ),
        Parameter::new(
            "vam_low_frequency_interval_ms",
            "ms",
            serde_json::json!(2_000),
            Source::new(
                SourceKind::Standard,
                "ETSI TS 103 300-3 V2.2.1 §6.2: low-frequency container first, then every \
                 >= 2,000 ms [04-models.md §8.1]",
            ),
        ),
        Parameter::new(
            "tx_power_dbm",
            "dBm",
            serde_json::json!(config.tx_power_dbm),
            Source::new(
                SourceKind::Standard,
                "3GPP TR 36.885 V14.0.0 Annex A.1.1: pedestrian UE 23 dBm, '33 dBm is not \
                 precluded'. An evaluation assumption rather than a handset datasheet \
                 figure, as `vru-device/handset-generic` records [R2 Topic A]",
            ),
        ),
        plan(
            "psm_interval_ms",
            "ms",
            serde_json::json!(1_000),
            "04-models.md §8.1: the J2945/9 PSM rate rules are UNVERIFIED — a patent claims \
             2-5 per second by speed and the PASS trial used 100 ms",
            "The design set's own plan: read SAE J2945/9 §6 primary text and replace the \
             1 Hz default. The rate drives both the channel load a pedestrian adds and the \
             battery the device spends, so a study of either must sweep it.",
        ),
        plan(
            "vam_path_points",
            "-",
            match config.vam_path_points {
                Some(n) => serde_json::json!(n),
                None => serde_json::Value::Null,
            },
            "no source gives how many path-history points a VAM carries in the field",
            "Absent, the size model's own nominal element count for the chosen content \
             profile is used, so the device transmits the message `codec/size-model/etsi` \
             was validated at. Its rows are checked against 235 B (C2C-CC), 300 B \
             (arXiv 2506.22052) and 350 B (TR 2050 Fig. 12, 5GAA the same), all at \
             secured-message scope; this runtime adds the real envelope on top, so the \
             secured figure is the *check* and not the payload. Replace the whole model by \
             committing VAM-PDU-Descriptions.asn and generating the encoder, which is the \
             plan `codec/size-model/etsi`'s own card carries.",
        ),
        Parameter::new(
            "content_profile",
            "-",
            serde_json::json!(config.content_profile.as_str()),
            Source::new(
                SourceKind::Code,
                "the content profile the size models are asked for; `typical` is \
                 v2xw-msg's own definition of 'what a deployment actually broadcasts'",
            ),
        ),
        plan(
            "n_gen_vam",
            "-",
            serde_json::json!(1),
            "TS 103 300-3 §6.2 states no N_GenVam: the clause EN 302 637-2 §6.1.3 uses to \
             return T_GenCam to its maximum has no VAM counterpart",
            "1 makes the shortened interval after a dynamics-triggered VAM apply to exactly \
             one message, which is the minimal reading of a clause that does not exist. \
             Check TS 103 300-3 V2.3.1 (2025-12), which 04-models.md §8.1 records as not \
             yet read, for a stated value.",
        ),
        plan(
            "pa_efficiency",
            "-",
            serde_json::json!(DEFAULT_PA_EFFICIENCY),
            "no power-amplifier efficiency for a handset V2X front end appears in R7 or R5, \
             and `vru-device/handset-generic`'s power_w is NOT PUBLISHED",
            "Measure the device's draw with the radio transmitting at a fixed power and at \
             a fixed rate, and with it idle; the difference divided by the radiated power \
             is this number. It scales the energy per message linearly, so a battery study \
             must sweep it. Note that it cannot make an unconfigured run wrong: with no \
             battery_j the budget accounts and enforces nothing.",
        ),
        plan(
            "battery_j",
            "J",
            serde_json::Value::Null,
            "06-node-models.md §7 publishes no VRU-device profile and no battery capacity \
             for one",
            "Take the usable energy from the device the study is about — a phone battery's \
             watt-hours times 3,600, times the fraction the V2X application is permitted — \
             and set it with PowerBudget::with_battery_j. Absent, the budget accounts every \
             transmission's energy and refuses nothing, which is why a run with no battery \
             configured still reports what one would have lost.",
        ),
        plan(
            "per_message_j",
            "J",
            serde_json::Value::Null,
            "the energy of building and signing a message, outside the amplifier, is not \
             published for any device",
            "Measure the draw while signing at a fixed rate with the radio disabled. It is \
             the term that makes a software-signing device more expensive than one with a \
             secure element, which is the comparison a VRU-device study wants.",
        ),
    ];
    card.assumptions = vec![
        format!(
            "Costs come from the hardware profile `{}` and nowhere else; an operation it \
             does not cost has no service time here either, so a device whose profile \
             costs no signature transmits nothing and records `no-signing-cost`.",
            profile.id
        ),
        "Air time is computed at one modulation for the whole run (v2xw_radio::Mcs, \
         default 6 Mbit/s QPSK 1/2, 'the default safety-channel rate'); rate adaptation is \
         the DCC family's and would change the duty cycle spent per message."
            .into(),
        "The IEEE 1609.2 envelope is real and signed; neither payload is. \
         PayloadProvenance travels with every frame so the two cannot be confused."
            .into(),
    ];
    card.limitations = vec![
        "TS 103 300-3's trajectory-interception trigger, redundancy mitigation and VRU \
         clustering are not implemented. All three *reduce* the VAM rate, so this device \
         transmits at least as often as a conformant one: pessimistic for channel load and \
         for battery, optimistic for detectability."
            .into(),
        "Neither payload is encoder output. The VAM's length is `codec/size-model/etsi`'s \
         literature-checked row and the PSM's is `codec/size-model/j2735`'s \
         derived-no-anchor row — 04-models.md §8.2 records 'PSM | none | | | UNVERIFIED' \
         for the published PSM anchors, so there is nothing to check that row against."
            .into(),
        "The energy model charges the amplifier and an optional per-message term. It does \
         not model receive energy, the screen, or the rest of the phone, so a handset's \
         real endurance is shorter than this budget predicts."
            .into(),
        "Antenna height and gain come from the profile, where the height is NOT PUBLISHED: \
         whether a pedestrian is heard at all depends on it, and a scenario must set it."
            .into(),
    ];
    card.sources = vec![
        Source::new(SourceKind::Standard, "ETSI EN 302 571 V2.1.1 §4.2.10.2"),
        Source::new(
            SourceKind::Standard,
            "ETSI TS 103 300-3 V2.2.1 §6.2, §6.4, §6.5",
        ),
        Source::new(
            SourceKind::Standard,
            "SAE J2945/9 (PSM from VRU devices over DSRC/1609; scope VERIFIED secondary, \
             rate rules UNVERIFIED) [R4 §A.5]",
        ),
        Source::new(
            SourceKind::Standard,
            "3GPP TR 36.885 V14.0.0 Annex A.1.1 and Table A.1.1-1 (pedestrian-UE power and \
             antenna gain) [R2 Topic A, R2c]",
        ),
        Source::new(
            SourceKind::Datasheet,
            format!("hardware profile {}@{}", profile.id, profile.version),
        ),
    ];
    card.validation = Validation::new(ValidationStatus::UnitTested);
    card.validation.tests = vec![
        "vru_device::the_duty_cycle_cap_is_what_stops_a_burst".to_string(),
        "vru_device::a_device_with_a_battery_goes_quiet_when_it_is_spent".to_string(),
        "vru_device::a_stationary_pedestrian_heart_beats_at_t_gen_vam_max".to_string(),
    ];
    card.determinism = Determinism::default();
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use v2xw_core::belief::FixQuality;
    use v2xw_core::geom::Vec3;
    use v2xw_core::time::NS_PER_MS;

    /// A frame length the duty-cycle arithmetic below is worked out at: 350 bytes, the
    /// whole-message VAM figure `codec/size-model/etsi`'s rows are anchored against
    /// (C2C-CC TR 2050 Fig. 12, with 5GAA stating the same). Used as a *frame* length
    /// here, which is what the channel sees.
    const VAM_FRAME_BYTES: u32 = 350;

    fn belief_at(pos: Vec3, speed: f64) -> PositionEstimate {
        let mut p = PositionEstimate::no_fix(0);
        p.pos = pos;
        p.vel = Vec3::new(speed, 0.0, 0.0);
        p.heading_rad = 0.0;
        p.fix = FixQuality::ThreeD;
        p
    }

    /// The VAM parameters are TS 103 300-3's bounds, not EN 302 637-2's.
    #[test]
    fn the_vam_bounds_are_the_ts103300_3_ones() {
        let p = vam_gen_params();
        assert_eq!(p.t_gen_cam_min, Duration::from_millis(100));
        assert_eq!(p.t_gen_cam_max, Duration::from_millis(5_000));
        assert_eq!(p.low_frequency_interval, Duration::from_millis(2_000));
        // The three dynamics thresholds are shared with the CAM, which is the point of
        // reusing the state machine.
        let cam = CamGenParams::en302637_2();
        assert_eq!(p.position_threshold_m, cam.position_threshold_m);
        assert_eq!(p.speed_threshold_mps, cam.speed_threshold_mps);
        assert_eq!(p.heading_threshold_rad, cam.heading_threshold_rad);
    }

    /// A stationary pedestrian heart-beats at `T_GenVamMax`, five seconds, and not at the
    /// CAM's one second.
    #[test]
    fn a_stationary_device_heart_beats_every_five_seconds() {
        let mut s = VruSchedule::new(VruServices::ETSI);
        let b = belief_at(Vec3::ZERO, 0.0);
        let dcc = DccState::UNRESTRICTED;
        let mut sent = 0;
        for k in 0..=100u64 {
            sent += s.due(k * 100 * NS_PER_MS, &b, &dcc).len();
        }
        // Ten seconds of checks: the first, then one every five seconds.
        assert_eq!(sent, 3, "the first VAM, then t = 5 s and t = 10 s");
    }

    /// The PSM cadence is the design set's 1 Hz default.
    #[test]
    fn psm_runs_at_one_hertz() {
        let mut s = VruSchedule::new(VruServices::SAE);
        let b = belief_at(Vec3::ZERO, 0.0);
        let dcc = DccState::UNRESTRICTED;
        let mut sent = 0;
        for k in 0..=100u64 {
            sent += s.due(k * 100 * NS_PER_MS, &b, &dcc).len();
        }
        assert_eq!(sent, 11, "the first, then one per second for ten seconds");
        assert_eq!(s.psm().msg_count(), 11);
    }

    /// Four metres of believed movement triggers a VAM early, exactly as it triggers a
    /// CAM — which is why the state machine is shared.
    #[test]
    fn four_metres_of_movement_triggers_a_vam() {
        let mut s = VruSchedule::new(VruServices::ETSI);
        let dcc = DccState::UNRESTRICTED;
        assert_eq!(s.due(0, &belief_at(Vec3::ZERO, 0.0), &dcc).len(), 1);
        assert!(
            s.due(
                200 * NS_PER_MS,
                &belief_at(Vec3::new(3.9, 0.0, 0.0), 0.0),
                &dcc
            )
            .is_empty()
        );
        let out = s.due(
            300 * NS_PER_MS,
            &belief_at(Vec3::new(4.1, 0.0, 0.0), 0.0),
            &dcc,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].msg_type, MsgType::Vam);
    }

    /// The air time of a 350-byte VAM payload at the default modulation is 512 us, which
    /// is well under the 4 ms `T_on` cap and 0.05 % of the one-second window.
    ///
    /// The arithmetic is 04-models.md §4.2's, evaluated at `v2xw_radio::Mcs::R6Qpsk12`'s
    /// 48 data bits per 8 us symbol (the half-clocked 10 MHz channel, EN 302 663 Table
    /// C.1): `32 + 8 + 8*ceil((16 + 8*350 + 6)/48) = 40 + 8*59 = 512 us`.
    #[test]
    fn the_air_time_of_a_vam_is_the_ofdm_one() {
        let d = DutyCycle::conformant();
        let airtime = d.airtime_of(VAM_FRAME_BYTES);
        assert!(d.floor().admits(airtime));
        assert_eq!(airtime, Duration::from_micros(512));
        assert_eq!(d.mcs().data_bits_per_symbol(), 48);
    }

    /// At the fastest cadence EN 302 571's own `T_off` permits, a VAM-sized frame does
    /// **not** breach the 3 % duty cycle — 40 frames a second of 512 us is 2.05 % — and
    /// this is the assertion that keeps the next one honest: the cap is a real constraint
    /// and not one that fires on everything.
    #[test]
    fn a_vam_sized_frame_at_the_t_off_floor_stays_inside_the_cap() {
        let mut d = DutyCycle::conformant();
        for k in 0..40u64 {
            assert!(
                d.gate(k * 25 * NS_PER_MS, VAM_FRAME_BYTES, None)
                    .is_transmit(),
                "frame {k} was refused"
            );
        }
        assert_eq!(d.refused_budget(), 0);
        let ratio = d.ratio(39 * 25 * NS_PER_MS);
        assert!(ratio > 0.02 && ratio < 0.03, "{ratio}");
    }

    /// The 3 % cap is what stops a burst of larger frames, and the counter says so.
    ///
    /// 800 bytes is 1,112 us of air time (`40 + 8*ceil((16 + 6400 + 6)/48) = 40 + 8*134`),
    /// so 40 frames a second would be 44.5 ms against a 30 ms cap. Injected fault: with
    /// `En302571Floor::disabled()` the identical burst is allowed, which is the second half
    /// of this test — without it the assertion could pass on a gate that refused
    /// everything.
    #[test]
    fn the_three_percent_cap_stops_a_burst() {
        let bytes = 800u32;
        let mut d = DutyCycle::conformant();
        assert_eq!(d.airtime_of(bytes), Duration::from_micros(1_112));
        let mut allowed = 0usize;
        for k in 0..40u64 {
            if d.gate(k * 25 * NS_PER_MS, bytes, None).is_transmit() {
                allowed += 1;
            }
        }
        assert!(allowed > 0, "the cap must not refuse everything");
        assert!(allowed < 40, "the cap must bind: {allowed} of 40");
        assert_eq!(d.refused_budget() as usize, 40 - allowed);
        assert!(d.ratio(39 * 25 * NS_PER_MS) <= 0.03 + 1e-9);

        let mut off = DutyCycle::new(
            v2xw_radio::En302571Floor::disabled(),
            v2xw_radio::Mcs::R6Qpsk12,
        );
        let mut unbounded = 0usize;
        for k in 0..40u64 {
            if off.gate(k * 25 * NS_PER_MS, bytes, None).is_transmit() {
                unbounded += 1;
            }
        }
        assert_eq!(unbounded, 40, "with the floor disabled nothing is refused");
    }

    /// `T_off` is 25 ms unconditionally, so a device asking every 10 ms is refused two
    /// attempts out of three.
    #[test]
    fn t_off_floors_the_rate_at_forty_hertz() {
        let mut d = DutyCycle::conformant();
        assert!(d.gate(0, 100, None).is_transmit());
        assert!(matches!(
            d.gate(10 * NS_PER_MS, 100, None),
            DutyVerdict::TooSoon { .. }
        ));
        assert!(d.gate(25 * NS_PER_MS, 100, None).is_transmit());
        assert_eq!(d.refused_too_soon(), 1);
    }

    /// A frame too long for `T_on` is refused outright rather than fragmented here.
    #[test]
    fn a_frame_over_four_milliseconds_is_refused() {
        let d = DutyCycle::conformant();
        // 4 ms at 6 Mbit/s is about 3 kB; 8 kB is well past it.
        let airtime = d.airtime_of(8_000);
        assert!(!d.floor().admits(airtime));
        let mut d = d;
        assert!(matches!(
            d.gate(0, 8_000, None),
            DutyVerdict::FrameTooLong { .. }
        ));
        assert_eq!(d.refused_too_long(), 1);
    }

    /// 23 dBm is 200 mW, and a 512 us frame at 35 % amplifier efficiency costs about
    /// 0.29 mJ — so a joule buys about 3,400 VAMs, which is the number a battery study
    /// starts from.
    #[test]
    fn the_energy_of_one_transmission_is_the_radiated_one_over_the_efficiency() {
        let p = PowerBudget::accounting_only(23.0);
        let watts = p.radiated_w();
        assert!((watts - 0.199_526).abs() < 1e-5, "{watts}");
        let e = p.energy_of(Duration::from_micros(512));
        let expected = watts * 512e-6 / DEFAULT_PA_EFFICIENCY;
        assert!((e - expected).abs() < 1e-12);
        assert!(e > 0.00025 && e < 0.0003, "{e}");
        // The efficiency scales it linearly, which is the sensitivity the card asks a
        // battery study to report.
        let ideal = PowerBudget {
            pa_efficiency: 1.0,
            ..p
        };
        assert!(
            (ideal.energy_of(Duration::from_micros(512)) * (1.0 / DEFAULT_PA_EFFICIENCY) - e).abs()
                < 1e-12
        );
    }

    /// An accounting-only budget refuses nothing and still reports what was spent.
    #[test]
    fn an_unconfigured_budget_accounts_and_does_not_enforce() {
        let mut p = PowerBudget::accounting_only(23.0);
        for _ in 0..10_000 {
            assert!(p.spend(Duration::from_micros(984)));
        }
        assert!(p.spent_j() > 0.0);
        assert_eq!(p.refusals(), 0);
        assert!(!p.is_exhausted());
        assert_eq!(p.remaining_j(), None);
    }

    /// A configured budget runs out, refuses, and stays refused.
    #[test]
    fn a_configured_budget_runs_out() {
        let mut p = PowerBudget::accounting_only(23.0).with_battery_j(0.01);
        let mut sent = 0;
        while p.spend(Duration::from_micros(512)) {
            sent += 1;
            assert!(sent < 1_000, "the budget must run out");
        }
        assert!(sent > 0);
        assert_eq!(p.refusals(), 1, "one refusal, and then it stays refused");
        assert!(!p.spend(Duration::from_micros(512)));
        assert_eq!(p.refusals(), 2);
    }

    /// A beacon has no receiver, and the frames it is handed are counted rather than
    /// silently discarded.
    #[test]
    fn a_beacon_has_no_receiver() {
        assert!(!VruDeviceKind::Beacon.receives());
        assert!(VruDeviceKind::Handset.receives());
    }

    /// The suppression causes are a closed set, and every string the runtime passes is in
    /// it. A cause spelled two ways is how a per-cause count silently loses half its
    /// subject.
    #[test]
    fn every_suppression_cause_is_in_the_published_set() {
        let mut sorted = SUPPRESSION_CAUSES;
        sorted.sort_unstable();
        assert_eq!(sorted.len(), SUPPRESSION_CAUSES.len());
        for c in SUPPRESSION_CAUSES {
            assert!(!c.is_empty());
        }
    }

    /// Both payloads come from a size model, both say which one, and neither claims to be
    /// real wire bytes.
    ///
    /// The VAM's length is the ETSI table's and not the 350 B planning figure: 350 B is a
    /// *secured-message* anchor, and this runtime adds a real IEEE 1609.2 envelope on top
    /// of whatever the payload is, so using the secured figure as a payload would count
    /// the envelope twice.
    #[test]
    fn both_payloads_come_from_a_size_model_and_say_so() {
        for ty in [MsgType::Psm, MsgType::Vam] {
            let (bytes, provenance) = modelled_payload(ty, ContentProfile::Typical, None)
                .unwrap_or_else(|| panic!("{ty} has a size-model row"));
            assert!(bytes > 0, "{ty} sized at zero");
            assert!(!provenance.is_real(), "{ty} must not claim real bytes");
            match provenance {
                PayloadProvenance::SizeModel { model, .. } => {
                    assert!(!model.is_empty());
                    assert_eq!(
                        model,
                        if ty == MsgType::Psm {
                            PSM_SIZE_MODEL
                        } else {
                            VAM_SIZE_MODEL
                        }
                    );
                }
                PayloadProvenance::Encoded { .. } => panic!("no encoder exists for {ty}"),
            }
            // A secured VAM is a published 235-350 B, so a payload anywhere near or above
            // that would be counting the envelope twice.
            assert!(
                bytes < 235,
                "{ty} payload {bytes} B is at secured-message scale"
            );
        }
        // An element count overrides the row's nominal one, and it is per-element linear.
        let (a, _) =
            modelled_payload(MsgType::Vam, ContentProfile::Typical, Some(0)).expect("a VAM row");
        let (b, _) =
            modelled_payload(MsgType::Vam, ContentProfile::Typical, Some(1)).expect("a VAM row");
        assert!(b > a);
        // Nothing else is sized here, which is what makes the `no-payload` suppression a
        // real branch.
        assert!(modelled_payload(MsgType::Cam, ContentProfile::Typical, None).is_none());
    }

    /// The default telemetry period is one second, and the default device is a handset on
    /// the ETSI stack: the combination a scenario with pedestrians gets if it says nothing.
    #[test]
    fn the_defaults_are_the_documented_ones() {
        let c = VruConfig::default();
        assert_eq!(c.kind, VruDeviceKind::Handset);
        assert_eq!(c.services, VruServices::ETSI);
        assert_eq!(c.telemetry_period, Duration::from_secs(1));
        assert_eq!(c.tx_power_dbm, 23.0);
        assert_eq!(c.content_profile, ContentProfile::Typical);
        assert_eq!(c.vam_path_points, None);
        assert_eq!(c.psm_path_points, None);
        let beacon = VruConfig::for_kind(VruDeviceKind::Beacon);
        assert!(!beacon.kind.receives());
    }
}
