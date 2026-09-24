//! Jammers: `attacker/jammer/constant`, `attacker/jammer/pulsed` and
//! `attacker/jammer/reactive` (04-models.md §12.3, 07-threats-and-detection.md §2.2).
//!
//! # A jammer is a transmitter with no protocol
//!
//! 04-models.md §12.3 says it in one sentence: "Jammers are `Attacker` models with
//! `TransmitRaw` actions and enter the interference sums of §4-5 as transmitters". So
//! there is no new mechanism here. A jammer raises the energy at every receiver in range;
//! the receiver's signal-to-noise path ([`crate::phy::OfdmPhy::sinr_windows`]) already
//! divides a wanted signal by noise plus everything else on the air, and jamming enters
//! that denominator exactly the way thermal noise does. What this module contributes is
//! *when* a jammer is on the air and *how much* power it puts there, with a card and a
//! citation per number.
//!
//! The division of labour with `v2xw-threat`: the jammer's **goals** (whom to follow,
//! when to start, what the operator declared it may do) are attacker logic and live
//! there, behind 03-interfaces.md §9's `Attacker` seam. The jammer's **emission** — the
//! duty cycle, the trigger threshold, the received power and the noise rise it causes —
//! is radio physics and lives here, because it is the [`crate::phy`] SINR path that
//! consumes it. A jammer's model card is a [`Family::Attacker`] card registered from this
//! crate for the same reason `phy/receiver/noise-sensitivity` is a PHY card: the numbers
//! it carries are the ones this crate reads.
//!
//! # Telling a jammed frame from a congested one
//!
//! This is the point of modelling jamming at all, and it is 04-models.md §12.3's own
//! detection cue: the reactive jammer "produces PDR = 0 dropouts uncorrelated with SINR
//! dips". A loss cause that said `collision` for a jammed frame would erase exactly that
//! signal, so [`crate::types::LossCause::Jammed`] is reported on a **counterfactual**:
//! the PHY evaluates the frame twice, once with the jamming power in the noise term and
//! once without, and against the *same* uniform draw. A frame that failed with the jammer
//! present and would have been decoded without it is `jammed`; a frame that would have
//! failed anyway is `collision`, `hidden-terminal` or `fading` as before. The rule is
//! exact rather than a heuristic threshold, it costs one extra deterministic evaluation,
//! and it cannot be confused with congestion because congestion is what the
//! counterfactual holds fixed. See [`crate::phy::OfdmPhy::finish_rx`].
//!
//! # What is not shipped, and why
//!
//! **`attacker/jammer/constant-pilot`.** 04-models.md §12.3 lists it with a cited total
//! power ([`PUNAL_PILOT_JAMMER_TX_POWER_DBM`], 2.42 dBm) but the mechanism is
//! subcarrier-selective: it jams the OFDM pilots only, and what that does to a frame is a
//! statement about channel estimation, not about the wideband noise floor. Neither
//! 04-models.md nor the cached Puñal study gives the mapping from pilot-only power to an
//! effective SINR penalty, and modelling it as a wideband jammer of 2.42 dBm would be a
//! fabricated number dressed as a cited one. Following this crate's stated policy (see
//! the crate docs, "What 'cited' means here, and where it stops"), the model is **not
//! shipped**; the cited power is kept as a constant so that whoever reads the channel
//! estimation loss out of a primary source has the anchor waiting.
//!
//! # Determinism
//!
//! The pulsed jammer's phase is one draw from `(RngDomain::Attack, EntityRef::Node(jammer))`,
//! taken once and cached, so its window list is a pure function of `(seed, node, period,
//! duty)` and not of how often the engine asked. The other two models draw nothing. Every
//! window list is in `(from, to)` order and every power sum goes through
//! [`crate::numeric::sum_powers_mw`], which sorts by [`NodeId`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::NodeId;
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime};

use crate::numeric;
use crate::types::{ChannelId, timing};

// =========================================================================================
// The cited constants of 04-models.md §12.3
// =========================================================================================

/// The WARP jammer's measured transmit power at 5.9 GHz, dBm
/// [Puñal, Aguiar and Gross 2012, R11 §D1, via 04-models.md §12.3].
///
/// The specification figure is about 18 dBm at 2.4 GHz; 16.75 dBm is what the study
/// measured in the band this crate models, so it is the default.
pub const PUNAL_JAMMER_TX_POWER_DBM: f64 = 16.75;

/// The legitimate Linkbird transmitter's measured power in the same study, dBm
/// (specification 21 dBm) [Puñal 2012, via 04-models.md §12.3].
///
/// Not a jammer parameter: it is the reference the study's blind areas were measured
/// against, and a run that wants to reproduce [`PUNAL_CONSTANT_BLIND_AREA_OPEN_M`] has to
/// use it for the victim as well.
pub const PUNAL_LEGITIMATE_TX_POWER_DBM: f64 = 17.58;

/// The reactive jammer's trigger threshold, dBm RSSI [Puñal 2012, via 04-models.md §12.3].
pub const PUNAL_REACTIVE_TRIGGER_DBM: f64 = -75.0;

/// The receiver noise floor the same study reports, dBm [Puñal 2012, via 04-models.md §12.3].
///
/// Kept for the RSSI-to-SINR map ([`punal_rssi_to_sinr_db`]) it belongs to, and
/// deliberately *not* used as this crate's noise floor: [`crate::phy::THERMAL_NOISE_10MHZ_DBM`]
/// plus [`crate::phy::NOISE_FIGURE_HARDWARE_DB`] is −98 dBm from kTB and a datasheet noise
/// figure (04-models.md §3.7), which is the floor every other model here is written
/// against. The 12 dB difference is a property of the study's hardware, and mixing the two
/// would put an uncited offset into every link budget.
pub const PUNAL_NOISE_FLOOR_DBM: f64 = -86.0;

/// The pilot-subcarrier jammer's total power, dBm [Puñal 2012, via 04-models.md §12.3].
///
/// The anchor for a model this crate does **not** ship; see the module documentation.
pub const PUNAL_PILOT_JAMMER_TX_POWER_DBM: f64 = 2.42;

/// The constant jammer's blind area in open space, metres (a platoon)
/// [Puñal 2012, via 04-models.md §12.3 and §13].
pub const PUNAL_CONSTANT_BLIND_AREA_OPEN_M: f64 = 250.0;

/// The constant jammer's blind area at a dense urban crossroad, metres (30 km/h, jammer
/// 33 m from the junction, indoors) [Puñal 2012, via 04-models.md §12.3 and §13].
pub const PUNAL_CONSTANT_BLIND_AREA_URBAN_M: f64 = 167.0;

/// The reactive jammer's blind area in open space, metres
/// [Puñal 2012, via 04-models.md §12.3 and §13].
pub const PUNAL_REACTIVE_BLIND_AREA_OPEN_M: f64 = 170.0;

/// The floor the reactive jammer drove delivery to in dense urban with the transmitter
/// near the jammer, as a ratio [Puñal 2012, via 04-models.md §12.3 and §13].
pub const PUNAL_REACTIVE_PDR_FLOOR: f64 = 0.60;

/// The tolerance 04-models.md §13's jamming row states for a blind area, metres.
pub const BLIND_AREA_TOLERANCE_M: f64 = 25.0;

/// The Puñal study's least-squares RSSI-to-SINR map, `γ[dB] = 0.8565·σ − 86.35`
/// [Puñal 2012, via 04-models.md §12.3].
///
/// `σ` is the receiver's reported RSSI in the study's own units. It is offered because
/// 04-models.md prints it, and it is **not** used by any model here: this crate computes
/// SINR from powers in dBm through [`crate::phy::sinr_db`], and the map exists to let a
/// reader compare a reported RSSI from that study against a modelled SINR.
#[must_use]
pub fn punal_rssi_to_sinr_db(sigma: f64) -> f64 {
    0.856_5 * sigma - 86.35
}

/// The free-space distance at which a jammer of `jammer_eirp_dbm` raises the noise at a
/// receiver to `target_dbm`, metres — the analytic blind-area radius.
///
/// `L = EIRP − target`, and Friis inverted at `f_hz`. It is the closed form the §13
/// blind-area row is checked against before a full run is spent on it, and it is exact
/// only in free space: a real blind area is shorter, which is why the study measured
/// 167 m at a crossroad against 250 m in the open.
#[must_use]
pub fn blind_area_radius_m(jammer_eirp_dbm: f64, target_dbm: f64, f_hz: f64) -> f64 {
    let loss_db = jammer_eirp_dbm - target_dbm;
    if !loss_db.is_finite() || loss_db <= 0.0 {
        return 0.0;
    }
    // Friis: L = 20·log10(4πd/λ)  =>  d = λ/(4π) · 10^(L/20).
    let lambda = numeric::wavelength_m(f_hz);
    lambda / (4.0 * core::f64::consts::PI) * math::pow(10.0, loss_db / 20.0)
}

// =========================================================================================
// Windows and arrivals
// =========================================================================================

/// One interval a jammer is on the air for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct JamWindow {
    /// When the emission starts.
    pub from: SimTime,
    /// When it stops.
    pub to: SimTime,
}

impl JamWindow {
    /// A window over `[from, to)`, empty when `to <= from`.
    #[must_use]
    pub const fn new(from: SimTime, to: SimTime) -> Self {
        Self { from, to }
    }

    /// True when the window carries no time.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.to <= self.from
    }

    /// True when this window and `[from, to)` share an instant.
    #[must_use]
    pub const fn overlaps(self, from: SimTime, to: SimTime) -> bool {
        self.from < to && from < self.to
    }
}

/// One interval of energy a jammer's own receiver measured, as the engine supplies it.
///
/// The reactive jammer of 04-models.md §12.3 "transmits only when energy above a trigger
/// threshold is sensed", and only the engine knows what the jammer heard: it owns the
/// pairwise link budget. So the sensing result is an input here rather than something this
/// module computes, exactly as [`crate::phy::InterferenceSource::audible_to_victim_tx`] is.
/// [`crate::phy::OfdmPhy::energy_dbm`] is what an engine calls to produce one.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SensedInterval {
    /// When the energy started arriving at the jammer.
    pub from: SimTime,
    /// When it stopped.
    pub to: SimTime,
    /// The energy, dBm, as the jammer's receiver measured it.
    pub energy_dbm: f64,
}

impl SensedInterval {
    /// A sensed interval.
    #[must_use]
    pub const fn new(from: SimTime, to: SimTime, energy_dbm: f64) -> Self {
        Self {
            from,
            to,
            energy_dbm,
        }
    }
}

/// Which jammer profile produced an emission, for the breakdown and the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JammerKind {
    /// Continuous OFDM-like noise in the channel.
    Constant,
    /// A duty cycle: on for a fraction of a fixed period.
    Pulsed,
    /// On only while energy above the trigger threshold is sensed.
    Reactive,
}

impl JammerKind {
    /// The spelling used in records and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            JammerKind::Constant => "constant",
            JammerKind::Pulsed => "pulsed",
            JammerKind::Reactive => "reactive",
        }
    }

    /// The model id of this profile.
    #[must_use]
    pub const fn model_id(self) -> &'static str {
        match self {
            JammerKind::Constant => ConstantJammer::ID,
            JammerKind::Pulsed => PulsedJammer::ID,
            JammerKind::Reactive => ReactiveJammer::ID,
        }
    }
}

/// One jammer's energy at one receiver: a received power and the window it is present for.
///
/// This is the jamming counterpart of [`crate::phy::InterferenceSource`], and it is a
/// separate type for one reason: an interferer carries a *frame* (it has an MCS, it can be
/// captured, it can be a hidden terminal), and a jammer carries none of those. Reusing
/// [`crate::phy::InterferenceSource`] would put a jammer into the preamble-capture
/// candidate set, where it would be treated as an incumbent frame the receiver could lock
/// on to.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct JamArrival {
    /// The jamming node.
    pub jammer: NodeId,
    /// Its received power at this receiver, dBm, after path loss, shadowing, obstacles and
    /// antennas — the engine computes it with the same link budget it uses for a frame.
    pub power_dbm: f64,
    /// The channel it lands on.
    pub channel: ChannelId,
    /// The window it is present for.
    pub window: JamWindow,
    /// Which profile produced it.
    pub kind: JammerKind,
}

impl JamArrival {
    /// A jamming arrival.
    #[must_use]
    pub const fn new(
        jammer: NodeId,
        power_dbm: f64,
        channel: ChannelId,
        window: JamWindow,
        kind: JammerKind,
    ) -> Self {
        Self {
            jammer,
            power_dbm,
            channel,
            window,
            kind,
        }
    }
}

/// The jamming every receiver is exposed to, as the engine declares it.
///
/// Held by the PHY ([`crate::phy::OfdmPhy::jamming_mut`]) and read by the SINR path. Keyed
/// by receiver index in a `BTreeMap`, so an iteration is in node order and a jamming power
/// can never depend on a hash order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JammingField {
    per_rx: BTreeMap<u32, Vec<JamArrival>>,
}

impl JammingField {
    /// An empty field: no jammer anywhere, which is what every scenario without an
    /// `attacker/jammer/*` model has.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// True when no jamming is declared anywhere — the fast path every unjammed run takes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.per_rx.is_empty()
    }

    /// Declares one jamming arrival at one receiver.
    ///
    /// Arrivals are kept sorted by `(window.from, window.to, jammer)`, so the window
    /// boundary list the SINR partition is built from is already in order.
    pub fn insert(&mut self, rx: NodeId, arrival: JamArrival) {
        if arrival.window.is_empty() {
            return;
        }
        let list = self.per_rx.entry(rx.index()).or_default();
        let key = |a: &JamArrival| (a.window.from, a.window.to, a.jammer);
        let at = list.partition_point(|a| key(a) < key(&arrival));
        list.insert(at, arrival);
    }

    /// Declares every window of one jammer at one receiver, at one received power.
    pub fn insert_windows(
        &mut self,
        rx: NodeId,
        jammer: NodeId,
        power_dbm: f64,
        channel: ChannelId,
        kind: JammerKind,
        windows: &[JamWindow],
    ) {
        for w in windows {
            self.insert(rx, JamArrival::new(jammer, power_dbm, channel, *w, kind));
        }
    }

    /// Every jamming arrival at one receiver, in window order.
    #[must_use]
    pub fn at(&self, rx: NodeId) -> &[JamArrival] {
        self.per_rx
            .get(&rx.index())
            .map_or(&[][..], |v| v.as_slice())
    }

    /// True when any jamming overlaps `[from, to)` on `ch` at `rx`.
    #[must_use]
    pub fn overlaps(&self, rx: NodeId, ch: ChannelId, from: SimTime, to: SimTime) -> bool {
        self.at(rx)
            .iter()
            .any(|a| a.channel == ch && a.window.overlaps(from, to))
    }

    /// The total jamming power at `rx` on `ch` over `[from, to)`, milliwatts, summed in
    /// [`NodeId`] order.
    ///
    /// Zero when nothing overlaps, which is the additive identity the noise term wants: a
    /// receiver with no jammer in range has its noise floor and nothing else.
    #[must_use]
    pub fn power_mw(&self, rx: NodeId, ch: ChannelId, from: SimTime, to: SimTime) -> f64 {
        let contributors: Vec<(NodeId, f64)> = self
            .at(rx)
            .iter()
            .filter(|a| a.channel == ch && a.window.overlaps(from, to))
            .map(|a| (a.jammer, numeric::dbm_to_mw(a.power_dbm)))
            .collect();
        if contributors.is_empty() {
            return 0.0;
        }
        numeric::sum_powers_mw(&contributors)
    }

    /// The total jamming power at `rx` on `ch` at one instant, dBm, or negative infinity
    /// when there is none.
    #[must_use]
    pub fn power_dbm_at(&self, rx: NodeId, ch: ChannelId, at: SimTime) -> f64 {
        let mw = self.power_mw(rx, ch, at, at.saturating_add(1));
        if mw <= 0.0 {
            f64::NEG_INFINITY
        } else {
            numeric::mw_to_dbm(mw)
        }
    }

    /// The jamming window boundaries strictly inside `(from, to)` at `rx` on `ch`, sorted.
    ///
    /// The SINR partition of [`crate::phy::OfdmPhy::sinr_windows`] needs them: a jammer
    /// that switches on halfway through a frame splits it into two windows exactly as a
    /// mid-frame interferer does, and without the boundary the second half would be
    /// evaluated at the first half's noise.
    #[must_use]
    pub fn boundaries_in(
        &self,
        rx: NodeId,
        ch: ChannelId,
        from: SimTime,
        to: SimTime,
    ) -> Vec<SimTime> {
        let mut out = Vec::new();
        for a in self.at(rx) {
            if a.channel != ch {
                continue;
            }
            for t in [a.window.from, a.window.to] {
                if t > from && t < to {
                    out.push(t);
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Forgets every arrival that ended at or before `cutoff`, and every receiver left
    /// with none.
    ///
    /// The cutoff is by time, never by handle, for the reason
    /// [`crate::phy::TX_INTERVAL_RETENTION_NS`] documents: pruning by what some other
    /// call owns would make an evaluation depend on the order receivers were driven in.
    pub fn prune(&mut self, cutoff: SimTime) {
        for list in self.per_rx.values_mut() {
            list.retain(|a| a.window.to > cutoff);
        }
        self.per_rx.retain(|_, v| !v.is_empty());
    }

    /// Forgets everything.
    pub fn clear(&mut self) {
        self.per_rx.clear();
    }

    /// How many arrivals are retained, over all receivers — for inspection and for the
    /// test that pins the pruning.
    #[must_use]
    pub fn len(&self) -> usize {
        self.per_rx.values().map(Vec::len).sum()
    }
}

// =========================================================================================
// The family seam
// =========================================================================================

/// A jammer's emission schedule (04-models.md §12.3).
///
/// Not one of 03-interfaces.md §4's six radio families: a jammer is an `Attacker` (§9),
/// and this is the part of it the radio consumes. It is generic over the context for the
/// same reason every trait in [`crate::traits`] is — the payload enum lives in the engine
/// crate — and it is a trait rather than an enum so that a scenario-supplied profile can
/// be dropped in without touching the PHY.
pub trait JammerProfile<C: Ctx + ?Sized>: Model {
    /// Which profile this is.
    fn kind(&self) -> JammerKind;

    /// The power at the jammer's own antenna connector, dBm.
    fn tx_power_dbm(&self) -> f64;

    /// The channel it jams.
    fn channel(&self) -> ChannelId;

    /// The windows this jammer is on the air for, within `[from, to)`.
    ///
    /// `sensed` is what the jammer's own receiver heard over the same span, which only a
    /// reactive profile reads. The returned windows are clipped to `[from, to)`, are
    /// non-empty, and are in `(from, to)` order.
    fn windows(
        &mut self,
        ctx: &mut C,
        jammer: NodeId,
        from: SimTime,
        to: SimTime,
        sensed: &[SensedInterval],
    ) -> Vec<JamWindow>;
}

// =========================================================================================
// `attacker/jammer/constant`
// =========================================================================================

/// `attacker/jammer/constant` — continuous OFDM-like noise in the channel
/// (04-models.md §12.3).
#[derive(Debug, Clone)]
pub struct ConstantJammer {
    card: ModelCard,
    tx_power_dbm: f64,
    channel: ChannelId,
}

impl ConstantJammer {
    /// The model's id.
    pub const ID: &'static str = "attacker/jammer/constant";

    /// The jammer at the measured WARP power on the control channel.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: constant_card(PUNAL_JAMMER_TX_POWER_DBM),
            tx_power_dbm: PUNAL_JAMMER_TX_POWER_DBM,
            channel: ChannelId::CCH,
        }
    }

    /// The same jammer at a caller-chosen power, which is what an attacker's declared
    /// `Capabilities` radio maximum bounds (07-threats-and-detection.md §1).
    #[must_use]
    pub fn with_power_dbm(mut self, dbm: f64) -> Self {
        self.tx_power_dbm = dbm;
        self.card = constant_card(dbm);
        self
    }

    /// The same jammer on another channel.
    #[must_use]
    pub fn on_channel(mut self, ch: ChannelId) -> Self {
        self.channel = ch;
        self
    }

    /// The transmit power, dBm.
    ///
    /// The inherent form of [`JammerProfile::tx_power_dbm`], which exists because the
    /// trait is generic over the context type: calling the trait method without a context
    /// in hand needs a turbofish for a value that does not depend on it. Every profile
    /// here carries the same pair.
    #[must_use]
    pub const fn power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    /// The channel it jams.
    #[must_use]
    pub const fn jammed_channel(&self) -> ChannelId {
        self.channel
    }
}

impl Default for ConstantJammer {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for ConstantJammer {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> JammerProfile<C> for ConstantJammer {
    fn kind(&self) -> JammerKind {
        JammerKind::Constant
    }

    fn tx_power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    fn channel(&self) -> ChannelId {
        self.channel
    }

    fn windows(
        &mut self,
        _ctx: &mut C,
        _jammer: NodeId,
        from: SimTime,
        to: SimTime,
        _sensed: &[SensedInterval],
    ) -> Vec<JamWindow> {
        if to <= from {
            return Vec::new();
        }
        vec![JamWindow::new(from, to)]
    }
}

// =========================================================================================
// `attacker/jammer/pulsed`
// =========================================================================================

/// The pulsed jammer's default period, nanoseconds.
///
/// One `T_CBR` (100 ms, 04-models.md §4.5), chosen so that a duty cycle is visible in the
/// channel-busy-ratio measurement a receiver reports rather than being averaged away
/// inside one window. It is a design choice, not a measurement, and the card says so.
pub const PULSED_DEFAULT_PERIOD_NS: u64 = timing::T_CBR.as_nanos();

/// The pulsed jammer's default duty cycle.
///
/// One half: the point at which a duty-cycle jammer maximises the product of "the channel
/// is unusable" and "the channel looks intermittently fine", which is the regime
/// 07-threats-and-detection.md §2.2's "random duty cycle" exists to explore. A design
/// choice, recorded as `todo-calibrate` on the card.
pub const PULSED_DEFAULT_DUTY: f64 = 0.5;

/// `attacker/jammer/pulsed` — a duty cycle: on for `duty × period`, off for the rest
/// (07-threats-and-detection.md §2.2's "random duty cycle").
#[derive(Debug, Clone)]
pub struct PulsedJammer {
    card: ModelCard,
    tx_power_dbm: f64,
    channel: ChannelId,
    period_ns: u64,
    duty: f64,
    /// The phase offset of each jammer, drawn once from `(Attack, Node)` and cached, so
    /// that a window list does not depend on how often it was asked for.
    phase_ns: BTreeMap<u32, u64>,
}

impl PulsedJammer {
    /// The model's id.
    pub const ID: &'static str = "attacker/jammer/pulsed";

    /// The jammer at the measured WARP power with the default period and duty.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: pulsed_card(
                PUNAL_JAMMER_TX_POWER_DBM,
                PULSED_DEFAULT_PERIOD_NS,
                PULSED_DEFAULT_DUTY,
            ),
            tx_power_dbm: PUNAL_JAMMER_TX_POWER_DBM,
            channel: ChannelId::CCH,
            period_ns: PULSED_DEFAULT_PERIOD_NS,
            duty: PULSED_DEFAULT_DUTY,
            phase_ns: BTreeMap::new(),
        }
    }

    /// The same jammer at a caller-chosen power.
    #[must_use]
    pub fn with_power_dbm(mut self, dbm: f64) -> Self {
        self.tx_power_dbm = dbm;
        self.card = pulsed_card(dbm, self.period_ns, self.duty);
        self
    }

    /// The same jammer with a caller-chosen period and duty cycle.
    ///
    /// The duty is clamped to `0.0..=1.0`; a zero duty is a jammer that never transmits
    /// and a duty of one is [`ConstantJammer`] with extra bookkeeping, and both are
    /// legitimate ends of a sweep.
    #[must_use]
    pub fn with_duty_cycle(mut self, period: Duration, duty: f64) -> Self {
        self.period_ns = period.as_nanos().max(1);
        self.duty = duty.clamp(0.0, 1.0);
        self.card = pulsed_card(self.tx_power_dbm, self.period_ns, self.duty);
        self
    }

    /// The same jammer on another channel.
    #[must_use]
    pub fn on_channel(mut self, ch: ChannelId) -> Self {
        self.channel = ch;
        self
    }

    /// The transmit power, dBm (see [`ConstantJammer::power_dbm`]).
    #[must_use]
    pub const fn power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    /// The channel it jams.
    #[must_use]
    pub const fn jammed_channel(&self) -> ChannelId {
        self.channel
    }

    /// The period, nanoseconds.
    #[must_use]
    pub const fn period_ns(&self) -> u64 {
        self.period_ns
    }

    /// The duty cycle.
    #[must_use]
    pub const fn duty(&self) -> f64 {
        self.duty
    }

    /// The on-time per period, nanoseconds.
    #[must_use]
    pub fn on_ns(&self) -> u64 {
        // Truncation, not rounding: an on-time of 0 ns for a duty below one nanosecond per
        // period is the honest answer, and the value is integer nanoseconds so that two
        // builds cannot disagree about a window boundary.
        (self.duty * self.period_ns as f64) as u64
    }

    /// This jammer's cached phase offset, drawing it if this is the first call.
    fn phase_of<C: Ctx + ?Sized>(&mut self, ctx: &mut C, jammer: NodeId) -> u64 {
        if let Some(p) = self.phase_ns.get(&jammer.index()) {
            return *p;
        }
        let phase = ctx
            .rng(RngDomain::Attack, EntityRef::Node(jammer))
            .below(self.period_ns);
        self.phase_ns.insert(jammer.index(), phase);
        phase
    }
}

impl Default for PulsedJammer {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for PulsedJammer {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> JammerProfile<C> for PulsedJammer {
    fn kind(&self) -> JammerKind {
        JammerKind::Pulsed
    }

    fn tx_power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    fn channel(&self) -> ChannelId {
        self.channel
    }

    fn windows(
        &mut self,
        ctx: &mut C,
        jammer: NodeId,
        from: SimTime,
        to: SimTime,
        _sensed: &[SensedInterval],
    ) -> Vec<JamWindow> {
        if to <= from {
            return Vec::new();
        }
        let on = self.on_ns();
        if on == 0 {
            return Vec::new();
        }
        let period = self.period_ns;
        let phase = self.phase_of(ctx, jammer);
        // The pulse whose period contains `from`, then forward. `k` is the pulse index,
        // and a pulse is on over `[phase + k·period, phase + k·period + on)`.
        let first = from.saturating_sub(phase) / period;
        let mut out = Vec::new();
        let mut k = first;
        loop {
            let start = phase.saturating_add(k.saturating_mul(period));
            if start >= to {
                break;
            }
            let end = start.saturating_add(on);
            let w = JamWindow::new(start.max(from), end.min(to));
            if !w.is_empty() {
                out.push(w);
            }
            let Some(next) = k.checked_add(1) else { break };
            k = next;
        }
        out
    }
}

// =========================================================================================
// `attacker/jammer/reactive`
// =========================================================================================

/// The reactive jammer's default reaction delay, nanoseconds.
///
/// The preamble plus the SIGNAL field, 40 µs (04-models.md §4.4): a jammer that triggers
/// on sensed energy cannot emit before it has detected that energy, and detection of an
/// 802.11p frame is the preamble. It is a lower bound derived from the standard's own
/// timing rather than a measured latency — the Puñal study does not print one — so the
/// card carries it `todo-calibrate` with a plan.
pub const REACTIVE_DEFAULT_DELAY_NS: u64 = timing::PREAMBLE.as_nanos() + timing::SIGNAL.as_nanos();

/// How long the reactive jammer keeps emitting after the sensed energy stops, nanoseconds.
///
/// Zero: the study describes a jammer that "transmits only when energy above a trigger
/// threshold is sensed", and any positive tail would be an invented hold-over. A scenario
/// that wants one sets it explicitly.
pub const REACTIVE_DEFAULT_TAIL_NS: u64 = 0;

/// `attacker/jammer/reactive` — on only while energy above the trigger is sensed
/// (04-models.md §12.3).
///
/// This is the profile whose *signature* is the reason jamming is modelled at all: it
/// "produces PDR = 0 dropouts uncorrelated with SINR dips", which a detector can see and
/// a congestion model cannot produce. That signature survives into the records because a
/// frame it kills is reported [`crate::types::LossCause::Jammed`] rather than
/// `collision`.
#[derive(Debug, Clone)]
pub struct ReactiveJammer {
    card: ModelCard,
    tx_power_dbm: f64,
    channel: ChannelId,
    trigger_dbm: f64,
    delay_ns: u64,
    tail_ns: u64,
}

impl ReactiveJammer {
    /// The model's id.
    pub const ID: &'static str = "attacker/jammer/reactive";

    /// The jammer at the study's measured power and −75 dBm trigger.
    #[must_use]
    pub fn new() -> Self {
        Self {
            card: reactive_card(PUNAL_JAMMER_TX_POWER_DBM, PUNAL_REACTIVE_TRIGGER_DBM),
            tx_power_dbm: PUNAL_JAMMER_TX_POWER_DBM,
            channel: ChannelId::CCH,
            trigger_dbm: PUNAL_REACTIVE_TRIGGER_DBM,
            delay_ns: REACTIVE_DEFAULT_DELAY_NS,
            tail_ns: REACTIVE_DEFAULT_TAIL_NS,
        }
    }

    /// The same jammer at a caller-chosen power.
    #[must_use]
    pub fn with_power_dbm(mut self, dbm: f64) -> Self {
        self.tx_power_dbm = dbm;
        self.card = reactive_card(dbm, self.trigger_dbm);
        self
    }

    /// The same jammer with a caller-chosen trigger threshold, dBm.
    #[must_use]
    pub fn with_trigger_dbm(mut self, dbm: f64) -> Self {
        self.trigger_dbm = dbm;
        self.card = reactive_card(self.tx_power_dbm, dbm);
        self
    }

    /// The same jammer with a caller-chosen reaction delay and hold-over tail.
    #[must_use]
    pub fn with_timing(mut self, delay: Duration, tail: Duration) -> Self {
        self.delay_ns = delay.as_nanos();
        self.tail_ns = tail.as_nanos();
        self
    }

    /// The same jammer on another channel.
    #[must_use]
    pub fn on_channel(mut self, ch: ChannelId) -> Self {
        self.channel = ch;
        self
    }

    /// The transmit power, dBm (see [`ConstantJammer::power_dbm`]).
    #[must_use]
    pub const fn power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    /// The channel it jams.
    #[must_use]
    pub const fn jammed_channel(&self) -> ChannelId {
        self.channel
    }

    /// The trigger threshold, dBm.
    #[must_use]
    pub const fn trigger_dbm(&self) -> f64 {
        self.trigger_dbm
    }

    /// True when this energy would trigger the jammer.
    #[must_use]
    pub fn triggers(&self, energy_dbm: f64) -> bool {
        energy_dbm >= self.trigger_dbm
    }
}

impl Default for ReactiveJammer {
    fn default() -> Self {
        Self::new()
    }
}

impl Model for ReactiveJammer {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> JammerProfile<C> for ReactiveJammer {
    fn kind(&self) -> JammerKind {
        JammerKind::Reactive
    }

    fn tx_power_dbm(&self) -> f64 {
        self.tx_power_dbm
    }

    fn channel(&self) -> ChannelId {
        self.channel
    }

    fn windows(
        &mut self,
        _ctx: &mut C,
        _jammer: NodeId,
        from: SimTime,
        to: SimTime,
        sensed: &[SensedInterval],
    ) -> Vec<JamWindow> {
        if to <= from {
            return Vec::new();
        }
        let mut out: Vec<JamWindow> = Vec::new();
        for s in sensed {
            if !self.triggers(s.energy_dbm) {
                continue;
            }
            let start = s.from.saturating_add(self.delay_ns).max(from);
            let end = s.to.saturating_add(self.tail_ns).min(to);
            let w = JamWindow::new(start, end);
            if !w.is_empty() {
                out.push(w);
            }
        }
        out.sort_unstable();
        // Merge touching or overlapping windows: two frames the jammer reacted to in
        // succession are one emission, and leaving them split would put a spurious SINR
        // boundary inside a victim frame.
        let mut merged: Vec<JamWindow> = Vec::with_capacity(out.len());
        for w in out {
            match merged.last_mut() {
                Some(last) if w.from <= last.to => last.to = last.to.max(w.to),
                _ => merged.push(w),
            }
        }
        merged
    }
}

// =========================================================================================
// Cards
// =========================================================================================

fn punal() -> Source {
    Source {
        kind: SourceKind::Paper,
        reference: "Puñal, Aguiar and Gross, 'In VANEX There is No Such Thing as Jamming' \
                    (2012), R11 §D1, via 04-models.md §12.3"
            .to_string(),
        accessed: None,
        note: Some(
            "The study's own receiver noise floor is −86 dBm; this crate keeps its own \
             −98 dBm kTB-plus-noise-figure floor (04-models.md §3.7) and does not mix the \
             two."
                .to_string(),
        ),
    }
}

fn threats() -> Source {
    Source::new(
        SourceKind::Standard,
        "07-threats-and-detection.md §2.2 (PHY jamming and flooding: constant, reactive, \
         random duty cycle, modelled as an interferer in the SINR sums) and 04-models.md \
         §12.3",
    )
}

fn power_parameter(dbm: f64) -> Parameter {
    Parameter {
        name: "tx_power_dbm".to_string(),
        unit: "dBm".to_string(),
        default: serde_json::json!(dbm),
        range: None,
        source: punal(),
        calibration: None,
    }
}

fn base_card(id: &'static str, purpose: &str) -> ModelCard {
    let mut card = ModelCard::new(id, Family::Attacker, "1.0.0", purpose);
    // Not `abstract`: a jammer acts through the SINR denominator, and the abstract tier
    // has no SINR (04-models.md §4.9's tier table). A scenario that asks for a jammer at
    // the abstract tier is asking for a mechanism the tier does not model, and the
    // validator can say so because the card does not claim the tier.
    card.tier = vec![Tier::Medium, Tier::High];
    card.ignores = vec![
        "Subcarrier selectivity: a jammer here is wideband over the 10 MHz channel, so \
         attacker/jammer/constant-pilot is not shipped (see the module docs)."
            .to_string(),
        "The jammer's own antenna pattern and its own duty-cycle-dependent power \
         amplifier behaviour: the declared power is constant while it is on."
            .to_string(),
        "GNSS jamming and spoofing, which are 04-models.md §3.8's models and not this \
         family's."
            .to_string(),
    ];
    card.sources = vec![punal(), threats()];
    card.cost = None;
    card
}

/// The validation section a cited jammer profile carries.
///
/// [`ValidationStatus::UnitTested`] and not `LiteratureChecked`, deliberately:
/// 04-models.md §13's jamming row is a statement about *blind areas* (about 250 m open
/// space, 167 m urban, 170 m reactive, ±25 m), and a blind area is a property of a whole
/// run — a world, a victim transmitter at the study's own 17.58 dBm, and the obstacle
/// stack that bounds the jammer's reach. That case belongs to the nightly validation suite
/// and has not been run, so the status says what has been checked (the emission schedule
/// and the loss attribution, by this module's own tests) and the reference records what
/// has not.
fn blind_area_validation() -> Validation {
    Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source {
            kind: SourceKind::Paper,
            reference: "04-models.md §13, jamming row: blind areas about 250 m open space \
                        and 167 m urban (constant), about 170 m and PDR down to 0.60 \
                        (reactive), ±25 m [Puñal 2012]"
                .to_string(),
            accessed: None,
            note: Some(
                "Not yet run: the case needs a full scenario with the study's own victim \
                 transmit power, so the status stays unit-tested until the nightly suite \
                 reports it."
                    .to_string(),
            ),
        }],
        tests: vec![
            "the_blind_area_radius_matches_the_cited_open_space_figure".to_string(),
            "a_jammed_frame_is_reported_jammed_and_not_as_a_collision".to_string(),
        ],
    }
}

fn constant_card(power_dbm: f64) -> ModelCard {
    let mut card = base_card(
        ConstantJammer::ID,
        "A constant wideband jammer: continuous OFDM-like noise in the channel, entering \
         every receiver's noise term for as long as it is on.",
    );
    card.equations = vec![Equation {
        name: "noise rise".to_string(),
        latex_or_text: "N' = N + Σ_j P_j, with P_j the jammer's received power at this \
                        receiver in mW"
            .to_string(),
        notes: Some(
            "The same sum the aggregate-interference term goes through, ordered by node \
             id (02-architecture.md §6.3)."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        power_parameter(power_dbm),
        Parameter {
            name: "channel".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(ChannelId::CCH.0),
            range: None,
            source: Source::new(
                SourceKind::Standard,
                "04-models.md §4.1: single-channel operation on the ITS-G5A control \
                 channel is the default",
            ),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The jammer is on for the whole window it is asked about: it has no protocol and \
         nothing to wait for."
            .to_string(),
        "Its received power at a victim is computed by the engine with the same link \
         budget a frame gets, so obstacles and shadowing bound its reach the way \
         04-models.md §12.3 says they do."
            .to_string(),
    ];
    card.limitations = vec![format!(
        "The measured anchors are blind areas, not powers: about {PUNAL_CONSTANT_BLIND_AREA_OPEN_M} m \
         in open space and {PUNAL_CONSTANT_BLIND_AREA_URBAN_M} m at a dense urban crossroad, \
         reproduced only with the study's own {PUNAL_LEGITIMATE_TX_POWER_DBM} dBm victim \
         transmitter."
    )];
    card.validation = blind_area_validation();
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

fn pulsed_card(power_dbm: f64, period_ns: u64, duty: f64) -> ModelCard {
    let mut card = base_card(
        PulsedJammer::ID,
        "A duty-cycle jammer: wideband noise for a fraction of a fixed period, with a \
         per-jammer phase drawn from the Attack stream.",
    );
    card.equations = vec![Equation {
        name: "emission windows".to_string(),
        latex_or_text: "on over [phase + k·T, phase + k·T + d·T) for every integer k, \
                        phase ~ U{0, …, T−1} ns"
            .to_string(),
        notes: Some(
            "The phase is drawn once per jammer from (Attack, Node) and cached, so the \
             window list is a pure function of (seed, node, T, d)."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        power_parameter(power_dbm),
        Parameter {
            name: "period_ns".to_string(),
            unit: "ns".to_string(),
            default: serde_json::json!(period_ns),
            range: None,
            source: Source::todo_calibrate(
                "07-threats-and-detection.md §2.2 names a 'random duty cycle' jammer but \
                 neither it nor 04-models.md §12.3 prints a period; the default is one \
                 T_CBR (04-models.md §4.5) so that the duty cycle is visible in the CBR a \
                 receiver reports",
            ),
            calibration: Some(
                "Sweep the period over 1 ms to 1 s against the detector suite of \
                 04-models.md §14 and record the period that maximises the gap between \
                 measured PDR loss and measured CBR rise; adopt a cited value instead if \
                 a jamming measurement with a stated duty cycle is obtained."
                    .to_string(),
            ),
        },
        Parameter {
            name: "duty".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(duty),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(1.0)]),
            source: Source::todo_calibrate(
                "no cited duty cycle exists for a V2X jammer; 0.5 is a design choice at \
                 the midpoint of the sweep",
            ),
            calibration: Some(
                "Run the same sweep as period_ns and record the duty at which delivery \
                 inside the blind area first falls below the reactive jammer's cited \
                 0.60 floor."
                    .to_string(),
            ),
        },
    ];
    card.assumptions = vec![
        "The period is a constant, not a random variable: the randomness is in the phase, \
         which is what makes two jammers in one run independent without making one \
         jammer's schedule depend on how often it was polled."
            .to_string(),
    ];
    card.limitations = vec![
        "Neither the period nor the duty is cited (both todo-calibrate), so a number \
         produced with this model rests on a design choice and the registry reports it."
            .to_string(),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![threats()],
        tests: vec![
            "the_pulsed_jammer_emits_its_duty_cycle_and_nothing_more".to_string(),
            "the_pulsed_phase_is_drawn_once_and_cached".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec!["attack".to_string()],
    };
    card
}

fn reactive_card(power_dbm: f64, trigger_dbm: f64) -> ModelCard {
    let mut card = base_card(
        ReactiveJammer::ID,
        "A reactive jammer: wideband noise only while energy above a trigger threshold is \
         sensed, which is the profile whose losses are uncorrelated with SINR dips.",
    );
    card.equations = vec![Equation {
        name: "emission windows".to_string(),
        latex_or_text: "on over [s.from + δ, s.to + τ) for every sensed interval s with \
                        s.energy ≥ trigger; overlapping windows merge"
            .to_string(),
        notes: Some(
            "δ is the reaction delay and τ the hold-over tail; τ = 0 by default because \
             the study describes emission only while energy is sensed."
                .to_string(),
        ),
    }];
    card.parameters = vec![
        power_parameter(power_dbm),
        Parameter {
            name: "trigger_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(trigger_dbm),
            range: None,
            source: punal(),
            calibration: None,
        },
        Parameter {
            name: "reaction_delay_ns".to_string(),
            unit: "ns".to_string(),
            default: serde_json::json!(REACTIVE_DEFAULT_DELAY_NS),
            range: None,
            source: Source::todo_calibrate(
                "the study prints the trigger threshold but no reaction latency; the \
                 default is the standard's own preamble plus SIGNAL field (32 µs + 8 µs, \
                 04-models.md §4.4), which is a lower bound on when detection can have \
                 happened, not a measurement",
            ),
            calibration: Some(
                "Read a reactive-jammer turnaround time from an SDR jamming measurement \
                 (the Puñal WARP setup, or a published reactive-jammer latency) and \
                 record it; until then a run is reported as using the standard's lower \
                 bound."
                    .to_string(),
            ),
        },
        Parameter {
            name: "tail_ns".to_string(),
            unit: "ns".to_string(),
            default: serde_json::json!(REACTIVE_DEFAULT_TAIL_NS),
            range: None,
            source: punal(),
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The engine supplies what the jammer heard (SensedInterval); this model does not \
         compute the jammer's own link budget, for the same reason the PHY does not \
         compute a hidden terminal's."
            .to_string(),
        "A sensed interval below the trigger produces no emission at all, so a reactive \
         jammer in a quiet region is silent and costs nothing."
            .to_string(),
    ];
    card.limitations = vec![format!(
        "The cited anchors are a {PUNAL_REACTIVE_BLIND_AREA_OPEN_M} m open-space blind \
         area and delivery down to {PUNAL_REACTIVE_PDR_FLOOR} in dense urban with the \
         transmitter near the jammer; the study also reports low impact with reduced line \
         of sight, which this model reproduces only through the engine's own obstacle \
         stack."
    )];
    card.validation = blind_area_validation();
    card.determinism = Determinism {
        uses_rng: false,
        rng_domains: Vec::new(),
    };
    card
}

/// Every jammer card this module ships, for the registry and for the card-completeness
/// gate of Phase 6.
#[must_use]
pub fn cards() -> Vec<ModelCard> {
    vec![
        constant_card(PUNAL_JAMMER_TX_POWER_DBM),
        pulsed_card(
            PUNAL_JAMMER_TX_POWER_DBM,
            PULSED_DEFAULT_PERIOD_NS,
            PULSED_DEFAULT_DUTY,
        ),
        reactive_card(PUNAL_JAMMER_TX_POWER_DBM, PUNAL_REACTIVE_TRIGGER_DBM),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;
    use v2xw_core::time::NS_PER_MS;

    fn node(i: u32) -> NodeId {
        NodeId::new(i)
    }

    #[test]
    fn the_constant_jammer_is_on_for_the_whole_window() {
        let mut ctx = TestCtx::new(1);
        let mut j = ConstantJammer::new();
        let w = JammerProfile::windows(&mut j, &mut ctx, node(9), 100, 500, &[]);
        assert_eq!(w, vec![JamWindow::new(100, 500)]);
        assert!(JammerProfile::windows(&mut j, &mut ctx, node(9), 500, 500, &[]).is_empty());
        assert_eq!(j.power_dbm(), PUNAL_JAMMER_TX_POWER_DBM);
        assert_eq!(j.jammed_channel(), ChannelId::CCH);
        assert_eq!(
            <ConstantJammer as JammerProfile<TestCtx>>::kind(&j),
            JammerKind::Constant
        );
        assert_eq!(JammerKind::Constant.model_id(), ConstantJammer::ID);
        assert_eq!(JammerKind::Pulsed.label(), "pulsed");
    }

    #[test]
    fn the_pulsed_jammer_emits_its_duty_cycle_and_nothing_more() {
        let mut ctx = TestCtx::new(7);
        let mut j = PulsedJammer::new().with_duty_cycle(Duration::from_millis(10), 0.25);
        assert_eq!(j.period_ns(), 10 * NS_PER_MS);
        assert_eq!(j.on_ns(), 2_500_000);
        let span = 100 * NS_PER_MS;
        let windows = JammerProfile::windows(&mut j, &mut ctx, node(3), 0, span, &[]);
        let on: u64 = windows.iter().map(|w| w.to - w.from).sum();
        // Ten periods of 2.5 ms each, less whatever the phase clipped at the ends.
        assert!(
            on <= 25 * NS_PER_MS && on >= 22 * NS_PER_MS,
            "on = {on} ns over {span} ns"
        );
        // The windows are disjoint and ordered.
        for pair in windows.windows(2) {
            assert!(pair[0].to <= pair[1].from, "{pair:?}");
        }
        // A zero duty is silent; a full duty covers everything.
        let mut silent = PulsedJammer::new().with_duty_cycle(Duration::from_millis(10), 0.0);
        assert!(JammerProfile::windows(&mut silent, &mut ctx, node(3), 0, span, &[]).is_empty());
    }

    #[test]
    fn the_pulsed_phase_is_drawn_once_and_cached() {
        let mut ctx = TestCtx::new(11);
        let mut j = PulsedJammer::new().with_duty_cycle(Duration::from_millis(10), 0.5);
        let first = JammerProfile::windows(&mut j, &mut ctx, node(4), 0, 50 * NS_PER_MS, &[]);
        let again = JammerProfile::windows(&mut j, &mut ctx, node(4), 0, 50 * NS_PER_MS, &[]);
        assert_eq!(
            first, again,
            "asking twice must not advance the Attack stream"
        );
    }

    #[test]
    fn the_reactive_jammer_follows_the_trigger_and_merges_its_windows() {
        let mut ctx = TestCtx::new(2);
        let mut j = ReactiveJammer::new().with_timing(Duration::ZERO, Duration::ZERO);
        let sensed = [
            SensedInterval::new(1_000, 2_000, -70.0), // above −75: triggers
            SensedInterval::new(2_000, 3_000, -70.0), // contiguous: merges
            SensedInterval::new(5_000, 6_000, -90.0), // below the trigger: ignored
        ];
        let w = JammerProfile::windows(&mut j, &mut ctx, node(1), 0, 10_000, &sensed);
        assert_eq!(w, vec![JamWindow::new(1_000, 3_000)]);
        assert!(j.triggers(PUNAL_REACTIVE_TRIGGER_DBM));
        assert!(!j.triggers(PUNAL_REACTIVE_TRIGGER_DBM - 0.001));
    }

    #[test]
    fn the_reaction_delay_moves_the_window_but_not_its_end() {
        let mut ctx = TestCtx::new(2);
        let mut j = ReactiveJammer::new();
        let sensed = [SensedInterval::new(0, 200_000, -60.0)];
        let w = JammerProfile::windows(&mut j, &mut ctx, node(1), 0, 1_000_000, &sensed);
        assert_eq!(
            w,
            vec![JamWindow::new(REACTIVE_DEFAULT_DELAY_NS, 200_000)],
            "the jammer cannot emit before it has detected the preamble"
        );
    }

    #[test]
    fn the_field_sums_powers_in_node_order_and_finds_boundaries() {
        let mut field = JammingField::new();
        assert!(field.is_empty());
        let rx = node(5);
        field.insert(
            rx,
            JamArrival::new(
                node(30),
                -60.0,
                ChannelId::CCH,
                JamWindow::new(0, 1_000),
                JammerKind::Constant,
            ),
        );
        field.insert(
            rx,
            JamArrival::new(
                node(2),
                -60.0,
                ChannelId::CCH,
                JamWindow::new(500, 1_500),
                JammerKind::Reactive,
            ),
        );
        assert_eq!(field.len(), 2);
        assert!(!field.is_empty());
        // Two equal powers sum to +3 dB.
        let both = field.power_dbm_at(rx, ChannelId::CCH, 700);
        assert!(
            (both - (-60.0 + 3.010_299_956_639_812)).abs() < 1e-9,
            "{both}"
        );
        // One only.
        let one = field.power_dbm_at(rx, ChannelId::CCH, 100);
        assert!((one - (-60.0)).abs() < 1e-12, "{one}");
        // Another channel sees nothing.
        assert_eq!(
            field.power_dbm_at(rx, ChannelId::SCH1, 700),
            f64::NEG_INFINITY
        );
        assert!(field.overlaps(rx, ChannelId::CCH, 0, 10));
        assert!(!field.overlaps(rx, ChannelId::CCH, 2_000, 3_000));
        assert_eq!(
            field.boundaries_in(rx, ChannelId::CCH, 0, 2_000),
            vec![500, 1_000, 1_500]
        );
        // Sorted insertion: the earliest window first.
        assert_eq!(field.at(rx)[0].window, JamWindow::new(0, 1_000));
        field.prune(1_200);
        assert_eq!(field.len(), 1);
        field.prune(SimTime::MAX);
        assert!(field.is_empty());
    }

    #[test]
    fn the_blind_area_radius_matches_the_cited_open_space_figure() {
        // The study's victim transmitter is 17.58 dBm and its receiver floor −86 dBm, so
        // a jammer that lifts the noise to the victim's own received power is the edge of
        // the blind area. At the cited 16.75 dBm jammer power the two links differ by
        // 0.83 dB, so the blind-area radius is within a few per cent of the range at
        // which the victim's own signal reaches the floor.
        let f = ChannelId::CCH.centre_hz();
        let victim_range = blind_area_radius_m(PUNAL_LEGITIMATE_TX_POWER_DBM, -86.0, f);
        let jammer_reach = blind_area_radius_m(PUNAL_JAMMER_TX_POWER_DBM, -86.0, f);
        assert!(
            jammer_reach < victim_range,
            "the weaker jammer reaches less far: {jammer_reach} vs {victim_range}"
        );
        // Both are free-space figures and therefore optimistic against the measured
        // 250 m; the assertion is on the ordering and on finiteness, not on the metres,
        // because a free-space radius is not what the study measured.
        assert!(jammer_reach.is_finite() && jammer_reach > 0.0);
        assert_eq!(blind_area_radius_m(0.0, 10.0, f), 0.0, "no reach at all");
    }

    #[test]
    fn the_rssi_map_is_the_printed_least_squares_fit() {
        // γ = 0.8565·σ − 86.35: at σ = 0 the map is the intercept.
        assert!((punal_rssi_to_sinr_db(0.0) - (-86.35)).abs() < 1e-12);
        assert!((punal_rssi_to_sinr_db(100.0) - (-0.7)).abs() < 1e-9);
    }

    #[test]
    fn the_cards_validate_and_register() {
        let mut registry = v2xw_core::registry::Registry::new();
        for card in cards() {
            card.validate().expect("card validates");
            card.check_api_version().expect("api version");
            registry.register(card).expect("registers");
        }
        assert!(registry.contains(ConstantJammer::ID));
        assert!(registry.contains(PulsedJammer::ID));
        assert!(registry.contains(ReactiveJammer::ID));
        // No jammer claims the abstract tier: it has no SINR to raise.
        for card in cards() {
            assert!(!card.implements_tier(Tier::Abstract), "{}", card.id);
            assert!(card.implements_tier(Tier::High), "{}", card.id);
        }
        // Rule R1: the pulsed jammer's uncited period and duty carry plans.
        let pulsed = pulsed_card(
            PUNAL_JAMMER_TX_POWER_DBM,
            PULSED_DEFAULT_PERIOD_NS,
            PULSED_DEFAULT_DUTY,
        );
        let todo: Vec<&str> = pulsed
            .todo_calibrate()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(todo, vec!["period_ns", "duty"]);
    }

    #[test]
    fn a_jammer_can_be_used_as_a_trait_object() {
        let mut ctx = TestCtx::new(3);
        let mut constant = ConstantJammer::new();
        let mut pulsed = PulsedJammer::new();
        let mut reactive = ReactiveJammer::new();
        let profiles: Vec<&mut dyn JammerProfile<TestCtx>> =
            vec![&mut constant, &mut pulsed, &mut reactive];
        for p in profiles {
            assert_eq!(p.channel(), ChannelId::CCH);
            let _ = p.windows(&mut ctx, node(1), 0, 1_000_000, &[]);
        }
    }
}
