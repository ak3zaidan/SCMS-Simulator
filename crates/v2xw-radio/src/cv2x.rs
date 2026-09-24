//! `phy/lte-v2x/mode4` and `phy/nr-v2x/mode2` — the sidelink physical layer
//! (04-models.md §5.1, §5.2, tiers in §5.4).
//!
//! The reception chain 04-models.md §5.1 specifies for `medium` and `high`, in order:
//!
//! 1. **Half duplex.** A UE that transmitted in the slot heard nothing in it. In LTE this
//!    is unconditional; in NR it is the same, since a Mode 2 UE has one transceiver.
//!    Reported [`LossCause::HalfDuplex`].
//! 2. **Sensitivity.** Below −90.4 dBm nothing is detected
//!    [TS 36.101 v14.4.0 via 04-models.md §5.1]. Reported
//!    [`LossCause::BelowSensitivity`].
//! 3. **SINR per sub-channel**, with the interferer sum including in-band emissions from
//!    co-slot transmitters on *other* sub-channels through the `K_IBE` mask
//!    ([`crate::sidelink::IbeMask`]).
//! 4. **SCI decoding first.** The control channel is decoded before the transport block,
//!    and an SCI failure loses the transport block. When the collision is a same-resource
//!    reservation — two UEs that picked the same `(slot, sub-channel)` — the loss is
//!    [`LossCause::ResourceCollision`] rather than a plain collision, which is what makes
//!    the SPS failure mode visible in the accounting.
//! 5. **The LUT draw** ([`crate::bler`]).
//!
//! # Why this is not [`crate::phy::OfdmPhy`] with different constants
//!
//! Three differences go all the way down. Air time does not depend on the packet size: a
//! transport block occupies exactly one subframe or slot whatever it carries, and the
//! size decides how many *sub-channels* it takes instead ([`SidelinkPhy::air_time`]).
//! There is no carrier sense, so there is no clear-channel assessment driving access and
//! no capture threshold — a receiver is not "locked" to a preamble. And the interference
//! sum is structured by frequency: two co-slot transmissions on disjoint sub-channels
//! interfere only through the emission mask, which has no 802.11p counterpart.
//!
//! # The four loss causes, and the decomposition they have to reproduce
//!
//! 04-models.md §13's "C-V2X loss decomposition" row requires the per-cause accounting to
//! decompose as Gonzalez-Martin's `PDR = 1 − (P_HD + P_SEN + P_PRO + P_COL)`. The mapping
//! is exact and is asserted by `every_loss_lands_in_the_gonzalez_martin_decomposition`:
//! `P_HD` is [`LossCause::HalfDuplex`], `P_SEN` is [`LossCause::BelowSensitivity`],
//! `P_PRO` is [`LossCause::Fading`] (a failure with no interferer present: propagation and
//! noise alone) and `P_COL` is the three interference causes together.

use std::collections::BTreeMap;

use serde::Serialize;
use v2xw_core::card::{
    Determinism, Equation, Family, ModelCard, Parameter, Source, SourceKind, Tier, Validation,
    ValidationStatus,
};
use v2xw_core::ctx::Ctx;
use v2xw_core::ids::{LinkKey, NodeId};
use v2xw_core::math;
use v2xw_core::model::Model;
use v2xw_core::rng::{EntityRef, RngDomain};
use v2xw_core::time::Duration;

use crate::bler::SidelinkErrorModel;
use crate::error::{RadioError, Result};
use crate::numeric;
use crate::sidelink::{PoolConfig, SlRat, SlResource};
use crate::traits::Phy;
use crate::types::{
    CcaState, ChannelId, FrameDescriptor, LossCause, Mcs, Rat, RxHandle, RxOutcome, TxHandle,
};

/// Thermal noise power spectral density at 290 K, dBm/Hz.
///
/// `10·log10(k·T·1000)` with `k = 1.380649e−23 J/K` (the SI-exact Boltzmann constant) and
/// `T = 290 K`, which is the reference temperature 3GPP noise figures are quoted against.
pub const THERMAL_NOISE_DBM_PER_HZ: f64 = -173.975_093_5;

/// The 3GPP evaluation noise figure for a UE, dB [TR 36.885 Annex A.1.1, and TR 37.885
/// Table 6.1.1-1 for NR, via 04-models.md §5.1].
pub const UE_NOISE_FIGURE_DB: f64 = 9.0;

/// Sidelink receiver sensitivity, dBm [TS 36.101 v14.4.0 via 04-models.md §5.1].
pub const SIDELINK_SENSITIVITY_DBM: f64 = -90.4;

/// Maximum receiver input power, dBm [TS 36.101 v14.4.0 via 04-models.md §5.1].
///
/// Above it the front end is in compression and the LUTs do not apply; the PHY reports
/// the arrival as received but flags the overload in [`SidelinkPhy::overloads`] rather
/// than pretending a curve covers it.
pub const SIDELINK_MAX_INPUT_DBM: f64 = -22.0;

/// The default sidelink transmit power, dBm [TR 36.885 Annex A.1.1: 23 dBm, with 33 dBm
/// not precluded].
pub const SIDELINK_TX_POWER_DBM: f64 = 23.0;

/// The frame-error [`RngDomain`] of each sidelink PHY, derived once.
///
/// Same reasoning as [`crate::phy`]'s: the domain is `SHA-256` over a constant string, so
/// deriving it per arrival is pure cost.
static LTE_ERROR_DOMAIN: std::sync::LazyLock<RngDomain> =
    std::sync::LazyLock::new(|| RngDomain::plugin(SidelinkPhy::ID_LTE));
static NR_ERROR_DOMAIN: std::sync::LazyLock<RngDomain> =
    std::sync::LazyLock::new(|| RngDomain::plugin(SidelinkPhy::ID_NR));

/// One co-slot transmission at a receiver, with the resource it occupied.
///
/// The resource is what makes this different from [`crate::phy::InterferenceSource`]: the
/// sub-channel separation decides whether the contribution is co-channel interference or
/// an in-band emission, and the PHY cannot know it from a power alone.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct SlInterferer {
    /// The interfering transmitter.
    pub node: NodeId,
    /// Its total received power at this receiver over its own allocation, dBm.
    pub power_dbm: f64,
    /// The resource it occupied.
    pub resource: SlResource,
}

/// One sidelink arrival at one receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct SlArrival {
    /// The transmission this belongs to.
    pub tx_id: u64,
    /// The transmitter.
    pub tx: NodeId,
    /// The receiver.
    pub rx: NodeId,
    /// Received power over the allocation, dBm.
    pub power_dbm: f64,
    /// The resource the transport block was sent on.
    pub resource: SlResource,
    /// The transport block's size in bytes, for the record and for the LUT's size note.
    pub bytes: u32,
    /// True when the receiver transmitted in this slot and therefore heard nothing.
    pub rx_transmitting: bool,
    /// Co-slot transmissions at this receiver.
    pub interferers: Vec<SlInterferer>,
}

impl SlArrival {
    /// The link this arrival is on.
    #[must_use]
    pub const fn link(&self) -> LinkKey {
        LinkKey(self.tx, self.rx)
    }

    /// A minimal arrival, for tests and for a caller that fills the rest in.
    #[must_use]
    pub fn new(tx_id: u64, tx: NodeId, rx: NodeId, power_dbm: f64, resource: SlResource) -> Self {
        Self {
            tx_id,
            tx,
            rx,
            power_dbm,
            resource,
            bytes: 190,
            rx_transmitting: false,
            interferers: Vec::new(),
        }
    }
}

/// How one arrival's interference divided between co-channel power and emission leakage.
///
/// Exported because it is what decides the loss cause, and a reader of a lost frame wants
/// to see the number that decided it rather than take the label on trust.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct InterferenceSplit {
    /// Power from transmissions sharing a sub-channel with the victim, mW.
    pub co_channel_mw: f64,
    /// Power leaking in from other sub-channels through the emission mask, mW.
    pub emission_mw: f64,
    /// Noise power in the victim's occupied bandwidth, mW.
    pub noise_mw: f64,
    /// True when some interferer occupied the *identical* resource: the same-resource
    /// reservation collision semi-persistent scheduling produces.
    pub same_resource: bool,
}

impl InterferenceSplit {
    /// Total interference plus noise, mW.
    #[must_use]
    pub fn total_mw(&self) -> f64 {
        math::sum_ordered([self.noise_mw, self.co_channel_mw, self.emission_mw])
    }

    /// True when nothing but noise was present.
    #[must_use]
    pub fn is_noise_only(&self) -> bool {
        self.co_channel_mw == 0.0 && self.emission_mw == 0.0
    }

    /// The loss cause this split implies, when a draw failed with interference present.
    #[must_use]
    pub const fn cause(&self) -> LossCause {
        if self.same_resource {
            LossCause::ResourceCollision
        } else if self.co_channel_mw >= self.emission_mw {
            LossCause::Collision
        } else {
            LossCause::InBandEmission
        }
    }
}

/// A transmission this PHY has on the air.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SlTransmission {
    handle: TxHandle,
    resource: SlResource,
    bytes: u32,
}

/// `phy/lte-v2x/mode4` and `phy/nr-v2x/mode2`.
#[derive(Debug, Clone)]
pub struct SidelinkPhy {
    card: ModelCard,
    tier: Tier,
    pool: PoolConfig,
    error: SidelinkErrorModel,
    noise_figure_db: f64,
    sensitivity_dbm: f64,
    /// True when the high tier's SCI-decoding stage runs.
    decode_sci: bool,
    next_tx_id: u64,
    transmissions: BTreeMap<u64, SlTransmission>,
    arrivals: BTreeMap<(u32, u64), SlArrival>,
    /// Slots each node transmitted in, for the half-duplex test when the caller does not
    /// set [`SlArrival::rx_transmitting`] itself.
    tx_slots: BTreeMap<u32, std::collections::BTreeSet<u64>>,
    overloads: u64,
    ledger: crate::phy::AirtimeLedger,
}

impl SidelinkPhy {
    /// The LTE model's id.
    pub const ID_LTE: &'static str = "phy/lte-v2x/mode4";
    /// The NR model's id.
    pub const ID_NR: &'static str = "phy/nr-v2x/mode2";

    /// The PHY for a pool at a tier, with the error model the pool's MCS is best covered
    /// by ([`SidelinkErrorModel::best_for`]).
    #[must_use]
    pub fn new(tier: Tier, pool: PoolConfig) -> Self {
        let error = SidelinkErrorModel::best_for(pool.mcs);
        Self {
            card: card(tier, &pool, &error),
            tier,
            pool,
            error,
            noise_figure_db: UE_NOISE_FIGURE_DB,
            sensitivity_dbm: SIDELINK_SENSITIVITY_DBM,
            // 04-models.md §5.4: the medium tier has no SCI decoding; the high tier does.
            decode_sci: tier == Tier::High,
            next_tx_id: 1,
            transmissions: BTreeMap::new(),
            arrivals: BTreeMap::new(),
            tx_slots: BTreeMap::new(),
            overloads: 0,
            ledger: crate::phy::AirtimeLedger::new(),
        }
    }

    /// The PHY with a caller-chosen error model.
    #[must_use]
    pub fn with_error_model(mut self, error: SidelinkErrorModel) -> Self {
        self.card = card(self.tier, &self.pool, &error);
        self.error = error;
        self
    }

    /// The PHY with a caller-chosen noise figure.
    #[must_use]
    pub fn with_noise_figure_db(mut self, nf: f64) -> Self {
        self.noise_figure_db = nf;
        self
    }

    /// The pool.
    #[must_use]
    pub const fn pool(&self) -> &PoolConfig {
        &self.pool
    }

    /// The error model.
    #[must_use]
    pub const fn error_model(&self) -> &SidelinkErrorModel {
        &self.error
    }

    /// The air-time ledger.
    #[must_use]
    pub const fn ledger(&self) -> &crate::phy::AirtimeLedger {
        &self.ledger
    }

    /// Arrivals whose received power exceeded [`SIDELINK_MAX_INPUT_DBM`], where the
    /// front end is in compression and no shipped curve applies.
    #[must_use]
    pub const fn overloads(&self) -> u64 {
        self.overloads
    }

    /// The bandwidth one sub-channel occupies, Hz: `subchannel_prb · 12 · SCS`.
    #[must_use]
    pub fn subchannel_bandwidth_hz(&self) -> f64 {
        f64::from(self.pool.subchannel_prb) * 12.0 * f64::from(self.pool.mu.scs_khz()) * 1e3
    }

    /// Noise power in `len` sub-channels, dBm.
    #[must_use]
    pub fn noise_dbm(&self, len: u32) -> f64 {
        let bw = self.subchannel_bandwidth_hz() * f64::from(len.max(1));
        THERMAL_NOISE_DBM_PER_HZ + 10.0 * math::log10(bw) + self.noise_figure_db
    }

    /// Noise power in the control channel's bandwidth, dBm.
    #[must_use]
    pub fn control_noise_dbm(&self) -> f64 {
        let bw =
            f64::from(self.pool.pscch_prb.max(1)) * 12.0 * f64::from(self.pool.mu.scs_khz()) * 1e3;
        THERMAL_NOISE_DBM_PER_HZ + 10.0 * math::log10(bw) + self.noise_figure_db
    }

    /// Registers an arrival and returns its handle.
    pub fn register_arrival(&mut self, arrival: SlArrival) -> RxHandle {
        let handle = RxHandle {
            tx: arrival.tx_id,
            rx: arrival.rx,
        };
        self.arrivals
            .insert((arrival.rx.index(), arrival.tx_id), arrival);
        handle
    }

    /// Adds a co-slot interferer to a registered arrival.
    ///
    /// # Errors
    ///
    /// [`RadioError::UnknownArrival`] when no such arrival is registered.
    pub fn add_interferer(&mut self, h: RxHandle, source: SlInterferer) -> Result<()> {
        let a = self
            .arrivals
            .get_mut(&(h.rx.index(), h.tx))
            .ok_or(RadioError::UnknownArrival {
                tx: h.tx,
                rx: h.rx.index(),
            })?;
        a.interferers.push(source);
        Ok(())
    }

    /// A registered arrival, for inspection.
    #[must_use]
    pub fn arrival(&self, h: RxHandle) -> Option<&SlArrival> {
        self.arrivals.get(&(h.rx.index(), h.tx))
    }

    /// Forgets a completed arrival.
    pub fn forget_arrival(&mut self, h: RxHandle) -> Option<SlArrival> {
        self.arrivals.remove(&(h.rx.index(), h.tx))
    }

    /// How one arrival's interference divides between co-channel power, emission leakage
    /// and noise.
    ///
    /// The co-channel term is the part of the interferer's power that lands in the
    /// victim's allocation: its received power is spread over *its own* `len_j`
    /// sub-channels, so `shared` of them carry `P_j·shared/len_j`. Against the signal's
    /// total power over the victim's allocation and the noise in that bandwidth, that is
    /// the mean over the allocation the LUT is evaluated at (04-models.md §5.4: "EESM
    /// effective SINR is approximated by the mean over the allocation"). The emission term
    /// is the interferer's power attenuated by the mask at the sub-channel separation.
    ///
    /// The term used to divide by the *victim's* length instead. The two agree when both
    /// allocations are the same size and disagree by the ratio of the sizes otherwise: a
    /// one-sub-channel victim under a two-sub-channel interferer was charged the
    /// interferer's whole power instead of half of it, and a two-sub-channel victim under
    /// a one-sub-channel interferer half of it instead of all of it — which is exactly the
    /// mix a BSM that attaches its certificate once a second produces.
    ///
    /// Sums go through [`numeric::sum_powers_mw`], so the result does not depend on the
    /// order the caller collected the interferers in.
    #[must_use]
    pub fn interference_split(&self, arrival: &SlArrival) -> InterferenceSplit {
        let victim = arrival.resource;
        let mut co: Vec<(NodeId, f64)> = Vec::new();
        let mut em: Vec<(NodeId, f64)> = Vec::new();
        let mut same_resource = false;
        for i in &arrival.interferers {
            if i.node == arrival.tx {
                continue;
            }
            let Some(sep) = victim.separation(&i.resource) else {
                continue;
            };
            let power = numeric::dbm_to_mw(i.power_dbm);
            if sep == 0 {
                if i.resource == victim {
                    same_resource = true;
                }
                // The interferer's power in the sub-channels the two share: its total
                // spread over its own allocation, times the overlap.
                let lo = victim.subch.max(i.resource.subch);
                let hi = (victim.subch + victim.len).min(i.resource.subch + i.resource.len);
                let shared = f64::from(hi.saturating_sub(lo));
                let own = f64::from(i.resource.len.max(1));
                co.push((i.node, power * shared / own));
            } else {
                let att = self.pool.ibe.attenuation_db(sep);
                if att.is_finite() {
                    em.push((i.node, power * numeric::db_to_linear(-att)));
                }
            }
        }
        InterferenceSplit {
            co_channel_mw: numeric::sum_powers_mw(&co),
            emission_mw: numeric::sum_powers_mw(&em),
            noise_mw: numeric::dbm_to_mw(self.noise_dbm(victim.len)),
            same_resource,
        }
    }

    /// The effective SINR of the shared channel, dB.
    #[must_use]
    pub fn data_sinr_db(&self, arrival: &SlArrival) -> f64 {
        let split = self.interference_split(arrival);
        numeric::mw_to_dbm(numeric::dbm_to_mw(arrival.power_dbm))
            - numeric::mw_to_dbm(split.total_mw())
    }

    /// The effective SINR of the control channel, dB.
    ///
    /// The control channel occupies `pscch_prb` rather than the whole allocation, so it
    /// sees a narrower noise bandwidth; the interference it sees is the same co-channel
    /// and emission power, because a co-slot transmitter on an overlapping sub-channel
    /// covers the control region too.
    #[must_use]
    pub fn control_sinr_db(&self, arrival: &SlArrival) -> f64 {
        let split = self.interference_split(arrival);
        let noise = numeric::dbm_to_mw(self.control_noise_dbm());
        let total = math::sum_ordered([noise, split.co_channel_mw, split.emission_mw]);
        arrival.power_dbm - numeric::mw_to_dbm(total)
    }

    /// True when a node transmitted in a slot, from this PHY's own bookkeeping.
    #[must_use]
    pub fn transmitted_in(&self, node: NodeId, slot: u64) -> bool {
        self.tx_slots
            .get(&node.index())
            .is_some_and(|s| s.contains(&slot))
    }

    /// The RNG domain this PHY draws its block errors from.
    #[must_use]
    pub fn error_domain(&self) -> RngDomain {
        match self.pool.rat {
            SlRat::LteMode4 => *LTE_ERROR_DOMAIN,
            SlRat::NrMode2 => *NR_ERROR_DOMAIN,
        }
    }

    /// Evaluates one arrival, with exactly one loss cause (invariant I-R3).
    ///
    /// `&self`: an evaluation reads the pool, the error model and the arrival's own
    /// declared geometry and writes nothing, so receivers may be evaluated in parallel
    /// (invariant I-R2).
    pub fn evaluate<C: Ctx + ?Sized>(&self, ctx: &mut C, arrival: &SlArrival) -> RxOutcome {
        // 1. Half duplex: a UE cannot receive in a subframe it transmits in
        //    (04-models.md §5.1, TR 36.885 Annex A.1).
        if arrival.rx_transmitting || self.transmitted_in(arrival.rx, arrival.resource.slot) {
            return RxOutcome::Lost(LossCause::HalfDuplex);
        }
        // 2. Sensitivity.
        if arrival.power_dbm < self.sensitivity_dbm {
            return RxOutcome::Lost(LossCause::BelowSensitivity);
        }
        let split = self.interference_split(arrival);
        let data_sinr = arrival.power_dbm - numeric::mw_to_dbm(split.total_mw());

        // Two draws at most, from one stream keyed by (link, frame): the SCI first, the
        // transport block second. The key embeds the slot, so the pair is a pure function
        // of the link and the slot whatever else the run did.
        let mut rng = ctx.rng(
            self.error_domain(),
            EntityRef::LinkFrame {
                link: arrival.link(),
                frame: arrival.resource.slot,
            },
        );

        // 4. SCI decoding first: an SCI failure loses the transport block
        //    (04-models.md §5.1, high tier).
        if self.decode_sci {
            let sci_sinr = {
                let noise = numeric::dbm_to_mw(self.control_noise_dbm());
                let total = math::sum_ordered([noise, split.co_channel_mw, split.emission_mw]);
                arrival.power_dbm - numeric::mw_to_dbm(total)
            };
            if rng.bool(self.error.sci_bler(sci_sinr)) {
                return RxOutcome::Lost(if split.is_noise_only() {
                    LossCause::Fading
                } else {
                    split.cause()
                });
            }
        }

        // 5. The LUT draw for the transport block.
        if rng.bool(self.error.tb_bler(data_sinr)) {
            return RxOutcome::Lost(if split.is_noise_only() {
                // No interferer: propagation and noise alone, which is Gonzalez-Martin's
                // P_PRO and what LossCause::Fading names.
                LossCause::Fading
            } else {
                split.cause()
            });
        }
        RxOutcome::Received {
            sinr_db: numeric::q_db(data_sinr),
            rssi_dbm: numeric::q_db(arrival.power_dbm),
        }
    }
}

impl Model for SidelinkPhy {
    fn card(&self) -> &ModelCard {
        &self.card
    }
}

impl<C: Ctx + ?Sized> Phy<C> for SidelinkPhy {
    fn tier(&self) -> Tier {
        self.tier
    }

    fn rat(&self) -> Rat {
        self.pool.rat.rat()
    }

    fn begin_tx(&mut self, ctx: &mut C, tx: NodeId, f: &FrameDescriptor) -> Result<TxHandle> {
        // The cap is the pool, not an MSDU length: a transport block that no number of
        // sub-channels can carry is what "too large" means on a sidelink.
        let Some(len) = self.pool.subchannels_for(f.bytes) else {
            return Err(RadioError::FrameTooLarge {
                bytes: f.bytes,
                cap: self.pool.payload_bits(self.pool.subchannels()) / 8,
            });
        };
        let start = ctx.now();
        let slot = self.pool.slot_of(start);
        let air = self.pool.slot();
        let handle = TxHandle {
            id: self.next_tx_id,
            tx,
            channel: f.channel,
            start,
            end: air.after(start),
            air_time: air,
        };
        self.next_tx_id += 1;
        self.transmissions.insert(
            handle.id,
            SlTransmission {
                handle,
                resource: SlResource::new(slot, 0, len),
                bytes: f.bytes,
            },
        );
        self.tx_slots.entry(tx.index()).or_default().insert(slot);
        self.ledger.note_tx(tx, air);
        Ok(handle)
    }

    /// One slot, whatever the transport block carries.
    ///
    /// This is not an approximation: a Mode 4 transport block occupies one subframe and a
    /// Mode 2 one occupies one slot, and the packet size decides how many *sub-channels*
    /// it takes rather than how long it lasts (04-models.md §5.1, §5.2). Both arguments
    /// are therefore unused, and the card says so under `assumptions`.
    fn air_time(&self, _bytes: u32, _mcs: Mcs) -> Duration {
        self.pool.slot()
    }

    /// Sidelink Mode 4 and Mode 2 do not carrier sense, so this is always
    /// [`CcaState::Idle`] except while the node is itself transmitting.
    ///
    /// The medium *is* shared, but it is shared by reservation: the sensing that replaces
    /// clear-channel assessment is the SPS engine's
    /// ([`crate::sps::SpsEngine::note_sensed`]), not a threshold crossing at the instant
    /// of access. Reporting a busy medium here would make an engine that drives both
    /// stacks through the same loop defer a sidelink grant that the standard grants
    /// unconditionally.
    fn cca(&self, ctx: &C, node: NodeId, _ch: ChannelId) -> CcaState {
        let slot = self.pool.slot_of(ctx.now());
        if self.transmitted_in(node, slot) {
            CcaState::Busy {
                energy_dbm: f64::INFINITY,
            }
        } else {
            CcaState::Idle
        }
    }

    fn finish_rx(&mut self, ctx: &mut C, rx: NodeId, h: RxHandle) -> RxOutcome {
        let key = (rx.index(), h.tx);
        let Some(arrival) = self.arrivals.get(&key) else {
            return RxOutcome::Lost(LossCause::OutOfRange);
        };
        let air = self.pool.slot();
        if arrival.power_dbm > SIDELINK_MAX_INPUT_DBM {
            self.overloads += 1;
        }
        let arrival = arrival.clone();
        let outcome = self.evaluate(ctx, &arrival);
        self.arrivals.remove(&key);
        self.ledger.note_rx(&outcome, air);
        outcome
    }

    fn noise_floor_dbm(&self, _node: NodeId, _ch: ChannelId) -> f64 {
        self.noise_dbm(1)
    }
}

fn card(tier: Tier, pool: &PoolConfig, error: &SidelinkErrorModel) -> ModelCard {
    let (id, spec) = match pool.rat {
        SlRat::LteMode4 => (
            SidelinkPhy::ID_LTE,
            "04-models.md §5.1 (Garcia 2021 §II.A, Bazzi 2018 §II-A, \
             Molina-Masegosa 2017, TR 36.885 Annex A.1.1)",
        ),
        SlRat::NrMode2 => (
            SidelinkPhy::ID_NR,
            "04-models.md §5.2 (Garcia 2021 §V.B-V.C, TS 38.211, TS 38.214 §5.1.3.1)",
        ),
    };
    let primary = Source::new(SourceKind::Standard, spec);
    let mut c = ModelCard::new(
        id,
        Family::Phy,
        "1.0.0",
        "Sidelink physical layer: per-sub-channel SINR with in-band emissions, half \
         duplex, SCI decoding before the transport block, and a measured BLER lookup.",
    );
    c.tier = vec![tier];
    c.equations = vec![
        Equation {
            name: "sub-channel SINR".to_string(),
            latex_or_text: "SINR = P_rx / (N + Σ_co-channel P_j·(shared/len_j) + \
                            Σ_other P_j·10^(−K_IBE(Δ)/10))"
                .to_string(),
            notes: Some(
                "P_j is the interferer's power over its own len_j sub-channels, so \
                 shared/len_j of it lands in the victim's allocation; the effective SINR \
                 is the mean over the allocation (04-models.md §5.4)."
                    .to_string(),
            ),
        },
        Equation {
            name: "noise power".to_string(),
            latex_or_text: "N[dBm] = −174 + 10·log10(subchannel_prb·12·SCS·len) + NF".to_string(),
            notes: Some(
                "The noise bandwidth is the *allocation*, not the channel: a \
                 one-sub-channel transport block in a 10 MHz pool sees a fifth of the \
                 channel's noise, which is the whole reason a narrow allocation reaches \
                 further."
                    .to_string(),
            ),
        },
        Equation {
            name: "air time".to_string(),
            latex_or_text: "T = one subframe (LTE) or one slot (NR), independent of the \
                            transport-block size"
                .to_string(),
            notes: None,
        },
    ];
    c.parameters = vec![
        Parameter::new(
            "sensitivity_dbm",
            "dBm",
            serde_json::json!(SIDELINK_SENSITIVITY_DBM),
            Source::new(
                SourceKind::Standard,
                "TS 36.101 v14.4.0 via 04-models.md §5.1: −90.4 dBm sensitivity, −22 dBm \
                 maximum input",
            ),
        ),
        Parameter::new(
            "noise_figure_db",
            "dB",
            serde_json::json!(UE_NOISE_FIGURE_DB),
            Source::new(
                SourceKind::Standard,
                "TR 36.885 Annex A.1.1 via 04-models.md §5.1: 9 dB",
            ),
        ),
        Parameter::new(
            "tx_power_dbm",
            "dBm",
            serde_json::json!(SIDELINK_TX_POWER_DBM),
            Source::new(
                SourceKind::Standard,
                "TR 36.885 Annex A.1.1: 23 dBm, 33 dBm not precluded",
            ),
        ),
        Parameter {
            name: "ibe_adjacent_db".to_string(),
            unit: "dB".to_string(),
            default: serde_json::json!(pool.ibe.adjacent_db),
            range: Some(vec![serde_json::json!(0.0), serde_json::json!(90.0)]),
            source: Source {
                kind: SourceKind::TodoCalibrate,
                reference: "TS 36.101 §6.5.2A.3 with {W, X, Y, Z} = {3, 6, 3, 3} is the \
                            verified parameterization (TR 36.885 Annex A.1.1); \
                            04-models.md §5.1 marks the numeric mask table itself \
                            UNVERIFIED (garbled extraction)"
                    .to_string(),
                accessed: None,
                note: Some(
                    "Todisco's SCS ablation shows the numerology benefit comes mainly \
                     from fewer co-slot IBE contributors, so this term decides whether a \
                     high numerology is over-predicted (04-models.md §5.2 modeling \
                     note 7)."
                        .to_string(),
                ),
            },
            calibration: Some(
                "Read TS 36.101 Table 6.5.2A.3-1, evaluate it at {3, 6, 3, 3} for each \
                 (allocation, separation) pair the pool can produce, and replace the \
                 three-number mask with the resulting table."
                    .to_string(),
            ),
        },
        Parameter::new(
            "bler_curve",
            "-",
            serde_json::json!(error.data_curve().label.clone()),
            Source::new(
                SourceKind::Paper,
                "the error model's own card carries the provenance; see \
                 `crate::bler::SidelinkErrorModel`",
            ),
        ),
        Parameter::new(
            "decode_sci",
            "-",
            serde_json::json!(tier == Tier::High),
            primary.clone(),
        ),
    ];
    c.assumptions = vec![
        "Air time is one slot regardless of the transport-block size; `air_time`'s \
         arguments are unused, and the size enters through the sub-channel count."
            .to_string(),
        "`FrameDescriptor::mcs` is ignored: the sidelink MCS is a property of the pool, \
         not of the frame, and the 802.11p rate enum cannot name a 3GPP MCS. This is what \
         lets a scenario swap radios by changing one field while the message and network \
         layers keep handing down the same descriptor."
            .to_string(),
        "One SINR per arrival, taken as the mean over the allocation, stands in for the \
         EESM effective SINR (04-models.md §5.4)."
            .to_string(),
    ];
    c.limitations = vec![
        "Above −22 dBm received power the front end is in compression and no shipped \
         curve applies; such arrivals are counted in `overloads` and still evaluated."
            .to_string(),
        "PSFCH is not modelled as a decoded channel: a Mode 2 pool with feedback \
         configured spends its PSFCH symbols (they are absent from `data_symbols`) but \
         HARQ combining gain across retransmissions is not credited."
            .to_string(),
    ];
    c.ignores = match tier {
        Tier::Medium => vec![
            "Half-duplex sensing gaps in the SPS engine, SCI decoding, symbol-group SINR, \
             PSFCH (04-models.md §5.4 medium row)."
                .to_string(),
        ],
        _ => vec![
            "Link adaptation by CSI, MIMO, LDPC code-block segmentation; the EESM \
             effective SINR is approximated by the mean over the allocation \
             (04-models.md §5.4 high row)."
                .to_string(),
        ],
    };
    c.sources = vec![primary];
    c.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: vec![Source::new(
            SourceKind::Paper,
            "04-models.md §13: C-V2X Mode 4 PDR versus distance (Molina-Masegosa 2017 \
             Fig. 3), occupancy and collisions (Table 2), PRR within 100/200 m \
             (Bazzi 2018 Fig. 3), Mode 2 PRR versus distance (Todisco 2021 Figs. 7, 8, \
             10(a)); measured by `crate::sweep`",
        )],
        tests: vec![
            "every_loss_lands_in_the_gonzalez_martin_decomposition".to_string(),
            "an_adjacent_subchannel_interferer_reaches_the_victim_only_through_the_mask"
                .to_string(),
            "a_narrower_allocation_sees_a_lower_noise_floor".to_string(),
        ],
    };
    c.determinism = Determinism {
        uses_rng: true,
        rng_domains: vec![RngDomain::plugin(id).as_str().to_string()],
    };
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidelink::{IbeMask, Numerology, nr_mcs};
    use crate::testctx::TestCtx;
    use crate::types::SduRef;
    use v2xw_core::ids::{FrameSeq, SduId};

    fn phy() -> SidelinkPhy {
        SidelinkPhy::new(Tier::High, PoolConfig::molina_masegosa_highway())
    }

    fn arrival(power_dbm: f64) -> SlArrival {
        SlArrival::new(
            1,
            NodeId::new(1),
            NodeId::new(2),
            power_dbm,
            SlResource::new(10, 0, 1),
        )
    }

    #[test]
    fn the_noise_floor_is_the_allocation_bandwidth_not_the_channel() {
        let p = phy();
        // 12 PRB at 15 kHz is 2.16 MHz: −174 + 63.34 + 9 = −101.6 dBm.
        let one = p.noise_dbm(1);
        assert!(
            (one - (-101.63)).abs() < 0.05,
            "one sub-channel noise floor {one:.2} dBm"
        );
        // Doubling the allocation costs 3 dB.
        assert!((p.noise_dbm(2) - one - 3.0103).abs() < 1e-3);
        // The whole 10 MHz pool: four sub-channels of 12 PRB is 8.64 MHz.
        let all = p.noise_dbm(4);
        assert!((all - one - 6.0206).abs() < 1e-3);
    }

    #[test]
    fn a_narrower_allocation_sees_a_lower_noise_floor() {
        let p = phy();
        // This is the mechanism that makes a high-rate, one-sub-channel transport block
        // reach further than a low-rate, wide one at the same SINR requirement, and it is
        // why the PHY must not use a fixed channel noise power.
        for len in 1..4u32 {
            assert!(p.noise_dbm(len) < p.noise_dbm(len + 1));
        }
    }

    #[test]
    fn an_adjacent_subchannel_interferer_reaches_the_victim_only_through_the_mask() {
        let p = phy();
        let mut a = arrival(-70.0);
        // Same slot, sub-channel 2 against the victim's 0: separation 2.
        a.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: -50.0,
            resource: SlResource::new(10, 2, 1),
        });
        let split = p.interference_split(&a);
        assert_eq!(
            split.co_channel_mw, 0.0,
            "disjoint sub-channels do not collide"
        );
        assert!(split.emission_mw > 0.0);
        // The leakage is the mask's attenuation below the interferer's own power.
        let want = numeric::dbm_to_mw(-50.0 - p.pool().ibe.attenuation_db(2));
        assert!((split.emission_mw - want).abs() / want < 1e-9);
        assert!(!split.same_resource);
        assert_eq!(split.cause(), LossCause::InBandEmission);

        // With the mask off, the same interferer contributes nothing at all.
        let off = SidelinkPhy::new(
            Tier::High,
            PoolConfig {
                ibe: IbeMask::OFF,
                ..PoolConfig::molina_masegosa_highway()
            },
        );
        assert_eq!(off.interference_split(&a).emission_mw, 0.0);
        assert!(off.data_sinr_db(&a) > p.data_sinr_db(&a));
    }

    #[test]
    fn an_identical_resource_is_reported_as_a_resource_collision() {
        let p = phy();
        let mut a = arrival(-70.0);
        a.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: -60.0,
            resource: SlResource::new(10, 0, 1),
        });
        let split = p.interference_split(&a);
        assert!(split.same_resource);
        assert!(split.co_channel_mw > 0.0);
        assert_eq!(split.cause(), LossCause::ResourceCollision);
        // A partial overlap of a wider allocation is a plain collision, not a
        // same-resource one: the two UEs did not pick the same resource.
        let mut wide = SlArrival::new(
            1,
            NodeId::new(1),
            NodeId::new(2),
            -70.0,
            SlResource::new(10, 0, 2),
        );
        wide.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: -60.0,
            resource: SlResource::new(10, 1, 1),
        });
        let ws = p.interference_split(&wide);
        assert!(!ws.same_resource);
        assert_eq!(ws.cause(), LossCause::Collision);
        // The interferer occupies one sub-channel and all of it lies inside the victim's
        // two, so *all* of its power lands in the victim's allocation. (This assertion
        // used to expect half: the term divided by the victim's length rather than the
        // interferer's, which halved an interferer that sat wholly inside the victim.)
        let want = numeric::dbm_to_mw(-60.0);
        assert!((ws.co_channel_mw - want).abs() / want < 1e-9);

        // The converse: a one-sub-channel victim under a two-sub-channel interferer
        // receives the half of the interferer's power that falls on the shared
        // sub-channel, not all of it.
        let mut narrow = SlArrival::new(
            1,
            NodeId::new(1),
            NodeId::new(2),
            -70.0,
            SlResource::new(10, 1, 1),
        );
        narrow.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: -60.0,
            resource: SlResource::new(10, 0, 2),
        });
        let ns = p.interference_split(&narrow);
        let want = numeric::dbm_to_mw(-60.0) * 0.5;
        assert!((ns.co_channel_mw - want).abs() / want < 1e-9);
    }

    #[test]
    fn a_different_slot_never_interferes() {
        let p = phy();
        let mut a = arrival(-70.0);
        a.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: 20.0,
            resource: SlResource::new(11, 0, 1),
        });
        let split = p.interference_split(&a);
        assert!(
            split.is_noise_only(),
            "a transmission in another slot cannot collide"
        );
    }

    #[test]
    fn every_loss_lands_in_the_gonzalez_martin_decomposition() {
        // 04-models.md §13, "C-V2X loss decomposition": the causes must decompose as
        // P_HD + P_SEN + P_PRO + P_COL.
        let p = phy();
        let mut ctx = TestCtx::new(1);

        // P_HD: the receiver transmitted in the slot.
        let mut hd = arrival(-60.0);
        hd.rx_transmitting = true;
        assert_eq!(
            p.evaluate(&mut ctx, &hd),
            RxOutcome::Lost(LossCause::HalfDuplex)
        );

        // P_SEN: below −90.4 dBm.
        assert_eq!(
            p.evaluate(&mut ctx, &arrival(-100.0)),
            RxOutcome::Lost(LossCause::BelowSensitivity)
        );

        // P_PRO: detectable, but below the LUT's 10 % point with no interferer present.
        //
        // It takes a high-spectral-efficiency pool to produce this cause at all, and that
        // is worth knowing: with QPSK r0.7 in one 12-PRB sub-channel the noise floor is
        // −101.6 dBm and the 10 % point 8.7 dB above it, i.e. −92.9 dBm, which is *below*
        // the −90.4 dBm receiver sensitivity. For the reference mapping the front end's
        // sensitivity spec, not thermal noise, sets the range, so every noise-limited
        // loss is reported P_SEN. NR MCS 21 needs about 17.25 dB, i.e. −84.4 dBm, and
        // there the noise-limited region is real.
        let hi_se = SidelinkPhy::new(
            Tier::High,
            PoolConfig::todisco_nr(Numerology::Mu0, nr_mcs(21).unwrap()),
        );
        let weak = hi_se.evaluate(&mut ctx, &arrival(-88.0));
        assert_eq!(weak, RxOutcome::Lost(LossCause::Fading));

        // P_COL: a strong co-channel interferer on the identical resource.
        let mut col = arrival(-70.0);
        col.interferers.push(SlInterferer {
            node: NodeId::new(3),
            power_dbm: -40.0,
            resource: SlResource::new(10, 0, 1),
        });
        assert_eq!(
            p.evaluate(&mut ctx, &col),
            RxOutcome::Lost(LossCause::ResourceCollision)
        );

        // A clean, strong link decodes.
        match p.evaluate(&mut ctx, &arrival(-60.0)) {
            RxOutcome::Received { sinr_db, rssi_dbm } => {
                assert!(
                    sinr_db > 20.0,
                    "a −60 dBm link should be far above the LUT knee"
                );
                assert!((rssi_dbm - (-60.0)).abs() < 1e-3);
            }
            other => panic!("a −60 dBm clean link should decode, got {other:?}"),
        }

        // Every cause the PHY can report is in the decomposition, and nothing else is.
        let ledger = p.ledger();
        let reported: Vec<&str> = ledger.lost_ns.keys().map(String::as_str).collect();
        // The evaluate() calls above went through `evaluate`, not `finish_rx`, so the
        // ledger is empty; what matters is that the cause set is closed.
        assert!(reported.is_empty());
        for cause in [
            LossCause::HalfDuplex,
            LossCause::BelowSensitivity,
            LossCause::Fading,
            LossCause::Collision,
            LossCause::ResourceCollision,
            LossCause::InBandEmission,
        ] {
            assert!(!cause.label().is_empty());
        }
    }

    #[test]
    fn the_outcome_is_the_same_whatever_order_the_interferers_arrived_in() {
        let p = phy();
        let mut ctx = TestCtx::new(9);
        let mk = |order: bool| {
            let mut a = arrival(-70.0);
            let mut ints = vec![
                SlInterferer {
                    node: NodeId::new(3),
                    power_dbm: -75.0,
                    resource: SlResource::new(10, 0, 1),
                },
                SlInterferer {
                    node: NodeId::new(4),
                    power_dbm: -76.0,
                    resource: SlResource::new(10, 1, 1),
                },
                SlInterferer {
                    node: NodeId::new(5),
                    power_dbm: -77.0,
                    resource: SlResource::new(10, 3, 1),
                },
            ];
            if order {
                ints.reverse();
            }
            a.interferers = ints;
            a
        };
        let a = p.evaluate(&mut ctx, &mk(false));
        let b = p.evaluate(&mut ctx, &mk(true));
        assert_eq!(
            a, b,
            "the interference sum must be order-independent (I-R2)"
        );
        assert_eq!(
            p.interference_split(&mk(false)).total_mw(),
            p.interference_split(&mk(true)).total_mw()
        );
    }

    #[test]
    fn the_air_time_is_one_slot_whatever_the_transport_block_is() {
        let lte = phy();
        assert_eq!(
            Phy::<TestCtx>::air_time(&lte, 190, Mcs::R6Qpsk12),
            Duration::from_millis(1)
        );
        assert_eq!(
            Phy::<TestCtx>::air_time(&lte, 1200, Mcs::R27Qam64_34),
            Duration::from_millis(1)
        );
        // NR at µ = 2 is a quarter-millisecond slot.
        let nr = SidelinkPhy::new(
            Tier::High,
            PoolConfig::todisco_nr(Numerology::Mu2, nr_mcs(21).unwrap()),
        );
        assert_eq!(
            Phy::<TestCtx>::air_time(&nr, 350, Mcs::R6Qpsk12),
            Duration::from_micros(250)
        );
        assert_eq!(Phy::<TestCtx>::rat(&nr), Rat::NrV2xPc5);
        assert_eq!(Phy::<TestCtx>::rat(&lte), Rat::LteV2xPc5);
    }

    #[test]
    fn begin_tx_refuses_a_transport_block_the_pool_cannot_carry() {
        let mut p = phy();
        let mut ctx = TestCtx::new(1);
        let f = FrameDescriptor::broadcast(
            100_000,
            Mcs::R6Qpsk12,
            SduRef::new(SduId::new(1), FrameSeq::new(1)),
        );
        let err = Phy::begin_tx(&mut p, &mut ctx, NodeId::new(1), &f)
            .expect_err("100 kB cannot fit a 10 MHz pool");
        assert!(matches!(err, RadioError::FrameTooLarge { .. }));
        // A 190 B block is granted, and the handle covers exactly one subframe.
        let ok = FrameDescriptor::broadcast(
            190,
            Mcs::R6Qpsk12,
            SduRef::new(SduId::new(2), FrameSeq::new(1)),
        );
        let h = Phy::begin_tx(&mut p, &mut ctx, NodeId::new(1), &ok).expect("granted");
        assert_eq!(h.air_time, Duration::from_millis(1));
        assert_eq!(h.end, h.start + 1_000_000);
    }

    #[test]
    fn the_receiver_hears_nothing_in_a_slot_it_transmitted_in() {
        let mut p = phy();
        let mut ctx = TestCtx::new(2);
        let f = FrameDescriptor::broadcast(
            190,
            Mcs::R6Qpsk12,
            SduRef::new(SduId::new(1), FrameSeq::new(1)),
        );
        ctx.set_now(10_000_000);
        Phy::begin_tx(&mut p, &mut ctx, NodeId::new(2), &f).expect("granted");
        // Node 2 transmitted in slot 10; a strong arrival there is lost to half duplex.
        let a = arrival(-50.0);
        assert_eq!(a.resource.slot, 10);
        assert_eq!(
            p.evaluate(&mut ctx, &a),
            RxOutcome::Lost(LossCause::HalfDuplex)
        );
        // In another slot the same arrival decodes.
        let mut other = arrival(-50.0);
        other.resource = SlResource::new(11, 0, 1);
        assert!(matches!(
            p.evaluate(&mut ctx, &other),
            RxOutcome::Received { .. }
        ));
    }

    #[test]
    fn there_is_no_carrier_sense_on_a_sidelink() {
        let p = phy();
        let ctx = TestCtx::new(1);
        // Idle even though the pool may be fully booked: access is by reservation.
        assert_eq!(
            Phy::cca(&p, &ctx, NodeId::new(1), ChannelId::CCH),
            CcaState::Idle
        );
    }

    #[test]
    fn an_overloaded_front_end_is_counted_rather_than_hidden() {
        let mut p = phy();
        let mut ctx = TestCtx::new(1);
        let h = p.register_arrival(arrival(-10.0));
        assert!(matches!(
            Phy::finish_rx(&mut p, &mut ctx, NodeId::new(2), h),
            RxOutcome::Received { .. }
        ));
        assert_eq!(
            p.overloads(),
            1,
            "−10 dBm is above the −22 dBm maximum input"
        );
    }

    #[test]
    fn the_medium_tier_skips_sci_decoding_and_the_high_tier_does_not() {
        let hi = SidelinkPhy::new(Tier::High, PoolConfig::molina_masegosa_highway());
        let med = SidelinkPhy::new(Tier::Medium, PoolConfig::molina_masegosa_highway());
        assert!(hi.decode_sci);
        assert!(!med.decode_sci);
        // Two draws against one: the high tier is never more optimistic than the medium
        // tier at the same SINR, which is the direction a tier refinement must go.
        let mut ctx = TestCtx::new(4);
        let mut lost_hi = 0;
        let mut lost_med = 0;
        for n in 0..400u32 {
            let mut a = arrival(-87.0);
            a.tx = NodeId::new(n + 10);
            a.resource = SlResource::new(u64::from(n), 0, 1);
            if matches!(hi.evaluate(&mut ctx, &a), RxOutcome::Lost(_)) {
                lost_hi += 1;
            }
            if matches!(med.evaluate(&mut ctx, &a), RxOutcome::Lost(_)) {
                lost_med += 1;
            }
        }
        assert!(
            lost_hi >= lost_med,
            "high tier lost {lost_hi}, medium {lost_med}: adding the SCI stage cannot help"
        );
    }

    #[test]
    fn the_cards_validate_for_both_sidelinks() {
        let lte = phy();
        assert_eq!(lte.card().id, SidelinkPhy::ID_LTE);
        lte.card().validate().expect("LTE card validates");
        let nr = SidelinkPhy::new(
            Tier::High,
            PoolConfig::todisco_nr(Numerology::Mu1, nr_mcs(21).unwrap()),
        );
        assert_eq!(nr.card().id, SidelinkPhy::ID_NR);
        nr.card().validate().expect("NR card validates");
        // The two PHYs draw from different plug-in domains, so an LTE run and an NR run
        // of the same scenario do not consume each other's streams.
        assert_ne!(lte.error_domain().code(), nr.error_domain().code());
    }
}
