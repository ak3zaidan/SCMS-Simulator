//! The IEEE 802.11p physical layer: `phy/80211p/ofdm-10mhz` (04-models.md §4.2, §4.7,
//! §4.8) and the receiver parameters of §3.7.
//!
//! What this module owns:
//!
//! * **Air time** ([`air_time`]), exact from the OFDM parameters: the 32 µs preamble, the
//!   8 µs SIGNAL field, and one 8 µs symbol per `N_DBPS` bits of
//!   `N_SERVICE + 8·bytes + N_TAIL`.
//! * **The receiver** ([`OfdmPhy::noise_floor_dbm`], [`SensitivityPreset`]): thermal noise
//!   −104 dBm in 10 MHz plus a noise figure, and the EN 302 663 sensitivity table.
//! * **SINR** ([`sinr_db`]), with the interference sum reduced in id order.
//! * **Frame outcomes** ([`OfdmPhy::finish_rx`]): half duplex, sensitivity, preamble
//!   capture, per-window SINR against the NIST PER model, and exactly one
//!   [`LossCause`] per lost frame (invariant I-R3).
//! * **Accounting** ([`AirtimeLedger`]): every transmission's air time counted once at the
//!   transmitter, and every arrival's air time attributed to exactly one outcome bucket at
//!   the receiver — the second half of I-R3.
//!
//! # What the engine does and what this does
//!
//! This crate has no event loop and no pairwise loss matrix, so the PHY is driven rather
//! than driving: the engine computes the received power of an arrival (with
//! [`crate::prop`], [`crate::obstacle`] and [`crate::fading`]) and registers it with
//! [`OfdmPhy::register_arrival`]; it declares its own overlaps with
//! [`InterferenceSource`]; it calls [`OfdmPhy::finish_rx`] at the arrival's end. The PHY
//! keeps the per-node state that makes those calls answerable — the live arrival set, the
//! node's own transmissions, the busy intervals — and nothing else.
//!
//! # The RNG domain of a frame error
//!
//! ADR 0004 §3's built-in domain list has no frame-error domain. Rather than borrow
//! `AbstractRx`, whose name promises the abstract tier, the per-frame Bernoulli draw comes
//! from `RngDomain::plugin("phy/80211p/ofdm-10mhz")`, the plug-in domain the core derives
//! from a model id as `0x8000_0000 | (LE_u32(SHA-256(id)[0..4]) >> 1)`. The high bit is
//! always set, so it can never collide with a built-in code, and it is keyed by
//! `EntityRef::LinkFrame { link, frame }` so the draw for one arrival does not depend on
//! how many other receivers were evaluated first (invariant I-R2). Adding a `FrameError`
//! variant to the core's domain list is a one-line change and is recorded on the card.

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
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::{Duration, SimTime};

use crate::error::{RadioError, Result};
use crate::jamming::{JamArrival, JammingField};
use crate::numeric;
use crate::per::PerModel;
use crate::traits::Phy;
use crate::types::{
    CcaState, ChannelId, FrameDescriptor, LossCause, Mcs, Rat, RxHandle, RxOutcome, TxHandle,
    timing,
};

/// Thermal noise in a 10 MHz channel: `−174 + 10·log10(10^7) = −104 dBm`
/// [kTB, R3 §F.2, 04-models.md §3.7].
pub const THERMAL_NOISE_10MHZ_DBM: f64 = -104.0;

/// The noise figure of the NXP SAF5400 automotive 802.11p radio, 6 dB
/// [NXP SAF5400 fact sheet, R3 §F.2]. With the thermal floor this gives −98 dBm.
pub const NOISE_FIGURE_HARDWARE_DB: f64 = 6.0;

/// The noise figure 3GPP evaluations assume, 9 dB [TR 36.885 Annex A.1.1, R2c]. With the
/// thermal floor this gives −95 dBm.
pub const NOISE_FIGURE_3GPP_DB: f64 = 9.0;

/// The CBR busy threshold, −85 dBm [EN 302 571 §4.2.10.1, 04-models.md §4.5].
pub const CBR_BUSY_THRESHOLD_DBM: f64 = -85.0;

/// The capture threshold Torrent-Moreno used for 3 Mbit/s BPSK 1/2, 5 dB
/// [Torrent-Moreno 2009 Eq. 1, R1 §B.4].
pub const CAPTURE_THRESHOLD_DB: f64 = 5.0;

/// The frame-error [`RngDomain`], derived once.
///
/// `RngDomain::plugin` runs SHA-256 over the model id, and the id is a constant: deriving
/// it inside `evaluate` cost 0.18 µs of the 2.35 µs a full `finish_rx` takes — 7.7 % of
/// the per-arrival cost — for a value that never changes. The domain is a pure function of
/// the id, so caching it changes no draw.
static FRAME_ERROR_DOMAIN: std::sync::LazyLock<RngDomain> =
    std::sync::LazyLock::new(|| RngDomain::plugin(OfdmPhy::ID));

/// Exact air time for a frame, per 04-models.md §4.2:
/// `T = 32 µs + 8 µs + 8 µs · ceil((N_SERVICE + 8·bytes + N_TAIL) / N_DBPS)`.
#[must_use]
pub fn air_time(bytes: u32, mcs: Mcs) -> Duration {
    let bits = crate::per::data_field_bits(bytes);
    let dbps = u64::from(mcs.data_bits_per_symbol());
    let symbols = bits.div_ceil(dbps);
    Duration::from_nanos(
        timing::PREAMBLE.as_nanos()
            + timing::SIGNAL.as_nanos()
            + timing::SYMBOL.as_nanos() * symbols,
    )
}

/// The number of OFDM data symbols a frame occupies.
#[must_use]
pub fn data_symbols(bytes: u32, mcs: Mcs) -> u64 {
    crate::per::data_field_bits(bytes).div_ceil(u64::from(mcs.data_bits_per_symbol()))
}

/// The propagation delay over a distance: `d / c`, 1 µs per 300 m (04-models.md §4.4).
#[must_use]
pub fn propagation_delay(d_m: f64) -> Duration {
    Duration::from_nanos((d_m.max(0.0) / numeric::SPEED_OF_LIGHT_M_S * 1e9) as u64)
}

/// The signal-to-interference-and-noise ratio, dB.
///
/// `SINR = P_rx − 10·log10(N + Σ I_k)` with the interferers summed in id order by
/// [`crate::numeric::sum_powers_mw`] (02-architecture.md §6.3). An empty interferer set
/// gives the plain SNR.
#[must_use]
pub fn sinr_db(rx_dbm: f64, interferers: &[(NodeId, f64)], noise_dbm: f64) -> f64 {
    let signal_mw = numeric::dbm_to_mw(rx_dbm);
    let noise_mw = numeric::dbm_to_mw(noise_dbm);
    let interference_mw = numeric::sum_powers_mw(interferers);
    numeric::mw_to_dbm(signal_mw) - numeric::mw_to_dbm(noise_mw + interference_mw)
}

/// Which receiver-sensitivity table the PHY enforces (04-models.md §3.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SensitivityPreset {
    /// EN 302 663 Table 1, the standards-exact minimum. The default.
    #[default]
    EtsiStatic,
    /// EN 302 663 Table 2, the interference-present figures: 3 dB above the static ones.
    EtsiDynamic,
    /// Cohda MK5 module datasheet Table 2: about 5-7 dB better than the ETSI minimum.
    CohdaMk5,
}

impl SensitivityPreset {
    /// The sensitivity for one MCS, dBm.
    #[must_use]
    pub const fn sensitivity_dbm(self, mcs: Mcs) -> f64 {
        match self {
            SensitivityPreset::EtsiStatic => mcs.sensitivity_static_dbm(),
            SensitivityPreset::EtsiDynamic => mcs.sensitivity_dynamic_dbm(),
            SensitivityPreset::CohdaMk5 => mcs.sensitivity_cohda_mk5_dbm(),
        }
    }

    /// The preset's id as a scenario spells it.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            SensitivityPreset::EtsiStatic => "etsi-static",
            SensitivityPreset::EtsiDynamic => "etsi-dynamic",
            SensitivityPreset::CohdaMk5 => "cohda-mk5",
        }
    }
}

/// How the PHY decides whether a frame survives its overlaps (04-models.md §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureRule {
    /// Per-window SINR against the error model, the way Veins does it. The default: no
    /// separate scalar is needed, because the error model already decides.
    #[default]
    SinrTrace,
    /// A hard threshold: the frame is decodable only while its power exceeds the
    /// cumulative interference plus noise by `cp_th_db` throughout reception
    /// [Torrent-Moreno 2009 Eq. 1].
    Threshold {
        /// The capture threshold, dB.
        cp_th_db: f64,
    },
}

/// The CCA thresholds of one PHY instance (04-models.md §4.5).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CcaConfig {
    /// The threshold above which received signal strength makes the channel busy for the
    /// CBR measurement, dBm. Default −85 dBm [EN 302 571 §4.2.10.1].
    pub busy_dbm: f64,
    /// The energy-detect threshold, when a preset distinguishes it. `None` means the same
    /// threshold as `busy_dbm`, which is what the ETSI default does.
    pub energy_detect_dbm: Option<f64>,
}

impl CcaConfig {
    /// The ETSI default: the CBR busy threshold, −85 dBm, used for CCA as well.
    ///
    /// The card states what this conflates: CCA-ED and the CBR threshold are related but
    /// are not defined as the same parameter by the standards (04-models.md §4.5).
    pub const ETSI: CcaConfig = CcaConfig {
        busy_dbm: CBR_BUSY_THRESHOLD_DBM,
        energy_detect_dbm: None,
    };
    /// The ns-3 preset: CcaSensitivity −82 dBm, CcaEdThreshold −62 dBm.
    pub const NS3: CcaConfig = CcaConfig {
        busy_dbm: -82.0,
        energy_detect_dbm: Some(-62.0),
    };
    /// The Veins preset: `ccaThreshold` −65 dBm.
    pub const VEINS: CcaConfig = CcaConfig {
        busy_dbm: -65.0,
        energy_detect_dbm: None,
    };

    /// The threshold that decides CCA busy, dBm.
    #[must_use]
    pub fn cca_threshold_dbm(&self) -> f64 {
        self.energy_detect_dbm.unwrap_or(self.busy_dbm)
    }
}

/// One overlapping transmission at a receiver, as the engine declares it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InterferenceSource {
    /// The interfering transmitter.
    pub node: NodeId,
    /// Its received power at this receiver, dBm.
    pub power_dbm: f64,
    /// When its energy starts arriving.
    pub start: SimTime,
    /// When it stops.
    pub end: SimTime,
    /// Whether the victim frame's *transmitter* could sense this interferer when it
    /// started transmitting.
    ///
    /// `Some(false)` is the hidden-terminal case: CSMA could not have prevented the
    /// overlap, and a frame lost with such an interferer present is reported
    /// [`LossCause::HiddenTerminal`] rather than [`LossCause::Collision`]. Only the engine
    /// knows this — it owns the pairwise link budget — so the PHY takes it as input.
    /// `None` means "not determined", and the loss is reported as a plain collision.
    ///
    /// 04-models.md §4.8 phrases the condition as the collider being "outside the
    /// receiver's CCA range"; the mechanism is about the *transmitter's* range, since that
    /// is the node whose carrier sense failed, and this field follows the mechanism. The
    /// card records the divergence.
    pub audible_to_victim_tx: Option<bool>,
}

impl InterferenceSource {
    /// An interferer of `power_dbm` overlapping `[start, end)`, with the hidden-terminal
    /// question left undetermined.
    #[must_use]
    pub const fn new(node: NodeId, power_dbm: f64, start: SimTime, end: SimTime) -> Self {
        Self {
            node,
            power_dbm,
            start,
            end,
            audible_to_victim_tx: None,
        }
    }

    /// The same interferer, marked as hidden from the victim's transmitter.
    #[must_use]
    pub const fn hidden(mut self) -> Self {
        self.audible_to_victim_tx = Some(false);
        self
    }
}

/// One arrival at one receiver, from the preamble to the last symbol.
#[derive(Debug, Clone, PartialEq)]
pub struct Arrival {
    /// The transmission this belongs to.
    pub tx_id: u64,
    /// The transmitter.
    pub tx: NodeId,
    /// The receiver.
    pub rx: NodeId,
    /// Received power, dBm, after path loss, shadowing, obstacles, fading and antennas.
    pub power_dbm: f64,
    /// When the preamble starts arriving.
    pub start: SimTime,
    /// When the last symbol has arrived: `start + air_time`.
    pub end: SimTime,
    /// The frame's descriptor, for the size, MCS and channel.
    pub frame: FrameDescriptor,
    /// Overlapping transmissions, as the engine declared them.
    pub interferers: Vec<InterferenceSource>,
}

impl Arrival {
    /// The air time of the arrival.
    #[must_use]
    pub fn air_time(&self) -> Duration {
        Duration::between(self.start, self.end)
    }

    /// The link this arrival is on.
    #[must_use]
    pub const fn link(&self) -> LinkKey {
        LinkKey(self.tx, self.rx)
    }
}

/// Airtime accounting: invariant I-R3's second half.
///
/// Two ledgers, because there are two questions. `transmitted` is what went on the air,
/// counted once per transmission. The outcome buckets are what happened at receivers,
/// counted once per arrival — and every arrival lands in exactly one bucket, so the
/// buckets sum to the total arrival air time and nothing is lost or double-counted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AirtimeLedger {
    /// Air time this PHY put on the air, per transmitting node, nanoseconds.
    pub transmitted_ns: BTreeMap<u32, u64>,
    /// Arrival air time that was decoded, nanoseconds.
    pub received_ns: u64,
    /// Arrival air time that was lost, by cause, nanoseconds. The key is
    /// [`LossCause::label`], so the map is stable and readable in a dump.
    pub lost_ns: BTreeMap<String, u64>,
}

impl AirtimeLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts a transmission.
    pub fn note_tx(&mut self, node: NodeId, air_time: Duration) {
        *self.transmitted_ns.entry(node.index()).or_insert(0) += air_time.as_nanos();
    }

    /// Counts one arrival's outcome.
    pub fn note_rx(&mut self, outcome: &RxOutcome, air_time: Duration) {
        match outcome {
            RxOutcome::Received { .. } => self.received_ns += air_time.as_nanos(),
            RxOutcome::Lost(cause) => {
                *self.lost_ns.entry(cause.label().to_string()).or_insert(0) += air_time.as_nanos();
            }
        }
    }

    /// Total air time transmitted by all nodes, nanoseconds.
    #[must_use]
    pub fn transmitted_total_ns(&self) -> u64 {
        self.transmitted_ns.values().copied().sum()
    }

    /// Total arrival air time accounted for, nanoseconds: received plus every loss
    /// bucket.
    #[must_use]
    pub fn accounted_total_ns(&self) -> u64 {
        self.received_ns + self.lost_ns.values().copied().sum::<u64>()
    }
}

/// One live transmission at this PHY.
#[derive(Debug, Clone, PartialEq)]
struct Transmission {
    handle: TxHandle,
    frame: FrameDescriptor,
}

/// A busy interval on one node's channel, for the CBR measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct BusyInterval {
    /// When the medium became busy.
    pub from: SimTime,
    /// When it became idle again.
    pub to: SimTime,
}

/// `phy/80211p/ofdm-10mhz` — the 802.11p physical layer at the medium and high tiers.
#[derive(Debug)]
pub struct OfdmPhy {
    card: ModelCard,
    tier: Tier,
    per: PerModel,
    sensitivity: SensitivityPreset,
    cca: CcaConfig,
    capture: CaptureRule,
    noise_figure_db: f64,
    next_tx_id: u64,
    /// Live and recent transmissions, by id.
    transmissions: BTreeMap<u64, Transmission>,
    /// Live arrivals, by `(receiver, transmission id)` — a `BTreeMap`, so an iteration
    /// over the arrival set is in id order and can never leak a hash order into an
    /// outcome.
    arrivals: BTreeMap<(u32, u64), Arrival>,
    /// The interval each node is transmitting for, for the half-duplex test.
    tx_intervals: BTreeMap<u32, Vec<BusyInterval>>,
    /// Deliberate interference, per receiver, as the engine declares it
    /// ([`crate::jamming`]). Empty in every run without a jammer, and every jamming query
    /// short-circuits on that, so an unjammed run pays nothing for this field.
    jamming: JammingField,
    /// The air-time ledger.
    ledger: AirtimeLedger,
}

impl OfdmPhy {
    /// The model's id.
    pub const ID: &'static str = "phy/80211p/ofdm-10mhz";

    /// The PHY at `tier` with the ETSI defaults: static sensitivity, −85 dBm CCA, the
    /// hardware noise figure, the ideal NIST error model.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self {
            card: phy_card(SensitivityPreset::EtsiStatic, 0.0),
            tier,
            per: PerModel::default(),
            sensitivity: SensitivityPreset::EtsiStatic,
            cca: CcaConfig::ETSI,
            capture: CaptureRule::default(),
            noise_figure_db: NOISE_FIGURE_HARDWARE_DB,
            next_tx_id: 1,
            transmissions: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            tx_intervals: BTreeMap::new(),
            jamming: JammingField::new(),
            ledger: AirtimeLedger::new(),
        }
    }

    /// The PHY with a caller-chosen error model (the `sjoberg-atheros` preset, say).
    #[must_use]
    pub fn with_per_model(mut self, per: PerModel) -> Self {
        self.card = phy_card(self.sensitivity, per.rx_impl_loss_db());
        self.per = per;
        self
    }

    /// The PHY with a caller-chosen sensitivity table.
    #[must_use]
    pub fn with_sensitivity(mut self, preset: SensitivityPreset) -> Self {
        self.sensitivity = preset;
        self.card = phy_card(preset, self.per.rx_impl_loss_db());
        self
    }

    /// The PHY with caller-chosen CCA thresholds.
    #[must_use]
    pub fn with_cca(mut self, cca: CcaConfig) -> Self {
        self.cca = cca;
        self
    }

    /// The PHY with the hard capture threshold instead of the SINR trace.
    #[must_use]
    pub fn with_capture(mut self, capture: CaptureRule) -> Self {
        self.capture = capture;
        self
    }

    /// The PHY with a caller-chosen noise figure (3GPP's 9 dB, say).
    #[must_use]
    pub fn with_noise_figure_db(mut self, nf: f64) -> Self {
        self.noise_figure_db = nf;
        self
    }

    /// The error model in use.
    #[must_use]
    pub const fn per_model(&self) -> &PerModel {
        &self.per
    }

    /// The CCA configuration in use.
    #[must_use]
    pub const fn cca_config(&self) -> CcaConfig {
        self.cca
    }

    /// The air-time ledger.
    #[must_use]
    pub const fn ledger(&self) -> &AirtimeLedger {
        &self.ledger
    }

    /// The receiver sensitivity for one MCS, dBm.
    #[must_use]
    pub fn sensitivity_dbm(&self, mcs: Mcs) -> f64 {
        self.sensitivity.sensitivity_dbm(mcs)
    }

    /// The noise floor, dBm: thermal noise in 10 MHz plus the noise figure.
    ///
    /// The inherent form of [`Phy::noise_floor_dbm`], which the trait method delegates
    /// to. It exists because the trait is generic over the context type, so calling the
    /// trait method from inside this crate's own code would need a turbofish at every
    /// call site for a value that does not depend on the context at all.
    #[must_use]
    pub fn noise_floor(&self) -> f64 {
        THERMAL_NOISE_10MHZ_DBM + self.noise_figure_db
    }

    /// The deliberate interference declared at this PHY's receivers ([`crate::jamming`]).
    #[must_use]
    pub fn jamming(&self) -> &JammingField {
        &self.jamming
    }

    /// The jamming field, to declare a jammer's energy at a receiver.
    ///
    /// The engine owns the jammer's link budget — it computes a jammer's received power
    /// the same way it computes a frame's — so the field is filled from outside, exactly
    /// as [`InterferenceSource`]s are.
    pub fn jamming_mut(&mut self) -> &mut JammingField {
        &mut self.jamming
    }

    /// Declares one jamming arrival at one receiver.
    ///
    /// Shorthand for `self.jamming_mut().insert(rx, arrival)`.
    pub fn note_jamming(&mut self, rx: NodeId, arrival: JamArrival) {
        self.jamming.insert(rx, arrival);
    }

    /// The noise power at a receiver over one window, milliwatts: the thermal floor plus
    /// whatever jamming is present.
    ///
    /// This is 04-models.md §12.3's whole mechanism — "jammers ... enter the interference
    /// sums of §4-5 as transmitters" — in one function. With no jammer in range it is the
    /// plain noise floor, bit for bit, which is why an unjammed run's outcomes are
    /// unchanged by the existence of this path.
    #[must_use]
    pub fn noise_mw_during(&self, rx: NodeId, ch: ChannelId, from: SimTime, to: SimTime) -> f64 {
        let thermal = numeric::dbm_to_mw(self.noise_floor());
        if self.jamming.is_empty() {
            return thermal;
        }
        thermal + self.jamming.power_mw(rx, ch, from, to)
    }

    /// The noise power at a receiver over one window, dBm — the effective noise floor the
    /// SINR of that window is computed against.
    ///
    /// Returns [`OfdmPhy::noise_floor`] **exactly**, bit for bit, when no jamming overlaps
    /// the window. That is not an optimisation: `mw_to_dbm(dbm_to_mw(x))` is not the
    /// identity in floating point, and a round trip through it on every window would move
    /// every SINR in the workspace by a fraction of a last bit — enough to shift a frame
    /// across a PER threshold and to change every golden digest in a run that has no
    /// jammer in it.
    #[must_use]
    pub fn noise_dbm_during(&self, rx: NodeId, ch: ChannelId, from: SimTime, to: SimTime) -> f64 {
        let floor = self.noise_floor();
        if self.jamming.is_empty() {
            return floor;
        }
        let jam_mw = self.jamming.power_mw(rx, ch, from, to);
        if jam_mw <= 0.0 {
            return floor;
        }
        numeric::mw_to_dbm(numeric::dbm_to_mw(floor) + jam_mw)
    }

    /// How many decibels of noise rise the jamming at a receiver accounts for over a
    /// window: `noise_with − noise_without`, non-negative.
    ///
    /// For the inspector and for the `phy.rx` breakdown: it is the number that tells a
    /// reader whether a loss was a jammer's doing, and it is reported rather than used —
    /// the loss *cause* is decided by the counterfactual in [`OfdmPhy::finish_rx`], not by
    /// a threshold on this.
    #[must_use]
    pub fn jamming_rise_db(&self, rx: NodeId, ch: ChannelId, from: SimTime, to: SimTime) -> f64 {
        if self.jamming.is_empty() {
            return 0.0;
        }
        let with = self.noise_mw_during(rx, ch, from, to);
        let without = numeric::dbm_to_mw(self.noise_floor());
        if without <= 0.0 || with <= without {
            return 0.0;
        }
        numeric::mw_to_dbm(with) - numeric::mw_to_dbm(without)
    }

    /// Registers an arrival at a receiver and returns its handle.
    ///
    /// The engine calls this after it has computed the received power with the
    /// propagation, obstacle and fading models. It is separate from `begin_tx` because
    /// only the engine knows who is in range and what the loss to each of them is.
    pub fn register_arrival(&mut self, arrival: Arrival) -> RxHandle {
        let handle = RxHandle {
            tx: arrival.tx_id,
            rx: arrival.rx,
        };
        self.arrivals
            .insert((arrival.rx.index(), arrival.tx_id), arrival);
        handle
    }

    /// Adds an interferer to a registered arrival.
    ///
    /// # Errors
    ///
    /// [`RadioError::UnknownArrival`] when no such arrival is registered.
    pub fn add_interferer(&mut self, h: RxHandle, source: InterferenceSource) -> Result<()> {
        let arrival =
            self.arrivals
                .get_mut(&(h.rx.index(), h.tx))
                .ok_or(RadioError::UnknownArrival {
                    tx: h.tx,
                    rx: h.rx.index(),
                })?;
        arrival.interferers.push(source);
        Ok(())
    }

    /// A registered arrival, for inspection.
    #[must_use]
    pub fn arrival(&self, h: RxHandle) -> Option<&Arrival> {
        self.arrivals.get(&(h.rx.index(), h.tx))
    }

    /// The instantaneous received energy at a node on a channel, dBm, and the arrivals
    /// that make it up.
    ///
    /// Sums in id order, like every other power reduction in the crate.
    #[must_use]
    pub fn energy_dbm(&self, node: NodeId, ch: ChannelId, at: SimTime) -> f64 {
        let contributors: Vec<(NodeId, f64)> = self
            .arrivals
            .values()
            .filter(|a| a.rx == node && a.frame.channel == ch && a.start <= at && at < a.end)
            .map(|a| (a.tx, numeric::dbm_to_mw(a.power_dbm)))
            .collect();
        // A jammer's energy is energy: it makes carrier sense report a busy medium and it
        // shows up in the channel busy ratio, which is exactly why telling a jammed frame
        // from a congested one needs the loss cause rather than the CBR
        // (04-models.md §12.3).
        let jam_mw = if self.jamming.is_empty() {
            0.0
        } else {
            self.jamming.power_mw(node, ch, at, at.saturating_add(1))
        };
        if contributors.is_empty() {
            if jam_mw <= 0.0 {
                return f64::NEG_INFINITY;
            }
            return numeric::mw_to_dbm(jam_mw);
        }
        let arrivals_mw = numeric::sum_powers_mw(&contributors);
        if jam_mw <= 0.0 {
            return numeric::mw_to_dbm(arrivals_mw);
        }
        numeric::mw_to_dbm(arrivals_mw + jam_mw)
    }

    /// How many transmit intervals are currently retained for a node.
    ///
    /// For inspection and for the test that pins the bound: see
    /// [`TX_INTERVAL_RETENTION_NS`].
    #[must_use]
    pub fn tx_interval_count(&self, node: NodeId) -> usize {
        self.tx_intervals.get(&node.index()).map_or(0, Vec::len)
    }

    /// True when this node is transmitting at `at` (802.11p is half duplex).
    #[must_use]
    pub fn is_transmitting(&self, node: NodeId, at: SimTime) -> bool {
        self.tx_intervals
            .get(&node.index())
            .is_some_and(|v| v.iter().any(|i| i.from <= at && at < i.to))
    }

    /// True when this node is transmitting at any instant of `[from, to)`.
    #[must_use]
    pub fn transmits_during(&self, node: NodeId, from: SimTime, to: SimTime) -> bool {
        self.tx_intervals
            .get(&node.index())
            .is_some_and(|v| v.iter().any(|i| i.from < to && from < i.to))
    }

    /// The SINR windows of one arrival: `(start, end, sinr_db)` for every interval over
    /// which the interferer set is constant.
    ///
    /// The boundaries are the interferer starts and ends clipped to the arrival, in
    /// integer nanoseconds, so the partition is exact and identical on every platform.
    #[must_use]
    pub fn sinr_windows(&self, arrival: &Arrival) -> Vec<(SimTime, SimTime, f64)> {
        self.sinr_windows_with(arrival, true)
    }

    /// The SINR windows of one arrival, with the declared jamming either counted or
    /// removed.
    ///
    /// `with_jamming = false` is the **counterfactual** the jamming loss cause is decided
    /// on (see [`crate::jamming`]): the same arrival, the same interferers, the same
    /// window partition, and the thermal noise floor alone in the denominator. Comparing
    /// the two against one uniform draw is what makes
    /// [`crate::types::LossCause::Jammed`] exact rather than a heuristic, and is why the
    /// window boundaries are computed from the union of the interferer *and* jamming edges
    /// in both cases: the partition has to be the same one, or the two evaluations would
    /// not be comparable.
    #[must_use]
    pub fn sinr_windows_with(
        &self,
        arrival: &Arrival,
        with_jamming: bool,
    ) -> Vec<(SimTime, SimTime, f64)> {
        let floor = self.noise_floor();
        let mut bounds: Vec<SimTime> = vec![arrival.start, arrival.end];
        for i in &arrival.interferers {
            for t in [i.start, i.end] {
                if t > arrival.start && t < arrival.end {
                    bounds.push(t);
                }
            }
        }
        // A jammer that switches on or off inside the frame splits it exactly as a
        // mid-frame interferer does. Included whatever `with_jamming` says, so that the
        // counterfactual is evaluated over the same partition.
        if !self.jamming.is_empty() {
            bounds.extend(self.jamming.boundaries_in(
                arrival.rx,
                arrival.frame.channel,
                arrival.start,
                arrival.end,
            ));
        }
        bounds.sort_unstable();
        bounds.dedup();
        let mut windows = Vec::with_capacity(bounds.len().saturating_sub(1));
        for w in bounds.windows(2) {
            let (from, to) = (w[0], w[1]);
            if to <= from {
                continue;
            }
            let active: Vec<(NodeId, f64)> = arrival
                .interferers
                .iter()
                .filter(|i| i.start < to && from < i.end)
                .map(|i| (i.node, numeric::dbm_to_mw(i.power_dbm)))
                .collect();
            let noise = if with_jamming {
                self.noise_dbm_during(arrival.rx, arrival.frame.channel, from, to)
            } else {
                floor
            };
            windows.push((from, to, sinr_db(arrival.power_dbm, &active, noise)));
        }
        windows
    }

    /// The probability that an arrival is decoded, from its SINR windows.
    ///
    /// The SIGNAL field's 24 bits are spread over the 8 µs after the preamble and the
    /// DATA field's bits over the rest, each apportioned across the windows by time; the
    /// survival probabilities multiply. This is the "per symbol-group SINR windows over
    /// the arrival set" of 04-models.md §4.8, and it reduces to the single-SINR PER of
    /// §4.7 exactly when the interferer set does not change during the frame.
    #[must_use]
    pub fn success_probability(&self, arrival: &Arrival) -> f64 {
        self.success_probability_with(arrival, true)
    }

    /// The probability that an arrival is decoded, with the declared jamming either
    /// counted or removed — the counterfactual of [`OfdmPhy::sinr_windows_with`].
    #[must_use]
    pub fn success_probability_with(&self, arrival: &Arrival, with_jamming: bool) -> f64 {
        let windows = self.sinr_windows_with(arrival, with_jamming);
        if windows.is_empty() {
            return 0.0;
        }
        let signal_from = arrival.start + timing::PREAMBLE.as_nanos();
        let signal_to = signal_from + timing::SIGNAL.as_nanos();
        let data_from = signal_to;
        let data_to = arrival.end.max(data_from);
        let data_bits = crate::per::data_field_bits(arrival.frame.bytes) as f64;
        let signal_bits = f64::from(timing::SIGNAL_BITS);
        let mut psr = 1.0;
        for (from, to, sinr) in windows {
            let signal_overlap = overlap_ns(from, to, signal_from, signal_to) as f64;
            if signal_overlap > 0.0 {
                let share = signal_overlap / (signal_to - signal_from) as f64;
                psr *= PerModel::survival(self.per.pe_signal(sinr), signal_bits * share);
            }
            let data_overlap = overlap_ns(from, to, data_from, data_to) as f64;
            if data_overlap > 0.0 && data_to > data_from {
                let share = data_overlap / (data_to - data_from) as f64;
                psr *= PerModel::survival(
                    self.per.pe_data(arrival.frame.mcs, sinr),
                    data_bits * share,
                );
            }
        }
        psr.clamp(0.0, 1.0)
    }

    /// Whether the receiver can lock this arrival's preamble.
    ///
    /// A receiver locks an arrival only when it is idle, or when the new preamble exceeds
    /// the frame it is locked to by the capture threshold (04-models.md §4.8). "Locked
    /// to" means the signal with the earliest start among those live when this one
    /// begins; ties go to the stronger signal, then to the lower transmitter id.
    ///
    /// # Why the candidates come from the interferer list
    ///
    /// The incumbent set is [`Arrival::interferers`] — what the engine *declared*
    /// overlaps this arrival — and not the live [`OfdmPhy::arrivals`] map. Two reasons,
    /// and they are both defects this replaced:
    ///
    /// 1. **Determinism (invariant I-R2).** The map loses an arrival as soon as its own
    ///    `finish_rx` runs, so scanning it made the answer depend on the order receivers
    ///    were evaluated in: the same two frames gave `PreambleMissed` or `Collision`
    ///    purely on which was finished first, and an engine driven by end-of-frame events
    ///    finishes the shorter frame first, which silently disabled capture against every
    ///    incumbent that ends before the challenger. The interferer list is fixed when the
    ///    arrival is registered and never mutated by an evaluation, so the answer is a
    ///    pure function of the registered geometry.
    /// 2. **One source of truth.** [`OfdmPhy::sinr_windows`] already reads the interferer
    ///    list, so scanning the map here gave the PHY two different notions of what else
    ///    was on the air at one receiver: an arrival the engine registered but did not
    ///    declare as an interferer blocked the lock without contributing to the SINR, and
    ///    the reverse.
    ///
    /// # The detection floor
    ///
    /// A signal the receiver could not detect cannot hold it: candidates below
    /// [`OfdmPhy::preamble_detection_dbm`] are skipped. That floor is the sensitivity of
    /// the *base rate*, not of the incumbent's own MCS, because the preamble and the
    /// SIGNAL field are BPSK 1/2 whatever the DATA field is (EN 302 663 Annex C.3, which
    /// is also why [`crate::per::PerModel::pe_signal`] takes no MCS). So an incumbent
    /// that will fail its own sensitivity test at a higher MCS can still hold the
    /// receiver — the preamble was detectable even though the data was not — while an
    /// arrival nothing could have detected no longer blocks anything.
    #[must_use]
    pub fn preamble_locked(&self, arrival: &Arrival) -> bool {
        let threshold = match self.capture {
            CaptureRule::Threshold { cp_th_db } => cp_th_db,
            CaptureRule::SinrTrace => CAPTURE_THRESHOLD_DB,
        };
        let floor = self.preamble_detection_dbm();
        let mut incumbent: Option<&InterferenceSource> = None;
        for other in &arrival.interferers {
            // A node cannot interfere with its own frame: 802.11p is half duplex.
            if other.node == arrival.tx {
                continue;
            }
            // Live when this arrival's preamble begins, and started before it.
            if other.start > arrival.start || other.end <= arrival.start {
                continue;
            }
            // Never detected, so never locked.
            if other.power_dbm < floor {
                continue;
            }
            incumbent = match incumbent {
                None => Some(other),
                Some(best) => {
                    let better = other.start < best.start
                        || (other.start == best.start
                            && (other.power_dbm > best.power_dbm
                                || (other.power_dbm == best.power_dbm && other.node < best.node)));
                    if better { Some(other) } else { Some(best) }
                }
            };
        }
        match incumbent {
            None => true,
            Some(locked) => arrival.power_dbm >= locked.power_dbm + threshold,
        }
    }

    /// The power below which no preamble is detected at all, dBm.
    ///
    /// The preamble and the SIGNAL field are always BPSK 1/2 (EN 302 663 Annex C.3), so
    /// this is the configured sensitivity table's lowest-rate row — −91 dBm for
    /// `etsi-static` — whatever MCS the DATA field uses.
    #[must_use]
    pub fn preamble_detection_dbm(&self) -> f64 {
        self.sensitivity_dbm(Mcs::R3Bpsk12)
    }

    /// Ends a transmission: forgets its interval bookkeeping.
    pub fn end_tx(&mut self, h: TxHandle) {
        self.transmissions.remove(&h.id);
    }

    /// Forgets a completed arrival. Called by `finish_rx`; public so an engine that
    /// cancels an arrival (a despawned node) can clean up too.
    pub fn forget_arrival(&mut self, h: RxHandle) -> Option<Arrival> {
        self.arrivals.remove(&(h.rx.index(), h.tx))
    }
}

/// How long a node's transmit intervals are kept for the half-duplex test, nanoseconds.
///
/// One second, against a longest possible frame of 6,192 µs (a 2,304 B MSDU at
/// 3 Mbit/s): a live arrival's window is `[end − air_time, end)` with `air_time` at most
/// that, so no query a driven PHY can make reaches more than about 6.2 ms into the past,
/// and the retention is 160 times that. The point of the bound is that the list is
/// bounded at all — EN 302 571's own 25 ms idle-time floor caps a node at 40 frames per
/// second, so a node holds at most ~40 intervals instead of one per frame of the run.
pub const TX_INTERVAL_RETENTION_NS: u64 = 1_000_000_000;

/// Drops every interval that ended at or before `cutoff`, in place and in order.
fn prune_intervals(intervals: &mut Vec<BusyInterval>, cutoff: SimTime) {
    intervals.retain(|i| i.to > cutoff);
}

/// The overlap of `[a0, a1)` and `[b0, b1)` in nanoseconds.
fn overlap_ns(a0: SimTime, a1: SimTime, b0: SimTime, b1: SimTime) -> u64 {
    let lo = a0.max(b0);
    let hi = a1.min(b1);
    hi.saturating_sub(lo)
}

impl Default for OfdmPhy {
    fn default() -> Self {
        Self::new(Tier::High)
    }
}

impl Model for OfdmPhy {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Phy<C> for OfdmPhy {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn rat(&self) -> Rat {
        Rat::Dsrc80211p
    }

    fn begin_tx(&mut self, ctx: &mut C, tx: NodeId, f: &FrameDescriptor) -> Result<TxHandle> {
        if f.bytes > timing::MAX_MSDU_BYTES {
            return Err(RadioError::FrameTooLarge {
                bytes: f.bytes,
                cap: timing::MAX_MSDU_BYTES,
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
        self.transmissions
            .insert(handle.id, Transmission { handle, frame: *f });
        let intervals = self.tx_intervals.entry(tx.index()).or_default();
        // Drop what can no longer matter, before appending. Without this the list grows
        // for the whole run — at the 02-architecture.md §7 design target (10,000 nodes,
        // 10 Hz, 600 s) that is 6e7 intervals and a 6,000-entry linear scan inside every
        // half-duplex test, i.e. once per arrival per receiver.
        //
        // The cutoff is by TIME, never by handle: pruning what `end_tx` owns would make
        // the half-duplex answer depend on whether the transmitter's `end_tx` had run
        // yet, which is the same order dependence this crate has just removed from
        // capture. A time cutoff is a pure function of the schedule.
        prune_intervals(intervals, start.saturating_sub(TX_INTERVAL_RETENTION_NS));
        intervals.push(BusyInterval {
            from: handle.start,
            to: handle.end,
        });
        self.ledger.note_tx(tx, air);
        Ok(handle)
    }

    fn air_time(&self, bytes: u32, mcs: Mcs) -> Duration {
        air_time(bytes, mcs)
    }

    fn cca(&self, ctx: &C, node: NodeId, ch: ChannelId) -> CcaState {
        let at = ctx.now();
        if self.is_transmitting(node, at) {
            // A transmitting radio is not listening; the medium is busy by definition.
            return CcaState::Busy {
                energy_dbm: f64::INFINITY,
            };
        }
        let energy = self.energy_dbm(node, ch, at);
        if energy >= self.cca.cca_threshold_dbm() {
            CcaState::Busy { energy_dbm: energy }
        } else {
            CcaState::Idle
        }
    }

    fn finish_rx(&mut self, ctx: &mut C, rx: NodeId, h: RxHandle) -> RxOutcome {
        let key = (rx.index(), h.tx);
        let Some(arrival) = self.arrivals.get(&key) else {
            // Nothing was ever registered for this receiver: as far as the PHY is
            // concerned the frame never reached it.
            return RxOutcome::Lost(LossCause::OutOfRange);
        };
        let air = arrival.air_time();
        // The arrival stays in the map while the outcome is computed, and is forgotten
        // only afterwards: nothing an evaluation reads may depend on how many other
        // arrivals have already been finished (invariant I-R2).
        let outcome = self.evaluate(ctx, arrival);
        self.arrivals.remove(&key);
        self.ledger.note_rx(&outcome, air);
        outcome
    }

    fn noise_floor_dbm(&self, _node: NodeId, _ch: ChannelId) -> f64 {
        self.noise_floor()
    }
}

impl OfdmPhy {
    /// The outcome of one arrival, with exactly one loss cause (invariant I-R3).
    ///
    /// The order of the tests is the order of the physics: a radio that is transmitting
    /// hears nothing at all; a signal below sensitivity is never detected; a preamble that
    /// cannot be locked is never decoded; and only then does the error model get a say.
    ///
    /// `&self`, deliberately: an evaluation reads the PHY's configuration, the node's own
    /// transmit intervals and the arrival's own declared geometry, and writes nothing. It
    /// therefore cannot depend on — or perturb — any other arrival's evaluation, which is
    /// what makes the phase-parallel receiver map of 02-architecture.md §6.4 legitimate.
    fn evaluate<C: Ctx + ?Sized>(&self, ctx: &mut C, arrival: &Arrival) -> RxOutcome {
        if self.transmits_during(arrival.rx, arrival.start, arrival.end) {
            return RxOutcome::Lost(LossCause::HalfDuplex);
        }
        if arrival.power_dbm < self.sensitivity_dbm(arrival.frame.mcs) {
            return RxOutcome::Lost(LossCause::BelowSensitivity);
        }
        if self.tier == Tier::High && !self.preamble_locked(arrival) {
            return RxOutcome::Lost(LossCause::PreambleMissed);
        }
        let windows = self.sinr_windows(arrival);
        let worst_sinr = windows
            .iter()
            .map(|(_, _, s)| *s)
            .fold(f64::INFINITY, f64::min);
        let mean_sinr = if windows.is_empty() {
            f64::NEG_INFINITY
        } else {
            // Reported, not used for the decision: the decision is per window.
            math::sum_ordered(windows.iter().map(|(_, _, s)| *s)) / windows.len() as f64
        };
        let has_interference = !arrival.interferers.is_empty();
        let jammed = !self.jamming.is_empty()
            && self.jamming.overlaps(
                arrival.rx,
                arrival.frame.channel,
                arrival.start,
                arrival.end,
            );

        // The hard-threshold shortcut of 04-models.md §4.8, when a scenario asks for it.
        if let CaptureRule::Threshold { cp_th_db } = self.capture {
            if worst_sinr < cp_th_db {
                // The same counterfactual as the error-model path below, in the form this
                // rule admits: would the frame have cleared the threshold without the
                // jammer?
                if jammed
                    && self
                        .sinr_windows_with(arrival, false)
                        .iter()
                        .map(|(_, _, s)| *s)
                        .fold(f64::INFINITY, f64::min)
                        >= cp_th_db
                {
                    return RxOutcome::Lost(LossCause::Jammed);
                }
                return RxOutcome::Lost(if has_interference {
                    self.interference_cause(arrival)
                } else {
                    LossCause::BelowSensitivity
                });
            }
            return RxOutcome::Received {
                sinr_db: numeric::q_db(worst_sinr),
                rssi_dbm: numeric::q_db(arrival.power_dbm),
            };
        }

        let psr = self.success_probability(arrival);
        // One uniform draw per arrival, keyed by (link, frame): see the module docs on
        // the plug-in domain. The domain is derived once (it is SHA-256 over a constant
        // string) rather than per arrival per receiver.
        //
        // `f64()` and not `bool(1.0 - psr)`, and the two are the same draw:
        // `RngStream::bool(p)` *is* `self.f64() < p`, consuming exactly one 64-bit draw
        // whatever `p` is. Keeping the uniform is what lets the jamming counterfactual be
        // exact — the same realisation is compared against two probabilities — without
        // consuming a second draw and shifting every stream after it.
        let draw = ctx
            .rng(
                *FRAME_ERROR_DOMAIN,
                EntityRef::LinkFrame {
                    link: arrival.link(),
                    frame: arrival.start,
                },
            )
            .f64();
        let error = draw < 1.0 - psr;
        if error {
            // Jamming attribution (04-models.md §12.3). The frame failed because the draw
            // landed below its error probability. Evaluate the *same* draw against the
            // *same* frame with the jamming power taken out of the noise term: if it would
            // have survived, the jammer is what killed it and nothing else is. This is the
            // distinction 04-models.md §12.3 exists to preserve — a reactive jammer
            // "produces PDR = 0 dropouts uncorrelated with SINR dips", and a frame
            // reported `collision` would have erased it.
            if jammed {
                let psr_clean = self.success_probability_with(arrival, false);
                if draw >= 1.0 - psr_clean {
                    return RxOutcome::Lost(LossCause::Jammed);
                }
            }
            return RxOutcome::Lost(if has_interference {
                self.interference_cause(arrival)
            } else {
                // No interferer: thermal noise and the fading realisation are what killed
                // it, which is what LossCause::Fading names.
                LossCause::Fading
            });
        }
        RxOutcome::Received {
            sinr_db: numeric::q_db(mean_sinr),
            rssi_dbm: numeric::q_db(arrival.power_dbm),
        }
    }

    /// Which interference cause to report: a hidden terminal when any interferer was
    /// inaudible to the victim's transmitter, a plain collision otherwise.
    fn interference_cause(&self, arrival: &Arrival) -> LossCause {
        let hidden = arrival
            .interferers
            .iter()
            .any(|i| i.audible_to_victim_tx == Some(false));
        if hidden {
            LossCause::HiddenTerminal
        } else {
            LossCause::Collision
        }
    }
}

fn en302663() -> Source {
    Source::new(
        SourceKind::Standard,
        "ETSI EN 302 663 V1.3.1 §4.2, Annex C.3, Table C.1, Table 1 and Table 2 (R1 §A.3-A.5), \
         via 04-models.md §4.2",
    )
}

fn phy_card(sensitivity: SensitivityPreset, rx_impl_loss_db: f64) -> ModelCard {
    let mut card = ModelCard::new(
        OfdmPhy::ID,
        Family::Phy,
        "1.0.0",
        "The IEEE 802.11p 10 MHz OFDM physical layer: exact air time, the EN 302 663 \
         sensitivity table, thermal noise and noise figure, per-window SINR with \
         id-ordered interference sums, preamble capture, half duplex and the NIST error \
         model.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.equations = vec![
        Equation {
            name: "air time".to_string(),
            latex_or_text: "T = 32 µs + 8 µs + 8 µs · ceil((N_SERVICE + 8·bytes + N_TAIL) \
                            / N_DBPS)"
                .to_string(),
            notes: Some(
                "N_SERVICE = 16 and N_TAIL = 6 are UNVERIFIED at the clause level \
                 (04-models.md §4.2): they come from the cached NIST reproduction and from \
                 Veins and ns-3, and IEEE 802.11-2016 clause 17.3.2 is not in the cache."
                    .to_string(),
            ),
        },
        Equation::new(
            "noise floor",
            "N = −174 + 10·log10(B) + NF = −104 dBm + NF in a 10 MHz channel",
        ),
        Equation {
            name: "SINR".to_string(),
            latex_or_text: "SINR = P_rx − 10·log10(N + Σ I_k)".to_string(),
            notes: Some(
                "Interferers summed in NodeId order with sum_ordered \
                 (02-architecture.md §6.3), over the overlap windows at the high tier and \
                 over the whole frame at the medium tier."
                    .to_string(),
            ),
        },
        Equation::new(
            "capture",
            "decodable while P_r >= I + CpTh throughout reception (the threshold \
             shortcut); otherwise per-window SINR against the error model",
        ),
    ];
    card.parameters = vec![
        Parameter {
            name: "sensitivity_preset".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!(sensitivity.label()),
            range: Some(vec![
                serde_json::json!("etsi-static"),
                serde_json::json!("etsi-dynamic"),
                serde_json::json!("cohda-mk5"),
            ]),
            source: en302663(),
            calibration: None,
        },
        Parameter {
            name: "thermal_noise_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(THERMAL_NOISE_10MHZ_DBM),
            range: None,
            source: Source::new(
                SourceKind::Paper,
                "kTB in 10 MHz (R3 §F.2), via 04-models.md §3.7",
            ),
            calibration: None,
        },
        Parameter {
            name: "noise_figure_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(NOISE_FIGURE_HARDWARE_DB),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(15.0)]),
            source: Source {
                kind: SourceKind::Datasheet,
                reference: "NXP SAF5400 fact sheet (R3 §F.2), via 04-models.md §3.7: 6 dB, \
                            giving a −98 dBm floor"
                    .to_string(),
                accessed: None,
                note: Some(
                    "3GPP evaluations use 9 dB (TR 36.885 Annex A.1.1), giving −95 dBm."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "cca_busy_dbm".to_string(),
            unit: "dBm".to_string(),
            default: serde_json::json!(CBR_BUSY_THRESHOLD_DBM),
            range: Some(vec![serde_json::json!(-110.0), serde_json::json!(-50.0)]),
            source: Source {
                kind: SourceKind::Standard,
                reference: "ETSI EN 302 571 V2.1.1 §4.2.10.1 (the CBR busy threshold), via \
                            04-models.md §4.5"
                    .to_string(),
                accessed: None,
                note: Some(
                    "CCA-ED and the CBR threshold are related but are not defined as the \
                     same parameter by the standards; this model uses one number for both \
                     unless a preset separates them (ns3: −82 / −62; veins: −65)."
                        .to_string(),
                ),
            },
            calibration: None,
        },
        Parameter {
            name: "capture".to_string(),
            unit: "-".to_string(),
            default: serde_json::json!("sinr-trace"),
            range: Some(vec![
                serde_json::json!("sinr-trace"),
                serde_json::json!("threshold"),
            ]),
            source: Source::new(
                SourceKind::Paper,
                "Torrent-Moreno 2009 Eq. 1 (R1 §B.4), via 04-models.md §4.8: CpTh 5 dB for \
                 3 Mbit/s BPSK 1/2",
            ),
            calibration: None,
        },
        Parameter {
            name: "cp_th_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(CAPTURE_THRESHOLD_DB),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(20.0)]),
            source: Source::new(
                SourceKind::Paper,
                "Torrent-Moreno 2009 (R1 §B.4): 5 dB, the lowest SINR for successful \
                 reception at 3 Mbit/s",
            ),
            calibration: None,
        },
        Parameter {
            name: "rx_impl_loss_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(rx_impl_loss_db),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(20.0)]),
            source: Source::new(
                SourceKind::Paper,
                "04-models.md §4.7: 0 dB default (standards-ideal), preset \
                 sjoberg-atheros 5 dB",
            ),
            calibration: None,
        },
        Parameter {
            name: "max_msdu_bytes".to_string(),
            unit: "B".to_string(),
            default: serde_json::json!(timing::MAX_MSDU_BYTES),
            range: None,
            source: Source {
                kind: SourceKind::Standard,
                reference: "IEEE 802.11 maximum MSDU, 2,304 bytes, via 04-models.md §4.6"
                    .to_string(),
                accessed: None,
                note: Some("Clause UNVERIFIED; the figure is well established.".to_string()),
            },
            calibration: None,
        },
    ];
    card.assumptions = vec![
        "The engine supplies the received power of each arrival and declares the \
         overlapping transmissions; this model owns the arrival set, the half-duplex \
         intervals and the outcome, not the link budget."
            .to_string(),
        "A frame's bits are apportioned across its SINR windows by time, which reproduces \
         the single-SINR PER of §4.7 exactly when the interferer set is constant."
            .to_string(),
        "The frame-error draw comes from the plug-in RNG domain derived from this model's \
         id, keyed by (link, frame): the core's built-in domain list has no frame-error \
         domain."
            .to_string(),
        "Declared jamming (crate::jamming) enters the noise term of each SINR window, \
         which is 04-models.md §12.3's whole mechanism; the jammer's own power and duty \
         cycle are its model's parameters, not this one's."
            .to_string(),
        "A frame lost to a jammer is reported LossCause::Jammed on a counterfactual: the \
         same uniform draw evaluated against the same frame with the jamming power taken \
         out of the noise. A frame that would have failed anyway keeps its congestion \
         cause, so the two are never confused."
            .to_string(),
    ];
    card.limitations = vec![
        "The hidden-terminal cause needs the engine to say whether the interferer was \
         audible to the victim's transmitter; 04-models.md §4.8 phrases the test in terms \
         of the receiver's CCA range, and this model follows the mechanism (the \
         transmitter's) and takes the answer as input."
            .to_string(),
        "No frequency-selective fading, no Doppler-dependent channel-estimation loss \
         beyond rx_impl_loss_db, no receiver AGC dynamics."
            .to_string(),
        "1609.4 channel switching is not implemented: its constants are UNVERIFIED \
         (04-models.md §4.1) and the default is continuous CCH operation."
            .to_string(),
        "Two detection gates run in series and the EN 302 663 sensitivity table \
         dominates the error model: with the default preset the per-MCS level sits \
         1.5-3.4 dB above the SNR at which the NIST model reaches 10 % PER for a 400 B \
         frame (3 Mbit/s 3.41 dB, 6 Mbit/s 3.40 dB, 27 Mbit/s 1.82 dB), so the PDR steps \
         from about 0.9999 to 0 at the sensitivity level rather than rolling off. \
         EN 302 663 defines that level as the power at which the receiver ACHIEVES 10 % \
         PER, so the two are not independent tests of the same thing; at the Abbas urban \
         exponent the gate shortens the modelled range by roughly 15-32 % relative to \
         the error model alone, and an rx_impl_loss_db below about 1.5 dB has no \
         observable effect at all. A study that wants the error model to set the range \
         uses a noise-referenced detection floor instead (ns-3 uses −101 dBm at 20 MHz, \
         a few dB above kTB+NF, not the conformance minimum). \
         `the_sensitivity_gate_sits_above_the_error_model_and_the_margin_is_pinned` \
         pins the margin per MCS."
            .to_string(),
    ];
    card.ignores = vec![
        "At the medium tier: preamble detection, capture timing and half duplex are not \
         evaluated, and the SINR is taken over the whole frame (04-models.md §4.9)."
            .to_string(),
    ];
    card.sources = vec![
        en302663(),
        Source::new(
            SourceKind::Paper,
            "G. Pei and T. R. Henderson 2010 (the error model; see phy/80211p/nist-per)",
        ),
        Source::new(
            SourceKind::Paper,
            "M. Torrent-Moreno et al., IEEE TVT 2009 Eq. 1 (capture)",
        ),
    ];
    card.validation = Validation {
        status: ValidationStatus::LiteratureChecked,
        references: vec![en302663()],
        tests: vec![
            "air_time_matches_a_hand_computation".to_string(),
            "air_time_matches_the_data_rate_for_every_mcs".to_string(),
            "sinr_is_order_independent".to_string(),
            "the_noise_floor_is_minus_98_dbm_with_the_hardware_noise_figure".to_string(),
            "a_frame_below_sensitivity_is_lost_for_that_reason".to_string(),
            "airtime_accounting_sums_to_the_transmitted_time".to_string(),
            "windowed_sinr_reduces_to_the_flat_per".to_string(),
            "the_sensitivity_gate_sits_above_the_error_model_and_the_margin_is_pinned".to_string(),
            "preamble_capture_does_not_depend_on_the_order_arrivals_are_finished".to_string(),
            "an_undetectable_signal_does_not_hold_the_receiver".to_string(),
            "every_completion_order_of_five_arrivals_gives_the_same_ledger".to_string(),
            "transmit_intervals_are_pruned_and_the_half_duplex_answer_is_unchanged".to_string(),
        ],
    };
    card.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![format!("plugin({})", OfdmPhy::ID)],
    };
    card
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testctx::TestCtx;
    use crate::types::{FrameKind, SduRef};
    use v2xw_core::ids::{FrameSeq, SduId};

    fn frame(bytes: u32, mcs: Mcs) -> FrameDescriptor {
        FrameDescriptor::broadcast(bytes, mcs, SduRef::new(SduId::new(1), FrameSeq::new(1)))
    }

    fn arrival(tx: u32, rx: u32, power_dbm: f64, start: SimTime, f: FrameDescriptor) -> Arrival {
        Arrival {
            tx_id: u64::from(tx) + 1,
            tx: NodeId::new(tx),
            rx: NodeId::new(rx),
            power_dbm,
            start,
            end: air_time(f.bytes, f.mcs).after(start),
            frame: f,
            interferers: Vec::new(),
        }
    }

    #[test]
    fn air_time_matches_a_hand_computation() {
        // 400 bytes at 6 Mbit/s (N_DBPS = 48): bits = 16 + 3,200 + 6 = 3,222;
        // symbols = ceil(3222/48) = 68; T = 32 + 8 + 8·68 = 584 µs.
        assert_eq!(air_time(400, Mcs::R6Qpsk12), Duration::from_micros(584));
        assert_eq!(data_symbols(400, Mcs::R6Qpsk12), 68);
        // 300 bytes at 3 Mbit/s (N_DBPS = 24): bits = 2,422; symbols = ceil(100.92) = 101;
        // T = 40 + 808 = 848 µs.
        assert_eq!(air_time(300, Mcs::R3Bpsk12), Duration::from_micros(848));
        // A zero-length PSDU is still a preamble, a SIGNAL field and one symbol: the 22
        // SERVICE and tail bits have to go somewhere.
        assert_eq!(air_time(0, Mcs::R3Bpsk12), Duration::from_micros(48));
        // And the shortest frame at the fastest rate.
        assert_eq!(air_time(0, Mcs::R27Qam64_34), Duration::from_micros(48));
    }

    #[test]
    fn air_time_matches_the_data_rate_for_every_mcs() {
        // The payload part of the air time must equal bits / rate to within one symbol,
        // which is the only rounding the formula does.
        for mcs in Mcs::ALL {
            for bytes in [100u32, 300, 400, 1_000, 2_304] {
                let total = air_time(bytes, mcs).as_nanos();
                let overhead = timing::PREAMBLE.as_nanos() + timing::SIGNAL.as_nanos();
                let data_ns = total - overhead;
                let ideal_ns =
                    crate::per::data_field_bits(bytes) as f64 / mcs.rate_mbps() * 1_000.0;
                let slack = data_ns as f64 - ideal_ns;
                assert!(
                    (0.0..=timing::SYMBOL.as_nanos() as f64).contains(&slack),
                    "{mcs} at {bytes} B: {data_ns} ns against {ideal_ns} ns"
                );
            }
        }
    }

    #[test]
    fn a_frame_above_the_msdu_cap_is_refused() {
        let mut ctx = TestCtx::new(1);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(timing::MAX_MSDU_BYTES + 1, Mcs::R6Qpsk12);
        let err = Phy::begin_tx(&mut phy, &mut ctx, NodeId::new(0), &f)
            .expect_err("the fragmenter must have acted first");
        assert!(matches!(err, RadioError::FrameTooLarge { .. }));
        // And exactly at the cap it is accepted.
        let ok = frame(timing::MAX_MSDU_BYTES, Mcs::R6Qpsk12);
        assert!(Phy::begin_tx(&mut phy, &mut ctx, NodeId::new(0), &ok).is_ok());
    }

    #[test]
    fn the_noise_floor_is_minus_98_dbm_with_the_hardware_noise_figure() {
        let phy = OfdmPhy::new(Tier::High);
        assert_eq!(phy.noise_floor(), -98.0);
        let phy3gpp = OfdmPhy::new(Tier::High).with_noise_figure_db(NOISE_FIGURE_3GPP_DB);
        assert_eq!(phy3gpp.noise_floor(), -95.0);
        assert_eq!(THERMAL_NOISE_10MHZ_DBM, -104.0);
    }

    #[test]
    fn sinr_is_order_independent() {
        // The property 02-architecture.md §6.3 asks for: the interference sum is reduced
        // in id order, so the SINR does not depend on the order the engine collected the
        // interferers in — bit for bit, not to a tolerance.
        let noise = -98.0;
        let forward = [
            (NodeId::new(9), numeric::dbm_to_mw(-92.5)),
            (NodeId::new(2), numeric::dbm_to_mw(-87.25)),
            (NodeId::new(17), numeric::dbm_to_mw(-101.0)),
            (NodeId::new(5), numeric::dbm_to_mw(-95.125)),
        ];
        let mut reversed = forward;
        reversed.reverse();
        let mut rotated = forward;
        rotated.rotate_left(2);
        let a = sinr_db(-80.0, &forward, noise);
        let b = sinr_db(-80.0, &reversed, noise);
        let c = sinr_db(-80.0, &rotated, noise);
        assert_eq!(a.to_bits(), b.to_bits());
        assert_eq!(a.to_bits(), c.to_bits());
        // With no interferer it is the plain SNR.
        assert!((sinr_db(-80.0, &[], noise) - 18.0).abs() < 1e-9);
    }

    #[test]
    fn a_frame_below_sensitivity_is_lost_for_that_reason() {
        let mut ctx = TestCtx::new(4);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(300, Mcs::R6Qpsk12);
        // −92 dBm is below the −88 dBm static sensitivity of 6 Mbit/s.
        let h = phy.register_arrival(arrival(0, 1, -92.0, 0, f));
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert_eq!(outcome, RxOutcome::Lost(LossCause::BelowSensitivity));
        // Just above it, the frame is decoded: −87 dBm against a −98 dBm floor is 11 dB
        // of SNR, well past the 6.6 dB the error model needs.
        let h = phy.register_arrival(arrival(0, 1, -87.0, 1_000_000, f));
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert!(matches!(outcome, RxOutcome::Received { .. }), "{outcome:?}");
    }

    #[test]
    fn a_receiver_that_is_transmitting_loses_the_arrival() {
        let mut ctx = TestCtx::new(5);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(300, Mcs::R6Qpsk12);
        // Node 1 transmits from t = 0.
        let own = Phy::begin_tx(&mut phy, &mut ctx, NodeId::new(1), &f).expect("starts");
        assert!(phy.is_transmitting(NodeId::new(1), own.start));
        // A strong frame arrives during it.
        let h = phy.register_arrival(arrival(0, 1, -60.0, own.start + 1_000, f));
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert_eq!(outcome, RxOutcome::Lost(LossCause::HalfDuplex));
    }

    #[test]
    fn a_weaker_frame_cannot_capture_a_locked_receiver() {
        let mut ctx = TestCtx::new(6);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(300, Mcs::R6Qpsk12);
        // The incumbent starts first and is strong.
        let first = arrival(0, 2, -70.0, 0, f);
        let h1 = phy.register_arrival(first.clone());
        // The second starts 100 µs later, above sensitivity but not 5 dB above the
        // incumbent.
        let mut second = arrival(1, 2, -68.0, 100_000, f);
        second.interferers.push(InterferenceSource::new(
            NodeId::new(0),
            -70.0,
            first.start,
            first.end,
        ));
        let h2 = phy.register_arrival(second);
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(2), h2);
        assert_eq!(outcome, RxOutcome::Lost(LossCause::PreambleMissed));
        // The incumbent is unaffected by the test on the other arrival.
        let _ = h1;
    }

    /// Two arrivals at one receiver, each declaring the other: whatever order the engine
    /// finishes them in, both outcomes and the whole loss-cause ledger must be identical
    /// bit for bit (invariant I-R2).
    ///
    /// Before the fix `finish_rx` removed the arrival from `self.arrivals` before
    /// `evaluate` ran and `preamble_locked` scanned the surviving map, so the incumbent
    /// existed or not depending on whose `finish_rx` had already run. This exact scenario
    /// — incumbent −70 dBm from t = 0, challenger −68 dBm from t = 100 µs, both 400 B,
    /// 2 dB apart against a 5 dB capture threshold — gave `Lost(PreambleMissed)` when the
    /// challenger was evaluated first and `Lost(Collision)` when the incumbent was
    /// finished first. Because an engine driven by end-of-frame events necessarily
    /// finishes the earlier-ending frame first, capture was silently disabled against
    /// every incumbent whose frame ends before the challenger's, which is the common case.
    #[test]
    fn preamble_capture_does_not_depend_on_the_order_arrivals_are_finished() {
        /// Builds the scene, finishes the arrivals in `order`, and reports what happened
        /// keyed by transmitter so the comparison itself is order-independent.
        fn run(order: &[u32], sizes: &[u32]) -> (BTreeMap<u32, RxOutcome>, AirtimeLedger) {
            let mut ctx = TestCtx::new(11);
            let mut phy = OfdmPhy::new(Tier::High);
            let rx = NodeId::new(9);
            // (transmitter, power, start) — one strong early incumbent and two later,
            // stronger challengers, none of them 5 dB above the incumbent.
            let scene: Vec<Arrival> = [(-70.0, 0u64), (-68.0, 100_000), (-67.0, 150_000)]
                .iter()
                .zip(sizes)
                .enumerate()
                .map(|(i, ((power, start), bytes))| {
                    arrival(
                        i as u32,
                        rx.index(),
                        *power,
                        *start,
                        frame(*bytes, Mcs::R6Qpsk12),
                    )
                })
                .collect();
            // Every arrival declares every other overlapping one, which is what the
            // engine does: one source of truth for what is on the air.
            let mut registered = BTreeMap::new();
            for a in &scene {
                let mut a = a.clone();
                for other in &scene {
                    if other.tx == a.tx || other.start >= a.end || a.start >= other.end {
                        continue;
                    }
                    a.interferers.push(InterferenceSource::new(
                        other.tx,
                        other.power_dbm,
                        other.start,
                        other.end,
                    ));
                }
                registered.insert(a.tx.index(), phy.register_arrival(a));
            }
            let mut outcomes = BTreeMap::new();
            for tx in order {
                let h = registered[tx];
                outcomes.insert(*tx, Phy::finish_rx(&mut phy, &mut ctx, rx, h));
            }
            (outcomes, phy.ledger().clone())
        }

        for sizes in [[400u32, 400, 400], [300, 1_000, 700]] {
            let reference = run(&[0, 1, 2], &sizes);
            // Every permutation of the completion order, including the end-of-frame order
            // an event-driven engine actually produces.
            for order in [
                [0, 1, 2],
                [0, 2, 1],
                [1, 0, 2],
                [1, 2, 0],
                [2, 0, 1],
                [2, 1, 0],
            ] {
                let got = run(&order, &sizes);
                assert_eq!(got.0, reference.0, "outcomes differ for order {order:?}");
                assert_eq!(got.1, reference.1, "ledger differs for order {order:?}");
            }
            // And the answer is the physical one: the incumbent decodes or fails on its
            // own merits, and neither challenger captures a receiver it is only 2-3 dB
            // above.
            assert_eq!(
                reference.0[&1],
                RxOutcome::Lost(LossCause::PreambleMissed),
                "{:?}",
                reference.0
            );
            assert_eq!(reference.0[&2], RxOutcome::Lost(LossCause::PreambleMissed));
        }
    }

    /// A signal the receiver could never have detected does not hold it.
    ///
    /// `preamble_locked` used to treat any registered arrival as a lock, including one
    /// far below any detection floor. The floor is the base rate's sensitivity, because
    /// the preamble and the SIGNAL field are BPSK 1/2 whatever the DATA field is.
    #[test]
    fn an_undetectable_signal_does_not_hold_the_receiver() {
        let mut ctx = TestCtx::new(12);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(400, Mcs::R6Qpsk12);
        let rx = NodeId::new(5);
        assert!((phy.preamble_detection_dbm() - (-91.0)).abs() < 1e-12);

        // An incumbent at −120 dBm: 29 dB below the base-rate sensitivity, so nothing
        // ever locked it.
        let ghost = arrival(0, rx.index(), -120.0, 0, f);
        let mut challenger = arrival(1, rx.index(), -70.0, 100_000, f);
        challenger.interferers.push(InterferenceSource::new(
            ghost.tx,
            ghost.power_dbm,
            ghost.start,
            ghost.end,
        ));
        assert!(
            phy.preamble_locked(&challenger),
            "an undetectable signal blocked the lock"
        );

        // Raise the same incumbent to −70 dBm and it does hold the receiver: the rule is
        // about detectability, not about the map.
        let mut blocked = arrival(1, rx.index(), -70.0, 100_000, f);
        blocked
            .interferers
            .push(InterferenceSource::new(ghost.tx, -70.0, 0, ghost.end));
        assert!(!phy.preamble_locked(&blocked));

        // An incumbent between the two floors — detectable at the base rate, below the
        // 6 Mbit/s sensitivity it carries — still holds the receiver, because the
        // preamble is what is locked (EN 302 663 Annex C.3).
        assert!((phy.sensitivity_dbm(Mcs::R6Qpsk12) - (-88.0)).abs() < 1e-12);
        let mut marginal = arrival(1, rx.index(), -85.0, 100_000, f);
        marginal
            .interferers
            .push(InterferenceSource::new(ghost.tx, -89.0, 0, ghost.end));
        assert!(!phy.preamble_locked(&marginal));
        let h = phy.register_arrival(marginal);
        assert_eq!(
            Phy::finish_rx(&mut phy, &mut ctx, rx, h),
            RxOutcome::Lost(LossCause::PreambleMissed)
        );
    }

    /// The transmit-interval list is bounded, and the bound is a time cutoff rather than
    /// anything `end_tx` does — so the half-duplex answer cannot depend on whether the
    /// transmitter's `end_tx` has run.
    ///
    /// It used to grow for the whole run: after 20,000 `begin_tx`/`end_tx` pairs,
    /// `is_transmitting(node, 1 µs)` still reported true for a transmission that ended
    /// 20 s earlier, and every half-duplex test scanned every interval ever recorded.
    #[test]
    fn transmit_intervals_are_pruned_and_the_half_duplex_answer_is_unchanged() {
        let mut ctx = TestCtx::new(13);
        let mut phy = OfdmPhy::new(Tier::High);
        let node = NodeId::new(0);
        let f = frame(400, Mcs::R6Qpsk12);
        let air = air_time(400, Mcs::R6Qpsk12).as_nanos();

        // 10 Hz for 200 s, the shape of a CAM generator.
        let mut handles = Vec::new();
        for i in 0..2_000u64 {
            ctx.set_now(i * 100_000_000);
            let h = Phy::begin_tx(&mut phy, &mut ctx, node, &f).expect("fits");
            handles.push(h);
            phy.end_tx(h);
        }
        // Bounded by the retention window, not by the run: one second of a 10 Hz
        // generator is eleven intervals at most.
        let held = phy.tx_interval_count(node);
        assert!(held <= 12, "{held} intervals retained");
        // Nothing from the start of the run is still claimed as busy.
        assert!(!phy.is_transmitting(node, 1_000));
        assert!(!phy.is_transmitting(node, 100_000_000 + air / 2));

        // And the intervals that can still matter are all there: the last frame is busy
        // for exactly its air time and idle either side.
        let last = 1_999 * 100_000_000;
        assert!(phy.is_transmitting(node, last));
        assert!(phy.is_transmitting(node, last + air - 1));
        assert!(!phy.is_transmitting(node, last + air));
        assert!(phy.transmits_during(node, last + air - 1, last + air + 10));
        assert!(!phy.transmits_during(node, last + air, last + air + 10));

        // Pruning is by time and never by handle: end_tx leaves the intervals alone, so
        // an arrival evaluated before or after the transmitter's end_tx gets the same
        // half-duplex answer.
        let mut fresh = OfdmPhy::new(Tier::High);
        ctx.set_now(5_000_000_000);
        let h = Phy::begin_tx(&mut fresh, &mut ctx, node, &f).expect("fits");
        let before = fresh.transmits_during(node, h.start, h.end);
        fresh.end_tx(h);
        assert_eq!(before, fresh.transmits_during(node, h.start, h.end));
        assert!(before);
    }

    /// Two detection gates run in series and the ETSI table wins: the per-MCS
    /// sensitivity sits 1.5-3.4 dB **above** the SNR at which this crate's own error
    /// model reaches 10 % PER, so with the default preset the error model never sets the
    /// range.
    ///
    /// EN 302 663 Table 1 defines its levels as the power at which the receiver
    /// *achieves* 10 % PER, not as a detection cliff, so applying both makes PDR step
    /// from about 0.9999 to 0 at the sensitivity level and shortens the modelled
    /// communication range by roughly 15-32 % at the Abbas urban exponent. This is a
    /// recorded property of the shipped configuration, not a bug to tune away — but it is
    /// pinned here per MCS so that a change to either number is visible rather than
    /// silent, and the card says which gate dominates.
    #[test]
    fn the_sensitivity_gate_sits_above_the_error_model_and_the_margin_is_pinned() {
        let phy = OfdmPhy::new(Tier::High);
        let per = PerModel::default();
        let noise = phy.noise_floor();
        assert!((noise - (-98.0)).abs() < 1e-12);
        // (MCS, gate SNR = sensitivity − noise floor, SNR at 10 % PER for 400 B, margin)
        let expected = [
            (Mcs::R3Bpsk12, 7.0, 3.589_453, 3.410_547),
            (Mcs::R4p5Bpsk34, 8.0, 6.458_623, 1.541_377),
            (Mcs::R6Qpsk12, 10.0, 6.597_610, 3.402_390),
            (Mcs::R9Qpsk34, 12.0, 9.468_923, 2.531_077),
            (Mcs::R12Qam16_12, 15.0, 13.095_067, 1.904_933),
            (Mcs::R18Qam16_34, 19.0, 16.193_600, 2.806_400),
            (Mcs::R24Qam64_23, 23.0, 20.938_027, 2.061_973),
            (Mcs::R27Qam64_34, 24.0, 22.181_726, 1.818_274),
        ];
        for (mcs, gate, at_10_percent, margin) in expected {
            let measured_gate = phy.sensitivity_dbm(mcs) - noise;
            assert!(
                (measured_gate - gate).abs() < 1e-9,
                "{}: gate {measured_gate}",
                mcs.label()
            );
            let threshold = per.snr_for_per(400, mcs, 0.1);
            assert!(
                (threshold - at_10_percent).abs() < 1e-5,
                "{}: 10 % PER at {threshold} dB",
                mcs.label()
            );
            assert!(
                (measured_gate - threshold - margin).abs() < 1e-5,
                "{}: margin {}",
                mcs.label(),
                measured_gate - threshold
            );
            // The gate is above the error model's threshold for every MCS, which is what
            // "the table dominates" means, and the PER at the gate is negligible.
            assert!(measured_gate > threshold, "{}", mcs.label());
            assert!(per.per(400, mcs, measured_gate) < 1e-3, "{}", mcs.label());
        }
        // The card says so rather than leaving a reader to measure it.
        assert!(
            phy.card()
                .limitations
                .iter()
                .any(|l| l.contains("dominates the error model")),
            "{:?}",
            phy.card().limitations
        );
    }

    #[test]
    fn every_completion_order_of_five_arrivals_gives_the_same_ledger() {
        // Five arrivals at one receiver, every one of the 120 completion orders.
        fn run(order: &[usize]) -> (BTreeMap<u32, RxOutcome>, AirtimeLedger) {
            let mut ctx = TestCtx::new(99);
            let mut phy = OfdmPhy::new(Tier::High);
            let rx = NodeId::new(9);
            let spec = [
                (-70.0, 0u64, 400u32),
                (-68.0, 100_000, 300),
                (-67.0, 150_000, 1_000),
                (-83.0, 220_000, 700),
                (-64.0, 600_000, 200),
            ];
            let scene: Vec<Arrival> = spec
                .iter()
                .enumerate()
                .map(|(i, (p, st, b))| {
                    arrival(i as u32, rx.index(), *p, *st, frame(*b, Mcs::R6Qpsk12))
                })
                .collect();
            let mut reg = BTreeMap::new();
            for a in &scene {
                let mut a = a.clone();
                for o in &scene {
                    if o.tx == a.tx || o.start >= a.end || a.start >= o.end {
                        continue;
                    }
                    a.interferers
                        .push(InterferenceSource::new(o.tx, o.power_dbm, o.start, o.end));
                }
                reg.insert(a.tx.index(), phy.register_arrival(a));
            }
            let mut out = BTreeMap::new();
            for i in order {
                let h = reg[&(*i as u32)];
                out.insert(*i as u32, Phy::finish_rx(&mut phy, &mut ctx, rx, h));
            }
            (out, phy.ledger().clone())
        }
        fn perms(v: Vec<usize>) -> Vec<Vec<usize>> {
            if v.len() <= 1 {
                return vec![v];
            }
            let mut out = Vec::new();
            for i in 0..v.len() {
                let mut rest = v.clone();
                let x = rest.remove(i);
                for mut p in perms(rest) {
                    p.insert(0, x);
                    out.push(p);
                }
            }
            out
        }
        let all = perms(vec![0, 1, 2, 3, 4]);
        let reference = run(&all[0]);
        let mut n = 0;
        for o in &all {
            let got = run(o);
            assert_eq!(got.0, reference.0, "{o:?}");
            assert_eq!(got.1, reference.1, "{o:?}");
            n += 1;
        }
        assert_eq!(n, 120);
        // The scene is not degenerate: it exercises capture, a signal below the base-rate
        // detection floor and a plain collision, and every arrival's air time lands in
        // exactly one bucket (invariant I-R3).
        assert_eq!(reference.0[&0], RxOutcome::Lost(LossCause::Collision));
        assert_eq!(reference.0[&1], RxOutcome::Lost(LossCause::PreambleMissed));
        assert_eq!(
            reference.1.accounted_total_ns(),
            [400u32, 300, 1_000, 700, 200]
                .iter()
                .map(|b| air_time(*b, Mcs::R6Qpsk12).as_nanos())
                .sum::<u64>()
        );
    }

    #[test]
    fn a_hidden_terminal_is_named_as_one() {
        let mut ctx = TestCtx::new(7);
        let mut phy = OfdmPhy::new(Tier::Medium);
        let f = frame(1_000, Mcs::R12Qam16_12);
        // A frame just above sensitivity with an equally strong, inaudible collider: the
        // SINR is around 0 dB, far below the 13 dB this MCS needs, so it is lost — and it
        // is lost as a hidden terminal, because CSMA could not have prevented it.
        let mut a = arrival(0, 3, -82.0, 0, f);
        a.interferers
            .push(InterferenceSource::new(NodeId::new(9), -82.0, 0, a.end).hidden());
        let h = phy.register_arrival(a);
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(3), h);
        assert_eq!(outcome, RxOutcome::Lost(LossCause::HiddenTerminal));
        // The same overlap with an audible collider is an ordinary collision.
        let mut b = arrival(0, 3, -82.0, 10_000_000, f);
        b.interferers.push(InterferenceSource {
            audible_to_victim_tx: Some(true),
            ..InterferenceSource::new(NodeId::new(9), -82.0, 10_000_000, b.end)
        });
        let h = phy.register_arrival(b);
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(3), h);
        assert_eq!(outcome, RxOutcome::Lost(LossCause::Collision));
    }

    #[test]
    fn windowed_sinr_reduces_to_the_flat_per() {
        // The property that keeps §4.8 consistent with §4.7: when the interferer set does
        // not change during the frame, the per-window evaluation must give the same
        // success probability as the single-SINR PER of the error model.
        let phy = OfdmPhy::new(Tier::High);
        let f = frame(400, Mcs::R6Qpsk12);
        let mut a = arrival(0, 1, -88.0, 0, f);
        // One interferer spanning the whole frame.
        a.interferers
            .push(InterferenceSource::new(NodeId::new(4), -95.0, 0, a.end));
        let windows = phy.sinr_windows(&a);
        assert_eq!(windows.len(), 1);
        let sinr = windows[0].2;
        let windowed = phy.success_probability(&a);
        let flat = 1.0 - phy.per_model().per(400, Mcs::R6Qpsk12, sinr);
        assert!((windowed - flat).abs() < 1e-9, "{windowed} vs {flat}");
    }

    #[test]
    fn a_mid_frame_interferer_splits_the_frame_into_windows() {
        let phy = OfdmPhy::new(Tier::High);
        let f = frame(400, Mcs::R6Qpsk12);
        let mut a = arrival(0, 1, -80.0, 0, f);
        let mid = a.start + (a.end - a.start) / 2;
        a.interferers
            .push(InterferenceSource::new(NodeId::new(4), -70.0, mid, a.end));
        let windows = phy.sinr_windows(&a);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].0, a.start);
        assert_eq!(windows[0].1, mid);
        assert_eq!(windows[1].1, a.end);
        // The first window is clean, the second is swamped.
        assert!(windows[0].2 > 15.0, "{}", windows[0].2);
        assert!(windows[1].2 < 0.0, "{}", windows[1].2);
        // And half a frame under a 10 dB-stronger interferer cannot survive.
        assert!(phy.success_probability(&a) < 1e-6);
    }

    #[test]
    fn airtime_accounting_sums_to_the_transmitted_time() {
        // Invariant I-R3: every transmission's air time is counted once at the
        // transmitter, and every arrival lands in exactly one outcome bucket.
        let mut ctx = TestCtx::new(11);
        let mut phy = OfdmPhy::new(Tier::High);
        let sizes = [100u32, 300, 400, 1_000];
        let mut expected_tx = 0u64;
        let mut expected_rx = 0u64;
        let mut t = 0u64;
        for (i, bytes) in sizes.into_iter().enumerate() {
            let f = frame(bytes, Mcs::R6Qpsk12);
            ctx.set_now(t);
            let handle = Phy::begin_tx(&mut phy, &mut ctx, NodeId::new(0), &f).expect("starts");
            expected_tx += handle.air_time.as_nanos();
            // Two receivers, one comfortable and one below sensitivity.
            for (rx, power) in [(1u32, -80.0), (2, -95.0)] {
                let mut a = arrival(0, rx, power, t, f);
                a.tx_id = handle.id;
                let h = phy.register_arrival(a);
                expected_rx += air_time(bytes, Mcs::R6Qpsk12).as_nanos();
                let _ = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(rx), h);
            }
            phy.end_tx(handle);
            t += 10_000_000 * (i as u64 + 1);
        }
        let ledger = phy.ledger();
        assert_eq!(ledger.transmitted_total_ns(), expected_tx);
        assert_eq!(ledger.accounted_total_ns(), expected_rx);
        // Four frames at two receivers: four decoded, four below sensitivity.
        assert_eq!(
            ledger
                .lost_ns
                .get(LossCause::BelowSensitivity.label())
                .copied(),
            Some(expected_tx)
        );
        assert_eq!(ledger.received_ns, expected_tx);
    }

    #[test]
    fn cca_is_busy_above_the_threshold_and_while_transmitting() {
        let mut ctx = TestCtx::new(12);
        let mut phy = OfdmPhy::new(Tier::High);
        let f = frame(300, Mcs::R6Qpsk12);
        let node = NodeId::new(1);
        assert_eq!(Phy::cca(&phy, &ctx, node, ChannelId::CCH), CcaState::Idle);
        // A frame at −70 dBm is well above the −85 dBm CBR threshold.
        phy.register_arrival(arrival(0, 1, -70.0, 0, f));
        ctx.set_now(100_000);
        assert!(Phy::cca(&phy, &ctx, node, ChannelId::CCH).is_busy());
        // A frame at −95 dBm is not.
        let mut phy2 = OfdmPhy::new(Tier::High);
        phy2.register_arrival(arrival(0, 1, -95.0, 0, f));
        assert_eq!(Phy::cca(&phy2, &ctx, node, ChannelId::CCH), CcaState::Idle);
        // The ns-3 and Veins presets move the threshold, as 04-models.md §4.5 records.
        let phy3 = OfdmPhy::new(Tier::High).with_cca(CcaConfig::NS3);
        assert_eq!(phy3.cca_config().cca_threshold_dbm(), -62.0);
        let phy4 = OfdmPhy::new(Tier::High).with_cca(CcaConfig::VEINS);
        assert_eq!(phy4.cca_config().cca_threshold_dbm(), -65.0);
    }

    #[test]
    fn the_same_arrival_always_gets_the_same_outcome() {
        // Invariant I-R2: the draw is keyed by (link, frame), so evaluating receivers in
        // any order gives each the same answer.
        let f = frame(400, Mcs::R6Qpsk12);
        let outcome_of = |order_reversed: bool| {
            let mut ctx = TestCtx::new(99);
            let mut phy = OfdmPhy::new(Tier::High);
            // A marginal power, so the draw actually decides.
            let receivers: Vec<u32> = if order_reversed {
                (1..=6).rev().collect()
            } else {
                (1..=6).collect()
            };
            let mut handles = Vec::new();
            for rx in &receivers {
                handles.push((*rx, phy.register_arrival(arrival(0, *rx, -86.5, 0, f))));
            }
            let mut out: Vec<(u32, bool)> = handles
                .into_iter()
                .map(|(rx, h)| {
                    let o = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(rx), h);
                    (rx, matches!(o, RxOutcome::Received { .. }))
                })
                .collect();
            out.sort_unstable();
            out
        };
        assert_eq!(outcome_of(false), outcome_of(true));
    }

    #[test]
    fn the_capture_threshold_shortcut_works() {
        let mut ctx = TestCtx::new(13);
        let mut phy = OfdmPhy::new(Tier::Medium).with_capture(CaptureRule::Threshold {
            cp_th_db: CAPTURE_THRESHOLD_DB,
        });
        let f = frame(300, Mcs::R3Bpsk12);
        // 4 dB of SINR against a 5 dB threshold: lost, with no draw involved.
        let mut a = arrival(0, 1, -80.0, 0, f);
        a.interferers
            .push(InterferenceSource::new(NodeId::new(2), -84.0, 0, a.end));
        let h = phy.register_arrival(a);
        assert_eq!(
            Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h),
            RxOutcome::Lost(LossCause::Collision)
        );
        // 10 dB of SINR: received.
        let mut b = arrival(0, 1, -80.0, 5_000_000, f);
        b.interferers.push(InterferenceSource::new(
            NodeId::new(2),
            -90.0,
            5_000_000,
            b.end,
        ));
        let h = phy.register_arrival(b);
        assert!(matches!(
            Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h),
            RxOutcome::Received { .. }
        ));
    }

    #[test]
    fn the_propagation_delay_is_one_microsecond_per_three_hundred_metres() {
        let d = propagation_delay(300.0);
        assert!((d.as_secs_f64() - 1e-6).abs() < 2e-9, "{d:?}");
        assert_eq!(propagation_delay(0.0), Duration::ZERO);
    }

    #[test]
    fn the_card_validates_and_registers() {
        let mut registry = v2xw_core::registry::Registry::new();
        let phy = OfdmPhy::new(Tier::High);
        phy.card().validate().expect("card validates");
        phy.card().check_api_version().expect("api version");
        registry.register(phy.card().clone()).expect("registers");
        assert!(registry.contains(OfdmPhy::ID));
        // The sensitivity presets all produce a valid card.
        for preset in [
            SensitivityPreset::EtsiStatic,
            SensitivityPreset::EtsiDynamic,
            SensitivityPreset::CohdaMk5,
        ] {
            OfdmPhy::new(Tier::High)
                .with_sensitivity(preset)
                .card()
                .validate()
                .expect("card validates");
        }
    }

    // ---------------------------------------------------------------------------------
    // Jamming (04-models.md §12.3)
    // ---------------------------------------------------------------------------------

    /// A jammer raises the noise floor at a receiver in range, and nowhere else.
    #[test]
    fn a_jammer_raises_the_noise_floor_of_the_receivers_in_range() {
        use crate::jamming::{JamArrival, JamWindow, JammerKind};

        let mut phy = OfdmPhy::new(Tier::High);
        let victim = NodeId::new(1);
        let elsewhere = NodeId::new(2);
        assert_eq!(phy.noise_floor(), -98.0);
        assert_eq!(
            phy.noise_dbm_during(victim, ChannelId::CCH, 0, 1_000),
            -98.0,
            "with no jammer the effective floor is the thermal one, bit for bit"
        );
        assert_eq!(phy.jamming_rise_db(victim, ChannelId::CCH, 0, 1_000), 0.0);

        // A jammer arriving at exactly the noise floor doubles the noise: +3.01 dB.
        phy.note_jamming(
            victim,
            JamArrival::new(
                NodeId::new(9),
                -98.0,
                ChannelId::CCH,
                JamWindow::new(0, 1_000_000),
                JammerKind::Constant,
            ),
        );
        let raised = phy.noise_dbm_during(victim, ChannelId::CCH, 0, 1_000);
        assert!((raised - (-94.989_700_043_360_2)).abs() < 1e-6, "{raised}");
        let rise = phy.jamming_rise_db(victim, ChannelId::CCH, 0, 1_000);
        assert!((rise - 3.010_299_956_639_812).abs() < 1e-9, "{rise}");
        // A receiver out of range is untouched, and so is another channel.
        assert_eq!(
            phy.noise_dbm_during(elsewhere, ChannelId::CCH, 0, 1_000),
            -98.0
        );
        assert_eq!(
            phy.noise_dbm_during(victim, ChannelId::SCH1, 0, 1_000),
            -98.0
        );
        // …and after the window, nothing.
        assert_eq!(
            phy.noise_dbm_during(victim, ChannelId::CCH, 2_000_000, 2_000_001),
            -98.0
        );
    }

    /// The point of modelling jamming: the loss cause distinguishes it from congestion.
    ///
    /// Three arrivals, identical but for what else is on the air:
    ///
    /// 1. clean — decoded;
    /// 2. jammed by a strong constant jammer and nothing else — `jammed`, never `fading`;
    /// 3. an ordinary interferer and no jammer — `collision`, as before.
    #[test]
    fn a_jammed_frame_is_reported_jammed_and_not_as_a_collision() {
        use crate::jamming::{JamArrival, JamWindow, JammerKind};

        let f = frame(300, Mcs::R6Qpsk12);
        // Far above the −88 dBm sensitivity for this MCS, so the sensitivity gate never
        // fires and the error model decides — and with a 28 dB SNR the clean frame's
        // survival probability is one to within a last bit, which is what makes the
        // counterfactual's verdict certain rather than probable.
        let power = -70.0;

        // 1. Clean.
        let mut ctx = TestCtx::new(31);
        let mut phy = OfdmPhy::new(Tier::High);
        let a = arrival(0, 1, power, 0, f);
        let h = phy.register_arrival(a);
        let clean = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert!(
            matches!(clean, RxOutcome::Received { .. }),
            "a clean −70 dBm frame decodes: {clean:?}"
        );

        // 2. Jammed. The jammer arrives 30 dB above the wanted signal for the whole
        //    frame, so the SINR is deeply negative and the frame cannot survive.
        let mut ctx = TestCtx::new(31);
        let mut phy = OfdmPhy::new(Tier::High);
        let a = arrival(0, 1, power, 0, f);
        let end = a.end;
        phy.note_jamming(
            NodeId::new(1),
            JamArrival::new(
                NodeId::new(9),
                power + 30.0,
                ChannelId::CCH,
                JamWindow::new(0, end + 1),
                JammerKind::Constant,
            ),
        );
        let h = phy.register_arrival(a);
        let jammed = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert_eq!(
            jammed,
            RxOutcome::Lost(LossCause::Jammed),
            "a frame killed by a jammer and by nothing else must say so, not 'fading'"
        );

        // 3. Congested, not jammed: an ordinary interferer 30 dB up.
        let mut ctx = TestCtx::new(31);
        let mut phy = OfdmPhy::new(Tier::High);
        let mut a = arrival(0, 1, power, 0, f);
        let end = a.end;
        a.interferers
            .push(InterferenceSource::new(NodeId::new(4), power + 30.0, 0, end));
        let h = phy.register_arrival(a);
        let congested = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert_eq!(
            congested,
            RxOutcome::Lost(LossCause::Collision),
            "an interferer is congestion, whatever a jammer would have done"
        );

        // The two are different buckets in the ledger, which is what a detector reads.
        assert_eq!(
            phy.ledger()
                .lost_ns
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["collision"]
        );
    }

    /// A frame that would have failed anyway keeps its congestion cause: the jammer gets
    /// the blame only when it is to blame.
    #[test]
    fn a_frame_that_would_have_failed_anyway_is_not_blamed_on_the_jammer() {
        use crate::jamming::{JamArrival, JamWindow, JammerKind};

        let f = frame(300, Mcs::R6Qpsk12);
        let power = -70.0;
        let mut ctx = TestCtx::new(31);
        let mut phy = OfdmPhy::new(Tier::High);
        let mut a = arrival(0, 1, power, 0, f);
        let end = a.end;
        // An interferer that already destroys the frame on its own…
        a.interferers
            .push(InterferenceSource::new(NodeId::new(4), power + 30.0, 0, end));
        // …plus a jammer far too weak to matter.
        phy.note_jamming(
            NodeId::new(1),
            JamArrival::new(
                NodeId::new(9),
                -130.0,
                ChannelId::CCH,
                JamWindow::new(0, end + 1),
                JammerKind::Constant,
            ),
        );
        let h = phy.register_arrival(a);
        let outcome = Phy::finish_rx(&mut phy, &mut ctx, NodeId::new(1), h);
        assert_eq!(
            outcome,
            RxOutcome::Lost(LossCause::Collision),
            "the counterfactual says the frame died of congestion: {outcome:?}"
        );
    }

    /// Adding the jamming path must not perturb a run that has no jammer in it: the same
    /// arrival gets the same outcome, the same SINR windows and the same draw.
    #[test]
    fn the_jamming_path_does_not_change_an_unjammed_run() {
        let f = frame(300, Mcs::R6Qpsk12);
        let phy = OfdmPhy::new(Tier::High);
        let mut a = arrival(0, 1, -85.0, 0, f);
        let end = a.end;
        a.interferers
            .push(InterferenceSource::new(NodeId::new(4), -95.0, 1_000, end));
        // With no jamming declared, the two evaluations are identical — bit for bit,
        // because `noise_dbm_during` returns the floor itself rather than a round trip
        // through milliwatts.
        let with = phy.sinr_windows_with(&a, true);
        let without = phy.sinr_windows_with(&a, false);
        assert_eq!(with.len(), 2, "one boundary at the interferer's start");
        for (x, y) in with.iter().zip(without.iter()) {
            assert_eq!(x.0, y.0);
            assert_eq!(x.1, y.1);
            assert_eq!(x.2.to_bits(), y.2.to_bits(), "{x:?} vs {y:?}");
        }
        assert_eq!(
            phy.success_probability(&a).to_bits(),
            phy.success_probability_with(&a, false).to_bits()
        );
        assert!(phy.jamming().is_empty());
    }

    /// A jammer that switches on mid-frame splits the frame into windows exactly as a
    /// mid-frame interferer does, and the counterfactual uses the same partition.
    #[test]
    fn a_mid_frame_jammer_splits_the_frame_into_windows() {
        use crate::jamming::{JamArrival, JamWindow, JammerKind};

        let f = frame(300, Mcs::R6Qpsk12);
        let mut phy = OfdmPhy::new(Tier::High);
        let a = arrival(0, 1, -80.0, 0, f);
        let mid = a.start + (a.end - a.start) / 2;
        phy.note_jamming(
            NodeId::new(1),
            JamArrival::new(
                NodeId::new(9),
                -60.0,
                ChannelId::CCH,
                JamWindow::new(mid, a.end + 1),
                JammerKind::Reactive,
            ),
        );
        let windows = phy.sinr_windows(&a);
        assert_eq!(windows.len(), 2, "{windows:?}");
        assert_eq!(windows[0].1, mid);
        assert!(
            windows[0].2 > windows[1].2 + 10.0,
            "the second half is far worse: {windows:?}"
        );
        // The counterfactual keeps the partition and removes only the noise rise.
        let clean = phy.sinr_windows_with(&a, false);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[0].0, windows[0].0);
        assert_eq!(clean[1].0, windows[1].0);
        assert_eq!(clean[0].2.to_bits(), clean[1].2.to_bits(), "no jammer, one SNR");
        // And the jammer's energy makes carrier sense report a busy medium, which is why
        // the loss cause and not the CBR is what distinguishes jamming from congestion.
        let energy = phy.energy_dbm(NodeId::new(1), ChannelId::CCH, mid + 1);
        assert!(energy > CBR_BUSY_THRESHOLD_DBM, "energy = {energy} dBm");
    }

    #[test]
    fn a_phy_can_be_used_as_a_trait_object() {
        let mut ctx = TestCtx::new(14);
        let mut phy: Box<dyn Phy<TestCtx>> = Box::new(OfdmPhy::new(Tier::High));
        assert_eq!(phy.rat(), Rat::Dsrc80211p);
        assert_eq!(phy.tier(), Tier::High);
        let f = frame(300, Mcs::R6Qpsk12);
        let h = phy.begin_tx(&mut ctx, NodeId::new(0), &f).expect("starts");
        assert_eq!(h.air_time, air_time(300, Mcs::R6Qpsk12));
        assert_eq!(phy.noise_floor_dbm(NodeId::new(0), ChannelId::CCH), -98.0);
        // FrameKind is carried through the descriptor, not the PHY.
        assert!(f.kind.is_group_addressed());
        assert_eq!(f.kind, FrameKind::Broadcast);
    }
}
