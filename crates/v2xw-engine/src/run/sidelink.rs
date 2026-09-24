//! The LTE-V2X and NR-V2X sidelink access layer, driven by the engine's event loop.
//!
//! `radio.rat: lte-v2x-pc5` and `nr-v2x-pc5` select it, and `radio.models.sidelink`
//! configures it (profile, MCS, blind retransmissions, congestion control). What it
//! composes is `v2xw-radio`'s own sidelink stack:
//!
//! * [`v2xw_radio::SpsEngine`] — sensing-based semi-persistent scheduling: the sensing
//!   window, RSRP exclusion with the 3 dB step-up, S-RSSI ranking, the random pick, the
//!   reservation with `C_resel` and `probResourceKeep`, blind-retransmission resources,
//!   the CBR/CR congestion control, and for NR the Rel-16 re-evaluation and pre-emption
//!   (04-models.md §5.1 steps 1-7, §5.2);
//! * [`v2xw_radio::SidelinkPhy`] — half duplex, the SCI drawn against the control
//!   channel's BLER, the sensitivity floor, per-sub-channel SINR with in-band emission from
//!   co-slot transmitters on other sub-channels, and the BLER lookup with chase combining
//!   of blind retransmissions.
//!
//! # How the event loop drives a slotted MAC
//!
//! A frame whose signature has finished waits in the engine's `pending_tx` exactly as an
//! 802.11p frame does, and a [`crate::event::Event::MacTimer`] at the ready instant hands
//! it to the SPS engine. A timer that does not fall on a slot boundary only enqueues and
//! re-arms at the next boundary, because a transport block can only start at one. At a
//! boundary the engine polls: a UE with no reservation (or one the MAC triggers of
//! TS 36.321 §5.14.1.1 invalidate) selects a resource `T1..T2` slots ahead, and a UE whose
//! reservation — or owed blind retransmission — is this slot is granted and goes on the
//! air at once, unless congestion control drops it. The next timer is the MAC's own
//! [`v2xw_radio::SpsEngine::next_grant_at`].
//!
//! Every co-slot transmission starts at the same instant, so when a frame starts, every
//! frame already started in its slot is exactly its interferer set; the two cross-declare
//! at each shared receiver, as the 802.11p path does. The outcome is decided at the slot's
//! end (`PhyEnd`, priority 3), which the kernel dispatches before any `MacTimer` of the
//! next slot (priority 4) — so by then the slot's transmitter set is complete.
//!
//! # What each receiver senses
//!
//! Each transmission puts its received power, split over its sub-channels, into every
//! in-range UE's S-RSSI history as it starts; a jammer's energy goes in the same way
//! ([`v2xw_radio::SpsEngine::note_jammer`]). The S-RSSI is what the CBR counts against
//! −94 dBm (ETSI TS 103 574 §5.2) and what step 5 ranks by. A UE learns a neighbour's
//! *reservation* only from an SCI it decoded: at the slot's end the PHY draws each
//! receiver's SCI against the control channel's BLER at the control channel's SINR —
//! interference included — and only a decoded SCI puts the announced resources, at their
//! PSSCH-RSRP (per resource element, TS 36.214 §5.1.29), into the sensing window.
//!
//! # Blind retransmissions
//!
//! With `max_transmissions` above one, each transport block is sent on up to three
//! resources the SPS engine reserved together (inside ±15 subframes for LTE, 32 slots for
//! NR). A receiver decodes each copy; a copy whose SCI it decoded goes into its soft
//! buffer, and the next copy is decoded at the combined SINR. The reception is recorded
//! once per transport block and receiver: when a copy first decodes (so the latency is
//! that copy's), or when the last copy fails. A copy that arrives after the receiver has
//! decoded is a duplicate the MAC discards, and is not a second reception.
//!
//! # What is not here
//!
//! * **Mode 3 / Mode 1** (base-station scheduled): out of coverage is the V2V case the
//!   J3161/1 deployment profile specifies, and nothing schedules from a gNB here.
//! * **MCS and sub-channel adaptation** under congestion: the UE gives up its blind
//!   retransmissions and then the transport block, which ETSI TS 103 574 §5.3 allows; it
//!   does not raise its MCS, which J3161/1's per-CBR MCS ranges would also allow.
//! * **SAE J3161/1 §7.3.3's SPS + one-shot interleaving.**

use std::collections::{BTreeMap, BTreeSet};

use v2xw_core::card::Tier;
use v2xw_core::event::EventClass;
use v2xw_core::ids::{FrameSeq, NodeId};
use v2xw_core::time::{Duration, SimTime};
use v2xw_radio::sidelink::{CrLimitTable, SlMcsSpec};
use v2xw_radio::{
    AccessCategory, ChannelId, Mac, MacSdu, PoolConfig, RxOutcome, SidelinkPhy, SlArrival,
    SlInterferer, SlResource, SpsEngine, SpsParams,
};

use super::{Engine, FrameState, LinkOutcome};
use crate::ctx::{EngineCtx, RunRecorder};
use crate::error::Result;
use crate::event::Event;
use crate::scenario::Scenario;
use crate::scenario::schema::Rat;
use crate::wiring::{CongestionChoice, SidelinkChoice, SidelinkProfile};

/// The US C-V2X channel: 5.905-5.925 GHz, channel 183, the 20 MHz the FCC's 2020 5.9 GHz
/// order (FCC 20-164) reserved for C-V2X and SAE J3161/1 deploys LTE-V2X on.
pub const CV2X_CHANNEL: ChannelId = ChannelId(183);
/// The carrier the sidelink link budget is evaluated at, hertz: the centre of channel 183.
pub const CV2X_FREQ_HZ: f64 = 5.915e9;
/// The sidelink UE transmit power, dBm: the 23 dBm power class of 3GPP TR 36.885
/// Table A.1.2-1 and TR 37.885 Table 6.1.1-1 (via 04-models.md §5.1), which SAE J3161/1
/// also specifies; it is [`v2xw_radio::SIDELINK_TX_POWER_DBM`].
pub const CV2X_TX_POWER_DBM: f64 = v2xw_radio::SIDELINK_TX_POWER_DBM;

/// What the sidelink access layer did over a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SidelinkReport {
    /// Which sidelink: `lte-v2x-mode4` or `nr-v2x-mode2`.
    pub rat: String,
    /// The configuration the pool and the scheduler come from.
    pub profile: String,
    /// The MCS, by name.
    pub mcs: String,
    /// The resource pool's sub-channel count.
    pub subchannels: u32,
    /// The slot duration, nanoseconds.
    pub slot_ns: u64,
    /// Transmissions per transport block the scheduler reserves for.
    pub max_transmissions: u32,
    /// The congestion-control table enforced, or `off`.
    pub congestion_control: String,
    /// Resource selections, by the trigger that caused them.
    pub selections: BTreeMap<String, u64>,
    /// Transport blocks granted a resource (initial transmissions).
    pub grants: u64,
    /// Blind retransmissions sent.
    pub retransmissions: u64,
    /// Transport blocks dropped because their latency budget passed before a grant.
    pub expired: u64,
    /// Transport blocks refused by a full per-UE queue or as larger than the whole pool.
    pub refused: u64,
    /// Transport blocks congestion control dropped (their CR limit could not be met).
    pub cc_dropped: u64,
    /// Blind retransmissions congestion control dropped.
    pub cc_dropped_retx: u64,
    /// Transmissions that shared at least one sub-channel of their slot with another
    /// transmission: the resource collisions sensing-based SPS is meant to avoid.
    pub overlapping_transmissions: u64,
    /// Sub-channel-slots used by transmissions, over all transmissions.
    pub subchannel_slots_used: u64,
    /// SCIs in-range UEs decoded, and so could schedule around.
    pub sci_decoded: u64,
    /// SCIs in-range UEs missed (control-channel BLER, half duplex).
    pub sci_missed: u64,
    /// Receptions that decoded only because a blind retransmission was combined with an
    /// earlier copy.
    pub combined_decodes: u64,
    /// Receptions first decoded on a blind retransmission, by combining or because the
    /// retransmission's own fade and interference were better.
    pub retx_decodes: u64,
    /// Duplicate copies discarded by receivers that had already decoded the block.
    pub duplicates: u64,
    /// Mean CBR congestion control read at its grants, per mille.
    pub mean_cbr_at_grant_pm: u64,
    /// Largest CBR congestion control read at a grant, per mille.
    pub max_cbr_at_grant_pm: u64,
    #[serde(skip)]
    cbr_sum_pm: u64,
    #[serde(skip)]
    cbr_n: u64,
}

/// A transport block's sidelink state across its transmissions.
#[derive(Debug, Clone, Default)]
pub(crate) struct SlFrame {
    /// The grant of the transmission now on the air.
    pub(crate) grant: Option<v2xw_radio::sps::SlGrantInfo>,
    /// Which transmission of the block is on the air (1-based) and how many there are.
    pub(crate) attempt: u32,
    pub(crate) total: u32,
    /// Each receiver's soft buffer: the linear SINR of the copies it could combine.
    pub(crate) soft: BTreeMap<NodeId, f64>,
    /// How many copies each receiver combined.
    pub(crate) copies: BTreeMap<NodeId, u32>,
    /// Receivers that have decoded the block.
    pub(crate) done: BTreeSet<NodeId>,
    /// Receivers that have not decoded it yet, with the outcome of their latest copy —
    /// what is recorded if no later copy decodes.
    pub(crate) held: BTreeMap<NodeId, LinkOutcome>,
}

/// The sidelink stack of one run.
pub(crate) struct SidelinkAccess {
    pub(crate) phy: SidelinkPhy,
    /// The same PHY at the high tier, for receivers inside a high focus region.
    pub(crate) phy_high: Option<SidelinkPhy>,
    pub(crate) mac: SpsEngine,
    pub(crate) channel: ChannelId,
    pub(crate) freq_hz: f64,
    /// TR 36.885's UE transmit power. A frame's power is now its node's
    /// `radio.devices` power (default the same 23 dBm), so this is the coupling card's
    /// reference value only.
    #[allow(dead_code)]
    pub(crate) tx_power_dbm: f64,
    /// Which nodes transmit in each recent slot, for the half-duplex test at the slot's
    /// end. Pruned to the last few slots as the run advances.
    slot_tx: BTreeMap<u64, BTreeSet<NodeId>>,
    /// The transport blocks on the air in each recent slot — frame, transmitter and
    /// resource — for the co-slot interferer walk and the overlap count.
    slot_frames: BTreeMap<u64, Vec<(FrameSeq, NodeId, SlResource)>>,
    /// Frames whose remaining retransmissions were abandoned, to be resolved when a
    /// recorder is at hand.
    pub(crate) finalize: Vec<FrameSeq>,
    pub(crate) report: SidelinkReport,
}

/// The pool and the scheduler parameters a scenario's sidelink runs.
fn configuration(rat: Rat, choice: SidelinkChoice) -> Option<(PoolConfig, SpsParams, String)> {
    use v2xw_radio::sidelink as sl;
    let (pool, mut params, profile) = match (rat, choice.profile) {
        (Rat::LteV2xPc5, Some(SidelinkProfile::MolinaMasegosa2017)) => (
            PoolConfig::molina_masegosa_highway(),
            SpsParams::molina_masegosa(10),
            "molina-masegosa-2017",
        ),
        (Rat::LteV2xPc5, _) => {
            let mcs: SlMcsSpec = match choice.mcs {
                Some(5) => sl::LTE_MCS5_J3161,
                Some(11) => sl::LTE_MCS11_J3161,
                _ => sl::LTE_MCS7_J3161,
            };
            (
                PoolConfig::sae_j3161(mcs),
                SpsParams::sae_j3161(),
                "sae-j3161",
            )
        }
        (Rat::NrV2xPc5, _) => {
            let mu = sl::Numerology::Mu1;
            let mcs = sl::nr_mcs(choice.mcs.unwrap_or(9))
                .unwrap_or_else(|| sl::nr_mcs(9).expect("MCS 9 is in TS 38.214 Table 5.1.3.1-1"));
            (
                PoolConfig::todisco_nr(mu, mcs),
                // The ETSI TS 103 574 table by default: TS 38.214 §8.1.6 runs the same
                // Σ CR ≤ CR_Limit procedure and no NR value set was found published.
                SpsParams {
                    cc: Some(CrLimitTable::ETSI_TS_103_574),
                    ..SpsParams::ali_todisco(mu, true)
                },
                "todisco-2021",
            )
        }
        _ => return None,
    };
    match choice.congestion {
        CongestionChoice::ProfileDefault => {
            if params.cc.is_none() {
                params.cc = Some(CrLimitTable::ETSI_TS_103_574);
            }
        }
        CongestionChoice::Off => params.cc = None,
        CongestionChoice::Table(id) => params.cc = CrLimitTable::by_id(id),
    }
    if let Some(n) = choice.max_transmissions {
        params = params.with_max_transmissions(n);
    }
    Some((pool, params, profile.to_string()))
}

impl SidelinkAccess {
    /// The access layer `radio.rat` selects, or `None` for 802.11p.
    ///
    /// **LTE-V2X PC5 Mode 4** defaults to the SAE J3161/1 deployment profile (US channel
    /// 183, 20 MHz, ten 10-PRB sub-channels with at least two per transport block, MCS 7,
    /// `probResourceKeep` 0.8, J3161/1's CR limits), with the Molina-Masegosa 2017 study
    /// pool selectable (`profile: molina-masegosa-2017`).
    ///
    /// **NR-V2X PC5 Mode 2** runs Todisco's pool at µ = 1 (30 kHz, 0.5 ms slots): 10 MHz
    /// is 24 PRB at that spacing, two 10-PRB sub-channels, 12 PSSCH symbols of which 2
    /// DMRS, at NR MCS 9 by default, with the Ali/Todisco Mode 2 profile — sensing window
    /// 100 ms, `T1` 2, `T2` 33 slots, RSRP −128 dBm, RRI 100 ms, Rel-16 re-evaluation and
    /// pre-emption on (04-models.md §5.2). No US NR-V2X deployment profile was found, so
    /// this stays a study configuration and the card says so.
    ///
    /// `Rat::Hybrid` is refused by the loader, so it never reaches here.
    pub(crate) fn for_scenario(scenario: &Scenario) -> Option<Self> {
        let tier = match scenario.radio.tiers.phy {
            // The sidelink PHY has medium and high tiers (04-models.md §5.4); an abstract
            // request runs the medium one, which is what the key-status note says.
            Tier::Abstract => Tier::Medium,
            t => t,
        };
        let choice = crate::wiring::radio_models(scenario)
            .ok()
            .and_then(|m| m.sidelink)
            .unwrap_or_default();
        let (pool, params, profile) = configuration(scenario.radio.rat, choice)?;
        let pool = PoolConfig {
            centre_hz: CV2X_FREQ_HZ,
            ..pool
        };
        let report = SidelinkReport {
            rat: pool.rat.label().to_string(),
            profile,
            mcs: pool.mcs.label.to_string(),
            subchannels: pool.subchannels(),
            slot_ns: pool.slot().as_nanos(),
            max_transmissions: 1 + SpsEngine::new(tier, pool.clone(), params.clone()).retx_budget(),
            congestion_control: params
                .cc
                .as_ref()
                .map_or_else(|| "off".to_string(), |t| t.id.to_string()),
            ..SidelinkReport::default()
        };
        let phy = SidelinkPhy::new(tier, pool.clone());
        let phy_high = scenario
            .radio
            .tiers
            .focus
            .as_ref()
            .filter(|f| f.tier == Tier::High && tier != Tier::High)
            .map(|_| phy.at_tier(Tier::High));
        Some(Self {
            phy,
            phy_high,
            mac: SpsEngine::new(tier, pool, params),
            channel: CV2X_CHANNEL,
            freq_hz: CV2X_FREQ_HZ,
            tx_power_dbm: CV2X_TX_POWER_DBM,
            slot_tx: BTreeMap::new(),
            slot_frames: BTreeMap::new(),
            finalize: Vec::new(),
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
        let mut cards = vec![
            self.phy.card().clone(),
            self.mac.card().clone(),
            self.phy.error_model().card().clone(),
            coupling_card(self.mac.pool(), self.mac.params(), &self.report),
        ];
        if let Some(h) = self.phy_high.as_ref() {
            cards.push(h.card().clone());
        }
        for card in cards {
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

    /// The node-level view of how a transmission went out, for `node.tx`.
    pub(crate) fn radio_view(
        &self,
        grant: Option<&v2xw_radio::sps::SlGrantInfo>,
    ) -> (Option<u8>, v2xw_metrics::channels::TxRadioView) {
        let pool = self.mac.pool();
        let mcs = pool.mcs;
        let index = match pool.rat {
            v2xw_radio::SlRat::LteMode4 => v2xw_radio::sidelink::lte_mcs_index(mcs),
            v2xw_radio::SlRat::NrMode2 => mcs
                .label
                .strip_prefix("nr-mcs")
                .and_then(|d| d.parse().ok()),
        };
        let q = |x: f64| (x * 1e4).round() / 1e4;
        let view = v2xw_metrics::channels::TxRadioView {
            rat: pool.rat.label().to_string(),
            mcs: mcs.label.to_string(),
            qm: Some(mcs.qm),
            code_rate: Some(q(mcs.code_rate())),
            slot: grant.map(|g| g.resource.slot),
            subch: grant.map(|g| g.resource.subch),
            subch_len: grant.map(|g| g.resource.len),
            subchannels: Some(pool.subchannels()),
            attempt: grant.map(|g| g.attempt),
            attempts: grant.map(|g| g.total),
            priority: grant.map(|g| g.pppp.0),
            cbr: grant.map(|g| q(g.cbr)),
            cr: grant.map(|g| q(g.cr)),
            cr_limit: grant.and_then(|g| g.cr_limit),
        };
        (index, view)
    }
}

/// The sidelink priority class a message is queued at: awareness messages (BSM, CAM)
/// at AC_VI, which [`v2xw_radio::sidelink::Pppp::of_access_category`] maps to J3161/1's
/// PPPP 5 for the BSM; everything else — event messages, reports, CRLs — at AC_VO.
fn sidelink_ac(msg: v2xw_msg::MsgType) -> AccessCategory {
    match msg {
        v2xw_msg::MsgType::Bsm | v2xw_msg::MsgType::Cam => AccessCategory::Vi,
        _ => AccessCategory::Vo,
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
            let Some((descriptor, msg_type)) =
                self.frames.get(&frame).map(|f| (f.descriptor, f.msg_type))
            else {
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
                    sidelink_ac(msg_type),
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

        // 3. Poll: selection, reselection triggers, congestion control, and this slot's
        //    grant if it has one.
        let (grant, info, expired, cc_dropped, abandoned, next) = {
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
            let info = grant
                .as_ref()
                .and_then(|_| sl.mac.last_grant(node).cloned());
            let expired = sl.mac.take_expired(node);
            let cc_dropped = sl.mac.take_cc_dropped(node);
            let abandoned = sl.mac.take_abandoned(node);
            let next = sl.mac.next_grant_at(node);
            (grant, info, expired, cc_dropped, abandoned, next)
        };
        for sdu in expired {
            self.frames.remove(&sdu.frame.sdu_ref.seq);
            self.report.mac_drops += 1;
            if let Some(sl) = self.sidelink.as_mut() {
                sl.report.expired += 1;
            }
        }
        for sdu in cc_dropped {
            self.frames.remove(&sdu.frame.sdu_ref.seq);
            self.report.mac_drops += 1;
            if let Some(sl) = self.sidelink.as_mut() {
                sl.report.cc_dropped += 1;
            }
        }
        for sdu in abandoned {
            if let Some(sl) = self.sidelink.as_mut() {
                sl.finalize.push(sdu.frame.sdu_ref.seq);
            }
        }
        if let (Some(grant), Some(info)) = (grant, info) {
            let frame = grant.sdu.frame.sdu_ref.seq;
            let first = info.attempt == 1;
            if let Some(sl) = self.sidelink.as_mut() {
                if first {
                    sl.report.grants += 1;
                } else {
                    sl.report.retransmissions += 1;
                }
                let pm = (info.cbr * 1000.0).round() as u64;
                sl.report.cbr_n += 1;
                sl.report.cbr_sum_pm += pm;
                sl.report.max_cbr_at_grant_pm = sl.report.max_cbr_at_grant_pm.max(pm);
            }
            if first {
                self.report.mac_grants += 1;
            }
            let air_ok = self
                .frames
                .get(&frame)
                .is_some_and(|f| f.air.after(now) <= horizon);
            if let Some(state) = self.frames.get_mut(&frame) {
                state.start = now;
                state.end = state.air.after(now);
                state.sl_resource = Some(info.resource);
                if first {
                    self.report.mac_access_delay_ns += now.saturating_sub(state.ready_at);
                }
                // A sidelink has no AIFS and no backoff: the whole wait for the reserved
                // resource is the access delay's remainder, `mac_defer`.
                state.mac_aifs_ns = 0;
                state.mac_backoff_ns = 0;
                state.sl.attempt = info.attempt;
                state.sl.total = info.total;
                state.sl.grant = Some(info);
            }
            if air_ok {
                self.start_frame(frame, horizon);
            } else if let Some(sl) = self.sidelink.as_mut() {
                // The copy would end after the run: whoever still waits on it is resolved
                // at the end of the run.
                if first {
                    self.frames.remove(&frame);
                } else {
                    sl.finalize.push(frame);
                }
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

    /// A transport block goes on the air: the receivers measure its energy, and it and
    /// every transport block already on the air in its slot become each other's
    /// interferers at every receiver they share.
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
        sl.prune(slot);

        // The transmitter's own bookkeeping: it sensed nothing in this slot (half duplex)
        // and the sub-channels count towards its channel-occupancy ratio.
        sl.mac.note_transmitted(state.tx, resource);
        sl.slot_tx.entry(slot).or_default().insert(state.tx);
        sl.report.subchannel_slots_used += u64::from(resource.len);

        // The energy every receiver in range measures: the S-RSSI each sub-channel of the
        // allocation carries, the received power split over them. A faint arrival — under
        // the noise floor less the `radio.range` margin — is energy in the receiver's
        // S-RSSI window too, split the same way, but carries no SCI it could decode, so it
        // is measured and not sensed.
        let share_db = 10.0 * v2xw_core::math::log10(f64::from(resource.len.max(1)));
        for (&rx, &(power, _)) in &state.arrivals {
            for sc in resource.range() {
                sl.mac.note_energy(rx, slot, sc, power - share_db);
            }
        }
        for (&rx, &power) in &state.faint {
            for sc in resource.range() {
                sl.mac.note_energy(rx, slot, sc, power - share_db);
            }
        }

        // Co-slot interference, both ways, at each shared receiver, whether the other
        // transport block is a reception attempt there or only energy.
        let co_slot = sl.slot_frames.get(&slot).cloned().unwrap_or_default();
        sl.slot_frames
            .entry(slot)
            .or_default()
            .push((frame, state.tx, resource));
        for (other_id, _, other_res) in co_slot {
            let Some(other) = self.frames.get_mut(&other_id) else {
                continue;
            };
            let receivers: std::collections::BTreeSet<NodeId> = state
                .arrivals
                .keys()
                .chain(state.faint.keys())
                .copied()
                .collect();
            for rx in receivers {
                if rx == other.tx {
                    continue;
                }
                let mine = state
                    .arrivals
                    .get(&rx)
                    .map(|&(p, _)| (p, true))
                    .or_else(|| state.faint.get(&rx).map(|&p| (p, false)));
                let theirs = other
                    .arrivals
                    .get(&rx)
                    .map(|&(p, _)| (p, true))
                    .or_else(|| other.faint.get(&rx).map(|&p| (p, false)));
                let (Some((power, mine_attempt)), Some((other_power, theirs_attempt))) =
                    (mine, theirs)
                else {
                    continue;
                };
                if mine_attempt {
                    state
                        .sl_interferers
                        .entry(rx)
                        .or_default()
                        .push(SlInterferer {
                            node: other.tx,
                            power_dbm: other_power,
                            resource: other_res,
                        });
                }
                if theirs_attempt {
                    other
                        .sl_interferers
                        .entry(rx)
                        .or_default()
                        .push(SlInterferer {
                            node: state.tx,
                            power_dbm: power,
                            resource,
                        });
                }
            }
        }
    }

    /// The sidelink reception decisions for one copy of a transport block, per receiver,
    /// and the sensing each receiver's SCI allows.
    ///
    /// Sequential, because [`SidelinkPhy::decode`] draws through a context; each draw is
    /// keyed by `(link, slot)`, so the order does not reach an outcome.
    fn sidelink_outcomes(&mut self, state: &mut FrameState) -> Vec<LinkOutcome> {
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
        // one — counted once per transmission, when its slot ends.
        if sl.slot_frames.get(&slot).is_some_and(|v| {
            v.iter()
                .any(|(_, tx, r)| *tx != state.tx && r.overlaps(&resource))
        }) {
            sl.report.overlapping_transmissions += 1;
        }
        let SidelinkAccess {
            phy: sl_phy,
            phy_high,
            mac,
            slot_tx,
            report,
            channel,
            ..
        } = sl;
        let sl_phy: &SidelinkPhy = sl_phy;
        let transmitting = slot_tx.get(&slot);
        let pool = mac.pool().clone();
        let whole_pool = pool.subchannels();
        let rri_slots = mac.params().rri.slots(pool.mu);
        let channel = *channel;
        let announced: Vec<SlResource> = state
            .sl
            .grant
            .as_ref()
            .map_or_else(|| vec![resource], |g| g.announced.clone());
        let jam_field = phy.jamming();
        let mut null = crate::ctx::NullRecorder::new();
        let mut ctx = EngineCtx::new(
            scheduler, rng, world, snapshot, provenance, params, &mut null,
        );
        for (&rx, &(power_dbm, distance_m)) in &state.arrivals {
            let rx_transmitting = transmitting.is_some_and(|t| t.contains(&rx));
            let mut arrival = SlArrival {
                tx_id,
                tx: state.tx,
                rx,
                power_dbm,
                resource,
                bytes: state.layers.psdu_bytes(),
                rx_transmitting,
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
            let rx_phy = if state.focus_high.contains(&rx) {
                phy_high.as_ref().unwrap_or(sl_phy)
            } else {
                sl_phy
            };
            let prior = state.sl.soft.get(&rx).copied().unwrap_or(0.0);
            let dec = rx_phy.decode(&mut ctx, &arrival, prior);

            // Sensing: a decoded SCI tells the receiver every resource this block holds
            // in the period, which it projects forward by the announced period. A
            // resource later in the period is recorded one period back, so that the
            // forward projection lands on it too.
            if !rx_transmitting {
                if dec.sci_decoded {
                    report.sci_decoded += 1;
                    let rsrp = pool.rsrp_dbm(power_dbm, resource.len);
                    for r in &announced {
                        let at = if r.slot > slot && rri_slots > 0 {
                            SlResource::new(r.slot.saturating_sub(rri_slots), r.subch, r.len)
                        } else {
                            *r
                        };
                        mac.note_sensed(rx, at, rsrp, rri_slots as u32);
                    }
                } else {
                    report.sci_missed += 1;
                }
            } else {
                report.sci_missed += 1;
            }

            if dec.soft_sinr_lin > 0.0 {
                *state.sl.soft.entry(rx).or_insert(0.0) += dec.soft_sinr_lin;
                *state.sl.copies.entry(rx).or_insert(0) += 1;
            }
            let (received, mut cause) = match dec.outcome {
                RxOutcome::Received { .. } => (true, None),
                RxOutcome::Lost(c) => (false, Some(c)),
            };
            if received && state.sl.attempt > 1 && !state.sl.done.contains(&rx) {
                report.retx_decodes += 1;
            }
            if received && prior > 0.0 && dec.data_sinr_db.is_finite() {
                // Would this copy alone have decoded? The same keyed draws without the
                // soft buffer answer it; if not, the retransmission's combining did.
                let alone = rx_phy.decode(&mut ctx, &arrival, 0.0);
                if !matches!(alone.outcome, RxOutcome::Received { .. }) {
                    report.combined_decodes += 1;
                }
            }
            // The jamming counterfactual: the same keyed draws, without the jammer. A
            // transport block it would have delivered is the jammer's loss.
            if let (false, Some(clean)) = (received, clean.as_ref())
                && matches!(
                    rx_phy.decode(&mut ctx, clean, prior).outcome,
                    RxOutcome::Received { .. }
                )
            {
                cause = Some(v2xw_radio::LossCause::Jammed);
            }
            out.push(LinkOutcome {
                rx,
                rssi_dbm: v2xw_radio::numeric::q_db(power_dbm),
                sinr_db: v2xw_radio::numeric::q_db(dec.effective_sinr_db),
                distance_m,
                received,
                cause,
                copies: (state.sl.total > 1)
                    .then(|| state.sl.copies.get(&rx).copied().unwrap_or(0)),
            });
        }
        out
    }

    /// One copy of a sidelink transport block ends.
    ///
    /// Receivers that decode it are recorded and delivered now; those that do not wait
    /// for the next copy, unless this was the last, when their loss is recorded. A
    /// receiver that had already decoded the block discards the copy as a duplicate.
    pub(super) fn sidelink_phy_end(
        &mut self,
        recorder: &mut dyn RunRecorder,
        frame: FrameSeq,
        mut state: FrameState,
        now: SimTime,
    ) {
        let frame_index = u64::from(frame.index());
        let tx_id = state.tx_id();
        let last = state.sl.attempt >= state.sl.total.max(1);
        let outcomes = self.sidelink_outcomes(&mut state);
        let mut record: Vec<LinkOutcome> = Vec::new();
        let mut duplicates = 0u64;
        for o in outcomes {
            if state.sl.done.contains(&o.rx) {
                duplicates += 1;
                continue;
            }
            if o.received {
                state.sl.held.remove(&o.rx);
                state.sl.done.insert(o.rx);
                record.push(o);
            } else if last {
                state.sl.held.remove(&o.rx);
                record.push(o);
            } else {
                state.sl.held.insert(o.rx, o);
            }
        }
        if last {
            // Receivers that heard an earlier copy and are out of this one's range.
            record.extend(core::mem::take(&mut state.sl.held).into_values());
        }
        if let Some(sl) = self.sidelink.as_mut() {
            sl.report.duplicates += duplicates;
        }
        record.sort_by_key(|o| o.rx);
        self.finish_phy_end(recorder, frame, &state, now, frame_index, tx_id, record);
        if !last {
            // The block waits for its next copy: its geometry is the next copy's, and
            // what it keeps is the receivers' soft buffers and who has decoded it.
            state.arrivals.clear();
            state.sl_interferers.clear();
            state.focus_high.clear();
            state.focus_placement.clear();
            state.tx_handle = None;
            self.frames.insert(frame, state);
        }
    }

    /// Resolves the frames whose remaining blind retransmissions will not be sent: every
    /// receiver still waiting on one is recorded with its latest copy's loss.
    pub(super) fn sidelink_finalize(&mut self, recorder: &mut dyn RunRecorder) {
        let Some(frames) = self
            .sidelink
            .as_mut()
            .map(|sl| core::mem::take(&mut sl.finalize))
        else {
            return;
        };
        let now = self.scheduler.now();
        for frame in frames {
            let Some(mut state) = self.frames.remove(&frame) else {
                continue;
            };
            // A block with a copy on the air right now is resolved at that copy's end.
            if state.tx_handle.is_some() && state.end > now {
                state.sl.total = state.sl.attempt;
                self.frames.insert(frame, state);
                continue;
            }
            let held: Vec<LinkOutcome> =
                core::mem::take(&mut state.sl.held).into_values().collect();
            if held.is_empty() {
                continue;
            }
            let tx_id = state.tx_id();
            self.finish_rx_only(recorder, frame, &state, now, tx_id, held);
        }
    }

    /// At the end of the run: every sidelink block still waiting for a copy is resolved.
    pub(super) fn sidelink_resolve_all(&mut self, recorder: &mut dyn RunRecorder) {
        if self.sidelink.is_none() {
            return;
        }
        let waiting: Vec<FrameSeq> = self
            .frames
            .iter()
            .filter(|(_, s)| !s.sl.held.is_empty())
            .map(|(f, _)| *f)
            .collect();
        if let Some(sl) = self.sidelink.as_mut() {
            sl.finalize.extend(waiting);
        }
        self.sidelink_finalize(recorder);
    }

    /// The access layer's run summary, when the run is a sidelink one.
    pub(super) fn sidelink_report(&self) -> Option<SidelinkReport> {
        let sl = self.sidelink.as_ref()?;
        let mut report = sl.report.clone();
        report.mean_cbr_at_grant_pm = report.cbr_sum_pm.checked_div(report.cbr_n).unwrap_or(0);
        report.cc_dropped_retx = sl.mac.cc_drops_total().1;
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

    /// A jammer's emission windows, as a sidelink UE's energy measurement sees them.
    pub(super) fn sidelink_note_jam(
        &mut self,
        rx: NodeId,
        power_dbm: f64,
        windows: &[v2xw_radio::JamWindow],
    ) {
        let Some(sl) = self.sidelink.as_mut() else {
            return;
        };
        let slot_ns = sl.mac.pool().slot().as_nanos();
        for w in windows {
            let from = w.from / slot_ns;
            // A slot any part of which the jammer covers is measured with it.
            let to = w.to.div_ceil(slot_ns);
            sl.mac.note_jammer(rx, from, to, power_dbm);
        }
    }
}

/// Model id of the engine's coupling between the sidelink PHY, the SPS engine and the run.
pub const SIDELINK_ACCESS_ID: &str = "access/sidelink/engine-coupling";

/// The card of what this module adds on top of the radio crate's sidelink models: the
/// pool and profile a scenario gets, and the approximations the coupling makes.
fn coupling_card(
    pool: &PoolConfig,
    params: &SpsParams,
    report: &SidelinkReport,
) -> v2xw_core::card::ModelCard {
    use v2xw_core::card::{
        Family, ModelCard, Parameter, Source, SourceKind, Validation, ValidationStatus,
    };
    let std_src = |r: &str| Source::new(SourceKind::Standard, r);
    let j3161 = report.profile == "sae-j3161";
    let mut card = ModelCard::new(
        SIDELINK_ACCESS_ID,
        Family::Mac,
        "2.0.0",
        "How a run drives the sidelink: slot-aligned SPS grants from the event loop, \
         CBR/CR congestion control before each transmission, blind retransmissions with \
         chase combining, sensing from decoded SCIs only, co-slot interference and in-band \
         emission at every shared receiver, and the resource pool and SPS profile \
         radio.rat and radio.models.sidelink select.",
    );
    card.tier = vec![Tier::Medium, Tier::High];
    let profile_src = if j3161 {
        Source::new(
            SourceKind::Standard,
            "SAE J3161/1 (2022, rev. 2024) via Abrar et al. 2026 (arXiv 2608.05087); the \
             standard's text is paywalled and was not read",
        )
    } else {
        Source::new(
            SourceKind::Paper,
            "a study configuration: Molina-Masegosa 2017 (LTE) or Todisco 2021 (NR), via \
             04-models.md §5.5",
        )
    };
    card.parameters = vec![
        Parameter::new(
            "profile",
            "-",
            serde_json::json!(report.profile),
            profile_src.clone(),
        ),
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
            std_src(
                "3GPP TR 36.885 Table A.1.2-1 / TR 37.885 Table 6.1.1-1: 23 dBm UE; \
                     SAE J3161/1 23 dBm",
            ),
        ),
        Parameter::new(
            "bandwidth_prb",
            "PRB",
            serde_json::json!(pool.bandwidth_prb),
            profile_src.clone(),
        ),
        Parameter::new(
            "subchannels",
            "count",
            serde_json::json!(pool.subchannels()),
            profile_src.clone(),
        ),
        Parameter::new(
            "min_subchannels",
            "count",
            serde_json::json!(pool.min_subchannels),
            profile_src.clone(),
        ),
        Parameter::new(
            "mcs",
            "-",
            serde_json::json!(pool.mcs.label),
            profile_src.clone(),
        ),
        Parameter::new(
            "prob_resource_keep",
            "-",
            serde_json::json!(params.prob_keep.probability()),
            profile_src.clone(),
        ),
        Parameter::new(
            "rsrp_threshold_dbm",
            "dBm",
            serde_json::json!(params.rsrp_threshold_dbm),
            Source::new(
                SourceKind::Paper,
                "a study value (04-models.md §5.1, §5.2): J3161/1's threshold is not quoted \
                 by the source read; compared against PSSCH-RSRP per resource element \
                 (TS 36.214 §5.1.29)",
            ),
        ),
        Parameter::new(
            "congestion_control",
            "-",
            serde_json::json!(report.congestion_control),
            params
                .cc
                .as_ref()
                .map_or_else(|| std_src("off"), |t| std_src(t.source)),
        ),
        Parameter::new(
            "cbr_srssi_threshold_dbm",
            "dBm",
            serde_json::json!(params.cbr_threshold_dbm),
            std_src("ETSI TS 103 574 V1.1.1 §5.2: S-RSSI above −94 dBm, window [n−100, n−1]"),
        ),
        Parameter::new(
            "max_transmissions",
            "-",
            serde_json::json!(report.max_transmissions),
            std_src(
                "TS 36.213 §14.1.1.4C / TS 36.212 §5.4.3.1.2 (LTE: one blind \
                 retransmission within 15 subframes); TS 38.214 §8.1.5 (NR: up to 3 \
                 resources within 32 slots)",
            ),
        ),
        Parameter::new(
            "latency_budget_ms",
            "ms",
            serde_json::json!(params.rri.0),
            std_src("3GPP TR 37.885 Table 5.1-1: 100 ms for periodic 10 Hz safety traffic"),
        ),
    ];
    card.assumptions = vec![
        "A UE records a neighbour's reservation only from an SCI it decoded, drawn at the \
         control channel's SINR against a PSCCH curve that is the shared channel's shifted \
         by 6 dB (todo-calibrate on the error model's card)."
            .to_string(),
        "Awareness messages (BSM, CAM) are PPPP 5, SAE J3161/1's priority for the BSM; \
         every other message is PPPP 2. The mapping from message to priority is not \
         published in a source read, beyond the BSM."
            .to_string(),
        "Blind retransmissions combine by chase combining; incremental redundancy's extra \
         gain is not credited."
            .to_string(),
        "A wideband jammer's power lands across the whole pool, so its share in a \
         transport block's allocation, and in each sub-channel's S-RSSI, is that share of \
         the channel."
            .to_string(),
        "NR-V2X enforces ETSI TS 103 574 Table 1 by default: TS 38.214 §8.1.6 runs the \
         same Σ CR ≤ CR_Limit procedure, and no published NR value set was found."
            .to_string(),
    ];
    card.limitations = vec![
        "Under congestion the UE drops its blind retransmissions and then the transport \
         block; it does not raise its MCS or narrow its allocation, which J3161/1's \
         per-CBR MCS ranges and ETSI TS 103 574 §5.3 would also allow."
            .to_string(),
        "SAE J3161/1 §7.3.3's SPS + one-shot interleaving is not modelled.".to_string(),
        "J3161/1 publishes CR limits for PPPP 5 only (as quoted); every priority gets that \
         row under the sae-j3161 table."
            .to_string(),
        "Links beyond the engine's 1 km candidate range are neither received nor counted \
         as interference or sensed energy."
            .to_string(),
    ];
    card.sources = vec![
        std_src("3GPP TS 36.213 §14.1.1.4C, §14.1.1.6, TS 36.321 §5.14.1.1 (Rel-14 Mode 4)"),
        std_src("3GPP TS 38.214 §8.1.4-8.1.6, TS 38.321 §5.22.1 (Rel-16 Mode 2)"),
        std_src("ETSI TS 103 574 V1.1.1 (2018-11): CR limits and the CBR measurement"),
        profile_src,
    ];
    card.validation = Validation {
        status: ValidationStatus::UnitTested,
        references: Vec::new(),
        tests: vec![
            "radio_access::each_radio_technology_runs_its_own_access_layer".to_string(),
            "radio_access::congestion_control_holds_the_cr_limit_under_load".to_string(),
            "radio_access::blind_retransmissions_raise_delivery_at_range".to_string(),
        ],
    };
    card
}
