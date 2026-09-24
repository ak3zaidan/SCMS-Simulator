//! The LTE-V2X and NR-V2X sidelink access layer, driven by the engine's event loop.
//!
//! `radio.rat: lte-v2x-pc5` and `nr-v2x-pc5` select it. What it composes is `v2xw-radio`'s
//! own sidelink stack, unchanged:
//!
//! * [`v2xw_radio::SpsEngine`] — sensing-based semi-persistent scheduling: the sensing
//!   window, RSRP exclusion with the 3 dB step-up, S-RSSI ranking, the random pick, the
//!   reservation with `C_resel` and `probResourceKeep`, and for NR the Rel-16
//!   re-evaluation and pre-emption (04-models.md §5.1 steps 1-7, §5.2);
//! * [`v2xw_radio::SidelinkPhy`] — half duplex, the sensitivity floor, per-sub-channel
//!   SINR with in-band emission from co-slot transmitters on other sub-channels, SCI
//!   decoding before the transport block at the high tier, and the BLER lookup.
//!
//! # How the event loop drives a slotted MAC
//!
//! A frame whose signature has finished waits in the engine's `pending_tx` exactly as an
//! 802.11p frame does, and a [`crate::event::Event::MacTimer`] at the ready instant hands
//! it to the SPS engine. A timer that does not fall on a slot boundary only enqueues and
//! re-arms at the next boundary, because a transport block can only start at one. At a
//! boundary the engine polls: a UE with no reservation (or one the MAC triggers of
//! TS 36.321 §5.14.1.1 invalidate) selects a resource `T1..T2` slots ahead, and a UE whose
//! reservation is this slot is granted and goes on the air at once. The next timer is the
//! MAC's own [`v2xw_radio::SpsEngine::next_grant_at`].
//!
//! Every co-slot transmission starts at the same instant, so when a frame starts, every
//! frame already started in its slot is exactly its interferer set; the two cross-declare
//! at each shared receiver, as the 802.11p path does. The outcome is decided at the slot's
//! end (`PhyEnd`, priority 3), which the kernel dispatches before any `MacTimer` of the
//! next slot (priority 4) — so by then the slot's transmitter set is complete.
//!
//! # What each receiver senses
//!
//! Each transmission puts its received power into every in-range UE's S-RSSI history on
//! each sub-channel it occupied, and into the UE's CBR meter; a UE that heard it above the
//! exclusion threshold also records the SCI's reservation, so its next selection excludes
//! that resource. Treating "heard above the exclusion threshold" as "decoded the SCI" is
//! the approximation `v2xw_radio::sweep` and the published simulators it follows make; it
//! is recorded on the access layer's card rather than hidden.
//!
//! # What is not here
//!
//! * **Sidelink congestion control.** The CBR-dependent CR limit of TS 36.213
//!   §14.1.1.4C (`v2xw_radio::cr_limit`) is measured and not enforced: no UE is throttled.
//! * **Blind retransmissions.** Both profiles send each transport block once, the
//!   04-models.md §5.1 default; `SpsParams::max_transmissions` is the seam.
//! * **Mode 3 / Mode 1** (base-station scheduled): out of coverage is the V2V case the
//!   J3161/1 deployment profile specifies, and nothing schedules from a gNB here.

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::Tier;
use v2xw_core::event::EventClass;
use v2xw_core::ids::{FrameSeq, NodeId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_radio::{
    AccessCategory, ChannelId, Mac, MacSdu, PoolConfig, RxOutcome, SidelinkPhy,
    SlArrival, SlInterferer, SlResource, SpsEngine, SpsParams,
};

use super::{Engine, FrameState, LinkOutcome};
use crate::ctx::EngineCtx;
use crate::error::Result;
use crate::event::Event;
use crate::scenario::Scenario;
use crate::scenario::schema::Rat;

/// The US C-V2X channel: 5.905-5.925 GHz, channel 183, the 20 MHz the FCC's 2020 5.9 GHz
/// order (FCC 20-164) reserved for C-V2X, of which the 10 MHz pools here occupy one half.
pub const CV2X_CHANNEL: ChannelId = ChannelId(183);
/// The carrier the sidelink link budget is evaluated at, hertz: the centre of channel 183.
pub const CV2X_FREQ_HZ: f64 = 5.915e9;
/// The sidelink UE transmit power, dBm: the 23 dBm power class of 3GPP TR 36.885
/// Table A.1.2-1 and TR 37.885 Table 6.1.1-1 (via 04-models.md §5.1), which is
/// [`v2xw_radio::SIDELINK_TX_POWER_DBM`].
pub const CV2X_TX_POWER_DBM: f64 = v2xw_radio::SIDELINK_TX_POWER_DBM;

/// What the sidelink access layer did over a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SidelinkReport {
    /// Which sidelink: `lte-v2x-mode4` or `nr-v2x-mode2`.
    pub rat: String,
    /// The resource pool's sub-channel count.
    pub subchannels: u32,
    /// The slot duration, nanoseconds.
    pub slot_ns: u64,
    /// Resource selections, by the trigger that caused them.
    pub selections: BTreeMap<String, u64>,
    /// Transport blocks granted a resource.
    pub grants: u64,
    /// Transport blocks dropped because their latency budget passed before a grant.
    pub expired: u64,
    /// Transport blocks refused by a full per-UE queue or as larger than the whole pool.
    pub refused: u64,
    /// Transmissions that shared at least one sub-channel of their slot with another
    /// transmission: the resource collisions sensing-based SPS is meant to avoid.
    pub overlapping_transmissions: u64,
    /// Sub-channel-slots used by transmissions, over all transmissions.
    pub subchannel_slots_used: u64,
}

/// The sidelink stack of one run.
pub(crate) struct SidelinkAccess {
    pub(crate) phy: SidelinkPhy,
    pub(crate) mac: SpsEngine,
    pub(crate) channel: ChannelId,
    pub(crate) freq_hz: f64,
    pub(crate) tx_power_dbm: f64,
    /// Which nodes transmit in each recent slot, for the half-duplex test at the slot's
    /// end. Pruned to the last few slots as the run advances.
    slot_tx: BTreeMap<u64, BTreeSet<NodeId>>,
    /// The transport blocks on the air in each recent slot — frame, transmitter and
    /// resource — for the co-slot interferer walk and the overlap count.
    slot_frames: BTreeMap<u64, Vec<(FrameSeq, NodeId, SlResource)>>,
    pub(crate) report: SidelinkReport,
}

impl SidelinkAccess {
    /// The access layer `radio.rat` selects, or `None` for 802.11p.
    ///
    /// **LTE-V2X PC5 Mode 4** runs the Molina-Masegosa validation configuration
    /// (04-models.md §5.5): a 10 MHz pool of four 12-PRB sub-channels with an adjacent
    /// 2-PRB PSCCH, nine data symbols, QPSK r0.7 — one sub-channel carries 190 B, which is
    /// a BSM signed with a digest, and a certificate-bearing BSM takes two — and SPS with a
    /// 1,000-subframe sensing window, `T1` 1, `T2` 100, RSRP threshold −110 dBm, 20 %
    /// candidates, RRI 100 ms and `probResourceKeep` 0.
    ///
    /// **NR-V2X PC5 Mode 2** runs Todisco's pool at µ = 1 (30 kHz, 0.5 ms slots): 10 MHz is
    /// 24 PRB at that spacing, two 10-PRB sub-channels, 12 PSSCH symbols of which 2 DMRS,
    /// at NR MCS 9 (QPSK, R = 679/1024, TS 38.214 Table 5.1.3.1-1) so a sub-channel
    /// carries a digest-signed BSM as LTE's does; and the Ali/Todisco Mode 2 profile —
    /// sensing window 100 ms, `T1` 2, `T2` 33 slots, RSRP −128 dBm, RRI 100 ms, Rel-16
    /// re-evaluation and pre-emption on (04-models.md §5.2).
    ///
    /// `Rat::Hybrid` is refused by the loader, so it never reaches here.
    pub(crate) fn for_scenario(scenario: &Scenario) -> Option<Self> {
        let tier = match scenario.radio.tiers.phy {
            // The sidelink PHY has medium and high tiers (04-models.md §5.4); an abstract
            // request runs the medium one, which is what the key-status note says.
            Tier::Abstract => Tier::Medium,
            t => t,
        };
        let (pool, params) = match scenario.radio.rat {
            Rat::LteV2xPc5 => (
                PoolConfig::molina_masegosa_highway(),
                SpsParams::molina_masegosa(10),
            ),
            Rat::NrV2xPc5 => {
                let mu = v2xw_radio::sidelink::Numerology::Mu1;
                let mcs = v2xw_radio::sidelink::nr_mcs(9).expect("MCS 9 is in Table 5.1.3.1-1");
                (
                    PoolConfig::todisco_nr(mu, mcs),
                    SpsParams::ali_todisco(mu, true),
                )
            }
            _ => return None,
        };
        let pool = PoolConfig {
            centre_hz: CV2X_FREQ_HZ,
            ..pool
        };
        let report = SidelinkReport {
            rat: pool.rat.label().to_string(),
            subchannels: pool.subchannels(),
            slot_ns: pool.slot().as_nanos(),
            ..SidelinkReport::default()
        };
        Some(Self {
            phy: SidelinkPhy::new(tier, pool.clone()),
            mac: SpsEngine::new(tier, pool, params),
            channel: CV2X_CHANNEL,
            freq_hz: CV2X_FREQ_HZ,
            tx_power_dbm: CV2X_TX_POWER_DBM,
            slot_tx: BTreeMap::new(),
            slot_frames: BTreeMap::new(),
            report,
        })
    }

    /// The slot a transport block occupies.
    pub(crate) fn slot(&self) -> Duration {
        self.mac.pool().slot()
    }

    /// Registers the PHY's and the MAC's cards, and the card of the coupling this module
    /// adds between them, so the manifest pins all three.
    pub(crate) fn register(&self, registry: &mut v2xw_core::registry::Registry) -> Result<()> {
        use v2xw_core::model::Model;
        for card in [
            self.phy.card().clone(),
            self.mac.card().clone(),
            coupling_card(self.mac.pool(), self.mac.params()),
        ] {
            if !registry.contains(&card.id) {
                registry.register(card)?;
            }
        }
        Ok(())
    }

    /// Forgets the per-slot bookkeeping older than a few slots.
    fn prune(&mut self, now_slot: u64) {
        let keep_from = now_slot.saturating_sub(4);
        self.slot_tx = self.slot_tx.split_off(&keep_from);
        self.slot_frames = self.slot_frames.split_off(&keep_from);
    }
}

impl Engine {
    /// One sidelink MAC timer: enqueue what has finished signing, and at a slot boundary
    /// poll the SPS engine for this UE's grant.
    pub(super) fn on_sidelink_timer(&mut self, node: NodeId, horizon: SimTime) {
        let now = self.scheduler.now();
        let (slot_ns, now_slot) = {
            let sl = self.sidelink.as_ref().expect("checked by the caller");
            let pool = sl.mac.pool();
            (pool.slot().as_nanos(), pool.slot_of(now))
        };

        // 1. Everything whose signature has finished, in ready order.
        let mut ready: Vec<(SimTime, FrameSeq)> = Vec::new();
        if let Some(pending) = self.pending_tx.get_mut(&node) {
            pending.sort_unstable();
            let split = pending.partition_point(|(t, _)| *t <= now);
            ready.extend(pending.drain(..split));
            if pending.is_empty() {
                self.pending_tx.remove(&node);
            }
        }
        for (_, frame) in ready {
            let Some(descriptor) = self.frames.get(&frame).map(|f| f.descriptor) else {
                continue;
            };
            let refused = {
                let Engine {
                    scheduler,
                    rng,
                    world,
                    snapshot,
                    provenance,
                    params,
                    sidelink,
                    ..
                } = self;
                let mut null = crate::ctx::NullRecorder::new();
                let mut ctx = EngineCtx::new(
                    scheduler, rng, world, snapshot, provenance, params, &mut null,
                );
                let sl = sidelink.as_mut().expect("checked by the caller");
                Mac::enqueue(
                    &mut sl.mac,
                    &mut ctx,
                    node,
                    MacSdu {
                        frame: descriptor,
                        enqueued_at: now,
                    },
                    AccessCategory::Vo,
                )
                .is_err()
            };
            if refused {
                self.frames.remove(&frame);
                self.report.mac_drops += 1;
                if let Some(sl) = self.sidelink.as_mut() {
                    sl.report.refused += 1;
                }
            }
        }

        // 2. A transport block can only start on a slot boundary: off one, wait for it.
        if now % slot_ns != 0 {
            let next = (now_slot + 1) * slot_ns;
            if next <= horizon {
                self.schedule_sidelink_timer(node, next);
            }
            return;
        }

        // 3. Poll: selection, reselection triggers, and this slot's grant if it has one.
        let (grant, expired, resource, next) = {
            let Engine {
                scheduler,
                rng,
                world,
                snapshot,
                provenance,
                params,
                sidelink,
                ..
            } = self;
            let mut null = crate::ctx::NullRecorder::new();
            let mut ctx = EngineCtx::new(
                scheduler, rng, world, snapshot, provenance, params, &mut null,
            );
            let sl = sidelink.as_mut().expect("checked by the caller");
            sl.mac.prune(node, now_slot);
            let grant = Mac::poll(&mut sl.mac, &mut ctx, node, sl.channel);
            let expired = sl.mac.take_expired(node);
            // The grant is on the resource of the selection that booked it; the slot is
            // this one.
            let resource = sl
                .mac
                .last_selection(node)
                .map(|s| SlResource::new(now_slot, s.resource.subch, s.resource.len));
            let next = sl.mac.next_grant_at(node);
            (grant, expired, resource, next)
        };
        for sdu in expired {
            self.frames.remove(&sdu.frame.sdu_ref.seq);
            self.report.mac_drops += 1;
            if let Some(sl) = self.sidelink.as_mut() {
                sl.report.expired += 1;
            }
        }
        if let Some(grant) = grant {
            let frame = grant.sdu.frame.sdu_ref.seq;
            self.report.mac_grants += 1;
            if let Some(sl) = self.sidelink.as_mut() {
                sl.report.grants += 1;
            }
            let air_ok = self.frames.get(&frame).is_some_and(|f| f.air.after(now) <= horizon);
            if let Some(state) = self.frames.get_mut(&frame) {
                state.start = now;
                state.end = state.air.after(now);
                state.sl_resource = resource;
                self.report.mac_access_delay_ns += now.saturating_sub(state.ready_at);
            }
            if air_ok {
                self.start_frame(frame, horizon);
            } else {
                self.frames.remove(&frame);
            }
        }

        // 4. The next time this UE has anything to do.
        if let Some(at) = next {
            let at = at.max(now + slot_ns);
            if at <= horizon {
                self.schedule_sidelink_timer(node, at);
            }
        }
    }

    fn schedule_sidelink_timer(&mut self, node: NodeId, at: SimTime) {
        let channel = self
            .sidelink
            .as_ref()
            .map_or(super::SAFETY_CHANNEL, |sl| sl.channel);
        self.scheduler.schedule(
            at,
            EventClass::MacTimer,
            Event::MacTimer {
                node,
                channel: channel.0,
            },
        );
    }

    /// A transport block goes on the air: the receivers sense it, and it and every
    /// transport block already on the air in its slot become each other's interferers at
    /// every receiver they share.
    pub(super) fn sidelink_register(&mut self, frame: FrameSeq, state: &mut FrameState) {
        let Some(sl) = self.sidelink.as_mut() else {
            return;
        };
        let pool = sl.mac.pool().clone();
        let slot = pool.slot_of(state.start);
        // The transport block carries the network PDU: the SPDU and its WSMP or
        // GeoNetworking header (`FrameState::layers`, with no 802.11 framing on a sidelink).
        let len = pool.subchannels_for(state.layers.psdu_bytes()).unwrap_or(1);
        let resource = state
            .sl_resource
            .unwrap_or_else(|| SlResource::new(slot, 0, len));
        state.sl_resource = Some(resource);
        let rri_slots = sl.mac.params().rri.slots(pool.mu) as u32;
        let threshold = sl.mac.params().rsrp_threshold_dbm;
        sl.prune(slot);

        // The transmitter's own bookkeeping: it sensed nothing in this slot (half duplex)
        // and the sub-channels count towards its channel-occupancy ratio.
        sl.mac.note_transmitted(state.tx, resource);
        sl.slot_tx.entry(slot).or_default().insert(state.tx);
        sl.report.subchannel_slots_used += u64::from(resource.len);

        // Sensing at every receiver in range.
        for (&rx, &(power, _)) in &state.arrivals {
            for sc in resource.range() {
                sl.mac.note_energy(rx, slot, sc, power);
            }
            if power >= threshold {
                sl.mac.note_sensed(rx, resource, power, rri_slots);
            }
        }

        // Co-slot interference, both ways, at each shared receiver.
        let co_slot = sl.slot_frames.get(&slot).cloned().unwrap_or_default();
        sl.slot_frames
            .entry(slot)
            .or_default()
            .push((frame, state.tx, resource));
        for (other_id, _, other_res) in co_slot {
            let Some(other) = self.frames.get_mut(&other_id) else {
                continue;
            };
            for (&rx, &(power, _)) in &state.arrivals {
                if rx == other.tx {
                    continue;
                }
                if let Some(&(other_power, _)) = other.arrivals.get(&rx) {
                    state.sl_interferers.entry(rx).or_default().push(SlInterferer {
                        node: other.tx,
                        power_dbm: other_power,
                        resource: other_res,
                    });
                    other.sl_interferers.entry(rx).or_default().push(SlInterferer {
                        node: state.tx,
                        power_dbm: power,
                        resource,
                    });
                }
            }
        }
    }

    /// The sidelink reception decisions for one transport block, per receiver.
    ///
    /// Sequential, because [`SidelinkPhy::evaluate`] draws through a context; each draw is
    /// keyed by `(link, slot)`, so the order does not reach an outcome.
    pub(super) fn sidelink_outcomes(&mut self, state: &FrameState) -> Vec<LinkOutcome> {
        let Some(resource) = state.sl_resource else {
            return Vec::new();
        };
        let slot = resource.slot;
        let tx_id = state.tx_id();
        let mut out = Vec::with_capacity(state.arrivals.len());
        let Engine {
            scheduler,
            rng,
            world,
            snapshot,
            provenance,
            params,
            sidelink,
            phy,
            ..
        } = self;
        let Some(sl) = sidelink.as_mut() else {
            return out;
        };
        // Whether any other transport block of this slot shared a sub-channel with this
        // one — counted once per transport block, when its slot ends.
        if sl
            .slot_frames
            .get(&slot)
            .is_some_and(|v| v.iter().any(|(_, tx, r)| *tx != state.tx && r.overlaps(&resource)))
        {
            sl.report.overlapping_transmissions += 1;
        }
        let sl = &*sl;
        let transmitting = sl.slot_tx.get(&slot);
        let whole_pool = sl.mac.pool().subchannels();
        let channel = sl.channel;
        let jam_field = phy.jamming();
        let mut null = crate::ctx::NullRecorder::new();
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, &mut null,
        );
        for (&rx, &(power_dbm, distance_m)) in &state.arrivals {
            let mut arrival = SlArrival {
                tx_id,
                tx: state.tx,
                rx,
                power_dbm,
                resource,
                bytes: state.layers.psdu_bytes(),
                rx_transmitting: transmitting.is_some_and(|t| t.contains(&rx)),
                interferers: state.sl_interferers.get(&rx).cloned().unwrap_or_default(),
            };
            // A wideband jammer lands across the whole pool, so its power in the victim's
            // allocation is its share of the band (`SidelinkPhy::interference_split`).
            let jam: Vec<SlInterferer> = jam_field
                .at(rx)
                .iter()
                .filter(|a| a.channel == channel && a.window.overlaps(state.start, state.end))
                .map(|a| SlInterferer {
                    node: a.jammer,
                    power_dbm: a.power_dbm,
                    resource: SlResource::new(slot, 0, whole_pool),
                })
                .collect();
            let jammed = !jam.is_empty();
            let clean = jammed.then(|| arrival.clone());
            arrival.interferers.extend(jam);
            let sinr = sl.phy.data_sinr_db(&arrival);
            let outcome = sl.phy.evaluate(&mut ctx, &arrival);
            let (received, mut cause) = match outcome {
                RxOutcome::Received { .. } => (true, None),
                RxOutcome::Lost(c) => (false, Some(c)),
            };
            // The jamming counterfactual: the same keyed draws, without the jammer. A
            // transport block it would have delivered is the jammer's loss.
            if let (false, Some(clean)) = (received, clean.as_ref())
                && matches!(sl.phy.evaluate(&mut ctx, clean), RxOutcome::Received { .. })
            {
                cause = Some(v2xw_radio::LossCause::Jammed);
            }
            out.push(LinkOutcome {
                rx,
                rssi_dbm: v2xw_radio::numeric::q_db(power_dbm),
                sinr_db: v2xw_radio::numeric::q_db(sinr),
                distance_m,
                received,
                cause,
            });
        }
        out
    }

    /// The access layer's run summary, when the run is a sidelink one.
    pub(super) fn sidelink_report(&self) -> Option<SidelinkReport> {
        let sl = self.sidelink.as_ref()?;
        let mut report = sl.report.clone();
        report.selections = sl
            .mac
            .selections_by_reason()
            .into_iter()
            .map(|(reason, n)| {
                let name = serde_json::to_value(reason)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_else(|| format!("{reason:?}"));
                (name, n)
            })
            .collect();
        Some(report)
    }
}

/// Model id of the engine's coupling between the sidelink PHY, the SPS engine and the run.
pub const SIDELINK_ACCESS_ID: &str = "access/sidelink/engine-coupling";

/// The card of what this module adds on top of the radio crate's sidelink models: the
/// pool and profile a scenario gets, and the approximations the coupling makes.
fn coupling_card(pool: &PoolConfig, params: &SpsParams) -> v2xw_core::card::ModelCard {
    use v2xw_core::card::{Family, ModelCard, Parameter, Source, SourceKind, Validation, ValidationStatus};
    let std_src = |r: &str| Source::new(SourceKind::Standard, r);
    let mut card = ModelCard::new(
        SIDELINK_ACCESS_ID,
        Family::Mac,
        "1.0.0",
        "How a run drives the sidelink: slot-aligned SPS grants from the event loop, \
         co-slot interference and in-band emission at every shared receiver, sensing at \
         every receiver in range, and the resource pool and SPS profile radio.rat selects.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    card.parameters = vec![
        Parameter::new(
            "channel",
            "-",
            serde_json::json!(CV2X_CHANNEL.0),
            std_src("FCC 20-164 (2020): 5.905-5.925 GHz reserved for C-V2X; channel 183"),
        ),
        Parameter::new(
            "tx_power_dbm",
            "dBm",
            serde_json::json!(CV2X_TX_POWER_DBM),
            std_src("3GPP TR 36.885 Table A.1.2-1 / TR 37.885 Table 6.1.1-1: 23 dBm UE"),
        ),
        Parameter::new(
            "subchannels",
            "count",
            serde_json::json!(pool.subchannels()),
            Source::new(
                SourceKind::Paper,
                "Molina-Masegosa 2017 (LTE) / Todisco 2021 (NR) pools, via 04-models.md §5.5",
            ),
        ),
        Parameter::new(
            "rsrp_threshold_dbm",
            "dBm",
            serde_json::json!(params.rsrp_threshold_dbm),
            Source::new(SourceKind::Paper, "04-models.md §5.1, §5.2 study profiles"),
        ),
        Parameter::new(
            "latency_budget_ms",
            "ms",
            serde_json::json!(params.rri.0),
            std_src("3GPP TR 37.885 Table 5.1-1: 100 ms for periodic 10 Hz safety traffic"),
        ),
    ];
    card.assumptions = vec![
        "A UE that receives a transmission at or above the RSRP exclusion threshold is \
         taken to have decoded its SCI and records the reservation; the published \
         simulators the radio crate's sweep follows make the same approximation."
            .to_string(),
        "A wideband jammer's power lands across the whole pool, so its share in a \
         transport block's allocation is the allocation's share of the pool."
            .to_string(),
    ];
    card.limitations = vec![
        "The CBR-dependent CR limit of TS 36.213 §14.1.1.4C is measured, not enforced: no \
         UE is throttled by sidelink congestion control."
            .to_string(),
        "Each transport block is sent once: no blind retransmission.".to_string(),
        "SPS sensing does not see a jammer's energy.".to_string(),
        "Links beyond the engine's 1 km candidate range are neither received nor counted \
         as interference or sensed energy."
            .to_string(),
    ];
    card.sources = vec![
        std_src("3GPP TS 36.213 §14.1.1.6, TS 36.321 §5.14.1.1 (Rel-14 Mode 4)"),
        std_src("3GPP TS 38.214 §8.1.4, TS 38.321 §5.22.1 (Rel-16 Mode 2)"),
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec!["radio_access::each_radio_technology_runs_its_own_access_layer".to_string()],
    };
    card
}
